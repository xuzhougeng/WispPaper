//! A web address pasted for a paper: the page of an article at its
//! publisher, or the address of its PDF. What the page's scholarly meta
//! tags say (`citation_pdf_url`, `citation_doi`, …, the tags Google Scholar
//! reads and nearly every publisher writes), a DOI written into the address
//! itself, and the address of a file named relative to the page's. Nothing
//! here fetches; the tool that takes an address in (`import_url`) is handed
//! the network by its caller.

use crate::lookup::Identifier;
use crate::model::{Paper, normalize_doi, normalize_pmid, valid_doi};

/// What a page says of the article it shows, read off its meta tags. Empty where it says nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageMeta {
    /// The address of the article's PDF, made absolute against the page's.
    pub pdf_url: String,
    pub doi: String,
    pub pmid: String,
    /// An arXiv number (`citation_arxiv_id`), for a preprint without a DOI of its own on the page.
    pub arxiv: String,
    pub title: String,
    pub authors: Vec<String>,
    pub journal: String,
    pub year: Option<i32>,
}

impl PageMeta {
    /// The number to look the article up by: its DOI, its PMID, or the DOI arXiv gives each preprint.
    pub fn identifier(&self) -> Option<Identifier> {
        if !self.doi.is_empty() { return Some(Identifier::Doi(self.doi.clone())) }
        if !self.pmid.is_empty() { return Some(Identifier::Pmid(self.pmid.clone())) }
        (!self.arxiv.is_empty()).then(|| Identifier::Doi(format!("10.48550/arxiv.{}", self.arxiv.to_lowercase())))
    }

    /// The record the page alone makes, for when no database knows the number (an arXiv DOI is not at Crossref).
    pub fn paper(&self) -> Paper {
        let doi = match self.identifier() {
            Some(Identifier::Doi(doi)) => doi,
            _ => String::new(),
        };
        Paper { title: self.title.clone(), doi, pmid: self.pmid.clone(), authors: self.authors.join("; "), journal: self.journal.clone(), year: self.year, ..Default::default() }
    }
}

/// Whether a pasted word is a web address.
pub fn is_address(word: &str) -> bool {
    let lower = word.trim().to_ascii_lowercase();
    ["http://", "https://"].iter().any(|scheme| lower.strip_prefix(scheme).is_some_and(|rest| rest.split('/').next().is_some_and(|host| host.contains('.'))))
}

/// The host of an address: `www.nature.com`.
pub fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split(['/', '?', '#']).next().unwrap_or(rest)
}

/// What a page's meta tags say. `base` is the page's own address, against which a relative PDF address is read.
pub fn meta(html: &str, base: &str) -> PageMeta {
    let mut found = PageMeta::default();
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find("<meta") {
        let start = from + at + "<meta".len();
        let (attributes, end) = attributes(html, start);
        from = end.max(start);
        let key = attributes.iter().find(|(name, _)| name == "name" || name == "property").map(|(_, value)| value.trim().to_ascii_lowercase()).unwrap_or_default();
        let Some(content) = attributes.iter().find(|(name, _)| name == "content").map(|(_, value)| value.trim().to_string()) else { continue };
        if content.is_empty() { continue }
        match key.as_str() {
            "citation_pdf_url" | "bepress_citation_pdf_url" if found.pdf_url.is_empty() => found.pdf_url = resolve(base, &content),
            // `dc.identifier` holds an ISSN or an address as often as a DOI: only a DOI is taken.
            "citation_doi" | "bepress_citation_doi" | "dc.identifier" | "dc.identifier.doi" | "prism.doi" | "dc.relation.doi" if found.doi.is_empty() => {
                let doi = normalize_doi(&content);
                if doi.starts_with("10.") && valid_doi(&doi) { found.doi = doi }
            }
            "citation_pmid" if found.pmid.is_empty() => {
                let pmid = normalize_pmid(&content);
                if !pmid.is_empty() && pmid.len() <= 9 && pmid.bytes().all(|b| b.is_ascii_digit()) { found.pmid = pmid }
            }
            "citation_arxiv_id" if found.arxiv.is_empty() => found.arxiv = content,
            "citation_title" | "dc.title" if found.title.is_empty() => found.title = crate::model::strip_markup(&content),
            "citation_author" => found.authors.push(content),
            "citation_journal_title" | "citation_conference_title" if found.journal.is_empty() => found.journal = content,
            "citation_publication_date" | "citation_date" | "citation_online_date" | "dc.date" if found.year.is_none() => {
                found.year = content.split(|c: char| !c.is_ascii_digit()).find(|part| part.len() == 4).and_then(|year| year.parse().ok()).filter(|year| (1800..=2200).contains(year));
            }
            _ => {}
        }
    }
    found
}

