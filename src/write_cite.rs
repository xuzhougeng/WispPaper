//! 配引用 on the writing page: the papers of the library that the selected
//! text, or the caret's paragraph, can cite. Its answer is not a version
//! of the paragraph but a list: under each sentence the papers found for
//! it, each with the sentence of its own that says so and why a model
//! takes it to fit. The person ticks; 插入 writes `[@Key]` at the ends of
//! those sentences and nothing else (`wisp_core::citefind`).
//!
//! Three steps, said in the card as they go: the translation model says
//! what each sentence does (background, a finding of someone else's, a
//! method …) and turns it into search words in the language of the
//! library's papers (so a Chinese paragraph finds English papers), the
//! library is searched, and the chat model says which of the papers found
//! bear each sentence out. Without a model the papers found by their words
//! are listed unticked, for the person to judge.
//!
//! What a sentence can cite goes by what it does. A paper that only tells
//! what others found is no source: it is not ticked, and the card says
//! under the sentence that it restates it and which works it names for it,
//! offering those the library holds.

use crate::app::{Cmd, Workspace};
use crate::i18n::{label, t};
use crate::theme::{Theme, text_px};
use crate::ui;
use crate::write_assist::{Card, Tip};
use gpui::{prelude::FluentBuilder, *};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::ops::Range;
use wisp_core::ai::{self, Assist, Profile};
use wisp_core::citefind::{self, Asked, Offered, Pick};
use wisp_core::libraries::LibraryInfo;
use wisp_core::settings::Task;
use wisp_core::structure::SECTIONS;

/// The papers looked at for one sentence.
const PER_SENTENCE: usize = 5;

/// Where a request stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stage {
    /// Search words are being made of the sentences.
    #[default]
    Words,
    Search,
    /// A model is looking at what was found.
    Judge,
    Done,
}

/// What 配引用 found, in the card.
#[derive(Default)]
pub struct Found {
    pub stage: Stage,
    /// The sentences of the text asked about, in order.
    pub rows: Vec<Row>,
    /// A model said which papers fit: its choices are ticked.
    pub judged: bool,
    /// The sentences whose other papers are on show, by their place.
    pub unfolded: HashSet<usize>,
    /// The papers found for the sentences a model chose none for are on show.
    pub rest: bool,
    /// How many papers were searched, and how many of them by their record alone, having no full text.
    pub searched: usize,
    pub without_text: usize,
}

/// A sentence and the papers found for it.
#[derive(Clone)]
pub struct Row {
    /// Where the sentence stands in the text asked about.
    pub range: Range<usize>,
    pub sentence: String,
    /// What the sentence does, a key of `citefind::INTENTS`; empty when no model said.
    pub intent: String,
    pub offers: Vec<Offer>,
}

/// A work a paper's sentence names by author and year.
#[derive(Clone, Default)]
pub struct Name {
    /// As the sentence names it: `Tang et al., 2009`.
    pub text: String,
    /// The paper of the library it is; empty when the library has none.
    pub paper: String,
    pub key: String,
    pub title: String,
    pub year: Option<i64>,
    pub cite: String,
}

/// A paper a sentence may cite.
#[derive(Clone)]
pub struct Offer {
    pub paper: String,
    pub key: String,
    pub title: String,
    pub year: Option<i64>,
    /// How the paper is cited in running text: `Zhang et al., 2021`.
    pub cite: String,
    /// The part of the paper its sentence is from, a key of `SECTIONS`; `record` without one.
    pub part: String,
    pub page: Option<usize>,
    /// The paper's own sentence that says most of it.
    pub evidence: String,
    /// Why the model takes it to fit.
    pub why: String,
    /// The model chose it.
    pub chosen: bool,
    pub ticked: bool,
    /// The paper reads as a review.
    pub review: bool,
    /// What the model took it for when it chose it: `citefind::SOURCE` or `REVIEW`; empty when it did not say.
    pub role: String,
    /// The model says its sentence only tells what others found: a lead to the works it names, not a citation.
    pub restates: bool,
    /// The works its sentence names.
    pub names: Vec<Name>,
    /// For a paper offered because another one names it for the sentence: how that one is cited.
    pub via: String,
}

impl Offer {
    /// Whether it stands under its sentence without being asked for: the model chose it, or a paper that restates the sentence names it.
    fn standing(&self) -> bool {
        self.chosen || !self.via.is_empty()
    }
}

/// What the person does in the card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Do {
    /// Tick a paper of a sentence, or take the tick away: (sentence, paper), by their places.
    Tick(usize, usize),
    /// Show the other papers found for a sentence, or put them away.
    Unfold(usize),
    /// Show the sentences nothing was chosen for.
    Rest,
    /// Write the ticked citations into the manuscript.
    Insert,
}

/// What the work in the background reports.
enum Event {
    Stage(Stage),
    Note(String),
    Rows(Vec<Row>, usize, usize),
    Picks(Vec<Pick>),
    Failed(String),
    Done,
}

impl Found {
    pub fn ticked(&self) -> usize {
        self.rows.iter().flat_map(|row| &row.offers).filter(|offer| offer.ticked).count()
    }

    /// Whether a sentence leads the card: a model chose a paper for it or found one that restates it, or, before a model has chosen, any was found.
    /// A tick of the person's own moves nothing: the card says what the model made of the sentences.
    pub fn leads(&self, n: usize) -> bool {
        self.rows.get(n).is_some_and(|row| if self.judged { row.offers.iter().any(|offer| offer.standing() || offer.restates) } else { !row.offers.is_empty() })
    }

    /// The sentences a model chose nothing for although papers were found by their words.
    fn spare(&self) -> Vec<usize> {
        (0..self.rows.len()).filter(|n| !self.leads(*n) && !self.rows[*n].offers.is_empty()).collect()
    }

