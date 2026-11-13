//! 写作's manuscripts: any number per library, each with a title, the
//! language it is written in and an ordered list of sections. A section has
//! the person's own name and a `kind` (a key of `structure::SECTIONS`) that
//! decides which part of the library's papers its candidate sentences come
//! from. A section's text is kept as it is typed; snapshots of it are kept
//! on a pause or when the person leaves it, so an earlier version can be
//! looked at and restored. Export turns a manuscript into Markdown, plain
//! text or a Word file, its section names becoming headings.
//!
//! A manuscript can also start from a file (`from_word`, `from_markdown`,
//! `Store::import_manuscript`): its headings become the sections, and the
//! comments a Word file came with become the manuscript's (`comment.rs`).
//! The Word file itself is kept beside the library (`manuscripts/`), and an
//! export to Word is that file written again (`docx::rewrite`): with its
//! formatting, tables and pictures, the text as it now stands, and the
//! comments made here.

use crate::comment::{self, Comment};
use crate::docx;
use crate::markdown;
use crate::model::new_id;
use crate::store::Store;
use crate::structure::{self, SECTIONS};
use crate::time::now_iso;
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::ops::Range;
use std::path::PathBuf;

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
-- The file a manuscript was started from.
CREATE TABLE IF NOT EXISTS manuscript_sources (
    manuscript_id TEXT PRIMARY KEY NOT NULL REFERENCES manuscripts(id) ON DELETE CASCADE,
    -- docx or markdown.
    kind TEXT NOT NULL,
    -- The file's name as it was taken in.
    name TEXT NOT NULL,
    -- Where the copy of a Word file is, relative to the library folder; empty when none is kept.
    path TEXT NOT NULL DEFAULT '',
    -- The manuscript's title is a paragraph of the file.
    titled INTEGER NOT NULL DEFAULT 0,
    imported_at TEXT NOT NULL
);
"#;

/// The folder of the library that holds the Word files manuscripts were started from.
pub const FOLDER: &str = "manuscripts";

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
    /// Text that stood under no heading in the file the manuscript came from: its name is the manuscript's own and is not written back.
    pub bare: bool,
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

/// A section's text as Markdown to stand under a heading of level
/// `under`: each line a paragraph of its own, as the editor has them, the
/// items of a list and the lines of fenced code kept together, and a
/// heading written in the section set that many levels deeper.
pub fn section_markdown(body: &str, under: u8) -> String {
    let mut blocks: Vec<String> = vec![];
    // What the block being made is: a list, code, or neither.
    let (mut list, mut code) = (false, false);
    for line in markdown::lines(body) {
        let text = &body[line.range.clone()];
        if line.code {
            match blocks.last_mut().filter(|_| code) {
                Some(block) => {
                    block.push('\n');
                    block.push_str(text);
                }
                None => blocks.push(text.to_string()),
            }
            (list, code) = (false, true);
            continue;
        }
        code = false;
        if text.trim().is_empty() {
            list = false;
            continue;
        }
        if line.item.is_some() {
            match blocks.last_mut().filter(|_| list) {
                Some(block) => {
                    block.push('\n');
                    block.push_str(text.trim_end());
                }
                None => blocks.push(text.trim_end().to_string()),
            }
            list = true;
            continue;
        }
        list = false;
        let text = text.trim();
        if line.heading > 0 && line.quote == 0 {
            let level = (line.heading + under).min(6) as usize;
            blocks.push(format!("{} {}", "#".repeat(level), text.trim_start_matches('#').trim_start()));
        } else {
            blocks.push(text.to_string());
        }
    }
    blocks.join("\n\n")
}

/// The manuscript as Markdown: the title, then each section with text under its name.
pub fn markdown(manuscript: &Manuscript) -> String {
    let mut out = format!("# {}\n", manuscript.shown_title());
    for (name, body) in sections_with_text(manuscript) {
        out.push_str(&format!("\n## {}\n\n", if name.is_empty() { "Section" } else { name }));
        out.push_str(&section_markdown(body, 2));
        out.push('\n');
    }
    out
}

/// The manuscript as plain text, for the clipboard: headings on lines of their own, without the Markdown.
pub fn plain_text(manuscript: &Manuscript) -> String {
    let mut out = format!("{}\n", manuscript.shown_title());
    for (name, body) in sections_with_text(manuscript) {
        out.push_str(&format!("\n{name}\n\n"));
        out.push_str(&paragraphs(&markdown::plain(body)).collect::<Vec<_>>().join("\n\n"));
        out.push('\n');
    }
    out
}

