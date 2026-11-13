//! What an agent may change about papers the library already holds: their
//! tags, the star, the reading status, and a paragraph added to the notes.
//!
//! It is the tool `organise_papers`. A call names any number of papers and
//! is written as a whole or not at all. The screening decision and the
//! custom fields are not among them: those have their own tools, which only
//! propose. In the window the assistant runs the tool after the person has
//! seen, paper by paper, what it would change (`preview`).

use crate::model::{Paper, READINGS, label, one_of};
use crate::store::Store;
use crate::time;
use anyhow::{Result, anyhow, bail};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

/// The most papers one call may change.
pub const MAX_CHANGES: usize = 200;

/// What is asked for one paper. Everything left out stays as it is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Change {
    pub id: String,
    pub add_tags: Vec<String>,
    pub remove_tags: Vec<String>,
    pub starred: Option<bool>,
    /// A key of `READINGS`.
    pub reading: Option<String>,
    /// A paragraph for the end of the paper's notes.
    pub note: String,
}

/// A paper after a call, and what the call changed about it, in words.
#[derive(Clone, Debug, PartialEq)]
pub struct Organised {
    pub paper: Paper,
    pub changed: Vec<String>,
}

fn words(value: &Value) -> Vec<String> {
    let list: Vec<&str> = match value {
        Value::Array(items) => items.iter().filter_map(Value::as_str).collect(),
        // A model sometimes writes one tag, or several with commas, where a list is asked for.
        Value::String(text) => text.split([',', '，', '、', ';', '；']).collect(),
        _ => vec![],
    };
    let mut out: Vec<String> = vec![];
    for word in list.into_iter().map(str::trim).filter(|word| !word.is_empty()) {
        if !out.iter().any(|kept| kept.eq_ignore_ascii_case(word)) { out.push(word.to_string()) }
    }
    out
}

/// Read a call's `changes`: a list, or (from the command line) the same list written as JSON.
pub fn parse(args: &Value) -> Result<Vec<Change>, String> {
    let parsed;
    let list = match &args["changes"] {
        Value::Array(items) => items,
        Value::String(text) => {
            parsed = serde_json::from_str::<Value>(text).map_err(|_| "参数 changes 应为 JSON 数组。".to_string())?;
            parsed.as_array().ok_or("参数 changes 应为 JSON 数组。")?
        }
        Value::Null => return Err("organise_papers 需要参数 changes。".into()),
        _ => return Err("参数 changes 应为数组，每项是一篇文献的改动。".into()),
    };
    if list.is_empty() { return Err("changes 是空的：没有要改的文献。".into()) }
    if list.len() > MAX_CHANGES { return Err(format!("一次最多改 {MAX_CHANGES} 篇文献，这次是 {} 篇。分几次调用。", list.len())) }
    let mut changes = vec![];
    for item in list {
        let id = item["id"].as_str().or_else(|| item["paperId"].as_str()).map(str::trim).unwrap_or("");
        if id.is_empty() { return Err("changes 的每一项都要有 id（文献的 ID 或标签）。".into()) }
        if !item["screening"].is_null() { return Err("organise_papers 不能改收录判断：用 suggest_screening 提出建议，由用户确认。".into()) }
        let reading = match &item["reading"] {
            Value::Null => None,
            Value::String(key) if one_of(&READINGS, key) => Some(key.clone()),
            other => return Err(format!("不支持的阅读状态：{}。可用：{}。", other.as_str().unwrap_or("（不是文字）"), READINGS.map(|(key, _)| key).join("、"))),
        };
        let change = Change { id: id.to_string(), add_tags: words(&item["addTags"]), remove_tags: words(&item["removeTags"]), starred: item["starred"].as_bool(), reading, note: item["note"].as_str().unwrap_or("").trim().to_string() };
        if change == (Change { id: change.id.clone(), ..Default::default() }) { return Err(format!("文献 {id} 没有写要改什么：addTags、removeTags、starred、reading、note 至少给一项。")) }
        changes.push(change);
    }
    Ok(changes)
}

