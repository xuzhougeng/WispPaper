//! Citations in a manuscript: how a paper of the library is cited, finding
//! the citations in a text, and the reference list and BibTeX they make.
//!
//! The written form is Pandoc's: `[@Zhang2024]`, several as
//! `[@Zhang2024; @Li2019b]`, and a locator may follow (`[@Zhang2024, p. 5]`).
//! Chosen because it is plain text that a person reads as "Zhang, 2024",
//! survives being typed, pasted and exported to Markdown unchanged, and is
//! one key per paper, so it resolves by lookup and never by guessing; and
//! because the exported Markdown with the BibTeX of the cited papers (same
//! keys, `bibtex`) is a manuscript Pandoc's citeproc formats in any journal
//! style. A form like `(Zhang et al., 2024)` reads better but names a paper
//! only as long as no other paper of the library has the same first author
//! and year; citations of that older form (`Paper::citation`, what 引用
//! inserted before) are still read, resolved when exactly one paper fits,
//! and can be rewritten into keys (`upgrades`).
//!
//! A key is the first author's family name and the year (`Zhang2024`), the
//! first two words of the title for a paper without authors
//! (`DeNovo2020`), `-nd` for one without a year. Two papers with the same key
//! are told apart by a letter: the paper added first keeps `Zhang2024`, the
//! next is `Zhang2024b`, so adding papers never changes a key already
//! given. A key a citation was written with is kept in the library
//! (`Store::keep_cite_key`), so it stays the paper's when its record is
//! corrected or another paper is removed. Keys are unique ignoring case.
//!
//! What cannot be resolved is reported, not dropped: a key no paper has
//! (`Unknown`), a key whose paper was deleted (`Removed`), a paper in the
//! trash (`Trashed`, still in the reference list), an old-style citation that
//! fits several papers (`Ambiguous`).

use crate::model::Paper;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;

/// How a paper is cited in the text.
pub fn written(key: &str) -> String {
    format!("[@{key}]")
}

/// A chat answer converted for the writing page. Each `(paper_id, key)` must
/// be kept with `Store::keep_cite_key` before saving `text` to a manuscript.
pub struct AiCitationConversion {
    pub text: String,
    pub keys: Vec<(String, String)>,
    /// The sentences the answer quoted for a citation of one paper: what that citation rests on (`passage::quoted`).
    pub quoted: Vec<Quoted>,
}

/// A sentence a chat citation quoted: `[P3 p.5 "…"]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Quoted {
    /// Where the citation stands in the converted text: its opening bracket.
    pub at: usize,
    pub paper_id: String,
    pub page: Option<usize>,
    pub quote: String,
}

/// Convert only complete, unambiguous chat citation groups. Unrecognised
/// text and groups with an unavailable source remain exactly as written.
/// The sentence a citation quotes (`[P3 p.5 "…"]`) is left out of the text,
/// the page kept: a manuscript cites, and the quotation was the
/// conversation's way of showing where. It is handed back beside the text
/// (`quoted`), for the caller to keep as the passage the citation rests on.
/// Conversion is not verification of the claim or page.
pub fn convert_ai_citations(answer: &str, sources: &[crate::ai::Source], index: &Index<'_>) -> AiCitationConversion {
    let mut text = String::new();
    let mut keys = Vec::new();
    let mut quoted = Vec::new();
    let mut end = 0;
    for (range, _) in crate::ai::citation_spans(answer) {
        if range.start < end { continue }
        text.push_str(&answer[end..range.start]);
        let original = &answer[range.clone()];
        if let Some(cited) = parse_chat_group(&without_quotation(original)) {
            let resolved: Option<Vec<_>> = cited.iter().map(|(label, page)| {
                let mut matches = sources.iter().filter(|source| &source.label == label);
                let source = matches.next()?;
                if matches.next().is_some() { return None }
                Some((source.paper_id.clone(), index.key(&source.paper_id)?.to_string(), *page))
            }).collect();
            if let Some(resolved) = resolved {
                let pieces: Vec<String> = resolved.iter().map(|(_, key, page)| match page {
                    Some(page) => format!("@{key}, p. {page}"),
                    None => format!("@{key}"),
                }).collect();
                if let ([(id, _, page)], Some(quote)) = (resolved.as_slice(), quotation(original)) { quoted.push(Quoted { at: text.len(), paper_id: id.clone(), page: *page, quote }) }
                text.push_str(&format!("[{}]", pieces.join("; ")));
                for (id, key, _) in resolved {
                    if !keys.iter().any(|pair| pair == &(id.clone(), key.clone())) { keys.push((id, key)) }
                }
            } else { text.push_str(original) }
        } else { text.push_str(original) }
        end = range.end;
    }
    text.push_str(&answer[end..]);
    AiCitationConversion { text, keys, quoted }
}

/// The sentence a citation group quotes, without its quotation marks: one that runs to the closing bracket.
fn quotation(group: &str) -> Option<String> {
    let at = group.find(['"', '“', '「'])?;
    let inner = group.strip_suffix(']')?.trim_end();
    let open = group[at..].chars().next()?.len_utf8();
    let close = inner.chars().last().filter(|c| ['"', '”', '」'].contains(c))?.len_utf8();
    let quote = inner.get(at + open..inner.len() - close)?.trim();
    (!quote.is_empty()).then(|| quote.to_string())
}

/// A citation group without the sentence it quotes: `[P3 p.5 "…"]` as `[P3 p.5]`.
/// Only a quotation that runs to the closing bracket is one (`ai::citation_spans`).
fn without_quotation(group: &str) -> String {
    let Some(at) = group.find(['"', '“', '「']) else { return group.to_string() };
    let closed = group.strip_suffix(']').is_some_and(|inner| inner.trim_end().ends_with(['"', '”', '」']) && inner.trim_end().len() > at + 1);
    if closed { format!("{}]", group[..at].trim_end()) } else { group.to_string() }
}

/// The deliberately narrow grammar has no free text or quotations, so
/// converting a group cannot silently drop a qualifier or source detail.
fn parse_chat_group(group: &str) -> Option<Vec<(String, Option<usize>)>> {
    let inner = group.strip_prefix('[')?.strip_suffix(']')?;
    if inner.contains(['[', ']', '\n', '"', '“', '”', '「', '」']) { return None }
    let mut citations: Vec<(String, Option<usize>)> = Vec::new();
    let parse_page = |word: &str| -> Option<usize> {
        let digits = word.strip_prefix("p.").or_else(|| word.strip_prefix('p'))?;
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) { return None }
        digits.parse::<usize>().ok().filter(|page| *page > 0)
    };
    for part in inner.split([',', '，', ';', '、']) {
        let mut words = part.split_whitespace();
        let first = words.next()?;
        if first.starts_with('P') && first.len() > 1 && first[1..].bytes().all(|b| b.is_ascii_digit()) {
            let page = match words.next() { Some(word) => Some(parse_page(word)?), None => None };
            if words.next().is_some() { return None }
            citations.push((first.to_string(), page));
        } else {
            let page = parse_page(first)?;
            if words.next().is_some() { return None }
            let last = citations.last_mut()?;
            if last.1.is_some() { return None }
            last.1 = Some(page);
        }
    }
    (!citations.is_empty()).then_some(citations)
}