/// A line of a section as a paragraph of a Word file: a heading written in
/// the section as a heading a level under the section's, the words with
/// their bold, italic, strike-through and code. Empty for a line that says
/// nothing (blank, a fence, a rule).
fn word_paragraph(body: &str, line: &markdown::Line) -> String {
    let mut runs = String::new();
    for piece in &line.pieces {
        let text = match piece.part {
            markdown::Part::Text => &body[piece.range.clone()],
            markdown::Part::Bullet => "• ",
            markdown::Part::Syntax => continue,
        };
        let marks = &piece.marks;
        let mut look = String::new();
        if marks.code { look.push_str(&format!("<w:rFonts w:ascii=\"{MONO}\" w:hAnsi=\"{MONO}\"/>")) }
        if marks.bold { look.push_str("<w:b/>") }
        if marks.italic { look.push_str("<w:i/>") }
        if marks.strike { look.push_str("<w:strike/>") }
        if marks.link.is_some() { look.push_str("<w:u w:val=\"single\"/>") }
        let look = if look.is_empty() { look } else { format!("<w:rPr>{look}</w:rPr>") };
        runs.push_str(&format!("<w:r>{look}<w:t xml:space=\"preserve\">{}</w:t></w:r>", xml_escape(text)));
    }
    if runs.trim().is_empty() || body[line.range.clone()].trim().is_empty() { return String::new() }
    let style = match (line.heading, line.quote, line.code) {
        (1, 0, _) => "Heading2",
        (2, 0, _) => "Heading3",
        (3.., 0, _) => "Heading4",
        (_, 1.., _) => "Quote",
        (_, _, true) => "Code",
        _ => "",
    };
    let style = if style.is_empty() { String::new() } else { format!("<w:pPr><w:pStyle w:val=\"{style}\"/></w:pPr>") };
    format!("<w:p>{style}{runs}</w:p>")
}

/// The font code is set in, in a Word file.
const MONO: &str = "Consolas";

/// A Word file's paragraphs say what is written in words alone: a line
/// that `markdown::plain` reads otherwise (`* p < 0.05`, `#3`) gets the
/// backslashes that keep its characters as they are.
fn escape_markdown(text: &str) -> String {
    if markdown::plain(text) == text { return text.to_string() }
    let mut out = String::with_capacity(text.len() + 8);
    let lead = text.len() - text.trim_start().len();
    out.push_str(&text[..lead]);
    let rest = &text[lead..];
    // What would open a block: `#`, `>`, a bullet, a number with its dot, a rule.
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let rest = if rest.starts_with(['#', '>', '-', '+', '*', '_', '`', '~']) {
        out.push('\\');
        out.push_str(&rest[..1]);
        &rest[1..]
    } else if digits > 0 && rest[digits..].starts_with(['.', ')']) {
        out.push_str(&rest[..digits]);
        out.push('\\');
        &rest[digits..]
    } else {
        rest
    };
    for c in rest.chars() {
        if matches!(c, '\\' | '*' | '_' | '`' | '~' | '[') { out.push('\\') }
        out.push(c);
    }
    // Never worse than the line as it was.
    if markdown::plain(&out) == text { out } else { text.to_string() }
}

pub(crate) fn xml_escape(text: &str) -> String {
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
        for line in markdown::lines(text) { body.push_str(&word_paragraph(text, &line)) }
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
<w:style w:type=\"paragraph\" w:styleId=\"Heading2\"><w:name w:val=\"heading 2\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/><w:pPr><w:keepNext/><w:spacing w:before=\"240\" w:after=\"120\"/><w:outlineLvl w:val=\"1\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"27\"/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Heading3\"><w:name w:val=\"heading 3\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/><w:pPr><w:keepNext/><w:spacing w:before=\"200\" w:after=\"80\"/><w:outlineLvl w:val=\"2\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"25\"/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Heading4\"><w:name w:val=\"heading 4\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/><w:pPr><w:keepNext/><w:spacing w:before=\"160\" w:after=\"80\"/><w:outlineLvl w:val=\"3\"/></w:pPr><w:rPr><w:b/><w:i/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Quote\"><w:name w:val=\"Quote\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/><w:pPr><w:ind w:left=\"720\"/></w:pPr><w:rPr><w:i/><w:color w:val=\"595959\"/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Code\"><w:name w:val=\"Code\"/><w:basedOn w:val=\"Normal\"/><w:qFormat/><w:pPr><w:spacing w:after=\"0\" w:line=\"240\" w:lineRule=\"auto\"/></w:pPr><w:rPr><w:rFonts w:ascii=\"Consolas\" w:hAnsi=\"Consolas\"/><w:sz w:val=\"20\"/></w:rPr></w:style>\
</w:styles>"
    );
    let content_types = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/><Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/></Types>";
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>";
    let document_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/></Relationships>";
    stored_zip(&[("[Content_Types].xml", content_types.as_bytes()), ("_rels/.rels", rels.as_bytes()), ("word/document.xml", document.as_bytes()), ("word/_rels/document.xml.rels", document_rels.as_bytes()), ("word/styles.xml", styles.as_bytes())])
}