    /// Show the papers found for the sentences nothing was chosen for, all at once, or put them away again.
    fn turn_rest(&mut self) {
        self.rest = !self.rest;
        for n in self.spare() {
            if self.rest { self.unfolded.insert(n); } else { self.unfolded.remove(&n); }
        }
    }

    /// Whether there is anything to tick on show, which is when the card has something to insert.
    pub fn offering(&self) -> bool {
        self.rest || self.ticked() > 0 || (0..self.rows.len()).any(|n| self.leads(n))
    }

    /// What the model made of the papers found. Its choices are ticked and put first in its order; a paper it names for a
    /// sentence it was found for under another is taken along. A paper it says only restates the sentence is marked, and
    /// the works that paper names which the library holds are offered after the choices, unticked: nobody read them for this.
    fn take(&mut self, picks: Vec<Pick>) {
        let known: Vec<Offer> = self.rows.iter().flat_map(|row| row.offers.iter().cloned()).collect();
        let mut order: Vec<Vec<String>> = vec![vec![]; self.rows.len()];
        for pick in picks {
            let Some(row) = self.rows.get_mut(pick.sentence) else { continue };
            let same = |offer: &Offer| offer.key.eq_ignore_ascii_case(&pick.key);
            if !pick.chosen() {
                let Some(at) = row.offers.iter().position(same) else { continue };
                if row.offers[at].chosen { continue }
                row.offers[at].restates = true;
                let (via, names) = (row.offers[at].cite.clone(), row.offers[at].names.clone());
                for name in names.iter().filter(|name| !name.paper.is_empty()) {
                    if citefind::cites(&row.sentence, &name.key) { continue }
                    match row.offers.iter_mut().find(|offer| offer.paper == name.paper) {
                        Some(held) => {
                            if held.via.is_empty() && !held.restates { held.via = via.clone() }
                        }
                        None => row.offers.push(Offer { paper: name.paper.clone(), key: name.key.clone(), title: name.title.clone(), year: name.year, cite: name.cite.clone(), part: "record".into(), page: None, evidence: String::new(), why: String::new(), chosen: false, ticked: false, review: false, role: String::new(), restates: false, names: vec![], via: via.clone() }),
                    }
                }
                continue;
            }
            if !row.offers.iter().any(same) {
                let Some(other) = known.iter().find(|offer| same(offer)) else { continue };
                if citefind::cites(&row.sentence, &other.key) { continue }
                row.offers.push(Offer { chosen: false, ticked: false, restates: false, why: String::new(), role: String::new(), via: String::new(), ..other.clone() });
            }
            let Some(offer) = row.offers.iter_mut().find(|offer| same(offer)) else { continue };
            (offer.chosen, offer.ticked, offer.restates, offer.why, offer.role) = (true, true, false, pick.why, pick.role);
            order[pick.sentence].push(offer.key.clone());
        }
        for (row, order) in self.rows.iter_mut().zip(order) {
            row.offers.sort_by_key(|offer| order.iter().position(|key| *key == offer.key).unwrap_or(if offer.via.is_empty() { usize::MAX } else { usize::MAX - 1 }));
        }
        self.judged = true;
    }
}

/// The rows of a card: the sentences of `text` with what `find_citations` found for each, named by citation key.
/// A work a paper's sentence names counts as held only when the library's paper has a key to cite it by.
fn rows_of(found: &Value, text: &str, ranges: &[Range<usize>], keys: &BTreeMap<String, String>) -> Vec<Row> {
    ranges
        .iter()
        .enumerate()
        .map(|(n, range)| {
            let sentence = text[range.clone()].to_string();
            let offers = found["sentences"][n]["candidates"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| {
                    let field = |name: &str| c[name].as_str().unwrap_or("").to_string();
                    let key = keys.get(c["paperId"].as_str()?)?.clone();
                    // A paper the sentence cites already is not offered again.
                    if citefind::cites(&sentence, &key) { return None }
                    let names = c["names"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|name| {
                            let said = |field: &str| name[field].as_str().unwrap_or("").to_string();
                            match name["paperId"].as_str().and_then(|paper| Some((paper, keys.get(paper)?))) {
                                Some((paper, key)) => Name { text: said("text"), paper: paper.to_string(), key: key.clone(), title: said("title"), year: name["year"].as_i64(), cite: said("cite") },
                                None => Name { text: said("text"), ..Default::default() },
                            }
                        })
                        .collect();
                    Some(Offer { paper: field("paperId"), key, title: field("title"), year: c["year"].as_i64(), cite: field("cite"), part: field("in"), page: c["page"].as_u64().map(|page| page as usize), evidence: field("snippet"), why: String::new(), chosen: false, ticked: false, review: c["review"].as_bool().unwrap_or(false), role: String::new(), restates: false, names, via: String::new() })
                })
                .collect();
            Row { range: range.clone(), sentence, intent: found["sentences"][n]["intent"].as_str().unwrap_or("").to_string(), offers }
        })
        .collect()
}