/// Every paper of a library with its key: the open papers and those in the
/// trash, and the keys kept from earlier citations (key → paper id).
pub struct Index<'a> {
    papers: HashMap<&'a str, (&'a Paper, bool)>,
    key_of: HashMap<&'a str, String>,
    /// By the key in lower case: the paper id, which may be one no longer in the library.
    by_key: HashMap<String, String>,
}

impl<'a> Index<'a> {
    pub fn new(papers: impl IntoIterator<Item = &'a Paper>, trashed: impl IntoIterator<Item = &'a Paper>, kept: &BTreeMap<String, String>) -> Index<'a> {
        let mut all: HashMap<&str, (&Paper, bool)> = HashMap::new();
        for paper in papers { all.insert(paper.id.as_str(), (paper, false)); }
        for paper in trashed { all.insert(paper.id.as_str(), (paper, true)); }
        let mut by_key: HashMap<String, String> = HashMap::new();
        let mut key_of: HashMap<&str, String> = HashMap::new();
        for (key, id) in kept {
            if by_key.contains_key(&key.to_lowercase()) { continue }
            by_key.insert(key.to_lowercase(), id.clone());
            if let Some((paper, _)) = all.get(id.as_str()) { key_of.entry(paper.id.as_str()).or_insert_with(|| key.clone()); }
        }
        // The rest in the order they came into the library: a later paper never takes a key from an earlier one.
        let mut rest: Vec<&Paper> = all.values().map(|(paper, _)| *paper).filter(|paper| !key_of.contains_key(paper.id.as_str())).collect();
        rest.sort_by(|a, b| (a.added_at.as_str(), a.id.as_str()).cmp(&(b.added_at.as_str(), b.id.as_str())));
        for paper in rest {
            let base = base_key(paper);
            let key = (1..).map(|n| suffixed(&base, n)).find(|key| !by_key.contains_key(&key.to_lowercase())).unwrap();
            by_key.insert(key.to_lowercase(), paper.id.clone());
            key_of.insert(paper.id.as_str(), key);
        }
        Index { papers: all, key_of, by_key }
    }

    /// The key a paper is cited by.
    pub fn key(&self, paper_id: &str) -> Option<&str> {
        self.key_of.get(paper_id).map(String::as_str)
    }

    pub fn paper(&self, paper_id: &str) -> Option<&'a Paper> {
        self.papers.get(paper_id).map(|(paper, _)| *paper)
    }

    fn resolve(&self, key: &str) -> Found {
        match self.by_key.get(&key.to_lowercase()) {
            None => Found::Unknown,
            Some(id) => match self.papers.get(id.as_str()) {
                None => Found::Removed,
                Some((_, true)) => Found::Trashed(id.clone()),
                Some((_, false)) => Found::Paper(id.clone()),
            },
        }
    }

    /// The papers an old-style citation can mean: `Zhang et al.`, `Zhang and Li`, `Zhang`, with the year.
    fn fitting(&self, who: &Who, year: i32) -> Vec<String> {
        let candidates: Vec<&Paper> = self.papers.values().map(|(paper, _)| *paper).filter(|paper| paper.year == Some(year)).collect();
        let strict: Vec<&Paper> = candidates.iter().copied().filter(|paper| who.fits(paper, true)).collect();
        let found = if strict.is_empty() { candidates.into_iter().filter(|paper| who.fits(paper, false)).collect() } else { strict };
        let mut ids: Vec<String> = found.into_iter().map(|paper| paper.id.clone()).collect();
        ids.sort_by_key(|id| self.key(id).unwrap_or("").to_string());
        ids
    }

    /// The papers whose author, title, key or year has every word of a query, best first: for citing a paper by typing part of it.
    pub fn matching(&self, query: &str, limit: usize) -> Vec<&'a Paper> {
        let words: Vec<String> = query.split_whitespace().map(plain).filter(|word| !word.is_empty()).collect();
        if words.is_empty() { return vec![] }
        let mut found: Vec<(u8, &Paper)> = self
            .papers
            .values()
            .filter(|(_, trashed)| !trashed)
            .filter_map(|(paper, _)| {
                let first = families(&paper.authors).into_iter().next().map(|name| plain(&name)).unwrap_or_default();
                let key = self.key(&paper.id).map(plain).unwrap_or_default();
                let haystack = format!("{} {} {} {key}", plain(&paper.authors), plain(&paper.title), paper.year.map(|y| y.to_string()).unwrap_or_default());
                if !words.iter().all(|word| haystack.contains(word.as_str())) { return None }
                // The first author or the key named first is what a person types who knows the paper.
                let rank = if !first.is_empty() && first.starts_with(words[0].as_str()) || key.starts_with(words[0].as_str()) { 0 } else { 1 };
                Some((rank, *paper))
            })
            .collect();
        found.sort_by(|(ra, a), (rb, b)| ra.cmp(rb).then(b.year.cmp(&a.year)).then(a.title.cmp(&b.title)));
        found.into_iter().take(limit).map(|(_, paper)| paper).collect()
    }
}

/// What a citation names.
#[derive(Clone, Debug, PartialEq)]
pub enum Found {
    Paper(String),
    /// A paper in the trash: cited, listed, and pointed out.
    Trashed(String),
    /// A key a paper had that has since been deleted.
    Removed,
    /// No paper has this key, or fits this citation.
    Unknown,
    /// An old-style citation several papers fit, best first.
    Ambiguous(Vec<String>),
}

impl Found {
    pub fn paper(&self) -> Option<&str> {
        match self {
            Found::Paper(id) | Found::Trashed(id) => Some(id),
            _ => None,
        }
    }
}

/// One citation in a text.
#[derive(Clone, Debug, PartialEq)]
pub struct Cited {
    /// Where it is written: `@Zhang2024`, or `Zhang et al., 2024` in an old-style one.
    pub range: Range<usize>,
    pub written: String,
    /// The brackets or parentheses it stands in, with the others cited there.
    pub group: Range<usize>,
    /// Old style: `(Zhang et al., 2024)`, not a key.
    pub old: bool,
    pub found: Found,
}

/// The citations of a text in the order they are written: the keys in
/// brackets, and the old author-year form in parentheses.
pub fn find(text: &str, index: &Index) -> Vec<Cited> {
    let mut found = keyed(text, index);
    let taken: Vec<Range<usize>> = found.iter().map(|cited| cited.group.clone()).collect();
    found.extend(old_style(text, index).into_iter().filter(|cited| !taken.iter().any(|group| group.start < cited.group.end && cited.group.start < group.end)));
    found.sort_by_key(|cited| cited.range.start);
    found
}

/// `[… @key …; @key …]`: Pandoc's bracketed citations.
fn keyed(text: &str, index: &Index) -> Vec<Cited> {
    let mut out = vec![];
    let mut from = 0;
    while let Some(open) = text[from..].find('[').map(|at| from + at) {
        let Some(close) = text[open + 1..].find([']', '[', '\n']).map(|at| open + 1 + at) else { break };
        if !text[close..].starts_with(']') {
            from = close;
            continue;
        }
        let group = open..close + 1;
        let inner = &text[open + 1..close];
        let mut keys = vec![];
        let mut previous: Option<char> = None;
        for (at, c) in inner.char_indices() {
            if c == '@' && previous.is_none_or(|p| p.is_whitespace() || p == ';' || p == '-') {
                let key = key_at(&inner[at + 1..]);
                if !key.is_empty() { keys.push((open + 1 + at, key)) }
            }
            previous = Some(c);
        }
        for (at, key) in keys {
            let range = at..at + 1 + key.len();
            out.push(Cited { written: text[range.clone()].to_string(), range, group: group.clone(), old: false, found: index.resolve(key) });
        }
        from = close + 1;
    }
    out
}

