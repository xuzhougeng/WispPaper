//! The actions on the selected text of the manuscript, or the paragraph the
//! caret is in. They are of two kinds, and the bar and the card keep them
//! apart, since what comes back is not the same thing:
//!
//! - looking in the library, which changes nothing: 找句子 (the candidates
//!   beside the manuscript, write_find.rs) and 配引用 (the papers each
//!   sentence can cite, write_cite.rs);
//! - a new version of the words: 润色, 译成…, 仿写 (rewritten after the
//!   library's sentences closest to it) and the person's own commands. The
//!   answer streams into a card over the candidates, beside the original
//!   with the words that changed marked; only the person puts it into the
//!   draft, and Undo takes that back.

use crate::i18n::t;
use crate::app::{Cmd, Workspace, Writing};
use crate::settings::SettingsPage;
use crate::theme::{Theme, text_px};
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use std::ops::Range;
use wisp_core::ai::{self, Assist, Change, ChatEvent, Profile};
use wisp_core::settings::Task;

/// The writing page's request to the model, if one was made.
#[derive(Default)]
pub struct Assisting {
    pub card: Option<Card>,
    /// Counts the requests: an answer to an earlier one, stopped or dropped, is not shown.
    pub(crate) generation: u64,
    /// 仿写 is waiting for the sentences closest to its paragraph: the request that asked for them.
    pub(crate) awaiting: Option<u64>,
}

/// One action's result, with what it was asked about.
pub struct Card {
    pub assist: Assist,
    pub language: String,
    /// The part of the manuscript the paragraph is in: the card is for that part only.
    pub section: String,
    /// The text sent, and where in the draft it stood when it was sent.
    pub original: String,
    pub range: Range<usize>,
    pub result: String,
    pub busy: bool,
    pub error: String,
    /// Why the answer ended early, or that it is being asked again.
    pub note: String,
    pub model: String,
    /// 仿写: the papers its candidates are from and their keys (paper, key), kept with their papers once a result that cites them is used.
    pub keys: Vec<(String, String)>,
    /// 配引用: what was found in the library, in place of a result to compare (write_cite.rs).
    pub cite: Option<crate::write_cite::Found>,
}

/// What an action works on: the selection, or the paragraph the caret is in; where it stands and what it says.
pub(crate) fn target(writing: &Writing, cx: &App) -> (Range<usize>, String) {
    let editor = writing.editor.read(cx);
    let (text, selection) = (editor.text(), editor.selection());
    let range = if selection.is_empty() { ai::paragraph_at(text, editor.caret()) } else { trimmed(text, selection) };
    (range.clone(), text[range].to_string())
}

/// A model as a card names it: its name, and what the service calls it when that is something else.
pub(crate) fn model_name(profile: &Profile) -> String {
    let (title, model) = (profile.title(), profile.model.trim());
    if model.is_empty() || title == model { title } else { format!("{title} · {model}") }
}

/// What the person does with a result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Use {
    Replace,
    Below,
    Copy,
    Drop,
}

impl Assisting {
    /// The person went on to another part of the manuscript: the card was about this one.
    pub fn leave(&mut self) {
        self.generation += 1;
        self.awaiting = None;
        self.card = None;
    }
}

impl Workspace {
    /// The language the manuscript is written in, as the prompts name it.
    /// The manuscript's own (`Writing::language`); English while no library is open.
    pub fn manuscript_language(&self) -> String {
        match self.lib.as_ref().map(|lib| lib.writing.language()) {
            None | Some("en") => "English".into(),
            Some(code) => wisp_core::model::label(&wisp_core::manuscript::LANGUAGES, code).to_string(),
        }
    }

    /// Do `assist` on the selection, or on the paragraph the caret is in.
    pub fn assist_start(&mut self, assist: Assist, cx: &mut Context<Self>) {
        self.assist_begin(assist, false, cx);
    }

