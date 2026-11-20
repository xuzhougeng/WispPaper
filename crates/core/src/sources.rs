//! Searching the literature databases: which URLs to ask and how to read
//! what comes back. No HTTP here; the caller hands in the function that
//! fetches a URL, so the same code runs in the app, in `wisp` and in tests.
//!
//! PubMed, Europe PMC, Crossref and arXiv need no key. Each takes its own
//! query syntax (`syntax`), which is what a saved search stores.

use crate::lookup::{crossref_work, europepmc_hit};
use crate::model::{Paper, normalize_doi, strip_markup};
use serde_json::Value;

/// The databases: (stored key, name shown).
pub const KINDS: [(&str, &str); 4] = [("pubmed", "PubMed"), ("europepmc", "Europe PMC"), ("crossref", "Crossref"), ("arxiv", "arXiv")];

/// The most records one search may bring back.
pub const MAX_RESULTS: usize = 2000;

/// The most records asked for in one request. A search that wants more
/// asks again for the next page, until it has them or the database has no more.
pub const PAGE: usize = 200;

/// How many records a search taken as it stands, without the person setting
/// a number, should ask for: all that a database sorted by date will give,
/// and one page of Crossref, which ranks loose matches by relevance and has
/// hundreds of thousands of them for any few words.
pub fn broad_limit(kind: &str) -> usize {
    if kind == "crossref" { PAGE } else { MAX_RESULTS }
}

/// GET a URL and return the body.
pub type Fetch<'a> = &'a dyn Fn(&str) -> Result<String, String>;

/// GET a file and return its bytes: a PDF, most of the time.
pub type Download<'a> = &'a dyn Fn(&str) -> Result<Vec<u8>, String>;

/// Several reasons as one sentence: each without the full stop it came
/// with, a semicolon between them and one full stop after the last, so
/// that a failure quoted among others does not read 「HTTP 403。；」.
pub fn reasons(parts: &[String]) -> String {
    let parts: Vec<&str> = parts.iter().map(|part| part.trim().trim_end_matches(['。', '.', '；', ';']).trim_end()).filter(|part| !part.is_empty()).collect();
    if parts.is_empty() { String::new() } else { format!("{}。", parts.join("；")) }
}

/// What a search found: how many records the database holds for the query
/// (when it says), and the newest of them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    pub total: Option<u64>,
    pub papers: Vec<Paper>,
    /// Why a later page did not come, when one did not: the papers are those of the pages before it.
    pub stopped: Option<String>,
    /// Records the library holds already and the search therefore did not fetch (`Known`).
    pub passed: usize,
}

/// What the caller knows of its library, for a search that runs again and
/// need not fetch again what the last run brought.
pub struct Known<'a> {
    /// Whether the library holds this record. PubMed is asked before it
    /// fetches a record, with the PMID alone.
    pub holds: &'a dyn Fn(&Paper) -> bool,
    /// The count the database gave when an earlier run went as deep as this
    /// one is to go; `None` when none did, and every page is asked for.
    pub since: Option<u64>,
}

/// Where a search stands between two of its requests.
#[derive(Default)]
struct Walk {
    /// The records the database has sent so far, those that could not be read included.
    sent: usize,
    total: Option<u64>,
    /// PubMed: the ids of the run that the library does not hold, asked for once.
    ids: Option<Vec<String>>,
    /// PubMed: how many ids the library held.
    passed: usize,
    /// Europe PMC: the mark its last page gave for the next one.
    cursor: Option<String>,
    /// The database has no more.
    done: bool,
}

/// How a query is written for a database, for whoever composes one.
pub fn syntax(kind: &str) -> &'static str {
    match kind {
        "pubmed" => "PubMed 检索式：布尔运算符 AND / OR / NOT 要大写，用括号分组，短语加引号；可以用字段标签，如 \"single cell\"[Title/Abstract]、Arabidopsis[MeSH Terms]、2020:2025[dp]；词尾 * 表示截词。",
        "europepmc" => "Europe PMC 检索式：布尔运算符 AND / OR / NOT 要大写，用括号分组，短语加引号；字段写在前面，如 TITLE:\"root\"、ABSTRACT:\"single cell\"、PUB_YEAR:[2020 TO 2025]、SRC:PPR（只要预印本）。",
        "crossref" => "Crossref 按词语匹配题目、作者和期刊，不支持布尔运算符和字段：只写 3 到 6 个最有区分度的词。",
        "arxiv" => "arXiv 检索式：字段前缀 ti:（题目）、abs:（摘要）、au:（作者）、cat:（分类，如 cs.CL）、all:（全部）；布尔运算符 AND / OR / ANDNOT 要大写，用括号分组，短语加引号。",
        _ => "",
    }
}