/// A key as Pandoc reads it: letters, digits and `_`, with punctuation only between them.
fn key_at(text: &str) -> &str {
    let mut end = 0;
    let mut chars = text.char_indices().peekable();
    if !chars.peek().is_some_and(|(_, c)| c.is_alphanumeric() || *c == '_') { return "" }
    while let Some((at, c)) = chars.next() {
        if c.is_alphanumeric() || c == '_' {
            end = at + c.len_utf8();
        } else if ":.#$%&-+?<>~/".contains(c) && chars.peek().is_some_and(|(_, next)| next.is_alphanumeric() || *next == '_') {
            continue;
        } else {
            break;
        }
    }
    &text[..end]
}

/// Who an old-style citation names.
struct Who {
    /// The words before `et al.`, `and`, or the comma: the first author's family name (possibly after "see").
    first: Vec<String>,
    second: Option<Vec<String>>,
    /// `et al.`: three authors or more.
    more: bool,
}

impl Who {
    fn read(text: &str) -> Option<Who> {
        let text = text.trim();
        let words = |text: &str| -> Vec<String> { text.split_whitespace().map(plain).filter(|word| !word.is_empty()).collect() };
        for end in ["et al.", "et al", "等"] {
            if let Some(first) = text.strip_suffix(end) { return Some(Who { first: words(first), second: None, more: true }).filter(|who| !who.first.is_empty()) }
        }
        for and in [" and ", " & ", "和", "与"] {
            if let Some((first, second)) = text.split_once(and) { return Some(Who { first: words(first), second: Some(words(second)), more: false }).filter(|who| !who.first.is_empty()) }
        }
        Some(Who { first: words(text), second: None, more: false }).filter(|who| !who.first.is_empty())
    }

    /// Strictly: the authors counted as the citation counts them (`Paper::citation`). Loosely: the first author only.
    fn fits(&self, paper: &Paper, strict: bool) -> bool {
        let names = families(&paper.authors);
        let ends = |words: &[String], name: &str| {
            let name: Vec<String> = name.split_whitespace().map(plain).filter(|w| !w.is_empty()).collect();
            !name.is_empty() && words.ends_with(&name)
        };
        if names.is_empty() {
            // `Paper::citation` cites a paper without authors by the first three words of its title.
            let title: Vec<String> = paper.title.split_whitespace().take(3).map(plain).filter(|w| !w.is_empty()).collect();
            return !title.is_empty() && self.first == title;
        }
        if !ends(&self.first, &names[0]) { return false }
        if let Some(second) = &self.second {
            return names.len() == 2 && second.join(" ") == names[1].split_whitespace().map(plain).collect::<Vec<_>>().join(" ");
        }
        !strict || if self.more { names.len() >= 3 } else { names.len() == 1 }
    }
}

/// `(Zhang et al., 2021; Li, 2019)`: the form `Paper::citation` writes, typed by hand or left by an older build.
fn old_style(text: &str, index: &Index) -> Vec<Cited> {
    let mut out = vec![];
    let mut from = 0;
    while let Some((open, width)) = text[from..].char_indices().find(|(_, c)| *c == '(' || *c == '（').map(|(at, c)| (from + at, c.len_utf8())) {
        let Some((close, close_width)) = text[open + width..].char_indices().find(|(_, c)| matches!(c, ')' | '）' | '(' | '（' | '\n')).map(|(at, c)| (open + width + at, c.len_utf8())) else { break };
        if !matches!(text[close..].chars().next(), Some(')' | '）')) {
            from = close;
            continue;
        }
        let group = open..close + close_width;
        let inner_start = open + width;
        let mut parts = vec![];
        let mut start = inner_start;
        for (at, c) in text[inner_start..close].char_indices().chain([(close - inner_start, ';')]) {
            if c != ';' && c != '；' { continue }
            let end = inner_start + at;
            parts.push(start..end);
            start = end + c.len_utf8();
        }
        let read: Option<Vec<(Range<usize>, Who, i32)>> = parts
            .into_iter()
            .map(|part| {
                let piece = &text[part.clone()];
                let (who, year) = piece.rsplit_once([',', '，'])?;
                let year = year.trim();
                let digits = year.trim_end_matches(|c: char| c.is_ascii_lowercase());
                if digits.len() != 4 || !digits.chars().all(|c| c.is_ascii_digit()) || year.len() > 5 { return None }
                if !who.trim().starts_with(char::is_alphabetic) || who.contains(|c: char| c.is_ascii_digit() || c == '=') { return None }
                let skip = piece.len() - piece.trim_start().len();
                let range = part.start + skip..part.start + piece.trim_end().len();
                Some((range, Who::read(who)?, digits.parse().ok()?))
            })
            .collect();
        for (range, who, year) in read.unwrap_or_default() {
            let found = match index.fitting(&who, year).as_slice() {
                [] => Found::Unknown,
                [one] => index.resolve(index.key(one).unwrap_or("")),
                many => Found::Ambiguous(many.to_vec()),
            };
            out.push(Cited { written: text[range.clone()].to_string(), range, group: group.clone(), old: true, found });
        }
        from = close + close_width;
    }
    out
}

/// The old-style citations that can be written as keys: each parenthesis
/// whose every part names one paper, with what replaces it. Last first, so
/// they can be applied in that order without moving the ones still to come.
pub fn upgrades(cited: &[Cited], index: &Index) -> Vec<(Range<usize>, String)> {
    let mut groups: Vec<(Range<usize>, Vec<&Cited>)> = vec![];
    for one in cited.iter().filter(|one| one.old) {
        match groups.last_mut() {
            Some((group, parts)) if *group == one.group => parts.push(one),
            _ => groups.push((one.group.clone(), vec![one])),
        }
    }
    let mut out: Vec<(Range<usize>, String)> = groups
        .into_iter()
        .filter_map(|(group, parts)| {
            let keys: Option<Vec<String>> = parts.iter().map(|part| part.found.paper().and_then(|id| index.key(id)).map(|key| format!("@{key}"))).collect();
            Some((group, format!("[{}]", keys?.join("; "))))
        })
        .collect();
    out.reverse();
    out
}

/// One entry of the reference list.
#[derive(Clone, Debug, PartialEq)]
pub struct Reference {
    pub key: String,
    pub paper_id: String,
    pub text: String,
    pub trashed: bool,
}

/// Each paper the citations name, once, in the order of an author-year
/// list: by the first author's family name (the title for a paper without
/// authors), then the year, then the key.
pub fn references(cited: &[Cited], index: &Index) -> Vec<Reference> {
    let mut seen = HashSet::new();
    let mut out: Vec<(String, Option<i32>, Reference)> = vec![];
    for one in cited {
        let Some(id) = one.found.paper() else { continue };
        if !seen.insert(id.to_string()) { continue }
        let (Some(paper), Some(key)) = (index.paper(id), index.key(id)) else { continue };
        let sort = families(&paper.authors).into_iter().next().unwrap_or_else(|| paper.title.clone());
        out.push((plain(&sort), paper.year, Reference { key: key.to_string(), paper_id: id.to_string(), text: reference(paper), trashed: matches!(one.found, Found::Trashed(_)) }));
    }
    out.sort_by(|a, b| (a.0.as_str(), a.1.is_none(), a.1, a.2.key.to_lowercase()).cmp(&(b.0.as_str(), b.1.is_none(), b.1, b.2.key.to_lowercase())));
    out.into_iter().map(|(_, _, reference)| reference).collect()
}

