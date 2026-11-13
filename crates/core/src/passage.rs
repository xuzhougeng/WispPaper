//! 出处: the passage of a paper that a citation of the manuscript rests
//! on. A reference list names the paper; this names the sentence of the
//! paper's own, with its part and page, so a citation can be followed to
//! where it is from.
//!
//! A passage is kept for a manuscript, a paper and the sentence that cites
//! it (`claim`: that sentence without its citations). Nothing of it is
//! written into the manuscript, whose text stays `[@Key]`: a citation finds
//! its passage again by what its sentence says (`assign`), so it stays with
//! the sentence while the text around it is written on. A sentence written
//! anew keeps the passage of its paper in that section, said to be `loose`.
//!
//! A passage is kept when the person cites from a sentence (引用 on a
//! candidate, a paper ticked in 配引用: `CITED`), or it is looked for
//! afterwards in the paper the citation names: `sentences_of` is what a
//! paper says of its own, `nearest` ranks it against the sentence and the
//! search words made of it, and a model says which of those bears the
//! sentence out, or that none does (`choose_prompt` / `parse_choice`:
//! `MODEL`, a row without a passage for none). Without a model the nearest
//! sentence is kept when it holds a fair share of the words (`WORDS`).

use crate::cite::Cited;
use crate::citefind;
use crate::model::{Paper, new_id};
use crate::store::{PaperSentence, Store};
use crate::structure;
use crate::time::now_iso;
use anyhow::Result;
use rusqlite::params;
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS cite_passages (
    id TEXT PRIMARY KEY NOT NULL,
    manuscript_id TEXT NOT NULL REFERENCES manuscripts(id) ON DELETE CASCADE,
    section_id TEXT NOT NULL REFERENCES manuscript_sections(id) ON DELETE CASCADE,
    paper_id TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
    -- The sentence of the manuscript that cites the paper, without its citations.
    claim TEXT NOT NULL DEFAULT '',
    -- The paper's own sentence; empty when a model looked and found none that bears the claim out.
    passage TEXT NOT NULL DEFAULT '',
    -- The part of the paper it is from, a key of structure::SECTIONS.
    part TEXT NOT NULL DEFAULT '',
    -- Its page in the paper's file; 0 for a sentence of the abstract.
    page INTEGER NOT NULL DEFAULT 0,
    -- Why a model takes it to bear the claim out, or why none does.
    why TEXT NOT NULL DEFAULT '',
    -- cited, model or words.
    origin TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_cite_passages ON cite_passages(manuscript_id, paper_id);
"#;

/// The person cited the paper from this sentence.
pub const CITED: &str = "cited";
/// Looked for afterwards, and chosen by a model among the paper's sentences.
pub const MODEL: &str = "model";
/// Looked for afterwards by its words alone: no model checked it.
pub const WORDS: &str = "words";

/// The sentences of a paper a model is shown for one citation.
pub const OFFERED: usize = 6;

/// The citations one run looks for: a section's, not a thesis's.
pub const MAX_ASKED: usize = 40;

/// Two claims this much alike (`docx::likeness`) are one sentence, written on.
const ALIKE: f32 = 0.6;

/// Two claims this much alike are the same sentence: a passage kept for one takes the place of the other's.
const SAME: f32 = 0.92;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Passage {
    pub id: String,
    pub manuscript_id: String,
    pub section_id: String,
    pub paper_id: String,
    pub claim: String,
    pub passage: String,
    pub part: String,
    pub page: usize,
    pub why: String,
    /// `CITED`, `MODEL` or `WORDS`.
    pub origin: String,
    pub created_at: String,
}

impl Passage {
    /// There is a sentence of the paper's; without one, a model looked and found none.
    pub fn found(&self) -> bool {
        !self.passage.trim().is_empty()
    }
}

/// What a sentence of the manuscript says, without the citations in it:
/// `[@A; @B]` goes, with the space before it.
pub fn claim(sentence: &str) -> String {
    let mut out = String::new();
    let mut rest = sentence;
    while let Some(open) = rest.find('[') {
        let close = rest[open..].find([']', '\n']).map(|at| open + at);
        match close {
            Some(close) if rest[close..].starts_with(']') && rest[open..close].contains('@') => {
                out.push_str(&rest[..open]);
                rest = &rest[close + 1..];
            }
            _ => {
                out.push_str(&rest[..open + 1]);
                rest = &rest[open + 1..];
            }
        }
    }
    out.push_str(rest);
    let mut folded = String::new();
    for word in out.split_whitespace() {
        // The mark a sentence ends with stood after its citation.
        let joins = word.chars().next().is_some_and(|c| ".,;:!?。，；：！？、".contains(c));
        if !folded.is_empty() && !joins { folded.push(' ') }
        folded.push_str(word);
    }
    folded
}

/// A citation of a text with the sentence it stands in.
#[derive(Clone, Debug, PartialEq)]
pub struct Citation {
    pub paper: String,
    /// Where the citation is written: `@Zhang2024`.
    pub at: Range<usize>,
    /// The sentence that cites.
    pub sentence: Range<usize>,
    /// What that sentence says (`claim`).
    pub claim: String,
}

/// The citations of `text` that name a paper (`cited`: `cite::find` of the
/// same text), each with its sentence. A citation written after the full
/// stop belongs to the sentence before it.
pub fn citations(text: &str, cited: &[Cited]) -> Vec<Citation> {
    let sentences = citefind::sentences(text);
    cited
        .iter()
        .filter_map(|one| {
            let paper = one.found.paper()?.to_string();
            let n = sentences.iter().position(|range| range.start <= one.range.start && one.range.start < range.end)?;
            let mut sentence = sentences[n].clone();
            let mut said = claim(&text[sentence.clone()]);
            if !said.chars().any(char::is_alphanumeric) && n > 0 && !text[sentences[n - 1].end..sentence.start].contains('\n') {
                said = claim(&text[sentences[n - 1].clone()]);
                sentence = sentences[n - 1].start..sentence.end;
            }
            Some(Citation { paper, at: one.range.clone(), sentence, claim: said })
        })
        .collect()
}

/// What an answer of the assistant's brings into a manuscript with its
/// citations: for each sentence it quoted (`cite::convert_ai_citations`),
/// the paper, what the sentence citing it says, and the quotation as the
/// paper's sentence. `text` is the converted answer, `cited` its citations.
/// Whether the paper says so is the caller's to check.
pub fn quoted(text: &str, cited: &[Cited], quoted: &[crate::cite::Quoted]) -> Vec<(String, String, Candidate)> {
    let citations = citations(text, cited);
    quoted.iter().filter_map(|one| citations.iter().find(|citation| citation.at.start == one.at + 1 && citation.paper == one.paper_id).map(|citation| (one.paper_id.clone(), citation.claim.clone(), Candidate { text: one.quote.clone(), part: String::new(), page: one.page.unwrap_or(0) }))).collect()
}

/// The passage a citation rests on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rests {
    /// Its place among the passages kept.
    pub passage: usize,
    /// Kept for a sentence that has been written anew since: the paper's passage in this section, not checked against what the sentence says now.
    pub loose: bool,
}

