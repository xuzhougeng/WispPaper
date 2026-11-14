//! 提取: a model reads a paper and proposes what the library's custom
//! fields (自定义词条) should hold for it, each value with a sentence quoted
//! word for word from the paper. A proposal only: `paper_fields` is written
//! by the person, who looks the proposals over and writes the ticked ones
//! together (`Store::set_fields`: one transaction, all or none), and can
//! take that back.
//!
//! One proposal is kept per paper and field (`field_proposals`), a new one
//! replacing the old. It stays after its value was written: that is where
//! the sentence behind a value is found again. Where it stands now is not
//! stored but read off the paper (`standing`): waiting, already what the
//! field holds, set aside by the person, made for the field as it was
//! defined before, or the model's word that the paper does not say.
//!
//! This file is the prompt and its reading (`prompt`, `parse`), the rules
//! for a value (`coerce`, `fits`: a Select answer is one of its options or
//! it is not written), which papers a run asks about (`wanted`), and the
//! person's side of the confirmation (`Reviewing`).

use crate::ai;
use crate::model::{FieldDef, FieldKind, Paper, clean_strings, split_list};
use crate::store::Store;
use crate::time::now_iso;
use anyhow::{Result, anyhow, bail};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// The table, made with the library's own (`store::SCHEMA`). An older build
/// does not know it and reads the rest of the database as before.
pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS field_proposals (
    paper_id TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
    field_id TEXT NOT NULL REFERENCES field_defs(id) ON DELETE CASCADE,
    -- What the field should hold, as it would be stored; '[]' when the paper does not say.
    values_json TEXT NOT NULL DEFAULT '[]',
    -- A sentence quoted word for word from the abstract or the full text, and its page there.
    excerpt TEXT NOT NULL DEFAULT '',
    page INTEGER,
    -- What was read: fulltext or abstract.
    basis TEXT NOT NULL DEFAULT '',
    -- Who proposed it: a model profile, or an outside agent.
    author TEXT NOT NULL DEFAULT '',
    -- The field as it was defined when it was asked about (`asked_as`).
    asked TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    -- When the person set it aside; empty while it stands.
    dismissed_at TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (paper_id, field_id)
);
"#;

/// The field the feature offers to add: what a paper found, in a sentence or two.
pub const SUMMARY: &str = "要点";
pub const SUMMARY_HINT: &str = "用一两句话概括这篇文献发现了什么：写结论，而不是做了什么。";

/// A paper and a field: what a proposal is about.
pub type Key = (String, String);

/// What a model or an agent thinks a field of a paper should hold.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Proposal {
    pub paper_id: String,
    pub field_id: String,
    /// As the field would hold it; empty when the paper does not say.
    pub values: Vec<String>,
    /// Quoted word for word from the abstract or the full text; empty when there is none.
    pub excerpt: String,
    /// The full-text page the excerpt is on.
    pub page: Option<usize>,
    /// What was read: fulltext or abstract.
    pub basis: String,
    pub author: String,
    /// The field as it was defined when asked (`asked_as`).
    pub asked: String,
    pub created_at: String,
    /// Set aside by the person: not offered again, not asked again.
    pub dismissed: bool,
}

/// Where a proposal stands for its paper and field as they are now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// Waiting for the person.
    Open,
    /// The field holds exactly this: the proposal is the sentence behind the value.
    Held,
    /// The model read the paper and found nothing for the field.
    Silent,
    /// Set aside by the person.
    Dismissed,
    /// Made for the field as it was defined before: asked again by the next run.
    Stale,
}

/// The definition a proposal answers: the name and what it is said to be
/// for, and the options where the value must be one of them.
pub fn asked_as(field: &FieldDef) -> String {
    let options = if field.kind == FieldKind::Select { field.options.join("|") } else { String::new() };
    format!("{}\n{}\n{}\n{}", field.name, field.kind.as_str(), field.hint, options)
}

/// Whether a field that holds `held` already has what is proposed: the
/// same value, or for a multi-value field every one of the proposed values.
fn holds(kind: FieldKind, held: &[String], proposed: &[String]) -> bool {
    if kind != FieldKind::Tags { return held == proposed }
    proposed.iter().all(|value| held.contains(value))
}

/// What a field holds once a proposal is taken: the proposed value, and for
/// a multi-value field the values it has with the new ones after them, so
/// nothing the person entered is lost.
pub fn taken(field: &FieldDef, held: &[String], proposed: &[String]) -> Vec<String> {
    if field.kind != FieldKind::Tags { return proposed.to_vec() }
    clean_strings(held.iter().chain(proposed))
}

pub fn standing(proposal: &Proposal, field: &FieldDef, paper: &Paper) -> Standing {
    if proposal.asked != asked_as(field) { return Standing::Stale }
    if proposal.values.is_empty() { return Standing::Silent }
    if paper.fields.get(&field.id).is_some_and(|held| holds(field.kind, held, &proposal.values)) { return Standing::Held }
    if proposal.dismissed { Standing::Dismissed } else { Standing::Open }
}

/// The proposals of a library, by paper.
#[derive(Clone, Debug, Default)]
pub struct Proposals(HashMap<String, Vec<Proposal>>);

impl Proposals {
    pub fn new(all: Vec<Proposal>) -> Proposals {
        let mut proposals = Proposals::default();
        for proposal in all {
            proposals.put(proposal);
        }
        proposals
    }

    pub fn get(&self, paper_id: &str, field_id: &str) -> Option<&Proposal> {
        self.0.get(paper_id)?.iter().find(|p| p.field_id == field_id)
    }

