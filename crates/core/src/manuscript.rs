//! 写作's manuscripts: any number per library, each with a title, the
//! language it is written in and an ordered list of sections. A section has
//! the person's own name and a `kind` (a key of `structure::SECTIONS`) that
//! decides which part of the library's papers its candidate sentences come
//! from. A section's text is kept as it is typed; snapshots of it are kept
//! on a pause or when the person leaves it, so an earlier version can be
//! looked at and restored. Export turns a manuscript into Markdown, plain
//! text or a Word file, its section names becoming headings.

use crate::model::new_id;
use crate::store::Store;
use crate::structure::SECTIONS;
use crate::time::now_iso;
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

/// The tables, made with the library's own (`store::SCHEMA`). An older build
/// does not know them and reads the rest of the database as before.
pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS manuscripts (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    -- What it is written in: a short code of `LANGUAGES`.
    language TEXT NOT NULL DEFAULT 'en',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    -- Set when the person removed it: it is kept, only not shown.
    removed_at TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS manuscript_sections (
    id TEXT PRIMARY KEY NOT NULL,
    manuscript_id TEXT NOT NULL REFERENCES manuscripts(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    name TEXT NOT NULL,
    -- A key of `structure::SECTIONS`: where its candidates come from.
    kind TEXT NOT NULL,
    body TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_manuscript_sections ON manuscript_sections(manuscript_id, position);
-- Earlier versions of a section's text, newest last (`KEEP_SNAPSHOTS` at most).
CREATE TABLE IF NOT EXISTS section_snapshots (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    section_id TEXT NOT NULL REFERENCES manuscript_sections(id) ON DELETE CASCADE,
    body TEXT NOT NULL,
    -- pause, leave or restore: what made it be kept.
    why TEXT NOT NULL DEFAULT '',
    taken_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_section_snapshots ON section_snapshots(section_id, id);
"#;

/// The languages a manuscript can be written in: (code, name).
pub const LANGUAGES: [(&str, &str); 6] = [("en", "英文"), ("zh", "中文"), ("de", "德文"), ("fr", "法文"), ("es", "西班牙文"), ("ja", "日文")];

/// A new manuscript's language.
pub const DEFAULT_LANGUAGE: &str = "en";

/// A new manuscript's title until the person gives it one.
pub const UNTITLED: &str = "未命名稿件";

/// The title of the manuscript made of the drafts of builds before manuscripts.
pub const FROM_DRAFTS: &str = "早先的草稿";

/// How many snapshots a section keeps; the oldest go first.
pub const KEEP_SNAPSHOTS: usize = 40;

/// How long typing stops before the text is kept as a snapshot, in seconds.
pub const PAUSE_SECS: u64 = 30;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Manuscript {
    pub id: String,
    pub title: String,
    /// A code of `LANGUAGES`.
    pub language: String,
    pub created_at: String,
    pub updated_at: String,
    /// In the order they are written.
    pub sections: Vec<Section>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Section {
    pub id: String,
    pub name: String,
    /// A key of `structure::SECTIONS`.
    pub kind: String,
    pub body: String,
    pub updated_at: String,
}

/// An earlier version of a section's text.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub id: i64,
    pub section_id: String,
    pub body: String,
    /// pause, leave or restore.
    pub why: String,
    pub taken_at: String,
}

impl Manuscript {
    pub fn section(&self, id: &str) -> Option<&Section> {
        self.sections.iter().find(|section| section.id == id)
    }

    /// The title, or what stands for it while there is none.
    pub fn shown_title(&self) -> &str {
        if self.title.trim().is_empty() { UNTITLED } else { self.title.trim() }
    }
}

/// The name a section of this kind starts with in a manuscript in this language.
pub fn default_name(kind: &str, language: &str) -> String {
    if language == "zh" { return crate::model::label(&SECTIONS, kind).to_string() }
    match kind {
        "abstract" => "Abstract",
        "introduction" => "Introduction",
        "methods" => "Methods",
        "results" => "Results",
        "discussion" => "Discussion",
        _ => "Section",
    }
    .to_string()
}

/// The sections a new manuscript starts with: IMRaD with its abstract.
pub fn imrad(language: &str) -> Vec<(String, &'static str)> {
    SECTIONS.iter().take(5).map(|(kind, _)| (default_name(kind, language), *kind)).collect()
}

pub fn is_kind(kind: &str) -> bool {
    SECTIONS.iter().any(|(key, _)| *key == kind)
}

pub fn is_language(code: &str) -> bool {
    LANGUAGES.iter().any(|(key, _)| *key == code)
}

/// Whether a section's text is worth a snapshot, given the newest one kept:
/// only when it changed, and an empty section with no history has nothing to keep.
pub fn worth_keeping(latest: Option<&str>, body: &str) -> bool {
    match latest {
        Some(latest) => latest.trim_end() != body.trim_end(),
        None => !body.trim().is_empty(),
    }
}

// ---- export

fn sections_with_text(manuscript: &Manuscript) -> impl Iterator<Item = (&str, &str)> {
    manuscript.sections.iter().map(|section| (section.name.trim(), section.body.trim())).filter(|(_, body)| !body.is_empty())
}

/// The paragraphs of a text: each line that is not blank.
pub fn paragraphs(text: &str) -> impl Iterator<Item = &str> {
    text.lines().map(str::trim).filter(|line| !line.is_empty())
}

/// The manuscript as Markdown: the title, then each section with text under its name.
pub fn markdown(manuscript: &Manuscript) -> String {
    let mut out = format!("# {}\n", manuscript.shown_title());
    for (name, body) in sections_with_text(manuscript) {
        out.push_str(&format!("\n## {}\n\n", if name.is_empty() { "Section" } else { name }));
        out.push_str(&paragraphs(body).collect::<Vec<_>>().join("\n\n"));
        out.push('\n');
    }
    out
}

/// The manuscript as plain text, for the clipboard: headings on lines of their own.
pub fn plain_text(manuscript: &Manuscript) -> String {
    let mut out = format!("{}\n", manuscript.shown_title());
    for (name, body) in sections_with_text(manuscript) {
        out.push_str(&format!("\n{name}\n\n"));
        out.push_str(&paragraphs(body).collect::<Vec<_>>().join("\n\n"));
        out.push('\n');
    }
    out
}

fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            // Control characters other than tab are not allowed in XML 1.0.
            c if (c as u32) < 0x20 && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

/// The manuscript as a Word document (.docx): a title, a Heading 1 for
/// each section, a paragraph for each line. Written by hand, with the zip
/// entries stored: Word reads a docx without compression.
pub fn docx(manuscript: &Manuscript) -> Vec<u8> {
    let paragraph = |style: &str, text: &str| {
        let style = if style.is_empty() { String::new() } else { format!("<w:pPr><w:pStyle w:val=\"{style}\"/></w:pPr>") };
        format!("<w:p>{style}<w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>", xml_escape(text))
    };
    let mut body = paragraph("Title", manuscript.shown_title());
    for (name, text) in sections_with_text(manuscript) {
        body.push_str(&paragraph("Heading1", name));
        for line in paragraphs(text) { body.push_str(&paragraph("", line)) }
    }
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body>{body}<w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/><w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" w:header=\"708\" w:footer=\"708\" w:gutter=\"0\"/></w:sectPr></w:body></w:document>"
    );
    let lang = match manuscript.language.as_str() {
        "zh" => "zh-CN",
        "en" | "" => "en-US",
        other => other,
    };
    let styles = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
<w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val=\"24\"/><w:lang w:val=\"{lang}\" w:eastAsia=\"zh-CN\"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after=\"160\" w:line=\"360\" w:lineRule=\"auto\"/></w:pPr></w:pPrDefault></w:docDefaults>\
<w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/><w:qFormat/></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Title\"><w:name w:val=\"Title\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/><w:pPr><w:spacing w:after=\"320\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"40\"/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Heading1\"><w:name w:val=\"heading 1\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/><w:pPr><w:keepNext/><w:spacing w:before=\"360\" w:after=\"120\"/><w:outlineLvl w:val=\"0\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"30\"/></w:rPr></w:style>\
</w:styles>"
    );
    let content_types = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/><Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/></Types>";
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>";
    let document_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/></Relationships>";
    stored_zip(&[("[Content_Types].xml", content_types.as_bytes()), ("_rels/.rels", rels.as_bytes()), ("word/document.xml", document.as_bytes()), ("word/_rels/document.xml.rels", document_rels.as_bytes()), ("word/styles.xml", styles.as_bytes())])
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 { crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 } }
    }
    !crc
}

