//! 写作's search: the shortcut `search` (⌘F / Ctrl+F) on the writing page
//! opens a bar over the manuscript that looks for words in every section of
//! it, instead of in the papers. What is found in the section on show is lit
//! as it is typed, and the first place after the caret selected; Enter and
//! the arrows go from one place to the next, into another section when that
//! is where the next one is. Esc puts the bar away with the caret on the
//! place gone to.

use crate::app::{Cmd, Workspace};
use crate::i18n::t;
use crate::input::{InputEvent, TextInput};
use crate::theme::{Theme, text_px};
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use std::ops::Range;
use wisp_core::manuscript;

/// Frames to wait for the editor to draw a section gone to before giving up on bringing the place into view.
const REVEAL_FRAMES: u8 = 8;

pub struct Seek {
    pub open: bool,
    pub input: Entity<TextInput>,
    /// The place gone to last: its section, and its bytes there.
    at: Option<(String, Range<usize>)>,
    /// A byte of the section on show to bring into view once the editor has drawn it, and the frames left to wait.
    reveal: Option<(usize, u8)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Do {
    /// The next place (true) or the one before.
    Step(bool),
    Close,
}

impl Seek {
    pub fn new(subscriptions: &mut Vec<Subscription>, cx: &mut Context<Workspace>) -> Seek {
        let input = cx.new(|cx| TextInput::new("在稿件里查找", cx));
        // Esc goes on to the workspace, which puts the bar away (`Workspace::dismiss`).
        subscriptions.push(cx.subscribe(&input, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Submit => this.seek_step(true, cx),
            InputEvent::Changed => this.seek_typed(cx),
            _ => {}
        }));
        Seek { open: false, input, at: None, reveal: None }
    }
}

impl Workspace {
    pub(crate) fn run_seek(&mut self, what: Do, window: &mut Window, cx: &mut Context<Self>) {
        match what {
            Do::Step(forward) => {
                self.seek_step(forward, cx);
                if let Some(lib) = self.lib.as_ref() { lib.writing.seek.input.read(cx).focus(window) }
            }
            Do::Close => self.seek_close(window, cx),
        }
    }