/// Everything that takes time, off the main thread: the search words, the search, the judgement.
#[allow(clippy::too_many_arguments)]
fn work(send: &dyn Fn(Event), info: &LibraryInfo, text: &str, language: &str, keys: &BTreeMap<String, String>, wording: Option<Profile>, judging: Option<Profile>, reply_in: &str) {
    let ranges: Vec<Range<usize>> = citefind::sentences(text).into_iter().take(citefind::MAX_SENTENCES).collect();
    let sentences: Vec<&str> = ranges.iter().map(|range| &text[range.clone()]).collect();
    let (mut queries, mut intents) = (vec![String::new(); sentences.len()], vec![String::new(); sentences.len()]);
    let mut worded = false;
    if let Some(profile) = &wording {
        send(Event::Stage(Stage::Words));
        // What each sentence does, with its search words: what it can cite goes by that.
        let (system, message) = citefind::plan_prompt(&sentences, language);
        match ai::plain_request(profile, &system, &message).and_then(|request| wisp_net::complete(request, profile.protocol)) {
            Ok(reply) => {
                (intents, queries) = citefind::parse_plan(&reply, sentences.len(), language).into_iter().map(|planned| (planned.intent, planned.words)).unzip();
                worded = queries.iter().any(|words| !words.is_empty());
            }
            Err(error) => send(Event::Note(tf!("没有生成检索词（{error}），按原文的词查找。", "No search terms were made ({error}); searching with the words as written."))),
        }
    }
    send(Event::Stage(Stage::Search));
    let found = wisp_core::store::Store::open(&info.dir)
        .map_err(|e| format!("{e:#}"))
        .and_then(|mut store| wisp_core::tools::call_in(&mut store, info, "find_citations", &json!({"text": text, "queries": queries, "intents": intents, "perSentence": PER_SENTENCE}), &wisp_net::get_text, &wisp_net::download));
    let found = match found {
        Ok(found) => found,
        Err(error) => return send(Event::Failed(error)),
    };
    let rows = rows_of(&found, text, &ranges, keys);
    let nothing = rows.iter().all(|row| row.offers.is_empty());
    let counted = |name: &str| found[name].as_u64().unwrap_or(0) as usize;
    send(Event::Rows(rows.clone(), counted("papersSearched"), counted("papersWithoutText")));
    if nothing && !rows.is_empty() && rows.iter().all(|row| row.intent == citefind::OWN) { return send(Event::Failed(t("这几句是作者对本文工作的说明，不需要配引用。").to_string())) }
    if nothing {
        // Words of one language find nothing in papers of another: said, since it is the likeliest reason.
        let other_script = sentences.iter().any(|sentence| sentence.chars().any(wisp_core::structure::is_cjk)) != matches!(language, "zh" | "ja" | "ko");
        return send(Event::Failed(if other_script && !worded {
            tf!("没有找到可引用的文献。这段文字和文献库里的文献（{}）不是同一种语言：在「设置 › AI › 任务」给翻译配一个模型，就能先把每句变成检索词再找。", "No papers to cite were found. This text and the library's papers ({}) are in different languages: give translation a model in Settings › AI › Tasks, and each sentence is turned into search terms first.", t(citefind::language_name(language)))
        } else {
            t("文献库里没有找到说这些内容的文献。可以先在「检索与导入」里补充文献，或在右边的「参考文献」页里按作者、题目自己找。").to_string()
        }));
    }
    match &judging {
        None => send(Event::Note(t("没有配置模型：下面是按用词找到的相近文献，没有核对过，请自己判断后勾选。").into())),
        Some(profile) => {
            send(Event::Stage(Stage::Judge));
            let offered: Vec<Asked> = rows
                .iter()
                .map(|row| Asked { sentence: row.sentence.clone(), intent: row.intent.clone(), offered: row.offers.iter().map(|offer| Offered { key: offer.key.clone(), title: offer.title.clone(), year: offer.year, part: if offer.part == "record" { String::new() } else { wisp_core::model::label(&SECTIONS, &offer.part).to_string() }, evidence: offer.evidence.clone(), review: offer.review }).collect() })
                .collect();
            let judged = citefind::judge_prompt(&offered, reply_in)
                .and_then(|(system, message)| ai::plain_request(profile, &system, &message))
                .and_then(|request| wisp_net::complete(request, profile.protocol))
                .and_then(|reply| citefind::parse_judgement(&reply));
            match judged {
                Ok(picks) => send(Event::Picks(picks)),
                Err(error) => send(Event::Note(tf!("模型没有核对成（{error}）：下面是按用词找到的相近文献，请自己判断后勾选。", "The model could not check them ({error}): these are papers found by their words, for you to judge."))),
            }
        }
    }
    send(Event::Done);
}