/// Which of the passages `kept` for a manuscript each citation of one
/// section rests on: the one of its paper whose claim is most like the
/// citation's sentence. A citation left without takes, in order, a passage
/// of its paper kept in this section that no sentence of the section
/// claims any more (the sentence was translated or written anew), which is
/// said to be loose.
pub fn assign(citations: &[Citation], section: &str, kept: &[Passage]) -> Vec<Option<Rests>> {
    let mut out: Vec<Option<Rests>> = citations
        .iter()
        .map(|citation| {
            kept.iter()
                .enumerate()
                .filter(|(_, passage)| passage.paper_id == citation.paper)
                .map(|(n, passage)| (crate::docx::likeness(&passage.claim, &citation.claim), n))
                .filter(|(alike, _)| *alike >= ALIKE)
                // The one kept last, among those as much alike.
                .max_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)))
                .map(|(_, n)| Rests { passage: n, loose: false })
        })
        .collect();
    let mut taken: HashSet<usize> = out.iter().flatten().map(|rests| rests.passage).collect();
    for (n, citation) in citations.iter().enumerate() {
        if out[n].is_some() { continue }
        // The same sentence citing the paper twice rests on one passage.
        if let Some(earlier) = (0..n).find(|m| citations[*m].paper == citation.paper && citations[*m].sentence == citation.sentence) {
            out[n] = out[earlier];
            continue;
        }
        let Some(free) = (0..kept.len()).find(|at| kept[*at].paper_id == citation.paper && kept[*at].section_id == section && !taken.contains(at)) else { continue };
        taken.insert(free);
        out[n] = Some(Rests { passage: free, loose: !kept[free].claim.is_empty() });
    }
    out
}