pub(crate) fn crc32(data: &[u8]) -> u32 {
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

// ---- starting from a file

/// What a file gives a manuscript.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Draft {
    pub title: String,
    /// The title is a paragraph of the file, not the file's name.
    pub titled: bool,
    /// A code of `LANGUAGES`.
    pub language: String,
    pub sections: Vec<DraftSection>,
    pub comments: Vec<DraftComment>,
    /// What a Word file holds besides: kept in the file, not in the manuscript.
    pub tables: usize,
    pub pictures: usize,
    /// The Word file had tracked changes: it was read as if they were accepted.
    pub tracked: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DraftSection {
    pub name: String,
    /// A key of `structure::SECTIONS`.
    pub kind: String,
    /// The text before the first heading: the name is not a heading of the file.
    pub bare: bool,
    pub paragraphs: Vec<String>,
}

/// A comment the file came with.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DraftComment {
    /// Which of `Draft::sections` it is in.
    pub section: usize,
    pub quote: String,
    pub author: String,
    pub date: String,
    pub body: String,
}

/// A paragraph of a file, as far as the outline goes.
struct Line {
    /// What it says; for a heading, its words alone.
    text: String,
    /// As it reads when it stays in a section's text (a Markdown heading keeps its marks there).
    kept: String,
    level: Option<u8>,
    title: bool,
}

/// A title is short and is not a sentence.
const TITLE_WIDTH: usize = 150;

/// A heading told by its name alone (in a file without heading styles) is no longer than this.
const HEADING_WIDTH: usize = 60;

fn manuscript_language(text: &str) -> String {
    let code = structure::language(&[text.to_string()]);
    if is_language(code) { return code.to_string() }
    // Too short for the rules of a paper's full text: a text mostly in Chinese characters is Chinese.
    let (cjk, letters) = (text.chars().filter(|c| structure::is_cjk(*c)).count(), text.chars().filter(|c| c.is_alphabetic()).count());
    if code.is_empty() && cjk * 2 > letters { "zh".to_string() } else { DEFAULT_LANGUAGE.to_string() }
}

/// The manuscript a file's paragraphs make, and the section each of them went to (none for the title).
fn outline(mut lines: Vec<Line>, file_name: &str) -> (Draft, Vec<Option<usize>>) {
    // A file without heading styles: the headings every paper has are told by their names.
    if lines.iter().all(|line| line.level.is_none()) {
        for line in &mut lines {
            if structure::width(&line.text) <= HEADING_WIDTH && structure::canonical(&line.text).is_some() { line.level = Some(1) }
        }
    }
    let language = manuscript_language(&lines.iter().filter(|line| line.level.is_none()).map(|line| line.text.as_str()).collect::<Vec<_>>().join("\n"));
    let stem = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
    let stem = stem.rsplit_once('.').map_or(stem, |(stem, _)| stem).trim();
    // The title: the paragraph styled as one, else a short first line that is not a sentence.
    let mut title = lines.iter().position(|line| line.title);
    if title.is_none() && lines.len() > 1 && lines[0].level.is_none() && structure::width(&lines[0].text) <= TITLE_WIDTH && !lines[0].text.ends_with(['。', '.', '；', ';', '：', ':', '，', ',']) { title = Some(0) }
    let top = lines.iter().enumerate().filter(|(n, _)| Some(*n) != title).filter_map(|(_, line)| line.level).min();
    let mut draft = Draft { title: title.map_or(stem.to_string(), |n| lines[n].text.clone()), titled: title.is_some(), language, ..Default::default() };
    let chinese = draft.language == "zh";
    let mut places = vec![];
    for (n, line) in lines.iter().enumerate() {
        if Some(n) == title {
            places.push(None);
            continue;
        }
        if line.level.is_some() && line.level == top {
            let kind = structure::canonical(&line.text).filter(|kind| is_kind(kind)).unwrap_or("other");
            draft.sections.push(DraftSection { name: line.text.clone(), kind: kind.to_string(), bare: false, paragraphs: vec![] });
        } else {
            if draft.sections.is_empty() {
                let name = match (top.is_some(), chinese) {
                    (true, true) => "开头",
                    (true, false) => "Front matter",
                    (false, true) => "正文",
                    (false, false) => "Text",
                };
                draft.sections.push(DraftSection { name: name.to_string(), kind: "other".to_string(), bare: true, paragraphs: vec![] });
            }
            if let Some(section) = draft.sections.last_mut() { section.paragraphs.push(line.kept.clone()) }
        }
        places.push(Some(draft.sections.len() - 1));
    }
    if draft.sections.is_empty() { draft.sections.push(DraftSection { name: if chinese { "正文" } else { "Text" }.to_string(), kind: "other".to_string(), bare: true, paragraphs: vec![] }) }
    (draft, places)
}

