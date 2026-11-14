//! 助手写: 写作's other mode (`wisp_core::compose`). The outline stays at
//! the left; in the middle the person talks with the assistant, and at the
//! right stands the manuscript, which the assistant writes with its tools
//! (`edit_section`, `set_outline`) while the person reads, corrects by hand
//! and goes back to an earlier version when a change was for the worse.
//! The conversation is `Library::write_chat`, the third of a library's
//! conversations (`Talk::Write`), kept in the history under the manuscript
//! it writes. After a tool wrote, the window reads the manuscripts again
//! (`manuscripts_synced`): the editor takes each change as a step of its
//! own Undo, the section changed is shown, and what changed is lit the way
//! 改稿 lights it (`Revising::show`), each change to be gone to or taken back.

use crate::app::{Cmd, Edge, Talk, Workspace};
use crate::i18n::t;
use crate::theme::{Theme, text_px};
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use std::collections::HashMap;
use std::ops::Range;
use wisp_core::compose;
use wisp_core::manuscript::{self, Snapshot};
use wisp_core::settings::Task;

/// The least the manuscript and the conversation are given of the page's width, and the share the manuscript comes with.
const MANUSCRIPT_MIN: f32 = 340.;
const CHAT_MIN: f32 = 440.;
const MANUSCRIPT_SHARE: f32 = 0.52;

/// What the mode keeps beside the manuscript.
#[derive(Default)]
pub struct Agent {
    /// The manuscript the conversation is about: its conversations are kept under it.
    pub about: String,
    /// What the sections read before the assistant changed them in answer to the question in hand, by section.
    pub changed: HashMap<String, String>,
    /// That question, as the changes are said to answer it.
    pub asked: String,
    pub versions: Versions,
}

/// The earlier versions of the section on show, to be looked at in its place.
#[derive(Default)]
pub struct Versions {
    /// Oldest first; the text as it is now comes after the last.
    pub kept: Vec<Snapshot>,
    /// The one on show; none is the text as it is.
    pub at: Option<usize>,
}

impl Versions {
    pub fn shown(&self) -> Option<&Snapshot> {
        self.at.and_then(|at| self.kept.get(at))
    }
}

/// What the mode can be told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Do {
    /// The assistant writes (true), or the person does.
    Mode(bool),
    /// The version before the one on show (false), or the one after.
    Step(bool),
    /// The text as it is now.
    Now,
    /// Put the version on show back into the section.
    Restore,
}

impl Workspace {
    pub(crate) fn run_agent(&mut self, what: Do, window: &mut Window, cx: &mut Context<Self>) {
        match what {
            Do::Mode(on) => {
                if self.settings.agent_writing == on { return }
                self.settings.agent_writing = on;
                self.save_settings(cx);
                self.agent_attend(cx);
                self.agent_versions(cx);
                let Some(lib) = self.lib.as_mut() else { return };
                lib.writing.agent.versions.at = None;
                if on { lib.write_chat.input.read(cx).focus(window) } else { lib.writing.editor.read(cx).focus(window) }
            }
            Do::Step(later) => {
                // The versions as they are kept now: one may have been taken since they were last read.
                let at = self.lib.as_ref().and_then(|lib| lib.writing.agent.versions.at);
                self.agent_versions(cx);
                let Some(lib) = self.lib.as_mut() else { return };
                let versions = &mut lib.writing.agent.versions;
                versions.at = match (at, later) {
                    (None, false) => versions.kept.len().checked_sub(1),
                    (None, true) => None,
                    (Some(at), false) => (!versions.kept.is_empty()).then_some(at.saturating_sub(1).min(versions.kept.len().saturating_sub(1))),
                    (Some(at), true) => Some(at + 1).filter(|next| *next < versions.kept.len()),
                };
            }
            Do::Now => {
                if let Some(lib) = self.lib.as_mut() { lib.writing.agent.versions.at = None }
            }
            Do::Restore => {
                let Some(lib) = self.lib.as_mut() else { return };
                let Some(version) = lib.writing.agent.versions.shown().cloned() else { return };
                match lib.store.restore_snapshot(version.id) {
                    Ok(_) => {
                        // The marks were of the text given up.
                        lib.writing.agent.changed.remove(&version.section_id);
                        self.write_reload(Some(version.section_id), cx);
                        self.say(t("已恢复这一版。恢复前的文字也留了一个版本，可以再换回去。"), cx);
                    }
                    Err(err) => self.fail(format!("{err:#}"), cx),
                }
            }
        }
        cx.notify();
    }

