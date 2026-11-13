//! 提取: the assistant reads papers and proposes what the library's custom
//! fields should hold for each, every value with a sentence of the paper
//! behind it (`wisp_core::extract`). It is started for the papers marked in
//! the list or for the list on show, after a dialog that says which fields
//! to ask about; the run is like the screening one (several papers side by
//! side, stoppable, a paper without an answer does not end it). What it
//! proposes is looked over on this page, a row per paper and field, and the
//! ticked rows are written together: nothing is written before that, and
//! what was written can be taken back. In the detail panel a sparkle by a
//! field asks about that one field of that paper; the answer waits under
//! the field for 采纳, and stays there afterwards as the sentence the value
//! rests on.

use crate::app::{Cmd, Library, Modal, Page, Tab, Workspace};
use crate::context::Nav;
use crate::i18n::t;
use crate::theme::Theme;
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use wisp_core::ai;
use wisp_core::extract::{self, Answer, Key, Proposal, Proposals, Reviewing, Row, Standing};
use wisp_core::model::{FieldDef, FieldKind, Paper};
use wisp_core::settings::Task;

/// The height of a row of the list: the value, the sentence behind it, the paper.
const ROW_H: f32 = 78.;
/// So many papers in a row without a usable answer end a run.
const GIVE_UP: usize = 3;
/// A Select field with more options than this shows them only where one has to be picked.
const OPTIONS_SHOWN: usize = 5;

/// Everything 提取 does, as one command (`Cmd::Extract`).
#[derive(Clone, Debug, PartialEq)]
pub enum Do {
    /// The dialog that starts a run: for these papers, or for the list on show.
    Open(Option<Vec<String>>),
    /// In the dialog: ask about a field or not, ask again where there is a value or an answer, add 要点.
    PickField(String),
    Again,
    AddSummary,
    Stop,
    /// On the page: tick a row, pick an option for it, show one field's rows, tick them all or none, move to a row.
    Tick(Key),
    Choose(Key, String),
    Part(String),
    TickAll(bool),
    Cursor(Key),
    /// The paper of the row the keys are on, beside the list; again puts it away.
    Beside,
    /// Write the ticked rows on show; take the last writing or setting aside back.
    Apply,
    Undo,
    /// Set proposals aside: these, or the unticked rows on show.
    Dismiss(Option<Vec<Key>>),
    /// The sparkle by a field of a paper: ask about that one field.
    Ask(Key),
    /// 采纳 under a field of a paper: write that one proposal.
    Take(Key),
}

/// The dialog that starts a run.
pub struct Form {
    /// The papers to read, in the order they are listed.
    pub papers: Vec<String>,
    /// They were marked in the list, rather than being the list on show.
    pub marked: bool,
    /// The fields asked about.
    pub fields: Vec<String>,
    /// Ask also where a field has a value or was asked about before.
    pub again: bool,
}

/// What the page last did, until it is left: said in a line, with the way back.
pub enum Last {
    /// Each field as it was before, the proposals set aside because the person picked another value, and how many papers it was.
    Wrote { before: Vec<(String, String, Vec<String>)>, set_aside: Vec<Key>, papers: usize },
    SetAside(Vec<Key>),
}

/// 提取 in an open library: the proposals, the run over the papers, and the list being looked over.
pub struct Extract {
    pub proposals: Proposals,
    /// The papers still to read in this run with the fields to ask about for each, last first.
    todo: Vec<(String, Vec<String>)>,
    /// The papers being read right now, in the order they were started.
    pub reading: Vec<String>,
    pub total: usize,
    pub done: usize,
    /// Papers of the run the assistant gave no usable answer for.
    pub failed: usize,
    /// Papers with neither an abstract nor a full text: not asked about.
    pub unread: usize,
    /// Values proposed by this run.
    pub found: usize,
    streak: usize,
    /// The model the run was started with.
    pub model: String,
    pub error: String,
    /// Bumped when a run starts or stops, so answers to an earlier one are dropped.
    run: u64,
    /// The fields the last run asked about: ticked again in the next dialog.
    asked: Vec<String>,
    pub review: Reviewing,
    pub last: Option<Last>,
    pub scroll: UniformListScrollHandle,
    /// The fields of papers asked about one by one from the detail panel, and what went wrong with such a question.
    pub asking: HashSet<Key>,
    pub ask_errors: HashMap<Key, String>,
}

impl Extract {
    pub fn new() -> Extract {
        Extract { proposals: Proposals::default(), todo: vec![], reading: vec![], total: 0, done: 0, failed: 0, unread: 0, found: 0, streak: 0, model: String::new(), error: String::new(), run: 0, asked: vec![], review: Reviewing::default(), last: None, scroll: UniformListScrollHandle::new(), asking: HashSet::new(), ask_errors: HashMap::new() }
    }

    /// Whether the assistant is working through the papers right now.
    pub fn running(&self) -> bool {
        !self.reading.is_empty() || !self.todo.is_empty()
    }
}

/// The language for what the model words itself (a summary): the window's.
fn language() -> &'static str {
    if crate::i18n::is_english() { "English" } else { "中文" }
}

/// Whether there is anything of a paper to read: an abstract, or a stored full text.
fn readable(paper: &Paper) -> bool {
    !paper.abstract_text.trim().is_empty() || paper.fulltext == "stored"
}

/// What was read, in words.
fn basis(proposal: &Proposal) -> &'static str {
    match proposal.basis.as_str() {
        "fulltext" => t("读了全文"),
        "abstract" => t("只读了摘要"),
        _ => t("没有可读的内容"),
    }
}

pub(crate) fn tick_box(ticked: bool, able: bool) -> Div {
    div().flex().flex_none().items_center().justify_center().size(px(16.)).rounded(px(4.)).border_1().when(ticked, |d| d.bg(Theme::accent()).border_color(Theme::accent()).child(ui::icon("check", 11., Theme::on_accent()))).when(!ticked, |d| d.border_color(Theme::line_strong()).bg(Theme::bg_body())).when(!able, |d| d.opacity(0.4))
}

/// The values of a field: chips for a multi-value field, a line of text otherwise.
fn values_view(field: &FieldDef, values: &[String]) -> Div {
    if field.kind == FieldKind::Tags { return div().flex().flex_1().min_w_0().gap(px(4.)).overflow_hidden().children(values.iter().map(|value| ui::chip(value.clone(), Theme::text(), Theme::bg_sunken()))) }
    div().flex_1().min_w_0().truncate().child(values.join("；"))
}

impl Library {
    /// The proposals waiting for the person, paper by paper.
    pub fn extract_rows(&self) -> Vec<Row<'_>> {
        extract::rows(&self.papers, &self.fields, &self.extract.proposals)
    }

    /// The waiting proposal for a field of a paper.
    fn extract_row(&self, key: &Key) -> Option<Row<'_>> {
        let (paper, field) = (self.paper(&key.0)?, self.fields.iter().find(|field| field.id == key.1)?);
        let proposal = self.extract.proposals.get(&key.0, &key.1).filter(|proposal| extract::standing(proposal, field, paper) == Standing::Open)?;
        Some(Row { paper, field, proposal })
    }

    /// After the papers, the fields or the proposals changed: what was ticked for rows that are gone is forgotten.
    pub fn extract_settle(&mut self) {
        let mut review = std::mem::take(&mut self.extract.review);
        review.settle(&self.extract_rows());
        self.extract.review = review;
    }

    /// Which papers a run reads, each with the fields it asks about.
    fn extract_plan(&self, papers: &[String], fields: &[String], again: bool) -> Vec<(String, Vec<String>)> {
        let fields: Vec<FieldDef> = self.fields.iter().filter(|field| fields.contains(&field.id)).cloned().collect();
        papers.iter().filter_map(|id| self.paper(id)).filter(|paper| readable(paper)).filter_map(|paper| {
            let wanted: Vec<String> = extract::wanted(paper, &fields, &self.extract.proposals, again).into_iter().map(|field| field.id.clone()).collect();
            (!wanted.is_empty()).then(|| (paper.id.clone(), wanted))
        }).collect()
    }

    /// Whether the library has the field that sums a paper up, under its name in either language.
    fn has_summary(&self) -> bool {
        self.fields.iter().any(|field| field.name == extract::SUMMARY || field.name == t(extract::SUMMARY))
    }

    /// Keep what the model said about a field of a paper as a proposal.
    fn keep_answer(&mut self, field: &FieldDef, paper_id: &str, answer: Answer, basis: &str, author: &str) -> Result<bool, String> {
        let mut proposal = Proposal { paper_id: paper_id.to_string(), field_id: field.id.clone(), values: answer.values, excerpt: answer.excerpt, page: answer.page, basis: basis.to_string(), author: author.to_string(), asked: extract::asked_as(field), ..Default::default() };
        self.store.save_proposal(&mut proposal).map_err(|err| format!("{err:#}"))?;
        let found = !proposal.values.is_empty();
        self.extract.proposals.put(proposal);
        Ok(found)
    }
}

