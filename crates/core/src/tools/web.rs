//! `import_url`: a web address a person pasted, taken in as a paper with its
//! PDF. The address is fetched once, as the person's own network stands (a
//! campus that the publisher lets in by its addresses gets the file); nothing
//! logs in, keeps a cookie or passes for a browser, and of the links on the
//! page only the PDF it names (`citation_pdf_url`) is followed. A wall is
//! said, not gone round.

use super::{fetch_pdf_from, file_name, text};
use crate::lookup::{self, Identifier};
use crate::model::Paper;
use crate::openaccess::{self, Candidate};
use crate::sources::{self, Download, Fetch};
use crate::store::Store;
use crate::webpage::{self, PageMeta};
use crate::fulltext;
use anyhow::{Result, bail};
use serde_json::{Value, json};

/// The most one address may take, the record's lookup and the search for an open copy included: a host that
/// holds a connection without answering is not waited for by the addresses pasted after it. What came in by
/// then stays; a paper left without its PDF can have it looked for again (`find_pdf`), which has no such limit.
const TIME_FOR_ADDRESS: std::time::Duration = std::time::Duration::from_secs(120);

pub(super) fn import_url(store: &mut Store, args: &Value, fetch: Fetch, download: Download) -> Result<Value> {
    // No request begins once the time is up, whoever's network function this is.
    let out_of_time = || format!("{}。", sources::TIME_UP);
    let fetch: Fetch = &|url: &str| if sources::time_up() { Err(out_of_time()) } else { fetch(url) };
    let download: Download = &|url: &str| if sources::time_up() { Err(out_of_time()) } else { download(url) };
    sources::within(TIME_FOR_ADDRESS, || import_address(store, args, fetch, download))
}