/// Make a change to a paper in memory and say what it changed. Nothing is
/// said, and nothing changed, where the paper already is as asked.
pub fn apply(paper: &mut Paper, change: &Change, date: &str) -> Vec<String> {
    let mut changed = vec![];
    let held = |tags: &[String], tag: &str| tags.iter().position(|kept| kept.eq_ignore_ascii_case(tag));
    let added: Vec<&String> = change.add_tags.iter().filter(|tag| held(&paper.tags, tag).is_none()).collect();
    if !added.is_empty() {
        changed.push(format!("加标签{}", added.iter().map(|tag| format!("「{tag}」")).collect::<String>()));
        paper.tags.extend(added.into_iter().cloned());
    }
    let mut removed = vec![];
    for tag in &change.remove_tags {
        if let Some(at) = held(&paper.tags, tag) { removed.push(paper.tags.remove(at)) }
    }
    if !removed.is_empty() { changed.push(format!("去掉标签{}", removed.iter().map(|tag| format!("「{tag}」")).collect::<String>())) }
    if let Some(starred) = change.starred.filter(|starred| *starred != paper.starred) {
        paper.starred = starred;
        changed.push(if starred { "加星标" } else { "取消星标" }.to_string());
    }
    if let Some(reading) = change.reading.as_ref().filter(|reading| **reading != paper.reading) {
        paper.reading = reading.clone();
        changed.push(format!("阅读状态改为「{}」", label(&READINGS, reading)));
    }
    if !change.note.is_empty() {
        // The notes are the person's own: what an assistant adds is set apart and says whose it is.
        let note = format!("{}\n（{date} 助手所记，未核对原文）", change.note);
        paper.notes = if paper.notes.trim().is_empty() { note } else { format!("{}\n\n{note}", paper.notes.trim_end()) };
        let opening: String = change.note.chars().take(NOTE_SHOWN).collect();
        changed.push(format!("笔记末尾追加：{}{}", opening.replace('\n', " "), if change.note.chars().count() > NOTE_SHOWN { "…" } else { "" }));
    }
    changed
}

/// How much of a note the words about a change show.
const NOTE_SHOWN: usize = 48;

impl Store {
    /// What the changes would make of their papers, without writing
    /// anything. A paper named twice gets its changes one after another.
    pub fn organise_plan(&self, changes: &[Change]) -> Result<Vec<Organised>> {
        let date = time::date_text(&time::now_iso());
        let mut out: Vec<Organised> = vec![];
        for change in changes {
            let at = match out.iter().position(|done| done.paper.id == change.id) {
                Some(at) => at,
                None => {
                    let paper = self.get_paper(&change.id).map_err(|_| anyhow!("没有找到这篇文献：{}", change.id))?;
                    let trashed: Option<i64> = self.db.query_row("SELECT 1 FROM trashed_papers WHERE paper_id = ?1", [&paper.id], |row| row.get(0)).optional()?;
                    if trashed.is_some() { bail!("「{}」在回收站里，不能整理。", paper.title) }
                    out.push(Organised { paper, changed: vec![] });
                    out.len() - 1
                }
            };
            let said = apply(&mut out[at].paper, change, &date);
            out[at].changed.extend(said);
        }
        Ok(out)
    }

    /// Write the changes: all of them or none. The papers come back as
    /// they are now, each with what was changed about it.
    pub fn organise(&mut self, changes: &[Change]) -> Result<Vec<Organised>> {
        let plan = self.organise_plan(changes)?;
        let now = time::now_iso();
        let tx = self.db.transaction()?;
        for done in plan.iter().filter(|done| !done.changed.is_empty()) {
            let paper = &done.paper;
            tx.execute("UPDATE papers SET tags_json = ?2, starred = ?3, reading_status = ?4, notes = ?5, updated_at = ?6 WHERE id = ?1", params![paper.id, serde_json::to_string(&paper.tags)?, paper.starred, paper.reading, paper.notes, now])?;
        }
        tx.commit()?;
        Ok(plan)
    }
}

/// The tool's answer.
pub fn report(done: &[Organised]) -> Value {
    let changed = done.iter().filter(|done| !done.changed.is_empty()).count();
    json!({
        "changed": changed,
        "unchanged": done.len() - changed,
        "papers": done.iter().map(|done| json!({"id": done.paper.id, "title": done.paper.title, "changed": done.changed, "tags": done.paper.tags, "starred": done.paper.starred, "reading": done.paper.reading})).collect::<Vec<_>>(),
    })
}

/// How much of a title a line of the preview shows.
const TITLE_SHOWN: usize = 44;

