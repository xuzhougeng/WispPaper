//! A review's working record. The record keeps user decisions separately from
//! model output; a UI must explicitly advance the confirmed stages.

use crate::model::new_id;
use crate::store::Store;
use crate::time::now_iso;
use anyhow::{Result, bail};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS review_projects (
    id TEXT PRIMARY KEY NOT NULL,
    revision INTEGER NOT NULL,
    body TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS review_conversations (
    conversation_id TEXT PRIMARY KEY NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    review_id TEXT UNIQUE NOT NULL REFERENCES review_projects(id) ON DELETE CASCADE
);
-- A review lives with its conversation: one whose conversation is gone (removed by a build
-- that knew nothing of reviews) could not be reached from anywhere.
DELETE FROM review_projects WHERE id NOT IN (SELECT review_id FROM review_conversations);
-- What builds between two releases put on the tables of the papers to watch a source change.
-- A judgment now keeps a fingerprint of what was read instead (`Store::evidence_fingerprint`).
DROP TRIGGER IF EXISTS review_preserve_evidence;
DROP TRIGGER IF EXISTS review_prevent_orphan;
DROP TRIGGER IF EXISTS review_digest_after_insert;
DROP TRIGGER IF EXISTS review_digest_after_update;
DROP TRIGGER IF EXISTS review_digest_after_delete;
DROP TRIGGER IF EXISTS review_version_paper_update;
DROP TRIGGER IF EXISTS review_version_paper_delete;
DROP TRIGGER IF EXISTS review_version_identifier_insert;
DROP TRIGGER IF EXISTS review_version_identifier_delete;
DROP TRIGGER IF EXISTS review_version_identifier_update;
DROP TRIGGER IF EXISTS review_version_resource_insert;
DROP TRIGGER IF EXISTS review_version_resource_delete;
DROP TRIGGER IF EXISTS review_version_resource_update;
DROP TRIGGER IF EXISTS review_version_text_insert;
DROP TRIGGER IF EXISTS review_version_text_update;
DROP TRIGGER IF EXISTS review_version_text_delete;
DROP TRIGGER IF EXISTS review_version_trashed;
DROP TRIGGER IF EXISTS review_version_restored;
DROP TABLE IF EXISTS review_text_digests;
DROP TABLE IF EXISTS review_source_versions;
"#;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReviewSection {
    pub name: String,
    pub body: String,
}

/// What the claim rests on, as the citation or the person says: not proof
/// that anything was read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSource {
    #[default]
    Unknown,
    Record,
    Abstract,
    FullText,
}

/// A person's explicit assessment of how this source relates to a claim.
/// A text match or model response never sets this on the person's behalf.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceJudgment {
    #[default]
    Unreviewed,
    Supports,
    Partial,
    Contradicts,
    Insufficient,
}

/// One claim and one source. `paper_id` is the library identity rather than
/// a conversation-local P label; the title snapshot remains if it is removed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EvidenceRow {
    pub id: String,
    /// Empty means a working claim not linked to a saved review section.
    pub section: String,
    /// An association is not silently restored if a same-named section is created later.
    pub section_detached: bool,
    pub claim: String,
    pub paper_id: String,
    pub paper_title: String,
    /// One-based page of the paper's PDF, as cited or entered.
    pub page: Option<usize>,
    pub quote: String,
    pub source: EvidenceSource,
    pub note: String,
    pub judgment: EvidenceJudgment,
    /// What the source said when the person judged: `Store::evidence_fingerprint`.
    pub assessed_fingerprint: String,
    /// The paper is in the library and not in its trash. Worked out on reading.
    #[serde(skip)]
    pub source_available: bool,
    /// The judgment was made on what the source says now. Worked out on reading.
    #[serde(skip)]
    pub source_current: bool,
}

impl EvidenceRow {
    /// The same claim resting on the same place of the same paper.
    fn same_evidence(&self, other: &EvidenceRow) -> bool {
        self.section == other.section && self.claim == other.claim && self.paper_id == other.paper_id && self.page == other.page && self.quote == other.quote
    }

