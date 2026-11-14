//! 助手写: the writing page's other mode, where the assistant leads. The
//! person talks with it in the middle of the window and it writes the
//! manuscript at the right. It looks through the library with the tools
//! every conversation has and changes the manuscript with two of its own:
//! `edit_section` (`edit`) writes or changes one section, `set_outline`
//! (`outline`) says which sections the manuscript has. What a section held
//! is kept as a snapshot before the assistant's first change to it, so a
//! version can always be gone back to. `note` is what each question of
//! such a conversation tells the model about the manuscript, `label_citations`
//! turns the citations of a conversation (`[P3 p.5 "…"]`) into the
//! manuscript's before a tool runs and `keep_quoted` keeps the sentences
//! they quoted as the passages those citations rest on, and `hunks` is
//! what changed between two texts, for the window to show.

use crate::ai::Source;
use crate::cite::{self, Found, Index, Quoted};
use crate::manuscript::{self, Manuscript, Section};
use crate::passage;
use crate::revise::{self, Edit, Placed};
use crate::store::Store;
use crate::structure::{self, SECTIONS};
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::ops::Range;

/// The most sections `set_outline` takes at once: a list longer than this is a mistake, not an outline.
const MOST_SECTIONS: usize = 60;

fn word<'a>(args: &'a Value, key: &str) -> &'a str {
    args[key].as_str().unwrap_or("").trim()
}

fn names(sections: &[Section]) -> String {
    sections.iter().map(|s| format!("「{}」", s.name)).collect::<Vec<_>>().join("、")
}

/// The manuscript `wanted` names by id or title; the only one when nothing is said.
pub fn find_manuscript(store: &Store, wanted: &str) -> Result<Manuscript> {
    let manuscripts = store.manuscripts()?;
    let listed = || manuscripts.iter().map(|m| format!("「{}」", m.shown_title())).collect::<Vec<_>>().join("、");
    match wanted {
        "" if manuscripts.len() == 1 => Ok(manuscripts[0].clone()),
        "" if manuscripts.is_empty() => bail!("写作页还没有稿件。"),
        "" => bail!("有 {} 份稿件，用 manuscript 指定一份：{}", manuscripts.len(), listed()),
        _ => manuscripts.iter().find(|m| m.id == wanted || m.shown_title() == wanted).cloned().ok_or_else(|| anyhow!("没有这份稿件：{wanted}。现有：{}", listed())),
    }
}