/// The manuscript a Word file makes: sections at its highest headings, the comments it came with.
pub fn from_word(read: &docx::Read, file_name: &str) -> Draft {
    // What a paragraph says is its words: Markdown's characters in it stay characters (`escape_markdown`).
    let lines = read.paragraphs.iter().map(|para| Line { text: para.text.clone(), kept: escape_markdown(&para.text), level: para.level, title: para.title }).collect();
    let (mut draft, places) = outline(lines, file_name);
    (draft.tables, draft.pictures, draft.tracked) = (read.tables, read.pictures, read.tracked);
    draft.comments = read.comments.iter().filter(|comment| !comment.body.trim().is_empty()).map(|comment| DraftComment { section: comment.paragraph.and_then(|n| places[n]).unwrap_or(0), quote: comment.quote.clone(), author: comment.author.clone(), date: comment.date.clone(), body: comment.body.clone() }).collect();
    draft
}

/// The manuscript a Markdown (or plain text) file makes. Lines of one
/// paragraph become one line, as the manuscript has them; a list item, a
/// table row or a line of code keeps its own.
pub fn from_markdown(text: &str, file_name: &str) -> Draft {
    let text = text.replace("\r\n", "\n");
    let mut rest = text.trim_start_matches('\u{feff}');
    // A header of fields before the text: its `title` is the title.
    let mut named = None;
    if let Some(after) = rest.strip_prefix("---\n") && let Some(end) = after.find("\n---") {
        named = after[..end].lines().find_map(|line| line.strip_prefix("title:")).map(|title| title.trim().trim_matches(['"', '\'']).to_string()).filter(|title| !title.is_empty());
        rest = after[end + 4..].split_once('\n').map_or("", |(_, rest)| rest);
    }
    let mut lines: Vec<Line> = vec![];
    let (mut joining, mut code) = (false, false);
    for line in rest.lines() {
        let line = line.trim_end();
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            code = !code;
            joining = false;
            lines.push(Line { text: line.to_string(), kept: line.to_string(), level: None, title: false });
            continue;
        }
        if code {
            if !line.is_empty() { lines.push(Line { text: line.to_string(), kept: line.to_string(), level: None, title: false }) }
            continue;
        }
        if trimmed.is_empty() {
            joining = false;
            continue;
        }
        let marks = trimmed.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&marks) && trimmed[marks..].starts_with(' ') {
            lines.push(Line { text: trimmed[marks..].trim().trim_end_matches('#').trim().to_string(), kept: trimmed.to_string(), level: Some(marks as u8), title: false });
            joining = false;
            continue;
        }
        let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
        let own = trimmed.starts_with(['|', '>']) || ["- ", "* ", "+ "].iter().any(|mark| trimmed.starts_with(mark)) || (digits > 0 && (trimmed[digits..].starts_with(". ") || trimmed[digits..].starts_with(") ")));
        match lines.last_mut().filter(|_| joining && !own) {
            Some(last) => {
                // Chinese is written without spaces between the lines of a paragraph.
                let apart = !(last.text.ends_with(structure::is_cjk) && trimmed.starts_with(structure::is_cjk));
                if apart { last.text.push(' ') }
                last.text.push_str(trimmed);
                last.kept = last.text.clone();
            }
            None => lines.push(Line { text: trimmed.to_string(), kept: trimmed.to_string(), level: None, title: false }),
        }
        joining = !own;
    }
    // One `#` heading before all others is the title; the sections are the next level.
    let firsts: Vec<usize> = lines.iter().enumerate().filter(|(_, line)| line.level == Some(1)).map(|(n, _)| n).collect();
    if named.is_none() && firsts.len() == 1 && lines.iter().position(|line| line.level.is_some()) == Some(firsts[0]) {
        lines[firsts[0]].title = true;
        lines[firsts[0]].level = None;
    }
    // With a title from the header, no line is taken for one.
    let guard = named.is_some();
    if guard { lines.insert(0, Line { text: named.unwrap_or_default(), kept: String::new(), level: None, title: true }) }
    let (mut draft, _) = outline(lines, file_name);
    draft.titled = false;
    draft
}

/// The file a manuscript was started from.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    /// `docx` or `markdown`.
    pub kind: String,
    pub name: String,
    /// The copy of a Word file, relative to the library folder.
    pub path: String,
    pub titled: bool,
    pub imported_at: String,
}

/// `2026-11-12T08:00:00Z`, as a Word file dates a comment or a change.
fn word_date(stamp: &str) -> String {
    match stamp.get(..19) {
        Some(seconds) if seconds.as_bytes().get(10) == Some(&b'T') => format!("{seconds}Z"),
        _ => now_iso().get(..19).map(|seconds| format!("{seconds}Z")).unwrap_or_default(),
    }
}

