//! A Word file written elsewhere, cited anew: its citations and its
//! reference list in a CSL style, each citation a field that jumps to its
//! entry. The file is no manuscript of the app and does not become one.
//!
//! `inspect` reads what the file cites, in this order of trust. Fields of
//! Zotero's (`ADDIN ZOTERO_ITEM CSL_CITATION {…}`, which Mendeley writes
//! too) carry every cited work as CSL-JSON, so nothing is guessed. Without
//! them the file is read as text: the paragraphs after a heading such as
//! References / 参考文献 (or inside EndNote's or Word's own list field) are
//! the entries, and the citations of the body are found by number (`[1]`,
//! `[2, 4–6]`, raised numbers) or by first author and year (`(Zhang et al.,
//! 2024a; Li & Wang, 2019)`, `张三等（2020）`). An entry becomes a record
//! from the library's paper it is (DOI, PMID, title), from a lookup of its
//! DOI, from its own text, or from a lookup by its text; one that could not
//! be read stays as written, with the citations that name it.
//!
//! `restyle` renders the records with `csl::render_records` and writes the
//! file again with `docx::rewrite`: each citation replaced where it stands
//! (a field of Zotero's or EndNote's first made plain text, whole), the old
//! list replaced by the new one. Nothing else of the file changes.

use crate::cite::Rich;
use crate::csl::{self, Cite, Record};
use crate::docx::{self, Link, Role, Wanted};
use crate::model::{Paper, normalize_doi, strip_markup, title_key};
use crate::sources::Fetch;
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::ops::Range;

/// How an entry came to be a record: carried by the file's own field, a
/// paper of the library, looked up, read from its own text, given by
/// whoever asked, or not at all.
pub const FIELD: &str = "field";
pub const LIBRARY: &str = "library";
pub const LOOKUP: &str = "lookup";
pub const TEXT: &str = "text";
pub const GIVEN: &str = "given";
pub const UNRESOLVED: &str = "unresolved";

/// How sure that is.
pub const SURE: &str = "sure";
pub const LIKELY: &str = "likely";
pub const GUESS: &str = "guess";

/// A work the file's list holds, or one its fields carry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    /// The record's id, and what a citation's link leads to: `r3`.
    pub id: String,
    /// Its paragraph in the file's list; none for a work known from a field alone.
    pub paragraph: Option<usize>,
    /// The number the file's citations name it by; 0 in a list without numbers.
    pub number: usize,
    /// As the file's list says it; empty for a work known from a field alone.
    pub text: String,
    pub origin: &'static str,
    pub sure: &'static str,
    /// The library's paper it is.
    pub paper: String,
    pub record: Option<Record>,
    /// What went wrong on the way, for the report.
    pub note: String,
    /// What a field names it by.
    key: String,
    /// The letter its year carries in the file: `a` of `2024a`.
    letter: String,
}

/// A citation in the file's text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    pub paragraph: usize,
    /// In bytes of the paragraph's text.
    pub range: Range<usize>,
    pub text: String,
    /// `field`, `numbered`, `raised` or `author-year`.
    pub kind: &'static str,
    /// The entries it names, by their ids.
    pub cites: Vec<Cite>,
    /// Why it stays as written; empty for one that is written anew.
    pub problem: String,
    /// The fields of the file its words are shown by.
    fields: Vec<usize>,
    raised: bool,
}

/// What a file cites.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inspected {
    /// Whose citations they are: `zotero`, `mendeley`, `endnote`, `word` or `text`.
    pub source: &'static str,
    /// `zh` or `en`, by the script of the text.
    pub language: String,
    pub paragraphs: Vec<docx::ReadPara>,
    pub entries: Vec<Entry>,
    pub citations: Vec<Found>,
    /// The paragraphs of the file's list.
    pub list: Option<Range<usize>>,
    pub notes: Vec<String>,
    list_fields: Vec<usize>,
}

/// What the assistant is told with a question that came with Word files: where they are and what it can do with them.
pub fn note(paths: &[String]) -> String {
    let files: Vec<String> = paths.iter().map(|path| format!("- {path}")).collect();
    format!("用户随这个问题附上了 Word 文件（本机路径）：\n{}\n这是在别处写好的稿件，不是写作页的稿件，不要用稿件工具去读写它。按用户的话决定做什么。用户要把它参考文献表里的文献收进来（建一个文献库）时，用 import_docx_references（导入前用户会在窗口里确认），再如实告诉用户收进了多少篇、跳过了多少篇、哪些条目没能识别；没能识别的条目你读得懂时，用 entries 给出它们再调用一次。用户只交来文件、没说做什么时，多半想重排它的引用格式：先用 read_docx_citations 读出它引用了什么，把识别结果如实告诉用户（引用来自 Zotero 域还是按文字识别、参考文献多少条、哪些没能识别、哪些引用会保持原样）；用户没说用哪个样式时问清楚（list_cite_styles 有可用的样式）；要在某个样式上改动时用 read_cite_style 读出它、改好、用 try_cite_style 试排给用户看，再把改好的 CSL 交给 restyle_docx。restyle_docx 写出一个新文件，保存位置由用户在保存对话框里选，原文件不会改动；不要自己填 output。", files.join("\n"))
}

// ---- small readers of text

fn is_cjk(c: char) -> bool {
    matches!(c, '\u{4e00}'..='\u{9fff}' | '\u{3400}'..='\u{4dbf}' | '\u{3040}'..='\u{30ff}' | '\u{ac00}'..='\u{d7af}')
}

fn language(paragraphs: &[docx::ReadPara]) -> &'static str {
    let (mut cjk, mut letters) = (0usize, 0usize);
    for c in paragraphs.iter().flat_map(|para| para.text.chars()).filter(|c| c.is_alphabetic()) {
        letters += 1;
        if is_cjk(c) { cjk += 1 }
    }
    if cjk * 3 > letters { "zh" } else { "en" }
}

/// A year at the start of `text`, with the letter after it: `2024a`. The year is not part of a longer number.
fn year_at(text: &str) -> Option<(i32, String, usize)> {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    if digits != 4 { return None }
    let year: i32 = text[..4].parse().ok()?;
    if !(1500..=2099).contains(&year) { return None }
    let rest = &text[4..];
    let letter = rest.chars().next().filter(|c| c.is_ascii_lowercase() && !rest[1..].starts_with(|c: char| c.is_alphanumeric()));
    Some((year, letter.map(String::from).unwrap_or_default(), 4 + letter.map_or(0, |_| 1)))
}

/// Every year a text names, where each stands.
fn years(text: &str) -> Vec<(usize, i32, String, usize)> {
    let mut out = vec![];
    let mut before = ' ';
    let mut skip = 0;
    for (at, c) in text.char_indices() {
        if at >= skip && c.is_ascii_digit() && !before.is_ascii_digit() && let Some((year, letter, len)) = year_at(&text[at..]) {
            out.push((at, year, letter, len));
            skip = at + len;
        }
        before = c;
    }
    out
}

/// The number an entry of a numbered list opens with, and where its own words begin: `[3] `, `3. `, `(3) `, `3 `.
fn label(text: &str) -> Option<(usize, usize)> {
    let open = text.chars().next().filter(|c| matches!(c, '[' | '(' | '［' | '（'));
    let from = open.map_or(0, char::len_utf8);
    let digits = text[from..].bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 4 { return None }
    let number: usize = text[from..from + digits].parse().ok()?;
    let rest = &text[from + digits..];
    let next = rest.chars().next()?;
    let after = match (open, next) {
        (Some(_), ']' | ')' | '］' | '）') => from + digits + next.len_utf8(),
        (Some(_), _) => return None,
        // Not a year that opens the entry, nor a number with a fraction.
        (None, '.' | '、' | '．' | ')' | '）') if number < 1500 && !rest[next.len_utf8()..].starts_with(|c: char| c.is_ascii_digit()) => from + digits + next.len_utf8(),
        (None, c) if c.is_whitespace() && number < 1000 => from + digits,
        _ => return None,
    };
    if number == 0 { return None }
    let words = text[after..].trim_start_matches(['.', '、', '．']).trim_start();
    Some((number, text.len() - words.len()))
}

/// `Zhang` of `ZHANG`: a family name set in capitals, as GB/T 7714 has them.
fn cased(name: &str) -> String {
    if name.chars().count() < 2 || name.chars().any(|c| c.is_lowercase()) || !name.chars().any(|c| c.is_alphabetic()) { return name.to_string() }
    let mut out = String::new();
    let mut start = true;
    for c in name.chars() {
        if start { out.extend(c.to_uppercase()) } else { out.extend(c.to_lowercase()) }
        start = !c.is_alphabetic();
    }
    out
}

/// A word that is initials: `W.`, `T.-Q.`, `WX`, `J.K.`.
fn initials(word: &str) -> bool {
    let letters: Vec<char> = word.chars().filter(|c| c.is_alphabetic()).collect();
    !letters.is_empty() && letters.len() <= 3 && letters.iter().all(|c| c.is_uppercase()) && word.chars().all(|c| c.is_alphabetic() || matches!(c, '.' | '-' | ' '))
}

/// Initials written together, each said by itself: `MCP` is `M. C. P.`, which a style then sets as it sets initials. Anything else as it is.
fn spelled(given: &str) -> String {
    if given.chars().count() < 2 || !given.chars().all(|c| c.is_uppercase()) || !initials(given) { return given.to_string() }
    given.chars().map(|c| format!("{c}.")).collect::<Vec<_>>().join(" ")
}

const COMPOUND: [&str; 12] = ["欧阳", "司马", "诸葛", "上官", "司徒", "夏侯", "皇甫", "尉迟", "公孙", "慕容", "令狐", "东方"];

/// One author as written, family name and given name.
fn person(name: &str) -> Option<(String, String)> {
    let name = name.trim().trim_matches(['.', ',', ';', '，', '&', ' ']).trim();
    let name = name.strip_prefix("and ").unwrap_or(name).trim();
    if name.is_empty() || name.chars().count() > 60 || name.chars().any(|c| c.is_ascii_digit()) { return None }
    if matches!(name.to_lowercase().as_str(), "et al" | "等" | "等人" | "others" | "…" | "...") { return None }
    if name.chars().all(|c| is_cjk(c) || c == '·') {
        let count = name.chars().count();
        if count < 2 { return Some((name.to_string(), String::new())) }
        let cut = if count > 2 && COMPOUND.iter().any(|family| name.starts_with(family)) { 2 } else { 1 };
        let at = name.char_indices().nth(cut).map_or(name.len(), |(at, _)| at);
        return Some((name[..at].to_string(), name[at..].to_string()));
    }
    if let Some((family, given)) = name.split_once(',') { return Some((cased(family.trim()), given.trim().to_string())) }
    let words: Vec<&str> = name.split_whitespace().collect();
    match words.as_slice() {
        [] => None,
        // A name of one word may be a body's: WHO.
        [one] => Some((one.to_string(), String::new())),
        // `Zhang W`, `van der Berg AB`: the family name first, initials after it. `ZHANG Wei` likewise.
        [family @ .., last] if initials(last) || family.iter().all(|word| !word.chars().any(|c| c.is_lowercase())) => Some((cased(&family.join(" ")), spelled(last))),
        // `Wei Zhang`: the family name last.
        [given @ .., last] => Some((cased(last), given.join(" "))),
    }
}

/// The authors a list of names holds: `Zhang, W., Li, Q., & Wang, F.`, `Zhang W, Li Q, Wang F`, `张三, 李四, 等`.
fn people(names: &str) -> Vec<(String, String)> {
    let names = names.replace(" & ", ", ").replace(" and ", ", ").replace(['，', '、', ';', '；'], ",");
    let mut out: Vec<(String, String)> = vec![];
    for piece in names.split(',').map(str::trim).filter(|piece| !piece.is_empty()) {
        // Initials after a comma are the given name of the family name before them.
        let given_alone = initials(piece.trim_end_matches('.')) && piece.contains('.') || (initials(piece) && out.last().is_some_and(|(_, given)| given.is_empty()));
        if given_alone && let Some((_, given)) = out.last_mut().filter(|(_, given)| given.is_empty()) {
            *given = piece.to_string();
            continue;
        }
        if let Some(found) = person(piece) { out.push(found) }
    }
    out
}

