//! 逐篇筛选: the queue looked at one paper at a time. The paper has the
//! page: what it is, what the assistant made of it, its abstract, and the
//! four decisions under it, each on a key. The round's papers are fixed
//! when it starts (`wisp_core::screening::Round`), listed in a rail at the
//! left with what became of each, so the person can go back and decide
//! otherwise. A decision brings the next paper up; past the last one the
//! round is summed up.

use crate::i18n::{label, t};
use crate::app::{Cmd, Library, Page, ScreenMode, Tab, Translated, Workspace};
use crate::context::Nav;
use crate::settings::SettingsPage;
use crate::theme::{self, Theme, text_px};
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use std::collections::HashMap;
use std::ops::Range;
use wisp_core::ai;
use wisp_core::model::{FULLTEXTS, Paper, SCREENINGS};
use wisp_core::screening::{self, Before, Confirming, DECISIONS, Round};
use wisp_core::settings::Task;
use wisp_core::store::translation_key;

/// Below this width of the page the rail is left out: the paper needs the room.
const RAIL_MIN: f32 = 700.;
const RAIL_W: f32 = 236.;
const RAIL_ROW: f32 = 30.;

/// A paper's place in screening as a dot: filled with its status once
/// decided, a ring in the colour of what the assistant suggests while it waits.
pub(crate) fn status_dot(status: &str, suggested: Option<&str>) -> Div {
    let dot = div().flex_none().size(px(8.)).rounded_full();
    if status != "pending" { return dot.bg(theme::status_colors(status).0) }
    dot.border_1().border_color(suggested.map_or(Theme::line_strong(), |verdict| theme::status_colors(verdict).0))
}

/// A key and what it does, for the line of hints under the decisions.
fn hint(key: &'static str, what: &'static str) -> Div {
    div().flex().flex_none().items_center().gap(px(5.)).child(ui::keycap(key)).child(what)
}

impl Library {
    /// The paper a round has on show, while 逐篇 is the page.
    pub fn round_paper(&self) -> Option<&Paper> {
        if self.page != Page::Screen || self.screen.mode != ScreenMode::Single { return None }
        self.screen.round.current().and_then(|id| self.paper(id))
    }
}

