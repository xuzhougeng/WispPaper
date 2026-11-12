//! The library on disk: `wisp-paper.sqlite` plus the attachments under
//! `papers/`. Paths in the database are relative to the library folder, so
//! the whole folder can be copied elsewhere and keeps working.

use crate::model::*;
use crate::time::now_iso;
use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const DB_FILE: &str = "wisp-paper.sqlite";
/// Bump when an older build would misread the database.
pub const SCHEMA_VERSION: i32 = 1;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS papers (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL CHECK (length(trim(title)) > 0),
    -- Lowercase letters and digits of the title: finds duplicates without identifiers.
    -- Not unique: the same title with another DOI is another paper (preprint and published version).
    title_key TEXT NOT NULL DEFAULT '',
    abstract TEXT NOT NULL DEFAULT '',
    authors_json TEXT NOT NULL DEFAULT '[]',
    journal TEXT NOT NULL DEFAULT '',
    year INTEGER CHECK (year IS NULL OR year BETWEEN 1800 AND 2200),
    publication_state TEXT NOT NULL DEFAULT 'unknown'
        CHECK (publication_state IN ('unknown','preprint','published','unpublished')),
    url TEXT NOT NULL DEFAULT '',
    source TEXT NOT NULL DEFAULT '',
    screening_status TEXT NOT NULL DEFAULT 'pending'
        CHECK (screening_status IN ('pending','included','excluded','uncertain','irrelevant')),
    screening_reason TEXT NOT NULL DEFAULT '',
    reviewed_at TEXT NOT NULL DEFAULT '',
    reading_status TEXT NOT NULL DEFAULT 'unread' CHECK (reading_status IN ('unread','skimmed','read')),
    starred INTEGER NOT NULL DEFAULT 0 CHECK (starred IN (0,1)),
    tags_json TEXT NOT NULL DEFAULT '[]',
    notes TEXT NOT NULL DEFAULT '',
    first_seen_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_papers_title_key ON papers(title_key);

-- Bare identifiers: lowercase DOI without the doi.org prefix, PMID digits only.
CREATE TABLE IF NOT EXISTS paper_identifiers (
    paper_id TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
    namespace TEXT NOT NULL CHECK (namespace IN ('doi','pmid')),
    value TEXT NOT NULL CHECK (length(value) > 0),
    PRIMARY KEY (namespace, value)
);
CREATE INDEX IF NOT EXISTS idx_identifiers_paper ON paper_identifiers(paper_id);

-- 自定义词条: the fields this library adds to every paper.
CREATE TABLE IF NOT EXISTS field_defs (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL UNIQUE CHECK (length(trim(name)) > 0),
    kind TEXT NOT NULL DEFAULT 'text' CHECK (kind IN ('text','tags','select')),
    options_json TEXT NOT NULL DEFAULT '[]',
    in_table INTEGER NOT NULL DEFAULT 0 CHECK (in_table IN (0,1)),
    position INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS paper_fields (
    paper_id TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
    field_id TEXT NOT NULL REFERENCES field_defs(id) ON DELETE CASCADE,
    values_json TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY (paper_id, field_id)
);

-- Attachments. local_path is relative to the library folder, `/` separated.
CREATE TABLE IF NOT EXISTS resources (
    id TEXT PRIMARY KEY NOT NULL,
    paper_id TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
    resource_type TEXT NOT NULL CHECK (resource_type IN ('fulltext','supplement','other')),
    title TEXT NOT NULL,
    format TEXT NOT NULL DEFAULT '',
    local_path TEXT NOT NULL,
    size_bytes INTEGER NOT NULL DEFAULT 0,
    sha256 TEXT NOT NULL DEFAULT '',
    added_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_resources_paper ON resources(paper_id);

-- Page text of a stored PDF, extracted once: 文献库提问 and search read it.
CREATE TABLE IF NOT EXISTS fulltext_text (
    resource_id TEXT PRIMARY KEY NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    pages TEXT NOT NULL DEFAULT '[]',
    error TEXT NOT NULL DEFAULT '',
    extracted_at TEXT NOT NULL
);

-- 回收站: the paper keeps its notes, fields and files until it is deleted for good.
CREATE TABLE IF NOT EXISTS trashed_papers (
    paper_id TEXT PRIMARY KEY NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
    deleted_at TEXT NOT NULL
);

-- 检索式: a query saved for one literature database, run by hand or on a schedule.
CREATE TABLE IF NOT EXISTS searches (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    query TEXT NOT NULL CHECK (length(trim(query)) > 0),
    max_results INTEGER NOT NULL DEFAULT 50,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    created_at TEXT NOT NULL
);

-- One run of a search, or any other import that can be undone as a whole.
-- search_id is not a reference: the record outlives the search.
CREATE TABLE IF NOT EXISTS import_batches (
    id TEXT PRIMARY KEY NOT NULL,
    search_id TEXT NOT NULL DEFAULT '',
    label TEXT NOT NULL DEFAULT '',
    started_at TEXT NOT NULL,
    remote_total INTEGER,
    fetched INTEGER NOT NULL DEFAULT 0,
    added INTEGER NOT NULL DEFAULT 0,
    skipped INTEGER NOT NULL DEFAULT 0,
    error TEXT NOT NULL DEFAULT '',
    undone_at TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_batches_search ON import_batches(search_id, started_at);
CREATE TABLE IF NOT EXISTS batch_papers (
    batch_id TEXT NOT NULL REFERENCES import_batches(id) ON DELETE CASCADE,
    paper_id TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
    PRIMARY KEY (batch_id, paper_id)
);

-- What an assistant thinks the screening decision should be. A suggestion
-- only: the decision in `papers` is made by the person. One per paper; a new
-- one replaces the old. It stays after the decision, for comparing the two.
CREATE TABLE IF NOT EXISTS screening_suggestions (
    paper_id TEXT PRIMARY KEY NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
    verdict TEXT NOT NULL CHECK (verdict IN ('included','excluded','uncertain','irrelevant')),
    reason TEXT NOT NULL DEFAULT '',
    -- A passage quoted word for word from the abstract or the full text, and its page there.
    excerpt TEXT NOT NULL DEFAULT '',
    page INTEGER,
    -- What was read: fulltext, abstract or none.
    basis TEXT NOT NULL DEFAULT '',
    confidence REAL,
    -- Who suggested it: a model profile, or an outside agent.
    author TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    -- The library's screening goal it was made against; NULL for one made
    -- before suggestions kept it (added by `add_columns`).
    criteria TEXT
);

-- Translations already made, so that a title or an abstract is translated
-- once. The key is `translation_key`: the source text and the instructions.
CREATE TABLE IF NOT EXISTS translations (
    cache_key TEXT PRIMARY KEY NOT NULL,
    translated_text TEXT NOT NULL,
    -- The model profile that made it.
    author TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL
);

-- 历史对话: what was asked of the library (paper_id empty) or of one paper,
-- and what was answered. body is an `ai::Transcript` as JSON. paper_id is
-- not a reference: `delete_paper` removes a paper's conversations itself.
CREATE TABLE IF NOT EXISTS conversations (
    id TEXT PRIMARY KEY NOT NULL,
    paper_id TEXT NOT NULL DEFAULT '',
    title TEXT NOT NULL DEFAULT '',
    questions INTEGER NOT NULL DEFAULT 0,
    body TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_conversations_paper ON conversations(paper_id, updated_at);
-- The pictures that went with a conversation's questions, as they were sent.
CREATE TABLE IF NOT EXISTS conversation_images (
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    id TEXT NOT NULL,
    media_type TEXT NOT NULL,
    data BLOB NOT NULL,
    PRIMARY KEY (conversation_id, id)
);

-- 写作: the sentences of a stored full text, each with the part of the paper
-- it stands in (a key of `structure::SECTIONS`), in reading order.
CREATE TABLE IF NOT EXISTS sentences (
    resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    section TEXT NOT NULL,
    heading TEXT NOT NULL DEFAULT '',
    page INTEGER NOT NULL,
    -- Its place among the sentences of that part: 0 opens the part.
    position INTEGER NOT NULL,
    text TEXT NOT NULL,
    PRIMARY KEY (resource_id, seq)
);
CREATE INDEX IF NOT EXISTS idx_sentences_section ON sentences(section);
-- How a full text was sorted into parts: by rule (with the version of the
-- rules), by a model (author says which), or not at all (none: no text).
-- A file without a row has not been through this yet.
CREATE TABLE IF NOT EXISTS structures (
    resource_id TEXT PRIMARY KEY NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    method TEXT NOT NULL CHECK (method IN ('rules','model','none')),
    author TEXT NOT NULL DEFAULT '',
    version INTEGER NOT NULL DEFAULT 0,
    -- Who read the text off the pages when the file gave none: a model profile.
    text_by TEXT NOT NULL DEFAULT '',
    -- How many paragraphs the rules only guessed the part of (`structure::open_paragraphs`).
    open INTEGER NOT NULL DEFAULT 0,
    made_at TEXT NOT NULL
);
-- 草稿: what the person is writing, one text per part of the manuscript.
CREATE TABLE IF NOT EXISTS drafts (
    section TEXT PRIMARY KEY NOT NULL,
    body TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL
);

-- Where a file that was fetched came from: the address, who listed it, which version it is.
CREATE TABLE IF NOT EXISTS file_origins (
    resource_id TEXT PRIMARY KEY NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    url TEXT NOT NULL,
    source TEXT NOT NULL DEFAULT '',
    version TEXT NOT NULL DEFAULT '',
    fetched_at TEXT NOT NULL
);
"#;

const PAPER_COLUMNS: &str = "id, title, abstract, authors_json, journal, year, publication_state, url, source, \
    screening_status, screening_reason, reviewed_at, reading_status, starred, tags_json, notes, first_seen_at, updated_at";

/// A record an import left out because the library already holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct Skipped {
    pub title: String,
    pub existing_id: String,
    pub in_trash: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportResult {
    pub added: Vec<Paper>,
    pub skipped: Vec<Skipped>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrashEntry {
    pub paper: Paper,
    pub deleted_at: String,
}

/// 检索式: a query saved for one literature database (`sources::KINDS`).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedSearch {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub query: String,
    /// How many of the newest records one run brings back.
    pub max_results: usize,
    /// Whether "run all" and the schedule include it.
    pub enabled: bool,
    pub created_at: String,
    /// Its latest run, read with the search.
    pub last_run: Option<ImportBatch>,
}

/// One run of a search: what was asked, what came back, what was added.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportBatch {
    pub id: String,
    pub search_id: String,
    /// The database and query at the time, for the record.
    pub label: String,
    pub started_at: String,
    /// How many records the database holds for the query, when it said.
    pub remote_total: Option<u64>,
    pub fetched: usize,
    pub added: usize,
    pub skipped: usize,
    /// Why the run failed; empty when it did not.
    pub error: String,
    /// When the batch was undone; empty when it was not.
    pub undone_at: String,
}

/// A conversation with the library or with one paper, as it is kept.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Conversation {
    /// Empty until it is first saved.
    pub id: String,
    /// The paper it is about; empty for a conversation with the library.
    pub paper_id: String,
    pub transcript: crate::ai::Transcript,
    pub created_at: String,
    pub updated_at: String,
}

/// A conversation as the history lists it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConversationInfo {
    pub id: String,
    pub title: String,
    pub questions: usize,
    pub updated_at: String,
}

/// What an assistant thinks the screening decision for a paper should be.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    pub paper_id: String,
    /// included, excluded, uncertain or irrelevant: the keys of `SCREENINGS` without pending.
    pub verdict: String,
    pub reason: String,
    /// Quoted word for word from the abstract or the full text; empty when there is none.
    pub excerpt: String,
    /// The full-text page the excerpt is on.
    pub page: Option<usize>,
    /// What was read: fulltext, abstract or none.
    pub basis: String,
    pub confidence: Option<f64>,
    pub author: String,
    pub created_at: String,
    /// The screening goal it was made against; `None` when it was made
    /// before suggestions kept their goal, and is taken as the current one.
    pub criteria: Option<String>,
}

impl Suggestion {
    /// Whether it was made against `goal`, the library's screening goal now.
    pub fn made_for(&self, goal: &str) -> bool {
        self.criteria.as_deref().is_none_or(|criteria| criteria.trim() == goal.trim())
    }
}

/// How one full text was sorted into the parts of a paper.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StructureInfo {
    pub paper_id: String,
    /// rules, model, or none when the file gave no text.
    pub method: String,
    /// The model profile that sorted it; empty for the rules.
    pub author: String,
    /// The model profile that read the text off the pages; empty when the file gave its own.
    pub text_by: String,
    /// How many paragraphs the rules only guessed the part of.
    pub open: usize,
    /// How many sentences each part holds, by a key of `structure::SECTIONS`.
    pub counts: BTreeMap<String, usize>,
}