fn encode(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { out.push(b as char) } else { out.push_str(&format!("%{b:02X}")) }
    }
    out
}

/// Run `query` against one database and return up to `limit` of the newest
/// records, a page at a time. A page that fails after others came ends the
/// search with what it has (`Found::stopped`); the first one failing is an error.
pub fn search(kind: &str, query: &str, limit: usize, fetch: Fetch) -> Result<Found, String> {
    search_new(kind, query, limit, fetch, None)
}

/// `search` for a library that may hold most of the records already. PubMed
/// names its records before it sends them, so those the library holds are
/// never fetched. The others send whole pages: once a run has gone as deep
/// as this one (`Known::since`), a page with nothing new ends the walk,
/// unless the database has grown by more records than were found, which
/// means a new one stands further down.
pub fn search_new(kind: &str, query: &str, limit: usize, fetch: Fetch, known: Option<&Known>) -> Result<Found, String> {
    let query = query.trim();
    if query.is_empty() { return Err("请填写检索式。".into()) }
    let limit = limit.clamp(1, MAX_RESULTS);
    let mut found = Found::default();
    let mut walk = Walk::default();
    let mut fresh = 0;
    while !walk.done && walk.sent < limit {
        match next_page(kind, query, limit, &mut walk, fetch, known) {
            Ok(papers) => {
                let new = known.map_or(papers.len(), |known| papers.iter().filter(|paper| !(known.holds)(paper)).count());
                fresh += new as u64;
                found.papers.extend(papers);
                let grown = |since: u64| walk.total.unwrap_or(0).saturating_sub(since);
                if new == 0 && kind != "pubmed" && known.and_then(|known| known.since).is_some_and(|since| fresh >= grown(since)) { break }
            }
            Err(error) if walk.sent > 0 => {
                found.stopped = Some(error);
                break;
            }
            Err(error) => return Err(error),
        }
    }
    (found.total, found.passed) = (walk.total, walk.passed);
    let name = KINDS.iter().find(|(key, _)| *key == kind).map_or(kind, |(_, name)| name);
    for paper in &mut found.papers {
        paper.source = name.to_string();
    }
    found.papers.truncate(limit);
    Ok(found)
}