    /// Keep a proposal, in place of the one for the same paper and field.
    pub fn put(&mut self, proposal: Proposal) {
        let of_paper = self.0.entry(proposal.paper_id.clone()).or_default();
        of_paper.retain(|p| p.field_id != proposal.field_id);
        of_paper.push(proposal);
    }
}

// ---- values

/// Letters and digits in lower case: how two spellings of a value are compared.
fn plain(value: &str) -> String {
    value.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// The ways a model says that there is nothing to say.
const NOTHING: [&str; 16] = ["null", "none", "na", "nil", "unknown", "notstated", "notmentioned", "notspecified", "notreported", "notapplicable", "未提及", "未说明", "未知", "无", "不适用", "文中未说明"];

/// The option that `value` is another spelling of.
fn spelled<'a>(options: &'a [String], value: &str) -> Option<&'a String> {
    let wanted = plain(value);
    options.iter().find(|option| plain(option) == wanted)
}

/// What a model answered for a field, as the field would hold it. A Text
/// field gets one line; a Tags field its values one by one, in an option's
/// spelling where it is one; a Select field the option the answer names,
/// and the answer as it came when it names none or several, which `fits`
/// then refuses. A way of saying nothing (`NOTHING`) is an answer when
/// it is one of the field's options (有 | 无).
pub fn coerce(field: &FieldDef, raw: &[String]) -> Vec<String> {
    let said: Vec<String> = raw.iter().map(|value| value.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|value| !value.is_empty() && (spelled(&field.options, value).is_some() || !NOTHING.contains(&plain(value).as_str()))).collect();
    match field.kind {
        FieldKind::Text => if said.is_empty() { vec![] } else { vec![said.join("；")] },
        FieldKind::Tags => clean_strings(said.iter().flat_map(|value| split_list(value)).map(|value| spelled(&field.options, &value).cloned().unwrap_or(value))).into_iter().take(MAX_VALUES).collect(),
        FieldKind::Select => {
            let Some(answer) = said.first() else { return vec![] };
            if let Some(option) = spelled(&field.options, answer) { return vec![option.clone()] }
            // "scRNA-seq (10x Chromium)" names one option; "scRNA-seq and snRNA-seq" does not choose.
            let within = plain(answer);
            let named: Vec<&String> = field.options.iter().filter(|option| plain(option).chars().count() >= 3 && within.contains(&plain(option))).collect();
            match named[..] {
                [option] => vec![option.clone()],
                _ => vec![answer.clone()],
            }
        }
    }
}

/// The most values kept for a multi-value field from one answer.
const MAX_VALUES: usize = 12;

/// Whether a field can hold `values`: one line of text, one of a Select's
/// options, any values of a Tags field. Nothing at all fits no field.
pub fn fits(field: &FieldDef, values: &[String]) -> bool {
    match field.kind {
        FieldKind::Text => values.len() == 1,
        FieldKind::Tags => !values.is_empty(),
        FieldKind::Select => values.len() == 1 && field.options.contains(&values[0]),
    }
}

// ---- the prompt and its reading

const SYSTEM: &str = "你是文献信息提取助手。用户在整理一个文献库，会给出要填写的若干词条和一篇文献的信息（题目、书目信息、摘要，可能还有全文）。请为每个词条从文中找出这篇文献的取值。\n\
要求：\n\
- 只依据给出的内容，不补充文中没有的信息，不猜测。文中没有说明的词条，value 用 null。\n\
- 「文本」词条的 value 是一个简短的字符串。「多值」词条的 value 是字符串数组，一个值一项，每个值尽量短。「单选」词条的 value 必须是所列选项之一，原样照抄；都不合适就用 null。\n\
- 列出了常用取值的词条，文中的说法与其中某个是一个意思时，用那个写法。\n\
- 专有名词（物种学名、基因、方法、试剂、数据集的名字）照原文书写，不翻译。需要你自己组织语言的内容（例如概括）用{language}写。\n\
- excerpt 是支持这个取值的一句原文，必须逐字摘自给出的摘要或全文，不超过 300 字，不翻译、不改写、不把几处拼在一起；value 为 null 时留空字符串。\n\
只输出一个 JSON 对象，不要其他文字，键是词条的编号：{\"F1\": {\"value\": \"…\", \"excerpt\": \"…\"}, \"F2\": {\"value\": [\"…\", \"…\"], \"excerpt\": \"…\"}, \"F3\": {\"value\": null, \"excerpt\": \"\"}}";

/// The instructions and the question that ask what `fields` should hold
/// for one paper, and what the model gets to read: "fulltext", "abstract",
/// or "none" when there is neither and asking would be guessing.
/// `language` is the language for what the model words itself: 中文, English.
pub fn prompt(paper: &Paper, pages: Option<&[String]>, fields: &[FieldDef], language: &str) -> (String, String, &'static str) {
    let mut prompt = String::from("## 要填写的词条\n");
    for (n, field) in fields.iter().enumerate() {
        prompt.push_str(&format!("F{} {}（{}）", n + 1, field.name, field.kind.label()));
        if !field.hint.is_empty() { prompt.push_str(&format!("：{}", field.hint)) }
        match field.kind {
            FieldKind::Select => prompt.push_str(&format!(" 选项：{}", field.options.join(" | "))),
            FieldKind::Tags if !field.options.is_empty() => prompt.push_str(&format!(" 常用取值：{}", field.options.join("、"))),
            _ => {}
        }
        prompt.push('\n');
    }
    let (text, basis) = ai::paper_to_read(paper, pages);
    prompt.push_str(&text);
    (SYSTEM.replace("{language}", language), prompt, basis)
}