    /// The conversation that writes is about the manuscript on show: one about another manuscript is put away
    /// (it is in that manuscript's history) and this manuscript's last one is taken up again.
    pub(crate) fn agent_attend(&mut self, cx: &mut Context<Self>) {
        let Some(manuscript) = self.lib.as_ref().map(|lib| lib.writing.manuscript.clone()).filter(|id| !id.is_empty()) else { return };
        if self.lib.as_ref().is_some_and(|lib| lib.writing.agent.about == manuscript) { return }
        // An answer still being given was about the manuscript left: it is kept as far as it got.
        self.stop_chat(Talk::Write, cx);
        let Some(lib) = self.lib.as_mut() else { return };
        lib.writing.agent = Agent { about: manuscript.clone(), ..Default::default() };
        match lib.store.list_conversations(&manuscript).ok().and_then(|list| list.into_iter().next()) {
            Some(last) => self.open_chat(Talk::Write, &last.id, cx),
            None => lib.write_chat.reset(cx),
        }
    }

    /// Read the versions kept of the section on show; the text as it is now is on show again.
    pub(crate) fn agent_versions(&mut self, cx: &App) {
        let Some(lib) = self.lib.as_mut() else { return };
        let section = lib.writing.section_id.clone();
        let mut kept = lib.store.snapshots(&section).unwrap_or_default();
        kept.reverse();
        // The newest that says what the section says now is no version to go back to.
        let text = lib.writing.editor.read(cx).text().trim_end().to_string();
        if kept.last().is_some_and(|last| last.body.trim_end() == text) { kept.pop(); }
        lib.writing.agent.versions = Versions { kept, at: None };
    }

    /// A question is on its way to the assistant that writes: what the last one changed has been seen.
    pub(crate) fn agent_asked(&mut self, question: &str) {
        let Some(lib) = self.lib.as_mut() else { return };
        lib.writing.agent.changed.clear();
        lib.writing.agent.asked = question.split_whitespace().collect::<Vec<_>>().join(" ");
        lib.writing.agent.versions.at = None;
        if lib.writing.revise.run.as_ref().is_some_and(|run| run.told) { lib.writing.revise.leave() }
    }

    /// What a question of that conversation tells the model about the manuscript (`compose::note`).
    pub(crate) fn agent_note(&self, cx: &App) -> Option<String> {
        let lib = self.lib.as_ref()?;
        let writing = &lib.writing;
        let manuscript = writing.manuscripts.iter().find(|m| m.id == writing.agent.about)?;
        // What is on show and selected is said only of the manuscript the conversation is about.
        let here = writing.manuscript == manuscript.id && !writing.reading;
        let selection = if here { writing.editor.read(cx).selected().to_string() } else { String::new() };
        Some(compose::note(&compose::Asked { manuscript, showing: if here { &writing.section_id } else { "" }, selection: &selection }))
    }

