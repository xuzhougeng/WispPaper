//! 出处 on the writing page: where in its paper each citation of the
//! manuscript is from (`wisp_core::passage`). Under every entry of the
//! reference list beside the manuscript stand the paper's own sentences
//! that the sentences citing it rest on, with their part and page; a click
//! shows one where it stands in the PDF. With the caret in a citation, a
//! line under the manuscript names its paper and shows the passage of that
//! sentence, and in the list its paper's entry is brought into view.
//!
//! A passage is kept when the person cites from a sentence (引用 on a
//! candidate, a paper ticked in 配引用), and can be looked for afterwards
//! for the citations that have none (the sparkle in the list's header, or
//! by a paper's entry): the translation model turns each citing sentence
//! into search words in the papers' language, the tool `find_passages`
//! ranks the cited paper's own sentences, and the chat model says which of
//! them the citation rests on, or that none bears it out, which is then
//! said under the entry. Without a model the nearest sentence is kept when
//! it shares a fair part of the words, and is said to be unchecked.
//! Nothing here writes a word of the manuscript's.

use crate::app::{Cmd, Workspace};
use crate::i18n::{label, t};
use crate::theme::{Theme, text_px};
use crate::ui;
use crate::write_assist::Tip;
use gpui::{prelude::FluentBuilder, *};
use serde_json::json;
use std::cell::RefCell;
use std::collections::HashSet;
use wisp_core::ai::{self, Profile};
use wisp_core::cite::Cited;
use wisp_core::citefind;
use wisp_core::libraries::LibraryInfo;
use wisp_core::model::Paper;
use wisp_core::passage::{self, Asked, Candidate, Citation, Passage, Rests};
use wisp_core::settings::Task;
use wisp_core::structure::SECTIONS;

/// The citations a model is asked about in one request.
const AT_ONCE: usize = 8;

/// What the writing page keeps of its citations' passages.
#[derive(Default)]
pub struct Sourcing {
    /// The passages kept for the manuscript being written.
    pub kept: Vec<Passage>,
    /// Passages are being looked for.
    pub run: Option<Run>,
    /// Counts the runs: what an earlier one finds, stopped or left, is not kept.
    generation: u64,
    /// Where the reference list is scrolled to.
    pub scroll: ScrollHandle,
    /// The citation the caret was in when the list was last drawn: (section, where it is written). The list goes to its paper once.
    at: RefCell<Option<(String, usize)>>,
}

/// A run that looks for passages.
pub struct Run {
    pub stage: Stage,
    pub total: usize,
    pub done: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Search words are being made of the citing sentences.
    Words,
    Search,
    /// A model is saying which sentence each citation rests on.
    Choose,
}

/// What the person does about passages.
#[derive(Clone, Debug, PartialEq)]
pub enum Do {
    /// Look for the passages of the citations on show that have none; of one paper's only.
    Find(Option<String>),
    Stop,
    /// Let go of a passage, by its id.
    Drop(String),
}

/// A citation on show with what it rests on.
pub struct Standing {
    pub section: String,
    pub citation: Citation,
    pub rests: Option<Rests>,
}

impl Standing {
    /// Its sentence says something a passage can be looked for by.
    fn askable(&self) -> bool {
        self.rests.is_none() && self.citation.claim.chars().any(char::is_alphanumeric)
    }
}

/// The citations of a manuscript's sections (section, text, its citations), each with the passage kept for it.
pub fn standing<'a>(found: impl IntoIterator<Item = (&'a str, &'a str, &'a [Cited])>, kept: &[Passage]) -> Vec<Standing> {
    found
        .into_iter()
        .flat_map(|(section, text, cited)| {
            let citations = passage::citations(text, cited);
            let rests = passage::assign(&citations, section, kept);
            citations.into_iter().zip(rests).map(|(citation, rests)| Standing { section: section.to_string(), citation, rests }).collect::<Vec<_>>()
        })
        .collect()
}

/// A citation a passage is looked for.
struct Want {
    section: String,
    paper: Paper,
    claim: String,
}

