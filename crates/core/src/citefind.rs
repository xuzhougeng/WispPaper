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
        let wide = matches!(c, '。' | '！' | '？' | '；') || (c == '．' && !between_digits);
        if depth == 0 && (wide || matches!(c, '.' | '!' | '?' | ';')) {
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
    counts.into_iter().max_by_key(|(language, count)| (*count, *language == "en")).map_or("en", |(language, _)| language)
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
    for line in reply.lines() {
        let line = line.trim().trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '-' | '*' | '•' | '#' | '[' | '(' | '（' | '句' | '第'));
        let digits = line.chars().take_while(char::is_ascii_digit).count();
        let Ok(n) = line[..digits].parse::<usize>() else { continue };
        if n == 0 || n > count { continue }
        let words = retrieval::clean(line[digits..].trim_start_matches(|c: char| c.is_whitespace() || matches!(c, ':' | '：' | '.' | '、' | ')' | '）' | ']' | '句' | '-' | '—')));
        if words.chars().any(char::is_alphanumeric) && fits(language, &words) { out[n - 1] = words }
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
pub fn search(papers: &[Paper], sentences: &[PaperSentence], queries: &[Vec<String>], per: usize) -> Vec<Vec<Hit>> {
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
    let unique = |text: &str| structure::words(text).into_iter().collect::<HashSet<String>>();
    queries
        .iter()
        .map(|queries| {
            let wanted: Vec<HashSet<String>> = queries.iter().map(|q| unique(q)).collect();
            if wanted.iter().all(|words| words.len() < 3) { return vec![] }
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
                let sentence = best.get(&paper).map(|(_, sentence)| *sentence).filter(|n| fair(*n) > 0).or_else(|| held.get(&paper)?.iter().map(|n| (fair(*n), *n)).filter(|(count, _)| *count > 0).max_by_key(|(count, n)| (*count, std::cmp::Reverse(*n))).map(|(_, n)| n));
                let mut hay = unique(&records[paper]);
                if let Some(sentence) = sentence { hay.extend(unique(texts[sentence])) }
                // The query it answers best, by the share of its words found.
                let Some((matched, of)) = wanted.iter().filter(|words| !words.is_empty()).map(|words| (words.intersection(&hay).count(), words.len())).max_by(|a, b| (a.0 * b.1).cmp(&(b.0 * a.1))) else { continue };
                if matched < 2 || matched * 3 < of { continue }
                hits.push(Hit { paper, sentence, matched, of });
                if hits.len() == per { break }
            }
            hits
        })
        .collect()
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
}

/// What a model is told to judge which of the papers found bear each
/// sentence out: the instructions and the message. `reply_in` is the
/// language its reasons are to be in.
pub fn judge_prompt(sentences: &[(String, Vec<Offered>)], reply_in: &str) -> Result<(String, String), String> {
    if sentences.iter().all(|(_, offered)| offered.is_empty()) { return Err("文献库里没有找到和这段文字相近的文献。".into()) }
    let system = "你是严谨的学术写作助手，帮用户判断他论文稿件里的每句话可以引用文献库里的哪些文献。只能从给出的候选里选，不要编造文献。只输出 JSON，不加解释，不用 Markdown。".to_string();
    let listed: Vec<String> = sentences
        .iter()
        .enumerate()
        .map(|(n, (sentence, offered))| {
            let candidates: Vec<String> = offered
                .iter()
                .map(|c| {
                    let year = c.year.map(|year| format!("（{year}）")).unwrap_or_default();
                    let evidence = c.evidence.split_whitespace().collect::<Vec<_>>().join(" ");
                    if evidence.is_empty() { format!("  - [{}] {}{year}", c.key, c.title.trim()) } else { format!("  - [{}] {}{year}｜{}里的原句：{evidence}", c.key, c.title.trim(), if c.part.is_empty() { "文献" } else { c.part.as_str() }) }
                })
                .collect();
            format!("句 {}：{}\n{}", n + 1, sentence.split_whitespace().collect::<Vec<_>>().join(" "), if candidates.is_empty() { "  （没有候选）".to_string() } else { candidates.join("\n") })
        })
        .collect();
    let message = format!(
        "下面是稿件里的一段话，按句编号，每句下面是我在文献库里按用词找到的候选文献（方括号里是它的 Key，后面是题目和它自己的一句原文）。用词相近不等于内容相符，请逐句判断哪些候选确实可以作为这句话的引用。\n\n\
         标准：\n\
         1. 候选的题目或原句表明这篇文献研究、报道或综述了这句话所说的内容，才可以引用；只是领域相同、用词相似的不算。\n\
         2. 先看题目：文献的主题要和这句话的主题一致。句子讲的是一项技术或一个领域的发展，而候选只是用这项技术研究了别的问题、在摘要里顺带提到同样的词，就不能引用。\n\
         3. 概括性的句子（一项技术的发展、一个领域的共识）引用以它为主题的综述或有代表性的研究。一句最多 3 篇，最贴切的排在前面。\n\
         4. 作者对本文工作的说明、过渡句和没有合适候选的句子不配引用。宁可少配，不要错配。\n\
         5. 原句出自候选文献的导言、只是在转述别人工作的，不如它自己的发现可靠，拿不准就不选。\n\n\
         输出一个 JSON 数组，可以配引用的句子各占一项：\n\
         [{{\"sentence\": 句子编号, \"cite\": [{{\"key\": \"候选的 Key\", \"why\": \"一句话说明这篇文献为什么支持这句话\"}}]}}]\n\
         why 用{reply_in}写，不超过 40 个字。没有一句可配时输出 []。\n\n{}",
        listed.join("\n")
    );
    Ok((system, message))
}