/// Change what is ticked, with the library at hand to find the rows in.
fn review<R>(lib: &mut Library, change: impl FnOnce(&mut Reviewing, &Library) -> R) -> R {
    let mut review = std::mem::take(&mut lib.extract.review);
    let out = change(&mut review, lib);
    lib.extract.review = review;
    out
}

impl Workspace {
    pub(crate) fn run_extract(&mut self, what: Do, window: &mut Window, cx: &mut Context<Self>) {
        match what {
            Do::Open(ids) => self.extract_dialog(ids, window, cx),
            Do::PickField(id) => {
                let Some(Modal::Extract(form)) = self.modal.as_mut() else { return };
                match form.fields.iter().position(|field| *field == id) {
                    Some(at) => { form.fields.remove(at); }
                    None => form.fields.push(id),
                }
                self.modal_error.clear();
            }
            Do::Again => {
                if let Some(Modal::Extract(form)) = self.modal.as_mut() { form.again = !form.again }
                self.modal_error.clear();
            }
            Do::AddSummary => {
                let mut field = FieldDef { name: t(extract::SUMMARY).into(), kind: FieldKind::Text, hint: t(extract::SUMMARY_HINT).into(), in_table: true, ..Default::default() };
                if !self.change(cx, |lib| lib.store.save_field(&mut field)) { return }
                if let Some(Modal::Extract(form)) = self.modal.as_mut() { form.fields.push(field.id.clone()) }
            }
            Do::Stop => {
                if let Some(lib) = self.lib.as_mut() {
                    // Answers still on their way belong to a run that is over.
                    lib.extract.run += 1;
                    lib.extract.todo.clear();
                    lib.extract.reading.clear();
                }
            }
            Do::Tick(key) => {
                let Some(lib) = self.lib.as_mut() else { return };
                review(lib, |review, lib| {
                    if let Some(row) = lib.extract_row(&key) { review.toggle(&row) }
                    review.cursor = Some(key);
                });
            }
            Do::Choose(key, option) => {
                let Some(lib) = self.lib.as_mut() else { return };
                review(lib, |review, lib| {
                    if let Some(row) = lib.extract_row(&key) { review.choose(&row, &option) }
                    review.cursor = Some(key);
                });
            }
            Do::Part(part) => {
                let Some(lib) = self.lib.as_mut() else { return };
                lib.extract.review.part = part;
                lib.extract.scroll = UniformListScrollHandle::new();
            }
            Do::TickAll(on) => {
                let Some(lib) = self.lib.as_mut() else { return };
                review(lib, |review, lib| {
                    let rows = review.on_show(lib.extract_rows());
                    review.tick_all(&rows, on);
                });
            }
            Do::Cursor(key) => {
                let Some(lib) = self.lib.as_mut() else { return };
                lib.extract.review.cursor = Some(key.clone());
                // A paper open beside the list follows the row.
                if lib.selected.is_some() { self.select(Some(key.0), window, cx) }
            }
            Do::Beside => {
                let Some(lib) = self.lib.as_ref() else { return };
                let Some((id, _)) = lib.extract.review.cursor.clone() else { return };
                if lib.selected.as_deref() == Some(id.as_str()) { return self.select(None, window, cx) }
                let tab = if lib.pdf_file(&id).is_some() { Tab::Pdf } else { Tab::Info };
                self.run(Cmd::OpenPaper(id, tab), window, cx);
            }
            Do::Apply => self.extract_apply(cx),
            Do::Undo => self.extract_undo(cx),
            Do::Dismiss(keys) => {
                let Some(lib) = self.lib.as_ref() else { return };
                let keys = keys.unwrap_or_else(|| {
                    let review = &lib.extract.review;
                    review.on_show(lib.extract_rows()).iter().filter(|row| !review.ticked(row)).map(Row::key).collect()
                });
                if keys.is_empty() { return }
                let on_page = lib.page == Page::Extract;
                if !self.change(cx, |lib| lib.store.dismiss_proposals(&keys, true)) { return }
                // On the page it is said in a line with the way back; under a field of a paper it is just gone.
                if on_page && let Some(lib) = self.lib.as_mut() { lib.extract.last = Some(Last::SetAside(keys)) }
            }
            Do::Ask(key) => self.extract_ask(key, cx),
            Do::Take(key) => {
                let Some(row) = self.lib.as_ref().and_then(|lib| lib.extract_row(&key)) else { return };
                if !extract::fits(row.field, &row.proposal.values) { return }
                let (line, name) = ((key.0.clone(), key.1.clone(), extract::taken(row.field, row.held(), &row.proposal.values)), row.field.name.clone());
                if self.change(cx, |lib| lib.store.set_fields(&[line])) { self.say(tf!("已写入「{name}」。", "Saved “{name}”."), cx) }
            }
        }
        cx.notify();
    }

    /// The dialog that starts a run: for the papers given, or for the list on show.
    fn extract_dialog(&mut self, ids: Option<Vec<String>>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_ref() else { return };
        // One run at a time: while one is under way, the page that shows it.
        if lib.extract.running() { return self.run(Cmd::SetPage(Page::Extract), window, cx) }
        let marked = ids.is_some();
        let papers = ids.unwrap_or_else(|| lib.shown.iter().map(|&i| lib.papers[i].id.clone()).collect());
        // The fields of the last run, while they are still there; else all of them.
        let mut fields: Vec<String> = lib.fields.iter().map(|field| field.id.clone()).filter(|id| lib.extract.asked.contains(id)).collect();
        if fields.is_empty() { fields = lib.fields.iter().map(|field| field.id.clone()).collect() }
        self.modal_subscriptions.clear();
        self.modal_error.clear();
        self.modal = Some(Modal::Extract(Box::new(Form { papers, marked, fields, again: false })));
    }