/// The attributes of one tag, from just after its name to its `>`: names in lowercase, values with their entities
/// read. Either quote, or none; in any order. Also where the tag ends.
fn attributes(html: &str, start: usize) -> (Vec<(String, String)>, usize) {
    let bytes = html.as_bytes();
    let mut out = vec![];
    let mut at = start;
    loop {
        while at < bytes.len() && (bytes[at].is_ascii_whitespace() || bytes[at] == b'/') { at += 1 }
        if at >= bytes.len() { return (out, at) }
        if bytes[at] == b'>' { return (out, at + 1) }
        let name_start = at;
        while at < bytes.len() && !bytes[at].is_ascii_whitespace() && !matches!(bytes[at], b'=' | b'>' | b'/') { at += 1 }
        let name = html[name_start..at].to_ascii_lowercase();
        while at < bytes.len() && bytes[at].is_ascii_whitespace() { at += 1 }
        if at >= bytes.len() || bytes[at] != b'=' {
            out.push((name, String::new()));
            continue;
        }
        at += 1;
        while at < bytes.len() && bytes[at].is_ascii_whitespace() { at += 1 }
        let value = match bytes.get(at) {
            Some(&quote) if quote == b'"' || quote == b'\'' => {
                let value_start = at + 1;
                let end = html[value_start..].find(quote as char).map_or(bytes.len(), |n| value_start + n);
                at = (end + 1).min(bytes.len());
                &html[value_start..end]
            }
            _ => {
                let value_start = at;
                while at < bytes.len() && !bytes[at].is_ascii_whitespace() && bytes[at] != b'>' { at += 1 }
                &html[value_start..at]
            }
        };
        out.push((name, entities(value)));
    }
}

/// Text with its HTML entities read: `&amp;`, `&quot;`, `&#47;`, `&#x2F;` and the few others a meta tag carries.
fn entities(text: &str) -> String {
    if !text.contains('&') { return text.to_string() }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.bytes().take(12).position(|b| b == b';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..end];
        let read = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")).map(|hex| u32::from_str_radix(hex, 16)).or_else(|| name.strip_prefix('#').map(str::parse::<u32>)).and_then(Result::ok).and_then(char::from_u32),
        };
        match read {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// An address as a page names it, made absolute against the page's own: whole, from the host's root (`/content/…`),
/// without its scheme (`//host/…`) or beside the page (`file.pdf`, `../pdf/file.pdf`).
pub fn resolve(base: &str, href: &str) -> String {
    let href = href.trim();
    let lower = href.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") { return href.to_string() }
    let (scheme, rest) = base.split_once("://").unwrap_or(("https", base));
    if let Some(other) = href.strip_prefix("//") { return format!("{scheme}://{other}") }
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if href.starts_with('/') { return format!("{scheme}://{host}{href}") }
    let path = rest[host.len()..].split(['?', '#']).next().unwrap_or("");
    let mut segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    // The page's own name goes; a path that ends in a slash names a folder.
    if !path.ends_with('/') { segments.pop(); }
    let (href_path, tail) = href.find(['?', '#']).map_or((href, ""), |at| href.split_at(at));
    for segment in href_path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            segment => segments.push(segment),
        }
    }
    let trailing = if href_path.ends_with('/') { "/" } else { "" };
    format!("{scheme}://{host}/{}{trailing}{tail}", segments.join("/"))
}

/// `%2F` and the like read back, for a DOI written into an address.
fn percent_decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' && let Some(byte) = text.get(at + 1..at + 3).and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
            out.push(byte);
            at += 3;
            continue;
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What a publisher puts after a DOI in an address to say which view of the article it is.
const VIEWS: [&str; 16] = ["/full", "/abstract", "/pdf", "/epdf", "/pdfdirect", "/fulltext", "/full-text", "/figures", "/references", "/metrics", "/html", "/meta", "/summary", "/citation", "/reader", "/supplementary"];

