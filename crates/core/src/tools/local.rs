//! Local collection tools. The parsers, deduplication and attachment copier
//! are the same ones used by the window; no network calls are made here.

use super::text;
use crate::{fulltext, import, libraries, lookup, zotero};
use crate::model::{Paper, clean_strings};
use crate::libraries::LibraryInfo;
use crate::store::Store;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};
use std::io::Read;
use std::path::{Path, PathBuf};

pub(super) fn create_library(root: &Path, args: &Value) -> Result<Value> {
    let color = if args["color"].is_null() { "blue" } else { text(args, "color")? };
    let info = libraries::create(root, text(args, "name")?, text(args, "topic")?, color)?;
    Ok(json!({"id": info.id, "name": info.name, "topic": info.topic, "color": info.color, "directory": info.dir, "papers": 0}))
}

/// A `.wispstudy` file from another Wisp Study (or `.wisppaper`, from before the rename), taken in as a new library.
pub(super) fn import_library(root: &Path, args: &Value) -> Result<Value> {
    let path = input_path(args)?;
    let info = crate::archive::import(root, &path)?;
    Ok(json!({"id": info.id, "name": info.name, "topic": info.topic, "color": info.color, "directory": info.dir, "papers": info.paper_count}))
}

/// The whole library as a `.wispstudy` file at a path the person chose; an existing file is left alone.
pub(super) fn export_library(info: &LibraryInfo, args: &Value) -> Result<Value> {
    let path = PathBuf::from(text(args, "path")?);
    if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case(crate::archive::EXTENSION)) { bail!("请选择 .wispstudy 文件路径。") }
    if path.exists() { bail!("{} 已存在，不会覆盖。", path.display()) }
    let attachments = args["attachments"].as_bool().unwrap_or(true);
    let manifest = crate::archive::export(info, &path, attachments)?;
    Ok(json!({"path": path, "papers": manifest.papers, "files": manifest.files, "bytes": std::fs::metadata(&path).map_or(0, |m| m.len()), "attachments": attachments}))
}

fn input_path(args: &Value) -> Result<PathBuf> {
    let value = text(args, "path")?;
    if value.is_empty() { bail!("请提供本机文件或文件夹路径。") }
    let path = std::fs::canonicalize(value).with_context(|| format!("无法打开路径：{value}"))?;
    Ok(path)
}

