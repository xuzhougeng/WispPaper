//! The libraries under the data folder. Each library is its own folder with
//! a `library.json` (name, topic, colour), a database and its attachments;
//! nothing is shared between two libraries, so a folder can be copied,
//! backed up or handed to someone else on its own.

use crate::model::new_id;
use crate::store::{DB_FILE, Store};
use crate::time::now_iso;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const INFO_FILE: &str = "library.json";
/// The accent colours a library can take; the app maps the key to a colour.
pub const COLORS: [&str; 7] = ["blue", "green", "amber", "rose", "violet", "teal", "slate"];

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LibraryInfo {
    pub id: String,
    pub name: String,
    /// What this library is about. Shown on its card and given to the
    /// assistant as background when questions are asked.
    pub topic: String,
    pub color: String,
    /// What a paper must be about to be included, in the owner's words: the
    /// assistant screens against it. Empty means the topic alone.
    pub criteria: String,
    /// Run the enabled saved searches on their own every this many hours; 0 is never.
    pub auto_sync_hours: u32,
    /// The paper list's last order (`Filters::sort`), so the library opens as it was left.
    pub sort: String,
    pub created_at: String,
    /// Last time the library was opened: the home page lists the latest first.
    pub opened_at: String,
    /// A conversation started from the home page that has not been made a library yet: the home page lists it
    /// under 最近对话, not among the libraries, and the tools of `wisp` and MCP see it only when it is named by
    /// its id. It is a library in every other way (its own folder, database, papers and manuscripts), so making
    /// it one (`promote`) only names it and clears this. Not written while false: a library's file stays as before.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub draft: bool,
    #[serde(skip)]
    pub dir: PathBuf,
    #[serde(skip)]
    pub paper_count: usize,
}

fn check(name: &str, topic: &str, criteria: &str, color: &str) -> Result<()> {
    if name.trim().is_empty() { bail!("请填写文献库名称。") }
    if name.chars().count() > 60 { bail!("文献库名称不能超过 60 个字符。") }
    if topic.chars().count() > 2000 { bail!("主题描述不能超过 2000 个字符。") }
    if criteria.chars().count() > 4000 { bail!("筛选标准不能超过 4000 个字符。") }
    if !COLORS.contains(&color) { bail!("不支持的颜色：{color}") }
    Ok(())
}

/// Every library under `root`, most recently opened first. A folder without
/// a readable `library.json` is not a library and is left alone.
pub fn list(root: &Path) -> Vec<LibraryInfo> {
    let mut found = vec![];
    let Ok(entries) = std::fs::read_dir(root) else { return found };
    for entry in entries.flatten() {
        let dir = entry.path();
        let Ok(text) = std::fs::read_to_string(dir.join(INFO_FILE)) else { continue };
        let Ok(mut info) = serde_json::from_str::<LibraryInfo>(&text) else { continue };
        if info.id.is_empty() || info.name.is_empty() { continue }
        info.paper_count = Store::count_papers(&dir.join(DB_FILE));
        info.dir = dir;
        found.push(info);
    }
    found.sort_by(|a, b| b.opened_at.cmp(&a.opened_at).then_with(|| b.created_at.cmp(&a.created_at)));
    found
}

pub fn create(root: &Path, name: &str, topic: &str, color: &str) -> Result<LibraryInfo> {
    make(root, name, topic, color, false)
}

fn make(root: &Path, name: &str, topic: &str, color: &str, draft: bool) -> Result<LibraryInfo> {
    check(name, topic, "", color)?;
    let id = new_id("L");
    let dir = root.join(&id);
    std::fs::create_dir_all(&dir).with_context(|| format!("无法创建文件夹 {}", dir.display()))?;
    let now = now_iso();
    let info = LibraryInfo { id, name: name.trim().into(), topic: topic.trim().into(), color: color.into(), created_at: now.clone(), opened_at: now, draft, dir, ..Default::default() };
    save(&info)?;
    // Create the database now so a broken folder shows up here, not later.
    Store::open(&info.dir)?;
    Ok(info)
}

/// The libraries on the shelf: every one under `root` but the conversations not yet made libraries.
pub fn shelved(root: &Path) -> Vec<LibraryInfo> {
    list(root).into_iter().filter(|info| !info.draft).collect()
}

/// How long the name a conversation gets from its first question is, in characters.
pub const DRAFT_TITLE_CHARS: usize = 24;