/// The DOI an address carries in its path or query (`/doi/10.1021/…`, `/articles/10.1186/…`, `?id=10.1371/…`),
/// without the view the publisher appends (`/full`, `.pdf`, a bioRxiv version `v2`). Only a part of the address
/// that begins with `10.`, a registrant number and a slash is taken: a version number or a date in a path is not one.
pub fn doi_in_address(url: &str) -> Option<String> {
    let decoded = percent_decoded(url.trim());
    let lower = decoded.to_lowercase();
    let after_scheme = lower.find("://").map_or(0, |at| at + 3);
    let path_start = lower[after_scheme..].find('/').map_or(lower.len(), |at| after_scheme + at);
    let mut from = path_start;
    while let Some(at) = lower[from..].find("10.") {
        let start = from + at;
        from = start + 3;
        if !matches!(lower.as_bytes()[start - 1], b'/' | b'=') { continue }
        let run = lower[start..].split(['?', '#', '&', ' ', '"', '\'', '<', '>']).next().unwrap_or("");
        let mut doi = run.trim_end_matches('/').to_string();
        loop {
            let before = doi.len();
            for view in VIEWS {
                if let Some(rest) = doi.strip_suffix(view) { doi = rest.trim_end_matches('/').to_string() }
            }
            for ending in [".pdf", ".full", ".html", ".abstract"] {
                if let Some(rest) = doi.strip_suffix(ending) { doi = rest.to_string() }
            }
            if doi.len() == before { break }
        }
        // bioRxiv and medRxiv write the version after the DOI: 10.1101/2020.01.01.123456v2.
        if doi.starts_with("10.1101/") && let Some(at) = doi.rfind('v').filter(|at| doi[at + 1..].bytes().all(|b| b.is_ascii_digit()) && *at + 1 < doi.len()) { doi.truncate(at) }
        if valid_doi(&doi) && doi.split_once('/').is_some_and(|(_, suffix)| suffix.len() >= 3) { return Some(doi) }
    }
    None
}

