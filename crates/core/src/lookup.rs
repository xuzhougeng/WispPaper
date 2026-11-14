//! Metadata by identifier: where to ask (Crossref for a DOI, Europe PMC for
//! a PMID or as a second opinion) and how to read the answers. The HTTP
//! requests themselves are made by the app.

use crate::model::{Paper, normalize_doi, normalize_pmid, strip_markup, valid_doi};
use serde_json::Value;

/// What a person typed into 通过 DOI / PMID 添加.
#[derive(Clone, Debug, PartialEq)]
pub enum Identifier {
    Doi(String),
    Pmid(String),
}

/// Recognise a DOI (bare, labelled or as a link) or a PMID (digits, labelled or as a link).
pub fn parse_identifier(value: &str) -> Option<Identifier> {
    let doi = normalize_doi(value);
    if !doi.is_empty() && valid_doi(&doi) { return Some(Identifier::Doi(doi)) }
    let pmid = normalize_pmid(value);
    (!pmid.is_empty() && pmid.len() <= 9 && pmid.bytes().all(|b| b.is_ascii_digit())).then_some(Identifier::Pmid(pmid))
}

pub(crate) fn encode(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) { out.push(b as char) } else { out.push_str(&format!("%{b:02X}")) }
    }
    out
}

pub fn crossref_url(doi: &str) -> String {
    format!("https://api.crossref.org/works/{}", encode(doi))
}

pub fn europepmc_url(id: &Identifier) -> String {
    let query = match id {
        Identifier::Doi(doi) => format!("DOI:\"{doi}\""),
        Identifier::Pmid(pmid) => format!("EXT_ID:{pmid} AND SRC:MED"),
    };
    format!("https://www.ebi.ac.uk/europepmc/webservices/rest/search?query={}&resultType=core&format=json&pageSize=1", encode(&query).replace('/', "%2F"))
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn first(v: &Value) -> String {
    v.as_array().and_then(|a| a.first()).map(text).unwrap_or_default()
}

/// A Crossref `/works/{doi}` response.
pub fn crossref_paper(body: &Value) -> Option<Paper> {
    crossref_work(body.get("message")?)
}

/// One Crossref work record.
pub fn crossref_work(work: &Value) -> Option<Paper> {
    let title = strip_markup(&first(&work["title"]));
    if title.is_empty() { return None }
    let authors: Vec<String> = work["author"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|a| {
                    let (family, given) = (text(&a["family"]), text(&a["given"]));
                    let name = if family.is_empty() { text(&a["name"]) } else if given.is_empty() { family } else { format!("{family}, {given}") };
                    (!name.is_empty()).then_some(name)
                })
                .collect()
        })
        .unwrap_or_default();
    let year = ["published-print", "published-online", "published", "issued", "posted", "created"]
        .iter()
        .find_map(|key| work[key]["date-parts"][0][0].as_i64())
        .map(|y| y as i32)
        .filter(|y| (1800..=2200).contains(y));
    let kind = text(&work["type"]);
    let mut journal = strip_markup(&first(&work["container-title"]));
    if journal.is_empty() && kind == "posted-content" {
        journal = text(&work["institution"][0]["name"]);
        if journal.is_empty() { journal = text(&work["publisher"]) }
    }
    Some(Paper {
        title,
        authors: authors.join("; "),
        journal,
        year,
        doi: normalize_doi(&text(&work["DOI"])),
        url: text(&work["URL"]),
        abstract_text: strip_markup(&text(&work["abstract"])).trim_start_matches("Abstract ").to_string(),
        publication_status: if kind == "posted-content" { "preprint" } else { "published" }.into(),
        source: "Crossref".into(),
        ..Default::default()
    })
}

/// The first hit of a Europe PMC search response.
pub fn europepmc_paper(body: &Value) -> Option<Paper> {
    europepmc_hit(body["resultList"]["result"].as_array()?.first()?)
}