/// A zip archive of these files, stored without compression.
fn stored_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let (mut out, mut central) = (Vec::new(), Vec::new());
    let u16le = |v: &mut Vec<u8>, n: u16| v.extend_from_slice(&n.to_le_bytes());
    let u32le = |v: &mut Vec<u8>, n: u32| v.extend_from_slice(&n.to_le_bytes());
    for (name, data) in files {
        let (offset, crc, size) = (out.len() as u32, crc32(data), data.len() as u32);
        // Local header: version 2.0, UTF-8 names (bit 11), stored, 1980-01-01.
        u32le(&mut out, 0x0403_4b50);
        for n in [20, 0x0800, 0, 0, 0x21] { u16le(&mut out, n) }
        for n in [crc, size, size] { u32le(&mut out, n) }
        u16le(&mut out, name.len() as u16);
        u16le(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        u32le(&mut central, 0x0201_4b50);
        for n in [20, 20, 0x0800, 0, 0, 0x21] { u16le(&mut central, n) }
        for n in [crc, size, size] { u32le(&mut central, n) }
        for n in [name.len() as u16, 0, 0, 0, 0] { u16le(&mut central, n) }
        u32le(&mut central, 0);
        u32le(&mut central, offset);
        central.extend_from_slice(name.as_bytes());
    }
    let (start, length) = (out.len() as u32, central.len() as u32);
    out.extend_from_slice(&central);
    u32le(&mut out, 0x0605_4b50);
    for n in [0, 0, files.len() as u16, files.len() as u16] { u16le(&mut out, n) }
    u32le(&mut out, length);
    u32le(&mut out, start);
    u16le(&mut out, 0);
    out
}

