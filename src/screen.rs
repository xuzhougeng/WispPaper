//! 筛选: the papers waiting for a decision. The assistant reads them, several
//! at a time, and suggests; the person decides. The page has three faces
//! (`ScreenMode`): 批量, the queue as a list with the assistant's run over
//! it (this file); 逐篇, one paper at a time (`screen_single.rs`); and 确认,
//! the suggestions looked over and written together (`screen_confirm.rs`).
//! The decisions have keys in all three (`Workspace::screen_key`).

use crate::i18n::{label, t};
use crate::app::{Cmd, MenuKind, Screen, ScreenMode, Workspace};
use crate::theme::{Theme, text_px};
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use std::time::Duration;
use wisp_core::model::{Paper, SCREENINGS};
use wisp_core::screening;
use wisp_core::store::Suggestion;

/// The rows drawn at most: a longer queue is worked off from the top.
const SHOWN: usize = 200;

/// A sheet of paper; while the assistant reads, a line passes over it.
fn reading_sheet(running: bool) -> Div {
    let lines = [1., 1., 1., 0.6].map(|width| div().h(px(2.)).w(relative(width)).rounded_full().bg(Theme::line_strong()));
    let sheet = div().relative().flex().flex_col().flex_none().justify_center().gap(px(6.)).w(px(42.)).h(px(54.)).px(px(8.)).rounded(px(4.)).border_1().border_color(Theme::line_strong()).bg(Theme::bg_body()).children(lines);
    if !running { return sheet }
    let scan = div().absolute().left(px(-4.)).right(px(-4.)).h(px(2.)).rounded_full().bg(Theme::accent());
    sheet.child(scan.with_animation("screen-scan", Animation::new(Duration::from_millis(2400)).repeat().with_easing(pulsating_between(0., 1.)), |line, at| line.top(px(5. + 42. * at))))
}

/// How far a run is, as a bar that fills up to each new answer instead of jumping there.
pub(crate) fn progress_bar(done: usize, total: usize) -> Div {
    let at = |n: usize| if total == 0 { 0. } else { (n as f32 / total as f32).min(1.) };
    let (from, to) = (at(done.saturating_sub(1)), at(done));
    let fill = div().h_full().rounded_full().bg(Theme::accent());
    div().w_full().h(px(6.)).rounded_full().bg(Theme::groove()).overflow_hidden().child(fill.with_animation(("screen-progress", done), Animation::new(Duration::from_millis(350)).with_easing(ease_out_quint()), move |fill, moved| fill.w(relative(from + (to - from) * moved))))
}

impl Workspace {
    /// The two ways of working through the queue, as a switch at the page's top.
    pub(crate) fn render_screen_modes(&self, cx: &mut Context<Self>) -> Div {
        let single = self.lib.as_ref().is_some_and(|lib| lib.screen.mode == ScreenMode::Single);
        crate::settings::segmented("screen-mode", vec![(t("批量"), Cmd::ScreenMode(ScreenMode::Batch)), (t("逐篇"), Cmd::ScreenMode(ScreenMode::Single))], single as usize, self, cx)
    }

