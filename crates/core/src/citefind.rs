//! 配引用: the papers of the library that what is written can cite,
//! sentence by sentence. Nothing here writes a word of the manuscript's:
//! the result is a list of papers per sentence, each with the sentence of
//! its own that says so, and the person's ticks become citations at the
//! ends of their sentences (`place`).
//!
//! The steps, each a function of its own so the window and the tool share
//! them: `sentences` cuts the text and keeps where each sentence stands;
//! a model may turn each into search words in the language the library's
//! papers are written in (`words_prompt` / `parse_words`), which is what
//! lets a Chinese paragraph find English papers; `search` ranks the papers
//! for every sentence by their record and by their sentences, and names
//! the sentence that matches best; a model then says which of those really
//! bear the sentence out (`judge_prompt` / `parse_judgement`). A paper is
//! only ever one the search found: a model chooses, it does not name.

use crate::model::Paper;
use crate::retrieval;
use crate::store::PaperSentence;
use crate::structure;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

/// The most sentences one request looks for: a section, not a manuscript.
pub const MAX_SENTENCES: usize = 40;

/// Where the sentences of a text stand, in bytes, without the spaces
/// around them. Full stops of either script, question and exclamation
/// marks, semicolons and line breaks end one; a full stop after an
/// abbreviation or inside a number does not, and nothing inside brackets
/// does (`[@A; @B]`, `(p < 0.05; n = 3)`).
pub fn sentences(text: &str) -> Vec<Range<usize>> {
    cut(text, true)
}

/// The sentences as a reader takes them: a semicolon goes on with the
/// sentence it stands in. What a click selects (`selection::sentence`).
pub fn whole_sentences(text: &str) -> Vec<Range<usize>> {
    cut(text, false)
}

fn cut(text: &str, semicolons: bool) -> Vec<Range<usize>> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let byte = |i: usize| chars.get(i).map_or(text.len(), |(at, _)| *at);
    let mut out: Vec<Range<usize>> = vec![];
    let mut keep = |from: usize, to: usize| {
        let piece = &text[from..to];
        let start = from + (piece.len() - piece.trim_start().len());
        let end = start + piece.trim().len();
        if end > start { out.push(start..end) }
    };
    let (mut start, mut depth, mut at) = (0, 0usize, 0);
    while at < chars.len() {
        let c = chars[at].1;
        match c {
            '\n' => {
                keep(byte(start), byte(at));
                (start, depth) = (at + 1, 0);
            }
            '(' | '[' | '（' | '【' => depth += 1,
            ')' | ']' | '）' | '】' => depth = depth.saturating_sub(1),
            _ => {}
        }
        let between_digits = at > 0 && chars[at - 1].1.is_ascii_digit() && chars.get(at + 1).is_some_and(|(_, c)| c.is_ascii_digit());
        let semicolon = matches!(c, ';' | '；');
        let wide = matches!(c, '。' | '！' | '？' | '；') || (c == '．' && !between_digits);
        if depth == 0 && (semicolons || !semicolon) && (wide || matches!(c, '.' | '!' | '?' | ';')) {
            // A closing quotation mark belongs to the sentence it closes.
            let mut end = at + 1;
            while end < chars.len() && matches!(chars[end].1, '"' | '”' | '’' | '\'' | '」' | '』') { end += 1 }
            let spaced = wide || end == chars.len() || chars[end].1.is_whitespace();
            let next = chars[end..].iter().map(|(_, c)| *c).take_while(|c| *c != '\n').find(|c| !c.is_whitespace());
            let starts_new = wide || c == ';' || next.is_none_or(|n| n.is_ascii_digit() || (n.is_alphabetic() && !n.is_lowercase()) || matches!(n, '(' | '[' | '“' | '"' | '（' | '「' | '【'));
            let word = text[byte(start)..byte(at)].rsplit(|c: char| c.is_whitespace() || c == '(').next().unwrap_or("").trim_start_matches(['[', '“', '"']).to_lowercase();
            let abbreviation = c == '.' && (structure::ABBREVIATIONS.contains(&word.as_str()) || (word.chars().count() == 1 && word.chars().all(char::is_alphabetic)) || word.ends_with("et al"));
            if spaced && starts_new && !abbreviation {
                keep(byte(start), byte(end));
                (start, at) = (end, end);
                continue;
            }
        }
        at += 1;
    }
    keep(byte(start), text.len());
    out
}

/// Whether a sentence says enough to look for: a few words, and not a heading of the manuscript's.
pub fn worth(sentence: &str) -> bool {
    !sentence.trim_start().starts_with('#') && structure::words(sentence).len() >= 3
}

/// The marks a sentence ends with; a citation goes before them.
const ENDINGS: [char; 10] = ['。', '．', '.', '!', '?', '！', '？', ';', '；', '…'];