    fn unjudge(&mut self) {
        self.judgment = EvidenceJudgment::Unreviewed;
        self.assessed_fingerprint.clear();
        self.source_current = false;
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Review {
    pub id: String,
    pub revision: i64,
    pub title: String,
    pub requirements: String,
    pub requirements_confirmed: bool,
    pub outline: String,
    pub outline_confirmed: bool,
    pub evidence: String,
    /// Structured annotations supplement (and do not overwrite) older notes.
    pub evidence_rows: Vec<EvidenceRow>,
    pub sections: Vec<ReviewSection>,
    pub checks: String,
    /// True only when checks were recorded after the last content change.
    pub checks_current: bool,
    pub completed: bool,
    pub updated_at: String,
}

impl Review {
    /// Where the review stands: 1 the requirements, 2 the outline, 3 writing.
    pub fn step(&self) -> usize {
        if !self.requirements_confirmed { 1 } else if !self.outline_confirmed { 2 } else { 3 }
    }

    /// The row's claim still stands in the section it was taken from (a row without a section is on its own).
    pub fn linked(&self, row: &EvidenceRow) -> bool {
        !row.section_detached && (row.section.is_empty() || self.sections.iter().any(|section| section.name == row.section && section.body.contains(row.claim.trim())))
    }
}

/// Each operation is an explicit user action, not a model's self-assessment.
pub enum ReviewAction {
    SetRequirements(String),
    ConfirmRequirements,
    SetOutline(String),
    ConfirmOutline,
    SetEvidence(String),
    /// Add (empty id) or edit a row. Assessment is always a separate action.
    SaveEvidence(EvidenceRow),
    JudgeEvidence { id: String, judgment: EvidenceJudgment },
    RemoveEvidence { id: String },
    /// A section's text, new or again. `cited` is what its citations rest on
    /// (`cited_evidence`): rows the record does not hold yet are added, unjudged.
    SaveSection { name: String, body: String, cited: Vec<EvidenceRow> },
    RemoveSection { name: String },
    SetChecks(String),
    Complete,
}

fn nonempty(value: &str, what: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{what}不能为空")
    }
    Ok(())
}

/// The claims of a section's text with what each citation names for them:
/// the sentence a citation closes, the paper behind its label, the page and
/// the quotation. This is the evidence table the text already carries; whether
/// the source bears the claim out stays the person's to say. A label that
/// names no source, or more than one, is left out.
pub fn cited_evidence(section: &str, body: &str, sources: &[crate::ai::Source]) -> Vec<EvidenceRow> {
    let spans = crate::ai::citation_spans(body);
    let inside = |at: usize| spans.iter().any(|(range, _)| range.contains(&at));
    // Where sentences end, outside the citations (a quotation has full stops of its own).
    let mut ends: Vec<usize> = vec![0];
    let mut letters = body.char_indices().peekable();
    while let Some((at, letter)) = letters.next() {
        if inside(at) { continue }
        let closes = match letter {
            '。' | '！' | '？' | '\n' => true,
            '.' | '!' | '?' => letters.peek().is_none_or(|(_, next)| next.is_whitespace()),
            _ => false,
        };
        if closes { ends.push(at + letter.len_utf8()) }
    }
    // "A sentence. [P1] The next one": a citation set after the full stop closes that sentence, and the next begins after it.
    let stops = |text: &str| text.trim_end().trim_end_matches(['"', '”', '」']).ends_with(['。', '！', '？', '.', '!', '?']);
    ends.extend(spans.iter().filter(|(range, _)| stops(&body[..range.start])).map(|(range, _)| range.end));
    // What a stretch of the text says, its citations aside.
    let says = |from: usize, to: usize| spans.iter().filter(|(range, _)| range.start >= from && range.end <= to).fold(body[from..to].to_string(), |text, (range, _)| text.replace(&body[range.clone()], "")).chars().any(char::is_alphanumeric);
    let mut rows: Vec<EvidenceRow> = vec![];
    let mut last_claim = String::new();
    for (range, cited) in &spans {
        // "… [P1] [P2]": the second citation closes the same sentence as the first, which is not part of it.
        let mut to = range.start;
        while let Some((before, _)) = spans.iter().find(|(other, _)| other.end <= to && body[other.end..to].trim().is_empty() && other.start < to) { to = before.start }
        let from = ends.iter().copied().filter(|end| *end <= to && says(*end, to)).max().unwrap_or(0);
        // A list mark or a heading's hashes are not part of what is claimed.
        let written = body[from..to].trim().trim_start_matches(['-', '*', '>', '#', ' ']).trim();
        let claim = if written.is_empty() { last_claim.clone() } else { written.to_string() };
        if claim.is_empty() { continue }
        last_claim = claim.clone();
        for citation in cited {
            let mut named = sources.iter().filter(|source| source.label == citation.label);
            let (Some(source), None) = (named.next(), named.next()) else { continue };
            // 原文为：“…” [P3]: a sentence quoted before the citation is its quotation, as one inside the brackets is.
            let quote = if citation.quote.is_empty() { quoted_last(&claim) } else { citation.quote.clone() };
            let row = EvidenceRow {
                section: section.to_string(), claim: claim.clone(), paper_id: source.paper_id.clone(), paper_title: source.title.clone(),
                page: citation.page, quote,
                source: if citation.page.is_some() { EvidenceSource::FullText } else { EvidenceSource::Unknown },
                ..Default::default()
            };
            if !rows.iter().any(|known| known.same_evidence(&row)) { rows.push(row) }
        }
    }
    rows
}

/// The quotation a claim ends with, when it is long enough to be a sentence of a source and not a term in quotation marks.
fn quoted_last(claim: &str) -> String {
    let text = claim.trim_end_matches(['。', '.', '，', ',', '；', ';', ' ']);
    let Some(closing) = text.chars().next_back().filter(|mark| ['"', '”', '」'].contains(mark)) else { return String::new() };
    let opening = match closing { '”' => '“', '」' => '「', _ => '"' };
    let inner = &text[..text.len() - closing.len_utf8()];
    let Some(at) = inner.rfind(opening) else { return String::new() };
    let quoted = inner[at + opening.len_utf8()..].trim();
    if quoted.chars().count() >= 20 { quoted.to_string() } else { String::new() }
}

impl Review {
    fn apply(&mut self, action: ReviewAction) -> Result<()> {
        match action {
            ReviewAction::SetRequirements(text) => {
                nonempty(&text, "写作要求")?;
                if self.requirements != text {
                    self.requirements = text;
                    self.requirements_confirmed = false;
                    self.outline_confirmed = false;
                    self.checks_current = false;
                    self.completed = false;
                }
            }
            ReviewAction::ConfirmRequirements => {
                nonempty(&self.requirements, "写作要求")?;
                self.requirements_confirmed = true;
            }
            ReviewAction::SetOutline(text) => {
                if !self.requirements_confirmed {
                    bail!("请先确认写作要求")
                }
                nonempty(&text, "大纲")?;
                if self.outline != text {
                    self.outline = text;
                    self.outline_confirmed = false;
                    self.checks_current = false;
                    self.completed = false;
                }
            }
            ReviewAction::ConfirmOutline => {
                if !self.requirements_confirmed {
                    bail!("请先确认写作要求")
                }
                nonempty(&self.outline, "大纲")?;
                self.outline_confirmed = true;
            }
            ReviewAction::SetEvidence(text) => {
                if !self.requirements_confirmed {
                    bail!("请先确认写作要求")
                }
                if self.evidence != text {
                    self.evidence = text;
                    self.checks_current = false;
                    self.completed = false;
                }
            }
            ReviewAction::SaveEvidence(mut row) => {
                nonempty(&row.claim, "论断")?;
                nonempty(&row.paper_id, "来源文献")?;
                if row.page == Some(0) { bail!("页码必须从 1 开始") }
                if !row.section.is_empty() {
                    let section = self.sections.iter().find(|section| section.name == row.section).ok_or_else(|| anyhow::anyhow!("找不到关联的章节"))?;
                    if !section.body.contains(row.claim.trim()) { bail!("论断不在所选章节中，请选择正文原文或取消章节关联") }
                }
                if row.id.is_empty() {
                    row.id = new_id("E");
                    row.section_detached = false;
                    row.unjudge();
                    self.evidence_rows.push(row);
                } else {
                    let old = self.evidence_rows.iter_mut().find(|old| old.id == row.id).ok_or_else(|| anyhow::anyhow!("证据记录不存在"))?;
                    // The saved assessment cannot be changed by editing the form.
                    row.judgment = old.judgment;
                    row.assessed_fingerprint = old.assessed_fingerprint.clone();
                    row.source_current = old.source_current;
                    // A note is the person's own remark; what the judgment was about is the rest.
                    let changed = old.section_detached || !old.same_evidence(&row);
                    row.section_detached = false;
                    if changed { row.unjudge() }
                    *old = row;
                }
                self.checks_current = false;
                self.completed = false;
            }
            ReviewAction::JudgeEvidence { id, judgment } => {
                let at = self.evidence_rows.iter().position(|row| row.id == id).ok_or_else(|| anyhow::anyhow!("证据记录不存在"))?;
                if judgment != EvidenceJudgment::Unreviewed && !self.linked(&self.evidence_rows[at]) { bail!("论断已不在原章节中，请重新关联") }
                self.evidence_rows[at].judgment = judgment;
                self.checks_current = false;
                self.completed = false;
            }
            ReviewAction::RemoveEvidence { id } => {
                let position = self.evidence_rows.iter().position(|row| row.id == id).ok_or_else(|| anyhow::anyhow!("证据记录不存在"))?;
                self.evidence_rows.remove(position);
                self.checks_current = false;
                self.completed = false;
            }
            ReviewAction::SaveSection { name, body, cited } => {
                if !self.outline_confirmed {
                    bail!("请先确认大纲")
                }
                nonempty(&name, "章节名")?;
                nonempty(&body, "章节正文")?;
                let name = name.trim().to_string();
                // A judgment was about a claim as written: it goes when the sentence does, and stays through a change elsewhere in the section.
                for row in self.evidence_rows.iter_mut().filter(|row| row.section == name && !row.section_detached && !body.contains(row.claim.trim())) { row.unjudge() }
                if let Some(section) = self.sections.iter_mut().find(|section| section.name == name) {
                    section.body = body;
                } else {
                    self.sections.push(ReviewSection { name: name.clone(), body });
                }
                for mut row in cited {
                    row.section = name.clone();
                    if row.claim.trim().is_empty() || row.paper_id.is_empty() || self.evidence_rows.iter().any(|known| known.same_evidence(&row)) { continue }
                    row.id = new_id("E");
                    row.section_detached = false;
                    row.unjudge();
                    self.evidence_rows.push(row);
                }
                self.checks_current = false;
                self.completed = false;
            }
            ReviewAction::RemoveSection { name } => {
                let Some(position) = self.sections.iter().position(|section| section.name == name) else {
                    bail!("找不到该章节")
                };
                self.sections.remove(position);
                // What the citations of the removed text gave goes with it, unless the person has judged or annotated it: such a row stays, cut loose.
                self.evidence_rows.retain(|row| row.section != name || row.judgment != EvidenceJudgment::Unreviewed || !row.note.is_empty());
                for row in self.evidence_rows.iter_mut().filter(|row| row.section == name) { row.unjudge(); row.section_detached = true }
                self.checks_current = false;
                self.completed = false;
            }
            ReviewAction::SetChecks(text) => {
                if !self.outline_confirmed || self.sections.is_empty() {
                    bail!("请先确认大纲并保存正文")
                }
                nonempty(&text, "检查记录")?;
                self.checks = text;
                self.checks_current = true;
                self.completed = false;
            }
            ReviewAction::Complete => {
                if !self.requirements_confirmed
                    || !self.outline_confirmed
                    || self.sections.is_empty()
                    || !self.checks_current
                {
                    bail!("请先确认要求和大纲、保存正文并记录当前检查结果")
                }
                self.completed = true;
            }
        }
        Ok(())
    }
}

impl Store {
    /// Look up an active library paper. A trashed or permanently removed paper
    /// remains named in the evidence snapshot, but is not an available source.
    fn evidence_paper_title(&self, id: &str) -> Result<Option<String>> {
        Ok(self.db.query_row("SELECT title FROM papers WHERE id = ?1 AND id NOT IN (SELECT paper_id FROM trashed_papers)", [id], |row| row.get(0)).optional()?)
    }

