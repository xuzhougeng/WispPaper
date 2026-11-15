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
    check(name, topic, "", color)?;
    let id = new_id("L");
    let dir = root.join(&id);
    std::fs::create_dir_all(&dir).with_context(|| format!("无法创建文件夹 {}", dir.display()))?;
    let now = now_iso();
    let info = LibraryInfo { id, name: name.trim().into(), topic: topic.trim().into(), color: color.into(), created_at: now.clone(), opened_at: now, dir, ..Default::default() };
    save(&info)?;
    // Create the database now so a broken folder shows up here, not later.
    Store::open(&info.dir)?;
    Ok(info)
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
}
