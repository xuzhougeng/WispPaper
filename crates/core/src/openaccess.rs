//! Where the full text of a paper can be had for free: the addresses to
//! ask, and the candidates read from the replies. Only open access is
//! looked for. Nothing here logs in, goes round a paywall, or asks a
//! service that wants to know who is asking. The fetching is the caller's
//! (`sources::Fetch`), as everywhere in this crate.

use crate::lookup::encode;
use crate::model::Paper;
use crate::sources::Fetch;
use serde_json::Value;

/// A file that may be the paper's full text.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub url: String,
    /// Who says the file is there: `arXiv`, `OpenAlex`, `Europe PMC`, or the publisher's name.
    pub source: String,
    /// `published`, `accepted` (the authors' manuscript after review), `preprint`, or empty when unknown.
    pub version: &'static str,
}

/// A version as it is shown.
pub fn version_label(version: &str) -> &'static str {
    match version {
        "published" => "正式版",
        "accepted" => "作者稿",
        "preprint" => "预印本",
        _ => "",
    }
}

/// The arXiv number of a paper that is on arXiv: from its DOI
/// (`10.48550/arXiv.1706.03762`) or its address.
pub fn arxiv_id(paper: &Paper) -> Option<String> {
    let doi = paper.doi.to_lowercase();
    let id = match doi.strip_prefix("10.48550/arxiv.") {
        Some(id) => id.to_string(),
        None => {
            let url = paper.url.to_lowercase();
            let at = url.find("arxiv.org/abs/").map(|at| at + "arxiv.org/abs/".len()).or_else(|| url.find("arxiv.org/pdf/").map(|at| at + "arxiv.org/pdf/".len()))?;
            url[at..].trim_end_matches(".pdf").to_string()
        }
    };
    let id: String = id.chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '/' | '-')).collect();
    (!id.is_empty()).then_some(id)
}

/// Whether a paper has a number it can be looked up by.
pub fn lookable(paper: &Paper) -> bool {
    !paper.doi.is_empty() || !paper.pmid.is_empty() || arxiv_id(paper).is_some()
}

/// What to ask OpenAlex about a DOI. No key, and no address of the asker.
pub fn openalex_url(doi: &str) -> String {
    format!("https://api.openalex.org/works/doi:{}?select=open_access,best_oa_location,locations", encode(doi))
}

fn openalex_version(value: &Value) -> &'static str {
    match value.as_str().unwrap_or("") {
        "publishedVersion" => "published",
        "acceptedVersion" => "accepted",
        "submittedVersion" => "preprint",
        _ => "",
    }
}

/// OpenAlex's reply: whether the paper is open access at all, and the
/// files it knows of, the best one first.
pub fn openalex_candidates(reply: &Value) -> (bool, Vec<Candidate>) {
    let open = reply["open_access"]["is_oa"].as_bool().unwrap_or(false);
    let mut found: Vec<Candidate> = vec![];
    let locations = std::iter::once(&reply["best_oa_location"]).chain(reply["locations"].as_array().into_iter().flatten());
    for location in locations.filter(|location| location["is_oa"].as_bool().unwrap_or(false)) {
        let Some(url) = location["pdf_url"].as_str().filter(|url| url.starts_with("https://") || url.starts_with("http://")) else { continue };
        if found.iter().any(|known| known.url == url) { continue }
        found.push(Candidate { url: url.to_string(), source: "OpenAlex".into(), version: openalex_version(&location["version"]) });
    }
    (open, found)
}

/// What to ask Europe PMC about a paper known by DOI or PMID.
pub fn europepmc_url(paper: &Paper) -> Option<String> {
    let query = if !paper.doi.is_empty() { format!("DOI:\"{}\"", paper.doi) } else if !paper.pmid.is_empty() { format!("EXT_ID:{} AND SRC:MED", paper.pmid) } else { return None };
    Some(format!("https://www.ebi.ac.uk/europepmc/webservices/rest/search?query={}&resultType=core&format=json&pageSize=1", encode(&query)))
}

/// Europe PMC's reply: whether the paper is open access, and the PDFs it
/// lists elsewhere. Its own rendering of a PDF is not among them: that
/// address is for browsers, and says so.
pub fn europepmc_candidates(reply: &Value) -> (bool, Vec<Candidate>) {
    let Some(record) = reply["resultList"]["result"].get(0) else { return (false, vec![]) };
    let open = record["isOpenAccess"].as_str() == Some("Y");
    let links = record["fullTextUrlList"]["fullTextUrl"].as_array().into_iter().flatten();
    let found = links
        .filter(|link| link["documentStyle"].as_str() == Some("pdf") && matches!(link["availabilityCode"].as_str(), Some("OA" | "F")) && link["site"].as_str() != Some("Europe_PMC"))
        .filter_map(|link| Some(Candidate { url: link["url"].as_str()?.to_string(), source: "Europe PMC".into(), version: "" }))
        .collect();
    (open, found)
}