/// What the work in the background reports.
enum Event {
    Stage(Stage),
    /// Passages to keep (section, paper, claim, the paper's sentence, why, origin), and how many citations are done with.
    Kept(Vec<(String, String, String, Candidate, String, String)>, usize),
    Failed(String),
    /// The citations nothing was kept for: their papers have no text to look in; nothing near was found; the model did not answer.
    Done { textless: usize, missed: usize, unanswered: usize, worded: bool },
}

/// Everything that takes time, off the main thread: the search words, the search in each paper, the model's choice.
/// `send` says whether anyone still listens.
fn work(send: &dyn Fn(Event) -> bool, info: &LibraryInfo, wanted: &[Want], language: &str, wording: Option<Profile>, judging: Option<Profile>, reply_in: &str) {
    let claims: Vec<&str> = wanted.iter().map(|want| want.claim.as_str()).collect();
    let mut queries = vec![String::new(); wanted.len()];
    if let Some(profile) = &wording {
        if !send(Event::Stage(Stage::Words)) { return }
        let (system, message) = citefind::words_prompt(&claims, language);
        // Without them the sentences are looked for by their own words, which finds what is written in their language.
        if let Ok(reply) = ai::plain_request(profile, &system, &message).and_then(|request| wisp_net::complete(request, profile.protocol)) { queries = citefind::parse_words(&reply, wanted.len(), language) }
    }
    let worded = queries.iter().any(|words| !words.is_empty());
    if !send(Event::Stage(Stage::Search)) { return }
    let mut store = match wisp_core::store::Store::open(&info.dir) {
        Ok(store) => store,
        Err(error) => {
            send(Event::Failed(format!("{error:#}")));
            return;
        }
    };
    // Each paper is looked through once, for all the sentences that cite it.
    let mut offered: Vec<Vec<(Candidate, bool)>> = vec![vec![]; wanted.len()];
    let mut textless: HashSet<&str> = HashSet::new();
    let mut papers: Vec<&str> = vec![];
    for want in wanted {
        if !papers.contains(&want.paper.id.as_str()) { papers.push(&want.paper.id) }
    }
    for paper in papers {
        let places: Vec<usize> = (0..wanted.len()).filter(|n| wanted[*n].paper.id == paper).collect();
        let args = json!({"paperId": paper, "texts": places.iter().map(|n| claims[*n]).collect::<Vec<_>>(), "queries": places.iter().map(|n| queries[*n].as_str()).collect::<Vec<_>>(), "limit": passage::OFFERED});
        let found = match wisp_core::tools::call_in(&mut store, info, "find_passages", &args, &wisp_net::get_text, &wisp_net::download) {
            Ok(found) => found,
            Err(error) => {
                send(Event::Failed(error));
                return;
            }
        };
        if found["sentences"].as_u64() == Some(0) { textless.insert(paper); }
        for (m, n) in places.iter().enumerate() {
            offered[*n] = found["results"][m]["candidates"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| (Candidate { text: c["text"].as_str().unwrap_or("").to_string(), part: c["in"].as_str().unwrap_or("").to_string(), page: c["page"].as_u64().unwrap_or(0) as usize }, c["fair"].as_bool().unwrap_or(false)))
                .collect();
        }
    }
    let keep = |n: usize, candidate: Candidate, why: String, origin: &str| (wanted[n].section.clone(), wanted[n].paper.id.clone(), wanted[n].claim.clone(), candidate, why, origin.to_string());
    let with: Vec<usize> = (0..wanted.len()).filter(|n| !offered[*n].is_empty()).collect();
    let without = wanted.len() - with.len();
    let no_text = wanted.iter().filter(|want| textless.contains(want.paper.id.as_str())).count();
    // A paper that has text and not a sentence sharing a word with the one that cites it: kept as a citation without a ground,
    // when the words looked for were in the paper's language (else the language is the likelier reason).
    let cjk_papers = matches!(language, "zh" | "ja" | "ko");
    let strangers: Vec<_> = (0..wanted.len())
        .filter(|n| offered[*n].is_empty() && !textless.contains(wanted[*n].paper.id.as_str()) && (!queries[*n].is_empty() || claims[*n].chars().any(wisp_core::structure::is_cjk) == cjk_papers))
        .map(|n| keep(n, Candidate::default(), t("这篇文献里没有一句话和它用词相近。").to_string(), passage::WORDS))
        .collect();
    let (mut done, mut missed, mut unanswered) = (without, without - no_text - strangers.len(), 0);
    if !send(Event::Kept(strangers, done)) { return }
    let Some(profile) = &judging else {
        // No model to check them: the nearest sentence, when it shares a fair part of the words.
        let kept: Vec<_> = with.iter().filter(|n| offered[**n][0].1).map(|n| keep(*n, offered[*n][0].0.clone(), String::new(), passage::WORDS)).collect();
        missed += with.len() - kept.len();
        send(Event::Kept(kept, wanted.len()));
        send(Event::Done { textless: no_text, missed, unanswered, worded });
        return;
    };
    if !send(Event::Stage(Stage::Choose)) { return }
    for group in with.chunks(AT_ONCE) {
        let asked: Vec<Asked> = group
            .iter()
            .map(|n| Asked { claim: wanted[*n].claim.clone(), title: wanted[*n].paper.title.clone(), year: wanted[*n].paper.year, offered: offered[*n].iter().map(|(candidate, _)| (wisp_core::model::label(&SECTIONS, &candidate.part).to_string(), candidate.text.clone())).collect() })
            .collect();
        let (system, message) = passage::choose_prompt(&asked, reply_in);
        let chosen = ai::plain_request(profile, &system, &message).and_then(|request| wisp_net::complete(request, profile.protocol)).and_then(|reply| passage::parse_choice(&reply));
        let chosen = match chosen {
            Ok(chosen) => chosen,
            Err(error) => {
                send(Event::Failed(tf!("模型没有核对成（{error}）；已经找到的出处保留了。", "The model could not check them ({error}); the passages found so far are kept.")));
                return;
            }
        };
        let mut kept = vec![];
        for (m, n) in group.iter().enumerate() {
            match chosen.iter().find(|choice| choice.cite == m) {
                None => unanswered += 1,
                // None of the paper's sentences bears it out: kept as that, so it is said under the paper and not asked again.
                Some(choice) => match choice.passage.and_then(|at| offered[*n].get(at)) {
                    Some((candidate, _)) => kept.push(keep(*n, candidate.clone(), choice.why.clone(), passage::MODEL)),
                    None if choice.passage.is_none() => kept.push(keep(*n, Candidate::default(), choice.why.clone(), passage::MODEL)),
                    None => unanswered += 1,
                },
            }
        }
        done += group.len();
        if !send(Event::Kept(kept, done)) { return }
    }
    send(Event::Done { textless: no_text, missed, unanswered, worded });
}