/// The name an address gives its file, for a paper nothing else names: `s41586-020-2649-2.pdf` → `s41586-020-2649-2`.
pub fn file_stem(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let last = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let last = percent_decoded(last);
    let stem = last.strip_suffix(".pdf").or_else(|| last.strip_suffix(".PDF")).unwrap_or(&last);
    if stem.is_empty() || stem.contains(':') { host(url).to_string() } else { stem.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_tags_either_way_round() {
        let page = r#"<html><head>
            <META NAME="citation_title" CONTENT="Root hairs &amp; soil">
            <meta content='10.1186/S12870-021-03000-1' name='citation_doi' />
            <meta name="citation_author" content="Zhu, Mo"><meta name="citation_author" content="Lee, An">
            <meta name="dc.identifier" content="ISSN 1471-2229">
            <meta name="citation_publication_date" content="2021/05/03">
            <meta name="citation_journal_title" content="BMC Plant Biology">
            <meta property="citation_pdf_url" content="https://bmcplantbiol.biomedcentral.com/counter/pdf/10.1186/s12870-021-03000-1?a=1&amp;b=2">
        </head><body><meta name="citation_pdf_url" content="/second.pdf"></body></html>"#;
        let found = meta(page, "https://bmcplantbiol.biomedcentral.com/articles/10.1186/s12870-021-03000-1");
        assert_eq!(found.title, "Root hairs & soil");
        assert_eq!(found.doi, "10.1186/s12870-021-03000-1");
        assert_eq!(found.authors, ["Zhu, Mo", "Lee, An"]);
        assert_eq!((found.year, found.journal.as_str()), (Some(2021), "BMC Plant Biology"));
        // The first PDF address counts, its entity read.
        assert_eq!(found.pdf_url, "https://bmcplantbiol.biomedcentral.com/counter/pdf/10.1186/s12870-021-03000-1?a=1&b=2");
        assert_eq!(found.identifier(), Some(Identifier::Doi("10.1186/s12870-021-03000-1".into())));
    }

    #[test]
    fn doi_from_dublin_core_and_prism() {
        assert_eq!(meta(r#"<meta name="DC.Identifier" content="doi:10.1093/nar/gkab123">"#, "https://a.org/x").doi, "10.1093/nar/gkab123");
        assert_eq!(meta(r#"<meta name="prism.doi" content="https://doi.org/10.1038/s41586-020-2649-2">"#, "https://a.org/x").doi, "10.1038/s41586-020-2649-2");
        assert_eq!(meta(r#"<meta name="citation_pmid" content="32939066">"#, "https://a.org/x").identifier(), Some(Identifier::Pmid("32939066".into())));
        // An arXiv abstract page names the preprint by its number.
        let arxiv = meta(r#"<meta name="citation_title" content="Attention Is All You Need" /><meta name="citation_arxiv_id" content="1706.03762" /><meta name="citation_pdf_url" content="https://arxiv.org/pdf/1706.03762" />"#, "https://arxiv.org/abs/1706.03762");
        assert_eq!(arxiv.identifier(), Some(Identifier::Doi("10.48550/arxiv.1706.03762".into())));
        assert_eq!(arxiv.paper().title, "Attention Is All You Need");
        assert_eq!(meta("<html><title>Sign in</title></html>", "https://a.org/x"), PageMeta::default());
    }

    #[test]
    fn relative_addresses() {
        let page = "https://www.example.org/journal/article/view/12?lang=en";
        assert_eq!(resolve(page, "/download/12.pdf"), "https://www.example.org/download/12.pdf");
        assert_eq!(resolve(page, "12/file.pdf"), "https://www.example.org/journal/article/view/12/file.pdf");
        assert_eq!(resolve(page, "../../pdf/12.pdf?x=1"), "https://www.example.org/journal/pdf/12.pdf?x=1");
        assert_eq!(resolve(page, "//cdn.example.org/12.pdf"), "https://cdn.example.org/12.pdf");
        assert_eq!(resolve(page, "http://other.org/a.pdf"), "http://other.org/a.pdf");
        assert_eq!(meta(r#"<meta name="citation_pdf_url" content="/content/pdf/a.pdf">"#, "https://link.springer.com/article/10.1007/a").pdf_url, "https://link.springer.com/content/pdf/a.pdf");
    }

    #[test]
    fn doi_written_in_the_address() {
        for (url, doi) in [
            ("https://pubs.acs.org/doi/10.1021/acs.jmedchem.0c00001", "10.1021/acs.jmedchem.0c00001"),
            ("https://onlinelibrary.wiley.com/doi/full/10.1002/pld3.123", "10.1002/pld3.123"),
            ("https://onlinelibrary.wiley.com/doi/epdf/10.1111/tpj.15000?af=R", "10.1111/tpj.15000"),
            ("https://bmcbioinformatics.biomedcentral.com/articles/10.1186/s12859-019-3000-0", "10.1186/s12859-019-3000-0"),
            ("https://www.frontiersin.org/articles/10.3389/fpls.2020.00001/full", "10.3389/fpls.2020.00001"),
            ("https://link.springer.com/content/pdf/10.1007/s00425-020-03400-1.pdf", "10.1007/s00425-020-03400-1"),
            ("https://journals.plos.org/plosone/article?id=10.1371/journal.pone.0230000", "10.1371/journal.pone.0230000"),
            ("https://www.biorxiv.org/content/10.1101/2020.01.01.123456v2.full.pdf", "10.1101/2020.01.01.123456"),
            ("https://academic.oup.com/nar/article/49/D1/D1/6000000?doi=10.1093%2Fnar%2Fgkaa1000", "10.1093/nar/gkaa1000"),
        ] {
            assert_eq!(doi_in_address(url).as_deref(), Some(doi), "{url}");
        }
        // Numbers that only look like the start of one.
        for url in ["https://www.nature.com/articles/s41586-020-2649-2", "https://10.1.2.3/doi/abc", "https://example.org/v/10.5/abc", "https://www.sciencedirect.com/science/article/pii/S0092867420300001", "https://example.org/files/10.12345/"] {
            assert_eq!(doi_in_address(url), None, "{url}");
        }
    }

    #[test]
    fn addresses_and_names() {
        assert!(is_address("https://www.nature.com/articles/s41586-020-2649-2"));
        assert!(is_address("HTTP://arxiv.org/abs/1706.03762"));
        assert!(!is_address("10.1038/s41586-020-2649-2"));
        assert!(!is_address("https://localhost"));
        assert_eq!(host("https://www.nature.com/articles/x.pdf"), "www.nature.com");
        assert_eq!(file_stem("https://www.nature.com/articles/s41586-020-2649-2.pdf?download=1"), "s41586-020-2649-2");
        assert_eq!(file_stem("https://arxiv.org/pdf/1706.03762"), "1706.03762");
        assert_eq!(file_stem("https://example.org/"), "example.org");
    }
}