/// The publisher's own address of a paper's PDF, for the publishers whose
/// open-access papers can be had by one: known to work for a plain client
/// in November 2026. Only asked once an index has said the paper is open access.
pub fn publisher_candidate(doi: &str) -> Option<Candidate> {
    let doi = doi.to_lowercase();
    let (prefix, suffix) = doi.split_once('/')?;
    let (url, source) = match prefix {
        "10.1186" | "10.1007" => (format!("https://link.springer.com/content/pdf/{doi}.pdf"), "Springer"),
        "10.1038" => (format!("https://www.nature.com/articles/{suffix}.pdf"), "Nature"),
        "10.3389" => (format!("https://www.frontiersin.org/articles/{doi}/pdf"), "Frontiers"),
        _ => return None,
    };
    Some(Candidate { url, source: source.into(), version: "published" })
}

/// The candidates for a paper, the likeliest first, and what could not be
/// asked. A published version comes before a manuscript, a manuscript
/// before a preprint; among equals the order of asking holds.
pub fn candidates(paper: &Paper, fetch: Fetch) -> (Vec<Candidate>, Vec<String>) {
    let mut found: Vec<Candidate> = vec![];
    let mut notes = vec![];
    let mut open = false;
    let ask = |url: &str, name: &str, notes: &mut Vec<String>| match fetch(url).map(|body| serde_json::from_str::<Value>(&body)) {
        Ok(Ok(reply)) => Some(reply),
        // Not known to the index: no news, and no trouble either.
        Ok(Err(_)) => None,
        Err(err) => {
            if !err.contains("404") { notes.push(format!("{name}：{err}")) }
            None
        }
    };
    if let Some(id) = arxiv_id(paper) { found.push(Candidate { url: format!("https://arxiv.org/pdf/{id}"), source: "arXiv".into(), version: "preprint" }) }
    if !paper.doi.is_empty() && let Some(reply) = ask(&openalex_url(&paper.doi), "OpenAlex", &mut notes) {
        let (is_open, files) = openalex_candidates(&reply);
        open |= is_open;
        found.extend(files);
    }
    if let Some(url) = europepmc_url(paper) && let Some(reply) = ask(&url, "Europe PMC", &mut notes) {
        let (is_open, files) = europepmc_candidates(&reply);
        open |= is_open;
        found.extend(files);
    }
    if open && let Some(own) = publisher_candidate(&paper.doi) { found.push(own) }
    let mut unique: Vec<Candidate> = vec![];
    for candidate in found {
        if !unique.iter().any(|known| known.url == candidate.url) { unique.push(candidate) }
    }
    let rank = |version: &str| match version {
        "published" => 0,
        "accepted" => 1,
        "" => 2,
        _ => 3,
    };
    unique.sort_by_key(|candidate| rank(candidate.version));
    (unique, notes)
}

/// Whether what came back is a PDF and not the page that stands in for
/// one (a login, a "checking your browser", an error).
pub fn is_pdf(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(1024)].windows(5).any(|window| window == b"%PDF-")
}

/// How many of a file's first pages say what it is.
const OPENING_PAGES: usize = 3;

/// Whether a line opens a supplementary figure or table: "Supp. Figure 1.",
/// "Supplementary Table 2", "Supplemental Fig. 3", "Figure S4", "Table S1".
fn supplementary_caption(line: &str) -> bool {
    let line = line.trim_start().to_lowercase();
    let numbered = |rest: &str| rest.trim_start_matches(|c: char| c.is_whitespace() || c == '.').trim_start_matches('s').starts_with(|c: char| c.is_ascii_digit());
    let item = |rest: &str| ["figure", "fig", "table", "movie", "video", "dataset", "data set", "note", "file"].iter().find_map(|name| rest.trim_start().strip_prefix(name)).is_some_and(numbered);
    if let Some(rest) = ["supplementary", "supplemental", "supporting", "supp.", "supp", "suppl.", "suppl"].iter().find_map(|word| line.strip_prefix(word)) { return item(rest) }
    // "Figure S4": the S is what says it.
    ["figure", "fig.", "fig", "table"].iter().find_map(|name| line.strip_prefix(name)).is_some_and(|rest| rest.trim_start().strip_prefix('s').is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit())))
}

