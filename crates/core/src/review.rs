//! A review's working record. The record keeps user decisions separately from
//! model output; a UI must explicitly advance the confirmed stages.

use crate::model::new_id;
use crate::store::Store;
use crate::time::now_iso;
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::UNIX_EPOCH;

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
-- A process from an older build may already have a v1 connection open when
-- another process adds evidence. Never allow it to rewrite that evidence away.
CREATE TRIGGER IF NOT EXISTS review_preserve_evidence BEFORE UPDATE OF body ON review_projects
WHEN CASE WHEN json_valid(OLD.body) THEN coalesce(json_array_length(OLD.body, '$.evidence_rows'), 0) > 0 ELSE 0 END
 AND CASE WHEN json_valid(NEW.body) THEN coalesce(json_type(NEW.body, '$.evidence_rows'), '') <> 'array' ELSE 1 END
BEGIN
    SELECT RAISE(ABORT, '此综述包含新版证据记录，请升级后再修改');
END;
-- Until there is an independent recovery UI, neither old nor new clients
-- may orphan non-empty evidence by deleting its conversation or association.
CREATE TRIGGER IF NOT EXISTS review_prevent_orphan BEFORE DELETE ON review_conversations
WHEN EXISTS (
    SELECT 1 FROM review_projects WHERE id = OLD.review_id
      AND CASE WHEN json_valid(body) THEN coalesce(json_array_length(body, '$.evidence_rows'), 0) > 0 ELSE 0 END
)
BEGIN
    SELECT RAISE(ABORT, '此对话关联的综述仍有证据记录，请先删除证据或保留对话');
END;
-- Hash full-text extraction once, not on every chat repaint. Triggers also
-- cover updates performed by other connections and by older app versions.
CREATE TABLE IF NOT EXISTS review_text_digests (
    resource_id TEXT PRIMARY KEY NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    digest TEXT NOT NULL
);
CREATE TRIGGER IF NOT EXISTS review_digest_after_insert AFTER INSERT ON fulltext_text BEGIN
    DELETE FROM review_text_digests WHERE resource_id = NEW.resource_id;
END;
CREATE TRIGGER IF NOT EXISTS review_digest_after_update AFTER UPDATE OF pages, error, extracted_at ON fulltext_text BEGIN
    DELETE FROM review_text_digests WHERE resource_id = NEW.resource_id;
END;
CREATE TRIGGER IF NOT EXISTS review_digest_after_delete AFTER DELETE ON fulltext_text BEGIN
    DELETE FROM review_text_digests WHERE resource_id = OLD.resource_id;
END;
-- Monotonic generations prevent a changed-then-reverted source or a restored
-- paper from silently reviving a judgment. No foreign key: evidence may
-- outlive a permanently removed paper and must retain the old generation.
CREATE TABLE IF NOT EXISTS review_source_versions (
    paper_id TEXT PRIMARY KEY NOT NULL,
    version INTEGER NOT NULL DEFAULT 0
);
CREATE TRIGGER IF NOT EXISTS review_version_paper_update AFTER UPDATE OF title, abstract, authors_json, journal, year, publication_state, url, source ON papers
WHEN OLD.title IS NOT NEW.title OR OLD.abstract IS NOT NEW.abstract OR OLD.authors_json IS NOT NEW.authors_json OR OLD.journal IS NOT NEW.journal OR OLD.year IS NOT NEW.year OR OLD.publication_state IS NOT NEW.publication_state OR OLD.url IS NOT NEW.url OR OLD.source IS NOT NEW.source
BEGIN
    UPDATE review_source_versions SET version = version + 1 WHERE paper_id = NEW.id;
END;
CREATE TRIGGER IF NOT EXISTS review_version_paper_delete AFTER DELETE ON papers BEGIN
    UPDATE review_source_versions SET version = version + 1 WHERE paper_id = OLD.id;
END;
CREATE TRIGGER IF NOT EXISTS review_version_identifier_insert AFTER INSERT ON paper_identifiers BEGIN
    UPDATE review_source_versions SET version = version + 1 WHERE paper_id = NEW.paper_id;
END;
CREATE TRIGGER IF NOT EXISTS review_version_identifier_delete AFTER DELETE ON paper_identifiers BEGIN
    UPDATE review_source_versions SET version = version + 1 WHERE paper_id = OLD.paper_id;
END;
CREATE TRIGGER IF NOT EXISTS review_version_identifier_update AFTER UPDATE ON paper_identifiers BEGIN
    UPDATE review_source_versions SET version = version + 1 WHERE paper_id = OLD.paper_id;
    UPDATE review_source_versions SET version = version + 1 WHERE paper_id = NEW.paper_id;
END;
CREATE TRIGGER IF NOT EXISTS review_version_resource_insert AFTER INSERT ON resources WHEN NEW.resource_type = 'fulltext' BEGIN
    UPDATE review_source_versions SET version = version + 1 WHERE paper_id = NEW.paper_id;
END;
CREATE TRIGGER IF NOT EXISTS review_version_resource_delete AFTER DELETE ON resources WHEN OLD.resource_type = 'fulltext' BEGIN
    UPDATE review_source_versions SET version = version + 1 WHERE paper_id = OLD.paper_id;
END;
CREATE TRIGGER IF NOT EXISTS review_version_resource_update AFTER UPDATE OF paper_id, resource_type, sha256, local_path, size_bytes ON resources WHEN OLD.resource_type = 'fulltext' OR NEW.resource_type = 'fulltext' BEGIN
    UPDATE review_source_versions SET version = version + 1 WHERE paper_id = OLD.paper_id;
    UPDATE review_source_versions SET version = version + 1 WHERE paper_id = NEW.paper_id;
END;
CREATE TRIGGER IF NOT EXISTS review_version_text_insert AFTER INSERT ON fulltext_text BEGIN
    UPDATE review_source_versions SET version = version + 1 WHERE paper_id IN (SELECT paper_id FROM resources WHERE id = NEW.resource_id);
END;
CREATE TRIGGER IF NOT EXISTS review_version_text_update AFTER UPDATE OF pages, error, extracted_at ON fulltext_text BEGIN
    UPDATE review_source_versions SET version = version + 1 WHERE paper_id IN (SELECT paper_id FROM resources WHERE id = NEW.resource_id);