/// A reference as its text says it, as far as rules can tell.
#[derive(Clone, Debug, Default, PartialEq)]
struct Parsed {
    authors: Vec<(String, String)>,
    year: Option<i32>,
    letter: String,
    title: String,
    container: String,
    volume: String,
    issue: String,
    pages: String,
    doi: String,
    pmid: String,
    kind: &'static str,
    publisher: String,
}

impl Parsed {
    fn complete(&self) -> bool {
        !self.authors.is_empty() && self.year.is_some() && self.title.chars().filter(|c| c.is_alphabetic()).count() >= 4
    }

    fn record(&self) -> Record {
        let mut record = Record::new();
        let mut set = |name: &str, value: &str| {
            if !value.trim().is_empty() { record.insert(name.into(), value.trim().into()); }
        };
        set("type", if self.kind.is_empty() { "article-journal" } else { self.kind });
        set("title", &self.title);
        set("container-title", &self.container);
        set("volume", &self.volume);
        set("issue", &self.issue);
        set("page", &self.pages);
        set("DOI", &self.doi);
        set("PMID", &self.pmid);
        set("publisher", &self.publisher);
        let authors: Vec<Value> = self.authors.iter().map(|(family, given)| if given.is_empty() { json!({"family": family}) } else { json!({"family": family, "given": given}) }).collect();
        if !authors.is_empty() { record.insert("author".into(), authors.into()); }
        if let Some(year) = self.year { record.insert("issued".into(), json!({"date-parts": [[year]]})); }
        record
    }
}

/// Where a sentence of a reference ends: a full stop (or `?`, `!`) before a space or a Chinese letter, not one after an initial.
fn sentence_end(text: &str, from: usize) -> Option<usize> {
    let mut before = ['\0', '\0'];
    for (at, c) in text[from..].char_indices().map(|(at, c)| (from + at, c)) {
        let next = text[at + c.len_utf8()..].chars().next();
        let ends = match c {
            '．' | '。' => true,
            '.' | '?' | '!' => next.is_none_or(|next| next.is_whitespace() || is_cjk(next)) && !(c == '.' && before[1].is_uppercase() && !before[0].is_alphabetic()),
            _ => false,
        };
        if ends { return Some(at) }
        before = [before[1], c];
    }
    None
}

/// Where the authors of a reference without a year in brackets end: the first full stop that is not an initial's, with a title after it.
fn authors_end(text: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(found) = text[from..].find(['.', '．', '。']) {
        let at = from + found;
        let c = text[at..].chars().next()?;
        let rest = &text[at + c.len_utf8()..];
        from = at + c.len_utf8();
        if c == '.' && !rest.starts_with(|c: char| c.is_whitespace() || is_cjk(c)) { continue }
        let next = rest.trim_start();
        if next.is_empty() { return None }
        // More names follow an initial's full stop: `Li, Q. & Wang, F.`, `W. X.`, `Q., Wang`.
        let word = next.split_whitespace().next().unwrap_or("");
        if c == '.' && (word == "&" || word == "and" || (word.contains('.') && initials(word.trim_end_matches(',')))) { continue }
        return Some(at);
    }
    None
}

/// Volume, issue and pages as the tail of a reference says them: `12(3), 45–67`, `2024;12(3):45-67`, `10, 45–67 (2024)`.
fn numbers(tail: &str, parsed: &mut Parsed) {
    let chars: Vec<(usize, char)> = tail.char_indices().collect();
    let digits_at = |at: usize| tail[at..].bytes().take_while(u8::is_ascii_digit).count();
    let mut n = 0;
    let mut loose: Vec<String> = vec![];
    while n < chars.len() {
        let (at, c) = chars[n];
        let fresh = n == 0 || !(chars[n - 1].1.is_alphanumeric());
        if !(c.is_ascii_digit() && fresh) && !(c == 'e' && fresh && digits_at(at + 1) >= 3) {
            n += 1;
            continue;
        }
        let lead = if c == 'e' { 1 } else { 0 };
        let len = lead + digits_at(at + lead);
        let word = &tail[at..at + len];
        let rest = &tail[at + len..];
        let dash = rest.trim_start().chars().next().filter(|c| matches!(c, '-' | '–' | '—' | '‐'));
        let after_dash = dash.map(|dash| rest.trim_start()[dash.len_utf8()..].trim_start());
        if let Some(end) = after_dash.filter(|after| after.starts_with(|c: char| c.is_ascii_digit())) {
            // A range is the pages.
            let last = end.bytes().take_while(|b| b.is_ascii_alphanumeric()).count();
            if parsed.pages.is_empty() { parsed.pages = format!("{word}–{}", &end[..last]) }
            n += tail[at..tail.len() - end.len() + last].chars().count();
            continue;
        }
        if rest.starts_with('(') && let Some(close) = rest.find(')') && parsed.volume.is_empty() && year_at(word).is_none() {
            // `12(3)`: volume and issue.
            parsed.volume = word.to_string();
            parsed.issue = rest[1..close].trim().to_string();
            n += tail[at..at + len + close + 1].chars().count();
            continue;
        }
        if year_at(word).is_none() || c == 'e' { loose.push(word.to_string()) }
        n += word.chars().count();
    }
    // What is left: a volume before the pages, an article number in their place.
    let mut loose = loose.into_iter();
    if parsed.volume.is_empty() && let Some(volume) = loose.next() { parsed.volume = volume }
    if parsed.pages.is_empty() && let Some(pages) = loose.last() { parsed.pages = pages }
}

/// The kinds GB/T 7714 marks a reference with, as CSL names them.
fn marked_kind(mark: &str) -> &'static str {
    match mark.split('/').next().unwrap_or("") {
        "M" => "book",
        "D" => "thesis",
        "C" => "paper-conference",
        "N" => "article-newspaper",
        "R" => "report",
        "P" => "patent",
        "S" => "standard",
        "EB" | "OL" => "webpage",
        _ => "article-journal",
    }
}

/// `[J]`, `[M]`, `[EB/OL]` in a reference: where it stands, how long it is, and what it says.
fn gb_mark(text: &str) -> Option<(usize, usize, &str)> {
    let mut from = 0;
    while let Some(found) = text[from..].find('[') {
        let at = from + found;
        from = at + 1;
        let Some(close) = text[at..].find(']') else { break };
        let mark = &text[at + 1..at + close];
        if (1..=5).contains(&mark.len()) && mark.chars().all(|c| c.is_ascii_uppercase() || c == '/') && mark.starts_with(|c: char| c.is_ascii_uppercase()) { return Some((at, close + 1, mark)) }
    }
    None
}

fn tidy(text: &str) -> String {
    text.trim().trim_matches(['.', ',', ';', ':', '，', '。', '．', ' ']).trim().to_string()
}

/// A reference read by rule. Three families are known: GB/T 7714 (`作者. 题名[J]. 刊名, 2020, 12(3): 45-67.`),
/// the author-date ones (`Zhang, W., & Li, Q. (2024). Title. Journal, 12(3), 45–67.`) and those with the year at
/// the end (`Zhang W, Li Q. Title. Journal. 2024;12(3):45-67.`, `… Nat. Plants 10, 45–67 (2024).`).
fn parse_entry(text: &str) -> Parsed {
    let text = label(text).map_or(text, |(_, at)| &text[at..]).trim();
    let mut parsed = Parsed { doi: crate::lookup::find_doi(text).unwrap_or_default(), ..Default::default() };
    if let Some(at) = text.find("PMID") {
        let digits: String = text[at + 4..].trim_start_matches([':', '：', ' ']).chars().take_while(char::is_ascii_digit).collect();
        if (4..=9).contains(&digits.len()) { parsed.pmid = digits }
    }
    // What follows the DOI or an address is no part of the reference's words.
    let lower = text.to_lowercase();
    let cut = ["https://", "http://", "doi:", "doi ", "pmid"].iter().filter_map(|mark| lower.find(mark)).min().unwrap_or(text.len());
    let text = text[..cut].trim();
    let found = years(text);
    if let Some((at, len, mark)) = gb_mark(text) {
        parsed.kind = marked_kind(mark);
        let head = &text[..at];
        let end = authors_end(head).unwrap_or(0);
        parsed.authors = people(&head[..end]);
        parsed.title = tidy(&head[end..]);
        let tail = tidy(&text[at + len..]);
        let year = years(&tail).into_iter().next();
        if let Some((_, year, letter, _)) = &year { (parsed.year, parsed.letter) = (Some(*year), letter.clone()) }
        let name_end = tail.find([',', '，']).unwrap_or(tail.len()).min(year.as_ref().map_or(tail.len(), |(at, ..)| *at));
        if parsed.kind == "article-journal" {
            parsed.container = tidy(&tail[..name_end]);
            let rest = year.map_or("", |(at, _, _, len)| &tail[at + len..]);
            numbers(rest, &mut parsed);
        } else {
            // `北京: 科学出版社, 2020`.
            parsed.publisher = tidy(tail[..name_end].rsplit([':', '：']).next().unwrap_or(""));
        }
        return parsed;
    }
    // Authors, then the year in brackets.
    let bracketed = found.iter().find(|(at, _, _, len)| text[..*at].trim_end().ends_with(['(', '（']) && text[at + len..].trim_start().starts_with([')', '）', ',']) && text[..*at].chars().any(|c| c.is_alphabetic()) && sentence_end(text, 0).is_none_or(|end| end + 2 >= *at || authors_end(text).is_none_or(|end| end + 2 >= *at)));
    if let Some((at, year, letter, len)) = bracketed {
        (parsed.year, parsed.letter) = (Some(*year), letter.clone());
        parsed.authors = people(text[..*at].trim_end().trim_end_matches(['(', '（']));
        let after = &text[at + len..];
        let after = after[after.find([')', '）']).map_or(0, |close| close + 1)..].trim_start_matches(['.', ' ', '．']);
        let end = sentence_end(after, 0).unwrap_or(after.len());
        parsed.title = tidy(&after[..end]);
        let tail = after[end..].trim_start_matches(['.', '?', '!', ' ', '．', '。']);
        let name_end = tail.char_indices().find(|(at, c)| (*c == ',' || *c == '，') && tail[at + c.len_utf8()..].trim_start().starts_with(|c: char| c.is_ascii_digit())).map_or_else(|| sentence_end(tail, 0).unwrap_or(tail.len()), |(at, _)| at);
        parsed.container = tidy(&tail[..name_end]);
        numbers(&tail[name_end..], &mut parsed);
        return parsed;
    }
    // Authors, title, then where it appeared with the year in it.
    let Some(end) = authors_end(text) else { return parsed };
    parsed.authors = people(&text[..end]);
    let after = text[end..].trim_start_matches(['.', ' ', '．', '。']);
    let title_end = sentence_end(after, 0).unwrap_or(after.len());
    parsed.title = tidy(&after[..title_end]);
    let tail = after[title_end..].trim_start_matches(['.', '?', '!', ' ', '．', '。']);
    let tail_years = years(tail);
    // The year in brackets or before a semicolon is the one meant; else the last.
    let year = tail_years.iter().find(|(at, _, _, len)| tail[..*at].ends_with('(') || tail[at + len..].starts_with(';')).or(tail_years.last());
    if let Some((_, year, letter, _)) = year { (parsed.year, parsed.letter) = (Some(*year), letter.clone()) }
    let name_end = tail.char_indices().find(|(at, c)| c.is_ascii_digit() && (*at == 0 || !tail[..*at].ends_with(|c: char| c.is_alphanumeric()))).map_or(tail.len(), |(at, _)| at);
    parsed.container = tidy(tail[..name_end].trim_end_matches(['(', ' ']));
    let rest: String = match year {
        // The year is no volume: what is around it is read without it.
        Some((at, _, _, len)) if *at >= name_end => format!("{} {}", &tail[name_end..*at], &tail[at + len..]),
        _ => tail[name_end..].to_string(),
    };
    numbers(&rest, &mut parsed);
    parsed
}