/// A sentence of a paper that a citation may rest on.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Candidate {
    pub text: String,
    /// The part of the paper it is from, a key of `structure::SECTIONS`.
    pub part: String,
    /// Its page in the paper's file; 0 for a sentence of the abstract.
    pub page: usize,
}

/// What a paper says of its own: the sentences of the abstract its record
/// has, then those kept of its full text (`stored`: the paper's).
pub fn sentences_of(paper: &Paper, stored: &[PaperSentence]) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = structure::split_sentences(&paper.abstract_text).into_iter().filter(|text| structure::usable(text)).map(|text| Candidate { text, part: "abstract".into(), page: 0 }).collect();
    out.extend(stored.iter().filter(|s| s.paper_id == paper.id && s.sentence.section != "abstract" && !structure::apparatus(&s.sentence.text)).map(|s| Candidate { text: s.sentence.text.clone(), part: s.sentence.section.clone(), page: s.sentence.page }));
    out
}

/// The sentences among `held` that say most of what the queries say (a
/// sentence of the manuscript, and any search words made of it), best
/// first and at most `most`: their places, and whether each holds a fair
/// share of some query's words. One that shares a word or two is no
/// ground to cite on, but a model may still be shown it.
pub fn nearest(held: &[Candidate], queries: &[String], most: usize) -> Vec<(usize, bool)> {
    let texts: Vec<&str> = held.iter().map(|candidate| candidate.text.as_str()).collect();
    let index = crate::retrieval::Index::new(&texts);
    let unique = |text: &str| structure::words(text).into_iter().collect::<HashSet<String>>();
    let wanted: Vec<HashSet<String>> = queries.iter().map(|query| unique(query)).collect();
    // Reciprocal rank fusion, as the candidates of 写作 are merged: high in one ranking, or found by several.
    let mut merged: HashMap<usize, (f32, usize)> = HashMap::new();
    for query in queries {
        for (place, (at, _)) in index.rank(query).into_iter().enumerate() {
            let entry = merged.entry(at).or_insert((0., usize::MAX));
            entry.0 += 1. / (61. + place as f32);
            entry.1 = entry.1.min(place);
        }
    }
    let mut order: Vec<(usize, (f32, usize))> = merged.into_iter().collect();
    order.sort_by(|a, b| b.1.0.total_cmp(&a.1.0).then(a.1.1.cmp(&b.1.1)).then(a.0.cmp(&b.0)));
    let fair = |at: usize| {
        let has = unique(texts[at]);
        wanted.iter().any(|words| {
            let shared = words.intersection(&has).count();
            shared >= 2 && shared * 3 >= words.len()
        })
    };
    // The same sentence is in the abstract and in the full text of many papers: once is enough.
    let mut seen = HashSet::new();
    order.into_iter().map(|(at, _)| at).filter(|at| seen.insert(texts[*at].split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase())).take(most).map(|at| (at, fair(at))).collect()
}

/// One citation as a model is asked about it: the sentence that cites, the
/// paper, and the sentences of the paper's own that were found for it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Asked {
    pub claim: String,
    pub title: String,
    pub year: Option<i32>,
    /// (the part as a label: 摘要, 结果; the sentence).
    pub offered: Vec<(String, String)>,
}

