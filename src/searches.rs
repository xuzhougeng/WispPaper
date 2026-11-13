//! 检索与导入: the saved searches of a library, a trial run of one of them,
//! and the record of what the runs brought in.

use crate::i18n::{label, t};
use crate::app::{Cmd, Workspace};
use crate::theme::Theme;
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use serde_json::Value;
use wisp_core::model::{SCREENINGS};
use wisp_core::sources;
use wisp_core::store::ImportBatch;
use wisp_core::time;

/// 「命中 90 篇，取回 5 篇，新增 5 篇，跳过 0 篇」, or why the run failed.
fn run_summary(batch: &ImportBatch) -> (String, bool) {
    if !batch.error.is_empty() { return (tf!("失败：{}", "Failed: {}", batch.error), true) }
    let total = batch.remote_total.map(|n| tf!("命中 {n} 篇，", "{n} results, ")).unwrap_or_default();
    (tf!("{total}取回 {} 篇，新增 {} 篇，跳过已有的 {} 篇", "{total}retrieved {}, added {}, skipped {} existing", batch.fetched, batch.added, batch.skipped), false)
}

impl Workspace {
    pub fn render_searches(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let lib = self.lib.as_ref().unwrap();
        let mut cards: Vec<AnyElement> = vec![];
        for (i, search) in lib.searches.iter().enumerate() {
            let source = label(&sources::KINDS, &search.kind).to_string();
            let running = lib.running.contains(&search.id);
            let last = match &search.last_run {
                Some(batch) => {
                    let (summary, failed) = run_summary(batch);
                    div().text_size(px(12.)).text_color(if failed { Theme::danger() } else { Theme::text_tertiary() }).child(tf!("上次运行 {}：{summary}", "Last run {}: {summary}", time::short_time(&batch.started_at)))
                }
                None => div().text_size(px(12.)).text_color(Theme::text_tertiary()).child(t("还没有运行过")),
            };
            let trial = lib.trial.as_ref().filter(|(id, _)| *id == search.id).map(|(_, found)| self.render_trial(found.as_ref(), cx));
            cards.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .p(px(14.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(Theme::line())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .flex_wrap()
                            .gap(px(8.))
                            .child(div().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).child(search.name.clone()))
                            .when(search.name != source, |d| d.child(ui::chip(source.clone(), Theme::text_secondary(), Theme::bg_sunken())))
                            .when(!search.enabled, |d| d.child(ui::chip(t("已停用"), Theme::text_tertiary(), Theme::bg_sunken())))
                            .child(div().flex_1())
                            .child(ui::secondary_button(("try-search", i), t("试检索")).on_click(self.on(Cmd::TrySearch(search.id.clone()), cx)))
                            .child(ui::primary_button(("run-search", i), if running { t("运行中…") } else { t("运行并导入") }).when(running, |d| d.opacity(0.5)).on_click(self.on(Cmd::RunSearch(search.id.clone()), cx)))
                            .child(ui::ghost_button(("toggle-search", i), None, Some(if search.enabled { t("停用").into() } else { t("启用").into() })).on_click(self.on(Cmd::ToggleSearch(search.id.clone()), cx)))
                            .child(ui::ghost_button(("edit-search", i), Some("pen-line"), None).on_click(self.on(Cmd::EditSearch(search.id.clone()), cx)))
                            .child(ui::ghost_button(("remove-search", i), Some("bin"), None).on_click(self.on(Cmd::AskRemoveSearch(search.id.clone()), cx))),
                    )
                    .child(div().px(px(10.)).py(px(8.)).rounded(px(6.)).bg(Theme::bg_sunken()).text_size(px(13.)).line_height(px(20.)).child(search.query.clone()))
                    .child(div().flex().items_center().flex_wrap().gap(px(12.)).child(last).child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child(tf!("每次取回最新的 {} 篇", "Retrieve the latest {} papers each run", search.max_results))))
                    .children(trial)
                    .into_any_element(),
            );
        }

        let batches: Vec<AnyElement> = lib
            .batches
            .iter()
            .enumerate()
            .map(|(i, batch)| {
                let (summary, failed) = run_summary(batch);
                let undone = !batch.undone_at.is_empty();
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .py(px(8.))
                    .border_b_1()
                    .border_color(Theme::line())
                    .child(div().flex_none().w(px(84.)).text_size(px(12.)).text_color(Theme::text_tertiary()).child(time::short_time(&batch.started_at)))
                    .child(div().flex().flex_col().flex_1().min_w_0().child(div().truncate().text_size(px(13.)).child(batch.label.clone())).child(div().truncate().text_size(px(12.)).text_color(if failed { Theme::danger() } else { Theme::text_tertiary() }).child(summary)))
                    .when(undone, |d| d.child(ui::chip(t("已撤销"), Theme::text_tertiary(), Theme::bg_sunken())))
                    .when(!undone && !failed && batch.added > 0, |d| d.child(ui::ghost_button(("undo-import", i), None, Some(t("撤销").into())).on_click(self.on(Cmd::AskUndoImport(batch.id.clone()), cx))))
                    .into_any_element()
            })
            .collect();

        let empty = cards.is_empty();
        // How often the enabled searches run on their own.
        let hours = lib.info.auto_sync_hours;
        let auto = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .px(px(16.))
            .pb(px(12.))
            .child(div().pr(px(4.)).text_size(px(13.)).text_color(Theme::text_secondary()).child(t("自动更新")))
            .children([(0, t("关闭")), (6, t("每 6 小时")), (24, t("每天")), (168, t("每周"))].into_iter().map(|(value, name)| {
                let on = hours == value;
                div()
                    .id(("auto-sync", value as usize))
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(26.))
                    .px(px(10.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(if on { Theme::accent() } else { Theme::line() })
                    .when(on, |d| d.bg(Theme::accent_soft()).text_color(Theme::accent()))
                    .text_size(px(12.))
                    .cursor_pointer()
                    .child(name)
                    .on_click(self.on(Cmd::SetAutoSync(value), cx))
            }))
            .child(div().pl(px(6.)).min_w_0().truncate().text_size(px(12.)).text_color(Theme::text_tertiary()).child(if hours == 0 { t("只在点「运行并导入」时访问数据库。") } else { t("应用开着时按这个间隔运行已启用的检索式；关着时错过的，下次打开时补上。") }));
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .flex_wrap()
                    .gap(px(8.))
                    .px(px(16.))
                    .pt(px(16.))
                    .pb(px(10.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(t("检索与导入")))
                            .child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child(t("为文献数据库保存检索式：先试检索，满意了再运行导入。导入的文献都是「待判断」，已有的会跳过，每次导入可以整批撤销。"))),
                    )
                    .when(!empty, |d| d.child(ui::secondary_button("run-all-searches", t("全部运行")).on_click(self.on(Cmd::RunAllSearches, cx))))
                    .child(ui::secondary_button("open-guide", t("让助手拟检索式")).on_click(self.on(Cmd::OpenGuide(false), cx)))
                    .child(ui::primary_button("new-search", t("新建检索式")).on_click(self.on(Cmd::NewSearch, cx))),
            )
            .when(!empty, |d| d.child(auto))
            .child(if empty {
                ui::empty_state("search", t("还没有检索式"), t("检索式是写给 PubMed、Europe PMC、Crossref 或 arXiv 的查询。保存后可以反复运行，把新发表的文献带进文献库。")).into_any_element()
            } else {
                div()
                    .id("searches")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(16.))
                    .pb(px(16.))
                    .gap(px(12.))
                    .children(cards)
                    .when(!batches.is_empty(), |d| d.child(div().flex().flex_col().pt(px(8.)).child(ui::caption(t("导入记录")).pb(px(4.))).children(batches)))
                    .into_any_element()
            })
            .into_any_element()
    }

    /// What a trial run found: the count in the database, and the newest
    /// records with what the library already knows about each.
    fn render_trial(&self, found: Option<&Result<Value, String>>, cx: &mut Context<Self>) -> AnyElement {
        let close = ui::ghost_button("close-trial", Some("x"), None).on_click(self.on(Cmd::CloseTrial, cx));
        let panel = div().flex().flex_col().gap(px(6.)).p(px(10.)).rounded(px(8.)).border_1().border_color(Theme::line()).bg(Theme::bg_body());
        let found = match found {
            None => return panel.child(div().flex().items_center().child(div().flex_1().text_size(px(12.)).text_color(Theme::text_tertiary()).child(t("正在检索…"))).child(close)).into_any_element(),
            Some(Err(error)) => return panel.child(div().flex().items_center().child(div().flex_1().child(ui::error_line(error))).child(close)).into_any_element(),
            Some(Ok(found)) => found,
        };
        let total = found["total"].as_u64().map(|n| tf!("数据库中共命中 {n} 篇。", "{n} results in the database.")).unwrap_or_default();
        let head = tf!("{total}取回最新的 {} 篇：{} 篇是新的，{} 篇已在文献库中。", "{total}Retrieved the latest {} papers: {} new, {} already in this library.", found["fetched"], found["new"], found["inLibrary"]);
        let rows = found["results"].as_array().into_iter().flatten().map(|record| {
            let text = |key: &str| record[key].as_str().unwrap_or("").to_string();
            let mut about: Vec<String> = vec![text("authors"), record["year"].as_i64().map(|y| y.to_string()).unwrap_or_default(), text("journal")];
            about.retain(|part| !part.is_empty());
            // Already in the library: say so, and what was decided about it.
            let known = record["inLibrary"].as_object().map(|held| if held["inTrash"] == true { t("在回收站").to_string() } else { tf!("已在库中 · {}", "In library · {}", label(&SCREENINGS, held["screening"].as_str().unwrap_or(""))) });
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .py(px(5.))
                .border_t_1()
                .border_color(Theme::line())
                .child(div().flex().flex_col().flex_1().min_w_0().child(div().truncate().text_size(px(13.)).text_color(if known.is_some() { Theme::text_tertiary() } else { Theme::text() }).child(text("title"))).child(div().truncate().text_size(px(12.)).text_color(Theme::text_tertiary()).child(about.join(" · "))))
                .children(known.map(|known| ui::chip(known, Theme::text_secondary(), Theme::bg_sunken())))
        });
        panel.child(div().flex().items_center().child(div().flex_1().text_size(px(12.)).text_color(Theme::text_secondary()).child(head)).child(close)).children(rows).into_any_element()
    }
}
