//! Notion notes: configuration, portable questions and the durable remote-page index.
//! No network here; `wisp_net::notion` performs the MCP calls.

use crate::{ai, libraries::LibraryInfo, model::Paper};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub parent_url: String,
    pub root_title: String,
    /// Read-only compatibility with earlier global custom-title settings.
    pub title_template: String,
    /// Custom mode asks for a literal title for this paper when saving.
    pub title_mode: String,
    pub title_parts: Vec<String>,
    pub fields: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { parent_url: String::new(), root_title: "WispPaper笔记".into(), title_template: "{journal} {year} {title}".into(), title_mode: String::new(), title_parts: ["journal", "year", "title"].map(String::from).to_vec(), fields: ["title", "doi", "journal", "date", "authors", "library"].map(String::from).to_vec() }
    }
}

pub const FIELDS: &[(&str, &str)] = &[("title", "题目"), ("doi", "DOI 链接"), ("journal", "出版期刊"), ("date", "出版日期"), ("authors", "作者"), ("pmid", "PMID"), ("abstract", "摘要"), ("tags", "标签"), ("url", "文献网址"), ("library", "来源文献库")];
pub const TITLE_PARTS: &[(&str, &str)] = &[("journal", "出版期刊"), ("year", "出版年份"), ("title", "题目"), ("authors", "作者"), ("doi", "DOI"), ("pmid", "PMID"), ("paper_id", "文献编号")];
const TITLE_LIMIT: usize = 100;
/// Scope for libraries created directly on the private home, never a remote page ID.
pub const PRIVATE_ROOT: &str = "private";

impl Settings {
    /// Collapse older duplicate date choices while keeping the first selected position.
    pub fn tidy(&mut self) {
        for (parts, old, new) in [(&mut self.title_parts, "date", "year"), (&mut self.fields, "year", "date")] {
            for part in parts.iter_mut() { if part == old { *part = new.into(); } }
            let mut seen = std::collections::BTreeSet::new();
            parts.retain(|part| seen.insert(part.clone()));
        }
        self.title_template = self.title_template.replace("{date}", "{year}");
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.parent_url.trim().is_empty() { page_id(&self.parent_url)?; }
        if !["", "parts", "custom"].contains(&self.title_mode.as_str()) { return Err("不支持的文献笔记标题方式。".into()) }
        if !self.custom_title() {
            if self.title_parts.is_empty() { return Err("请选择至少一个文献笔记标题组成项。".into()) }
            for (i, part) in self.title_parts.iter().enumerate() {
                if !TITLE_PARTS.iter().any(|(key, _)| key == part) || self.title_parts[..i].contains(part) { return Err("文献笔记标题包含重复或未知组成项。".into()) }
            }
        }
        for (i, field) in self.fields.iter().enumerate() {
            if !FIELDS.iter().any(|(key, _)| key == field) || self.fields[..i].contains(field) { return Err("文献基本信息包含重复或不支持的字段。".into()) }
        }
        Ok(())
    }

    pub fn destination(&self) -> Result<String, String> {
        if self.root_title.trim().is_empty() {
            return if self.parent_url.trim().is_empty() { Ok(PRIVATE_ROOT.into()) } else { Ok(format!("{}/direct", page_id(&self.parent_url)?)) };
        }
        if self.parent_url.trim().is_empty() { Ok(format!("private:{}", self.root_title.trim())) } else { Ok(format!("{}/root:{}", page_id(&self.parent_url)?, fingerprint(self.root_title.trim()))) }
    }

    pub fn paper_title(&self, paper: &Paper) -> String {
        let value = |key: &str| match key {
            "journal" => line(&paper.journal), "year" => paper.year.map(|y| y.to_string()).or_else(|| (!paper.publication_date.is_empty() && valid_date(&paper.publication_date)).then(|| paper.publication_date[..4].to_string())).unwrap_or_default(), "title" => line(&paper.title),
            "authors" => authors_for_notes(&paper.authors), "doi" => paper.doi.clone(), "pmid" => paper.pmid.clone(), "paper_id" => paper.id.clone(), _ => String::new(),
        };
        let title = self.title_parts.iter().map(|key| value(key)).filter(|s| !s.trim().is_empty()).collect::<Vec<_>>().join(" · ");
        let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        if title.chars().count() > TITLE_LIMIT { format!("{}…", title.chars().take(TITLE_LIMIT - 1).collect::<String>()) } else if title.is_empty() { paper.id.clone() } else { title }
    }

    pub fn custom_title(&self) -> bool { self.title_mode == "custom" || (self.title_mode.is_empty() && self.title_template != "{journal} {year} {title}") }

    pub fn header(&self, library: &LibraryInfo, paper: Option<&Paper>) -> String {
        let mut out = String::from("## 文献基本信息\n\n");
        if let Some(p) = paper {
            for field in &self.fields {
                let value = match field.as_str() {
                    "title" => line(&p.title), "doi" if !p.doi.is_empty() => format!("https://doi.org/{}", crate::lookup::encode(&p.doi)),
                    "journal" => line(&p.journal), "date" => p.publication_date.clone(),
                    "authors" => authors_for_notes(&p.authors), "pmid" => p.pmid.clone(), "abstract" => p.abstract_text.clone(), "tags" => p.tags.join("、"),
                    "url" => p.url.clone(), "library" => format!("{}（{}）", line(&library.name), library.id), _ => String::new(),
                };
                if !value.trim().is_empty() {
                    let label = FIELDS.iter().find(|(key, _)| key == field).unwrap().1;
                    out.push_str(&format!("**{label}**：{value}\n\n"));
                }
            }
        } else {
            out = format!("## 文献库信息\n\n**来源文献库**：{}（{}）\n\n", line(&library.name), library.id);
            if !library.topic.trim().is_empty() { out.push_str(&format!("**主题**：{}\n\n", library.topic)); }
        }
        out.push_str("## 笔记内容\n\n");
        out
    }
}