/// Whether `sentence` already cites `key`.
pub fn cites(sentence: &str, key: &str) -> bool {
    let wanted = format!("@{key}");
    sentence.match_indices(&wanted).any(|(at, _)| !sentence[at + wanted.len()..].chars().next().is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_'))
}

/// `sentence` citing `keys` as well: `[@A; @B]` before the mark it ends
/// with, joined to a citation that already stands there. Not a word of the
/// sentence changes, and a key it cites already is not written again.
pub fn place(sentence: &str, keys: &[&str]) -> String {
    let mut fresh: Vec<&str> = vec![];
    for key in keys {
        if !key.is_empty() && !cites(sentence, key) && !fresh.contains(key) { fresh.push(key) }
    }
    if fresh.is_empty() { return sentence.to_string() }
    let listed = fresh.iter().map(|key| format!("@{key}")).collect::<Vec<_>>().join("; ");
    let (body, tail) = sentence.split_at(sentence.trim_end_matches(ENDINGS).len());
    let words = body.trim_end();
    let gap = &body[words.len()..];
    // A citation at the sentence's end takes the new ones in.
    if words.ends_with(']') && words.rfind('[').is_some_and(|open| words[open..].starts_with("[@")) {
        return format!("{}; {listed}]{gap}{tail}", words[..words.len() - 1].trim_end());
    }
    // After a word a space; after Chinese or an opening bracket, none.
    let space = if words.chars().last().is_some_and(|c| c.is_ascii() && !c.is_whitespace() && !"([".contains(c)) { " " } else { "" };
    format!("{words}{space}[{listed}]{gap}{tail}")
}

/// `text` with citations added to some of its sentences: (where the
/// sentence stands, the keys it is to cite). Nothing else changes.
pub fn placed(text: &str, cited: &[(Range<usize>, Vec<String>)]) -> String {
    let mut order: Vec<&(Range<usize>, Vec<String>)> = cited.iter().filter(|(range, _)| text.get(range.clone()).is_some()).collect();
    order.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    let mut out = text.to_string();
    let mut before = text.len();
    for (range, keys) in order {
        // Sentences do not overlap: one that does is left alone.
        if range.end > before { continue }
        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
        out.replace_range(range.clone(), &place(&text[range.clone()], &keys));
        before = range.start;
    }
    out
}

/// The language most of the library's papers are written in, a key of
/// `structure::LANGUAGES`: what search words are to be in. English when
/// nothing tells.
pub fn library_language(papers: &[Paper]) -> &'static str {
    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    for paper in papers {
        let text = format!("{} {}", paper.title, paper.abstract_text);
        let told = structure::language(std::slice::from_ref(&text));
        // A title alone is too little to count words in: its script still tells Chinese from the rest.
        let language = if !told.is_empty() { told } else if text.chars().filter(|c| structure::is_cjk(*c)).count() >= 4 { "zh" } else { "en" };
        *counts.entry(language).or_default() += 1;
    }
    // Of two languages with as many papers, English, else the first by its key: the same answer every time.
    counts.into_iter().max_by_key(|(language, count)| (*count, *language == "en", std::cmp::Reverse(*language))).map_or("en", |(language, _)| language)
}

/// A language's name as the prompts say it: 英文.
pub fn language_name(key: &str) -> &'static str {
    structure::LANGUAGES.iter().find(|(code, _)| *code == key).map_or("英文", |(_, name)| *name)
}

/// What a model is told to turn each sentence into search words, in the
/// language the library's papers are written in: the instructions and the
/// numbered sentences.
pub fn words_prompt(sentences: &[&str], language: &str) -> (String, String) {
    let language = language_name(language);
    let system = format!(
        "你是文献检索助手。用户正在写学术论文，要在自己的文献库里给下面编号的句子找可以引用的文献。句子可能是任何语言；文献库里的文献主要是{language}的。\n\
         给每一句写一行{language}检索词：这句话说的对象、方法和发现，用该领域论文的题目和摘要里通行的术语，加上常见的同义说法和缩写，6 到 12 个词或短语，用空格分隔。不要写整句，不要写虚词。\n\
         格式是「编号: 检索词」，每句一行，编号照抄。只输出这些行，不要解释。"
    );
    let message = sentences.iter().enumerate().map(|(n, sentence)| format!("{}: {}", n + 1, sentence.split_whitespace().collect::<Vec<_>>().join(" "))).collect::<Vec<_>>().join("\n");
    (system, message)
}

/// What a sentence of a manuscript does, which decides what it can cite
/// (key, label): the consensus of a field, a finding of someone else's that
/// it tells, a method it names, a question it says is open, a result it
/// sets beside its own, or the paper's own words, which cite nothing.
pub const INTENTS: [(&str, &str); 6] = [("background", "背景"), ("finding", "他人发现"), ("method", "方法"), ("gap", "空白"), ("compare", "对比"), ("own", "本文")];

/// The intent that needs no citation: what the authors say of their own work.
pub const OWN: &str = "own";

/// An intent as a model wrote it, by key or by label; empty for anything else.
pub fn intent_of(said: &str) -> &'static str {
    let said = said.trim().trim_matches(['[', ']', '（', '）', '(', ')', '「', '」', '"', '*', ' ']).to_lowercase();
    INTENTS.iter().find(|(key, label)| said == *key || said == *label).map_or("", |(key, _)| *key)
}

/// What a model is told to say of each sentence before the library is
/// searched: what the sentence does (`INTENTS`) and the search words for
/// it, in the language the library's papers are written in.
pub fn plan_prompt(sentences: &[&str], language: &str) -> (String, String) {
    let language = language_name(language);
    let system = format!(
        "你是文献检索助手。用户正在写学术论文，要在自己的文献库里给下面编号的句子找可以引用的文献。句子可能是任何语言；文献库里的文献主要是{language}的。\n\
         对每一句做两件事。\n\
         一、判断这句话在做什么，从下面六种里选一种，写它的英文名：\n\
         background：领域的背景或共识，一项技术、一个方向的发展。\n\
         finding：转述前人的一项具体发现或结论。\n\
         method：说到某种方法、工具、数据或材料。\n\
         gap：指出尚未解决的问题、局限或争议。\n\
         compare：把本文的结果和前人的结果比较。\n\
         own：作者对本文工作的说明、过渡句，不需要引用。\n\
         二、写一行{language}检索词：这句话说的对象、方法和发现，用该领域论文的题目和摘要里通行的术语，加上常见的同义说法和缩写，6 到 12 个词或短语，用空格分隔。不要写整句，不要写虚词。own 的句子检索词写 -。\n\
         格式是「编号: 英文名 | 检索词」，每句一行，编号照抄。只输出这些行，不要解释。"
    );
    let message = sentences.iter().enumerate().map(|(n, sentence)| format!("{}: {}", n + 1, sentence.split_whitespace().collect::<Vec<_>>().join(" "))).collect::<Vec<_>>().join("\n");
    (system, message)
}

/// What a model made of a sentence before the search.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Planned {
    /// A key of `INTENTS`; empty when the model did not say.
    pub intent: String,
    /// The search words; empty when it gave none that fit the library's language.
    pub words: String,
}

