//! Local collection tools. The parsers, deduplication and attachment copier
//! are the same ones used by the window; no network calls are made here.

use super::text;
use crate::{fulltext, import, libraries, lookup, zotero};
use crate::model::{Paper, clean_strings};
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
    let paper = target.cloned().unwrap_or_else(|| Paper {
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