/// One Europe PMC search hit.
pub fn europepmc_hit(hit: &Value) -> Option<Paper> {
    let title = strip_markup(&text(&hit["title"])).trim_end_matches('.').to_string();
    if title.is_empty() { return None }
    // Europe PMC writes a name as `Harris CR`: the family name first and initials after it, which a reader of names would take
    // for a given name and the family name CR. With the name's parts it is kept as `Harris, Charles R`; else the comma is put in.
    let named = |a: &Value| {
        let (family, given) = (text(&a["lastName"]), text(&a["firstName"]));
        let given = if given.is_empty() { text(&a["initials"]) } else { given };
        if family.is_empty() { Some(text(&a["fullName"])).filter(|name| !name.is_empty()).unwrap_or_else(|| text(&a["collectiveName"])) } else if given.is_empty() { family } else { format!("{family}, {given}") }
    };
    let mut authors: Vec<String> = hit["authorList"]["author"].as_array().map(|list| list.iter().map(named).filter(|n| !n.is_empty()).collect()).unwrap_or_default();
    if authors.is_empty() {
        authors = text(&hit["authorString"]).trim_end_matches('.').split(',').map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect();
    }
    let authors: Vec<String> = crate::model::family_first(&authors.join("; ")).split("; ").filter(|name| !name.is_empty()).map(str::to_string).collect();
    let preprint = text(&hit["source"]) == "PPR" || text(&hit["pubTypeList"]["pubType"][0]).to_lowercase().contains("preprint");
    let mut journal = text(&hit["journalInfo"]["journal"]["title"]);
    if journal.is_empty() { journal = text(&hit["journalTitle"]) }
    if journal.is_empty() { journal = text(&hit["bookOrReportDetails"]["publisher"]) }
    Some(Paper {
        title,
        authors: authors.join("; "),
        journal,
        year: text(&hit["pubYear"]).parse().ok().filter(|y| (1800..=2200).contains(y)),
        doi: normalize_doi(&text(&hit["doi"])),
        pmid: text(&hit["pmid"]),
        abstract_text: strip_markup(&text(&hit["abstractText"])),
        publication_status: if preprint { "preprint" } else { "published" }.into(),
        source: "Europe PMC".into(),
        ..Default::default()
    })
}

/// Fill what `paper` leaves blank from a second source.
pub fn merge_lookup(mut paper: Paper, extra: Paper) -> Paper {
    fn fill(target: &mut String, value: String) {
        if target.is_empty() { *target = value }
    }
    fill(&mut paper.title, extra.title);
    fill(&mut paper.authors, extra.authors);
    fill(&mut paper.journal, extra.journal);
    fill(&mut paper.doi, extra.doi);
    fill(&mut paper.pmid, extra.pmid);
    fill(&mut paper.url, extra.url);
    fill(&mut paper.abstract_text, extra.abstract_text);
    if paper.year.is_none() { paper.year = extra.year }
    if paper.publication_status.is_empty() || paper.publication_status == "unknown" { paper.publication_status = extra.publication_status }
    paper
}

/// The stretch of a text a DOI can run over: up to a space, a quote or a
/// bracket. Round brackets belong to it while they close
/// (`10.1016/S0092-8674(00)81683-9`); one that closes nothing, or is not
/// closed, is the text's own.
fn doi_run(text: &str) -> &str {
    let mut open = vec![];
    let mut end = text.len();
    for (at, c) in text.char_indices() {
        let closes_nothing = c == ')' && open.pop().is_none();
        if closes_nothing || c.is_whitespace() || "\"<>[]{}，。；".contains(c) {
            end = at;
            break;
        }
        if c == '(' { open.push(at) }
    }
    &text[..open.first().map_or(end, |at| end.min(*at))]
}