/// What a conversation started from the home page is called until it is named: its first question on one
/// line, cut on a whole character with 「…」. A question with no words (only a file) is named after the file.
pub fn draft_title(question: &str) -> String {
    let line = question.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= DRAFT_TITLE_CHARS { return line }
    format!("{}…", line.chars().take(DRAFT_TITLE_CHARS - 1).collect::<String>().trim_end())
}

/// A conversation started from the home page: a library folder like any other, marked `draft`, named after
/// `about` (the first question, or the file it came with). Made only when that first question is sent.
pub fn create_draft(root: &Path, about: &str) -> Result<LibraryInfo> {
    let name = draft_title(about);
    make(root, if name.is_empty() { "新对话" } else { &name }, "", "blue", true)
}

/// 建成文献库: the conversation becomes a library on the shelf under this name, topic and colour. Nothing
/// is copied or moved: its papers, conversations and manuscripts are the library's from now on.
pub fn promote(info: &mut LibraryInfo, name: &str, topic: &str, color: &str) -> Result<()> {
    check(name, topic, &info.criteria, color)?;
    let mut made = LibraryInfo { name: name.trim().into(), topic: topic.trim().into(), color: color.into(), draft: false, ..info.clone() };
    made.dir = info.dir.clone();
    save(&made)?;
    *info = made;
    Ok(())
}

/// What a model is asked for a library's name and topic, from the conversation it grows out of (`said`: the
/// questions and answers as text, the latest kept when it is long).
pub fn proposal_prompt(said: &str) -> (String, String) {
    const MOST: usize = 12_000;
    let said = said.trim();
    let said = if said.len() > MOST { &said[said.floor_char_boundary(said.len() - MOST)..] } else { said };
    let system = "你帮用户给一个文献库起名字、写主题描述。文献库是从下面这段对话发展来的：用户和助手围绕一个研究想法或一份稿件聊过，找过文献。\n\
要求：\n\
1. name：文献库的名称，像一个研究主题，简短具体，不超过 20 个字，不加引号和书名号，用对话所用的语言。\n\
2. topic：主题描述，一两句话，说这个文献库收集什么、关注哪些问题，会作为以后向助手提问的背景。不超过 200 字。\n\
3. 只输出一个 JSON 对象，不要其他文字、不要代码块标记：{\"name\": \"…\", \"topic\": \"…\"}";
    (system.to_string(), format!("对话：\n{said}"))
}

/// The name and topic in a model's answer to `proposal_prompt`, made to fit what a library takes.
pub fn parse_proposal(reply: &str) -> Result<(String, String), String> {
    let wrong = "模型的回答不是要求的格式。";
    let (start, end) = reply.find('{').zip(reply.rfind('}')).filter(|(start, end)| start < end).ok_or(wrong)?;
    let value: serde_json::Value = serde_json::from_str(&reply[start..=end]).map_err(|_| wrong)?;
    let field = |key: &str| value[key].as_str().unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ").trim_matches(|c| matches!(c, '"' | '「' | '」' | '《' | '》')).to_string();
    let (name, topic) = (field("name"), field("topic"));
    if name.is_empty() { return Err(wrong.into()) }
    Ok((name.chars().take(60).collect(), topic.chars().take(2000).collect()))
}

/// Write `library.json`; the temporary file keeps a crash from leaving half of one.
pub fn save(info: &LibraryInfo) -> Result<()> {
    check(&info.name, &info.topic, &info.criteria, &info.color)?;
    let path = info.dir.join(INFO_FILE);
    let temporary = info.dir.join(format!("{INFO_FILE}.tmp"));
    std::fs::write(&temporary, serde_json::to_string_pretty(info)?).with_context(|| format!("无法写入 {}", temporary.display()))?;
    std::fs::rename(&temporary, &path).with_context(|| format!("无法写入 {}", path.display()))?;
    Ok(())
}