fn import_address(store: &mut Store, args: &Value, fetch: Fetch, download: Download) -> Result<Value> {
    let url = text(args, "url")?.to_string();
    if !webpage::is_address(&url) { bail!("这不是网址：{url}。请给出以 http:// 或 https:// 开头的文献页面或 PDF 地址。") }
    let mut row = json!({"url": url});
    // What went wrong on the way, said together when no PDF comes of the address.
    let mut troubles: Vec<String> = vec![];
    let meta = match download(&url) {
        // The address of the PDF itself: taken in as a dropped file is.
        Ok(bytes) if openaccess::is_pdf(&bytes) => return take_pdf(store, &url, &bytes, fetch, row),
        Ok(bytes) => {
            let meta = webpage::meta(&String::from_utf8_lossy(&bytes), &url);
            if meta == PageMeta::default() { troubles.push("网页没有给出文献信息，多半是登录页或验证页".into()) }
            meta
        }
        Err(err) => {
            troubles.push(format!("打开网页：{err}"));
            PageMeta::default()
        }
    };
    let Some(id) = meta.identifier().or_else(|| webpage::doi_in_address(&url).map(Identifier::Doi)) else {
        // No number to know the paper by: a PDF the page names is still taken in as a dropped one, its own DOI read off its text.
        if !meta.pdf_url.is_empty() && !sources::out_of_time(&mut troubles) {
            match download(&meta.pdf_url) {
                Ok(bytes) if openaccess::is_pdf(&bytes) => return take_pdf(store, &meta.pdf_url, &bytes, fetch, row),
                Ok(_) => troubles.push(format!("{}：返回的不是 PDF，多半是登录页或验证页", webpage::host(&meta.pdf_url))),
                Err(err) => troubles.push(format!("{}：{err}", webpage::host(&meta.pdf_url))),
            }
        }
        troubles.push("没有在这个网址里认出文献：网页和地址里都没有 DOI、PMID 或 PDF".into());
        row["outcome"] = json!("not_recognised");
        row["reason"] = json!(sources::reasons(&troubles));
        return Ok(row);
    };
    // The record from the databases, the page's own tags filling what they leave blank; the page alone when they do not know it.
    // The time may have gone on the page: the databases are then not asked, and what the page said has to do.
    let found = if sources::out_of_time(&mut troubles) { Err(String::new()) } else { lookup::find(&id, fetch) };
    let mut paper = match found {
        Ok(found) if meta.title.is_empty() => found,
        Ok(found) => lookup::merge_lookup(found, meta.paper()),
        Err(_) if !meta.title.is_empty() => meta.paper(),
        Err(err) => {
            troubles.push(err);
            row["outcome"] = json!("failed");
            row["reason"] = json!(sources::reasons(&troubles));
            return Ok(row);
        }
    };
    if paper.url.is_empty() { paper.url = url.clone() }
    let mut result = store.import_papers(vec![paper])?;
    let paper = match (result.added.pop(), result.skipped.pop()) {
        (Some(added), _) => {
            row["outcome"] = json!("added");
            added
        }
        (None, Some(held)) => {
            row["paperId"] = json!(held.existing_id);
            row["title"] = json!(held.title);
            if held.in_trash {
                row["outcome"] = json!("in_trash");
                row["reason"] = json!("这篇文献在回收站里，没有导入。");
                return Ok(row);
            }
            row["outcome"] = json!("held");
            store.get_paper(&held.existing_id)?
        }
        (None, None) => bail!("没有查到这篇文献。"),
    };
    row["paperId"] = json!(paper.id);
    row["title"] = json!(paper.title);
    if matches!(paper.fulltext.as_str(), "stored" | "textless") {
        row["pdf"] = json!("had");
        return Ok(row);
    }
    // The publisher's own file first; the open copies the indexes know of when it is not to be had.
    let first = (!meta.pdf_url.is_empty()).then(|| Candidate { url: meta.pdf_url.clone(), source: webpage::host(&meta.pdf_url).to_string(), version: "" });
    let fetched = if sources::out_of_time(&mut troubles) { json!({}) } else { fetch_pdf_from(store, &paper, first, false, fetch, download)? };
    if fetched["outcome"] == "saved" {
        row["pdf"] = json!("saved");
        for (key, from) in [("pdfSource", "source"), ("pdfUrl", "url"), ("pages", "pages"), ("textKind", "textKind"), ("textReason", "textReason")] {
            if !fetched[from].is_null() { row[key] = fetched[from].clone() }
        }
    } else {
        row["pdf"] = json!("none");
        troubles.extend(fetched["reason"].as_str().map(str::to_string));
        row["pdfReason"] = json!(sources::reasons(&troubles));
    }
    Ok(row)
}