/// The first DOI printed in a text (the first page of a PDF, usually).
pub fn find_doi(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find("10.") {
        let start = from + at;
        let doi = normalize_doi(doi_run(&lower[start..]));
        // Something must follow the slash, and a registrant number precede it.
        if valid_doi(&doi) && doi.split_once('/').is_some_and(|(_, suffix)| suffix.len() >= 3) { return Some(doi) }
        from = start + 3;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn identifiers() {
        assert_eq!(parse_identifier(" https://doi.org/10.1038/S41586-020-2649-2 "), Some(Identifier::Doi("10.1038/s41586-020-2649-2".into())));
        assert_eq!(parse_identifier("PMID: 32939066"), Some(Identifier::Pmid("32939066".into())));
        assert_eq!(parse_identifier("32939066"), Some(Identifier::Pmid("32939066".into())));
        assert_eq!(parse_identifier("hello"), None);
        assert_eq!(crossref_url("10.1000/a b"), "https://api.crossref.org/works/10.1000/a%20b");
        assert!(europepmc_url(&Identifier::Pmid("1".into())).contains("EXT_ID%3A1%20AND%20SRC%3AMED"));
    }

    #[test]
    fn crossref() {
        let body = json!({"message": {
            "title": ["Array programming with <i>NumPy</i>"], "DOI": "10.1038/S41586-020-2649-2", "type": "journal-article",
            "author": [{"given": "Charles R.", "family": "Harris"}, {"name": "The NumPy Team"}],
            "container-title": ["Nature"], "issued": {"date-parts": [[2020, 9, 16]]},
            "abstract": "<jats:title>Abstract</jats:title><jats:p>Arrays are &amp; will be.</jats:p>", "URL": "https://doi.org/10.1038/s41586-020-2649-2"
        }});
        let p = crossref_paper(&body).unwrap();
        assert_eq!(p.title, "Array programming with NumPy");
        assert_eq!(p.authors, "Harris, Charles R.; The NumPy Team");
        assert_eq!((p.journal.as_str(), p.year, p.doi.as_str()), ("Nature", Some(2020), "10.1038/s41586-020-2649-2"));
        assert_eq!(p.abstract_text, "Arrays are & will be.");
        assert_eq!(p.publication_status, "published");
        assert!(crossref_paper(&json!({"message": {}})).is_none());
    }

    #[test]
    fn europepmc_and_merge() {
        let body = json!({"resultList": {"result": [{
            "title": "Array programming with NumPy.", "authorString": "Harris CR, Millman KJ.", "pubYear": "2020",
            "journalInfo": {"journal": {"title": "Nature"}}, "doi": "10.1038/s41586-020-2649-2", "pmid": "32939066", "abstractText": "Text."
        }]}});
        let p = europepmc_paper(&body).unwrap();
        // Names written family name first get their comma, so the family name is not taken for CR.
        assert_eq!((p.title.as_str(), p.authors.as_str(), p.pmid.as_str()), ("Array programming with NumPy", "Harris, CR; Millman, KJ", "32939066"));
        assert_eq!(p.citation(), "Harris and Millman, 2020");
        // With the parts of each name, those are kept; a group is named as it is.
        let listed = json!({"title": "T", "authorString": "Harris CR, de Melo AV.", "authorList": {"author": [
            {"fullName": "Harris CR", "firstName": "Charles R", "lastName": "Harris", "initials": "CR"},
            {"fullName": "de Melo AV", "lastName": "de Melo", "initials": "AV"},
            {"fullName": "Marín-Sanz M"},
            {"collectiveName": "NumPy Team"},
        ]}});
        assert_eq!(europepmc_hit(&listed).unwrap().authors, "Harris, Charles R; de Melo, AV; Marín-Sanz, M; NumPy Team");
        let merged = merge_lookup(Paper { title: "T".into(), doi: "10.1038/s41586-020-2649-2".into(), ..Default::default() }, p);
        assert_eq!((merged.title.as_str(), merged.pmid.as_str(), merged.year), ("T", "32939066", Some(2020)));
        assert!(europepmc_paper(&json!({"resultList": {"result": []}})).is_none());
    }

    #[test]
    fn doi_in_text() {
        assert_eq!(find_doi("Nature 585 (2020). https://doi.org/10.1038/s41586-020-2649-2\nmore"), Some("10.1038/s41586-020-2649-2".into()));
        assert_eq!(find_doi("version 10.2 of 10.5 items"), None);
        // Brackets that close are the DOI's own (old Elsevier ones); the text's are not.
        assert_eq!(find_doi("Cell 100, 57 (doi:10.1016/S0092-8674(00)81683-9) 2000"), Some("10.1016/s0092-8674(00)81683-9".into()));
        assert_eq!(find_doi("see 10.1038/s41586-020-2649-2(Fig. 1"), Some("10.1038/s41586-020-2649-2".into()));
    }
}