// ---- entries

fn family_of(record: &Record) -> Vec<String> {
    let Some(first) = record.get("author").and_then(|authors| authors.get(0)) else { return vec![] };
    let (family, given, literal) = (first["family"].as_str().unwrap_or(""), first["given"].as_str().unwrap_or(""), first["literal"].as_str().unwrap_or(""));
    // A Chinese name is said whole in a citation: 张三等.
    [family.to_string(), format!("{family}{given}"), literal.to_string()].into_iter().filter(|name| !name.is_empty()).collect()
}

fn second_family(record: &Record) -> String {
    record.get("author").and_then(|authors| authors.get(1)).and_then(|second| second["family"].as_str().or(second["literal"].as_str())).unwrap_or("").to_string()
}

fn year_of(record: &Record) -> Option<i32> {
    let first = record.get("issued")?.get("date-parts")?.get(0)?.get(0)?;
    first.as_i64().map(|year| year as i32).or_else(|| first.as_str()?.trim().parse().ok())
}

/// A work as Crossref says it, which is CSL-JSON but for its lists of one title.
fn crossref_record(work: &Value) -> Option<Record> {
    let first = |name: &str| work[name].get(0).and_then(Value::as_str).or(work[name].as_str()).map(strip_markup).unwrap_or_default();
    let title = first("title");
    if title.is_empty() { return None }
    let mut record = Record::new();
    let kind = match work["type"].as_str().unwrap_or("") {
        "book-chapter" | "book-section" | "book-part" => "chapter",
        "proceedings-article" => "paper-conference",
        "book" | "monograph" | "edited-book" | "reference-book" => "book",
        "dissertation" => "thesis",
        "report" => "report",
        "posted-content" => "article",
        _ => "article-journal",
    };
    record.insert("type".into(), kind.into());
    record.insert("title".into(), title.into());
    for (from, to) in [("container-title", "container-title"), ("volume", "volume"), ("issue", "issue"), ("page", "page"), ("DOI", "DOI"), ("publisher", "publisher")] {
        let value = first(from);
        if !value.is_empty() { record.insert(to.into(), value.into()); }
    }
    let authors: Vec<Value> = work["author"].as_array().into_iter().flatten().filter_map(|author| match (author["family"].as_str(), author["given"].as_str(), author["name"].as_str()) {
        (Some(family), Some(given), _) => Some(json!({"family": family, "given": given})),
        (Some(family), None, _) => Some(json!({"family": family})),
        (None, _, Some(name)) => Some(json!({"literal": name})),
        _ => None,
    }).collect();
    if !authors.is_empty() { record.insert("author".into(), authors.into()); }
    let year = ["published-print", "published", "issued", "published-online"].iter().find_map(|name| work[*name]["date-parts"][0][0].as_i64());
    if let Some(year) = year { record.insert("issued".into(), json!({"date-parts": [[year]]})); }
    Some(record)
}

/// The library's paper a reference is: by DOI or PMID, else by its title standing in the reference's text.
fn held<'a>(text: &str, parsed: &Parsed, papers: &'a [Paper]) -> Option<(&'a Paper, &'static str)> {
    if !parsed.doi.is_empty() && let Some(paper) = papers.iter().find(|paper| !paper.doi.is_empty() && normalize_doi(&paper.doi) == parsed.doi) { return Some((paper, SURE)) }
    if !parsed.pmid.is_empty() && let Some(paper) = papers.iter().find(|paper| paper.pmid.trim() == parsed.pmid) { return Some((paper, SURE)) }
    let key = title_key(text);
    let long_enough = |title: &str| title.chars().count() >= if title.chars().any(is_cjk) { 8 } else { 16 };
    let paper = papers.iter().map(|paper| (paper, title_key(&paper.title))).filter(|(_, title)| long_enough(title) && key.contains(title.as_str())).max_by_key(|(_, title)| title.len())?.0;
    // The same title in another year is a preprint and its paper, or two editions: said, not refused.
    let same_year = paper.year.is_some_and(|year| text.contains(&year.to_string()));
    Some((paper, if same_year { SURE } else { LIKELY }))
}

/// What the entry's own text adds to a record that has no volume, issue or pages.
fn filled(mut record: Record, parsed: &Parsed) -> Record {
    for (name, value) in [("volume", &parsed.volume), ("issue", &parsed.issue), ("page", &parsed.pages)] {
        if !value.is_empty() && !record.contains_key(name) { record.insert(name.into(), value.clone().into()); }
    }
    record
}

fn with_id(mut record: Record, id: &str) -> Record {
    record.insert("id".into(), id.into());
    record
}

/// Crossref searched by an entry's words: only a record whose title the entry itself says, since the first hit of
/// a search is not the work meant.
fn searched(text: &str, fetch: Fetch) -> Result<Option<Record>, String> {
    let words: String = label(text).map_or(text, |(_, at)| &text[at..]).chars().take(300).collect();
    let url = format!("https://api.crossref.org/works?rows=3&query.bibliographic={}", crate::lookup::encode(&words));
    let body = serde_json::from_str::<Value>(&fetch(&url)?).unwrap_or_default();
    let key = title_key(text);
    Ok(body["message"]["items"].as_array().into_iter().flatten().filter_map(crossref_record).find(|record| {
        let title = title_key(record["title"].as_str().unwrap_or(""));
        title.chars().count() >= 12 && key.contains(&title)
    }))
}

/// One entry of the file's list as a record, by the best way there is.
fn resolve(entry: &mut Entry, papers: &[Paper], given: Option<&Record>, fetch: Option<Fetch>) {
    let parsed = parse_entry(&entry.text);
    entry.letter = parsed.letter.clone();
    if let Some(record) = given {
        (entry.origin, entry.sure, entry.record) = (GIVEN, LIKELY, Some(with_id(record.clone(), &entry.id)));
        return;
    }
    if let Some((paper, sure)) = held(&entry.text, &parsed, papers) {
        (entry.origin, entry.sure, entry.paper, entry.record) = (LIBRARY, sure, paper.id.clone(), Some(with_id(filled(csl::record_of(paper), &parsed), &entry.id)));
        return;
    }
    let mut notes: Vec<String> = vec![];
    if let (Some(fetch), false) = (fetch, parsed.doi.is_empty()) {
        match fetch(&crate::lookup::crossref_url(&parsed.doi)).map(|body| serde_json::from_str::<Value>(&body).ok().and_then(|body| crossref_record(&body["message"]))) {
            Ok(Some(record)) => {
                (entry.origin, entry.sure, entry.record) = (LOOKUP, SURE, Some(with_id(record, &entry.id)));
                return;
            }
            Ok(None) => notes.push(format!("Crossref 没有 {} 的记录", parsed.doi)),
            Err(error) => notes.push(format!("查询 Crossref 失败：{error}")),
        }
    }
    if parsed.complete() {
        (entry.origin, entry.sure, entry.record) = (TEXT, if !parsed.container.is_empty() || !parsed.kind.is_empty() { LIKELY } else { GUESS }, Some(with_id(parsed.record(), &entry.id)));
        entry.note = crate::sources::reasons(&notes);
        return;
    }
    if let Some(fetch) = fetch {
        match searched(&entry.text, fetch) {
            Ok(Some(record)) => {
                (entry.origin, entry.sure, entry.record) = (LOOKUP, LIKELY, Some(with_id(record, &entry.id)));
                return;
            }
            Ok(None) => notes.push("按文字在 Crossref 没有查到题目相符的记录".into()),
            Err(error) => notes.push(format!("查询 Crossref 失败：{error}")),
        }
    }
    notes.insert(0, "从这条的文字里读不出作者、年份和题目".into());
    (entry.origin, entry.sure, entry.note) = (UNRESOLVED, "", crate::sources::reasons(&notes));
}

// ---- the file's fields

/// The JSON a citation field of Zotero's or Mendeley's carries.
fn csl_citation(code: &str) -> Option<Value> {
    let at = code.find("CSL_CITATION")?;
    let json = &code[at + code[at..].find('{')?..];
    serde_json::Deserializer::from_str(json).into_iter::<Value>().next()?.ok()
}

fn text_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        _ => String::new(),
    }
}

/// The citations of a file that Zotero or Mendeley keeps: every field with its works, and the works once each.
fn from_fields(marks: &docx::Marks, paragraphs: &[docx::ReadPara], entries: &mut Vec<Entry>, citations: &mut Vec<Found>) {
    for (n, field) in marks.fields.iter().enumerate() {
        let Some(citation) = csl_citation(&field.code) else { continue };
        let mut found = Found { paragraph: field.from, range: field.start..field.end, kind: "field", fields: vec![n], ..Default::default() };
        let mut problems: Vec<String> = vec![];
        if !field.whole || field.from != field.to || field.from >= paragraphs.len() || field.end <= field.start {
            problems.push("这个引用域跨了段落或在表格里".into());
        } else {
            found.text = paragraphs[field.from].text.get(field.range()).unwrap_or("").to_string();
            found.raised = marks.raised[field.from].iter().any(|raised| raised.start < field.end && field.start < raised.end);
        }
        if citation["properties"]["dontUpdate"] == true { problems.push("在 Zotero 里手工改过文字".into()) }
        for item in citation["citationItems"].as_array().into_iter().flatten() {
            let key = [&item["uris"][0], &item["uri"][0], &item["itemData"]["id"], &item["id"]].into_iter().map(text_of).find(|key| !key.is_empty()).unwrap_or_default();
            let Some(data) = item["itemData"].as_object() else {
                problems.push("域里没有随文件保存这篇文献的信息".into());
                continue;
            };
            let id = match entries.iter().find(|entry| !key.is_empty() && entry.key == key) {
                Some(entry) => entry.id.clone(),
                None => {
                    let id = format!("r{}", entries.len() + 1);
                    entries.push(Entry { id: id.clone(), origin: FIELD, sure: SURE, record: Some(with_id(data.clone(), &id)), key, ..Default::default() });
                    id
                }
            };
            if [&item["prefix"], &item["suffix"]].into_iter().any(|affix| !text_of(affix).trim().is_empty()) { problems.push("带有前缀或后缀".into()) }
            if item["author-only"] == true { problems.push("只写作者的引用".into()) }
            let locator = text_of(&item["locator"]);
            if !locator.trim().is_empty() && !matches!(item["label"].as_str(), None | Some("page")) { problems.push("定位不是页码".into()) }
            found.cites.push(Cite { id, locator, narrative: item["suppress-author"] == true });
        }
        if found.cites.is_empty() && problems.is_empty() { problems.push("域里没有文献".into()) }
        problems.dedup();
        found.problem = problems.join("；");
        citations.push(found);
    }
}

impl docx::Field {
    fn range(&self) -> Range<usize> {
        self.start..self.end
    }
}

// ---- the file's text

/// Where the list of a file without a list field begins and ends: the paragraphs after the last heading that names one.
fn list_by_heading(paragraphs: &[docx::ReadPara]) -> Option<Range<usize>> {
    let heading = paragraphs.iter().rposition(|para| para.text.chars().count() <= 40 && crate::headings::opens_references(para.text.trim_end_matches([':', '：'])))?;
    let closes = |para: &docx::ReadPara| {
        let text = para.text.trim();
        let short = text.chars().count() <= 40 && !text.chars().any(|c| c.is_ascii_digit());
        para.level.is_some() || (short && crate::structure::canonical(text).is_some()) || ["Figure ", "Fig. ", "Table ", "Supplementary ", "图", "表", "附录", "附表", "附图"].iter().any(|open| text.starts_with(open) && text[open.len()..].trim_start().starts_with(|c: char| c.is_ascii_digit() || "一二三四五六七八九十".contains(c)))
    };
    let end = paragraphs[heading + 1..].iter().position(closes).map_or(paragraphs.len(), |n| heading + 1 + n);
    (end > heading + 1).then_some(heading + 1..end)
}