impl Workspace {
    /// Look for the papers the selection, or the caret's paragraph, can cite.
    pub fn cite_start(&mut self, cx: &mut Context<Self>) {
        let configured = |profile: Profile| Some(profile).filter(|profile| profile.missing().is_none());
        let (wording, judging) = (configured(self.settings.profile_for_or_default(Task::Translate)), configured(self.settings.profile_for_or_default(Task::Chat)));
        let reply_in = if crate::i18n::is_english() { "英文" } else { "中文" };
        let language = self.manuscript_language();
        let Some(lib) = self.lib.as_mut() else { return };
        let (range, original) = crate::write_assist::target(&lib.writing, cx);
        let writing = &mut lib.writing;
        writing.assist.generation += 1;
        let generation = writing.assist.generation;
        let model = judging.as_ref().or(wording.as_ref()).map(crate::write_assist::model_name).unwrap_or_default();
        let mut card = Card { assist: Assist::Cite, language, section: writing.section_id.clone(), original, range, result: String::new(), busy: true, error: String::new(), note: String::new(), model, keys: vec![], cite: Some(Found::default()) };
        let refused = if card.original.trim().is_empty() { t("没有要处理的文字：先选中一段，或把光标放在要处理的段落里。") } else if card.original.len() > ai::ASSIST_LIMIT { t("选中的文字太长，一次处理一两段就好。") } else { "" };
        if !refused.is_empty() {
            (card.busy, card.error) = (false, refused.to_string());
            writing.assist.card = Some(card);
            return cx.notify();
        }
        let index = wisp_core::cite::Index::new(&lib.papers, lib.trash.iter().map(|entry| &entry.paper), &lib.citing.kept);
        let keys: BTreeMap<String, String> = lib.papers.iter().filter_map(|paper| Some((paper.id.clone(), index.key(&paper.id)?.to_string()))).collect();
        let papers_in = citefind::library_language(&lib.papers);
        let (section, library, info, text) = (card.section.clone(), lib.info.id.clone(), lib.info.clone(), card.original.clone());
        writing.assist.card = Some(card);
        cx.notify();

        let (sender, events) = async_channel::unbounded();
        cx.background_executor()
            .spawn(async move {
                let send = |event: Event| drop(sender.send_blocking(event));
                work(&send, &info, &text, papers_in, &keys, wording, judging, reply_in);
            })
            .detach();
        cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let alive = this.update(cx, |this, cx| {
                    let Some(lib) = this.lib.as_mut().filter(|l| l.info.id == library) else { return false };
                    let writing = &mut lib.writing;
                    if writing.assist.generation != generation { return false }
                    // The person went on to another part: an answer about this one is not wanted there.
                    if writing.section_id != section {
                        writing.assist.card = None;
                        return false;
                    }
                    let Some(card) = writing.assist.card.as_mut() else { return false };
                    let Some(found) = card.cite.as_mut() else { return false };
                    let more = match event {
                        Event::Stage(stage) => {
                            found.stage = stage;
                            true
                        }
                        Event::Note(note) => {
                            card.note = note;
                            true
                        }
                        Event::Rows(rows, searched, without_text) => {
                            (found.rows, found.searched, found.without_text) = (rows, searched, without_text);
                            true
                        }
                        Event::Picks(picks) => {
                            found.take(picks);
                            true
                        }
                        Event::Failed(error) => {
                            (card.busy, card.error, found.stage) = (false, error, Stage::Done);
                            found.rows.clear();
                            false
                        }
                        Event::Done => {
                            (card.busy, found.stage) = (false, Stage::Done);
                            false
                        }
                    };
                    cx.notify();
                    more
                });
                if !matches!(alive, Ok(true)) { break }
            }
        })
        .detach();
    }

    pub fn cite_do(&mut self, what: Do, window: &mut Window, cx: &mut Context<Self>) {
        if what == Do::Insert { return self.cite_insert(window, cx) }
        let Some(found) = self.lib.as_mut().and_then(|lib| lib.writing.assist.card.as_mut()).and_then(|card| card.cite.as_mut()) else { return };
        match what {
            Do::Tick(row, offer) => {
                if let Some(offer) = found.rows.get_mut(row).and_then(|row| row.offers.get_mut(offer)) { offer.ticked = !offer.ticked }
            }
            Do::Unfold(row) => {
                if !found.unfolded.remove(&row) { found.unfolded.insert(row); }
            }
            Do::Rest => found.turn_rest(),
            Do::Insert => {}
        }
        cx.notify();
    }

    /// Write the ticked citations at the ends of their sentences: one step for Undo, and not a word of the text changes.
    fn cite_insert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let writing = &mut lib.writing;
        let Some(card) = writing.assist.card.as_ref().filter(|card| !card.busy) else { return };
        let Some(found) = card.cite.as_ref() else { return };
        let ticked = |row: &Row| row.offers.iter().filter(|offer| offer.ticked).map(|offer| offer.key.clone()).collect::<Vec<_>>();
        let cited: Vec<(Range<usize>, Vec<String>)> = found.rows.iter().map(|row| (row.range.clone(), ticked(row))).filter(|(_, keys)| !keys.is_empty()).collect();
        if cited.is_empty() { return self.fail(t("还没有勾选要引用的文献。"), cx) }
        let pins: Vec<(String, String)> = found.rows.iter().flat_map(|row| &row.offers).filter(|offer| offer.ticked).map(|offer| (offer.paper.clone(), offer.key.clone())).collect();
        let count = pins.len();
        // The sentence of its own each paper was ticked for: what the citation rests on from now on.
        let rested: Vec<_> = found.rows.iter().flat_map(|row| row.offers.iter().filter(|offer| offer.ticked && !offer.evidence.is_empty()).map(|offer| (card.section.clone(), offer.paper.clone(), wisp_core::passage::claim(&row.sentence), wisp_core::passage::Candidate { text: offer.evidence.clone(), part: if offer.part == "record" { String::new() } else { offer.part.clone() }, page: offer.page.unwrap_or(0) }, offer.why.clone(), wisp_core::passage::CITED.to_string()))).collect();
        // The draft may have been edited meanwhile: the text is found again by what it says, else each sentence by itself.
        let text = writing.editor.read(cx).text().to_string();
        let whole = if text.get(card.range.clone()) == Some(card.original.as_str()) { Some(card.range.clone()) } else { text.find(&card.original).map(|at| at..at + card.original.len()) };
        let (span, before, after) = match whole {
            Some(span) => (span, card.original.clone(), citefind::placed(&card.original, &cited)),
            None => {
                let standing: Vec<(Range<usize>, Vec<String>)> = found.rows.iter().filter_map(|row| Some((text.find(&row.sentence).map(|at| at..at + row.sentence.len())?, ticked(row)))).filter(|(_, keys)| !keys.is_empty()).collect();
                let Some(span) = standing.iter().map(|(range, _)| range.start).min().zip(standing.iter().map(|(range, _)| range.end).max()).map(|(start, end)| start..end) else { return self.fail(t("原来那段已经改动，找不到这些句子了。"), cx) };
                let within: Vec<(Range<usize>, Vec<String>)> = standing.into_iter().map(|(range, keys)| (range.start - span.start..range.end - span.start, keys)).collect();
                (span.clone(), text[span.clone()].to_string(), citefind::placed(&text[span], &within))
            }
        };
        // The caret stays where the person left it: nothing is selected for what was only added to.
        writing.editor.update(cx, |input, cx| input.replace_apart(span, &after, cx));
        writing.assist.card = None;
        writing.editor.read(cx).focus(window);
        // The keys written stay with their papers from now on, as when a paper is cited by hand.
        for (paper, key) in pins {
            if lib.citing.kept.contains_key(&key) { continue }
            if lib.store.keep_cite_key(&paper, &key).is_ok() { lib.citing.kept.insert(key, paper); }
        }
        // In a manuscript that came from a Word file, what was added is said on the spot, as a comment.
        self.sources_keep(rested, cx);
        self.comment_on_change(&before, &after, t("配引用"));
        self.say(tf!("已加上 {count} 条引用", "Added {count} citations"), cx);
    }

    /// What the card shows while it works and when it has found something.
    pub(crate) fn render_cite_body(&self, card: &Card, found: &Found, cx: &mut Context<Self>) -> Stateful<Div> {
        let stage = match found.stage {
            Stage::Words => t("正在把每句话变成检索词…"),
            Stage::Search => t("正在文献库里查找…"),
            Stage::Judge => t("正在核对找到的文献是否真的支持这些句子…"),
            Stage::Done => "",
        };
        let waiting = card.busy.then(|| div().flex().items_center().gap(px(6.)).text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(ui::spinner("write-cite-wait", 12., Theme::accent())).child(stage));
        let (first, rest): (Vec<usize>, Vec<usize>) = (0..found.rows.len()).partition(|n| found.leads(*n));
        // What the authors say of their own work was not searched for: it is neither found nor missing.
        let (own, rest): (Vec<usize>, Vec<usize>) = rest.into_iter().partition(|n| found.rows[*n].intent == citefind::OWN && found.rows[*n].offers.is_empty());
        let with_offers = rest.iter().filter(|n| !found.rows[**n].offers.is_empty()).count();
        // The model looked and chose nothing: one line says so, and the papers found by their words come up only when asked for.
        let none = found.judged && first.is_empty();
        let chose = found.rows.iter().flat_map(|row| &row.offers).any(|offer| offer.chosen);
        let about = (!card.busy && found.judged && !none).then(|| if chose { t("勾选的是模型核对后认为可以引用的文献；每篇下面是它自己的原句，点一下可以取消或另选。") } else { t("模型没有选出可以直接引用的文献，但找到了转述这些内容的文献：出处是它们所引的文献。") });
        let mut body = div().id("write-cite-body").flex().flex_col().gap(px(10.)).max_h(px(480.)).overflow_y_scroll().children(waiting).children(about.map(|text| div().text_size(text_px(11.)).line_height(text_px(16.)).text_color(Theme::text_tertiary()).child(text)));
        for n in &first {
            body = body.child(self.render_cite_row(*n, &found.rows[*n], found, cx));
        }
        if !rest.is_empty() && !card.busy {
            let line = if none {
                if found.rows.len() == 1 { t("模型核对后，认为找到的文献都不足以支持这句话。").to_string() } else { t("模型核对后，认为找到的文献都不足以支持这些句子。").to_string() }
            } else if first.is_empty() {
                String::new()
            } else if with_offers > 0 {
                tf!("另外 {} 句没有合适的文献", "{} more sentences have no fitting paper", rest.len())
            } else {
                tf!("另外 {} 句没有找到文献", "No papers were found for {} more sentences", rest.len())
            };
            if !line.is_empty() {
                body = body.child(
                    div().flex().flex_wrap().items_center().gap(px(4.)).text_size(text_px(11.)).line_height(text_px(16.)).text_color(Theme::text_tertiary()).child(line).when(with_offers > 0, |d| {
                        d.child(ui::ghost_button("write-cite-rest", None, Some(if found.rest { t("收起").into() } else { t("看相近的").into() })).h(text_px(20.)).px(px(5.)).text_size(text_px(11.)).on_click(self.on(Cmd::WriteCited(Do::Rest), cx)))
                    }),
                );
            }
            // Put away, a sentence stays only for a paper the person ticked under it.
            for n in rest.iter().filter(|n| found.rest || found.rows[**n].offers.iter().any(|offer| offer.ticked)) {
                if !found.rows[*n].offers.is_empty() { body = body.child(self.render_cite_row(*n, &found.rows[*n], found, cx)) }
            }
        }
        if !own.is_empty() && !card.busy {
            body = body.child(div().text_size(text_px(11.)).line_height(text_px(16.)).text_color(Theme::text_tertiary()).child(tf!("{} 句是作者自己的话，没有去找文献。", "{} sentences are the authors' own words; nothing was looked for.", own.len())));
        }
        // Where it looked, so that an empty answer is not taken for a narrow search: every part of the full texts, not the part being written.
        if !card.busy && found.searched > 0 {
            let scope = tf!("查找范围：{} 篇文献的题录、摘要和全文的各个部分（导言、方法、结果、讨论）", "Searched: the records, abstracts and every part of the full text (introduction, methods, results, discussion) of {} papers", found.searched);
            let lacking = if found.without_text > 0 { tf!("；其中 {} 篇没有全文，只查了题录和摘要。", "; {} of them have no full text and were searched by record and abstract only.", found.without_text) } else { tf!("。", ".") };
            body = body.child(div().text_size(text_px(11.)).line_height(text_px(16.)).text_color(Theme::text_tertiary()).child(format!("{scope}{lacking}")));
        }
        body
    }

    /// One sentence of the text and the papers found for it.
    fn render_cite_row(&self, n: usize, row: &Row, found: &Found, cx: &mut Context<Self>) -> Div {
        // What the model chose, what a restating paper names, and what the person ticked besides, are on show; the other papers found come up when asked for.
        let shown = |offer: &Offer| !found.judged || offer.standing() || offer.ticked || found.unfolded.contains(&n);
        let hidden = row.offers.iter().filter(|offer| !shown(offer)).count();
        let others = row.offers.iter().filter(|offer| !offer.standing()).count();
        let offers: Vec<AnyElement> = row.offers.iter().enumerate().filter(|(_, offer)| shown(offer)).map(|(m, offer)| self.render_cite_offer(n, m, offer, cx)).collect();
        // A paper that only tells what others found is a lead to them, said in a line of its own.
        let hints: Vec<AnyElement> = row.offers.iter().filter(|offer| found.judged && offer.restates).map(|offer| self.render_cite_hint(n, offer, cx)).collect();
        let intent = (!row.intent.is_empty()).then(|| div().flex_none().mt(px(1.)).px(px(5.)).h(px(16.)).flex().items_center().rounded(px(4.)).bg(Theme::bg_sunken()).text_size(text_px(10.)).text_color(Theme::text_tertiary()).child(label(&citefind::INTENTS, &row.intent).to_string()));
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(6.))
                    .child(div().flex().flex_none().items_center().justify_center().min_w(px(16.)).h(px(16.)).mt(px(1.)).px(px(3.)).rounded(px(8.)).bg(Theme::bg_sunken()).text_size(text_px(10.)).text_color(Theme::text_secondary()).child((n + 1).to_string()))
                    .child(div().flex_1().min_w_0().text_size(text_px(12.)).line_height(text_px(18.)).text_color(Theme::text_secondary()).line_clamp(2).text_ellipsis().child(row.sentence.clone()))
                    .children(intent),
            )
            .children(offers)
            .children(hints)
            .when(found.judged && others > 0 && (hidden > 0 || found.unfolded.contains(&n)), |d| {
                // 「还有」 only where some papers stand under the sentence already.
                let name = if hidden == 0 { t("收起").to_string() } else if hidden == row.offers.len() { tf!("看 {hidden} 篇相近的", "Show {hidden} similar papers") } else { tf!("还有 {hidden} 篇相近的", "{hidden} more similar papers") };
                d.child(div().flex().child(ui::ghost_button(("write-cite-more", n), None, Some(name.into())).h(text_px(20.)).px(px(5.)).text_size(text_px(11.)).text_color(Theme::text_tertiary()).on_click(self.on(Cmd::WriteCited(Do::Unfold(n)), cx))))
            })
    }

    /// A paper that restates the sentence: where it does, and the works it names for it. A click shows that sentence in the paper.
    fn render_cite_hint(&self, n: usize, offer: &Offer, cx: &mut Context<Self>) -> AnyElement {
        let part = if offer.part != "record" && !offer.part.is_empty() { label(&SECTIONS, &offer.part) } else { "" };
        let told = if part.is_empty() { tf!("{} 转述了这一点", "{} restates this", offer.cite) } else { tf!("{} 在{}里转述了这一点", "{} restates this in its {}", offer.cite, part) };
        let names: Vec<String> = offer.names.iter().map(|name| if name.paper.is_empty() { tf!("{}（库里没有）", "{} (not in the library)", name.text) } else { tf!("{}（库里有，见上）", "{} (in the library, above)", name.text) }).collect();
        let whose = if names.is_empty() { tf!("，出处是它在那里引的文献：点开原文看。", "; the source is what it cites there: open the sentence to see.") } else { tf!("，引的是 {}。", ", citing {}.", names.join(if crate::i18n::is_english() { ", " } else { "、" })) };
        let (paper, page, evidence) = (offer.paper.clone(), offer.page, offer.evidence.clone());
        div()
            .id(SharedString::from(format!("write-cite-hint-{n}-{}", offer.paper)))
            .pl(px(7.))
            .border_l_2()
            .border_color(Theme::accent().opacity(0.45))
            .text_size(text_px(11.))
            .line_height(text_px(16.))
            .text_color(Theme::text_secondary())
            .cursor_pointer()
            .hover(|s| s.text_color(Theme::text()))
            .on_click(self.on(Cmd::GotoSource(paper, page, if page.is_some() { evidence } else { String::new() }), cx))
            .child(format!("{told}{whose}"))
            .into_any_element()
    }

    /// A paper a sentence may cite: who and when, its title, why it fits, and the sentence of its own that says so.
    fn render_cite_offer(&self, n: usize, m: usize, offer: &Offer, cx: &mut Context<Self>) -> AnyElement {
        let id = |part: &str| SharedString::from(format!("write-cite-{part}-{n}-{}", offer.paper));
        let mut from = vec![];
        if offer.part != "record" && !offer.part.is_empty() { from.push(label(&SECTIONS, &offer.part).to_string()) }
        if let Some(page) = offer.page { from.push(tf!("第 {page} 页", "Page {page}")) }
        let from = from.join(" · ");
        let (paper, page, evidence) = (offer.paper.clone(), offer.page, offer.evidence.clone());
        let tip = if page.is_some() { t("在 PDF 里看这句原文") } else { t("打开这篇文献") };
        // What kind of source it is for the sentence: as the model took it, else as the paper reads.
        let kind = if offer.role == citefind::SOURCE { Some(t("原始研究")) } else if offer.role == citefind::REVIEW || offer.review { Some(t("综述")) } else { None };
        // The way to the paper stands over the corner of its card and takes the click for itself: the card under it is a tick.
        div()
            .relative()
            .child(
                div()
                    .id(id("tick"))
                    .flex()
                    .items_start()
                    .gap(px(8.))
                    .px(px(8.))
                    .py(px(7.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(if offer.ticked { Theme::accent().opacity(0.45) } else { Theme::line() })
                    .bg(if offer.ticked { Theme::accent_soft() } else { Theme::bg_body() })
                    .cursor_pointer()
                    .hover(|s| s.border_color(Theme::accent().opacity(0.6)))
                    .on_click(self.on(Cmd::WriteCited(Do::Tick(n, m)), cx))
                    .child(div().mt(px(1.)).child(crate::extract::tick_box(offer.ticked, true)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(3.))
                            .child(div().flex().items_baseline().gap(px(6.)).pr(px(22.)).child(div().flex_none().text_size(text_px(12.)).font_weight(FontWeight::MEDIUM).text_color(Theme::text()).child(offer.cite.clone())).children(kind.map(|kind| div().flex_none().text_size(text_px(10.)).text_color(Theme::text_secondary()).child(kind))).child(div().flex_1().min_w_0().truncate().text_size(text_px(11.)).text_color(Theme::text_tertiary()).child(format!("@{}", offer.key))))
                            .child(div().text_size(text_px(12.)).line_height(text_px(17.)).text_color(Theme::text()).line_clamp(2).text_ellipsis().child(offer.title.clone()))
                            .when(!offer.why.is_empty(), |d| d.child(div().text_size(text_px(11.)).line_height(text_px(16.)).text_color(Theme::accent()).child(offer.why.clone())))
                            .when(!offer.via.is_empty() && !offer.chosen, |d| d.child(div().text_size(text_px(11.)).line_height(text_px(16.)).text_color(Theme::accent()).child(tf!("{} 把这一点归于它。它自己的原文没有核对过。", "{} attributes this to it. Its own text was not checked.", offer.via))))
                            .when(!offer.evidence.is_empty(), |d| {
                                d.child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap(px(2.))
                                        .pl(px(7.))
                                        .border_l_2()
                                        .border_color(Theme::line())
                                        .child(div().text_size(text_px(11.)).line_height(text_px(16.)).text_color(Theme::text_secondary()).line_clamp(3).text_ellipsis().child(offer.evidence.clone()))
                                        .when(!from.is_empty(), |d| d.child(div().text_size(text_px(10.)).text_color(Theme::text_tertiary()).child(from.clone()))),
                                )
                            }),
                    ),
            )
            .child(div().absolute().top(px(3.)).right(px(4.)).occlude().child(ui::icon_action(id("source"), "file-text", Theme::text_tertiary(), tip).on_click(self.on(Cmd::GotoSource(paper, page, if page.is_some() { evidence } else { String::new() }), cx))))
            .into_any_element()
    }

    /// Under the list: write the ticked citations, or let it be.
    pub(crate) fn render_cite_actions(&self, found: &Found, cx: &mut Context<Self>) -> Div {
        let ticked = found.ticked();
        let small = |id: &'static str, name: &'static str| ui::ghost_button(id, None, Some(name.into())).h(text_px(24.)).px(px(7.)).text_size(text_px(12.));
        div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap(px(4.))
            .child(
                ui::primary_button("write-cite-insert", if ticked == 0 { t("插入引用").to_string() } else { tf!("插入 {ticked} 条引用", "Insert {ticked} citations") })
                    .h(text_px(26.))
                    .px(px(10.))
                    .text_size(text_px(12.))
                    .when(ticked == 0, |d| d.opacity(0.45).cursor_default())
                    .when(ticked > 0, |d| d.on_click(self.on(Cmd::WriteCited(Do::Insert), cx))),
            )
            .child(small("write-cite-drop", t("放弃")).on_click(self.on(Cmd::WriteAssistUse(crate::write_assist::Use::Drop), cx)))
            .child(div().flex_1())
            .child(div().id("write-cite-about").flex_none().text_size(text_px(11.)).text_color(Theme::text_tertiary()).child(t("只在句末加引用，不改文字")).tooltip(|_, cx| cx.new(|_| Tip(t("引用写作 [@Key]，加在各句句末的标点之前；可以撤销。").into())).into()))
    }
}