/// What a model is told to say which of a paper's sentences each citation
/// rests on: the instructions and the message. `reply_in` is the language
/// its reasons are to be in.
pub fn choose_prompt(asked: &[Asked], reply_in: &str) -> (String, String) {
    let system = "你是严谨的学术写作助手，帮用户核对引用出处：他论文稿件里的每处引用，出自所引文献的哪一句原文。只能从给出的原文里选，不要编造。只输出 JSON，不加解释，不用 Markdown。".to_string();
    let listed: Vec<String> = asked
        .iter()
        .enumerate()
        .map(|(n, item)| {
            let year = item.year.map(|year| format!("（{year}）")).unwrap_or_default();
            let offered: Vec<String> = item.offered.iter().enumerate().map(|(m, (part, text))| format!("  {}. [{}] {}", m + 1, if part.is_empty() { "文献" } else { part.as_str() }, text.split_whitespace().collect::<Vec<_>>().join(" "))).collect();
            format!("引用 {}：{}\n所引文献：{}{year}\n{}", n + 1, item.claim.split_whitespace().collect::<Vec<_>>().join(" "), item.title.trim(), offered.join("\n"))
        })
        .collect();
    let message = format!(
        "下面是稿件里的引用，各有编号：先是稿件里引用这篇文献的那句话，然后是所引文献的题目，再是我在这篇文献里按用词找到的几句原文（各有编号，方括号里是它所在的部分）。请逐条判断：这句话引用这篇文献，依据的是哪一句原文。\n\n\
         标准：\n\
         1. 原文要说出稿件这句话所说的内容，或是它的直接依据；只是用词相近、说的是别的事的不算。\n\
         2. 稿件的句子概括了好几件事时，选说出其中与这篇文献最相关的那件事的原文。\n\
         3. 文献自己的发现和结论（摘要、结果、讨论里的）比它在导言里转述别人工作的句子更适合作出处；一样贴切时选前者。\n\
         4. 没有一句原文能作为依据时，passage 写 0，并在 why 里说明给出的原文说的是什么、为什么对不上。宁可写 0，不要勉强选。\n\n\
         输出一个 JSON 数组，每处引用一项：\n\
         [{{\"cite\": 引用编号, \"passage\": 原文编号或 0, \"why\": \"一句话说明\"}}]\n\
         why 用{reply_in}写，不超过 40 个字。\n\n{}",
        listed.join("\n\n")
    );
    (system, message)
}

/// What a model said of one citation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Choice {
    /// The citation, from 0.
    pub cite: usize,
    /// The sentence it chose among those offered, from 0; none when it found none to rest on.
    pub passage: Option<usize>,
    pub why: String,
}

/// A model's choices, one per citation it answered about. Whether a
/// number names a citation asked about and a sentence offered is the
/// caller's to check.
pub fn parse_choice(reply: &str) -> Result<Vec<Choice>, String> {
    let wrong = "模型的回答不是要求的格式。";
    let (start, end) = reply.find('[').zip(reply.rfind(']')).filter(|(start, end)| start < end).ok_or(wrong)?;
    let list: Vec<Value> = serde_json::from_str(&reply[start..=end]).map_err(|_| wrong)?;
    let number = |value: &Value| value.as_u64().or_else(|| value.as_str().and_then(|text| text.chars().filter(char::is_ascii_digit).collect::<String>().parse().ok()));
    let mut out: Vec<Choice> = vec![];
    for item in &list {
        let Some(cite) = ["cite", "citation", "引用", "n"].iter().find_map(|name| number(&item[*name])).filter(|n| *n > 0) else { continue };
        if out.iter().any(|choice| choice.cite == cite as usize - 1) { continue }
        let passage = ["passage", "sentence", "原文"].iter().find_map(|name| number(&item[*name])).filter(|n| *n > 0).map(|n| n as usize - 1);
        out.push(Choice { cite: cite as usize - 1, passage, why: item["why"].as_str().unwrap_or("").trim().to_string() });
    }
    Ok(out)
}

const COLUMNS: &str = "id, manuscript_id, section_id, paper_id, claim, passage, part, page, why, origin, created_at";

fn read_passage(r: &rusqlite::Row) -> rusqlite::Result<Passage> {
    Ok(Passage { id: r.get(0)?, manuscript_id: r.get(1)?, section_id: r.get(2)?, paper_id: r.get(3)?, claim: r.get(4)?, passage: r.get(5)?, part: r.get(6)?, page: r.get::<_, i64>(7)? as usize, why: r.get(8)?, origin: r.get(9)?, created_at: r.get(10)? })
}