/// The numbers a citation names: `1`, `2, 4–6`, `1-3`. None for words that are not numbers alone.
fn cited_numbers(text: &str) -> Option<Vec<usize>> {
    let mut out: Vec<usize> = vec![];
    let mut range_from: Option<usize> = None;
    let mut digits = String::new();
    let close = |digits: &mut String, range_from: &mut Option<usize>, out: &mut Vec<usize>| -> Option<()> {
        if digits.is_empty() { return if range_from.is_some() { None } else { Some(()) } }
        let number: usize = digits.parse().ok().filter(|number| *number > 0 && *number < 10_000)?;
        digits.clear();
        match range_from.take() {
            Some(from) if number > from && number - from <= 200 => out.extend(from + 1..=number),
            Some(_) => return None,
            None => out.push(number),
        }
        Some(())
    };
    for c in text.chars() {
        match c {
            c if c.is_ascii_digit() => digits.push(c),
            '-' | '–' | '—' | '‐' | '~' | '～' => {
                if digits.is_empty() || range_from.is_some() { return None }
                close(&mut digits, &mut range_from, &mut out)?;
                range_from = out.last().copied();
            }
            ',' | '，' | '、' | ';' | '；' => close(&mut digits, &mut range_from, &mut out)?,
            c if c.is_whitespace() => {}
            _ => return None,
        }
    }
    close(&mut digits, &mut range_from, &mut out)?;
    (!out.is_empty()).then_some(out)
}

/// A unit a raised number is a power of, not a citation: m², cm³.
const UNITS: [&str; 16] = ["m", "cm", "mm", "km", "nm", "μm", "µm", "dm", "s", "g", "kg", "ha", "h", "L", "mL", "x"];

/// The numbered citations of a paragraph: in square brackets, or raised.
fn numbered(text: &str, raised: &[Range<usize>]) -> Vec<(Range<usize>, Vec<usize>, bool)> {
    let mut out: Vec<(Range<usize>, Vec<usize>, bool)> = vec![];
    let mut from = 0;
    while let Some(found) = text[from..].find(['[', '［']) {
        let at = from + found;
        let open = text[at..].chars().next().map_or(1, char::len_utf8);
        from = at + open;
        let Some(close) = text[from..].find([']', '］']) else { break };
        let Some(numbers) = cited_numbers(&text[from..from + close]) else { continue };
        let end = from + close + text[from + close..].chars().next().map_or(1, char::len_utf8);
        out.push((at..end, numbers, raised.iter().any(|raised| raised.start < end && at < raised.end)));
        from = end;
    }
    for range in raised {
        let Some(said) = text.get(range.clone()) else { continue };
        let lead = said.len() - said.trim_start().len();
        let range = range.start + lead..range.start + said.trim_end().len();
        if range.is_empty() || out.iter().any(|(known, ..)| known.start < range.end && range.start < known.end) { continue }
        let Some(numbers) = cited_numbers(&text[range.clone()]) else { continue };
        let before = &text[..range.start];
        let word: String = before.chars().rev().take_while(|c| c.is_alphabetic()).collect::<Vec<_>>().into_iter().rev().collect();
        // A power: of ten, of a unit, of a letter standing alone.
        if before.ends_with(|c: char| c.is_ascii_digit() || matches!(c, '+' | '-' | '−' | '^')) || UNITS.contains(&word.as_str()) || word.chars().count() == 1 { continue }
        out.push((range, numbers, true));
    }
    out.sort_by_key(|(range, ..)| range.start);
    out
}

/// A work a citation names by author and year.
#[derive(Clone, Debug, Default, PartialEq)]
struct NamedWork {
    family: String,
    second: String,
    year: i32,
    letter: String,
    locator: String,
}

const PARTICLES: [&str; 18] = ["de", "van", "von", "der", "den", "da", "di", "la", "le", "el", "al", "bin", "ten", "ter", "du", "dos", "del", "st"];

/// The first author (and the second, when two are named) a citation's words before its year say.
fn names_of(names: &str) -> Option<(String, String)> {
    let names = names.trim().trim_end_matches([',', '，', '(', '（', ' ']).trim();
    if names.is_empty() || names.chars().count() > 80 || names.chars().any(|c| c.is_ascii_digit()) { return None }
    let lower = names.to_lowercase();
    // 张三等, 张三和李四, 张三.
    if names.chars().next().is_some_and(is_cjk) {
        let head = names.trim_end_matches("等人").trim_end_matches('等').trim();
        let (first, second) = match head.split_once(['和', '与', '、', ',', '，']) {
            Some((first, second)) => (first.trim(), second.trim()),
            None => (head, ""),
        };
        let fits = |name: &str| (2..=4).contains(&name.chars().count()) && name.chars().all(is_cjk);
        return (fits(first) && (second.is_empty() || second.split(['、', ',', '，', '和', '与']).all(|name| fits(name.trim())))).then(|| (first.to_string(), second.split(['、', ',', '，']).next().unwrap_or("").trim().to_string()));
    }
    let cut = [" et al", " and ", " & ", ","].iter().filter_map(|mark| lower.find(mark)).min().unwrap_or(names.len());
    let family = names[..cut].trim();
    let words: Vec<&str> = family.split_whitespace().collect();
    let named = !words.is_empty() && words.len() <= 4 && words.iter().enumerate().all(|(n, word)| {
        let particle = PARTICLES.contains(&word.to_lowercase().trim_end_matches('.')) && n + 1 < words.len();
        particle || (word.chars().next().is_some_and(char::is_uppercase) && word.chars().all(|c| c.is_alphabetic() || matches!(c, '-' | '\'' | '’' | '.')))
    });
    if !named { return None }
    let rest = names[cut..].trim_start_matches([',', ' ']);
    let second = rest.strip_prefix("and ").or(rest.strip_prefix("& ")).map(|second| second.trim().trim_end_matches(['.', ',']).to_string()).filter(|second| !second.contains(' ') || second.split_whitespace().count() <= 3).unwrap_or_default();
    Some((family.to_string(), if lower[cut..].contains("et al") { String::new() } else { second }))
}

/// What stands before the names of a citation that is worded: longer ones first.
const PREFIXES: [&str; 14] = ["see also", "see e.g.", "see", "e.g.,", "e.g.", "cf.", "reviewed in", "reviewed by", "参见", "详见", "例如", "见", "如", "综述见"];

/// One part of a citation in brackets, between its semicolons: the works it names, or why it is none.
fn named_part(part: &str) -> Option<Result<Vec<NamedWork>, String>> {
    // Words before the names (`see`, `参见`): a citation still, which stays as its writer worded it.
    let said = part.trim_start();
    let lower = said.to_lowercase();
    if let Some(lead) = PREFIXES.iter().find(|lead| lower.starts_with(**lead) && !said[lead.len()..].starts_with(|c: char| c.is_ascii_alphabetic())) {
        let rest = said[lead.len()..].trim_start_matches([',', '，', ':', '：', ' ']);
        return named_part(rest).map(|_| Err(format!("带有前缀「{}」", said[..lead.len()].trim())));
    }
    let found = years(part);
    let (first, ..) = found.first()?;
    let (family, second) = names_of(&part[..*first])?;
    let mut works: Vec<NamedWork> = vec![];
    let mut at = 0;
    for (n, (start, year, letter, len)) in found.iter().enumerate() {
        // Between two years of one author: a comma and nothing else.
        if n > 0 && !part[at..*start].chars().all(|c| c.is_whitespace() || matches!(c, ',' | '，' | '、')) { return Some(Err(format!("读不懂「{}」", part.trim()))) }
        works.push(NamedWork { family: family.clone(), second: second.clone(), year: *year, letter: letter.clone(), locator: String::new() });
        at = start + len;
        // `2020a, b`: the second letter is the same year's other work.
        let mut rest = &part[at..];
        while let Some(next) = rest.trim_start_matches([',', '，', ' ']).chars().next().filter(|c| c.is_ascii_lowercase() && !letter.is_empty()) {
            let after = &rest.trim_start_matches([',', '，', ' '])[1..];
            if after.starts_with(|c: char| c.is_alphanumeric()) { break }
            works.push(NamedWork { letter: next.to_string(), ..works[works.len() - 1].clone() });
            at = part.len() - after.len();
            rest = after;
        }
    }
    let tail = part[at..].trim().trim_start_matches([',', '，', ':', '：']).trim();
    if !tail.is_empty() {
        // A page: `p. 12`, `pp. 12–15`, `12`.
        let page = tail.trim_start_matches("pp.").trim_start_matches("p.").trim_start_matches("pp").trim_start_matches('第').trim_end_matches('页').trim();
        if !page.is_empty() && page.chars().all(|c| c.is_ascii_digit() || matches!(c, '-' | '–' | ' ')) && works.len() == 1 { works[0].locator = page.to_string() } else { return Some(Err(format!("「{}」后面还有别的话", part[..at].trim()))) }
    }
    Some(Ok(works))
}

/// The entry a named work is: the one of that year whose first author is so named.
fn entry_named(work: &NamedWork, entries: &[Entry]) -> Result<usize, String> {
    let same = |a: &str, b: &str| !a.is_empty() && a.to_lowercase() == b.to_lowercase();
    let mut fits: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| match &entry.record {
            Some(record) => year_of(record) == Some(work.year) && family_of(record).iter().any(|family| same(family, &work.family)),
            // One that could not be read is known by how it opens.
            None => entry.text.contains(&work.year.to_string()) && label(&entry.text).map_or(entry.text.as_str(), |(_, at)| &entry.text[at..]).to_lowercase().starts_with(&work.family.to_lowercase()),
        })
        .map(|(n, _)| n)
        .collect();
    if fits.len() > 1 && !work.letter.is_empty() { fits.retain(|n| entries[*n].letter == work.letter) }
    if fits.len() > 1 && !work.second.is_empty() { fits.retain(|n| entries[*n].record.as_ref().is_some_and(|record| same(&second_family(record), &work.second))) }
    let said = format!("{}{}{}", work.family, if work.family.chars().any(is_cjk) { "，" } else { ", " }, work.year) + &work.letter;
    match fits.as_slice() {
        [one] => Ok(*one),
        [] => Err(format!("参考文献表里没有「{said}」")),
        _ => Err(format!("参考文献表里有不止一条「{said}」")),
    }
}

