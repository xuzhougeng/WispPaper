//! PDF reading marks kept in SQLite, separate from the source file. Geometry
//! uses page points from the top left, just like `pdftext::Mark`, so zoom and
//! raster cache eviction do not change an annotation's anchor.

use crate::model::new_id;
use crate::store::Store;
use crate::time::now_iso;
use anyhow::{Result, bail};
use rusqlite::{OptionalExtension, params};
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
-- Marks placed on a page rather than on its text: a box drawn round a figure or a passage,
-- a sticky note anywhere. Beside the first table, whose kinds are fixed by its CHECK.
-- A box's `rect` is its four edges; a note's is the spot its icon stands on (left = right, top = bottom).
CREATE TABLE IF NOT EXISTS pdf_shapes (
    id TEXT PRIMARY KEY NOT NULL,
    paper_id TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
    resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    page INTEGER NOT NULL CHECK (page > 0),
    kind TEXT NOT NULL CHECK (kind IN ('box', 'note')),
    rect_json TEXT NOT NULL,
    note TEXT NOT NULL DEFAULT '',
    color TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_pdf_shapes_source ON pdf_shapes(resource_id, page);
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Underline,
    Highlight,
    /// A hollow box drawn round part of a page.
    Box,
    /// A sticky note at a spot of a page; its text is `note`.
    Note,
}

impl Kind {
    pub fn key(self) -> &'static str {
        match self { Self::Underline => "underline", Self::Highlight => "highlight", Self::Box => "box", Self::Note => "note" }
    }

    /// Placed on the page (`pdf_shapes`), not on its text.
    pub fn shape(self) -> bool {
        matches!(self, Self::Box | Self::Note)
    }
}

/// The colours a highlight or a box can have, stored by these keys; the first is what a new highlight gets.
/// A highlight of a build from before colours reads as the first.
pub const COLORS: &[&str] = &["yellow", "green", "blue", "pink", "purple", "red", "orange"];
/// What a new box is drawn in: it stands out from the highlights round it.
pub const BOX_COLOR: &str = "red";

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Annotation {
    pub id: String,
    pub paper_id: String,
    pub resource_id: String,
    /// 1-based page in this attachment, not the printed page number.
    pub page: usize,
    pub kind: Kind,
    /// Original selected text, empty for a box or a note. The user's interpretation lives in `note`.
    pub quote: String,
    /// One rectangle per selected line: [left, top, right, bottom] in points. A box has its own edges
    /// as the one rectangle, a note the spot it stands on (left = right, top = bottom).
    pub boxes: Vec<[f32; 4]>,
    /// The person's note on the passage, or a sticky note's text.
    pub note: String,
    /// A key of `COLORS` for a highlight or a box; empty for an underline and a note.
    pub color: String,
    pub created_at: String,
    pub updated_at: String,
}