/// What the model said about one field of a paper, with its quotation checked.
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub field_id: String,
    /// As the field would hold it (`coerce`); empty when the paper does not say.
    pub values: Vec<String>,
    /// The quotation, when it really is in the abstract or the full text; empty otherwise.
    pub excerpt: String,
    pub page: Option<usize>,
}

/// The strings in what a model gave as a value: one, several, or none.
fn raw_values(value: &Value) -> Vec<String> {
    match value {
        Value::String(text) => vec![text.clone()],
        Value::Number(n) => vec![n.to_string()],
        Value::Array(items) => items.iter().flat_map(raw_values).collect(),
        _ => vec![],
    }
}

/// Read the reply to `prompt`: one answer for every field the reply speaks
/// of, by its number or by its name. A field it leaves out gets none and is
/// asked about again; a quotation that is not in the text the model was
/// given is dropped rather than shown as evidence.
pub fn parse(reply: &str, fields: &[FieldDef], paper: &Paper, pages: Option<&[String]>) -> Result<Vec<Answer>, String> {
    let value: Value = reply.find('{').zip(reply.rfind('}')).filter(|(start, end)| start < end).and_then(|(start, end)| serde_json::from_str(&reply[start..=end]).ok()).ok_or("模型的回答不是要求的格式。")?;
    // Some models wrap the answers: {"fields": {…}} or {"fields": [{"field": "F1", …}]}.
    let inner = ["fields", "answers", "result"].iter().map(|key| &value[*key]).find(|inner| inner.is_object() || inner.is_array()).unwrap_or(&value);
    let entry = |n: usize, field: &FieldDef| -> Option<&Value> {
        let number = format!("F{}", n + 1);
        match inner {
            Value::Object(map) => map.iter().find(|(key, _)| key.trim().eq_ignore_ascii_case(&number) || key.trim() == field.name).map(|(_, entry)| entry),
            Value::Array(items) => items.iter().find(|item| ["field", "id", "name", "key"].iter().filter_map(|key| item[*key].as_str()).any(|named| named.trim().eq_ignore_ascii_case(&number) || named.trim() == field.name)),
            _ => None,
        }
    };
    let mut answers = vec![];
    for (n, field) in fields.iter().enumerate() {
        let Some(entry) = entry(n, field) else { continue };
        // An answer is {"value": …, "excerpt": "…"}; a bare value is taken as one without a quotation.
        let (said, quote) = match entry {
            Value::Object(map) => (map.get("value").or(map.get("values")).unwrap_or(&Value::Null), map.get("excerpt").or(map.get("quote")).and_then(Value::as_str).unwrap_or("").trim()),
            bare => (bare, ""),
        };
        let values = coerce(field, &raw_values(said));
        let found = if values.is_empty() { None } else { ai::locate_quote(quote, paper, pages) };
        answers.push(Answer { field_id: field.id.clone(), values, excerpt: if found.is_some() { quote.to_string() } else { String::new() }, page: found.flatten() });
    }
    if answers.is_empty() { return Err("模型的回答里没有要填写的词条。".into()) }
    Ok(answers)
}

/// Which of `fields` a run asks about for one paper. A field the paper has
/// a value for is left alone, and so is one already asked about as it is
/// defined now, whatever came of it, unless that was read off the abstract
/// and the full text has been stored since. `again` asks about them all.
pub fn wanted<'a>(paper: &Paper, fields: &'a [FieldDef], proposals: &Proposals, again: bool) -> Vec<&'a FieldDef> {
    fields
        .iter()
        .filter(|field| {
            if again { return true }
            if paper.fields.get(&field.id).is_some_and(|held| !held.is_empty()) { return false }
            match proposals.get(&paper.id, &field.id) {
                Some(proposal) => proposal.asked != asked_as(field) || (proposal.basis == "abstract" && paper.fulltext == "stored"),
                None => true,
            }
        })
        .collect()
}

// ---- the person's side

/// One proposal waiting for the person, with the paper and the field it is for.
#[derive(Clone, Copy, Debug)]
pub struct Row<'a> {
    pub paper: &'a Paper,
    pub field: &'a FieldDef,
    pub proposal: &'a Proposal,
}

impl<'a> Row<'a> {
    pub fn key(&self) -> Key {
        (self.paper.id.clone(), self.field.id.clone())
    }

    /// What the field holds now.
    pub fn held(&self) -> &'a [String] {
        self.paper.fields.get(&self.field.id).map_or(&[], Vec::as_slice)
    }
}

/// The proposals waiting for the person: paper by paper in the order
/// given, each paper's in the order of the fields.
pub fn rows<'a>(papers: &'a [Paper], fields: &'a [FieldDef], proposals: &'a Proposals) -> Vec<Row<'a>> {
    papers.iter().flat_map(|paper| fields.iter().filter_map(move |field| proposals.get(&paper.id, &field.id).filter(|proposal| standing(proposal, field, paper) == Standing::Open).map(|proposal| Row { paper, field, proposal }))).collect()
}

/// One value to write: a paper, a field, and what the field will hold.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub paper_id: String,
    pub field_id: String,
    pub values: Vec<String>,
}