impl Workspace {
    /// The commands of 筛选 that are the person's: the page's three faces, a round's place, a decision.
    pub(crate) fn run_screen(&mut self, cmd: Cmd, window: &mut Window, cx: &mut Context<Self>) {
        match cmd {
            Cmd::ScreenMode(mode) => {
                let Some(lib) = self.lib.as_mut() else { return };
                if mode == ScreenMode::Confirm && lib.screen.running() { return self.say(t("助手还在判断，等这一轮结束再确认。"), cx) }
                (lib.page, lib.reading) = (Page::Screen, false);
                // The paper beside the page stays beside it, on the paper the round opens with.
                let beside = lib.selected.is_some();
                match mode {
                    // A round over the part of the queue on show, opening on the paper that was open in it.
                    ScreenMode::Single => {
                        let ids: Vec<String> = lib.screen_view().0.iter().map(|p| p.id.clone()).collect();
                        lib.screen.round = Round::new(ids, lib.selected.as_deref());
                    }
                    // The suggestions as they are now, opening on the verdict that was on show.
                    ScreenMode::Confirm => {
                        let part = if SCREENINGS.iter().any(|(key, _)| *key == lib.screen.filter) { lib.screen.filter.clone() } else { String::new() };
                        lib.screen.confirm = Confirming::default();
                        lib.screen.confirm.part = part;
                        lib.screen.confirmed = None;
                        lib.screen.confirm_scroll = UniformListScrollHandle::new();
                    }
                    ScreenMode::Batch => {}
                }
                lib.screen.mode = mode;
                match mode {
                    ScreenMode::Single if beside => self.round_moved(window, cx),
                    ScreenMode::Single => {
                        self.round_moved(window, cx);
                        window.focus(&self.focus);
                    }
                    // The list of suggestions has the page to itself.
                    ScreenMode::Confirm => {
                        self.select(None, window, cx);
                        window.focus(&self.focus);
                    }
                    ScreenMode::Batch => {}
                }
            }
            Cmd::ScreenGo(at) => {
                let Some(lib) = self.lib.as_mut() else { return };
                lib.screen.round.go(at);
                self.round_moved(window, cx);
            }
            Cmd::ScreenDecide(id, status) => {
                let Some(lib) = self.lib.as_ref() else { return };
                let Some(paper) = lib.paper(&id) else { return };
                let before = Before::of(paper);
                let reason = screening::reason_for(paper, lib.current_suggestion(&id), &status);
                let single = lib.screen.mode == ScreenMode::Single;
                // In the queue, the paper after this one comes up next; the one before when it was the last.
                let queue: Vec<&str> = lib.screen_view().0.iter().map(|p| p.id.as_str()).collect();
                let next = queue.iter().position(|p| *p == id).and_then(|at| queue.get(at + 1).or(at.checked_sub(1).and_then(|before| queue.get(before)))).map(|p| p.to_string());
                if !self.edit_paper(&id, cx, |p| p.decide(&status, &reason)) { return }
                if single {
                    if let Some(lib) = self.lib.as_mut() { lib.screen.round.decided(before) }
                    self.round_moved(window, cx);
                } else {
                    self.select(next, window, cx);
                }
            }
            Cmd::ScreenUndo => {
                let Some(before) = self.lib.as_mut().and_then(|lib| lib.screen.round.undo()) else { return };
                if self.edit_paper(&before.paper_id, cx, |p| before.restore(p)) { self.say(tf!("已撤销：回到「{}」。", "Undone: back to “{}”.", label(&SCREENINGS, &before.screening)), cx) }
                self.round_moved(window, cx);
            }
            Cmd::ScreenAskOne(id) => {
                if self.lib.as_ref().is_some_and(|lib| lib.screen.running()) { return self.say(t("助手正在判断别的文献，等这一轮结束。"), cx) }
                self.screen_start(false, Some(vec![id]), cx);
            }
            Cmd::ScreenBeside => {
                let Some(lib) = self.lib.as_ref() else { return };
                let id = match lib.screen.mode {
                    ScreenMode::Single => lib.screen.round.current().map(str::to_string),
                    ScreenMode::Confirm => lib.screen.confirm.cursor.clone(),
                    ScreenMode::Batch => lib.selected.clone(),
                };
                let Some(id) = id else { return };
                // Asked for a second time, it is put away again.
                if lib.selected.as_deref() == Some(id.as_str()) && lib.screen.mode != ScreenMode::Batch { return self.select(None, window, cx) }
                let tab = if lib.pdf_file(&id).is_some() { Tab::Pdf } else { Tab::Info };
                self.run(Cmd::OpenPaper(id, tab), window, cx);
            }
            _ => {}
        }
        cx.notify();
    }