/// The manuscript's paragraphs as a Word file is to say them, each with the
/// comments made here on it. With a `source`, they are the paragraphs of
/// the file the manuscript came from; without, those `docx` writes.
pub fn wanted(manuscript: &Manuscript, source: Option<&Source>, comments: &[Comment]) -> Vec<docx::Wanted> {
    let mut out: Vec<docx::Wanted> = vec![];
    // Where each section's paragraphs are in `out`: its heading, and its lines.
    let mut places: Vec<(&str, Option<usize>, Range<usize>)> = vec![];
    let paragraph = |text: &str, role: docx::Role| docx::Wanted { text: text.to_string(), role, notes: vec![] };
    match source {
        Some(source) => {
            if source.titled && !manuscript.title.trim().is_empty() { out.push(paragraph(manuscript.title.trim(), docx::Role::Title)) }
        }
        None => out.push(paragraph(manuscript.shown_title(), docx::Role::Title)),
    }
    for section in &manuscript.sections {
        // A file made here leaves out the sections without text; one written again keeps every heading it had.
        if source.is_none() && section.body.trim().is_empty() { continue }
        let heading = (source.is_none() || !section.bare).then(|| {
            out.push(paragraph(section.name.trim(), docx::Role::Heading));
            out.len() - 1
        });
        let start = out.len();
        // What a paragraph says, as the editor draws it: a Word file has its own bold and headings.
        out.extend(paragraphs(&markdown::plain(&section.body)).map(|line| paragraph(line, docx::Role::Body)));
        places.push((section.id.as_str(), heading, start..out.len()));
    }
    for comment in comments.iter().filter(|comment| comment.origin != comment::FILE && comment.state != comment::DONE) {
        let said = comment.said();
        if said.is_empty() { continue }
        let Some((_, heading, lines)) = places.iter().find(|(id, _, _)| *id == comment.section_id) else { continue };
        // Its words, in the paragraph that has them; a comment whose words were written over since goes on the section's heading, with the words it was about.
        let quote = markdown::plain(&comment.quote);
        let first = quote.lines().next().unwrap_or("").trim();
        let found = lines.clone().find_map(|n| out[n].text.find(first).filter(|_| !first.is_empty()).map(|at| (n, at..at + first.len())));
        let named = heading.filter(|n| !first.is_empty() && out[*n].text.contains(first)).map(|n| (n, 0..0));
        let (at, range, body) = match found.or(named) {
            Some((n, range)) => (n, range, said),
            None => {
                let Some(n) = heading.or(lines.clone().next()) else { continue };
                (n, 0..0, if first.is_empty() { said } else { format!("「{first}」\n{said}") })
            }
        };
        out[at].notes.push(docx::Note { range, author: comment.author.clone(), date: word_date(&comment.created_at), body });
    }
    out
}