/// A model's plan, one entry per sentence (`count` of them): lines of
/// `number: intent | words`. A line without an intent is its words alone.
pub fn parse_plan(reply: &str, count: usize, language: &str) -> Vec<Planned> {
    let mut out = vec![Planned::default(); count];
    for (n, rest) in numbered(reply, count) {
        let (intent, words) = match rest.split_once(['|', '｜']) {
            Some((said, words)) if !intent_of(said).is_empty() => (intent_of(said), words),
            _ => ("", rest),
        };
        let words = retrieval::clean(words);
        out[n] = Planned { intent: intent.to_string(), words: if words.chars().any(char::is_alphanumeric) && fits(language, &words) { words } else { String::new() } };
    }
    out
}

/// The numbered lines of a reply: (the sentence from 0, what follows its number).
fn numbered(reply: &str, count: usize) -> Vec<(usize, &str)> {
    let mut out = vec![];
    for line in reply.lines() {
        let line = line.trim().trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '-' | '*' | '•' | '#' | '[' | '(' | '（' | '句' | '第'));
        let digits = line.chars().take_while(char::is_ascii_digit).count();
        let Ok(n) = line[..digits].parse::<usize>() else { continue };
        if n == 0 || n > count { continue }
        out.push((n - 1, line[digits..].trim_start_matches(|c: char| c.is_whitespace() || matches!(c, ':' | '：' | '.' | '、' | ')' | '）' | ']' | '句' | '-' | '—' | '*'))));
    }
    out
}

/// Whether search words can find anything in papers written in `language`:
/// words in Chinese find nothing in a library of English papers (a model
/// may slip into the language of the sentence).
fn fits(language: &str, words: &str) -> bool {
    !matches!(language, "en" | "de" | "fr" | "es" | "pt" | "it") || !words.chars().any(structure::is_cjk)
}

/// The search words in a model's reply, one entry per sentence (`count`
/// of them), empty where it gave none: lines of `number: words`.
pub fn parse_words(reply: &str, count: usize, language: &str) -> Vec<String> {
    let mut out = vec![String::new(); count];
    for (n, rest) in numbered(reply, count) {
        let words = retrieval::clean(rest);
        if words.chars().any(char::is_alphanumeric) && fits(language, &words) { out[n] = words }
    }
    out
}

/// A paper found for a sentence.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    /// Its place in the papers searched.
    pub paper: usize,
    /// The sentence of its own that matches best, by its place in the sentences searched; none when only its record matched.
    pub sentence: Option<usize>,
    /// How many of the words searched for its record and that sentence hold, of how many.
    pub matched: usize,
    pub of: usize,
    /// The paper reads as a review (`reviews`).
    pub review: bool,
}

/// Which of the papers read as reviews, by their place: a full text with
/// parts and neither methods nor results among them (a review's own
/// headings are `other`), or a record that says so.
pub fn reviews(papers: &[Paper], sentences: &[PaperSentence]) -> Vec<bool> {
    let place_of: HashMap<&str, usize> = papers.iter().enumerate().map(|(n, p)| (p.id.as_str(), n)).collect();
    let (mut text, mut work) = (vec![false; papers.len()], vec![false; papers.len()]);
    for held in sentences {
        let Some(n) = place_of.get(held.paper_id.as_str()) else { continue };
        match held.sentence.section.as_str() {
            "abstract" => {}
            "methods" | "results" => work[*n] = true,
            _ => text[*n] = true,
        }
    }
    papers
        .iter()
        .enumerate()
        .map(|(n, paper)| {
            let (title, summary) = (paper.title.to_lowercase(), paper.abstract_text.to_lowercase());
            let said = title.split(|c: char| !c.is_alphanumeric()).any(|word| word == "review") || title.contains("综述") || ["this review", "we review", "here we review", "本文综述"].iter().any(|cue| summary.contains(cue));
            if text[n] { !work[n] } else { said }
        })
        .collect()
}

/// How far down a ranking of sentences a paper is still looked for.
const SENTENCES_DEEP: usize = 400;

/// How far down a ranking of records.
const RECORDS_DEEP: usize = 60;