/// A sentence of a stored full text, with the paper it is from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaperSentence {
    pub paper_id: String,
    pub sentence: crate::structure::Sentence,
}

/// What undoing a batch did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Undone {
    pub removed: usize,
    /// (id, title) of the papers left in place because someone has worked on them.
    pub kept: Vec<(String, String)>,
}

const BATCH_COLUMNS: &str = "id, search_id, label, started_at, remote_total, fetched, added, skipped, error, undone_at";

fn row_to_batch(r: &rusqlite::Row) -> rusqlite::Result<ImportBatch> {
    Ok(ImportBatch {
        id: r.get(0)?,
        search_id: r.get(1)?,
        label: r.get(2)?,
        started_at: r.get(3)?,
        remote_total: r.get::<_, Option<i64>>(4)?.map(|n| n as u64),
        fetched: r.get::<_, i64>(5)? as usize,
        added: r.get::<_, i64>(6)? as usize,
        skipped: r.get::<_, i64>(7)? as usize,
        error: r.get(8)?,
        undone_at: r.get(9)?,
    })
}

pub struct Store {
    root: PathBuf,
    db: Connection,
}

/// Table, column and type of each column added after its table was first made.
const ADDED_COLUMNS: &[(&str, &str, &str)] = &[("screening_suggestions", "criteria", "TEXT")];

