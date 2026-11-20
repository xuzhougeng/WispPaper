//! Note drafts shared by the window and tools; originals stay in the library.

use crate::{ai::Said, libraries::LibraryInfo, store::Store};
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;
use rusqlite::OptionalExtension;

pub type Publish<'a> = &'a dyn Fn(&mut Store, &LibraryInfo, &Value) -> Result<Value, String>;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Selection {
    #[serde(default)]
    pub paper_id: String,
    #[serde(default)]
    pub include_notes: Option<bool>,
    #[serde(default)]
    pub conversations: Vec<Questions>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Questions {
    pub id: String,
    /// One-based; empty selects all questions.
    #[serde(default)]
    pub questions: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct Attachment {
    pub name: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Document {
    pub subject: String,
    pub title: String,
    pub markdown: String,
    pub attachments: Vec<Attachment>,
}

impl Document {
    pub fn summary(&self) -> Value {
        json!({"title": self.title, "markdown": self.markdown, "attachments": self.attachments.iter().map(|a| json!({"name": a.name, "mediaType": a.media_type, "bytes": a.bytes.len()})).collect::<Vec<_>>()})
    }

    pub fn fingerprint(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(self.title.as_bytes());
        digest.update([0]);
        digest.update(self.markdown.as_bytes());
        for a in self.used_attachments() {
            digest.update([0]);
            digest.update(a.name.as_bytes());
            digest.update(&a.bytes);
        }
        format!("{:x}", digest.finalize())
    }

    pub fn used_attachments(&self) -> impl Iterator<Item = &Attachment> {
        self.attachments.iter().filter(|a| self.markdown.contains(&format!("({})", a.name)))
    }

    pub fn validate(&self) -> Result<()> {
        if self.title.trim().is_empty() || self.markdown.trim().is_empty() { bail!("笔记标题和正文不能为空。") }
        if self.markdown.len() > 500_000 { bail!("笔记太长，请分批保存（每批最多 500 KB）。") }
        Ok(())
    }
}

fn line(value: &str) -> String { value.split_whitespace().collect::<Vec<_>>().join(" ") }

pub fn properties(extra: Option<(&str, &str)>) -> Value {
    let mut properties = json!({
        "paperId": {"type": "string", "description": "文献 ID；省略表示文献库级问答。"},
        "includeNotes": {"type": "boolean", "description": "是否包含「我的笔记」；文献默认 true。"},
        "conversations": {"type": "array", "items": {"type": "object", "properties": {
            "id": {"type": "string"}, "questions": {"type": "array", "items": {"type": "integer", "minimum": 1}, "description": "从 1 开始的问题序号，省略或空数组表示该对话全部问题。"}
        }, "required": ["id"], "additionalProperties": false}},
        "title": {"type": "string", "description": "可选的草稿标题。不改文献题目。"},
        "markdown": {"type": "string", "description": "可选的编辑后草稿正文；附件保留 prepare_note 给出的图片文件名。只影响本次保存，不改原笔记。"}
    });
    if let Some((key, description)) = extra { properties[key] = json!({"type": "string", "description": description}) }
    properties
}

/// Metadata is kept at its known precision; this feature introduces no new date.
pub fn prepare(store: &Store, info: &LibraryInfo, selection: &Selection) -> Result<Document> {
    let paper = if selection.paper_id.is_empty() { None } else { Some(store.get_paper(&selection.paper_id)?) };
    let mut body = format!("文献库：{}\n\n", line(&info.name));
    let title = if let Some(paper) = &paper {
        let title = [paper.journal.clone(), paper.year.map(|y| y.to_string()).unwrap_or_default(), paper.title.clone()].into_iter().filter(|s| !s.trim().is_empty()).map(|s| line(&s)).collect::<Vec<_>>().join(" · ");
        body = format!("题目：{}\n\n{body}", line(&paper.title));
        for (label, value) in [
            ("作者", paper.authors.clone()), ("期刊", paper.journal.clone()),
            ("年份", paper.year.map(|y| y.to_string()).unwrap_or_default()),
            ("DOI", if paper.doi.is_empty() { String::new() } else { format!("https://doi.org/{}", paper.doi) }),
            ("PMID", paper.pmid.clone()), ("网址", paper.url.clone()),
        ] {
            if !value.trim().is_empty() { body.push_str(&format!("{label}：{}\n\n", line(&value))) }
        }
        title
    } else { format!("{} · 文献库问答", line(&info.name)) };
    let mut attachments = vec![];
    let mut content = false;
    if selection.include_notes.unwrap_or(paper.is_some()) && let Some(paper) = &paper && !paper.notes.trim().is_empty() {
        body.push_str(&format!("## 我的笔记\n\n{}\n\n", paper.notes.trim()));
        content = true;
    }
    let mut seen = std::collections::HashSet::new();
    for chosen in &selection.conversations {
        if !seen.insert(&chosen.id) { bail!("同一对话只能选择一次。") }
        let conversation = store.conversation(&chosen.id)?.ok_or_else(|| anyhow!("对话不存在：{}", chosen.id))?;
        if conversation.paper_id != selection.paper_id { bail!("所选对话不属于这篇文献或文献库。") }
        let count = conversation.transcript.questions();
        if chosen.questions.iter().any(|&n| n == 0 || n > count) { bail!("问题序号超出了对话范围。") }
        body.push_str(&format!("## 对话：{} · {}\n\n", line(&conversation.transcript.title()), conversation.created_at));
        let mut question = 0;
        let mut include = false;
        for item in &conversation.transcript.items {
            match item {
                Said::User { text, images, .. } => {
                    question += 1;
                    include = chosen.questions.is_empty() || chosen.questions.contains(&question);
                    if !include { continue }
                    content = true;
                    let heading = line(text).chars().take(100).collect::<String>();
                    body.push_str(&format!("### Q：{}\n\n", if heading.is_empty() { "图片提问" } else { &heading }));
                    if text.trim() != heading { body.push_str(&format!("{}\n\n", nested_headings(text))) }
                    for id in images {
                        let (media_type, bytes) = store.conversation_image(&conversation.id, id)?.ok_or_else(|| anyhow!("对话图片缺失：{id}，没有导出不完整的笔记。"))?;
                        let extension = match media_type.as_str() {
                            "image/png" => "png", "image/jpeg" => "jpg", "image/gif" => "gif", "image/webp" => "webp",
                            _ => bail!("不支持的笔记图片格式：{media_type}"),
                        };
                        let hash = format!("{:x}", Sha256::digest(&bytes));
                        let name = format!("image-{}.{}", &hash[..24], extension);
                        body.push_str(&format!("![提问图片]({name})\n\n"));
                        if !attachments.iter().any(|a: &Attachment| a.name == name) { attachments.push(Attachment { name, media_type, bytes }) }
                    }
                }
                Said::Answer { text } if include => body.push_str(&format!("{}\n\n", nested_headings(text))),
                _ => {}
            }
        }
    }
    if !content { bail!("没有要保存的笔记或问答，请先填写笔记或选择对话。") }
    Ok(Document { subject: selection.paper_id.clone(), title: title.chars().take(180).collect(), markdown: body.trim().to_string(), attachments })
}

/// Answers cannot break out of their question's heading hierarchy.
fn nested_headings(text: &str) -> String {
    let mut fence: Option<char> = None;
    text.lines().map(|line| {
        let trimmed = line.trim_start();
        if trimmed.starts_with(&"\u{60}".repeat(3)) || trimmed.starts_with("~~~") {
            let marker = trimmed.chars().next().unwrap();
            if fence == Some(marker) { fence = None } else if fence.is_none() { fence = Some(marker) }
            return line.to_string();
        }
        if fence.is_none() {
            let hashes = trimmed.chars().take_while(|&c| c == '#').count();
            if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') { return format!("{}{}", "#".repeat((hashes + 3).min(6)), &trimmed[hashes..]) }
            if !trimmed.is_empty() && trimmed.chars().all(|c| c == '=') { return String::new() }
        }
        line.to_string()
    }).collect::<Vec<_>>().join("\n")
}

pub fn from_args(store: &Store, info: &LibraryInfo, args: &Value) -> Result<Document> {
    let selection: Selection = serde_json::from_value(json!({
        "paperId": args["paperId"].as_str().unwrap_or(""),
        "includeNotes": args["includeNotes"],
        "conversations": args.get("conversations").cloned().unwrap_or_else(|| json!([])),
    }))?;
    let mut doc = prepare(store, info, &selection)?;
    if let Some(title) = args["title"].as_str() { doc.title = title.trim().chars().take(180).collect() }
    if let Some(markdown) = args["markdown"].as_str() { doc.markdown = markdown.trim().to_string() }
    doc.validate()?;
    Ok(doc)
}

pub fn sources(store: &Store, paper: &str) -> Result<Value> {
    if !paper.is_empty() { store.get_paper(paper)?; }
    let conversations = store.list_conversations(paper)?.into_iter().map(|info| {
        let conversation = store.conversation(&info.id)?.ok_or_else(|| anyhow!("对话不存在"))?;
        let questions: Vec<_> = conversation.transcript.items.iter().filter_map(|said| match said { Said::User { text, images, .. } => Some((text, images.len())), _ => None }).enumerate().map(|(i, (text, images))| json!({"number": i + 1, "text": text, "images": images})).collect();
        Ok(json!({"id": info.id, "title": info.title, "questions": questions}))
    }).collect::<Result<Vec<_>>>()?;
    Ok(json!({"paperId": paper, "conversations": conversations, "saved": store.note_saves(paper)?}))
}

/// Create a new Markdown file and a sibling attachment directory. Never overwrite.
pub fn export(doc: &Document, path: &Path) -> Result<Value> {
    doc.validate()?;
    if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("md")) { bail!("请选择 .md 文件路径。") }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() { bail!("导出目录不存在：{}", parent.display()) }
    let assets_name = format!("note-assets-{}", crate::model::new_id("A"));
    let assets = parent.join(&assets_name);
    let used: Vec<_> = doc.used_attachments().collect();
    let mut markdown = doc.markdown.clone();
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path).with_context(|| format!("无法创建 {}（已有文件不会覆盖）", path.display()))?;
    let mut created_assets = false;
    let result = (|| -> Result<()> {
        if !used.is_empty() {
            std::fs::create_dir(&assets).with_context(|| format!("无法创建附件目录：{}", assets.display()))?;
            created_assets = true;
            for attachment in &used {
                std::fs::write(assets.join(&attachment.name), &attachment.bytes)?;
                markdown = markdown.replace(&format!("({})", attachment.name), &format!("({assets_name}/{})", attachment.name));
            }
        }
        write!(file, "# {}\n\n{}\n", doc.title, markdown)?;
        file.sync_all()?;
        Ok(())
    })();
    drop(file);
    if let Err(error) = result {
        let _ = std::fs::remove_file(path);
        if created_assets {
            for a in &used { let _ = std::fs::remove_file(assets.join(&a.name)); }
            let _ = std::fs::remove_dir(&assets);
        }
        return Err(error);
    }
    Ok(json!({"path": path, "attachments": used.len()}))
}

pub fn page_id(input: &str) -> Result<String> {
    let input = input.trim();
    let tail = if input.starts_with("https://") {
        let rest = input.trim_start_matches("https://");
        let (host, path) = rest.split_once('/').ok_or_else(|| anyhow!("Notion 页面链接不完整。"))?;
        if !["notion.so", "www.notion.so", "app.notion.com", "notion.com", "www.notion.com"].contains(&host) && !host.ends_with(".notion.site") { bail!("请输入 Notion 页面链接。") }
        path.split(['?', '#']).next().unwrap_or("").rsplit('/').next().unwrap_or("")
    } else { input };
    let compact = tail.replace('-', "");
    if !input.starts_with("https://") && compact.len() != 32 { bail!("Notion 页面 ID 应为 32 位十六进制字符。") }
    let id = compact.get(compact.len().saturating_sub(32)..).unwrap_or("");
    if id.len() != 32 || !id.chars().all(|c| c.is_ascii_hexdigit()) { bail!("Notion 页面 ID 应为 32 位十六进制字符。") }
    Ok(id.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ai::Transcript, model::Paper, store::Conversation};

    struct Fixture { root: std::path::PathBuf, info: LibraryInfo, store: Store, paper: Paper, conversation: Conversation }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(crate::model::new_id("note-test"));
            let info = crate::libraries::create(&root, "笔记库", "", "blue").unwrap();
            let mut store = Store::open(&info.dir).unwrap();
            let mut paper = Paper { id: crate::model::new_id("P"), title: "A paper".into(), year: Some(2020), notes: "原始笔记".into(), ..Default::default() };
            store.save_paper(&mut paper).unwrap();
            let mut conversation = Conversation { paper_id: paper.id.clone(), transcript: Transcript { items: vec![
                Said::User { text: "第一问\n\n> 引文".into(), images: vec!["I1".into()], files: vec![], prompt: String::new() },
                Said::Answer { text: "# Answer\n\n[reference](https://example.org)\n\n~~~\n# code\n~~~".into() },
                Said::User { text: "第二问".into(), images: vec!["missing".into()], files: vec![], prompt: String::new() },
                Said::Answer { text: "不应出现在只选第一问的笔记".into() },
            ], ..Default::default() }, ..Default::default() };
            store.save_conversation(&mut conversation).unwrap();
            store.save_conversation_image(&conversation.id, "I1", "image/png", &[1,2,3]).unwrap();
            Self { root, info, store, paper, conversation }
        }
        fn selection(&self) -> Selection {
            Selection { paper_id: self.paper.id.clone(), include_notes: Some(true), conversations: vec![Questions { id: self.conversation.id.clone(), questions: vec![1] }] }
        }
        fn clean(self) { drop(self.store); std::fs::remove_dir_all(self.root).unwrap(); }
    }

    #[test]
    fn a_draft_keeps_selected_questions_images_quotes_and_known_metadata() {
        let f = Fixture::new();
        let doc = prepare(&f.store, &f.info, &f.selection()).unwrap();
        assert!(doc.title.contains("2020"));
        assert!(doc.markdown.contains("年份：2020") && !doc.markdown.contains("2020-01"));
        assert!(doc.markdown.contains("> 引文") && doc.markdown.contains("#### Answer"));
        assert!(doc.markdown.contains("[reference](https://example.org)") && doc.markdown.contains("\n# code\n"));
        assert!(!doc.markdown.contains("第二问") && !doc.markdown.contains("引用文献"));
        assert_eq!(doc.attachments.len(), 1);
        let mut bad = f.selection();
        bad.conversations[0].questions = vec![2];
        assert!(prepare(&f.store, &f.info, &bad).unwrap_err().to_string().contains("图片缺失"));
        bad.paper_id.clear();
        assert!(prepare(&f.store, &f.info, &bad).is_err());
        f.clean();
    }

    #[test]
    fn editing_and_exporting_a_draft_never_changes_originals_or_overwrites() {
        let f = Fixture::new();
        let args = json!({"paperId": f.paper.id, "conversations": [{"id": f.conversation.id, "questions": [1]}], "title": "Edited title"});
        let mut doc = from_args(&f.store, &f.info, &args).unwrap();
        let path = f.info.dir.join("note.md");
        let result = export(&doc, &path).unwrap();
        assert_eq!(result["attachments"], 1);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Edited title") && text.contains("note-assets-"));
        let asset_dir = std::fs::read_dir(&f.info.dir).unwrap().flatten().find(|e| e.file_name().to_string_lossy().starts_with("note-assets-")).unwrap().path();
        assert_eq!(std::fs::read(asset_dir.join(&doc.attachments[0].name)).unwrap(), [1, 2, 3]);
        assert!(export(&doc, &path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        doc.markdown = "图片和原笔记从这次导出中移除".into();
        assert_eq!(export(&doc, &f.info.dir.join("without-images.md")).unwrap()["attachments"], 0);
        assert_eq!(f.store.get_paper(&f.paper.id).unwrap().notes, f.paper.notes);
        assert_eq!(f.store.conversation(&f.conversation.id).unwrap().unwrap().transcript, f.conversation.transcript);
        f.clean();
    }

    #[test]
    fn reservations_survive_reopening_and_deduplicate_and_append_only_when_confirmed() {
        let mut f = Fixture::new();
        let mut doc = prepare(&f.store, &f.info, &f.selection()).unwrap();
        let parent = "11111111222243338444555555555555";
        let page = "aaaaaaaa222243338444555555555555";
        let pending = f.store.reserve_note(&doc, parent).unwrap();
        let mut second = Store::open(&f.info.dir).unwrap();
        assert!(second.reserve_note(&doc, parent).is_err());
        second.resolve_note(&pending.id, Some(page)).unwrap();
        let duplicate = f.store.reserve_note(&doc, parent).unwrap();
        assert_eq!(duplicate.status, "saved");
        doc.markdown.push_str("\n追加");
        let next = f.store.reserve_note(&doc, parent).unwrap();
        assert_eq!(next.page, page);
        assert!(second.reserve_note(&doc, parent).is_err());
        second.resolve_note(&next.id, None).unwrap();
        assert!(f.store.reserve_note(&doc, parent).is_ok());
        drop(second);
        f.clean();
    }

    #[test]
    fn page_links_are_validated_before_use() {
        let id = "11111111222243338444555555555555";
        assert_eq!(page_id(&format!("https://app.notion.com/p/AI-{id}?pvs=12")).unwrap(), id);
        assert_eq!(page_id("11111111-2222-4333-8444-555555555555").unwrap(), id);
        for bad in ["", "some page", "javascript:alert(1)", &format!("https://notion.so.evil.invalid/{id}"), &format!("prefix{id}")] { assert!(page_id(bad).is_err(), "{bad}"); }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Saved {
    pub id: String,
    pub subject: String,
    pub parent: String,
    pub fingerprint: String,
    pub page: String,
    pub status: String,
}

impl Saved {
    pub fn url(&self) -> String { format!("https://www.notion.so/{}", if self.page.is_empty() { &self.parent } else { &self.page }) }
}

impl Store {
    /// An OS lock is released on process exit, so crash recovery stays possible.
    /// It prevents another window/process from reconciling a write still in flight.
    pub fn lock_notes(&self) -> Result<std::fs::File> {
        let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(self.root().join(".note-publish.lock"))?;
        file.try_lock().map_err(|_| anyhow!("这个文献库仍有笔记正在保存，请等它完成后再操作。"))?;
        Ok(file)
    }

    pub fn note_links(&self) -> Result<std::collections::BTreeMap<String, String>> {
        let mut query = self.db.prepare("SELECT subject, page FROM note_saves WHERE status = 'saved' ORDER BY rowid")?;
        Ok(query.query_map([], |r| Ok((r.get::<_, String>(0)?, format!("https://www.notion.so/{}", r.get::<_, String>(1)?))))?.collect::<rusqlite::Result<_>>()?)
    }

    pub fn note_saves(&self, subject: &str) -> Result<Vec<Saved>> {
        let mut query = self.db.prepare("SELECT id, subject, parent, fingerprint, page, status FROM note_saves WHERE subject = ?1 ORDER BY rowid DESC")?;
        Ok(query.query_map([subject], |r| Ok(Saved { id: r.get(0)?, subject: r.get(1)?, parent: r.get(2)?, fingerprint: r.get(3)?, page: r.get(4)?, status: r.get(5)? }))?.collect::<rusqlite::Result<_>>()?)
    }

    /// A committed reservation survives crashes and serializes writers across processes.
    pub fn reserve_note(&mut self, doc: &Document, parent: &str) -> Result<Saved> {
        let tx = self.db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let pending: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM note_saves WHERE subject = ?1 AND parent = ?2 AND status = 'pending')", rusqlite::params![doc.subject, parent], |r| r.get(0))?;
        if pending { bail!("上次写入的结果尚未确认。请先在 Notion 核对，再处理待确认记录；不会自动重复写入。") }
        let mut saved = Saved { id: crate::model::new_id("N"), subject: doc.subject.clone(), parent: parent.to_string(), fingerprint: doc.fingerprint(), page: String::new(), status: "pending".into() };
        let last: Option<String> = tx.query_row("SELECT page FROM note_saves WHERE subject = ?1 AND parent = ?2 AND status = 'saved' ORDER BY rowid DESC LIMIT 1", rusqlite::params![doc.subject, parent], |r| r.get(0)).optional()?;
        saved.page = last.unwrap_or_default();
        let duplicate: Option<Saved> = tx.query_row("SELECT id, subject, parent, fingerprint, page, status FROM note_saves WHERE subject = ?1 AND parent = ?2 AND fingerprint = ?3 AND status = 'saved'", rusqlite::params![doc.subject, parent, saved.fingerprint], |r| Ok(Saved { id: r.get(0)?, subject: r.get(1)?, parent: r.get(2)?, fingerprint: r.get(3)?, page: r.get(4)?, status: r.get(5)? })).optional()?;
        if let Some(saved) = duplicate { return Ok(saved) }
        tx.execute("INSERT INTO note_saves(id, subject, parent, fingerprint, page, status) VALUES (?1, ?2, ?3, ?4, ?5, 'pending')", rusqlite::params![saved.id, saved.subject, saved.parent, saved.fingerprint, saved.page])?;
        tx.commit()?;
        Ok(saved)
    }

    /// Clear only a confirmed result or the person's explicit reconciliation.
    pub fn resolve_note(&mut self, id: &str, page: Option<&str>) -> Result<()> {
        let _guard = self.lock_notes()?;
        self.finish_note(id, page)
    }

    /// The publisher already holds the OS lock when recording its result.
    pub fn finish_note(&mut self, id: &str, page: Option<&str>) -> Result<()> {
        let changed = if let Some(page) = page {
            let page = page_id(page)?;
            self.db.execute("UPDATE note_saves SET status = 'saved', page = ?2 WHERE id = ?1 AND status = 'pending'", rusqlite::params![id, page])?
        } else {
            self.db.execute("DELETE FROM note_saves WHERE id = ?1 AND status = 'pending'", [id])?
        };
        if changed != 1 { bail!("待确认记录不存在或已处理。") }
        Ok(())
    }
}