/// The author-year citations of a paragraph: `(Zhang et al., 2024a; Li & Wang, 2019)`, `Zhang et al. (2024)`, `张三等（2020）`.
fn author_year(text: &str, paragraph: usize, entries: &[Entry]) -> Vec<Found> {
    let mut out: Vec<Found> = vec![];
    let mut from = 0;
    while let Some(found) = text[from..].find(['(', '（']) {
        let at = from + found;
        let open = text[at..].chars().next().map_or(1, char::len_utf8);
        from = at + open;
        let Some(close) = text[from..].find([')', '）', '(', '（']) else { break };
        if text[from + close..].starts_with(['(', '（']) || close > 400 { continue }
        let inner = &text[from..from + close];
        let end = from + close + text[from + close..].chars().next().map_or(1, char::len_utf8);
        let listed = years(inner);
        if listed.is_empty() { continue }
        let mut cite = Found { paragraph, range: at..end, text: text[at..end].to_string(), kind: "author-year", ..Default::default() };
        let mut problems: Vec<String> = vec![];
        // Years alone: the authors stand before the bracket.
        let bare = inner.chars().all(|c| c.is_ascii_digit() || c.is_ascii_lowercase() || c.is_whitespace() || matches!(c, ',' | '，' | '、' | ';' | '；'));
        if bare {
            let before = text[..at].trim_end();
            let head = [" et al.", " et al", "等人", "等"].iter().find_map(|tail| before.strip_suffix(tail)).unwrap_or(before);
            let (head, second) = match [" and ", " & ", "和", "与"].iter().filter_map(|mark| head.rfind(mark).map(|found| (found, mark.len()))).max() {
                Some((found, len)) if head[found + len..].chars().count() <= 30 && !head[found + len..].contains([',', '.', ';', '，', '。']) => (head[..found].trim_end(), head[found + len..].trim()),
                _ => (head, ""),
            };
            let lower = head.to_lowercase();
            let mut any = false;
            for (_, year, letter, _) in &listed {
                // The entries of that year whose first author's name the sentence ends in.
                let fits: Vec<&Entry> = entries.iter().filter(|entry| entry.record.as_ref().is_some_and(|record| year_of(record) == Some(*year) && family_of(record).iter().any(|family| lower.ends_with(&family.to_lowercase()) && (family.chars().any(is_cjk) || !lower[..lower.len() - family.len()].ends_with(|c: char| c.is_alphabetic()))))).collect();
                let fits: Vec<&Entry> = if fits.len() > 1 && !letter.is_empty() { fits.into_iter().filter(|entry| entry.letter == *letter).collect() } else { fits };
                let fits: Vec<&Entry> = if fits.len() > 1 && !second.is_empty() { fits.into_iter().filter(|entry| entry.record.as_ref().is_some_and(|record| second_family(record).to_lowercase() == second.to_lowercase())).collect() } else { fits };
                match fits.as_slice() {
                    [entry] => {
                        any = true;
                        cite.cites.push(Cite { id: entry.id.clone(), locator: String::new(), narrative: true });
                    }
                    [] => {}
                    _ => problems.push(format!("参考文献表里有不止一条 {year} 年的同名文献")),
                }
            }
            // A year in brackets that no entry's author stands before is a year, not a citation.
            if !any && problems.is_empty() { continue }
            if cite.cites.len() < listed.len() && problems.is_empty() { problems.push("括号里有的年份对不上参考文献".into()) }
        } else {
            let parts: Vec<Option<Result<Vec<NamedWork>, String>>> = inner.split([';', '；']).filter(|part| !part.trim().is_empty()).map(named_part).collect();
            // Words in brackets that name no work at all: not a citation.
            if !parts.iter().any(Option::is_some) { continue }
            for part in parts {
                match part {
                    Some(Ok(works)) => {
                        for work in works {
                            match entry_named(&work, entries) {
                                Ok(n) if entries[n].record.is_some() => cite.cites.push(Cite { id: entries[n].id.clone(), locator: work.locator, narrative: false }),
                                Ok(n) => problems.push(format!("第 {} 条参考文献没能识别", n + 1)),
                                Err(problem) => problems.push(problem),
                            }
                        }
                    }
                    Some(Err(problem)) => problems.push(problem),
                    None => problems.push("括号里除了引用还有别的话".into()),
                }
            }
        }
        cite.problem = problems.join("；");
        out.push(cite);
        from = end;
    }
    out
}

// ---- reading a file

/// What a Word file cites. `papers` are the library's; `given` are records for entries, by their place in the list
/// counted from one, that whoever asks has read better than the rules here can; `fetch` asks Crossref for the
/// entries neither the library nor their own text settles.
pub fn inspect(bytes: &[u8], papers: &[Paper], given: &[(usize, Record)], fetch: Option<Fetch>) -> Result<Inspected> {
    let read = docx::read(bytes)?;
    let marks = docx::marks(bytes)?;
    let paragraphs = read.paragraphs;
    let mut out = Inspected { source: "text", language: language(&paragraphs).to_string(), ..Default::default() };
    let coded = |open: &str| marks.fields.iter().enumerate().filter(|(_, field)| field.code.trim_start().starts_with(open)).map(|(n, _)| n).collect::<Vec<_>>();
    // A list field's paragraphs, when it stands whole in the body.
    let list_of = |fields: &[usize]| fields.iter().map(|n| &marks.fields[*n]).find(|field| field.whole && field.from <= field.to && field.to < paragraphs.len()).map(|field| field.from..field.to + 1);

    from_fields(&marks, &paragraphs, &mut out.entries, &mut out.citations);
    if !out.citations.is_empty() {
        let mendeley = marks.fields.iter().any(|field| field.code.contains("Mendeley"));
        out.source = if mendeley { "mendeley" } else { "zotero" };
        out.list_fields = marks.fields.iter().enumerate().filter(|(_, field)| field.code.contains("ZOTERO_BIBL") || field.code.contains("CSL_BIBLIOGRAPHY")).map(|(n, _)| n).collect();
        out.list = list_of(&out.list_fields).or_else(|| list_by_heading(&paragraphs));
        if out.list.is_none() { out.notes.push("文件里没有参考文献表；新的参考文献表会加在文末。".into()) }
        out.notes.push(format!("引用是 {} 写的域，文献信息随文件保存，不需要猜。重排之后这些引用是普通的跳转域，不再由 {0} 管理：之后要增删引用，请在原文件里改好再重排一次。", if mendeley { "Mendeley" } else { "Zotero" }));
        out.paragraphs = paragraphs;
        return Ok(out);
    }

    let (endnote, endnote_list) = (coded("ADDIN EN.CITE"), coded("ADDIN EN.REFLIST"));
    let (word, word_list) = (coded("CITATION "), coded("BIBLIOGRAPHY"));
    if !endnote.is_empty() || !endnote_list.is_empty() {
        out.source = "endnote";
        out.notes.push("引用是 EndNote 的域。域里的文献信息这里读不了，按正文和参考文献表的文字来识别；被重排的引用会变成普通的跳转域，不再由 EndNote 管理。".into());
    } else if !word.is_empty() || !word_list.is_empty() {
        out.source = "word";
        out.notes.push("引用是 Word 自带的「引文与书目」域。它的文献信息这里读不了，按正文和参考文献表的文字来识别。".into());
    }
    out.list_fields = if endnote_list.is_empty() { word_list } else { endnote_list };
    out.list = list_of(&out.list_fields).or_else(|| {
        out.list_fields.clear();
        list_by_heading(&paragraphs)
    });
    let Some(list) = out.list.clone() else {
        bail!("没有在这个文件里找到参考文献表：既没有 Zotero 的引用域，也没有「参考文献」「References」这样的标题和它后面的条目。{}", if out.source == "word" { "Word 自带书目放在内容控件里，这里读不到；请先在 Word 里把它「转换为静态文本」。" } else { "" });
    };
    let labelled = list.clone().filter(|n| label(&paragraphs[*n].text).is_some()).count();
    let numbered_list = labelled * 5 >= list.len() * 3;
    for (place, n) in list.clone().enumerate() {
        let text = paragraphs[n].text.clone();
        let number = if numbered_list { label(&text).map_or(place + 1, |(number, _)| number) } else { place + 1 };
        let mut entry = Entry { id: format!("r{}", place + 1), paragraph: Some(n), number, text, ..Default::default() };
        resolve(&mut entry, papers, given.iter().find(|(at, _)| *at == place + 1).map(|(_, record)| record), fetch);
        out.entries.push(entry);
    }

    // The citations of the body, by number and by author and year: the file cites the way more of them are found.
    let field_of = |paragraph: usize, range: &Range<usize>| -> Vec<usize> { marks.fields.iter().enumerate().filter(|(_, field)| field.whole && field.from == paragraph && field.to == paragraph && field.start < range.end && range.start < field.end).map(|(n, _)| n).collect() };
    let (mut by_number, mut by_name): (Vec<Found>, Vec<Found>) = (vec![], vec![]);
    for (n, para) in paragraphs.iter().enumerate() {
        if list.contains(&n) || n + 1 == list.start { continue }
        for (range, numbers, raised) in numbered(&para.text, &marks.raised[n]) {
            let mut cite = Found { paragraph: n, text: para.text[range.clone()].to_string(), range, kind: "numbered", raised, ..Default::default() };
            if !cite.text.starts_with(['[', '［']) { cite.kind = "raised" }
            let mut problems: Vec<String> = vec![];
            for number in numbers {
                match out.entries.iter().find(|entry| entry.number == number) {
                    Some(entry) if entry.record.is_some() => cite.cites.push(Cite { id: entry.id.clone(), ..Default::default() }),
                    Some(_) => problems.push(format!("第 {number} 条参考文献没能识别")),
                    None => problems.push(format!("参考文献表里没有第 {number} 条")),
                }
            }
            cite.problem = problems.join("；");
            by_number.push(cite);
        }
        by_name.extend(author_year(&para.text, n, &out.entries));
    }
    let hits = |found: &[Found]| found.iter().filter(|cite| !cite.cites.is_empty()).count();
    // A raised number that names no entry is a power or a footnote, not a citation to report.
    by_number.retain(|cite| cite.kind != "raised" || !cite.cites.is_empty());
    out.citations = if hits(&by_number) >= hits(&by_name) && hits(&by_number) > 0 { by_number } else { by_name };
    for cite in &mut out.citations { cite.fields = field_of(cite.paragraph, &cite.range) }
    out.citations.sort_by_key(|cite| (cite.paragraph, cite.range.start));
    if out.citations.is_empty() { out.notes.push("正文里没有找到能和参考文献表对上的引用（方括号编号、上标编号或「作者, 年份」）。".into()) }
    out.paragraphs = paragraphs;
    Ok(out)
}

impl Inspected {
    /// How often each entry is cited by a citation that will be written anew.
    fn cited(&self, id: &str) -> usize {
        self.citations.iter().filter(|cite| cite.problem.is_empty()).flat_map(|cite| &cite.cites).filter(|cite| cite.id == id).count()
    }

    /// The report a tool hands back: where the citations are from, every entry with how it was resolved, the citations that stay as written.
    pub fn report(&self) -> Value {
        let short = |text: &str, most: usize| if text.chars().count() > most { format!("{}…", text.chars().take(most).collect::<String>()) } else { text.to_string() };
        let entries: Vec<Value> = self
            .entries
            .iter()
            .enumerate()
            .map(|(n, entry)| {
                let mut out = json!({"n": n + 1, "resolved": entry.origin, "cited": self.cited(&entry.id)});
                if !entry.text.is_empty() { out["text"] = json!(short(&entry.text, 240)) }
                if !entry.sure.is_empty() { out["sure"] = json!(entry.sure) }
                if !entry.paper.is_empty() { out["paperId"] = json!(entry.paper) }
                if !entry.note.is_empty() { out["note"] = json!(entry.note) }
                if let Some(record) = &entry.record {
                    let authors: Vec<String> = record.get("author").and_then(Value::as_array).into_iter().flatten().map(|author| author["family"].as_str().or(author["literal"].as_str()).unwrap_or("").to_string()).collect();
                    out["read"] = json!({"authors": if authors.len() > 3 { format!("{} 等 {} 人", authors[0], authors.len()) } else { authors.join(", ") }, "year": year_of(record), "title": short(record.get("title").and_then(Value::as_str).unwrap_or(""), 160), "in": record.get("container-title").and_then(Value::as_str).unwrap_or(""), "volume": record.get("volume").map(text_of).unwrap_or_default(), "page": record.get("page").map(text_of).unwrap_or_default()});
                }
                out
            })
            .collect();
        let kept: Vec<Value> = self.citations.iter().filter(|cite| !cite.problem.is_empty()).take(60).map(|cite| json!({"paragraph": cite.paragraph + 1, "text": short(&cite.text, 120), "why": cite.problem})).collect();
        let sample: Vec<Value> = self.citations.iter().filter(|cite| cite.problem.is_empty()).take(8).map(|cite| json!({"paragraph": cite.paragraph + 1, "text": short(&cite.text, 120), "entries": cite.cites.iter().filter_map(|cited| self.entries.iter().position(|entry| entry.id == cited.id)).map(|n| n + 1).collect::<Vec<_>>()})).collect();
        let unresolved: Vec<usize> = self.entries.iter().enumerate().filter(|(_, entry)| entry.record.is_none()).map(|(n, _)| n + 1).collect();
        let uncited: Vec<usize> = self.entries.iter().enumerate().filter(|(_, entry)| entry.record.is_some() && self.cited(&entry.id) == 0).map(|(n, _)| n + 1).collect();
        json!({
            "source": self.source,
            "language": self.language,
            "citations": {"found": self.citations.len(), "rewritten": self.citations.iter().filter(|cite| cite.problem.is_empty()).count(), "keptAsWritten": kept, "sample": sample},
            "entries": entries,
            "unresolved": unresolved,
            "uncited": uncited,
            "notes": self.notes,
        })
    }
}