// ---- migration

/// The drafts of builds before manuscripts (one text per part, in the
/// `drafts` table) become the sections of one manuscript, and leave that
/// table. An older build opening the library afterwards finds no drafts,
/// and what it writes there is taken in as another manuscript next time.
pub(crate) fn migrate_drafts(db: &mut Connection) -> Result<()> {
    let drafts: Vec<(String, String)> = {
        let mut stmt = db.prepare("SELECT section, body FROM drafts")?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?
    };
    if drafts.is_empty() { return Ok(()) }
    let tx = db.transaction()?;
    if drafts.iter().any(|(_, body)| !body.trim().is_empty()) {
        let body = |kind: &str| drafts.iter().find(|(section, _)| section == kind).map(|(_, body)| body.clone()).unwrap_or_default();
        let mut sections: Vec<(String, String, String)> = imrad(DEFAULT_LANGUAGE).into_iter().map(|(name, kind)| (name, kind.to_string(), body(kind))).collect();
        // Parts the five do not cover (`other`, or a key of a later build) are kept as sections of their own.
        for (section, text) in &drafts {
            if sections.iter().any(|(_, kind, _)| kind == section) || text.trim().is_empty() { continue }
            let kind = if is_kind(section) { section.clone() } else { "other".to_string() };
            sections.push((if section == "other" { "Other".into() } else { section.clone() }, kind, text.clone()));
        }
        let (id, now) = (new_id("M"), now_iso());
        tx.execute("INSERT INTO manuscripts(id, title, language, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)", params![id, FROM_DRAFTS, DEFAULT_LANGUAGE, now])?;
        for (position, (name, kind, body)) in sections.iter().enumerate() {
            tx.execute("INSERT INTO manuscript_sections(id, manuscript_id, position, name, kind, body, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)", params![new_id("S"), id, position as i64, name, kind, body, now])?;
        }
    }
    tx.execute("DELETE FROM drafts", [])?;
    tx.commit()?;
    Ok(())
}

// ---- the store

fn read_section(r: &rusqlite::Row) -> rusqlite::Result<Section> {
    Ok(Section { id: r.get(0)?, name: r.get(1)?, kind: r.get(2)?, body: r.get(3)?, updated_at: r.get(4)? })
}