pub(super) fn import_references(store: &mut Store, args: &Value) -> Result<Value> {
    let path = input_path(args)?;
    let bytes = std::fs::read(&path).with_context(|| format!("无法读取 {}", path.display()))?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let papers = import::parse_references(&String::from_utf8_lossy(&bytes), &format!("导入 · {name}")).map_err(anyhow::Error::msg)?;
    let total = papers.len();
    let result = store.import_papers(papers)?;
    Ok(json!({
        "path": path, "total": total, "added": result.added.len(), "skipped": result.skipped.len(),
        "addedPapers": result.added.iter().map(|p| json!({"id": p.id, "title": p.title})).collect::<Vec<_>>(),
        "skippedPapers": result.skipped.iter().map(|p| json!({"title": p.title, "existingId": p.existing_id, "inTrash": p.in_trash})).collect::<Vec<_>>(),
    }))
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct Row {
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<PathBuf>,
    #[serde(flatten)]
    record: zotero::Row,
    /// Text extraction failure does not mean that the PDF was not copied.
    #[serde(skip_serializing_if = "String::is_empty")]
    text_error: String,
}

fn report(path: &Path, rows: Vec<Row>) -> Value {
    json!({
        "path": path, "total": rows.len(),
        "added": rows.iter().filter(|r| r.record.added).count(),
        "skipped": rows.iter().filter(|r| !r.record.added && (r.record.paper_id.is_some() || r.record.in_trash)).count(),
        "inTrash": rows.iter().filter(|r| r.record.in_trash).count(),
        "filesAdded": rows.iter().map(|r| r.record.files_added).sum::<usize>(),
        "filesExisting": rows.iter().map(|r| r.record.files_existing).sum::<usize>(),
        "failed": rows.iter().filter(|r| !r.record.problems.is_empty()).count(),
        "textless": rows.iter().filter(|r| !r.text_error.is_empty()).count(),
        "results": rows,
    })
}

pub(super) fn import_zotero(store: &mut Store, args: &Value) -> Result<Value> {
    let path = input_path(args)?;
    let imported = zotero::read(&path)?;
    let rows = imported.entries.into_iter().map(|entry| Row { record: zotero::import_entry(store, entry), ..Default::default() }).collect();
    let mut result = report(&path, rows);
    // Zotero copies attachments here; text is extracted by the reading tools.
    result.as_object_mut().unwrap().remove("textless");
    result["otherAttachments"] = json!(imported.other_attachments);
    Ok(result)
}

pub(super) fn import_pdf(store: &mut Store, args: &Value) -> Result<Value> {
    let path = input_path(args)?;
    let paper_id = text(args, "paperId")?;
    if path.is_dir() && !paper_id.is_empty() { bail!("paperId 只能用于单个 PDF，不能用于文件夹。") }
    // Validate an explicit destination before starting any work.
    let target = if paper_id.is_empty() { None } else { Some(store.get_paper(paper_id)?) };
    let scanned = if path.is_dir() { import::scan_pdf_folder(&path) } else { import::PdfFolder { files: vec![(path.clone(), vec![])], errors: vec![] } };
    if scanned.files.is_empty() && scanned.errors.is_empty() { bail!("文件夹 {} 中没有 PDF。", path.display()) }
    let mut rows: Vec<Row> = scanned.errors.into_iter().map(|(path, error)| Row { path: Some(path), record: zotero::Row { problems: vec![error], ..Default::default() }, ..Default::default() }).collect();
    for (file, tags) in scanned.files {
        let mut row = Row { path: Some(file.clone()), ..Default::default() };
        if let Err(error) = import_one_pdf(store, &file, &tags, target.as_ref(), &mut row) { row.record.problems.push(format!("{error:#}")) }
        rows.push(row);
    }
    Ok(report(&path, rows))
}

fn import_one_pdf(store: &mut Store, path: &Path, tags: &[String], target: Option<&Paper>, row: &mut Row) -> Result<()> {
    if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")) { bail!("请选择 PDF 文件：{}", path.display()) }
    // Reject a missing file or a disguised HTML download before adding a
    // paper. A genuine PDF without readable text is still worth collecting.
    let mut header = vec![];
    std::fs::File::open(path)?.take(1024).read_to_end(&mut header)?;
    if !crate::openaccess::is_pdf(&header) { bail!("文件没有 PDF 标识：{}", path.display()) }
    let pages = fulltext::file_pages(path);
    // The same file under another name is the paper that holds it, whether or not it names a DOI.
    let held = match target {
        Some(_) => None,
        None => store.paper_holding(path)?.map(|id| store.get_paper(&id)).transpose()?,
    };
    let paper = target.cloned().or(held).unwrap_or_else(|| Paper {
        title: path.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
        doi: pages.as_ref().ok().and_then(|pages| lookup::find_doi(&pages.iter().take(2).cloned().collect::<Vec<_>>().join("\n"))).unwrap_or_default(),
        source: "PDF 导入".into(),
        tags: tags.to_vec(),
        ..Default::default()
    });
    row.record = zotero::import_entry(store, zotero::Entry { paper, pdfs: vec![Ok(path.to_path_buf())] });
    let Some(id) = row.record.paper_id.as_deref() else { return Ok(()) };
    if !row.record.problems.is_empty() { return Ok(()) }
    let mut paper = store.get_paper(id)?;
    if tags.iter().any(|tag| !paper.tags.contains(tag)) {
        paper.tags = clean_strings(paper.tags.iter().chain(tags));
        store.save_paper(&mut paper)?;
    }
    // A repeated attachment returns the existing resource ID. Match IDs,
    // not filenames: two different attachments may have the same name.
    for file in store.paper_files(id)? {
        if row.record.file_ids.contains(&file.id) && store.needs_structure(&file.id)? {
            store.save_fulltext_pages(&file.id, &pages)?;
        }
    }
    if let Err(error) = pages { row.text_error = error }
    Ok(())
}

/// A Word file written elsewhere, read for what it cites (`restyle::inspect`): `lookup` asks Crossref about the
/// entries neither the library nor their own text settles, `entries` are records the caller read better than the rules.
fn docx_cited(store: &Store, args: &Value, fetch: crate::sources::Fetch) -> Result<(PathBuf, Vec<u8>, crate::restyle::Inspected)> {
    let path = input_path(args)?;
    if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("docx")) { bail!("这里只读 Word 的 .docx 文件：{}", path.display()) }
    let bytes = std::fs::read(&path).with_context(|| format!("无法读取 {}", path.display()))?;
    let mut given: Vec<(usize, crate::csl::Record)> = vec![];
    for entry in args["entries"].as_array().into_iter().flatten() {
        let (Some(n), Some(record)) = (entry["n"].as_u64(), entry["csl"].as_object()) else { bail!("entries 的每一项应有 n（参考文献表里的第几条）和 csl（这一条的 CSL-JSON 对象）。") };
        given.push((n as usize, record.clone()));
    }
    let papers = store.list_papers()?;
    let inspected = crate::restyle::inspect(&bytes, &papers, &given, args["lookup"].as_bool().unwrap_or(false).then_some(fetch))?;
    Ok((path, bytes, inspected))
}