END;
CREATE TRIGGER IF NOT EXISTS review_version_text_delete AFTER DELETE ON fulltext_text BEGIN
    UPDATE review_source_versions SET version = version + 1 WHERE paper_id IN (SELECT paper_id FROM resources WHERE id = OLD.resource_id);
END;
CREATE TRIGGER IF NOT EXISTS review_version_trashed AFTER INSERT ON trashed_papers BEGIN
    UPDATE review_source_versions SET version = version + 1 WHERE paper_id = NEW.paper_id;
END;
CREATE TRIGGER IF NOT EXISTS review_version_restored AFTER DELETE ON trashed_papers BEGIN
    UPDATE review_source_versions SET version = version + 1 WHERE paper_id = OLD.paper_id;
END;
"#;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReviewSection {
    pub name: String,
    pub body: String,
}

/// A description supplied by the person recording the evidence, not proof
/// that the application has retrieved or inspected that source.
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
    /// One-based page as entered by the user; not a verified location.
    pub page: Option<usize>,
    pub quote: String,
    pub source: EvidenceSource,
    pub note: String,
    pub judgment: EvidenceJudgment,
    /// Source content as it stood when the user explicitly recorded a judgment.
    pub assessed_fingerprint: String,
    /// Recomputed when loading a review; never inferred from the snapshot.
    #[serde(skip)]
    pub source_available: bool,
    /// At least one registered full-text attachment is missing or unreadable.
    #[serde(skip)]
    pub local_files_missing: bool,
    /// A derived state, not proof that the claim is supported.
    #[serde(skip)]
    pub source_current: bool,
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
    SaveSection { name: String, body: String },
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