    /// What the assistant made of a paper, in full: its verdict, how sure
    /// it was and what it read, its reason, the words it rests on (with a
    /// page, a click opens the PDF there). Without a suggestion, the way to
    /// ask for one; `ask` also offers to ask again.
    pub(crate) fn render_advice(&self, paper: &Paper, ask: bool, cx: &mut Context<Self>) -> Div {
        let lib = self.lib.as_ref().unwrap();
        let id = |part: &str| SharedString::from(format!("advice-{part}-{}", paper.id));
        let group = lib.screen_group(&paper.id);
        let reading = lib.screen.reading.contains(&paper.id);
        let mut again = |text: &'static str| ui::ghost_button(id("ask"), None, Some(text.into())).h(text_px(24.)).text_size(text_px(12.)).child(ui::keycap("G")).on_click(self.on(Cmd::ScreenAskOne(paper.id.clone()), cx));
        let head = div().flex().items_center().gap(px(8.)).min_h(px(24.)).child(ui::icon("sparkle", 14., Theme::accent())).child(div().flex_none().text_size(text_px(12.)).font_weight(FontWeight::MEDIUM).text_color(Theme::text_secondary()).child(t("助手的建议")));
        let card = div().flex().flex_col().gap(px(8.)).px(px(12.)).py(px(10.)).rounded(px(10.)).border_1().border_color(Theme::line()).bg(Theme::bg_sunken());
        if reading { return card.child(head.child(ui::spinner(id("spin"), 13., Theme::accent())).child(div().text_size(text_px(12.)).text_color(Theme::accent()).child(t("正在读这一篇…")))) }
        let Some(s) = lib.suggestions.get(&paper.id) else {
            let head = head.child(div().flex_1().min_w_0().text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(t("还没有判断这一篇。")));
            let head = if !ask { head } else if self.screen_configured() { head.child(again(t("让助手判断"))) } else { head.child(self.screen_setup_button(cx)) };
            return card.child(head).when(ask && !lib.screen.error.is_empty(), |d| d.child(ui::error_line(&lib.screen.error)));
        };
        let read = match s.basis.as_str() {
            "fulltext" => t("读了全文"),
            "abstract" => t("只读了摘要"),
            _ => t("没有可读的内容"),
        };
        let chip = if group == "stale" { ui::chip(tf!("旧目标：{}", "Previous criteria: {}", label(&SCREENINGS, &s.verdict)), Theme::text_tertiary(), Theme::groove()) } else { ui::status_chip(&s.verdict, label(&SCREENINGS, &s.verdict)) };
        let sure = s.confidence.map(|c| tf!("把握 {:.0}% · ", "Confidence {:.0}% · ", c * 100.)).unwrap_or_default();
        let mut card = card.child(head.child(chip).child(div().flex_1().min_w_0().truncate().text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(format!("{sure}{read} · {}", s.author))).when(ask && self.screen_configured(), |d| d.child(again(t("重新判断")))));
        card = card.child(self.selectable(format!("advice-reason-{}", paper.id), s.reason.clone(), cx).text_size(text_px(13.)).line_height(text_px(21.)));
        if !s.excerpt.is_empty() {
            let quote = div().id(id("quote")).flex().gap(px(8.)).px(px(10.)).py(px(8.)).rounded(px(6.)).bg(Theme::bg_body()).border_l_2().border_color(Theme::accent()).text_size(text_px(12.)).line_height(text_px(18.)).text_color(Theme::text_secondary()).child(div().flex_1().min_w_0().child(s.excerpt.clone()));
            card = card.child(match s.page {
                Some(page) => quote.cursor_pointer().child(div().flex_none().text_color(Theme::accent()).child(tf!("第 {page} 页", "Page {page}"))).on_click(self.on(Cmd::GotoSource(paper.id.clone(), Some(page), s.excerpt.clone()), cx)),
                None => quote.child(div().flex_none().text_color(Theme::text_tertiary()).child(t("摘要"))),
            });
        }
        if group == "stale" {
            let old = s.criteria.as_deref().filter(|c| !c.is_empty()).map_or(t("按文献库的主题").to_string(), |c| format!("「{c}」"));
            card = card.child(div().text_size(text_px(12.)).text_color(Theme::warning()).child(tf!("这条建议是按之前的筛选目标 {old} 给出的；让助手按当前目标判断后会被替换。", "This suggestion used the previous criteria {old}. Screening again will replace it with one using the current criteria.")));
        }
        if group == "differs" {
            card = card.child(div().text_size(text_px(12.)).text_color(Theme::warning()).child(tf!("你之前判断为「{}」，按当前的筛选目标，助手的建议不同。", "Your decision was “{}”; the assistant disagrees under the current criteria.", label(&SCREENINGS, &paper.screening))));
        }
        card.when(ask && !lib.screen.error.is_empty(), |d| d.child(ui::error_line(&lib.screen.error)))
    }

