//! 批注: remarks on words of a manuscript, as in Word's review. One is
//! about a quotation of its section (`quote`) and is found again by what
//! the quotation says, so it stays with its words while the text around
//! them is written on; when the words are changed it says so and is no
//! longer anchored. A comment comes with a Word file (`FILE`: it stays in
//! that file, here it is only read and marked as dealt with), is written by
//! the person, or is the assistant's.
//!
//! The assistant reviews a section the way a reader with a pen would
//! (`review_prompt` / `parse_review`): each remark names the words it is
//! about, and may propose words to put in their place (`suggestion`).
//! Only the person takes a proposal: the text is changed in the editor,
//! and the comment then stands on the new words and keeps what they
//! replaced (`Comment::taken`), which is what a reader of the exported Word
//! file sees as the reason for a change. Taking it back restores both.

use crate::model::new_id;
use crate::store::Store;
use crate::time::now_iso;
use anyhow::{Result, bail};
use rusqlite::params;
use serde::Serialize;
use serde_json::Value;
use std::ops::Range;

pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS manuscript_comments (
    id TEXT PRIMARY KEY NOT NULL,
    manuscript_id TEXT NOT NULL REFERENCES manuscripts(id) ON DELETE CASCADE,
    section_id TEXT NOT NULL REFERENCES manuscript_sections(id) ON DELETE CASCADE,
    -- The words it is about, as they stand in the section; empty for one about the section as a whole.
    quote TEXT NOT NULL DEFAULT '',
    body TEXT NOT NULL DEFAULT '',
    author TEXT NOT NULL DEFAULT '',
    -- file, person or assistant.
    origin TEXT NOT NULL,
    -- The assistant's: what it would put in place of the quote; empty for a remark.
    suggestion TEXT NOT NULL DEFAULT '',
    -- What the quote replaced, once a suggestion was taken.
    replaced TEXT NOT NULL DEFAULT '',
    -- open, applied (its suggestion was taken) or done (dealt with).
    state TEXT NOT NULL DEFAULT 'open',
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_manuscript_comments ON manuscript_comments(manuscript_id, section_id);
"#;

/// Where a comment is from.
pub const FILE: &str = "file";
pub const PERSON: &str = "person";
pub const ASSISTANT: &str = "assistant";

/// Where a comment stands.
pub const OPEN: &str = "open";
pub const APPLIED: &str = "applied";
pub const DONE: &str = "done";

/// Who the assistant's comments are by, in the manuscript and in an exported file.
pub const ASSISTANT_NAME: &str = "Wisp Paper 助手";

/// The most remarks one review is asked for.
pub const MAX_REMARKS: usize = 12;

/// The longest text one review reads, in bytes: a section of a paper is shorter, a whole thesis chapter is to be reviewed in parts.
pub const REVIEW_LIMIT: usize = 36_000;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Comment {
    pub id: String,
    pub manuscript_id: String,
    pub section_id: String,
    pub quote: String,
    pub body: String,
    pub author: String,
    /// `FILE`, `PERSON` or `ASSISTANT`.
    pub origin: String,
    pub suggestion: String,
    pub replaced: String,
    /// `OPEN`, `APPLIED` or `DONE`.
    pub state: String,
    pub created_at: String,
}

impl Comment {
    /// A proposal waits for the person: there are words to put in place of the quote.
    pub fn proposes(&self) -> bool {
        self.state == OPEN && !self.suggestion.is_empty()
    }

    /// The comment once its proposal is in the text: it is about the new words, and keeps the old.
    pub fn taken(&self) -> Comment {
        Comment { quote: self.suggestion.clone(), replaced: self.quote.clone(), suggestion: String::new(), state: APPLIED.into(), ..self.clone() }
    }

    /// The comment once the old words are back: its proposal waits again.
    pub fn taken_back(&self) -> Comment {
        Comment { quote: self.replaced.clone(), suggestion: self.quote.clone(), replaced: String::new(), state: OPEN.into(), ..self.clone() }
    }

    /// What a reader of the exported file is told: the remark, and what is proposed while that waits.
    pub fn said(&self) -> String {
        let mut said = self.body.trim().to_string();
        if self.proposes() {
            if !said.is_empty() { said.push('\n') }
            said.push_str(&format!("建议改为：{}", self.suggestion.trim()));
        }
        said
    }
}

/// A remark of the assistant's as it was read off its answer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Remark {
    /// The words it is about, as the text has them.
    pub quote: String,
    pub body: String,
    /// What to put in their place; empty for a remark that changes nothing.
    pub suggestion: String,
}