    /// A tool wrote a manuscript: the window reads them again. The editor of the section on show takes each
    /// change as a step of its own Undo; when the changes are elsewhere, the section last changed is shown
    /// (in the assistant's mode); and what changed is lit.
    pub(crate) fn manuscripts_synced(&mut self, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let now = match lib.store.manuscripts() {
            Ok(list) => list,
            Err(err) => return self.fail(format!("{err:#}"), cx),
        };
        if now == lib.writing.manuscripts { return }
        let before = std::mem::replace(&mut lib.writing.manuscripts, now);
        let writing = &mut lib.writing;
        let Some(current) = writing.manuscripts.iter().find(|m| m.id == writing.manuscript) else {
            // The manuscript on show is no longer listed: nothing of this window's to bring along.
            return;
        };
        let was = before.iter().find(|m| m.id == current.id);
        let reshaped = was.is_none_or(|was| was.sections.iter().map(|s| (&s.id, &s.name, &s.kind)).ne(current.sections.iter().map(|s| (&s.id, &s.name, &s.kind))));
        // The sections that read otherwise now, with what they read before the first change in answer to this question.
        let mut touched: Vec<String> = vec![];
        // In a manuscript that came from a Word file each change is said where it was made, as 改稿 says its own: section, the words before, the words now.
        let mut said: Vec<(String, String, String)> = vec![];
        for section in &current.sections {
            let old = was.and_then(|was| was.section(&section.id)).map_or("", |s| s.body.as_str());
            if old == section.body { continue }
            writing.agent.changed.entry(section.id.clone()).or_insert_with(|| old.to_string());
            touched.push(section.id.clone());
            said.extend(compose::hunks(old, &section.body).into_iter().map(|hunk| (section.id.clone(), old[hunk.before].to_string(), section.body[hunk.after].to_string())));
        }
        let why = writing.agent.asked.clone();
        if was.is_none_or(|was| was.title != current.title) {
            let title = current.title.clone();
            writing.title.update(cx, |input, cx| input.set_text(&title, cx));
        }
        let (manuscript, showing) = (current.id.clone(), writing.section_id.clone());
        let body = current.section(&showing).map(|s| s.body.clone());
        let following = self.settings.agent_writing && lib.page == crate::app::Page::Write && !lib.writing.reading;
        match body {
            // The section on show: its editor takes the changes from the last to the first, so each stays where it belongs.
            Some(body) if touched.contains(&showing) || !following || touched.is_empty() => {
                let text = lib.writing.editor.read(cx).text().to_string();
                for hunk in compose::hunks(&text, &body).iter().rev() {
                    lib.writing.editor.update(cx, |input, cx| input.replace_apart(hunk.before.clone(), &body[hunk.after.clone()], cx));
                }
                self.agent_marks(cx);
                self.agent_versions(cx);
            }
            // The changes are in another section, or the one on show is gone: the assistant is followed there.
            _ => {
                let to = touched.last().cloned().unwrap_or_default();
                self.write_show(manuscript, to, cx);
            }
        }
        for (section, old, new) in &said { self.comment_on_change_in(section, old, new, &why) }
        // The first change comes into view.
        if touched.contains(&self.lib.as_ref().map(|lib| lib.writing.section_id.clone()).unwrap_or_default()) { self.run_revise(crate::revise::Do::Go(0), cx) }
        let Some(lib) = self.lib.as_mut() else { return };
        // What the manuscript cites and what its citations rest on may be more now.
        if let Ok(kept) = lib.store.cite_keys() { lib.citing.kept = kept }
        self.sources_load();
        // A section's kind decides its candidates.
        if reshaped {
            if let Some(lib) = self.lib.as_mut() { lib.writing.search.asked = None }
            self.write_openers(cx);
        }
        cx.notify();
    }

    /// Light what the assistant changed in the section on show in answer to the question in hand.
    pub(crate) fn agent_marks(&mut self, cx: &App) {
        let model = self.settings.profile_for_or_default(Task::Chat).labelled();
        let Some(lib) = self.lib.as_mut() else { return };
        let writing = &mut lib.writing;
        let Some(before) = writing.agent.changed.get(&writing.section_id) else { return };
        let text = writing.editor.read(cx).text().to_string();
        let changes: Vec<(Range<usize>, String)> = compose::hunks(before, &text).into_iter().map(|hunk| (hunk.after, before[hunk.before].to_string())).collect();
        if changes.is_empty() { return }
        writing.revise.show(&writing.section_id, &writing.agent.asked, &model, &text, changes);
    }

