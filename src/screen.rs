//! 筛选: the papers waiting for a decision. The assistant reads them, several
//! at a time, and suggests; the person decides, one paper at a time, with
//! the paper's record and PDF open beside the list. While the assistant is
//! at it, a panel shows how far the run is. The queue can be looked at one
//! suggested verdict at a time, and such a part decided in one go.

use crate::app::{Cmd, Screen, Workspace};
use crate::settings::SettingsPage;
use crate::theme::Theme;
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use std::time::Duration;
use wisp_core::model::{Paper, SCREENINGS, label};
use wisp_core::settings::Task;
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
    pub fn render_screen(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
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
        let parts: Vec<(&str, &str, usize)> = SCREENINGS.iter().filter(|(key, _)| *key != "pending").map(|(key, name)| (*key, *name)).chain([("differs", "与你的判断不同"), ("stale", "按旧目标的建议"), ("none", "还没有建议")]).map(|(key, name)| (key, name, all.iter().filter(|p| lib.screen_group(&p.id) == key).count())).filter(|(_, _, count)| *count > 0).collect();
        let part = |n: usize, key: &str, text: String, this: &Self, cx: &mut Context<Self>| {
            let on = filter == key;
            let (color, fill) = match key {
                "" | "none" | "stale" => (Theme::text_secondary(), Theme::groove()),
                "differs" => crate::theme::status_colors("uncertain"),
                verdict => crate::theme::status_colors(verdict),
            };
            div().id(("screen-part", n)).flex().flex_none().items_center().h(px(26.)).px(px(10.)).rounded(px(13.)).text_size(px(12.)).cursor_pointer().border_1().when(on, |d| d.bg(fill).text_color(color).border_color(color.opacity(0.35)).font_weight(FontWeight::MEDIUM)).when(!on, |d| d.border_color(Theme::line()).text_color(Theme::text_secondary()).hover(|s| s.bg(Theme::hover()))).on_click(this.on(Cmd::ScreenFilter(key.to_string()), cx)).child(text)
        };
        let mut bar: Vec<AnyElement> = vec![part(0, "", format!("全部 {}", all.len()), self, cx).into_any_element()];
        bar.extend(parts.iter().enumerate().map(|(n, (key, name, count))| part(n + 1, key, format!("{name} {count}"), self, cx).into_any_element()));
        // A whole part can be decided at once, as the assistant suggested, once it has been looked over.
        let accept = (SCREENINGS.iter().any(|(key, _)| *key == filter) && !screen.running()).then(|| ui::secondary_button("screen-accept", format!("全部采纳为「{}」（{} 篇）", label(&SCREENINGS, filter), queue.len())).h(px(26.)).text_size(px(12.)).on_click(self.on(Cmd::AskScreenAccept(filter.to_string()), cx)));
        let configured = !self.settings.profile_for_or_default(Task::Screen).key().is_empty();
        let mut status = if pending == 0 { "没有待判断的文献。".to_string() } else { format!("待判断 {pending} 篇，其中 {suggested} 篇已有按当前目标的建议。") };
        if stale > 0 { status.push_str(&format!("{stale} 篇的建议是按旧目标给出的。")) }
        if differs > 0 { status.push_str(&format!("已判断的文献里，{differs} 篇按当前目标看与你的判断不同。")) }
        let start = match (waiting, stale) {
            (0, _) => "让助手判断".to_string(),
            (_, 0) => format!("让助手判断 {waiting} 篇"),
            _ => format!("按当前目标判断 {waiting} 篇"),
        };
        let rows: Vec<AnyElement> = queue.iter().take(SHOWN).map(|paper| self.render_screen_row(paper, lib.suggestions.get(&paper.id), lib.screen_group(&paper.id), lib.selected.as_deref() == Some(paper.id.as_str()), screen.reading.contains(&paper.id), cx)).collect();
        let titles: Vec<String> = screen.reading.iter().filter_map(|id| lib.paper(id)).map(|p| p.title.clone()).collect();

        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(px(10.))
                    .px(px(16.))
                    .pt(px(16.))
                    .pb(px(12.))
                    .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child("筛选"))
                    .child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child("助手阅读待判断的文献（有全文读全文，否则读摘要），几篇同时进行，给出建议、理由和原文依据。建议只是建议：每一篇的收录判断由你确认。"))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(div().flex().items_baseline().gap(px(8.)).child(div().flex_none().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child("筛选目标")).child(div().flex_1().min_w_0().truncate().text_size(px(12.)).text_color(Theme::text_tertiary()).child(if lib.info.criteria.is_empty() && !lib.info.topic.trim().is_empty() { format!("留空时按主题判断：{}", lib.info.topic.trim()) } else { "什么样的文献应当收进这个文献库".to_string() })))
                            .child(ui::field(&screen.criteria, window, cx))
                            .child(div().text_size(px(11.)).text_color(Theme::text_tertiary()).child("随时可以换一个目标：已有的建议会标为「按旧目标」，再让助手按新目标判断一遍；已判断的文献也可以复核，你的判断不会被改动。")),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(div().flex_1().min_w_0().text_size(px(12.)).text_color(Theme::text_secondary()).child(status))
                            .when(!configured, |d| d.child(ui::ghost_button("screen-settings", Some("settings-gear"), Some("先配置模型".into())).on_click(self.on(Cmd::OpenSettings(SettingsPage::Ai), cx))))
                            // The decided papers looked at again against the goal as it is now; only those seen otherwise come back.
                            .when(!screen.running() && unchecked > 0, |d| d.child(ui::secondary_button("screen-recheck", format!("复核已判断的 {unchecked} 篇")).on_click(self.on(Cmd::ScreenRecheck, cx))))
                            .child(if screen.running() {
                                ui::secondary_button("screen-stop", "停止").on_click(self.on(Cmd::ScreenStop, cx))
                            } else {
                                ui::primary_button("screen-start", start).when(waiting == 0, |d| d.opacity(0.5)).on_click(self.on(Cmd::ScreenStart, cx))
                            }),
                    )
                    .when(screen.total > 0, |d| d.child(self.render_screen_run(screen, &titles)))
                    .when(!screen.error.is_empty(), |d| d.child(ui::error_line(&screen.error)))
                    .when(parts.len() > 1 || parts.first().is_some_and(|(key, _, _)| *key != "none"), |d| d.child(div().flex().items_center().flex_wrap().gap(px(6.)).pt(px(2.)).children(bar).child(div().flex_1()).children(accept))),
            )
            .child(if rows.is_empty() {
                ui::empty_state("check", "都判断过了", "新导入的文献会出现在这里。").into_any_element()
            } else {
                div()
                    .id("screen-queue")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .border_t_1()
                    .border_color(Theme::line())
                    .children(rows)
                    .when(queue.len() > SHOWN, |d| d.child(div().px(px(16.)).py(px(10.)).text_size(px(12.)).text_color(Theme::text_tertiary()).child(format!("还有 {} 篇，判断完上面的会继续出现。", queue.len() - SHOWN))))
                    .into_any_element()
            })
            .into_any_element()
    }

    /// The run of the assistant, under way or over: how far it got, and the
    /// papers it is reading at this moment.
    fn render_screen_run(&self, screen: &Screen, reading: &[String]) -> Div {
        let running = screen.running();
        let heading = if running { "助手正在阅读" } else if screen.done >= screen.total { "这一轮判断完了" } else { "这一轮停在了这里" };
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
                            .child(div().flex_none().text_size(px(12.)).text_color(Theme::text_secondary()).child(format!("{} / {}", screen.done, screen.total)))
                            // What the assistant said is counted in the parts of the queue below; what it could not say, here.
                            .when(screen.failed > 0, |d| d.child(ui::chip(format!("没判断成 {}", screen.failed), Theme::danger(), Theme::danger_soft())))
                            .child(div().flex_1().min_w_0().truncate().text_right().text_size(px(11.)).text_color(Theme::text_tertiary()).child(format!("{} · 同时 {} 篇", screen.model, self.settings.screen_workers))),
                    )
                    .child(progress_bar(screen.done, screen.total))
                    .when(running, |d| {
                        d.children(reading.iter().enumerate().map(|(n, title)| {
                            let line = div().flex().items_center().gap(px(6.)).text_size(px(12.)).text_color(Theme::text_secondary()).child(ui::spinner(("screen-reading", n), 12., Theme::accent())).child(div().flex_1().min_w_0().truncate().child(title.clone()));
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
            _ if reading => Some(div().flex().flex_none().items_center().gap(px(5.)).text_size(px(12.)).text_color(Theme::accent()).child(ui::spinner(id("spin"), 13., Theme::accent())).child("正在阅读…").into_any_element()),
            // Made against an earlier goal: there, but quieter, and not to be taken at once.
            Some(s) if group == "stale" => Some(ui::chip(format!("旧目标：{}", label(&SCREENINGS, &s.verdict)), Theme::text_tertiary(), Theme::groove()).into_any_element()),
            Some(s) => {
                let sure = s.confidence.map(|c| format!(" · 把握 {:.0}%", c * 100.)).unwrap_or_default();
                let chip = ui::status_chip(&s.verdict, &format!("建议：{}{sure}", label(&SCREENINGS, &s.verdict)));
                let mine = decided.then(|| ui::status_chip(&paper.screening, &format!("你的判断：{}", label(&SCREENINGS, &paper.screening))));
                Some(ui::fade_in(id("chip"), div().flex().flex_none().items_center().gap(px(6.)).children(mine).child(chip)).into_any_element())
            }
            None => None,
        };
        let head = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(div().flex().flex_col().flex_1().min_w_0().child(div().truncate().font_weight(if selected { FontWeight::SEMIBOLD } else { FontWeight::NORMAL }).child(paper.title.clone())).child(div().truncate().text_size(px(12.)).text_color(Theme::text_tertiary()).child(about.join(" · "))))
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
            .when(selected, |d| d.bg(Theme::accent_soft()))
            .when(!selected, |d| d.cursor_pointer().hover(|s| s.bg(Theme::hover())).on_click(self.on(Cmd::Select(paper.id.clone()), cx)))
            .child(head);
        if !selected { return row.into_any_element() }

        if let Some(s) = suggestion {
            let read = match s.basis.as_str() {
                "fulltext" => "读了全文",
                "abstract" => "只读了摘要",
                _ => "没有可读的内容",
            };
            row = row.child(div().text_size(px(13.)).line_height(px(20.)).child(s.reason.clone()));
            if !s.excerpt.is_empty() {
                // The words the suggestion rests on; with a page, a click opens the PDF there.
                let quote = div().id(id("quote")).flex().gap(px(8.)).px(px(10.)).py(px(8.)).rounded(px(6.)).bg(Theme::bg_body()).border_l_2().border_color(Theme::accent()).text_size(px(12.)).line_height(px(18.)).text_color(Theme::text_secondary()).child(div().flex_1().min_w_0().child(s.excerpt.clone()));
                row = row.child(match s.page {
                    Some(page) => quote.cursor_pointer().child(div().flex_none().text_color(Theme::accent()).child(format!("第 {page} 页"))).on_click(self.on(Cmd::GotoSource(paper.id.clone(), Some(page), s.excerpt.clone()), cx)),
                    None => quote.child(div().flex_none().text_color(Theme::text_tertiary()).child("摘要")),
                });
            }
            row = row.child(div().text_size(px(11.)).text_color(Theme::text_tertiary()).child(format!("{read} · {}", s.author)));
            if group == "stale" {
                let old = s.criteria.as_deref().filter(|c| !c.is_empty()).map_or("按文献库的主题".to_string(), |c| format!("「{c}」"));
                row = row.child(div().text_size(px(12.)).text_color(Theme::warning()).child(format!("这条建议是按之前的筛选目标 {old} 给出的；让助手按当前目标判断后会被替换。")));
            }
            if decided {
                row = row.child(div().text_size(px(12.)).text_color(Theme::warning()).child(format!("你之前判断为「{}」，按当前的筛选目标，助手的建议不同。选一个，这篇就不再出现在这里。", label(&SCREENINGS, &paper.screening))));
            }
        } else {
            row = row.child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child(if reading { "助手正在读这一篇。" } else { "助手还没有判断这一篇。" }));
        }
        // The decision: every choice is there, the suggested one stands out.
        let buttons = SCREENINGS.iter().filter(|(key, _)| *key != "pending").enumerate().map(|(n, (key, name))| {
            let agree = group != "stale" && suggestion.is_some_and(|s| s.verdict == *key);
            let button = if agree { ui::primary_button(("decide", n), format!("采纳：{name}")) } else if decided && paper.screening == *key { ui::secondary_button(("decide", n), format!("保持：{name}")) } else { ui::secondary_button(("decide", n), *name) };
            button.on_click(self.on(Cmd::ScreenDecide(paper.id.clone(), key.to_string()), cx))
        });
        row.child(div().flex().items_center().gap(px(8.)).pt(px(2.)).children(buttons)).into_any_element()
    }
}