/// The papers each sentence may cite, best first, at most `per` each.
/// `queries` holds, for every sentence, what to search with: the sentence
/// itself and any search words made of it (none: nothing is looked for).
/// Each query ranks the papers twice, by their record (title, abstract,
/// tags, notes) and by their best sentence among `sentences`, and the
/// rankings are merged; a paper is kept when its record and that sentence
/// hold a fair share of some query's words.
///
/// `intents` says what each sentence does (a key of `INTENTS`, empty when
/// not known), which decides what of a paper is shown for it: a finding,
/// a comparison or a method rests on what the paper says of its own, so a
/// sentence outside its introduction is named when one fits; background
/// puts the reviews first; the paper's own words are not searched for.
pub fn search(papers: &[Paper], sentences: &[PaperSentence], queries: &[Vec<String>], intents: &[&str], per: usize) -> Vec<Vec<Hit>> {
    let records: Vec<String> = papers.iter().map(|p| format!("{} {} {} {}", p.title, p.abstract_text, p.tags.join(" "), p.notes)).collect();
    let record_index = retrieval::index_for(&records.iter().map(String::as_str).collect::<Vec<_>>());
    let texts: Vec<&str> = sentences.iter().map(|s| s.sentence.text.as_str()).collect();
    let sentence_index = retrieval::index_for(&texts);
    let place_of: HashMap<&str, usize> = papers.iter().enumerate().map(|(n, p)| (p.id.as_str(), n)).collect();
    let owner: Vec<Option<usize>> = sentences.iter().map(|s| place_of.get(s.paper_id.as_str()).copied()).collect();
    let mut held: HashMap<usize, Vec<usize>> = HashMap::new();
    for (n, paper) in owner.iter().enumerate() {
        if let Some(paper) = paper { held.entry(*paper).or_default().push(n) }
    }
    let ids: Vec<&str> = papers.iter().map(|p| p.id.as_str()).collect();
    let review = reviews(papers, sentences);
    let unique = |text: &str| structure::words(text).into_iter().collect::<HashSet<String>>();
    queries
        .iter()
        .enumerate()
        .map(|(asked, queries)| {
            let intent = intents.get(asked).copied().unwrap_or("");
            let wanted: Vec<HashSet<String>> = queries.iter().map(|q| unique(q)).collect();
            if intent == OWN || wanted.iter().all(|words| words.len() < 3) { return vec![] }
            // What a paper found itself stands outside its introduction, which mostly tells what others found.
            let own_first = matches!(intent, "finding" | "compare" | "method");
            let stated = |n: usize| sentences[n].sentence.section != "introduction";
            let mut lists: Vec<Vec<(usize, f32)>> = vec![];
            // The best place any of a paper's sentences took, and which sentence.
            let mut best: HashMap<usize, (usize, usize)> = HashMap::new();
            for query in queries {
                lists.push(record_index.rank(query).into_iter().take(RECORDS_DEEP).collect());
                let mut by_sentence: Vec<(usize, f32)> = vec![];
                let mut seen = HashSet::new();
                for (place, (sentence, score)) in sentence_index.rank(query).into_iter().take(SENTENCES_DEEP).enumerate() {
                    let Some(paper) = owner[sentence] else { continue };
                    if seen.insert(paper) { by_sentence.push((paper, score)) }
                    let entry = best.entry(paper).or_insert((place, sentence));
                    if place < entry.0 { *entry = (place, sentence) }
                }
                lists.push(by_sentence);
            }
            let mut hits = vec![];
            for paper in retrieval::fuse(&lists, &ids, per * 4 + 8) {
                // The sentence of its own that says it: the best any query ranked, when it holds a fair share of the words searched for; else whichever of the paper's does. One that shares a word or two is no ground to cite on.
                let fair = |n: usize| wanted.iter().map(|words| (unique(texts[n]).intersection(words).count(), words.len())).filter(|(count, of)| *count >= 2 && count * 3 >= *of).map(|(count, _)| count).max().unwrap_or(0);
                let ranked = best.get(&paper).map(|(_, sentence)| *sentence).filter(|n| fair(*n) > 0);
                let any = |stated_only: bool| held.get(&paper)?.iter().filter(|n| !stated_only || stated(**n)).map(|n| (fair(*n), *n)).filter(|(count, _)| *count > 0).max_by_key(|(count, n)| (*count, std::cmp::Reverse(*n))).map(|(_, n)| n);
                let sentence = if own_first { ranked.filter(|n| stated(*n)).or_else(|| any(true)).or(ranked).or_else(|| any(false)) } else { ranked.or_else(|| any(false)) };
                let mut hay = unique(&records[paper]);
                if let Some(sentence) = sentence { hay.extend(unique(texts[sentence])) }
                // The query it answers best, by the share of its words found.
                let Some((matched, of)) = wanted.iter().filter(|words| !words.is_empty()).map(|words| (words.intersection(&hay).count(), words.len())).max_by(|a, b| (a.0 * b.1).cmp(&(b.0 * a.1))) else { continue };
                if matched < 2 || matched * 3 < of { continue }
                hits.push(Hit { paper, sentence, matched, of, review: review[paper] });
                // A sentence about a field looks through all that was found for the reviews of it.
                if hits.len() == per && intent != "background" { break }
            }
            if intent == "background" { hits.sort_by_key(|hit| !hit.review) }
            hits.truncate(per);
            hits
        })
        .collect()
}

/// A work a sentence names in running text by author and year, as papers
/// do when they tell what others found: `(Tang et al., 2009)`.
#[derive(Clone, Debug, PartialEq)]
pub struct Named {
    /// The first author's family name.
    pub family: String,
    pub year: i32,
    /// As it is said again: `Tang et al., 2009`.
    pub text: String,
}

/// Words that stand before a year without being an author.
const NOT_NAMES: [&str; 28] = ["In", "Since", "Until", "By", "From", "During", "After", "Before", "Around", "Of", "Year", "Fig", "Figure", "Table", "The", "And", "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// The works a sentence names by author and year: `Tang et al. (2009)`,
/// `(Efroni and Birnbaum, 2016; Shahan et al., 2022)`, `(Sharma, 2003)`.
/// Numbered citations name nothing: the reader leaves raised numbers out.
pub fn named(sentence: &str) -> Vec<Named> {
    let chars: Vec<char> = sentence.chars().collect();
    let name = |word: &str| -> Option<String> {
        let word = word.trim_start_matches(['(', '[', ';', '（']).trim_end_matches([',', '.', '，']);
        let fits = word.chars().count() >= 2 && word.chars().next().is_some_and(char::is_uppercase) && word.chars().all(|c| c.is_alphabetic() || matches!(c, '-' | '\'' | '’')) && !NOT_NAMES.contains(&word);
        fits.then(|| word.to_string())
    };
    let mut out: Vec<Named> = vec![];
    for at in 0..chars.len().saturating_sub(3) {
        let digits = |from: usize, count: usize| chars.get(from..from + count).is_some_and(|run| run.iter().all(char::is_ascii_digit));
        if !digits(at, 4) || (at > 0 && chars[at - 1].is_ascii_digit()) || digits(at + 4, 1) { continue }
        let year: i32 = chars[at..at + 4].iter().collect::<String>().parse().unwrap_or(0);
        if !(1900..=2099).contains(&year) { continue }
        let before: String = chars[..at].iter().collect();
        let before = before.trim_end();
        // A comma or an opening bracket between the name and the year: `Sharma, 2003`, `Sharma (2003)`.
        let bare = before.trim_end_matches(['(', ',', '（', '，']).trim_end();
        let parted = bare.len() < before.len();
        let words: Vec<&str> = bare.split_whitespace().collect();
        let pair = match words.as_slice() {
            [.., first, and, second] if matches!(*and, "and" | "&") => name(first).zip(name(second)),
            _ => None,
        };
        let found = match words.as_slice() {
            [.., first, "et", al] if al.trim_end_matches([',', '.']) == "al" => name(first).map(|family| (family.clone(), format!("{family} et al., {year}"))),
            _ => pair.map(|(family, second)| (family.clone(), format!("{family} and {second}, {year}"))).or_else(|| words.last().filter(|_| parted).and_then(|last| name(last)).map(|family| (family.clone(), format!("{family}, {year}")))),
        };
        let Some((family, text)) = found else { continue };
        if !out.iter().any(|known| known.family == family && known.year == year) { out.push(Named { family, year, text }) }
    }
    out
}

/// The paper of the library a named work is, by its place: the one of
/// that year whose first author has that family name. None when the
/// library has no such paper, or more than one.
pub fn holds(papers: &[Paper], named: &Named) -> Option<usize> {
    let mut same = papers.iter().enumerate().filter(|(_, paper)| paper.year == Some(named.year) && crate::model::author_names(&paper.authors).first().is_some_and(|(family, _)| family.eq_ignore_ascii_case(&named.family)));
    let first = same.next()?;
    same.next().is_none().then_some(first.0)
}

/// A paper offered for a sentence, as the model that judges it is told.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Offered {
    /// The paper's citation key, as the manuscript writes it: `Zhang2024`.
    pub key: String,
    pub title: String,
    pub year: Option<i64>,
    /// Which part of the paper the sentence is from, as a label (摘要, 结果); empty without a sentence.
    pub part: String,
    /// The sentence of the paper that matched.
    pub evidence: String,
    /// The paper reads as a review.
    pub review: bool,
}