/// The list as plain text, one entry a line, for copying.
pub fn reference_list(references: &[Reference]) -> String {
    references.iter().map(|reference| reference.text.as_str()).collect::<Vec<_>>().join("\n")
}

/// The BibTeX of the cited papers, under the keys they are cited by.
pub fn bibtex(references: &[Reference], index: &Index) -> String {
    let entries: Vec<(&str, &Paper)> = references.iter().filter_map(|reference| Some((reference.key.as_str(), index.paper(&reference.paper_id)?))).collect();
    crate::export::bibtex_keyed(&entries)
}

/// One entry, from what the record has: `Zhang, W., & Li, Q. (2024). Title. Journal. https://doi.org/…`.
/// The records keep no volume, issue or pages, so an entry has none.
pub fn reference(paper: &Paper) -> String {
    reference_dated(paper, &paper.year.map_or("n.d.".to_string(), |year| year.to_string()))
}

/// `reference` with the year written as given: `2024a` when two entries would otherwise read alike.
fn reference_dated(paper: &Paper, year: &str) -> String {
    let names = authors_shown(&paper.authors);
    let authors = match names.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        many if many.len() > 20 => format!("{}, … {}", many[..19].join(", "), many[many.len() - 1]),
        [rest @ .., last] => format!("{}, & {last}", rest.join(", ")),
    };
    let title = paper.title.trim();
    let title = if title.ends_with(['.', '?', '!', '。', '？']) { title.to_string() } else { format!("{title}.") };
    let mut out = if authors.is_empty() { format!("{title} ({year}).") } else { format!("{authors} ({year}). {title}") };
    let journal = paper.journal.trim();
    if !journal.is_empty() {
        out.push_str(&format!(" {}{}", journal, if journal.ends_with('.') { "" } else { "." }));
    } else if paper.publication_status == "preprint" {
        out.push_str(" Preprint.");
    }
    if !paper.doi.is_empty() {
        out.push_str(&format!(" https://doi.org/{}", paper.doi));
    } else if paper.url.starts_with("http") {
        out.push_str(&format!(" {}", paper.url));
    } else if !paper.pmid.is_empty() {
        out.push_str(&format!(" PMID: {}", paper.pmid));
    }
    out
}

// ---- what a citation says besides its key

/// What a citation writes after its key: the page it points to and the
/// sentence of the paper it quotes, as in `[@Xue2025 p.2 "a core set …"]`
/// or `[@Zhang2024, p. 5]`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Said {
    pub page: Option<usize>,
    pub quote: Option<String>,
}

/// What follows a citation's key up to the next key of its group (`;`
/// outside a quotation) or the closing bracket. Nothing for an old-style one.
pub fn said(text: &str, cited: &Cited) -> Said {
    if cited.old || cited.range.end > text.len() || cited.group.end > text.len() { return Said::default() }
    let rest = &text[cited.range.end..cited.group.end.saturating_sub(1).max(cited.range.end)];
    let mut quoting: Option<char> = None;
    let mut end = rest.len();
    for (at, c) in rest.char_indices() {
        match quoting {
            Some(close) if c == close => quoting = None,
            Some(_) => {}
            None if c == '"' => quoting = Some('"'),
            None if c == '“' => quoting = Some('”'),
            None if c == '「' => quoting = Some('」'),
            None if c == ';' || c == '；' => {
                end = at;
                break;
            }
            None => {}
        }
    }
    let piece = &rest[..end];
    let open = piece.find(['"', '“', '「']);
    let quote = open.and_then(|at| {
        let first = piece[at..].chars().next()?;
        let close = match first { '“' => '”', '「' => '」', _ => '"' };
        let inner = &piece[at + first.len_utf8()..];
        let inner = inner.rfind(close).map_or(inner, |to| &inner[..to]).trim();
        (!inner.is_empty()).then(|| inner.to_string())
    });
    Said { page: page_of(&piece[..open.unwrap_or(piece.len())]), quote }
}

/// The first page a locator names: `p. 5`, `p.5`, `pp. 3–4`, `, p 12`.
fn page_of(locator: &str) -> Option<usize> {
    let words: Vec<&str> = locator.split([' ', ',', '，']).filter(|word| !word.is_empty()).collect();
    let digits = |word: &str| -> Option<usize> {
        let lead: String = word.chars().take_while(char::is_ascii_digit).collect();
        lead.parse().ok().filter(|page| *page > 0)
    };
    for (n, word) in words.iter().enumerate() {
        let word = word.to_lowercase();
        if ["p", "p.", "pp", "pp."].contains(&word.as_str()) { return words.get(n + 1).and_then(|next| digits(next)) }
        if let Some(rest) = word.strip_prefix("pp.").or_else(|| word.strip_prefix("p.")) { return digits(rest) }
        if let Some(rest) = word.strip_prefix('p') && rest.starts_with(|c: char| c.is_ascii_digit()) { return digits(rest) }
    }
    None
}

// ---- how a manuscript leaving the app cites

/// How the citations of a manuscript exported to Word are written. In the
/// app they stay keys (`[@Zhang2024]`), which find their paper by lookup;
/// a reader of the exported file sees a journal's form and a reference list.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Style {
    /// `(Zhang et al., 2024)`, the list by author and year.
    #[default]
    AuthorYear,
    /// `[1]`, numbered in the order first cited, the list in that order.
    Numeric,
    /// A journal's own form, as a CSL file defines it (`csl`).
    Csl(std::sync::Arc<crate::csl::Loaded>),
}

impl Style {
    /// The two simple styles as kept in the settings: `numeric`, else author-year. A CSL style is read by `csl::style`.
    pub fn from_key(key: &str) -> Style {
        if key == "numeric" { Style::Numeric } else { Style::AuthorYear }
    }

    pub fn key(&self) -> &str {
        match self {
            Style::AuthorYear => "author-year",
            Style::Numeric => "numeric",
            Style::Csl(style) => &style.key,
        }
    }
}

/// A text as a Word file sets it: what is in italics, bold or raised, and
/// which of its words are a citation of which paper (by the paper's id),
/// so the file can lead from a citation to its entry in the list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rich {
    pub text: String,
    pub looks: Vec<(Range<usize>, crate::docx::Look)>,
    pub links: Vec<(Range<usize>, String)>,
}

impl Rich {
    fn push(&mut self, text: &str) {
        self.text.push_str(text);
    }

    /// Text that cites a paper.
    fn push_linked(&mut self, text: &str, paper_id: &str) {
        let start = self.text.len();
        self.text.push_str(text);
        self.links.push((start..self.text.len(), paper_id.to_string()));
    }

    fn append(&mut self, other: &Rich) {
        let base = self.text.len();
        self.text.push_str(&other.text);
        self.looks.extend(other.looks.iter().map(|(range, look)| (base + range.start..base + range.end, *look)));
        self.links.extend(other.links.iter().map(|(range, to)| (base + range.start..base + range.end, to.clone())));
    }
}

