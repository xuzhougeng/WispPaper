//! 技能: instructions the person keeps for the assistant, each a folder
//! with a `SKILL.md` (a `name` and `description` between `---` lines, then
//! the instructions), the way Claude's skills are written. The assistant is
//! told the names and descriptions and reads one with `use_skill` when a
//! request calls for it. A skill is words only: nothing in its folder is run.

use crate::connector::Tool;
use serde_json::json;
use std::path::{Path, PathBuf};

/// The tool a skill is read with.
pub const TOOL: &str = "use_skill";
const FILE: &str = "SKILL.md";

#[derive(Clone, Debug, PartialEq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// The instructions: the file without its front matter.
    pub body: String,
    pub dir: PathBuf,
}

/// The folder the skills are kept in: beside the settings file.
pub fn folder(settings_file: &Path) -> PathBuf {
    settings_file.parent().unwrap_or(Path::new(".")).join("skills")
}

/// A `SKILL.md` as (name, description, instructions); a field the front matter lacks is empty.
pub fn parse(text: &str) -> (String, String, String) {
    let text = text.trim_start_matches('\u{feff}');
    let Some((front, body)) = text.strip_prefix("---").and_then(|rest| rest.split_once("\n---")) else { return (String::new(), String::new(), text.trim().to_string()) };
    let (mut name, mut description, mut field) = (String::new(), String::new(), "");
    for line in front.lines() {
        let said = |value: &str| value.trim().trim_matches(['"', '\'']).to_string();
        // An indented line goes on with the field above it (`description: >`).
        if line.starts_with([' ', '\t']) {
            let more = said(line);
            if field == "description" && !more.is_empty() {
                if !description.is_empty() { description.push(' ') }
                description.push_str(&more);
            }
        } else if let Some(value) = line.strip_prefix("name:") {
            (name, field) = (said(value), "name");
        } else if let Some(value) = line.strip_prefix("description:") {
            (description, field) = (said(value.trim().trim_start_matches(['>', '|', '-'])), "description");
        } else {
            field = "";
        }
    }
    // The rest of the closing line of dashes is not part of the instructions.
    (name, description, body.split_once('\n').map_or("", |(_, rest)| rest).trim().to_string())
}

/// The skills in `dir`, by name; a folder without a `SKILL.md` is none.
pub fn list(dir: &Path) -> Vec<Skill> {
    let mut skills: Vec<Skill> = std::fs::read_dir(dir).into_iter().flatten().flatten().filter_map(|entry| {
        let dir = entry.path();
        let (name, description, body) = parse(&std::fs::read_to_string(dir.join(FILE)).ok()?);
        let name = if name.is_empty() { dir.file_name()?.to_string_lossy().into_owned() } else { name };
        Some(Skill { name, description, body, dir })
    }).collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// Copy a skill into `dir`: a folder holding a `SKILL.md`, or one Markdown
/// file, which becomes the `SKILL.md` of a folder named after it. A skill
/// already there under that folder name is left alone.
pub fn import(dir: &Path, from: &Path) -> Result<String, String> {
    let file = if from.is_dir() { from.join(FILE) } else { from.to_path_buf() };
    if !file.is_file() { return Err(format!("{} 里没有 {FILE}。", from.display())) }
    let name = from.file_stem().map(|stem| stem.to_string_lossy().into_owned()).filter(|stem| !stem.is_empty()).ok_or("这个路径没有名字。")?;
    let to = dir.join(&name);
    if to.exists() { return Err(format!("已经有名为 {name} 的技能文件夹。")) }
    let failed = |err: std::io::Error| format!("无法保存技能：{err}");
    if from.is_dir() { copy_folder(from, &to).map_err(failed)? } else {
        std::fs::create_dir_all(&to).map_err(failed)?;
        std::fs::copy(&file, to.join(FILE)).map_err(failed)?;
    }
    Ok(name)
}

fn copy_folder(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() { copy_folder(&entry.path(), &target)? } else { std::fs::copy(entry.path(), target).map(|_| ())? }
    }
    Ok(())
}

/// The tool that reads a skill, its description listing those on offer; none without skills.
pub fn tool(skills: &[Skill]) -> Option<Tool> {
    if skills.is_empty() { return None }
    let listed: String = skills.iter().map(|skill| format!("\n- {}：{}", skill.name, skill.description)).collect();
    Some(Tool {
        name: TOOL.into(),
        tool: TOOL.into(),
        description: format!("读取用户保存的一项技能（一份做某类事情的说明）。当前请求符合下面某项技能的描述时，先用它读出说明，再照着做。可用的技能：{listed}"),
        input_schema: json!({"type": "object", "properties": {"name": {"type": "string", "enum": skills.iter().map(|skill| skill.name.as_str()).collect::<Vec<_>>()}}, "required": ["name"]}),
        read_only: true,
    })
}

/// What `use_skill` answers for `name`.
pub fn read(skills: &[Skill], name: &str) -> Result<String, String> {
    skills.iter().find(|skill| skill.name == name).map(|skill| skill.body.clone()).ok_or_else(|| format!("没有名为 {name} 的技能。"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_skill_file_is_read_with_and_without_front_matter() {
        assert_eq!(parse("---\nname: journal-club\ndescription: \"Prepare a journal club\"\nlicense: MIT\n---\n\n# Steps\n1. Read."), ("journal-club".into(), "Prepare a journal club".into(), "# Steps\n1. Read.".into()));
        assert_eq!(parse("---\nname: a\ndescription: >\n  first line\n  second line\nallowed-tools:\n  - x\n---\nBody"), ("a".into(), "first line second line".into(), "Body".into()));
        assert_eq!(parse("Just do it.\n"), (String::new(), String::new(), "Just do it.".into()));
    }

    #[test]
    fn skills_are_imported_listed_and_read() {
        let root = std::env::temp_dir().join(format!("wisp-skills-{}", crate::model::new_id("T")));
        let (dir, from) = (root.join("skills"), root.join("from"));
        std::fs::create_dir_all(from.join("review-notes/refs")).unwrap();
        std::fs::write(from.join("review-notes/SKILL.md"), "---\nname: review-notes\ndescription: 整理读书笔记\n---\n按题目分组。").unwrap();
        std::fs::write(from.join("review-notes/refs/a.md"), "x").unwrap();
        std::fs::write(from.join("plain.md"), "只说结论。").unwrap();
        assert_eq!(import(&dir, &from.join("review-notes")), Ok("review-notes".into()));
        assert_eq!(import(&dir, &from.join("plain.md")), Ok("plain".into()));
        assert!(import(&dir, &from.join("plain.md")).is_err() && import(&dir, &from).is_err());
        assert!(dir.join("review-notes/refs/a.md").is_file());
        let skills = list(&dir);
        assert_eq!(skills.iter().map(|skill| skill.name.as_str()).collect::<Vec<_>>(), ["plain", "review-notes"]);
        let tool = tool(&skills).unwrap();
        assert!(tool.description.contains("- review-notes：整理读书笔记") && tool.input_schema["properties"]["name"]["enum"][0] == "plain");
        assert_eq!(read(&skills, "review-notes"), Ok("按题目分组。".into()));
        assert!(read(&skills, "none").is_err() && super::tool(&[]).is_none() && list(&root.join("nowhere")).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