/// A PDF fetched from an address, taken in as a dropped one is: the DOI on its first pages looked up, the same file
/// under another name known by its bytes, a paper already held given the file, one in the trash left alone.
fn take_pdf(store: &mut Store, origin: &str, bytes: &[u8], fetch: Fetch, mut row: Value) -> Result<Value> {
    let pages = fulltext::pdf_pages(bytes);
    let doi = pages.as_ref().ok().and_then(|pages| lookup::find_doi(&pages.iter().take(2).cloned().collect::<Vec<_>>().join("\n")));
    let found = doi.as_ref().and_then(|doi| lookup::find(&Identifier::Doi(doi.clone()), fetch).ok());
    let recognised = found.is_some();
    let mut paper = found.unwrap_or_else(|| Paper { title: webpage::file_stem(origin), doi: doi.unwrap_or_default(), source: "网址导入".into(), ..Default::default() });
    if paper.url.is_empty() { paper.url = origin.to_string() }
    let held = match store.paper_holding_bytes(bytes)? {
        Some(id) => Some((id, true)),
        None => {
            let mut result = store.import_papers(vec![paper])?;
            match (result.added.pop(), result.skipped.pop()) {
                (Some(added), _) => {
                    row["outcome"] = json!("added");
                    row["unrecognised"] = json!(!recognised);
                    Some((added.id, false))
                }
                (None, Some(skipped)) => Some((skipped.existing_id, false)),
                (None, None) => None,
            }
        }
    };
    let Some((id, same_file)) = held else { bail!("没有查到这篇文献。") };
    let paper = store.get_paper(&id)?;
    (row["paperId"], row["title"]) = (json!(paper.id), json!(paper.title));
    if store.in_trash(&id)? {
        row["outcome"] = json!("in_trash");
        row["reason"] = json!("这篇文献在回收站里，没有导入。");
        return Ok(row);
    }
    // The very file is there already: nothing to do.
    if same_file {
        row["outcome"] = json!("held");
        row["pdf"] = json!("had");
        return Ok(row);
    }
    if row["outcome"].is_null() { row["outcome"] = json!("attached") }
    // A paper that has its full text gets the file beside it, as a dropped one does.
    let kind = if store.paper_files(&id)?.iter().any(|f| f.kind == "fulltext") { "supplement" } else { "fulltext" };
    let file = store.import_fetched(&id, &file_name(&paper), bytes, kind, origin, webpage::host(origin), "")?;
    if store.needs_structure(&file.id)? { store.save_fulltext_pages(&file.id, &pages)? }
    row["pdf"] = json!("saved");
    row["pdfSource"] = json!(webpage::host(origin));
    row["pdfUrl"] = json!(origin);
    match pages {
        Ok(pages) => row["pages"] = json!(pages.len()),
        Err(reason) => {
            row["textKind"] = json!(fulltext::failure_kind(&reason));
            row["textReason"] = json!(reason);
        }
    }
    Ok(row)
}

#[cfg(test)]
mod tests {
    use crate::libraries;
    use crate::store::Store;
    use crate::tools::{call, tests::sample};
    use serde_json::{Value, json};

    /// Crossref knows the DOIs under 10.1186 and 10.1371; Europe PMC and OpenAlex know nothing, unless `open` names an
    /// open copy for OpenAlex to give.
    fn index(open: &'static str) -> impl Fn(&str) -> Result<String, String> {
        move |url: &str| {
            if url.contains("api.crossref.org") {
                let doi = url.rsplit("/works/").next().unwrap_or("").replace("%2F", "/");
                if !doi.starts_with("10.1186/") && !doi.starts_with("10.1371/") { return Err("api.crossref.org 返回 HTTP 404。".into()) }
                return Ok(json!({"message": {"title": [format!("Paper {doi}")], "DOI": doi, "author": [{"family": "Zhu", "given": "Mo"}], "issued": {"date-parts": [[2021]]}, "type": "journal-article"}}).to_string());
            }
            if url.contains("openalex") && !open.is_empty() { return Ok(json!({"open_access": {"is_oa": true}, "best_oa_location": {"is_oa": true, "pdf_url": open, "version": "acceptedVersion"}, "locations": []}).to_string()) }
            if url.contains("openalex") { return Ok(json!({"open_access": {"is_oa": false}, "best_oa_location": null, "locations": []}).to_string()) }
            Ok(json!({"resultList": {"result": []}}).to_string())
        }
    }

    fn import(root: &std::path::Path, url: &str, fetch: &dyn Fn(&str) -> Result<String, String>, download: &dyn Fn(&str) -> Result<Vec<u8>, String>) -> Value {
        call(root, "import_url", &json!({"url": url}), fetch, download).unwrap()
    }