/// `[@<paper id>]` written as `[@Key]`: a caller that knows a paper only by its id cites it so.
pub fn keyed(text: &str, index: &Index) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('@') {
        out.push_str(&rest[..at + 1]);
        rest = &rest[at + 1..];
        let end = rest.find(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_')).unwrap_or(rest.len());
        match index.key(&rest[..end]) {
            Some(key) => out.push_str(key),
            None => out.push_str(&rest[..end]),
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// The tool `edit_section`: write or change one section of a manuscript.
/// `edits` are changes each made where its words stand (`revise::place`; no
/// words to find is the end of the section), `text` is the section's whole
/// new text. The citations of the new words name papers by key or by id;
/// the keys are kept with their papers. What the section held is kept as a
/// snapshot first.
pub fn edit(store: &mut Store, args: &Value) -> Result<Value> {
    let manuscript = find_manuscript(store, word(args, "manuscript"))?;
    let wanted = word(args, "section");
    if wanted.is_empty() { bail!("用 section 指定要写的一节（节的 ID 或名称）。现有：{}", names(&manuscript.sections)) }
    let Some(section) = manuscript.sections.iter().find(|s| s.id == wanted).or_else(|| manuscript.sections.iter().find(|s| s.name == wanted)) else {
        bail!("这份稿件没有「{wanted}」这一节。现有：{}。要新加一节，用 set_outline。", names(&manuscript.sections))
    };
    let (papers, trash, kept) = (store.list_papers()?, store.list_trash()?, store.cite_keys()?);
    let index = Index::new(&papers, trash.iter().map(|entry| &entry.paper), &kept);
    let mut body = section.body.clone();
    let (mut changed, mut missed) = (0, vec![]);
    match (args["text"].as_str(), args["edits"].as_array()) {
        (Some(text), None) => {
            let text = keyed(text.trim(), &index);
            if text.is_empty() && !section.body.trim().is_empty() { bail!("text 是空的：不会把写了内容的一节清空。要删去一段，用 edits 并把 new 留空。") }
            if text != body.trim() {
                body = text;
                changed = 1;
            }
        }
        (None, Some(edits)) if !edits.is_empty() => {
            let mut near = None;
            for one in edits {
                let (old, new) = (one["old"].as_str().unwrap_or(""), keyed(one["new"].as_str().unwrap_or(""), &index));
                match revise::place(&body, &Edit { old: old.to_string(), new }, near) {
                    Placed::At(range, new) => {
                        near = Some(range.start + new.len());
                        body.replace_range(range, &new);
                        changed += 1;
                    }
                    Placed::Same => {}
                    Placed::Missing => missed.push(old.to_string()),
                }
            }
        }
        _ => bail!("给出 edits（逐处修改，每项是 old 和 new）或 text（这一节的全部新正文）其中之一。"),
    }
    let cited = cite::find(&body, &index);
    if changed > 0 {
        store.take_snapshot(&section.id, "assistant")?;
        store.save_section_body(&section.id, &body)?;
        // A key written into a manuscript stays with its paper from then on.
        for one in &cited {
            if let (Found::Paper(paper), false) = (&one.found, one.old) && let Some(key) = index.key(paper) { store.keep_cite_key(paper, key)? }
        }
    }
    let unknown: Vec<&str> = cited.iter().filter(|one| !one.old && one.found == Found::Unknown).map(|one| one.written.as_str()).collect();
    let mut out = json!({
        "manuscript": manuscript.id,
        "section": {"id": section.id, "name": section.name},
        "edited": changed,
        "characters": body.chars().count(),
    });
    if !missed.is_empty() {
        out["missed"] = json!(missed);
        out["hint"] = json!("missed 里的原文没有在这一节里找到，这几处没有改。原文要从这一节现在的文字里逐字摘出：先用 read_manuscript 读这一节，再改。");
    }
    if !unknown.is_empty() { out["unknownCitations"] = json!(unknown) }
    Ok(out)
}

/// One line of the outline `set_outline` asks for.
struct Line {
    /// The section it keeps: its id or its present name; empty for a new one.
    of: String,
    name: String,
    kind: String,
}

/// The tool `set_outline`: the sections a manuscript is to have, in order,
/// and its title and language when given. A section named by id or by the
/// name it has stays what it is, under the new name, kind and place; the
/// others are new. A section left out goes only when nothing was ever
/// written in it: one with text stays, after those asked for, and is said.
/// Without `sections` the outline stays as it is.
pub fn outline(store: &mut Store, args: &Value) -> Result<Value> {
    let manuscript = find_manuscript(store, word(args, "manuscript"))?;
    let title = args["title"].as_str().map(str::trim).filter(|title| !title.is_empty());
    let language = Some(word(args, "language")).filter(|code| !code.is_empty());
    if let Some(code) = language && !manuscript::is_language(code) { bail!("不认识这种语言：{code}。可以是：{}", manuscript::LANGUAGES.iter().map(|(code, _)| *code).collect::<Vec<_>>().join("、")) }
    let asked: &[Value] = match &args["sections"] {
        Value::Array(list) if !list.is_empty() => list,
        Value::Null if title.is_some() || language.is_some() => &[],
        _ => bail!("用 sections 给出稿件的各节，按先后排列；每项有 name，保留现有的一节时带上它的 id。只改题目或语言时可以不给 sections。"),
    };
    if asked.len() > MOST_SECTIONS { bail!("一份稿件最多 {MOST_SECTIONS} 节。") }
    let mut lines = vec![];
    for one in asked {
        let line = Line { of: word(one, "id").to_string(), name: word(one, "name").to_string(), kind: word(one, "kind").to_string() };
        if !line.kind.is_empty() && !manuscript::is_kind(&line.kind) { bail!("没有这种类型：{}。可以是：{}", line.kind, SECTIONS.iter().map(|(key, _)| *key).collect::<Vec<_>>().join("、")) }
        lines.push(line);
    }
    // Which section each line keeps: the one its id names, else the one that has its name; each once.
    let mut taken: Vec<&str> = vec![];
    let mut wanted = vec![];
    for line in &lines {
        let free = |s: &&Section| !taken.contains(&s.id.as_str());
        let found = if line.of.is_empty() { manuscript.sections.iter().filter(free).find(|s| s.name == line.name) } else { manuscript.sections.iter().filter(free).find(|s| s.id == line.of).or_else(|| manuscript.sections.iter().filter(free).find(|s| s.name == line.of)) };
        if found.is_none() && !line.of.is_empty() { bail!("这份稿件没有「{}」这一节。现有：{}。新加的一节不要带 id。", line.of, names(&manuscript.sections)) }
        let name = if line.name.is_empty() { found.map(|s| s.name.clone()).unwrap_or_default() } else { line.name.clone() };
        if name.is_empty() { bail!("每一节都需要 name。") }
        // A new section's kind is what its name says, when it says: that is where its candidate sentences come from.
        let kind = match (line.kind.as_str(), found) {
            ("", Some(section)) => section.kind.clone(),
            ("", None) => structure::canonical(&name).filter(|kind| manuscript::is_kind(kind)).unwrap_or("other").to_string(),
            (kind, _) => kind.to_string(),
        };
        if let Some(section) = found { taken.push(&section.id) }
        wanted.push(manuscript::Arrange { id: found.map(|s| s.id.clone()).unwrap_or_default(), name, kind });
    }
    let arranged = if wanted.is_empty() { manuscript::Arranged::default() } else { store.arrange_sections(&manuscript.id, &wanted)? };
    if let Some(title) = title { store.rename_manuscript(&manuscript.id, title)? }
    if let Some(code) = language { store.set_manuscript_language(&manuscript.id, code)? }
    let now = store.manuscript(&manuscript.id)?.unwrap_or_default();
    let mut out = json!({
        "manuscript": now.id,
        "title": now.shown_title(),
        "language": now.language,
        "sections": now.sections.iter().map(|s| json!({"id": s.id, "name": s.name, "kind": s.kind, "characters": s.body.chars().count()})).collect::<Vec<_>>(),
        "added": arranged.added,
        "removed": arranged.removed,
    });
    if !arranged.kept.is_empty() {
        out["kept"] = json!(arranged.kept);
        out["hint"] = json!("kept 里的节没有列在大纲里，但写过内容，所以没有删除，排在了最后。要去掉它们，请用户在大纲里自己删除。");
    }
    Ok(out)
}

/// What a call of one of the two tools would do, in a line or two: for the card that asks the person first,
/// where the conversation is not the manuscript's own.
pub fn preview(store: &Store, name: &str, args: &Value) -> Result<Vec<String>> {
    let manuscript = find_manuscript(store, word(args, "manuscript"))?;
    let title = manuscript.shown_title();
    Ok(match name {
        "edit_section" => {
            let section = word(args, "section");
            let name = manuscript.sections.iter().find(|s| s.id == section || s.name == section).map_or(section, |s| s.name.as_str());
            match (args["text"].as_str(), args["edits"].as_array()) {
                (Some(text), _) => vec![format!("《{title}》的「{name}」：整节写成新的 {} 字", text.trim().chars().count())],
                (None, Some(edits)) => vec![format!("《{title}》的「{name}」：改 {} 处", edits.len())],
                _ => vec![],
            }
        }
        _ => {
            let sections: Vec<&str> = args["sections"].as_array().into_iter().flatten().map(|one| word(one, "name")).filter(|name| !name.is_empty()).collect();
            let mut lines = vec![];
            if !sections.is_empty() { lines.push(format!("《{title}》的大纲改为：{}", sections.join("、"))) }
            if !word(args, "title").is_empty() { lines.push(format!("《{title}》的题目改为：{}", word(args, "title"))) }
            if !word(args, "language").is_empty() { lines.push(format!("《{title}》的正文语言改为：{}", crate::ai::language_name(word(args, "language")))) }
            lines
        }
    })
}

/// What the model is told about the manuscript with every question of the
/// conversation that writes it: the record, not what was said earlier, is
/// where the manuscript stands.
pub struct Asked<'a> {
    pub manuscript: &'a Manuscript,
    /// The section on show beside the conversation: its id.
    pub showing: &'a str,
    /// What is selected in it.
    pub selection: &'a str,
}

pub fn note(asked: &Asked) -> String {
    let manuscript = asked.manuscript;
    let language = crate::ai::language_name(&manuscript.language);
    let mut out = format!(
        "【写作模式】你在主导这份稿件的写作：用户在窗口右边看着稿件，在这里和你对话，由你动笔。\n稿件：《{}》（ID {}），正文用{language}写。\n大纲（节 ID · 名称 · 类型 · 现有字数）：\n",
        manuscript.shown_title(),
        manuscript.id
    );
    for (n, section) in manuscript.sections.iter().enumerate() {
        let kind = SECTIONS.iter().find(|(key, _)| *key == section.kind).map_or(section.kind.as_str(), |(_, name)| name);
        let length = section.body.chars().filter(|c| !c.is_whitespace()).count();
        out.push_str(&format!("{}. {} · {} · {kind} · {}{}\n", n + 1, section.id, section.name, if length == 0 { "未写".to_string() } else { format!("{length} 字") }, if section.id == asked.showing { " ← 右边正显示这一节" } else { "" }));
    }
    let selection = asked.selection.trim();
    if !selection.is_empty() {
        let name = manuscript.section(asked.showing).map_or("", |s| s.name.as_str());
        out.push_str(&format!("用户在「{name}」里选中了下面这段文字，这次的要求多半是针对它的：\n{selection}\n"));
    }
    out.push_str(&format!(
        "做法：\n\
- 稿件只通过工具改动：edit_section 写或改一节（edits 逐处修改；text 写一节的全文，用于空的一节或整节重写），set_outline 调整各节、题目和正文语言（用户要的语言和上面写的不一样时，先用它改过来）。要写进稿件的正文不要贴在对话里；对话里用几句话说明做了什么、依据哪些文献、还缺什么，让用户去右边看稿。\n\
- 动笔前先读：要改的一节用 read_manuscript 读它现在的文字，用户可能刚亲手改过，以稿件现在的文字为准；edits 的 old 要从中逐字摘出。\n\
- 论断要有出处：先用 find_sentences、find_citations、search_fulltext 等在文献库里找到文献和原句，再写。写进稿件的引用和回答里一样，尽量带上它所依据的原句：[P3 p.5 \"逐字摘出的原句\"]，出自摘要的原句不写页码：[P3 \"逐字摘出的原句\"]，实在没有可摘的原句才写 [P3]。写入时会换成稿件的引用格式，引的那句原文会记为这处引用的出处，用户点一下就能核对。文献库里找不到依据的论断不要配引用，更不要编造文献；在对话里指出来。\n\
- 正文用{language}写，是 Markdown；节的名称不要作为标题写进正文。\n\
- 只做用户要求的范围。删去或合并写了内容的节、大段删改之前，先在对话里说明并征得同意。"
    ));
    out
}

/// A piece of text the assistant is about to write into a manuscript: the
/// text as the manuscript takes it, and the sentences its citations quoted.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Piece {
    pub text: String,
    pub quoted: Vec<Quoted>,
}

/// Before `edit_section` runs for a conversation: the citations of the
/// words to be written (`text`, and every `new` of `edits`) are turned from
/// the conversation's (`[P3 p.5 "…"]`) into the manuscript's (`[@Key, p. 5]`)
/// in `args`. The pieces written, each with what it quoted, for `keep_quoted`.
pub fn label_citations(args: &mut Value, sources: &[Source], index: &Index) -> Vec<Piece> {
    let mut pieces = vec![];
    let mut convert = |value: &mut Value| {
        let Some(text) = value.as_str() else { return };
        let converted = cite::convert_ai_citations(text, sources, index);
        pieces.push(Piece { text: converted.text.clone(), quoted: converted.quoted });
        *value = json!(converted.text);
    };
    // Only what the call has: asking a JSON object for a key it lacks would put the key there.
    if let Some(text) = args.get_mut("text") { convert(text) }
    for one in args.get_mut("edits").and_then(Value::as_array_mut).into_iter().flatten() {
        if let Some(new) = one.get_mut("new") { convert(new) }
    }
    pieces
}

/// A tool called for a conversation: as `tools::call_in`, and for
/// `edit_section` the citations of the words to be written are the
/// conversation's (`label_citations`), and the sentences they quote are
/// kept as the passages those citations rest on (`keep_quoted`). The
/// result says how many were kept and which quotations their papers do not hold.
pub fn call(store: &mut Store, info: &crate::libraries::LibraryInfo, name: &str, args: &Value, sources: &[Source], fetch: crate::sources::Fetch, download: crate::sources::Download) -> Result<Value, String> {
    if name != "edit_section" { return crate::tools::call_in(store, info, name, args, fetch, download) }
    let mut args = args.clone();
    let pieces = {
        let read = |store: &Store| -> Result<_> { Ok((store.list_papers()?, store.list_trash()?, store.cite_keys()?)) };
        let (papers, trash, kept) = read(store).map_err(|e| format!("{e:#}"))?;
        label_citations(&mut args, sources, &Index::new(&papers, trash.iter().map(|entry| &entry.paper), &kept))
    };
    let mut value = crate::tools::call_in(store, info, name, &args, fetch, download)?;
    // A label that names no paper of this conversation went into the manuscript as it was written: the model is told, to put it right.
    let mut stray: Vec<String> = pieces.iter().flat_map(|piece| crate::ai::citation_spans(&piece.text)).flat_map(|(_, cited)| cited.into_iter().map(|one| one.label)).collect();
    stray.dedup();
    if !stray.is_empty() {
        value["unknownLabels"] = json!(stray);
        value["labelsHint"] = json!("unknownLabels 里的标签在这次对话里不对应任何文献，已原样留在稿件里。用 edits 把它们改成正确的文献标签，或去掉这几处引用。");
    }
    let (Some(manuscript), Some(section)) = (value["manuscript"].as_str().map(str::to_string), value["section"]["id"].as_str().map(str::to_string)) else { return Ok(value) };
    let body = store.manuscript(&manuscript).ok().flatten().and_then(|m| m.section(&section).map(|s| s.body.clone())).unwrap_or_default();
    // The section is written by now: a passage that could not be kept takes nothing back.
    if let Ok((kept, unfound)) = keep_quoted(store, &manuscript, &section, &body, &pieces) {
        if kept > 0 { value["passagesKept"] = json!(kept) }
        if !unfound.is_empty() {
            value["quotesNotFound"] = json!(unfound);
            value["quotesHint"] = json!("quotesNotFound 里的原句没有在所引文献的摘要或全文里找到，没有记为出处。核对这几处引用：换成文献里确有的句子，或去掉引用，并在对话里告诉用户。");
        }
    }
    Ok(value)
}

/// After `edit_section` wrote `pieces` into a section that now reads
/// `body`: the sentences they quoted for their citations are kept as what
/// those citations rest on, where the paper really says them, on the page
/// it does. How many were kept, and the quotations their papers do not hold.
pub fn keep_quoted(store: &mut Store, manuscript_id: &str, section_id: &str, body: &str, pieces: &[Piece]) -> Result<(usize, Vec<String>)> {
    // Where each piece stands in the section, as it was written or without the space around it.
    let mut quoted: Vec<Quoted> = vec![];
    for piece in pieces.iter().filter(|piece| !piece.quoted.is_empty()) {
        let lead = piece.text.len() - piece.text.trim_start().len();
        let Some(at) = body.find(piece.text.trim()) else { continue };
        quoted.extend(piece.quoted.iter().filter(|one| one.at >= lead).map(|one| Quoted { at: at + one.at - lead, ..one.clone() }));
    }
    if quoted.is_empty() { return Ok((0, vec![])) }
    let (papers, trash, kept) = (store.list_papers()?, store.list_trash()?, store.cite_keys()?);
    let index = Index::new(&papers, trash.iter().map(|entry| &entry.paper), &kept);
    let (mut new, mut unfound) = (vec![], vec![]);
    for (paper_id, claim, mut candidate) in passage::quoted(body, &cite::find(body, &index), &quoted) {
        let Some(paper) = papers.iter().find(|paper| paper.id == paper_id) else { continue };
        let pages = store.paper_pages(&paper_id)?;
        match crate::ai::locate_quote(&candidate.text, paper, pages.as_deref()) {
            Some(Some(page)) => candidate.page = page,
            Some(None) => (candidate.part, candidate.page) = ("abstract".into(), 0),
            // The paper does not say it: nothing to rest on.
            None => {
                unfound.push(candidate.text);
                continue;
            }
        }
        new.push((section_id.to_string(), paper_id, claim, candidate, String::new(), passage::CITED.to_string()));
    }
    if !new.is_empty() { store.keep_passages(manuscript_id, &new)?; }
    Ok((new.len(), unfound))
}

/// A stretch that differs between two texts: these bytes of the one before, those of the one after.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub before: Range<usize>,
    pub after: Range<usize>,
}

