//! Read a local Zotero library without modifying its database or attachments.
//! Field names are looked up in Zotero's schema, never assumed to have fixed IDs.

use crate::model::{Paper, clean_strings, normalize_doi, normalize_pmid, strip_markup};
use crate::store::Store;
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, ErrorCode, OpenFlags};
use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path, PathBuf};

pub struct Entry {
    pub paper: Paper,
    pub pdfs: Vec<Result<PathBuf, String>>,
}

#[derive(Default)]
pub struct Import {
    pub entries: Vec<Entry>,
    pub other_attachments: usize,
}

#[derive(Default)]
pub struct Row {
    pub title: String,
    pub paper_id: Option<String>,
    pub added: bool,
    pub in_trash: bool,
    pub files_added: usize,
    pub files_existing: usize,
    pub problems: Vec<String>,
}

impl Row {
    pub fn summary(&self) -> String {
        let status = if self.in_trash { "在回收站中，未导入" } else if self.added { "新增文献" } else if self.paper_id.is_some() { "已有文献，保留原记录" } else { "文献未导入" };
        format!("{status} · 新增 PDF {} · 已有 PDF {}{}", self.files_added, self.files_existing, if self.problems.is_empty() { String::new() } else { format!(" · {} 项待处理", self.problems.len()) })
    }
}

/// A read transaction includes committed WAL data and keeps all queries on the
/// same snapshot. Do not use `immutable=1`, which would ignore Zotero's WAL.
pub fn read(path: &Path) -> Result<Import> {
    read_snapshot(path).map_err(|error| {
        let locked = matches!(error.downcast_ref::<rusqlite::Error>(), Some(rusqlite::Error::SqliteFailure(code, _)) if matches!(code.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked));
        if locked {
            error.context("Zotero 数据库正在被占用。请在 Zotero 中选择「退出」，等待 Zotero 后台进程结束，再重新导入同一个 zotero.sqlite；仅关闭窗口可能仍在后台运行。")
        } else {
            error.context("无法读取 Zotero 数据库，请选择 Zotero 6 / 7 数据目录中的 zotero.sqlite（不是导出文件或 Wisp Paper 数据库）")
        }
    })
}

fn read_snapshot(path: &Path) -> Result<Import> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("无法只读打开 {}", path.display()))?;
    db.busy_timeout(std::time::Duration::from_secs(2))?;
    db.execute_batch("BEGIN")?;
    read_items(&db, path.parent().unwrap_or_else(|| Path::new(".")))
}