    #[test]
    fn a_publisher_page_brings_its_record_and_its_pdf() {
        let (root, _) = sample();
        let article = crate::fulltext::text_pdf(&["Root hairs in dry soil\nAbstract\nRoots respond to drought.", "Introduction\nRoot hairs grow."]);
        let page = "https://bmcplantbiol.biomedcentral.com/articles/10.1186/s12870-021-03000-1";
        // `content` before `name`, single quotes, a relative address with an entity in it.
        let html = "<html><head><meta content='10.1186/s12870-021-03000-1' name='citation_doi'><meta content=\"/counter/pdf/10.1186/s12870-021-03000-1.pdf?a=1&amp;b=2\" name=\"citation_pdf_url\"></head></html>";
        let asked = std::cell::RefCell::new(vec![]);
        let files = |url: &str| -> Result<Vec<u8>, String> {
            asked.borrow_mut().push(url.to_string());
            if url == page { Ok(html.as_bytes().to_vec()) } else if url == "https://bmcplantbiol.biomedcentral.com/counter/pdf/10.1186/s12870-021-03000-1.pdf?a=1&b=2" { Ok(article.clone()) } else { Err(format!("unexpected {url}")) }
        };
        let added = import(&root, page, &index(""), &files);
        assert_eq!((added["outcome"].as_str(), added["pdf"].as_str(), added["pages"].as_u64(), added["pdfSource"].as_str()), (Some("added"), Some("saved"), Some(2), Some("bmcplantbiol.biomedcentral.com")));
        // The page and the file it names, nothing else.
        assert_eq!(asked.borrow().len(), 2);
        let id = added["paperId"].as_str().unwrap();
        let store = Store::open(&libraries::list(&root)[0].dir).unwrap();
        let paper = store.get_paper(id).unwrap();
        assert_eq!((paper.title.as_str(), paper.doi.as_str(), paper.fulltext.as_str()), ("Paper 10.1186/s12870-021-03000-1", "10.1186/s12870-021-03000-1", "stored"));
        let file = store.paper_files(id).unwrap().remove(0);
        assert_eq!(store.file_origins().unwrap()[&file.id].0, "https://bmcplantbiol.biomedcentral.com/counter/pdf/10.1186/s12870-021-03000-1.pdf?a=1&b=2");
        drop(store);
        // Pasted again: the paper is the one the library holds, and it has its PDF.
        let again = import(&root, page, &index(""), &files);
        assert_eq!((again["outcome"].as_str(), again["pdf"].as_str(), again["paperId"].as_str()), (Some("held"), Some("had"), Some(id)));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_refused_page_is_added_by_the_doi_in_its_address() {
        let (root, _) = sample();
        let refused = |_: &str| -> Result<Vec<u8>, String> { Err("pubs.example.org 返回 HTTP 403。".into()) };
        let added = import(&root, "https://pubs.example.org/doi/full/10.1371/journal.pone.0230000", &index(""), &refused);
        assert_eq!((added["outcome"].as_str(), added["pdf"].as_str()), (Some("added"), Some("none")));
        assert_eq!(added["pdfReason"], "打开网页：pubs.example.org 返回 HTTP 403；没有找到开放获取的全文。可以打开文献的链接自己下载，再把 PDF 拖进窗口：它会附到这一条上。");
        let store = Store::open(&libraries::list(&root)[0].dir).unwrap();
        assert_eq!(store.get_paper(added["paperId"].as_str().unwrap()).unwrap().doi, "10.1371/journal.pone.0230000");
        drop(store);
        // An open copy the index knows of is still had.
        let article = crate::fulltext::text_pdf(&["An open copy\nAbstract\nText."]);
        let open = |url: &str| -> Result<Vec<u8>, String> { if url.contains("repo.example") { Ok(article.clone()) } else { Err("pubs.example.org 返回 HTTP 403。".into()) } };
        let other = import(&root, "https://pubs.example.org/doi/10.1371/journal.pone.0230001", &index("https://repo.example/open.pdf"), &open);
        assert_eq!((other["outcome"].as_str(), other["pdf"].as_str(), other["pdfSource"].as_str()), (Some("added"), Some("saved"), Some("OpenAlex")));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_login_page_or_a_supplement_is_not_kept_as_the_pdf() {
        let (root, _) = sample();
        let html = r#"<meta name="citation_doi" content="10.1186/walled-1"><meta name="citation_pdf_url" content="https://journal.example/pdf/walled-1">"#;
        let login = |url: &str| -> Result<Vec<u8>, String> { Ok(if url.contains("/pdf/") { b"<html>Sign in through your institution</html>".to_vec() } else { html.as_bytes().to_vec() }) };
        let walled = import(&root, "https://journal.example/article/walled-1", &index(""), &login);
        assert_eq!((walled["outcome"].as_str(), walled["pdf"].as_str()), (Some("added"), Some("none")));
        assert_eq!(walled["pdfReason"], "journal.example：返回的不是 PDF，多半是登录页或验证页；拿不到文件时，可以在浏览器里打开文献的链接下载，再把 PDF 拖进窗口。");
        let supplement = crate::fulltext::text_pdf(&["Supplementary Figure 1. Gene expression.\nVolcano plot", "Supplementary Table 2. Primers."]);
        let html = r#"<meta name="citation_doi" content="10.1186/supp-1"><meta name="citation_pdf_url" content="https://journal.example/pdf/supp-1">"#;
        let given = |url: &str| -> Result<Vec<u8>, String> { Ok(if url.contains("/pdf/") { supplement.clone() } else { html.as_bytes().to_vec() }) };
        let supp = import(&root, "https://journal.example/article/supp-1", &index(""), &given);
        assert_eq!(supp["pdf"], "none");
        assert!(supp["pdfReason"].as_str().unwrap().starts_with("journal.example：给出的文件是补充材料"));
        let store = Store::open(&libraries::list(&root)[0].dir).unwrap();
        assert!(store.paper_files(supp["paperId"].as_str().unwrap()).unwrap().is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_pdf_address_is_taken_in_as_a_dropped_file() {
        let (root, _) = sample();
        let pdf = crate::fulltext::text_pdf(&["Root hairs\ndoi:10.1186/s12870-021-09999-9\nAbstract\nRoots.", "Introduction\nText."]);
        let files = |_: &str| -> Result<Vec<u8>, String> { Ok(pdf.clone()) };
        let added = import(&root, "https://journal.example/content/pdf/root.pdf", &index(""), &files);
        assert_eq!((added["outcome"].as_str(), added["pdf"].as_str(), added["unrecognised"].as_bool()), (Some("added"), Some("saved"), Some(false)));
        let store = Store::open(&libraries::list(&root)[0].dir).unwrap();
        let paper = store.get_paper(added["paperId"].as_str().unwrap()).unwrap();
        assert_eq!((paper.doi.as_str(), paper.fulltext.as_str()), ("10.1186/s12870-021-09999-9", "stored"));
        drop(store);
        // The same bytes under another address are the paper that holds them.
        let again = import(&root, "https://mirror.example/root-copy.pdf", &index(""), &files);
        assert_eq!((again["outcome"].as_str(), again["pdf"].as_str(), again["paperId"].as_str()), (Some("held"), Some("had"), added["paperId"].as_str()));
        // A PDF naming no DOI is still a paper, named after its file, for the person to complete.
        let nameless = crate::fulltext::text_pdf(&["Some notes\nNo number here."]);
        let plain = |_: &str| -> Result<Vec<u8>, String> { Ok(nameless.clone()) };
        let unknown = import(&root, "https://example.org/files/notes-2021.pdf", &index(""), &plain);
        assert_eq!((unknown["outcome"].as_str(), unknown["unrecognised"].as_bool(), unknown["title"].as_str()), (Some("added"), Some(true), Some("notes-2021")));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn an_address_that_is_no_paper_adds_nothing() {
        let (root, _) = sample();
        let before = call(&root, "list_papers", &json!({}), &index(""), &crate::tools::tests::no_download).unwrap()["total"].clone();
        let blog = |_: &str| -> Result<Vec<u8>, String> { Ok(b"<html><head><title>A blog</title><meta name=\"description\" content=\"Hello\"></head></html>".to_vec()) };
        let none = import(&root, "https://blog.example/2021/10/roots", &index(""), &blog);
        assert_eq!(none["outcome"], "not_recognised");
        assert!(none["reason"].as_str().unwrap().contains("没有在这个网址里认出文献"));
        assert_eq!(call(&root, "list_papers", &json!({}), &index(""), &crate::tools::tests::no_download).unwrap()["total"], before);
        assert!(call(&root, "import_url", &json!({"url": "10.1186/abc"}), &index(""), &crate::tools::tests::no_download).unwrap_err().contains("不是网址"));
        // A preprint server's page whose number no database knows: the page's own record is kept.
        let arxiv = |url: &str| -> Result<Vec<u8>, String> {
            if url.contains("/abs/") { Ok(br#"<meta name="citation_title" content="Attention Is All You Need" /><meta name="citation_author" content="Vaswani, Ashish" /><meta name="citation_date" content="2017/06/12" /><meta name="citation_pdf_url" content="https://arxiv.org/pdf/1706.03762" /><meta name="citation_arxiv_id" content="1706.03762" />"#.to_vec()) } else { Ok(crate::fulltext::text_pdf(&["Attention Is All You Need\nAbstract\nThe dominant sequence transduction models."])) }
        };
        let preprint = import(&root, "https://arxiv.org/abs/1706.03762", &index(""), &arxiv);
        assert_eq!((preprint["outcome"].as_str(), preprint["pdf"].as_str(), preprint["title"].as_str()), (Some("added"), Some("saved"), Some("Attention Is All You Need")));
        let store = Store::open(&libraries::list(&root)[0].dir).unwrap();
        let paper = store.get_paper(preprint["paperId"].as_str().unwrap()).unwrap();
        assert_eq!((paper.doi.as_str(), paper.year, paper.authors.as_str()), ("10.48550/arxiv.1706.03762", Some(2017), "Vaswani, Ashish"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn an_address_takes_no_longer_than_its_time() {
        use crate::sources::{self, TIME_UP};
        let (root, _) = sample();
        let asked = std::cell::Cell::new(0);
        let counted = |url: &str| {
            asked.set(asked.get() + 1);
            index("https://repo.example/open.pdf")(url)
        };
        // A page that takes all the time there is to arrive: its own tags make the record, and nothing more is asked of
        // anyone, neither the databases nor the file the page names.
        let html = r#"<meta name="citation_title" content="A slow page"><meta name="citation_doi" content="10.1186/slow-1"><meta name="citation_pdf_url" content="https://slow.example/pdf/slow-1">"#;
        let slow = |_: &str| -> Result<Vec<u8>, String> {
            asked.set(asked.get() + 1);
            while !sources::time_up() { std::thread::sleep(std::time::Duration::from_millis(5)) }
            Ok(html.as_bytes().to_vec())
        };
        let late = sources::within(std::time::Duration::from_secs(1), || import(&root, "https://slow.example/article/slow-1", &counted, &slow));
        assert_eq!((late["outcome"].as_str(), late["pdf"].as_str(), late["title"].as_str()), (Some("added"), Some("none"), Some("A slow page")));
        assert_eq!(late["pdfReason"], format!("{TIME_UP}。"));
        assert_eq!(asked.get(), 1);
        // With no time at all nothing is asked, and the reason is said once.
        let none = sources::within(std::time::Duration::ZERO, || import(&root, "https://slow.example/doi/10.1371/journal.pone.0230000", &counted, &slow));
        assert_eq!((none["outcome"].as_str(), asked.get()), (Some("failed"), 1));
        assert_eq!(none["reason"], format!("打开网页：{TIME_UP}。"));
        // The limit goes with the work: what comes after has all its time again.
        assert!(!sources::time_up() && sources::time_left().is_none());
        let after = import(&root, "https://slow.example/doi/10.1371/journal.pone.0230000", &counted, &|url: &str| if url.contains("repo.example") { Ok(crate::fulltext::text_pdf(&["An open copy\nAbstract\nText."])) } else { Err("slow.example 返回 HTTP 403。".into()) });
        assert_eq!((after["outcome"].as_str(), after["pdf"].as_str()), (Some("added"), Some("saved")));
        std::fs::remove_dir_all(root).ok();
    }
}