/// What a call would do, a line per paper, for the person to read before
/// it runs; or why it cannot run as written, in which case there is
/// nothing to ask the person about.
pub fn preview(store: &Store, args: &Value) -> Result<Vec<String>, String> {
    let plan = parse(args).and_then(|changes| store.organise_plan(&changes).map_err(|e| format!("{e:#}")))?;
    Ok(plan.iter()
        .map(|done| {
            let title: String = done.paper.title.chars().take(TITLE_SHOWN).collect();
            let title = format!("《{title}{}》", if done.paper.title.chars().count() > TITLE_SHOWN { "…" } else { "" });
            if done.changed.is_empty() { format!("{title}：已经是这样，不改") } else { format!("{title}：{}", done.changed.join("；")) }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library() -> (Store, String, String) {
        let mut store = Store::in_memory(std::path::Path::new("/tmp/wisp-organise-test")).unwrap();
        let a = store.add_paper(Paper { title: "Root atlas".into(), tags: vec!["scRNA".into()], notes: "我的想法。".into(), ..Default::default() }).unwrap();
        let b = store.add_paper(Paper { title: "Leaf atlas".into(), ..Default::default() }).unwrap();
        (store, a.id, b.id)
    }

    #[test]
    fn a_call_is_read_and_refused_for_what_it_lacks() {
        let changes = parse(&json!({"changes": [{"id": "a", "addTags": ["根", " 根 ", "叶"], "removeTags": "旧, 更旧", "starred": true, "reading": "read", "note": " 值得重读 "}]})).unwrap();
        assert_eq!(changes, vec![Change { id: "a".into(), add_tags: vec!["根".into(), "叶".into()], remove_tags: vec!["旧".into(), "更旧".into()], starred: Some(true), reading: Some("read".into()), note: "值得重读".into() }]);
        // The command line hands the list over as text.
        assert_eq!(parse(&json!({"changes": "[{\"id\":\"a\",\"starred\":false}]"})).unwrap()[0].starred, Some(false));
        assert!(parse(&json!({})).unwrap_err().contains("changes"));
        assert!(parse(&json!({"changes": []})).unwrap_err().contains("空"));
        assert!(parse(&json!({"changes": [{"id": "a"}]})).unwrap_err().contains("至少给一项"));
        assert!(parse(&json!({"changes": [{"addTags": ["x"]}]})).unwrap_err().contains("id"));
        assert!(parse(&json!({"changes": [{"id": "a", "reading": "done"}]})).unwrap_err().contains("阅读状态"));
        // The decision is the person's.
        assert!(parse(&json!({"changes": [{"id": "a", "screening": "included"}]})).unwrap_err().contains("suggest_screening"));
    }

    #[test]
    fn only_what_differs_is_changed_and_said() {
        let mut paper = Paper { title: "T".into(), tags: vec!["scRNA".into(), "根".into()], starred: true, reading: "unread".into(), notes: "我的想法。".into(), ..Default::default() };
        let change = Change { id: "a".into(), add_tags: vec!["scrna".into(), "综述".into()], remove_tags: vec!["根".into(), "没有的".into()], starred: Some(true), reading: Some("read".into()), note: "方法可借鉴".into() };
        let said = apply(&mut paper, &change, "2026-11-12");
        assert_eq!(said, ["加标签「综述」", "去掉标签「根」", "阅读状态改为「已精读」", "笔记末尾追加：方法可借鉴"]);
        assert_eq!(paper.tags, ["scRNA", "综述"]);
        assert_eq!(paper.notes, "我的想法。\n\n方法可借鉴\n（2026-11-12 助手所记，未核对原文）");
        // Asked again, there is nothing left to do but the note.
        let again = Change { note: String::new(), ..change };
        assert!(apply(&mut paper, &again, "2026-11-12").is_empty());
    }

    #[test]
    fn a_call_is_written_whole_or_not_at_all() {
        let (mut store, a, b) = library();
        let changes = vec![Change { id: a.clone(), add_tags: vec!["综述".into()], starred: Some(true), ..Default::default() }, Change { id: b.clone(), reading: Some("skimmed".into()), ..Default::default() }, Change { id: a.clone(), note: "再看图 3".into(), ..Default::default() }];
        let done = store.organise(&changes).unwrap();
        // The paper named twice is one row with both changes.
        assert_eq!(done.len(), 2);
        assert_eq!(done[0].changed.len(), 3);
        let kept = store.get_paper(&a).unwrap();
        assert_eq!((kept.tags.clone(), kept.starred), (vec!["scRNA".to_string(), "综述".to_string()], true));
        assert!(kept.notes.starts_with("我的想法。\n\n再看图 3\n（"));
        assert_eq!(store.get_paper(&b).unwrap().reading, "skimmed");
        assert_eq!(report(&done)["changed"], 2);

        // One paper that is not there, and the other is left alone too.
        let bad = vec![Change { id: b.clone(), starred: Some(true), ..Default::default() }, Change { id: "nowhere".into(), starred: Some(true), ..Default::default() }];
        assert!(store.organise(&bad).unwrap_err().to_string().contains("没有找到"));
        assert!(!store.get_paper(&b).unwrap().starred);

        // A paper in the trash is not organised.
        store.trash_paper(&b).unwrap();
        assert!(store.organise(&[Change { id: b, starred: Some(true), ..Default::default() }]).unwrap_err().to_string().contains("回收站"));
    }

    #[test]
    fn the_preview_says_what_would_happen_and_writes_nothing() {
        let (store, a, b) = library();
        let lines = preview(&store, &json!({"changes": [{"id": a, "addTags": ["综述"], "starred": true}, {"id": b, "reading": "unread"}]}));
        assert_eq!(lines.unwrap(), ["《Root atlas》：加标签「综述」；加星标", "《Leaf atlas》：已经是这样，不改"]);
        assert_eq!(store.get_paper(&a).unwrap().tags, ["scRNA"]);
        assert!(preview(&store, &json!({"changes": [{"id": "nowhere", "starred": true}]})).unwrap_err().contains("没有找到"));
    }
}