/// User-entered titles are literal text, never global templates or truncated names.
pub fn manual_title(value: &str) -> Result<String, String> {
    let title = value.trim();
    if title.is_empty() { return Err("请填写本篇文献的笔记标题。".into()) }
    if title.chars().any(char::is_control) { return Err("文献笔记标题请使用单行文字。".into()) }
    Ok(title.to_string())
}

fn line(text: &str) -> String { text.split_whitespace().collect::<Vec<_>>().join(" ") }

/// The library keeps bibliographic "Family, Given" names. Notes use natural
/// display order only when that delimiter is explicit; never infer full names.
fn authors_for_notes(authors: &str) -> String {
    authors.split(';').map(line).filter(|name| !name.is_empty()).map(|name| {
        if name.matches(',').count() == 1 && let Some((family, given)) = name.split_once(',') && !family.trim().is_empty() && !given.trim().is_empty() && !name.chars().any(|c| ('\u{3400}'..='\u{9fff}').contains(&c)) {
            format!("{} {}", given.trim(), family.trim())
        } else { name }
    }).collect::<Vec<_>>().join("; ")
}

pub fn valid_date(value: &str) -> bool {
    crate::model::valid_publication_date(value)
}

/// Accept Notion's app /p/ links, www links, and bare page IDs. Never fetch a pasted host.
pub fn page_id(value: &str) -> Result<String, String> {
    let value = value.trim();
    let path = if value.contains("://") {
        let rest = value.strip_prefix("https://").ok_or("请使用 HTTPS Notion 页面链接。")?;
        let (host, path) = rest.split_once('/').ok_or("Notion 页面链接缺少页面 ID。")?;
        if !matches!(host.to_ascii_lowercase().as_str(), "notion.so" | "www.notion.so" | "notion.com" | "www.notion.com" | "app.notion.com") { return Err("请填写 notion.so 或 notion.com 的页面链接。".into()) }
        path.split(['?', '#']).next().unwrap_or("").trim_end_matches('/').rsplit('/').next().unwrap_or("")
    } else { value };
    let compact: String = path.chars().filter(|c| *c != '-').collect();
    let tail = compact.get(compact.len().saturating_sub(32)..).unwrap_or("");
    if tail.len() != 32 || !tail.bytes().all(|b| b.is_ascii_hexdigit()) { return Err("无法从 Notion 页面链接中识别页面 ID。".into()) }
    let id = tail.to_ascii_lowercase();
    Ok(format!("{}-{}-{}-{}-{}", &id[..8], &id[8..12], &id[12..16], &id[16..20], &id[20..]))
}

pub fn fingerprint(text: &str) -> String { format!("{:x}", Sha256::digest(text.as_bytes())) }

#[derive(Clone, Debug)]
pub struct Picture { pub conversation: String, pub id: String, pub media_type: String, pub bytes: Vec<u8> }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group { pub id: String, pub title: String, pub created_at: String }