impl Store {
    /// Visible annotations in reading order. Empty filters mean the whole
    /// library; trashed papers retain their marks but are not exposed here.
    pub fn pdf_annotations(&self, paper: &str, resource: &str) -> Result<Vec<Annotation>> {
        let mut stmt = self.db.prepare(
            "SELECT id, paper_id, resource_id, page, kind, quote, boxes_json, note, created_at, updated_at, color FROM (
               SELECT id, paper_id, resource_id, page, kind, quote, boxes_json, note, created_at, updated_at, color FROM pdf_annotations
               UNION ALL
               SELECT id, paper_id, resource_id, page, kind, '', '[' || rect_json || ']', note, created_at, updated_at, color FROM pdf_shapes)
             WHERE (?1 = '' OR paper_id = ?1) AND (?2 = '' OR resource_id = ?2)
             AND paper_id NOT IN (SELECT paper_id FROM trashed_papers)
             ORDER BY paper_id, resource_id, page, created_at, id",
        )?;
        let rows = stmt.query_map(params![paper, resource], |r| {
            let kind: String = r.get(4)?;
            let boxes: String = r.get(6)?;
            let color: String = r.get(10)?;
            let invalid = |i, e| rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e));
            let kind: Kind = serde_json::from_value(serde_json::Value::String(kind)).map_err(|e| invalid(4, e))?;
            let color = match kind {
                Kind::Underline | Kind::Note => String::new(),
                Kind::Highlight | Kind::Box if COLORS.contains(&color.as_str()) => color,
                Kind::Highlight => COLORS[0].into(),
                Kind::Box => BOX_COLOR.into(),
            };
            Ok(Annotation {
                id: r.get(0)?, paper_id: r.get(1)?, resource_id: r.get(2)?, page: r.get::<_, i64>(3)? as usize, kind,
                quote: r.get(5)?, boxes: serde_json::from_str(&boxes).map_err(|e| invalid(6, e))?,
                note: r.get(7)?, color, created_at: r.get(8)?, updated_at: r.get(9)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn add_pdf_annotation(&mut self, paper: &str, resource: &str, page: usize, kind: Kind, quote: &str, boxes: &[[f32; 4]]) -> Result<Annotation> {
        let quote = quote.trim();
        if kind.shape() { bail!("方框和便签用 add_pdf_shape 保存。") }
        if page == 0 || page > i32::MAX as usize || quote.is_empty() || quote.len() > 100_000 { bail!("PDF 标注需要有效的页码和选中文字。") }
        if boxes.is_empty() || boxes.len() > 10_000 || boxes.iter().any(|b| b.iter().any(|n| !n.is_finite()) || b[2] <= b[0] || b[3] <= b[1]) { bail!("PDF 标注的位置无效。") }
        self.check_pdf(paper, resource)?;
        // A repeated click on the same action must not create duplicate marks.
        if let Some(existing) = self.pdf_annotations(paper, resource)?.into_iter().find(|a| a.page == page && a.kind == kind && a.quote == quote && a.boxes == boxes) { return Ok(existing) }
        let now = now_iso();
        let color = if kind == Kind::Highlight { COLORS[0] } else { "" };
        let annotation = Annotation { id: new_id("A"), paper_id: paper.into(), resource_id: resource.into(), page, kind, quote: quote.into(), boxes: boxes.to_vec(), note: String::new(), color: color.into(), created_at: now.clone(), updated_at: now };
        self.db.execute(
            "INSERT INTO pdf_annotations (id, paper_id, resource_id, page, kind, quote, boxes_json, note, color, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '', ?8, ?9, ?9)",
            params![annotation.id, paper, resource, page as i64, kind.key(), quote, serde_json::to_string(boxes)?, color, annotation.created_at],
        )?;
        Ok(annotation)
    }

    fn check_pdf(&self, paper: &str, resource: &str) -> Result<()> {
        let valid: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM resources WHERE id = ?1 AND paper_id = ?2 AND lower(format) = 'pdf'
             AND paper_id NOT IN (SELECT paper_id FROM trashed_papers))", params![resource, paper], |r| r.get(0),
        )?;
        if !valid { bail!("PDF 附件不存在或不属于这篇文献。") }
        Ok(())
    }

    /// A box drawn round part of a page (`rect` its edges), or a sticky note at a spot of it (`rect` that spot twice,
    /// `note` its text). A box is drawn in `BOX_COLOR` until another is chosen.
    pub fn add_pdf_shape(&mut self, paper: &str, resource: &str, page: usize, kind: Kind, rect: [f32; 4], note: &str) -> Result<Annotation> {
        if !kind.shape() { bail!("下划线和高亮用 add_pdf_annotation 保存。") }
        if page == 0 || page > i32::MAX as usize { bail!("PDF 标注需要有效的页码。") }
        let rect = valid_rect(kind, rect)?;
        if note.len() > 100_000 { bail!("便签过长。") }
        self.check_pdf(paper, resource)?;
        let now = now_iso();
        let color = if kind == Kind::Box { BOX_COLOR } else { "" };
        let shape = Annotation { id: new_id("A"), paper_id: paper.into(), resource_id: resource.into(), page, kind, quote: String::new(), boxes: vec![rect], note: note.trim().into(), color: color.into(), created_at: now.clone(), updated_at: now };
        self.db.execute(
            "INSERT INTO pdf_shapes (id, paper_id, resource_id, page, kind, rect_json, note, color, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
            params![shape.id, paper, resource, page as i64, kind.key(), serde_json::to_string(&rect)?, shape.note, color, shape.created_at],
        )?;
        Ok(shape)
    }

    /// A box moved or sized, a note moved: on the same page, to `rect`.
    pub fn move_pdf_shape(&mut self, resource: &str, id: &str, rect: [f32; 4]) -> Result<()> {
        let kind: Option<String> = self.db.query_row("SELECT kind FROM pdf_shapes WHERE id = ?1 AND resource_id = ?2", params![id, resource], |r| r.get(0)).optional()?;
        let kind = match kind.as_deref() { Some("box") => Kind::Box, Some(_) => Kind::Note, None => bail!("PDF 标注不存在。") };
        let rect = valid_rect(kind, rect)?;
        self.db.execute("UPDATE pdf_shapes SET rect_json = ?1, updated_at = ?2 WHERE id = ?3", params![serde_json::to_string(&rect)?, now_iso(), id])?;
        Ok(())
    }