    /// The round shows another paper: from its top, in view in the rail, and beside the page when a paper was open there.
    fn round_moved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        lib.screen.paper_scroll = ScrollHandle::new();
        lib.screen.rail_scroll.scroll_to_item(lib.screen.round.at.min(lib.screen.round.ids.len().saturating_sub(1)), ScrollStrategy::Center);
        if lib.selected.is_some() {
            let current = lib.screen.round.current().map(str::to_string);
            self.select(current, window, cx);
        }
        self.translate_selected(false, cx);
        cx.notify();
    }

    /// A key of 筛选 (`shortcuts.rs`): a decision, the suggestion taken, a
    /// star, the translation, the assistant asked, the last decision taken
    /// back. It is about the round's paper, or the one open in the queue.
    pub fn screen_key(&mut self, key: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal.is_some() || self.menu.is_some() || self.settings_view.is_some() { return }
        // 提取 hears the same keys for its own rows.
        if self.lib.as_ref().is_some_and(|lib| lib.page == Page::Extract) { return self.extract_key(key, window, cx) }
        let Some(lib) = self.lib.as_ref().filter(|lib| lib.page == Page::Screen) else { return };
        if lib.screen.mode == ScreenMode::Confirm { return self.confirm_key(key, window, cx) }
        let single = lib.screen.mode == ScreenMode::Single;
        if single && key == "z" { return self.run(Cmd::ScreenUndo, window, cx) }
        let paper = if single { lib.round_paper() } else { lib.selected_paper().filter(|paper| lib.screen_view().0.iter().any(|queued| queued.id == paper.id)) };
        let Some(id) = paper.map(|paper| paper.id.clone()) else { return };
        let cmd = match key {
            "a" => match lib.current_suggestion(&id) {
                Some(suggestion) => Cmd::ScreenDecide(id, suggestion.verdict.clone()),
                None => return self.say(t("这一篇还没有按当前目标的建议。"), cx),
            },
            "s" => Cmd::ToggleStar(id),
            "t" => Cmd::ToggleTranslation,
            "g" => Cmd::ScreenAskOne(id),
            key => match screening::status_of(key) {
                Some(status) => Cmd::ScreenDecide(id, status.to_string()),
                None => return,
            },
        };
        self.run(cmd, window, cx);
    }

    /// The arrow keys and Enter on a face of 筛选 that is not the queue.
    pub(crate) fn screen_nav(&mut self, nav: Nav, window: &mut Window, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_ref() else { return };
        if nav == Nav::Open { return self.run(Cmd::ScreenBeside, window, cx) }
        if lib.screen.mode == ScreenMode::Confirm { return self.confirm_nav(nav, window, cx) }
        let (at, count) = (lib.screen.round.at, lib.screen.round.ids.len());
        let to = match nav {
            Nav::Up | Nav::Left | Nav::PageUp => at.saturating_sub(1),
            Nav::Down | Nav::Right | Nav::PageDown => (at + 1).min(count),
            Nav::First => 0,
            Nav::Last => count.saturating_sub(1),
            _ => return,
        };
        if to != at { self.run(Cmd::ScreenGo(to), window, cx) }
    }

    pub fn render_screen_single(&mut self, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let lib = self.lib.as_ref().unwrap();
        let round = &lib.screen.round;
        let (at, total) = (round.at, round.ids.len());
        let by_id: HashMap<&str, &Paper> = lib.papers.iter().map(|p| (p.id.as_str(), p)).collect();
        let counts = screening::tally(round.ids.iter().filter_map(|id| by_id.get(id.as_str())).map(|p| p.screening.as_str()));
        let decided = total - counts[0];
        let place = if round.ended() { tf!("共 {total} 篇 · 已判断 {decided}", "{total} papers · {decided} screened") } else { tf!("第 {} / {total} 篇 · 已判断 {decided}", "Paper {} / {total} · {decided} screened", at + 1) };
        // The round so far, as one bar: a stretch per decision, the rest still waiting.
        let segments: Vec<ui::ProgressSegment> = SCREENINGS.iter().enumerate().skip(1).map(|(n, (key, name))| (counts[n], t(name), theme::status_colors(key).0)).collect();
        let legend = segments.iter().filter(|(count, _, _)| *count > 0).map(|(count, name, color)| div().flex().flex_none().items_center().gap(px(5.)).child(div().flex_none().size(px(7.)).rounded_full().bg(*color)).child(format!("{name} {count}")));
        let head = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(10.))
            .px(px(16.))
            .pt(px(16.))
            .pb(px(12.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(px(12.))
                    .child(div().flex_none().text_size(text_px(16.)).font_weight(FontWeight::SEMIBOLD).child(t("筛选")))
                    .child(self.render_screen_modes(cx))
                    .child(div().flex_1())
                    .child(div().flex_none().text_size(text_px(12.)).text_color(Theme::text_secondary()).child(place))
                    .child(div().flex().flex_none().child(ui::ghost_button("round-prev", Some("chevron-up"), None).when(at == 0, |d| d.opacity(0.4)).on_click(self.on(Cmd::ScreenGo(at.saturating_sub(1)), cx))).child(ui::ghost_button("round-next", Some("chevron-down"), None).when(at >= total, |d| d.opacity(0.4)).on_click(self.on(Cmd::ScreenGo((at + 1).min(total)), cx)))),
            )
            .child(ui::progress_bar(total, &segments).h(px(5.)))
            .child(div().flex().flex_wrap().items_center().gap_x(px(14.)).gap_y(px(2.)).min_h(px(16.)).text_size(text_px(11.)).line_height(text_px(16.)).text_color(Theme::text_tertiary()).children(legend).when(counts[0] > 0, |d| d.child(tf!("待判断 {}", "{} pending", counts[0]))));

        let rail = (width >= RAIL_MIN && total > 0).then(|| {
            div()
                .flex()
                .flex_col()
                .flex_none()
                .w(px(RAIL_W))
                .h_full()
                .py(px(6.))
                .border_r_1()
                .border_color(Theme::line())
                .child(uniform_list("screen-rail", total, cx.processor(|this, range: Range<usize>, _, cx| this.render_rail_rows(range, cx))).track_scroll(lib.screen.rail_scroll.clone()).flex_1())
        });
        let body = match lib.round_paper() {
            Some(paper) => self.render_round_paper(paper, cx),
            None => self.render_round_end(&counts, total, cx),
        };
        div().flex().flex_col().size_full().child(head).child(div().flex().flex_1().min_h_0().border_t_1().border_color(Theme::line()).children(rail).child(body)).into_any_element()
    }

    /// The round's papers, each with what became of it. A click shows the paper.
    fn render_rail_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(lib) = self.lib.as_ref() else { return vec![] };
        let round = &lib.screen.round;
        range
            .map(|i| {
                let paper = round.ids.get(i).and_then(|id| lib.paper(id));
                let current = i == round.at;
                let (status, title) = paper.map_or(("pending", String::new()), |p| (p.screening.as_str(), p.title.clone()));
                let suggested = paper.and_then(|p| lib.current_suggestion(&p.id)).map(|s| s.verdict.as_str());
                let row = div()
                    .id(("rail", i))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .w_full()
                    .h_full()
                    .px(px(8.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_size(text_px(12.))
                    .when(current, |d| d.bg(Theme::accent_soft()).font_weight(FontWeight::MEDIUM))
                    // A decided paper steps back: what is left to do stands out.
                    .when(!current, |d| d.hover(|s| s.bg(Theme::hover())).when(status != "pending", |d| d.text_color(Theme::text_tertiary())))
                    .on_click(self.on(Cmd::ScreenGo(i), cx))
                    .child(status_dot(status, suggested))
                    .child(div().flex_none().min_w(px(16.)).text_size(text_px(11.)).text_color(Theme::text_tertiary()).child((i + 1).to_string()))
                    .child(div().flex().flex_col().flex_1().min_w_0().child(div().truncate().child(title)));
                // A row of the list is as wide as the rail, whatever its title's length.
                div().w_full().h(text_px(RAIL_ROW)).px(px(6.)).child(row).into_any_element()
            })
            .collect()
    }

    /// The paper on show: what it is, the assistant's suggestion, the
    /// abstract; under them, always in the same place, the decisions.
    fn render_round_paper(&self, paper: &Paper, cx: &mut Context<Self>) -> AnyElement {
        let lib = self.lib.as_ref().unwrap();
        let has_pdf = lib.pdf_file(&paper.id).is_some();
        let beside = lib.selected.as_deref() == Some(paper.id.as_str());
        let mut about: Vec<String> = vec![paper.journal.clone(), paper.year.map(|y| y.to_string()).unwrap_or_default()];
        about.retain(|part| !part.is_empty());
        let prompt = ai::translation_prompt(&self.settings.translation_prompt);
        let translated = |text: &str| lib.show_translation.then(|| lib.translations.get(&translation_key(text, prompt))).flatten();
        let star = if paper.starred { Theme::star() } else { Theme::text_tertiary() };
        let tools = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.))
            .child(div().id("round-star").flex().flex_none().items_center().justify_center().size(px(28.)).rounded(px(6.)).cursor_pointer().hover(|s| s.bg(Theme::hover())).on_click(self.on(Cmd::ToggleStar(paper.id.clone()), cx)).child(ui::icon("star", 16., star)))
            .child(ui::ghost_button("round-beside", Some("file-text"), Some(if beside { t("收起").into() } else if has_pdf { t("对照 PDF").into() } else { t("完整记录").into() })).on_click(self.on(Cmd::ScreenBeside, cx)));
        let marks = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.))
            .child(ui::status_chip(&paper.screening, label(&SCREENINGS, &paper.screening)))
            .child(ui::status_chip(&paper.fulltext, label(&FULLTEXTS, &paper.fulltext)))
            .when(lib.screen_group(&paper.id) == "differs", |d| d.child(ui::chip(t("助手按当前目标看法不同"), Theme::warning(), theme::status_colors("uncertain").1)))
            .child(div().flex_1())
            .child(tools);
        let title = div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(self.selectable(format!("round-title-{}", paper.id), paper.title.clone(), cx).text_size(text_px(20.)).line_height(text_px(28.)).font_weight(FontWeight::SEMIBOLD))
            // The translated title under its source: the source is what a search or a citation needs.
            .children(match translated(&paper.title) {
                Some(Translated::Done(text)) => Some(div().text_size(text_px(15.)).line_height(text_px(23.)).text_color(Theme::text_secondary()).child(text.clone())),
                _ => None,
            })
            .when(!paper.authors.is_empty(), |d| d.child(div().pt(px(2.)).line_clamp(2).text_color(Theme::text_secondary()).child(paper.authors.clone())))
            .when(!about.is_empty(), |d| d.child(div().text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(about.join(" · "))));

        let state = translated(&paper.abstract_text);
        let abstract_text = match state {
            Some(Translated::Done(text)) => text.clone(),
            _ => paper.abstract_text.clone(),
        };
        let abstract_head = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(ui::caption(t("摘要")))
            .child(div().flex_1())
            .children(matches!(state, Some(Translated::Busy)).then(|| div().flex().items_center().gap(px(5.)).text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(ui::spinner("round-translating", 12., Theme::accent())).child(t("正在翻译…"))))
            .children(match state {
                Some(Translated::Failed(_)) => Some(ui::ghost_button("round-translate-retry", None, Some(t("翻译没有成功，重试").into())).h(text_px(22.)).text_size(text_px(12.)).text_color(Theme::danger()).on_click(self.on(Cmd::RetryTranslation, cx))),
                _ => None,
            })
            .child(ui::ghost_button("round-translate", None, Some(if lib.show_translation { t("显示原文").into() } else { t("显示译文").into() })).h(text_px(22.)).text_size(text_px(12.)).when(lib.show_translation, |d| d.text_color(Theme::accent())).on_click(self.on(Cmd::ToggleTranslation, cx)));
        let abstract_body = if abstract_text.trim().is_empty() {
            div().text_color(Theme::text_placeholder()).child(if has_pdf { t("这篇文献没有摘要。按 Enter 对照它的 PDF。") } else { t("这篇文献没有摘要，也没有保存全文。") })
        } else {
            self.selectable(format!("round-abstract-{}", paper.id), abstract_text, cx).text_size(text_px(14.)).line_height(text_px(23.))
        };
        let reason = (paper.screening != "pending" && !paper.screening_reason.is_empty()).then(|| div().flex().flex_col().gap(px(4.)).child(ui::caption(t("判断理由"))).child(div().text_color(Theme::text_secondary()).child(paper.screening_reason.clone())));

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                div().id("round-paper").flex_1().min_h_0().overflow_y_scroll().track_scroll(&lib.screen.paper_scroll).child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(16.))
                        .max_w(px(820.))
                        .px(px(28.))
                        .pt(px(18.))
                        .pb(px(28.))
                        .child(marks)
                        .child(title)
                        .child(self.render_advice(paper, true, cx))
                        .child(div().flex().flex_col().gap(px(6.)).child(abstract_head).child(abstract_body))
                        .children(reason),
                ),
            )
            .child(self.render_decisions(paper, cx))
            .into_any_element()
    }

    /// The four decisions, each with its key; the one the assistant
    /// suggests and the one the paper has are marked. Under them, the other keys.
    fn render_decisions(&self, paper: &Paper, cx: &mut Context<Self>) -> Div {
        let lib = self.lib.as_ref().unwrap();
        let suggested = lib.current_suggestion(&paper.id).map(|s| s.verdict.as_str());
        let buttons: Vec<Stateful<Div>> = DECISIONS.iter().enumerate().map(|(n, (status, _, _))| {
            let (color, fill) = theme::status_colors(status);
            let (mine, advised) = (paper.screening == *status, suggested == Some(*status));
            div()
                .id(("round-decide", n))
                .flex()
                .flex_none()
                .items_center()
                .gap(px(8.))
                .h(text_px(36.))
                .pl(px(8.))
                .pr(px(12.))
                .rounded(px(8.))
                .border_1()
                .cursor_pointer()
                .when(mine, |d| d.bg(fill).border_color(color.opacity(0.5)).text_color(color).font_weight(FontWeight::MEDIUM))
                .when(!mine && advised, |d| d.bg(Theme::accent_soft()).border_color(Theme::accent().opacity(0.5)))
                .when(!mine && !advised, |d| d.bg(Theme::bg_body()).border_color(Theme::line_strong()).hover(|s| s.bg(Theme::bg_sunken())))
                .active(|s| s.bg(Theme::pressed()))
                .on_click(self.on(Cmd::ScreenDecide(paper.id.clone(), status.to_string()), cx))
                .child(ui::keycap(screening::key_of(status)))
                .child(label(&SCREENINGS, status))
                .when(mine, |d| d.child(ui::icon("check", 13., color)))
                .when(advised && !mine, |d| d.child(div().text_size(text_px(11.)).text_color(Theme::accent()).child(t("建议"))))
        }).collect();
        let undo = lib.screen.round.can_undo().then(|| ui::ghost_button("round-undo", None, Some(t("撤销上一个判断").into())).h(text_px(26.)).text_size(text_px(12.)).on_click(self.on(Cmd::ScreenUndo, cx)));
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(10.))
            .px(px(28.))
            .pt(px(12.))
            .pb(px(12.))
            .border_t_1()
            .border_color(Theme::line())
            .bg(Theme::bg_body())
            .child(div().flex().flex_wrap().items_center().gap(px(8.)).children(buttons).child(div().flex_1()).children(undo))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_x(px(14.))
                    .gap_y(px(6.))
                    .text_size(text_px(11.))
                    .line_height(text_px(18.))
                    .text_color(Theme::text_tertiary())
                    .child(hint("↑↓", t("上一篇 / 下一篇")))
                    .when(suggested.is_some(), |d| d.child(hint("A", t("采纳建议"))))
                    .child(hint("P", t("留待判断")))
                    .child(hint("Z", t("撤销")))
                    .child(hint("S", t("收藏")))
                    .child(hint("T", t("译文")))
                    .child(hint("Enter", t("对照 PDF")))
                    .child(hint("Esc", t("回到队列"))),
            )
    }

    /// Past the last paper: what the round came to, and where to go from here.
    fn render_round_end(&self, counts: &[usize; 5], total: usize, cx: &mut Context<Self>) -> AnyElement {
        let lib = self.lib.as_ref().unwrap();
        let round = &lib.screen.round;
        let waiting = lib.screen_queue().len();
        if total == 0 {
            return div().flex().flex_col().flex_1().items_center().justify_center().child(ui::empty_state("check", t("没有待判断的文献"), t("新导入的文献会出现在队列里，到时再逐篇看。")).flex_none()).into_any_element();
        }
        let first_pending = round.first(|id| lib.paper(id).is_some_and(|p| p.screening == "pending"));
        let chips = SCREENINGS.iter().enumerate().skip(1).filter(|(n, _)| counts[*n] > 0).map(|(n, (key, name))| ui::status_chip(key, &format!("{name} {}", counts[n])));
        let message = match counts[0] {
            0 => tf!("这一轮的 {total} 篇都判断过了。", "All {total} papers in this round have been screened."),
            left => tf!("这一轮 {total} 篇，还有 {left} 篇留在待判断。", "Of {total} papers in this round, {left} remain pending."),
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .items_center()
            .justify_center()
            .gap(px(14.))
            .p(px(32.))
            .child(div().flex().items_center().justify_center().size(px(44.)).rounded_full().bg(theme::status_colors("included").1).child(ui::icon("check", 22., theme::status_colors("included").0)))
            .child(div().text_size(text_px(16.)).font_weight(FontWeight::SEMIBOLD).child(t("这一轮看完了")))
            .child(div().text_color(Theme::text_secondary()).child(message))
            .child(div().flex().flex_wrap().justify_center().gap(px(6.)).children(chips).when(counts[0] > 0, |d| d.child(ui::status_chip("pending", &tf!("待判断 {}", "{} pending", counts[0])))))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_center()
                    .gap(px(8.))
                    .pt(px(6.))
                    .children(first_pending.map(|at| ui::primary_button("round-pending", t("回到没判断的第一篇")).on_click(self.on(Cmd::ScreenGo(at), cx))))
                    // What the queue holds now: the papers this round left waiting, and those that came since.
                    .when(first_pending.is_none() && waiting > 0, |d| d.child(ui::primary_button("round-again", tf!("再看一轮（队列里还有 {waiting} 篇）", "Another round ({waiting} papers in queue)")).on_click(self.on(Cmd::ScreenMode(ScreenMode::Single), cx))))
                    .child(ui::secondary_button("round-review", t("从头回看")).on_click(self.on(Cmd::ScreenGo(0), cx)))
                    .child(ui::secondary_button("round-leave", t("回到队列")).on_click(self.on(Cmd::ScreenMode(ScreenMode::Batch), cx))),
            )
            .when(round.can_undo(), |d| d.child(ui::ghost_button("round-end-undo", None, Some(t("撤销上一个判断").into())).h(text_px(26.)).text_size(text_px(12.)).on_click(self.on(Cmd::ScreenUndo, cx))))
            .into_any_element()
    }

    /// Whether a model is there to be asked for a suggestion.
    pub(crate) fn screen_configured(&self) -> bool {
        !self.settings.profile_for_or_default(Task::Screen).key().is_empty()
    }

    /// The way to the model settings, where none is set up.
    pub(crate) fn screen_setup_button(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        ui::ghost_button("screen-settings", Some("settings-gear"), Some(t("先配置模型").into())).on_click(self.on(Cmd::OpenSettings(SettingsPage::Ai), cx))
    }
}
