//! Completing a record that came without its metadata (a PDF with no DOI
//! on its first pages, most of the time): ask Crossref, Europe PMC and
//! arXiv for records by title, say how each one found compares with what is known,
//! and write the fields the person takes into the paper. No HTTP here (the
//! caller hands in `Fetch`), no browser, and nothing is written without a
//! person choosing the record and the fields: the first hit of a title
//! search is not the paper.

use crate::lookup::{crossref_work, encode, europepmc_hit, merge_lookup};
use crate::model::{PUBLICATION_STATUSES, Paper, PaperFile, label, title_key};
use crate::sources::{Fetch, arxiv_found};
use serde_json::Value;

/// How many records each source is asked for.
pub const PER_SOURCE: usize = 5;

/// A record less like the title than this is left out.
const LEAST_ALIKE: f32 = 0.5;

/// What to look for: the title, and what else is known to tell records apart.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    pub title: String,
    /// A name of one of the authors; may be empty.
    pub author: String,
    pub year: Option<i32>,
}

/// A record a source holds that may be the paper.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub paper: Paper,
    /// The sources that hold it; more than one when they name the same DOI.
    pub sources: Vec<&'static str>,
    /// How alike its title and the title asked for are, 0 to 1. For the order only.
    pub similarity: f32,
    /// Where it agrees with what was asked for, and where it does not.
    pub same: Vec<String>,
    pub differs: Vec<String>,
    /// The source gave only some of the authors.
    pub partial_authors: bool,
}

/// What came of asking one source.
#[derive(Clone, Debug, PartialEq)]
pub struct Asked {
    pub source: &'static str,
    /// How many records it answered with; 0 with an empty `error` is "nothing found".
    pub found: usize,
    /// Why it could not be asked; empty when it answered.
    pub error: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    pub candidates: Vec<Candidate>,
    pub asked: Vec<Asked>,
}

/// A title as it is asked for: one line, without the quotation marks a query would read.
fn plain(title: &str) -> String {
    title.replace(['"', '“', '”'], " ").split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn crossref_url(query: &Query) -> String {
    let wanted = [plain(&query.title), query.year.map(|y| y.to_string()).unwrap_or_default()].join(" ");
    let author = if query.author.trim().is_empty() { String::new() } else { format!("&query.author={}", encode(query.author.trim())) };
    format!("https://api.crossref.org/works?rows={PER_SOURCE}&select={}&query.bibliographic={}{author}", encode("DOI,title,author,published,published-print,published-online,issued,container-title,abstract,type,URL,publisher"), encode(wanted.trim()))
}

/// `exact` asks for the title as a phrase; without it the words may stand anywhere in the title.
pub fn europepmc_url(query: &Query, exact: bool) -> String {
    let title = plain(&query.title);
    let mut wanted = if exact { format!("TITLE:\"{title}\"") } else { format!("TITLE:({})", title.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty()).collect::<Vec<_>>().join(" ")) };
    if !query.author.trim().is_empty() { wanted.push_str(&format!(" AND AUTH:\"{}\"", plain(&query.author))) }
    format!("https://www.ebi.ac.uk/europepmc/webservices/rest/search?format=json&resultType=core&pageSize={PER_SOURCE}&query={}", encode(&wanted))
}

/// arXiv, where a preprint the other two do not list is found.
pub fn arxiv_url(query: &Query) -> String {
    let mut wanted = format!("ti:\"{}\"", plain(&query.title));
    if !query.author.trim().is_empty() { wanted.push_str(&format!(" AND au:\"{}\"", plain(&query.author))) }
    format!("https://export.arxiv.org/api/query?max_results={PER_SOURCE}&search_query={}", encode(&wanted))
}

/// The words of a title, for comparing two of them.
fn words(title: &str) -> Vec<String> {
    title.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty()).map(str::to_string).collect()
}