    /// The bar over the manuscript, with the words selected: what is selected in the manuscript, if a short piece of one line, else what was looked for last.
    pub(crate) fn seek_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let writing = &mut lib.writing;
        let picked = writing.editor.read(cx).selected().to_string();
        writing.seek.open = true;
        let input = writing.seek.input.clone();
        input.update(cx, |input, cx| {
            if !picked.trim().is_empty() && !picked.contains('\n') && picked.chars().count() <= 80 { input.set_text(&picked, cx) }
            let end = input.text().len();
            input.select_range(0..end, cx);
        });
        input.read(cx).focus(window);
        self.seek_typed(cx);
    }

    fn seek_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let seek = &mut lib.writing.seek;
        if !seek.open { return }
        (seek.open, seek.at) = (false, None);
        // Back to writing, where the place gone to is selected.
        if !lib.writing.reading { lib.writing.editor.read(cx).focus(window) }
        cx.notify();
    }

    /// Every place the words stand in the manuscript on show, section by section in its order; the section being written as the editor has it.
    fn seek_hits(&self, cx: &App) -> Vec<(String, Range<usize>)> {
        let Some(lib) = self.lib.as_ref() else { return vec![] };
        let writing = &lib.writing;
        let Some(manuscript) = writing.current() else { return vec![] };
        let query = writing.seek.input.read(cx).text();
        let current = writing.editor.read(cx).text();
        manuscript
            .sections
            .iter()
            .flat_map(|section| {
                let text = if section.id == writing.section_id { current } else { section.body.as_str() };
                manuscript::occurrences(text, query).into_iter().map(|range| (section.id.clone(), range)).collect::<Vec<_>>()
            })
            .collect()
    }

    /// The words changed: the first place from the selection on in the section on show is selected, without leaving the section.
    fn seek_typed(&mut self, cx: &mut Context<Self>) {
        let hits = self.seek_hits(cx);
        let Some(lib) = self.lib.as_mut() else { return };
        let writing = &mut lib.writing;
        writing.seek.at = None;
        let from = writing.editor.read(cx).selection().start;
        let here = hits.into_iter().find(|(section, range)| *section == writing.section_id && range.start >= from);
        if let Some((section, range)) = here.filter(|_| !writing.reading) {
            writing.editor.update(cx, |input, cx| input.select_range(range.clone(), cx));
            writing.seek.reveal = Some((range.start, REVEAL_FRAMES));
            writing.seek.at = Some((section, range));
        }
        cx.notify();
    }

    /// Go to the next place found, or the one before, from the place gone to last or else from the caret; past the end, around to the beginning.
    fn seek_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let hits = self.seek_hits(cx);
        let Some(lib) = self.lib.as_ref() else { return };
        let writing = &lib.writing;
        if hits.is_empty() { return }
        let order = |section: &str| writing.current().and_then(|m| m.sections.iter().position(|s| s.id == section)).unwrap_or(0);
        let wanted = match writing.seek.at.as_ref().and_then(|at| hits.iter().position(|hit| hit == at)) {
            Some(n) if forward => (n + 1) % hits.len(),
            Some(n) => (n + hits.len() - 1) % hits.len(),
            None => {
                // The first place at or after the caret, in the manuscript's order.
                let caret = (order(&writing.section_id), writing.editor.read(cx).selection().start);
                let after = hits.iter().position(|(section, range)| (order(section), range.start) >= caret).unwrap_or(hits.len());
                if forward { after % hits.len() } else { (after + hits.len() - 1) % hits.len() }
            }
        };
        let (section, range) = hits[wanted].clone();
        self.seek_go(section, range, cx);
    }

    /// Show the section a place is in, out of 预览, and select the place.
    fn seek_go(&mut self, section: String, range: Range<usize>, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        lib.writing.reading = false;
        if lib.writing.section_id != section {
            let manuscript = lib.writing.manuscript.clone();
            self.write_show(manuscript, section.clone(), cx);
        }
        let Some(lib) = self.lib.as_mut() else { return };
        let writing = &mut lib.writing;
        writing.editor.update(cx, |input, cx| input.select_range(range.clone(), cx));
        writing.seek.reveal = Some((range.start, REVEAL_FRAMES));
        writing.seek.at = Some((section, range));
        cx.notify();
    }

    /// Before the manuscript is drawn: the place gone to into view, once the editor has drawn the text it is in.
    pub(crate) fn seek_reveal(&mut self, window: &mut Window, cx: &App) {
        let Some(lib) = self.lib.as_mut() else { return };
        let writing = &mut lib.writing;
        let Some((at, frames)) = writing.seek.reveal else { return };
        let Some(top) = writing.editor.read(cx).top_of(at) else {
            writing.seek.reveal = frames.checked_sub(1).map(|left| (at, left));
            return window.request_animation_frame();
        };
        writing.seek.reveal = None;
        let (seen, height) = (-writing.editor_scroll.offset().y, writing.editor_scroll.bounds().size.height);
        // A few lines above it stay in view, so it is read in its place.
        if top < seen + px(20.) || top + px(60.) > seen + height { writing.editor_scroll.set_offset(point(px(0.), -(top - px(90.)).max(px(0.)))) }
    }

    /// The places found in the section on show, to be lit in the editor: the one gone to stronger.
    pub(crate) fn seek_marks(&self, text: &str, cx: &App) -> Vec<(Range<usize>, Hsla)> {
        let Some(lib) = self.lib.as_ref() else { return vec![] };
        let seek = &lib.writing.seek;
        if !seek.open { return vec![] }
        let current = seek.at.as_ref().filter(|(section, _)| *section == lib.writing.section_id).map(|(_, range)| range.clone());
        manuscript::occurrences(text, seek.input.read(cx).text()).into_iter().map(|range| { let tint = if Some(&range) == current.as_ref() { Theme::found_current() } else { Theme::found() }; (range, tint) }).collect()
    }

    /// The bar over the manuscript while it is open: the words, how many places there are and which is gone to, the arrows, and ×.
    pub(crate) fn render_seek(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let lib = self.lib.as_ref()?;
        let seek = &lib.writing.seek;
        if !seek.open { return None }
        let typed = !seek.input.read(cx).text().trim().is_empty();
        let hits = self.seek_hits(cx);
        let gone = seek.at.as_ref().and_then(|at| hits.iter().position(|hit| hit == at));
        let here = hits.iter().filter(|(section, _)| *section == lib.writing.section_id).count();
        let (count, color) = match (typed, hits.len(), gone) {
            (false, ..) => (String::new(), Theme::text_tertiary()),
            (true, 0, _) => (t("没有找到").to_string(), Theme::danger()),
            (true, n, Some(at)) => (format!("{}/{n}", at + 1), Theme::text_secondary()),
            (true, n, None) if here == 0 => (tf!("{n} 处，在其他节", "{n}, in other sections"), Theme::text_secondary()),
            (true, n, None) => (tf!("{n} 处", "{n} found"), Theme::text_secondary()),
        };
        let able = !hits.is_empty();
        let arrow = |id: &'static str, icon: &str, tip: &'static str, forward: bool, cx: &mut Context<Self>| ui::icon_action(id, icon, Theme::text_secondary(), t(tip)).when(!able, |d| d.opacity(0.4).cursor_default()).when(able, |d| d.on_click(self.on(Cmd::Seek(Do::Step(forward)), cx)));
        Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(4.))
                .mx(px(16.))
                .mt(px(10.))
                .child(div().flex_1().min_w_0().child(ui::field(&seek.input, window, cx).py(px(4.))))
                .child(div().flex_none().min_w(px(36.)).px(px(4.)).text_size(text_px(12.)).text_color(color).child(count))
                .child(arrow("seek-previous", "chevron-up", "上一处", false, cx))
                .child(arrow("seek-next", "chevron-down", "下一处（Enter）", true, cx))
                .child(ui::icon_action("seek-close", "x", Theme::text_tertiary(), t("关闭（Esc）")).on_click(self.on(Cmd::Seek(Do::Close), cx))),
        )
    }
}