/// Recover tracking if a v1 library already contains evidence (for example,
/// after an interrupted or mixed-version upgrade). Missing evidence is not an
/// error; existing source judgments without a fingerprint stay stale.
pub(crate) fn initialize_legacy_watches(db: &Connection) -> Result<bool> {
    let mut stmt = db.prepare("SELECT body FROM review_projects")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut found = false;
    let mut paper_ids = std::collections::BTreeSet::new();
    for body in rows {
        let Ok(body): Result<serde_json::Value, _> = serde_json::from_str(&body?) else { continue };
        if let Some(evidence) = body.get("evidence_rows").and_then(|rows| rows.as_array()) {
            found |= !evidence.is_empty();
            for row in evidence {
                if let Some(paper_id) = row.get("paper_id").and_then(|id| id.as_str()).filter(|id| !id.is_empty()) { paper_ids.insert(paper_id.to_string()); }
            }
        }
    }
    for paper_id in paper_ids {
        // A missing watcher means changes might have occurred unobserved; a
        // nonzero generation prevents a v1/default-zero fingerprint reviving.
        db.execute("INSERT OR IGNORE INTO review_source_versions(paper_id, version) VALUES (?1, 1)", [paper_id])?;
    }
    Ok(found)
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
                    for row in &mut self.evidence_rows { row.judgment = EvidenceJudgment::Unreviewed; row.assessed_fingerprint.clear(); row.source_current = false }
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
                    for row in &mut self.evidence_rows { row.judgment = EvidenceJudgment::Unreviewed; row.assessed_fingerprint.clear(); row.source_current = false }
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
                    row.judgment = EvidenceJudgment::Unreviewed;
                    row.assessed_fingerprint.clear();
                    row.source_current = false;
                    self.evidence_rows.push(row);
                } else {
                    let old = self.evidence_rows.iter_mut().find(|old| old.id == row.id).ok_or_else(|| anyhow::anyhow!("证据记录不存在"))?;
                    // The saved assessment cannot be changed by editing the form.
                    row.judgment = old.judgment;
                    row.assessed_fingerprint = old.assessed_fingerprint.clone();
                    row.source_current = old.source_current;
                    let changed = old.section_detached || old.section != row.section || old.claim != row.claim || old.paper_id != row.paper_id || old.page != row.page || old.quote != row.quote || old.source != row.source || old.note != row.note;
                    row.section_detached = false;
                    if changed { row.judgment = EvidenceJudgment::Unreviewed; row.assessed_fingerprint.clear(); row.source_current = false }
                    *old = row;
                }
                self.checks_current = false;
                self.completed = false;
            }
            ReviewAction::JudgeEvidence { id, judgment } => {
                let row = self.evidence_rows.iter_mut().find(|row| row.id == id).ok_or_else(|| anyhow::anyhow!("证据记录不存在"))?;
                if judgment != EvidenceJudgment::Unreviewed && row.section_detached { bail!("原章节已移除，请重新关联论断") }
                if judgment != EvidenceJudgment::Unreviewed && !row.section.is_empty() {
                    if !self.requirements_confirmed || !self.outline_confirmed { bail!("请先重新确认写作要求和大纲") }
                    let section = self.sections.iter().find(|section| section.name == row.section).ok_or_else(|| anyhow::anyhow!("原章节已移除，请重新关联论断"))?;
                    if !section.body.contains(row.claim.trim()) { bail!("论断已不在原章节中，请重新关联") }
                }
                row.judgment = judgment;
                self.checks_current = false;
                self.completed = false;
            }
            ReviewAction::RemoveEvidence { id } => {
                let position = self.evidence_rows.iter().position(|row| row.id == id).ok_or_else(|| anyhow::anyhow!("证据记录不存在"))?;
                self.evidence_rows.remove(position);
                self.checks_current = false;
                self.completed = false;
            }
            ReviewAction::SaveSection { name, body } => {
                if !self.outline_confirmed {
                    bail!("请先确认大纲")
                }
                nonempty(&name, "章节名")?;
                nonempty(&body, "章节正文")?;
                let name = name.trim().to_string();
                if self.sections.iter().any(|section| section.name == name && section.body != body) {
                    for row in self.evidence_rows.iter_mut().filter(|row| row.section == name) { row.judgment = EvidenceJudgment::Unreviewed; row.assessed_fingerprint.clear(); row.source_current = false }
                }
                if let Some(section) = self
                    .sections
                    .iter_mut()
                    .find(|section| section.name == name)
                {
                    section.body = body;
                } else {
                    self.sections.push(ReviewSection { name, body });
                }
                self.checks_current = false;
                self.completed = false;
            }
            ReviewAction::RemoveSection { name } => {
                let Some(position) = self.sections.iter().position(|section| section.name == name) else {
                    bail!("找不到该章节")
                };
                self.sections.remove(position);
                for row in self.evidence_rows.iter_mut().filter(|row| row.section == name) { row.judgment = EvidenceJudgment::Unreviewed; row.assessed_fingerprint.clear(); row.source_current = false; row.section_detached = true }
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

    /// Old libraries are populated lazily. Full-text table triggers evict a
    /// cached digest for every insert, update or removal, including rollbacks.
    fn evidence_text_digest(&self, resource_id: &str) -> Result<Option<String>> {
        let cached: Option<String> = self.db.query_row("SELECT digest FROM review_text_digests WHERE resource_id = ?1", [resource_id], |row| row.get(0)).optional()?;
        if cached.is_some() { return Ok(cached) }
        let text: Option<(String, String, String)> = self.db.query_row("SELECT pages, error, extracted_at FROM fulltext_text WHERE resource_id = ?1", [resource_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional()?;
        let Some((pages, error, extracted_at)) = text else { return Ok(None) };
        let hash = Sha256::digest(serde_json::to_vec(&(&pages, &error, &extracted_at))?);
        let digest: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
        // Do not install an old digest if another connection changed the
        // extraction while this connection was computing it.
        let saved = self.db.execute("INSERT INTO review_text_digests(resource_id, digest) SELECT ?1, ?2 WHERE EXISTS (SELECT 1 FROM fulltext_text WHERE resource_id = ?1 AND pages = ?3 AND error = ?4 AND extracted_at = ?5) ON CONFLICT(resource_id) DO UPDATE SET digest = excluded.digest", params![resource_id, digest, pages, error, extracted_at])?;
        if saved != 1 { bail!("全文在核验期间发生变化，请重试") }
        Ok(Some(digest))
    }

    /// A conservative snapshot of the paper and its local full-text evidence.
    /// `sha256` is the import-time file hash; filesystem metadata is checked
    /// too, but files are not re-hashed on every UI render.
    fn evidence_fingerprint(&self, id: &str) -> Result<Option<(String, bool)>> {
        let Some(metadata): Option<Vec<String>> = self.db.query_row(
            "SELECT title, abstract, authors_json, journal, coalesce(CAST(year AS TEXT), ''), publication_state, url, source FROM papers WHERE id = ?1 AND id NOT IN (SELECT paper_id FROM trashed_papers)",
            [id], |r| (0..8).map(|i| r.get(i)).collect(),
        ).optional()? else { return Ok(None) };
        let version: i64 = self.db.query_row("SELECT coalesce((SELECT version FROM review_source_versions WHERE paper_id = ?1), 0)", [id], |r| r.get(0))?;
        let mut identifiers = self.db.prepare("SELECT namespace, value FROM paper_identifiers WHERE paper_id = ?1 ORDER BY namespace, value")?;
        let identifiers = identifiers.query_map([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut resources = self.db.prepare("SELECT id, sha256, local_path, size_bytes FROM resources WHERE paper_id = ?1 AND resource_type = 'fulltext' ORDER BY id")?;
        let rows = resources.query_map([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?)))?;
        let mut attachments = Vec::new();
        let mut local_files_missing = false;
        for row in rows {
            let (resource_id, sha, path, size) = row?;
            let text_digest = self.evidence_text_digest(&resource_id)?;
            // Preserve an explicit missing/error marker in the fingerprint:
            // the paper and abstract may still be usable without the file.
            let file = std::fs::File::open(self.root().join(&path)).and_then(|file| file.metadata());
            let file_state = file.ok().filter(|metadata| metadata.is_file()).and_then(|metadata| metadata.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map(|time| (metadata.len(), time.as_nanos())));
            if file_state.is_none() { local_files_missing = true }
            attachments.push((resource_id, sha, path, size, file_state, text_digest));
        }
        // JSON serialization frames every field and preserves ordering, so no
        // two different field sequences collide through concatenation.
        let bytes = serde_json::to_vec(&(version, metadata, identifiers, attachments))?;
        let hash = Sha256::digest(bytes);
        Ok(Some((hash.iter().map(|byte| format!("{byte:02x}")).collect(), local_files_missing)))
    }

    fn evidence_availability(&self, mut review: Review) -> Result<Review> {
        let mut fingerprints: HashMap<String, Option<(String, bool)>> = HashMap::new();
        for row in &mut review.evidence_rows {
            if !fingerprints.contains_key(&row.paper_id) {
                fingerprints.insert(row.paper_id.clone(), self.evidence_fingerprint(&row.paper_id)?);
            }
            let fingerprint = fingerprints.get(&row.paper_id).and_then(Option::as_ref);
            row.source_available = fingerprint.is_some();
            row.local_files_missing = fingerprint.is_some_and(|(_, missing)| *missing);
            row.source_current = !row.section_detached && row.judgment != EvidenceJudgment::Unreviewed && fingerprint.is_some_and(|(current, _)| !row.assessed_fingerprint.is_empty() && &row.assessed_fingerprint == current);
            if row.section_detached || !row.source_available || (row.judgment != EvidenceJudgment::Unreviewed && !row.source_current) {
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

    /// Create a separate review; never modifies an existing manuscript.
    pub fn create_review(&mut self, title: &str) -> Result<Review> {
        nonempty(title, "综述题目")?;
        let review = Review {
            id: new_id("R"),
            title: title.trim().to_string(),
            updated_at: now_iso(),
            ..Default::default()
        };
        self.db.execute(
            "INSERT INTO review_projects(id, revision, body, updated_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                review.id,
                review.revision,
                serde_json::to_string(&review)?,
                review.updated_at
            ],
        )?;
        Ok(review)
    }

    pub fn review(&self, id: &str) -> Result<Option<Review>> {
        let body: Option<String> = self
            .db
            .query_row(
                "SELECT body FROM review_projects WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        body.map(|body| self.evidence_availability(serde_json::from_str(&body)?)).transpose()
    }

    pub fn reviews(&self) -> Result<Vec<Review>> {
        let mut stmt = self
            .db
            .prepare("SELECT body FROM review_projects ORDER BY updated_at DESC, id")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|row| self.evidence_availability(serde_json::from_str(&row?)?)).collect()
    }

    /// Associate an existing conversation and review without replacing an existing link.
    /// Deleting a conversation removes the link, but leaves the review intact.
    pub fn link_review_conversation(
        &mut self,
        review_id: &str,
        conversation_id: &str,
    ) -> Result<()> {
        nonempty(review_id, "综述 ID")?;
        nonempty(conversation_id, "对话 ID")?;
        if let Some(existing) = self.review_for_conversation(conversation_id)? {
            if existing.id == review_id {
                return Ok(());
            }
            bail!("此对话已关联另一份综述")
        }
        self.db.execute(
            "INSERT INTO review_conversations(conversation_id, review_id) VALUES (?1, ?2)",
            params![conversation_id, review_id],
        )?;
        Ok(())
    }

    pub fn review_for_conversation(&self, conversation_id: &str) -> Result<Option<Review>> {
        let body: Option<String> = self.db.query_row(
            "SELECT p.body FROM review_projects p JOIN review_conversations c ON c.review_id = p.id WHERE c.conversation_id = ?1",
            [conversation_id], |r| r.get(0),
        ).optional()?;
        body.map(|body| self.evidence_availability(serde_json::from_str(&body)?)).transpose()
    }

    /// Reject a stale editor rather than silently overwriting another window's work.
    pub fn apply_review(
        &mut self,
        id: &str,
        expected_revision: i64,
        action: ReviewAction,
    ) -> Result<Review> {
        self.apply_review_inner(id, expected_revision, None, action)
    }

    /// For conversation-bound UI actions, confirm the link within the write
    /// transaction. An older process may have removed an empty conversation
    /// after the UI loaded it but before the first evidence row was saved.
    pub fn apply_linked_review(
        &mut self,
        id: &str,
        expected_revision: i64,
        conversation_id: &str,
        action: ReviewAction,
    ) -> Result<Review> {
        nonempty(conversation_id, "对话 ID")?;
        self.apply_review_inner(id, expected_revision, Some(conversation_id), action)
    }

    fn apply_review_inner(
        &mut self,
        id: &str,
        expected_revision: i64,
        conversation_id: Option<&str>,
        mut action: ReviewAction,
    ) -> Result<Review> {
        let mut review = self
            .review(id)?
            .ok_or_else(|| anyhow::anyhow!("综述不存在"))?;
        if review.revision != expected_revision {
            bail!("综述已在别处修改，请重新载入")
        }
        let mut watched_paper = None;
        match &mut action {
            ReviewAction::SaveEvidence(row) => {
                if let Some(title) = self.evidence_paper_title(&row.paper_id)? {
                    row.paper_title = review.evidence_rows.iter().find(|old| old.id == row.id && old.paper_id == row.paper_id && !old.paper_title.is_empty()).map_or(title, |old| old.paper_title.clone());
                    row.source_available = self.evidence_fingerprint(&row.paper_id)?.is_some();
                } else if let Some(old) = review.evidence_rows.iter().find(|old| old.id == row.id && old.paper_id == row.paper_id) {
                    row.paper_title = old.paper_title.clone();
                    row.source_available = false;
                } else { bail!("来源文献不在文献库中") }
                watched_paper = Some(row.paper_id.clone());
            }
            ReviewAction::JudgeEvidence { id, judgment } => {
                let row = review.evidence_rows.iter_mut().find(|row| row.id == *id).ok_or_else(|| anyhow::anyhow!("证据记录不存在"))?;
                if *judgment == EvidenceJudgment::Unreviewed {
                    row.assessed_fingerprint.clear();
                    row.source_current = false;
                } else {
                    row.assessed_fingerprint = self.evidence_fingerprint(&row.paper_id)?.ok_or_else(|| anyhow::anyhow!("来源文献不可用，请先恢复后再核验"))?.0;
                    row.source_current = true;
                }
            }
            _ => {}
        }
        review.apply(action)?;
        review.revision += 1;
        review.updated_at = now_iso();
        let tx = self.db.transaction()?;
        if let Some(conversation_id) = conversation_id {
            let linked: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM review_conversations WHERE review_id = ?1 AND conversation_id = ?2)", params![id, conversation_id], |row| row.get(0))?;
            if !linked { bail!("对话与综述的关联已不存在，请重新打开对话") }
        }
        let changed = tx.execute("UPDATE review_projects SET revision = ?1, body = ?2, updated_at = ?3 WHERE id = ?4 AND revision = ?5", params![review.revision, serde_json::to_string(&review)?, review.updated_at, id, expected_revision])?;
        if changed != 1 {
            bail!("综述已在别处修改，请重新载入")
        }
        // Track only papers the user actually records as evidence. Keeping the
        // tombstone after a row is removed prevents accidental version reuse.
        if let Some(paper_id) = watched_paper {
            tx.execute("INSERT OR IGNORE INTO review_source_versions(paper_id, version) VALUES (?1, 0)", [paper_id])?;
            let version: i32 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
            if version > crate::store::SCHEMA_VERSION { bail!("文献库已由更新版本升级，请升级程序后再操作") }
            if version < 2 { tx.pragma_update(None, "user_version", 2)? }
        }
        tx.commit()?;
        self.evidence_availability(review)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn action(store: &mut Store, r: &Review, a: ReviewAction) -> Review {
        store.apply_review(&r.id, r.revision, a).unwrap()
    }

    #[test]
    fn lazy_upgrade_guards_old_connections_and_preserves_legacy_data() {
        let root = std::env::temp_dir().join(new_id("wisp-review-upgrade"));
        let (review_id, conversation_id, paper_id) = {
            let mut s = Store::open(&root).unwrap();
            assert_eq!(s.db.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0)).unwrap(), 1);
            let paper = s.add_paper(crate::model::Paper { title: "Source".into(), ..Default::default() }).unwrap();
            let mut conversation = crate::store::Conversation::default();
            s.save_conversation(&mut conversation).unwrap();
            let r = s.create_review_for_conversation("Legacy review", &conversation.id).unwrap();
            let r = action(&mut s, &r, ReviewAction::SetRequirements("Legacy requirements".into()));
            (r.id, conversation.id, paper.id)
        };
        let mut s = Store::open(&root).unwrap();
        let mut r = s.review(&review_id).unwrap().unwrap();
        assert_eq!(r.requirements, "Legacy requirements");
        assert_eq!(s.db.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0)).unwrap(), 1, "opening a v1 library without evidence remains v1");
        let old = Connection::open(root.join(crate::store::DB_FILE)).unwrap();
        old.pragma_update(None, "foreign_keys", "ON").unwrap();
        assert_eq!(old.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0)).unwrap(), 1);
        let invalid = EvidenceRow { claim: "".into(), paper_id: paper_id.clone(), ..Default::default() };
        assert!(s.apply_review(&r.id, r.revision, ReviewAction::SaveEvidence(invalid)).is_err());
        assert_eq!(s.db.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0)).unwrap(), 1, "failed first save must not upgrade");
        assert_eq!(s.db.query_row("SELECT count(*) FROM review_source_versions", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        s.db.execute_batch("CREATE TRIGGER test_fail_review BEFORE UPDATE OF body ON review_projects BEGIN SELECT RAISE(ABORT, 'test write failure'); END;").unwrap();
        let valid = EvidenceRow { claim: "Claim".into(), paper_id: paper_id.clone(), ..Default::default() };
        assert!(s.apply_review(&r.id, r.revision, ReviewAction::SaveEvidence(valid)).is_err());
        assert_eq!(s.db.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0)).unwrap(), 1, "a failed transactional save must not upgrade");
        assert_eq!(s.db.query_row("SELECT count(*) FROM review_source_versions", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        s.db.execute_batch("DROP TRIGGER test_fail_review").unwrap();
        r = action(&mut s, &r, ReviewAction::SaveEvidence(EvidenceRow { claim: "Claim".into(), paper_id: paper_id.clone(), ..Default::default() }));
        assert_eq!(s.db.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0)).unwrap(), 2);
        assert_eq!(old.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0)).unwrap(), 2);
        let mut old_json: serde_json::Value = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        old_json.as_object_mut().unwrap().remove("evidence_rows");
        assert!(old.execute("UPDATE review_projects SET body = ?1 WHERE id = ?2", params![old_json.to_string(), review_id]).is_err(), "an already-open old client cannot discard evidence");
        assert!(old.execute("DELETE FROM conversations WHERE id = ?1", [&conversation_id]).is_err(), "an already-open old client cannot orphan evidence by deleting its conversation");
        assert_eq!(s.review_for_conversation(&conversation_id).unwrap().unwrap().evidence_rows.len(), 1);
        let row_id = r.evidence_rows[0].id.clone();
        r = action(&mut s, &r, ReviewAction::JudgeEvidence { id: row_id, judgment: EvidenceJudgment::Supports });
        assert!(r.evidence_rows[0].source_current);
        // An interrupted/mixed-version state with evidence but v1 is repaired
        // on next open, including tracked paper IDs.
        s.db.pragma_update(None, "user_version", 1).unwrap();
        s.db.execute("DELETE FROM review_source_versions", []).unwrap();
        drop(old);
        drop(s);
        let mut s = Store::open(&root).unwrap();
        assert_eq!(s.db.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0)).unwrap(), 2);
        assert_eq!(s.db.query_row("SELECT count(*) FROM review_source_versions WHERE paper_id = ?1", [&paper_id], |r| r.get::<_, i64>(0)).unwrap(), 1);
        let recovered = s.review_for_conversation(&conversation_id).unwrap().unwrap();
        assert_eq!(recovered.evidence_rows.len(), 1);
        assert!(!recovered.evidence_rows[0].source_current, "evidence with a missing watcher must be manually rechecked");
        let row_id = s.review(&review_id).unwrap().unwrap().evidence_rows[0].id.clone();
        let current = s.review(&review_id).unwrap().unwrap();
        action(&mut s, &current, ReviewAction::RemoveEvidence { id: row_id });
        assert!(s.remove_conversation(&conversation_id).is_ok(), "explicitly removing all evidence permits normal deletion");
        drop(s);
        let s = Store::open(&root).unwrap();
        assert_eq!(s.db.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0)).unwrap(), 2, "a v2 library never silently downgrades");
        drop(s);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn source_generations_track_only_recorded_papers() {
        let mut s = Store::in_memory(Path::new("/tmp/review-watch-test")).unwrap();
        let mut paper = s.add_paper(crate::model::Paper { title: "Untracked".into(), doi: "10.1234/watch".into(), ..Default::default() }).unwrap();
        paper.abstract_text = "New abstract".into();
        s.save_paper(&mut paper).unwrap();
        s.trash_paper(&paper.id).unwrap();
        s.restore_paper(&paper.id).unwrap();
        assert_eq!(s.db.query_row("SELECT count(*) FROM review_source_versions", [], |r| r.get::<_, i64>(0)).unwrap(), 0, "normal libraries do not accumulate version rows");
        let r = s.create_review("Review").unwrap();
        let r = action(&mut s, &r, ReviewAction::SaveEvidence(EvidenceRow { claim: "Claim".into(), paper_id: paper.id.clone(), ..Default::default() }));
        let id = r.evidence_rows[0].id.clone();
        assert_eq!(s.db.query_row("SELECT count(*) FROM review_source_versions", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        let r = action(&mut s, &r, ReviewAction::JudgeEvidence { id: id.clone(), judgment: EvidenceJudgment::Supports });
        paper.abstract_text = "Changed".into();
        s.save_paper(&mut paper).unwrap();
        paper.abstract_text = "New abstract".into();
        s.save_paper(&mut paper).unwrap();
        let r = s.review(&r.id).unwrap().unwrap();
        assert!(!r.evidence_rows[0].source_current, "tracked change and revert remain stale");
        action(&mut s, &r, ReviewAction::RemoveEvidence { id });
        assert_eq!(s.db.query_row("SELECT count(*) FROM review_source_versions", [], |r| r.get::<_, i64>(0)).unwrap(), 1, "keep a version tombstone for previously referenced papers");
        s.delete_paper(&paper.id).unwrap();
        assert_eq!(s.db.query_row("SELECT count(*) FROM review_source_versions", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    }

    #[test]
    fn first_evidence_cannot_race_with_deleting_empty_conversation() {
        let root = std::env::temp_dir().join(new_id("wisp-review-link-race"));
        let mut s = Store::open(&root).unwrap();
        let paper = s.add_paper(crate::model::Paper { title: "Source".into(), ..Default::default() }).unwrap();
        let mut conversation = crate::store::Conversation::default();
        s.save_conversation(&mut conversation).unwrap();
        let r = s.create_review_for_conversation("Review", &conversation.id).unwrap();
        let row = || EvidenceRow { claim: "A claim".into(), paper_id: paper.id.clone(), ..Default::default() };
        assert!(s.apply_linked_review(&r.id, r.revision, "different-conversation", ReviewAction::SaveEvidence(row())).is_err());
        let old = Connection::open(root.join(crate::store::DB_FILE)).unwrap();
        old.pragma_update(None, "foreign_keys", "ON").unwrap();
        old.execute("DELETE FROM conversations WHERE id = ?1", [&conversation.id]).unwrap();
        assert!(s.apply_linked_review(&r.id, r.revision, &conversation.id, ReviewAction::SaveEvidence(row())).is_err());
        assert!(s.review(&r.id).unwrap().unwrap().evidence_rows.is_empty());
        assert_eq!(s.db.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0)).unwrap(), 1);
        assert_eq!(s.db.query_row("SELECT count(*) FROM review_source_versions", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        drop(old);
        drop(s);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn evidence_is_explicit_and_invalidated_by_content_changes() {
        let mut s = Store::in_memory(Path::new("/tmp/review-evidence-test")).unwrap();
        let paper = s.add_paper(crate::model::Paper { title: "A paper".into(), ..Default::default() }).unwrap();
        let r = s.create_review("Review").unwrap();
        let r = action(&mut s, &r, ReviewAction::SetRequirements("Requirements".into()));
        let r = action(&mut s, &r, ReviewAction::ConfirmRequirements);
        let r = action(&mut s, &r, ReviewAction::SetOutline("Outline".into()));
        let r = action(&mut s, &r, ReviewAction::ConfirmOutline);
        let r = action(&mut s, &r, ReviewAction::SaveSection { name: "A".into(), body: "The claim is limited to cells.".into() });
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
        let r = action(&mut s, &r, ReviewAction::SetChecks("Manually checked".into()));
        let r = action(&mut s, &r, ReviewAction::Complete);
        row = r.evidence_rows[0].clone();
        row.note = "Changed source context".into();
        row.judgment = EvidenceJudgment::Supports;
        let changed = action(&mut s, &r, ReviewAction::SaveEvidence(row));
        assert_eq!(changed.evidence_rows[0].judgment, EvidenceJudgment::Unreviewed);
        assert!(!changed.checks_current && !changed.completed);
        assert!(s.apply_review(&r.id, r.revision, ReviewAction::RemoveEvidence { id: changed.evidence_rows[0].id.clone() }).is_err(), "stale revision must not overwrite");
        let id = changed.evidence_rows[0].id.clone();
        let changed = action(&mut s, &changed, ReviewAction::JudgeEvidence { id: id.clone(), judgment: EvidenceJudgment::Supports });
        let changed = action(&mut s, &changed, ReviewAction::SaveSection { name: "A".into(), body: "The claim was revised.".into() });
        assert_eq!(changed.evidence_rows[0].judgment, EvidenceJudgment::Unreviewed);
        assert!(s.apply_review(&changed.id, changed.revision, ReviewAction::JudgeEvidence { id: id.clone(), judgment: EvidenceJudgment::Supports }).is_err(), "a detached claim cannot be revalidated");
        let changed = action(&mut s, &changed, ReviewAction::RemoveSection { name: "A".into() });
        assert_eq!(changed.evidence_rows[0].section, "A", "keep the evidence snapshot even when its section is removed");
        assert!(changed.evidence_rows[0].section_detached);
        let changed = action(&mut s, &changed, ReviewAction::SaveSection { name: "A".into(), body: "The claim is limited to cells.".into() });
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
            let r = s.create_review("Review").unwrap();
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
            assert!(r.evidence_rows[0].source_available);
            s.trash_paper(&paper_id).unwrap();
            let r = s.review(&review_id).unwrap().unwrap();
            assert!(!r.evidence_rows[0].source_available);
            assert!(s.apply_review(&r.id, r.revision, ReviewAction::JudgeEvidence { id: row_id.clone(), judgment: EvidenceJudgment::Supports }).is_err());
            s.restore_paper(&paper_id).unwrap();
            let restored = s.review(&review_id).unwrap().unwrap();
            assert!(restored.evidence_rows[0].source_available);
            assert!(!restored.evidence_rows[0].source_current, "restoring a paper cannot silently restore a former judgment");
            let restored = action(&mut s, &restored, ReviewAction::JudgeEvidence { id: row_id.clone(), judgment: EvidenceJudgment::Partial });
            assert!(restored.evidence_rows[0].source_current);
            s.trash_paper(&paper_id).unwrap();
            s.delete_paper(&paper_id).unwrap();
            let r = s.review(&review_id).unwrap().unwrap();
            assert!(!r.evidence_rows[0].source_available);
            assert_eq!(r.evidence_rows[0].paper_title, "Original title");
            let r = action(&mut s, &r, ReviewAction::JudgeEvidence { id: row_id, judgment: EvidenceJudgment::Unreviewed });
            assert_eq!(r.evidence_rows[0].judgment, EvidenceJudgment::Unreviewed);
        }
        // Older reviews have no `evidence_rows`; the default preserves them.
        let old: Review = serde_json::from_str(r#"{"title":"Old review","evidence":"notes"}"#).unwrap();
        assert_eq!(old.evidence, "notes");
        assert!(old.evidence_rows.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn source_changes_invalidate_judgments_and_cache_tracks_fulltext_transactions() {
        let root = std::env::temp_dir().join(new_id("wisp-evidence-version"));
        std::fs::create_dir_all(&root).unwrap();
        let external = root.join("source.pdf");
        std::fs::write(&external, b"test pdf").unwrap();
        let mut s = Store::open(&root).unwrap();
        let paper = s.add_paper(crate::model::Paper { title: "A paper".into(), abstract_text: "Old abstract".into(), doi: "10.1234/example".into(), ..Default::default() }).unwrap();
        let file = s.import_file(&paper.id, &external, "fulltext").unwrap();
        s.save_fulltext_pages(&file.id, &Ok(vec!["First text".into()])).unwrap();
        let r = s.create_review("Review").unwrap();
        let r = action(&mut s, &r, ReviewAction::SetRequirements("Requirements".into()));
        let r = action(&mut s, &r, ReviewAction::ConfirmRequirements);
        let r = action(&mut s, &r, ReviewAction::SetOutline("Outline".into()));
        let r = action(&mut s, &r, ReviewAction::ConfirmOutline);
        let r = action(&mut s, &r, ReviewAction::SaveSection { name: "A".into(), body: "A claim".into() });
        let r = action(&mut s, &r, ReviewAction::SaveEvidence(EvidenceRow { section: "A".into(), claim: "A claim".into(), paper_id: paper.id.clone(), source: EvidenceSource::Abstract, ..Default::default() }));
        let row_id = r.evidence_rows[0].id.clone();
        let r = action(&mut s, &r, ReviewAction::JudgeEvidence { id: row_id.clone(), judgment: EvidenceJudgment::Supports });
        assert!(r.evidence_rows[0].source_current);
        let r = action(&mut s, &r, ReviewAction::SetChecks("Checked".into()));
        let r = action(&mut s, &r, ReviewAction::Complete);
        let mut tag_edit = s.get_paper(&paper.id).unwrap();
        tag_edit.tags.push("A tag".into());
        s.save_paper(&mut tag_edit).unwrap();
        assert!(s.review(&r.id).unwrap().unwrap().evidence_rows[0].source_current, "unrelated tag changes do not invalidate source judgments");
        let mut changed_paper = s.get_paper(&paper.id).unwrap();
        changed_paper.abstract_text = "New abstract".into();
        s.save_paper(&mut changed_paper).unwrap();
        let changed = s.review(&r.id).unwrap().unwrap();
        assert!(!changed.evidence_rows[0].source_current && !changed.checks_current && !changed.completed);
        // Deriving stale state does not rewrite the review record.
        let raw: String = s.db.query_row("SELECT body FROM review_projects WHERE id = ?1", [&r.id], |r| r.get(0)).unwrap();
        let stored: Review = serde_json::from_str(&raw).unwrap();
        assert!(stored.checks_current && stored.completed);
        changed_paper.abstract_text = "Old abstract".into();
        s.save_paper(&mut changed_paper).unwrap();
        assert!(!s.review(&r.id).unwrap().unwrap().evidence_rows[0].source_current, "reverting content cannot silently revive the judgment");
        let changed = action(&mut s, &changed, ReviewAction::JudgeEvidence { id: row_id.clone(), judgment: EvidenceJudgment::Partial });
        assert!(changed.evidence_rows[0].source_current && !changed.checks_current);
        assert_eq!(s.db.query_row("SELECT count(*) FROM review_text_digests", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        // Equal-length updates with identical extraction time still invalidate
        // the cached digest, and a rolled-back update restores the cache.
        let original_pages: String = s.db.query_row("SELECT pages FROM fulltext_text WHERE resource_id = ?1", [&file.id], |r| r.get(0)).unwrap();
        let replacement = original_pages.replace("First", "Other");
        assert_eq!(original_pages.len(), replacement.len());
        let version: i64 = s.db.query_row("SELECT version FROM review_source_versions WHERE paper_id = ?1", [&paper.id], |r| r.get(0)).unwrap();
        {
            let tx = s.db.transaction().unwrap();
            tx.execute("UPDATE fulltext_text SET pages = ?1 WHERE resource_id = ?2", params![replacement, file.id]).unwrap();
            assert_eq!(tx.query_row("SELECT count(*) FROM review_text_digests", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
            tx.rollback().unwrap();
        }
        assert_eq!(s.db.query_row("SELECT count(*) FROM review_text_digests", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(s.db.query_row("SELECT version FROM review_source_versions WHERE paper_id = ?1", [&paper.id], |r| r.get::<_, i64>(0)).unwrap(), version, "rolled-back changes cannot advance source version");
        assert!(s.review(&r.id).unwrap().unwrap().evidence_rows[0].source_current);
        s.db.execute("UPDATE fulltext_text SET pages = ?1 WHERE resource_id = ?2", params![replacement, file.id]).unwrap();
        assert_eq!(s.db.query_row("SELECT count(*) FROM review_text_digests", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        let changed = s.review(&r.id).unwrap().unwrap();
        assert!(!changed.evidence_rows[0].source_current);
        let changed = action(&mut s, &changed, ReviewAction::JudgeEvidence { id: row_id.clone(), judgment: EvidenceJudgment::Partial });
        assert!(changed.evidence_rows[0].source_current);
        s.db.execute("DELETE FROM fulltext_text WHERE resource_id = ?1", [&file.id]).unwrap();
        assert_eq!(s.db.query_row("SELECT count(*) FROM review_text_digests", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        let changed = s.review(&r.id).unwrap().unwrap();
        assert!(!changed.evidence_rows[0].source_current);
        let changed = action(&mut s, &changed, ReviewAction::JudgeEvidence { id: row_id.clone(), judgment: EvidenceJudgment::Partial });
        assert!(changed.evidence_rows[0].source_current);
        std::fs::remove_file(root.join(&file.local_path)).unwrap();
        let missing = s.review(&r.id).unwrap().unwrap();
        assert!(missing.evidence_rows[0].source_available && missing.evidence_rows[0].local_files_missing);
        assert!(!missing.evidence_rows[0].source_current, "missing attachment makes the old judgment stale");
        let rejudged = action(&mut s, &missing, ReviewAction::JudgeEvidence { id: row_id, judgment: EvidenceJudgment::Partial });
        assert!(rejudged.evidence_rows[0].source_current && rejudged.evidence_rows[0].local_files_missing, "abstract can still be manually judged with an explicit missing-file warning");
        let mut legacy = rejudged.clone();
        legacy.evidence_rows[0].assessed_fingerprint.clear();
        legacy.checks_current = true;
        legacy.completed = true;
        s.db.execute("UPDATE review_projects SET body = ?1 WHERE id = ?2", params![serde_json::to_string(&legacy).unwrap(), legacy.id]).unwrap();
        let read = s.review(&r.id).unwrap().unwrap();
        assert!(!read.evidence_rows[0].source_current && !read.checks_current && !read.completed, "a legacy verdict has no proven source version");
        drop(s);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn identifier_changes_invalidate_without_breaking_conflict_rollback() {
        let mut s = Store::in_memory(Path::new("/tmp/review-identifier-test")).unwrap();
        let mut first = s.add_paper(crate::model::Paper { title: "First paper".into(), doi: "10.1234/one".into(), pmid: "111".into(), ..Default::default() }).unwrap();
        let second = s.add_paper(crate::model::Paper { title: "Second paper".into(), doi: "10.1234/two".into(), ..Default::default() }).unwrap();
        let r = s.create_review("Review").unwrap();
        let r = action(&mut s, &r, ReviewAction::SaveEvidence(EvidenceRow { claim: "Claim".into(), paper_id: first.id.clone(), ..Default::default() }));
        let id = r.evidence_rows[0].id.clone();
        let r = action(&mut s, &r, ReviewAction::JudgeEvidence { id: id.clone(), judgment: EvidenceJudgment::Supports });
        first.tags.push("changed tag".into());
        s.save_paper(&mut first).unwrap();
        assert!(s.review(&r.id).unwrap().unwrap().evidence_rows[0].source_current, "unchanged identifiers must not invalidate after a tag edit");
        first.title = "Must roll back".into();
        first.doi = second.doi.to_uppercase();
        assert!(s.save_paper(&mut first).is_err(), "case-normalized conflicting DOI must be rejected");
        let kept = s.get_paper(&first.id).unwrap();
        assert_eq!(kept.title, "First paper");
        assert_eq!(kept.doi, "10.1234/one");
        assert_eq!(kept.pmid, "111");
        assert!(s.review(&r.id).unwrap().unwrap().evidence_rows[0].source_current, "failed transaction cannot invalidate evidence");
        first = kept;
        first.doi = "10.1234/three".into();
        s.save_paper(&mut first).unwrap();
        let changed = s.review(&r.id).unwrap().unwrap();
        assert!(!changed.evidence_rows[0].source_current);
        let changed = action(&mut s, &changed, ReviewAction::JudgeEvidence { id, judgment: EvidenceJudgment::Supports });
        first.doi.clear();
        s.save_paper(&mut first).unwrap();
        let kept = s.get_paper(&first.id).unwrap();
        assert!(kept.doi.is_empty());
        assert_eq!(kept.pmid, "111");
        assert!(!s.review(&changed.id).unwrap().unwrap().evidence_rows[0].source_current);
    }

    #[test]
    fn stages_reconfirm_and_keep_drafts() {
        let mut s = Store::in_memory(Path::new("/tmp/review-test")).unwrap();
        let r = s.create_review("Roots").unwrap();
        assert!(
            s.apply_review(&r.id, r.revision, ReviewAction::ConfirmOutline)
                .is_err()
        );
        assert!(
            s.apply_review(
                &r.id,
                r.revision,
                ReviewAction::SaveSection {
                    name: "A".into(),
                    body: "Draft".into()
                }
            )
            .is_err()
        );
        let r = action(
            &mut s,
            &r,
            ReviewAction::SetRequirements("Audience: researchers".into()),
        );
        let r = action(&mut s, &r, ReviewAction::ConfirmRequirements);
        let r = action(&mut s, &r, ReviewAction::SetOutline("A. Roots".into()));
        let r = action(&mut s, &r, ReviewAction::ConfirmOutline);
        let r = action(
            &mut s,
            &r,
            ReviewAction::SetEvidence("P1: abstract only".into()),
        );
        let r = action(
            &mut s,
            &r,
            ReviewAction::SaveSection {
                name: "A".into(),
                body: "Draft [P1]".into(),
            },
        );
        assert!(
            s.apply_review(&r.id, r.revision, ReviewAction::Complete)
                .is_err()
        );
        let r = action(
            &mut s,
            &r,
            ReviewAction::SetChecks("P1 is abstract-only; full text unavailable".into()),
        );
        let r = action(&mut s, &r, ReviewAction::Complete);
        assert!(r.completed);
        let edited = action(
            &mut s,
            &r,
            ReviewAction::SetOutline("A. New outline".into()),
        );
        assert!(!edited.completed && !edited.outline_confirmed && !edited.checks_current);
        assert_eq!(edited.sections[0].body, "Draft [P1]");
        assert!(
            s.apply_review(
                &edited.id,
                edited.revision,
                ReviewAction::SaveSection {
                    name: "A".into(),
                    body: "New".into()
                }
            )
            .is_err()
        );
        assert!(
            s.apply_review(&r.id, r.revision, ReviewAction::ConfirmOutline)
                .is_err(),
            "stale editor must not overwrite"
        );
        let edited = action(&mut s, &edited, ReviewAction::ConfirmOutline);
        assert!(
            s.apply_review(&edited.id, edited.revision, ReviewAction::Complete)
                .is_err(),
            "old checks are stale"
        );
        let edited = action(&mut s, &edited, ReviewAction::SetChecks("Checked".into()));
        let edited = action(&mut s, &edited, ReviewAction::Complete);
        let edited = action(&mut s, &edited, ReviewAction::RemoveSection { name: "A".into() });
        assert!(edited.sections.is_empty() && !edited.checks_current && !edited.completed);
        assert!(s.apply_review(&edited.id, edited.revision, ReviewAction::RemoveSection { name: "missing".into() }).is_err());
    }

    #[test]
    fn record_survives_close_and_reopen() {
        let root = std::env::temp_dir().join(new_id("wisp-review"));
        let (id, revision, conversation_id) = {
            let mut s = Store::open(&root).unwrap();
            assert!(s.create_review_for_conversation("Missing", "missing").is_err());
            assert!(s.reviews().unwrap().is_empty(), "a failed link leaves no orphan");
            let mut conversation = crate::store::Conversation::default();
            s.save_conversation(&mut conversation).unwrap();
            let r = s.create_review_for_conversation("Review", &conversation.id).unwrap();
            assert!(s.create_review_for_conversation("Duplicate", &conversation.id).is_err());
            assert_eq!(s.reviews().unwrap().len(), 1, "duplicate link rolls back the project");
            assert!(s.link_review_conversation(&r.id, "missing").is_err());
            s.link_review_conversation(&r.id, &conversation.id).unwrap();
            s.link_review_conversation(&r.id, &conversation.id).unwrap();
            let other = s.create_review("Other").unwrap();
            assert!(
                s.link_review_conversation(&other.id, &conversation.id)
                    .is_err()
            );
            let r = action(
                &mut s,
                &r,
                ReviewAction::SetRequirements("English, 3000 words".into()),
            );
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
            assert_eq!(
                s.review_for_conversation(&conversation_id).unwrap(),
                Some(r)
            );
            assert_eq!(s.reviews().unwrap().len(), 2);
            s.remove_conversation(&conversation_id).unwrap();
            assert!(
                s.review_for_conversation(&conversation_id)
                    .unwrap()
                    .is_none()
            );
            assert!(s.review(&id).unwrap().is_some());
        }
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
            let r = action(&mut store, &r, ReviewAction::SaveSection { name: "Roots".into(), body: "Root finding [P1 p.5].".into() });
            let r = action(&mut store, &r, ReviewAction::SaveSection { name: "Leaves".into(), body: "Abstract-only finding [P2].".into() });
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
            assert!(exported.contains("[@Smith2020, p. 5]") && exported.contains("[@Jones2021]"));
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