    /// Notes can change; the selected original text and its anchor cannot. A sticky note's text is its note.
    pub fn update_pdf_annotation_note(&mut self, resource: &str, id: &str, note: &str) -> Result<()> {
        if note.len() > 100_000 { bail!("标注笔记过长。") }
        let params = params![note.trim(), now_iso(), id, resource];
        if self.db.execute("UPDATE pdf_annotations SET note = ?1, updated_at = ?2 WHERE id = ?3 AND resource_id = ?4", params)? == 0
            && self.db.execute("UPDATE pdf_shapes SET note = ?1, updated_at = ?2 WHERE id = ?3 AND resource_id = ?4", params)? == 0 { bail!("PDF 标注不存在。") }
        Ok(())
    }

    /// A highlight or a box takes another colour; its text and place stay as they were.
    pub fn recolor_pdf_annotation(&mut self, resource: &str, id: &str, color: &str) -> Result<()> {
        if !COLORS.contains(&color) { bail!("不支持的颜色：{color}") }
        let params = params![color, now_iso(), id, resource];
        if self.db.execute("UPDATE pdf_annotations SET color = ?1, updated_at = ?2 WHERE id = ?3 AND resource_id = ?4 AND kind = 'highlight'", params)? == 0
            && self.db.execute("UPDATE pdf_shapes SET color = ?1, updated_at = ?2 WHERE id = ?3 AND resource_id = ?4 AND kind = 'box'", params)? == 0 { bail!("这条高亮不存在。") }
        Ok(())
    }

    pub fn remove_pdf_annotation(&mut self, resource: &str, id: &str) -> Result<()> {
        if self.db.execute("DELETE FROM pdf_annotations WHERE id = ?1 AND resource_id = ?2", params![id, resource])? == 0
            && self.db.execute("DELETE FROM pdf_shapes WHERE id = ?1 AND resource_id = ?2", params![id, resource])? == 0 { bail!("PDF 标注不存在。") }
        Ok(())
    }
}

