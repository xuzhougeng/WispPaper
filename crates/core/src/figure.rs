//! 图片库: pieces cut out of the pages of a library's PDFs and kept as
//! pictures. A figure has the number the paper gives it (`label`), its
//! legend, the person's own tags and notes, and the place it was cut from
//! (paper, file, page, box), so it always leads back to its source. The
//! pictures are files under `figures/` in the library folder; one that is
//! removed moves to `removed/` like any other file. A picture a model drew
//! from a description (`imagegen`) is a figure too, one without a paper:
//! its `paper_id` is empty and it leads back to nothing.
//!
//! The legend is not typed when the page says it: `caption` finds the
//! caption that belongs to a piece of a page among the page's lines, for
//! the person to look over before the figure is kept. What the page does
//! not say a model that sees pictures can be asked for (`Ask`): a legend,
//! tags, a reading of the figure. Its answer goes into a field of the
//! dialog, never straight into the library.

use crate::model::{Paper, clean_strings, new_id};
use crate::pdftext::{self, Line, Mark};
use crate::store::Store;
use crate::structure::is_cjk;
use crate::time::now_iso;
use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use std::path::PathBuf;

/// The table, made with the library's own (`store::SCHEMA`). An older build
/// does not know it and reads the rest of the database as before.
pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS figures (
    id TEXT PRIMARY KEY NOT NULL,
    -- The paper it was cut from; empty for a picture a model drew. Not a
    -- reference, so that it can be empty: the trigger below is what takes a
    -- paper's figures along with it.
    paper_id TEXT NOT NULL DEFAULT '',
    -- The file it was cut from. Not a reference: the figure stays when that file is taken off its paper.
    resource_id TEXT NOT NULL DEFAULT '',
    -- The 1-based page it stands on (0 when unknown), and its box there in
    -- points from the top left, as JSON [x0, y0, x1, y1]; empty when unknown.
    page INTEGER NOT NULL DEFAULT 0,
    bbox TEXT NOT NULL DEFAULT '',
    -- The number the paper gives it: "Fig. 2".
    label TEXT NOT NULL DEFAULT '',
    legend TEXT NOT NULL DEFAULT '',
    notes TEXT NOT NULL DEFAULT '',
    tags_json TEXT NOT NULL DEFAULT '[]',
    -- The picture, relative to the library folder, `/` separated.
    local_path TEXT NOT NULL,
    width INTEGER NOT NULL DEFAULT 0,
    height INTEGER NOT NULL DEFAULT 0,
    size_bytes INTEGER NOT NULL DEFAULT 0,
    added_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_figures_paper ON figures(paper_id);
CREATE TRIGGER IF NOT EXISTS figures_follow_paper AFTER DELETE ON papers BEGIN
    DELETE FROM figures WHERE paper_id = old.id;
END;
"#;

/// A library from before pictures could be drawn has `paper_id` as a
/// reference to `papers`, which an empty one would break. Its table is made
/// again without the reference, the rows as they are. An older build reads
/// the result as before: the columns are the same, and the trigger removes a
/// deleted paper's figures as the reference did.
pub(crate) fn migrate(db: &mut rusqlite::Connection) -> Result<()> {
    let made: Option<String> = db.query_row("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'figures'", [], |r| r.get(0)).optional()?;
    if !made.is_some_and(|sql| sql.contains("REFERENCES papers")) { return Ok(()) }
    let tx = db.transaction()?;
    tx.execute_batch("ALTER TABLE figures RENAME TO figures_referring; DROP INDEX IF EXISTS idx_figures_paper; DROP TRIGGER IF EXISTS figures_follow_paper;")?;
    tx.execute_batch(SCHEMA)?;
    tx.execute_batch(&format!("INSERT INTO figures({COLUMNS}) SELECT {COLUMNS} FROM figures_referring; DROP TABLE figures_referring;"))?;
    tx.commit()?;
    Ok(())
}

/// The folder of a library that holds its figures' pictures.
pub const FOLDER: &str = "figures";

/// The most bytes a figure's picture may have: a piece of a page is far below it.
pub const MAX_BYTES: usize = 40_000_000;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Figure {
    /// Empty until it is first kept.
    pub id: String,
    /// The paper it was cut from; empty for a picture a model drew.
    pub paper_id: String,
    /// The file it was cut from; empty when unknown.
    pub resource_id: String,
    /// The 1-based page it stands on; 0 when unknown.
    pub page: usize,
    /// Its box on that page, in points from the top left.
    pub bbox: Option<[f32; 4]>,
    /// The number the paper gives it: "Fig. 2".
    pub label: String,
    pub legend: String,
    pub notes: String,
    pub tags: Vec<String>,
    /// Relative to the library folder, `/` separated.
    pub local_path: String,
    pub width: u32,
    pub height: u32,
    pub size_bytes: u64,
    pub added_at: String,
    pub updated_at: String,
}

impl Figure {
    /// Whether a model drew it: it was cut from no paper.
    pub fn drawn(&self) -> bool {
        self.paper_id.is_empty()
    }

    /// What stands for the figure in a line: its number, else the start of its legend.
    pub fn headline(&self) -> &str {
        if !self.label.trim().is_empty() { return self.label.trim() }
        self.legend.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("")
    }

    /// Whether every word is found in what the figure says or in the record of its paper.
    /// The words are lowercase already (`search_words`).
    pub fn matches(&self, paper: Option<&Paper>, words: &[String]) -> bool {
        if words.is_empty() { return true }
        let mut hay = [self.label.as_str(), self.legend.as_str(), self.notes.as_str()].join("\n");
        for tag in &self.tags { hay.push('\n'); hay.push_str(tag) }
        if let Some(paper) = paper { hay.push_str(&format!("\n{}\n{}\n{}\n{}", paper.title, paper.authors, paper.journal, paper.year.map(|y| y.to_string()).unwrap_or_default())) }
        let hay = hay.to_lowercase();
        words.iter().all(|word| hay.contains(word.as_str()))
    }
}

/// What was typed into a search box, as the words to look for.
pub fn search_words(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}

/// The tags of the figures with how many figures carry each, the commonest first.
pub fn tag_counts(figures: &[Figure]) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, usize)> = vec![];
    for tag in figures.iter().flat_map(|figure| &figure.tags) {
        match counts.iter_mut().find(|(known, _)| known == tag) {
            Some((_, count)) => *count += 1,
            None => counts.push((tag.clone(), 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts
}

/// The kind of picture the bytes are, as a file extension; `None` for what is not a picture kept here.
pub fn picture_kind(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") { return Some("png") }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) { return Some("jpg") }
    if bytes.len() > 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" { return Some("webp") }
    None
}

/// The width and height a PNG says it has.
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if picture_kind(bytes) != Some("png") || bytes.len() < 24 || &bytes[12..16] != b"IHDR" { return None }
    let number = |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    Some((number(16), number(20)))
}

const COLUMNS: &str = "id, paper_id, resource_id, page, bbox, label, legend, notes, tags_json, local_path, width, height, size_bytes, added_at, updated_at";

fn row_to_figure(r: &rusqlite::Row) -> rusqlite::Result<Figure> {
    Ok(Figure {
        id: r.get(0)?,
        paper_id: r.get(1)?,
        resource_id: r.get(2)?,
        page: r.get::<_, i64>(3)?.max(0) as usize,
        bbox: serde_json::from_str(&r.get::<_, String>(4)?).ok(),
        label: r.get(5)?,
        legend: r.get(6)?,
        notes: r.get(7)?,
        tags: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default(),
        local_path: r.get(9)?,
        width: r.get::<_, i64>(10)?.max(0) as u32,
        height: r.get::<_, i64>(11)?.max(0) as u32,
        size_bytes: r.get::<_, i64>(12)?.max(0) as u64,
        added_at: r.get(13)?,
        updated_at: r.get(14)?,
    })
}

/// What a person may write about a figure, tidied: one line for the number, no blank ends, no tag twice.
fn tidy(figure: &mut Figure) {
    figure.label = figure.label.split_whitespace().collect::<Vec<_>>().join(" ");
    figure.legend = figure.legend.trim().to_string();
    figure.notes = figure.notes.trim().to_string();
    figure.tags = clean_strings(figure.tags.iter());
}

impl Store {
    /// Keep a picture as a figure of a paper, or, with no paper named, as one
    /// a model drew. The figure gets its id, its file under `figures/` and
    /// its dates; the bytes must be a PNG, JPEG or WebP.
    pub fn add_figure(&mut self, figure: &mut Figure, picture: &[u8]) -> Result<()> {
        let Some(kind) = picture_kind(picture) else { bail!("这不是一张可以收藏的图片（支持 PNG、JPEG 和 WebP）。") };
        if picture.len() > MAX_BYTES { bail!("这张图片太大了（{} MB），收藏不了。", picture.len() / 1_000_000) }
        if !figure.drawn() {
            let held: Option<String> = self.db.query_row("SELECT id FROM papers WHERE id = ?1", [&figure.paper_id], |r| r.get(0)).optional()?;
            if held.is_none() { bail!("文献不存在：{}", figure.paper_id) }
        }
        tidy(figure);
        figure.id = new_id("G");
        figure.local_path = format!("{FOLDER}/{}.{kind}", figure.id);
        if let Some((width, height)) = png_size(picture) { (figure.width, figure.height) = (width, height) }
        figure.size_bytes = picture.len() as u64;
        figure.added_at = now_iso();
        figure.updated_at = figure.added_at.clone();
        let path = self.figure_path(figure);
        std::fs::create_dir_all(self.root().join(FOLDER)).with_context(|| format!("无法创建图片文件夹 {}", self.root().join(FOLDER).display()))?;
        std::fs::write(&path, picture).with_context(|| format!("无法保存图片 {}", path.display()))?;
        let written = self.db.execute(
            &format!("INSERT INTO figures({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)"),
            params![
                figure.id,
                figure.paper_id,
                figure.resource_id,
                figure.page as i64,
                figure.bbox.map(|bbox| serde_json::to_string(&bbox).unwrap_or_default()).unwrap_or_default(),
                figure.label,
                figure.legend,
                figure.notes,
                serde_json::to_string(&figure.tags)?,
                figure.local_path,
                figure.width,
                figure.height,
                figure.size_bytes as i64,
                figure.added_at,
                figure.updated_at
            ],
        );
        if let Err(err) = written {
            // No row, no file: nothing is left behind that no figure names.
            let _ = std::fs::remove_file(&path);
            return Err(err.into());
        }
        Ok(())
    }

    /// Keep what a person changed about a figure: its number, legend, notes and tags.
    /// Where it came from and its picture stay as they are.
    pub fn save_figure(&mut self, figure: &mut Figure) -> Result<()> {
        tidy(figure);
        figure.updated_at = now_iso();
        let changed = self.db.execute(
            "UPDATE figures SET label = ?2, legend = ?3, notes = ?4, tags_json = ?5, updated_at = ?6 WHERE id = ?1",
            params![figure.id, figure.label, figure.legend, figure.notes, serde_json::to_string(&figure.tags)?, figure.updated_at],
        )?;
        if changed == 0 { bail!("图片不存在：{}", figure.id) }
        Ok(())
    }

    /// Every figure whose paper is outside the trash, and those a model drew, the newest first.
    pub fn list_figures(&self) -> Result<Vec<Figure>> {
        let mut stmt = self.db.prepare(&format!("SELECT {COLUMNS} FROM figures WHERE paper_id NOT IN (SELECT paper_id FROM trashed_papers) ORDER BY added_at DESC, id DESC"))?;
        let figures = stmt.query_map([], row_to_figure)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(figures)
    }

    pub fn get_figure(&self, id: &str) -> Result<Figure> {
        self.db.query_row(&format!("SELECT {COLUMNS} FROM figures WHERE id = ?1"), [id], row_to_figure).optional()?.ok_or_else(|| anyhow!("图片不存在：{id}"))
    }

    /// The absolute path of a figure's picture.
    pub fn figure_path(&self, figure: &Figure) -> PathBuf {
        self.root().join(figure.local_path.replace('/', std::path::MAIN_SEPARATOR_STR))
    }

    /// Take a figure out of the library. Its picture moves to `removed/` inside the library folder.
    pub fn remove_figure(&mut self, id: &str) -> Result<()> {
        let path: Option<String> = self.db.query_row("SELECT local_path FROM figures WHERE id = ?1", [id], |r| r.get(0)).optional()?;
        self.db.execute("DELETE FROM figures WHERE id = ?1", [id])?;
        if let Some(path) = path { self.set_aside(&path) }
        Ok(())
    }

    /// The pictures of a paper's figures, as the database names them: `delete_paper` sets them aside.
    pub(crate) fn figure_files(&self, paper_id: &str) -> Result<Vec<String>> {
        let mut stmt = self.db.prepare("SELECT local_path FROM figures WHERE paper_id = ?1")?;
        let paths = stmt.query_map([paper_id], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(paths)
    }
}

// ---- what a model is asked about a figure

/// What a model that sees the picture is asked to write for a figure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ask {
    /// Its number and legend.
    Legend,
    /// Tags to find it by.
    Tags,
    /// A reading of it, for the notes.
    Notes,
}

/// What the model is told it is, whatever is asked.
pub const ASK_SYSTEM: &str = "你是图片库助手，帮研究者整理从学术论文里截下来的图和表。只依据图片本身和给出的文字作答，不编造看不到的内容；只输出要求的内容，不加说明，不用 Markdown 标记。";

/// How many tags a model's answer may add.
const TAGS_ASKED: usize = 6;

/// What goes with the picture: the paper it is from, what is written about it so far,
/// and (for tags) the tags the library's figures already carry.
pub fn ask_prompt(ask: Ask, title: &str, label: &str, legend: &str, known_tags: &[String]) -> String {
    let mut prompt = String::from("这是从一篇学术论文的 PDF 页面上截下来的一张图（或一个表）。");
    if !title.trim().is_empty() { prompt.push_str(&format!("\n所属文献：{}", title.trim())) }
    let written = [label.trim(), legend.trim()].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" ");
    if !written.is_empty() { prompt.push_str(&format!("\n现有的图号和图注：{written}")) }
    prompt.push_str("\n\n");
    prompt.push_str(&match ask {
        Ask::Legend => "请为它写图注。第一行只写图号（例如 Fig. 2；看不出来就写「无」），空一行，然后是图注正文：先用一句话说明整张图，再依次说明各个子图（a、b、c……）。有现有图注时以它为准，去掉与这张截图无关的部分，保持它的语言；没有时依据图片用中文写。不超过 150 个词。".to_string(),
        Ask::Tags => {
            let known = if known_tags.is_empty() { String::new() } else { format!("图片库里已有的标签：{}。合适时优先用已有的。", known_tags.join("、")) };
            format!("请给它 3 到 5 个标签，便于以后在图片库里查找：图的类型（例如 UMAP、热图、示意图、流程图、显微图像、western blot、柱状图）、内容的主题、值得借鉴的做法（例如 配色、排版）。{known}只输出标签，用逗号隔开。")
        }
        Ask::Notes => "请用中文写两到四句话的解读：这张图要说明什么，是怎么呈现的（图的类型、关键的对比或趋势），有什么值得借鉴或需要留意的地方。".to_string(),
    });
    prompt
}

/// A model's answer without the wrapping some models put around it.
fn bare(reply: &str) -> &str {
    reply.trim().trim_start_matches("```text").trim_start_matches("```markdown").trim_start_matches("```").trim_end_matches("```").trim()
}

/// The number and the legend a model wrote: its first line is the number, unless it says there is none.
pub fn read_legend(reply: &str) -> Caption {
    let reply = bare(reply);
    let (first, rest) = reply.split_once('\n').unwrap_or((reply, ""));
    let first = first.trim().trim_matches(['「', '」', '*', '#', ' ']);
    // An answer of one paragraph is a legend that begins with its number, or has none.
    if rest.trim().is_empty() {
        return match opening(first) {
            Some((label_end, legend_start)) => Caption { label: first[..label_end].trim().to_string(), legend: first[legend_start..].trim().to_string() },
            None => Caption { label: String::new(), legend: first.to_string() },
        };
    }
    let none = first.is_empty() || ["无", "none", "n/a", "图号：无"].iter().any(|word| first.eq_ignore_ascii_case(word));
    let label = if none || first.chars().count() > 40 { String::new() } else { first.trim_start_matches("图号：").trim_start_matches("图号:").trim().to_string() };
    let legend = if !none && label.is_empty() { reply.to_string() } else { rest.split_whitespace().collect::<Vec<_>>().join(" ") };
    Caption { label, legend }
}

/// The tags a model gave: however it listed them, without what is not a tag (a sentence is too long for one).
pub fn read_tags(reply: &str) -> Vec<String> {
    let tags = bare(reply).split([',', '，', '、', ';', '；', '\n']).map(|tag| tag.trim().trim_start_matches(['#', '-', '*', '·', '•']).trim_end_matches(['。', '.']).trim().to_string());
    clean_strings(tags.filter(|tag| !tag.is_empty() && crate::structure::width(tag) <= 30 && !tag.starts_with("标签"))).into_iter().take(TAGS_ASKED).collect()
}

/// A model's reading of a figure, as it goes into the notes.
pub fn read_notes(reply: &str) -> String {
    bare(reply).to_string()
}

// ---- the caption a piece of a page belongs to

/// What a page says about a figure: the number it gives it, and the legend after it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Caption {
    pub label: String,
    pub legend: String,
}