/// 提取's confirmation: the proposals looked over as one list. Every row is
/// ticked or not; what the person changed against the rule is kept here,
/// and nothing is written before they confirm.
#[derive(Clone, Debug, Default)]
pub struct Reviewing {
    /// The rows ticked or unticked against `ticked_by_rule`.
    flipped: BTreeSet<Key>,
    /// The option picked for a Select field whose answer was none of its options.
    chosen: BTreeMap<Key, String>,
    /// The field whose rows are on show; empty for all of them.
    pub part: String,
    /// The row the keys are on.
    pub cursor: Option<Key>,
}

impl Reviewing {
    /// What the row would write: the option the person picked, else what
    /// was proposed (`taken`: a multi-value field keeps what it has).
    pub fn values(&self, row: &Row) -> Vec<String> {
        match self.chosen.get(&row.key()) {
            Some(option) => vec![option.clone()],
            None => taken(row.field, row.held(), &row.proposal.values),
        }
    }

    /// Whether the row can be written as it stands: a Select answer that is
    /// none of the options cannot, until the person picks one.
    pub fn writable(&self, row: &Row) -> bool {
        fits(row.field, &self.values(row))
    }

    /// Ticked without the person's doing: a value the field can hold, for a
    /// field that is still empty, with a sentence of the paper behind it.
    pub fn ticked_by_rule(&self, row: &Row) -> bool {
        row.held().is_empty() && !row.proposal.excerpt.is_empty()
    }

    pub fn ticked(&self, row: &Row) -> bool {
        self.writable(row) && (self.ticked_by_rule(row) != self.flipped.contains(&row.key()))
    }

    fn set(&mut self, row: &Row, on: bool) {
        let key = row.key();
        if self.ticked_by_rule(row) == on { self.flipped.remove(&key); } else { self.flipped.insert(key); }
    }

    /// Tick or untick a row; one that cannot be written stays unticked.
    pub fn toggle(&mut self, row: &Row) {
        if !self.writable(row) { return }
        let on = !self.ticked(row);
        self.set(row, on);
    }

    pub fn tick_all(&mut self, rows: &[Row], on: bool) {
        for row in rows {
            if self.writable(row) { self.set(row, on) }
        }
    }

    /// Pick an option for a Select field: the row then writes it and is ticked.
    /// Picking the one already picked takes the choice back.
    pub fn choose(&mut self, row: &Row, option: &str) {
        if row.field.kind != FieldKind::Select || !row.field.options.iter().any(|o| o == option) { return }
        let key = row.key();
        if self.chosen.get(&key).is_some_and(|chosen| chosen == option) {
            self.chosen.remove(&key);
            self.flipped.remove(&key);
            return;
        }
        self.chosen.insert(key, option.to_string());
        self.set(row, true);
    }

    pub fn chosen(&self, row: &Row) -> Option<&str> {
        self.chosen.get(&row.key()).map(String::as_str)
    }

    /// The rows on show: those of the field picked, or all of them.
    pub fn on_show<'a>(&self, rows: Vec<Row<'a>>) -> Vec<Row<'a>> {
        if self.part.is_empty() || !rows.iter().any(|row| row.field.id == self.part) { return rows }
        rows.into_iter().filter(|row| row.field.id == self.part).collect()
    }

    /// What confirming writes: the ticked rows, each with its value.
    pub fn plan(&self, rows: &[Row]) -> Vec<Line> {
        rows.iter().filter(|row| self.ticked(row)).map(|row| Line { paper_id: row.paper.id.clone(), field_id: row.field.id.clone(), values: self.values(row) }).collect()
    }

    /// Forget what was changed for rows that are gone (written, set aside, asked again).
    pub fn settle(&mut self, rows: &[Row]) {
        let there: BTreeSet<Key> = rows.iter().map(Row::key).collect();
        self.flipped.retain(|key| there.contains(key));
        self.chosen.retain(|key, _| there.contains(key));
        if self.cursor.as_ref().is_some_and(|key| !there.contains(key)) { self.cursor = None }
    }
}

// ---- the store

fn row_to_proposal(r: &rusqlite::Row) -> rusqlite::Result<Proposal> {
    Ok(Proposal {
        paper_id: r.get(0)?,
        field_id: r.get(1)?,
        values: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default(),
        excerpt: r.get(3)?,
        page: r.get::<_, Option<i64>>(4)?.map(|p| p as usize),
        basis: r.get(5)?,
        author: r.get(6)?,
        asked: r.get(7)?,
        created_at: r.get(8)?,
        dismissed: !r.get::<_, String>(9)?.is_empty(),
    })
}

fn to_json(values: &[String]) -> String {
    serde_json::to_string(values).unwrap_or_else(|_| "[]".into())
}

impl Store {
    /// Record a proposal, replacing the earlier one for the same paper and
    /// field. The field's value is not touched.
    pub fn save_proposal(&mut self, proposal: &mut Proposal) -> Result<()> {
        self.db.query_row("SELECT 1 FROM papers WHERE id = ?1", [&proposal.paper_id], |_| Ok(())).optional()?.ok_or_else(|| anyhow!("文献不存在：{}", proposal.paper_id))?;
        self.db.query_row("SELECT 1 FROM field_defs WHERE id = ?1", [&proposal.field_id], |_| Ok(())).optional()?.ok_or_else(|| anyhow!("词条不存在：{}", proposal.field_id))?;
        proposal.values = clean_strings(proposal.values.iter());
        proposal.excerpt = proposal.excerpt.trim().to_string();
        proposal.created_at = now_iso();
        proposal.dismissed = false;
        self.db.execute(
            "INSERT INTO field_proposals(paper_id, field_id, values_json, excerpt, page, basis, author, asked, created_at, dismissed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, '') \
             ON CONFLICT(paper_id, field_id) DO UPDATE SET values_json = excluded.values_json, excerpt = excluded.excerpt, page = excluded.page, basis = excluded.basis, author = excluded.author, asked = excluded.asked, created_at = excluded.created_at, dismissed_at = ''",
            params![proposal.paper_id, proposal.field_id, to_json(&proposal.values), proposal.excerpt, proposal.page.map(|p| p as i64), proposal.basis, proposal.author, proposal.asked, proposal.created_at],
        )?;
        Ok(())
    }

