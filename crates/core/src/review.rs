//! A review's working record. The record keeps user decisions separately from
//! model output; a UI must explicitly advance the confirmed stages.

use crate::model::new_id;
use crate::store::Store;
use crate::time::now_iso;
use anyhow::{Result, bail};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

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
"#;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReviewSection {
    pub name: String,
    pub body: String,
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
            ReviewAction::SaveSection { name, body } => {
                if !self.outline_confirmed {
                    bail!("请先确认大纲")
                }
                nonempty(&name, "章节名")?;
                nonempty(&body, "章节正文")?;
                let name = name.trim().to_string();
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
        body.map(|body| Ok(serde_json::from_str(&body)?))
            .transpose()
    }

    pub fn reviews(&self) -> Result<Vec<Review>> {
        let mut stmt = self
            .db
            .prepare("SELECT body FROM review_projects ORDER BY updated_at DESC, id")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
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
        body.map(|body| Ok(serde_json::from_str(&body)?))
            .transpose()
    }

    /// Reject a stale editor rather than silently overwriting another window's work.
    pub fn apply_review(
        &mut self,
        id: &str,
        expected_revision: i64,
        action: ReviewAction,
    ) -> Result<Review> {
        let mut review = self
            .review(id)?
            .ok_or_else(|| anyhow::anyhow!("综述不存在"))?;
        if review.revision != expected_revision {
            bail!("综述已在别处修改，请重新载入")
        }
        review.apply(action)?;
        review.revision += 1;
        review.updated_at = now_iso();
        let changed = self.db.execute("UPDATE review_projects SET revision = ?1, body = ?2, updated_at = ?3 WHERE id = ?4 AND revision = ?5", params![review.revision, serde_json::to_string(&review)?, review.updated_at, id, expected_revision])?;
        if changed != 1 {
            bail!("综述已在别处修改，请重新载入")
        }
        Ok(review)
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