/// How far from the piece a caption may stand, in points.
const REACH: f32 = 150.;

/// The longest legend read off a page, in bytes: what runs on after that is not a legend.
const LEGEND_BYTES: usize = 6000;

/// The words a caption opens with, the longer first. Those in Latin letters are matched whatever their case.
const OPENERS: [&str; 16] = [
    "extended data figure",
    "extended data table",
    "extended data fig",
    "supplementary figure",
    "supplementary table",
    "supplementary fig",
    "supplemental figure",
    "supplemental table",
    "supplemental fig",
    "figure",
    "scheme",
    "table",
    "fig",
    "附图",
    "图",
    "表",
];

/// Whether a line opens a caption: the bytes of its number ("Fig. 2",
/// "Table S1", "图 3"), and where the legend starts after it. A line of
/// running text that merely begins with "Figure 2 shows" does not.
pub(crate) fn opening(text: &str) -> Option<(usize, usize)> {
    let text_start = text.len() - text.trim_start().len();
    let rest = &text[text_start..];
    let opener = OPENERS.iter().find(|opener| rest.get(..opener.len()).is_some_and(|start| start.eq_ignore_ascii_case(opener)))?;
    let latin = opener.is_ascii();
    let mut at = opener.len();
    let after = |at: usize| rest[at..].chars().next();
    // "Figures 2 and 3" is not a caption, nor is "Tablet".
    if latin && after(at).is_some_and(char::is_alphabetic) { return None }
    if after(at) == Some('.') { at += 1 }
    at += rest[at..].len() - rest[at..].trim_start().len();
    // The number: 2, S1, 3a.
    let number_start = at;
    if after(at).is_some_and(|c| c == 'S' || c == 's') && rest[at + 1..].starts_with(|c: char| c.is_ascii_digit()) { at += 1 }
    let digits = rest[at..].chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 3 { return None }
    at += digits;
    if after(at).is_some_and(|c| c.is_ascii_alphabetic()) && !rest[at + 1..].starts_with(|c: char| c.is_alphanumeric()) { at += 1 }
    if at == number_start { return None }
    let label_end = text_start + at;
    // What separates the number from the legend, and then the legend.
    let tail = &rest[at..];
    let legend = tail.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '.' | ':' | '|' | '—' | '–' | '-' | '：' | '．' | '。' | '丨' | '｜'));
    let separated = legend.len() < tail.trim_start().len();
    let opens = legend.chars().next();
    // Running text goes on in lowercase or closes a bracket: "Figure 2 shows", "Fig. 2a). These". A caption ends
    // there, is set off, or starts a sentence.
    if latin && !separated && opens.is_some_and(|c| !(c.is_uppercase() || c == '(' || is_cjk(c))) { return None }
    Some((label_end, text.len() - legend.len()))
}