    /// Every proposal, for papers in the trash too.
    pub fn list_proposals(&self) -> Result<Vec<Proposal>> {
        let mut stmt = self.db.prepare("SELECT paper_id, field_id, values_json, excerpt, page, basis, author, asked, created_at, dismissed_at FROM field_proposals ORDER BY created_at, paper_id, field_id")?;
        let proposals = stmt.query_map([], row_to_proposal)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(proposals)
    }

    /// Set proposals aside, or bring them back: they stay in the table, so
    /// a run does not ask the same thing again.
    pub fn dismiss_proposals(&mut self, keys: &[Key], dismissed: bool) -> Result<()> {
        let tx = self.db.transaction()?;
        let at = if dismissed { now_iso() } else { String::new() };
        for (paper_id, field_id) in keys {
            tx.execute("UPDATE field_proposals SET dismissed_at = ?3 WHERE paper_id = ?1 AND field_id = ?2", params![paper_id, field_id, at])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Write what fields hold for several papers in one transaction: all of
    /// it or none. Each is a paper, a field and its values (none empties
    /// the field). A Select field takes one of its options and nothing else.
    pub fn set_fields(&mut self, lines: &[(String, String, Vec<String>)]) -> Result<()> {
        let fields = self.list_fields()?;
        let tx = self.db.transaction()?;
        let now = now_iso();
        for (paper_id, field_id, values) in lines {
            let field = fields.iter().find(|field| field.id == *field_id).ok_or_else(|| anyhow!("词条不存在：{field_id}"))?;
            let values = clean_strings(values.iter());
            if !values.is_empty() && !fits(field, &values) {
                match field.kind {
                    FieldKind::Select => bail!("「{}」是单选词条，取值应为 {} 之一：{}", field.name, field.options.join("、"), values.join("、")),
                    _ => bail!("「{}」只能有一个值：{}", field.name, values.join("、")),
                }
            }
            if tx.execute("UPDATE papers SET updated_at = ?2 WHERE id = ?1", params![paper_id, now])? == 0 { bail!("没有找到这篇文献：{paper_id}") }
            if values.is_empty() {
                tx.execute("DELETE FROM paper_fields WHERE paper_id = ?1 AND field_id = ?2", params![paper_id, field_id])?;
            } else {
                tx.execute("INSERT INTO paper_fields(paper_id, field_id, values_json) VALUES (?1, ?2, ?3) ON CONFLICT(paper_id, field_id) DO UPDATE SET values_json = excluded.values_json", params![paper_id, field_id, to_json(&values)])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(id: &str, name: &str, kind: FieldKind, options: &[&str]) -> FieldDef {
        FieldDef { id: id.into(), name: name.into(), kind, options: options.iter().map(|o| o.to_string()).collect(), ..Default::default() }
    }

    fn fields() -> Vec<FieldDef> {
        vec![field("F-species", "物种", FieldKind::Tags, &["Arabidopsis thaliana"]), field("F-method", "方法", FieldKind::Select, &["scRNA-seq", "snRNA-seq"]), FieldDef { hint: SUMMARY_HINT.into(), ..field("F-summary", SUMMARY, FieldKind::Text, &[]) }]
    }

    fn paper() -> Paper {
        Paper { id: "P-1".into(), title: "A root atlas".into(), authors: "Zhang Wei".into(), year: Some(2021), abstract_text: "We profiled root tips of Arabidopsis thaliana. WOX5 maintains the quiescent centre.".into(), fulltext: "stored".into(), ..Default::default() }
    }

    fn pages() -> Vec<String> {
        vec!["Introduction. Roots grow.".into(), "Methods. Libraries were prepared with 10x Genomics\nChromium for single-cell RNA sequencing.".into()]
    }

    #[test]
    fn the_prompt_names_every_field() {
        let (system, prompt, basis) = prompt(&paper(), Some(&pages()), &fields(), "中文");
        assert!(system.contains("用中文写") && !system.contains("{language}"));
        assert!(prompt.contains("F1 物种（多值） 常用取值：Arabidopsis thaliana\n"), "{prompt}");
        assert!(prompt.contains("F2 方法（单选） 选项：scRNA-seq | snRNA-seq\n"), "{prompt}");
        assert!(prompt.contains(&format!("F3 要点（文本）：{SUMMARY_HINT}\n")), "{prompt}");
        assert!(prompt.contains("题目：A root atlas") && prompt.contains("[p.2]"));
        assert_eq!(basis, "fulltext");
        assert_eq!(super::prompt(&paper(), None, &fields(), "English").2, "abstract");
        assert_eq!(super::prompt(&Paper { title: "Only a title".into(), ..Default::default() }, None, &fields(), "English").2, "none");
    }

    #[test]
    fn replies_are_read_leniently() {
        let (fields, paper, pages) = (fields(), paper(), pages());
        let reply = "```json\n{\"F1\": {\"value\": [\"arabidopsis  thaliana\", \"Oryza sativa, Zea mays\"], \"excerpt\": \"We profiled root tips of Arabidopsis thaliana.\"}, \"f2\": {\"value\": \"scRNA-seq (10x Chromium)\", \"excerpt\": \"Libraries were prepared with 10x Genomics Chromium\"}, \"要点\": {\"value\": \"WOX5 维持静止中心。\", \"excerpt\": \"This sentence is not in the paper at all.\"}}\n```";
        let answers = parse(reply, &fields, &paper, Some(&pages)).unwrap();
        assert_eq!(answers[0], Answer { field_id: "F-species".into(), values: vec!["Arabidopsis thaliana".into(), "Oryza sativa".into(), "Zea mays".into()], excerpt: "We profiled root tips of Arabidopsis thaliana.".into(), page: None });
        // The line break and the hyphenation of the page text do not hide the quotation; the page is where it was found.
        assert_eq!(answers[1], Answer { field_id: "F-method".into(), values: vec!["scRNA-seq".into()], excerpt: "Libraries were prepared with 10x Genomics Chromium".into(), page: Some(2) });
        // A quotation the paper does not hold is not kept as evidence.
        assert_eq!(answers[2], Answer { field_id: "F-summary".into(), values: vec!["WOX5 维持静止中心。".into()], excerpt: String::new(), page: None });

        // Wrapped, as a list, with bare values; nothing said in several ways; a field left out gets no answer.
        let wrapped = parse(r#"{"fields": [{"field": "F2", "value": null, "excerpt": "WOX5 maintains the quiescent centre."}, {"name": "物种", "value": "未提及"}]}"#, &fields, &paper, None).unwrap();
        assert_eq!(wrapped, vec![Answer { field_id: "F-species".into(), values: vec![], excerpt: String::new(), page: None }, Answer { field_id: "F-method".into(), values: vec![], excerpt: String::new(), page: None }]);
        let bare = parse(r#"Here you go: {"F3": "Roots  have\n a niche.", "F1": ["N/A"]}"#, &fields, &paper, None).unwrap();
        assert_eq!(bare.iter().map(|a| a.values.clone()).collect::<Vec<_>>(), vec![vec![], vec!["Roots have a niche.".to_string()]]);
        for bad in ["I cannot answer.", "{}", r#"{"F9": {"value": "x"}}"#, "{not json}"] {
            assert!(parse(bad, &fields, &paper, None).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_select_answer_is_an_option_or_is_not_written() {
        let fields = fields();
        let method = &fields[1];
        assert_eq!(coerce(method, &["SCRNA seq".into()]), vec!["scRNA-seq"]);
        assert_eq!(coerce(method, &["single-nucleus (snRNA-seq)".into()]), vec!["snRNA-seq"]);
        // Two options named, or none: left as it was said, for the person to look at.
        let both = coerce(method, &["scRNA-seq and snRNA-seq".into()]);
        assert_eq!(both, vec!["scRNA-seq and snRNA-seq"]);
        assert!(!fits(method, &both));
        let other = coerce(method, &["Smart-seq2".into()]);
        assert!(!fits(method, &other) && fits(method, &["snRNA-seq".into()]) && !fits(method, &[]));
        assert!(coerce(method, &["None".into()]).is_empty());
        // A way of saying nothing is an answer where it is one of the options.
        let asked = field("F-control", "对照", FieldKind::Select, &["有", "无", "未知"]);
        assert_eq!((coerce(&asked, &["无".into()]), coerce(&asked, &["未知".into()]), coerce(&asked, &["N/A".into()])), (vec!["无".to_string()], vec!["未知".to_string()], vec![]));
        // Text is one line, whatever the model gave; tags are split and take an option's spelling.
        assert_eq!(coerce(&fields[2], &["a".into(), " b ".into()]), vec!["a；b"]);
        assert_eq!(coerce(&fields[0], &["ARABIDOPSIS THALIANA; rice".into(), "rice".into()]), vec!["Arabidopsis thaliana", "rice"]);
        assert!(fits(&fields[0], &["rice".into()]) && fits(&fields[2], &["a".into()]) && !fits(&fields[2], &["a".into(), "b".into()]));
    }

    fn store_with(dir: &str) -> (std::path::PathBuf, Store, Vec<FieldDef>, Paper, Paper) {
        let dir = std::env::temp_dir().join(format!("wisp-extract-{dir}-{}", crate::model::new_id("T")));
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = Store::in_memory(&dir).unwrap();
        let mut fields = fields();
        for field in &mut fields {
            field.id.clear();
            store.save_field(field).unwrap();
        }
        let a = store.add_paper(Paper { id: String::new(), fulltext: String::new(), ..paper() }).unwrap();
        let b = store.add_paper(Paper { title: "Leaf development".into(), abstract_text: "Leaves of Zea mays were sampled.".into(), ..Default::default() }).unwrap();
        (dir, store, fields, a, b)
    }

    #[test]
    fn proposals_do_not_fill_fields() {
        let (dir, mut store, fields, a, _) = store_with("propose");
        let (species, method) = (&fields[0], &fields[1]);
        let mut first = Proposal { paper_id: a.id.clone(), field_id: species.id.clone(), values: vec![" Arabidopsis thaliana ".into(), "".into()], excerpt: " root tips of Arabidopsis thaliana ".into(), basis: "abstract".into(), author: "mock".into(), asked: asked_as(species), ..Default::default() };
        store.save_proposal(&mut first).unwrap();
        assert_eq!((first.values.clone(), first.excerpt.as_str(), first.created_at.is_empty()), (vec!["Arabidopsis thaliana".to_string()], "root tips of Arabidopsis thaliana", false));
        // The paper is as it was: only the person fills a field.
        let paper = store.get_paper(&a.id).unwrap();
        assert!(paper.fields.is_empty());
        assert_eq!(standing(&first, species, &paper), Standing::Open);
        // A new one replaces the old; one set aside comes back with a new answer.
        store.dismiss_proposals(&[(a.id.clone(), species.id.clone())], true).unwrap();
        assert!(store.list_proposals().unwrap()[0].dismissed);
        assert_eq!(standing(&store.list_proposals().unwrap()[0], species, &paper), Standing::Dismissed);
        store.dismiss_proposals(&[(a.id.clone(), species.id.clone())], false).unwrap();
        assert!(!store.list_proposals().unwrap()[0].dismissed);
        store.save_proposal(&mut Proposal { paper_id: a.id.clone(), field_id: species.id.clone(), values: vec!["Oryza sativa".into()], page: Some(3), asked: asked_as(species), ..Default::default() }).unwrap();
        let all = store.list_proposals().unwrap();
        assert_eq!((all.len(), all[0].values.clone(), all[0].page), (1, vec!["Oryza sativa".to_string()], Some(3)));
        // Nothing found, and a field defined otherwise since.
        let silent = Proposal { paper_id: a.id.clone(), field_id: method.id.clone(), asked: asked_as(method), ..Default::default() };
        assert_eq!(standing(&silent, method, &paper), Standing::Silent);
        let renamed = FieldDef { hint: "只算主要的物种".into(), ..species.clone() };
        assert_eq!(standing(&all[0], &renamed, &paper), Standing::Stale);
        for bad in [Proposal { paper_id: "P-none".into(), field_id: species.id.clone(), ..Default::default() }, Proposal { paper_id: a.id.clone(), field_id: "F-none".into(), ..Default::default() }] {
            assert!(store.save_proposal(&mut bad.clone()).is_err(), "{bad:?}");
        }
        // They go with the paper and with the field.
        store.save_proposal(&mut Proposal { paper_id: a.id.clone(), field_id: method.id.clone(), values: vec!["scRNA-seq".into()], ..Default::default() }).unwrap();
        store.delete_field(&method.id).unwrap();
        assert_eq!(store.list_proposals().unwrap().len(), 1);
        store.delete_paper(&a.id).unwrap();
        assert!(store.list_proposals().unwrap().is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn fields_are_written_together_and_taken_back() {
        let (dir, mut store, fields, a, b) = store_with("apply");
        let (species, method, summary) = (&fields[0], &fields[1], &fields[2]);
        let line = |paper: &Paper, field: &FieldDef, values: &[&str]| (paper.id.clone(), field.id.clone(), values.iter().map(|v| v.to_string()).collect::<Vec<_>>());
        store.set_fields(&[line(&a, species, &["Arabidopsis thaliana", " Oryza sativa "]), line(&a, method, &["scRNA-seq"]), line(&b, summary, &["叶的发育。"])]).unwrap();
        let (held_a, held_b) = (store.get_paper(&a.id).unwrap(), store.get_paper(&b.id).unwrap());
        assert_eq!((held_a.fields[&species.id].clone(), held_a.fields[&method.id].clone(), held_b.fields[&summary.id].clone()), (vec!["Arabidopsis thaliana".to_string(), "Oryza sativa".to_string()], vec!["scRNA-seq".to_string()], vec!["叶的发育。".to_string()]));
        // All or none: a value that is not an option, a paper that is not there, two lines for a text field.
        for bad in [line(&b, method, &["Smart-seq2"]), line(&Paper { id: "P-none".into(), ..Default::default() }, species, &["x"]), line(&b, summary, &["a", "b"]), (b.id.clone(), "F-none".to_string(), vec!["x".to_string()])] {
            assert!(store.set_fields(&[line(&b, species, &["Zea mays"]), bad.clone()]).is_err(), "{bad:?}");
            assert!(!store.get_paper(&b.id).unwrap().fields.contains_key(&species.id));
        }
        // Taking it back is writing what was there before: nothing, or the earlier value.
        store.set_fields(&[line(&a, species, &[]), line(&a, method, &["snRNA-seq"]), line(&b, summary, &[])]).unwrap();
        let back = store.get_paper(&a.id).unwrap();
        assert_eq!((back.fields.contains_key(&species.id), back.fields[&method.id].clone(), store.get_paper(&b.id).unwrap().fields.is_empty()), (false, vec!["snRNA-seq".to_string()], true));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_run_asks_what_is_missing() {
        let fields = fields();
        let mut paper = paper();
        let mut proposals = Proposals::default();
        let names = |wanted: Vec<&FieldDef>| wanted.iter().map(|f| f.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(wanted(&paper, &fields, &proposals, false)), ["物种", "方法", "要点"]);
        // A field with a value is left alone; so is one asked about before, whatever came of it.
        paper.fields.insert("F-species".into(), vec!["Arabidopsis thaliana".into()]);
        proposals.put(Proposal { paper_id: "P-1".into(), field_id: "F-method".into(), basis: "fulltext".into(), asked: asked_as(&fields[1]), ..Default::default() });
        assert_eq!(names(wanted(&paper, &fields, &proposals, false)), ["要点"]);
        assert_eq!(names(wanted(&paper, &fields, &proposals, true)), ["物种", "方法", "要点"]);
        // Read off the abstract, and the full text is there now; or the field is defined otherwise.
        proposals.put(Proposal { paper_id: "P-1".into(), field_id: "F-summary".into(), basis: "abstract".into(), asked: asked_as(&fields[2]), ..Default::default() });
        assert_eq!(names(wanted(&paper, &fields, &proposals, false)), ["要点"]);
        paper.fulltext = "linked".into();
        assert!(wanted(&paper, &fields, &proposals, false).is_empty());
        let mut redefined = fields.clone();
        redefined[1].options.push("Smart-seq2".into());
        assert_eq!(names(wanted(&paper, &redefined, &proposals, false)), ["方法"]);
    }

    #[test]
    fn the_person_confirms() {
        let fields = fields();
        let mut a = paper();
        let b = Paper { id: "P-2".into(), title: "Leaf".into(), ..Default::default() };
        a.fields.insert("F-summary".into(), vec!["我自己写的。".into()]);
        let propose = |paper: &str, n: usize, values: &[&str], excerpt: &str| Proposal { paper_id: paper.into(), field_id: fields[n].id.clone(), values: values.iter().map(|v| v.to_string()).collect(), excerpt: excerpt.into(), asked: asked_as(&fields[n]), ..Default::default() };
        let mut proposals = Proposals::new(vec![propose("P-1", 0, &["Arabidopsis thaliana"], "root tips of Arabidopsis thaliana"), propose("P-1", 1, &["Smart-seq2"], "Libraries were prepared"), propose("P-1", 2, &["WOX5 维持静止中心。"], "WOX5 maintains the quiescent centre."), propose("P-2", 0, &["Zea mays"], ""), propose("P-2", 1, &[], "")]);
        // Held already, and set aside: neither waits.
        proposals.put(Proposal { dismissed: true, ..propose("P-2", 2, &["叶。"], "") });
        let papers = vec![a, b];
        let all = rows(&papers, &fields, &proposals);
        assert_eq!(all.iter().map(|row| (row.paper.id.as_str(), row.field.name.as_str())).collect::<Vec<_>>(), [("P-1", "物种"), ("P-1", "方法"), ("P-1", "要点"), ("P-2", "物种")]);

        let mut reviewing = Reviewing::default();
        // Ticked: backed by a sentence, for an empty field. Not: none of the options, another value held, no sentence.
        assert_eq!(all.iter().map(|row| reviewing.ticked(row)).collect::<Vec<_>>(), [true, false, false, false]);
        assert!(!reviewing.writable(&all[1]));
        reviewing.toggle(&all[1]);
        assert!(!reviewing.ticked(&all[1]));
        // An option picked makes the row writable and ticks it; picked again, it is as before.
        reviewing.choose(&all[1], "nothing like it");
        reviewing.choose(&all[1], "snRNA-seq");
        assert_eq!((reviewing.ticked(&all[1]), reviewing.values(&all[1]), reviewing.chosen(&all[1])), (true, vec!["snRNA-seq".to_string()], Some("snRNA-seq")));
        reviewing.toggle(&all[0]);
        reviewing.toggle(&all[2]);
        let plan = reviewing.plan(&all);
        assert_eq!(plan, vec![Line { paper_id: "P-1".into(), field_id: "F-method".into(), values: vec!["snRNA-seq".into()] }, Line { paper_id: "P-1".into(), field_id: "F-summary".into(), values: vec!["WOX5 维持静止中心。".into()] }]);
        reviewing.choose(&all[1], "snRNA-seq");
        assert!(!reviewing.ticked(&all[1]) && reviewing.chosen(&all[1]).is_none());

        reviewing.tick_all(&all, true);
        assert_eq!(all.iter().map(|row| reviewing.ticked(row)).collect::<Vec<_>>(), [true, false, true, true]);
        reviewing.tick_all(&all, false);
        assert!(reviewing.plan(&all).is_empty());

        // A multi-value field keeps what it has: the proposed values come after, and those it has already do not wait.
        let mut tagged = papers[1].clone();
        tagged.fields.insert("F-species".into(), vec!["Oryza sativa".into()]);
        let more = vec![tagged];
        let adding = rows(&more, &fields, &proposals);
        assert_eq!((adding.len(), reviewing.values(&adding[0]), reviewing.ticked_by_rule(&adding[0])), (1, vec!["Oryza sativa".to_string(), "Zea mays".to_string()], false));
        let mut has_it = more[0].clone();
        has_it.fields.insert("F-species".into(), vec!["Zea mays".into(), "Oryza sativa".into()]);
        assert_eq!(standing(proposals.get("P-2", "F-species").unwrap(), &fields[0], &has_it), Standing::Held);
        assert_eq!(taken(&fields[1], &["scRNA-seq".to_string()], &["snRNA-seq".to_string()]), ["snRNA-seq"]);

        // One field on show; a field without rows shows them all.
        reviewing.part = "F-species".into();
        assert_eq!(reviewing.on_show(all.clone()).len(), 2);
        reviewing.part = "F-gone".into();
        assert_eq!(reviewing.on_show(all.clone()).len(), 4);
        // What was changed for a row that is gone is forgotten.
        reviewing.toggle(&all[3]);
        reviewing.cursor = Some(all[3].key());
        reviewing.settle(&all[..3]);
        assert!(reviewing.cursor.is_none() && !reviewing.flipped.contains(&all[3].key()) && reviewing.flipped.contains(&all[0].key()));
    }
}