/// What changed from `before` to `after`, line by line: each run of lines
/// that differ is one hunk, in the order of the text, and two runs with
/// nothing but empty lines between them are one. A line here is a
/// paragraph of the manuscript or the empty line between two. Putting each
/// hunk's words of `after` where its bytes of `before` are, from the last
/// to the first, makes `after` of `before`.
pub fn hunks(before: &str, after: &str) -> Vec<Hunk> {
    if before == after { return vec![] }
    // Every line with its line break, the last one given one: lines are alike only when all of them is.
    let (old, new) = (format!("{before}\n"), format!("{after}\n"));
    let lines = |text: &str| -> Vec<Range<usize>> {
        let mut at = 0;
        text.split_inclusive('\n').map(|line| { at += line.len(); at - line.len()..at }).collect()
    };
    let (a, b) = (lines(&old), lines(&new));
    let same = |i: usize, j: usize| old[a[i].clone()] == new[b[j].clone()];
    // How many lines the two have in common from each pair of places on, in order.
    let mut table = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            table[i][j] = if same(i, j) { table[i + 1][j + 1] + 1 } else { table[i + 1][j].max(table[i][j + 1]) };
        }
    }
    let start = |ranges: &[Range<usize>], at: usize, text: &str| ranges.get(at).map_or(text.len(), |range| range.start);
    let (mut i, mut j) = (0, 0);
    let mut out: Vec<Hunk> = vec![];
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && same(i, j) {
            (i, j) = (i + 1, j + 1);
            continue;
        }
        let (from_a, from_b) = (start(&a, i, &old), start(&b, j, &new));
        while (i < a.len() || j < b.len()) && !(i < a.len() && j < b.len() && same(i, j)) {
            if j >= b.len() || (i < a.len() && table[i + 1][j] > table[i][j + 1]) { i += 1 } else { j += 1 }
        }
        let hunk = Hunk { before: from_a..start(&a, i, &old), after: from_b..start(&b, j, &new) };
        match out.last_mut() {
            Some(last) if old[last.before.end..hunk.before.start].trim().is_empty() => (last.before.end, last.after.end) = (hunk.before.end, hunk.after.end),
            _ => out.push(hunk),
        }
    }
    // The line break given to the last line is not the text's: a hunk that reaches it takes the one before its lines instead.
    for hunk in &mut out {
        if hunk.before.end <= before.len() && hunk.after.end <= after.len() { continue }
        if hunk.before.start > 0 && hunk.after.start > 0 { (hunk.before.start, hunk.after.start) = (hunk.before.start - 1, hunk.after.start - 1) }
        (hunk.before.end, hunk.after.end) = (hunk.before.end.min(before.len()), hunk.after.end.min(after.len()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Paper;
    use std::path::Path;

    fn store() -> Store {
        Store::in_memory(Path::new("/tmp/wisp-compose-test")).unwrap()
    }

    fn library() -> (Store, Manuscript, Paper) {
        let mut store = store();
        let paper = store.add_paper(Paper { title: "Root stem cell niche".into(), authors: "Zhang, Wei; Li, Na".into(), year: Some(2021), abstract_text: "WOX5 maintains the quiescent centre of the root.".into(), ..Default::default() }).unwrap();
        let made = store.create_manuscript("根的综述", "zh").unwrap();
        (store, made, paper)
    }

    #[test]
    fn a_section_is_written_changed_and_its_versions_kept() {
        let (mut store, made, paper) = library();
        let section = made.sections[1].clone();
        // The whole text of an empty section, citing a paper by its id.
        let done = edit(&mut store, &json!({"section": section.name, "text": format!("根尖有干细胞微环境 [@{}]。\n\n第二段。", paper.id)})).unwrap();
        assert_eq!((done["edited"].as_u64(), done["section"]["id"].as_str()), (Some(1), Some(section.id.as_str())));
        let body = store.manuscript(&made.id).unwrap().unwrap().section(&section.id).unwrap().body.clone();
        assert_eq!(body, "根尖有干细胞微环境 [@Zhang2021]。\n\n第二段。");
        // The key stays with its paper; nothing was there to keep as a version.
        assert_eq!(store.cite_keys().unwrap().get("Zhang2021"), Some(&paper.id));
        assert!(store.snapshots(&section.id).unwrap().is_empty());
        // Changes where the words stand; one whose words are not there is said, the others are made.
        let done = edit(&mut store, &json!({"manuscript": made.id, "section": section.id, "edits": [{"old": "第二段。", "new": "第二段写长了。"}, {"old": "没有这句", "new": "x"}, {"old": "", "new": "结尾一段 [@Nobody1999]。"}]})).unwrap();
        assert_eq!((done["edited"].as_u64(), done["missed"][0].as_str(), done["unknownCitations"][0].as_str()), (Some(2), Some("没有这句"), Some("@Nobody1999")));
        let now = store.manuscript(&made.id).unwrap().unwrap().section(&section.id).unwrap().body.clone();
        assert_eq!(now, "根尖有干细胞微环境 [@Zhang2021]。\n\n第二段写长了。\n\n结尾一段 [@Nobody1999]。");
        // What the section said before is a version to go back to.
        let versions = store.snapshots(&section.id).unwrap();
        assert_eq!((versions.len(), versions[0].body.as_str(), versions[0].why.as_str()), (1, body.as_str(), "assistant"));
        // Nothing to do changes nothing and keeps no version.
        let done = edit(&mut store, &json!({"section": section.id, "edits": [{"old": "第二段写长了。", "new": "第二段写长了。"}]})).unwrap();
        assert_eq!((done["edited"].as_u64(), store.snapshots(&section.id).unwrap().len()), (Some(0), 1));
        // What is refused, and why.
        assert!(edit(&mut store, &json!({"section": "没有", "text": "x"})).unwrap_err().to_string().contains("没有「没有」这一节"));
        assert!(edit(&mut store, &json!({"section": section.id})).unwrap_err().to_string().contains("其中之一"));
        assert!(edit(&mut store, &json!({"section": section.id, "text": "  "})).unwrap_err().to_string().contains("清空"));
        assert!(edit(&mut store, &json!({"text": "x"})).unwrap_err().to_string().contains("section"));
    }

    #[test]
    fn an_outline_keeps_what_was_written() {
        let (mut store, made, _) = library();
        let (abstract_id, intro) = (made.sections[0].id.clone(), made.sections[1].clone());
        store.save_section_body(&intro.id, "已经写了的导言。").unwrap();
        // The introduction under a new name, the abstract by its name, two new sections; the three unwritten ones are left out.
        let done = outline(&mut store, &json!({"title": "根尖干细胞", "sections": [{"name": made.sections[0].name}, {"id": intro.id, "name": "研究背景"}, {"name": "干细胞微环境"}, {"name": "Discussion"}]})).unwrap();
        let now = store.manuscript(&made.id).unwrap().unwrap();
        assert_eq!(now.title, "根尖干细胞");
        assert_eq!(now.sections.iter().map(|s| (s.name.as_str(), s.kind.as_str())).collect::<Vec<_>>(), vec![(made.sections[0].name.as_str(), "abstract"), ("研究背景", "introduction"), ("干细胞微环境", "other"), ("Discussion", "discussion")]);
        assert_eq!((now.sections[0].id.as_str(), now.sections[1].id.as_str(), now.sections[1].body.as_str()), (abstract_id.as_str(), intro.id.as_str(), "已经写了的导言。"));
        assert_eq!((done["added"].as_array().map(Vec::len), done["removed"].as_array().map(Vec::len), done["kept"].is_null()), (Some(2), Some(3), true));
        // A section with text that the outline leaves out stays, at the end, and is said.
        let done = outline(&mut store, &json!({"sections": [{"name": "干细胞微环境"}]})).unwrap();
        let now = store.manuscript(&made.id).unwrap().unwrap();
        assert_eq!(now.sections.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), vec!["干细胞微环境", "研究背景"]);
        assert_eq!(done["kept"][0], "研究背景");
        assert!(outline(&mut store, &json!({"sections": []})).is_err() && outline(&mut store, &json!({})).is_err());
        // The language and the title alone: the outline stays as it is.
        let done = outline(&mut store, &json!({"language": "en", "title": "Root stem cells"})).unwrap();
        assert_eq!((done["language"].as_str(), done["title"].as_str(), done["sections"].as_array().map(Vec::len)), (Some("en"), Some("Root stem cells"), Some(2)));
        assert!(outline(&mut store, &json!({"language": "tlh"})).unwrap_err().to_string().contains("不认识这种语言"));
        outline(&mut store, &json!({"title": "根尖干细胞", "language": "zh"})).unwrap();
        assert!(outline(&mut store, &json!({"sections": [{"id": "S-nowhere", "name": "x"}]})).unwrap_err().to_string().contains("没有「S-nowhere」"));
        assert!(outline(&mut store, &json!({"sections": [{"name": "x", "kind": "poem"}]})).unwrap_err().to_string().contains("没有这种类型"));
        assert_eq!(preview(&store, "set_outline", &json!({"sections": [{"name": "甲"}, {"name": "乙"}]})).unwrap(), vec!["《根尖干细胞》的大纲改为：甲、乙"]);
        assert_eq!(preview(&store, "edit_section", &json!({"section": "研究背景", "edits": [{}, {}]})).unwrap(), vec!["《根尖干细胞》的「研究背景」：改 2 处"]);
    }

    #[test]
    fn a_conversations_citations_become_the_manuscripts_with_their_passages() {
        let (mut store, made, paper) = library();
        let section = made.sections[1].id.clone();
        let sources = vec![Source { label: "P1".into(), paper_id: paper.id.clone(), title: paper.title.clone() }];
        let mut args = json!({"section": section, "edits": [{"old": "", "new": "WOX5 维持静止中心 [P1 \"WOX5 maintains the quiescent centre of the root.\"]。另一句没有依据 [P1 \"The paper never says this sentence at all.\"]。"}]});
        let pieces = {
            let (papers, kept) = (store.list_papers().unwrap(), store.cite_keys().unwrap());
            label_citations(&mut args, &sources, &Index::new(&papers, std::iter::empty(), &kept))
        };
        assert_eq!(args["edits"][0]["new"], "WOX5 维持静止中心 [@Zhang2021]。另一句没有依据 [@Zhang2021]。");
        assert!(args.get("text").is_none() && pieces[0].quoted.len() == 2);
        edit(&mut store, &args).unwrap();
        let body = store.manuscript(&made.id).unwrap().unwrap().section(&section).unwrap().body.clone();
        // Only the sentence the paper says is kept as what its citation rests on.
        assert_eq!(keep_quoted(&mut store, &made.id, &section, &body, &pieces).unwrap(), (1, vec!["The paper never says this sentence at all.".to_string()]));
        let kept = store.passages(&made.id).unwrap();
        assert_eq!((kept.len(), kept[0].paper_id.as_str(), kept[0].passage.as_str(), kept[0].claim.as_str()), (1, paper.id.as_str(), "WOX5 maintains the quiescent centre of the root.", "WOX5 维持静止中心。"));
        // The same through the call a conversation makes: what was kept and what was not is said, and a label of no paper too.
        let info = crate::libraries::LibraryInfo::default();
        let nothing = |_: &str| -> Result<String, String> { Err("no network".into()) };
        let none = |_: &str| -> Result<Vec<u8>, String> { Err("no network".into()) };
        let done = call(&mut store, &info, "edit_section", &json!({"section": section, "edits": [{"old": "", "new": "再写一句 [P1 \"WOX5 maintains the quiescent centre of the root.\"]，又一句 [P9]。"}]}), &sources, &nothing, &none).unwrap();
        assert_eq!((done["passagesKept"].as_u64(), done["unknownLabels"][0].as_str(), done["quotesNotFound"].is_null()), (Some(1), Some("P9"), true));
        assert!(store.manuscript(&made.id).unwrap().unwrap().section(&section).unwrap().body.ends_with("再写一句 [@Zhang2021]，又一句 [P9]。"));
        // Any other tool goes as the table has it.
        assert!(call(&mut store, &info, "list_manuscripts", &json!({}), &sources, &nothing, &none).unwrap()["manuscripts"].is_array());
    }

    #[test]
    fn the_note_says_where_the_manuscript_stands() {
        let (mut store, made, _) = library();
        store.save_section_body(&made.sections[1].id, "写了 六个字。").unwrap();
        let now = store.manuscript(&made.id).unwrap().unwrap();
        let said = note(&Asked { manuscript: &now, showing: &now.sections[1].id, selection: " 六个字 " });
        assert!(said.contains(&format!("（ID {}）", now.id)) && said.contains("正文用中文写"));
        assert!(said.contains(&format!("2. {} · {} · 导言 · 6 字 ← 右边正显示这一节", now.sections[1].id, now.sections[1].name)));
        assert!(said.contains(&format!("1. {} · {} · 摘要 · 未写\n", now.sections[0].id, now.sections[0].name)));
        assert!(said.contains(&format!("用户在「{}」里选中了下面这段文字", now.sections[1].name)) && said.contains("\n六个字\n"));
        assert!(!note(&Asked { manuscript: &now, showing: "", selection: "" }).contains("选中了"));
    }

    #[test]
    fn what_changed_between_two_texts() {
        assert_eq!(hunks("a\n\nb\n\nc", "a\n\nb\n\nc"), vec![]);
        // A paragraph changed in place.
        assert_eq!(hunks("a\n\nb\n\nc", "a\n\nB!\n\nc"), vec![Hunk { before: 3..5, after: 3..6 }]);
        // One put in, one taken out, each a hunk of its own.
        let (before, after) = ("one\n\ntwo\n\nthree\n\nfour", "one\n\nnew\n\ntwo\n\nthree");
        let found = hunks(before, after);
        assert_eq!(found.len(), 2);
        assert_eq!((&before[found[0].before.clone()], &after[found[0].after.clone()]), ("", "new\n\n"));
        assert_eq!((&before[found[1].before.clone()], &after[found[1].after.clone()]), ("\n\nfour", ""));
        // Words added at the end of an empty text, and everything taken away.
        assert_eq!(hunks("", "x\n\ny"), vec![Hunk { before: 0..0, after: 0..4 }]);
        assert_eq!(hunks("x", ""), vec![Hunk { before: 0..1, after: 0..0 }]);
        // Applying the hunks from the last to the first makes the one text of the other.
        let mut made = before.to_string();
        for hunk in found.iter().rev() { made.replace_range(hunk.before.clone(), &after[hunk.after.clone()]) }
        assert_eq!(made, after);
        assert_eq!(hunks("a\n", "a"), vec![Hunk { before: 1..2, after: 1..1 }]);
    }
}