/// Put the next line of a legend after what is there: a word broken at the
/// line's end is one word again, and Chinese runs on without a space.
fn join_line(text: &mut String, line: &str) {
    let line = line.trim();
    if line.is_empty() { return }
    let (last, first) = (text.chars().next_back(), line.chars().next());
    let broken = last == Some('-') && text.chars().rev().nth(1).is_some_and(char::is_alphabetic) && first.is_some_and(char::is_lowercase);
    if broken {
        text.pop();
    } else if !text.is_empty() && !(last.is_some_and(is_cjk) && first.is_some_and(is_cjk)) {
        text.push(' ');
    }
    text.push_str(line);
}

/// The caption that belongs to a piece of a page: `marks` are the page's
/// glyphs, `piece` the box cut out, in the page's points from the top left.
/// A figure's caption stands under it, a table's over it; a caption cut out
/// with the piece is that piece's. `None` when the page shows none near it.
pub fn caption(marks: &[Mark], piece: [f32; 4]) -> Option<Caption> {
    let [x0, y0, x1, y1] = [piece[0].min(piece[2]), piece[1].min(piece[3]), piece[0].max(piece[2]), piece[1].max(piece[3])];
    let lines = pdftext::lines(marks);
    // How far a line that opens a caption stands from the piece; none for one in another column or too far off.
    let away = |line: &Line| {
        if line.x1 <= x0 || line.x0 >= x1 { return None }
        let (top, bottom) = (line.y - line.size * 0.8, line.y + line.size * 0.25);
        let table = line.text.trim_start().get(..5).is_some_and(|start| start.eq_ignore_ascii_case("table")) || line.text.trim_start().starts_with('表');
        let distance = if top >= y1 {
            (top - y1) * if table { 2. } else { 1. }
        } else if bottom <= y0 {
            (y0 - bottom) * if table { 1. } else { 2. }
        } else {
            0.
        };
        (distance <= REACH).then_some(distance)
    };
    let (start, opened, _) = lines
        .iter()
        .enumerate()
        .filter_map(|(at, line)| Some((at, opening(&line.text)?, away(line)?)))
        .min_by(|a, b| a.2.total_cmp(&b.2))?;
    Some(read_caption(&lines, start, opened))
}