/// The next page of a search: what could be read of it, with `walk` moved past it.
fn next_page(kind: &str, query: &str, limit: usize, walk: &mut Walk, fetch: Fetch, known: Option<&Known>) -> Result<Vec<Paper>, String> {
    let json = |url: &str| -> Result<Value, String> { serde_json::from_str(&fetch(url)?).map_err(|e| format!("无法解析返回内容：{e}")) };
    let ask = PAGE.min(limit - walk.sent);
    let (papers, sent) = match kind {
        "pubmed" => {
            let base = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils";
            if walk.ids.is_none() {
                let hits = json(&format!("{base}/esearch.fcgi?db=pubmed&retmode=json&sort=pub+date&retmax={limit}&term={}", encode(query)))?;
                let result = &hits["esearchresult"];
                // PubMed answers a query it cannot read with an error inside a normal reply.
                if let Some(error) = result["ERROR"].as_str() { return Err(format!("PubMed 没有接受这个检索式：{error}")) }
                walk.total = result["count"].as_str().and_then(|n| n.parse().ok());
                let ids: Vec<String> = result["idlist"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
                let wanted: Vec<String> = ids.iter().filter(|id| !known.is_some_and(|known| (known.holds)(&Paper { pmid: id.to_string(), ..Default::default() }))).cloned().collect();
                walk.passed = ids.len() - wanted.len();
                walk.ids = Some(wanted);
            }
            // The ids came in one reply; the records are fetched a page of them at a time.
            let ids = walk.ids.as_deref().unwrap_or_default();
            let part = &ids[walk.sent.min(ids.len())..(walk.sent + ask).min(ids.len())];
            if walk.sent + part.len() >= ids.len() { walk.done = true }
            let papers = if part.is_empty() { vec![] } else { pubmed_papers(&fetch(&format!("{base}/efetch.fcgi?db=pubmed&retmode=xml&id={}", part.join(",")))?) };
            (papers, part.len())
        }
        "europepmc" => {
            let body = json(&format!("https://www.ebi.ac.uk/europepmc/webservices/rest/search?format=json&resultType=core&sort={}&pageSize={ask}&cursorMark={}&query={}", encode("P_PDATE_D desc"), encode(walk.cursor.as_deref().unwrap_or("*")), encode(query)))?;
            let hits = body["resultList"]["result"].as_array().map(Vec::as_slice).unwrap_or_default();
            // The last page gives no mark, or the one it was asked with.
            let next = body["nextCursorMark"].as_str().filter(|next| Some(*next) != walk.cursor.as_deref()).map(str::to_string);
            if next.is_none() { walk.done = true }
            walk.cursor = next;
            walk.total = walk.total.or(body["hitCount"].as_u64());
            (hits.iter().filter_map(europepmc_hit).collect(), hits.len())
        }
        "crossref" => {
            let body = json(&format!("https://api.crossref.org/works?rows={ask}&offset={}&select={}&query.bibliographic={}", walk.sent, encode("DOI,title,author,published,published-print,published-online,issued,container-title,abstract,type,URL,publisher"), encode(query)))?;
            let items = body["message"]["items"].as_array().map(Vec::as_slice).unwrap_or_default();
            walk.total = walk.total.or(body["message"]["total-results"].as_u64());
            (items.iter().filter_map(crossref_work).collect(), items.len())
        }
        "arxiv" => {
            let xml = fetch(&format!("https://export.arxiv.org/api/query?sortBy=submittedDate&sortOrder=descending&start={}&max_results={ask}&search_query={}", walk.sent, encode(query)))?;
            let found = arxiv_found(&xml);
            let sent = elements(&xml, "entry").len();
            walk.total = walk.total.or(found.total);
            // arXiv now and then answers a later page with nothing in it.
            if sent == 0 && walk.total.is_some_and(|total| (walk.sent as u64) < total) && walk.sent > 0 { return Err("arXiv 没有返回后面的记录，请稍后再运行一次。".into()) }
            (found.papers, sent)
        }
        other => return Err(format!("不支持的来源：{other}。可用：{}", KINDS.iter().map(|(key, _)| *key).collect::<Vec<_>>().join("、"))),
    };
    walk.sent += sent;
    if sent < ask || walk.total.is_some_and(|total| (walk.sent + walk.passed) as u64 >= total) { walk.done = true }
    Ok(papers)
}

// ---- XML, as far as PubMed and arXiv need it

/// The attributes and the inside of every `<tag …>…</tag>` in `xml`, in
/// order. Enough for the flat records these services return; an element
/// nested in one of its own kind is not handled.
fn elements<'a>(xml: &'a str, tag: &str) -> Vec<(&'a str, &'a str)> {
    let (open, close) = (format!("<{tag}"), format!("</{tag}>"));
    let mut found = vec![];
    let mut rest = xml;
    while let Some(at) = rest.find(&open) {
        let after = &rest[at + open.len()..];
        // `<Author` must not match `<AuthorList`.
        if !after.starts_with(|c: char| c == '>' || c == '/' || c.is_whitespace()) {
            rest = after;
            continue;
        }
        let Some(end) = after.find('>') else { break };
        let attributes = &after[..end];
        let body = &after[end + 1..];
        if attributes.ends_with('/') {
            rest = body;
            continue;
        }
        let Some(stop) = body.find(&close) else { break };
        found.push((attributes, &body[..stop]));
        rest = &body[stop + close.len()..];
    }
    found
}

fn element<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    elements(xml, tag).first().map(|(_, inside)| *inside)
}

/// The text of the first `<tag>`, without markup.
fn text_of(xml: &str, tag: &str) -> String {
    element(xml, tag).map(strip_markup).unwrap_or_default()
}

fn attribute(attributes: &str, name: &str) -> Option<String> {
    let at = attributes.find(&format!("{name}=\""))?;
    let value = &attributes[at + name.len() + 2..];
    Some(value[..value.find('"')?].to_string())
}

fn year_in(text: &str) -> Option<i32> {
    let digits: String = text.chars().skip_while(|c| !c.is_ascii_digit()).take(4).collect();
    digits.parse().ok().filter(|y| (1800..=2200).contains(y))
}

