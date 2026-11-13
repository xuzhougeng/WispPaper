//! 确认: once the assistant has been through the waiting papers, its
//! suggestions are looked over as one list and written together. Each row
//! is a paper, what the assistant made of it and the status it will get;
//! the person changes a status, leaves a paper out, and confirms what is
//! on show. Nothing is written before that, and what was written can be
//! taken back (`wisp_core::screening::Confirming`, `Store::set_screenings`).

use crate::app::{Cmd, Confirmed, Library, ScreenMode, Workspace};
use crate::context::Nav;
use crate::theme::{self, Theme};
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use std::ops::Range;
use wisp_core::model::{Paper, SCREENINGS, label};
use wisp_core::screening::{self, Before, DECISIONS};
use wisp_core::store::Suggestion;
use wisp_core::time;

const ROW: f32 = 58.;

/// What the assistant read and how sure it was, before its reason.
fn basis(suggestion: &Suggestion) -> String {
    let read = match suggestion.basis.as_str() {
        "fulltext" => "读了全文",
        "abstract" => "只读了摘要",
        _ => "没有可读的内容",
    };
    match suggestion.confidence {
        Some(sure) => format!("把握 {:.0}% · {read}", sure * 100.),
        None => read.to_string(),
    }
}

/// How many papers get each status, as words: 「已纳入 3 · 不相关 12」.
fn breakdown(counts: &[usize; 5]) -> String {
    SCREENINGS.iter().enumerate().filter(|(n, _)| counts[*n] > 0).map(|(n, (_, name))| format!("{name} {}", counts[n])).collect::<Vec<_>>().join(" · ")
}

impl Library {
    /// The suggestions that can be confirmed: those for the waiting papers,
    /// made against the goal as it is now, in the queue's order.
    pub fn confirmable(&self) -> Vec<(&Paper, &Suggestion)> {
        self.screen_queue().into_iter().filter(|p| p.screening == "pending").filter_map(|p| self.current_suggestion(&p.id).map(|s| (p, s))).collect()
    }

    /// Those of them on show, in the order they are shown.
    pub fn confirm_rows(&self) -> Vec<(&Paper, &Suggestion)> {
        self.screen.confirm.order(self.confirmable())
    }
}

impl Workspace {
    pub(crate) fn run_confirm(&mut self, cmd: Cmd, window: &mut Window, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        match cmd {
            Cmd::ConfirmTick(id) => {
                lib.screen.confirm.toggle(&id);
                lib.screen.confirm.cursor = Some(id);
            }
            Cmd::ConfirmChoose(id, status) => {
                if let Some(suggestion) = lib.suggestions.get(&id) { lib.screen.confirm.choose(suggestion, &status) }
                lib.screen.confirm.cursor = Some(id);
            }
            Cmd::ConfirmPart(part) => {
                lib.screen.confirm.part = part;
                lib.screen.confirm_scroll = UniformListScrollHandle::new();
            }
            Cmd::ConfirmTickAll(on) => {
                let ids: Vec<String> = lib.confirm_rows().iter().map(|(p, _)| p.id.clone()).collect();
                lib.screen.confirm.tick_all(ids.iter().map(String::as_str), on);
            }
            Cmd::ConfirmSort => {
                lib.screen.confirm.doubtful_first = !lib.screen.confirm.doubtful_first;
                lib.screen.confirm_scroll = UniformListScrollHandle::new();
            }
            Cmd::ConfirmCursor(id) => {
                lib.screen.confirm.cursor = Some(id.clone());
                // A paper open beside the list follows the row.
                if lib.selected.is_some() { self.select(Some(id), window, cx) }
            }
            Cmd::ConfirmApply => self.confirm_apply(window, cx),
            Cmd::ConfirmUndo => {
                let Some(confirmed) = lib.screen.confirmed.take() else { return };
                let lines: Vec<(String, String, String, String)> = confirmed.before.iter().map(|b| (b.paper_id.clone(), b.screening.clone(), b.reason.clone(), b.reviewed_at.clone())).collect();
                if self.change(cx, |lib| lib.store.set_screenings(&lines)) { self.say(format!("已撤销：{} 篇回到确认之前。", lines.len()), cx) }
            }
            _ => {}
        }
        cx.notify();
    }