/// A shape's place, its corners put in order: a box needs some size, a note is a spot.
fn valid_rect(kind: Kind, [x0, y0, x1, y1]: [f32; 4]) -> Result<[f32; 4]> {
    if [x0, y0, x1, y1].iter().any(|n| !n.is_finite() || n.abs() > 100_000.) { bail!("PDF 标注的位置无效。") }
    match kind {
        Kind::Note => Ok([x0, y0, x0, y0]),
        _ => {
            let rect = [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)];
            if rect[2] - rect[0] < 1. || rect[3] - rect[1] < 1. { bail!("方框太小。") }
            Ok(rect)
        }
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
    fn a_highlight_changes_colour_in_place_and_only_the_one_removed_goes() {
        let root = std::env::temp_dir().join(new_id("wisp-annotation-colour"));
        let source = root.join("source.pdf");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&source, b"%PDF-1.4 test source").unwrap();
        let mut store = Store::in_memory(&root).unwrap();
        let paper = store.add_paper(Paper { title: "Roots".into(), ..Default::default() }).unwrap();
        let file = store.import_file(&paper.id, &source, "fulltext").unwrap();
        let boxes = vec![[10., 20., 80., 30.]];
        let first = store.add_pdf_annotation(&paper.id, &file.id, 1, Kind::Highlight, "Root growth", &boxes).unwrap();
        let second = store.add_pdf_annotation(&paper.id, &file.id, 1, Kind::Highlight, "growth", &[[40., 20., 80., 30.]]).unwrap();
        let line = store.add_pdf_annotation(&paper.id, &file.id, 1, Kind::Underline, "Root", &[[10., 20., 30., 30.]]).unwrap();
        assert_eq!((first.color.as_str(), line.color.as_str()), (COLORS[0], ""));
        store.update_pdf_annotation_note(&file.id, &first.id, "Compare").unwrap();
        store.recolor_pdf_annotation(&file.id, &first.id, "green").unwrap();
        let saved = store.pdf_annotations("", &file.id).unwrap();
        let recoloured = saved.iter().find(|a| a.id == first.id).unwrap();
        assert_eq!((recoloured.color.as_str(), &recoloured.boxes, recoloured.quote.as_str(), recoloured.note.as_str()), ("green", &boxes, "Root growth", "Compare"));
        assert_eq!(saved.iter().find(|a| a.id == second.id).unwrap().color, COLORS[0]);
        assert!(store.recolor_pdf_annotation(&file.id, &first.id, "black").is_err());
        assert!(store.recolor_pdf_annotation(&file.id, &line.id, "green").is_err());
        assert!(store.recolor_pdf_annotation("other", &first.id, "blue").is_err());
        // A highlight of a build from before colours reads as the first colour.
        store.db.execute("UPDATE pdf_annotations SET color = '' WHERE id = ?1", [&second.id]).unwrap();
        assert_eq!(store.pdf_annotations("", &file.id).unwrap().iter().find(|a| a.id == second.id).unwrap().color, COLORS[0]);
        store.remove_pdf_annotation(&file.id, &second.id).unwrap();
        let mut left: Vec<_> = store.pdf_annotations("", &file.id).unwrap().into_iter().map(|a| (a.id, a.color)).collect();
        left.sort();
        let mut kept = vec![(first.id, "green".to_string()), (line.id, String::new())];
        kept.sort();
        assert_eq!(left, kept);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn boxes_and_notes_are_kept_moved_recoloured_and_removed_beside_the_marks_on_text() {
        let root = std::env::temp_dir().join(new_id("wisp-annotation-shapes"));
        let source = root.join("source.pdf");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&source, b"%PDF-1.4 test source").unwrap();
        let mut store = Store::in_memory(&root).unwrap();
        let paper = store.add_paper(Paper { title: "Roots".into(), ..Default::default() }).unwrap();
        let file = store.import_file(&paper.id, &source, "fulltext").unwrap();
        let mark = store.add_pdf_annotation(&paper.id, &file.id, 1, Kind::Highlight, "Root", &[[10., 20., 30., 30.]]).unwrap();
        // Drawn from the bottom right up: the corners are put in order.
        let frame = store.add_pdf_shape(&paper.id, &file.id, 2, Kind::Box, [200., 300., 50., 100.], "").unwrap();
        let note = store.add_pdf_shape(&paper.id, &file.id, 1, Kind::Note, [40., 60., 90., 99.], " Compare with Fig. 2 ").unwrap();
        assert_eq!((frame.boxes[0], frame.color.as_str()), ([50., 100., 200., 300.], BOX_COLOR));
        assert_eq!((note.boxes[0], note.note.as_str(), note.color.as_str()), ([40., 60., 40., 60.], "Compare with Fig. 2", ""));
        assert!(store.add_pdf_shape(&paper.id, &file.id, 1, Kind::Box, [1., 1., 1.5, 40.], "").is_err());
        assert!(store.add_pdf_shape(&paper.id, &file.id, 1, Kind::Highlight, [1., 1., 40., 40.], "").is_err());
        assert!(store.add_pdf_annotation(&paper.id, &file.id, 1, Kind::Box, "x", &[[1., 1., 40., 40.]]).is_err());
        assert!(store.add_pdf_shape(&paper.id, "missing", 1, Kind::Note, [1., 1., 1., 1.], "").is_err());
        let all = store.pdf_annotations(&paper.id, &file.id).unwrap();
        // By page; the highlight and the note of page 1 are made in the same instant, so their order is the ids' and not fixed.
        let mut first: Vec<String> = all[..2].iter().map(|a| a.kind.key().to_string()).collect();
        first.sort();
        assert_eq!((all.iter().map(|a| a.page).collect::<Vec<_>>(), first, all[2].kind), (vec![1, 1, 2], vec![Kind::Highlight.key().to_string(), Kind::Note.key().to_string()], Kind::Box));
        store.move_pdf_shape(&file.id, &frame.id, [60., 110., 210., 320.]).unwrap();
        store.move_pdf_shape(&file.id, &note.id, [70., 80., 0., 0.]).unwrap();
        store.recolor_pdf_annotation(&file.id, &frame.id, "orange").unwrap();
        store.update_pdf_annotation_note(&file.id, &note.id, "Rewritten").unwrap();
        assert!(store.recolor_pdf_annotation(&file.id, &note.id, "green").is_err());
        assert!(store.move_pdf_shape(&file.id, &mark.id, [1., 1., 40., 40.]).is_err());
        let all = store.pdf_annotations("", &file.id).unwrap();
        let find = |id: &str| all.iter().find(|a| a.id == id).unwrap().clone();
        assert_eq!((find(&frame.id).boxes[0], find(&frame.id).color), ([60., 110., 210., 320.], "orange".to_string()));
        assert_eq!((find(&note.id).boxes[0], find(&note.id).note), ([70., 80., 70., 80.], "Rewritten".to_string()));
        store.remove_pdf_annotation(&file.id, &note.id).unwrap();
        assert!(store.remove_pdf_annotation(&file.id, &note.id).is_err());
        assert_eq!(store.pdf_annotations("", "").unwrap().len(), 2);
        store.trash_paper(&paper.id).unwrap();
        assert!(store.pdf_annotations("", "").unwrap().is_empty());
        store.restore_paper(&paper.id).unwrap();
        store.remove_file(&file.id).unwrap();
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
