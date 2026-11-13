//! PDF reading marks kept in SQLite, separate from the source file. Geometry
//! uses page points from the top left, just like `pdftext::Mark`, so zoom and
//! raster cache eviction do not change an annotation's anchor.

use crate::model::new_id;
use crate::store::Store;
use crate::time::now_iso;
use anyhow::{Result, bail};
use rusqlite::params;
use serde::{Deserialize, Serialize};

pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS pdf_annotations (
    id TEXT PRIMARY KEY NOT NULL,
    paper_id TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
    resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    page INTEGER NOT NULL CHECK (page > 0),
    kind TEXT NOT NULL CHECK (kind IN ('underline', 'highlight')),
    quote TEXT NOT NULL CHECK (length(trim(quote)) > 0),
    boxes_json TEXT NOT NULL,
    note TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_pdf_annotations_source ON pdf_annotations(resource_id, page);
CREATE INDEX IF NOT EXISTS idx_pdf_annotations_paper ON pdf_annotations(paper_id);
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind { Underline, Highlight }

impl Kind {
    pub fn key(self) -> &'static str {
        match self { Self::Underline => "underline", Self::Highlight => "highlight" }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Annotation {
    pub id: String,
    pub paper_id: String,
    pub resource_id: String,
    /// 1-based page in this attachment, not the printed page number.
    pub page: usize,
    pub kind: Kind,
    /// Original selected text. The user's interpretation lives in `note`.
    pub quote: String,
    /// One rectangle per selected line: [left, top, right, bottom] in points.
    pub boxes: Vec<[f32; 4]>,
    pub note: String,
    pub created_at: String,
    pub updated_at: String,
}

impl Store {
    /// Visible annotations in reading order. Empty filters mean the whole
    /// library; trashed papers retain their marks but are not exposed here.
    pub fn pdf_annotations(&self, paper: &str, resource: &str) -> Result<Vec<Annotation>> {
        let mut stmt = self.db.prepare(
            "SELECT id, paper_id, resource_id, page, kind, quote, boxes_json, note, created_at, updated_at
             FROM pdf_annotations WHERE (?1 = '' OR paper_id = ?1) AND (?2 = '' OR resource_id = ?2)
             AND paper_id NOT IN (SELECT paper_id FROM trashed_papers)
             ORDER BY paper_id, resource_id, page, created_at, id",
        )?;
        let rows = stmt.query_map(params![paper, resource], |r| {
            let kind: String = r.get(4)?;
            let boxes: String = r.get(6)?;
            let invalid = |i, e| rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e));
            Ok(Annotation {
                id: r.get(0)?, paper_id: r.get(1)?, resource_id: r.get(2)?, page: r.get::<_, i64>(3)? as usize,
                kind: serde_json::from_value(serde_json::Value::String(kind)).map_err(|e| invalid(4, e))?,
                quote: r.get(5)?, boxes: serde_json::from_str(&boxes).map_err(|e| invalid(6, e))?,
                note: r.get(7)?, created_at: r.get(8)?, updated_at: r.get(9)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn add_pdf_annotation(&mut self, paper: &str, resource: &str, page: usize, kind: Kind, quote: &str, boxes: &[[f32; 4]]) -> Result<Annotation> {
        let quote = quote.trim();
        if page == 0 || page > i32::MAX as usize || quote.is_empty() || quote.len() > 100_000 { bail!("PDF 标注需要有效的页码和选中文字。") }
        if boxes.is_empty() || boxes.len() > 10_000 || boxes.iter().any(|b| b.iter().any(|n| !n.is_finite()) || b[2] <= b[0] || b[3] <= b[1]) { bail!("PDF 标注的位置无效。") }
        let valid: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM resources WHERE id = ?1 AND paper_id = ?2 AND lower(format) = 'pdf'
             AND paper_id NOT IN (SELECT paper_id FROM trashed_papers))", params![resource, paper], |r| r.get(0),
        )?;
        if !valid { bail!("PDF 附件不存在或不属于这篇文献。") }
        // A repeated click on the same action must not create duplicate marks.
        if let Some(existing) = self.pdf_annotations(paper, resource)?.into_iter().find(|a| a.page == page && a.kind == kind && a.quote == quote && a.boxes == boxes) { return Ok(existing) }
        let now = now_iso();
        let annotation = Annotation { id: new_id("A"), paper_id: paper.into(), resource_id: resource.into(), page, kind, quote: quote.into(), boxes: boxes.to_vec(), note: String::new(), created_at: now.clone(), updated_at: now };
        self.db.execute(
            "INSERT INTO pdf_annotations (id, paper_id, resource_id, page, kind, quote, boxes_json, note, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '', ?8, ?8)",
            params![annotation.id, paper, resource, page as i64, kind.key(), quote, serde_json::to_string(boxes)?, annotation.created_at],
        )?;
        Ok(annotation)
    }

    /// Notes can change; the selected original text and its anchor cannot.
    pub fn update_pdf_annotation_note(&mut self, resource: &str, id: &str, note: &str) -> Result<()> {
        if note.len() > 100_000 { bail!("标注笔记过长。") }
        if self.db.execute("UPDATE pdf_annotations SET note = ?1, updated_at = ?2 WHERE id = ?3 AND resource_id = ?4", params![note.trim(), now_iso(), id, resource])? == 0 { bail!("PDF 标注不存在。") }
        Ok(())
    }

    pub fn remove_pdf_annotation(&mut self, resource: &str, id: &str) -> Result<()> {
        if self.db.execute("DELETE FROM pdf_annotations WHERE id = ?1 AND resource_id = ?2", params![id, resource])? == 0 { bail!("PDF 标注不存在。") }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Paper;

    #[test]
    fn marks_survive_reopening_without_changing_pdf_and_follow_their_attachment() {
        let root = std::env::temp_dir().join(new_id("wisp-annotations"));
        let source = root.join("source.pdf");
        let mut store = Store::open(&root).unwrap();
        std::fs::write(&source, b"%PDF-1.4 test source").unwrap();
        let paper = store.add_paper(Paper { title: "Roots".into(), ..Default::default() }).unwrap();
        let other = store.add_paper(Paper { title: "Leaves".into(), ..Default::default() }).unwrap();
        let file = store.import_file(&paper.id, &source, "fulltext").unwrap();
        let other_file = store.import_file(&other.id, &source, "fulltext").unwrap();
        let boxes = vec![[10., 20., 80., 30.], [10., 35., 50., 45.]];
        let mark = store.add_pdf_annotation(&paper.id, &file.id, 2, Kind::Underline, "Root growth", &boxes).unwrap();
        assert_eq!(store.add_pdf_annotation(&paper.id, &file.id, 2, Kind::Underline, "Root growth", &boxes).unwrap().id, mark.id);
        assert!(store.add_pdf_annotation(&paper.id, &other_file.id, 1, Kind::Highlight, "Wrong paper", &boxes).is_err());
        assert!(store.update_pdf_annotation_note(&other_file.id, &mark.id, "wrong").is_err());
        assert!(store.remove_pdf_annotation(&other_file.id, &mark.id).is_err());
        store.update_pdf_annotation_note(&file.id, &mark.id, " Compare with leaf growth. ").unwrap();
        let path = store.file_path(&file);
        drop(store);
        let mut store = Store::open(&root).unwrap();
        let saved = store.pdf_annotations(&paper.id, &file.id).unwrap().remove(0);
        assert_eq!((&saved.quote, &saved.boxes, saved.page, saved.kind), (&mark.quote, &boxes, 2, Kind::Underline));
        assert_eq!(saved.note, "Compare with leaf growth.");
        assert_eq!(std::fs::read(path).unwrap(), std::fs::read(&source).unwrap());
        assert!(store.pdf_annotations("", &other_file.id).unwrap().is_empty());
        store.trash_paper(&paper.id).unwrap();
        assert!(store.pdf_annotations("", "").unwrap().is_empty());
        store.restore_paper(&paper.id).unwrap();
        assert_eq!(store.pdf_annotations("", "").unwrap().len(), 1);
        store.remove_pdf_annotation(&file.id, &mark.id).unwrap();
        assert!(store.pdf_annotations("", "").unwrap().is_empty());
        store.add_pdf_annotation(&paper.id, &file.id, 1, Kind::Highlight, "Root", &boxes).unwrap();
        store.remove_file(&file.id).unwrap();
        assert!(store.pdf_annotations("", "").unwrap().is_empty());
        store.add_pdf_annotation(&other.id, &other_file.id, 1, Kind::Underline, "Leaf", &boxes).unwrap();
        store.delete_paper(&other.id).unwrap();
        assert!(store.pdf_annotations("", "").unwrap().is_empty());
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_anchors_are_rejected_and_old_libraries_gain_the_table() {
        let root = std::env::temp_dir().join(new_id("wisp-annotation-migration"));
        let mut store = Store::open(&root).unwrap();
        store.db.execute_batch("DROP TABLE pdf_annotations").unwrap();
        let paper = store.add_paper(Paper { title: "Existing paper".into(), ..Default::default() }).unwrap();
        drop(store);
        let mut store = Store::open(&root).unwrap();
        assert_eq!(store.get_paper(&paper.id).unwrap().title, "Existing paper");
        assert!(store.pdf_annotations("", "").unwrap().is_empty());
        for (page, quote, boxes) in [(0, "word", vec![[1., 2., 3., 4.]]), (1, " ", vec![[1., 2., 3., 4.]]), (1, "word", vec![]), (1, "word", vec![[3., 2., 1., 4.]]), (1, "word", vec![[1., 2., f32::NAN, 4.]])] {
            assert!(store.add_pdf_annotation(&paper.id, "missing", page, Kind::Underline, quote, &boxes).is_err());
        }
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}