/// How alike two titles are: 1 when they differ in punctuation and case
/// only, else the share of their words that stand in the same order in
/// both, so that "Is attention all you need?" is not "Attention is all
/// you need".
pub fn similarity(a: &str, b: &str) -> f32 {
    if !title_key(a).is_empty() && title_key(a) == title_key(b) { return 1. }
    let (a, b) = (words(a), words(b));
    if a.is_empty() || b.is_empty() { return 0. }
    // The longest run of words, gaps allowed, that both have in this order.
    let mut row = vec![0usize; b.len() + 1];
    for word in &a {
        let mut diagonal = 0;
        for (at, other) in b.iter().enumerate() {
            let above = row[at + 1];
            row[at + 1] = if word == other { diagonal + 1 } else { above.max(row[at]) };
            diagonal = above;
        }
    }
    // Never quite 1: only the same title is the same title.
    (2. * row[b.len()] as f32 / (a.len() + b.len()) as f32).min(0.99)
}

fn candidate(mut paper: Paper, source: &'static str, query: &Query) -> Candidate {
    paper.normalize();
    paper.source = source.into();
    let similarity = similarity(&query.title, &paper.title);
    let (mut same, mut differs) = (vec![], vec![]);
    if similarity >= 1. { same.push("题目一致".to_string()) } else { differs.push(format!("题目有 {}% 的词相同", (similarity * 100.).round() as u32)) }
    if let Some(wanted) = query.year {
        match paper.year {
            Some(year) if year == wanted => same.push(format!("年份 {year}")),
            Some(year) => differs.push(format!("年份是 {year}，不是 {wanted}")),
            None => {}
        }
    }
    let wanted: Vec<String> = words(&query.author).into_iter().filter(|word| word.chars().count() >= 2).collect();
    if !wanted.is_empty() && !paper.authors.is_empty() {
        let authors = paper.authors.to_lowercase();
        if wanted.iter().any(|word| authors.contains(word)) { same.push(format!("作者里有 {}", query.author.trim())) } else { differs.push(format!("作者里没有 {}", query.author.trim())) }
    }
    let lower = paper.authors.to_lowercase();
    let partial_authors = lower.contains("et al") || lower.trim_end().ends_with("...") || lower.trim_end().ends_with('…');
    Candidate { paper, sources: vec![source], similarity, same, differs, partial_authors }
}

/// Ask the sources about a title. One that fails or finds nothing takes
/// nothing from what the other found: each is reported on its own. Records
/// of the same DOI are one candidate; a preprint and the article it became
/// have two DOIs and stay two.
pub fn find(query: &Query, fetch: Fetch) -> Found {
    let json = |url: &str| -> Result<Value, String> { serde_json::from_str(&fetch(url)?).map_err(|e| format!("无法解析返回内容：{e}")) };
    let crossref = || -> Result<Vec<Paper>, String> { Ok(json(&crossref_url(query))?["message"]["items"].as_array().into_iter().flatten().filter_map(crossref_work).collect()) };
    let europepmc = || -> Result<Vec<Paper>, String> {
        let hits = |exact: bool| -> Result<Vec<Paper>, String> { Ok(json(&europepmc_url(query, exact))?["resultList"]["result"].as_array().into_iter().flatten().filter_map(europepmc_hit).collect()) };
        // A title typed a little differently from the record is no phrase of it.
        match hits(true)? {
            exact if !exact.is_empty() => Ok(exact),
            _ => hits(false),
        }
    };
    let mut found = Found::default();
    let arxiv = || -> Result<Vec<Paper>, String> { Ok(arxiv_found(&fetch(&arxiv_url(query))?).papers) };
    for (source, answer) in [("Crossref", crossref()), ("Europe PMC", europepmc()), ("arXiv", arxiv())] {
        match answer {
            Ok(papers) => {
                found.asked.push(Asked { source, found: papers.len(), error: String::new() });
                for paper in papers.into_iter().take(PER_SOURCE) {
                    let new = candidate(paper, source, query);
                    match found.candidates.iter_mut().find(|held| !new.paper.doi.is_empty() && held.paper.doi == new.paper.doi) {
                        Some(held) => {
                            held.paper = merge_lookup(held.paper.clone(), new.paper);
                            held.sources.push(source);
                        }
                        None => found.candidates.push(new),
                    }
                }
            }
            Err(error) => found.asked.push(Asked { source, found: 0, error }),
        }
    }
    found.candidates.retain(|candidate| candidate.similarity >= LEAST_ALIKE);
    found.candidates.sort_by(|a, b| b.similarity.total_cmp(&a.similarity));
    found
}