/// A sentence of the manuscript with the papers found for it, as the model that judges them is told.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Asked {
    pub sentence: String,
    /// What the sentence does, a key of `INTENTS`; empty when not known.
    pub intent: String,
    pub offered: Vec<Offered>,
}

/// What a model is told to judge which of the papers found bear each
/// sentence out: the instructions and the message. What a sentence can
/// cite goes by what it does, and a paper that only tells what others
/// found is not chosen but named as restating it. `reply_in` is the
/// language its reasons are to be in.
pub fn judge_prompt(sentences: &[Asked], reply_in: &str) -> Result<(String, String), String> {
    if sentences.iter().all(|asked| asked.offered.is_empty()) { return Err("文献库里没有找到和这段文字相近的文献。".into()) }
    let system = "你是严谨的学术写作助手，帮用户判断他论文稿件里的每句话可以引用文献库里的哪些文献。只能从给出的候选里选，不要编造文献。只输出 JSON，不加解释，不用 Markdown。".to_string();
    let listed: Vec<String> = sentences
        .iter()
        .enumerate()
        .map(|(n, asked)| {
            let candidates: Vec<String> = asked
                .offered
                .iter()
                .map(|c| {
                    let about: Vec<String> = c.year.map(|year| year.to_string()).into_iter().chain(c.review.then(|| "综述".to_string())).collect();
                    let about = if about.is_empty() { String::new() } else { format!("（{}）", about.join("，")) };
                    let evidence = c.evidence.split_whitespace().collect::<Vec<_>>().join(" ");
                    if evidence.is_empty() { format!("  - [{}] {}{about}", c.key, c.title.trim()) } else { format!("  - [{}] {}{about}｜{}里的原句：{evidence}", c.key, c.title.trim(), if c.part.is_empty() { "文献" } else { c.part.as_str() }) }
                })
                .collect();
            let intent = if asked.intent.is_empty() { String::new() } else { format!("（{}）", crate::model::label(&INTENTS, &asked.intent)) };
            format!("句 {}{intent}：{}\n{}", n + 1, asked.sentence.split_whitespace().collect::<Vec<_>>().join(" "), if candidates.is_empty() { "  （没有候选）".to_string() } else { candidates.join("\n") })
        })
        .collect();
    let message = format!(
        "下面是稿件里的一段话，按句编号，编号后的括号里是这句话在做什么。每句下面是我在文献库里按用词找到的候选文献（方括号里是它的 Key，后面是题目和它自己的一句原文，注明「综述」的是综述类文献）。用词相近不等于内容相符，请逐句判断哪些候选确实可以作为这句话的引用。\n\n\
         一句话该引什么，看它在做什么：\n\
         - 背景：领域的共识，一项技术或一个方向的发展。引以它为主题的综述，或公认的代表性研究；看题目，主题要一致。只是用这项技术研究了别的问题、顺带提到同样的词的不算。\n\
         - 他人发现：转述前人的一项具体发现或结论。引做出这个发现的原始研究（它的题目、摘要、结果或讨论报道了这件事），或者以这个问题为主题的综述。\n\
         - 方法：说到某种方法、工具、数据或材料。引提出或系统描述它的文献。\n\
         - 空白：指出尚未解决的问题或争议。引指出这一点的综述或研究，没有贴切的就不配。\n\
         - 对比：把本文的结果和前人的比较。引报道那个结果的原始研究。\n\
         题目和原句一起看：题目表明这篇文献研究的就是这件事，原句不必逐字对上。\n\
         - 本文：作者对本文工作的说明、过渡句。不配引用。\n\
         没有注明的句子，先自己判断它属于哪一种。\n\n\
         转述不是出处。候选的原句如果只是在转述别人的工作（多见于导言，常带着 (Tang et al., 2009) 这样的引用），而这篇文献自己既不是这项发现的出处，也不是讲这个主题的综述，就不要把它放进 cite，而是把它的 Key 放进 restates：它说明这句话的出处是它所引的文献。原句和这句话说的不是一回事的候选，两处都不放。\n\n\
         一句最多配 3 篇，最贴切的排在前面。宁可少配，不要错配。\n\n\
         输出一个 JSON 数组，有可引用的文献或有转述的句子各占一项：\n\
         [{{\"sentence\": 句子编号, \"cite\": [{{\"key\": \"候选的 Key\", \"role\": \"source 或 review\", \"why\": \"一句话说明这篇文献为什么支持这句话\"}}], \"restates\": [\"只是转述了这句话内容的候选的 Key\"]}}]\n\
         role：source 是做出这项发现、提出这个方法的原始研究，review 是综述。why 用{reply_in}写，不超过 40 个字。一句都没有时输出 []。\n\n{}",
        listed.join("\n")
    );
    Ok((system, message))
}