#[cfg(test)]
mod tests {
    use super::{Found, Name, Offer, Row, rows_of};
    use serde_json::json;
    use std::collections::BTreeMap;
    use wisp_core::citefind::{self, Pick};

    #[test]
    fn what_was_found_and_what_a_model_chose() {
        let text = "Roots sense compaction [@Zhang2021]. 乙烯在紧实土壤里积累。";
        let ranges = citefind::sentences(text);
        let found = json!({"sentences": [
            {"candidates": [{"paperId": "a", "title": "Root atlas", "cite": "Zhang et al., 2021", "in": "results", "page": 5, "snippet": "roots sense soil compaction"}, {"paperId": "b", "title": "Ethylene", "cite": "Lee, 2020", "in": "record"}, {"paperId": "gone", "title": "No key"}]},
            {"candidates": [{"paperId": "c", "title": "Soil", "cite": "Pandey et al., 2021", "in": "abstract", "snippet": "Ethylene accumulates"}]},
        ]});
        let keys = BTreeMap::from([("a".to_string(), "Zhang2021".to_string()), ("b".to_string(), "Lee2020".to_string()), ("c".to_string(), "Pandey2021".to_string())]);
        let rows: Vec<Row> = rows_of(&found, text, &ranges, &keys);
        // A paper the sentence cites already, and one without a key, are not offered.
        assert_eq!(rows[0].offers.iter().map(|offer| offer.key.as_str()).collect::<Vec<_>>(), ["Lee2020"]);
        assert_eq!((rows[1].sentence.as_str(), rows[1].offers[0].page, rows[1].offers[0].part.as_str()), ("乙烯在紧实土壤里积累。", None, "abstract"));
        let mut card = Found { rows, ..Default::default() };
        assert_eq!(card.ticked(), 0);
        // The model's choices are ticked, first, with its reason; a paper found under another sentence comes along, an unknown one does not.
        let pick = |sentence: usize, key: &str, why: &str, role: &str| Pick { sentence, key: key.into(), why: why.into(), role: role.into() };
        card.take(vec![pick(1, "lee2020", "乙烯", citefind::SOURCE), pick(1, "Nobody1999", "", ""), pick(7, "Lee2020", "", "")]);
        let keys = |row: &Row| row.offers.iter().map(|offer: &Offer| (offer.key.clone(), offer.ticked)).collect::<Vec<_>>();
        assert_eq!(keys(&card.rows[1]), [("Lee2020".to_string(), true), ("Pandey2021".to_string(), false)]);
        assert_eq!((card.rows[1].offers[0].why.as_str(), card.judged, card.ticked()), ("乙烯", true, 1));
        assert!(!card.rows[0].offers[0].ticked);
        // The sentence the model chose for leads; the other has papers found by words alone, which come up together when asked for.
        assert!(card.leads(1) && !card.leads(0) && card.offering());
        card.turn_rest();
        assert!(card.rest && card.unfolded.contains(&0) && !card.unfolded.contains(&1));
        card.turn_rest();
        assert!(!card.rest && card.unfolded.is_empty());
    }

