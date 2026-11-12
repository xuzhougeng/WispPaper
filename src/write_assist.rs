//! The model on the writing page: an action on the selected text of the
//! manuscript, or the paragraph the caret is in (rewrite after the
//! candidates beside it, put into the manuscript's language, polish, or
//! one of the person's own commands). The answer streams into a card over
//! the candidates, beside the original with the words that changed marked;
//! only the person puts it into the draft, and Undo takes that back.

use crate::app::{Cmd, Workspace};
use crate::settings::SettingsPage;
use crate::theme::Theme;
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use std::ops::Range;
use wisp_core::ai::{self, Assist, Change, ChatEvent};
use wisp_core::settings::Task;

/// The writing page's request to the model, if one was made.
#[derive(Default)]
pub struct Assisting {
    pub card: Option<Card>,
    /// Counts the requests: an answer to an earlier one, stopped or dropped, is not shown.
    generation: u64,
}

/// One action's result, with what it was asked about.
pub struct Card {
    pub assist: Assist,
    pub title: String,
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
        self.card = None;
    }
}

impl Workspace {
    /// The language the manuscript is written in, as the prompts name it.
    /// English until the manuscript carries its own (another change adds that; read it here).
    pub fn manuscript_language(&self) -> String {
        "English".into()
    }

    /// Ask the model to do `assist` on the selection, or on the paragraph the caret is in.
    pub fn assist_start(&mut self, assist: Assist, cx: &mut Context<Self>) {
        let profile = self.settings.profile_for_or_default(Task::Chat);
        let language = self.manuscript_language();
        let own = self.settings.commands.clone();
        let Some(lib) = self.lib.as_mut() else { return };
        let writing = &mut lib.writing;
        if writing.assist.card.as_ref().is_some_and(|card| card.busy) { return }
        let (text, selection, caret) = {
            let editor = writing.editor.read(cx);
            (editor.text().to_string(), editor.selection(), editor.caret())
        };
        let range = if selection.is_empty() { ai::paragraph_at(&text, caret) } else { trimmed(&text, selection) };
        let original = text[range.clone()].to_string();
        let sentences = writing.candidates.as_ref().and_then(|found| found.as_ref().ok()).and_then(|found| found["sentences"].as_array()).cloned().unwrap_or_default();
        let examples: Vec<ai::Example> = sentences
            .iter()
            .map(|sentence| {
                let field = |key: &str| sentence[key].as_str().unwrap_or("").to_string();
                ai::Example { text: field("text"), cite: field("cite"), page: sentence["page"].as_u64().map(|page| page as usize), heading: field("heading") }
            })
            .collect();
        writing.assist.generation += 1;
        let generation = writing.assist.generation;
        let mut card = Card { title: assist.title(&language), assist: assist.clone(), section: writing.section.clone(), original, range, result: String::new(), busy: true, error: String::new(), note: String::new(), model: format!("{} · {}", profile.title(), profile.model) };
        let request = ai::assist_request(&profile, &assist, &language, &card.original, &examples, &own);
        let request = match request {
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
        cx.background_executor().spawn(async move { wisp_net::stream_chat(request, protocol, sender) }).detach();
        cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let alive = this.update(cx, |this, cx| {
                    let Some(lib) = this.lib.as_mut().filter(|l| l.info.id == library) else { return false };
                    let writing = &mut lib.writing;
                    if writing.assist.generation != generation { return false }
                    // The person went on to another part: an answer about this one is not wanted there.
                    if writing.section != section {
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
                            card.note = format!("模型服务暂时没有回应（{why}），正在重试，第 {attempt} 次…");
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
                            if card.result.trim().is_empty() && card.error.is_empty() { card.error = "模型没有返回内容。".into() }
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
        if let Some(card) = assist.card.as_mut().filter(|card| card.busy) {
            card.busy = false;
            card.note = if card.result.trim().is_empty() { "已停止。".into() } else { "已停止，下面是停止前收到的部分。".into() };
        }
        cx.notify();
    }

    /// Do what the person chose with the result.
    pub fn assist_use(&mut self, how: Use, window: &mut Window, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let writing = &mut lib.writing;
        if how == Use::Drop {
            writing.assist.generation += 1;
            writing.assist.card = None;
            return cx.notify();
        }
        let Some(card) = writing.assist.card.as_ref().filter(|card| !card.busy) else { return };
        let result = card.result.trim().to_string();
        if result.is_empty() { return }
        if how == Use::Copy {
            cx.write_to_clipboard(ClipboardItem::new_string(result));
            return self.say("已复制", cx);
        }
        // The draft may have been edited meanwhile: the paragraph is found again by what it says.
        let text = writing.editor.read(cx).text().to_string();
        let found = if text.get(card.range.clone()) == Some(card.original.as_str()) { Some(card.range.clone()) } else { text.find(&card.original).map(|at| at..at + card.original.len()) };
        match (how, found) {
            (Use::Replace, Some(range)) => writing.editor.update(cx, |input, cx| input.replace_range(range, &result, cx)),
            (Use::Replace, None) => return self.fail("原来那段已经改动，找不到了：可以复制结果，自己放进去。", cx),
            (_, found) => {
                // Below the paragraph's line; without it, at the end of the draft.
                let end = found.map_or(text.len(), |range| text[range.end..].find('\n').map_or(text.len(), |at| range.end + at));
                let before = if text[..end].trim().is_empty() { "" } else { "\n\n" };
                writing.editor.update(cx, |input, cx| input.replace_range(end..end, &format!("{before}{result}"), cx));
            }
        }
        writing.assist.card = None;
        writing.editor.read(cx).focus(window);
        cx.notify();
    }

    /// The actions under the manuscript, for the selection or the caret's paragraph.
    pub fn render_assist_bar(&self, cx: &mut Context<Self>) -> Div {
        let lib = self.lib.as_ref().unwrap();
        let writing = &lib.writing;
        let language = self.manuscript_language();
        let selected = !writing.editor.read(cx).selection().is_empty();
        let busy = writing.assist.card.as_ref().is_some_and(|card| card.busy);
        let configured = self.settings.profile_for_or_default(Task::Chat).missing().is_none();
        let mut actions = vec![Assist::Rewrite, Assist::Translate, Assist::Polish];
        actions.extend(self.settings.commands.iter().filter(|command| !command.template.trim().is_empty()).map(|command| Assist::Own(command.name.clone())));
        let mut button = |n: usize, assist: &Assist| {
            let tip = match assist {
                Assist::Own(name) => self.settings.commands.iter().find(|command| command.name == *name).map(|command| command.about.clone()).unwrap_or_default(),
                _ => String::new(),
            };
            ui::ghost_button(("write-assist", n), (n == 0).then_some("sparkle"), Some(assist.title(&language).into()))
                .h(px(24.))
                .px(px(7.))
                .text_size(px(12.))
                .when(busy, |d| d.opacity(0.45).cursor_default())
                .when(!busy, |d| d.on_click(self.on(Cmd::WriteAssist(assist.clone()), cx)))
                .when(!tip.is_empty(), |d| d.tooltip(move |_, cx| cx.new(|_| Tip(tip.clone())).into()))
        };
        let buttons: Vec<AnyElement> = if configured { actions.iter().enumerate().map(|(n, assist)| button(n, assist).into_any_element()).collect() } else { vec![] };
        div()
            .flex()
            .flex_none()
            .items_center()
            .flex_wrap()
            .gap(px(2.))
            .px(px(20.))
            .pb(px(6.))
            .child(div().flex_none().mr(px(4.)).text_size(px(11.)).text_color(Theme::text_tertiary()).child(if selected { "对选中的文字：" } else { "对光标所在的段落：" }))
            .when(configured, |d| d.children(buttons))
            .when(!configured, |d| d.child(ui::ghost_button("write-assist-settings", Some("settings-gear"), Some("配置模型后可以改写、翻译和润色".into())).h(px(24.)).text_size(px(12.)).on_click(self.on(Cmd::OpenSettings(SettingsPage::Ai), cx))))
    }

    /// The result of the last action, over the candidates; `None` without one for this part.
    /// `width` is the panel's: wrapped text is measured by it, not by what a column would give it.
    pub fn render_assist_card(&self, width: f32, cx: &mut Context<Self>) -> Option<AnyElement> {
        let lib = self.lib.as_ref().unwrap();
        let card = lib.writing.assist.card.as_ref().filter(|card| card.section == lib.writing.section)?;
        let done = !card.busy && !card.result.trim().is_empty();
        // Words marked once the answer is in: while it streams the comparison would flicker.
        // A command of the person's own may answer with comments, which are not a version of the paragraph.
        let marks = if done && !matches!(card.assist, Assist::Own(_)) { ai::word_diff(&card.original, card.result.trim()) } else { None };
        let (before, after) = match &marks {
            Some((before, after)) => (marked(before), marked(after)),
            None => (plain(&card.original), plain(card.result.trim())),
        };
        let small = |id: &'static str, name: &'static str| ui::ghost_button(id, None, Some(name.into())).h(px(24.)).px(px(7.)).text_size(px(12.));
        let label = |text: &'static str| div().text_size(px(11.)).text_color(Theme::text_tertiary()).child(text);
        let body = div()
            .id("write-assist-body")
            .flex()
            .flex_col()
            .gap(px(8.))
            .max_h(px(380.))
            .overflow_y_scroll()
            .child(label("原文"))
            .child(div().px(px(10.)).py(px(8.)).rounded(px(8.)).bg(Theme::bg_sunken()).text_size(px(12.)).line_height(px(19.)).text_color(Theme::text_secondary()).child(before))
            .child(label(match card.assist { Assist::Translate => "译文", Assist::Own(_) => "结果", _ => "改后" }))
            .child(if card.result.trim().is_empty() && card.busy {
                div().flex().items_center().gap(px(6.)).text_size(px(12.)).text_color(Theme::text_tertiary()).child(ui::spinner("write-assist-wait", 12., Theme::accent())).child("正在写…")
            } else {
                div().text_size(px(13.)).line_height(px(21.)).text_color(Theme::text()).child(after)
            });
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
                    .child(ui::icon("sparkle", 13., Theme::accent()))
                    .child(div().flex_none().text_size(px(12.)).font_weight(FontWeight::MEDIUM).child(card.title.clone()))
                    .when(card.busy && !card.result.is_empty(), |d| d.child(ui::spinner("write-assist-busy", 11., Theme::accent())))
                    .child(div().flex_1().min_w_0().truncate().text_right().text_size(px(11.)).text_color(Theme::text_tertiary()).child(card.model.clone()))
                    .when(card.busy, |d| d.child(small("write-assist-stop", "停止").on_click(self.on(Cmd::WriteAssistStop, cx))))
                    .when(!card.busy, |d| d.child(ui::ghost_button("write-assist-close", Some("x"), None).size(px(22.)).min_w(px(22.)).on_click(self.on(Cmd::WriteAssistUse(Use::Drop), cx)))),
            )
            // A request refused before it was sent has nothing to compare.
            .when(card.busy || !card.result.is_empty(), |d| d.child(body))
            .when(!card.note.is_empty(), |d| d.child(div().text_size(px(11.)).text_color(Theme::text_tertiary()).child(card.note.clone())))
            .when(!card.error.is_empty(), |d| d.child(ui::error_line(&card.error)))
            .when(done, |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .flex_wrap()
                        .gap(px(4.))
                        .child(ui::primary_button("write-assist-replace", "替换原文").h(px(26.)).px(px(10.)).text_size(px(12.)).on_click(self.on(Cmd::WriteAssistUse(Use::Replace), cx)))
                        .child(ui::secondary_button("write-assist-below", "插在下面").h(px(26.)).px(px(10.)).text_size(px(12.)).on_click(self.on(Cmd::WriteAssistUse(Use::Below), cx)))
                        .child(small("write-assist-copy", "复制").on_click(self.on(Cmd::WriteAssistUse(Use::Copy), cx)))
                        .child(small("write-assist-drop", "放弃").on_click(self.on(Cmd::WriteAssistUse(Use::Drop), cx)))
                        .child(div().flex_1())
                        .child(div().flex_none().text_size(px(11.)).text_color(Theme::text_tertiary()).child(if cfg!(target_os = "macos") { "⌘Z 可撤销" } else { "Ctrl+Z 可撤销" })),
                )
            });
        Some(card_el.into_any_element())
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
fn marked(pieces: &[(Change, String)]) -> StyledText {
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
struct Tip(String);

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().max_w(px(280.)).px(px(8.)).py(px(5.)).rounded(px(6.)).border_1().border_color(Theme::line()).bg(Theme::bg_body()).text_size(px(12.)).text_color(Theme::text_secondary()).child(self.0.clone())
    }
}