impl Sourcing {
    /// For a library just opened: the passages of the manuscript it shows.
    pub fn new(kept: Vec<Passage>) -> Sourcing {
        Sourcing { kept, ..Default::default() }
    }
}

impl Workspace {
    /// Read the passages of the manuscript being written.
    pub(crate) fn sources_load(&mut self) {
        let Some(lib) = self.lib.as_mut() else { return };
        let sources = &mut lib.citing.sources;
        sources.generation += 1;
        sources.run = None;
        sources.at.replace(None);
        sources.kept = lib.store.passages(&lib.writing.manuscript).unwrap_or_default();
    }

    /// Keep the passage a citation was made from: the paper's sentence the person cited, for the sentence of the manuscript that cites it.
    pub(crate) fn sources_keep(&mut self, new: Vec<(String, String, String, Candidate, String, String)>, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        if new.is_empty() { return }
        match lib.store.keep_passages(&lib.writing.manuscript, &new) {
            Ok(kept) => lib.citing.sources.kept = kept,
            Err(err) => self.fail(tf!("出处没有记下：{err:#}", "Source passage not saved: {err:#}"), cx),
        }
    }

    /// An answer of the assistant's went into a section (`text`, as converted): the sentences it quoted for its citations
    /// are kept as what those rest on, where the paper really says them, on the page it does.
    pub(crate) fn sources_quoted(&mut self, manuscript: &str, section: &str, text: &str, quoted: &[wisp_core::cite::Quoted], cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        if quoted.is_empty() { return }
        let brought = {
            let index = crate::cite::index(lib);
            passage::quoted(text, &wisp_core::cite::find(text, &index), quoted)
        };
        let new: Vec<_> = brought
            .into_iter()
            .filter_map(|(paper, claim, mut candidate)| {
                let pages = lib.store.paper_pages(&paper).ok().flatten();
                match ai::locate_quote(&candidate.text, lib.paper(&paper)?, pages.as_deref())? {
                    Some(page) => candidate.page = page,
                    None => (candidate.part, candidate.page) = ("abstract".into(), 0),
                }
                Some((section.to_string(), paper, claim, candidate, String::new(), passage::CITED.to_string()))
            })
            .collect();
        if new.is_empty() { return }
        match lib.store.keep_passages(manuscript, &new) {
            Ok(kept) => if lib.writing.manuscript == manuscript { lib.citing.sources.kept = kept },
            Err(err) => self.fail(tf!("出处没有记下：{err:#}", "Source passage not saved: {err:#}"), cx),
        }
    }