    pub fn render_screen(&mut self, width: f32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.lib.as_ref().map_or(ScreenMode::Batch, |lib| lib.screen.mode) {
            ScreenMode::Single => return self.render_screen_single(width, cx),
            ScreenMode::Confirm => return self.render_screen_confirm(width, cx),
            ScreenMode::Batch => {}
        }
        let lib = self.lib.as_ref().unwrap();
        let screen = &lib.screen;
        let all = lib.screen_queue();
        let pending = all.iter().filter(|p| p.screening == "pending").count();
        let suggested = all.iter().filter(|p| p.screening == "pending" && lib.current_suggestion(&p.id).is_some()).count();
        let stale = all.iter().filter(|p| lib.screen_group(&p.id) == "stale").count();
        let differs = all.len() - pending;
        let waiting = lib.screen_unasked(false).len();
        let unchecked = lib.screen_unasked(true).len();
        let (queue, filter) = lib.screen_view();
        // The parts of the queue: one per suggested verdict, the decided papers seen otherwise, the suggestions made for an earlier goal, and the papers without a suggestion.
        let parts: Vec<(&str, &str, usize)> = SCREENINGS.iter().filter(|(key, _)| *key != "pending").map(|(key, name)| (*key, t(name))).chain([("differs", t("与你的判断不同")), ("stale", t("按旧目标的建议")), ("none", t("还没有建议"))]).map(|(key, name)| (key, name, all.iter().filter(|p| lib.screen_group(&p.id) == key).count())).filter(|(_, _, count)| *count > 0).collect();
        let part = |n: usize, key: &str, text: String, this: &Self, cx: &mut Context<Self>| {
            let on = filter == key;
            let (color, fill) = match key {
                "" | "none" | "stale" => (Theme::text_secondary(), Theme::groove()),
                "differs" => crate::theme::status_colors("uncertain"),
                verdict => crate::theme::status_colors(verdict),
            };
            div().id(("screen-part", n)).flex().flex_none().items_center().h(text_px(26.)).px(px(10.)).rounded(px(13.)).text_size(text_px(12.)).cursor_pointer().border_1().when(on, |d| d.bg(fill).text_color(color).border_color(color.opacity(0.35)).font_weight(FontWeight::MEDIUM)).when(!on, |d| d.border_color(Theme::line()).text_color(Theme::text_secondary()).hover(|s| s.bg(Theme::hover()))).on_click(this.on(Cmd::ScreenFilter(key.to_string()), cx)).child(text)
        };
        let mut bar: Vec<AnyElement> = vec![part(0, "", tf!("全部 {}", "All {}", all.len()), self, cx).into_any_element()];
        bar.extend(parts.iter().enumerate().map(|(n, (key, name, count))| part(n + 1, key, format!("{name} {count}"), self, cx).into_any_element()));
        // The suggestions are confirmed together on a page of their own: all of them, or the verdict on show.
        let confirming = if SCREENINGS.iter().any(|(key, _)| *key == filter) { queue.len() } else { suggested };
        let confirm = (confirming > 0 && !screen.running()).then(|| {
            let text = if SCREENINGS.iter().any(|(key, _)| *key == filter) { tf!("确认这 {confirming} 条「{}」", "Confirm {confirming} “{}” suggestions", label(&SCREENINGS, filter)) } else { tf!("确认 {confirming} 条建议", "Confirm {confirming} suggestions") };
            // Once the assistant has nothing left to read, confirming is what comes next.
            if waiting == 0 { ui::primary_button("screen-confirm", text) } else { ui::secondary_button("screen-confirm", text) }.on_click(self.on(Cmd::ScreenMode(ScreenMode::Confirm), cx))
        });
        // A run that is over leads on to confirming what it suggested.
        let next = (confirming > 0 && !screen.running() && screen.total > 0).then(|| div().id("screen-run-confirm").flex().flex_none().items_center().gap(px(2.)).cursor_pointer().text_size(text_px(12.)).text_color(Theme::accent()).hover(|s| s.underline()).on_click(self.on(Cmd::ScreenMode(ScreenMode::Confirm), cx)).child(t("助手读完了，去确认它的建议")).child(ui::icon("chevron-right", 13., Theme::accent())));
        let configured = self.screen_configured();
        let mut status = if pending == 0 { t("没有待判断的文献。").to_string() } else { tf!("待判断 {pending} 篇，其中 {suggested} 篇已有按当前目标的建议。", "{pending} pending papers, {suggested} with suggestions under the current criteria.") };
        if stale > 0 { status.push_str(&tf!("{stale} 篇的建议是按旧目标给出的。", "{stale} suggestions were made under previous criteria.")) }
        if differs > 0 { status.push_str(&tf!("已判断的文献里，{differs} 篇按当前目标看与你的判断不同。", "The assistant disagrees with your decisions on {differs} papers under the current criteria.")) }
        let start = match (waiting, stale) {
            (0, _) => t("让助手判断").to_string(),
            (_, 0) => tf!("让助手判断 {waiting} 篇", "Ask assistant to screen {waiting} papers"),
            _ => tf!("按当前目标判断 {waiting} 篇", "Screen {waiting} under current criteria"),
        };
        let rows: Vec<AnyElement> = queue.iter().take(SHOWN).map(|paper| self.render_screen_row(paper, lib.suggestions.get(&paper.id), lib.screen_group(&paper.id), lib.selected.as_deref() == Some(paper.id.as_str()), screen.reading.contains(&paper.id), cx)).collect();
        let titles: Vec<String> = screen.reading.iter().filter_map(|id| lib.paper(id)).map(|p| p.title.clone()).collect();

        div()
            .id("screen-page")
            .flex()
            .flex_col()
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(px(10.))
                    .px(px(16.))
                    .pt(px(16.))
                    .pb(px(12.))
                    .child(div().flex().flex_wrap().items_center().gap(px(12.)).child(div().flex_none().text_size(text_px(16.)).font_weight(FontWeight::SEMIBOLD).child(t("筛选"))).child(self.render_screen_modes(cx)))
                    .child(div().text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(t("助手读待判断的文献（有全文读全文，否则读摘要），几篇同时进行，给出建议、理由和原文依据；读完后一起确认，或切到「逐篇」一篇篇用按键判断。建议只是建议：收录判断由你确认。")))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(div().flex().items_baseline().gap(px(8.)).child(div().flex_none().text_size(text_px(13.)).font_weight(FontWeight::MEDIUM).child(t("筛选目标"))).child(div().flex_1().min_w_0().truncate().text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(if lib.info.criteria.is_empty() && !lib.info.topic.trim().is_empty() { tf!("留空时按主题判断：{}", "Leave blank to use the topic: {}", lib.info.topic.trim()) } else { t("什么样的文献应当收进这个文献库").to_string() })))
                            .child(ui::field(&screen.criteria, window, cx))
                            .child(div().text_size(text_px(11.)).text_color(Theme::text_tertiary()).child(t("随时可以换一个目标：已有的建议会标为「按旧目标」，再让助手按新目标判断一遍；已判断的文献也可以复核，你的判断不会被改动。"))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .flex_wrap()
                            .gap(px(10.))
                            .child(div().flex_1().min_w(px(200.)).text_size(text_px(12.)).text_color(Theme::text_secondary()).child(status))
                            .when(!configured, |d| d.child(self.screen_setup_button(cx)))
                            // The decided papers looked at again against the goal as it is now; only those seen otherwise come back.
                            .when(!screen.running() && unchecked > 0, |d| d.child(ui::secondary_button("screen-recheck", tf!("复核已判断的 {unchecked} 篇", "Review {unchecked} screened papers")).on_click(self.on(Cmd::ScreenRecheck, cx))))
                            .child(if screen.running() {
                                ui::secondary_button("screen-stop", t("停止")).on_click(self.on(Cmd::ScreenStop, cx))
                            } else if waiting == 0 && confirm.is_some() {
                                // Everything waiting has a suggestion: asking again is no longer the thing to do.
                                ui::secondary_button("screen-start", start).opacity(0.5).on_click(self.on(Cmd::ScreenStart, cx))
                            } else {
                                ui::primary_button("screen-start", start).when(waiting == 0, |d| d.opacity(0.5)).on_click(self.on(Cmd::ScreenStart, cx))
                            })
                            .children(confirm),
                    )
                    .when(screen.total > 0, |d| d.child(self.render_screen_run(screen, &titles, next)))
                    .when(!screen.error.is_empty(), |d| d.child(ui::error_line(&screen.error)))
                    .when(parts.len() > 1 || parts.first().is_some_and(|(key, _, _)| *key != "none"), |d| d.child(div().flex().items_center().flex_wrap().gap(px(6.)).pt(px(2.)).children(bar).child(div().flex_1()).when(!queue.is_empty(), |d| d.child(div().text_size(text_px(11.)).text_color(Theme::text_tertiary()).child(t("点开一篇，按 Y / R / U / N 判断，A 采纳建议")))))),
            )
            .child(if rows.is_empty() {
                ui::empty_state("check", t("都判断过了"), t("新导入的文献会出现在这里。")).into_any_element()
            } else {
                div()
                    .id("screen-queue")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(240.))
                    .overflow_y_scroll()
                    .border_t_1()
                    .border_color(Theme::line())
                    .children(rows)
                    .when(queue.len() > SHOWN, |d| d.child(div().px(px(16.)).py(px(10.)).text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(tf!("还有 {} 篇，判断完上面的会继续出现。", "{} more papers will appear as you screen the ones above.", queue.len() - SHOWN))))
                    .into_any_element()
            })
            .into_any_element()
    }

    /// The run of the assistant, under way or over: how far it got, and the
    /// papers it is reading at this moment.
    fn render_screen_run(&self, screen: &Screen, reading: &[String], next: Option<Stateful<Div>>) -> Div {
        let running = screen.running();
        let heading = if running { t("助手正在阅读") } else if screen.done >= screen.total { t("这一轮判断完了") } else { t("这一轮停在了这里") };
        div()
            .flex()
            .items_start()
            .gap(px(14.))
            .p(px(12.))
            .rounded(px(10.))
            .border_1()
            .border_color(Theme::line())
            .bg(Theme::bg_sunken())
            .child(reading_sheet(running))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().flex_none().font_weight(FontWeight::MEDIUM).child(heading))
                            .child(div().flex_none().text_size(text_px(12.)).text_color(Theme::text_secondary()).child(format!("{} / {}", screen.done, screen.total)))
                            // What the assistant said is counted in the parts of the queue below; what it could not say, here.
                            .when(screen.failed > 0, |d| d.child(ui::chip(tf!("没判断成 {}", "{} could not be screened", screen.failed), Theme::danger(), Theme::danger_soft())))
                            .child(div().flex_1().min_w_0().truncate().text_right().text_size(text_px(11.)).text_color(Theme::text_tertiary()).child(tf!("{} · 同时 {} 篇", "{} · {} papers at a time", screen.model, self.settings.screen_workers))),
                    )
                    .child(progress_bar(screen.done, screen.total))
                    .children(next.map(|next| div().flex().child(next)))
                    .when(running, |d| {
                        d.children(reading.iter().enumerate().map(|(n, title)| {
                            let line = div().flex().items_center().gap(px(6.)).text_size(text_px(12.)).text_color(Theme::text_secondary()).child(ui::spinner(("screen-reading", n), 12., Theme::accent())).child(div().flex_1().min_w_0().truncate().child(title.clone()));
                            // Keyed by the title, so a line that takes another paper's place appears anew.
                            ui::fade_in(SharedString::from(format!("screen-reading-{title}")), line)
                        }))
                    }),
            )
    }

    /// One paper of the queue. The selected one opens up: what the
    /// assistant found, and the four decisions.
    fn render_screen_row(&self, paper: &Paper, suggestion: Option<&Suggestion>, group: &str, selected: bool, reading: bool, cx: &mut Context<Self>) -> AnyElement {
        // Rows are known by their paper, not their place: the queue is sorted anew with every suggestion.
        let id = |part: &str| SharedString::from(format!("screen-{part}-{}", paper.id));
        let mut about: Vec<String> = vec![paper.short_authors(), paper.year.map(|y| y.to_string()).unwrap_or_default(), paper.journal.clone()];
        about.retain(|part| !part.is_empty());
        let decided = paper.screening != "pending";
        let mark: Option<AnyElement> = match suggestion {
            _ if reading => Some(div().flex().flex_none().items_center().gap(px(5.)).text_size(text_px(12.)).text_color(Theme::accent()).child(ui::spinner(id("spin"), 13., Theme::accent())).child(t("正在阅读…")).into_any_element()),
            // Made against an earlier goal: there, but quieter, and not to be taken at once.
            Some(s) if group == "stale" => Some(ui::chip(tf!("旧目标：{}", "Previous criteria: {}", label(&SCREENINGS, &s.verdict)), Theme::text_tertiary(), Theme::groove()).into_any_element()),
            Some(s) => {
                let sure = s.confidence.map(|c| tf!(" · 把握 {:.0}%", " · Confidence {:.0}%", c * 100.)).unwrap_or_default();
                let chip = ui::status_chip(&s.verdict, &tf!("建议：{}{sure}", "Suggestion: {}{sure}", label(&SCREENINGS, &s.verdict)));
                let mine = decided.then(|| ui::status_chip(&paper.screening, &tf!("你的判断：{}", "Your decision: {}", label(&SCREENINGS, &paper.screening))));
                Some(ui::fade_in(id("chip"), div().flex().flex_none().items_center().gap(px(6.)).children(mine).child(chip)).into_any_element())
            }
            None => None,
        };
        let head = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(div().flex().flex_col().flex_1().min_w_0().child(div().truncate().font_weight(if selected { FontWeight::SEMIBOLD } else { FontWeight::NORMAL }).child(paper.title.clone())).child(div().truncate().text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(about.join(" · "))))
            .children(mark);
        let mut row = div()
            .id(id("row"))
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(16.))
            .py(px(10.))
            .border_b_1()
            .border_color(Theme::line())
            .on_mouse_down(MouseButton::Right, self.on_context(MenuKind::Paper(paper.id.clone()), cx))
            .when(selected, |d| d.bg(Theme::accent_soft()))
            .when(!selected, |d| d.cursor_pointer().hover(|s| s.bg(Theme::hover())).on_click(self.on(Cmd::Select(paper.id.clone()), cx)))
            .child(head);
        if !selected { return row.into_any_element() }

        row = row.child(self.render_advice(paper, true, cx));
        if decided && suggestion.is_some() {
            row = row.child(div().text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(t("选一个，这篇就不再出现在这里。")));
        }
        // The decision: every choice is there, the suggested one stands out.
        let buttons = SCREENINGS.iter().filter(|(key, _)| *key != "pending").enumerate().map(|(n, (key, name))| {
            let name = t(name);
            let agree = group != "stale" && suggestion.is_some_and(|s| s.verdict == *key);
            // Each with its key; on the suggested one, the key's cap is drawn for the accent fill.
            let cap = ui::keycap(screening::key_of(key));
            let button = if agree { ui::primary_button(("decide", n), tf!("采纳：{name}", "Accept: {name}")).child(cap.bg(Theme::on_accent().opacity(0.18)).border_color(Theme::on_accent().opacity(0.35)).text_color(Theme::on_accent())) } else if decided && paper.screening == *key { ui::secondary_button(("decide", n), tf!("保持：{name}", "Keep: {name}")).child(cap) } else { ui::secondary_button(("decide", n), name).child(cap) };
            button.on_click(self.on(Cmd::ScreenDecide(paper.id.clone(), key.to_string()), cx))
        });
        row.child(div().flex().flex_wrap().items_center().gap(px(8.)).pt(px(2.)).children(buttons)).into_any_element()
    }
}