pub(super) fn read_docx_citations(store: &Store, args: &Value, fetch: crate::sources::Fetch) -> Result<Value> {
    let (path, _, inspected) = docx_cited(store, args, fetch)?;
    let mut report = inspected.report();
    report["path"] = json!(path);
    let unresolved = report["unresolved"].as_array().map_or(0, Vec::len);
    report["next"] = json!(if unresolved > 0 {
        "unresolved 里的条目没能识别：它们会原样留在参考文献表末尾，引用它们的地方也保持原样。可以带 lookup: true 再读一次（联网查 Crossref），或者你自己读懂这一条的文字后用 entries 给出它的 CSL-JSON（只转写条目里写着的信息，不要补写没有的）。把识别结果和保持原样的引用如实告诉用户，再用 restyle_docx 输出。"
    } else {
        "把识别结果（来源、条目数、保持原样的引用）告诉用户，问清要用的引用样式，再用 restyle_docx 输出新文件。"
    });
    Ok(report)
}

pub(super) fn restyle_docx(store: &Store, info: &LibraryInfo, args: &Value, fetch: crate::sources::Fetch) -> Result<Value> {
    let root = info.dir.parent().map(Path::to_path_buf).unwrap_or_else(|| info.dir.clone());
    let style = match (text(args, "csl")?, text(args, "style")?) {
        ("", "") => bail!("需要 style（引用样式的 key，见 list_cite_styles）或 csl（一份 CSL 定义）。"),
        ("", key) if !key.contains(':') => bail!("style 应为 CSL 样式的 key，例如 csl:nature、csl:apa、file:my-journal.csl；见 list_cite_styles。"),
        ("", key) => crate::csl::load(key, &root)?,
        (xml, _) => crate::csl::parse(xml, &root)?,
    };
    let (path, bytes, inspected) = docx_cited(store, args, fetch)?;
    let output = match text(args, "output")? {
        "" => path.with_file_name(format!("{}-restyled.docx", path.file_stem().unwrap_or_default().to_string_lossy())),
        output => PathBuf::from(output),
    };
    if !output.extension().is_some_and(|e| e.eq_ignore_ascii_case("docx")) { bail!("输出路径应是 .docx 文件：{}", output.display()) }
    // The file the person gave is never written over, nor any other.
    if output.exists() { bail!("{} 已存在，不会覆盖。请换一个文件名。", output.display()) }
    let options = crate::restyle::Options { track: args["track"].as_bool().unwrap_or(false), author: match text(args, "author")? { "" => "Wisp Study".into(), author => author.to_string() }, date: crate::time::now_iso(), language: text(args, "language")?.to_string() };
    let done = crate::restyle::restyle(&bytes, &inspected, &style, &options)?;
    std::fs::write(&output, &done.bytes).with_context(|| format!("无法写入 {}", output.display()))?;
    let report = inspected.report();
    Ok(json!({
        "output": output,
        "source": path,
        "style": if style.name.is_empty() { style.key.clone() } else { style.name.clone() },
        "tracked": options.track,
        "citations": {"rewritten": done.rewritten, "keptAsWritten": report["citations"]["keptAsWritten"], "sample": done.sample.iter().map(|(before, after)| json!({"before": before, "after": after})).collect::<Vec<_>>()},
        "references": {"entries": done.entries, "leftAsWritten": done.unresolved, "sample": done.list_sample},
        "unresolved": report["unresolved"],
        "notes": report["notes"],
        "next": "新文件已写好，原文件没有改动。告诉用户文件在哪里、改写了多少处引用、哪些保持原样及原因；请用户在 Word 里打开核对。",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools;

    struct Fixture { root: PathBuf, id: String }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(crate::model::new_id("wisp-local-tools"));
            let created = tools::call(&root, "create_library", &json!({"name": "植物 单细胞", "topic": "根尖", "color": "green"}), &tools::tests::offline, &tools::tests::no_download).unwrap();
            Self { root, id: created["id"].as_str().unwrap().into() }
        }

        fn call(&self, name: &str, mut args: Value) -> Result<Value, String> {
            if name != "create_library" { args["library"] = json!(self.id) }
            tools::call(&self.root, name, &args, &tools::tests::offline, &tools::tests::no_download)
        }

        fn store(&self) -> Store { Store::open(&self.root.join(&self.id)).unwrap() }
    }

    impl Drop for Fixture {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.root); }
    }

    fn pdf(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let content = format!("BT /F1 12 Tf 20 150 Td ({text}) Tj ET");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 500 200] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".into(),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        ];
        let mut bytes = "%PDF-1.4\n".to_string();
        let mut offsets = vec![];
        for (i, object) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            bytes.push_str(&format!("{} 0 obj\n{object}\nendobj\n", i + 1));
        }
        let xref = bytes.len();
        bytes.push_str("xref\n0 6\n0000000000 65535 f \n");
        for offset in offsets { bytes.push_str(&format!("{offset:010} 00000 n \n")) }
        bytes.push_str(&format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"));
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn creates_libraries_and_imports_reference_files_without_overwriting_edits() {
        let fixture = Fixture::new();
        assert_eq!(fixture.call("library_status", json!({})).unwrap()["topic"], "根尖");
        for args in [json!({"name": " "}), json!({"name": "x", "color": "invalid"}), json!({"name": 42})] {
            assert!(fixture.call("create_library", args).is_err());
        }
        assert_eq!(libraries::list(&fixture.root).len(), 1);
        let bib = fixture.root.join("引用 文献.bib");
        std::fs::write(&bib, "\u{FEFF}@article{a, title={Root atlas}, doi={10.1234/atlas}, year={2024}}\n@article{b, title={Leaf atlas}}").unwrap();
        let first = fixture.call("import_references", json!({"path": bib})).unwrap();
        assert_eq!((first["total"].as_u64(), first["added"].as_u64(), first["skipped"].as_u64()), (Some(2), Some(2), Some(0)));
        let id = first["addedPapers"][0]["id"].as_str().unwrap();
        let mut store = fixture.store();
        let mut held = store.get_paper(id).unwrap();
        held.notes = "Keep my notes".into();
        store.save_paper(&mut held).unwrap();
        store.trash_paper(first["addedPapers"][1]["id"].as_str().unwrap()).unwrap();
        drop(store);
        let second = fixture.call("import_references", json!({"path": bib})).unwrap();
        assert_eq!((second["added"].as_u64(), second["skipped"].as_u64(), second["skippedPapers"][1]["inTrash"].as_bool()), (Some(0), Some(2), Some(true)));
        let ris = fixture.root.join("references.ris");
        std::fs::write(&ris, "TY  - JOUR\nTI  - A changed title\nDO  - 10.1234/atlas\nER  - \nTY  - JOUR\nTI  - New RIS paper\nER  - \n").unwrap();
        let third = fixture.call("import_references", json!({"path": ris})).unwrap();
        assert_eq!((third["added"].as_u64(), third["skipped"].as_u64()), (Some(1), Some(1)));
        assert_eq!(fixture.store().get_paper(id).unwrap().notes, "Keep my notes");
        std::fs::write(&ris, "Not a reference file").unwrap();
        assert!(fixture.call("import_references", json!({"path": ris})).is_err());
        assert!(fixture.call("import_references", json!({"path": fixture.root.join("absent.bib")})).is_err());
        assert!(fixture.call("import_references", json!({"path": ""})).is_err());
        assert_eq!(fixture.store().list_papers().unwrap().len(), 2); // The other record stays in trash.
    }

    #[test]
    fn pdf_folder_reports_duplicates_trash_invalid_files_and_textless_pdfs() {
        let fixture = Fixture::new();
        let folder = fixture.root.join("PDF collection");
        let source = folder.join("植物/根/paper.pdf");
        pdf(&source, "Root cells. DOI: 10.1234/atlas");
        let bytes = std::fs::read(&source).unwrap();
        let mut store = fixture.store();
        let existing = store.add_paper(Paper { title: "Root atlas".into(), doi: "10.1234/atlas".into(), notes: "My notes".into(), ..Default::default() }).unwrap();
        drop(store);
        pdf(&folder.join("scan.pdf"), "");
        pdf(&folder.join(".hidden/hidden.pdf"), "Hidden paper");
        std::fs::write(folder.join("bad.pdf"), "<html>not a PDF</html>").unwrap();
        std::fs::write(folder.join("notes.txt"), "Not a PDF").unwrap();
        let first = fixture.call("import_pdf", json!({"path": folder})).unwrap();
        assert_eq!((first["total"].as_u64(), first["added"].as_u64(), first["skipped"].as_u64(), first["failed"].as_u64()), (Some(3), Some(1), Some(1), Some(1)));
        assert_eq!((first["filesAdded"].as_u64(), first["textless"].as_u64()), (Some(2), Some(1)));
        let held = fixture.store().get_paper(&existing.id).unwrap();
        assert_eq!((held.title.as_str(), held.notes.as_str()), ("Root atlas", "My notes"));
        assert_eq!(held.tags, ["植物", "根"]);
        let read = fixture.call("read_fulltext", json!({"paperId": existing.id})).unwrap();
        assert!(read["pages"][0]["text"].as_str().unwrap().contains("Root cells"));
        let repeated = fixture.call("import_pdf", json!({"path": source})).unwrap();
        assert_eq!((repeated["added"].as_u64(), repeated["filesAdded"].as_u64(), repeated["filesExisting"].as_u64()), (Some(0), Some(0), Some(1)));
        assert_eq!(bytes, std::fs::read(&source).unwrap());
        fixture.store().trash_paper(&existing.id).unwrap();
        let trashed = fixture.call("import_pdf", json!({"path": source})).unwrap();
        assert_eq!((trashed["inTrash"].as_u64(), trashed["filesAdded"].as_u64()), (Some(1), Some(0)));
        assert!(fixture.call("import_pdf", json!({"path": folder, "paperId": existing.id})).is_err());
        let empty = fixture.root.join("empty");
        std::fs::create_dir(&empty).unwrap();
        assert!(fixture.call("import_pdf", json!({"path": empty})).unwrap_err().contains("没有 PDF"));
        assert!(!import::scan_pdf_folder(&fixture.root.join("missing")).errors.is_empty());
    }

    #[test]
    fn the_same_pdf_under_another_name_is_not_a_second_paper() {
        let fixture = Fixture::new();
        let (first, second, other) = (fixture.root.join("in/Zhu 2021.pdf"), fixture.root.join("in/下载 (1).pdf"), fixture.root.join("in/other.pdf"));
        // No DOI on its pages: the record is named after the file, and nothing but the file says the two are one.
        pdf(&first, "Root cells without a number");
        std::fs::copy(&first, &second).unwrap();
        pdf(&other, "Leaf cells without a number");
        let added = fixture.call("import_pdf", json!({"path": first})).unwrap();
        assert_eq!((added["added"].as_u64(), added["filesAdded"].as_u64()), (Some(1), Some(1)));
        let id = added["results"][0]["paperId"].as_str().unwrap().to_string();
        let again = fixture.call("import_pdf", json!({"path": second})).unwrap();
        assert_eq!((again["added"].as_u64(), again["skipped"].as_u64(), again["filesAdded"].as_u64(), again["filesExisting"].as_u64()), (Some(0), Some(1), Some(0), Some(1)));
        assert_eq!(again["results"][0]["paperId"].as_str(), Some(id.as_str()));
        // A folder holding both and a third: one new paper.
        let folder = fixture.call("import_pdf", json!({"path": fixture.root.join("in")})).unwrap();
        assert_eq!((folder["total"].as_u64(), folder["added"].as_u64(), folder["skipped"].as_u64()), (Some(3), Some(1), Some(2)));
        let store = fixture.store();
        assert_eq!(store.list_papers().unwrap().len(), 2);
        assert_eq!((store.paper_holding(&second).unwrap(), store.paper_files(&id).unwrap().len()), (Some(id.clone()), 1));
        // In the trash, the paper still stands for its file.
        fixture.store().trash_paper(&id).unwrap();
        let trashed = fixture.call("import_pdf", json!({"path": second})).unwrap();
        assert_eq!((trashed["added"].as_u64(), trashed["inTrash"].as_u64()), (Some(0), Some(1)));
    }

    #[test]
    fn attaches_a_pdf_to_an_existing_paper_without_confusing_same_named_files() {
        let fixture = Fixture::new();
        let original = fixture.root.join("first/paper.pdf");
        let other = fixture.root.join("second/paper.pdf");
        pdf(&original, "Original full text");
        pdf(&other, "Supplementary text");
        let mut store = fixture.store();
        let paper = store.add_paper(Paper { title: "My paper".into(), ..Default::default() }).unwrap();
        let first = store.import_file(&paper.id, &original, "fulltext").unwrap();
        drop(store);
        let result = fixture.call("import_pdf", json!({"path": other, "paperId": paper.id})).unwrap();
        assert_eq!((result["added"].as_u64(), result["filesAdded"].as_u64(), result["failed"].as_u64()), (Some(0), Some(1), Some(0)));
        assert!(fixture.store().needs_structure(&first.id).unwrap());
        let read = fixture.call("read_fulltext", json!({"paperId": paper.id})).unwrap();
        assert!(read["pages"][0]["text"].as_str().unwrap().contains("Original full text"));
        let again = fixture.call("import_pdf", json!({"path": other, "paperId": paper.id})).unwrap();
        assert_eq!(again["filesExisting"], 1);
        assert!(fixture.call("import_pdf", json!({"path": other, "paperId": "missing"})).is_err());
        assert_eq!(fixture.store().list_papers().unwrap().len(), 1);
    }
}