/// Add the columns added to a table after it was first made: `CREATE TABLE IF NOT
/// EXISTS` leaves a table of an earlier build as it is. An older build reads
/// a database with these columns as before, so the version stays.
fn add_columns(db: &Connection) -> Result<()> {
    for (table, column, kind) in ADDED_COLUMNS {
        let has: bool = db.query_row(&format!("SELECT count(*) FROM pragma_table_info('{table}') WHERE name = ?1"), [column], |r| r.get::<_, i64>(0))? > 0;
        if !has { db.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {kind}"))? }
    }
    Ok(())
}

fn json_strings(value: &str) -> Vec<String> {
    serde_json::from_str(value).unwrap_or_default()
}

fn to_json(values: &[String]) -> String {
    serde_json::to_string(values).unwrap_or_else(|_| "[]".into())
}

impl Store {
    /// Open (creating it when new) the library in `root`.
    pub fn open(root: &Path) -> Result<Store> {
        std::fs::create_dir_all(root.join("papers")).with_context(|| format!("无法创建文献库文件夹 {}", root.display()))?;
        let db = Connection::open(root.join(DB_FILE)).with_context(|| format!("无法打开数据库 {}", root.join(DB_FILE).display()))?;
        Self::init(root.to_path_buf(), db)
    }

    /// A library that lives only in memory; attachments go under `root`.
    pub fn in_memory(root: &Path) -> Result<Store> {
        Self::init(root.to_path_buf(), Connection::open_in_memory()?)
    }

    fn init(root: PathBuf, db: Connection) -> Result<Store> {
        db.pragma_update(None, "foreign_keys", "ON")?;
        // WAL keeps a crash from losing the library; ignored for in-memory databases.
        let _ = db.pragma_update(None, "journal_mode", "WAL");
        let version: i32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            bail!("这个文献库由更新版本的 Wisp Paper 创建（数据库版本 {version}），请升级后再打开。");
        }
        db.execute_batch(SCHEMA)?;
        add_columns(&db)?;
        db.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(Store { root, db })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// How many papers the database at `path` holds outside the trash,
    /// without creating or changing anything; 0 when it cannot be read.
    pub fn count_papers(path: &Path) -> usize {
        let Ok(db) = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return 0 };
        db.query_row("SELECT count(*) FROM papers WHERE id NOT IN (SELECT paper_id FROM trashed_papers)", [], |r| r.get::<_, i64>(0)).map(|n| n as usize).unwrap_or(0)
    }

    // ---- papers

    /// Every paper outside the trash, newest first.
    pub fn list_papers(&self) -> Result<Vec<Paper>> {
        self.read_papers("WHERE id NOT IN (SELECT paper_id FROM trashed_papers) ORDER BY first_seen_at DESC, id")
    }

    pub fn get_paper(&self, id: &str) -> Result<Paper> {
        let mut stmt = self.db.prepare(&format!("SELECT {PAPER_COLUMNS} FROM papers WHERE id = ?1"))?;
        let mut paper = stmt.query_row([id], row_to_paper).optional()?.ok_or_else(|| anyhow!("文献不存在：{id}"))?;
        self.attach_details(std::slice::from_mut(&mut paper))?;
        Ok(paper)
    }

    fn read_papers(&self, tail: &str) -> Result<Vec<Paper>> {
        let mut stmt = self.db.prepare(&format!("SELECT {PAPER_COLUMNS} FROM papers {tail}"))?;
        let mut papers = stmt.query_map([], row_to_paper)?.collect::<rusqlite::Result<Vec<_>>>()?;
        self.attach_details(&mut papers)?;
        Ok(papers)
    }

    /// Identifiers, custom field values and the full-text status live in other tables.
    fn attach_details(&self, papers: &mut [Paper]) -> Result<()> {
        let index: HashMap<String, usize> = papers.iter().enumerate().map(|(i, p)| (p.id.clone(), i)).collect();
        let mut stmt = self.db.prepare("SELECT paper_id, namespace, value FROM paper_identifiers")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let (id, namespace, value): (String, String, String) = (row.get(0)?, row.get(1)?, row.get(2)?);
            if let Some(&i) = index.get(&id) {
                if namespace == "doi" { papers[i].doi = value } else { papers[i].pmid = value }
            }
        }
        let mut stmt = self.db.prepare("SELECT paper_id, field_id, values_json FROM paper_fields")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let (id, field, values): (String, String, String) = (row.get(0)?, row.get(1)?, row.get(2)?);
            let values = json_strings(&values);
            if let Some(&i) = index.get(&id) && !values.is_empty() {
                papers[i].fields.insert(field, values);
            }
        }
        let mut stmt = self.db.prepare("SELECT DISTINCT paper_id FROM resources WHERE resource_type = 'fulltext'")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            if let Some(&i) = index.get(&id) {
                papers[i].fulltext = "stored".into();
            }
        }
        // Stored, read, and no text came of any of its full-text files.
        let mut stmt = self.db.prepare("SELECT r.paper_id FROM resources r JOIN fulltext_text t ON t.resource_id = r.id WHERE r.resource_type = 'fulltext' GROUP BY r.paper_id HAVING SUM(t.error = '') = 0")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            if let Some(&i) = index.get(&id) {
                papers[i].fulltext = "textless".into();
            }
        }
        for paper in papers.iter_mut() {
            if !matches!(paper.fulltext.as_str(), "stored" | "textless") {
                paper.fulltext = if paper.link().is_empty() { "missing" } else { "linked" }.into();
            }
        }
        Ok(())
    }

    /// The paper the library already holds for `p`: same DOI, same PMID, or,
    /// when one side has no DOI, the same title. Papers in the trash count.
    pub fn existing_paper(&self, p: &Paper) -> Result<Option<String>> {
        existing_paper(&self.db, p)
    }

    /// Add one paper; an error names the duplicate when the library holds it.
    pub fn add_paper(&mut self, paper: Paper) -> Result<Paper> {
        let title = paper.title.clone();
        let mut result = self.import_papers(vec![paper])?;
        match result.added.pop() {
            Some(added) => Ok(added),
            None => {
                let skipped = result.skipped.pop().map(|s| if s.in_trash { "在回收站中" } else { "已在文献库中" }).unwrap_or("已存在");
                bail!("「{title}」{skipped}。")
            }
        }
    }

    /// Insert a batch in one transaction: an invalid record rolls everything
    /// back, a known paper is skipped and reported, so rerunning an import is safe.
    pub fn import_papers(&mut self, papers: Vec<Paper>) -> Result<ImportResult> {
        let tx = self.db.transaction()?;
        let mut result = ImportResult::default();
        for (i, mut paper) in papers.into_iter().enumerate() {
            paper.normalize();
            paper.validate().map_err(|e| anyhow!("第 {} 条：{e}", i + 1))?;
            if let Some(existing) = existing_paper(&tx, &paper)? {
                let in_trash = tx.query_row("SELECT 1 FROM trashed_papers WHERE paper_id = ?1", [&existing], |_| Ok(())).optional()?.is_some();
                result.skipped.push(Skipped { title: paper.title, existing_id: existing, in_trash });
                continue;
            }
            if paper.id.is_empty() || tx.query_row("SELECT 1 FROM papers WHERE id = ?1", [&paper.id], |_| Ok(())).optional()?.is_some() {
                paper.id = new_id("P");
            }
            write_paper(&tx, &paper)?;
            result.added.push(paper);
        }
        tx.commit()?;
        Ok(result)
    }

    /// Save an edited paper. The identifiers must not belong to another paper.
    pub fn save_paper(&mut self, paper: &mut Paper) -> Result<()> {
        paper.normalize();
        paper.validate().map_err(|e| anyhow!(e))?;
        let tx = self.db.transaction()?;
        for (namespace, value) in [("doi", &paper.doi), ("pmid", &paper.pmid)] {
            if value.is_empty() { continue }
            let other: Option<String> = tx.query_row("SELECT paper_id FROM paper_identifiers WHERE namespace = ?1 AND value = ?2 AND paper_id <> ?3", params![namespace, value, paper.id], |r| r.get(0)).optional()?;
            if let Some(other) = other {
                let title: String = tx.query_row("SELECT title FROM papers WHERE id = ?1", [&other], |r| r.get(0))?;
                bail!("{} {value} 已属于另一篇文献：「{title}」。", namespace.to_uppercase());
            }
        }
        write_paper(&tx, paper)?;
        tx.commit()?;
        Ok(())
    }

    // ---- trash

    pub fn trash_paper(&mut self, id: &str) -> Result<()> {
        self.db.execute("INSERT OR IGNORE INTO trashed_papers(paper_id, deleted_at) VALUES (?1, ?2)", params![id, now_iso()])?;
        Ok(())
    }

    pub fn restore_paper(&mut self, id: &str) -> Result<()> {
        self.db.execute("DELETE FROM trashed_papers WHERE paper_id = ?1", [id])?;
        Ok(())
    }

    pub fn list_trash(&self) -> Result<Vec<TrashEntry>> {
        let papers = self.read_papers("WHERE id IN (SELECT paper_id FROM trashed_papers) ORDER BY id")?;
        let mut entries = vec![];
        for paper in papers {
            let deleted_at: String = self.db.query_row("SELECT deleted_at FROM trashed_papers WHERE paper_id = ?1", [&paper.id], |r| r.get(0))?;
            entries.push(TrashEntry { paper, deleted_at });
        }
        entries.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at));
        Ok(entries)
    }

    /// Remove a paper and its records for good. Its files move to `removed/`
    /// inside the library folder rather than being unlinked.
    pub fn delete_paper(&mut self, id: &str) -> Result<()> {
        let files = self.paper_files(id)?;
        self.db.execute("DELETE FROM papers WHERE id = ?1", [id])?;
        self.db.execute("DELETE FROM conversations WHERE paper_id = ?1", [id])?;
        for file in files {
            self.set_aside(&file.local_path);
        }
        Ok(())
    }

    // ---- custom fields

    pub fn list_fields(&self) -> Result<Vec<FieldDef>> {
        let mut stmt = self.db.prepare("SELECT id, name, kind, options_json, in_table FROM field_defs ORDER BY position, name")?;
        let fields = stmt
            .query_map([], |r| {
                Ok(FieldDef { id: r.get(0)?, name: r.get(1)?, kind: FieldKind::parse(&r.get::<_, String>(2)?), options: json_strings(&r.get::<_, String>(3)?), in_table: r.get::<_, i32>(4)? == 1 })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(fields)
    }

    /// Create (empty id) or update a field definition.
    pub fn save_field(&mut self, field: &mut FieldDef) -> Result<()> {
        field.name = field.name.trim().to_string();
        field.options = clean_strings(field.options.iter());
        if field.name.is_empty() { bail!("请填写词条名称。") }
        if field.name.chars().count() > 40 { bail!("词条名称不能超过 40 个字符。") }
        if field.kind == FieldKind::Select && field.options.is_empty() { bail!("单选词条至少需要一个选项。") }
        let clash: Option<String> = self.db.query_row("SELECT id FROM field_defs WHERE name = ?1 AND id <> ?2", params![field.name, field.id], |r| r.get(0)).optional()?;
        if clash.is_some() { bail!("已有同名词条「{}」。", field.name) }
        if field.id.is_empty() {
            field.id = new_id("F");
            let position: i64 = self.db.query_row("SELECT coalesce(max(position), 0) + 1 FROM field_defs", [], |r| r.get(0))?;
            self.db.execute(
                "INSERT INTO field_defs(id, name, kind, options_json, in_table, position) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![field.id, field.name, field.kind.as_str(), to_json(&field.options), field.in_table as i32, position],
            )?;
        } else {
            self.db.execute(
                "UPDATE field_defs SET name = ?2, kind = ?3, options_json = ?4, in_table = ?5 WHERE id = ?1",
                params![field.id, field.name, field.kind.as_str(), to_json(&field.options), field.in_table as i32],
            )?;
        }
        Ok(())
    }

    /// Delete a field and every value papers hold for it.
    pub fn delete_field(&mut self, id: &str) -> Result<()> {
        self.db.execute("DELETE FROM field_defs WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Move a field one place up (`-1`) or down (`1`) in the order.
    pub fn move_field(&mut self, id: &str, step: i32) -> Result<()> {
        let mut ids: Vec<String> = self.list_fields()?.into_iter().map(|f| f.id).collect();
        let Some(at) = ids.iter().position(|f| f == id) else { return Ok(()) };
        let to = at as i32 + step;
        if to < 0 || to as usize >= ids.len() { return Ok(()) }
        ids.swap(at, to as usize);
        let tx = self.db.transaction()?;
        for (position, id) in ids.iter().enumerate() {
            tx.execute("UPDATE field_defs SET position = ?2 WHERE id = ?1", params![id, position as i64])?;
        }
        tx.commit()?;
        Ok(())
    }

    // ---- files

    pub fn list_files(&self) -> Result<Vec<PaperFile>> {
        self.read_files("", [])
    }

    pub fn paper_files(&self, paper_id: &str) -> Result<Vec<PaperFile>> {
        self.read_files("WHERE paper_id = ?1", [paper_id])
    }

    fn read_files<P: rusqlite::Params>(&self, filter: &str, args: P) -> Result<Vec<PaperFile>> {
        let mut stmt = self.db.prepare(&format!("SELECT id, paper_id, resource_type, title, format, local_path, size_bytes, added_at FROM resources {filter} ORDER BY resource_type, added_at"))?;
        let files = stmt
            .query_map(args, |r| {
                Ok(PaperFile { id: r.get(0)?, paper_id: r.get(1)?, kind: r.get(2)?, title: r.get(3)?, format: r.get(4)?, local_path: r.get(5)?, size_bytes: r.get::<_, i64>(6)? as u64, added_at: r.get(7)? })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(files)
    }

    /// Copy `source` into `papers/` and attach it to the paper. The original
    /// stays where it is. The stored name is the resource id, so odd file
    /// names survive a move between systems; the same content attached to
    /// the same paper twice is stored once.
    pub fn import_file(&mut self, paper_id: &str, source: &Path, kind: &str) -> Result<PaperFile> {
        if !["fulltext", "supplement", "other"].contains(&kind) { bail!("不支持的附件类型：{kind}") }
        self.db.query_row("SELECT 1 FROM papers WHERE id = ?1", [paper_id], |_| Ok(())).optional()?.ok_or_else(|| anyhow!("文献不存在：{paper_id}"))?;
        let title = source.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "附件".into());
        let format: String = source.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default().chars().filter(|c| c.is_ascii_alphanumeric()).take(10).collect();
        let id = new_id("R");
        let relative = if format.is_empty() { format!("papers/{id}") } else { format!("papers/{id}.{format}") };
        let target = self.root.join(&relative);
        std::fs::create_dir_all(self.root.join("papers"))?;
        let (size, sha) = copy_hashed(source, &target).with_context(|| format!("无法复制文件 {}", source.display()))?;
        let same: Option<String> = self.db.query_row("SELECT id FROM resources WHERE paper_id = ?1 AND sha256 = ?2", params![paper_id, sha], |r| r.get(0)).optional()?;
        if let Some(same) = same {
            let _ = std::fs::remove_file(&target);
            return self.read_files("WHERE id = ?1", [same])?.pop().ok_or_else(|| anyhow!("附件记录丢失"));
        }
        let file = PaperFile { id, paper_id: paper_id.to_string(), kind: kind.to_string(), title, format, local_path: relative, size_bytes: size, added_at: now_iso() };
        let inserted = self.db.execute(
            "INSERT INTO resources(id, paper_id, resource_type, title, format, local_path, size_bytes, sha256, added_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![file.id, file.paper_id, file.kind, file.title, file.format, file.local_path, file.size_bytes as i64, sha, file.added_at],
        );
        if let Err(err) = inserted {
            let _ = std::fs::remove_file(&target);
            return Err(err.into());
        }
        Ok(file)
    }

    /// Keep a fetched file with a paper: `bytes` as `name`, and where it came from.
    pub fn import_fetched(&mut self, paper_id: &str, name: &str, bytes: &[u8], kind: &str, url: &str, source: &str, version: &str) -> Result<PaperFile> {
        // Through a file of that name, so that it is stored the way a dropped one is.
        let staging = self.root.join("papers").join(format!("fetching-{}", new_id("F")));
        std::fs::create_dir_all(&staging)?;
        let path = staging.join(name);
        let stored = std::fs::write(&path, bytes).map_err(anyhow::Error::from).and_then(|()| self.import_file(paper_id, &path, kind));
        let _ = std::fs::remove_dir_all(&staging);
        let file = stored?;
        self.db.execute(
            "INSERT INTO file_origins(resource_id, url, source, version, fetched_at) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(resource_id) DO UPDATE SET url = excluded.url, source = excluded.source, version = excluded.version, fetched_at = excluded.fetched_at",
            params![file.id, url, source, version, now_iso()],
        )?;
        Ok(file)
    }

    /// Where the fetched files came from, by file: (address, who listed it, version).
    pub fn file_origins(&self) -> Result<BTreeMap<String, (String, String, String)>> {
        let mut stmt = self.db.prepare("SELECT resource_id, url, source, version FROM file_origins")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?))))?.collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
        Ok(rows)
    }

    /// The absolute path of a stored file.
    pub fn file_path(&self, file: &PaperFile) -> PathBuf {
        self.root.join(file.local_path.replace('/', std::path::MAIN_SEPARATOR_STR))
    }

    /// Detach a file. It moves to `removed/` inside the library folder.
    pub fn remove_file(&mut self, id: &str) -> Result<()> {
        let path: Option<String> = self.db.query_row("SELECT local_path FROM resources WHERE id = ?1", [id], |r| r.get(0)).optional()?;
        self.db.execute("DELETE FROM resources WHERE id = ?1", [id])?;
        if let Some(path) = path {
            self.set_aside(&path);
        }
        Ok(())
    }

    fn set_aside(&self, relative: &str) {
        let from = self.root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
        let Some(name) = from.file_name() else { return };
        let dir = self.root.join("removed");
        if std::fs::create_dir_all(&dir).is_ok() {
            let _ = std::fs::rename(&from, dir.join(name));
        }
    }

    // ---- saved searches and import batches

    pub fn list_searches(&self) -> Result<Vec<SavedSearch>> {
        let mut stmt = self.db.prepare("SELECT id, kind, name, query, max_results, enabled, created_at FROM searches ORDER BY created_at, id")?;
        let mut searches = stmt
            .query_map([], |r| Ok(SavedSearch { id: r.get(0)?, kind: r.get(1)?, name: r.get(2)?, query: r.get(3)?, max_results: r.get::<_, i64>(4)? as usize, enabled: r.get::<_, i32>(5)? == 1, created_at: r.get(6)?, last_run: None }))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for search in &mut searches {
            search.last_run = self.db.query_row(&format!("SELECT {BATCH_COLUMNS} FROM import_batches WHERE search_id = ?1 ORDER BY rowid DESC LIMIT 1"), [&search.id], row_to_batch).optional()?;
        }
        Ok(searches)
    }

    /// Create (empty id) or update a saved search.
    pub fn save_search(&mut self, search: &mut SavedSearch) -> Result<()> {
        search.query = search.query.trim().to_string();
        search.name = search.name.trim().to_string();
        let Some((_, kind_name)) = crate::sources::KINDS.iter().find(|(key, _)| *key == search.kind) else { bail!("不支持的来源：{}", search.kind) };
        if search.query.is_empty() { bail!("请填写检索式。") }
        if search.query.chars().count() > 4000 { bail!("检索式不能超过 4000 个字符。") }
        if search.name.chars().count() > 60 { bail!("名称不能超过 60 个字符。") }
        if !(1..=crate::sources::MAX_RESULTS).contains(&search.max_results) { bail!("每次取回的篇数应在 1 到 {} 之间。", crate::sources::MAX_RESULTS) }
        if search.name.is_empty() { search.name = kind_name.to_string() }
        if search.id.is_empty() {
            search.id = new_id("S");
            search.created_at = now_iso();
            self.db.execute(
                "INSERT INTO searches(id, kind, name, query, max_results, enabled, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![search.id, search.kind, search.name, search.query, search.max_results as i64, search.enabled as i32, search.created_at],
            )?;
        } else {
            let changed = self.db.execute("UPDATE searches SET kind = ?2, name = ?3, query = ?4, max_results = ?5, enabled = ?6 WHERE id = ?1", params![search.id, search.kind, search.name, search.query, search.max_results as i64, search.enabled as i32])?;
            if changed == 0 { bail!("检索式不存在：{}", search.id) }
        }
        Ok(())
    }

    /// Forget a saved search. The papers it brought in and the record of its runs stay.
    pub fn remove_search(&mut self, id: &str) -> Result<()> {
        if self.db.execute("DELETE FROM searches WHERE id = ?1", [id])? == 0 { bail!("检索式不存在：{id}") }
        Ok(())
    }

    /// Import what a search brought back as one batch that can be undone.
    pub fn import_batch(&mut self, search_id: &str, label: &str, remote_total: Option<u64>, papers: Vec<Paper>) -> Result<(ImportBatch, ImportResult)> {
        let fetched = papers.len();
        let result = self.import_papers(papers)?;
        let batch = ImportBatch { id: new_id("B"), search_id: search_id.into(), label: label.into(), started_at: now_iso(), remote_total, fetched, added: result.added.len(), skipped: result.skipped.len(), ..Default::default() };
        let tx = self.db.transaction()?;
        write_batch(&tx, &batch)?;
        for paper in &result.added {
            tx.execute("INSERT INTO batch_papers(batch_id, paper_id) VALUES (?1, ?2)", params![batch.id, paper.id])?;
        }
        tx.commit()?;
        Ok((batch, result))
    }

    /// Keep the record of a run that failed, so the search shows why.
    pub fn record_failed_run(&mut self, search_id: &str, label: &str, error: &str) -> Result<ImportBatch> {
        let batch = ImportBatch { id: new_id("B"), search_id: search_id.into(), label: label.into(), started_at: now_iso(), error: error.into(), ..Default::default() };
        let tx = self.db.transaction()?;
        write_batch(&tx, &batch)?;
        tx.commit()?;
        Ok(batch)
    }

    /// The latest runs, newest first.
    pub fn list_batches(&self, limit: usize) -> Result<Vec<ImportBatch>> {
        let mut stmt = self.db.prepare(&format!("SELECT {BATCH_COLUMNS} FROM import_batches ORDER BY rowid DESC LIMIT ?1"))?;
        Ok(stmt.query_map([limit as i64], row_to_batch)?.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Take back what a batch added. A paper someone has since worked on
    /// (a screening decision, reading progress, a star, notes, tags, field
    /// values, a file, or a place in the trash) stays and is reported; the
    /// others are removed for good, so a later run can bring them in again.
    pub fn undo_batch(&mut self, id: &str) -> Result<Undone> {
        let undone_at: Option<String> = self.db.query_row("SELECT undone_at FROM import_batches WHERE id = ?1", [id], |r| r.get(0)).optional()?;
        match undone_at {
            None => bail!("没有这个导入批次：{id}"),
            Some(at) if !at.is_empty() => bail!("这个批次已经撤销过了。"),
            _ => {}
        }
        let ids: Vec<String> = {
            let mut stmt = self.db.prepare("SELECT paper_id FROM batch_papers WHERE batch_id = ?1")?;
            stmt.query_map([id], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut undone = Undone::default();
        for paper_id in ids {
            let paper = self.get_paper(&paper_id)?;
            let trashed = self.db.query_row("SELECT 1 FROM trashed_papers WHERE paper_id = ?1", [&paper_id], |_| Ok(())).optional()?.is_some();
            let touched = paper.screening != "pending" || paper.reading != "unread" || paper.starred || !paper.notes.trim().is_empty() || !paper.tags.is_empty() || !paper.fields.is_empty() || trashed || !self.paper_files(&paper_id)?.is_empty();
            if touched {
                undone.kept.push((paper.id, paper.title));
            } else {
                self.delete_paper(&paper_id)?;
                undone.removed += 1;
            }
        }
        self.db.execute("UPDATE import_batches SET undone_at = ?2 WHERE id = ?1", params![id, now_iso()])?;
        Ok(undone)
    }

    // ---- screening suggestions

    /// Record a suggestion, replacing the paper's earlier one. The paper's
    /// own screening decision is not touched.
    pub fn save_suggestion(&mut self, suggestion: &mut Suggestion) -> Result<()> {
        if suggestion.verdict == "pending" || !one_of(&SCREENINGS, &suggestion.verdict) { bail!("建议应为 included、excluded、uncertain 或 irrelevant：{}", suggestion.verdict) }
        if let Some(confidence) = suggestion.confidence && !(0.0..=1.0).contains(&confidence) { bail!("把握应在 0 到 1 之间。") }
        self.db.query_row("SELECT 1 FROM papers WHERE id = ?1", [&suggestion.paper_id], |_| Ok(())).optional()?.ok_or_else(|| anyhow!("文献不存在：{}", suggestion.paper_id))?;
        suggestion.reason = suggestion.reason.trim().to_string();
        suggestion.excerpt = suggestion.excerpt.trim().to_string();
        suggestion.created_at = now_iso();
        self.db.execute(
            "INSERT INTO screening_suggestions(paper_id, verdict, reason, excerpt, page, basis, confidence, author, created_at, criteria) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
             ON CONFLICT(paper_id) DO UPDATE SET verdict = excluded.verdict, reason = excluded.reason, excerpt = excluded.excerpt, page = excluded.page, basis = excluded.basis, \
                confidence = excluded.confidence, author = excluded.author, created_at = excluded.created_at, criteria = excluded.criteria",
            params![suggestion.paper_id, suggestion.verdict, suggestion.reason, suggestion.excerpt, suggestion.page.map(|p| p as i64), suggestion.basis, suggestion.confidence, suggestion.author, suggestion.created_at, suggestion.criteria.as_deref().map(str::trim)],
        )?;
        Ok(())
    }

    /// Every suggestion, for papers in the trash too.
    pub fn list_suggestions(&self) -> Result<Vec<Suggestion>> {
        let mut stmt = self.db.prepare("SELECT paper_id, verdict, reason, excerpt, page, basis, confidence, author, created_at, criteria FROM screening_suggestions")?;
        Ok(stmt
            .query_map([], |r| {
                Ok(Suggestion { paper_id: r.get(0)?, verdict: r.get(1)?, reason: r.get(2)?, excerpt: r.get(3)?, page: r.get::<_, Option<i64>>(4)?.map(|p| p as usize), basis: r.get(5)?, confidence: r.get(6)?, author: r.get(7)?, created_at: r.get(8)?, criteria: r.get(9)? })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn suggestion(&self, paper_id: &str) -> Result<Option<Suggestion>> {
        Ok(self.list_suggestions()?.into_iter().find(|s| s.paper_id == paper_id))
    }

    // ---- translations

    /// The translation kept under a `translation_key`, if there is one.
    pub fn translation(&self, key: &str) -> Result<Option<String>> {
        Ok(self.db.query_row("SELECT translated_text FROM translations WHERE cache_key = ?1", [key], |r| r.get(0)).optional()?)
    }

    /// Keep a translation. The first one for a key stays: two requests for
    /// the same text may both come back.
    pub fn save_translation(&mut self, key: &str, text: &str, author: &str) -> Result<()> {
        let text = text.trim();
        if text.is_empty() { bail!("译文是空的。") }
        self.db.execute("INSERT INTO translations(cache_key, translated_text, author, created_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(cache_key) DO NOTHING", params![key, text, author, now_iso()])?;
        Ok(())
    }

    // ---- conversations

    /// Keep a conversation: a new one gets its id here, a known one is
    /// replaced. `paper_id` is empty for a conversation with the library.
    pub fn save_conversation(&mut self, conversation: &mut Conversation) -> Result<()> {
        if conversation.id.is_empty() { conversation.id = new_id("C") }
        if conversation.created_at.is_empty() { conversation.created_at = now_iso() }
        conversation.updated_at = now_iso();
        let body = serde_json::to_string(&conversation.transcript)?;
        self.db.execute(
            "INSERT INTO conversations(id, paper_id, title, questions, body, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT(id) DO UPDATE SET title = excluded.title, questions = excluded.questions, body = excluded.body, updated_at = excluded.updated_at",
            params![conversation.id, conversation.paper_id, conversation.transcript.title(), conversation.transcript.questions() as i64, body, conversation.created_at, conversation.updated_at],
        )?;
        Ok(())
    }

    /// The conversations with the library (`paper_id` empty) or with one
    /// paper, the latest first, without what was said in them.
    pub fn list_conversations(&self, paper_id: &str) -> Result<Vec<ConversationInfo>> {
        let mut stmt = self.db.prepare("SELECT id, title, questions, updated_at FROM conversations WHERE paper_id = ?1 ORDER BY updated_at DESC, id DESC LIMIT 200")?;
        Ok(stmt.query_map([paper_id], |r| Ok(ConversationInfo { id: r.get(0)?, title: r.get(1)?, questions: r.get::<_, i64>(2)? as usize, updated_at: r.get(3)? }))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn conversation(&self, id: &str) -> Result<Option<Conversation>> {
        Ok(self
            .db
            .query_row("SELECT id, paper_id, body, created_at, updated_at FROM conversations WHERE id = ?1", [id], |r| {
                // A body this build cannot read shows as an empty conversation, not as an error.
                Ok(Conversation { id: r.get(0)?, paper_id: r.get(1)?, transcript: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default(), created_at: r.get(3)?, updated_at: r.get(4)? })
            })
            .optional()?)
    }

    /// Forget a conversation and its pictures.
    pub fn remove_conversation(&mut self, id: &str) -> Result<()> {
        self.db.execute("DELETE FROM conversations WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Keep a picture of a conversation under an id of the caller's choosing; one already kept stays.
    pub fn save_conversation_image(&mut self, conversation_id: &str, id: &str, media_type: &str, data: &[u8]) -> Result<()> {
        self.db.execute("INSERT INTO conversation_images(conversation_id, id, media_type, data) VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING", params![conversation_id, id, media_type, data])?;
        Ok(())
    }

    /// A picture of a conversation: its media type and bytes.
    pub fn conversation_image(&self, conversation_id: &str, id: &str) -> Result<Option<(String, Vec<u8>)>> {
        Ok(self.db.query_row("SELECT media_type, data FROM conversation_images WHERE conversation_id = ?1 AND id = ?2", [conversation_id, id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)
    }

    /// The pages of a paper's stored full text, when its text has been extracted.
    pub fn paper_pages(&self, paper_id: &str) -> Result<Option<Vec<String>>> {
        for file in self.paper_files(paper_id)?.iter().filter(|f| f.kind == "fulltext") {
            if let Some(Ok(pages)) = self.fulltext_pages(&file.id)? { return Ok(Some(pages)) }
        }
        Ok(None)
    }

    // ---- full text

    /// The cached page text of a file: `None` until it has been extracted,
    /// `Some(Err)` when extraction failed.
    pub fn fulltext_pages(&self, resource_id: &str) -> Result<Option<std::result::Result<Vec<String>, String>>> {
        let row: Option<(String, String)> = self.db.query_row("SELECT pages, error FROM fulltext_text WHERE resource_id = ?1", [resource_id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        Ok(row.map(|(pages, error)| if error.is_empty() { Ok(json_strings(&pages)) } else { Err(error) }))
    }

    /// Keep the page text of a file (or why there is none), and sort it
    /// into the parts of a paper by rule, for 写作.
    pub fn save_fulltext_pages(&mut self, resource_id: &str, pages: &std::result::Result<Vec<String>, String>) -> Result<()> {
        self.save_pages(resource_id, pages, "")
    }

    /// Keep page text a model read off the pages of a file that gave none.
    pub fn save_read_pages(&mut self, resource_id: &str, pages: Vec<String>, model: &str) -> Result<()> {
        self.save_pages(resource_id, &Ok(pages), model)
    }

    fn save_pages(&mut self, resource_id: &str, pages: &std::result::Result<Vec<String>, String>, text_by: &str) -> Result<()> {
        let (json, error) = match pages {
            Ok(pages) => (to_json(pages), String::new()),
            Err(error) => ("[]".to_string(), error.clone()),
        };
        // The file may have been removed while its text was being extracted.
        let kept = self.db.execute(
            "INSERT INTO fulltext_text(resource_id, pages, error, extracted_at) SELECT ?1, ?2, ?3, ?4 WHERE EXISTS (SELECT 1 FROM resources WHERE id = ?1) \
             ON CONFLICT(resource_id) DO UPDATE SET pages = excluded.pages, error = excluded.error, extracted_at = excluded.extracted_at",
            params![resource_id, json, error, now_iso()],
        )?;
        if kept == 0 { return Ok(()) }
        let abstract_text: String = self.db.query_row("SELECT p.abstract FROM papers p JOIN resources r ON r.paper_id = p.id WHERE r.id = ?1", [resource_id], |r| r.get(0)).optional()?.unwrap_or_default();
        match pages {
            Ok(pages) => {
                let (sentences, open) = crate::structure::by_rule(pages, &abstract_text);
                self.write_structure(resource_id, "rules", "", text_by, open, &sentences)
            }
            Err(_) => self.write_structure(resource_id, "none", "", "", 0, &[]),
        }
    }

    // ---- structure: the sentences of a full text, by the part of the paper

    fn write_structure(&mut self, resource_id: &str, method: &str, author: &str, text_by: &str, open: usize, sentences: &[crate::structure::Sentence]) -> Result<()> {
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM sentences WHERE resource_id = ?1", [resource_id])?;
        tx.execute(
            "INSERT INTO structures(resource_id, method, author, version, text_by, open, made_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT(resource_id) DO UPDATE SET method = excluded.method, author = excluded.author, version = excluded.version, open = excluded.open, made_at = excluded.made_at, \
                text_by = CASE WHEN excluded.text_by = '' AND excluded.method = 'model' THEN structures.text_by ELSE excluded.text_by END",
            params![resource_id, method, author, crate::structure::VERSION, text_by, open as i64, now_iso()],
        )?;
        {
            let mut insert = tx.prepare("INSERT INTO sentences(resource_id, seq, section, heading, page, position, text) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)")?;
            for (seq, sentence) in sentences.iter().enumerate() {
                insert.execute(params![resource_id, seq as i64, sentence.section, sentence.heading, sentence.page as i64, sentence.position as i64, sentence.text])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Replace the sentences of a file with those a model's reading of its
    /// outline gave where the rules only guessed. `author` is the model profile.
    pub fn save_model_structure(&mut self, resource_id: &str, author: &str, sentences: &[crate::structure::Sentence]) -> Result<()> {
        if self.db.query_row("SELECT 1 FROM resources WHERE id = ?1", [resource_id], |_| Ok(())).optional()?.is_none() { return Ok(()) }
        self.write_structure(resource_id, "model", author, "", 0, sentences)
    }

    /// Whether a file has yet to be read and sorted for 写作: one stored
    /// before there was such a thing, or just added.
    pub fn needs_structure(&self, resource_id: &str) -> Result<bool> {
        Ok(self.db.query_row("SELECT 1 FROM structures WHERE resource_id = ?1", [resource_id], |_| Ok(())).optional()?.is_none())
    }

    /// Sort again, by rule, the texts sorted by an earlier version of the
    /// rules. What a model sorted stays. Returns how many were redone.
    pub fn restructure_outdated(&mut self) -> Result<usize> {
        let outdated: Vec<String> = {
            let mut stmt = self.db.prepare("SELECT resource_id FROM structures WHERE method = 'rules' AND version < ?1")?;
            stmt.query_map([crate::structure::VERSION], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for id in &outdated {
            let text_by: String = self.db.query_row("SELECT text_by FROM structures WHERE resource_id = ?1", [id], |r| r.get(0))?;
            if let Some(pages) = self.fulltext_pages(id)? { self.save_pages(id, &pages, &text_by)? }
        }
        Ok(outdated.len())
    }

    /// How each full text outside the trash was sorted, by file id.
    pub fn structures(&self) -> Result<BTreeMap<String, StructureInfo>> {
        let mut out = BTreeMap::new();
        let mut stmt = self.db.prepare(
            "SELECT s.resource_id, r.paper_id, s.method, s.author, s.text_by, s.open FROM structures s JOIN resources r ON r.id = s.resource_id \
             WHERE r.resource_type = 'fulltext' AND r.paper_id NOT IN (SELECT paper_id FROM trashed_papers)",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            out.insert(row.get::<_, String>(0)?, StructureInfo { paper_id: row.get(1)?, method: row.get(2)?, author: row.get(3)?, text_by: row.get(4)?, open: row.get::<_, i64>(5)? as usize, counts: BTreeMap::new() });
        }
        let mut stmt = self.db.prepare("SELECT resource_id, section, count(*) FROM sentences GROUP BY resource_id, section")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            if let Some(info) = out.get_mut(&row.get::<_, String>(0)?) { info.counts.insert(row.get(1)?, row.get::<_, i64>(2)? as usize); }
        }
        Ok(out)
    }

    /// The sentences of one part, of every paper outside the trash or of one paper, in reading order.
    pub fn section_sentences(&self, section: &str, paper_id: &str) -> Result<Vec<PaperSentence>> {
        let mut stmt = self.db.prepare(
            "SELECT r.paper_id, s.section, s.heading, s.page, s.position, s.text FROM sentences s JOIN resources r ON r.id = s.resource_id \
             WHERE s.section = ?1 AND r.resource_type = 'fulltext' AND (?2 = '' OR r.paper_id = ?2) AND r.paper_id NOT IN (SELECT paper_id FROM trashed_papers) \
             ORDER BY r.paper_id, s.seq",
        )?;
        Ok(stmt
            .query_map(params![section, paper_id], |r| Ok(PaperSentence { paper_id: r.get(0)?, sentence: crate::structure::Sentence { section: r.get(1)?, heading: r.get(2)?, page: r.get::<_, i64>(3)? as usize, position: r.get::<_, i64>(4)? as usize, text: r.get(5)? } }))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // ---- drafts

    /// What the person has written for each part of the manuscript, by a key of `structure::SECTIONS`.
    pub fn drafts(&self) -> Result<BTreeMap<String, String>> {
        let mut stmt = self.db.prepare("SELECT section, body FROM drafts")?;
        Ok(stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<BTreeMap<_, _>>>()?)
    }

    pub fn save_draft(&mut self, section: &str, body: &str) -> Result<()> {
        if !crate::structure::SECTIONS.iter().any(|(key, _)| *key == section) { bail!("没有这个部分：{section}") }
        self.db.execute("INSERT INTO drafts(section, body, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT(section) DO UPDATE SET body = excluded.body, updated_at = excluded.updated_at", params![section, body, now_iso()])?;
        Ok(())
    }

    /// Every extracted full text, by paper id: (file, pages).
    /// The full-text files that gave no text, with why: (file, paper, reason).
    /// A paper that has another file with text is not among them.
    pub fn fulltext_failures(&self) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self.db.prepare(
            "SELECT r.id, r.paper_id, t.error FROM resources r JOIN fulltext_text t ON t.resource_id = r.id \
             WHERE r.resource_type = 'fulltext' AND t.error != '' AND r.paper_id NOT IN (SELECT paper_id FROM trashed_papers) \
               AND r.paper_id NOT IN (SELECT r2.paper_id FROM resources r2 JOIN fulltext_text t2 ON t2.resource_id = r2.id WHERE r2.resource_type = 'fulltext' AND t2.error = '') \
             ORDER BY r.paper_id, r.id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn all_fulltext(&self) -> Result<BTreeMap<String, (PaperFile, Vec<String>)>> {
        let mut out = BTreeMap::new();
        for file in self.read_files("WHERE resource_type = 'fulltext' AND paper_id NOT IN (SELECT paper_id FROM trashed_papers)", [])? {
            if out.contains_key(&file.paper_id) { continue }
            if let Some(Ok(pages)) = self.fulltext_pages(&file.id)? {
                out.insert(file.paper_id.clone(), (file, pages));
            }
        }
        Ok(out)
    }
}

fn row_to_paper(r: &rusqlite::Row) -> rusqlite::Result<Paper> {
    Ok(Paper {
        id: r.get(0)?,
        title: r.get(1)?,
        abstract_text: r.get(2)?,
        authors: json_strings(&r.get::<_, String>(3)?).join("; "),
        journal: r.get(4)?,
        year: r.get(5)?,
        publication_status: r.get(6)?,
        url: r.get(7)?,
        source: r.get(8)?,
        screening: r.get(9)?,
        screening_reason: r.get(10)?,
        reviewed_at: r.get(11)?,
        reading: r.get(12)?,
        starred: r.get::<_, i32>(13)? == 1,
        tags: json_strings(&r.get::<_, String>(14)?),
        notes: r.get(15)?,
        added_at: r.get(16)?,
        updated_at: r.get(17)?,
        ..Default::default()
    })
}

fn existing_paper(db: &Connection, p: &Paper) -> Result<Option<String>> {
    for (namespace, value) in [("doi", &p.doi), ("pmid", &p.pmid)] {
        if value.is_empty() { continue }
        let found: Option<String> = db.query_row("SELECT paper_id FROM paper_identifiers WHERE namespace = ?1 AND value = ?2", params![namespace, value], |r| r.get(0)).optional()?;
        if found.is_some() { return Ok(found) }
    }
    let key = title_key(&p.title);
    if key.is_empty() { return Ok(None) }
    // The same title under two DOIs is two papers; without a DOI on either side it is one.
    let found: Option<String> = db
        .query_row(
            "SELECT id FROM papers WHERE title_key = ?1 AND (?2 = '' OR NOT EXISTS (SELECT 1 FROM paper_identifiers i WHERE i.paper_id = papers.id AND i.namespace = 'doi')) LIMIT 1",
            params![key, p.doi],
            |r| r.get(0),
        )
        .optional()?;
    Ok(found)
}

fn write_paper(tx: &Transaction, p: &Paper) -> Result<()> {
    let authors: Vec<String> = p.authors.split(';').map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect();
    tx.execute(
        "INSERT INTO papers(id, title, title_key, abstract, authors_json, journal, year, publication_state, url, source, \
            screening_status, screening_reason, reviewed_at, reading_status, starred, tags_json, notes, first_seen_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19) \
         ON CONFLICT(id) DO UPDATE SET title = excluded.title, title_key = excluded.title_key, abstract = excluded.abstract, \
            authors_json = excluded.authors_json, journal = excluded.journal, year = excluded.year, \
            publication_state = excluded.publication_state, url = excluded.url, source = excluded.source, \
            screening_status = excluded.screening_status, screening_reason = excluded.screening_reason, reviewed_at = excluded.reviewed_at, \
            reading_status = excluded.reading_status, starred = excluded.starred, tags_json = excluded.tags_json, notes = excluded.notes, \
            first_seen_at = excluded.first_seen_at, updated_at = excluded.updated_at",
        params![
            p.id, p.title, title_key(&p.title), p.abstract_text, to_json(&authors), p.journal, p.year, p.publication_status, p.url, p.source,
            p.screening, p.screening_reason, p.reviewed_at, p.reading, p.starred as i32, to_json(&p.tags), p.notes, p.added_at, p.updated_at
        ],
    )
    .with_context(|| format!("保存文献失败：{}", p.title))?;
    tx.execute("DELETE FROM paper_identifiers WHERE paper_id = ?1", [&p.id])?;
    for (namespace, value) in [("doi", &p.doi), ("pmid", &p.pmid)] {
        if !value.is_empty() {
            tx.execute("INSERT INTO paper_identifiers(paper_id, namespace, value) VALUES (?1, ?2, ?3)", params![p.id, namespace, value])?;
        }
    }
    tx.execute("DELETE FROM paper_fields WHERE paper_id = ?1", [&p.id])?;
    for (field, values) in &p.fields {
        // Values of a field that was deleted meanwhile are dropped.
        tx.execute(
            "INSERT INTO paper_fields(paper_id, field_id, values_json) SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM field_defs WHERE id = ?2)",
            params![p.id, field, to_json(values)],
        )?;
    }
    Ok(())
}

fn write_batch(tx: &Transaction, batch: &ImportBatch) -> Result<()> {
    tx.execute(
        &format!("INSERT INTO import_batches({BATCH_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"),
        params![batch.id, batch.search_id, batch.label, batch.started_at, batch.remote_total.map(|n| n as i64), batch.fetched as i64, batch.added as i64, batch.skipped as i64, batch.error, batch.undone_at],
    )?;
    Ok(())
}

/// What a translation is kept under: the text and the instructions it was
/// made with, so changed instructions translate anew.
pub fn translation_key(text: &str, prompt: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_string(&[text.trim(), prompt.trim()]).unwrap_or_default());
    hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

fn copy_hashed(source: &Path, target: &Path) -> Result<(u64, String)> {
    let mut input = std::fs::File::open(source)?;
    let mut output = std::fs::File::create(target)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    let mut size = 0u64;
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 { break }
        hasher.update(&buffer[..n]);
        output.write_all(&buffer[..n])?;
        size += n as u64;
    }
    output.flush()?;
    Ok((size, hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structure_and_drafts() {
        let dir = temp_dir("structure");
        let mut store = Store::in_memory(&dir).unwrap();
        let p = store.add_paper(paper("A paper with parts", "")).unwrap();
        let source = dir.join("a.txt");
        std::fs::write(&source, "x").unwrap();
        let file = store.import_file(&p.id, &source, "fulltext").unwrap();
        assert!(store.needs_structure(&file.id).unwrap());
        let page = "Introduction\n\nRoots grow continuously because of the stem cells at their tips. However, how they respond to soil remains poorly understood.\n\nMethods\n\nSeeds were sown on plates and grown for five days under long days.";
        store.save_fulltext_pages(&file.id, &Ok(vec![page.into()])).unwrap();
        assert!(!store.needs_structure(&file.id).unwrap());
        let info = &store.structures().unwrap()[&file.id];
        assert_eq!((info.method.as_str(), info.paper_id.as_str(), info.counts.get("introduction"), info.counts.get("methods")), ("rules", p.id.as_str(), Some(&2), Some(&1)));
        let intro = store.section_sentences("introduction", "").unwrap();
        assert_eq!(intro.iter().map(|s| (s.paper_id.as_str(), s.sentence.position, s.sentence.page)).collect::<Vec<_>>(), [(p.id.as_str(), 0, 1), (p.id.as_str(), 1, 1)]);
        assert!(store.section_sentences("introduction", "P-other").unwrap().is_empty());

        // A model's sorting replaces the rules' and is not redone by them.
        let moved = crate::structure::Sentence { section: "results".into(), page: 1, text: "Seeds were sown on plates and grown for five days under long days.".into(), ..Default::default() };
        store.save_model_structure(&file.id, "模型 · m", &[moved]).unwrap();
        store.db.execute("UPDATE structures SET version = 0", []).unwrap();
        assert_eq!(store.restructure_outdated().unwrap(), 0);
        let info = &store.structures().unwrap()[&file.id];
        assert_eq!((info.method.as_str(), info.author.as_str(), info.counts.get("results"), info.counts.get("introduction")), ("model", "模型 · m", Some(&1), None));
        // The rules' own work is done again when the rules change.
        store.db.execute("UPDATE structures SET method = 'rules'", []).unwrap();
        assert_eq!(store.restructure_outdated().unwrap(), 1);
        assert_eq!(store.section_sentences("introduction", &p.id).unwrap().len(), 2);

        // A file that gives no text is known as such, and may get its text from a model later.
        let scan = dir.join("scan.txt");
        std::fs::write(&scan, "y").unwrap();
        let q = store.add_paper(paper("A scan", "")).unwrap();
        let scanned = store.import_file(&q.id, &scan, "fulltext").unwrap();
        store.save_fulltext_pages(&scanned.id, &Err("没有可提取的文字".into())).unwrap();
        assert_eq!(store.structures().unwrap()[&scanned.id].method, "none");
        store.save_read_pages(&scanned.id, vec![page.into()], "视觉 · v").unwrap();
        let info = &store.structures().unwrap()[&scanned.id];
        assert_eq!((info.method.as_str(), info.text_by.as_str(), info.counts.get("methods")), ("rules", "视觉 · v", Some(&1)));
        store.save_model_structure(&scanned.id, "模型 · m", &[]).unwrap();
        assert_eq!(store.structures().unwrap()[&scanned.id].text_by, "视觉 · v");

        // The trash and a removed file take their sentences along.
        store.trash_paper(&p.id).unwrap();
        assert!(store.section_sentences("introduction", "").unwrap().iter().all(|s| s.paper_id == q.id));
        store.remove_file(&scanned.id).unwrap();
        assert!(store.section_sentences("introduction", "").unwrap().is_empty());

        assert!(store.drafts().unwrap().is_empty());
        store.save_draft("introduction", "Roots are").unwrap();
        store.save_draft("introduction", "Roots are long.").unwrap();
        assert_eq!(store.drafts().unwrap().get("introduction").map(String::as_str), Some("Roots are long."));
        assert!(store.save_draft("preface", "x").is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wisp-test-{name}-{}", new_id("T")));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn paper(title: &str, doi: &str) -> Paper {
        Paper { title: title.into(), doi: doi.into(), authors: "Zhang Wei; O'Brien Ann".into(), year: Some(2024), ..Default::default() }
    }

    #[test]
    fn add_list_and_duplicates() {
        let dir = temp_dir("add");
        let mut store = Store::open(&dir).unwrap();
        let added = store.add_paper(paper("Single-cell atlas", "https://doi.org/10.1000/ABC")).unwrap();
        assert_eq!(added.doi, "10.1000/abc");
        // Same DOI, same title without a DOI: both known. Same title, other DOI: new.
        let result = store.import_papers(vec![paper("Other title", "10.1000/abc"), paper("single cell ATLAS.", ""), paper("Single-cell atlas", "10.1000/xyz")]).unwrap();
        assert_eq!(result.skipped.len(), 2);
        assert_eq!(result.added.len(), 1);
        let papers = store.list_papers().unwrap();
        assert_eq!(papers.len(), 2);
        assert_eq!(papers[0].authors, "Zhang Wei; O'Brien Ann");
        assert_eq!(papers[0].fulltext, "linked");
        assert!(store.add_paper(paper("", "")).is_err());
        drop(store);
        // Reopening reads the same library.
        assert_eq!(Store::open(&dir).unwrap().list_papers().unwrap().len(), 2);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn save_checks_identifiers() {
        let dir = temp_dir("save");
        let mut store = Store::in_memory(&dir).unwrap();
        let a = store.add_paper(paper("A paper", "10.1000/a")).unwrap();
        let mut b = store.add_paper(paper("B paper", "10.1000/b")).unwrap();
        b.doi = a.doi.clone();
        assert!(store.save_paper(&mut b).is_err());
        b.doi = "10.1000/b2".into();
        b.decide("included", "fits");
        b.tags = vec!["atlas".into()];
        store.save_paper(&mut b).unwrap();
        let read = store.get_paper(&b.id).unwrap();
        assert_eq!((read.doi.as_str(), read.screening.as_str(), read.tags.len()), ("10.1000/b2", "included", 1));
        assert!(!read.reviewed_at.is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn custom_fields() {
        let dir = temp_dir("fields");
        let mut store = Store::in_memory(&dir).unwrap();
        let mut species = FieldDef { name: "物种".into(), kind: FieldKind::Tags, in_table: true, ..Default::default() };
        store.save_field(&mut species).unwrap();
        let mut method = FieldDef { name: "方法".into(), kind: FieldKind::Select, options: vec!["scRNA-seq".into(), "snRNA-seq".into()], ..Default::default() };
        store.save_field(&mut method).unwrap();
        assert!(store.save_field(&mut FieldDef { name: "物种".into(), ..Default::default() }).is_err());
        assert!(store.save_field(&mut FieldDef { name: "空单选".into(), kind: FieldKind::Select, ..Default::default() }).is_err());

        let mut p = store.add_paper(paper("Rice roots", "")).unwrap();
        p.fields.insert(species.id.clone(), vec!["Oryza sativa".into(), "Oryza sativa".into()]);
        p.fields.insert(method.id.clone(), vec!["scRNA-seq".into()]);
        p.fields.insert("F-gone".into(), vec!["x".into()]);
        store.save_paper(&mut p).unwrap();
        let read = store.get_paper(&p.id).unwrap();
        assert_eq!(read.fields.len(), 2);
        assert_eq!(read.fields[&species.id], vec!["Oryza sativa"]);

        store.move_field(&method.id, -1).unwrap();
        assert_eq!(store.list_fields().unwrap()[0].name, "方法");
        store.delete_field(&species.id).unwrap();
        assert_eq!(store.get_paper(&p.id).unwrap().fields.len(), 1);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn searches_and_batches() {
        let dir = temp_dir("batches");
        let mut store = Store::in_memory(&dir).unwrap();
        let mut search = SavedSearch { kind: "pubmed".into(), query: "  root[Title] ".into(), max_results: 50, enabled: true, ..Default::default() };
        store.save_search(&mut search).unwrap();
        assert_eq!((search.name.as_str(), search.query.as_str()), ("PubMed", "root[Title]"));
        for bad in [SavedSearch { kind: "scholar".into(), query: "x".into(), max_results: 5, ..Default::default() }, SavedSearch { kind: "pubmed".into(), max_results: 5, ..Default::default() }, SavedSearch { kind: "pubmed".into(), query: "x".into(), max_results: 0, ..Default::default() }] {
            assert!(store.save_search(&mut bad.clone()).is_err(), "{bad:?}");
        }
        search.name = "根".into();
        search.enabled = false;
        store.save_search(&mut search).unwrap();
        assert!(store.list_searches().unwrap()[0].last_run.is_none());

        let existing = store.add_paper(paper("Known", "10.1000/known")).unwrap();
        let (batch, result) = store.import_batch(&search.id, "PubMed: root[Title]", Some(120), vec![paper("One", "10.1000/1"), paper("Two", "10.1000/2"), paper("Known again", "10.1000/known")]).unwrap();
        assert_eq!((batch.remote_total, batch.fetched, batch.added, batch.skipped, result.added.len()), (Some(120), 3, 2, 1, 2));
        store.record_failed_run(&search.id, "PubMed: root[Title]", "网络请求失败").unwrap();
        let listed = store.list_searches().unwrap();
        assert_eq!((listed.len(), listed[0].name.as_str(), listed[0].enabled, listed[0].last_run.as_ref().unwrap().error.as_str()), (1, "根", false, "网络请求失败"));
        assert_eq!(store.list_batches(10).unwrap().len(), 2);
        assert_eq!(store.list_batches(1).unwrap()[0].error, "网络请求失败");

        // Undo removes what nobody touched and keeps what someone has decided on.
        let mut two = result.added[1].clone();
        two.decide("included", "");
        store.save_paper(&mut two).unwrap();
        let undone = store.undo_batch(&batch.id).unwrap();
        assert_eq!((undone.removed, undone.kept.clone()), (1, vec![(two.id.clone(), "Two".to_string())]));
        let left: Vec<String> = store.list_papers().unwrap().into_iter().map(|p| p.id).collect();
        assert!(left.contains(&existing.id) && left.contains(&two.id) && left.len() == 2);
        // Removed for good: the same record can come in again.
        assert_eq!(store.import_papers(vec![paper("One", "10.1000/1")]).unwrap().added.len(), 1);
        assert!(store.undo_batch(&batch.id).unwrap_err().to_string().contains("已经撤销"));
        assert!(store.undo_batch("B-none").is_err());

        store.remove_search(&search.id).unwrap();
        assert!(store.list_searches().unwrap().is_empty() && store.list_batches(10).unwrap().len() == 2);
        assert!(store.remove_search(&search.id).is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn suggestions_of_an_earlier_build_gain_a_goal() {
        let dir = temp_dir("goal-column");
        let db = Connection::open_in_memory().unwrap();
        // The table as builds before the goal made it.
        db.execute_batch("CREATE TABLE screening_suggestions (paper_id TEXT PRIMARY KEY NOT NULL, verdict TEXT NOT NULL, reason TEXT NOT NULL DEFAULT '', excerpt TEXT NOT NULL DEFAULT '', page INTEGER, basis TEXT NOT NULL DEFAULT '', confidence REAL, author TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL);").unwrap();
        let store = Store::init(dir.clone(), db).unwrap();
        assert!(store.list_suggestions().unwrap().is_empty());
        // Opening it again leaves the column alone.
        add_columns(&store.db).unwrap();
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn suggestions_do_not_decide() {
        let dir = temp_dir("suggest");
        let mut store = Store::in_memory(&dir).unwrap();
        let p = store.add_paper(paper("A paper", "")).unwrap();
        let mut first = Suggestion { paper_id: p.id.clone(), verdict: "irrelevant".into(), reason: " 讲的是叶 ".into(), basis: "abstract".into(), confidence: Some(0.9), author: "mock".into(), ..Default::default() };
        store.save_suggestion(&mut first).unwrap();
        assert_eq!((first.reason.as_str(), store.get_paper(&p.id).unwrap().screening.as_str()), ("讲的是叶", "pending"));
        // A second one replaces the first.
        store.save_suggestion(&mut Suggestion { paper_id: p.id.clone(), verdict: "included".into(), excerpt: "roots".into(), page: Some(3), ..Default::default() }).unwrap();
        let all = store.list_suggestions().unwrap();
        assert_eq!((all.len(), all[0].verdict.as_str(), all[0].page, all[0].confidence), (1, "included", Some(3), None));
        assert_eq!(store.suggestion(&p.id).unwrap().unwrap().excerpt, "roots");
        // The goal it was made against is kept; one from before goals were kept counts as made for any.
        assert!(all[0].criteria.is_none() && all[0].made_for("只收根"));
        store.save_suggestion(&mut Suggestion { paper_id: p.id.clone(), verdict: "included".into(), criteria: Some(" 只收根 ".into()), ..Default::default() }).unwrap();
        let kept = store.suggestion(&p.id).unwrap().unwrap();
        assert_eq!(kept.criteria.as_deref(), Some("只收根"));
        assert!(kept.made_for("只收根 ") && !kept.made_for("只收叶") && !kept.made_for(""));
        for bad in [Suggestion { paper_id: p.id.clone(), verdict: "pending".into(), ..Default::default() }, Suggestion { paper_id: p.id.clone(), verdict: "maybe".into(), ..Default::default() }, Suggestion { paper_id: "P-none".into(), verdict: "included".into(), ..Default::default() }, Suggestion { paper_id: p.id.clone(), verdict: "included".into(), confidence: Some(1.5), ..Default::default() }] {
            assert!(store.save_suggestion(&mut bad.clone()).is_err(), "{bad:?}");
        }
        assert!(store.paper_pages(&p.id).unwrap().is_none());
        // It goes with the paper.
        store.delete_paper(&p.id).unwrap();
        assert!(store.list_suggestions().unwrap().is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn conversations_are_kept() {
        use crate::ai::{Said, Source, Transcript};
        let dir = temp_dir("conversations");
        let mut store = Store::in_memory(&dir).unwrap();
        let paper = store.add_paper(Paper { title: "Alpha".into(), ..Default::default() }).unwrap();
        let transcript = Transcript { items: vec![Said::User { text: "WOX5 做什么？".into(), images: vec!["I-1".into()] }, Said::Answer { text: "见 [P1]。".into() }], sources: vec![Source { label: "P1".into(), paper_id: paper.id.clone(), title: "Alpha".into() }] };
        let mut with_library = Conversation { transcript: transcript.clone(), ..Default::default() };
        store.save_conversation(&mut with_library).unwrap();
        let mut with_paper = Conversation { paper_id: paper.id.clone(), transcript: Transcript { items: vec![Said::User { text: "总结一下".into(), images: vec![] }], sources: vec![] }, ..Default::default() };
        store.save_conversation(&mut with_paper).unwrap();
        assert!(with_library.id.starts_with("C-") && with_library.id != with_paper.id);
        // Each side lists its own, with a title and the number of questions.
        let listed = store.list_conversations("").unwrap();
        assert_eq!(listed.iter().map(|c| (c.id.as_str(), c.title.as_str(), c.questions)).collect::<Vec<_>>(), [(with_library.id.as_str(), "WOX5 做什么？", 1)]);
        assert_eq!(store.list_conversations(&paper.id).unwrap()[0].title, "总结一下");
        assert_eq!(store.conversation(&with_library.id).unwrap().unwrap().transcript, transcript);
        // Saved again it is the same conversation, with what was added.
        let created = with_library.created_at.clone();
        with_library.transcript.items.push(Said::User { text: "再说说 SCR".into(), images: vec![] });
        store.save_conversation(&mut with_library).unwrap();
        let again = store.conversation(&with_library.id).unwrap().unwrap();
        assert_eq!((store.list_conversations("").unwrap().len(), again.transcript.questions(), again.created_at), (1, 2, created));
        // A picture is kept once and goes with its conversation.
        store.save_conversation_image(&with_library.id, "I-1", "image/png", &[1, 2, 3]).unwrap();
        store.save_conversation_image(&with_library.id, "I-1", "image/png", &[9]).unwrap();
        assert_eq!(store.conversation_image(&with_library.id, "I-1").unwrap(), Some(("image/png".to_string(), vec![1, 2, 3])));
        store.remove_conversation(&with_library.id).unwrap();
        assert!(store.conversation(&with_library.id).unwrap().is_none() && store.conversation_image(&with_library.id, "I-1").unwrap().is_none());
        // A paper deleted for good takes its conversations with it.
        store.trash_paper(&paper.id).unwrap();
        store.delete_paper(&paper.id).unwrap();
        assert!(store.list_conversations(&paper.id).unwrap().is_empty());
    }

    #[test]
    fn translations_are_kept() {
        let mut store = Store::in_memory(Path::new("/tmp/wisp-translations")).unwrap();
        let key = translation_key(" Root stem cells ", "译成中文");
        assert_eq!(key.len(), 64);
        // The same text under other instructions, or another text, is another translation.
        assert_eq!(key, translation_key("Root stem cells", "译成中文 "));
        assert!(key != translation_key("Root stem cells", "译成日语") && key != translation_key("Root stem cell", "译成中文"));
        assert_eq!(store.translation(&key).unwrap(), None);
        assert!(store.save_translation(&key, "  ", "m").is_err());
        store.save_translation(&key, " 根干细胞 ", "Haiku").unwrap();
        // A second answer for the same text does not replace the first.
        store.save_translation(&key, "根部干细胞", "Haiku").unwrap();
        assert_eq!(store.translation(&key).unwrap().as_deref(), Some("根干细胞"));
    }

    #[test]
    fn trash_and_files() {
        let dir = temp_dir("files");
        let mut store = Store::open(&dir).unwrap();
        let p = store.add_paper(paper("With file", "")).unwrap();
        let source = dir.join("我的 论文.PDF");
        std::fs::write(&source, b"%PDF-1.4 fake").unwrap();
        let file = store.import_file(&p.id, &source, "fulltext").unwrap();
        assert_eq!((file.format.as_str(), file.title.as_str(), file.size_bytes), ("pdf", "我的 论文.PDF", 13));
        assert!(store.file_path(&file).exists());
        assert!(source.exists());
        // The same content again is the same attachment.
        assert_eq!(store.import_file(&p.id, &source, "fulltext").unwrap().id, file.id);
        assert_eq!(store.paper_files(&p.id).unwrap().len(), 1);
        assert_eq!(store.get_paper(&p.id).unwrap().fulltext, "stored");

        assert!(store.fulltext_pages(&file.id).unwrap().is_none());
        store.save_fulltext_pages(&file.id, &Ok(vec!["page one".into(), "page two".into()])).unwrap();
        assert_eq!(store.fulltext_pages(&file.id).unwrap().unwrap().unwrap().len(), 2);
        assert_eq!(store.all_fulltext().unwrap().len(), 1);

        store.trash_paper(&p.id).unwrap();
        assert!(store.list_papers().unwrap().is_empty());
        assert_eq!(store.list_trash().unwrap().len(), 1);
        // A trashed paper still blocks its re-import.
        assert!(store.import_papers(vec![paper("With file", "")]).unwrap().skipped[0].in_trash);
        store.restore_paper(&p.id).unwrap();
        assert_eq!(store.list_papers().unwrap().len(), 1);

        store.delete_paper(&p.id).unwrap();
        assert!(!store.file_path(&file).exists());
        assert!(dir.join("removed").read_dir().unwrap().count() == 1);
        assert!(store.list_files().unwrap().is_empty());
        std::fs::remove_dir_all(dir).ok();
    }
}