    /// `searched`: the sentences 仿写 asked for have come, and it goes on with them.
    pub(crate) fn assist_begin(&mut self, assist: Assist, searched: bool, cx: &mut Context<Self>) {
        if self.lib.as_ref().is_none_or(|lib| !searched && lib.writing.assist.card.as_ref().is_some_and(|card| card.busy)) { return }
        // Its answer is a list of papers, not a version of the paragraph.
        if assist == Assist::Cite { return self.cite_start(cx) }
        let profile = self.settings.profile_for_or_default(Task::Chat);
        let language = self.manuscript_language();
        let own = self.settings.commands.clone();
        let Some(lib) = self.lib.as_mut() else { return };
        let writing = &mut lib.writing;
        let (range, original) = target(writing, cx);
        writing.assist.generation += 1;
        let generation = writing.assist.generation;
        let mut card = Card { language: language.clone(), assist: assist.clone(), section: writing.section_id.clone(), original, range, result: String::new(), busy: true, error: String::new(), note: String::new(), model: model_name(&profile), keys: vec![], cite: None };
        // 仿写 goes by the library's sentences closest to this paragraph. Those beside the manuscript may be how the papers open the part, or what was found for another paragraph: then they are looked for first.
        if assist == Assist::Rewrite && !searched && !card.original.trim().is_empty() && profile.missing().is_none() {
            let search = &writing.search;
            let for_this = !search.searching && search.asked.as_ref().is_some_and(|asked| !asked.text.is_empty() && (asked.literal || card.original.trim().ends_with(asked.text.as_str())));
            if !for_this {
                card.note = t("正在文献库里找与这段相近的句子…").into();
                writing.assist.card = Some(card);
                writing.assist.awaiting = Some(generation);
                return self.write_find(false, cx);
            }
        }
        let sentences = writing.search.candidates.as_ref().and_then(|found| found.as_ref().ok()).and_then(|found| found["sentences"].as_array()).cloned().unwrap_or_default();
        // A paper the rewrite draws on is cited by its key, as everywhere in the manuscript.
        let index = wisp_core::cite::Index::new(&lib.papers, lib.trash.iter().map(|entry| &entry.paper), &lib.citing.kept);
        let examples: Vec<ai::Example> = sentences
            .iter()
            .map(|sentence| {
                let field = |key: &str| sentence[key].as_str().unwrap_or("").to_string();
                ai::Example { text: field("text"), cite: field("cite"), key: index.key(&field("paperId")).unwrap_or("").to_string(), page: sentence["page"].as_u64().map(|page| page as usize), heading: field("heading") }
            })
            .collect();
        if assist == Assist::Rewrite { card.keys = sentences.iter().filter_map(|sentence| sentence["paperId"].as_str()).filter_map(|paper| Some((paper.to_string(), index.key(paper)?.to_string()))).collect() }
        let request = match ai::assist_request(&profile, &assist, &language, &card.original, &examples, &own) {
            Ok(request) => request,
            Err(error) => {
                (card.busy, card.error) = (false, error);
                writing.assist.card = Some(card);
                return cx.notify();
            }
        };
        let (section, library, protocol) = (card.section.clone(), lib.info.id.clone(), profile.protocol);
        writing.assist.card = Some(card);
        cx.notify();

        let (sender, events) = async_channel::unbounded();
        cx.background_executor()
            .spawn(async move {
                wisp_net::stream_chat(request, protocol, sender);
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
                    let more = match event {
                        ChatEvent::Text(text) => {
                            card.note.clear();
                            card.result.push_str(&text);
                            true
                        }
                        ChatEvent::Retrying(attempt, why) => {
                            card.note = tf!("模型服务暂时没有回应（{why}），正在重试，第 {attempt} 次…", "The model service has not responded ({why}). Retrying, attempt {attempt}…");
                            true
                        }
                        ChatEvent::Error(error) => {
                            (card.busy, card.error) = (false, error);
                            card.note.clear();
                            false
                        }
                        ChatEvent::Done(turn) => {
                            card.busy = false;
                            card.note = ai::stop_note(&turn.stop).unwrap_or_default().to_string();
                            if card.result.trim().is_empty() && card.error.is_empty() { card.error = t("模型没有返回内容。").into() }
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

    /// Stop the request in flight, keeping what arrived.
    pub fn assist_stop(&mut self, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let assist = &mut lib.writing.assist;
        assist.generation += 1;
        assist.awaiting = None;
        if let Some(card) = assist.card.as_mut().filter(|card| card.busy) {
            card.busy = false;
            // What 配引用 had found by then stays, for the person to tick.
            let partial = card.cite.as_ref().map_or(!card.result.trim().is_empty(), |found| !found.rows.is_empty());
            if let Some(found) = card.cite.as_mut() { found.stage = crate::write_cite::Stage::Done }
            card.note = if partial { t("已停止，下面是停止前收到的部分。").into() } else { t("已停止。").into() };
        }
        cx.notify();
    }

    /// Do what the person chose with the result.
    pub fn assist_use(&mut self, how: Use, window: &mut Window, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let writing = &mut lib.writing;
        if how == Use::Drop {
            writing.assist.generation += 1;
            writing.assist.awaiting = None;
            writing.assist.card = None;
            return cx.notify();
        }
        let Some(card) = writing.assist.card.as_ref().filter(|card| !card.busy && card.cite.is_none()) else { return };
        let result = card.result.trim().to_string();
        if result.is_empty() { return }
        if how == Use::Copy {
            cx.write_to_clipboard(ClipboardItem::new_string(result));
            return self.say(t("已复制"), cx);
        }
        // The keys the result cites stay with their papers from now on, as when a paper is cited by hand.
        let pins: Vec<(String, String)> = card.keys.iter().filter(|(_, key)| wisp_core::citefind::cites(&result, key)).cloned().collect();
        // The draft may have been edited meanwhile: the paragraph is found again by what it says.
        let text = writing.editor.read(cx).text().to_string();
        let found = if text.get(card.range.clone()) == Some(card.original.as_str()) { Some(card.range.clone()) } else { text.find(&card.original).map(|at| at..at + card.original.len()) };
        let (changed, why) = (how == Use::Replace && found.is_some(), (card.original.clone(), crate::i18n::assist_title(&card.assist, &card.language)));
        match (how, found) {
            (Use::Replace, Some(range)) => writing.editor.update(cx, |input, cx| input.replace_range(range, &result, cx)),
            (Use::Replace, None) => return self.fail(t("原来那段已经改动，找不到了：可以复制结果，自己放进去。"), cx),
            (_, found) => {
                // Below the paragraph's line; without it, at the end of the draft.
                let end = found.map_or(text.len(), |range| text[range.end..].find('\n').map_or(text.len(), |at| range.end + at));
                let before = if text[..end].trim().is_empty() { "" } else { "\n\n" };
                writing.editor.update(cx, |input, cx| input.replace_range(end..end, &format!("{before}{result}"), cx));
            }
        }
        writing.assist.card = None;
        writing.editor.read(cx).focus(window);
        for (paper, key) in pins {
            if lib.citing.kept.contains_key(&key) { continue }
            if lib.store.keep_cite_key(&paper, &key).is_ok() { lib.citing.kept.insert(key, paper); }
        }
        // In a manuscript that came from a Word file, what the model changed is said on the spot, as a comment.
        if changed { self.comment_on_change(&why.0, &result, &why.1) }
        cx.notify();
    }

    /// The actions for the selection (a popover beside it) and for the caret's paragraph (under the manuscript),
    /// in groups a line apart: what looks in the library and changes nothing, what proposes a new version of the words, and the person's own notes and cuts.
    pub fn render_assist_bar(&self, floating: bool, cx: &mut Context<Self>) -> Div {
        let lib = self.lib.as_ref().unwrap();
        let writing = &lib.writing;
        let language = self.manuscript_language();
        let busy = writing.assist.card.as_ref().is_some_and(|card| card.busy);
        let configured = self.settings.profile_for_or_default(Task::Chat).missing().is_none();
        let height = if floating { 30. } else { 24. };
        let button = |id: ElementId, icon: Option<&'static str>, name: String, tip: String, able: bool, cmd: Cmd, cx: &mut Context<Self>| {
            ui::ghost_button(id, icon, Some(name.into()))
                .h(px(height))
                .px(px(7.))
                .text_size(text_px(12.))
                .when(!able, |d| d.opacity(0.45).cursor_default())
                .when(able, |d| d.on_click(self.on(cmd, cx)))
                .when(!tip.is_empty(), |d| d.tooltip(move |_, cx| cx.new(|_| Tip(tip.clone())).into()))
                .into_any_element()
        };
        let rule = || div().flex_none().w(px(1.)).h(px(14.)).mx(px(4.)).bg(Theme::line()).into_any_element();
        let key = self.find_key();
        // Looking in the library: nothing of the text changes.
        let mut items = vec![
            button("write-bar-find".into(), Some("scan-text"), t("找句子").into(), tf!("在文献库里找与这段相近的句子，列在右边供参考和引用（{key}）。不改你的文字。", "Find sentences in the library similar to this text, listed at the right for reference and citing ({key}). Your text is not changed."), true, Cmd::WriteFind, cx),
            button("write-bar-cite".into(), Some("library"), crate::i18n::assist_title(&Assist::Cite, &language), t("在文献库里逐句找可以引用的文献，列出每篇和它自己的原句。勾选后只把引用加到句末，不改你的文字。").into(), !busy, Cmd::WriteAssist(Assist::Cite), cx),
            rule(),
        ];
        // A new version of the words, for the person to take or leave.
        if configured {
            let mut actions = vec![Assist::Polish, Assist::Translate, Assist::Rewrite];
            actions.extend(self.settings.commands.iter().filter(|command| !command.template.trim().is_empty()).map(|command| Assist::Own(command.name.clone())));
            for (n, assist) in actions.into_iter().enumerate() {
                let tip = match &assist {
                    Assist::Polish => t("只改语法、用词和不通顺的地方。给出改后的文字并标出改了哪里，由你决定是否替换。").to_string(),
                    Assist::Translate => t("把这段译成稿件的语言，用这个领域的论文惯用的说法。由你决定是否替换。").to_string(),
                    Assist::Rewrite => t("先在文献库里找与这段相近的句子，再参照它们的结构和措辞改写；意思以你写的为准。由你决定是否替换。").to_string(),
                    Assist::Own(name) => self.settings.commands.iter().find(|command| command.name == *name).map(|command| command.about.clone()).unwrap_or_default(),
                    Assist::Cite => String::new(),
                };
                items.push(button(("write-assist", n).into(), (n == 0).then_some("sparkle"), crate::i18n::assist_title(&assist, &language), tip, !busy, Cmd::WriteAssist(assist), cx));
            }
        } else {
            items.push(button("write-assist-settings".into(), Some("settings-gear"), t("配置模型").into(), t("配置模型后可以润色、翻译和仿写").into(), true, Cmd::OpenSettings(SettingsPage::Ai), cx));
        }
        items.push(rule());
        items.push(button("write-comment".into(), Some("message-square"), t("批注").into(), t("给这段文字写一条批注").into(), true, Cmd::Comment(crate::comments::Do::Write), cx));
        // The caret's paragraph as a heading: the section is cut there.
        if !floating {
            items.push(button("write-split-heading".into(), Some("heading"), t("设为标题").into(), t("这一段成为新一节的名称，后面的文字归入新的一节").into(), true, Cmd::SplitSection(true), cx));
            items.push(button("write-split".into(), Some("scissors"), t("从这里拆成新节").into(), t("从这一段起拆成新的一节，再给它起名").into(), true, Cmd::SplitSection(false), cx));
        }
        div()
            .flex()
            .flex_none()
            .items_center()
            .flex_wrap()
            .gap(px(2.))
            .when(floating, |d| d.p(px(4.)).text_color(Theme::text()))
            .when(!floating, |d| d.px(px(16.)).pb(px(6.)))
            .children(items)
            // What the bar under the manuscript works on, and what its groups are, said on hover instead of in front of it.
            .when(!floating, |d| d.child(ui::hint("write-assist-about", t("这些按钮对光标所在的段落；选中文字时它们出现在选区旁，对选中的文字。「找句子」「配引用」只在文献库里查找，不改文字；「润色」「翻译」「仿写」给出改后的文字，由你决定是否替换。"))))
    }

    /// What the last action came to, over the candidates; `None` without one for this part.
    /// `width` is the panel's: wrapped text is measured by it, not by what a column would give it.
    pub fn render_assist_card(&self, width: f32, cx: &mut Context<Self>) -> Option<AnyElement> {
        let lib = self.lib.as_ref().unwrap();
        let card = lib.writing.assist.card.as_ref().filter(|card| card.section == lib.writing.section_id)?;
        let small = |id: &'static str, name: &'static str| ui::ghost_button(id, None, Some(name.into())).h(text_px(24.)).px(px(7.)).text_size(text_px(12.));
        // 配引用 lists papers; every other action shows a version of the words beside the original.
        let (body, actions, working) = match &card.cite {
            Some(found) => ((card.busy || !found.rows.is_empty()).then(|| self.render_cite_body(card, found, cx)), (!card.busy && found.offering()).then(|| self.render_cite_actions(found, cx)), false),
            None => {
                let done = !card.busy && !card.result.trim().is_empty();
                ((card.busy || !card.result.is_empty()).then(|| self.render_assist_compared(card, done)), done.then(|| self.render_assist_actions(cx)), card.busy && !card.result.is_empty())
            }
        };
        let card_el = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(width - 20.))
            .gap(px(8.))
            .mx(px(10.))
            .mb(px(10.))
            .px(px(12.))
            .py(px(10.))
            .rounded(px(10.))
            .border_1()
            .border_color(Theme::line())
            .bg(Theme::bg_body())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(ui::icon(if card.cite.is_some() { "library" } else { "sparkle" }, 13., Theme::accent()))
                    .child(div().flex_none().text_size(text_px(12.)).font_weight(FontWeight::MEDIUM).child(crate::i18n::assist_title(&card.assist, &card.language)))
                    .when(working, |d| d.child(ui::spinner("write-assist-busy", 11., Theme::accent())))
                    .child(div().flex_1().min_w_0().truncate().text_right().text_size(text_px(11.)).text_color(Theme::text_tertiary()).child(card.model.clone()))
                    .when(card.busy, |d| d.child(small("write-assist-stop", t("停止")).on_click(self.on(Cmd::WriteAssistStop, cx))))
                    .when(!card.busy, |d| d.child(ui::ghost_button("write-assist-close", Some("x"), None).size(px(22.)).min_w(px(22.)).on_click(self.on(Cmd::WriteAssistUse(Use::Drop), cx)))),
            )
            // A request refused before it was sent has nothing to show.
            .children(body)
            .when(!card.note.is_empty(), |d| d.child(div().text_size(text_px(11.)).line_height(text_px(16.)).text_color(Theme::text_tertiary()).child(card.note.clone())))
            .when(!card.error.is_empty(), |d| d.child(ui::error_line(t(&card.error))))
            .children(actions);
        Some(card_el.into_any_element())
    }

    /// The original and the result of an action that gives a new version of the words.
    fn render_assist_compared(&self, card: &Card, done: bool) -> Stateful<Div> {
        // Words marked once the answer is in: while it streams the comparison would flicker.
        // A command of the person's own may answer with comments, which are not a version of the paragraph.
        let marks = if done && !matches!(card.assist, Assist::Own(_)) { ai::word_diff(&card.original, card.result.trim()) } else { None };
        let (before, after) = match &marks {
            Some((before, after)) => (marked(before), marked(after)),
            None => (plain(&card.original), plain(card.result.trim())),
        };
        let label = |text: &'static str| div().text_size(text_px(11.)).text_color(Theme::text_tertiary()).child(text);
        div()
            .id("write-assist-body")
            .flex()
            .flex_col()
            .gap(px(8.))
            .max_h(px(380.))
            .overflow_y_scroll()
            .child(label(t("原文")))
            .child(div().px(px(10.)).py(px(8.)).rounded(px(8.)).bg(Theme::bg_sunken()).text_size(text_px(12.)).line_height(text_px(19.)).text_color(Theme::text_secondary()).child(before))
            .child(label(match card.assist { Assist::Translate => t("译文"), Assist::Own(_) => t("结果"), _ => t("改后") }))
            .child(if card.result.trim().is_empty() && card.busy {
                div().flex().items_center().gap(px(6.)).text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(ui::spinner("write-assist-wait", 12., Theme::accent())).child(if card.note.is_empty() { t("正在写…") } else { "" })
            } else {
                div().text_size(text_px(13.)).line_height(text_px(21.)).text_color(Theme::text()).child(after)
            })
    }

    /// What can be done with a new version of the words.
    fn render_assist_actions(&self, cx: &mut Context<Self>) -> Div {
        let small = |id: &'static str, name: &'static str| ui::ghost_button(id, None, Some(name.into())).h(text_px(24.)).px(px(7.)).text_size(text_px(12.));
        div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap(px(4.))
            .child(ui::primary_button("write-assist-replace", t("替换原文")).h(text_px(26.)).px(px(10.)).text_size(text_px(12.)).on_click(self.on(Cmd::WriteAssistUse(Use::Replace), cx)))
            .child(ui::secondary_button("write-assist-below", t("插在下面")).h(text_px(26.)).px(px(10.)).text_size(text_px(12.)).on_click(self.on(Cmd::WriteAssistUse(Use::Below), cx)))
            .child(small("write-assist-copy", t("复制")).on_click(self.on(Cmd::WriteAssistUse(Use::Copy), cx)))
            .child(small("write-assist-drop", t("放弃")).on_click(self.on(Cmd::WriteAssistUse(Use::Drop), cx)))
            .child(div().flex_1())
            .child(div().flex_none().text_size(text_px(11.)).text_color(Theme::text_tertiary()).child(if cfg!(target_os = "macos") { t("⌘Z 可撤销") } else { t("Ctrl+Z 可撤销") }))
    }
}

/// A selection without the spaces and line breaks around it.
fn trimmed(text: &str, range: Range<usize>) -> Range<usize> {
    let picked = &text[range.clone()];
    let start = range.start + (picked.len() - picked.trim_start().len());
    start..start + picked.trim().len()
}

fn plain(text: &str) -> StyledText {
    StyledText::new(text.to_string())
}

/// A version of the paragraph with what the other lacks marked: struck out in the original, shaded in the result.
pub(crate) fn marked(pieces: &[(Change, String)]) -> StyledText {
    let (mut text, mut highlights) = (String::new(), vec![]);
    for (n, (change, piece)) in pieces.iter().enumerate() {
        let range = text.len()..text.len() + piece.len();
        text.push_str(piece);
        // A space between two changed words is part of one change, not a gap in it.
        let between = piece.trim().is_empty() && n > 0 && pieces.get(n + 1).is_some_and(|(next, _)| *next != Change::Same && *next == pieces[n - 1].0);
        let change = if between { pieces[n - 1].0 } else { *change };
        let style = match change {
            Change::Same => continue,
            Change::Removed => HighlightStyle { color: Some(Theme::danger()), strikethrough: Some(StrikethroughStyle { thickness: px(1.), color: None }), ..Default::default() },
            Change::Added => HighlightStyle { background_color: Some(Theme::accent_soft()), ..Default::default() },
        };
        // Spaces alone are not worth marking.
        if between || !piece.trim().is_empty() { highlights.push((range, style)) }
    }
    StyledText::new(text).with_highlights(highlights)
}

/// The description of one of the person's commands, over its button.
pub(crate) struct Tip(pub(crate) String);

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().max_w(px(280.)).px(px(8.)).py(px(5.)).rounded(px(6.)).border_1().border_color(Theme::line()).bg(Theme::bg_body()).text_size(text_px(12.)).text_color(Theme::text_secondary()).child(self.0.clone())
    }
}