/// The paper made the finding or the method itself.
pub const SOURCE: &str = "source";
/// The paper is a review of what the sentence is about.
pub const REVIEW: &str = "review";
/// The paper only tells what others found: not a citation, a lead to the works it names.
pub const RESTATES: &str = "restates";

/// What the model made of a paper offered for a sentence.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pick {
    /// The sentence, from 0.
    pub sentence: usize,
    pub key: String,
    pub why: String,
    /// `SOURCE` or `REVIEW` for a paper it chose (empty when it did not say which), `RESTATES` for one it did not choose because it only tells what others found.
    pub role: String,
}

impl Pick {
    /// Whether the paper was chosen to be cited.
    pub fn chosen(&self) -> bool {
        self.role != RESTATES
    }
}

/// What a model made of the papers offered, in its order: those it chose
/// and those it says only restate. A key is taken as written, without the
/// brackets or the `@` a model may put around it; whether it is one of
/// the papers offered is the caller's to check.
pub fn parse_judgement(reply: &str) -> Result<Vec<Pick>, String> {
    let wrong = "模型的回答不是要求的格式。";
    let (start, end) = reply.find('[').zip(reply.rfind(']')).filter(|(start, end)| start < end).ok_or(wrong)?;
    let list: Vec<Value> = serde_json::from_str(&reply[start..=end]).map_err(|_| wrong)?;
    let number = |value: &Value| value.as_u64().or_else(|| value.as_str().and_then(|text| text.chars().filter(char::is_ascii_digit).collect::<String>().parse().ok()));
    let bare = |key: &str| key.trim().trim_matches(['[', ']', '@', ' ']).to_string();
    let mut picks: Vec<Pick> = vec![];
    for item in &list {
        let Some(sentence) = ["sentence", "句", "n"].iter().find_map(|name| number(&item[*name])).filter(|n| *n > 0) else { continue };
        let sentence = sentence as usize - 1;
        let listed = |names: &[&str]| names.iter().find_map(|name| item[*name].as_array()).cloned().unwrap_or_default();
        let mut take = |key: String, why: String, role: String| {
            if !key.is_empty() && !picks.iter().any(|pick| pick.sentence == sentence && pick.key == key) { picks.push(Pick { sentence, key, why, role }) }
        };
        for one in &listed(&["cite", "cites", "citations", "keys"]) {
            match one {
                Value::String(key) => take(bare(key), String::new(), String::new()),
                other => {
                    let role = other["role"].as_str().unwrap_or("").trim().to_lowercase();
                    take(bare(other["key"].as_str().unwrap_or("")), other["why"].as_str().unwrap_or("").trim().to_string(), if role == SOURCE || role == REVIEW { role } else { String::new() });
                }
            }
        }
        for one in &listed(&["restates", "restate", "restated"]) {
            take(bare(one.as_str().or_else(|| one["key"].as_str()).unwrap_or("")), String::new(), RESTATES.to_string());
        }
    }
    Ok(picks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structure::Sentence;

    fn cut(text: &str) -> Vec<&str> {
        sentences(text).into_iter().map(|range| &text[range]).collect()
    }

    #[test]
    fn sentences_and_where_they_stand() {
        assert_eq!(cut("Roots grow (p < 0.05). 根尖有干细胞。WOX5 is key;\nDone"), ["Roots grow (p < 0.05).", "根尖有干细胞。", "WOX5 is key;", "Done"]);
        // An abbreviation, a number and what stands in brackets end nothing.
        assert_eq!(cut("As Smith et al. showed in Fig. 2, growth fell by 0.5 mm [@A; @B]. It \"stopped.\" Then?"), ["As Smith et al. showed in Fig. 2, growth fell by 0.5 mm [@A; @B].", "It \"stopped.\"", "Then?"]);
        assert_eq!(cut("细胞分离（见图 1；n = 3）很快；测序更快。\n\n  第二段  "), ["细胞分离（见图 1；n = 3）很快；", "测序更快。", "第二段"]);
        assert!(cut("  \n ").is_empty());
        // The ranges are those of the text itself.
        let text = "一句。  Two words here. ";
        assert_eq!(sentences(text), [0..9, 11..26]);
        assert!(worth("Roots sense soil compaction.") && worth("单细胞测序") && !worth("Too short.") && !worth("# Results of the root atlas"));
    }

    #[test]
    fn citations_placed() {
        assert_eq!(place("Roots sense compaction.", &["Zhang2021"]), "Roots sense compaction [@Zhang2021].");
        assert_eq!(place("单细胞测序成为重要手段。", &["A2019", "B2020"]), "单细胞测序成为重要手段[@A2019; @B2020]。");
        // A citation already at the end takes the new one in; a key cited already is not written twice.
        assert_eq!(place("Roots sense compaction [@Zhang2021].", &["Lee2020", "Zhang2021"]), "Roots sense compaction [@Zhang2021; @Lee2020].");
        assert_eq!(place("Roots [@Zhang2021] sense compaction", &["Zhang2021"]), "Roots [@Zhang2021] sense compaction");
        assert_eq!(place("Roots [@Zhang2021] sense compaction", &["Zhang2021b"]), "Roots [@Zhang2021] sense compaction [@Zhang2021b]");
        assert_eq!(place("用 scRNA-seq 测序；", &["A2019"]), "用 scRNA-seq 测序[@A2019]；");
        assert_eq!(place("He called it \"new.\"", &["A2019"]), "He called it \"new.\" [@A2019]");
        assert!(cites("x [@A2019, p. 5]", "A2019") && !cites("x [@A2019b]", "A2019"));
        // In a text: only the sentences named change, wherever they stand.
        let text = "第一句。Second one. 第三句！";
        let ranges = sentences(text);
        assert_eq!(placed(text, &[(ranges[2].clone(), vec!["C".into()]), (ranges[0].clone(), vec!["A".into(), "B".into()])]), "第一句[@A; @B]。Second one. 第三句[@C]！");
        assert_eq!(placed(text, &[(0..999, vec!["A".into()])]), text);
    }

    #[test]
    fn search_words_of_a_reply() {
        let (system, message) = words_prompt(&["单细胞测序\n成为重要手段。", "We extend this."], "en");
        assert!(system.contains("文献主要是英文的") && message == "1: 单细胞测序 成为重要手段。\n2: We extend this.");
        let reply = "好的：\n1: single-cell RNA sequencing, scRNA-seq; cell heterogeneity\n- **2**：-\n句 3. spatial transcriptomics multi-omics\n4: 空间 转录组\n9: beyond the count";
        assert_eq!(parse_words(reply, 4, "en"), ["single-cell RNA sequencing scRNA-seq cell heterogeneity", "", "spatial transcriptomics multi-omics", ""]);
        // A library of Chinese papers takes Chinese words.
        assert_eq!(parse_words("1: 空间 转录组", 1, "zh"), ["空间 转录组"]);
        assert!(parse_words("Here are 2 lines", 2, "en").iter().all(String::is_empty));
    }

    fn paper(id: &str, title: &str, abstract_text: &str) -> Paper {
        Paper { id: id.into(), title: title.into(), abstract_text: abstract_text.into(), ..Default::default() }
    }

    fn sentence(paper: &str, section: &str, text: &str) -> PaperSentence {
        PaperSentence { paper_id: paper.into(), sentence: Sentence { section: section.into(), text: text.into(), ..Default::default() } }
    }

    #[test]
    fn papers_found_for_sentences() {
        let papers = [
            paper("a", "A single-cell RNA sequencing atlas of the Arabidopsis root", "We profiled root cells and resolved the trajectories of cell types."),
            paper("b", "Soil compaction and root growth", "Compacted soil slows the growth of roots through ethylene."),
            paper("c", "Spatial transcriptomics of maize leaves", "Spatially resolved expression across the leaf."),
        ];
        let held = [
            sentence("a", "results", "Protoplasts were isolated and libraries were prepared with the Chromium platform."),
            sentence("a", "abstract", "We profiled root cells and resolved the trajectories of cell types."),
            sentence("b", "results", "Ethylene accumulates around roots in compacted soil and restricts their elongation."),
            sentence("c", "abstract", "Spatially resolved expression across the leaf."),
        ];
        let found = search(&papers, &held, &[
            vec!["Libraries were prepared with the Chromium platform after protoplast isolation.".into()],
            // Written in Chinese: only the search words made of it find anything.
            vec!["土壤紧实通过乙烯抑制根的伸长。".into(), "soil compaction ethylene root elongation restricted".into()],
            vec!["Unrelated sentence about volcanic eruptions in Iceland.".into()],
            vec!["Too short.".into()],
            vec![],
        ], &[], 3);
        // Found by a sentence of the full text, which is named.
        assert_eq!((found[0][0].paper, found[0][0].sentence), (0, Some(0)));
        assert_eq!((found[1][0].paper, found[1][0].sentence), (1, Some(2)));
        assert!(found[1].iter().all(|hit| hit.paper != 2));
        assert!(found[2].is_empty() && found[3].is_empty() && found[4].is_empty());
        // Without the search words the Chinese sentence finds nothing.
        assert!(search(&papers, &held, &[vec!["土壤紧实通过乙烯抑制根的伸长。".into()]], &[], 3)[0].is_empty());
        // A paper found by its record alone still names the sentence of its own that is closest.
        let by_record = search(&papers, &held[..1], &[vec!["Compacted soil slows root growth through ethylene.".into()]], &[], 3);
        assert_eq!((by_record[0][0].paper, by_record[0][0].sentence), (1, None));
        assert_eq!(library_language(&papers), "en");
        assert_eq!(library_language(&[paper("z", "单细胞转录组图谱", "")]), "zh");
    }

    #[test]
    fn what_a_sentence_does_decides_what_is_shown() {
        let papers = [
            paper("a", "Ethylene restricts root elongation in compacted soil", "We show that ethylene accumulates around roots in compacted soil."),
            paper("b", "A root cell atlas", "Single cells of the root were profiled."),
            paper("c", "Soil compaction and roots: a review", "This review covers how roots respond to compacted soil and ethylene."),
        ];
        let held = [
            sentence("a", "abstract", "We show that ethylene accumulates around roots in compacted soil."),
            sentence("a", "introduction", "Compacted soil restricts root elongation through ethylene, as earlier work on soil compaction suggested."),
            sentence("a", "results", "Ethylene accumulated around roots grown in compacted soil and restricted their elongation."),
            sentence("b", "introduction", "Ethylene restricts root elongation in compacted soil (Pandey et al., 2021)."),
            sentence("b", "results", "Root cells were profiled with the Chromium platform."),
            sentence("c", "other", "Roots in compacted soil accumulate ethylene, which restricts elongation."),
        ];
        assert_eq!(reviews(&papers, &held), [false, false, true]);
        let query = vec![vec!["Ethylene restricts root elongation in compacted soil through its accumulation.".to_string()]];
        let section = |hits: &[Hit], paper: usize| hits.iter().find(|hit| hit.paper == paper).and_then(|hit| hit.sentence).map(|n| held[n].sentence.section.as_str());
        // Not known what the sentence does: each paper's closest sentence, wherever it stands.
        let plain = search(&papers, &held, &query, &[], 3);
        assert_eq!(section(&plain[0], 1), Some("introduction"));
        // A finding rests on what a paper says of its own; one that only tells it in its introduction still shows that sentence.
        let finding = search(&papers, &held, &query, &["finding"], 3);
        assert!(matches!(section(&finding[0], 0), Some("results" | "abstract")));
        assert_eq!(section(&finding[0], 1), Some("introduction"));
        // Background puts the review first; the paper's own words are not searched for.
        let background = search(&papers, &held, &query, &["background"], 3);
        assert!((background[0][0].paper, background[0][0].review) == (2, true) && background[0].len() == 3);
        assert!(search(&papers, &held, &query, &[OWN], 3)[0].is_empty());
    }

    #[test]
    fn the_plan_of_a_reply() {
        let (system, message) = plan_prompt(&["单细胞测序\n成为重要手段。", "We extend this."], "en");
        assert!(system.contains("文献主要是英文的") && system.contains("编号: 英文名 | 检索词") && message == "1: 单细胞测序 成为重要手段。\n2: We extend this.");
        let reply = "1: background | single-cell RNA sequencing, scRNA-seq\n2: own | -\n- **3**：Finding｜root atlas trajectories cell types\n4: 他人发现 | 根 图谱\n5: quiescent centre WOX5 stem cell niche\n9: gap | beyond the count";
        let plan = parse_plan(reply, 5, "en");
        let said: Vec<(&str, &str)> = plan.iter().map(|planned| (planned.intent.as_str(), planned.words.as_str())).collect();
        // Words in another script than the library's find nothing and are left out; a line without an intent is its words.
        assert_eq!(said, [("background", "single-cell RNA sequencing scRNA-seq"), ("own", ""), ("finding", "root atlas trajectories cell types"), ("finding", ""), ("", "quiescent centre WOX5 stem cell niche")]);
        assert_eq!((intent_of(" 背景 "), intent_of("[Method]"), intent_of("review")), ("background", "method", ""));
    }

    #[test]
    fn works_named_in_a_sentence() {
        let texts = |sentence: &str| named(sentence).into_iter().map(|named| named.text).collect::<Vec<_>>();
        assert_eq!(texts("Root growth relies on stem cells (Sharma et al., 2003; Efroni and Birnbaum, 2016)."), ["Sharma et al., 2003", "Efroni and Birnbaum, 2016"]);
        assert_eq!(texts("Tang et al. (2009) sequenced the transcriptome of a single cell, and Zhang (2019a) followed."), ["Tang et al., 2009", "Zhang, 2019"]);
        assert_eq!(texts("As shown before (Qinghao-Kanglue-Hezuo-Yanjiu-Zu, 1977; Tang et al., 2009, 2011)."), ["Qinghao-Kanglue-Hezuo-Yanjiu-Zu, 1977", "Tang et al., 2009"]);
        // A year is not a citation by itself, and numbers are not years.
        assert!(texts("In 2009, 12000 cells were profiled in March 2010 (Table 2021).").is_empty());
        let papers = [Paper { id: "a".into(), authors: "Tang, Fuchou; Lao, Kaiqin".into(), year: Some(2009), ..Default::default() }, Paper { id: "b".into(), authors: "Lin Zhang".into(), year: Some(2019), ..Default::default() }, Paper { id: "c".into(), authors: "Zhang, Tian-Qi".into(), year: Some(2019), ..Default::default() }];
        let all = named("Tang et al. (2009) and Zhang et al. (2019) and Lee et al. (2020).");
        // The paper of that author and year; none when two could be meant or the library has none.
        assert_eq!(all.iter().map(|named| holds(&papers, named)).collect::<Vec<_>>(), [Some(0), None, None]);
    }

    #[test]
    fn judging() {
        let offered = vec![
            Asked { sentence: "Roots sense\ncompaction.".into(), intent: "finding".into(), offered: vec![Offered { key: "Zhang2021".into(), title: "Root atlas".into(), year: Some(2021), part: "结果".into(), evidence: "roots  sense\nsoil compaction".into(), review: false }, Offered { key: "Lee2020".into(), title: "No sentence".into(), review: true, ..Default::default() }] },
            Asked { sentence: "We extend this.".into(), ..Default::default() },
        ];
        let (system, message) = judge_prompt(&offered, "中文").unwrap();
        assert!(system.contains("只能从给出的候选里选"));
        assert!(message.contains("句 1（他人发现）：Roots sense compaction.\n  - [Zhang2021] Root atlas（2021）｜结果里的原句：roots sense soil compaction\n  - [Lee2020] No sentence（综述）\n句 2：We extend this.\n  （没有候选）"));
        assert!(message.contains("why 用中文写") && message.contains("转述不是出处"));
        assert!(judge_prompt(&offered[1..], "中文").is_err());

        let reply = "```json\n[{\"sentence\": 1, \"cite\": [{\"key\": \"[@Zhang2021]\", \"role\": \"Source\", \"why\": \"报道了根对紧实土壤的感知\"}, {\"key\": \"Zhang2021\"}], \"restates\": [\"Lee2020\", \"Zhang2021\"]}, {\"sentence\": \"句 2\", \"keys\": [\"Lee2020\"]}, {\"sentence\": 0, \"cite\": [\"X\"]}]\n```";
        let pick = |sentence: usize, key: &str, why: &str, role: &str| Pick { sentence, key: key.into(), why: why.into(), role: role.into() };
        // A paper chosen is not also one that restates; a role the model made up is left out.
        assert_eq!(parse_judgement(reply).unwrap(), [pick(0, "Zhang2021", "报道了根对紧实土壤的感知", SOURCE), pick(0, "Lee2020", "", RESTATES), pick(1, "Lee2020", "", "")]);
        assert!(parse_judgement(reply).unwrap()[0].chosen() && !parse_judgement(reply).unwrap()[1].chosen());
        assert!(parse_judgement("[]").unwrap().is_empty());
        assert!(parse_judgement("没有可配的。").is_err());
    }
}
