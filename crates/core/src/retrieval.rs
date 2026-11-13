//! 写作's search for candidate sentences: what is searched for (the
//! selection or the paragraph at the caret), the groups of search words a
//! model makes of it in the language of the library's sentences, one
//! ranking per group over a word index kept between searches, and the
//! lists merged into one.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

/// The most a search is made of: a long paragraph is searched by its last part.
const SEARCH_CHARS: usize = 1500;

/// What a search asks for when nothing is selected: the whole paragraph
/// the caret stands in (a line of the manuscript), trimmed.
pub fn paragraph_at(text: &str, caret: usize) -> String {
    let caret = text.floor_char_boundary(caret.min(text.len()));
    let start = text[..caret].rfind('\n').map_or(0, |at| at + 1);
    let end = text[caret..].find('\n').map_or(text.len(), |at| caret + at);
    let paragraph = text[start..end].trim();
    let skip = paragraph.chars().count().saturating_sub(SEARCH_CHARS);
    paragraph.chars().skip(skip).collect()
}

/// The most groups a reply is taken for.
pub const MOST_GROUPS: usize = 5;

/// What a model is told to turn a piece of a manuscript into groups of
/// search words, in `language`, the language of the library's sentences.
pub fn queries_system(language: &str) -> String {
    format!(
        "你是写作检索词助手。用户正在写学术论文，下面是他写的一段（可能是任何语言，也可能只是想表达的意思）。文献库里的句子是{language}的；请给出 3 到 5 组用来在这些句子里检索相似写法的{language}检索词，每组一行，格式为「类别：词语」，类别只写下面三个词之一：\n\
         内容：这段讲的概念、对象、方法和结果，用该领域论文里通行的术语；\n\
         论述：这段在论证中做的事，用论文里表达这种意思的惯用说法（例如 remains unclear、in contrast to、we hypothesized that、little is known about）；\n\
         说法：同一意思在这个领域的其他说法、同义词和缩写。\n\
         每组 4 到 10 个词或短语，用空格分隔；内容可以分两组（主要对象一组，方法或结果一组）。只输出这几行，不要解释，不要编号。"
    )
}

/// One group of search words a model gave, with what kind of words they are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    /// As the model named it (内容, 论述, 说法…); empty when it named none.
    pub kind: String,
    pub words: String,
}

/// The groups in a model's reply: one per line, `kind: words`. Bullets,
/// numbering and code fences are dropped; a line that is only a heading
/// (`Here are the groups:`) has no words and is dropped too. When no line
/// names its kind, every line with words is a group.
pub fn parse_groups(reply: &str) -> Vec<Group> {
    let mut named = vec![];
    let mut plain = vec![];
    for line in reply.lines() {
        let line = line.trim().trim_start_matches(|c: char| c == '-' || c == '*' || c == '•' || c == '#' || c.is_ascii_digit() || c == '.' || c == '、' || c == ')' || c.is_whitespace());
        // Empty, a fence, or a heading over what follows.
        if line.is_empty() || line.starts_with("```") || line.ends_with([':', '：']) { continue }
        let split = line.find([':', '：']).map(|at| (&line[..at], line[at..].trim_start_matches([':', '：'])));
        match split {
            // A kind is a word or two, not a sentence that has a colon in it.
            Some((kind, words)) if kind.chars().count() <= 16 && kind.split_whitespace().count() <= 2 => {
                let words = clean(words);
                if !words.is_empty() { named.push(Group { kind: kind.trim().trim_matches(['*', '【', '】', '「', '」']).to_string(), words }) }
            }
            _ => {
                let words = clean(line);
                if !words.is_empty() { plain.push(Group { kind: String::new(), words }) }
            }
        }
    }
    let mut groups = if named.is_empty() { plain } else { named };
    let mut seen = HashSet::new();
    groups.retain(|group| seen.insert(group.words.to_lowercase()));
    groups.truncate(MOST_GROUPS);
    groups
}