/// Whether a line is the heading a supplement stands under: "Supplementary Information", "Supporting Information for".
fn supplementary_heading(line: &str) -> bool {
    let line = line.trim_start().to_lowercase();
    let Some(rest) = ["supplementary", "supplemental", "supporting"].iter().find_map(|word| line.strip_prefix(word)) else { return false };
    ["information", "material", "data", "figures", "tables", "methods", "appendix", "text", "file", "online"].iter().any(|name| rest.trim_start().starts_with(name))
}

/// Whether a file is a paper's supplement and not the paper itself, by the
/// text of its pages: every one of its first pages that says anything
/// holds the caption of a supplementary figure or table, or opens with a
/// supplement's heading. An article names its supplement too, but not at
/// the start of a line on each of its first pages. A repository lists the
/// supplement as a file of the paper, and an index hands it on as its PDF.
pub fn supplement(pages: &[String]) -> bool {
    let opening: Vec<&String> = pages.iter().filter(|page| !page.trim().is_empty()).take(OPENING_PAGES).collect();
    !opening.is_empty() && opening.iter().all(|page| page.lines().any(supplementary_caption) || page.lines().filter(|line| !line.trim().is_empty()).take(3).any(supplementary_heading))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn paper(doi: &str, pmid: &str, url: &str) -> Paper {
        Paper { title: "T".into(), doi: doi.into(), pmid: pmid.into(), url: url.into(), ..Default::default() }
    }

    #[test]
    fn arxiv_numbers() {
        assert_eq!(arxiv_id(&paper("10.48550/arXiv.1706.03762", "", "")), Some("1706.03762".into()));
        assert_eq!(arxiv_id(&paper("", "", "https://arxiv.org/abs/2401.00001v2")), Some("2401.00001v2".into()));
        assert_eq!(arxiv_id(&paper("", "", "http://arxiv.org/pdf/hep-th/9901001.pdf")), Some("hep-th/9901001".into()));
        assert_eq!(arxiv_id(&paper("10.1038/s41586-020-2649-2", "", "https://example.org/x")), None);
    }

    #[test]
    fn replies_of_the_indexes() {
        let openalex = json!({"open_access": {"is_oa": true}, "best_oa_location": {"is_oa": true, "pdf_url": "https://a.org/x.pdf", "version": "acceptedVersion"}, "locations": [
            {"is_oa": true, "pdf_url": "https://a.org/x.pdf", "version": "acceptedVersion"},
            {"is_oa": false, "pdf_url": "https://paywall.org/x.pdf", "version": "publishedVersion"},
            {"is_oa": true, "pdf_url": null, "version": "publishedVersion"},
            {"is_oa": true, "pdf_url": "https://b.org/y.pdf", "version": "submittedVersion"}]});
        let (open, files) = openalex_candidates(&openalex);
        assert!(open);
        assert_eq!(files.iter().map(|c| (c.url.as_str(), c.version)).collect::<Vec<_>>(), [("https://a.org/x.pdf", "accepted"), ("https://b.org/y.pdf", "preprint")]);
        assert_eq!(openalex_candidates(&json!({"open_access": {"is_oa": false}, "best_oa_location": null, "locations": []})), (false, vec![]));

        // Europe PMC's own rendering is left out; a link that needs a subscription too.
        let europepmc = json!({"resultList": {"result": [{"isOpenAccess": "Y", "fullTextUrlList": {"fullTextUrl": [
            {"availabilityCode": "S", "documentStyle": "doi", "site": "DOI", "url": "https://doi.org/10.1/x"},
            {"availabilityCode": "OA", "documentStyle": "pdf", "site": "Europe_PMC", "url": "https://europepmc.org/articles/PMC1?pdf=render"},
            {"availabilityCode": "S", "documentStyle": "pdf", "site": "Publisher", "url": "https://paywall.org/x.pdf"},
            {"availabilityCode": "F", "documentStyle": "pdf", "site": "Unpaywall", "url": "https://repo.org/x.pdf"}]}}]}});
        let (open, files) = europepmc_candidates(&europepmc);
        assert!(open);
        assert_eq!(files.iter().map(|c| c.url.as_str()).collect::<Vec<_>>(), ["https://repo.org/x.pdf"]);
        assert_eq!(europepmc_candidates(&json!({"resultList": {"result": []}})), (false, vec![]));
        assert!(europepmc_url(&paper("10.1/x", "", "")).unwrap().contains("DOI%3A%2210.1/x%22"));
        assert!(europepmc_url(&paper("", "123", "")).unwrap().contains("EXT_ID%3A123"));
        assert_eq!(europepmc_url(&paper("", "", "")), None);
    }

    #[test]
    fn candidates_in_order() {
        // An index says open access and lists no file: the publisher's own address is tried. Its manuscript elsewhere comes after.
        let fetch = |url: &str| -> Result<String, String> {
            if url.contains("openalex") {
                Ok(json!({"open_access": {"is_oa": true}, "best_oa_location": {"is_oa": true, "pdf_url": "https://repo.org/ms.pdf", "version": "acceptedVersion"}, "locations": []}).to_string())
            } else {
                Err("www.ebi.ac.uk 返回 HTTP 503。".into())
            }
        };
        let (found, notes) = candidates(&paper("10.1186/s12870-026-08596-6", "", ""), &fetch);
        assert_eq!(found.iter().map(|c| (c.source.as_str(), c.version)).collect::<Vec<_>>(), [("Springer", "published"), ("OpenAlex", "accepted")]);
        assert_eq!(found[0].url, "https://link.springer.com/content/pdf/10.1186/s12870-026-08596-6.pdf");
        assert_eq!(notes, ["Europe PMC：www.ebi.ac.uk 返回 HTTP 503。"]);
        // Not said to be open: the publisher is not asked. An arXiv paper needs no index.
        let closed = |_: &str| -> Result<String, String> { Ok(json!({"open_access": {"is_oa": false}, "resultList": {"result": []}}).to_string()) };
        assert_eq!(candidates(&paper("10.1038/s41586-020-2649-2", "", ""), &closed).0, vec![]);
        let offline = |_: &str| -> Result<String, String> { Err("HTTP 404".into()) };
        let (found, notes) = candidates(&paper("10.48550/arXiv.1706.03762", "", ""), &offline);
        assert_eq!((found[0].url.as_str(), notes.len()), ("https://arxiv.org/pdf/1706.03762", 0));
        assert_eq!((publisher_candidate("10.3389/fpls.2021.661361").unwrap().source.as_str(), publisher_candidate("10.1016/j.cell.2020.01.001")), ("Frontiers", None));
    }

    #[test]
    fn a_supplement_is_not_the_paper() {
        let pages = |texts: &[&str]| texts.iter().map(|text| text.to_string()).collect::<Vec<_>>();
        // The file two records were given as their full text: figures and their legends, nothing else.
        let figures = pages(&[
            "Supp. Figure 1. Gene expression changes upon cell wall digestion. (A)\n\nVolcano plot of the cell wall\ndigestion triggered response\n\nA\n\n-Log10 FDR",
            "Cluster 1 Cluster 2\n\nU D FD U D FD\n\nA\n\nB\n\nSupp. Figure 2. Functional analysis of gene expression changes caused by cell-wall digestion. (A) k-\n\nmeans clustering",
            "Supp. Figure 3. Comparison of the transcriptome profiles of Digested and Fixed-Digested samples",
            "A\n\nB\n\nNot significant",
        ]);
        assert!(supplement(&figures));
        // Other ways a supplement is written: a title page, numbered with an S, tables; a blank first page is passed over.
        assert!(supplement(&pages(&["", "Supplementary Information for\n\nA root atlas\n\nZhu M, Lee A", "Figure S1. Quality control.\nReads per cell", "Table S2: Marker genes"])));
        assert!(supplement(&pages(&["Supplemental Table 1 | Primers used in this study\nGene Forward Reverse"])));
        // An article: it names its supplement in running text and under its first page, not on every page.
        let article = pages(&[
            "RESEARCH Open Access\nA single-cell atlas of the root\nZhu M, Lee A\n\nAbstract\nBackground: Roots grow.\n\nSupplementary Information The online version contains supplementary material available at https://doi.org/10.1186/x.",
            "Introduction\nRoots take up water (Fig. 1).\nSupplementary Figure 1 shows the quality of the libraries, and",
            "Results\nWe profiled 10,000 cells.",
        ]);
        assert!(!supplement(&article));
        // An article that shows a supplementary figure early is still one: its first page is not a supplement's.
        assert!(!supplement(&pages(&["A root atlas\nAbstract\nRoots grow.", "Figure S1. Quality control.", "Figure S2. Clusters."])));
        assert!(!supplement(&pages(&["Figures 2 and 3 show the clusters.\nTable salt content was measured.", "Supported by a grant."])));
        assert!(!supplement(&[]) && !supplement(&pages(&["", " "])));
    }

    #[test]
    fn a_pdf_or_a_page_in_its_place() {
        assert!(is_pdf(b"%PDF-1.7\n...") && is_pdf(b"\n\n%PDF-1.4"));
        assert!(!is_pdf(b"<!DOCTYPE html><html>Checking your browser") && !is_pdf(b""));
    }
}