impl Store {
    /// The passages kept for a manuscript's citations, in the order they were kept.
    pub fn passages(&self, manuscript_id: &str) -> Result<Vec<Passage>> {
        let mut stmt = self.db.prepare(&format!("SELECT {COLUMNS} FROM cite_passages WHERE manuscript_id = ?1 AND paper_id NOT IN (SELECT paper_id FROM trashed_papers) ORDER BY created_at, rowid"))?;
        Ok(stmt.query_map([manuscript_id], read_passage)?.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Keep passages for citations of a manuscript, all or none: (section,
    /// paper, claim, the paper's sentence, why, origin). One kept before for
    /// the same paper and the same sentence gives way. The passages as kept.
    pub fn keep_passages(&mut self, manuscript_id: &str, new: &[(String, String, String, Candidate, String, String)]) -> Result<Vec<Passage>> {
        let earlier = self.passages(manuscript_id)?;
        let now = now_iso();
        let made: Vec<Passage> = new.iter().map(|(section, paper, claim, candidate, why, origin)| Passage { id: new_id("Q"), manuscript_id: manuscript_id.into(), section_id: section.clone(), paper_id: paper.clone(), claim: claim.trim().into(), passage: candidate.text.trim().into(), part: candidate.part.clone(), page: candidate.page, why: why.trim().into(), origin: origin.clone(), created_at: now.clone() }).collect();
        let same = |a: &Passage, b: &Passage| a.paper_id == b.paper_id && if a.claim.is_empty() || b.claim.is_empty() { a.claim == b.claim && a.section_id == b.section_id } else { crate::docx::likeness(&a.claim, &b.claim) >= SAME };
        let tx = self.db.transaction()?;
        for (n, passage) in made.iter().enumerate() {
            // Of two for one sentence in what is kept now, the later stands.
            if made[n + 1..].iter().any(|later| same(later, passage)) { continue }
            for old in earlier.iter().filter(|old| same(old, passage)) { tx.execute("DELETE FROM cite_passages WHERE id = ?1", [&old.id])?; }
            tx.execute(&format!("INSERT INTO cite_passages({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"), params![passage.id, passage.manuscript_id, passage.section_id, passage.paper_id, passage.claim, passage.passage, passage.part, passage.page as i64, passage.why, passage.origin, passage.created_at])?;
        }
        tx.commit()?;
        self.passages(manuscript_id)
    }

    /// Let go of a passage: its citation is then without one, and can be looked for again.
    pub fn remove_passage(&mut self, id: &str) -> Result<()> {
        self.db.execute("DELETE FROM cite_passages WHERE id = ?1", [id])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cite;
    use crate::structure::Sentence;
    use std::collections::BTreeMap;
    use std::path::Path;

    fn paper(id: &str, authors: &str, year: i32, title: &str, abstract_text: &str) -> Paper {
        Paper { id: id.into(), authors: authors.into(), year: Some(year), title: title.into(), abstract_text: abstract_text.into(), ..Default::default() }
    }

    fn kept(id: &str, section: &str, paper: &str, claim: &str) -> Passage {
        Passage { id: id.into(), section_id: section.into(), paper_id: paper.into(), claim: claim.into(), passage: format!("what {paper} says"), origin: CITED.into(), ..Default::default() }
    }

    #[test]
    fn what_a_sentence_says_without_its_citations() {
        assert_eq!(claim("Roots sense  compaction [@Zhang2021; @Lee2020, p. 5]."), "Roots sense compaction.");
        assert_eq!(claim("单细胞测序[@A2019]成为重要手段[@B2020]。"), "单细胞测序成为重要手段。");
        // Brackets that cite nothing stay, and one that never closes.
        assert_eq!(claim("Growth fell [n = 3] in soil [@A]\nand [@B"), "Growth fell [n = 3] in soil and [@B");
        assert_eq!(claim("  "), "");
    }

    #[test]
    fn citations_with_their_sentences() {
        let papers = [paper("a", "Zhang, Wei", 2021, "Root atlas", ""), paper("b", "Lee, Min", 2020, "Ethylene", "")];
        let nothing = BTreeMap::new();
        let index = cite::Index::new(&papers, [], &nothing);
        let text = "Roots sense compaction [@Zhang2021; @Lee2020]. Ethylene builds up. [@Lee2020]\n\n[@Zhang2021] opens a line, and [@Nobody1999] is unknown.";
        let found = citations(text, &cite::find(text, &index));
        assert_eq!(found.iter().map(|c| (c.paper.as_str(), &text[c.at.clone()], c.claim.as_str())).collect::<Vec<_>>(), [
            ("a", "@Zhang2021", "Roots sense compaction."),
            ("b", "@Lee2020", "Roots sense compaction."),
            // Written after the full stop: the sentence before is what it is cited for.
            ("b", "@Lee2020", "Ethylene builds up."),
            ("a", "@Zhang2021", "opens a line, and is unknown."),
        ]);
        assert_eq!(&text[found[2].sentence.clone()], "Ethylene builds up. [@Lee2020]");
        // An answer of the assistant's: the sentence it quoted for a citation is that citation's passage.
        let sources = [crate::ai::Source { label: "P1".into(), paper_id: "a".into(), title: String::new() }, crate::ai::Source { label: "P2".into(), paper_id: "b".into(), title: String::new() }];
        let answer = cite::convert_ai_citations("Roots sense compaction [P1 p.4 \"Roots sense soil compaction.\"]. Ethylene builds up [P2].", &sources, &index);
        assert_eq!(quoted(&answer.text, &cite::find(&answer.text, &index), &answer.quoted), [("a".to_string(), "Roots sense compaction.".to_string(), Candidate { text: "Roots sense soil compaction.".into(), part: String::new(), page: 4 })]);
    }

    #[test]
    fn a_citation_finds_its_passage_by_its_sentence() {
        let cite = |paper: &str, at: usize, claim: &str| Citation { paper: paper.into(), at: at..at + 1, sentence: at..at + 1, claim: claim.into() };
        let passages = [
            kept("1", "s", "a", "Roots sense soil compaction through ethylene."),
            kept("2", "s", "a", "Single-cell atlases resolve the cell types of the root."),
            kept("3", "s", "b", "乙烯在紧实土壤里积累。"),
            kept("4", "other", "c", "Kept in another section."),
        ];
        let found = assign(&[
            // Written on a little: still its sentence.
            cite("a", 0, "Roots sense the compaction of soil through ethylene."),
            cite("a", 10, "Single-cell atlases resolve the cell types of the root."),
            // Translated: the paper's passage in this section, loosely.
            cite("b", 20, "Ethylene accumulates in compacted soil."),
            // A paper without a passage here, and one whose passage is another section's.
            cite("d", 30, "Nothing kept."),
            cite("c", 40, "Something else."),
        ], "s", &passages);
        assert_eq!(found, [Some(Rests { passage: 0, loose: false }), Some(Rests { passage: 1, loose: false }), Some(Rests { passage: 2, loose: true }), None, None]);
        // A passage a sentence still claims is not handed to another sentence of its paper.
        let two = assign(&[cite("a", 0, "Roots sense soil compaction through ethylene."), cite("a", 10, "A new sentence about leaves.")], "s", &passages[..1]);
        assert_eq!(two, [Some(Rests { passage: 0, loose: false }), None]);
        // Cited before a word was written: the passage is the citation's, and not loose.
        let early = [kept("5", "s", "a", "")];
        assert_eq!(assign(&[cite("a", 0, "Written afterwards.")], "s", &early), [Some(Rests { passage: 0, loose: false })]);
    }

    #[test]
    fn the_sentences_of_a_paper_nearest_to_a_claim() {
        let one = paper("a", "Zhang, Wei", 2021, "Root atlas", "We profiled the cells of the Arabidopsis root and resolved the trajectories of its cell types. Short one.");
        let stored = [
            PaperSentence { paper_id: "a".into(), sentence: Sentence { section: "results".into(), page: 4, text: "Ethylene accumulates around roots in compacted soil and restricts their elongation.".into(), ..Default::default() } },
            PaperSentence { paper_id: "a".into(), sentence: Sentence { section: "methods".into(), page: 9, text: "Protoplasts were isolated and libraries were prepared with the Chromium platform.".into(), ..Default::default() } },
            PaperSentence { paper_id: "b".into(), sentence: Sentence { section: "results".into(), page: 2, text: "Another paper's sentence about ethylene in compacted soil.".into(), ..Default::default() } },
        ];
        let held = sentences_of(&one, &stored);
        assert_eq!(held.iter().map(|c| (c.part.as_str(), c.page)).collect::<Vec<_>>(), [("abstract", 0), ("results", 4), ("methods", 9)]);
        let found = nearest(&held, &["Soil compaction restricts root elongation through ethylene.".into()], 3);
        assert_eq!(found[0], (1, true));
        // Written in Chinese: only the search words made of it find the sentence.
        assert!(nearest(&held, &["土壤紧实通过乙烯抑制根的伸长。".into()], 3).is_empty());
        assert_eq!(nearest(&held, &["土壤紧实通过乙烯抑制根的伸长。".into(), "soil compaction ethylene root elongation".into()], 3)[0], (1, true));
        // A word in common is found, and not called fair.
        assert_eq!(nearest(&held, &["Volcanic eruptions isolated the island for centuries.".into()], 3), [(2, false)]);
    }

    #[test]
    fn a_model_chooses_among_the_sentences() {
        let asked = [Asked { claim: "Roots sense\ncompaction.".into(), title: "Root atlas".into(), year: Some(2021), offered: vec![("结果".into(), "Ethylene  accumulates\naround roots.".into()), (String::new(), "No part.".into())] }];
        let (system, message) = choose_prompt(&asked, "中文");
        assert!(system.contains("核对引用出处"));
        assert!(message.ends_with("引用 1：Roots sense compaction.\n所引文献：Root atlas（2021）\n  1. [结果] Ethylene accumulates around roots.\n  2. [文献] No part."));
        assert!(message.contains("why 用中文写"));
        let reply = "```json\n[{\"cite\": 1, \"passage\": 2, \"why\": \"报道了乙烯积累\"}, {\"cite\": \"引用 2\", \"passage\": 0, \"why\": \"说的是别的事\"}, {\"cite\": 1, \"passage\": 1}, {\"passage\": 1}]\n```";
        assert_eq!(parse_choice(reply).unwrap(), [Choice { cite: 0, passage: Some(1), why: "报道了乙烯积累".into() }, Choice { cite: 1, passage: None, why: "说的是别的事".into() }]);
        assert!(parse_choice("[]").unwrap().is_empty());
        assert!(parse_choice("都对得上。").is_err());
    }

    #[test]
    fn passages_are_kept_replaced_and_let_go() {
        let mut store = Store::in_memory(Path::new("/tmp/wisp-passage-test")).unwrap();
        let a = store.add_paper(paper("", "Zhang, Wei", 2021, "Root atlas", "")).unwrap();
        let manuscript = store.create_manuscript("Draft", "en").unwrap();
        let (first, second) = (manuscript.sections[1].id.clone(), manuscript.sections[2].id.clone());
        let from = |text: &str, page: usize| Candidate { text: text.into(), part: "results".into(), page };
        let made = store.keep_passages(&manuscript.id, &[(first.clone(), a.id.clone(), "Roots sense compaction.".into(), from(" Ethylene accumulates. ", 4), String::new(), CITED.into()), (second.clone(), a.id.clone(), "Atlases resolve cell types.".into(), from("We profiled cells.", 2), "概述".into(), MODEL.into())]).unwrap();
        assert_eq!(made.iter().map(|p| (p.passage.as_str(), p.page, p.origin.as_str(), p.found())).collect::<Vec<_>>(), [("Ethylene accumulates.", 4, CITED, true), ("We profiled cells.", 2, MODEL, true)]);
        // The same sentence again: the new passage takes the old one's place; a model that found none keeps a row without one.
        let again = store.keep_passages(&manuscript.id, &[(first.clone(), a.id.clone(), "Roots sense compaction.".into(), Candidate::default(), "原文说的是别的".into(), MODEL.into())]).unwrap();
        assert_eq!(again.len(), 2);
        assert!(again.iter().any(|p| p.claim == "Roots sense compaction." && !p.found() && p.why == "原文说的是别的"));
        store.remove_passage(&again[0].id).unwrap();
        assert_eq!(store.passages(&manuscript.id).unwrap().len(), 1);
        // A section merged into the one before takes its passages along; a paper in the trash shows none, and gets them back.
        let merged = store.merge_section(&second).unwrap();
        assert_eq!(store.passages(&manuscript.id).unwrap().iter().map(|p| p.section_id == merged.id).collect::<Vec<_>>(), [true]);
        store.trash_paper(&a.id).unwrap();
        assert!(store.passages(&manuscript.id).unwrap().is_empty());
        store.restore_paper(&a.id).unwrap();
        store.remove_section(&merged.id).unwrap();
        assert!(store.passages(&manuscript.id).unwrap().is_empty());
    }
}