/// A text without what a model may write differently from the page: spaces, and the two forms of quotation marks. With it, where each character stood.
fn bare(text: &str) -> (String, Vec<usize>) {
    let (mut out, mut at) = (String::new(), vec![]);
    for (offset, c) in text.char_indices() {
        if c.is_whitespace() { continue }
        let c = match c {
            '“' | '”' | '„' | '«' | '»' => '"',
            '‘' | '’' => '\'',
            c => c,
        };
        at.push(offset);
        out.push(c);
    }
    at.push(text.len());
    (out, at)
}

/// Where `quote` stands in `text`: as written, the place nearest to `near` when there are several; else by its
/// characters alone, so a line break or a curled quotation mark the model wrote differently does not lose it.
pub fn locate(text: &str, quote: &str, near: Option<usize>) -> Option<Range<usize>> {
    let quote = quote.trim();
    if quote.is_empty() { return None }
    let places: Vec<usize> = text.match_indices(quote).map(|(at, _)| at).collect();
    if let Some(at) = places.iter().copied().min_by_key(|at| near.map_or(*at, |near| at.abs_diff(near))) { return Some(at..at + quote.len()) }
    let ((plain, offsets), (wanted, _)) = (bare(text), bare(quote));
    if wanted.chars().count() < 4 { return None }
    let found = plain.find(&wanted)?;
    let (first, last) = (plain[..found].chars().count(), plain[..found + wanted.len()].chars().count());
    // To the end of the last character, not to the start of the next: spaces after the quotation are not part of it.
    let end = text[offsets[last - 1]..].chars().next().map_or(text.len(), |c| offsets[last - 1] + c.len_utf8());
    Some(offsets[first]..end)
}

/// What the assistant is told and asked for a review of `text` (a section, or what is selected of it).
/// `wish` is what the person wants looked at, `theirs` the comments of others on this text (the words, who, what was said), `reply_in` the language to remark in.
pub fn review_prompt(language: &str, section: &str, text: &str, wish: &str, theirs: &[(String, String, String)], reply_in: &str) -> Result<(String, String), String> {
    let text = text.trim();
    if text.is_empty() { return Err("这一节还没有文字可以审阅。".into()) }
    if text.len() > REVIEW_LIMIT { return Err("这一节太长，一次审阅不完：先选中其中几段，再让助手审阅。".into()) }
    let language = crate::ai::language_name(language);
    let system = format!(
        "你是学术写作的审稿助手，像在 Word 的审阅模式里那样，给用户正在写的稿件加批注。稿件语言是{language}。\n\
规则：\n\
1. 每条批注针对稿件里的一处原文。`quote` 必须从稿件中逐字摘出：不改一个字，不跨段落，尽量短但能唯一定位（一句或半句）。\n\
2. `comment` 用{reply_in}写，一两句话：指出问题，或说明这处改了什么、为什么改。\n\
3. 需要改文字时给出 `replacement`：用来整体替换 quote 的文字，用稿件的语言（{language}）写；只提醒、不改文字时给空字符串。要补充内容时，quote 取要接着写的那一句，replacement 是那一句加上补充的内容。\n\
4. 不要编造文献、数据和引用；原文里的引用标记（如 [@Zhang2024]、(Zhang et al., 2021)、[1]）原样保留。没有依据的补充要在 comment 里说明需要作者核实。\n\
5. 各条批注的 quote 不要重叠。最多 {MAX_REMARKS} 条，先说最要紧的；没有可说的就输出 []。\n\
6. 只输出一个 JSON 数组，不要其他文字、不要代码块标记：[{{\"quote\": \"…\", \"comment\": \"…\", \"replacement\": \"…\"}}]"
    );
    let mut message = format!("这一节的名称：{}\n", if section.trim().is_empty() { "（未命名）" } else { section.trim() });
    message.push_str(&format!("用户的要求：{}\n", if wish.trim().is_empty() { "通读这一节，指出最值得改的地方：逻辑是否连贯、表述是否准确简洁、论据和细节是否充分。能直接改好的给出修改。" } else { wish.trim() }));
    if !theirs.is_empty() {
        message.push_str("\n合作者已经在稿件上留了这些批注，请优先回应（能改的给出修改，quote 可以取批注所指的原文）：\n");
        for (quote, author, body) in theirs {
            let quote: String = quote.chars().take(120).collect();
            message.push_str(&format!("- {}对「{quote}」说：{}\n", if author.is_empty() { "合作者" } else { author }, body.replace('\n', " ")));
        }
    }
    message.push_str(&format!("\n稿件：\n{text}"));
    Ok((system, message))
}