/// The citations of a manuscript written in a `Style`: what each cited
/// paper is called in the text and its entry in the reference list. Made
/// once from every section, so the numbers and the letters that tell two
/// papers apart run through the whole manuscript. Only a group whose every
/// key names a paper is rewritten; one with a key nothing has stays as
/// written, so a reader of the file sees that something is missing.
pub struct Formatter {
    style: Style,
    chinese: bool,
    /// Paper id → `Zhang et al., 2024a` (author-year) or its number.
    labels: HashMap<String, String>,
    numbers: HashMap<String, usize>,
    /// The reference list in the style's order: the paper, and its entry.
    list: Vec<(String, Rich)>,
    /// A CSL style: how each group of papers is written, by the papers it cites in the order written. A group
    /// cited again reads as it did the first time, also in a style that would shorten a later citation.
    groups: HashMap<Vec<String>, Rich>,
}

/// The papers a group cites, each once in the order written; none when a key of it names no paper.
fn cited_ids(group: &[Cited], known: impl Fn(&str) -> bool) -> Option<Vec<&str>> {
    let mut ids: Vec<&str> = group.iter().map(|one| one.found.paper().filter(|id| known(id))).collect::<Option<_>>()?;
    let mut seen = HashSet::new();
    ids.retain(|id| seen.insert(*id));
    Some(ids)
}

impl Formatter {
    pub fn new(texts: &[&str], index: &Index, style: &Style, language: &str) -> Formatter {
        let chinese = language == "zh";
        // Only the groups that will be written count: a paper cited beside a key nothing has gets no number and no entry from that group.
        let groups: Vec<Vec<Cited>> = texts.iter().flat_map(|text| written_groups(text, index)).filter(|group| group.iter().all(|one| one.found.paper().is_some_and(|id| index.paper(id).is_some()))).collect();
        let cited: Vec<Cited> = groups.iter().flatten().cloned().collect();
        let mut numbers: HashMap<String, usize> = HashMap::new();
        let mut order: Vec<String> = vec![];
        for one in &cited {
            let Some(id) = one.found.paper() else { continue };
            if index.paper(id).is_none() || numbers.contains_key(id) { continue }
            order.push(id.to_string());
            numbers.insert(id.to_string(), order.len());
        }
        let mut labels = HashMap::new();
        let mut written = HashMap::new();
        let plain = |text: String| Rich { text, ..Default::default() };
        let list = match style {
            Style::Numeric => order.iter().filter_map(|id| Some((id.clone(), plain(format!("[{}] {}", numbers[id], reference(index.paper(id)?)))))).collect(),
            Style::AuthorYear => {
                let listed = references(&cited, index);
                // Two papers that would read alike, `Zhang et al., 2024`, are 2024a and 2024b, in the list's order.
                let mut alike: HashMap<(String, String), Vec<String>> = HashMap::new();
                for one in &listed {
                    let Some(paper) = index.paper(&one.paper_id) else { continue };
                    alike.entry((who(paper), year_of(paper))).or_default().push(one.paper_id.clone());
                }
                let mut out = vec![];
                for one in &listed {
                    let Some(paper) = index.paper(&one.paper_id) else { continue };
                    let (name, year) = (who(paper), year_of(paper));
                    let same = &alike[&(name.clone(), year.clone())];
                    let year = match same.iter().position(|id| *id == one.paper_id) {
                        Some(n) if same.len() > 1 && n < 26 => format!("{year}{}", (b'a' + n as u8) as char),
                        _ => year,
                    };
                    labels.insert(one.paper_id.clone(), format!("{name}, {year}"));
                    out.push((one.paper_id.clone(), plain(reference_dated(paper, &year))));
                }
                out
            }
            Style::Csl(csl) => {
                // Every group as the processor is to see it: in the order written, the ones with a key nothing has left out.
                let asked: Vec<Vec<&str>> = groups.iter().filter_map(|group| cited_ids(group, |id| numbers.contains_key(id))).collect();
                let papers: Vec<Vec<&Paper>> = asked.iter().map(|ids| ids.iter().filter_map(|id| index.paper(id)).collect()).collect();
                let (cited, list) = crate::csl::render(csl, &papers, language);
                for (ids, rich) in asked.iter().zip(cited) { written.entry(ids.iter().map(|id| id.to_string()).collect()).or_insert(rich); }
                list
            }
        };
        Formatter { style: style.clone(), chinese, labels, numbers, list, groups: written }
    }

    /// How a group that cites these papers is written.
    fn written(&self, ids: &[&str]) -> Option<Rich> {
        let mut out = Rich::default();
        match &self.style {
            Style::Numeric => {
                let mut numbered: Vec<(usize, &str)> = ids.iter().map(|id| (self.numbers[*id], *id)).collect();
                numbered.sort_unstable();
                out.push("[");
                // `1, 3–5, 7`: three numbers in a row or more as a range, which names its two ends.
                let mut at = 0;
                while at < numbered.len() {
                    let mut end = at;
                    while end + 1 < numbered.len() && numbered[end + 1].0 == numbered[end].0 + 1 { end += 1 }
                    if at > 0 { out.push(", ") }
                    out.push_linked(&numbered[at].0.to_string(), numbered[at].1);
                    if end > at {
                        out.push(if end - at == 1 { ", " } else { "–" });
                        out.push_linked(&numbered[end].0.to_string(), numbered[end].1);
                    }
                    at = end + 1;
                }
                out.push("]");
            }
            Style::AuthorYear => {
                out.push(if self.chinese { "（" } else { "(" });
                for (n, id) in ids.iter().filter(|id| self.labels.contains_key(**id)).enumerate() {
                    if n > 0 { out.push(if self.chinese { "；" } else { "; " }) }
                    out.push_linked(&self.labels[*id], id);
                }
                out.push(if self.chinese { "）" } else { ")" });
            }
            Style::Csl(_) => return self.groups.get(&ids.iter().map(|id| id.to_string()).collect::<Vec<_>>()).cloned(),
        }
        Some(out)
    }

    /// `text` with each citation group that names papers written in the
    /// style, the rest as it was; with what the style sets apart, and
    /// which words cite which paper.
    pub fn rich(&self, text: &str, index: &Index) -> Rich {
        let (mut out, mut at) = (Rich::default(), 0);
        for group in written_groups(text, index) {
            let Some(first) = group.first() else { continue };
            let Some(written) = cited_ids(&group, |id| self.numbers.contains_key(id)).and_then(|ids| self.written(&ids)) else { continue };
            out.push(&text[at..first.group.start]);
            out.append(&written);
            at = first.group.end;
        }
        out.push(&text[at..]);
        out
    }

    /// `rich` as words alone.
    pub fn apply(&self, text: &str, index: &Index) -> String {
        self.rich(text, index).text
    }

    /// The reference list in the style's order: each paper's id and its entry.
    pub fn references(&self) -> &[(String, Rich)] {
        &self.list
    }

    /// What the list is headed: in the manuscript's language.
    pub fn heading(&self) -> &'static str {
        if self.chinese { "参考文献" } else { "References" }
    }
}