/// Search words as one line: separators a model may use become spaces, quotes go.
pub(crate) fn clean(words: &str) -> String {
    words.replace([',', '，', ';', '；', '、', '"', '“', '”', '`'], " ").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The sentences of one part as words, ready for ranking any number of
/// queries: built once with `words` and kept (`index_for`).
pub struct Index {
    /// For each word, the sentences that hold it and how often.
    postings: HashMap<String, Vec<(u32, u16)>>,
    lengths: Vec<u32>,
    average: f32,
}

impl Index {
    pub fn new(texts: &[&str]) -> Index {
        let mut postings: HashMap<String, Vec<(u32, u16)>> = HashMap::new();
        let mut lengths = Vec::with_capacity(texts.len());
        for (doc, text) in texts.iter().enumerate() {
            let bag = crate::structure::words(text);
            lengths.push(bag.len() as u32);
            let mut counts: HashMap<String, u16> = HashMap::new();
            for word in bag { *counts.entry(word).or_default() += 1 }
            for (word, count) in counts { postings.entry(word).or_default().push((doc as u32, count)) }
        }
        let average = if lengths.is_empty() { 0. } else { lengths.iter().sum::<u32>() as f32 / lengths.len() as f32 };
        Index { postings, lengths, average }
    }

    pub fn len(&self) -> usize {
        self.lengths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lengths.is_empty()
    }

    /// The sentences that say most of what `query` says, the best first:
    /// `(index, score)`. BM25, as `structure::rank` scores them, without
    /// its limit per paper (that is for after merging).
    pub fn rank(&self, query: &str) -> Vec<(usize, f32)> {
        let mut seen = HashSet::new();
        let wanted: Vec<String> = crate::structure::words(query).into_iter().filter(|word| seen.insert(word.clone())).collect();
        if wanted.is_empty() || self.is_empty() { return vec![] }
        let total = self.lengths.len() as f32;
        let mut scores: HashMap<u32, f32> = HashMap::new();
        for word in &wanted {
            let Some(holders) = self.postings.get(word) else { continue };
            let weight = (1. + (total - holders.len() as f32 + 0.5) / (holders.len() as f32 + 0.5)).ln();
            for (doc, hits) in holders {
                let (hits, length) = (*hits as f32, self.lengths[*doc as usize] as f32);
                *scores.entry(*doc).or_default() += weight * hits * 2.2 / (hits + 1.2 * (0.25 + 0.75 * length / self.average.max(1.)));
            }
        }
        let mut scored: Vec<(usize, f32)> = scores.into_iter().map(|(doc, score)| (doc as usize, score)).collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        scored
    }
}

/// The indexes made lately, by what their sentences say: a search over the
/// same sentences finds its index ready. Kept by a hash of the texts
/// themselves, so a text sorted anew, added or removed makes a new one.
static INDEXES: Mutex<Vec<(u64, Arc<Index>)>> = Mutex::new(vec![]);

/// How many indexes are kept: a part or two of a few libraries.
const KEPT_INDEXES: usize = 8;

/// The index of these sentences, made now or kept from before.
pub fn index_for(texts: &[&str]) -> Arc<Index> {
    let mut hasher = DefaultHasher::new();
    texts.len().hash(&mut hasher);
    for text in texts { text.hash(&mut hasher) }
    let key = hasher.finish();
    if let Some((_, index)) = INDEXES.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(k, _)| *k == key) { return index.clone() }
    let index = Arc::new(Index::new(texts));
    let mut kept = INDEXES.lock().unwrap_or_else(|e| e.into_inner());
    kept.retain(|(k, _)| *k != key);
    kept.push((key, index.clone()));
    if kept.len() > KEPT_INDEXES { kept.remove(0); }
    index
}

/// The sentences one paper may place among the merged candidates.
pub const PER_PAPER: usize = 3;

/// How far down a list a place still counts in merging (reciprocal rank fusion's k).
const FUSION_K: f32 = 60.;