/// The remarks in a model's answer, each with its quotation as `text` has it, and how many were left out because their quotation is not in the text.
pub fn parse_review(reply: &str, text: &str) -> Result<(Vec<Remark>, usize), String> {
    let wrong = "模型的回答不是要求的格式。";
    let (start, end) = reply.find('[').zip(reply.rfind(']')).filter(|(start, end)| start < end).ok_or(wrong)?;
    let list: Vec<Value> = serde_json::from_str(&reply[start..=end]).map_err(|_| wrong)?;
    let field = |item: &Value, names: &[&str]| names.iter().find_map(|name| item[*name].as_str()).unwrap_or("").trim().to_string();
    let (mut remarks, mut lost): (Vec<Remark>, usize) = (vec![], 0);
    let mut taken: Vec<Range<usize>> = vec![];
    for item in list.iter().take(MAX_REMARKS * 2) {
        let (quote, body, suggestion) = (field(item, &["quote", "原文"]), field(item, &["comment", "批注"]), field(item, &["replacement", "改为", "suggestion"]));
        if quote.is_empty() || (body.is_empty() && suggestion.is_empty()) { continue }
        // The quotation as the text has it, within one paragraph; one that overlaps an earlier remark's would be changed twice.
        let Some(found) = locate(text, &quote, None).filter(|found| !text[found.clone()].contains('\n')) else {
            lost += 1;
            continue;
        };
        if taken.iter().any(|other| other.start < found.end && found.start < other.end) { continue }
        let quote = text[found.clone()].to_string();
        let suggestion = if suggestion == quote { String::new() } else { suggestion.lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>().join("\n\n") };
        // A proposal to leave the words as they are, with nothing said about them, is no remark.
        if body.is_empty() && suggestion.is_empty() { continue }
        taken.push(found);
        remarks.push(Remark { quote, body, suggestion });
        if remarks.len() == MAX_REMARKS { break }
    }
    Ok((remarks, lost))
}

const COLUMNS: &str = "id, manuscript_id, section_id, quote, body, author, origin, suggestion, replaced, state, created_at";

fn read_comment(r: &rusqlite::Row) -> rusqlite::Result<Comment> {
    Ok(Comment { id: r.get(0)?, manuscript_id: r.get(1)?, section_id: r.get(2)?, quote: r.get(3)?, body: r.get(4)?, author: r.get(5)?, origin: r.get(6)?, suggestion: r.get(7)?, replaced: r.get(8)?, state: r.get(9)?, created_at: r.get(10)? })
}

pub(crate) fn insert_comment(db: &rusqlite::Connection, comment: &Comment) -> rusqlite::Result<()> {
    db.execute(
        &format!("INSERT INTO manuscript_comments({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"),
        params![comment.id, comment.manuscript_id, comment.section_id, comment.quote, comment.body, comment.author, comment.origin, comment.suggestion, comment.replaced, comment.state, comment.created_at],
    )?;
    Ok(())
}