impl Store {
    /// A manuscript made of a file: its sections, the comments it came with,
    /// and for a Word file (`bytes`) a copy of the file itself, which an
    /// export to Word is written from. All of it or nothing.
    pub fn import_manuscript(&mut self, draft: &Draft, kind: &str, file_name: &str, bytes: Option<&[u8]>) -> Result<Manuscript> {
        if !is_language(&draft.language) { bail!("不认识这种语言：{}", draft.language) }
        if draft.sections.is_empty() || draft.sections.iter().any(|section| section.name.trim().is_empty()) { bail!("稿件需要有名称的章节") }
        let (id, now) = (new_id("M"), now_iso());
        let path = if bytes.is_some() { format!("{FOLDER}/{id}.docx") } else { String::new() };
        let (folder, file) = (self.root().join(FOLDER), self.root().join(path.replace('/', std::path::MAIN_SEPARATOR_STR)));
        let tx = self.db.transaction()?;
        tx.execute("INSERT INTO manuscripts(id, title, language, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)", params![id, draft.title.trim(), draft.language, now])?;
        let mut sections = vec![];
        for (position, section) in draft.sections.iter().enumerate() {
            let section_id = new_id("S");
            tx.execute("INSERT INTO manuscript_sections(id, manuscript_id, position, name, kind, body, updated_at, bare) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)", params![section_id, id, position as i64, section.name.trim(), if is_kind(&section.kind) { section.kind.as_str() } else { "other" }, section.paragraphs.join("\n\n"), now, section.bare])?;
            sections.push(section_id);
        }
        for (n, theirs) in draft.comments.iter().enumerate() {
            let Some(section) = sections.get(theirs.section) else { continue };
            let made = Comment { id: new_id("C"), manuscript_id: id.clone(), section_id: section.clone(), quote: theirs.quote.trim().to_string(), body: theirs.body.trim().to_string(), author: theirs.author.trim().to_string(), origin: comment::FILE.into(), state: comment::OPEN.into(), created_at: if theirs.date.is_empty() { format!("{now}{n:04}") } else { theirs.date.clone() }, ..Default::default() };
            comment::insert_comment(&tx, &made)?;
        }
        tx.execute("INSERT INTO manuscript_sources(manuscript_id, kind, name, path, titled, imported_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![id, kind, file_name, path, draft.titled, now])?;
        if let Some(bytes) = bytes {
            std::fs::create_dir_all(&folder).with_context(|| format!("无法创建稿件文件夹 {}", folder.display()))?;
            std::fs::write(&file, bytes).with_context(|| format!("无法保存 {}", file.display()))?;
        }
        if let Err(err) = tx.commit() {
            if bytes.is_some() { let _ = std::fs::remove_file(&file); }
            return Err(err.into());
        }
        self.manuscript(&id)?.ok_or_else(|| anyhow::anyhow!("新稿件保存后未找到"))
    }

    /// The file a manuscript was started from, when it was.
    pub fn manuscript_source(&self, manuscript_id: &str) -> Result<Option<Source>> {
        Ok(self.db.query_row("SELECT kind, name, path, titled, imported_at FROM manuscript_sources WHERE manuscript_id = ?1", [manuscript_id], |r| Ok(Source { kind: r.get(0)?, name: r.get(1)?, path: r.get(2)?, titled: r.get(3)?, imported_at: r.get(4)? })).optional()?)
    }

    /// Where the copy of a manuscript's Word file is.
    pub fn source_file(&self, source: &Source) -> PathBuf {
        self.root().join(source.path.replace('/', std::path::MAIN_SEPARATOR_STR))
    }

    /// The manuscript as a Word file with its comments. One that came from a
    /// Word file is that file written again, with `track` its changes as
    /// revisions by `author`; any other is a plain document made here. The
    /// second value says whether the file it came from was written again.
    pub fn manuscript_word(&self, manuscript: &Manuscript, comments: &[Comment], track: bool, author: &str) -> Result<(docx::Rewritten, bool)> {
        let source = self.manuscript_source(&manuscript.id)?.filter(|source| source.kind == "docx" && !source.path.is_empty());
        if let Some(source) = &source && let Ok(bytes) = std::fs::read(self.source_file(source)) {
            let options = docx::Options { track, author: author.to_string(), date: word_date(&now_iso()) };
            return Ok((docx::rewrite(&bytes, &wanted(manuscript, Some(source), comments), &options)?, true));
        }
        Ok((docx::rewrite(&docx(manuscript), &wanted(manuscript, None, comments), &docx::Options::default())?, false))
    }
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
    Ok(Section { id: r.get(0)?, name: r.get(1)?, kind: r.get(2)?, body: r.get(3)?, updated_at: r.get(4)?, bare: r.get(5)? })
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
        let mut stmt = self.db.prepare("SELECT id, name, kind, body, updated_at, bare FROM manuscript_sections WHERE manuscript_id = ?1 ORDER BY position, id")?;
        Ok(stmt.query_map([manuscript_id], read_section)?.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn section_row(&self, id: &str) -> Result<(String, Section)> {
        let found = self.db.query_row("SELECT id, name, kind, body, updated_at, bare, manuscript_id FROM manuscript_sections WHERE id = ?1", [id], |r| Ok((r.get(6)?, read_section(r)?))).optional()?;
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
    fn a_markdown_file_becomes_sections_at_its_headings() {
        let text = "---\ntitle: \"Roots and shoots\"\nauthor: x\n---\n\nA line before\nany heading.\n\n## Introduction\n\nRoots grow\ndown.\n根向下\n生长。\n\n- one\n- two\n\n### Hairs\n\nShort.\n\n## 方法\n\n```\ncode  line\n```\n";
        let draft = from_markdown(text, "notes/draft.md");
        assert_eq!((draft.title.as_str(), draft.language.as_str()), ("Roots and shoots", "en"));
        let shown: Vec<(&str, &str, bool, Vec<&str>)> = draft.sections.iter().map(|s| (s.name.as_str(), s.kind.as_str(), s.bare, s.paragraphs.iter().map(String::as_str).collect())).collect();
        assert_eq!(shown, [("Front matter", "other", true, vec!["A line before any heading."]), ("Introduction", "introduction", false, vec!["Roots grow down. 根向下生长。", "- one", "- two", "### Hairs", "Short."]), ("方法", "methods", false, vec!["```", "code  line", "```"])]);
        // One `#` heading before the rest is the title; without any heading the file's name is, and the text is one section.
        let draft = from_markdown("# 根的综述\n\n## 引言\n\n根向下生长，吸收水分。\n", "x.md");
        assert_eq!((draft.title.as_str(), draft.language.as_str(), draft.sections.len(), draft.sections[0].name.as_str(), draft.sections[0].kind.as_str()), ("根的综述", "zh", 1, "引言", "introduction"));
        let draft = from_markdown("只有一段话，没有标题，写得长一点才不会被当成题目。\n\n第二段。", "随手记.txt");
        assert_eq!((draft.title.as_str(), draft.sections.len(), draft.sections[0].name.as_str(), draft.sections[0].bare), ("随手记", 1, "正文", true));
        assert_eq!(from_markdown("", "empty.md").sections.len(), 1);
    }

    /// A Word file as this app writes one, with a comment put on it.
    fn word_file() -> Vec<u8> {
        let section = |name: &str, body: &str| Section { name: name.into(), kind: "other".into(), body: body.into(), ..Default::default() };
        let written = Manuscript { title: "Roots".into(), language: "en".into(), sections: vec![section("Introduction", "Roots grow down into the soil.\n\nThey take up water."), section("Methods", "We grew plants.")], ..Default::default() };
        let mut paragraphs = wanted(&written, None, &[]);
        paragraphs[1 + 1].notes.push(docx::Note { range: 11..15, author: "Li Hua".into(), date: "2026-01-02T03:04:05Z".into(), body: "How far down?".into() });
        docx::rewrite(&docx(&written), &paragraphs, &docx::Options::default()).unwrap().bytes
    }

    #[test]
    fn a_word_file_is_taken_in_and_written_again_with_what_changed() {
        let dir = std::env::temp_dir().join(format!("wisp-manuscript-{}", new_id("T")));
        let mut store = Store::open(&dir).unwrap();
        let bytes = word_file();
        let draft = from_word(&docx::read(&bytes).unwrap(), "Roots v3.docx");
        assert_eq!((draft.title.as_str(), draft.titled, draft.sections.iter().map(|s| (s.name.as_str(), s.kind.as_str())).collect::<Vec<_>>()), ("Roots", true, vec![("Introduction", "introduction"), ("Methods", "methods")]));
        let made = store.import_manuscript(&draft, "docx", "Roots v3.docx", Some(&bytes)).unwrap();
        assert_eq!(made.sections[0].body, "Roots grow down into the soil.\n\nThey take up water.");
        let source = store.manuscript_source(&made.id).unwrap().unwrap();
        assert_eq!((source.kind.as_str(), source.name.as_str(), source.titled, std::fs::read(store.source_file(&source)).unwrap() == bytes), ("docx", "Roots v3.docx", true, true));
        let theirs = store.comments(&made.id).unwrap();
        assert_eq!(theirs.iter().map(|c| (c.origin.as_str(), c.author.as_str(), c.quote.as_str(), c.body.as_str(), c.section_id == made.sections[0].id)).collect::<Vec<_>>(), [("file", "Li Hua", "down", "How far down?", true)]);

        // Nothing changed: the file is written again as it was, and their comment is not put in twice.
        let (same, from_file) = store.manuscript_word(&made, &theirs, true, "Xu").unwrap();
        assert!(from_file);
        assert_eq!((same.kept, same.changed, same.added, same.removed, same.comments), (6, 0, 0, 0, 0));

        // A paragraph changed, one put in, a section added, and comments of the person's and the assistant's.
        let (intro, methods) = (made.sections[0].id.clone(), made.sections[1].id.clone());
        store.save_section_body(&intro, "Roots grow two metres down into the soil.\n\nThey take up water.\n\nA new paragraph.").unwrap();
        store.add_section(&made.id, "Outlook", "discussion").unwrap();
        store.add_comment(&made.id, &intro, "take up water", "And salts?", "Xu", comment::PERSON).unwrap();
        let remark = store.add_remarks(&made.id, &methods, &[comment::Remark { quote: "We grew plants.".into(), body: "说明品种".into(), suggestion: "We grew Arabidopsis.".into() }]).unwrap();
        store.add_comment(&made.id, &methods, "words that are gone", "Lost its place.", "Xu", comment::PERSON).unwrap();
        let now = store.manuscript(&made.id).unwrap().unwrap();
        let (out, _) = store.manuscript_word(&now, &store.comments(&made.id).unwrap(), true, "Xu").unwrap();
        assert_eq!((out.kept, out.changed, out.added, out.removed, out.comments), (5, 1, 2, 0, 3));
        let read = docx::read(&out.bytes).unwrap();
        assert_eq!(read.paragraphs.iter().map(|p| p.text.as_str()).collect::<Vec<_>>(), ["Roots", "Introduction", "Roots grow two metres down into the soil.", "They take up water.", "A new paragraph.", "Methods", "We grew plants.", "Outlook"]);
        let said: Vec<(&str, &str, &str)> = read.comments.iter().map(|c| (c.author.as_str(), c.quote.as_str(), c.body.as_str())).collect();
        assert_eq!(said, [("Li Hua", "down", "How far down?"), ("Xu", "take up water", "And salts?"), ("Xu", "Methods", "「words that are gone」\nLost its place."), (comment::ASSISTANT_NAME, "We grew plants.", "说明品种\n建议改为：We grew Arabidopsis.")]);
        assert!(read.tracked);

        // Its proposal taken, the comment stands on the new words.
        store.save_section_body(&methods, "We grew Arabidopsis.").unwrap();
        store.save_comment(&remark[0].taken()).unwrap();
        let now = store.manuscript(&made.id).unwrap().unwrap();
        let (out, _) = store.manuscript_word(&now, &store.comments(&made.id).unwrap(), false, "Xu").unwrap();
        let read = docx::read(&out.bytes).unwrap();
        assert!(read.comments.iter().any(|c| c.quote == "We grew Arabidopsis." && c.body == "说明品种") && !read.tracked);

        // Without its file a manuscript is a plain document, comments included.
        std::fs::remove_file(store.source_file(&source)).unwrap();
        let (plain, from_file) = store.manuscript_word(&now, &store.comments(&made.id).unwrap(), true, "Xu").unwrap();
        assert!(!from_file && docx::read(&plain.bytes).unwrap().comments.len() == 3);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_word_file_without_heading_styles_is_cut_at_the_names_every_paper_has() {
        let para = |text: &str| docx::ReadPara { text: text.into(), level: None, title: false };
        let read = docx::Read { paragraphs: vec![para("植物单细胞组学的现状"), para("张三，李四"), para("摘要"), para("单细胞技术正在改变植物学研究的方式，本文对此作一综述。"), para("1 引言"), para("植物器官由多种细胞类型组成。"), para("参考文献"), para("[1] Zhang 2021.")], comments: vec![docx::ReadComment { author: "Li".into(), body: "补充".into(), paragraph: Some(5), quote: "多种细胞类型".into(), ..Default::default() }, docx::ReadComment { body: "on a table".into(), ..Default::default() }], ..Default::default() };
        let draft = from_word(&read, "稿子.docx");
        assert_eq!((draft.title.as_str(), draft.titled, draft.language.as_str()), ("植物单细胞组学的现状", true, "zh"));
        assert_eq!(draft.sections.iter().map(|s| (s.name.as_str(), s.kind.as_str(), s.bare, s.paragraphs.len())).collect::<Vec<_>>(), [("开头", "other", true, 1), ("摘要", "abstract", false, 1), ("1 引言", "introduction", false, 1), ("参考文献", "other", false, 1)]);
        assert_eq!(draft.comments.iter().map(|c| (c.section, c.quote.as_str())).collect::<Vec<_>>(), [(2, "多种细胞类型"), (0, "")]);
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

    #[test]
    fn markdown_written_in_a_section_is_exported_as_markdown_and_as_formatting() {
        let section = Section { name: "Results".into(), kind: "results".into(), body: "# Roots\nThey **grew** fast.\n- one\n- two\n\n```\nlet x = 1;\n\nlet y = 2;\n```\n---".into(), ..Default::default() };
        let manuscript = Manuscript { title: "T".into(), sections: vec![section], ..Default::default() };
        // The heading goes under the section's; a list and code keep their lines together.
        assert_eq!(markdown(&manuscript), "# T\n\n## Results\n\n### Roots\n\nThey **grew** fast.\n\n- one\n- two\n\n```\nlet x = 1;\n\nlet y = 2;\n```\n\n---\n");
        assert_eq!(plain_text(&manuscript), "T\n\nResults\n\nRoots\n\nThey grew fast.\n\n• one\n\n• two\n\n```\n\nlet x = 1;\n\nlet y = 2;\n\n```\n");
        let text = String::from_utf8_lossy(&docx(&manuscript)).to_string();
        assert!(text.contains("<w:p><w:pPr><w:pStyle w:val=\"Heading2\"/></w:pPr><w:r><w:t xml:space=\"preserve\">Roots</w:t></w:r></w:p>"));
        assert!(text.contains("<w:r><w:rPr><w:b/></w:rPr><w:t xml:space=\"preserve\">grew</w:t></w:r>"));
        assert!(!text.contains("**") && !text.contains("---"));
    }

    #[test]
    fn a_word_paragraph_keeps_its_characters() {
        for said in ["Plain words.", "* p < 0.05 for all", "#3 was the best", "1. Not a list in Word", "a_b and *c* and `d` [x](https://e.org)", "---", "> not quoted"] {
            let kept = escape_markdown(said);
            assert_eq!(markdown::plain(&kept), said, "{kept}");
        }
        assert_eq!(escape_markdown("Plain words."), "Plain words.");
    }
}