// ---- the fields of a record

/// The fields a candidate can give: (key, name shown).
pub const FIELDS: [(&str, &str); 8] = [("title", "题目"), ("authors", "作者"), ("year", "年份"), ("journal", "期刊"), ("status", "发表状态"), ("doi", "DOI"), ("pmid", "PMID"), ("abstract", "摘要")];

/// A field of a paper as text; empty when the paper does not say.
pub fn value(paper: &Paper, key: &str) -> String {
    match key {
        "title" => paper.title.clone(),
        "authors" => paper.authors.clone(),
        "year" => paper.year.map(|y| y.to_string()).unwrap_or_default(),
        "journal" => paper.journal.clone(),
        "status" if paper.publication_status != "unknown" => paper.publication_status.clone(),
        "doi" => paper.doi.clone(),
        "pmid" => paper.pmid.clone(),
        "abstract" => paper.abstract_text.clone(),
        _ => String::new(),
    }
}

/// The same for showing: a status by its label.
pub fn shown(paper: &Paper, key: &str) -> String {
    let value = value(paper, key);
    if key == "status" { label(&PUBLICATION_STATUSES, &value).to_string() } else { value }
}

/// The paper's title is still the name of its file, as an import without a DOI leaves it.
pub fn placeholder_title(paper: &Paper, files: &[PaperFile]) -> bool {
    let stem = |name: &str| name.rsplit_once('.').map_or(name, |(stem, _)| stem).trim().to_string();
    files.iter().any(|file| file.paper_id == paper.id && (file.title.trim() == paper.title.trim() || stem(&file.title) == paper.title.trim()))
}

/// From here on a record's title counts as the title asked for, give or
/// take a subtitle or a misread letter. Under it no field is taken unless
/// the person picks it: a title that is only alike is another paper more
/// often than not.
pub const ALIKE_ENOUGH: f32 = 0.9;

/// The fields in which the candidate says something the paper does not.
pub fn offered(paper: &Paper, candidate: &Paper) -> Vec<&'static str> {
    FIELDS.iter().map(|(key, _)| *key).filter(|key| !value(candidate, key).is_empty() && value(candidate, key) != value(paper, key)).collect()
}

/// Of those, the ones taken unless the person says otherwise: what the
/// paper leaves blank, and a title that is only a file name. A value the
/// paper has is replaced only when the person picks the field.
pub fn suggested(paper: &Paper, candidate: &Paper, placeholder: bool) -> Vec<&'static str> {
    offered(paper, candidate).into_iter().filter(|key| value(paper, key).is_empty() || (*key == "title" && placeholder)).collect()
}

/// Write the picked fields of a candidate into the paper, and say where they are from.
pub fn apply(paper: &mut Paper, candidate: &Candidate, keys: &[&str]) {
    let from = &candidate.paper;
    for key in keys {
        if value(from, key).is_empty() { continue }
        match *key {
            "title" => paper.title = from.title.clone(),
            "authors" => paper.authors = from.authors.clone(),
            "year" => paper.year = from.year,
            "journal" => paper.journal = from.journal.clone(),
            "status" => paper.publication_status = from.publication_status.clone(),
            "doi" => paper.doi = from.doi.clone(),
            "pmid" => paper.pmid = from.pmid.clone(),
            "abstract" => paper.abstract_text = from.abstract_text.clone(),
            _ => {}
        }
    }
    if keys.is_empty() { return }
    let note = format!("按题目从 {} 补全", candidate.sources.join("、"));
    let before = paper.source.split('；').next().unwrap_or("").trim().to_string();
    paper.source = if before.is_empty() { note } else { format!("{before}；{note}") };
}

#[cfg(test)]
mod tests {
    use super::*;