impl Store {
    /// Create a manuscript from prepared sections and pin the citation keys
    /// in the same transaction. A failure leaves neither a partial manuscript
    /// nor newly reserved keys. `keys` holds (paper_id, citation_key).
    pub fn create_manuscript_from_sections(&mut self, title: &str, language: &str, sections: &[(String, String)], keys: &[(String, String)]) -> Result<Manuscript> {
        if !is_language(language) { bail!("不认识这种语言：{language}") }
        if sections.is_empty() || sections.iter().any(|(name, _)| name.trim().is_empty()) { bail!("稿件需要有名称的章节") }
        let (id, now) = (new_id("M"), now_iso());
        let tx = self.db.transaction()?;
        for (paper_id, key) in keys {
            if key.trim().is_empty() { bail!("引用键不能为空") }
            let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM papers WHERE id = ?1)", [paper_id], |r| r.get(0))?;
            if !exists { bail!("引用的文献已不存在：{paper_id}") }
            tx.execute("INSERT OR IGNORE INTO cite_keys(key, paper_id, made_at) VALUES (?1, ?2, ?3)", params![key, paper_id, now])?;
            let actual: String = tx.query_row("SELECT paper_id FROM cite_keys WHERE key = ?1", [key], |r| r.get(0))?;
            if &actual != paper_id { bail!("引用键与其他文献冲突：{key}") }
        }
        tx.execute("INSERT INTO manuscripts(id, title, language, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)", params![id, title.trim(), language, now])?;
        for (position, (name, body)) in sections.iter().enumerate() {
            tx.execute("INSERT INTO manuscript_sections(id, manuscript_id, position, name, kind, body, updated_at) VALUES (?1, ?2, ?3, ?4, 'other', ?5, ?6)", params![new_id("S"), id, position as i64, name.trim(), body, now])?;
        }
        tx.commit()?;
        self.manuscript(&id)?.ok_or_else(|| anyhow::anyhow!("新稿件保存后未找到"))
    }

    /// The manuscripts of the library that were not removed, oldest first, with their sections.
    pub fn manuscripts(&self) -> Result<Vec<Manuscript>> {
        let mut stmt = self.db.prepare("SELECT id, title, language, created_at, updated_at FROM manuscripts WHERE removed_at = '' ORDER BY created_at, id")?;
        let mut list = stmt.query_map([], |r| Ok(Manuscript { id: r.get(0)?, title: r.get(1)?, language: r.get(2)?, created_at: r.get(3)?, updated_at: r.get(4)?, sections: vec![] }))?.collect::<rusqlite::Result<Vec<_>>>()?;
        for manuscript in &mut list { manuscript.sections = self.sections_of(&manuscript.id)? }
        Ok(list)
    }

    pub fn manuscript(&self, id: &str) -> Result<Option<Manuscript>> {
        let found = self.db.query_row("SELECT id, title, language, created_at, updated_at FROM manuscripts WHERE id = ?1 AND removed_at = ''", [id], |r| Ok(Manuscript { id: r.get(0)?, title: r.get(1)?, language: r.get(2)?, created_at: r.get(3)?, updated_at: r.get(4)?, sections: vec![] })).optional()?;
        let Some(mut manuscript) = found else { return Ok(None) };
        manuscript.sections = self.sections_of(id)?;
        Ok(Some(manuscript))
    }

    fn sections_of(&self, manuscript_id: &str) -> Result<Vec<Section>> {
        let mut stmt = self.db.prepare("SELECT id, name, kind, body, updated_at FROM manuscript_sections WHERE manuscript_id = ?1 ORDER BY position, id")?;
        Ok(stmt.query_map([manuscript_id], read_section)?.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn section_row(&self, id: &str) -> Result<(String, Section)> {
        let found = self.db.query_row("SELECT id, name, kind, body, updated_at, manuscript_id FROM manuscript_sections WHERE id = ?1", [id], |r| Ok((r.get(5)?, read_section(r)?))).optional()?;
        found.ok_or_else(|| anyhow::anyhow!("没有这一节：{id}"))
    }

    fn touch_manuscript(&self, id: &str) -> Result<()> {
        self.db.execute("UPDATE manuscripts SET updated_at = ?2 WHERE id = ?1", params![id, now_iso()])?;
        Ok(())
    }

    /// A new manuscript with the IMRaD sections, named in its language.
    pub fn create_manuscript(&mut self, title: &str, language: &str) -> Result<Manuscript> {
        if !is_language(language) { bail!("不认识这种语言：{language}") }
        let (id, now) = (new_id("M"), now_iso());
        let tx = self.db.transaction()?;
        tx.execute("INSERT INTO manuscripts(id, title, language, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)", params![id, title.trim(), language, now])?;
        for (position, (name, kind)) in imrad(language).into_iter().enumerate() {
            tx.execute("INSERT INTO manuscript_sections(id, manuscript_id, position, name, kind, body, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, '', ?6)", params![new_id("S"), id, position as i64, name, kind, now])?;
        }
        tx.commit()?;
        Ok(self.manuscript(&id)?.unwrap_or_default())
    }

    pub fn rename_manuscript(&mut self, id: &str, title: &str) -> Result<()> {
        self.db.execute("UPDATE manuscripts SET title = ?2, updated_at = ?3 WHERE id = ?1", params![id, title.trim(), now_iso()])?;
        Ok(())
    }

    pub fn set_manuscript_language(&mut self, id: &str, language: &str) -> Result<()> {
        if !is_language(language) { bail!("不认识这种语言：{language}") }
        self.db.execute("UPDATE manuscripts SET language = ?2, updated_at = ?3 WHERE id = ?1", params![id, language, now_iso()])?;
        Ok(())
    }

    /// Put a manuscript away: it stays in the database, only no longer listed.
    pub fn remove_manuscript(&mut self, id: &str) -> Result<()> {
        self.db.execute("UPDATE manuscripts SET removed_at = ?2 WHERE id = ?1", params![id, now_iso()])?;
        Ok(())
    }

    /// A section added at the end of a manuscript.
    pub fn add_section(&mut self, manuscript_id: &str, name: &str, kind: &str) -> Result<Section> {
        if !is_kind(kind) { bail!("没有这种部分：{kind}") }
        if name.trim().is_empty() { bail!("这一节需要一个名称。") }
        let position: i64 = self.db.query_row("SELECT coalesce(max(position) + 1, 0) FROM manuscript_sections WHERE manuscript_id = ?1", [manuscript_id], |r| r.get(0))?;
        let id = new_id("S");
        self.db.execute("INSERT INTO manuscript_sections(id, manuscript_id, position, name, kind, body, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, '', ?6)", params![id, manuscript_id, position, name.trim(), kind, now_iso()])?;
        self.touch_manuscript(manuscript_id)?;
        Ok(self.section_row(&id)?.1)
    }

    /// Give a section another name and kind.
    pub fn edit_section(&mut self, id: &str, name: &str, kind: &str) -> Result<()> {
        if !is_kind(kind) { bail!("没有这种部分：{kind}") }
        if name.trim().is_empty() { bail!("这一节需要一个名称。") }
        let (manuscript, _) = self.section_row(id)?;
        self.db.execute("UPDATE manuscript_sections SET name = ?2, kind = ?3 WHERE id = ?1", params![id, name.trim(), kind])?;
        self.touch_manuscript(&manuscript)
    }

    /// Move a section by `by` places (negative: towards the start), within the manuscript.
    pub fn move_section(&mut self, id: &str, by: i64) -> Result<()> {
        let (manuscript, _) = self.section_row(id)?;
        let mut order: Vec<String> = self.sections_of(&manuscript)?.into_iter().map(|section| section.id).collect();
        let Some(from) = order.iter().position(|other| other == id) else { return Ok(()) };
        let to = (from as i64 + by).clamp(0, order.len() as i64 - 1) as usize;
        if to == from { return Ok(()) }
        let moved = order.remove(from);
        order.insert(to, moved);
        let tx = self.db.transaction()?;
        for (position, section) in order.iter().enumerate() { tx.execute("UPDATE manuscript_sections SET position = ?2 WHERE id = ?1", params![section, position as i64])?; }
        tx.commit()?;
        self.touch_manuscript(&manuscript)
    }

    /// Remove a section with its snapshots. The window asks first when it has text.
    pub fn remove_section(&mut self, id: &str) -> Result<()> {
        let (manuscript, _) = self.section_row(id)?;
        self.db.execute("DELETE FROM manuscript_sections WHERE id = ?1", [id])?;
        self.touch_manuscript(&manuscript)
    }

    /// Keep what is typed in a section.
    pub fn save_section_body(&mut self, id: &str, body: &str) -> Result<()> {
        let changed = self.db.execute("UPDATE manuscript_sections SET body = ?2, updated_at = ?3 WHERE id = ?1", params![id, body, now_iso()])?;
        if changed == 0 { bail!("没有这一节：{id}") }
        Ok(())
    }

    /// Add text at the end of a section, after keeping what it held as a
    /// snapshot, and keep the citation keys the text uses with their papers
    /// (`keys`: paper and key). The text the section now has.
    pub fn append_to_section(&mut self, id: &str, text: &str, keys: &[(String, String)]) -> Result<String> {
        if text.trim().is_empty() { bail!("没有可以加进稿件的文字") }
        self.take_snapshot(id, "insert")?;
        let now = now_iso();
        let tx = self.db.transaction()?;
        let Some(body) = tx.query_row("SELECT body FROM manuscript_sections WHERE id = ?1", [id], |r| r.get::<_, String>(0)).optional()? else { bail!("没有这一节：{id}") };
        for (paper_id, key) in keys {
            tx.execute("INSERT OR IGNORE INTO cite_keys(key, paper_id, made_at) VALUES (?1, ?2, ?3)", params![key, paper_id, now])?;
            let actual: String = tx.query_row("SELECT paper_id FROM cite_keys WHERE key = ?1", [key], |r| r.get(0))?;
            if &actual != paper_id { bail!("引用键与其他文献冲突：{key}") }
        }
        let body = if body.trim().is_empty() { text.trim().to_string() } else { format!("{}\n\n{}", body.trim_end(), text.trim()) };
        tx.execute("UPDATE manuscript_sections SET body = ?2, updated_at = ?3 WHERE id = ?1", params![id, body, now])?;
        tx.commit()?;
        Ok(body)
    }

    /// A section's snapshots, newest first.
    pub fn snapshots(&self, section_id: &str) -> Result<Vec<Snapshot>> {
        let mut stmt = self.db.prepare("SELECT id, section_id, body, why, taken_at FROM section_snapshots WHERE section_id = ?1 ORDER BY id DESC")?;
        Ok(stmt.query_map([section_id], |r| Ok(Snapshot { id: r.get(0)?, section_id: r.get(1)?, body: r.get(2)?, why: r.get(3)?, taken_at: r.get(4)? }))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Keep the section's saved text as a snapshot when it is `worth_keeping`;
    /// the oldest beyond `KEEP_SNAPSHOTS` go. Whether one was taken.
    pub fn take_snapshot(&mut self, section_id: &str, why: &str) -> Result<bool> {
        let Some(body) = self.db.query_row("SELECT body FROM manuscript_sections WHERE id = ?1", [section_id], |r| r.get::<_, String>(0)).optional()? else { return Ok(false) };
        let latest: Option<String> = self.db.query_row("SELECT body FROM section_snapshots WHERE section_id = ?1 ORDER BY id DESC LIMIT 1", [section_id], |r| r.get(0)).optional()?;
        if !worth_keeping(latest.as_deref(), &body) { return Ok(false) }
        self.db.execute("INSERT INTO section_snapshots(section_id, body, why, taken_at) VALUES (?1, ?2, ?3, ?4)", params![section_id, body, why, now_iso()])?;
        self.db.execute("DELETE FROM section_snapshots WHERE section_id = ?1 AND id NOT IN (SELECT id FROM section_snapshots WHERE section_id = ?1 ORDER BY id DESC LIMIT ?2)", params![section_id, KEEP_SNAPSHOTS as i64])?;
        Ok(true)
    }

    /// Put a snapshot's text back into its section, after keeping the
    /// current text as a snapshot of its own, so the restoring can be undone.
    /// The text the section now has.
    pub fn restore_snapshot(&mut self, snapshot_id: i64) -> Result<(String, String)> {
        let found = self.db.query_row("SELECT section_id, body FROM section_snapshots WHERE id = ?1", [snapshot_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).optional()?;
        let Some((section, body)) = found else { bail!("这个版本已经不在了。") };
        self.take_snapshot(&section, "restore")?;
        self.save_section_body(&section, &body)?;
        Ok((section, body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn store() -> Store {
        Store::in_memory(Path::new("/tmp/wisp-manuscript-test")).unwrap()
    }

    #[test]
    fn text_is_added_at_a_sections_end_with_its_keys() {
        let mut store = store();
        let paper = store.add_paper(crate::model::Paper { title: "Roots".into(), ..Default::default() }).unwrap();
        let other = store.add_paper(crate::model::Paper { title: "Leaves".into(), ..Default::default() }).unwrap();
        let made = store.create_manuscript("Draft", "en").unwrap();
        let section = made.sections[1].id.clone();
        assert_eq!(store.append_to_section(&section, " First [@Root2024]. \n", &[(paper.id.clone(), "Root2024".into())]).unwrap(), "First [@Root2024].");
        assert_eq!(store.append_to_section(&section, "Second.", &[]).unwrap(), "First [@Root2024].\n\nSecond.");
        assert_eq!(store.cite_keys().unwrap().get("Root2024"), Some(&paper.id));
        // What the section held before the second addition can be brought back.
        assert_eq!(store.snapshots(&section).unwrap()[0].body, "First [@Root2024].");
        // A key that is another paper's stops the whole addition.
        assert!(store.append_to_section(&section, "Third [@Root2024].", &[(other.id.clone(), "Root2024".into())]).is_err());
        assert_eq!(store.manuscript(&made.id).unwrap().unwrap().sections[1].body, "First [@Root2024].\n\nSecond.");
        assert!(store.append_to_section(&section, "  ", &[]).is_err());
        assert!(store.append_to_section("nowhere", "Text.", &[]).is_err());
    }

    #[test]
    fn imported_sections_and_citation_keys_commit_together() {
        let mut store = store();
        let paper = store.add_paper(crate::model::Paper { title: "Roots".into(), ..Default::default() }).unwrap();
        let other = store.add_paper(crate::model::Paper { title: "Leaves".into(), ..Default::default() }).unwrap();
        let sections = vec![("Introduction".into(), "Text [@Root2024, p. 5]".into()), ("Conclusion".into(), "More".into())];
        let bad = vec![(paper.id.clone(), "Root2024".into()), ("missing".into(), "Missing2024".into())];
        assert!(store.create_manuscript_from_sections("Review", "en", &sections, &bad).is_err());
        assert!(store.manuscripts().unwrap().is_empty());
        assert!(store.cite_keys().unwrap().is_empty(), "an earlier reserved key must roll back");

        let conflict = vec![(paper.id.clone(), "Root2024".into()), (other.id.clone(), "root2024".into())];
        assert!(store.create_manuscript_from_sections("Review", "en", &sections, &conflict).is_err());
        assert!(store.manuscripts().unwrap().is_empty() && store.cite_keys().unwrap().is_empty());

        let made = store.create_manuscript_from_sections("Review", "en", &sections, &[(paper.id.clone(), "Root2024".into())]).unwrap();
        assert_eq!(made.sections.iter().map(|s| (s.name.clone(), s.body.clone())).collect::<Vec<_>>(), sections);
        assert_eq!(store.cite_keys().unwrap().get("Root2024"), Some(&paper.id));
    }

    #[test]
    fn a_new_manuscript_has_imrad_in_its_language() {
        let mut store = store();
        let en = store.create_manuscript("", "en").unwrap();
        assert_eq!(en.sections.iter().map(|s| (s.name.as_str(), s.kind.as_str())).collect::<Vec<_>>(), [("Abstract", "abstract"), ("Introduction", "introduction"), ("Methods", "methods"), ("Results", "results"), ("Discussion", "discussion")]);
        assert_eq!(en.shown_title(), UNTITLED);
        let zh = store.create_manuscript("根系", "zh").unwrap();
        assert_eq!(zh.sections[1].name, "导言");
        assert!(store.create_manuscript("x", "klingon").is_err());
        assert_eq!(store.manuscripts().unwrap().len(), 2);
        store.remove_manuscript(&en.id).unwrap();
        assert_eq!(store.manuscripts().unwrap().iter().map(|m| m.title.as_str()).collect::<Vec<_>>(), ["根系"]);
    }

    #[test]
    fn sections_can_be_added_renamed_moved_and_removed() {
        let mut store = store();
        let review = store.create_manuscript("A review", "en").unwrap();
        for section in &review.sections[2..] { store.remove_section(&section.id).unwrap() }
        let roots = store.add_section(&review.id, "Root architecture", "other").unwrap();
        let hairs = store.add_section(&review.id, "Root hairs", "other").unwrap();
        assert!(store.add_section(&review.id, "x", "preface").is_err());
        assert!(store.add_section(&review.id, "  ", "other").is_err());
        store.move_section(&hairs.id, -1).unwrap();
        store.move_section(&hairs.id, -10).unwrap();
        store.edit_section(&roots.id, "Architecture", "results").unwrap();
        let names: Vec<(String, String)> = store.manuscript(&review.id).unwrap().unwrap().sections.into_iter().map(|s| (s.name, s.kind)).collect();
        assert_eq!(names, [("Root hairs".to_string(), "other".to_string()), ("Abstract".into(), "abstract".into()), ("Introduction".into(), "introduction".into()), ("Architecture".into(), "results".into())]);
    }

    #[test]
    fn snapshots_are_kept_only_on_change_bounded_and_restoring_is_undoable() {
        let mut store = store();
        let manuscript = store.create_manuscript("", "en").unwrap();
        let section = manuscript.sections[1].id.clone();
        assert!(!store.take_snapshot(&section, "pause").unwrap(), "nothing written, nothing kept");
        store.save_section_body(&section, "Roots grow.").unwrap();
        assert!(store.take_snapshot(&section, "pause").unwrap());
        assert!(!store.take_snapshot(&section, "leave").unwrap(), "unchanged");
        store.save_section_body(&section, "Roots grow down.").unwrap();
        assert!(store.take_snapshot(&section, "leave").unwrap());
        let first = store.snapshots(&section).unwrap().last().unwrap().clone();
        assert_eq!(first.body, "Roots grow.");
        store.save_section_body(&section, "Roots grow down. Typed after.").unwrap();
        let (restored_in, body) = store.restore_snapshot(first.id).unwrap();
        assert_eq!((restored_in.as_str(), body.as_str()), (section.as_str(), "Roots grow."));
        let newest = store.snapshots(&section).unwrap()[0].clone();
        assert_eq!((newest.body.as_str(), newest.why.as_str()), ("Roots grow down. Typed after.", "restore"));
        assert_eq!(store.manuscript(&manuscript.id).unwrap().unwrap().sections[1].body, "Roots grow.");
        for n in 0..KEEP_SNAPSHOTS + 5 {
            store.save_section_body(&section, &format!("version {n}")).unwrap();
            store.take_snapshot(&section, "pause").unwrap();
        }
        let kept = store.snapshots(&section).unwrap();
        assert_eq!(kept.len(), KEEP_SNAPSHOTS);
        assert_eq!(kept[0].body, format!("version {}", KEEP_SNAPSHOTS + 4));
        store.remove_section(&section).unwrap();
        assert!(store.snapshots(&section).unwrap().is_empty());
    }

    #[test]
    fn the_policy() {
        assert!(!worth_keeping(None, "  \n"));
        assert!(worth_keeping(None, "a"));
        assert!(!worth_keeping(Some("a\n"), "a"));
        assert!(worth_keeping(Some("a"), ""));
    }

    #[test]
    fn old_drafts_become_one_manuscript() {
        let mut store = store();
        store.db.execute_batch("INSERT INTO drafts(section, body, updated_at) VALUES ('introduction', 'Roots are long.', 'x'), ('discussion', '', 'x'), ('other', 'Notes.', 'x'), ('preface', 'Later key.', 'x')").unwrap();
        migrate_drafts(&mut store.db).unwrap();
        let list = store.manuscripts().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, FROM_DRAFTS);
        let sections: Vec<(&str, &str, &str)> = list[0].sections.iter().map(|s| (s.name.as_str(), s.kind.as_str(), s.body.as_str())).collect();
        assert_eq!(sections, [("Abstract", "abstract", ""), ("Introduction", "introduction", "Roots are long."), ("Methods", "methods", ""), ("Results", "results", ""), ("Discussion", "discussion", ""), ("Other", "other", "Notes."), ("preface", "other", "Later key.")]);
        let left: i64 = store.db.query_row("SELECT count(*) FROM drafts", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 0);
        // Run again (every open does): nothing more.
        migrate_drafts(&mut store.db).unwrap();
        assert_eq!(store.manuscripts().unwrap().len(), 1);
        // Drafts that are all empty make no manuscript, and go.
        store.db.execute_batch("INSERT INTO drafts(section, body, updated_at) VALUES ('methods', ' ', 'x')").unwrap();
        migrate_drafts(&mut store.db).unwrap();
        assert_eq!(store.manuscripts().unwrap().len(), 1);
    }

    #[test]
    fn a_library_with_drafts_opens_with_them() {
        let dir = std::env::temp_dir().join(format!("wisp-manuscript-{}", new_id("T")));
        {
            let store = Store::open(&dir).unwrap();
            store.db.execute_batch("INSERT INTO drafts(section, body, updated_at) VALUES ('methods', 'We grew plants.', 'x')").unwrap();
        }
        let store = Store::open(&dir).unwrap();
        let list = store.manuscripts().unwrap();
        assert_eq!(list[0].sections[2].body, "We grew plants.");
        std::fs::remove_dir_all(&dir).ok();
    }

    fn sample() -> Manuscript {
        let section = |name: &str, body: &str| Section { name: name.into(), kind: "other".into(), body: body.into(), ..Default::default() };
        Manuscript { title: "Roots & <shoots>".into(), language: "en".into(), sections: vec![section("Introduction", "First line.\n\n\nSecond line.  \n"), section("Methods", "  "), section("Results", "Grew.")], ..Default::default() }
    }

    #[test]
    fn exports_put_names_as_headings_and_leave_out_empty_sections() {
        assert_eq!(markdown(&sample()), "# Roots & <shoots>\n\n## Introduction\n\nFirst line.\n\nSecond line.\n\n## Results\n\nGrew.\n");
        assert_eq!(plain_text(&sample()), "Roots & <shoots>\n\nIntroduction\n\nFirst line.\n\nSecond line.\n\nResults\n\nGrew.\n");
    }

    #[test]
    fn a_docx_is_a_zip_with_the_text_escaped() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        let bytes = docx(&sample());
        assert_eq!(&bytes[..4], b"PK\x03\x04");
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("word/document.xml") && text.contains("[Content_Types].xml"));
        assert!(text.contains("Roots &amp; &lt;shoots&gt;"));
        assert!(text.contains("<w:pStyle w:val=\"Heading1\"/></w:pPr><w:r><w:t xml:space=\"preserve\">Results"));
        assert!(!text.contains("Methods"));
        // The end of central directory says five entries.
        let end = &bytes[bytes.len() - 22..];
        assert_eq!(&end[..4], b"PK\x05\x06");
        assert_eq!(u16::from_le_bytes([end[10], end[11]]), 5);
    }
}