/// Take a library off the home page. Its folder moves to `removed/` under
/// the data folder, papers and files intact, and can be moved back by hand.
pub fn remove(root: &Path, info: &LibraryInfo) -> Result<PathBuf> {
    let removed = root.join("removed");
    std::fs::create_dir_all(&removed)?;
    let target = removed.join(format!("{}-{}", info.id, crate::time::now_millis()));
    std::fs::rename(&info.dir, &target).with_context(|| format!("无法移动 {}（是否有文件正被其他程序使用？）", info.dir.display()))?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Paper;

    #[test]
    fn libraries_are_independent() {
        let root = std::env::temp_dir().join(format!("wisp-libs-{}", new_id("T")));
        std::fs::create_dir_all(root.join("not-a-library")).unwrap();
        assert!(list(&root).is_empty());
        assert!(create(&root, " ", "", "blue").is_err());
        assert!(create(&root, "x", "", "pink").is_err());

        let roots = create(&root, "植物根单细胞", "根尖发育的单细胞转录组研究", "green").unwrap();
        let mut llm = create(&root, "LLM agents", "", "violet").unwrap();
        Store::open(&roots.dir).unwrap().add_paper(Paper { title: "Root atlas".into(), ..Default::default() }).unwrap();

        let listed = list(&root);
        assert_eq!(listed.len(), 2);
        let count = |name: &str| listed.iter().find(|l| l.name == name).unwrap().paper_count;
        assert_eq!((count("植物根单细胞"), count("LLM agents")), (1, 0));
        assert_eq!(Store::open(&llm.dir).unwrap().list_papers().unwrap().len(), 0);

        llm.name = "Agents".into();
        llm.opened_at = "9999".into();
        save(&llm).unwrap();
        assert_eq!(list(&root)[0].name, "Agents");

        let moved = remove(&root, &roots).unwrap();
        assert!(moved.join(DB_FILE).exists());
        assert_eq!(list(&root).len(), 1);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_conversation_is_a_library_off_the_shelf_until_it_is_made_one() {
        let root = std::env::temp_dir().join(format!("wisp-drafts-{}", new_id("T")));
        std::fs::create_dir_all(&root).unwrap();
        let shelf = create(&root, "根", "", "green").unwrap();
        let mut draft = create_draft(&root, "  帮我调研一下拟南芥根毛发育中\n乙烯和生长素的相互作用，最近五年有什么进展  ").unwrap();
        assert!(draft.draft && draft.dir.join(DB_FILE).exists());
        assert_eq!(draft.name, "帮我调研一下拟南芥根毛发育中 乙烯和生长素的相…");
        assert_eq!(draft.name.chars().count(), DRAFT_TITLE_CHARS);
        // Listed with the rest, and told apart by those who list.
        assert_eq!(list(&root).len(), 2);
        assert_eq!(shelved(&root).iter().map(|l| l.id.as_str()).collect::<Vec<_>>(), [shelf.id.as_str()]);
        // A library's file says nothing of drafts; a draft's says it is one, and reads back so.
        assert!(!std::fs::read_to_string(shelf.dir.join(INFO_FILE)).unwrap().contains("draft"));
        assert!(list(&root).iter().find(|l| l.id == draft.id).unwrap().draft);
        assert_eq!(create_draft(&root, "   ").unwrap().name, "新对话");
        assert_eq!(draft_title("short one"), "short one");

        Store::open(&draft.dir).unwrap().add_paper(Paper { title: "Ethylene and root hairs".into(), ..Default::default() }).unwrap();
        assert!(promote(&mut draft, " ", "", "blue").is_err() && draft.draft);
        promote(&mut draft, "根毛发育", "乙烯与生长素在根毛发育中的相互作用", "teal").unwrap();
        assert!(!draft.draft && draft.name == "根毛发育" && draft.color == "teal");
        let shelf_now = shelved(&root);
        let made = shelf_now.iter().find(|l| l.id == draft.id).unwrap();
        // The same folder, its papers with it.
        assert_eq!((made.dir.clone(), made.paper_count, made.topic.as_str()), (draft.dir.clone(), 1, "乙烯与生长素在根毛发育中的相互作用"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_name_and_topic_are_read_off_a_proposal() {
        let (system, message) = proposal_prompt(&format!("{}问：根毛", "旧".repeat(20_000)));
        assert!(system.contains("JSON") && message.ends_with("问：根毛") && message.len() < 13_000);
        assert_eq!(parse_proposal("好的：```json\n{\"name\": \"《根毛发育》\", \"topic\": \"乙烯与生长素\\n如何调控根毛。\"}\n```").unwrap(), ("根毛发育".to_string(), "乙烯与生长素 如何调控根毛。".to_string()));
        assert!(parse_proposal("{\"name\": \"\", \"topic\": \"x\"}").is_err());
        assert!(parse_proposal("根毛发育").is_err());
    }
}