/// The bracketed citations of a text, a group each, in order: old-style ones are left as they are written.
fn written_groups(text: &str, index: &Index) -> Vec<Vec<Cited>> {
    let mut groups: Vec<Vec<Cited>> = vec![];
    for one in find(text, index).into_iter().filter(|one| !one.old) {
        match groups.last_mut() {
            Some(group) if group[0].group == one.group => group.push(one),
            _ => groups.push(vec![one]),
        }
    }
    groups
}

/// Who a paper is in an author-year citation: `Zhang`, `Zhang & Li`, `Zhang et al.`; `张等` for a name in Chinese.
fn who(paper: &Paper) -> String {
    let names = families(&paper.authors);
    let chinese = names.first().is_some_and(|name| name.chars().any(crate::structure::is_cjk));
    match names.as_slice() {
        [] => paper.title.split_whitespace().take(3).collect::<Vec<_>>().join(" ").trim_end_matches(|c: char| !c.is_alphanumeric()).to_string(),
        [one] => one.clone(),
        [one, two] if chinese => format!("{one}和{two}"),
        [one, two] => format!("{one} & {two}"),
        [first, ..] if chinese => format!("{first}等"),
        [first, ..] => format!("{first} et al."),
    }
}

fn year_of(paper: &Paper) -> String {
    paper.year.map_or("n.d.".to_string(), |year| year.to_string())
}

/// `Zhang, Tian-Qi`, `Tian-Qi Zhang` or `Zhang TQ` → `Zhang, T.-Q.`; a name in one piece stays as it is.
/// The names are read together (`model::author_names`): how a list is written tells which word is the family name.
fn authors_shown(authors: &str) -> Vec<String> {
    crate::model::author_names(authors)
        .into_iter()
        .map(|(family, given)| {
            let initials: Vec<String> = given
                .split_whitespace()
                // `TQ` is two initials already; `Tian-Qi` gives one for each piece.
                .map(|part| if part.chars().count() <= 3 && part.chars().all(char::is_uppercase) { part.chars().map(|c| format!("{c}.")).collect::<Vec<_>>().join(" ") } else { part.split('-').filter_map(|piece| piece.chars().next()).map(|c| format!("{}.", c.to_uppercase())).collect::<Vec<_>>().join("-") })
                .filter(|part| !part.is_empty())
                .collect();
            if initials.is_empty() { family } else { format!("{family}, {}", initials.join(" ")) }
        })
        .collect()
}

/// The family names of `A; B; C`, the way `Paper::citation` reads them.
fn families(authors: &str) -> Vec<String> {
    crate::model::author_names(authors).into_iter().map(|(family, _)| family).collect()
}

const STOP_WORDS: [&str; 16] = ["a", "an", "the", "of", "on", "in", "and", "for", "to", "with", "by", "at", "from", "into", "is", "are"];

/// The key before a letter tells it apart: `Zhang2024`, `DeNovo2020`, `Zhang-nd`.
fn base_key(paper: &Paper) -> String {
    let word = |text: &str| -> String {
        let folded: String = text.chars().map(fold).collect();
        let mut out = String::new();
        for c in folded.chars() {
            if c.is_alphanumeric() {
                out.push(c);
            } else if c == '-' && !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
        }
        out.trim_end_matches('-').to_string()
    };
    let mut who = families(&paper.authors).first().map(|name| word(&name.replace(char::is_whitespace, ""))).unwrap_or_default();
    if who.is_empty() {
        who = paper
            .title
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty() && !STOP_WORDS.contains(&w.to_lowercase().as_str()))
            .take(2)
            .map(|w| {
                let w = word(w);
                let mut chars = w.chars();
                chars.next().map(|first| first.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default()
            })
            .collect();
    }
    if who.is_empty() { who = "Anon".into() }
    if !who.starts_with(|c: char| c.is_alphanumeric()) { who.insert_str(0, "Anon") }
    match paper.year {
        Some(year) => format!("{who}{year}"),
        None => format!("{who}-nd"),
    }
}

/// The n-th key of a base, from 1: `Zhang2024`, `Zhang2024b`, … `Zhang2024z`, `Zhang2024-27`.
fn suffixed(base: &str, n: usize) -> String {
    let sep = if base.ends_with(|c: char| c.is_ascii_digit()) { "" } else { "-" };
    match n {
        1 => base.to_string(),
        2..=26 => format!("{base}{sep}{}", (b'a' + n as u8 - 1) as char),
        _ => format!("{base}-{n}"),
    }
}

/// Lower case, accents taken off, letters and digits only: how names and words are compared.
fn plain(text: &str) -> String {
    text.chars().map(fold).collect::<String>().to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect()
}