/// Several rankings merged into one (reciprocal rank fusion): a sentence
/// high in any list, or found by several, comes first. `owners` names the
/// paper of each sentence; no paper takes more than `PER_PAPER` places.
pub fn fuse(lists: &[Vec<(usize, f32)>], owners: &[&str], limit: usize) -> Vec<usize> {
    let mut merged: HashMap<usize, (f32, usize)> = HashMap::new();
    for list in lists {
        for (place, (index, _)) in list.iter().enumerate() {
            let entry = merged.entry(*index).or_insert((0., usize::MAX));
            entry.0 += 1. / (FUSION_K + place as f32 + 1.);
            entry.1 = entry.1.min(place);
        }
    }
    let mut order: Vec<(usize, (f32, usize))> = merged.into_iter().collect();
    order.sort_by(|a, b| b.1.0.total_cmp(&a.1.0).then(a.1.1.cmp(&b.1.1)).then(a.0.cmp(&b.0)));
    let mut taken: HashMap<&str, usize> = HashMap::new();
    order
        .into_iter()
        .map(|(index, _)| index)
        .filter(|index| {
            let count = taken.entry(owners.get(*index).copied().unwrap_or("")).or_default();
            *count += 1;
            *count <= PER_PAPER
        })
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_search_is_for() {
        let text = "First paragraph, done.\nRoots adapt to soil. However, how they respond\n\nThird.";
        assert_eq!(paragraph_at(text, 30), "Roots adapt to soil. However, how they respond");
        assert_eq!(paragraph_at(text, text.find("\n\n").unwrap()), "Roots adapt to soil. However, how they respond");
        assert_eq!(paragraph_at(text, 3), "First paragraph, done.");
        assert_eq!(paragraph_at(text, text.len() - 2), "Third.");
        assert_eq!(paragraph_at("根系如何感知土壤", 100), "根系如何感知土壤");
        assert_eq!(paragraph_at("", 0), "");
        assert_eq!(paragraph_at(&"a".repeat(2000), 5).len(), SEARCH_CHARS);
    }

    #[test]
    fn groups_of_a_reply() {
        let reply = "好的，检索词如下：\n```\n1. 内容：root soil compaction, penetration resistance\n- **论述**: remains unclear; little is known about\n说法：hard soil、mechanical impedance\n内容：root soil compaction penetration resistance\n```";
        assert_eq!(
            parse_groups(reply),
            [
                Group { kind: "内容".into(), words: "root soil compaction penetration resistance".into() },
                Group { kind: "论述".into(), words: "remains unclear little is known about".into() },
                Group { kind: "说法".into(), words: "hard soil mechanical impedance".into() },
            ]
        );
        // Without kinds every line is a group; a colon inside a sentence names no kind.
        assert_eq!(parse_groups("root compaction\nin contrast, we hypothesized: roots").iter().map(|g| g.words.as_str()).collect::<Vec<_>>(), ["root compaction", "in contrast we hypothesized: roots"]);
        assert_eq!(parse_groups("a: 1\nb: 2\nc: 3\nd: 4\ne: 5\nf: 6").len(), MOST_GROUPS);
        assert!(parse_groups("").is_empty() && parse_groups("Here are the groups:").is_empty());
        assert!(queries_system("英文").contains("英文的"));
    }

    #[test]
    fn index_ranks_as_rank_does() {
        let texts = [
            "However, how roots respond to soil compaction at the cellular level remains poorly understood.",
            "Single-cell RNA sequencing has transformed the study of plant development.",
            "Roots respond to compacted soils by regulating cell wall remodelling.",
            "The regulation of root growth by auxin is well known.",
            "We thank the members of the laboratory for discussions.",
        ];
        let owners = ["a", "b", "c", "d", "e"];
        let index = Index::new(&texts);
        for query in ["root response to soil compaction", "regulated", "cell wall"] {
            let old = crate::structure::rank(&texts, &owners, query, 10);
            let new = index.rank(query);
            assert_eq!(old.iter().map(|(i, _)| *i).collect::<Vec<_>>(), new.iter().map(|(i, _)| *i).collect::<Vec<_>>(), "{query}");
            for ((_, a), (_, b)) in old.iter().zip(&new) { assert!((a - b).abs() < 1e-4) }
        }
        assert!(index.rank("的 the").is_empty());
        // The same sentences find the same index; others make their own.
        assert!(Arc::ptr_eq(&index_for(&texts), &index_for(&texts)));
        assert!(!Arc::ptr_eq(&index_for(&texts), &index_for(&texts[1..])));
    }

    #[test]
    fn lists_merged() {
        // 2 is found by both lists, so it beats the head of either.
        let lists = [vec![(0, 9.), (2, 5.), (3, 1.)], vec![(1, 8.), (2, 7.)]];
        let owners = ["a", "b", "c", "d"];
        assert_eq!(fuse(&lists, &owners, 10), [2, 0, 1, 3]);
        assert_eq!(fuse(&lists, &owners, 2), [2, 0]);
        // At most three places per paper, after merging.
        let one: Vec<(usize, f32)> = (0..6).map(|i| (i, 1.)).collect();
        assert_eq!(fuse(&[one], &["a"; 6], 10), [0, 1, 2]);
        assert!(fuse(&[], &owners, 10).is_empty());
    }
}