impl Group {
    pub fn heading(&self) -> String {
        let title = line(&self.title);
        let title = if title.is_empty() { "未命名对话" } else { &title };
        let date = chrono::DateTime::parse_from_rfc3339(&self.created_at).ok().map(|date| format!(" · {}", date.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M"))).unwrap_or_default();
        format!("## 对话：{title}{date}")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryState { New, Saved, Pending }

pub fn note_scope(account: &str, root: &str, library: &str, paper: &str) -> String {
    format!("{account}/root:{root}/library:{}/note:{library}/{paper}", fingerprint(library))
}

fn heading_text(heading: &str) -> String {
    crate::markdown::parse(heading).into_iter().find_map(|block| if let crate::markdown::Block::Heading(_, rich) = block { Some(rich.text) } else { None }).unwrap_or_else(|| heading.to_string())
}

fn fence_line(line: &str, fence: &mut Option<(char, usize)>) -> bool {
    let trimmed = line.trim_start();
    let first = trimmed.chars().next().unwrap_or(' ');
    let count = trimmed.chars().take_while(|c| *c == first).count();
    if !matches!(first, '`' | '~') || count < 3 { return false }
    match *fence { Some((mark, length)) if mark == first && count >= length && trimmed[count..].trim().is_empty() => *fence = None, None => *fence = Some((first, count)), _ => {} }
    true
}

/// Q headings are level three; body headings must stay below conversation
/// sections. Code examples retain their exact original heading characters.
fn nested_body(content: &str) -> String {
    let mut fence = None;
    let mut out = String::new();
    for line in content.split_inclusive('\n') {
        if !fence_line(line, &mut fence) && fence.is_none() && let Some(text) = line.strip_prefix("# ").or_else(|| line.strip_prefix("## ")) {
            out.push_str("### ");
            out.push_str(text);
        } else { out.push_str(line); }
    }
    out
}

pub fn section_headings(content: &str) -> Vec<String> {
    let mut fence: Option<(char, usize)> = None;
    let mut headings = vec![];
    for line in content.lines() {
        if fence_line(line, &mut fence) { continue }
        if fence.is_none() && (line.starts_with("## ") || line.starts_with("# ")) { headings.push(line.to_string()); }
    }
    headings
}

pub fn heading_exists(content: &str, heading: &str) -> bool {
    let text = heading_text(heading);
    section_headings(content).iter().any(|line| heading_text(line) == text)
}

/// Insert before the next top-level section, keeping every existing block intact.
/// None means this is the last section, so normal append is sufficient.
pub fn section_insert_before(content: &str, heading: &str) -> Result<Option<String>, String> {
    let headings = section_headings(content);
    let text = heading_text(heading);
    let positions = headings.iter().enumerate().filter(|(_, line)| heading_text(line) == text).map(|(i, _)| i).collect::<Vec<_>>();
    if positions.len() != 1 { return Err("无法唯一定位原对话分组，请检查 Notion 中的分组标题；已停止写入，避免重复或放错位置。".into()) }
    let next = headings.get(positions[0] + 1).cloned();
    if let Some(line) = &next && content.matches(line.as_str()).count() != 1 { return Err("Notion 分组边界重复，无法安全追加。请检查分组标题。".into()) }
    Ok(next)
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub asked_at: String,
    pub key: String,
    pub source_key: String,
    pub group: Option<Group>,
    pub label: String,
    pub original_question: String,
    pub question: String,
    pub answer: String,
    pub images: Vec<String>,
    /// User-edited Markdown. Empty uses the original question and answer.
    pub body: String,
}

/// One complete question, including answer pieces separated by tool calls.
pub fn entries(conversation: &str, transcript: &ai::Transcript, only: Option<usize>) -> Vec<Entry> {
    let mut entries = vec![];
    let mut question_index = 0;
    for (i, item) in transcript.items.iter().enumerate() {
        if let ai::Said::User { text, images, prompt, asked_at } = item {
            let selected = only.is_none_or(|n| n == question_index);
            question_index += 1;
            if !selected { continue }
            let answer = transcript.items[i + 1..].iter().take_while(|item| !matches!(item, ai::Said::User { .. })).filter_map(|item| if let ai::Said::Answer { text } = item { Some(text.as_str()) } else { None }).collect::<Vec<_>>().join("\n\n");
            if !answer.trim().is_empty() {
                // Keep supplied PDF quotations; do not export slash-command expansion instructions.
                let question = if !prompt.is_empty() && !text.trim_start().starts_with('/') { prompt.clone() } else { text.clone() };
                let key = fingerprint(&serde_json::json!([conversation, i, question, answer, images]).to_string());
                entries.push(Entry { asked_at: asked_at.clone(), source_key: key.clone(), key, group: Some(Group { id: conversation.into(), title: transcript.title(), created_at: String::new() }), label: if text.trim().is_empty() { "图片提问".into() } else { line(text) }, original_question: text.clone(), question, answer, images: images.clone(), body: String::new() });
            }
        }
    }
    entries
}

pub fn conversation_entries(conversation: &crate::store::Conversation, only: Option<usize>) -> Vec<Entry> {
    let mut result = entries(&conversation.id, &conversation.transcript, only);
    for entry in &mut result { if let Some(group) = &mut entry.group { group.created_at.clone_from(&conversation.created_at); } }
    result
}

/// Order indices, not entries: editors and image references keep stable identities.
pub fn insert_in_time_order(order: &mut Vec<usize>, entries: &[Entry], new: &Entry) {
    let time = |e: &Entry| crate::time::millis(if e.asked_at.is_empty() {
        e.group.as_ref().map_or("", |g| g.created_at.as_str())
    } else { &e.asked_at });
    let timestamp = time(new);
    let pos = order.iter().position(|&i| time(&entries[i]) > timestamp).unwrap_or(order.len());
    order.insert(pos, entries.len());
}

impl Entry {
    pub fn personal(paper_id: &str, notes: &str) -> Result<Self, String> {
        if notes.trim().is_empty() { return Err("请先在「我的笔记」中填写内容。".into()) }
        let key = fingerprint(&serde_json::json!(["personal-note", paper_id, notes.trim()]).to_string());
        Ok(Self { asked_at: String::new(), source_key: key.clone(), key, group: None, label: "我的笔记".into(), original_question: String::new(), question: "我的笔记".into(), answer: String::new(), images: vec![], body: format!("### 我的笔记\n\n{}", notes.trim()) })
    }
    /// Read-only compatibility with notes saved by the earlier preview implementation.
    pub fn marker(&self) -> String { format!("wisppaper-note-{}", self.key) }
    pub fn draft(&self) -> String {
        let mut out = format!("### Q：{}\n\n", self.label);
        // Keep quotations and multi-line question content below the question title.
        if self.question.trim() != self.label.trim() { out.push_str(&format!("{}\n\n", nested_body(&self.question))); }
        for id in &self.images { out.push_str(&format!("![原图](wisppaper-image://{id})\n\n")); }
        let answer = nested_body(&self.answer);
        let block = crate::markdown::parse(&answer).first().is_some_and(|block| !matches!(block, crate::markdown::Block::Paragraph(_)));
        out.push_str(&format!("A：{}{}\n\n", if block { "\n\n" } else { "" }, answer));
        out
    }
    pub fn edited(&self, body: &str) -> Result<Self, String> {
        let body = body.replace("\r\n", "\n").trim().to_string();
        let body = if self.group.is_some() { nested_body(&body) } else { body };
        if body.is_empty() { return Err("笔记内容不能为空；不需要的问答请返回上一步取消勾选。".into()) }
        let mut entry = self.clone();
        entry.images.retain(|id| body.contains(&format!("![原图](wisppaper-image://{id})")));
        let mut remaining = body.clone();
        for id in &entry.images { remaining = remaining.replace(&format!("![原图](wisppaper-image://{id})"), ""); }
        if remaining.contains("wisppaper-image://") { return Err("图片占位符无效；请保留完整的原图占位行，或删除整行以移除图片。".into()) }
        let original = if self.body.is_empty() { self.draft() } else { self.body.clone() };
        if body != original.trim() { entry.key = fingerprint(&serde_json::json!([self.key, body]).to_string()); }
        entry.body = body;
        Ok(entry)
    }
    pub fn markdown(&self, images: &std::collections::BTreeMap<String, String>) -> Result<String, String> {
        let mut out = if self.body.is_empty() { self.draft() } else { self.body.clone() };
        for id in &self.images { out = out.replace(&format!("![原图](wisppaper-image://{id})"), images.get(id).ok_or_else(|| format!("缺少图片附件：{id}"))?); }
        out.push_str("\n\n");
        Ok(out)
    }
}

#[derive(Clone, Debug)]
pub struct Page { pub id: String, pub url: String }

/// Global index, separate from credentials and settings. Each confirmed page is kept
/// before starting the next network call; uncertain creates are never blindly retried.
pub struct Index { db: Connection }

impl Index {
    pub fn open(path: &Path) -> Result<Self, String> {
        let db = Connection::open(path).map_err(|e| e.to_string())?;
        db.busy_timeout(std::time::Duration::from_secs(5)).map_err(|e| e.to_string())?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS pages (key TEXT PRIMARY KEY, id TEXT NOT NULL DEFAULT '', url TEXT NOT NULL DEFAULT ''); CREATE TABLE IF NOT EXISTS images (key TEXT PRIMARY KEY, markdown TEXT NOT NULL); CREATE TABLE IF NOT EXISTS pending_entries (key TEXT PRIMARY KEY); CREATE TABLE IF NOT EXISTS saved_entries (key TEXT PRIMARY KEY);").map_err(|e| e.to_string())?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS saved_sources (key TEXT PRIMARY KEY); CREATE TABLE IF NOT EXISTS conversation_groups (key TEXT PRIMARY KEY, heading TEXT NOT NULL);").map_err(|e| e.to_string())?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS pending_sources (key TEXT PRIMARY KEY, entry_key TEXT NOT NULL);").map_err(|e| e.to_string())?;
        Ok(Self { db })
    }
    pub fn page(&self, key: &str) -> Result<Option<Page>, String> {
        self.db.query_row("SELECT id,url FROM pages WHERE key=?1", [key], |r| Ok(Page { id: r.get(0)?, url: r.get(1)? })).optional().map_err(|e| e.to_string())
    }
    pub fn creating(&self, key: &str) -> Result<(), String> {
        self.db.execute("INSERT INTO pages(key) VALUES (?1)", [key]).map(|_| ()).map_err(|e| e.to_string())
    }
    pub fn keep(&self, key: &str, page: &Page) -> Result<(), String> {
        self.db.execute("INSERT INTO pages(key,id,url) VALUES (?1,?2,?3) ON CONFLICT(key) DO UPDATE SET id=excluded.id,url=excluded.url", params![key,page.id,page.url]).map(|_| ()).map_err(|e| e.to_string())
    }
    pub fn forget_uncreated(&self, key: &str) -> Result<(), String> {
        self.db.execute("DELETE FROM pages WHERE key=?1 AND id=''", [key]).map(|_| ()).map_err(|e| e.to_string())
    }
    pub fn forget_deleted(&self, key: &str, id: &str) -> Result<(), String> {
        self.db.execute("DELETE FROM pages WHERE key=?1 AND id=?2", params![key,id]).map(|_| ()).map_err(|e| e.to_string())
    }
    pub fn image(&self, key: &str) -> Result<Option<String>, String> {
        self.db.query_row("SELECT markdown FROM images WHERE key=?1", [key], |r| r.get(0)).optional().map_err(|e| e.to_string())
    }
    pub fn keep_image(&self, key: &str, markdown: &str) -> Result<(), String> {
        self.db.execute("INSERT OR REPLACE INTO images(key,markdown) VALUES (?1,?2)", params![key,markdown]).map(|_| ()).map_err(|e| e.to_string())
    }
    pub fn entry_pending(&self, key: &str) -> Result<bool, String> {
        self.db.query_row("SELECT EXISTS(SELECT 1 FROM pending_entries WHERE key=?1)", [key], |r| r.get(0)).map_err(|e| e.to_string())
    }
    pub fn begin_entry(&self, key: &str) -> Result<(), String> {
        self.begin_note(key, key)
    }
    pub fn confirm_entry(&self, key: &str) -> Result<(), String> {
        let tx = self.db.unchecked_transaction().map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM pending_entries WHERE key=?1", [key]).map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM pending_sources WHERE entry_key=?1", [key]).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }
    pub fn entry_saved(&self, key: &str) -> Result<bool, String> {
        self.db.query_row("SELECT EXISTS(SELECT 1 FROM saved_entries WHERE key=?1)", [key], |r| r.get(0)).map_err(|e| e.to_string())
    }
    pub fn finish_entry(&self, key: &str) -> Result<(), String> {
        self.finish_note(key, key)
    }
    pub fn begin_note(&self, key: &str, source: &str) -> Result<(), String> {
        let tx = self.db.unchecked_transaction().map_err(|e| e.to_string())?;
        tx.execute("INSERT INTO pending_entries(key) VALUES (?1)", [key]).map_err(|e| e.to_string())?;
        tx.execute("INSERT INTO pending_sources(key,entry_key) VALUES (?1,?2)", params![source,key]).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }
    pub fn finish_note(&self, key: &str, source: &str) -> Result<(), String> {
        let tx = self.db.unchecked_transaction().map_err(|e| e.to_string())?;
        tx.execute("INSERT OR IGNORE INTO saved_entries(key) VALUES (?1)", [key]).map_err(|e| e.to_string())?;
        tx.execute("INSERT OR IGNORE INTO saved_sources(key) VALUES (?1)", [source]).map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM pending_entries WHERE key=?1", [key]).map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM pending_sources WHERE entry_key=?1", [key]).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }
    pub fn source_pending(&self, source: &str) -> Result<bool, String> {
        self.db.query_row("SELECT EXISTS(SELECT 1 FROM pending_sources WHERE key=?1)", [source], |r| r.get(0)).map_err(|e| e.to_string())
    }
    pub fn group(&self, key: &str) -> Result<Option<String>, String> {
        self.db.query_row("SELECT heading FROM conversation_groups WHERE key=?1", [key], |r| r.get(0)).optional().map_err(|e| e.to_string())
    }
    pub fn keep_group(&self, key: &str, heading: &str) -> Result<(), String> {
        self.db.execute("INSERT INTO conversation_groups(key,heading) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET heading=excluded.heading", params![key,heading]).map(|_| ()).map_err(|e| e.to_string())
    }
    pub fn forget_group(&self, key: &str) -> Result<(), String> {
        self.db.execute("DELETE FROM conversation_groups WHERE key=?1", [key]).map(|_| ()).map_err(|e| e.to_string())
    }
    pub fn states(&self, account: &str, settings: &Settings, library: &LibraryInfo, paper: Option<&Paper>, entries: &[Entry]) -> Result<Vec<EntryState>, String> {
        let root = if settings.root_title.trim().is_empty() {
            if settings.parent_url.trim().is_empty() { PRIVATE_ROOT.into() } else { page_id(&settings.parent_url)? }
        } else {
            let root = self.page(&format!("{account}/{}", settings.destination()?))?;
            let Some(root) = root.filter(|page| !page.id.is_empty()) else { return Ok(vec![EntryState::New; entries.len()]) };
            root.id
        };
        let scope = note_scope(account, &root, &library.id, paper.map_or("", |p| p.id.as_str()));
        entries.iter().map(|entry| {
            let source = format!("{scope}/{}", entry.source_key);
            let pending = self.entry_pending(&source)? || self.source_pending(&source)?;
            let saved: bool = self.db.query_row("SELECT EXISTS(SELECT 1 FROM saved_sources WHERE key=?1 UNION ALL SELECT 1 FROM saved_entries WHERE key=?1)", [&source], |r| r.get(0)).map_err(|e| e.to_string())?;
            Ok(if pending { EntryState::Pending } else if saved { EntryState::Saved } else { EntryState::New })
        }).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chronological_order_interleaves_histories_and_preserves_legacy_turn_order() {
        let mut all = Vec::new();
        let mut order = Vec::new();
        for (id, asked) in [("A","12:00"), ("A","15:00"), ("B","13:00"), ("C",""), ("C","")] {
            let mut e = Entry::personal("P", "note").unwrap();
            e.asked_at = if asked.is_empty() { String::new() } else { format!("2026-11-14T{asked}:00Z") };
            e.group = Some(Group { id: id.into(), title: id.into(), created_at: "2026-11-14T14:00:00Z".into() });
            insert_in_time_order(&mut order, &all, &e);
            all.push(e);
        }
        assert_eq!(order, [0, 2, 3, 4, 1]);
    }
    #[test]
    fn incomplete_questions_do_not_shift_the_selected_answer() {
        let transcript = ai::Transcript { items: vec![ai::Said::User { asked_at: String::new(), text: "未完成问题".into(), images: vec![], prompt: String::new() }, ai::Said::User { asked_at: String::new(), text: "要保存的问题".into(), images: vec!["I2".into()], prompt: String::new() }, ai::Said::Answer { text: "对应回答".into() }], ..Default::default() };
        assert!(entries("C", &transcript, Some(0)).is_empty());
        let chosen = entries("C", &transcript, Some(1));
        assert_eq!(chosen[0].question, "要保存的问题");
        assert_eq!(chosen[0].images, ["I2"]);
        assert_eq!(chosen[0].key, entries("C", &transcript, None)[0].key);
    }
    #[test]
    fn answer_headings_do_not_split_conversation_groups_or_change_code_examples() {
        let transcript = ai::Transcript { items: vec![ai::Said::User { asked_at: String::new(), text: "解释标题".into(), images: vec![], prompt: String::new() }, ai::Said::Answer { text: "## 关键点\n\n说明\n\n```md\n# 示例标题\n```".into() }], ..Default::default() };
        let entry = entries("C", &transcript, None).remove(0);
        let draft = entry.draft();
        assert!(draft.starts_with("### Q：解释标题"));
        assert!(draft.contains("A：\n\n### 关键点"));
        assert!(draft.contains("```md\n# 示例标题\n```"));
        assert!(section_headings(&draft).is_empty());
        assert!(entry.edited(&draft).unwrap().body.contains("### 关键点"));
        assert!(entry.edited("### Q：新标题\n\n## 回答子标题").unwrap().body.contains("### 回答子标题"));
    }
    #[test]
    fn metadata_uses_configured_order_and_title_label() {
        let settings = Settings { fields: vec!["doi".into(), "title".into(), "authors".into()], ..Default::default() };
        let paper = Paper { title: "完整保留的文献题目".into(), doi: "10.1/example".into(), authors: "Smith, Jane".into(), ..Default::default() };
        let body = settings.header(&LibraryInfo::default(), Some(&paper));
        assert!(body.find("**DOI 链接**").unwrap() < body.find("**题目**").unwrap());
        assert!(body.find("**题目**").unwrap() < body.find("**作者**").unwrap());
        assert!(!body.contains("完整题目**"));
        let restored: Settings = serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored.fields, settings.fields);
    }
    #[test]
    fn title_uses_only_the_year_while_metadata_keeps_the_known_date() {
        let settings = Settings { title_parts: vec!["year".into(), "title".into()], fields: vec!["date".into()], ..Default::default() };
        assert!(!TITLE_PARTS.iter().any(|(key, _)| *key == "date"));
        assert!(!FIELDS.iter().any(|(key, _)| *key == "year"));
        for date in ["2020-02-29", "2020-02", "2020"] {
            let paper = Paper { title: "Paper".into(), publication_date: date.into(), ..Default::default() };
            assert_eq!(settings.paper_title(&paper), "2020 · Paper");
            let body = settings.header(&LibraryInfo::default(), Some(&paper));
            assert!(body.contains(&format!("**出版日期**：{date}")));
            assert!(!body.contains("**出版年份**"));
        }
    }
    #[test]
    fn legacy_date_choices_merge_without_losing_order_or_custom_titles() {
        let mut settings: Settings = serde_json::from_value(serde_json::json!({
            "rootTitle":"", "titleParts":["title","date","journal","year"],
            "fields":["year","title","date","doi"], "titleMode":"custom", "titleTemplate":"{date} · {title}"
        })).unwrap();
        settings.tidy();
        assert_eq!(settings.title_parts, ["title", "year", "journal"]);
        assert_eq!(settings.fields, ["date", "title", "doi"]);
        assert_eq!(settings.title_template, "{year} · {title}");
        assert!(settings.root_title.is_empty());
        assert!(settings.validate().is_ok());
        let previous = settings.clone();
        settings.tidy();
        assert_eq!(settings, previous);
        assert!(settings.custom_title());
    }
    #[test]
    fn conversation_boundaries_preserve_qa_and_ignore_fenced_examples() {
        let body = "## 对话：A\n\n### Q：第一题\n\nA：原回答\n\n````md\n## 对话：B\n```\n## 仍然是示例\n````\n\n## 对话：B\n\n### Q：第二题\n\nA：另一个对话";
        assert!(section_insert_before(body, "## 对话：A").is_err()); // B also occurs literally in a code example: no ambiguous replacement.
        let body = "## 对话：A\n\n### Q：第一题\n\nA：原回答\n\n## 对话：B\n\n### Q：第二题\n\nA：另一个对话";
        assert_eq!(section_insert_before(body, "## 对话：A").unwrap(), Some("## 对话：B".into()));
        assert_eq!(section_insert_before(body, "## 对话：B").unwrap(), None);
        assert!(section_insert_before(body, "## 已被手工删除的分组").is_err());
        assert!(section_insert_before("## 对话：A\n\n## 对话：A", "## 对话：A").is_err());
        assert!(heading_exists("## 对话：\\[P1\\] 分析", "## 对话：[P1] 分析"));
    }
    #[test]
    fn edited_versions_and_uncertain_writes_are_tracked_by_original_question() {
        let index = Index::open(Path::new(":memory:")).unwrap();
        let settings = Settings::default();
        let library = LibraryInfo { id: "L1".into(), ..Default::default() };
        let root = Page { id: "root".into(), url: "https://example.org/root".into() };
        index.keep(&format!("account/{}", settings.destination().unwrap()), &root).unwrap();
        let transcript = ai::Transcript { items: vec![ai::Said::User { asked_at: String::new(), text: "同样的问题".into(), images: vec![], prompt: String::new() }, ai::Said::Answer { text: "同样的回答".into() }], ..Default::default() };
        let a = entries("CA", &transcript, None).remove(0);
        let b = entries("CB", &transcript, None).remove(0);
        assert_ne!(a.key, b.key);
        let edited = a.edited("### Q：同样的问题\n\nA：编辑后的版本").unwrap();
        assert_eq!(a.source_key, edited.source_key);
        let scope = note_scope("account", "root", "L1", "");
        let key = format!("{scope}/{}", edited.key);
        let source = format!("{scope}/{}", edited.source_key);
        index.begin_note(&key, &source).unwrap();
        assert_eq!(index.states("account", &settings, &library, None, &[a.clone(), b.clone()]).unwrap(), [EntryState::Pending, EntryState::New]);
        index.finish_note(&key, &source).unwrap();
        assert_eq!(index.states("account", &settings, &library, None, &[a, b]).unwrap(), [EntryState::Saved, EntryState::New]);
        assert!(index.entry_saved(&key).unwrap());
    }
    #[test]
    fn pasted_app_links_and_hosts() {
        assert_eq!(page_id("https://app.notion.com/p/DLPFC-BA9-BA46-L1-L6-418b89d080ff8185925fe63b08fcd400?source=copy_link").unwrap(), "418b89d0-80ff-8185-925f-e63b08fcd400");
        assert!(page_id("https://notion.so.evil.test/418b89d080ff8185925fe63b08fcd400").is_err());
        assert!(page_id("https://notion.so/中文无编号").is_err());
    }
    #[test]
    fn metadata_never_invents_dates_and_titles_are_unicode_safe() {
        let settings = Settings::default();
        let paper = Paper { title: "中文题目".repeat(40), journal: "Nature".into(), year: Some(2020), ..Default::default() };
        assert_eq!(settings.paper_title(&paper).chars().count(), TITLE_LIMIT);
        let body = settings.header(&LibraryInfo::default(), Some(&paper));
        assert!(body.contains(&paper.title));
        assert!(!body.contains("出版日期"));
        assert!(!body.contains("2020-01-01"));
    }
    #[test]
    fn complete_turns_keep_images_and_ignore_tools() {
        use ai::Said::*;
        let transcript = ai::Transcript { items: vec![User { asked_at: String::new(), text: "图？".into(), images: vec!["I1".into()], prompt: String::new() }, Answer { text: "第一段".into() }, Tool { call: "read".into(), result: "private tool log".into(), failed: false }, Answer { text: "第二段".into() }], ..Default::default() };
        let all = entries("C1", &transcript, None);
        assert_eq!(all[0].answer, "第一段\n\n第二段");
        assert!(all[0].markdown(&Default::default()).is_err());
        assert_eq!(all[0].key, entries("C1", &transcript, Some(0))[0].key);
    }
    #[test]
    fn edited_notes_keep_only_selected_images_and_have_no_generated_footer() {
        let transcript = ai::Transcript { items: vec![ai::Said::User { asked_at: String::new(), text: "图？".into(), images: vec!["I1".into(), "I2".into()], prompt: String::new() }, ai::Said::Answer { text: "原始回答 [P1]".into() }], ..Default::default() };
        let entry = entries("C1", &transcript, None).remove(0);
        assert_eq!(entry.edited(&entry.draft()).unwrap().key, entry.key);
        let draft = entry.draft().replace("![原图](wisppaper-image://I2)\n\n", "").replace("原始回答", "整理后的回答");
        let edited = entry.edited(&draft).unwrap();
        assert_ne!(edited.key, entry.key);
        assert_eq!(edited.key, entry.edited(&draft).unwrap().key);
        assert_eq!(edited.images, ["I1"]);
        let markdown = edited.markdown(&std::collections::BTreeMap::from([("I1".into(), "![原图](https://example.org/original.png)".into())])).unwrap();
        assert!(markdown.contains("整理后的回答 [P1]"));
        assert!(!markdown.contains("原始回答") && !markdown.contains("I2"));
        assert!(!markdown.contains("来自 WispPaper") && !markdown.contains("wisppaper-note-") && !markdown.contains("引用文献"));
        assert!(entry.edited("![原图](wisppaper-image://unknown)").is_err());
        assert!(entry.edited(" ").is_err());
        assert_eq!(entry.answer, "原始回答 [P1]");
    }
    #[test]
    fn personal_notes_are_exported_without_question_answer_wrappers() {
        let entry = Entry::personal("P1", "我的归纳\n\n- 重要结论").unwrap();
        let body = entry.edited(&entry.body).unwrap().markdown(&Default::default()).unwrap();
        assert!(body.contains("### 我的笔记\n\n我的归纳\n\n- 重要结论"));
        assert!(!body.contains("### 提问") && !body.contains("AI 回答"));
        assert_eq!(entry.key, entry.edited(&entry.body).unwrap().key);
        assert_ne!(entry.key, Entry::personal("P1", "新归纳").unwrap().key);
        assert!(Entry::personal("P1", "  ").is_err());
    }
    #[test]
    fn authors_use_given_family_order_without_expanding_or_guessing_names() {
        assert_eq!(authors_for_notes("Vaswani, Ashish; Jones, Llion; Gomez, Aidan N.; Kaiser, Lukasz"), "Ashish Vaswani; Llion Jones; Aidan N. Gomez; Lukasz Kaiser");
        assert_eq!(authors_for_notes("Harris, CR; Ashish Vaswani; 张三; NumPy Team; Smith, John, Jr.; Marín-Sanz, María"), "CR Harris; Ashish Vaswani; 张三; NumPy Team; Smith, John, Jr.; María Marín-Sanz");
        let paper = Paper { authors: "Vaswani, Ashish".into(), ..Default::default() };
        let settings = Settings::default();
        assert!(settings.header(&LibraryInfo::default(), Some(&paper)).contains("**作者**：Ashish Vaswani"));
        assert_eq!(paper.authors, "Vaswani, Ashish");
    }
    #[test]
    fn durable_index_does_not_confuse_creating_with_confirmed() {
        let index = Index::open(Path::new(":memory:")).unwrap();
        index.creating("root/library/paper").unwrap();
        assert_eq!(index.page("root/library/paper").unwrap().unwrap().id, "");
        index.keep("root/library/paper", &Page { id: "remote".into(), url: "https://www.notion.so/remote".into() }).unwrap();
        assert_eq!(index.page("root/library/paper").unwrap().unwrap().id, "remote");
        assert!(index.page("different-root/library/paper").unwrap().is_none());
        index.begin_entry("entry").unwrap();
        assert!(index.entry_pending("entry").unwrap());
        index.confirm_entry("entry").unwrap();
        assert!(!index.entry_pending("entry").unwrap());
        assert!(!index.entry_saved("entry").unwrap());
        index.begin_entry("entry").unwrap();
        index.finish_entry("entry").unwrap();
        assert!(!index.entry_pending("entry").unwrap());
        assert!(index.entry_saved("entry").unwrap());
    }
    #[test]
    fn dates_keep_precision_and_settings_validate() {
        for date in ["", "2020", "2020-02", "2020-02-29"] { assert!(valid_date(date), "{date}"); }
        for date in ["2020-02-30", "2020-00", "2020-2-1"] { assert!(!valid_date(date), "{date}"); }
        let settings = Settings { title_parts: vec!["unknown".into()], ..Default::default() };
        assert!(settings.validate().is_err());
    }
    #[test]
    fn obsolete_hierarchy_settings_are_ignored_and_root_names_affect_destinations() {
        let settings: Settings = serde_json::from_value(serde_json::json!({"levels":["year","journal"],"parentUrl":"https://app.notion.com/p/418b89d080ff8185925fe63b08fcd400"})).unwrap();
        assert!(settings.validate().is_ok());
        assert!(serde_json::to_value(&settings).unwrap().get("levels").is_none());
        let changed = Settings { root_title: "其他根页面".into(), ..settings.clone() };
        assert_ne!(settings.destination().unwrap(), changed.destination().unwrap());
    }
    #[test]
    fn omitted_roots_keep_saved_and_pending_states_without_a_root_page_mapping() {
        let index = Index::open(Path::new(":memory:")).unwrap();
        let library = LibraryInfo { id: "L1".into(), ..Default::default() };
        let entry = Entry::personal("P1", "Note").unwrap();
        for parent in ["", "418b89d080ff80048fe0edb12f9fb033"] {
            let settings = Settings { root_title: "  ".into(), parent_url: parent.into(), ..Default::default() };
            assert!(settings.validate().is_ok());
            let restored: Settings = serde_json::from_value(serde_json::to_value(&settings).unwrap()).unwrap();
            assert_eq!(restored.root_title, "  ");
            assert_ne!(settings.destination().unwrap(), Settings { root_title: "WispPaper笔记".into(), ..settings.clone() }.destination().unwrap());
            let root = if parent.is_empty() { PRIVATE_ROOT.into() } else { page_id(parent).unwrap() };
            let scope = note_scope("account", &root, &library.id, "");
            let (key, source) = (format!("{scope}/{}", entry.key), format!("{scope}/{}", entry.source_key));
            assert_eq!(index.states("account", &settings, &library, None, std::slice::from_ref(&entry)).unwrap(), [EntryState::New]);
            index.begin_note(&key, &source).unwrap();
            assert_eq!(index.states("account", &settings, &library, None, std::slice::from_ref(&entry)).unwrap(), [EntryState::Pending]);
            index.finish_note(&key, &source).unwrap();
            assert_eq!(index.states("account", &settings, &library, None, std::slice::from_ref(&entry)).unwrap(), [EntryState::Saved]);
        }
    }
    #[test]
    fn note_titles_support_order_and_custom_mode_without_a_shared_template() {
        let paper = Paper { id: "P1".into(), title: "Full title {year}".into(), journal: "Nature".into(), year: Some(2020), ..Default::default() };
        let mut settings = Settings { title_parts: vec!["year".into(), "journal".into(), "title".into()], ..Default::default() };
        assert_eq!(settings.paper_title(&paper), "2020 · Nature · Full title {year}");
        settings.title_parts = vec!["title".into(), "doi".into()];
        assert_eq!(settings.paper_title(&paper), "Full title {year}");
        settings.title_mode = "custom".into();
        settings.title_template.clear();
        assert!(settings.validate().is_ok());
        assert!(settings.custom_title());
        let old: Settings = serde_json::from_value(serde_json::json!({"titleTemplate":"{title} - {year}","titleLimit":0})).unwrap();
        assert!(old.custom_title());
        assert!(old.validate().is_ok());
        assert_eq!(old.paper_title(&paper), "Nature · 2020 · Full title {year}");
        assert_eq!(Settings::default().root_title, "WispPaper笔记");
    }
    #[test]
    fn manually_entered_titles_are_literal_and_never_silently_truncated() {
        assert_eq!(manual_title("  我的笔记 {year}  ").unwrap(), "我的笔记 {year}");
        let long = "中文标题".repeat(40);
        assert_eq!(manual_title(&long).unwrap(), long);
        for invalid in ["", "  ", "标题\n另一行", "标题\t另一段"] { assert!(manual_title(invalid).is_err()); }
    }
}