fn read_items(db: &Connection, root: &Path) -> Result<Import> {
    let mut items = db.prepare("SELECT i.itemID, t.typeName FROM items i JOIN itemTypes t USING(itemTypeID) JOIN libraries l USING(libraryID) WHERE l.type IN ('user', 'group') AND t.typeName NOT IN ('attachment', 'note', 'annotation') AND NOT EXISTS (SELECT 1 FROM deletedItems d WHERE d.itemID = i.itemID) ORDER BY i.itemID")?;
    let items = items.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut fields = db.prepare("SELECT f.fieldName, CAST(v.value AS TEXT) FROM itemData d JOIN fieldsCombined f USING(fieldID) JOIN itemDataValues v USING(valueID) WHERE d.itemID = ?1")?;
    let mut creators = db.prepare("SELECT c.firstName, c.lastName, c.fieldMode FROM itemCreators ic JOIN creators c USING(creatorID) JOIN creatorTypes ct USING(creatorTypeID) WHERE ic.itemID = ?1 AND ct.creatorType = 'author' ORDER BY ic.orderIndex")?;
    let mut tags = db.prepare("SELECT t.name FROM itemTags it JOIN tags t USING(tagID) WHERE it.itemID = ?1 ORDER BY t.name")?;
    let mut notes = db.prepare("SELECT n.note FROM itemNotes n WHERE n.parentItemID = ?1 AND NOT EXISTS (SELECT 1 FROM deletedItems d WHERE d.itemID = n.itemID) ORDER BY n.itemID")?;
    let mut attachments = db.prepare("SELECT i.key, a.path, a.contentType, a.linkMode FROM itemAttachments a JOIN items i USING(itemID) WHERE a.parentItemID = ?1 AND NOT EXISTS (SELECT 1 FROM deletedItems d WHERE d.itemID = a.itemID) ORDER BY a.itemID")?;
    let mut collections = db.prepare("SELECT collectionID, collectionName, parentCollectionID FROM collections")?;
    let collections = collections.query_map([], |r| Ok((r.get::<_, i64>(0)?, (r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?))))?.collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
    let mut memberships = db.prepare("SELECT collectionID FROM collectionItems WHERE itemID = ?1 ORDER BY collectionID")?;
    let mut imported = Import::default();
    for (id, kind) in items {
        let values = fields.query_map([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
        let get = |key: &str| values.get(key).cloned().unwrap_or_default();
        let authors = creators.query_map([id], |r| {
            let first = r.get::<_, Option<String>>(0)?.unwrap_or_default();
            let last = r.get::<_, Option<String>>(1)?.unwrap_or_default();
            Ok(if r.get::<_, i64>(2)? == 1 { last } else { format!("{first} {last}").trim().to_string() })
        })?.collect::<rusqlite::Result<Vec<_>>>()?.join("; ");
        let mut paper_tags = tags.query_map([id], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        for collection in memberships.query_map([id], |r| r.get::<_, i64>(0))? {
            if let Some(path) = collection_path(collection?, &collections) { paper_tags.push(format!("Zotero/{path}")) }
        }
        let mut paper_notes = notes.query_map([id], |r| r.get::<_, Option<String>>(0))?.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter().flatten().map(|note| strip_markup(&note)).filter(|note| !note.is_empty()).collect::<Vec<_>>();
        let extra = get("extra");
        if !extra.trim().is_empty() { paper_notes.push(format!("Zotero Extra：\n{extra}")) }
        let extra_field = |key: &str| extra.lines().find_map(|line| line.split_once(':').filter(|(name, _)| name.trim().eq_ignore_ascii_case(key)).map(|(_, value)| value.trim().to_string())).unwrap_or_default();
        let doi = get("DOI");
        let date = get("date");
        let year = date.split(|c: char| !c.is_ascii_digit()).find(|word| word.len() == 4).and_then(|word| word.parse().ok());
        let paper = Paper {
            title: strip_markup(&get("title")), authors, year,
            doi: normalize_doi(&if doi.is_empty() { extra_field("DOI") } else { doi }),
            pmid: normalize_pmid(&extra_field("PMID")),
            journal: [get("publicationTitle"), get("bookTitle"), get("proceedingsTitle"), get("university"), get("publisher")].into_iter().find(|v| !v.is_empty()).unwrap_or_default(),
            publication_status: match kind.as_str() { "preprint" => "preprint", "journalArticle" | "conferencePaper" | "book" | "bookSection" => "published", _ => "unknown" }.into(),
            url: get("url"), abstract_text: strip_markup(&get("abstractNote")),
            tags: clean_strings(paper_tags), notes: paper_notes.join("\n\n"), source: "Zotero 本地导入".into(),
            ..Default::default()
        };
        let mut pdfs = vec![];
        for file in attachments.query_map([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default(), r.get::<_, Option<String>>(2)?.unwrap_or_default(), r.get::<_, i64>(3)?)))? {
            let (key, path, mime, mode) = file?;
            if mime.eq_ignore_ascii_case("application/pdf") || path.to_lowercase().ends_with(".pdf") {
                pdfs.push(attachment_path(root, &key, &path, mode));
            } else { imported.other_attachments += 1 }
        }
        imported.entries.push(Entry { paper, pdfs });
    }
    Ok(imported)
}

fn collection_path(id: i64, collections: &BTreeMap<i64, (String, Option<i64>)>) -> Option<String> {
    let mut parts = vec![];
    let mut seen = HashSet::new();
    let mut next = Some(id);
    while let Some(id) = next {
        if !seen.insert(id) { return None }
        let (name, parent) = collections.get(&id)?;
        parts.push(name.as_str());
        next = *parent;
    }
    parts.reverse();
    Some(parts.join("/"))
}

fn attachment_path(root: &Path, key: &str, value: &str, mode: i64) -> Result<PathBuf, String> {
    let path = if let Some(name) = value.strip_prefix("storage:") {
        let relative = Path::new(name);
        if key.is_empty() || !key.bytes().all(|c| c.is_ascii_alphanumeric()) || name.is_empty() || name.contains(['\\', ':']) || relative.components().any(|part| !matches!(part, Component::Normal(_))) {
            return Err(format!("附件路径无效：{value}"));
        }
        let base = root.join("storage").join(key);
        let path = base.join(relative);
        // Also reject symlinks leading outside the selected Zotero storage.
        if let (Ok(storage), Ok(file)) = (root.join("storage").canonicalize(), path.canonicalize()) && !file.starts_with(storage) {
            return Err(format!("附件路径超出 Zotero storage 目录：{value}"));
        }
        path
    } else if value.starts_with("attachments:") {
        return Err(format!("相对链接附件需要 Zotero 的链接附件基础目录，请在 Zotero 中改用绝对路径或在 Wisp Paper 中手动添加：{value}"));
    } else if mode == 2 && Path::new(value).is_absolute() {
        PathBuf::from(value)
    } else {
        return Err(format!("附件没有可用的本地路径，请先在 Zotero 中下载：{value}"));
    };
    if !path.is_file() { return Err(format!("找不到 PDF，请先在 Zotero 中下载或修复路径：{}", path.display())) }
    Ok(path)
}

/// One record at a time: a bad item or a missing PDF does not discard the rest.
/// Existing papers keep their metadata; missing PDFs can still be filled in.
pub fn import_entry(store: &mut Store, entry: Entry) -> Row {
    let mut row = Row { title: entry.paper.title.clone(), ..Default::default() };
    let result = (|| -> Result<()> {
        let mut result = store.import_papers(vec![entry.paper])?;
        let id = if let Some(paper) = result.added.pop() {
            row.added = true;
            paper.id
        } else if let Some(skipped) = result.skipped.pop() {
            if skipped.in_trash { row.in_trash = true; return Ok(()) }
            skipped.existing_id
        } else { bail!("没有返回文献导入结果") };
        row.paper_id = Some(id.clone());
        let files = store.paper_files(&id)?;
        let mut known: HashSet<String> = files.iter().map(|f| f.id.clone()).collect();
        let mut fulltext = files.iter().any(|f| f.kind == "fulltext");
        for path in entry.pdfs {
            let saved = path.map_err(anyhow::Error::msg).and_then(|path| store.import_file(&id, &path, if fulltext { "supplement" } else { "fulltext" }));
            match saved {
                Ok(file) => {
                    fulltext |= file.kind == "fulltext";
                    if known.insert(file.id) { row.files_added += 1 } else { row.files_existing += 1 }
                }
                Err(err) => row.problems.push(format!("{err:#}")),
            }
        }
        Ok(())
    })();
    if let Err(err) = result { row.problems.push(format!("{err:#}")) }
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::new_id;

    struct Fixture { root: PathBuf, db: Connection }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(new_id("wisp-zotero-test"));
            std::fs::create_dir_all(root.join("storage/PDFKEY01")).unwrap();
            std::fs::write(root.join("storage/PDFKEY01/研究.pdf"), b"%PDF-1.4 fixture attachment").unwrap();
            let db = Connection::open(root.join("zotero.sqlite")).unwrap();
            // Minimal Zotero 6/7 schema, with deliberately nonstandard field IDs.
            db.execute_batch("
                CREATE TABLE libraries(libraryID INTEGER PRIMARY KEY, type TEXT);
                CREATE TABLE itemTypes(itemTypeID INTEGER PRIMARY KEY, typeName TEXT);
                CREATE TABLE items(itemID INTEGER PRIMARY KEY, itemTypeID INT, libraryID INT, key TEXT);
                CREATE TABLE fieldsCombined(fieldID INTEGER PRIMARY KEY, fieldName TEXT);
                CREATE TABLE itemDataValues(valueID INTEGER PRIMARY KEY, value TEXT);
                CREATE TABLE itemData(itemID INT, fieldID INT, valueID INT);
                CREATE TABLE creatorTypes(creatorTypeID INTEGER PRIMARY KEY, creatorType TEXT);
                CREATE TABLE creators(creatorID INTEGER PRIMARY KEY, firstName TEXT, lastName TEXT, fieldMode INT);
                CREATE TABLE itemCreators(itemID INT, creatorID INT, creatorTypeID INT, orderIndex INT);
                CREATE TABLE tags(tagID INTEGER PRIMARY KEY, name TEXT);
                CREATE TABLE itemTags(itemID INT, tagID INT);
                CREATE TABLE itemNotes(itemID INTEGER PRIMARY KEY, parentItemID INT, note TEXT);
                CREATE TABLE itemAttachments(itemID INTEGER PRIMARY KEY, parentItemID INT, linkMode INT, contentType TEXT, path TEXT);
                CREATE TABLE deletedItems(itemID INTEGER PRIMARY KEY);
                CREATE TABLE collections(collectionID INTEGER PRIMARY KEY, collectionName TEXT, parentCollectionID INT);
                CREATE TABLE collectionItems(collectionID INT, itemID INT);
                INSERT INTO libraries VALUES(1, 'user'), (2, 'group'), (3, 'feed');
                INSERT INTO itemTypes VALUES(1, 'journalArticle'), (2, 'note'), (3, 'attachment'), (4, 'annotation'), (5, 'preprint');
                INSERT INTO items VALUES(1, 1, 1, 'PAPER001'), (2, 2, 1, 'NOTE0001'), (3, 3, 1, 'PDFKEY01'), (4, 1, 1, 'TRASH001'), (5, 2, 1, 'LOOSE001'), (6, 3, 1, 'MISSING1'), (7, 3, 1, 'HTML0001'), (8, 4, 1, 'ANNOT001'), (9, 5, 2, 'GROUP001'), (10, 1, 3, 'FEED0001'), (11, 2, 1, 'DELNOTE1'), (12, 3, 1, 'DELPDF01');
                INSERT INTO fieldsCombined VALUES(101, 'title'), (102, 'DOI'), (103, 'date'), (104, 'publicationTitle'), (105, 'abstractNote'), (106, 'extra'), (107, 'url');
                INSERT INTO itemDataValues VALUES(1, 'Root <i>atlas</i>'), (2, 'https://doi.org/10.1234/ATLAS'), (3, '2024-03-20'), (4, 'Plant Journal'), (5, 'Cells &amp; roots'), (6, 'PMID: 12345678'), (7, 'https://example.org/paper'), (8, 'Deleted'), (9, 'Group preprint');
                INSERT INTO itemData VALUES(1,101,1), (1,102,2), (1,103,3), (1,104,4), (1,105,5), (1,106,6), (1,107,7), (4,101,8), (9,101,9), (10,101,1);
                INSERT INTO creatorTypes VALUES(8, 'author'), (9, 'editor');
                INSERT INTO creators VALUES(1, 'Jane', 'Doe', 0), (2, '', 'Atlas Consortium', 1), (3, 'An', 'Editor', 0);
                INSERT INTO itemCreators VALUES(1,2,8,1), (1,1,8,0), (1,3,9,2);
                INSERT INTO tags VALUES(1, '单细胞');
                INSERT INTO itemTags VALUES(1,1);
                INSERT INTO itemNotes VALUES(2,1,'<p>Keep &amp; compare</p>'), (5,NULL,'Standalone'), (11,1,'Deleted note');
                INSERT INTO itemAttachments VALUES(3,1,0,'application/pdf','storage:研究.pdf'), (6,1,0,'application/pdf','storage:missing.pdf'), (7,1,1,'text/html','storage:snapshot.html'), (12,1,0,'application/pdf','storage:deleted.pdf');
                INSERT INTO deletedItems VALUES(4), (11), (12);
                INSERT INTO collections VALUES(1, '植物', NULL), (2, '根', 1);
                INSERT INTO collectionItems VALUES(2,1);
            ").unwrap();
            Self { root, db }
        }

        fn read(&self) -> Import { read(&self.root.join("zotero.sqlite")).unwrap() }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            // Close before removing on Windows.
            let db = std::mem::replace(&mut self.db, Connection::open_in_memory().unwrap());
            drop(db);
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn reads_metadata_notes_collections_and_local_pdfs_without_changing_source() {
        let fixture = Fixture::new();
        let before = std::fs::read(fixture.root.join("zotero.sqlite")).unwrap();
        let imported = fixture.read();
        assert_eq!(imported.entries.len(), 2); // User and group; no feed/trash/child items.
        assert_eq!(imported.other_attachments, 1);
        let entry = &imported.entries[0];
        assert_eq!(entry.paper.title, "Root atlas");
        assert_eq!(entry.paper.authors, "Jane Doe; Atlas Consortium");
        assert_eq!(entry.paper.doi, "10.1234/atlas");
        assert_eq!(entry.paper.pmid, "12345678");
        assert_eq!(entry.paper.year, Some(2024));
        assert_eq!(entry.paper.journal, "Plant Journal");
        assert_eq!(entry.paper.abstract_text, "Cells & roots");
        assert_eq!(entry.paper.tags, ["单细胞", "Zotero/植物/根"]);
        assert_eq!(entry.paper.notes, "Keep & compare\n\nZotero Extra：\nPMID: 12345678");
        assert_eq!(entry.pdfs[0].as_ref().unwrap(), &fixture.root.join("storage/PDFKEY01/研究.pdf"));
        assert!(entry.pdfs[1].as_ref().unwrap_err().contains("找不到 PDF"));
        assert_eq!(imported.entries[1].paper.publication_status, "preprint");
        assert_eq!(before, std::fs::read(fixture.root.join("zotero.sqlite")).unwrap());
    }

    #[test]
    fn copies_pdfs_deduplicates_and_preserves_existing_edits_and_trash() {
        let fixture = Fixture::new();
        let mut store = Store::in_memory(&fixture.root.join("destination")).unwrap();
        let first = import_entry(&mut store, fixture.read().entries.remove(0));
        assert!(first.added);
        assert_eq!((first.files_added, first.problems.len()), (1, 1));
        let id = first.paper_id.unwrap();
        let mut paper = store.get_paper(&id).unwrap();
        assert_eq!(paper.screening, "pending");
        assert_eq!(paper.reading, "unread");
        paper.notes = "Edited in Wisp".into();
        store.save_paper(&mut paper).unwrap();
        let repeated = import_entry(&mut store, fixture.read().entries.remove(0));
        assert!(!repeated.added);
        assert_eq!(repeated.paper_id.as_deref(), Some(id.as_str()));
        assert_eq!((repeated.files_added, repeated.files_existing), (0, 1));
        assert_eq!(store.get_paper(&id).unwrap().notes, "Edited in Wisp");
        let files = store.paper_files(&id).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].kind, "fulltext");
        let copy = store.file_path(&files[0]);
        std::fs::remove_file(fixture.root.join("storage/PDFKEY01/研究.pdf")).unwrap();
        assert_eq!(std::fs::read(copy).unwrap(), b"%PDF-1.4 fixture attachment");
        store.trash_paper(&id).unwrap();
        let trashed = import_entry(&mut store, fixture.read().entries.remove(0));
        assert!(trashed.in_trash);
        assert!(trashed.paper_id.is_none());
        assert_eq!(store.list_trash().unwrap().len(), 1);
    }

    #[test]
    fn fills_existing_paper_pdfs_and_reports_bad_records_independently() {
        let fixture = Fixture::new();
        let mut store = Store::in_memory(&fixture.root.join("destination")).unwrap();
        let entry = fixture.read().entries.remove(0);
        let existing = store.add_paper(entry.paper.clone()).unwrap();
        let row = import_entry(&mut store, entry);
        assert!(!row.added);
        assert_eq!(row.paper_id, Some(existing.id));
        assert_eq!(row.files_added, 1);
        let bad = import_entry(&mut store, Entry { paper: Paper { title: "Bad DOI".into(), doi: "invalid".into(), ..Default::default() }, pdfs: vec![] });
        assert!(bad.paper_id.is_none());
        assert!(bad.problems[0].contains("DOI"));
        assert!(import_entry(&mut store, fixture.read().entries.remove(1)).added);
    }

    #[test]
    fn reads_committed_wal_and_rejects_other_databases() {
        let fixture = Fixture::new();
        fixture.db.pragma_update(None, "journal_mode", "WAL").unwrap();
        fixture.db.execute("UPDATE itemDataValues SET value = 'Updated through WAL' WHERE valueID = 1", []).unwrap();
        assert_eq!(fixture.read().entries[0].paper.title, "Updated through WAL");
        let wrong = fixture.root.join("wrong.sqlite");
        drop(Connection::open(&wrong).unwrap());
        assert!(read(&wrong).err().unwrap().to_string().contains("无法读取 Zotero"));
        let missing = fixture.root.join("absent.sqlite");
        assert!(read(&missing).is_err());
        assert!(!missing.exists());
    }

    #[test]
    fn locked_database_explains_how_to_release_it_and_can_be_retried() {
        let fixture = Fixture::new();
        fixture.db.execute_batch("BEGIN EXCLUSIVE").unwrap();
        let error = read(&fixture.root.join("zotero.sqlite")).err().unwrap();
        let message = format!("{error:#}");
        assert!(message.contains("正在被占用"));
        assert!(message.contains("退出"));
        assert!(message.contains("同一个 zotero.sqlite"));
        assert!(!message.contains("请选择 Zotero 6 / 7"));
        assert!(matches!(error.downcast_ref::<rusqlite::Error>(), Some(rusqlite::Error::SqliteFailure(code, _)) if code.code == ErrorCode::DatabaseBusy));
        fixture.db.execute_batch("ROLLBACK").unwrap();
        assert_eq!(fixture.read().entries.len(), 2);
    }

    #[test]
    fn resolves_linked_files_and_rejects_escaping_storage_paths() {
        let fixture = Fixture::new();
        let absolute = fixture.root.join("storage/PDFKEY01/研究.pdf");
        assert_eq!(attachment_path(&fixture.root, "KEY", &absolute.to_string_lossy(), 2).unwrap(), absolute);
        assert!(attachment_path(&fixture.root, "KEY", "attachments:papers/a.pdf", 2).unwrap_err().contains("基础目录"));
        for path in ["storage:../outside.pdf", "storage:/outside.pdf", "storage:C:\\outside.pdf", "storage:..\\outside.pdf", "storage:"] {
            assert!(attachment_path(&fixture.root, "KEY", path, 0).is_err(), "{path}");
        }
        assert!(attachment_path(&fixture.root, "../bad", "storage:paper.pdf", 0).is_err());
        let cycle = BTreeMap::from([(1, ("one".into(), Some(2))), (2, ("two".into(), Some(1)))]);
        assert!(collection_path(1, &cycle).is_none());
    }
}