/// The records of a PubMed `efetch` reply.
pub fn pubmed_papers(xml: &str) -> Vec<Paper> {
    let mut papers = vec![];
    for (_, article) in elements(xml, "PubmedArticle") {
        let title = text_of(article, "ArticleTitle").trim_end_matches('.').to_string();
        let pmid = text_of(article, "PMID");
        if title.is_empty() || pmid.is_empty() { continue }
        let authors: Vec<String> = element(article, "AuthorList")
            .map(|list| {
                elements(list, "Author")
                    .iter()
                    .filter_map(|(_, author)| {
                        let (family, given) = (text_of(author, "LastName"), text_of(author, "ForeName"));
                        let name = if family.is_empty() { text_of(author, "CollectiveName") } else if given.is_empty() { family } else { format!("{family}, {given}") };
                        (!name.is_empty()).then_some(name)
                    })
                    .collect()
            })
            .unwrap_or_default();
        // A structured abstract comes in labelled parts.
        let parts: Vec<String> = element(article, "Abstract")
            .map(|block| {
                elements(block, "AbstractText")
                    .iter()
                    .map(|(attributes, part)| match attribute(attributes, "Label") {
                        Some(label) if !label.is_empty() => format!("{label}: {}", strip_markup(part)),
                        _ => strip_markup(part),
                    })
                    .filter(|part| !part.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        // The article's own identifiers: the list in PubmedData, before the references and theirs.
        let own = element(article, "PubmedData").map(|data| data.split("<ReferenceList").next().unwrap_or(data)).unwrap_or("");
        let doi = elements(article, "ELocationID")
            .into_iter()
            .chain(elements(own, "ArticleId"))
            .find(|(attributes, _)| attributes.contains("\"doi\""))
            .map(|(_, value)| normalize_doi(&strip_markup(value)))
            .unwrap_or_default();
        let year = element(article, "PubDate").and_then(year_in).or_else(|| element(article, "ArticleDate").and_then(year_in));
        let journal = element(article, "Journal");
        // `357-62` as PubMed writes it, or the first and last page apart.
        let pages = element(article, "Pagination").map(|pagination| match text_of(pagination, "MedlinePgn") {
            written if !written.is_empty() => written,
            _ => [text_of(pagination, "StartPage"), text_of(pagination, "EndPage")].into_iter().filter(|page| !page.is_empty()).collect::<Vec<_>>().join("-"),
        }).unwrap_or_default();
        let preprint = elements(article, "PublicationType").iter().any(|(_, kind)| kind.trim() == "Preprint");
        papers.push(Paper {
            title,
            pmid,
            doi,
            authors: authors.join("; "),
            journal: journal.map(|journal| text_of(journal, "Title")).unwrap_or_default(),
            volume: journal.map(|journal| text_of(journal, "Volume")).unwrap_or_default(),
            issue: journal.map(|journal| text_of(journal, "Issue")).unwrap_or_default(),
            pages: crate::model::clean_pages(&pages),
            year,
            abstract_text: parts.join("\n"),
            publication_status: if preprint { "preprint" } else { "published" }.into(),
            ..Default::default()
        });
    }
    papers
}

/// An arXiv API reply (Atom).
pub fn arxiv_found(xml: &str) -> Found {
    let mut papers = vec![];
    for (_, entry) in elements(xml, "entry") {
        let title = text_of(entry, "title");
        // `http://arxiv.org/abs/1706.03762v7`: the id without its version.
        let id = text_of(entry, "id");
        let number = id.rsplit("/abs/").next().unwrap_or("").trim();
        let number = number.rfind('v').filter(|at| number[at + 1..].bytes().all(|b| b.is_ascii_digit()) && *at + 1 < number.len()).map_or(number, |at| &number[..at]);
        if title.is_empty() || number.is_empty() { continue }
        // A published version's DOI when the authors gave one, else arXiv's own.
        let doi = element(entry, "arxiv:doi").map(strip_markup).filter(|doi| !doi.is_empty());
        papers.push(Paper {
            title,
            authors: elements(entry, "author").iter().map(|(_, author)| text_of(author, "name")).filter(|name| !name.is_empty()).collect::<Vec<_>>().join("; "),
            year: year_in(&text_of(entry, "published")),
            journal: text_of(entry, "arxiv:journal_ref"),
            publication_status: if doi.is_some() { "published" } else { "preprint" }.into(),
            doi: normalize_doi(&doi.unwrap_or_else(|| format!("10.48550/arxiv.{number}"))),
            url: format!("https://arxiv.org/abs/{number}"),
            abstract_text: text_of(entry, "summary"),
            ..Default::default()
        });
    }
    Found { total: element(xml, "opensearch:totalResults").and_then(|n| n.trim().parse().ok()), papers, ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const PUBMED: &str = r#"<?xml version="1.0" ?><PubmedArticleSet>
<PubmedArticle><MedlineCitation Status="MEDLINE"><PMID Version="1">31178400</PMID>
<Article PubModel="Print-Electronic"><Journal><ISSN>1534-5807</ISSN><JournalIssue CitedMedium="Internet"><Volume>48</Volume><Issue>6</Issue><PubDate><Year>2019</Year><Month>Mar</Month></PubDate></JournalIssue><Title>Developmental cell</Title></Journal>
<ArticleTitle>A Single-Cell RNA Sequencing Profiles the Developmental Landscape of <i>Arabidopsis</i> Root.</ArticleTitle>
<Pagination><StartPage>840</StartPage><EndPage>852.e5</EndPage><MedlinePgn>840-852.e5</MedlinePgn></Pagination><ELocationID EIdType="pii" ValidYN="Y">S1534-5807(19)30145-4</ELocationID><ELocationID EIdType="doi" ValidYN="Y">10.1016/j.devcel.2019.02.022</ELocationID>
<Abstract><AbstractText Label="BACKGROUND">Roots &amp; cells.</AbstractText><AbstractText Label="RESULTS">We profiled <sup>7,000</sup> cells.</AbstractText></Abstract>
<AuthorList CompleteYN="Y"><Author ValidYN="Y"><LastName>Zhang</LastName><ForeName>Tian-Qi</ForeName><Initials>TQ</Initials></Author><Author><CollectiveName>Root Consortium</CollectiveName></Author></AuthorList>
<PublicationTypeList><PublicationType UI="D016428">Journal Article</PublicationType></PublicationTypeList></Article></MedlineCitation>
<PubmedData><ArticleIdList><ArticleId IdType="pubmed">31178400</ArticleId></ArticleIdList>
<ReferenceList><Reference><ArticleIdList><ArticleId IdType="doi">10.9999/cited</ArticleId></ArticleIdList></Reference></ReferenceList></PubmedData></PubmedArticle>
<PubmedArticle><MedlineCitation><PMID>2</PMID><Article><Journal><JournalIssue><PubDate><MedlineDate>2021 Jan-Feb</MedlineDate></PubDate></JournalIssue><Title>bioRxiv</Title></Journal>
<ArticleTitle>No DOI in the article</ArticleTitle><PublicationTypeList><PublicationType>Preprint</PublicationType></PublicationTypeList></Article></MedlineCitation>
<PubmedData><ArticleIdList><ArticleId IdType="pubmed">2</ArticleId><ArticleId IdType="doi">10.1101/2021.01.01.1</ArticleId></ArticleIdList></PubmedData></PubmedArticle>
<PubmedArticle><MedlineCitation><PMID>3</PMID><Article><ArticleTitle></ArticleTitle></Article></MedlineCitation></PubmedArticle>
</PubmedArticleSet>"#;

    #[test]
    fn pubmed_records() {
        let papers = pubmed_papers(PUBMED);
        assert_eq!(papers.len(), 2);
        let p = &papers[0];
        assert_eq!(p.title, "A Single-Cell RNA Sequencing Profiles the Developmental Landscape of Arabidopsis Root");
        assert_eq!((p.pmid.as_str(), p.doi.as_str(), p.year), ("31178400", "10.1016/j.devcel.2019.02.022", Some(2019)));
        assert_eq!((p.authors.as_str(), p.journal.as_str()), ("Zhang, Tian-Qi; Root Consortium", "Developmental cell"));
        assert_eq!((p.volume.as_str(), p.issue.as_str(), p.pages.as_str()), ("48", "6", "840-852.e5"));
        assert_eq!((papers[1].volume.as_str(), papers[1].pages.as_str()), ("", ""));
        assert_eq!(p.abstract_text, "BACKGROUND: Roots & cells.\nRESULTS: We profiled 7,000 cells.");
        assert_eq!(p.publication_status, "published");
        // The DOI of the record, never one from its reference list.
        assert_eq!((papers[1].doi.as_str(), papers[1].year, papers[1].publication_status.as_str()), ("10.1101/2021.01.01.1", Some(2021), "preprint"));
    }

    #[test]
    fn arxiv_records() {
        let xml = r#"<feed xmlns="http://www.w3.org/2005/Atom"><title>ArXiv Query</title><opensearch:totalResults xmlns:opensearch="http://a9.com/-/spec/opensearch/1.1/">1234</opensearch:totalResults>
<entry><id>http://arxiv.org/abs/1706.03762v7</id><published>2017-06-12T17:57:34Z</published><title>Attention Is All
  You Need</title><summary>  The dominant sequence transduction models.
</summary><author><name>Ashish Vaswani</name></author><author><name>Noam Shazeer</name></author><link href="http://arxiv.org/abs/1706.03762v7" rel="alternate" type="text/html"/></entry>
<entry><id>http://arxiv.org/abs/cs/9901001v1</id><published>1999-01-02T00:00:00Z</published><title>Old style id</title><summary>S</summary><arxiv:doi xmlns:arxiv="http://arxiv.org/schemas/atom">10.1000/Published</arxiv:doi><arxiv:journal_ref xmlns:arxiv="http://arxiv.org/schemas/atom">J. Old 1</arxiv:journal_ref></entry></feed>"#;
        let found = arxiv_found(xml);
        assert_eq!((found.total, found.papers.len()), (Some(1234), 2));
        let p = &found.papers[0];
        assert_eq!((p.title.as_str(), p.authors.as_str(), p.year), ("Attention Is All You Need", "Ashish Vaswani; Noam Shazeer", Some(2017)));
        assert_eq!((p.doi.as_str(), p.url.as_str(), p.publication_status.as_str()), ("10.48550/arxiv.1706.03762", "https://arxiv.org/abs/1706.03762", "preprint"));
        assert_eq!(p.abstract_text, "The dominant sequence transduction models.");
        let old = &found.papers[1];
        assert_eq!((old.doi.as_str(), old.url.as_str(), old.journal.as_str(), old.publication_status.as_str()), ("10.1000/published", "https://arxiv.org/abs/cs/9901001", "J. Old 1", "published"));
    }

    #[test]
    fn searches_ask_the_right_urls() {
        let asked = RefCell::new(vec![]);
        let fetch = |url: &str| -> Result<String, String> {
            asked.borrow_mut().push(url.to_string());
            Ok(if url.contains("esearch") {
                r#"{"esearchresult":{"count":"321","idlist":["31178400","2"]}}"#.to_string()
            } else if url.contains("efetch") {
                PUBMED.to_string()
            } else if url.contains("europepmc") {
                r#"{"hitCount":7,"resultList":{"result":[{"title":"Root atlas.","pubYear":"2020","pmid":"9","authorString":"Li N."}]}}"#.to_string()
            } else if url.contains("crossref") {
                r#"{"message":{"total-results":50,"items":[{"title":["A"],"DOI":"10.1000/A"},{"title":["B"],"DOI":"10.1000/b"},{"DOI":"10.1000/untitled"}]}}"#.to_string()
            } else {
                "<feed></feed>".to_string()
            })
        };
        let pubmed = search("pubmed", " \"single cell\"[Title] AND root ", 20, &fetch).unwrap();
        assert_eq!((pubmed.total, pubmed.papers.len(), pubmed.papers[0].source.as_str()), (Some(321), 2, "PubMed"));
        assert!(asked.borrow()[0].ends_with("retmax=20&term=%22single%20cell%22%5BTitle%5D%20AND%20root"), "{}", asked.borrow()[0]);
        assert!(asked.borrow()[1].ends_with("&id=31178400,2"));

        let pmc = search("europepmc", "TITLE:root", 500, &fetch).unwrap();
        assert_eq!((pmc.total, pmc.papers[0].title.as_str(), pmc.papers[0].source.as_str()), (Some(7), "Root atlas", "Europe PMC"));
        assert!(asked.borrow()[2].contains("pageSize=200&cursorMark=%2A&query=TITLE%3Aroot"));

        // The limit holds even when a service sends more than it was asked for.
        let crossref = search("crossref", "root atlas", 1, &fetch).unwrap();
        assert_eq!((crossref.total, crossref.papers.len(), crossref.papers[0].doi.as_str()), (Some(50), 1, "10.1000/a"));
        assert_eq!(search("arxiv", "all:root", 5, &fetch).unwrap(), Found::default());
        assert!(asked.borrow()[4].starts_with("https://export.arxiv.org/api/query?") && asked.borrow()[4].ends_with("search_query=all%3Aroot"));

        assert!(search("pubmed", "  ", 5, &fetch).unwrap_err().contains("检索式"));
        assert!(search("scholar", "x", 5, &fetch).unwrap_err().contains("pubmed"));
        let refused = |_: &str| -> Result<String, String> { Ok(r#"{"esearchresult":{"ERROR":"Invalid field"}}"#.to_string()) };
        assert!(search("pubmed", "x[zz]", 5, &refused).unwrap_err().contains("Invalid field"));
        let offline = |_: &str| -> Result<String, String> { Err("网络请求失败".into()) };
        assert_eq!(search("crossref", "x", 5, &offline), Err("网络请求失败".into()));
        assert!(KINDS.iter().all(|(kind, _)| !syntax(kind).is_empty()));
    }

    /// The number after `name=` in a URL.
    fn asked_for(url: &str, name: &str) -> usize {
        url.split(['?', '&']).find_map(|part| part.strip_prefix(name)?.strip_prefix('=')?.parse().ok()).unwrap()
    }

    #[test]
    fn a_search_asks_page_after_page() {
        let asked = RefCell::new(vec![]);
        // Every database holds 450 records for the query and sends as many as it is asked for.
        let fetch = |url: &str| -> Result<String, String> {
            asked.borrow_mut().push(url.to_string());
            let records = |from: usize, ask: usize| from..(from + ask).min(450);
            Ok(if url.contains("esearch") {
                format!(r#"{{"esearchresult":{{"count":"450","idlist":[{}]}}}}"#, (0..asked_for(url, "retmax").min(450)).map(|n| format!("\"{n}\"")).collect::<Vec<_>>().join(","))
            } else if url.contains("efetch") {
                url.rsplit("id=").next().unwrap().split(',').map(|id| format!("<PubmedArticle><PMID>{id}</PMID><ArticleTitle>Paper {id}</ArticleTitle></PubmedArticle>")).collect()
            } else if url.contains("europepmc") {
                let from = if url.contains("cursorMark=%2A") { 0 } else { asked_for(url, "cursorMark") };
                let hits: Vec<String> = records(from, asked_for(url, "pageSize")).map(|n| format!(r#"{{"title":"Paper {n}","pmid":"{n}"}}"#)).collect();
                format!(r#"{{"hitCount":450,"nextCursorMark":"{}","resultList":{{"result":[{}]}}}}"#, from + hits.len(), hits.join(","))
            } else if url.contains("crossref") {
                let items: Vec<String> = records(asked_for(url, "offset"), asked_for(url, "rows")).map(|n| format!(r#"{{"title":["Paper {n}"],"DOI":"10.1000/{n}"}}"#)).collect();
                format!(r#"{{"message":{{"total-results":450,"items":[{}]}}}}"#, items.join(","))
            } else {
                let entries: String = records(asked_for(url, "start"), asked_for(url, "max_results")).map(|n| format!("<entry><id>http://arxiv.org/abs/2401.{n:05}v1</id><title>Paper {n}</title></entry>")).collect();
                format!("<feed><opensearch:totalResults>450</opensearch:totalResults>{entries}</feed>")
            })
        };
        for (kind, _) in KINDS {
            // More than the database has: everything, and no request past the end.
            asked.borrow_mut().clear();
            let all = search(kind, "root", 1000, &fetch).unwrap();
            assert_eq!((all.total, all.papers.len(), all.stopped.as_deref()), (Some(450), 450, None), "{kind}");
            assert_eq!((all.papers[0].title.as_str(), all.papers[449].title.as_str()), ("Paper 0", "Paper 449"), "{kind}");
            assert_eq!(asked.borrow().len(), if kind == "pubmed" { 4 } else { 3 }, "{kind}: {:?}", asked.borrow());
            // Fewer: the last page is asked for no more than what is missing.
            asked.borrow_mut().clear();
            assert_eq!(search(kind, "root", 250, &fetch).unwrap().papers.len(), 250, "{kind}");
            let last = asked.borrow().last().cloned().unwrap();
            match kind {
                "pubmed" => assert_eq!(last.rsplit("id=").next().unwrap().split(',').count(), 50),
                "europepmc" => assert!(last.contains("pageSize=50&cursorMark=200&"), "{last}"),
                "crossref" => assert!(last.contains("rows=50&offset=200&"), "{last}"),
                _ => assert!(last.contains("start=200&max_results=50&"), "{last}"),
            }
            // One page is one request, as before.
            asked.borrow_mut().clear();
            assert_eq!(search(kind, "root", PAGE, &fetch).unwrap().papers.len(), PAGE);
            assert_eq!(asked.borrow().len(), if kind == "pubmed" { 2 } else { 1 }, "{kind}");
        }
        assert_eq!(search("crossref", "root", 99_999, &fetch).unwrap().papers.len(), 450);
    }

    #[test]
    fn a_search_run_again_fetches_what_is_new() {
        let asked = RefCell::new(vec![]);
        // 450 records, numbered from the newest.
        let fetch = |url: &str| -> Result<String, String> {
            asked.borrow_mut().push(url.to_string());
            Ok(if url.contains("esearch") {
                format!(r#"{{"esearchresult":{{"count":"450","idlist":[{}]}}}}"#, (0..450).map(|n| format!("\"{n}\"")).collect::<Vec<_>>().join(","))
            } else if url.contains("efetch") {
                url.rsplit("id=").next().unwrap().split(',').map(|id| format!("<PubmedArticle><PMID>{id}</PMID><ArticleTitle>Paper {id}</ArticleTitle></PubmedArticle>")).collect()
            } else {
                let from = if url.contains("cursorMark=%2A") { 0 } else { asked_for(url, "cursorMark") };
                let hits: Vec<String> = (from..(from + asked_for(url, "pageSize")).min(450)).map(|n| format!(r#"{{"title":"Paper {n}","pmid":"{n}"}}"#)).collect();
                format!(r#"{{"hitCount":450,"nextCursorMark":"{}","resultList":{{"result":[{}]}}}}"#, from + hits.len(), hits.join(","))
            })
        };
        // The library holds all but these.
        let run = |kind: &str, missing: &[&str], since: Option<u64>| {
            asked.borrow_mut().clear();
            let holds = |paper: &Paper| !missing.contains(&paper.pmid.as_str());
            let found = search_new(kind, "root", 1000, &fetch, Some(&Known { holds: &holds, since })).unwrap();
            (found.papers.iter().filter(|paper| missing.contains(&paper.pmid.as_str())).count(), found.papers.len(), found.passed, asked.borrow().len())
        };
        // PubMed names its records first: only the missing ones are fetched, wherever they stand and whatever ran before.
        assert_eq!(run("pubmed", &["3", "300"], None), (2, 2, 448, 2));
        assert_eq!(run("pubmed", &[], None), (0, 0, 450, 1));
        // Europe PMC sends pages. No run went this deep yet: all of them.
        assert_eq!(run("europepmc", &[], None), (0, 450, 0, 3));
        // One did, and the database has not grown since: the first page says so.
        assert_eq!(run("europepmc", &[], Some(450)), (0, 200, 0, 1));
        // Grown by one, which is at the top: the page after it ends the walk.
        assert_eq!(run("europepmc", &["3"], Some(449)), (1, 400, 0, 2));
        // Grown by one that stands further down: looked for until it is found.
        assert_eq!(run("europepmc", &["300"], Some(449)), (1, 450, 0, 3));
    }

    #[test]
    fn a_page_that_fails_keeps_the_ones_before_it() {
        let count = RefCell::new(0);
        let page = |from: usize| (from..from + PAGE).map(|n| format!(r#"{{"title":["Paper {n}"],"DOI":"10.1000/{n}"}}"#)).collect::<Vec<_>>().join(",");
        let breaks = |url: &str| -> Result<String, String> {
            *count.borrow_mut() += 1;
            if *count.borrow() == 3 { return Err("网络请求失败".into()) }
            Ok(format!(r#"{{"message":{{"total-results":5000,"items":[{}]}}}}"#, page(asked_for(url, "offset"))))
        };
        let found = search("crossref", "root", 1000, &breaks).unwrap();
        assert_eq!((found.total, found.papers.len(), found.stopped.as_deref(), *count.borrow()), (Some(5000), 400, Some("网络请求失败"), 3));
        // arXiv sending an empty page before the end is such a failure, not the end.
        let empty = |url: &str| -> Result<String, String> {
            let entries: String = if asked_for(url, "start") == 0 { (0..PAGE).map(|n| format!("<entry><id>http://arxiv.org/abs/2401.{n:05}v1</id><title>Paper {n}</title></entry>")).collect() } else { String::new() };
            Ok(format!("<feed><opensearch:totalResults>450</opensearch:totalResults>{entries}</feed>"))
        };
        let found = search("arxiv", "all:root", 450, &empty).unwrap();
        assert!(found.papers.len() == PAGE && found.stopped.as_deref().is_some_and(|why| why.contains("arXiv")));
    }
}