    #[test]
    fn a_model_that_chose_nothing() {
        let offer = |key: &str| Offer { paper: key.to_lowercase(), key: key.into(), title: String::new(), year: None, cite: String::new(), part: "record".into(), page: None, evidence: String::new(), why: String::new(), chosen: false, ticked: false, review: false, role: String::new(), restates: false, names: vec![], via: String::new() };
        let row = |offers: Vec<Offer>| Row { range: 0..0, sentence: String::new(), intent: String::new(), offers };
        let mut card = Found { rows: vec![row(vec![offer("A2020"), offer("B2021")]), row(vec![])], ..Default::default() };
        // Before a model has looked, a sentence leads with whatever was found for it.
        assert!(card.leads(0) && !card.leads(1) && card.offering());
        card.take(vec![]);
        // It chose nothing: no sentence leads and there is nothing to insert, until the papers found are asked for.
        assert!(card.judged && !card.leads(0) && !card.offering());
        card.turn_rest();
        assert!(card.offering() && card.unfolded.contains(&0) && !card.unfolded.contains(&1));
        // A tick of the person's own does not make the sentence one the model chose for, and keeps the card's actions when the rest is put away.
        card.rows[0].offers[1].ticked = true;
        card.turn_rest();
        assert!(!card.leads(0) && card.offering() && card.ticked() == 1);
    }