    pub(crate) fn submit_extract(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::Extract(form)) = self.modal.as_ref() else { return };
        let (papers, fields, again) = (form.papers.clone(), form.fields.clone(), form.again);
        if fields.is_empty() {
            self.modal_error = t("至少勾选一个词条。").into();
            return cx.notify();
        }
        match self.extract_start(papers, fields, again, cx) {
            Ok(()) => {
                self.run(Cmd::CloseModal, window, cx);
                self.run(Cmd::SetPage(Page::Extract), window, cx);
            }
            Err(err) => self.modal_error = err,
        }
        cx.notify();
    }

    /// Start asking the assistant about `fields` for `papers`, several
    /// papers at a time (设置 › AI says how many; the model is the one that
    /// screens). What it says is kept as proposals; no field is written.
    fn extract_start(&mut self, papers: Vec<String>, fields: Vec<String>, again: bool, cx: &mut Context<Self>) -> Result<(), String> {
        let profile = self.settings.speaking_profile_for(Task::Screen);
        let workers = self.settings.screen_workers;
        let Some(lib) = self.lib.as_mut() else { return Ok(()) };
        if lib.extract.running() { return Err(t("助手还在读上一轮，等它结束或先停止。").into()) }
        // The key and address are checked once here, not once per paper.
        ai::plain_request(&profile, "s", "p")?;
        let mut todo = lib.extract_plan(&papers, &fields, again);
        if todo.is_empty() {
            let nothing = !papers.iter().filter_map(|id| lib.paper(id)).any(readable);
            return Err(if nothing { t("这些文献没有摘要，也没有保存全文，助手没有可读的内容。") } else { t("没有要读的：这些文献的这些词条都已经填好，或已经问过。勾选「重新提取」可以再问一遍。") }.into());
        }
        todo.reverse();
        let state = &mut lib.extract;
        state.run += 1;
        (state.total, state.done, state.failed, state.unread, state.found, state.streak, state.todo) = (todo.len(), 0, 0, 0, 0, 0, todo);
        state.reading.clear();
        state.error.clear();
        state.last = None;
        state.asked = fields;
        state.model = format!("{} · {}", profile.title(), profile.model);
        let run = state.run;
        for _ in 0..workers {
            self.extract_next(run, cx);
        }
        Ok(())
    }

    /// Ask about the next paper of the run, and about the one after it once
    /// the answer is in. A paper without a usable answer is counted and
    /// left for another run; several of them in a row end this one.
    fn extract_next(&mut self, run: u64, cx: &mut Context<Self>) {
        let profile = self.settings.speaking_profile_for(Task::Screen);
        let Some(lib) = self.lib.as_mut().filter(|l| l.extract.run == run) else { return };
        let Some((id, wanted)) = lib.extract.todo.pop() else { return };
        let Some(paper) = lib.paper(&id).cloned() else { return self.extract_next(run, cx) };
        // The fields as they are defined now: one removed meanwhile is not asked about.
        let fields: Vec<FieldDef> = lib.fields.iter().filter(|field| wanted.contains(&field.id)).cloned().collect();
        let pages = lib.store.paper_pages(&id).ok().flatten();
        let (system, prompt, basis) = extract::prompt(&paper, pages.as_deref(), &fields, language());
        // Nothing to read, or nothing left to ask: counted without asking, rather than have the model guess from a title.
        if basis == "none" || fields.is_empty() {
            lib.extract.done += 1;
            if !fields.is_empty() { lib.extract.unread += 1 }
            return self.extract_next(run, cx);
        }
        let request = match ai::plain_request(&profile, &system, &prompt) {
            Ok(request) => request,
            Err(err) => {
                lib.extract.error = err;
                lib.extract.todo.clear();
                return;
            }
        };
        lib.extract.reading.push(id);
        let author = format!("{} · {}", profile.title(), profile.model);
        let (library, protocol) = (lib.info.id.clone(), profile.protocol);
        let work = cx.background_executor().spawn(async move {
            let answers = wisp_net::complete(request, protocol).and_then(|reply| extract::parse(&reply, &fields, &paper, pages.as_deref()));
            (paper, fields, answers)
        });
        cx.spawn(async move |this, cx| {
            let (paper, fields, answers) = work.await;
            this.update(cx, |this, cx| {
                let Some(lib) = this.lib.as_mut().filter(|l| l.info.id == library && l.extract.run == run) else { return };
                lib.extract.reading.retain(|reading| *reading != paper.id);
                lib.extract.done += 1;
                let kept = answers.and_then(|answers| {
                    let mut found = 0;
                    for answer in answers {
                        let Some(field) = fields.iter().find(|field| field.id == answer.field_id) else { continue };
                        if lib.keep_answer(field, &paper.id, answer, basis, &author)? { found += 1 }
                    }
                    Ok(found)
                });
                match kept {
                    Ok(found) => {
                        lib.extract.streak = 0;
                        lib.extract.found += found;
                        this.extract_next(run, cx);
                    }
                    Err(err) => {
                        lib.extract.failed += 1;
                        lib.extract.streak += 1;
                        if lib.extract.streak >= GIVE_UP {
                            lib.extract.error = tf!("连续 {GIVE_UP} 篇没有得到可用的回答，已停止。最后一篇「{}」：{err}", "Stopped after {GIVE_UP} consecutive unusable responses. Last paper “{}”: {err}", paper.title);
                            lib.extract.todo.clear();
                        } else {
                            lib.extract.error = tf!("「{}」没有读成，留待下一轮：{err}", "Could not read “{}”; left for the next run: {err}", paper.title);
                            this.extract_next(run, cx);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The sparkle by a field of a paper: ask about that one field, whatever
    /// was said of it before. The answer waits under the field as a proposal.
    fn extract_ask(&mut self, key: Key, cx: &mut Context<Self>) {
        let profile = self.settings.speaking_profile_for(Task::Screen);
        let Some(lib) = self.lib.as_mut() else { return };
        if lib.extract.asking.contains(&key) { return }
        lib.extract.ask_errors.remove(&key);
        let (Some(paper), Some(field)) = (lib.paper(&key.0).cloned(), lib.fields.iter().find(|field| field.id == key.1).cloned()) else { return };
        let pages = lib.store.paper_pages(&paper.id).ok().flatten();
        let fields = vec![field];
        let (system, prompt, basis) = extract::prompt(&paper, pages.as_deref(), &fields, language());
        if basis == "none" {
            lib.extract.ask_errors.insert(key, t("这篇文献没有摘要，也没有保存全文，没有可读的内容。").into());
            return;
        }
        let request = match if profile.missing().is_some() { Err(t("还没有配置模型：在 设置 › AI 里添加。").to_string()) } else { ai::plain_request(&profile, &system, &prompt) } {
            Ok(request) => request,
            Err(err) => {
                lib.extract.ask_errors.insert(key, err);
                return;
            }
        };
        lib.extract.asking.insert(key.clone());
        let author = format!("{} · {}", profile.title(), profile.model);
        let (library, protocol) = (lib.info.id.clone(), profile.protocol);
        let work = cx.background_executor().spawn(async move {
            let answer = wisp_net::complete(request, protocol).and_then(|reply| extract::parse(&reply, &fields, &paper, pages.as_deref())).and_then(|mut answers| answers.pop().ok_or_else(|| t("模型没有回答这个词条。").to_string()));
            (fields, answer)
        });
        cx.spawn(async move |this, cx| {
            let (fields, answer) = work.await;
            this.update(cx, |this, cx| {
                let Some(lib) = this.lib.as_mut().filter(|l| l.info.id == library) else { return };
                lib.extract.asking.remove(&key);
                if let Err(err) = answer.and_then(|answer| lib.keep_answer(&fields[0], &key.0, answer, basis, &author)) {
                    lib.extract.ask_errors.insert(key, tf!("模型没有答上来：{err}", "Model request failed: {err}"));
                }
                lib.extract_settle();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Write the ticked rows on show, all of them or none. What the fields
    /// held is kept, to take it back.
    fn extract_apply(&mut self, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_ref() else { return };
        if lib.extract.running() { return self.say(t("助手还在读，等这一轮结束再写入。"), cx) }
        let review = &lib.extract.review;
        let rows = review.on_show(lib.extract_rows());
        let plan = review.plan(&rows);
        if plan.is_empty() { return }
        let before: Vec<(String, String, Vec<String>)> = rows.iter().filter(|row| review.ticked(row)).map(|row| (row.paper.id.clone(), row.field.id.clone(), row.held().to_vec())).collect();
        // Where the person picked another option than the one proposed, the proposal has been dealt with too.
        let set_aside: Vec<Key> = rows.iter().filter(|row| review.ticked(row) && review.chosen(row).is_some_and(|option| row.proposal.values != [option])).map(Row::key).collect();
        let lines: Vec<(String, String, Vec<String>)> = plan.iter().map(|line| (line.paper_id.clone(), line.field_id.clone(), line.values.clone())).collect();
        let papers = lines.iter().map(|(paper, _, _)| paper.as_str()).collect::<HashSet<_>>().len();
        if !self.change(cx, |lib| {
            lib.store.set_fields(&lines)?;
            lib.store.dismiss_proposals(&set_aside, true)
        }) { return }
        let Some(lib) = self.lib.as_mut() else { return };
        lib.extract.last = Some(Last::Wrote { before, set_aside, papers });
        lib.extract.scroll = UniformListScrollHandle::new();
    }

    fn extract_undo(&mut self, cx: &mut Context<Self>) {
        let Some(last) = self.lib.as_mut().and_then(|lib| lib.extract.last.take()) else { return };
        match last {
            Last::Wrote { before, set_aside, .. } => {
                let done = self.change(cx, |lib| {
                    lib.store.set_fields(&before)?;
                    lib.store.dismiss_proposals(&set_aside, false)
                });
                if done { self.say(tf!("已撤销：{} 个取值回到写入之前。", "Undone: {} values returned to their previous state.", before.len()), cx) }
            }
            Last::SetAside(keys) => {
                if self.change(cx, |lib| lib.store.dismiss_proposals(&keys, false)) { self.say(tf!("已撤销：{} 条建议回到列表。", "Undone: {} proposals are back in the list.", keys.len()), cx) }
            }
        }
    }

    /// A key of 筛选 heard on this page: Space ticks the row the keys are on.
    pub(crate) fn extract_key(&mut self, key: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_ref() else { return };
        let rows = lib.extract.review.on_show(lib.extract_rows());
        let Some(row) = lib.extract.review.cursor.as_ref().and_then(|key| rows.iter().find(|row| row.key() == *key)).or(rows.first()) else { return };
        let on = row.key();
        // The first key only shows which row the keys are on.
        if lib.extract.review.cursor.as_ref() != Some(&on) { return self.run(Cmd::Extract(Do::Cursor(on)), window, cx) }
        if matches!(key, "space" | "x") { self.run(Cmd::Extract(Do::Tick(on)), window, cx) }
    }

    /// The arrow keys and Enter on this page: along the rows, and the row's paper beside them.
    pub(crate) fn extract_nav(&mut self, nav: Nav, window: &mut Window, cx: &mut Context<Self>) {
        if nav == Nav::Open { return self.run(Cmd::Extract(Do::Beside), window, cx) }
        let Some(lib) = self.lib.as_ref() else { return };
        let keys: Vec<Key> = lib.extract.review.on_show(lib.extract_rows()).iter().map(Row::key).collect();
        if keys.is_empty() { return }
        let (at, last) = (lib.extract.review.cursor.as_ref().and_then(|key| keys.iter().position(|other| other == key)), keys.len() - 1);
        let to = match (nav, at) {
            (Nav::First, _) => 0,
            (Nav::Last, _) => last,
            (Nav::Up, None) => last,
            (_, None) => 0,
            (Nav::Up, Some(at)) => at.saturating_sub(1),
            (Nav::Down, Some(at)) => (at + 1).min(last),
            (Nav::PageUp | Nav::Left, Some(at)) => at.saturating_sub(6),
            (Nav::PageDown | Nav::Right, Some(at)) => (at + 6).min(last),
            _ => return,
        };
        lib.extract.scroll.scroll_to_item(to, if at.is_some_and(|at| to > at) { ScrollStrategy::Bottom } else { ScrollStrategy::Top });
        self.run(Cmd::Extract(Do::Cursor(keys[to].clone())), window, cx);
    }

    /// The steps of `WISP_PAPER_DEV` for 提取, after `extract:`. Rows and fields are named by their place, from 0:
    /// `marked`, `field;<n>`, `again`, `summary`, `stop`, `tick;<n>`, `cursor;<n>`, `choose;<n>;<option>`, `part` / `part;<n>`,
    /// `all`, `none`, `beside`, `apply`, `undo`, `dismiss`, and for the selected paper `ask;<n>`, `take;<n>`, `drop;<n>`.
    #[cfg(debug_assertions)]
    pub(crate) fn extract_dev(&self, arg: &str) -> Option<Cmd> {
        let lib = self.lib.as_ref()?;
        let mut parts = arg.splitn(3, ';');
        let (what, first, second) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let n = first.parse::<usize>().unwrap_or(0);
        let rows = lib.extract.review.on_show(lib.extract_rows());
        let row = || rows.get(n).map(Row::key);
        let field = || lib.fields.get(n).map(|field| field.id.clone());
        let of_selected = || Some((lib.selected.clone()?, field()?));
        Some(Cmd::Extract(match what {
            "" => Do::Open(None),
            "marked" => Do::Open(Some(lib.marked.clone())),
            "field" => Do::PickField(field()?),
            "again" => Do::Again,
            "summary" => Do::AddSummary,
            "stop" => Do::Stop,
            "tick" => Do::Tick(row()?),
            "cursor" => Do::Cursor(row()?),
            "choose" => Do::Choose(row()?, second.to_string()),
            "part" => Do::Part(if first.is_empty() { String::new() } else { field()? }),
            "all" => Do::TickAll(true),
            "none" => Do::TickAll(false),
            "beside" => Do::Beside,
            "apply" => Do::Apply,
            "undo" => Do::Undo,
            "dismiss" => Do::Dismiss(None),
            "ask" => Do::Ask(of_selected()?),
            "take" => Do::Take(of_selected()?),
            "drop" => Do::Dismiss(Some(vec![of_selected()?])),
            _ => return None,
        }))
    }

    // ---- drawing

    /// The run in the sidebar while it is under way: it leads to the page.
    pub(crate) fn render_extract_progress(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = &self.lib.as_ref()?.extract;
        if !state.running() { return None }
        let read = state.done - state.failed - state.unread;
        let segments = [(read, t("已读"), Theme::success()), (state.unread, t("没有可读的内容"), Theme::warning()), (state.failed, t("没读成"), Theme::danger()), (state.reading.len(), t("正在阅读"), crate::theme::accent_of("blue").strong()), (state.todo.len(), t("等待中"), Theme::text_placeholder())];
        Some(ui::progress_entry("extract-progress", t("提取词条"), state.done, state.total, true, &segments).on_click(self.on(Cmd::SetPage(Page::Extract), cx)).into_any_element())
    }

    /// The dialog that starts a run: which fields, for how many papers.
    pub(crate) fn render_extract_dialog(&self, cx: &mut Context<Self>) -> Option<Div> {
        let lib = self.lib.as_ref()?;
        let Some(Modal::Extract(form)) = self.modal.as_ref() else { return None };
        let plan = lib.extract_plan(&form.papers, &form.fields, form.again);
        let with_text = plan.iter().filter(|(id, _)| lib.paper(id).is_some_and(|paper| paper.fulltext == "stored")).count();
        let unreadable = form.papers.iter().filter_map(|id| lib.paper(id)).filter(|paper| !readable(paper)).count();
        let resting = form.papers.len() - plan.len() - unreadable;
        let profile = self.settings.speaking_profile_for(Task::Screen);
        let configured = self.screen_configured();
        let rows: Vec<Stateful<Div>> = lib.fields.iter().enumerate().map(|(n, field)| {
            let on = form.fields.contains(&field.id);
            div()
                .id(("extract-field", n))
                .flex()
                .items_center()
                .gap(px(10.))
                .min_h(px(34.))
                .px(px(8.))
                .rounded(px(8.))
                .cursor_pointer()
                .hover(|s| s.bg(Theme::hover()))
                .on_click(self.on(Cmd::Extract(Do::PickField(field.id.clone())), cx))
                .child(tick_box(on, true))
                .child(div().flex_none().max_w(px(180.)).truncate().child(field.name.clone()))
                .child(div().flex_none().text_size(px(12.)).text_color(Theme::text_tertiary()).child(t(field.kind.label())))
                .child(div().flex_1().min_w_0().truncate().text_size(px(12.)).text_color(Theme::text_tertiary()).child(if field.hint.is_empty() && field.kind == FieldKind::Select { field.options.join(" / ") } else { field.hint.clone() }))
        }).collect();
        // The one thing to extract that needs no field of the person's own making.
        let summary = (!lib.has_summary()).then(|| {
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .min_h(px(34.))
                .px(px(8.))
                .child(div().flex().flex_none().items_center().justify_center().size(px(16.)).child(ui::icon("plus", 13., Theme::text_tertiary())))
                .child(div().flex_1().min_w_0().truncate().text_color(Theme::text_secondary()).child(tf!("「{}」：一两句话概括每篇文献发现了什么", "“{}”: what each paper found, in a sentence or two", t(extract::SUMMARY))))
                .child(ui::ghost_button("extract-summary", None, Some(t("添加这个词条").into())).h(px(24.)).text_size(px(12.)).text_color(Theme::accent()).on_click(self.on(Cmd::Extract(Do::AddSummary), cx)))
        });
        let scope = if form.marked { tf!("选中的 {} 篇", "the {} selected papers", form.papers.len()) } else { tf!("列表里的 {} 篇", "the {} papers in the list", form.papers.len()) };
        let count = if form.fields.is_empty() {
            t("勾选要让助手填写的词条。").to_string()
        } else if plan.is_empty() && unreadable == form.papers.len() {
            tf!("{scope}没有摘要，也没有保存全文，助手没有可读的内容。", "Nothing to read in {scope}: no abstract and no stored full text.")
        } else if plan.is_empty() {
            tf!("{scope}都不用读：这些词条已经填好，或已经问过。", "Nothing to read in {scope}: these fields are filled or were asked about already.")
        } else {
            let mut text = tf!("将读{scope}中的 {} 篇，其中 {with_text} 篇有全文，其余读摘要。", "Will read {} of {scope}: {with_text} with full text, the rest by abstract.", plan.len());
            if resting > 0 { text.push_str(&tf!("{resting} 篇不用读：词条已经填好，或已经问过。", " {resting} need no reading: filled or asked about already.")) }
            if unreadable > 0 { text.push_str(&tf!("{unreadable} 篇没有摘要也没有全文，读不了。", " {unreadable} have neither abstract nor full text.")) }
            text
        };
        let again = div()
            .id("extract-again")
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(8.))
            .cursor_pointer()
            .on_click(self.on(Cmd::Extract(Do::Again), cx))
            .child(tick_box(form.again, true))
            .child(div().flex_1().min_w_0().text_color(Theme::text_secondary()).child(t("重新提取：已有取值或已经问过的也再问一遍")));
        Some(
            ui::dialog(540.)
                .child(ui::dialog_title(t("让助手填写词条")))
                .child(
                    div()
                        .id("modal-body")
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .gap(px(12.))
                        .px(px(20.))
                        .py(px(8.))
                        .child(div().text_size(px(12.)).line_height(px(19.)).text_color(Theme::text_tertiary()).child(t("助手逐篇阅读（有全文读全文，否则读摘要），为勾选的词条提出取值，并摘一句原文作依据。它只提出建议：读完后在「提取」里过目，勾选的才写入词条。")))
                        .child(if lib.fields.is_empty() {
                            div().flex().flex_col().gap(px(2.)).child(div().px(px(8.)).py(px(6.)).text_color(Theme::text_secondary()).child(t("这个文献库还没有自定义词条。先添加要提取的词条，例如物种、研究方法、样本量。"))).children(summary).child(div().flex().px(px(2.)).child(ui::ghost_button("extract-fields", Some("list"), Some(t("管理自定义词条").into())).on_click(self.on(Cmd::ManageFields, cx))))
                        } else {
                            div().flex().flex_col().gap(px(2.)).children(rows).children(summary)
                        })
                        .when(!lib.fields.is_empty(), |d| d.child(again))
                        .child(div().px(px(8.)).text_size(px(12.)).line_height(px(19.)).text_color(Theme::text_secondary()).child(count))
                        .child(if configured {
                            div().px(px(8.)).text_size(px(12.)).text_color(Theme::text_tertiary()).child(tf!("用筛选的模型：{} · {}，同时读 {} 篇。", "Uses the screening model: {} · {}, {} papers at a time.", profile.title(), profile.model, self.settings.screen_workers))
                        } else {
                            div().flex().items_center().gap(px(6.)).px(px(8.)).text_size(px(12.)).text_color(Theme::warning()).child(t("还没有配置模型。")).child(self.screen_setup_button(cx))
                        })
                        .child(ui::error_line(&self.modal_error)),
                )
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .justify_end()
                        .gap(px(8.))
                        .px(px(20.))
                        .py(px(14.))
                        .child(ui::secondary_button("modal-cancel", t("取消")).on_click(self.on(Cmd::CloseModal, cx)))
                        .child(ui::primary_button("modal-ok", if plan.is_empty() { t("开始提取").to_string() } else { tf!("开始读 {} 篇", "Read {} papers", plan.len()) }).when(plan.is_empty(), |d| d.opacity(0.5)).on_click(self.on(Cmd::SubmitModal, cx))),
                ),
        )
    }

    /// The custom fields of a paper in the detail panel: each with what it
    /// holds, a sparkle that asks the assistant about it, and under it what
    /// the assistant proposed, or the sentence the value rests on.
    pub(crate) fn render_field_rows(&self, paper: &Paper, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(lib) = self.lib.as_ref() else { return vec![] };
        lib.fields
            .iter()
            .enumerate()
            .map(|(n, field)| {
                let key: Key = (paper.id.clone(), field.id.clone());
                let values = paper.fields.get(&field.id).cloned().unwrap_or_default();
                let proposal = lib.extract.proposals.get(&paper.id, &field.id);
                let standing = proposal.map(|proposal| extract::standing(proposal, field, paper));
                let busy = lib.extract.asking.contains(&key) || (lib.extract.reading.contains(&paper.id) && lib.extract.asked.contains(&field.id));
                let tip = tf!("让助手从文中提取「{}」", "Have the assistant extract “{}” from the paper", field.name);
                // By the field's name, the sparkle that has the assistant read the paper for it. No words: the field says what.
                let sparkle = div()
                    .id(("field-ask", n))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(22.))
                    .rounded(px(6.))
                    .when(busy, |d| d.child(ui::spinner(("field-asking", n), 13., Theme::accent())))
                    .when(!busy, |d| d.cursor_pointer().hover(|s| s.bg(Theme::accent_soft())).active(|s| s.bg(Theme::pressed())).child(ui::icon("sparkle", 14., Theme::accent())).on_click(self.on(Cmd::Extract(Do::Ask(key.clone())), cx)))
                    .tooltip(move |_, cx| cx.new(|_| crate::write_assist::Tip(tip.clone())).into());
                let held = if values.is_empty() {
                    div().flex_1().min_w_0().pt(px(1.)).text_color(Theme::text_placeholder()).child(if standing == Some(Standing::Silent) { t("未填写 · 助手没有在文中找到") } else { t("未填写") })
                } else if field.kind == FieldKind::Text {
                    // A line of text may be a sentence or two: it wraps, and can be selected.
                    self.selectable(format!("field-value-{}-{}", paper.id, field.id), values.join("；"), cx).flex_1().min_w_0().line_height(px(20.)).text_color(Theme::text_secondary())
                } else {
                    div().flex().flex_wrap().flex_1().min_w_0().gap(px(4.)).children(values.into_iter().map(|v| ui::chip(v, Theme::text_secondary(), Theme::bg_sunken())))
                };
                let small = |id: (&'static str, usize), text: &'static str| ui::ghost_button(id, None, Some(text.into())).h(px(22.)).px(px(6.)).text_size(px(12.));
                // The sentence a value rests on; with a page, a click opens the PDF there.
                let quote = |proposal: &Proposal, cx: &mut Context<Self>| {
                    let line = div().id(("field-quote", n)).flex().items_start().gap(px(6.)).text_size(px(12.)).line_height(px(18.)).text_color(Theme::text_tertiary()).child(div().flex_1().min_w_0().child(format!("“{}”", proposal.excerpt)));
                    match proposal.page {
                        Some(page) => line.cursor_pointer().hover(|s| s.text_color(Theme::text_secondary())).child(div().flex_none().text_color(Theme::accent()).child(tf!("第 {page} 页", "Page {page}"))).on_click(self.on(Cmd::GotoSource(paper.id.clone(), Some(page), proposal.excerpt.clone()), cx)),
                        None => line.child(div().flex_none().child(t("摘要"))),
                    }
                };
                let under: Option<Div> = match (proposal, standing) {
                    _ if lib.extract.ask_errors.contains_key(&key) => Some(div().text_size(px(12.)).line_height(px(18.)).text_color(Theme::warning()).child(lib.extract.ask_errors[&key].clone())),
                    (Some(proposal), Some(Standing::Open)) => {
                        let fits = extract::fits(field, &proposal.values);
                        let said = if fits && field.kind == FieldKind::Text { div().flex_1().min_w_0().line_height(px(20.)).text_color(Theme::accent()).child(proposal.values.join("；")) } else if fits { div().flex().flex_wrap().flex_1().min_w_0().gap(px(4.)).children(proposal.values.iter().map(|v| ui::chip(v.clone(), Theme::accent(), Theme::accent_soft()))) } else { div().flex_1().min_w_0().text_size(px(12.)).line_height(px(18.)).text_color(Theme::warning()).child(tf!("助手答「{}」，不在选项里。", "The assistant said “{}”, which is not one of the options.", proposal.values.join("；"))) };
                        let line = div().flex().items_start().gap(px(4.)).child(said).when(fits, |d| d.child(small(("field-take", n), t("采纳")).text_color(Theme::accent()).on_click(self.on(Cmd::Extract(Do::Take(key.clone())), cx)))).child(small(("field-drop", n), t("忽略")).on_click(self.on(Cmd::Extract(Do::Dismiss(Some(vec![key.clone()]))), cx)));
                        Some(div().flex().flex_col().gap(px(4.)).child(line).when(!proposal.excerpt.is_empty(), |d| d.child(quote(proposal, cx))).when(proposal.excerpt.is_empty(), |d| d.child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child(tf!("没有找到原文依据 · {}", "No sentence of the paper found for it · {}", basis(proposal))))))
                    }
                    (Some(proposal), Some(Standing::Held)) if !proposal.excerpt.is_empty() => Some(div().child(quote(proposal, cx))),
                    _ => None,
                };
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(div().flex().items_start().gap(px(8.)).child(div().flex_none().w(px(84.)).pt(px(1.)).truncate().text_size(px(12.)).text_color(Theme::text_tertiary()).child(field.name.clone())).child(held).child(sparkle))
                    .children(under.map(|under| div().pl(px(92.)).pr(px(4.)).child(under)))
                    .into_any_element()
            })
            .collect()
    }

    pub fn render_extract(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let lib = self.lib.as_ref().unwrap();
        let state = &lib.extract;
        let review = &state.review;
        let all = lib.extract_rows();
        let rows = review.on_show(all.clone());
        let on_show = if rows.len() == all.len() { "" } else { review.part.as_str() };
        let plan = review.plan(&rows);
        let unticked = rows.len() - plan.len();
        let papers = plan.iter().map(|line| line.paper_id.as_str()).collect::<HashSet<_>>().len();

        let start = if all.is_empty() && !state.running() { ui::primary_button("extract-open", t("让助手填写…")) } else { ui::secondary_button("extract-open", t("让助手填写…")) }.on_click(self.on(Cmd::Extract(Do::Open(None)), cx));
        let head = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(10.))
            .px(px(16.))
            .pt(px(16.))
            .pb(px(12.))
            .child(div().flex().flex_wrap().items_center().gap(px(10.)).child(div().flex_none().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(t("提取"))).child(div().flex_1()).when(!lib.fields.is_empty() && !state.running(), |d| d.child(start)))
            .child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child(t("助手读文献，为自定义词条提出取值，每个取值附一句原文。在这里过目：不对的不勾选，再把勾选的一次写入词条。写入之前什么都不会改动，写入之后也能撤销。")))
            .when(state.total > 0, |d| d.child(self.render_extract_run(cx)))
            .when(!state.error.is_empty(), |d| d.child(ui::error_line(&state.error)));

        // What the page last did, with the way back.
        let done = state.last.as_ref().map(|last| {
            let (color, fill) = crate::theme::status_colors("included");
            let text = match last {
                Last::Wrote { before, papers, .. } => tf!("已写入 {} 个取值，涉及 {papers} 篇文献。", "Saved {} values for {papers} papers.", before.len()),
                Last::SetAside(keys) => tf!("已清除 {} 条建议：不再列出，也不会再问。", "Set aside {} proposals: not listed, and not asked again.", keys.len()),
            };
            div().flex().items_center().gap(px(8.)).mx(px(16.)).mb(px(10.)).px(px(12.)).py(px(8.)).rounded(px(8.)).bg(fill).child(ui::icon("check", 14., color)).child(div().flex_1().min_w_0().text_color(color).child(text)).child(ui::ghost_button("extract-undo", None, Some(t("撤销").into())).h(px(24.)).text_size(px(12.)).on_click(self.on(Cmd::Extract(Do::Undo), cx)))
        });
        if all.is_empty() {
            return div().flex().flex_col().size_full().child(head).children(done).child(self.render_extract_empty(cx)).into_any_element();
        }

        // One part per field that has something waiting.
        let part = |n: usize, key: &str, text: String, this: &Self, cx: &mut Context<Self>| {
            let on = on_show == key;
            div().id(("extract-part", n)).flex().flex_none().items_center().h(px(26.)).px(px(10.)).rounded(px(13.)).text_size(px(12.)).cursor_pointer().border_1().when(on, |d| d.bg(Theme::groove()).text_color(Theme::text()).border_color(Theme::line_strong()).font_weight(FontWeight::MEDIUM)).when(!on, |d| d.border_color(Theme::line()).text_color(Theme::text_secondary()).hover(|s| s.bg(Theme::hover()))).on_click(this.on(Cmd::Extract(Do::Part(key.to_string())), cx)).child(text)
        };
        let mut bar: Vec<AnyElement> = vec![part(0, "", tf!("全部 {}", "All {}", all.len()), self, cx).into_any_element()];
        for (n, field) in lib.fields.iter().enumerate() {
            let count = all.iter().filter(|row| row.field.id == field.id).count();
            if count > 0 { bar.push(part(n + 1, &field.id, format!("{} {count}", field.name), self, cx).into_any_element()) }
        }
        let small = |id: &'static str, text: &'static str| ui::ghost_button(id, None, Some(text.into())).h(px(26.)).text_size(px(12.));
        let tools = div().flex().items_center().flex_wrap().gap(px(6.)).px(px(16.)).pb(px(10.)).children(bar).child(div().flex_1()).child(small("extract-all", t("全选")).on_click(self.on(Cmd::Extract(Do::TickAll(true)), cx))).child(small("extract-none", t("全不选")).on_click(self.on(Cmd::Extract(Do::TickAll(false)), cx)));

        let list = uniform_list("extract-rows", rows.len(), cx.processor(move |this, range: Range<usize>, _, cx| this.render_extract_rows(range, cx))).track_scroll(state.scroll.clone()).flex_1();
        // The row the keys are on, in full: a row of the list has one line for the value and one for the sentence.
        let cursor = review.cursor.as_ref().and_then(|key| rows.iter().find(|row| row.key() == *key)).map(|row| {
            let (paper, proposal) = (row.paper, row.proposal);
            let beside = lib.selected.as_deref() == Some(paper.id.as_str());
            let open = ui::ghost_button("extract-beside", Some("file-text"), Some(if beside { t("收起").into() } else if lib.pdf_file(&paper.id).is_some() { t("对照 PDF").into() } else { t("完整记录").into() })).h(px(24.)).text_size(px(12.)).on_click(self.on(Cmd::Extract(Do::Beside), cx));
            let again = ui::ghost_button("extract-ask-again", Some("sparkle"), Some(t("再问一次").into())).h(px(24.)).text_size(px(12.)).on_click(self.on(Cmd::Extract(Do::Ask(row.key())), cx));
            let asking = state.asking.contains(&row.key());
            let mut about = vec![basis(proposal).to_string(), proposal.author.clone()];
            if !row.held().is_empty() { about.push(tf!("词条现为：{}", "The field holds: {}", row.held().join("；"))) }
            about.retain(|part| !part.is_empty());
            let mut block = div()
                .id("extract-cursor")
                .flex()
                .flex_col()
                .flex_none()
                .gap(px(8.))
                .max_h(px(230.))
                .overflow_y_scroll()
                .px(px(16.))
                .py(px(10.))
                .border_t_1()
                .border_color(Theme::line())
                .child(div().flex().items_center().gap(px(8.)).child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::MEDIUM).child(paper.title.clone())).child(if asking { div().flex().flex_none().items_center().gap(px(5.)).text_size(px(12.)).text_color(Theme::accent()).child(ui::spinner("extract-asking", 13., Theme::accent())).child(t("正在读这一篇…")).into_any_element() } else { again.into_any_element() }).child(open))
                .child(div().flex().items_start().gap(px(8.)).child(div().flex_none().pt(px(1.)).text_size(px(12.)).text_color(Theme::text_tertiary()).child(row.field.name.clone())).child(self.selectable(format!("extract-value-{}-{}", paper.id, row.field.id), review.values(row).join("；"), cx).flex_1().min_w_0().line_height(px(21.))));
            if !proposal.excerpt.is_empty() {
                let quote = div().id("extract-quote").flex().gap(px(8.)).px(px(10.)).py(px(8.)).rounded(px(6.)).bg(Theme::bg_sunken()).border_l_2().border_color(Theme::accent()).text_size(px(12.)).line_height(px(18.)).text_color(Theme::text_secondary()).child(div().flex_1().min_w_0().child(proposal.excerpt.clone()));
                block = block.child(match proposal.page {
                    Some(page) => quote.cursor_pointer().child(div().flex_none().text_color(Theme::accent()).child(tf!("第 {page} 页", "Page {page}"))).on_click(self.on(Cmd::GotoSource(paper.id.clone(), Some(page), proposal.excerpt.clone()), cx)),
                    None => quote.child(div().flex_none().text_color(Theme::text_tertiary()).child(t("摘要"))),
                });
            }
            block.child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child(about.join(" · "))).when_some(state.ask_errors.get(&row.key()), |d, err| d.child(div().text_size(px(12.)).text_color(Theme::warning()).child(err.clone())))
        });

        let summary = if plan.is_empty() {
            t("没有勾选的取值：勾选要写入的，或把用不上的清除。").to_string()
        } else {
            let mut text = tf!("将写入 {} 个取值，涉及 {papers} 篇文献。", "Will save {} values for {papers} papers.", plan.len());
            if unticked > 0 { text.push_str(&tf!("{unticked} 条没有勾选，留在这里。", " {unticked} unchecked proposals stay here.")) }
            text
        };
        let hint = |key: &'static str, what: &'static str| div().flex().flex_none().items_center().gap(px(5.)).child(ui::keycap(key)).child(what);
        let foot = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(8.))
            .px(px(16.))
            .py(px(12.))
            .border_t_1()
            .border_color(Theme::line())
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(10.))
                    .child(div().flex_1().min_w(px(160.)).text_size(px(12.)).text_color(Theme::text_secondary()).child(summary))
                    .when(unticked > 0, |d| d.child(ui::ghost_button("extract-dismiss", None, Some(tf!("清除没勾选的 {unticked} 条", "Set aside the {unticked} unchecked").into())).h(px(30.)).text_size(px(12.)).on_click(self.on(Cmd::Extract(Do::Dismiss(None)), cx))))
                    .child(ui::primary_button("extract-apply", if plan.is_empty() { t("写入").to_string() } else { tf!("写入这 {} 个取值", "Save {} values", plan.len()) }).when(plan.is_empty(), |d| d.opacity(0.5)).on_click(self.on(Cmd::Extract(Do::Apply), cx))),
            )
            .child(div().flex().flex_wrap().items_center().gap_x(px(14.)).gap_y(px(6.)).text_size(px(11.)).line_height(px(18.)).text_color(Theme::text_tertiary()).child(hint("↑↓", t("移动"))).child(hint("Space", t("勾选 / 不勾选"))).child(hint("Enter", t("对照 PDF"))).child(div().flex_1().min_w_0().truncate().text_right().child(t("默认勾选：词条还空着、且找到了原文依据的取值。"))));

        div().id("extract-page").flex().flex_col().size_full().overflow_y_scroll().child(head).children(done).child(tools).child(div().flex().flex_col().flex_1().min_h(px(240.)).border_t_1().border_color(Theme::line()).child(list)).children(cursor).child(foot).into_any_element()
    }

    /// The run of the assistant, under way or over: how far it got, what came of it, and the papers it is reading.
    fn render_extract_run(&self, cx: &mut Context<Self>) -> Div {
        let lib = self.lib.as_ref().unwrap();
        let state = &lib.extract;
        let running = state.running();
        let heading = if running { t("助手正在阅读") } else if state.done >= state.total { t("这一轮读完了") } else { t("这一轮停在了这里") };
        let titles: Vec<String> = state.reading.iter().filter_map(|id| lib.paper(id)).map(|paper| paper.title.clone()).collect();
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(12.))
            .rounded(px(10.))
            .border_1()
            .border_color(Theme::line())
            .bg(Theme::bg_sunken())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_none().font_weight(FontWeight::MEDIUM).child(heading))
                    .child(div().flex_none().text_size(px(12.)).text_color(Theme::text_secondary()).child(format!("{} / {}", state.done, state.total)))
                    .when(state.found > 0, |d| d.child(div().flex_none().text_size(px(12.)).text_color(Theme::text_secondary()).child(tf!("提出 {} 个取值", "{} values proposed", state.found))))
                    .when(state.unread > 0, |d| d.child(ui::chip(tf!("没有可读的内容 {}", "{} with nothing to read", state.unread), Theme::warning(), Theme::groove())))
                    .when(state.failed > 0, |d| d.child(ui::chip(tf!("没读成 {}", "{} could not be read", state.failed), Theme::danger(), Theme::danger_soft())))
                    .child(div().flex_1().min_w_0().truncate().text_right().text_size(px(11.)).text_color(Theme::text_tertiary()).child(tf!("{} · 同时 {} 篇", "{} · {} papers at a time", state.model, self.settings.screen_workers)))
                    .when(running, |d| d.child(ui::secondary_button("extract-stop", t("停止")).h(px(26.)).text_size(px(12.)).on_click(self.on(Cmd::Extract(Do::Stop), cx)))),
            )
            .child(crate::screen::progress_bar(state.done, state.total))
            .when(running, |d| {
                d.children(titles.into_iter().enumerate().map(|(n, title)| {
                    let line = div().flex().items_center().gap(px(6.)).text_size(px(12.)).text_color(Theme::text_secondary()).child(ui::spinner(("extract-reading", n), 12., Theme::accent())).child(div().flex_1().min_w_0().truncate().child(title.clone()));
                    // Keyed by the title, so a line that takes another paper's place appears anew.
                    ui::fade_in(SharedString::from(format!("extract-reading-{title}")), line)
                }))
            })
    }

    /// Nothing waiting: how to begin, or that everything was dealt with.
    fn render_extract_empty(&self, cx: &mut Context<Self>) -> Div {
        let lib = self.lib.as_ref().unwrap();
        let state = &lib.extract;
        let (title, hint) = if state.running() {
            (t("助手正在读"), t("它提出的取值会陆续出现在这里。可以先去做别的，侧栏里看得到进度。").to_string())
        } else if lib.fields.is_empty() {
            (t("先有词条，才有可提取的"), t("词条是这个文献库给每篇文献加的字段，例如物种、研究方法、样本量。添加之后，助手可以替你从文中找出每篇的取值。").to_string())
        } else if state.last.is_some() {
            (t("没有待确认的取值"), t("助手提出的都处理完了。新加的文献或词条，可以再让它读一遍。").to_string())
        } else if state.total > 0 {
            (t("这一轮没有提出取值"), t("助手没有在读过的文献里找到这些词条的内容。没有摘要也没有全文的文献，它读不了。").to_string())
        } else {
            (t("还没有待确认的取值"), t("让助手读列表里的文献，为词条找出取值和原文依据。也可以在文献列表里选中几篇，右键「让助手填写词条」；或在一篇文献的词条旁点 ✦ 只问那一项。").to_string())
        };
        let add = lib.fields.is_empty().then(|| ui::primary_button("extract-add-fields", t("添加词条")).on_click(self.on(Cmd::ManageFields, cx)));
        let summary = (!lib.has_summary() && !state.running()).then(|| ui::secondary_button("extract-add-summary", tf!("添加「{}」词条", "Add a “{}” field", t(extract::SUMMARY))).on_click(self.on(Cmd::Extract(Do::AddSummary), cx)));
        div().flex().flex_col().flex_1().items_center().justify_center().gap(px(4.)).child(ui::empty_state("scan-text", title, &hint).flex_none()).child(div().flex().flex_wrap().justify_center().gap(px(8.)).children(add).children(summary))
    }

    fn render_extract_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(lib) = self.lib.as_ref() else { return vec![] };
        let review = &lib.extract.review;
        let rows = review.on_show(lib.extract_rows());
        range
            .filter_map(|i| {
                let row = rows.get(i)?;
                let (paper, field, proposal, key) = (row.paper, row.field, row.proposal, row.key());
                let id = |part: &str| SharedString::from(format!("extract-{part}-{}-{}", paper.id, field.id));
                let (ticked, writable, on_cursor) = (review.ticked(row), review.writable(row), review.cursor.as_ref() == Some(&key));
                let (values, chosen) = (review.values(row), review.chosen(row));
                let tick = div().id(id("tick")).flex().flex_none().items_center().justify_center().w(px(28.)).h(px(22.)).when(writable, |d| d.cursor_pointer()).on_click(self.on(Cmd::Extract(Do::Tick(key.clone())), cx)).child(tick_box(ticked, writable));
                // What the row would write. A Select answer that is none of the options says so, until one is picked.
                let value = if writable { values_view(field, if chosen.is_some() { &values } else { &proposal.values }).font_weight(FontWeight::MEDIUM) } else { div().flex_1().min_w_0().truncate().text_color(Theme::warning()).child(tf!("助手答「{}」，不在选项里：选一个，或留着不写", "The assistant said “{}”, not one of the options: pick one, or leave it", proposal.values.join("；"))) };
                // The options of a Select field: the one to be written stands out, another can be picked.
                let options = (field.kind == FieldKind::Select && (!extract::fits(field, &proposal.values) || field.options.len() <= OPTIONS_SHOWN)).then(|| {
                    div().flex().flex_none().items_center().gap(px(2.)).children(field.options.iter().enumerate().map(|(n, option)| {
                        let chosen = writable && values.first() == Some(option);
                        div()
                            .id(SharedString::from(format!("extract-option-{n}-{}-{}", paper.id, field.id)))
                            .flex()
                            .flex_none()
                            .items_center()
                            .h(px(22.))
                            .px(px(8.))
                            .rounded(px(6.))
                            .text_size(px(12.))
                            .cursor_pointer()
                            .when(chosen, |d| d.bg(Theme::accent_soft()).text_color(Theme::accent()).font_weight(FontWeight::MEDIUM))
                            .when(!chosen, |d| d.text_color(Theme::text_tertiary()).hover(|s| s.bg(Theme::hover()).text_color(Theme::text())))
                            .on_click(self.on(Cmd::Extract(Do::Choose(key.clone(), option.clone())), cx))
                            .child(option.clone())
                    }))
                });
                // What the field holds now, and what writing would do to it: a multi-value field keeps it, another is replaced.
                let held = (!row.held().is_empty()).then(|| div().flex_none().max_w(px(260.)).truncate().text_size(px(12.)).text_color(Theme::warning()).child(if field.kind == FieldKind::Tags { tf!("已有：{}（加在后面）", "Has: {} (added after)", row.held().join("；")) } else { tf!("现为：{}（会被替换）", "Holds: {} (will be replaced)", row.held().join("；")) }));
                // With options on show, the one picked out is the value: words stand before them only where the answer was none of them.
                let with_options = options.is_some();
                let said = if options.is_some() && writable { div().flex_none() } else if options.is_some() { value.flex_initial() } else { value };
                let first = div().flex().items_center().gap(px(8.)).h(px(22.)).child(div().flex_none().w(px(88.)).truncate().text_size(px(12.)).text_color(Theme::text_tertiary()).child(field.name.clone())).child(said).children(options).when(with_options, |d| d.child(div().flex_1())).children(held);
                let quote = if proposal.excerpt.is_empty() {
                    div().id(id("quote")).truncate().text_size(px(12.)).text_color(Theme::warning()).child(tf!("没有找到原文依据 · {}", "No sentence of the paper found for it · {}", basis(proposal)))
                } else {
                    let line = div().id(id("quote")).flex().items_center().gap(px(6.)).text_size(px(12.)).text_color(Theme::text_secondary()).child(div().min_w_0().truncate().child(format!("“{}”", proposal.excerpt)));
                    match proposal.page {
                        Some(page) => line.cursor_pointer().hover(|s| s.text_color(Theme::text())).child(div().flex_none().text_color(Theme::accent()).child(tf!("第 {page} 页", "Page {page}"))).on_click(self.on(Cmd::GotoSource(paper.id.clone(), Some(page), proposal.excerpt.clone()), cx)),
                        None => line.child(div().flex_none().text_color(Theme::text_tertiary()).child(t("摘要"))),
                    }
                };
                let mut about: Vec<String> = vec![paper.short_authors(), paper.year.map(|y| y.to_string()).unwrap_or_default(), paper.title.clone()];
                about.retain(|part| !part.is_empty());
                let text = div()
                    .id(id("text"))
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(1.))
                    .cursor_pointer()
                    .on_click(self.on(Cmd::Extract(Do::Cursor(key.clone())), cx))
                    .child(first)
                    .child(div().pl(px(96.)).child(quote))
                    .child(div().pl(px(96.)).truncate().text_size(px(12.)).text_color(Theme::text_tertiary()).child(about.join(" · ")));
                Some(
                    div()
                        .flex()
                        .items_start()
                        .gap(px(6.))
                        .w_full()
                        .h(px(ROW_H))
                        .pt(px(9.))
                        .pl(px(10.))
                        .pr(px(16.))
                        .border_b_1()
                        .border_color(Theme::line())
                        .when(on_cursor, |d| d.bg(Theme::accent_soft()))
                        .when(!on_cursor, |d| d.hover(|s| s.bg(Theme::hover())))
                        .child(tick)
                        .child(text.when(!ticked, |d| d.opacity(0.62)))
                        .into_any_element(),
                )
            })
            .collect()
    }
}