// ---- the works it cites, as papers

/// An entry of the file's list as a paper to take into a library, and how it came to be one.
#[derive(Clone, Debug, PartialEq)]
pub struct Work {
    /// Its place in the list, from one.
    pub n: usize,
    /// As the file's list says it; empty for a work known from a field alone.
    pub text: String,
    /// None for an entry that could not be read.
    pub paper: Option<Paper>,
    pub origin: &'static str,
    pub sure: &'static str,
    pub note: String,
    /// How often the file's text cites it.
    pub cited: usize,
}

/// The works a file cites, as papers (`csl::paper_of`), in the order of its list. With `fetch`, an entry read from
/// its own text that names no DOI is searched on Crossref by its words, and the record found (one whose title the
/// entry says, of the same year) gives it its DOI and what else the text left out; a library is to find the work
/// again, which a DOI does best. `only` keeps the entries of those places, for records given anew.
pub fn works(inspected: &Inspected, fetch: Option<Fetch>, only: &[usize]) -> Vec<Work> {
    let mut out = vec![];
    for (place, entry) in inspected.entries.iter().enumerate() {
        let n = place + 1;
        if !only.is_empty() && !only.contains(&n) { continue }
        let mut work = Work { n, text: entry.text.clone(), paper: entry.record.as_ref().and_then(csl::paper_of), origin: entry.origin, sure: entry.sure, note: entry.note.clone(), cited: inspected.cited(&entry.id) };
        let unnamed = entry.origin == TEXT && !entry.text.is_empty() && work.paper.as_ref().is_some_and(|paper| paper.doi.is_empty());
        if let (Some(fetch), Some(paper), true) = (fetch, work.paper.as_mut(), unnamed) {
            match searched(&entry.text, fetch) {
                Ok(Some(record)) => match csl::paper_of(&record).filter(|found| found.year.is_none() || paper.year.is_none() || found.year == paper.year) {
                    Some(found) => {
                        // Crossref's record is the work's own; what it lacks the entry's text still says.
                        let text = std::mem::replace(paper, found);
                        for (field, said) in [(&mut paper.authors, text.authors), (&mut paper.journal, text.journal), (&mut paper.volume, text.volume), (&mut paper.issue, text.issue), (&mut paper.pages, text.pages)] {
                            if field.is_empty() { *field = said }
                        }
                        if paper.year.is_none() { paper.year = text.year }
                        (work.origin, work.sure) = (LOOKUP, LIKELY);
                    }
                    None => work.note = "Crossref 上题目相符的记录年份不同，未采用".into(),
                },
                Ok(None) => {}
                Err(error) => work.note = format!("查询 Crossref 失败：{error}"),
            }
        }
        out.push(work);
    }
    out
}

// ---- writing it again

#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Write the changes as Word's revisions.
    pub track: bool,
    pub author: String,
    pub date: String,
    /// `zh` or `en` for the style's terms; empty for the file's own language.
    pub language: String,
}

/// A file cited anew.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Restyled {
    pub bytes: Vec<u8>,
    /// Citations written anew, and left as written.
    pub rewritten: usize,
    pub kept: usize,
    /// Entries of the new list, and those of the old one left as written after it.
    pub entries: usize,
    pub unresolved: usize,
    /// The first citations and entries as they now read.
    pub sample: Vec<(String, String)>,
    pub list_sample: Vec<String>,
}

fn all_raised(rich: &Rich) -> bool {
    !rich.text.is_empty() && rich.looks.iter().filter(|(_, look)| look.sup).map(|(range, _)| range.len()).sum::<usize>() >= rich.text.trim().len()
}

/// The file with its citations and its list in `style`.
pub fn restyle(bytes: &[u8], inspected: &Inspected, style: &csl::Loaded, options: &Options) -> Result<Restyled> {
    let language = if options.language.trim().is_empty() { inspected.language.as_str() } else { options.language.trim() };
    let records: Vec<Record> = inspected.entries.iter().filter_map(|entry| entry.record.clone()).collect();
    let active: Vec<&Found> = inspected.citations.iter().filter(|cite| cite.problem.is_empty() && !cite.cites.is_empty()).collect();
    if records.is_empty() { bail!("这个文件的参考文献一条也没能识别，没有可以重排的内容。") }
    let groups: Vec<Vec<Cite>> = active.iter().map(|cite| cite.cites.clone()).collect();
    let uncited: Vec<String> = inspected.entries.iter().filter(|entry| entry.record.is_some() && !groups.iter().flatten().any(|cite| cite.id == entry.id)).map(|entry| entry.id.clone()).collect();
    let (cited, list) = csl::render_records(style, &groups, &uncited, &records, language);
    let mut done = Restyled::default();

    let role = |para: &docx::ReadPara| if para.title { Role::Title } else if para.level.is_some() { Role::Heading } else { Role::Body };
    // The old list goes only when the style writes a new one.
    let old_list = inspected.list.clone().filter(|_| !list.is_empty());
    let new_list = |done: &mut Restyled| -> Vec<Wanted> {
        let mut out: Vec<Wanted> = list.iter().map(|(id, rich)| Wanted { text: rich.text.clone(), anchor: Some(id.clone()), looks: rich.looks.clone(), fresh: true, ..Default::default() }).collect();
        done.entries = out.len();
        done.list_sample = list.iter().take(3).map(|(_, rich)| rich.text.clone()).collect();
        // What could not be read stays, after the others, as it was written.
        for entry in inspected.entries.iter().filter(|entry| entry.record.is_none() && !entry.text.is_empty()) {
            out.push(Wanted { text: entry.text.clone(), ..Default::default() });
            done.unresolved += 1;
        }
        out
    };
    let mut wanted: Vec<Wanted> = vec![];
    let mut fields: Vec<usize> = vec![];
    for (n, para) in inspected.paragraphs.iter().enumerate() {
        if let Some(old) = &old_list && old.contains(&n) {
            if n == old.start { wanted.extend(new_list(&mut done)) }
            continue;
        }
        let mut out = Wanted { text: String::new(), role: role(para), ..Default::default() };
        let mut at = 0;
        for (cite, rich) in active.iter().zip(&cited).filter(|(cite, _)| cite.paragraph == n) {
            // One the style wrote nothing for, or that stands where another was just written, stays.
            if rich.text.is_empty() || cite.range.start < at || cite.range.end > para.text.len() { continue }
            let mut rich = rich.clone();
            let raised = all_raised(&rich);
            // The year alone keeps the brackets it stood in: `Zhang et al. (2024)`.
            if cite.cites.iter().all(|cited| cited.narrative) && !rich.text.starts_with(['(', '[', '（', '［']) && !raised && let (Some(open), Some(close)) = (cite.text.chars().next().filter(|c| matches!(c, '(' | '（')), cite.text.chars().last().filter(|c| matches!(c, ')' | '）'))) {
                rich = shifted(&rich, open.len_utf8(), format!("{open}{}{close}", rich.text));
            }
            let mut start = cite.range.start;
            let before = &para.text[at..start];
            if raised && !cite.raised {
                // A raised number stands against the word before it.
                start -= before.len() - before.trim_end().len();
            } else if !raised && cite.raised && para.text[..start].ends_with(|c: char| c.is_ascii() && !c.is_whitespace() && !matches!(c, '(' | '[')) {
                rich = shifted(&rich, 1, format!(" {}", rich.text));
            }
            out.text.push_str(&para.text[at..start]);
            let base = out.text.len();
            out.text.push_str(&rich.text);
            out.edits.push((start..cite.range.end, rich.text.clone()));
            out.looks.extend(rich.looks.iter().map(|(range, look)| (base + range.start..base + range.end, *look)));
            out.links.extend(rich.links.iter().map(|(range, to)| Link { range: base + range.start..base + range.end, to: to.clone() }));
            if cite.raised && !raised { out.level.push(base..out.text.len()) }
            fields.extend(&cite.fields);
            if done.sample.len() < 5 { done.sample.push((cite.text.clone(), rich.text.trim().to_string())) }
            done.rewritten += 1;
            at = cite.range.end;
        }
        out.text.push_str(&para.text[at..]);
        wanted.push(out);
    }
    if inspected.list.is_none() && !list.is_empty() {
        wanted.push(Wanted { text: if language == "zh" { "参考文献" } else { "References" }.into(), role: Role::Heading, ..Default::default() });
        wanted.extend(new_list(&mut done));
    }
    if old_list.is_some() { fields.extend(&inspected.list_fields) }
    done.kept = inspected.citations.len() - done.rewritten;
    let plain = docx::unfielded(bytes, &fields)?;
    done.bytes = docx::rewrite(&plain, &wanted, &docx::Options { track: options.track, author: options.author.clone(), date: options.date.clone() })?.bytes;
    Ok(done)
}