impl Store {
    /// A manuscript's comments, in the order they were made.
    pub fn comments(&self, manuscript_id: &str) -> Result<Vec<Comment>> {
        let mut stmt = self.db.prepare(&format!("SELECT {COLUMNS} FROM manuscript_comments WHERE manuscript_id = ?1 ORDER BY created_at, rowid"))?;
        Ok(stmt.query_map([manuscript_id], read_comment)?.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The person's or the assistant's comment on words of a section.
    pub fn add_comment(&mut self, manuscript_id: &str, section_id: &str, quote: &str, body: &str, author: &str, origin: &str) -> Result<Comment> {
        if body.trim().is_empty() { bail!("批注还没有写内容。") }
        let comment = Comment { id: new_id("C"), manuscript_id: manuscript_id.into(), section_id: section_id.into(), quote: quote.trim().into(), body: body.trim().into(), author: author.trim().into(), origin: origin.into(), state: OPEN.into(), created_at: now_iso(), ..Default::default() };
        insert_comment(&self.db, &comment)?;
        Ok(comment)
    }

    /// The remarks of one review of the assistant's, kept together or not at all.
    pub fn add_remarks(&mut self, manuscript_id: &str, section_id: &str, remarks: &[Remark]) -> Result<Vec<Comment>> {
        let now = now_iso();
        let made: Vec<Comment> = remarks.iter().map(|remark| Comment { id: new_id("C"), manuscript_id: manuscript_id.into(), section_id: section_id.into(), quote: remark.quote.clone(), body: remark.body.clone(), author: ASSISTANT_NAME.into(), origin: ASSISTANT.into(), suggestion: remark.suggestion.clone(), state: OPEN.into(), created_at: now.clone(), ..Default::default() }).collect();
        let tx = self.db.transaction()?;
        for comment in &made { insert_comment(&tx, comment)? }
        tx.commit()?;
        Ok(made)
    }

    /// Keep a comment as it now is: its words, what it proposes or replaced, where it stands.
    pub fn save_comment(&mut self, comment: &Comment) -> Result<()> {
        let changed = self.db.execute("UPDATE manuscript_comments SET quote = ?2, body = ?3, suggestion = ?4, replaced = ?5, state = ?6 WHERE id = ?1", params![comment.id, comment.quote, comment.body, comment.suggestion, comment.replaced, comment.state])?;
        if changed == 0 { bail!("这条批注已经不在了。") }
        Ok(())
    }

    /// Remove a comment of the person's or the assistant's. One that came with a Word file stays: it is that file's.
    pub fn remove_comment(&mut self, id: &str) -> Result<()> {
        let origin: Option<String> = rusqlite::OptionalExtension::optional(self.db.query_row("SELECT origin FROM manuscript_comments WHERE id = ?1", [id], |r| r.get(0)))?;
        if origin.as_deref() == Some(FILE) { bail!("这条批注是随 Word 文件来的，会留在文件里；处理完可以标为已处理。") }
        self.db.execute("DELETE FROM manuscript_comments WHERE id = ?1", [id])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn a_quotation_is_found_as_written_or_by_its_characters() {
        let text = "Roots grow down. Roots grow “deep” into\nthe soil. Roots grow down.";
        assert_eq!(locate(text, "Roots grow down.", None), Some(0..16));
        assert_eq!(locate(text, "Roots grow down.", Some(60)), Some(text.len() - 16..text.len()));
        // The model wrote straight quotation marks and one line.
        let found = locate(text, "grow \"deep\" into the soil", None).unwrap();
        assert_eq!(&text[found], "grow “deep” into\nthe soil");
        assert_eq!(locate(text, "leaves", None), None);
        assert_eq!(locate(text, "  ", None), None);
        assert_eq!(locate("单细胞 时空组学的方法", "单细胞时空组学", None).map(|found| &"单细胞 时空组学的方法"[found]), Some("单细胞 时空组学"));
    }

    #[test]
    fn a_review_is_read_with_its_quotations_checked() {
        let text = "Roots grow down into the soil.\n\nRoot hairs take up water. They are short-lived.";
        let reply = "```json\n[{\"quote\": \"Roots grow down\", \"comment\": \"太笼统\", \"replacement\": \"Primary roots grow downward\"},\n {\"quote\": \"grow down into\", \"comment\": \"与上一条重叠\", \"replacement\": \"x\"},\n {\"quote\": \"Leaves are green\", \"comment\": \"不在文中\", \"replacement\": \"\"},\n {\"quote\": \"They are short-lived.\", \"comment\": \"需要引用\", \"replacement\": \"They are short-lived.\"},\n {\"quote\": \"soil.\\n\\nRoot hairs\", \"comment\": \"跨段\"},\n {\"quote\": \"\", \"comment\": \"没有原文\"}]\n```";
        let (remarks, lost) = parse_review(reply, text).unwrap();
        assert_eq!(remarks, [Remark { quote: "Roots grow down".into(), body: "太笼统".into(), suggestion: "Primary roots grow downward".into() }, Remark { quote: "They are short-lived.".into(), body: "需要引用".into(), suggestion: String::new() }]);
        assert_eq!(lost, 2);
        assert_eq!(parse_review("[]", text).unwrap(), (vec![], 0));
        assert!(parse_review("我觉得写得很好。", text).is_err());
        let (system, message) = review_prompt("en", "Introduction", text, "", &[("the soil".into(), "Li".into(), "which soil?".into())], "中文").unwrap();
        assert!(system.contains("稿件语言是英文") && system.contains("用中文写") && message.contains("Li对「the soil」说：which soil?") && message.ends_with(text));
        assert!(review_prompt("en", "x", "  ", "", &[], "中文").is_err());
        assert!(review_prompt("en", "x", &"长".repeat(REVIEW_LIMIT), "", &[], "中文").is_err());
    }

    #[test]
    fn comments_are_kept_taken_and_taken_back() {
        let mut store = Store::in_memory(Path::new("/tmp/wisp-comment-test")).unwrap();
        let manuscript = store.create_manuscript("Draft", "en").unwrap();
        let section = manuscript.sections[1].id.clone();
        let mine = store.add_comment(&manuscript.id, &section, " grow down ", "Say how fast.", "Xu", PERSON).unwrap();
        assert!(store.add_comment(&manuscript.id, &section, "x", "  ", "Xu", PERSON).is_err());
        let made = store.add_remarks(&manuscript.id, &section, &[Remark { quote: "Roots".into(), body: "更具体".into(), suggestion: "Primary roots".into() }, Remark { quote: "soil".into(), body: "哪种土？".into(), suggestion: String::new() }]).unwrap();
        assert!(made[0].proposes() && !made[1].proposes());
        assert_eq!(made[0].said(), "更具体\n建议改为：Primary roots");
        let taken = made[0].taken();
        assert_eq!((taken.quote.as_str(), taken.replaced.as_str(), taken.suggestion.as_str(), taken.state.as_str()), ("Primary roots", "Roots", "", APPLIED));
        assert_eq!(taken.said(), "更具体");
        store.save_comment(&taken).unwrap();
        assert_eq!(store.comments(&manuscript.id).unwrap().iter().map(|c| (c.quote.as_str(), c.state.as_str())).collect::<Vec<_>>(), [("grow down", OPEN), ("Primary roots", APPLIED), ("soil", OPEN)]);
        assert_eq!(taken.taken_back(), made[0]);
        store.remove_comment(&mine.id).unwrap();
        // One that came with a file stays; a section removed takes its comments along.
        let theirs = Comment { id: "C-file".into(), manuscript_id: manuscript.id.clone(), section_id: section.clone(), body: "from Word".into(), origin: FILE.into(), state: OPEN.into(), created_at: now_iso(), ..Default::default() };
        insert_comment(&store.db, &theirs).unwrap();
        assert!(store.remove_comment("C-file").is_err());
        assert_eq!(store.comments(&manuscript.id).unwrap().len(), 3);
        store.remove_section(&section).unwrap();
        assert!(store.comments(&manuscript.id).unwrap().is_empty());
    }
}