/// The accented Latin letters of names, written without the accent (`ü` → `u`, `ß` → `ss`).
fn fold(c: char) -> String {
    let lower = c.to_lowercase().next().unwrap_or(c);
    let plain = match lower {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'č' => "c",
        'ď' | 'đ' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => "e",
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'į' | 'ı' => "i",
        'ł' | 'ľ' | 'ĺ' => "l",
        'ñ' | 'ń' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => "o",
        'œ' => "oe",
        'ř' | 'ŕ' => "r",
        'ś' | 'š' | 'ş' | 'ș' => "s",
        'ß' => "ss",
        'ť' | 'ţ' | 'ț' => "t",
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' | 'ų' => "u",
        'ý' | 'ÿ' => "y",
        'ź' | 'ž' | 'ż' => "z",
        _ => return c.to_string(),
    };
    if c.is_uppercase() {
        let mut chars = plain.chars();
        chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
    } else {
        plain.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paper(id: &str, authors: &str, year: Option<i32>, title: &str, added: &str) -> Paper {
        Paper { id: id.into(), authors: authors.into(), year, title: title.into(), added_at: added.into(), ..Default::default() }
    }

    fn library() -> Vec<Paper> {
        vec![
            paper("P1", "Zhang, Wei; Li, Qiang; Wang, Fang", Some(2024), "Single-cell atlas of roots", "2024-01-01"),
            paper("P2", "Zhang, Min; Chen, Yu; Liu, Bo", Some(2024), "WOX5 and the quiescent centre", "2024-02-01"),
            paper("P3", "", Some(2020), "De Novo Domestication: an alternative route", "2024-03-01"),
            paper("P4", "Jürgen Müller", None, "Auxin", "2024-04-01"),
            paper("P5", "Smith, Alice; Jones, Bob", Some(2019), "Two authors", "2024-05-01"),
        ]
    }

    #[test]
    fn converts_only_complete_chat_citations_and_preserves_locators() {
        let papers = library();
        let index = Index::new(&papers, [], &BTreeMap::new());
        let sources = vec![
            crate::ai::Source { label: "P1".into(), paper_id: "P1".into(), title: "One".into() },
            crate::ai::Source { label: "P2".into(), paper_id: "P2".into(), title: "Two".into() },
        ];
        let original = "测试 [P1 p.5] and [P1, p.6; P2 p.7] [P1, P9] [P1 \"quote\"] [P2 p.3 “a sentence. With a stop”] [P1 p.5 extra] [P1 p.0].";
        let converted = convert_ai_citations(original, &sources, &index);
        assert_eq!(converted.text, "测试 [@Zhang2024, p. 5] and [@Zhang2024, p. 6; @Zhang2024b, p. 7] [P1, P9] [@Zhang2024] [@Zhang2024b, p. 3] [P1 p.5 extra] [P1 p.0].");
        assert_eq!(converted.keys, vec![("P1".into(), "Zhang2024".into()), ("P2".into(), "Zhang2024b".into())]);
        // What was quoted is handed back with where its citation now stands.
        assert_eq!(converted.quoted.iter().map(|q| (&converted.text[q.at..q.at + 2], q.paper_id.as_str(), q.page, q.quote.as_str())).collect::<Vec<_>>(), [("[@", "P1", None, "quote"), ("[@", "P2", Some(3), "a sentence. With a stop")]);
        assert_eq!(&converted.text[converted.quoted[1].at..], "[@Zhang2024b, p. 3] [P1 p.5 extra] [P1 p.0].");
        let references = find(&converted.text, &index);
        assert_eq!(references.iter().filter(|citation| citation.found.paper().is_some()).count(), 5);

        let mut ambiguous = sources.clone();
        ambiguous.push(crate::ai::Source { label: "P1".into(), paper_id: "P2".into(), title: "duplicate".into() });
        let result = convert_ai_citations("[P1, P2] [P2]", &ambiguous, &index);
        assert_eq!(result.text, "[P1, P2] [@Zhang2024b]");
        assert_eq!(result.keys, vec![("P2".into(), "Zhang2024b".into())]);

        let removed = vec![crate::ai::Source { label: "P1".into(), paper_id: "gone".into(), title: "Gone".into() }];
        let result = convert_ai_citations("[P1 p.5]", &removed, &index);
        assert_eq!(result.text, "[P1 p.5]");
        assert!(result.keys.is_empty());
    }

    #[test]
    fn keys_are_readable_and_unique() {
        let papers = library();
        let index = Index::new(&papers, [], &BTreeMap::new());
        let keys: Vec<&str> = ["P1", "P2", "P3", "P4", "P5"].iter().map(|id| index.key(id).unwrap()).collect();
        assert_eq!(keys, ["Zhang2024", "Zhang2024b", "DeNovo2020", "Muller-nd", "Smith2019"]);
        // Authors as Europe PMC writes them, the family name first: the key is the family name's, not the initials'.
        let pmc = [paper("E1", "Bono M; Marín-Sanz M; Uberegui E", Some(2026), "TaPYL", "2026-01-01"), paper("E2", "de Melo AV; Costa LC", Some(2026), "Iron", "2026-01-02")];
        let index = Index::new(&pmc, [], &BTreeMap::new());
        assert_eq!((index.key("E1"), index.key("E2")), (Some("Bono2026"), Some("deMelo2026")));
        // A key cited before stays the paper's, whatever it would be called now.
        let kept = BTreeMap::from([("M2026".to_string(), "E1".to_string())]);
        assert_eq!(Index::new(&pmc, [], &kept).key("E1"), Some("M2026"));
    }

    #[test]
    fn keys_hold_when_papers_come_and_go() {
        let mut papers = library();
        // A paper added later does not take the key of an earlier one.
        papers.push(paper("P0", "Zhang, Lei", Some(2024), "Later", "2025-01-01"));
        let index = Index::new(&papers, [], &BTreeMap::new());
        assert_eq!((index.key("P1"), index.key("P2"), index.key("P0")), (Some("Zhang2024"), Some("Zhang2024b"), Some("Zhang2024c")));
        // Cited keys are kept: deleting P1 leaves P2 its key, and a citation of P1 says it is gone.
        let kept: BTreeMap<String, String> = [("Zhang2024".to_string(), "P1".to_string()), ("Zhang2024b".to_string(), "P2".to_string())].into();
        let rest: Vec<Paper> = papers.iter().filter(|p| p.id != "P1").cloned().collect();
        let index = Index::new(&rest, [], &kept);
        assert_eq!(index.key("P2"), Some("Zhang2024b"));
        assert_eq!(index.key("P0"), Some("Zhang2024c"));
        let cited = find("as shown [@Zhang2024; @zhang2024B]", &index);
        assert_eq!(cited.iter().map(|c| c.found.clone()).collect::<Vec<_>>(), [Found::Removed, Found::Paper("P2".into())]);
        // A corrected record keeps the key it was cited by.
        let mut fixed = papers.clone();
        fixed[0].year = Some(2023);
        let index = Index::new(&fixed, [], &kept);
        assert_eq!(index.key("P1"), Some("Zhang2024"));
        assert_eq!(index.key("P0"), Some("Zhang2024c"));
    }

    #[test]
    fn finds_bracketed_keys() {
        let papers = library();
        let (open, trash) = papers.split_at(4);
        let index = Index::new(open, trash, &BTreeMap::new());
        let text = "Roots [@Zhang2024, p. 5; see @DeNovo2020]. Mail me@x.org [@Nobody1999] and [@Smith2019]. [P3 p.5] [@].";
        let cited = find(text, &index);
        let got: Vec<(&str, Found)> = cited.iter().map(|c| (c.written.as_str(), c.found.clone())).collect();
        assert_eq!(got, [("@Zhang2024", Found::Paper("P1".into())), ("@DeNovo2020", Found::Paper("P3".into())), ("@Nobody1999", Found::Unknown), ("@Smith2019", Found::Trashed("P5".into()))]);
        assert_eq!(&text[cited[0].group.clone()], "[@Zhang2024, p. 5; see @DeNovo2020]");
        assert_eq!(&text[cited[0].range.clone()], "@Zhang2024");
        // A sentence's full stop after a key is not part of it.
        assert_eq!(key_at("Zhang2024."), "Zhang2024");
        assert_eq!(key_at("a:b-c_d]"), "a:b-c_d");
    }

    #[test]
    fn reads_old_style_citations() {
        let papers = library();
        let index = Index::new(&papers, [], &BTreeMap::new());
        let text = "A (Zhang et al., 2024) B (Smith and Jones, 2019; De Novo Domestication:, 2020) C (Müller, 2021) D (n = 12, 2021) (March 2021) （Smith, 2019）";
        let cited = find(text, &index);
        let got: Vec<(&str, Found)> = cited.iter().map(|c| (c.written.as_str(), c.found.clone())).collect();
        assert_eq!(
            got,
            [
                ("Zhang et al., 2024", Found::Ambiguous(vec!["P1".into(), "P2".into()])),
                ("Smith and Jones, 2019", Found::Paper("P5".into())),
                ("De Novo Domestication:, 2020", Found::Paper("P3".into())),
                ("Müller, 2021", Found::Unknown),
                // Loosely: Smith is the first author of one paper of 2019, though not its only one.
                ("Smith, 2019", Found::Paper("P5".into())),
            ]
        );
        let upgrades = upgrades(&cited, &index);
        assert_eq!(upgrades.iter().map(|(_, to)| to.as_str()).collect::<Vec<_>>(), ["[@Smith2019]", "[@Smith2019; @DeNovo2020]"]);
        let mut fixed = text.to_string();
        for (range, to) in &upgrades { fixed.replace_range(range.clone(), to) }
        assert!(fixed.starts_with("A (Zhang et al., 2024) B [@Smith2019; @DeNovo2020] C (Müller, 2021)") && fixed.ends_with(" [@Smith2019]"), "{fixed}");
        // Read again, the keys are found and the old forms no longer.
        assert_eq!(find(&fixed, &index).iter().filter(|c| c.old).count(), 2);
    }

    #[test]
    fn reference_list() {
        let mut papers = library();
        papers[0].journal = "Nature Plants".into();
        papers[0].doi = "10.1000/x".into();
        papers[2].publication_status = "preprint".into();
        papers[4].pmid = "123".into();
        let index = Index::new(&papers, [], &BTreeMap::new());
        let cited = find("[@Zhang2024] [@Smith2019; @DeNovo2020] again [@Zhang2024] [@Muller-nd] [@Gone2000]", &index);
        let refs = references(&cited, &index);
        assert_eq!(refs.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), ["DeNovo2020", "Muller-nd", "Smith2019", "Zhang2024"]);
        assert_eq!(refs[0].text, "De Novo Domestication: an alternative route. (2020). Preprint.");
        assert_eq!(refs[1].text, "Müller, J. (n.d.). Auxin.");
        assert_eq!(refs[2].text, "Smith, A., & Jones, B. (2019). Two authors. PMID: 123");
        assert_eq!(refs[3].text, "Zhang, W., Li, Q., & Wang, F. (2024). Single-cell atlas of roots. Nature Plants. https://doi.org/10.1000/x");
        let bib = bibtex(&refs, &index);
        assert!(bib.starts_with("@article{DeNovo2020,\n") && bib.contains("@article{Zhang2024,\n") && bib.matches("@article").count() == 4, "{bib}");
        assert_eq!(authors_shown("Tian-Qi Zhang; 张伟"), ["Zhang, T.-Q.", "张伟"]);
        // Europe PMC writes the family name first and initials after it.
        assert_eq!(authors_shown("Bono M; Marín-Sanz M; de Melo AV"), ["Bono, M.", "Marín-Sanz, M.", "de Melo, A. V."]);
        assert_eq!(authors_shown("Zhang, TQ; Harris, Charles R."), ["Zhang, T. Q.", "Harris, C. R."]);
    }

    #[test]
    fn picking_a_paper_by_typing() {
        let papers = library();
        let index = Index::new(&papers, [], &BTreeMap::new());
        let ids = |query: &str| index.matching(query, 5).iter().map(|p| p.id.as_str()).collect::<Vec<_>>();
        assert_eq!(ids("zhang wox5"), ["P2"]);
        assert_eq!(ids("muller"), ["P4"]);
        assert_eq!(ids("domestication"), ["P3"]);
        assert_eq!(ids("2019"), ["P5"]);
        assert!(ids("  ").is_empty());
        // The first author first, then the papers that only mention the word.
        assert_eq!(ids("smith"), ["P5"]);
        assert_eq!(ids("jones two"), ["P5"]);
    }

    #[test]
    fn what_a_citation_says_besides_its_key() {
        let papers = library();
        let index = Index::new(&papers, [], &BTreeMap::new());
        let text = "根系[@Zhang2024 p.2 \"a core set; of genes\"]，见 [@Smith2019, p. 5; @Zhang2024b “Another one.”] 和 [@DeNovo2020, pp. 3–4] [@Smith2019 p12] [@Smith2019]";
        let said: Vec<Said> = find(text, &index).iter().map(|one| said(text, one)).collect();
        let s = |page: Option<usize>, quote: Option<&str>| Said { page, quote: quote.map(str::to_string) };
        assert_eq!(said, [s(Some(2), Some("a core set; of genes")), s(Some(5), None), s(None, Some("Another one.")), s(Some(3), None), s(Some(12), None), s(None, None)]);
    }

    #[test]
    fn citations_written_for_a_reader() {
        let papers = library();
        let index = Index::new(&papers, [], &BTreeMap::new());
        let one = "Roots [@Zhang2024 p.2 \"quoted\"] and shoots [@Smith2019; @Zhang2024b] [@Gone2000].";
        let two = "Again [@DeNovo2020; @Zhang2024; @Smith2019; @Zhang2024b] (Zhang et al., 2024).";
        // Author and year: two papers that read alike get letters, the list by author.
        let format = Formatter::new(&[one, two], &index, &Style::AuthorYear, "en");
        assert_eq!(format.apply(one, &index), "Roots (Zhang et al., 2024a) and shoots (Smith & Jones, 2019; Zhang et al., 2024b) [@Gone2000].");
        assert_eq!(format.apply(two, &index), "Again (De Novo Domestication, 2020; Zhang et al., 2024a; Smith & Jones, 2019; Zhang et al., 2024b) (Zhang et al., 2024).");
        assert_eq!(format.references().len(), 4);
        assert!(format.references()[2].1.text.starts_with("Zhang, W., Li, Q., & Wang, F. (2024a). Single-cell"), "{:?}", format.references());
        assert!(format.references()[3].1.text.contains("(2024b). WOX5"));
        assert_eq!(format.heading(), "References");
        // Numbered in the order first cited, three in a row as a range.
        let format = Formatter::new(&[one, two], &index, &Style::Numeric, "zh");
        assert_eq!(format.apply(one, &index), "Roots [1] and shoots [2, 3] [@Gone2000].");
        assert_eq!(format.apply(two, &index), "Again [1–4] (Zhang et al., 2024).");
        assert!(format.references()[0].1.text.starts_with("[1] Zhang, W.") && format.references()[3].1.text.starts_with("[4] De Novo"));
        assert_eq!(format.heading(), "参考文献");
        let chinese = Formatter::new(&["差异[@Smith2019]。"], &index, &Style::AuthorYear, "zh");
        assert_eq!(chinese.apply("差异[@Smith2019]。", &index), "差异（Smith & Jones, 2019）。");
        // Which words cite which paper: a range by its two ends, an author-year group name by name.
        let linked = |rich: Rich| rich.links.iter().map(|(range, to)| (rich.text[range.clone()].to_string(), index.key(to).unwrap_or("").to_string())).collect::<Vec<_>>();
        let pair = |words: &str, key: &str| (words.to_string(), key.to_string());
        assert_eq!(linked(format.rich(two, &index)), [pair("1", "Zhang2024"), pair("4", "DeNovo2020")]);
        assert_eq!(linked(format.rich(one, &index)), [pair("1", "Zhang2024"), pair("2", "Smith2019"), pair("3", "Zhang2024b")]);
        assert_eq!(linked(chinese.rich("差异[@Smith2019]。", &index)), [pair("Smith & Jones, 2019", "Smith2019")]);
        // A journal's style: the same groups, as its CSL file says.
        let nature = Style::Csl(std::sync::Arc::new(crate::csl::load("csl:nature", std::path::Path::new("/nowhere")).unwrap()));
        let format = Formatter::new(&[one, two], &index, &nature, "en");
        let rich = format.rich(one, &index);
        assert_eq!(rich.text, "Roots 1 and shoots 2,3 [@Gone2000].");
        assert_eq!(linked(rich.clone()), [pair("1", "Zhang2024"), pair("2", "Smith2019"), pair("3", "Zhang2024b")]);
        assert!(rich.looks.iter().all(|(_, look)| look.sup) && !rich.looks.is_empty());
        assert_eq!(format.apply(two, &index), "Again 1–4 (Zhang et al., 2024).");
        assert_eq!(format.references().len(), 4);
        assert!(format.references()[0].1.text.starts_with("1. Zhang, W., Li, Q. & Wang, F."), "{:?}", format.references()[0]);
    }
}