/// A paper the model chose for a sentence.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pick {
    /// The sentence, from 0.
    pub sentence: usize,
    pub key: String,
    pub why: String,
}

/// The papers a model chose, in its order. A key is taken as written,
/// without the brackets or the `@` a model may put around it; whether it
/// is one of the papers offered is the caller's to check.
pub fn parse_judgement(reply: &str) -> Result<Vec<Pick>, String> {
    let wrong = "模型的回答不是要求的格式。";
    let (start, end) = reply.find('[').zip(reply.rfind(']')).filter(|(start, end)| start < end).ok_or(wrong)?;
    let list: Vec<Value> = serde_json::from_str(&reply[start..=end]).map_err(|_| wrong)?;
    let number = |value: &Value| value.as_u64().or_else(|| value.as_str().and_then(|text| text.chars().filter(char::is_ascii_digit).collect::<String>().parse().ok()));
    let bare = |key: &str| key.trim().trim_matches(['[', ']', '@', ' ']).to_string();
    let mut picks = vec![];
    for item in &list {
        let Some(sentence) = ["sentence", "句", "n"].iter().find_map(|name| number(&item[*name])).filter(|n| *n > 0) else { continue };
        let cited = ["cite", "cites", "citations", "keys"].iter().find_map(|name| item[*name].as_array()).cloned().unwrap_or_default();
        for one in &cited {
            let (key, why) = match one {
                Value::String(key) => (bare(key), String::new()),
                other => (bare(other["key"].as_str().unwrap_or("")), other["why"].as_str().unwrap_or("").trim().to_string()),
            };
            if !key.is_empty() && !picks.iter().any(|pick: &Pick| pick.sentence == sentence as usize - 1 && pick.key == key) { picks.push(Pick { sentence: sentence as usize - 1, key, why }) }
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
        ], 3);
        // Found by a sentence of the full text, which is named.
        assert_eq!((found[0][0].paper, found[0][0].sentence), (0, Some(0)));
        assert_eq!((found[1][0].paper, found[1][0].sentence), (1, Some(2)));
        assert!(found[1].iter().all(|hit| hit.paper != 2));
        assert!(found[2].is_empty() && found[3].is_empty() && found[4].is_empty());
        // Without the search words the Chinese sentence finds nothing.
        assert!(search(&papers, &held, &[vec!["土壤紧实通过乙烯抑制根的伸长。".into()]], 3)[0].is_empty());
        // A paper found by its record alone still names the sentence of its own that is closest.
        let by_record = search(&papers, &held[..1], &[vec!["Compacted soil slows root growth through ethylene.".into()]], 3);
        assert_eq!((by_record[0][0].paper, by_record[0][0].sentence), (1, None));
        assert_eq!(library_language(&papers), "en");
        assert_eq!(library_language(&[paper("z", "单细胞转录组图谱", "")]), "zh");
    }

    #[test]
    fn judging() {
        let offered = vec![
            ("Roots sense\ncompaction.".to_string(), vec![Offered { key: "Zhang2021".into(), title: "Root atlas".into(), year: Some(2021), part: "结果".into(), evidence: "roots  sense\nsoil compaction".into() }, Offered { key: "Lee2020".into(), title: "No sentence".into(), ..Default::default() }]),
            ("We extend this.".to_string(), vec![]),
        ];
        let (system, message) = judge_prompt(&offered, "中文").unwrap();
        assert!(system.contains("只能从给出的候选里选"));
        assert!(message.contains("句 1：Roots sense compaction.\n  - [Zhang2021] Root atlas（2021）｜结果里的原句：roots sense soil compaction\n  - [Lee2020] No sentence\n句 2：We extend this.\n  （没有候选）"));
        assert!(message.contains("why 用中文写"));
        assert!(judge_prompt(&offered[1..], "中文").is_err());

        let reply = "```json\n[{\"sentence\": 1, \"cite\": [{\"key\": \"[@Zhang2021]\", \"why\": \"报道了根对紧实土壤的感知\"}, {\"key\": \"Zhang2021\"}]}, {\"sentence\": \"句 2\", \"keys\": [\"Lee2020\"]}, {\"sentence\": 0, \"cite\": [\"X\"]}]\n```";
        assert_eq!(parse_judgement(reply).unwrap(), [Pick { sentence: 0, key: "Zhang2021".into(), why: "报道了根对紧实土壤的感知".into() }, Pick { sentence: 1, key: "Lee2020".into(), why: String::new() }]);
        assert!(parse_judgement("[]").unwrap().is_empty());
        assert!(parse_judgement("没有可配的。").is_err());
    }
}