    #[test]
    fn a_paper_that_only_restates() {
        let text = "单细胞转录组测序最早在 2009 年实现。";
        let ranges = citefind::sentences(text);
        let found = json!({"sentences": [{"intent": "finding", "candidates": [
            {"paperId": "a", "title": "Root atlas", "cite": "Zhang et al., 2019", "in": "introduction", "page": 1, "snippet": "Single-cell RNA-seq was first achieved by Tang et al. (2009) and scaled by Macosko et al. (2015).", "names": [{"text": "Tang et al., 2009", "paperId": "t", "title": "mRNA-Seq of a single cell", "year": 2009, "cite": "Tang et al., 2009"}, {"text": "Macosko et al., 2015"}, {"text": "No key, 2001", "paperId": "gone"}]},
            {"paperId": "r", "title": "Single-cell methods: a review", "cite": "Lee, 2020", "in": "record", "review": true},
        ]}]});
        let keys = BTreeMap::from([("a".to_string(), "Zhang2019".to_string()), ("t".to_string(), "Tang2009".to_string()), ("r".to_string(), "Lee2020".to_string())]);
        let rows: Vec<Row> = rows_of(&found, text, &ranges, &keys);
        // A named work is held only when the library's paper has a key to cite it by.
        let held = |name: &Name| !name.paper.is_empty();
        assert_eq!((rows[0].intent.as_str(), rows[0].offers[0].names.iter().map(held).collect::<Vec<_>>(), rows[0].offers[1].review), ("finding", vec![true, false, false], true));
        let mut card = Found { rows, ..Default::default() };
        card.take(vec![Pick { sentence: 0, key: "Zhang2019".into(), why: String::new(), role: citefind::RESTATES.into() }]);
        // It is marked and not ticked; the work it names that the library holds is offered after it, unticked, and the sentence leads the card.
        let row = &card.rows[0];
        assert!(row.offers.iter().any(|offer| offer.key == "Zhang2019" && offer.restates && !offer.chosen && !offer.ticked));
        let traced = row.offers.iter().find(|offer| offer.key == "Tang2009").unwrap();
        assert_eq!((traced.via.as_str(), traced.ticked, traced.standing(), traced.evidence.as_str()), ("Zhang et al., 2019", false, true, ""));
        assert_eq!(row.offers.iter().map(|offer| offer.key.as_str()).collect::<Vec<_>>(), ["Tang2009", "Zhang2019", "Lee2020"]);
        assert!(card.leads(0) && card.ticked() == 0);
        // A paper the model chose is not also one that restates.
        card.take(vec![Pick { sentence: 0, key: "Lee2020".into(), why: "综述".into(), role: citefind::REVIEW.into() }, Pick { sentence: 0, key: "Lee2020".into(), why: String::new(), role: citefind::RESTATES.into() }]);
        let lee = card.rows[0].offers.iter().find(|offer| offer.key == "Lee2020").unwrap();
        assert!(lee.chosen && lee.ticked && !lee.restates && lee.role == citefind::REVIEW);
    }
}