    /// Write the ticked rows on show: each paper gets its status, with the
    /// assistant's reason where the person agreed. What they were is kept, to take it back.
    fn confirm_apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_ref() else { return };
        if lib.screen.running() { return self.say("助手还在判断，等这一轮结束再确认。", cx) }
        let plan = lib.screen.confirm.plan(&lib.confirm_rows());
        if plan.is_empty() { return }
        let before: Vec<Before> = plan.iter().filter_map(|line| lib.paper(&line.paper_id)).map(Before::of).collect();
        let now = time::now_iso();
        let lines: Vec<(String, String, String, String)> = plan.iter().map(|line| (line.paper_id.clone(), line.status.clone(), line.reason.clone(), now.clone())).collect();
        if !self.change(cx, |lib| lib.store.set_screenings(&lines)) { return }
        self.select(None, window, cx);
        let Some(lib) = self.lib.as_mut() else { return };
        let counts = screening::tally(plan.iter().map(|line| line.status.as_str()));
        lib.screen.confirmed = Some(Confirmed { before, counts });
        lib.screen.confirm.cursor = None;
        lib.screen.confirm_scroll = UniformListScrollHandle::new();
    }

    /// A key of 筛选 while the suggestions are being confirmed: it is about the row the keys are on.
    pub(crate) fn confirm_key(&mut self, key: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_ref() else { return };
        let rows = lib.confirm_rows();
        let Some((paper, suggestion)) = lib.screen.confirm.cursor.as_ref().and_then(|id| rows.iter().find(|(p, _)| p.id == *id)).or(rows.first()) else { return };
        let id = paper.id.clone();
        // The first key only shows which row the keys are on.
        if lib.screen.confirm.cursor.as_deref() != Some(id.as_str()) { return self.run(Cmd::ConfirmCursor(id), window, cx) }
        let cmd = match key {
            "space" | "x" => Cmd::ConfirmTick(id),
            "a" => Cmd::ConfirmChoose(id, suggestion.verdict.clone()),
            "s" => Cmd::ToggleStar(id),
            // Left waiting: not written.
            "p" | "0" if lib.screen.confirm.ticked(&id) => Cmd::ConfirmTick(id),
            key => match screening::status_of(key).filter(|status| *status != "pending") {
                Some(status) => Cmd::ConfirmChoose(id, status.to_string()),
                None => return,
            },
        };
        self.run(cmd, window, cx);
    }

    pub(crate) fn confirm_nav(&mut self, nav: Nav, window: &mut Window, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_ref() else { return };
        let ids: Vec<String> = lib.confirm_rows().iter().map(|(p, _)| p.id.clone()).collect();
        if ids.is_empty() { return }
        let (at, last) = (lib.screen.confirm.cursor.as_ref().and_then(|id| ids.iter().position(|other| other == id)), ids.len() - 1);
        let to = match (nav, at) {
            (Nav::First, _) => 0,
            (Nav::Last, _) => last,
            (Nav::Up, None) => last,
            (_, None) => 0,
            (Nav::Up, Some(at)) => at.saturating_sub(1),
            (Nav::Down, Some(at)) => (at + 1).min(last),
            (Nav::PageUp | Nav::Left, Some(at)) => at.saturating_sub(8),
            (Nav::PageDown | Nav::Right, Some(at)) => (at + 8).min(last),
            _ => return,
        };
        lib.screen.confirm_scroll.scroll_to_item(to, if at.is_some_and(|at| to > at) { ScrollStrategy::Bottom } else { ScrollStrategy::Top });
        self.run(Cmd::ConfirmCursor(ids[to].clone()), window, cx);
    }

    pub fn render_screen_confirm(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let lib = self.lib.as_ref().unwrap();
        let confirm = &lib.screen.confirm;
        let all = lib.confirmable();
        let rows = confirm.order(all.clone());
        let on_show = if rows.len() == all.len() { "" } else { confirm.part.as_str() };
        let plan = confirm.plan(&rows);
        let counts = screening::tally(plan.iter().map(|line| line.status.as_str()));
        let changed = plan.iter().filter(|line| confirm.changed(&line.paper_id)).count();
        let skipped = rows.len() - plan.len();

        let back = ui::ghost_button("confirm-back", Some("arrow-left"), Some("回到队列".into())).on_click(self.on(Cmd::ScreenMode(ScreenMode::Batch), cx));
        let head = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(8.))
            .px(px(16.))
            .pt(px(16.))
            .pb(px(12.))
            .child(div().flex().items_center().gap(px(10.)).child(div().flex_none().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child("确认建议")).child(div().flex_1()).child(back))
            .child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child("助手的建议在这里一起过目：不同意的改掉，拿不准的不勾选，再把勾选的一次写成收录判断。写入之前什么都不会改动，写入之后也能撤销。"));

        // What the last confirmation wrote, with the way back.
        let done = lib.screen.confirmed.as_ref().map(|confirmed| {
            let (color, fill) = theme::status_colors("included");
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .mx(px(16.))
                .mb(px(10.))
                .px(px(12.))
                .py(px(8.))
                .rounded(px(8.))
                .bg(fill)
                .child(ui::icon("check", 14., color))
                .child(div().flex_1().min_w_0().text_color(color).child(format!("已确认 {} 篇：{}。", confirmed.before.len(), breakdown(&confirmed.counts))))
                .child(ui::ghost_button("confirm-undo", None, Some("撤销".into())).h(px(24.)).text_size(px(12.)).on_click(self.on(Cmd::ConfirmUndo, cx)))
        });
        if all.is_empty() {
            return div().flex().flex_col().size_full().child(head).children(done).child(self.render_confirm_empty(cx)).into_any_element();
        }

        // One part per suggested verdict, as in the queue.
        let part = |n: usize, key: &str, text: String, this: &Self, cx: &mut Context<Self>| {
            let on = on_show == key;
            let (color, fill) = if key.is_empty() { (Theme::text_secondary(), Theme::groove()) } else { theme::status_colors(key) };
            div().id(("confirm-part", n)).flex().flex_none().items_center().h(px(26.)).px(px(10.)).rounded(px(13.)).text_size(px(12.)).cursor_pointer().border_1().when(on, |d| d.bg(fill).text_color(color).border_color(color.opacity(0.35)).font_weight(FontWeight::MEDIUM)).when(!on, |d| d.border_color(Theme::line()).text_color(Theme::text_secondary()).hover(|s| s.bg(Theme::hover()))).on_click(this.on(Cmd::ConfirmPart(key.to_string()), cx)).child(text)
        };
        let mut bar: Vec<AnyElement> = vec![part(0, "", format!("全部 {}", all.len()), self, cx).into_any_element()];
        for (n, (key, name)) in SCREENINGS.iter().enumerate().skip(1) {
            let count = all.iter().filter(|(_, s)| s.verdict == *key).count();
            if count > 0 { bar.push(part(n, key, format!("建议{name} {count}"), self, cx).into_any_element()) }
        }
        let small = |id: &'static str, text: &'static str| ui::ghost_button(id, None, Some(text.into())).h(px(26.)).text_size(px(12.));
        let tools = div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap(px(6.))
            .px(px(16.))
            .pb(px(10.))
            .children(bar)
            .child(div().flex_1())
            .child(small("confirm-sort", if confirm.doubtful_first { "把握低的在前 ✓" } else { "把握低的在前" }).when(confirm.doubtful_first, |d| d.text_color(Theme::accent())).on_click(self.on(Cmd::ConfirmSort, cx)))
            .child(small("confirm-all", "全选").on_click(self.on(Cmd::ConfirmTickAll(true), cx)))
            .child(small("confirm-none", "全不选").on_click(self.on(Cmd::ConfirmTickAll(false), cx)));

        let list = uniform_list("confirm-rows", rows.len(), cx.processor(|this, range: Range<usize>, _, cx| this.render_confirm_rows(range, cx))).track_scroll(lib.screen.confirm_scroll.clone()).flex_1();
        // The row the keys are on, in full: a row of the list has one line for the reason.
        let cursor = confirm.cursor.as_ref().and_then(|id| rows.iter().find(|(p, _)| p.id == *id)).map(|(paper, _)| {
            let beside = lib.selected.as_deref() == Some(paper.id.as_str());
            let open = ui::ghost_button("confirm-beside", Some("file-text"), Some(if beside { "收起".into() } else if lib.pdf_file(&paper.id).is_some() { "对照 PDF".into() } else { "完整记录".into() })).h(px(24.)).text_size(px(12.)).on_click(self.on(Cmd::ScreenBeside, cx));
            div()
                .id("confirm-cursor")
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
                .child(div().flex().items_center().gap(px(8.)).child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::MEDIUM).child(paper.title.clone())).child(open))
                .child(self.render_advice(paper, false, cx))
        });

        let summary = if plan.is_empty() {
            "没有勾选的文献：勾选要写入的，或回到队列。".to_string()
        } else {
            let mut text = format!("将写入 {} 篇：{}。", plan.len(), breakdown(&counts));
            if changed > 0 { text.push_str(&format!("其中 {changed} 篇改了助手的建议。")) }
            if skipped > 0 { text.push_str(&format!("{skipped} 篇没有勾选，留在待判断。")) }
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
                    .items_center()
                    .gap(px(10.))
                    .child(div().flex_1().min_w_0().text_size(px(12.)).text_color(Theme::text_secondary()).child(summary))
                    .child(ui::primary_button("confirm-apply", if plan.is_empty() { "确认".to_string() } else { format!("确认这 {} 篇", plan.len()) }).when(plan.is_empty(), |d| d.opacity(0.5)).on_click(self.on(Cmd::ConfirmApply, cx))),
            )
            .child(div().flex().flex_wrap().items_center().gap_x(px(14.)).gap_y(px(6.)).text_size(px(11.)).line_height(px(18.)).text_color(Theme::text_tertiary()).child(hint("↑↓", "移动")).child(hint("Space", "勾选 / 不勾选")).child(hint("Y", "已纳入")).child(hint("R", "相关")).child(hint("U", "证据不足")).child(hint("N", "不相关")).child(hint("A", "按建议")).child(hint("Enter", "对照 PDF")).child(hint("Esc", "回到队列")));

        div().flex().flex_col().size_full().child(head).children(done).child(tools).child(div().flex().flex_col().flex_1().min_h_0().border_t_1().border_color(Theme::line()).child(list)).children(cursor).child(foot).into_any_element()
    }

    /// Nothing left to confirm: what was just written, or that there never was anything.
    fn render_confirm_empty(&self, cx: &mut Context<Self>) -> Div {
        let lib = self.lib.as_ref().unwrap();
        let waiting = lib.screen_queue().len();
        let leave = ui::secondary_button("confirm-leave", "回到队列").on_click(self.on(Cmd::ScreenMode(ScreenMode::Batch), cx));
        let single = (waiting > 0).then(|| ui::primary_button("confirm-single", format!("逐篇看剩下的 {waiting} 篇")).on_click(self.on(Cmd::ScreenMode(ScreenMode::Single), cx)));
        let (title, hint) = match &lib.screen.confirmed {
            Some(_) if waiting == 0 => ("都确认了", "队列里没有待判断的文献了。新导入的文献会再出现在筛选里。".to_string()),
            Some(_) => ("建议都确认了", format!("队列里还有 {waiting} 篇：助手没有建议的，或需要你再看一眼的。")),
            None => ("没有可以确认的建议", "先回到队列，让助手判断待判断的文献；它给出建议后再来这里确认。".to_string()),
        };
        div().flex().flex_col().flex_1().items_center().justify_center().gap(px(4.)).child(ui::empty_state("check", title, &hint).flex_none()).child(div().flex().gap(px(8.)).children(single).child(leave))
    }

    fn render_confirm_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(lib) = self.lib.as_ref() else { return vec![] };
        let confirm = &lib.screen.confirm;
        let rows = lib.confirm_rows();
        range
            .filter_map(|i| {
                let (paper, suggestion) = rows.get(i)?;
                let id = |part: &str| SharedString::from(format!("confirm-{part}-{}", paper.id));
                let (ticked, on_cursor) = (confirm.ticked(&paper.id), confirm.cursor.as_deref() == Some(paper.id.as_str()));
                let status = confirm.status(suggestion);
                let tick = div()
                    .id(id("tick"))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(28.))
                    .cursor_pointer()
                    .on_click(self.on(Cmd::ConfirmTick(paper.id.clone()), cx))
                    .child(div().flex().items_center().justify_center().size(px(16.)).rounded(px(4.)).border_1().when(ticked, |d| d.bg(Theme::accent()).border_color(Theme::accent()).child(ui::icon("check", 11., Theme::on_accent()))).when(!ticked, |d| d.border_color(Theme::line_strong()).bg(Theme::bg_body())));
                let mut about: Vec<String> = vec![paper.short_authors(), paper.year.map(|y| y.to_string()).unwrap_or_default(), basis(suggestion), suggestion.reason.clone()];
                about.retain(|part| !part.is_empty());
                let text = div()
                    .id(id("text"))
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .justify_center()
                    .cursor_pointer()
                    .on_click(self.on(Cmd::ConfirmCursor(paper.id.clone()), cx))
                    .child(div().truncate().when(on_cursor, |d| d.font_weight(FontWeight::MEDIUM)).child(paper.title.clone()))
                    .child(div().truncate().text_size(px(12.)).text_color(Theme::text_tertiary()).child(about.join(" · ")));
                // The status the paper will get: the suggested one unless the person picks another.
                let choices = DECISIONS.iter().enumerate().map(|(n, (key, _, _))| {
                    let (color, fill) = theme::status_colors(key);
                    let (chosen, advised) = (status == *key, suggestion.verdict == *key);
                    div()
                        .id(SharedString::from(format!("confirm-choice-{n}-{}", paper.id)))
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(4.))
                        .h(px(24.))
                        .px(px(8.))
                        .rounded(px(6.))
                        .text_size(px(12.))
                        .cursor_pointer()
                        .when(chosen, |d| d.bg(fill).text_color(color).font_weight(FontWeight::MEDIUM))
                        .when(!chosen, |d| d.text_color(Theme::text_tertiary()).hover(|s| s.bg(Theme::hover()).text_color(Theme::text())))
                        // Where the person chose otherwise, a dot keeps what the assistant said in sight.
                        .when(advised && !chosen, |d| d.child(div().flex_none().size(px(5.)).rounded_full().bg(color)))
                        .on_click(self.on(Cmd::ConfirmChoose(paper.id.clone(), key.to_string()), cx))
                        .child(label(&SCREENINGS, key))
                });
                Some(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .w_full()
                        .h(px(ROW))
                        .pl(px(10.))
                        .pr(px(16.))
                        .border_b_1()
                        .border_color(Theme::line())
                        .when(on_cursor, |d| d.bg(Theme::accent_soft()))
                        .when(!on_cursor, |d| d.hover(|s| s.bg(Theme::hover())))
                        .child(tick)
                        .child(text.when(!ticked, |d| d.opacity(0.5)))
                        .child(div().flex().flex_none().items_center().gap(px(2.)).when(!ticked, |d| d.opacity(0.5)).children(choices))
                        .into_any_element(),
                )
            })
            .collect()
    }
}