/// Every caption a page shows, in the order the page draws them.
pub fn captions(marks: &[Mark]) -> Vec<Caption> {
    let lines = pdftext::lines(marks);
    (0..lines.len()).filter_map(|at| Some(read_caption(&lines, at, opening(&lines[at].text)?))).collect()
}

/// The caption that opens at a line: its number, and the legend through the lines that go on under it.
fn read_caption(lines: &[Line], start: usize, (label_end, legend_start): (usize, usize)) -> Caption {
    let first = &lines[start];
    let label = first.text[..label_end].split_whitespace().collect::<Vec<_>>().join(" ");
    let mut legend = String::new();
    join_line(&mut legend, &first.text[legend_start..]);
    // The lines that go on under it: set alike, in its column, at a paragraph's leading.
    let mut last = first;
    for next in &lines[start + 1..] {
        let leading = next.y - last.y;
        let size = last.size.max(next.size);
        if leading < size * 0.5 || leading > size * 1.75 { break }
        if (next.size - first.size).abs() > first.size * 0.06 { break }
        if next.x1 <= first.x0 || next.x0 >= first.x1.max(last.x1) { break }
        if opening(&next.text).is_some() { break }
        join_line(&mut legend, &next.text);
        last = next;
        if legend.len() > LEGEND_BYTES { break }
    }
    Caption { label, legend }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PNG's first bytes, saying it is `width` by `height`.
    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec();
        bytes.extend(width.to_be_bytes());
        bytes.extend(height.to_be_bytes());
        bytes.extend([8, 2, 0, 0, 0]);
        bytes
    }

    fn library(name: &str) -> (PathBuf, Store, Paper) {
        let root = std::env::temp_dir().join(format!("wisp-figure-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut store = Store::in_memory(&root).unwrap();
        let mut paper = Paper { title: "Auxin and the root stem cell niche".into(), authors: "Zhang, Wei; Li, Na".into(), year: Some(2021), ..Default::default() };
        store.save_paper(&mut paper).unwrap();
        (root, store, paper)
    }

    #[test]
    fn figures_are_kept_changed_and_removed() {
        let (root, mut store, paper) = library("kept");
        let mut figure = Figure { paper_id: paper.id.clone(), page: 3, bbox: Some([40., 60., 300., 280.]), label: " Fig.  2 ".into(), legend: "WOX5 marks the quiescent centre.\n".into(), tags: vec!["confocal".into(), " confocal ".into(), "root".into()], ..Default::default() };
        store.add_figure(&mut figure, &png(640, 480)).unwrap();
        assert_eq!((figure.label.as_str(), figure.legend.as_str(), figure.tags.len()), ("Fig. 2", "WOX5 marks the quiescent centre.", 2));
        assert_eq!((figure.width, figure.height, figure.local_path.clone()), (640, 480, format!("figures/{}.png", figure.id)));
        assert!(store.figure_path(&figure).is_file());
        assert_eq!(store.list_figures().unwrap(), vec![figure.clone()]);

        // What is not a picture, or names no paper, is refused and leaves nothing behind.
        let mut stray = Figure { paper_id: paper.id.clone(), ..Default::default() };
        assert!(store.add_figure(&mut stray, b"%PDF-1.7").unwrap_err().to_string().contains("不是一张"));
        let mut orphan = Figure { paper_id: "nobody".into(), ..Default::default() };
        assert!(store.add_figure(&mut orphan, &png(1, 1)).unwrap_err().to_string().contains("文献不存在"));
        assert_eq!(std::fs::read_dir(root.join(FOLDER)).unwrap().count(), 1);

        // Its words can be changed; where it came from cannot.
        let mut changed = Figure { legend: "Changed.".into(), notes: "compare with Fig. 4".into(), page: 9, ..figure.clone() };
        store.save_figure(&mut changed).unwrap();
        let read = store.get_figure(&figure.id).unwrap();
        assert_eq!((read.legend.as_str(), read.notes.as_str(), read.page, read.bbox), ("Changed.", "compare with Fig. 4", 3, Some([40., 60., 300., 280.])));

        // A paper in the trash takes its figures out of the list; restored, they are back.
        store.trash_paper(&paper.id).unwrap();
        assert!(store.list_figures().unwrap().is_empty());
        store.restore_paper(&paper.id).unwrap();
        assert_eq!(store.list_figures().unwrap().len(), 1);

        // Removed, the picture is set aside, not deleted.
        store.remove_figure(&figure.id).unwrap();
        assert!(store.list_figures().unwrap().is_empty() && !store.figure_path(&figure).exists());
        assert!(root.join("removed").join(format!("{}.png", figure.id)).is_file());
        assert!(store.save_figure(&mut changed).unwrap_err().to_string().contains("图片不存在"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_deleted_paper_takes_its_figures_along() {
        let (root, mut store, paper) = library("deleted");
        let mut figure = Figure { paper_id: paper.id.clone(), ..Default::default() };
        store.add_figure(&mut figure, &png(10, 10)).unwrap();
        store.delete_paper(&paper.id).unwrap();
        assert!(store.get_figure(&figure.id).is_err() && !store.figure_path(&figure).exists());
        assert!(root.join("removed").join(format!("{}.png", figure.id)).is_file());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_picture_a_model_drew_has_no_paper() {
        let (root, mut store, _) = library("drawn");
        // A paper as the library gives it out, with an id of its own.
        let paper = store.add_paper(Paper { title: "Cytokinin in the shoot".into(), ..Default::default() }).unwrap();
        let mut drawn = Figure { legend: "A schematic of the root tip.".into(), tags: vec!["生成".into()], ..Default::default() };
        store.add_figure(&mut drawn, &png(1024, 1024)).unwrap();
        assert!(drawn.drawn() && store.figure_path(&drawn).is_file());
        // It is listed with the others, and stays when papers come and go.
        let mut cut = Figure { paper_id: paper.id.clone(), ..Default::default() };
        store.add_figure(&mut cut, &png(10, 10)).unwrap();
        assert_eq!(store.list_figures().unwrap().len(), 2);
        store.trash_paper(&paper.id).unwrap();
        assert_eq!(store.list_figures().unwrap(), vec![drawn.clone()]);
        store.delete_paper(&paper.id).unwrap();
        assert_eq!(store.list_figures().unwrap(), vec![drawn.clone()]);
        store.remove_figure(&drawn.id).unwrap();
        assert!(root.join("removed").join(format!("{}.png", drawn.id)).is_file());
        std::fs::remove_dir_all(root).ok();
    }

    /// A library whose figures still refer to their papers is made over, rows kept.
    #[test]
    fn an_older_table_is_made_over() {
        let root = std::env::temp_dir().join(format!("wisp-figure-older-{}", new_id("T")));
        let mut figure;
        let paper;
        {
            let mut store = Store::open(&root).unwrap();
            let held = store.add_paper(Paper { title: "Auxin".into(), ..Default::default() }).unwrap();
            figure = Figure { paper_id: held.id.clone(), label: "Fig. 1".into(), tags: vec!["root".into()], ..Default::default() };
            store.add_figure(&mut figure, &png(10, 10)).unwrap();
            paper = held;
            // The table as builds before this one made it.
            let old = SCHEMA.replace("paper_id TEXT NOT NULL DEFAULT ''", "paper_id TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE");
            let old = old.split("CREATE TRIGGER").next().unwrap().replace("CREATE TABLE IF NOT EXISTS figures", "CREATE TABLE figures_old");
            store.db.execute_batch(&format!("{old} INSERT INTO figures_old SELECT * FROM figures; DROP TABLE figures; DROP TRIGGER IF EXISTS figures_follow_paper; ALTER TABLE figures_old RENAME TO figures;")).unwrap();
            let mut drawn = Figure::default();
            assert!(store.add_figure(&mut drawn, &png(10, 10)).is_err());
        }
        let mut store = Store::open(&root).unwrap();
        assert_eq!(store.list_figures().unwrap(), vec![figure.clone()]);
        let mut drawn = Figure::default();
        store.add_figure(&mut drawn, &png(10, 10)).unwrap();
        // A deleted paper still takes its own figures along, and only those.
        store.delete_paper(&paper.id).unwrap();
        assert_eq!(store.list_figures().unwrap(), vec![drawn]);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn figures_are_found_by_their_words_and_their_paper() {
        let paper = Paper { title: "Auxin and the root stem cell niche".into(), authors: "Zhang, Wei".into(), year: Some(2021), ..Default::default() };
        let figure = Figure { label: "Fig. 2".into(), legend: "Confocal images of WOX5 expression.".into(), tags: vec!["共聚焦".into(), "root".into()], notes: "配色好看".into(), ..Default::default() };
        for (query, found) in [("", true), ("wox5 confocal", true), ("共聚焦", true), ("配色", true), ("zhang 2021", true), ("fig. 2 niche", true), ("leaf", false), ("wox5 leaf", false)] {
            assert_eq!(figure.matches(Some(&paper), &search_words(query)), found, "{query}");
        }
        assert!(!figure.matches(None, &search_words("zhang")));
        let other = Figure { tags: vec!["root".into(), "western".into()], ..Default::default() };
        assert_eq!(tag_counts(&[figure.clone(), other]), vec![("root".to_string(), 2), ("western".to_string(), 1), ("共聚焦".to_string(), 1)]);
        assert_eq!((figure.headline(), Figure { legend: "\n  A legend alone.\nMore.".into(), ..Default::default() }.headline()), ("Fig. 2", "A legend alone."));
    }

    #[test]
    fn lines_that_open_a_caption() {
        let read = |text: &str| opening(text).map(|(label, legend)| (text[..label].trim().to_string(), text[legend..].to_string()));
        let caption = |label: &str, legend: &str| Some((label.to_string(), legend.to_string()));
        assert_eq!(read("Fig. 2 | WOX5 expression in the root tip."), caption("Fig. 2", "WOX5 expression in the root tip."));
        assert_eq!(read("Figure 3. Auxin response."), caption("Figure 3", "Auxin response."));
        assert_eq!(read("FIGURE 1 Overview of the pipeline"), caption("FIGURE 1", "Overview of the pipeline"));
        assert_eq!(read("Figure 2: (A) Schematic."), caption("Figure 2", "(A) Schematic."));
        assert_eq!(read("Extended Data Fig. 4 | Controls."), caption("Extended Data Fig. 4", "Controls."));
        assert_eq!(read("Supplementary Figure S12. Raw blots."), caption("Supplementary Figure S12", "Raw blots."));
        assert_eq!(read("Table 1. Primers used in this study"), caption("Table 1", "Primers used in this study"));
        assert_eq!(read("  Fig. 2a"), caption("Fig. 2a", ""));
        assert_eq!(read("图 3 根尖干细胞微环境"), caption("图 3", "根尖干细胞微环境"));
        assert_eq!(read("图2　生长素响应"), caption("图2", "生长素响应"));
        assert_eq!(read("表 1：引物序列"), caption("表 1", "引物序列"));
        // Running text, and words that only begin alike.
        for text in ["Figure 2 shows that auxin accumulates", "Fig. 2a and b were taken", "Figures 2 and 3. Both", "Tablet 3. Devices", "Figure legends", "Fig. S. Something", "图像 2 的处理", "In Figure 2. we", "Fig. 2a). These additional peaks", "Fig. 6b, c) and"] {
            assert_eq!(read(text), None, "{text}");
        }
    }

    #[test]
    fn what_a_model_is_asked_and_what_it_answers() {
        let known = vec!["配色".to_string(), "UMAP".to_string()];
        let legend = ask_prompt(Ask::Legend, " Root atlas ", "Fig. 2", "WOX5 expression.", &known);
        assert!(legend.contains("所属文献：Root atlas") && legend.contains("现有的图号和图注：Fig. 2 WOX5 expression.") && legend.contains("第一行只写图号"));
        // Nothing read off the page: nothing is said about it. The library's tags go with a request for tags only.
        let bare_prompt = ask_prompt(Ask::Notes, "", "", "", &known);
        assert!(!bare_prompt.contains("所属文献") && !bare_prompt.contains("现有的") && !bare_prompt.contains("配色、UMAP") && bare_prompt.contains("解读"));
        assert!(ask_prompt(Ask::Tags, "", "", "", &known).contains("已有的标签：配色、UMAP"));
        assert!(!ask_prompt(Ask::Tags, "", "", "", &[]).contains("已有的标签"));

        let caption = |label: &str, legend: &str| Caption { label: label.into(), legend: legend.into() };
        assert_eq!(read_legend("Fig. 2\n\nWOX5 expression in the root tip.\na, Overview. b, Detail."), caption("Fig. 2", "WOX5 expression in the root tip. a, Overview. b, Detail."));
        assert_eq!(read_legend("无\n\n一张示意图，说明流程。"), caption("", "一张示意图，说明流程。"));
        assert_eq!(read_legend("```\n图号：Table 1\n\nPrimers.\n```"), caption("Table 1", "Primers."));
        // One paragraph: a legend that begins with its number, or one without.
        assert_eq!(read_legend("Fig. 3 | Auxin response."), caption("Fig. 3", "Auxin response."));
        assert_eq!(read_legend("一张热图。"), caption("", "一张热图。"));
        // A first line too long to be a number: the model wrote the legend only.
        let long = "This figure shows the expression of WOX5 across the root tip in detail.\nSecond line.";
        assert_eq!(read_legend(long), caption("", long));

        assert_eq!(read_tags("UMAP, 配色，单细胞、 #示意图\n- 流程图。"), vec!["UMAP", "配色", "单细胞", "示意图", "流程图"]);
        assert_eq!(read_tags("标签：\na, b, a, c, d, e, f, g"), vec!["a", "b", "c", "d", "e", "f"]);
        assert!(read_tags("这张图片展示的内容非常丰富，我无法用几个词来概括它的全部含义和细节").is_empty());
        assert_eq!(read_notes("```\n这张图说明了流程。\n```"), "这张图说明了流程。");
    }

    /// The glyphs of a line of text set at `size` from `x`, its baseline at `y`: half the size per letter.
    fn glyphs(text: &str, x: f32, y: f32, size: f32) -> Vec<Mark> {
        text.chars().enumerate().map(|(at, c)| Mark { text: c.to_string(), x0: x + at as f32 * size * 0.5, x1: x + (at + 1) as f32 * size * 0.5, y, size, bold: false }).collect()
    }

    #[test]
    fn the_caption_of_a_piece_of_a_page() {
        // Two columns. Left: a figure (y 100 to 300) and its caption of three lines, then body text set larger.
        // Right: body text level with the caption, and another figure's caption lower down.
        let mut page = vec![];
        page.extend(glyphs("Fig. 2 | WOX5 expression in the root tip", 50., 320., 8.));
        page.extend(glyphs("after auxin treat-", 50., 330., 8.));
        page.extend(glyphs("ment. Scale bars, 50 um.", 50., 340., 8.));
        page.extend(glyphs("Body text goes on here in the left column.", 50., 372., 10.));
        page.extend(glyphs("Figure 2 shows the response to auxin.", 320., 320., 10.));
        page.extend(glyphs("Fig. 3 | Another figure.", 320., 520., 8.));
        let found = caption(&page, [50., 100., 290., 300.]).unwrap();
        assert_eq!(found, Caption { label: "Fig. 2".into(), legend: "WOX5 expression in the root tip after auxin treatment. Scale bars, 50 um.".into() });
        // Cut out with its caption: the same. Dragged from the other corner: the same.
        assert_eq!(caption(&page, [290., 345., 50., 100.]), Some(found.clone()));
        // The piece in the right column has its own caption below it, and not the sentence about Figure 2.
        assert_eq!(caption(&page, [320., 380., 560., 500.]).unwrap().label, "Fig. 3");
        // Nothing near: no caption.
        assert_eq!(caption(&page, [320., 700., 560., 760.]), None);
        assert_eq!(caption(&[], [0., 0., 10., 10.]), None);

        // A table's caption stands over it, and wins against a figure's caption as far below.
        let mut page = vec![];
        page.extend(glyphs("Table 1. Primers used in this study", 50., 90., 8.));
        page.extend(glyphs("Figure 4. Something under the table.", 50., 330., 8.));
        assert_eq!(caption(&page, [50., 110., 400., 300.]).unwrap().label, "Table 1");
        // Chinese runs on without spaces between its lines.
        let mut page = vec![];
        page.extend(glyphs("图 3 根尖干细胞", 50., 320., 9.));
        page.extend(glyphs("微环境的结构", 50., 332., 9.));
        assert_eq!(caption(&page, [50., 100., 290., 300.]), Some(Caption { label: "图 3".into(), legend: "根尖干细胞微环境的结构".into() }));
    }
}