    /// How wide the manuscript stands beside the conversation, of a page this wide.
    pub(crate) fn agent_manuscript_width(&self, page: f32) -> f32 {
        let own = if self.settings.agent_manuscript_width > 0. { self.settings.agent_manuscript_width } else { page * MANUSCRIPT_SHARE };
        own.min(page - CHAT_MIN).max(MANUSCRIPT_MIN.min(page * 0.5))
    }

    /// The edge between the conversation and the manuscript is being dragged: the manuscript reaches from the pointer to the page's right edge.
    pub fn drag_manuscript(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.resizing != Some(Edge::Manuscript) { return }
        if event.pressed_button != Some(MouseButton::Left) { return self.end_drag(cx) }
        let right = f32::from(window.viewport_size().width) - self.detail_width(window);
        let page = right - self.sidebar_width(window);
        let width = (right - f32::from(event.position.x)).min(page - CHAT_MIN).max(MANUSCRIPT_MIN.min(page * 0.5));
        if (width - self.settings.agent_manuscript_width).abs() < 0.5 { return }
        self.settings.agent_manuscript_width = width;
        cx.notify();
    }

    /// 写作 with the assistant leading: the conversation, and the manuscript to its right.
    pub(crate) fn render_write_agent(&mut self, width: f32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let manuscript_width = self.agent_manuscript_width(width);
        let chat = self.render_chat(Talk::Write, window, cx);
        let manuscript = self.render_manuscript_column(manuscript_width, true, false, window, cx);
        // The edge between the two: drag it to give either more room, click it twice for the widths they came with.
        let held = self.resizing == Some(Edge::Manuscript);
        let between = div()
            .id("agent-edge")
            .group("agent-edge")
            .absolute()
            .top_0()
            .right(px(manuscript_width - 4.))
            .w(px(9.))
            .h_full()
            .flex()
            .justify_center()
            .cursor_col_resize()
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &MouseDownEvent, _, cx| {
                if event.click_count >= 2 {
                    this.resizing = None;
                    this.settings.agent_manuscript_width = 0.;
                    this.save_settings(cx);
                } else {
                    this.resizing = Some(Edge::Manuscript);
                }
                cx.notify();
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.end_drag(cx)))
            .child(div().w(px(2.)).h_full().when(held, |d| d.bg(Theme::accent())).when(!held, |d| d.group_hover("agent-edge", |s| s.bg(Theme::accent().opacity(0.5)))));
        div()
            .id("write-page")
            .relative()
            .flex()
            .size_full()
            // Widths in pixels for both: a conversation that shares the row is measured wider than it is drawn.
            .child(div().flex_none().w(px(width - manuscript_width)).h_full().overflow_hidden().child(chat))
            // The column fills a place of its own: it comes as one that shares a row, and its basis would outdo a width given to it here.
            .child(div().flex().flex_none().w(px(manuscript_width)).h_full().border_l_1().border_color(Theme::line()).bg(Theme::bg_sunken()).child(manuscript))
            .child(between)
            .into_any_element()
    }

    /// The two ways of writing, to choose between, as two icons: beside the outline's title, or among the manuscript's buttons while the outline is away.
    pub(crate) fn render_mode_switch(&self, cx: &mut Context<Self>) -> Div {
        let on = self.settings.agent_writing;
        let tab = |id: &'static str, icon: &'static str, name: &'static str, tip: &'static str, lit: bool, to: bool, this: &Self, cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .w(px(30.))
                .h(text_px(24.))
                .rounded(px(6.))
                .cursor_pointer()
                .when(lit, |d| d.bg(Theme::segment()))
                .when(!lit, |d| d.hover(|s| s.bg(Theme::hover())))
                .on_click(this.on(Cmd::Agent(Do::Mode(to)), cx))
                // Icons alone, so the name is said with what the mode does.
                .tooltip(move |_, cx| cx.new(|_| crate::write_assist::Tip(format!("{} — {}", t(name), t(tip)))).into())
                .child(ui::icon(icon, 14., if lit { Theme::accent() } else { Theme::text_tertiary() }))
        };
        div()
            .flex()
            .flex_none()
            .gap(px(2.))
            .p(px(2.))
            .rounded(px(8.))
            .bg(Theme::groove())
            .child(tab("write-mode-own", "pen-line", "自己写", "你来写：助手在旁边找句子、配引用，按你的话改一节", !on, false, self, cx))
            .child(tab("write-mode-agent", "sparkle", "助手写", "和助手对话，由它查文献、动笔；稿件在右边，你看着它写，随时亲手改或换回早先的版本", on, true, self, cx))
    }

    /// Over the manuscript beside the conversation: which version of the section is on show, and the way to the others.
    pub(crate) fn render_version_stepper(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let lib = self.lib.as_ref()?;
        let versions = &lib.writing.agent.versions;
        if lib.writing.reading || versions.kept.is_empty() { return None }
        let (count, at) = (versions.kept.len() + 1, versions.at.unwrap_or(versions.kept.len()) + 1);
        let arrow = |id: &'static str, icon: &'static str, on: bool, later: bool, this: &Self, cx: &mut Context<Self>| {
            ui::ghost_button(id, Some(icon), None).size(px(22.)).min_w(px(22.)).when(!on, |d| d.opacity(0.35)).when(on, |d| d.on_click(this.on(Cmd::Agent(Do::Step(later)), cx)))
        };
        Some(
            div()
                .id("agent-versions")
                .flex()
                .flex_none()
                .items_center()
                .h(text_px(26.))
                .px(px(2.))
                .rounded(px(6.))
                .border_1()
                .border_color(if versions.at.is_some() { Theme::accent().opacity(0.45) } else { Theme::line() })
                .tooltip(|_, cx| cx.new(|_| crate::write_assist::Tip(t("这一节的各个版本：助手每次动笔前、你停笔或离开时都会留一版。翻到早先的一版可以看，也可以恢复").to_string())).into())
                .child(arrow("agent-version-earlier", "chevron-left", at > 1, false, self, cx))
                .child(div().flex_none().px(px(4.)).text_size(text_px(12.)).text_color(if versions.at.is_some() { Theme::accent() } else { Theme::text_secondary() }).child(tf!("版本 {at}/{count}", "Version {at}/{count}")))
                .child(arrow("agent-version-later", "chevron-right", at < count, true, self, cx)),
        )
    }

    /// An earlier version of the section, in the manuscript's place: what it said, when it was kept, and the two ways on.
    pub(crate) fn render_version(&self, version: &Snapshot, cx: &mut Context<Self>) -> AnyElement {
        let why = crate::manuscripts::snapshot_why(&version.why);
        let when = wisp_core::time::short_time(&version.taken_at);
        div()
            .id("agent-version")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .rounded(px(10.))
            .border_1()
            .border_color(Theme::accent().opacity(0.45))
            .bg(Theme::bg_body())
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.))
                    .px(px(14.))
                    .py(px(8.))
                    .border_b_1()
                    .border_color(Theme::line())
                    .bg(Theme::accent_soft())
                    .child(ui::icon("history", 13., Theme::accent()))
                    .child(div().flex_1().min_w(px(120.)).text_size(text_px(12.)).text_color(Theme::text()).child(if why.is_empty() { tf!("{when} 的版本", "Version of {when}") } else { tf!("{when} 的版本 · {why}", "Version of {when} · {why}") }))
                    .child(ui::secondary_button("agent-version-now", t("回到现在")).h(text_px(24.)).text_size(text_px(12.)).on_click(self.on(Cmd::Agent(Do::Now), cx)))
                    .child(ui::primary_button("agent-version-restore", t("恢复这一版")).h(text_px(24.)).text_size(text_px(12.)).on_click(self.on(Cmd::Agent(Do::Restore), cx))),
            )
            .child(
                div().id("agent-version-text").flex_1().min_h_0().overflow_y_scroll().child(
                    div()
                        .w_full()
                        .max_w(crate::theme::manuscript_width())
                        .mx_auto()
                        .px(px(18.))
                        .py(px(14.))
                        .text_size(text_px(15.))
                        .text_color(Theme::text())
                        .when(version.body.trim().is_empty(), |d| d.child(div().text_size(text_px(13.)).text_color(Theme::text_tertiary()).child(t("（这一版是空的）"))))
                        .when(!version.body.trim().is_empty(), |d| d.child(crate::markdown::render("agent-version-body", &manuscript::section_markdown(&version.body, 2), None, None))),
                ),
            )
            .into_any_element()
    }

    /// The conversation that writes, before anything is said: what the assistant can be asked to do with this manuscript.
    pub(crate) fn render_agent_start(&self, cx: &mut Context<Self>) -> AnyElement {
        let lib = self.lib.as_ref().unwrap();
        let writing = &lib.writing;
        let manuscript = writing.manuscripts.iter().find(|m| m.id == writing.agent.about);
        let written = manuscript.is_some_and(|m| m.sections.iter().any(|s| !s.body.trim().is_empty()));
        let section = writing.section().map(|s| s.name.clone()).unwrap_or_default();
        // What fits a manuscript that is still to be written, and one that has text.
        let asks: Vec<String> = if written {
            vec![
                tf!("通读全文，说说哪里论证不够、衔接不顺，再把你有把握的地方改好。", "Read the whole manuscript, tell me where the argument is thin or the flow breaks, and fix what you are sure of."),
                tf!("给「{section}」里还没有出处的论断配上文献库里的引用。", "Find citations in the library for the claims of “{section}” that have none."),
                tf!("把「{section}」写得更紧凑，不改变意思，也不动已有的引用。", "Tighten “{section}” without changing its meaning or its citations."),
                tf!("对照文献库，看看「{section}」还缺哪些重要的工作，补上一段。", "Check “{section}” against the library for important work it leaves out, and add a paragraph."),
            ]
        } else {
            vec![
                tf!("看看文献库里有什么，为这份稿件拟一个大纲，写进左边的大纲里。", "Look through the library and draft an outline for this manuscript; put it into the outline at the left."),
                tf!("写「{section}」的初稿：每个论断都要有文献库里的出处。", "Write a first draft of “{section}”: every claim needs a source in the library."),
                tf!("我来说要点，你来成文。先问我这篇稿件要讲什么。", "I give the points, you write. Start by asking me what this manuscript is about."),
            ]
        };
        ui::empty_state("sparkle", t("让助手来写"), t("交代要写什么、改什么，助手会查文献库、读全文，直接写进右边的稿件，并说明依据。每次动笔前，那一节的文字都会留一个版本，随时可以换回去；稿件你也可以随时亲手改。"))
            .id("agent-start")
            .w_full()
            .min_h_0()
            .overflow_y_scroll()
            .justify_start()
            .children(asks.into_iter().enumerate().map(|(n, text)| ui::secondary_button(("agent-suggest", n), "").w_full().max_w(px(520.)).h_auto().min_h(px(30.)).py(px(6.)).child(div().flex_1().min_w_0().child(text.clone())).on_click(self.on(Cmd::Suggest(Talk::Write, text), cx))))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that would bring in gpui's own `test` attribute.
    use super::Versions;
    use wisp_core::manuscript::Snapshot;

    #[test]
    fn the_version_on_show() {
        let kept: Vec<Snapshot> = (1..=2).map(|id| Snapshot { id, body: format!("v{id}"), ..Default::default() }).collect();
        let mut versions = Versions { kept, at: None };
        assert!(versions.shown().is_none());
        versions.at = Some(1);
        assert_eq!(versions.shown().map(|v| v.body.as_str()), Some("v2"));
        versions.at = Some(5);
        assert!(versions.shown().is_none());
    }
}