/// A text with `lead` bytes put before its words, its ranges moved along.
fn shifted(rich: &Rich, lead: usize, text: String) -> Rich {
    Rich { text, looks: rich.looks.iter().map(|(range, look)| (range.start + lead..range.end + lead, *look)).collect(), links: rich.links.iter().map(|(range, to)| (range.start + lead..range.end + lead, to.clone())).collect() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docx::tests::{body_of, field, file};

    fn p(text: &str) -> String {
        format!("<w:p><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>", text.replace('&', "&amp;").replace('<', "&lt;"))
    }

    fn texts(bytes: &[u8]) -> Vec<String> {
        docx::read(bytes).unwrap().paragraphs.into_iter().map(|para| para.text).collect()
    }

    fn style(name: &str) -> csl::Loaded {
        csl::load(&format!("csl:{name}"), &std::env::temp_dir()).unwrap()
    }

    fn said(parsed: &Parsed) -> (Vec<String>, Option<i32>, &str, &str, &str, &str, &str) {
        (parsed.authors.iter().map(|(family, given)| format!("{family}|{given}")).collect(), parsed.year, parsed.title.as_str(), parsed.container.as_str(), parsed.volume.as_str(), parsed.issue.as_str(), parsed.pages.as_str())
    }

    #[test]
    fn a_reference_is_read_by_rule_in_the_three_families() {
        let apa = parse_entry("Zhang, W., Li, Q., & Wang, F. (2024a). Single-cell atlas of roots. Nature Plants, 10(3), 45–67. https://doi.org/10.1038/s41477-024-0001-2");
        assert_eq!(said(&apa), (vec!["Zhang|W.".to_string(), "Li|Q.".into(), "Wang|F.".into()], Some(2024), "Single-cell atlas of roots", "Nature Plants", "10", "3", "45–67"));
        assert_eq!((apa.letter.as_str(), apa.doi.as_str()), ("a", "10.1038/s41477-024-0001-2"));
        let vancouver = parse_entry("[2] Smith JA, van der Berg AB, Li Q. Roots and shoots: a review. Plant Cell. 2019;31(2):123-134. PMID: 30123456");
        assert_eq!(said(&vancouver), (vec!["Smith|J. A.".to_string(), "van der Berg|A. B.".into(), "Li|Q".into()], Some(2019), "Roots and shoots: a review", "Plant Cell", "31", "2", "123–134"));
        assert_eq!(vancouver.pmid, "30123456");
        let nature = parse_entry("3. Zhang, W., Li, Q. & Wang, F. Single-cell atlas of roots. Nat. Plants 10, 45–67 (2024).");
        assert_eq!(said(&nature), (vec!["Zhang|W.".to_string(), "Li|Q.".into(), "Wang|F".into()], Some(2024), "Single-cell atlas of roots", "Nat. Plants", "10", "", "45–67"));
        let gb = parse_entry("[4] 张三, 欧阳修文, 李四, 等. 水稻根系发育的单细胞图谱[J]. 植物学报, 2020, 55(3): 12-20.");
        assert_eq!(said(&gb), (vec!["张|三".to_string(), "欧阳|修文".into(), "李|四".into()], Some(2020), "水稻根系发育的单细胞图谱", "植物学报", "55", "3", "12–20"));
        let gb_english = parse_entry("ZHANG W, LI Q. Root hairs take up water[J]. Nature, 2021, 590: e1234.");
        assert_eq!(said(&gb_english), (vec!["Zhang|W".to_string(), "Li|Q".into()], Some(2021), "Root hairs take up water", "Nature", "590", "", "e1234"));
        let book = parse_entry("李四. 植物生理学[M]. 北京: 科学出版社, 2018.");
        assert_eq!((book.kind, book.publisher.as_str(), book.year, book.title.as_str()), ("book", "科学出版社", Some(2018), "植物生理学"));
        // Words that are no reference are not made one.
        assert!(!parse_entry("See the supplementary material for more.").complete());
        assert_eq!(label("[12] Zhang"), Some((12, 5)));
        assert_eq!(label("12. Zhang"), Some((12, 4)));
        assert_eq!(label("（3）张三"), Some((3, 7)));
        assert_eq!(label("2020. A year"), None);
        assert_eq!(label("Zhang 2020"), None);
        assert_eq!(cited_numbers("2, 4–6"), Some(vec![2, 4, 5, 6]));
        assert_eq!(cited_numbers("1-3,7"), Some(vec![1, 2, 3, 7]));
        assert_eq!(cited_numbers("a1"), None);
        assert_eq!(cited_numbers("3-1"), None);
    }

    const ZOTERO_A: &str = r#" ADDIN ZOTERO_ITEM CSL_CITATION {"citationID":"a1","properties":{"formattedCitation":"(Zhang et al., 2024)","plainCitation":"(Zhang et al., 2024)","noteIndex":0},"citationItems":[{"id":12,"uris":["http://zotero.org/users/1/items/AAAA"],"itemData":{"id":12,"type":"article-journal","title":"Single-cell atlas of roots","container-title":"Nature Plants","volume":"10","issue":"3","page":"45-67","DOI":"10.1038/x","author":[{"family":"Zhang","given":"Wei"},{"family":"Li","given":"Qiang"},{"family":"Wang","given":"Fang"}],"issued":{"date-parts":[["2024",3,1]]},"accessed":{"date-parts":[["2025"]]}}}],"schema":"https://github.com/citation-style-language/schema/raw/master/csl-citation.json"} "#;
    const ZOTERO_B: &str = r#" ADDIN ZOTERO_ITEM CSL_CITATION {"citationID":"b2","properties":{"noteIndex":0},"citationItems":[{"id":15,"uris":["http://zotero.org/users/1/items/BBBB"],"itemData":{"id":15,"type":"article-journal","title":"Roots & shoots","container-title":"Cell","volume":"1","page":"1-9","author":[{"family":"Smith","given":"John"}],"issued":{"date-parts":[[2019]]}},"locator":"5","label":"page"},{"id":12,"uris":["http://zotero.org/users/1/items/AAAA"],"itemData":{"id":12,"type":"article-journal","title":"Single-cell atlas of roots","author":[{"family":"Zhang","given":"Wei"}],"issued":{"date-parts":[["2024"]]}}}]} "#;
    const ZOTERO_C: &str = r#" ADDIN ZOTERO_ITEM CSL_CITATION {"citationID":"c3","properties":{"noteIndex":0},"citationItems":[{"id":15,"uris":["http://zotero.org/users/1/items/BBBB"],"itemData":{"id":15,"type":"article-journal","title":"Roots & shoots","author":[{"family":"Smith","given":"John"}],"issued":{"date-parts":[[2019]]}},"prefix":"see"}]} "#;

    fn zotero_file() -> Vec<u8> {
        let run = |text: &str| format!("<w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r>");
        let body = format!(
            "{}<w:p>{}{}{}{}{}{}{}</w:p>{}<w:p><w:pPr><w:pStyle w:val=\"Bib\"/></w:pPr>{}{}</w:p><w:p><w:pPr><w:pStyle w:val=\"Bib\"/></w:pPr>{}<w:r><w:fldChar w:fldCharType=\"end\"/></w:r></w:p>{}",
            p("Roots"),
            run("Roots grow down "),
            field(ZOTERO_A, &run("(Zhang et al., 2024)")),
            run(" and up "),
            field(ZOTERO_B, &run("(Smith, 2019, p. 5; Zhang et al., 2024)")),
            run("; "),
            field(ZOTERO_C, &run("(see Smith, 2019)")),
            run("."),
            p("References"),
            "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText> ADDIN ZOTERO_BIBL {\"uncited\":[],\"omitted\":[],\"custom\":[]} CSL_BIBLIOGRAPHY </w:instrText></w:r><w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>",
            run("Smith, J. (2019). Roots &amp; shoots. <w:rPr><w:i/></w:rPr>Cell, 1, 1–9."),
            run("Zhang, W., Li, Q., &amp; Wang, F. (2024). Single-cell atlas of roots. Nature Plants, 10(3), 45–67."),
            p("Thanks to all."),
        );
        file(&body, "")
    }

    #[test]
    fn a_file_of_zoteros_is_cited_anew_from_what_its_fields_carry() {
        let source = zotero_file();
        let found = inspect(&source, &[], &[], None).unwrap();
        assert_eq!((found.source, found.list.clone(), found.entries.len(), found.citations.len()), ("zotero", Some(3..5), 2, 3));
        assert!(found.entries.iter().all(|entry| entry.origin == FIELD && entry.sure == SURE));
        assert_eq!(found.citations.iter().map(|cite| (cite.text.as_str(), cite.cites.len(), cite.problem.as_str())).collect::<Vec<_>>(), [("(Zhang et al., 2024)", 1, ""), ("(Smith, 2019, p. 5; Zhang et al., 2024)", 2, ""), ("(see Smith, 2019)", 1, "带有前缀或后缀")]);
        let report = found.report();
        assert_eq!((report["citations"]["rewritten"].as_u64(), report["citations"]["keptAsWritten"][0]["why"].as_str(), report["entries"][0]["read"]["page"].as_str()), (Some(2), Some("带有前缀或后缀"), Some("45-67")));

        let done = restyle(&source, &found, &style("nature"), &Options::default()).unwrap();
        assert_eq!((done.rewritten, done.kept, done.entries, done.unresolved), (2, 1, 2, 0));
        let said = texts(&done.bytes);
        // Raised numbers stand against the word before them; the one with a prefix is as it was.
        assert_eq!(said[1], "Roots grow down1 and up1,2; (see Smith, 2019).");
        assert_eq!(said[2], "References");
        assert!(said[3].starts_with("1. Zhang, W., Li, Q. & Wang, F. Single-cell atlas of roots. Nature Plants 10, 45–67 (2024)"), "{}", said[3]);
        assert!(said[4].starts_with("2. Smith, J. Roots & shoots. Cell 1, 1–9 (2019)"), "{}", said[4]);
        assert_eq!(said[5], "Thanks to all.");
        // The fields written anew are Zotero's no more, each is a link to its entry; the one left is whole.
        let body = body_of(&done.bytes);
        assert_eq!((body.matches("ZOTERO_ITEM").count(), body.matches("ZOTERO_BIBL").count(), body.matches("HYPERLINK \\l \"_WPREF_").count()), (1, 0, 3));
        assert!(body.contains("&quot;prefix&quot;:&quot;see&quot;") && body.contains("<w:bookmarkStart w:id=\"1\" w:name=\"_WPREF_1\"/>"));
        assert!(body.contains("<w:pStyle w:val=\"Bib\"/>") && body.contains("<w:rPr><w:vertAlign w:val=\"superscript\"/></w:rPr>"));
        assert!(body.ends_with(&p("Thanks to all.")));
        // Nothing of the old entry's italics stays on the new one's words.
        let entry = &body[body.find("_WPREF_1\"/>").unwrap()..];
        assert!(entry[..entry.find("</w:p>").unwrap()].contains("<w:rPr><w:i/></w:rPr><w:t xml:space=\"preserve\">Nature Plants</w:t>"));

        // Another author-date style: the page stays, and the changes can be tracked.
        let tracked = restyle(&source, &found, &style("chicago-author-date"), &Options { track: true, author: "Wisp".into(), date: "2026-11-19T00:00:00Z".into(), ..Default::default() }).unwrap();
        let said = texts(&tracked.bytes);
        assert!(said[1].contains("(Smith 2019, 5; Zhang et al. 2024)"), "{}", said[1]);
        assert!(body_of(&tracked.bytes).contains("<w:del ") && body_of(&tracked.bytes).contains("<w:ins "));
    }

    /// Each citation with the entries it names, and why it stays when it does.
    fn named(found: &Inspected) -> Vec<String> {
        found.citations.iter().map(|cite| format!("{} = {} {}", cite.text, cite.cites.iter().map(|cited| cited.id.as_str()).collect::<Vec<_>>().join(","), cite.problem).trim().to_string()).collect()
    }

    fn sup(text: &str) -> String {
        format!("<w:r><w:rPr><w:vertAlign w:val=\"superscript\"/></w:rPr><w:t>{text}</w:t></w:r>")
    }

    #[test]
    fn a_numbered_file_is_read_as_text_and_what_cannot_be_read_stays() {
        let run = |text: &str| format!("<w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r>");
        let body = format!(
            "{}{}<w:p>{}{}{}{}{}</w:p>{}{}{}{}{}",
            p("Roots grow down [1] and take up water [2, 3]. Others disagree [4], and one more [7]."),
            p("An area of 5 m2 [1–3] was sampled."),
            run("Raised"),
            sup("1,2"),
            run(" and a power 10"),
            sup("3"),
            run("."),
            p("References:"),
            p("[1] Zhang, W., Li, Q. & Wang, F. Single-cell atlas of roots. Nat. Plants 10, 45–67 (2024)."),
            p("[2] Smith JA, Li Q. Roots and shoots: a review. Plant Cell. 2019;31(2):123-134."),
            p("[3] 张三, 李四. 水稻根系发育的单细胞图谱[J]. 植物学报, 2020, 55(3): 12-20."),
            p("[4] Personal communication with a colleague."),
        );
        let source = file(&body, "");
        let held = Paper { id: "p9".into(), title: "Roots and shoots: a review".into(), authors: "Smith, Jane A.; Li, Qiang".into(), year: Some(2019), journal: "The Plant Cell".into(), ..Default::default() };
        let found = inspect(&source, &[held], &[], None).unwrap();
        assert_eq!((found.source, found.list.clone()), ("text", Some(4..8)));
        assert_eq!(found.entries.iter().map(|entry| (entry.number, entry.origin, entry.sure, entry.paper.as_str())).collect::<Vec<_>>(), [(1, TEXT, LIKELY, ""), (2, LIBRARY, SURE, "p9"), (3, TEXT, LIKELY, ""), (4, UNRESOLVED, "", "")]);
        assert_eq!(found.citations.iter().map(|cite| (cite.text.as_str(), cite.kind, cite.cites.len(), cite.problem.as_str())).collect::<Vec<_>>(), [("[1]", "numbered", 1, ""), ("[2, 3]", "numbered", 2, ""), ("[4]", "numbered", 0, "第 4 条参考文献没能识别"), ("[7]", "numbered", 0, "参考文献表里没有第 7 条"), ("[1–3]", "numbered", 3, ""), ("1,2", "raised", 2, "")]);
        assert_eq!(found.report()["unresolved"], json!([4]));

        let done = restyle(&source, &found, &style("apa"), &Options::default()).unwrap();
        assert_eq!((done.rewritten, done.kept, done.entries, done.unresolved), (4, 2, 3, 1));
        let said = texts(&done.bytes);
        assert_eq!(said[0], "Roots grow down (Zhang et al., 2024) and take up water (Smith & Li, 2019; 张 & 李, 2020). Others disagree [4], and one more [7].");
        assert_eq!(said[1], "An area of 5 m2 (Smith & Li, 2019; Zhang et al., 2024; 张 & 李, 2020) was sampled.");
        // What was raised is on the line now, a space before it; the power is a power still.
        assert_eq!(said[2], "Raised (Smith & Li, 2019; Zhang et al., 2024) and a power 103.");
        let body = body_of(&done.bytes);
        assert_eq!(body.matches("superscript").count(), 1);
        assert_eq!(said[3], "References:");
        // The library's paper gives its own record, the entry's text the volume and pages it lacks.
        assert_eq!(said[4], "Smith, J. A., & Li, Q. (2019). Roots and shoots: a review. The Plant Cell, 31(2), 123–134.");
        assert!(said[5].starts_with("Zhang, W., Li, Q., & Wang, F. (2024). Single-cell atlas of roots. Nat. Plants, 10, 45–67."), "{}", said[5]);
        assert_eq!(said[7], "[4] Personal communication with a colleague.");
        assert_eq!(said.len(), 8);

        // Whoever asks may read the entry the rules could not; then its citation is written too.
        let given: Record = serde_json::from_str(r#"{"type": "personal_communication", "title": "Personal communication", "author": [{"family": "Colleague", "given": "A."}], "issued": {"date-parts": [[2023]]}}"#).unwrap();
        let found = inspect(&source, &[], &[(4, given)], None).unwrap();
        assert_eq!((found.entries[3].origin, found.entries[1].origin), (GIVEN, TEXT));
        let done = restyle(&source, &found, &style("apa"), &Options::default()).unwrap();
        assert!(texts(&done.bytes)[0].contains("Others disagree (A. Colleague, personal communication, 2023), and one more [7]."), "{}", texts(&done.bytes)[0]);
    }

    #[test]
    fn an_author_year_file_is_numbered_in_english_and_in_chinese() {
        let body = format!(
            "{}{}{}{}{}{}{}",
            p("Roots grow down (Zhang et al., 2024a; Smith & Li, 2019) as Zhang et al. (2024b) and Smith and Li (2019) showed (see Fig. 2). In 2020 nothing happened (n = 2020)."),
            p("Some doubt it (Jones, 1999; see also Smith & Li, 2019, p. 5) and (Smith & Li, 2019, pp. 5–7)."),
            p("References"),
            p("Smith, J. A., & Li, Q. (2019). Roots and shoots: A review. The Plant Cell, 31(2), 123–134."),
            p("Zhang, W., Li, Q., & Wang, F. (2024a). Single-cell atlas of roots. Nature Plants, 10(3), 45–67."),
            p("Zhang, W., Li, Q., & Wang, F. (2024b). Another atlas of roots. Cell, 12(1), 1–9."),
            p("Wang, F. (2001). Never cited. Science, 1, 2."),
        );
        let source = file(&body, "");
        let found = inspect(&source, &[], &[], None).unwrap();
        assert_eq!(named(&found), [
            "(Zhang et al., 2024a; Smith & Li, 2019) = r2,r1",
            "(2024b) = r3",
            "(2019) = r1",
            "(Jones, 1999; see also Smith & Li, 2019, p. 5) =  参考文献表里没有「Jones, 1999」；带有前缀「see also」",
            "(Smith & Li, 2019, pp. 5–7) = r1",
        ]);
        assert_eq!(found.report()["uncited"], json!([4]));
        let done = restyle(&source, &found, &style("ieee"), &Options::default()).unwrap();
        let said = texts(&done.bytes);
        // A numbered style has the whole citation where the year stood; a work nothing cites keeps its place in the list, after the others.
        assert_eq!(said[0], "Roots grow down [1], [2] as Zhang et al. [3] and Smith and Li [2] showed (see Fig. 2). In 2020 nothing happened (n = 2020).");
        assert_eq!(said[1], "Some doubt it (Jones, 1999; see also Smith & Li, 2019, p. 5) and [2, pp. 5–7].");
        assert!(said[3].starts_with("[1] W. Zhang, Q. Li, and F. Wang, “Single-cell atlas of roots,”"), "{}", said[3]);
        assert!(said[6].starts_with("[4] F. Wang, “Never cited,”"), "{}", said[6]);
        // An author-date style keeps the authors in the sentence and the year in its brackets.
        let done = restyle(&source, &found, &style("apa"), &Options::default()).unwrap();
        assert!(texts(&done.bytes)[0].contains("as Zhang et al. (2024b) and Smith and Li (2019) showed"), "{}", texts(&done.bytes)[0]);

        let body = format!(
            "{}{}{}{}{}",
            p("根系向下生长（张三等，2020；李四和王五，2019a），张三等（2020）也这样认为（见图 2），也有不同意见（参见李四和王五，2019b）。"),
            p("参考文献"),
            p("李四, 王五. 根毛的发育[J]. 植物学报, 2019a, 54(1): 1-9."),
            p("李四, 王五. 根毛的吸水[J]. 植物学报, 2019b, 54(2): 10-19."),
            p("张三, 李四, 赵六, 等. 水稻根系发育的单细胞图谱[J]. 植物学报, 2020, 55(3): 12-20."),
        );
        let source = file(&body, "");
        let found = inspect(&source, &[], &[], None).unwrap();
        assert_eq!(found.language, "zh");
        assert_eq!(named(&found), ["（张三等，2020；李四和王五，2019a） = r3,r1", "（2020） = r3", "（参见李四和王五，2019b） =  带有前缀「参见」"]);
        let done = restyle(&source, &found, &style("gb-7714-2015-numeric"), &Options::default()).unwrap();
        let said = texts(&done.bytes);
        assert_eq!(said[0], "根系向下生长[1,2]，张三等[1]也这样认为（见图 2），也有不同意见（参见李四和王五，2019b）。");
        assert_eq!(said[2], "[1] 张三, 李四, 赵六. 水稻根系发育的单细胞图谱[J]. 植物学报, 2020, 55(3): 12-20.");
        assert!(said[3].starts_with("[2] 李四, 王五. 根毛的发育[J]") && said[4].starts_with("[3] 李四, 王五. 根毛的吸水[J]"), "{said:?}");
        assert_eq!(said.len(), 5);
    }

    #[test]
    fn an_entry_is_looked_up_by_its_doi_or_its_words_when_asked() {
        let body = format!(
            "{}{}{}{}{}",
            p("Roots grow down [1], [2] and [3]."),
            p("References"),
            p("1. An atlas. doi:10.1038/s41477-024-0001-2"),
            p("2. Somebody wrote about root hairs taking up water, in Nature."),
            p("3. Nothing to find here at all."),
        );
        let source = file(&body, "");
        let work = r#"{"type": "journal-article", "title": ["Single-cell atlas of <i>roots</i>"], "container-title": ["Nature Plants"], "volume": "10", "page": "45-67", "DOI": "10.1038/s41477-024-0001-2", "author": [{"family": "Zhang", "given": "Wei"}], "issued": {"date-parts": [[2024, 3]]}}"#;
        let fetch = |url: &str| -> Result<String, String> {
            if url.ends_with("/works/10.1038/s41477-024-0001-2") { return Ok(format!("{{\"message\": {work}}}")) }
            if url.contains("Somebody") { return Ok(r#"{"message": {"items": [{"title": ["Something else entirely"], "author": [{"family": "X"}]}, {"title": ["Root hairs taking up water"], "author": [{"family": "Li", "given": "Q"}], "issued": {"date-parts": [[2021]]}, "container-title": ["Nature"]}]}}"#.into()) }
            Err("HTTP 503".into())
        };
        let found = inspect(&source, &[], &[], Some(&fetch)).unwrap();
        assert_eq!(found.entries.iter().map(|entry| (entry.origin, entry.sure)).collect::<Vec<_>>(), [(LOOKUP, SURE), (LOOKUP, LIKELY), (UNRESOLVED, "")]);
        assert!(found.entries[2].note.contains("HTTP 503"), "{}", found.entries[2].note);
        let done = restyle(&source, &found, &style("apa"), &Options::default()).unwrap();
        let said = texts(&done.bytes);
        assert_eq!(said[0], "Roots grow down (Zhang, 2024), (Li, 2021) and [3].");
        assert_eq!(said[3], "Zhang, W. (2024). Single-cell atlas of roots. Nature Plants, 10, 45–67. https://doi.org/10.1038/s41477-024-0001-2");
        // Without being asked nothing is looked up, and a file without a list is said to have none.
        assert_eq!(inspect(&source, &[], &[], None).unwrap().entries.iter().filter(|entry| entry.record.is_some()).count(), 0);
        assert!(inspect(&file(&p("No list here [1]."), ""), &[], &[], None).unwrap_err().to_string().contains("参考文献表"));
    }

    #[test]
    fn the_works_a_file_cites_become_papers() {
        let body = format!(
            "{}{}{}{}{}{}",
            p("Roots grow down [1, 2], as [2] and [4] said."),
            p("References"),
            p("[1] Zhang W, Li N. Root hairs take up water. Plant Cell, 2021, 33(2): 10-19."),
            p("[2] Wang F. Lateral roots branch out. Nature, 2019, 570: 1-5."),
            p("[3] Nothing to read here at all"),
            p("[4] Chen J. Root caps sense gravity. Science, 2020, 368: 7-9."),
        );
        let source = file(&body, "");
        let found = inspect(&source, &[], &[], None).unwrap();
        // Read by rule, with no network: three papers, and the entry nobody could read said, not made up.
        let read = works(&found, None, &[]);
        assert_eq!(read.iter().map(|work| (work.n, work.paper.as_ref().map(|paper| paper.title.as_str()), work.cited)).collect::<Vec<_>>(), [(1, Some("Root hairs take up water"), 1), (2, Some("Lateral roots branch out"), 2), (3, None, 0), (4, Some("Root caps sense gravity"), 1)]);
        let first = read[0].paper.as_ref().unwrap();
        assert_eq!((first.year, first.journal.as_str(), first.placed().as_str(), first.doi.is_empty()), (Some(2021), "Plant Cell", "33(2), 10–19", true));
        // Searched by its words, an entry gets the DOI of the record whose title it says, of its year; another year is not taken.
        let fetch = |url: &str| -> Result<String, String> {
            if url.contains("Lateral") { return Ok(r#"{"message": {"items": [{"title": ["Lateral roots branch out"], "DOI": "10.1038/s41586-019-0001-1", "author": [{"family": "Wang", "given": "Fang"}], "issued": {"date-parts": [[2019]]}, "container-title": ["Nature"]}]}}"#.into()) }
            if url.contains("caps") { return Ok(r#"{"message": {"items": [{"title": ["Root caps sense gravity"], "DOI": "10.1126/science.0002", "issued": {"date-parts": [[2011]]}}]}}"#.into()) }
            Ok(r#"{"message": {"items": []}}"#.into())
        };
        let looked = works(&found, Some(&fetch), &[2, 4]);
        assert_eq!(looked.iter().map(|work| work.n).collect::<Vec<_>>(), [2, 4]);
        let second = looked[0].paper.as_ref().unwrap();
        assert_eq!((second.doi.as_str(), second.authors.as_str(), second.volume.as_str(), looked[0].origin), ("10.1038/s41586-019-0001-1", "Wang, Fang", "570", LOOKUP));
        assert_eq!((looked[1].paper.as_ref().unwrap().doi.as_str(), looked[1].origin), ("", TEXT));
        assert!(looked[1].note.contains("年份不同"), "{}", looked[1].note);
    }
}