    const CROSSREF: &str = r#"{"message":{"items":[
        {"DOI":"10.1104/pp.18.01482","title":["A Single-Cell RNA Sequencing Profiles the Developmental Landscape of Arabidopsis Root"],"author":[{"family":"Zhang","given":"Tian-Qi"},{"family":"Wang","given":"Jia-Wei"}],"published-print":{"date-parts":[[2019,5]]},"container-title":["Molecular Plant"],"type":"journal-article","URL":"https://doi.org/10.1104/pp.18.01482"},
        {"DOI":"10.1101/448514","title":["A single cell RNA sequencing profiles the developmental landscape of Arabidopsis root"],"author":[{"family":"Zhang","given":"Tian-Qi"}],"posted":{"date-parts":[[2018,10]]},"type":"posted-content","publisher":"bioRxiv"},
        {"DOI":"10.1/other","title":["Leaf development in maize"],"type":"journal-article"}]}}"#;
    const EUROPEPMC: &str = r#"{"resultList":{"result":[
        {"title":"A Single-Cell RNA Sequencing Profiles the Developmental Landscape of Arabidopsis Root.","authorString":"Zhang TQ, Xu ZG, et al.","pubYear":"2019","doi":"10.1104/PP.18.01482","pmid":"31004836","journalTitle":"Mol Plant","abstractText":"Cells of the root.","source":"MED"}]}}"#;

    const ARXIV: &str = "<feed><entry><id>http://arxiv.org/abs/1810.00001v2</id><published>2018-10-01T00:00:00Z</published><title>A single cell RNA sequencing profiles the developmental\n landscape of Arabidopsis root</title><summary>Cells.</summary><author><name>Tian-Qi Zhang</name></author></entry></feed>";

    fn query() -> Query {
        Query { title: "A single-cell RNA sequencing profiles the developmental landscape of Arabidopsis root".into(), author: "Wang".into(), year: Some(2019) }
    }

    #[test]
    fn what_is_asked() {
        let crossref = crossref_url(&query());
        assert!(crossref.contains("rows=5") && crossref.contains("query.bibliographic=A%20single-cell%20RNA") && crossref.ends_with("Arabidopsis%20root%202019&query.author=Wang"));
        let quoted = Query { title: " The “root”  cap\n\"atlas\" ".into(), ..Default::default() };
        assert!(europepmc_url(&quoted, true).ends_with(&format!("query={}", encode("TITLE:\"The root cap atlas\""))));
        assert!(europepmc_url(&query(), false).ends_with(&encode("TITLE:(A single cell RNA sequencing profiles the developmental landscape of Arabidopsis root) AND AUTH:\"Wang\"")));
        assert!(!crossref_url(&quoted).contains("query.author"));
        assert!(arxiv_url(&quoted).ends_with(&format!("max_results=5&search_query={}", encode("ti:\"The root cap atlas\""))) && arxiv_url(&query()).ends_with(&encode(" AND au:\"Wang\"")));
    }

    #[test]
    fn titles_compared() {
        assert_eq!(similarity("Single-cell atlas: the Root", "single cell atlas the root."), 1.);
        assert_eq!(similarity("root cap atlas", "leaf vein map"), 0.);
        assert_eq!(similarity("", ""), 0.);
        // The same words in another order are another title.
        assert_eq!(similarity("Attention is all you need", "Is attention all you need?"), 0.8);
        let close = similarity("A single-cell atlas of the root", "A single-cell atlas of the leaf");
        assert!(close > 0.8 && close < 1., "{close}");
    }

    #[test]
    fn candidates_from_two_sources() {
        let fetch = |url: &str| -> Result<String, String> { Ok(if url.contains("crossref") { CROSSREF } else if url.contains("arxiv") { ARXIV } else { EUROPEPMC }.into()) };
        let found = find(&query(), &fetch);
        assert_eq!(found.asked, [Asked { source: "Crossref", found: 3, error: String::new() }, Asked { source: "Europe PMC", found: 1, error: String::new() }, Asked { source: "arXiv", found: 1, error: String::new() }]);
        // The same DOI from both is one record with what each knows; the preprints stay their own; the unlike one is left out.
        assert_eq!(found.candidates.len(), 3);
        assert_eq!((found.candidates[2].sources.as_slice(), found.candidates[2].paper.doi.as_str(), found.candidates[2].similarity), (["arXiv"].as_slice(), "10.48550/arxiv.1810.00001", 1.));
        let (article, preprint) = (&found.candidates[0], &found.candidates[1]);
        assert_eq!((article.sources.as_slice(), article.paper.pmid.as_str(), article.paper.abstract_text.as_str(), article.paper.authors.as_str()), (["Crossref", "Europe PMC"].as_slice(), "31004836", "Cells of the root.", "Zhang, Tian-Qi; Wang, Jia-Wei"));
        assert_eq!((article.same.as_slice(), article.differs.is_empty(), article.partial_authors), (["题目一致".to_string(), "年份 2019".into(), "作者里有 Wang".into()].as_slice(), true, false));
        assert_eq!((preprint.paper.publication_status.as_str(), preprint.paper.journal.as_str(), preprint.sources.as_slice()), ("preprint", "bioRxiv", ["Crossref"].as_slice()));
        assert_eq!(preprint.differs, ["年份是 2018，不是 2019", "作者里没有 Wang"]);

        // One source down: said for that source, and the other's records stand.
        let half = |url: &str| -> Result<String, String> { if url.contains("crossref") { Err("连接超时".into()) } else if url.contains("arxiv") { Ok("<feed></feed>".into()) } else { Ok(EUROPEPMC.into()) } };
        let found = find(&query(), &half);
        assert_eq!((found.asked[0].error.as_str(), found.asked[1].found, found.candidates.len()), ("连接超时", 1, 1));
        assert!(found.candidates[0].partial_authors);
        // Nothing as a phrase: Europe PMC is asked again by the words. Nothing found is not a failure.
        let asked = std::cell::RefCell::new(vec![]);
        let none = |url: &str| -> Result<String, String> {
            asked.borrow_mut().push(url.to_string());
            Ok(if url.contains("crossref") { r#"{"message":{"items":[]}}"#.into() } else if url.contains("arxiv") { "<feed></feed>".into() } else { r#"{"resultList":{"result":[]}}"#.into() })
        };
        let found = find(&query(), &none);
        assert!(found.candidates.is_empty() && found.asked.iter().all(|asked| asked.found == 0 && asked.error.is_empty()));
        assert_eq!((asked.borrow().len(), asked.borrow()[2].contains("TITLE%3A%28")), (4, true));
    }

    #[test]
    fn fields_taken() {
        let fetch = |url: &str| -> Result<String, String> { Ok(if url.contains("crossref") { CROSSREF } else if url.contains("arxiv") { ARXIV } else { EUROPEPMC }.into()) };
        let found = find(&query(), &fetch);
        let candidate = &found.candidates[0];
        let mut paper = Paper { id: "P-1".into(), title: "zhang2019".into(), year: Some(2018), source: "PDF 导入".into(), publication_status: "unknown".into(), ..Default::default() };
        let files = [PaperFile { paper_id: "P-1".into(), title: "zhang2019.pdf".into(), ..Default::default() }];
        assert!(placeholder_title(&paper, &files) && !placeholder_title(&Paper { title: "A real title".into(), ..paper.clone() }, &files));
        assert_eq!(offered(&paper, &candidate.paper), ["title", "authors", "year", "journal", "status", "doi", "pmid", "abstract"]);
        // The year the paper has is not replaced unasked; a title that is one, neither.
        assert_eq!(suggested(&paper, &candidate.paper, true), ["title", "authors", "journal", "status", "doi", "pmid", "abstract"]);
        assert_eq!(suggested(&paper, &candidate.paper, false), ["authors", "journal", "status", "doi", "pmid", "abstract"]);
        apply(&mut paper, candidate, &["title", "authors", "doi"]);
        assert_eq!((paper.title.as_str(), paper.authors.as_str(), paper.doi.as_str(), paper.year, paper.journal.as_str()), (candidate.paper.title.as_str(), "Zhang, Tian-Qi; Wang, Jia-Wei", "10.1104/pp.18.01482", Some(2018), ""));
        assert_eq!(paper.source, "PDF 导入；按题目从 Crossref、Europe PMC 补全");
        // Completed again, the note is said once.
        apply(&mut paper, &found.candidates[1], &["year"]);
        assert_eq!((paper.year, paper.source.as_str()), (Some(2018), "PDF 导入；按题目从 Crossref 补全"));
        assert_eq!((shown(&paper, "status"), shown(&candidate.paper, "status")), (String::new(), "正式发表".to_string()));
    }
}