    /// What a judgment about a paper was made on: its title and abstract, and
    /// the text of the page named, if one is. `None` for a paper that is gone
    /// or in the trash. The same words give the same fingerprint, so a record
    /// changed and changed back leaves a judgment standing, and a tag, a note
    /// or another page changes nothing.
    fn evidence_fingerprint(&self, id: &str, page: Option<usize>, texts: &mut HashMap<String, Option<Vec<String>>>) -> Result<Option<String>> {
        let record: Option<(String, String)> = self.db.query_row("SELECT title, abstract FROM papers WHERE id = ?1 AND id NOT IN (SELECT paper_id FROM trashed_papers)", [id], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
        let Some((title, abstract_text)) = record else { return Ok(None) };
        let on_page = match page {
            Some(page) => {
                if !texts.contains_key(id) { texts.insert(id.to_string(), self.paper_pages(id)?); }
                texts[id].as_ref().and_then(|pages| pages.get(page.checked_sub(1)?)).cloned().unwrap_or_default()
            }
            None => String::new(),
        };
        // JSON frames each field, so no two different records read alike.
        let hash = Sha256::digest(serde_json::to_vec(&(title, abstract_text, on_page))?);
        Ok(Some(hash.iter().map(|byte| format!("{byte:02x}")).collect()))
    }

    /// Fill in what is worked out on reading: whether each source is there, and whether its judgment is about what it says now.
    fn evidence_availability(&self, mut review: Review) -> Result<Review> {
        let mut texts = HashMap::new();
        let unlinked: Vec<bool> = review.evidence_rows.iter().map(|row| !review.linked(row)).collect();
        for (row, unlinked) in review.evidence_rows.iter_mut().zip(unlinked) {
            let fingerprint = self.evidence_fingerprint(&row.paper_id, row.page, &mut texts)?;
            row.source_available = fingerprint.is_some();
            row.source_current = row.judgment != EvidenceJudgment::Unreviewed && !row.assessed_fingerprint.is_empty() && fingerprint.as_ref() == Some(&row.assessed_fingerprint);
            if unlinked || !row.source_available || (row.judgment != EvidenceJudgment::Unreviewed && !row.source_current) {
                review.checks_current = false;
                review.completed = false;
            }
        }
        Ok(review)
    }

    /// Create and attach a review in one transaction, so a failed link cannot
    /// leave an inaccessible review behind.
    pub fn create_review_for_conversation(&mut self, title: &str, conversation_id: &str) -> Result<Review> {
        nonempty(title, "综述题目")?;
        nonempty(conversation_id, "对话 ID")?;
        let review = Review { id: new_id("R"), title: title.trim().to_string(), updated_at: now_iso(), ..Default::default() };
        let tx = self.db.transaction()?;
        tx.execute("INSERT INTO review_projects(id, revision, body, updated_at) VALUES (?1, ?2, ?3, ?4)", params![review.id, review.revision, serde_json::to_string(&review)?, review.updated_at])?;
        tx.execute("INSERT INTO review_conversations(conversation_id, review_id) VALUES (?1, ?2)", params![conversation_id, review.id])?;
        tx.commit()?;
        Ok(review)
    }

    pub fn review(&self, id: &str) -> Result<Option<Review>> {
        let body: Option<String> = self.db.query_row("SELECT body FROM review_projects WHERE id = ?1", [id], |r| r.get(0)).optional()?;
        body.map(|body| self.evidence_availability(serde_json::from_str(&body)?)).transpose()
    }

    pub fn review_for_conversation(&self, conversation_id: &str) -> Result<Option<Review>> {
        let body: Option<String> = self.db.query_row(
            "SELECT p.body FROM review_projects p JOIN review_conversations c ON c.review_id = p.id WHERE c.conversation_id = ?1",
            [conversation_id], |r| r.get(0),
        ).optional()?;
        body.map(|body| self.evidence_availability(serde_json::from_str(&body)?)).transpose()
    }

    /// Reject a stale editor rather than silently overwriting another window's work.
    pub fn apply_review(&mut self, id: &str, expected_revision: i64, mut action: ReviewAction) -> Result<Review> {
        let mut review = self.review(id)?.ok_or_else(|| anyhow::anyhow!("综述不存在"))?;
        if review.revision != expected_revision {
            bail!("综述已在别处修改，请重新载入")
        }
        match &mut action {
            ReviewAction::SaveEvidence(row) => {
                let known = review.evidence_rows.iter().find(|old| old.id == row.id && old.paper_id == row.paper_id);
                match (self.evidence_paper_title(&row.paper_id)?, known) {
                    // The title as it was when the row was made stays with it.
                    (Some(title), known) => row.paper_title = known.filter(|old| !old.paper_title.is_empty()).map_or(title, |old| old.paper_title.clone()),
                    (None, Some(old)) => row.paper_title = old.paper_title.clone(),
                    (None, None) => bail!("来源文献不在文献库中"),
                }
            }
            ReviewAction::SaveSection { cited, .. } => {
                // A citation of a paper that has left the library gives no row.
                let mut kept = vec![];
                for mut row in std::mem::take(cited) {
                    if let Some(title) = self.evidence_paper_title(&row.paper_id)? { row.paper_title = title; kept.push(row) }
                }
                *cited = kept;
            }
            ReviewAction::JudgeEvidence { id, judgment } => {
                let row = review.evidence_rows.iter_mut().find(|row| row.id == *id).ok_or_else(|| anyhow::anyhow!("证据记录不存在"))?;
                if *judgment == EvidenceJudgment::Unreviewed {
                    row.assessed_fingerprint.clear();
                } else {
                    row.assessed_fingerprint = self.evidence_fingerprint(&row.paper_id, row.page, &mut HashMap::new())?.ok_or_else(|| anyhow::anyhow!("来源文献不可用，请先恢复后再判断"))?;
                }
            }
            _ => {}
        }
        review.apply(action)?;
        review.revision += 1;
        review.updated_at = now_iso();
        let changed = self.db.execute("UPDATE review_projects SET revision = ?1, body = ?2, updated_at = ?3 WHERE id = ?4 AND revision = ?5", params![review.revision, serde_json::to_string(&review)?, review.updated_at, id, expected_revision])?;
        if changed != 1 {
            bail!("综述已在别处修改，请重新载入")
        }
        self.evidence_availability(review)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::path::Path;

    fn action(store: &mut Store, r: &Review, a: ReviewAction) -> Review {
        store.apply_review(&r.id, r.revision, a).unwrap()
    }

    fn section(name: &str, body: &str) -> ReviewAction {
        ReviewAction::SaveSection { name: name.into(), body: body.into(), cited: vec![] }
    }

    /// A review of a conversation made for it, as the window makes one.
    fn review(store: &mut Store, title: &str) -> Review {
        let mut conversation = crate::store::Conversation::default();
        store.save_conversation(&mut conversation).unwrap();
        store.create_review_for_conversation(title, &conversation.id).unwrap()
    }

    /// A review with its requirements and outline confirmed.
    fn writing(store: &mut Store) -> Review {
        let r = review(store, "Review");
        let r = action(store, &r, ReviewAction::SetRequirements("Requirements".into()));
        let r = action(store, &r, ReviewAction::ConfirmRequirements);
        let r = action(store, &r, ReviewAction::SetOutline("Outline".into()));
        action(store, &r, ReviewAction::ConfirmOutline)
    }

    #[test]
    fn citations_give_the_evidence_rows() {
        let sources = vec![
            crate::ai::Source { label: "P1".into(), paper_id: "one".into(), title: "One".into() },
            crate::ai::Source { label: "P2".into(), paper_id: "two".into(), title: "Two".into() },
        ];
        let body = "## 机制\n\n根系靠乙烯感知紧实 [P1 p.5 \"Ethylene accumulates. It then signals\"]。另一组没有看到这一点 [P2]，也有人认为相反 [P1 p.7] [P2 p.2]。\n- A compact soil slows growth [P1, P9]. Unsupported sentence.";
        let rows = cited_evidence("机制", body, &sources);
        let told: Vec<(&str, &str, Option<usize>, &str)> = rows.iter().map(|row| (row.claim.as_str(), row.paper_id.as_str(), row.page, row.quote.as_str())).collect();
        assert_eq!(told, vec![
            // The full stop inside the quotation does not end the sentence.
            ("根系靠乙烯感知紧实", "one", Some(5), "Ethylene accumulates. It then signals"),
            ("另一组没有看到这一点", "two", None, ""),
            ("另一组没有看到这一点 [P2]，也有人认为相反", "one", Some(7), ""),
            // A citation right after another closes the same sentence.
            ("另一组没有看到这一点 [P2]，也有人认为相反", "two", Some(2), ""),
            // The list mark is not part of the claim; the label nobody knows gives no row.
            ("A compact soil slows growth", "one", None, ""),
        ]);
        assert!(rows.iter().all(|row| row.section == "机制" && body.contains(&row.claim)), "a claim is the text as written, so it can be found again");
        assert_eq!(rows[0].source, EvidenceSource::FullText);
        assert_eq!(rows[1].source, EvidenceSource::Unknown);
        // As models also write it: the citation after the full stop, and the sentence quoted before its citation.
        let body = "循环网络学习长期信息很慢。[P1] 原文为：“Learning to store information over extended time intervals takes a very long time.” [P1] 它被称为“注意力”[P2]。";
        let rows = cited_evidence("A", body, &sources);
        let told: Vec<(&str, &str, &str)> = rows.iter().map(|row| (row.claim.as_str(), row.paper_id.as_str(), row.quote.as_str())).collect();
        assert_eq!(told, vec![
            ("循环网络学习长期信息很慢。", "one", ""),
            ("原文为：“Learning to store information over extended time intervals takes a very long time.”", "one", "Learning to store information over extended time intervals takes a very long time."),
            ("它被称为“注意力”", "two", ""),
        ]);
        assert!(rows.iter().all(|row| body.contains(&row.claim)));
        // A label that names two sources names none.
        let twice = [sources.clone(), vec![crate::ai::Source { label: "P1".into(), paper_id: "three".into(), title: "Three".into() }]].concat();
        assert!(cited_evidence("A", "Claim [P1].", &twice).is_empty());
    }

    #[test]
    fn a_saved_section_brings_its_evidence_once() {
        let mut s = Store::in_memory(Path::new("/tmp/review-cited-test")).unwrap();
        let paper = s.add_paper(crate::model::Paper { title: "Roots".into(), ..Default::default() }).unwrap();
        let sources = vec![
            crate::ai::Source { label: "P1".into(), paper_id: paper.id.clone(), title: "as the chat called it".into() },
            crate::ai::Source { label: "P2".into(), paper_id: "gone".into(), title: "Gone".into() },
        ];
        let body = "Roots sense compaction [P1 p.2]. Leaves do not [P2].";
        let r = writing(&mut s);
        let r = action(&mut s, &r, ReviewAction::SaveSection { name: "A".into(), body: body.into(), cited: cited_evidence("A", body, &sources) });
        assert_eq!(r.evidence_rows.len(), 1, "a paper that is not in the library gives no row");
        let row = r.evidence_rows[0].clone();
        assert_eq!((row.claim.as_str(), row.paper_title.as_str(), row.page, row.judgment), ("Roots sense compaction", "Roots", Some(2), EvidenceJudgment::Unreviewed));
        assert!(row.source_available && r.linked(&row));
        let r = action(&mut s, &r, ReviewAction::JudgeEvidence { id: row.id.clone(), judgment: EvidenceJudgment::Supports });
        // The section saved again with a sentence added: the row is not doubled and its judgment stands.
        let longer = format!("{body} Stems were not studied [P1].");
        let r = action(&mut s, &r, ReviewAction::SaveSection { name: "A".into(), body: longer.clone(), cited: cited_evidence("A", &longer, &sources) });
        assert_eq!(r.evidence_rows.len(), 2);
        assert_eq!(r.evidence_rows[0].judgment, EvidenceJudgment::Supports);
        assert!(r.evidence_rows[0].source_current);
        // The sentence rewritten: the old judgment was about other words.
        let rewritten = "Roots may sense compaction [P1 p.2].";
        let r = action(&mut s, &r, ReviewAction::SaveSection { name: "A".into(), body: rewritten.into(), cited: cited_evidence("A", rewritten, &sources) });
        assert_eq!(r.evidence_rows[0].judgment, EvidenceJudgment::Unreviewed);
        assert!(!r.linked(&r.evidence_rows[0]) && r.linked(r.evidence_rows.last().unwrap()));
        // Removing the section takes the rows nobody judged or annotated.
        let judged = r.evidence_rows.last().unwrap().id.clone();
        let r = action(&mut s, &r, ReviewAction::JudgeEvidence { id: judged.clone(), judgment: EvidenceJudgment::Partial });
        let r = action(&mut s, &r, ReviewAction::RemoveSection { name: "A".into() });
        assert_eq!(r.evidence_rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), vec![judged.as_str()]);
        assert!(r.evidence_rows[0].section_detached && r.evidence_rows[0].judgment == EvidenceJudgment::Unreviewed);
    }

    #[test]
    fn evidence_is_explicit_and_invalidated_by_content_changes() {
        let mut s = Store::in_memory(Path::new("/tmp/review-evidence-test")).unwrap();
        let paper = s.add_paper(crate::model::Paper { title: "A paper".into(), ..Default::default() }).unwrap();
        let r = writing(&mut s);
        let r = action(&mut s, &r, section("A", "The claim is limited to cells."));
        let mut row = EvidenceRow { section: "A".into(), claim: "The claim is limited to cells.".into(), paper_id: paper.id.clone(), paper_title: "fake title".into(), page: Some(3), quote: "Cells were tested".into(), source: EvidenceSource::Abstract, judgment: EvidenceJudgment::Supports, ..Default::default() };
        let mut invalid = row.clone();
        invalid.page = Some(0);
        assert!(s.apply_review(&r.id, r.revision, ReviewAction::SaveEvidence(invalid)).is_err());
        let mut invalid = row.clone();
        invalid.claim = "Not in the section".into();
        assert!(s.apply_review(&r.id, r.revision, ReviewAction::SaveEvidence(invalid)).is_err());
        let mut invalid = row.clone();
        invalid.paper_id = "missing".into();
        assert!(s.apply_review(&r.id, r.revision, ReviewAction::SaveEvidence(invalid)).is_err());
        let r = action(&mut s, &r, ReviewAction::SaveEvidence(row));
        row = r.evidence_rows[0].clone();
        assert_eq!(row.paper_title, "A paper");
        assert_eq!(row.judgment, EvidenceJudgment::Unreviewed, "editing cannot assign a verdict");
        assert!(row.source_available);
        let r = action(&mut s, &r, ReviewAction::JudgeEvidence { id: row.id.clone(), judgment: EvidenceJudgment::Partial });
        assert_eq!(r.evidence_rows[0].judgment, EvidenceJudgment::Partial);
        // The requirements worded anew ask for the confirmations again, and leave what a source says about a claim alone.
        let r = action(&mut s, &r, ReviewAction::SetRequirements("Requirements, reworded".into()));
        assert!(!r.requirements_confirmed && !r.outline_confirmed);
        assert_eq!(r.evidence_rows[0].judgment, EvidenceJudgment::Partial);
        let r = action(&mut s, &r, ReviewAction::ConfirmRequirements);
        let r = action(&mut s, &r, ReviewAction::ConfirmOutline);
        let r = action(&mut s, &r, ReviewAction::SetChecks("Manually checked".into()));
        let r = action(&mut s, &r, ReviewAction::Complete);
        // A note is the person's remark: the judgment stands. Another quotation is other evidence.
        row = r.evidence_rows[0].clone();
        row.note = "Only in cell culture".into();
        row.judgment = EvidenceJudgment::Supports;
        let noted = action(&mut s, &r, ReviewAction::SaveEvidence(row.clone()));
        assert_eq!(noted.evidence_rows[0].judgment, EvidenceJudgment::Partial);
        assert!(!noted.checks_current && !noted.completed);
        row.quote = "Other words".into();
        let changed = action(&mut s, &noted, ReviewAction::SaveEvidence(row));
        assert_eq!(changed.evidence_rows[0].judgment, EvidenceJudgment::Unreviewed);
        assert!(s.apply_review(&r.id, r.revision, ReviewAction::RemoveEvidence { id: changed.evidence_rows[0].id.clone() }).is_err(), "stale revision must not overwrite");
        let id = changed.evidence_rows[0].id.clone();
        let changed = action(&mut s, &changed, ReviewAction::JudgeEvidence { id: id.clone(), judgment: EvidenceJudgment::Supports });
        let changed = action(&mut s, &changed, section("A", "The claim was revised."));
        assert_eq!(changed.evidence_rows[0].judgment, EvidenceJudgment::Unreviewed);
        assert!(s.apply_review(&changed.id, changed.revision, ReviewAction::JudgeEvidence { id: id.clone(), judgment: EvidenceJudgment::Supports }).is_err(), "a claim that left its section cannot be judged");
        let changed = action(&mut s, &changed, ReviewAction::JudgeEvidence { id: id.clone(), judgment: EvidenceJudgment::Unreviewed });
        let changed = action(&mut s, &changed, section("A", "The claim is limited to cells."));
        let changed = action(&mut s, &changed, ReviewAction::JudgeEvidence { id: id.clone(), judgment: EvidenceJudgment::Supports });
        let changed = action(&mut s, &changed, ReviewAction::RemoveSection { name: "A".into() });
        assert_eq!(changed.evidence_rows[0].section, "A", "a judged row outlives its section");
        assert!(changed.evidence_rows[0].section_detached);
        let changed = action(&mut s, &changed, section("A", "The claim is limited to cells."));
        assert!(changed.evidence_rows[0].section_detached, "same-named recreated section must not silently reattach");
        assert!(s.apply_review(&changed.id, changed.revision, ReviewAction::JudgeEvidence { id: id.clone(), judgment: EvidenceJudgment::Supports }).is_err());
        let row = changed.evidence_rows[0].clone();
        let changed = action(&mut s, &changed, ReviewAction::SaveEvidence(row));
        assert!(!changed.evidence_rows[0].section_detached, "explicit edit can reattach");
        let changed = action(&mut s, &changed, ReviewAction::JudgeEvidence { id: id.clone(), judgment: EvidenceJudgment::Supports });
        let changed = action(&mut s, &changed, ReviewAction::RemoveEvidence { id });
        assert!(changed.evidence_rows.is_empty());
    }

    #[test]
    fn evidence_survives_restart_and_marks_unavailable_sources() {
        let root = std::env::temp_dir().join(new_id("wisp-evidence"));
        let (review_id, paper_id, row_id) = {
            let mut s = Store::open(&root).unwrap();
            let paper = s.add_paper(crate::model::Paper { title: "Original title".into(), ..Default::default() }).unwrap();
            let r = review(&mut s, "Review");
            let row = EvidenceRow { claim: "Working hypothesis".into(), paper_id: paper.id.clone(), source: EvidenceSource::FullText, page: Some(1), ..Default::default() };
            let r = action(&mut s, &r, ReviewAction::SaveEvidence(row));
            let row_id = r.evidence_rows[0].id.clone();
            let r = action(&mut s, &r, ReviewAction::JudgeEvidence { id: row_id.clone(), judgment: EvidenceJudgment::Insufficient });
            (r.id, paper.id, row_id)
        };
        {
            let mut s = Store::open(&root).unwrap();
            let r = s.review(&review_id).unwrap().unwrap();
            assert_eq!(r.evidence_rows[0].paper_title, "Original title");
            assert_eq!(r.evidence_rows[0].judgment, EvidenceJudgment::Insufficient);
            assert!(r.evidence_rows[0].source_available && r.evidence_rows[0].source_current);
            s.trash_paper(&paper_id).unwrap();
            let r = s.review(&review_id).unwrap().unwrap();
            assert!(!r.evidence_rows[0].source_available && !r.evidence_rows[0].source_current);
            assert!(s.apply_review(&r.id, r.revision, ReviewAction::JudgeEvidence { id: row_id.clone(), judgment: EvidenceJudgment::Supports }).is_err());
            // Back from the trash with the same words, it is what was judged.
            s.restore_paper(&paper_id).unwrap();
            let restored = s.review(&review_id).unwrap().unwrap();
            assert!(restored.evidence_rows[0].source_available && restored.evidence_rows[0].source_current);
            s.trash_paper(&paper_id).unwrap();
            s.delete_paper(&paper_id).unwrap();
            let r = s.review(&review_id).unwrap().unwrap();
            assert!(!r.evidence_rows[0].source_available);
            assert_eq!(r.evidence_rows[0].paper_title, "Original title");
            let r = action(&mut s, &r, ReviewAction::JudgeEvidence { id: row_id, judgment: EvidenceJudgment::Unreviewed });
            assert_eq!(r.evidence_rows[0].judgment, EvidenceJudgment::Unreviewed);
        }
        // A review from before the evidence rows reads as one without any.
        let old: Review = serde_json::from_str(r#"{"title":"Old review","evidence":"notes"}"#).unwrap();
        assert_eq!(old.evidence, "notes");
        assert!(old.evidence_rows.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_judgment_is_about_what_the_source_said() {
        let root = std::env::temp_dir().join(new_id("wisp-evidence-source"));
        std::fs::create_dir_all(&root).unwrap();
        let external = root.join("source.pdf");
        std::fs::write(&external, b"test pdf").unwrap();
        let mut s = Store::open(&root).unwrap();
        let paper = s.add_paper(crate::model::Paper { title: "A paper".into(), abstract_text: "Old abstract".into(), ..Default::default() }).unwrap();
        let file = s.import_file(&paper.id, &external, "fulltext").unwrap();
        s.save_fulltext_pages(&file.id, &Ok(vec!["First page".into(), "Second page".into()])).unwrap();
        let r = writing(&mut s);
        let r = action(&mut s, &r, section("A", "A claim. Another claim."));
        let r = action(&mut s, &r, ReviewAction::SaveEvidence(EvidenceRow { section: "A".into(), claim: "A claim".into(), paper_id: paper.id.clone(), ..Default::default() }));
        let r = action(&mut s, &r, ReviewAction::SaveEvidence(EvidenceRow { section: "A".into(), claim: "Another claim".into(), paper_id: paper.id.clone(), page: Some(2), ..Default::default() }));
        let (by_record, by_page) = (r.evidence_rows[0].id.clone(), r.evidence_rows[1].id.clone());
        let r = action(&mut s, &r, ReviewAction::JudgeEvidence { id: by_record.clone(), judgment: EvidenceJudgment::Supports });
        let r = action(&mut s, &r, ReviewAction::JudgeEvidence { id: by_page.clone(), judgment: EvidenceJudgment::Supports });
        let r = action(&mut s, &r, ReviewAction::SetChecks("Checked".into()));
        let r = action(&mut s, &r, ReviewAction::Complete);
        let current = |s: &Store| { let r = s.review(&r.id).unwrap().unwrap(); (r.evidence_rows[0].source_current, r.evidence_rows[1].source_current, r.completed) };
        // A tag is not what the judgment was about.
        let mut edited = s.get_paper(&paper.id).unwrap();
        edited.tags.push("A tag".into());
        s.save_paper(&mut edited).unwrap();
        assert_eq!(current(&s), (true, true, true));
        // Another abstract: both judgments read it.
        edited.abstract_text = "New abstract".into();
        s.save_paper(&mut edited).unwrap();
        assert_eq!(current(&s), (false, false, false));
        // What is worked out on reading is not written back.
        let raw: String = s.db.query_row("SELECT body FROM review_projects WHERE id = ?1", [&r.id], |r| r.get(0)).unwrap();
        assert!(serde_json::from_str::<Review>(&raw).unwrap().completed);
        // The same words again are the same source.
        edited.abstract_text = "Old abstract".into();
        s.save_paper(&mut edited).unwrap();
        assert_eq!(current(&s), (true, true, true));
        // The text read anew: a change on the page named touches the judgment made on that page, and no other.
        s.save_fulltext_pages(&file.id, &Ok(vec!["First page, read better".into(), "Second page".into()])).unwrap();
        assert_eq!(current(&s), (true, true, true));
        s.save_fulltext_pages(&file.id, &Ok(vec!["First page".into(), "Second page, read better".into()])).unwrap();
        assert_eq!(current(&s), (true, false, false));
        let stale = s.review(&r.id).unwrap().unwrap();
        let again = action(&mut s, &stale, ReviewAction::JudgeEvidence { id: by_page, judgment: EvidenceJudgment::Partial });
        assert!(again.evidence_rows[1].source_current && !again.checks_current);
        // A judgment kept without a fingerprint (by a build between releases) is not known to be current.
        let mut legacy = again.clone();
        legacy.evidence_rows[0].assessed_fingerprint.clear();
        s.db.execute("UPDATE review_projects SET body = ?1 WHERE id = ?2", params![serde_json::to_string(&legacy).unwrap(), legacy.id]).unwrap();
        assert!(!s.review(&r.id).unwrap().unwrap().evidence_rows[0].source_current);
        drop(s);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stages_reconfirm_and_keep_drafts() {
        let mut s = Store::in_memory(Path::new("/tmp/review-test")).unwrap();
        let r = review(&mut s, "Roots");
        assert_eq!(r.step(), 1);
        assert!(s.apply_review(&r.id, r.revision, ReviewAction::ConfirmOutline).is_err());
        assert!(s.apply_review(&r.id, r.revision, section("A", "Draft")).is_err());
        let r = action(&mut s, &r, ReviewAction::SetRequirements("Audience: researchers".into()));
        let r = action(&mut s, &r, ReviewAction::ConfirmRequirements);
        assert_eq!(r.step(), 2);
        let r = action(&mut s, &r, ReviewAction::SetOutline("A. Roots".into()));
        let r = action(&mut s, &r, ReviewAction::ConfirmOutline);
        assert_eq!(r.step(), 3);
        let r = action(&mut s, &r, ReviewAction::SetEvidence("P1: abstract only".into()));
        let r = action(&mut s, &r, section("A", "Draft [P1]"));
        assert!(s.apply_review(&r.id, r.revision, ReviewAction::Complete).is_err());
        let r = action(&mut s, &r, ReviewAction::SetChecks("P1 is abstract-only; full text unavailable".into()));
        let r = action(&mut s, &r, ReviewAction::Complete);
        assert!(r.completed);
        let edited = action(&mut s, &r, ReviewAction::SetOutline("A. New outline".into()));
        assert!(!edited.completed && !edited.outline_confirmed && !edited.checks_current);
        assert_eq!(edited.sections[0].body, "Draft [P1]");
        assert!(s.apply_review(&edited.id, edited.revision, section("A", "New")).is_err());
        assert!(s.apply_review(&r.id, r.revision, ReviewAction::ConfirmOutline).is_err(), "stale editor must not overwrite");
        let edited = action(&mut s, &edited, ReviewAction::ConfirmOutline);
        assert!(s.apply_review(&edited.id, edited.revision, ReviewAction::Complete).is_err(), "old checks are stale");
        let edited = action(&mut s, &edited, ReviewAction::SetChecks("Checked".into()));
        let edited = action(&mut s, &edited, ReviewAction::Complete);
        let edited = action(&mut s, &edited, ReviewAction::RemoveSection { name: "A".into() });
        assert!(edited.sections.is_empty() && !edited.checks_current && !edited.completed);
        assert!(s.apply_review(&edited.id, edited.revision, ReviewAction::RemoveSection { name: "missing".into() }).is_err());
    }

    #[test]
    fn record_survives_close_and_reopen_and_goes_with_its_conversation() {
        let root = std::env::temp_dir().join(new_id("wisp-review"));
        let count = |s: &Store| s.db.query_row("SELECT count(*) FROM review_projects", [], |r| r.get::<_, i64>(0)).unwrap();
        let (id, revision, conversation_id) = {
            let mut s = Store::open(&root).unwrap();
            assert!(s.create_review_for_conversation("Missing", "missing").is_err());
            assert_eq!(count(&s), 0, "a failed link leaves no orphan");
            let mut conversation = crate::store::Conversation::default();
            s.save_conversation(&mut conversation).unwrap();
            let r = s.create_review_for_conversation("Review", &conversation.id).unwrap();
            assert!(s.create_review_for_conversation("Duplicate", &conversation.id).is_err());
            assert_eq!(count(&s), 1, "duplicate link rolls back the project");
            let r = action(&mut s, &r, ReviewAction::SetRequirements("English, 3000 words".into()));
            let r = action(&mut s, &r, ReviewAction::ConfirmRequirements);
            let r = action(&mut s, &r, ReviewAction::SetOutline("1. Context".into()));
            (r.id, r.revision, conversation.id)
        };
        {
            let mut s = Store::open(&root).unwrap();
            let r = s.review(&id).unwrap().unwrap();
            assert_eq!(r.revision, revision);
            assert_eq!(r.requirements, "English, 3000 words");
            assert!(r.requirements_confirmed && !r.outline_confirmed);
            assert_eq!(r.outline, "1. Context");
            assert_eq!(s.review_for_conversation(&conversation_id).unwrap(), Some(r));
            // A build that knows nothing of reviews removes the conversation: the next open clears what it left.
            let mut other = crate::store::Conversation::default();
            s.save_conversation(&mut other).unwrap();
            s.create_review_for_conversation("Left behind", &other.id).unwrap();
            s.db.execute("DELETE FROM conversations WHERE id = ?1", [&other.id]).unwrap();
            assert_eq!(count(&s), 2);
        }
        {
            let mut s = Store::open(&root).unwrap();
            assert_eq!(count(&s), 1);
            s.remove_conversation(&conversation_id).unwrap();
            assert!(s.review_for_conversation(&conversation_id).unwrap().is_none());
            assert!(s.review(&id).unwrap().is_none(), "the review goes with its conversation");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_library_touched_between_releases_opens_and_is_tidied() {
        let root = std::env::temp_dir().join(new_id("wisp-review-between"));
        drop(Store::open(&root).unwrap());
        {
            // What a build between 0.5.0 and the next release left: version 2, its tables and a trigger on the papers.
            let db = Connection::open(root.join(crate::store::DB_FILE)).unwrap();
            db.execute_batch("CREATE TABLE review_source_versions (paper_id TEXT PRIMARY KEY NOT NULL, version INTEGER NOT NULL DEFAULT 0);
                CREATE TRIGGER review_version_trashed AFTER INSERT ON trashed_papers BEGIN UPDATE review_source_versions SET version = version + 1 WHERE paper_id = NEW.paper_id; END;
                PRAGMA user_version = 2;").unwrap();
        }
        let mut s = Store::open(&root).unwrap();
        assert_eq!(s.db.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0)).unwrap(), crate::store::SCHEMA_VERSION);
        assert_eq!(s.db.query_row("SELECT count(*) FROM sqlite_master WHERE name LIKE 'review_version%' OR name = 'review_source_versions'", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        let paper = s.add_paper(crate::model::Paper { title: "A paper".into(), ..Default::default() }).unwrap();
        s.trash_paper(&paper.id).unwrap();
        drop(s);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reviewed_sections_and_citations_survive_manuscript_export_and_reopen() {
        use crate::{ai::Source, cite, manuscript, model::Paper};
        let root = std::env::temp_dir().join(new_id("wisp-review-flow"));
        let (review_id, conversation_id, manuscript_id, original_id, original_text, sources) = {
            let mut store = Store::open(&root).unwrap();
            let p1 = store.add_paper(Paper { title: "Roots".into(), authors: "Smith, Alice".into(), year: Some(2020), ..Default::default() }).unwrap();
            let p2 = store.add_paper(Paper { title: "Leaves".into(), authors: "Jones, Bob".into(), year: Some(2021), ..Default::default() }).unwrap();
            let sources = vec![Source { label: "P1".into(), paper_id: p1.id.clone(), title: p1.title.clone() }, Source { label: "P2".into(), paper_id: p2.id.clone(), title: p2.title.clone() }];
            let original = store.create_manuscript("Existing", "en").unwrap();
            let original_text = "The user's previous draft.".to_string();
            store.save_section_body(&original.sections[0].id, &original_text).unwrap();
            let mut conversation = crate::store::Conversation::default();
            store.save_conversation(&mut conversation).unwrap();
            let r = store.create_review_for_conversation("A review", &conversation.id).unwrap();
            let r = action(&mut store, &r, ReviewAction::SetRequirements("English; researchers".into()));
            let r = action(&mut store, &r, ReviewAction::ConfirmRequirements);
            let r = action(&mut store, &r, ReviewAction::SetOutline("1. Roots; 2. Leaves".into()));
            let r = action(&mut store, &r, ReviewAction::ConfirmOutline);
            let r = action(&mut store, &r, ReviewAction::SetEvidence("P1 full text; P2 abstract only".into()));
            let roots = "Root finding [P1 p.5 \"Roots were found.\"].";
            let r = action(&mut store, &r, ReviewAction::SaveSection { name: "Roots".into(), body: roots.into(), cited: cited_evidence("Roots", roots, &sources) });
            let r = action(&mut store, &r, section("Leaves", "Abstract-only finding [P2]."));
            assert_eq!(r.evidence_rows[0].quote, "Roots were found.", "the quotation stays in the record");
            let r = action(&mut store, &r, ReviewAction::SetChecks("P1 page checked; P2 full text unavailable".into()));
            let r = action(&mut store, &r, ReviewAction::Complete);

            let papers = vec![p1, p2];
            let kept = store.cite_keys().unwrap();
            let index = cite::Index::new(&papers, [], &kept);
            let mut keys = Vec::new();
            let sections: Vec<(String, String)> = r.sections.iter().map(|section| {
                let converted = cite::convert_ai_citations(&section.body, &sources, &index);
                keys.extend(converted.keys);
                (section.name.clone(), converted.text)
            }).collect();
            let manuscript = store.create_manuscript_from_sections(&r.title, "en", &sections, &keys).unwrap();
            (r.id, conversation.id, manuscript.id, original.id, original_text, sources)
        };
        {
            let mut store = Store::open(&root).unwrap();
            let draft = store.manuscript(&manuscript_id).unwrap().unwrap();
            assert_eq!(draft.sections.len(), 2);
            let exported = manuscript::markdown(&draft);
            assert!(exported.contains("Root finding [@Smith2020, p. 5].") && exported.contains("[@Jones2021]"), "{exported}");
            let original = store.manuscript(&original_id).unwrap().unwrap();
            assert_eq!(original.sections[0].body, original_text);
            let papers = store.list_papers().unwrap();
            let kept = store.cite_keys().unwrap();
            assert_eq!(kept.get("Smith2020"), Some(&sources[0].paper_id));
            assert_eq!(kept.get("Jones2021"), Some(&sources[1].paper_id));
            let index = cite::Index::new(&papers, [], &kept);
            let cited = cite::find(&exported, &index);
            assert_eq!(cited.iter().filter(|item| item.found.paper().is_some()).count(), 2);
            let bib = cite::bibtex(&cite::references(&cited, &index), &index);
            assert!(bib.contains("Smith2020") && bib.contains("Jones2021"));
            let r = store.review_for_conversation(&conversation_id).unwrap().unwrap();
            assert_eq!(r.id, review_id);
            assert!(r.completed);
            let r = action(&mut store, &r, ReviewAction::SetOutline("1. Revised".into()));
            assert!(!r.outline_confirmed && !r.checks_current && !r.completed);
            assert_eq!(r.sections.len(), 2, "outline edits must not delete previous work");
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