    pub fn run_source(&mut self, what: Do, cx: &mut Context<Self>) {
        match what {
            Do::Find(only) => self.sources_find(only, cx),
            Do::Stop => {
                let Some(lib) = self.lib.as_mut() else { return };
                lib.citing.sources.generation += 1;
                lib.citing.sources.run = None;
                self.say(t("已停止，找到的出处已经记下。"), cx);
            }
            Do::Drop(id) => {
                let Some(lib) = self.lib.as_mut() else { return };
                match lib.store.remove_passage(&id) {
                    Ok(()) => lib.citing.sources.kept.retain(|passage| passage.id != id),
                    Err(err) => return self.fail(format!("{err:#}"), cx),
                }
            }
        }
        cx.notify();
    }

    /// Look for the passages of the citations on show that have none, of one paper's only when `only` names it.
    fn sources_find(&mut self, only: Option<String>, cx: &mut Context<Self>) {
        let configured = |profile: Profile| Some(profile).filter(|profile| profile.missing().is_none());
        let (wording, judging) = (configured(self.settings.profile_for_or_default(Task::Translate)), configured(self.settings.profile_for_or_default(Task::Chat)));
        let reply_in = if crate::i18n::is_english() { "英文" } else { "中文" };
        let texts = self.cited_texts(cx);
        let Some(lib) = self.lib.as_mut() else { return };
        if lib.citing.sources.run.is_some() { return }
        let mut wanted: Vec<Want> = vec![];
        let mut empty = 0;
        {
            let index = crate::cite::index(lib);
            let found: Vec<(&str, &str, Vec<Cited>)> = texts.iter().map(|(section, text)| (section.as_str(), text.as_str(), wisp_core::cite::find(text, &index))).collect();
            for one in standing(found.iter().map(|(section, text, cited)| (*section, *text, cited.as_slice())), &lib.citing.sources.kept) {
                if one.rests.is_some() || only.as_ref().is_some_and(|paper| *paper != one.citation.paper) { continue }
                // A paper in the trash is not looked through.
                let Some(paper) = lib.paper(&one.citation.paper) else { continue };
                if !one.askable() {
                    empty += 1;
                    continue;
                }
                if !wanted.iter().any(|want| want.paper.id == paper.id && want.claim == one.citation.claim) { wanted.push(Want { section: one.section, paper: paper.clone(), claim: one.citation.claim }) }
            }
        }
        if wanted.is_empty() { return self.say(if empty > 0 { t("这些引用所在的句子还没有写内容，没有可以依据的话去找出处。") } else { t("这里的引用都已经有出处了。") }, cx) }
        let later = wanted.len().saturating_sub(passage::MAX_ASKED);
        wanted.truncate(passage::MAX_ASKED);
        let sources = &mut lib.citing.sources;
        sources.generation += 1;
        sources.run = Some(Run { stage: if wording.is_some() { Stage::Words } else { Stage::Search }, total: wanted.len(), done: 0 });
        let (generation, library, info, manuscript) = (sources.generation, lib.info.id.clone(), lib.info.clone(), lib.writing.manuscript.clone());
        let papers_in = citefind::library_language(&lib.papers);
        let other_script = wanted.iter().any(|want| want.claim.chars().any(wisp_core::structure::is_cjk)) != matches!(papers_in, "zh" | "ja" | "ko");
        let checked = judging.is_some();
        cx.notify();

        let (sender, events) = async_channel::unbounded();
        cx.background_executor()
            .spawn(async move {
                let send = |event: Event| sender.send_blocking(event).is_ok();
                work(&send, &info, &wanted, papers_in, wording, judging, reply_in);
            })
            .detach();
        cx.spawn(async move |this, cx| {
            let mut found = 0;
            let mut none = 0;
            while let Ok(event) = events.recv().await {
                let alive = this.update(cx, |this, cx| {
                    let Some(lib) = this.lib.as_mut().filter(|l| l.info.id == library) else { return false };
                    if lib.citing.sources.generation != generation { return false }
                    let more = match event {
                        Event::Stage(stage) => {
                            if let Some(run) = lib.citing.sources.run.as_mut() { run.stage = stage }
                            true
                        }
                        Event::Kept(new, done) => {
                            if let Some(run) = lib.citing.sources.run.as_mut() { run.done = done }
                            found += new.iter().filter(|(_, _, _, candidate, _, _)| !candidate.text.is_empty()).count();
                            none += new.iter().filter(|(_, _, _, candidate, _, _)| candidate.text.is_empty()).count();
                            if !new.is_empty() {
                                match lib.store.keep_passages(&manuscript, &new) {
                                    // The person may be writing another manuscript by now: kept all the same, shown when that one is opened.
                                    Ok(kept) => if lib.writing.manuscript == manuscript { lib.citing.sources.kept = kept },
                                    Err(err) => {
                                        lib.citing.sources.run = None;
                                        this.fail(tf!("出处没有记下：{err:#}", "Source passage not saved: {err:#}"), cx);
                                        return false;
                                    }
                                }
                            }
                            true
                        }
                        Event::Failed(error) => {
                            lib.citing.sources.run = None;
                            this.fail(error, cx);
                            false
                        }
                        Event::Done { textless, missed, unanswered, worded } => {
                            lib.citing.sources.run = None;
                            let mut said = vec![if found > 0 { tf!("找到 {found} 处引用的出处", "Found the source passage of {found} citations") } else { t("没有找到新的出处").to_string() }];
                            if none > 0 { said.push(tf!("{none} 处在文献里没有找到依据，请核对", "{none} have no support in their paper: please check")) }
                            if missed > 0 { said.push(tf!("{missed} 处没有相近的原文", "{missed} have no similar sentence")) }
                            if textless > 0 { said.push(tf!("{textless} 处所引的文献没有摘要和全文可查", "{textless} cite papers without an abstract or full text")) }
                            if unanswered > 0 { said.push(tf!("{unanswered} 处模型没有回答", "{unanswered} were not answered by the model")) }
                            if later > 0 { said.push(tf!("还有 {later} 处，再点一次接着找", "{later} more: click again to go on")) }
                            if !checked && found > 0 { said.push(t("没有配置模型，这些是按用词找到的，没有核对过").to_string()) }
                            // Words of one language find nothing in papers of another: said, since it is the likeliest reason.
                            if found == 0 && none == 0 && other_script && !worded { said.push(t("稿件和文献不是同一种语言：在「设置 › AI › 任务」给翻译配一个模型，就能先把每句变成检索词再找").to_string()) }
                            this.say(said.join(if crate::i18n::is_english() { "; " } else { "；" }), cx);
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

    /// In the reference list's header: the passages still to look for, or the run that looks for them.
    pub(crate) fn render_source_actions(&self, standing: &[&Standing], cx: &mut Context<Self>) -> Option<AnyElement> {
        let lib = self.lib.as_ref()?;
        if let Some(run) = &lib.citing.sources.run {
            let stage = match run.stage {
                Stage::Words => t("生成检索词…").to_string(),
                Stage::Search => t("查找原文…").to_string(),
                Stage::Choose => tf!("核对出处 {} / {}", "Checking {} / {}", run.done, run.total),
            };
            return Some(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(5.))
                    .text_size(text_px(11.))
                    .text_color(Theme::text_tertiary())
                    .child(ui::spinner("cite-source-busy", 11., Theme::accent()))
                    .child(stage)
                    .child(ui::ghost_button("cite-source-stop", None, Some(t("停止").into())).h(text_px(22.)).px(px(6.)).text_size(text_px(12.)).on_click(self.on(Cmd::Source(Do::Stop), cx)))
                    .into_any_element(),
            );
        }
        let lacking = standing.iter().filter(|one| one.askable() && lib.paper(&one.citation.paper).is_some()).count();
        if lacking == 0 { return None }
        let tip = tf!("{lacking} 处引用还没有出处：在各自所引的文献里，找出引用它的那句话依据的原文。不改稿件的文字。", "{lacking} citations have no source passage yet: look in each cited paper for the sentence the citing sentence rests on. The manuscript is not changed.");
        Some(
            ui::ghost_button("cite-source-find", None, Some(lacking.to_string().into()))
                .h(text_px(22.))
                .px(px(6.))
                .gap(px(4.))
                .text_size(text_px(12.))
                .text_color(Theme::accent())
                .flex_row_reverse()
                .child(ui::icon("sparkle", 13., Theme::accent()))
                .tooltip(move |_, cx| cx.new(|_| Tip(tip.clone())).into())
                .on_click(self.on(Cmd::Source(Do::Find(None)), cx))
                .into_any_element(),
        )
    }

    /// The citation the caret is in or right after, among the manuscript's: its paper's entry is where the list goes.
    pub(crate) fn source_at<'a>(&self, standing: &'a [Standing], cx: &App) -> Option<&'a Standing> {
        let lib = self.lib.as_ref()?;
        let editor = lib.writing.editor.read(cx);
        if !editor.selection().is_empty() { return None }
        let caret = editor.caret();
        standing.iter().find(|one| one.section == lib.writing.section_id && one.citation.at.start <= caret && caret <= one.citation.at.end + 1)
    }

    /// Bring the entry of the paper the caret's citation names into view, once for each citation the caret comes to.
    pub(crate) fn source_follow(&self, at: Option<&Standing>, row: Option<usize>) {
        let Some(lib) = self.lib.as_ref() else { return };
        let sources = &lib.citing.sources;
        let now = at.map(|one| (one.section.clone(), one.citation.at.start));
        if *sources.at.borrow() == now { return }
        if let Some(row) = row { sources.scroll.scroll_to_item(row) }
        sources.at.replace(now);
    }

    /// With the caret in a citation, under the manuscript: the paper it names and the passage that sentence rests on,
    /// or, without one, the way to look for it. `width` is the manuscript's column.
    pub(crate) fn render_source_strip(&self, width: f32, cx: &mut Context<Self>) -> Option<AnyElement> {
        let lib = self.lib.as_ref()?;
        let listed = &lib.citing.listed;
        let at = self.source_at(&listed.standing, cx)?;
        let entry = listed.whole.iter().find(|entry| entry.reference.paper_id == at.citation.paper)?;
        let paper = lib.paper(&at.citation.paper);
        let named = paper.map(|paper| format!("{} · {}", paper.citation(), paper.title)).unwrap_or_else(|| entry.reference.text.clone());
        let passages = self.render_passages("strip", &[at], None, width - 22., cx);
        let busy = lib.citing.sources.run.is_some();
        let id = at.citation.paper.clone();
        let head = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(div().id("source-strip-paper").flex().flex_1().min_w_0().items_center().gap(px(6.)).rounded(px(4.)).cursor_pointer().hover(|s| s.bg(Theme::hover())).text_size(text_px(11.)).child(div().flex_none().text_color(Theme::accent()).child(format!("@{}", entry.reference.key))).child(div().flex_1().min_w_0().truncate().text_color(Theme::text_secondary()).child(named)).on_click(self.on(Cmd::GotoSource(id.clone(), None, String::new()), cx)))
            .when(passages.is_empty() && busy, |d| d.child(ui::spinner("source-strip-busy", 11., Theme::accent())))
            .when(passages.is_empty() && !busy && at.askable() && paper.is_some(), |d| d.child(ui::icon_action("source-strip-find", "sparkle", Theme::accent(), t("在这篇文献里找出这句话依据的原文。不改稿件的文字。")).size(px(20.)).on_click(self.on(Cmd::Source(Do::Find(Some(id.clone()))), cx))));
        Some(
            div()
                .flex()
                .flex_col()
                .flex_none()
                .gap(px(4.))
                .mx(px(16.))
                .mb(px(8.))
                .px(px(10.))
                .py(px(6.))
                .rounded(px(8.))
                .border_1()
                .border_color(Theme::line())
                .bg(Theme::bg_body())
                .child(head)
                .when(passages.is_empty(), |d| d.child(div().text_size(text_px(11.)).text_color(Theme::text_tertiary()).child(t("这处引用还没有记下出处。"))))
                .children(passages)
                .into_any_element(),
        )
    }

    /// Under an entry of the reference list: the passages its citations on show rest on, and what was looked for and not found.
    /// `own` are the citations of this paper, `at` the one the caret is in; `place` tells the list's lines from the strip's, `width` is what a line has.
    pub(crate) fn render_passages(&self, place: &str, own: &[&Standing], at: Option<&Standing>, width: f32, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(lib) = self.lib.as_ref() else { return vec![] };
        let kept = &lib.citing.sources.kept;
        // One line for a passage, however many citations rest on it; loose when it is that for all of them.
        let mut shown: Vec<(usize, bool)> = vec![];
        for rests in own.iter().filter_map(|one| one.rests) {
            match shown.iter_mut().find(|(n, _)| *n == rests.passage) {
                Some((_, loose)) => *loose = *loose && rests.loose,
                None => shown.push((rests.passage, rests.loose)),
            }
        }
        let here = at.and_then(|one| one.rests).map(|rests| rests.passage);
        shown
            .into_iter()
            .filter_map(|(n, loose)| {
                let passage = kept.get(n)?;
                let lit = here == Some(n);
                let group = SharedString::from(format!("cite-passage-{place}-{}", passage.id));
                let mut about = vec![];
                if !passage.claim.is_empty() { about.push(tf!("引用它的句子：{}", "Cited by: {}", passage.claim)) }
                if loose { about.push(t("这句话后来改写过，出处是为原来的句子记下的").to_string()) }
                if !passage.why.is_empty() { about.push(passage.why.clone()) }
                about.push(match passage.origin.as_str() {
                    passage::MODEL => t("助手在这篇文献里找到并核对的"),
                    passage::WORDS => t("按用词找到的，没有核对过"),
                    _ => t("引用时记下的"),
                }.to_string());
                let about = about.join(if crate::i18n::is_english() { " · " } else { "；" });
                // A column of a width in pixels: a text that shares a row is measured narrower than it is drawn.
                let line = div().id(SharedString::from(format!("cite-passage-line-{place}-{}", passage.id))).flex().flex_col().w(px(width)).gap(px(1.)).pl(px(8.)).pr(px(6.)).py(px(2.)).border_l_2().rounded_r(px(4.)).when(lit, |d| d.bg(Theme::accent_soft())).tooltip(move |_, cx| cx.new(|_| Tip(about.clone())).into());
                let line = if passage.found() {
                    let mut from = vec![];
                    if !passage.part.is_empty() { from.push(label(&SECTIONS, &passage.part).to_string()) }
                    if passage.page > 0 { from.push(tf!("第 {} 页", "Page {}", passage.page)) }
                    let page = (passage.page > 0).then_some(passage.page);
                    line.border_color(if lit { Theme::accent() } else { Theme::line() })
                        .cursor_pointer()
                        .hover(|s| s.bg(Theme::hover()))
                        .child(div().text_size(text_px(11.)).line_height(text_px(17.)).text_color(if loose { Theme::text_tertiary() } else { Theme::text_secondary() }).child(passage.passage.clone()))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .text_size(text_px(10.))
                                .text_color(Theme::text_tertiary())
                                .when(passage.origin != passage::CITED, |d| d.child(ui::icon("sparkle", 10., if passage.origin == passage::MODEL { Theme::accent() } else { Theme::text_tertiary() })))
                                .child(from.join(" · "))
                                .when(loose, |d| d.child(t("· 句子已改写"))),
                        )
                        // With a page, the sentence is shown where it stands in the PDF; an abstract's opens the record.
                        .on_click(self.on(Cmd::GotoSource(passage.paper_id.clone(), page, if page.is_some() { passage.passage.clone() } else { String::new() }), cx))
                } else {
                    line.border_color(Theme::warning()).child(div().text_size(text_px(11.)).line_height(text_px(17.)).text_color(Theme::warning()).child(if passage.claim.is_empty() { t("这篇文献里没有找到这处引用的依据").to_string() } else { tf!("这篇文献里没有找到这句话的依据：{}", "No support found in this paper for: {}", passage.claim) }))
                };
                // Letting go of it stands over the end of its line and takes the click for itself.
                let drop = div().absolute().top(px(0.)).right(px(0.)).occlude().rounded(px(6.)).bg(Theme::bg_body()).opacity(0.).group_hover(group.clone(), |s| s.opacity(1.)).child(
                    ui::icon_action(SharedString::from(format!("cite-passage-drop-{place}-{}", passage.id)), "x", Theme::text_tertiary(), t("不要这条出处；之后可以再找")).size(px(18.)).on_click(self.on(Cmd::Source(Do::Drop(passage.id.clone())), cx)),
                );
                Some(div().relative().w(px(width)).group(group).child(line).child(drop).into_any_element())
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::standing;
    use std::collections::BTreeMap;
    use wisp_core::cite;
    use wisp_core::model::Paper;
    use wisp_core::passage::{self, Passage};

    #[test]
    fn citations_on_show_with_what_they_rest_on() {
        let papers = [Paper { id: "a".into(), authors: "Zhang, Wei".into(), year: Some(2021), title: "Root atlas".into(), ..Default::default() }, Paper { id: "b".into(), authors: "Lee, Min".into(), year: Some(2020), title: "Ethylene".into(), ..Default::default() }];
        let nothing = BTreeMap::new();
        let index = cite::Index::new(&papers, [], &nothing);
        let (first, second) = ("Roots sense compaction [@Zhang2021; @Lee2020].", "Atlases resolve cell types [@Zhang2021]. [@Lee2020]");
        let cited = [cite::find(first, &index), cite::find(second, &index)];
        let found = [("s1", first, cited[0].as_slice()), ("s2", second, cited[1].as_slice())];
        let kept = [Passage { id: "1".into(), section_id: "s1".into(), paper_id: "a".into(), claim: "Roots sense compaction.".into(), passage: "Roots sense soil compaction.".into(), origin: passage::CITED.into(), ..Default::default() }];
        let all = standing(found, &kept);
        assert_eq!(all.iter().map(|one| (one.section.as_str(), one.citation.paper.as_str(), one.rests.map(|rests| rests.passage), one.askable())).collect::<Vec<_>>(), [("s1", "a", Some(0), false), ("s1", "b", None, true), ("s2", "a", None, true), ("s2", "b", None, true)]);
    }
}
