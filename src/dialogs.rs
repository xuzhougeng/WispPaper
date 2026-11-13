//! The dialogs: library, add by identifier, paper form, custom fields
//! and confirmations. Their state and what happens on submit are
//! in app.rs; this file draws them.

use crate::i18n::{label, t};
use crate::app::{Cmd, ImportRow, Imported, Modal, Workspace};
use crate::theme::{self, Theme};
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use wisp_core::complete;
use wisp_core::model::{FieldKind, PUBLICATION_STATUSES};
use wisp_core::sources;

/// A choice among a few: the chosen one filled with the accent.
fn option_chip(id: impl Into<ElementId>, label: impl Into<SharedString>, chosen: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .h(px(28.))
        .px(px(11.))
        .rounded(px(14.))
        .border_1()
        .cursor_pointer()
        .text_size(px(12.))
        .when(chosen, |d| d.bg(Theme::accent()).border_color(Theme::accent()).text_color(Theme::on_accent()))
        .when(!chosen, |d| d.border_color(Theme::line_strong()).text_color(Theme::text_secondary()).hover(|s| s.bg(Theme::hover())))
        .child(label.into())
}

impl Workspace {
    pub fn render_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.tend_figure_dialog(window, cx);
        let modal = self.modal.as_ref()?;
        let error = ui::error_line(&self.modal_error);
        let footer = |confirm: &str, danger: bool, this: &Self, cx: &mut Context<Self>| {
            div()
                .flex()
                .flex_none()
                .justify_end()
                .gap(px(8.))
                .px(px(20.))
                .py(px(14.))
                .child(ui::secondary_button("modal-cancel", t("取消")).on_click(this.on(Cmd::CloseModal, cx)))
                .child(ui::primary_button("modal-ok", confirm.to_string()).when(danger, |d| d.bg(Theme::danger())).on_click(this.on(Cmd::SubmitModal, cx)))
        };
        let body = || div().id("modal-body").flex().flex_col().flex_1().min_h_0().overflow_y_scroll().gap(px(12.)).px(px(20.)).py(px(8.));

        let card = match modal {
            Modal::Section { .. } => self.render_section_dialog(window, cx)?,
            Modal::History { .. } => self.render_history_dialog(cx)?,
            Modal::ReviewEvidence(_) => self.render_review_evidence_dialog(window, cx)?,
            Modal::ReviewText { .. } => self.render_review_text_dialog(window, cx)?,
            Modal::Figure(_) => self.render_figure_dialog(window, cx)?,
            Modal::Zotero => ui::dialog(560.)
                .child(ui::dialog_title(t("从 Zotero 导入")))
                .child(body()
                    .child(div().line_height(px(23.)).child(t("1. 在 Zotero 的「设置 › 高级 › 文件和文件夹」中打开数据目录。\n2. 等同步完成，下载需要的 PDF，然后退出 Zotero。\n3. 点击下方按钮，选择该目录中的 zotero.sqlite。")))
                    .child(div().text_color(Theme::text_secondary()).line_height(px(21.)).child(t("导入本机个人库和群组库的文献信息、标签、笔记与 PDF。分类转为 Zotero/分类路径 标签；原数据库和 PDF 保留。已有文献保留原记录，并补充 PDF。")))
                    .child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).line_height(px(19.)).child(t("这是一次本地复制，不会持续同步。暂不迁移独立笔记、PDF 批注、网页快照及其他非 PDF 附件；相对链接附件会在结果中提示手动处理。只迁移部分文献时，可从 Zotero 导出 BibTeX / RIS，再使用对应入口。")))
                    .child(error))
                .child(footer(t("选择 zotero.sqlite…"), false, self, cx)),
            Modal::ZoteroReport => {
                let run = self.lib.as_ref()?.zotero.as_ref()?;
                let rows = run.rows.iter().enumerate().map(|(i, row)| {
                    let status = if row.in_trash { t("在回收站中，未导入") } else if row.added { t("新增文献") } else if row.paper_id.is_some() { t("已有文献，保留原记录") } else { t("文献未导入") };
                    let summary = tf!("{status} · 新增 PDF {} · 已有 PDF {}{}", "{status} · {} PDFs added · {} PDFs already present{}", row.files_added, row.files_existing, if row.problems.is_empty() { String::new() } else { tf!(" · {} 项待处理", " · {} issues to review", row.problems.len()) });
                    div().id(("zotero-row", i)).flex().flex_none().flex_col().gap(px(4.)).py(px(8.)).border_b_1().border_color(Theme::line())
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(if row.title.is_empty() { t("（无题目）").into() } else { row.title.clone() }))
                        .child(div().text_size(px(12.)).text_color(Theme::text_secondary()).child(summary))
                        .children(row.problems.iter().map(|problem| div().text_size(px(12.)).text_color(Theme::warning()).child(problem.clone())))
                        .when_some(row.paper_id.clone().filter(|_| !run.running), |d, id| d.cursor_pointer().hover(|s| s.bg(Theme::hover())).on_click(self.on(Cmd::OpenImported(id), cx)))
                });
                ui::dialog(720.).max_w(relative(0.92)).h(px(620.)).child(ui::dialog_title(t("Zotero 导入")))
                    .child(ui::progress_overview(run.rows.len(), run.total, if run.running { t("正在导入") } else if !run.error.is_empty() { t("导入失败") } else { t("导入完成") }, run.running, &ui::zotero_segments(run))
                        .when(run.running && !run.current.is_empty(), |d| d.child(div().p(px(10.)).rounded(px(8.)).bg(theme::accent_of("blue").soft()).line_height(px(20.)).child(run.current.clone()))))
                    .child(body().border_t_1().border_color(Theme::line())
                        .when(!run.running, |d| d.child(div().line_height(px(21.)).text_color(Theme::text_secondary()).child(run.summary())))
                        .when(!run.error.is_empty(), |d| d.child(ui::error_line(&run.error)))
                        .when(!run.running && run.total == 0 && run.error.is_empty(), |d| d.child(div().child(t("没有找到可导入的文献。回收站、订阅源、独立笔记和独立附件不导入。"))))
                        .when(run.other_attachments > 0, |d| d.child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child(tf!("另有 {} 个非 PDF 附件未迁移。", "{} non-PDF attachments were not migrated.", run.other_attachments))))
                        .when(run.running, |d| d.child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child(t("可以收起此窗口。返回首页或切换文献库会停止后续条目，已导入的保留；重新导入会跳过重复项。"))))
                        .children(rows))
                    .child(div().flex().flex_none().items_center().gap(px(8.)).px(px(20.)).py(px(14.)).border_t_1().border_color(Theme::line()).child(div().flex_1().text_size(px(12.)).text_color(Theme::text_tertiary()).child(if run.running { t("收起后继续在后台导入") } else { t("点击结果可查看文献") })).child(ui::primary_button("zotero-close", if run.running { t("收起") } else { crate::i18n::tc("dialog.close", "关闭") }).on_click(self.on(Cmd::CloseModal, cx))))
            }
            Modal::Confirm { title, message, action, .. } => ui::dialog(440.).child(ui::dialog_title(title.clone())).child(body().child(div().text_color(Theme::text_secondary()).line_height(px(21.)).child(message.clone())).child(error)).child(footer(action, true, self, cx)),

            Modal::ImportReport => {
                let lib = self.lib.as_ref()?;
                let run = lib.import.as_ref()?;
                // What still wants doing comes first; the rest in the order the files were read.
                let mut rows: Vec<(usize, &ImportRow)> = run.rows.iter().enumerate().collect();
                rows.sort_by_key(|(_, row)| !row.open());
                let textless = run.count(|row| !row.textless.is_empty());
                let listed: Vec<AnyElement> = rows
                    .into_iter()
                    .map(|(i, row)| {
                        let (state, color) = match &row.outcome {
                            Imported::Failed(err) => (tf!("没有导入：{err}", "Not imported: {err}"), Theme::danger()),
                            Imported::InTrash => (t("它的文献在回收站里，没有导入").to_string(), Theme::warning()),
                            Imported::Attached => (t("文献库里已有这篇，PDF 附到了原条目").to_string(), Theme::success()),
                            Imported::Added if row.unrecognised => (t("已添加 · 没认出 DOI，可在文献详情里按题目补全").to_string(), Theme::warning()),
                            Imported::Added => (t("已添加").to_string(), Theme::success()),
                        };
                        let title = row.paper_id.as_deref().and_then(|id| lib.paper(id)).map(|paper| paper.title.clone()).filter(|title| !row.unrecognised && !title.is_empty());
                        div()
                            .id(("imported", i))
                            .flex_none()
                            .flex()
                            .flex_col()
                            .gap(px(1.))
                            .px(px(8.))
                            .py(px(6.))
                            .rounded(px(8.))
                            .when_some(row.paper_id.clone(), |d, id| d.cursor_pointer().hover(|s| s.bg(Theme::hover())).on_click(self.on(Cmd::OpenImported(id), cx)))
                            .child(div().line_height(px(20.)).child(title.unwrap_or_else(|| row.name.clone())))
                            .child(div().text_size(px(12.)).line_height(px(17.)).text_color(color).child(state))
                            .when(!row.textless.is_empty(), |d| d.child(div().text_size(px(12.)).line_height(px(17.)).text_color(Theme::warning()).child(tf!("没读出文字，全文暂时不能用于提问：{}", "No text extracted; full text unavailable for chat: {}", row.textless))))
                            .into_any_element()
                    })
                    .collect();
                ui::dialog(720.).max_w(relative(0.92)).h(px(620.))
                    .child(ui::dialog_title(t("PDF 导入")))
                    .child(ui::progress_overview(run.rows.len(), run.total, if run.running() { t("正在导入") } else { t("导入完成") }, run.running(), &ui::import_segments(run))
                        .when(!run.working.is_empty(), |d| d.child(div().flex().flex_col().gap(px(6.)).p(px(10.)).rounded(px(8.)).bg(theme::accent_of("blue").soft())
                            .child(div().text_size(px(12.)).text_color(theme::accent_of("blue").strong()).child(t("正在读取")))
                            .children(run.working.iter().map(|name| div().line_height(px(19.)).child(name.clone()))))))
                    .child(body().gap(px(4.)).border_t_1().border_color(Theme::line())
                        .child(ui::caption(tf!("处理结果 · {} 个 PDF", "Results · {} PDFs", run.rows.len())))
                        .when(!run.rows.is_empty(), |d| d.child(div().text_size(px(12.)).text_color(Theme::text_secondary()).child(run.summary())))
                        .when(run.rows.is_empty(), |d| d.child(div().py(px(24.)).text_color(Theme::text_tertiary()).child(t("正在读取第一批文件，结果会实时出现在这里。"))))
                        .children(listed))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(px(8.))
                            .px(px(20.))
                            .py(px(14.))
                            .when(textless > 0, |d| d.child(ui::secondary_button("import-textless", tf!("在列表里看没读出文字的 {textless} 篇", "Show {textless} papers without readable text")).on_click(self.on(Cmd::ShowTextless, cx))))
                            .child(div().flex_1())
                            .when(!run.running(), |d| d.child(ui::ghost_button("import-clear", None, Some(t("清除记录").into())).on_click(self.on(Cmd::CloseImport, cx))))
                            .child(ui::primary_button("modal-cancel", if run.running() { t("收起") } else { crate::i18n::tc("dialog.close", "关闭") }).on_click(self.on(Cmd::CloseModal, cx))),
                    )
            }

            Modal::FetchReport => {
                let lib = self.lib.as_ref()?;
                let run = lib.fetching.as_ref()?;
                // What was not had comes first: that is where something is left to do.
                let mut rows: Vec<(usize, &crate::app::FetchRow)> = run.rows.iter().enumerate().collect();
                rows.sort_by_key(|(_, row)| match row.outcome.as_str() {
                    "failed" => 0,
                    "not_found" => 1,
                    _ => 2,
                });
                let listed: Vec<AnyElement> = rows
                    .into_iter()
                    .map(|(i, row)| {
                        let (state, color) = match row.outcome.as_str() {
                            "saved" if row.textless => (t("已保存 PDF，但没读出文字"), Theme::warning()),
                            "saved" => (t("已保存 PDF"), Theme::success()),
                            "not_found" => (t("未找到可下载地址"), Theme::warning()),
                            _ => (t("下载失败"), Theme::danger()),
                        };
                        div()
                            .id(("fetched", i))
                            .flex()
                            .flex_col()
                            .flex_none()
                            .gap(px(4.))
                            .px(px(10.))
                            .py(px(10.))
                            .rounded(px(8.))
                            .cursor_pointer()
                            .hover(|s| s.bg(Theme::hover()))
                            .on_click(self.on(Cmd::OpenImported(row.paper_id.clone()), cx))
                            .child(div().line_height(px(20.)).child(row.title.clone()))
                            .child(div().flex().items_center().gap(px(5.)).text_size(px(12.)).line_height(px(17.)).text_color(color)
                                .child(ui::icon(if row.outcome == "saved" { "check" } else { "info" }, 12., color)).child(state))
                            .when(!row.detail.is_empty(), |d| d.child(div().text_size(px(12.)).line_height(px(17.)).text_color(Theme::text_tertiary()).child(row.detail.clone())))
                            .into_any_element()
                    })
                    .collect();
                let blue = theme::accent_of("blue").strong();
                let state_color = if run.running() { blue } else { Theme::text_secondary() };
                ui::dialog(720.).max_w(relative(0.92)).h(px(620.))
                    .child(div().flex().flex_none().items_center()
                        .child(ui::dialog_title(t("全文查找")).flex_1())
                        .child(ui::ghost_button("fetch-dismiss", Some("x"), None).mt(px(10.)).mr(px(16.)).on_click(self.on(Cmd::CloseModal, cx))))
                    .child(div().flex().flex_none().flex_col().gap(px(12.)).px(px(20.)).py(px(14.))
                        .child(div().flex().items_center().gap(px(8.))
                            .child(div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).child(format!("{} / {}", run.rows.len(), run.total)))
                            .child(div().text_color(Theme::text_tertiary()).child(t("篇已处理")))
                            .child(div().flex_1())
                            .when(run.running(), |d| d.child(ui::spinner("fetch-modal-working", 13., state_color)))
                            .child(div().text_color(state_color).child(run.status())))
                        .child(ui::fetch_progress(run).h(px(9.)))
                        .child(div().flex().flex_wrap().gap(px(14.)).children(ui::fetch_segments(run).into_iter().map(|(count, label, color)| {
                            div().flex().items_center().gap(px(5.)).text_size(px(12.))
                                .child(div().flex_none().size(px(7.)).rounded_full().bg(color))
                                .child(div().text_color(Theme::text_secondary()).child(format!("{label} {count}")))
                        })))
                        .when(!run.working.is_empty(), |d| d.child(div().flex().flex_col().gap(px(6.)).p(px(10.)).rounded(px(8.)).bg(theme::accent_of("blue").soft())
                            .child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(blue).child(if run.stopped { t("正在结束当前请求") } else { t("正在处理") }))
                            .children(run.working.iter().map(|id| {
                                let title = lib.paper(id).map(|paper| paper.title.clone()).unwrap_or_else(|| id.clone());
                                div().line_height(px(19.)).child(title)
                            }))))
                        .child(div().text_size(px(12.)).line_height(px(18.)).text_color(Theme::text_tertiary()).child(t("未找到地址：本次未检索到可下载地址；下载失败：找到了地址，但未能保存 PDF。"))))
                    .child(body().gap(px(4.)).border_t_1().border_color(Theme::line())
                        .child(ui::caption(tf!("处理结果 · {} 篇", "Results · {} papers", run.rows.len())))
                        .when(run.rows.is_empty(), |d| d.child(div().py(px(24.)).text_color(Theme::text_tertiary()).child(if run.running() { t("正在查找第一批文献，结果会实时出现在这里。") } else { t("本次没有已处理的文献。") })))
                        .children(listed))
                    .child(div().flex().flex_none().items_center().gap(px(8.)).px(px(20.)).py(px(14.)).border_t_1().border_color(Theme::line())
                        .child(div().flex_1().min_w_0().text_size(px(12.)).text_color(Theme::text_tertiary()).child(if run.running() && !run.stopped { t("收起后继续在后台查找") } else { t("点击结果可查看文献") }))
                        .when(run.running() && !run.stopped, |d| d.child(ui::secondary_button("fetch-stop", t("停止查找")).text_color(Theme::danger()).on_click(self.on(Cmd::StopFetch, cx))))
                        .when(!run.running(), |d| d.child(ui::ghost_button("fetch-clear", None, Some(t("清除记录").into())).on_click(self.on(Cmd::CloseFetch, cx))))
                        .child(ui::primary_button("modal-cancel", if run.running() { t("收起") } else { crate::i18n::tc("dialog.close", "关闭") }).on_click(self.on(Cmd::CloseModal, cx))))
            }

            Modal::Tag { tag, name } => {
                let count = self.lib.as_ref().map_or(0, |lib| lib.papers.iter().filter(|p| p.tags.contains(tag)).count());
                ui::dialog(420.)
                    .child(ui::dialog_title(t("重命名标签")))
                    .child(body().child(div().text_size(px(12.)).line_height(px(18.)).text_color(Theme::text_tertiary()).child(tf!("「{tag}」在 {count} 篇文献上，都会改成新的名字。改成一个已有的标签，两个就合成一个。", "“{tag}” is used on {count} papers. All will be renamed. Choosing an existing tag merges them."))).child(ui::field(name, window, cx)).child(error))
                    .child(footer(t("改名"), false, self, cx))
            }

            Modal::TagPapers { ids, name } => ui::dialog(420.)
                .child(ui::dialog_title(tf!("为 {} 篇文献添加标签", "Add tags to {} papers", ids.len())))
                .child(body().child(div().text_size(px(12.)).line_height(px(18.)).text_color(Theme::text_tertiary()).child(t("多个标签用逗号或分号隔开。已有的标签不动，只添加还没有的。"))).child(ui::field(name, window, cx)).child(error))
                .child(footer(t("添加"), false, self, cx)),

            Modal::Note { paper_id, text } => {
                let title = self.lib.as_ref().and_then(|lib| lib.paper(paper_id)).map(|paper| paper.title.clone()).unwrap_or_default();
                ui::dialog(620.)
                    .child(ui::dialog_title(t("存为笔记")))
                    .child(
                        body()
                            .child(div().text_size(px(12.)).line_height(px(18.)).text_color(Theme::text_tertiary()).child(tf!("会加到「{title}」的「我的笔记」末尾，已有的笔记不动。笔记会随文献记录一起提供给以后的提问，所以先看一遍，把不对的地方改掉。", "Append to “My notes” for “{title}”. Notes are included in future questions, so review and correct the content first.")))
                            .child(ui::field(text, window, cx))
                            .child(error),
                    )
                    .child(footer(t("存入笔记"), false, self, cx))
            }

            Modal::Library { editing, name, topic, color } => {
                let swatches: Vec<Stateful<Div>> = theme::ACCENTS.iter().map(|accent| {
                    let chosen = accent.key == color;
                    div()
                        .id(accent.key)
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(28.))
                        .rounded_full()
                        .cursor_pointer()
                        .border_2()
                        .border_color(if chosen { accent.strong() } else { gpui::transparent_black() })
                        .on_click(self.on(Cmd::PickColor(accent.key.to_string()), cx))
                        .child(div().size(px(18.)).rounded_full().bg(accent.strong()))
                }).collect();
                ui::dialog(480.)
                    .child(ui::dialog_title(if editing.is_some() { t("编辑文献库") } else { t("新建文献库") }))
                    .child(
                        body()
                            .child(ui::form_row(t("名称"), ui::field(name, window, cx)))
                            .child(ui::form_row(t("主题"), ui::field(topic, window, cx)))
                            .child(ui::form_row(t("标识色"), div().flex().gap(px(6.)).children(swatches)))
                            .child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child(t("每个文献库是独立的文件夹：自己的数据库、PDF 和自定义词条，可以单独备份或拷走。")))
                            .child(error),
                    )
                    .child(footer(if editing.is_some() { t("保存") } else { t("创建") }, false, self, cx))
            }

            Modal::AddById { input, busy } => ui::dialog(480.)
                .child(ui::dialog_title(t("通过 DOI / PMID 添加")))
                .child(
                    body()
                        .child(ui::form_row(t("DOI 或 PMID"), ui::field(input, window, cx)))
                        .child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child(t("题目、作者、期刊、年份和摘要会从 Crossref 与 Europe PMC 查询后填入。查询会把这个编号发送给这两个服务。")))
                        .child(error),
                )
                .child(footer(if *busy { t("正在查询…") } else { t("查询并添加") }, false, self, cx)),

            Modal::Complete(form) => {
                let lib = self.lib.as_ref()?;
                let paper = lib.paper(&form.paper_id)?;
                let small = |text: String, color: Hsla| div().text_size(px(12.)).line_height(px(18.)).text_color(color).child(text);
                // What a record is, in two lines.
                let summary = |record: &wisp_core::model::Paper| {
                    let mut line = vec![];
                    if !record.authors.is_empty() { line.push(record.short_authors()) }
                    if let Some(year) = record.year { line.push(year.to_string()) }
                    if !record.journal.is_empty() { line.push(record.journal.clone()) }
                    if record.publication_status != "unknown" { line.push(label(&PUBLICATION_STATUSES, &record.publication_status).to_string()) }
                    let numbers = [("DOI", &record.doi), ("PMID", &record.pmid)].into_iter().filter(|(_, value)| !value.is_empty()).map(|(name, value)| format!("{name} {value}")).collect::<Vec<_>>().join(" · ");
                    div().flex().flex_col().gap(px(2.)).min_w_0().child(div().font_weight(FontWeight::MEDIUM).line_height(px(19.)).child(record.title.clone())).child(small(line.join(" · "), Theme::text_secondary())).when(!numbers.is_empty(), |d| d.child(small(numbers, Theme::text_tertiary())))
                };
                let held_line = |held: &Option<(String, String)>| held.as_ref().map(|(_, title)| small(tf!("文献库里已经有这条记录：「{title}」。它不能再写进这一篇；多半是同一篇文献导入了两次。", "A record already exists: “{title}”. It cannot replace this paper; these may be duplicate imports."), Theme::danger()));
                match form.chosen.and_then(|at| Some((at, form.found.as_ref()?.candidates.get(at)?))) {
                    // The record looked at, field by field.
                    Some((at, candidate)) => {
                        let record = &candidate.paper;
                        let link = record.link();
                        let (ok, _) = theme::status_colors("included");
                        let rows: Vec<AnyElement> = complete::offered(paper, record)
                            .into_iter()
                            .map(|key| {
                                let name = complete::FIELDS.iter().find(|(field, _)| *field == key).map_or(key, |(_, name)| t(name));
                                let taken = form.fields.contains(&key);
                                let brief = |text: String| if text.chars().count() > 220 { format!("{}…", text.chars().take(220).collect::<String>()) } else { text };
                                let now = complete::shown(paper, key);
                                let now_line = if now.is_empty() { t("现在：空着").to_string() } else if key == "title" && form.placeholder { tf!("现在：{now}（文件名）", "Current: {now} (file name)") } else { tf!("现在：{}", "Current: {}", brief(now)) };
                                div()
                                    .id(SharedString::from(format!("complete-field-{key}")))
                                    .flex()
                                    .items_start()
                                    .gap(px(10.))
                                    .px(px(10.))
                                    .py(px(8.))
                                    .rounded(px(8.))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(Theme::hover()))
                                    .on_click(self.on(Cmd::CompleteField(key), cx))
                                    .child(div().flex().flex_none().items_center().justify_center().size(px(16.)).mt(px(2.)).rounded(px(4.)).border_1().when(taken, |d| d.bg(Theme::accent()).border_color(Theme::accent()).child(ui::icon("check", 12., Theme::on_accent()))).when(!taken, |d| d.border_color(Theme::line_strong())))
                                    .child(div().flex_none().w(px(64.)).text_color(Theme::text_secondary()).child(name))
                                    .child(div().flex().flex_col().flex_1().min_w_0().gap(px(2.)).child(div().line_height(px(19.)).child(brief(complete::shown(record, key)))).child(small(now_line, Theme::text_tertiary())))
                                    .into_any_element()
                            })
                            .collect();
                        let nothing = rows.is_empty();
                        ui::dialog(640.)
                            .child(ui::dialog_title(t("按题目补全 · 核对这条记录")))
                            .child(
                                body()
                                    .child(div().flex().items_start().gap(px(10.)).p(px(12.)).rounded(px(10.)).bg(Theme::bg_sunken()).child(div().flex().flex_col().flex_1().min_w_0().gap(px(4.)).child(summary(record)).child(small(tf!("来自 {}", "From {}", candidate.sources.join("、")), Theme::text_tertiary()))).when(!link.is_empty(), |d| d.child(ui::secondary_button("complete-open", t("打开网页")).h(px(28.)).on_click(self.on(Cmd::OpenUrl(link.clone()), cx)))))
                                    .when(!candidate.same.is_empty(), |d| d.child(small(tf!("与所查一致：{}", "Matches query: {}", candidate.same.join("，")), ok)))
                                    .when(!candidate.differs.is_empty(), |d| d.child(small(tf!("不一致：{}", "Differs: {}", candidate.differs.join("，")), Theme::warning())))
                                    .when(candidate.similarity < complete::ALIKE_ENOUGH, |d| d.child(small(t("这条记录的题目和所查的不一样，很可能是另一篇文献，所以没有预先勾选任何字段。").into(), Theme::warning())))
                                    .when(candidate.partial_authors, |d| d.child(small(t("这个来源只给了部分作者，名单不完整：写入后请对照原文补齐。").into(), Theme::warning())))
                                    .children(held_line(form.held.get(at).unwrap_or(&None)))
                                    .child(div().flex().flex_col().gap(px(2.)).children(rows))
                                    .when(nothing, |d| d.child(small(t("这条记录没有比现有内容多出什么。").into(), Theme::text_tertiary())))
                                    .child(small(t("默认只勾选现在空着的字段，以及只是文件名的题目；已有的内容，勾选后才会被替换。题目相近不等于同一篇：拿不准时打开网页对照，或返回看别的候选。").into(), Theme::text_tertiary()))
                                    .child(error),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .gap(px(8.))
                                    .px(px(20.))
                                    .py(px(14.))
                                    .child(ui::ghost_button("complete-back", Some("arrow-left"), Some(t("返回候选").into())).h(px(32.)).on_click(self.on(Cmd::CompleteChoose(None), cx)))
                                    .child(div().flex_1())
                                    .child(ui::secondary_button("modal-cancel", t("取消")).on_click(self.on(Cmd::CloseModal, cx)))
                                    .child(ui::primary_button("modal-ok", tf!("写入 {} 个字段", "Save {} fields", form.fields.len())).on_click(self.on(Cmd::SubmitModal, cx))),
                            )
                    }
                    // What to look for, and the records found.
                    None => {
                        let about_title = if form.guessed {
                            t("题目读自 PDF 首页，可能不全或有错：查找前请核对。")
                        } else if form.placeholder {
                            t("这篇文献现在的题目只是文件名，不会拿它去查：请写上或粘贴题目。")
                        } else {
                            t("可以修改用来查找的题目；文献本身的题目不会因此改变。")
                        };
                        let asked: Vec<AnyElement> = form.found.iter().flat_map(|found| found.asked.iter()).map(|asked| {
                            let (text, color) = if !asked.error.is_empty() { (tf!("{}：没有查成（{}）", "{}: search failed ({})", asked.source, asked.error), Theme::warning()) } else if asked.found == 0 { (tf!("{}：没有查到", "{}: no results", asked.source), Theme::text_tertiary()) } else { (tf!("{}：{} 条", "{}: {} results", asked.source, asked.found), Theme::text_tertiary()) };
                            small(text, color).into_any_element()
                        }).collect();
                        let records: Vec<AnyElement> = form.found.iter().flat_map(|found| found.candidates.iter().enumerate()).map(|(i, candidate)| {
                            let (ok, ok_fill) = theme::status_colors("included");
                            let (warn, warn_fill) = theme::status_colors("uncertain");
                            div()
                                .id(("complete-candidate", i))
                                .flex()
                                .flex_col()
                                .gap(px(6.))
                                .p(px(12.))
                                .rounded(px(10.))
                                .border_1()
                                .border_color(Theme::line())
                                .cursor_pointer()
                                .hover(|s| s.bg(Theme::hover()))
                                .on_click(self.on(Cmd::CompleteChoose(Some(i)), cx))
                                .child(summary(&candidate.paper))
                                .child(div().flex().flex_wrap().items_center().gap(px(4.)).children(candidate.same.iter().map(|note| ui::chip(note.clone(), ok, ok_fill))).children(candidate.differs.iter().map(|note| ui::chip(note.clone(), warn, warn_fill))).child(div().pl(px(4.)).text_size(px(12.)).text_color(Theme::text_tertiary()).child(candidate.sources.join("、"))))
                                .children(held_line(form.held.get(i).unwrap_or(&None)))
                                .into_any_element()
                        }).collect();
                        let searched = form.found.is_some();
                        let none = searched && records.is_empty();
                        ui::dialog(640.)
                            .child(ui::dialog_title(t("按题目补全")))
                            .child(
                                body()
                                    .child(ui::form_row(t("题目"), ui::field(&form.title, window, cx)).child(small(about_title.into(), if form.guessed { Theme::warning() } else { Theme::text_tertiary() })))
                                    .child(div().flex().gap(px(12.)).child(div().flex_1().min_w_0().child(ui::form_row(t("作者"), ui::field(&form.author, window, cx)))).child(div().w(px(160.)).child(ui::form_row(t("年份"), ui::field(&form.year, window, cx)))))
                                    .child(small(t("到 Crossref、Europe PMC 和 arXiv 按题目查找，每个来源取最相近的几条。发出去的只有题目、作者和年份，不上传 PDF。").into(), Theme::text_tertiary()))
                                    .when(searched, |d| d.child(div().flex().flex_col().gap(px(8.)).pt(px(4.)).child(div().flex().flex_wrap().gap(px(12.)).children(asked)).when(!none, |d| d.child(small(t("点一条记录核对它的字段。排在前面只说明题目更像，是不是这一篇由你判断；预印本和正式发表的版本是两条记录。").into(), Theme::text_secondary()))).children(records)))
                                    .when(none, |d| d.child(small(t("没有找到相近的记录。改一改题目再查，或者关掉这里，用「编辑信息与词条」手动填写：一篇文献可以一直没有 DOI。").into(), Theme::text_secondary())))
                                    .child(error),
                            )
                            .child(footer(if form.busy { t("正在查找…") } else if searched { t("重新查找") } else { t("查找") }, false, self, cx))
                    }
                }
            }

            Modal::Paper(form) => {
                let lib = self.lib.as_ref()?;
                let pair = |a: AnyElement, b: AnyElement| div().flex().gap(px(12.)).child(div().flex_1().min_w_0().child(a)).child(div().flex_1().min_w_0().child(b));
                let custom: Vec<AnyElement> = lib
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(i, field)| match field.kind {
                        FieldKind::Select => {
                            let current = form.selects.get(&field.id);
                            ui::form_row(
                                field.name.clone(),
                                div().flex().flex_wrap().gap(px(6.)).children(field.options.iter().enumerate().map(|(j, option)| option_chip(SharedString::from(format!("option-{i}-{j}")), option.clone(), current == Some(option)).on_click(self.on(Cmd::PickOption(field.id.clone(), option.clone()), cx)))),
                            )
                            .into_any_element()
                        }
                        _ => match form.fields.iter().find(|(id, _)| id == &field.id) {
                            Some((_, input)) => {
                                let hint = if field.kind == FieldKind::Tags && !field.options.is_empty() { tf!("{}（常用：{}）", "{} (common: {})", field.name, field.options.join("、")) } else { field.name.clone() };
                                ui::form_row(hint, ui::field(input, window, cx)).into_any_element()
                            }
                            None => div().into_any_element(),
                        },
                    })
                    .collect();
                ui::dialog(620.)
                    .child(ui::dialog_title(if form.id.is_some() { t("编辑文献") } else { t("手动录入文献") }))
                    .child(
                        body()
                            .child(ui::form_row(t("题目"), ui::field(&form.title, window, cx)))
                            .child(ui::form_row(t("作者"), ui::field(&form.authors, window, cx)))
                            .child(pair(ui::form_row(t("期刊 / 会议"), ui::field(&form.journal, window, cx)).into_any_element(), ui::form_row(t("年份"), ui::field(&form.year, window, cx)).into_any_element()))
                            .child(pair(ui::form_row("DOI", ui::field(&form.doi, window, cx)).into_any_element(), ui::form_row("PMID", ui::field(&form.pmid, window, cx)).into_any_element()))
                            .child(ui::form_row(t("网址"), ui::field(&form.url, window, cx)))
                            .child(ui::form_row(t("标签"), ui::field(&form.tags, window, cx)))
                            .when(!custom.is_empty(), |d| d.child(div().pt(px(6.)).border_t_1().border_color(Theme::line()).child(ui::caption(t("自定义词条")))).children(custom))
                            .child(ui::form_row(t("摘要"), ui::field(&form.abstract_text, window, cx)))
                            .child(error),
                    )
                    .child(footer(t("保存"), false, self, cx))
            }

            Modal::Search { editing, kind, name, query, max } => ui::dialog(560.)
                .child(ui::dialog_title(if editing.is_some() { t("修改检索式") } else { t("新建检索式") }))
                .child(
                    body()
                        .child(ui::form_row(t("来源"), div().flex().flex_wrap().gap(px(6.)).children(sources::KINDS.iter().map(|(key, label)| option_chip(*key, t(label), key == kind).on_click(self.on(Cmd::PickSource(key.to_string()), cx))))))
                        .child(ui::form_row(t("检索式"), ui::field(query, window, cx)))
                        // Each database reads a query its own way.
                        .child(div().text_size(px(12.)).line_height(px(18.)).text_color(Theme::text_tertiary()).child(t(sources::syntax(kind))))
                        .child(ui::form_row(t("名称"), ui::field(name, window, cx)))
                        .child(ui::form_row(t("每次取回"), div().flex().items_center().gap(px(8.)).child(ui::field(max, window, cx).w(px(90.))).child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child(tf!("篇最新的记录（最多 {}）", "latest records (maximum {})", sources::MAX_RESULTS)))))
                        .child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child(t("保存后先「试检索」看看命中的是不是想要的文献，再「运行」导入。检索式会发送给所选的数据库。")))
                        .child(error),
                )
                .child(footer(t("保存"), false, self, cx)),

            Modal::Fields { editing, name, options, kind, in_table } => {
                let lib = self.lib.as_ref()?;
                let count = lib.fields.len();
                let rows: Vec<AnyElement> = lib
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(i, field)| {
                        let current = editing.as_ref() == Some(&field.id);
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .h(px(38.))
                            .px(px(8.))
                            .rounded(px(8.))
                            .when(current, |d| d.bg(Theme::accent_soft()))
                            .child(div().flex_1().min_w_0().truncate().child(field.name.clone()))
                            .child(ui::chip(t(field.kind.label()), Theme::text_secondary(), Theme::bg_sunken()))
                            .child(option_chip(("field-table", i), t("表格列"), field.in_table).h(px(22.)).px(px(8.)).on_click(self.on(Cmd::ToggleFieldInTable(field.id.clone()), cx)))
                            .child(ui::ghost_button(("field-up", i), Some("chevron-up"), None).when(i == 0, |d| d.opacity(0.3)).on_click(self.on(Cmd::MoveField(field.id.clone(), -1), cx)))
                            .child(ui::ghost_button(("field-down", i), Some("chevron-down"), None).when(i + 1 == count, |d| d.opacity(0.3)).on_click(self.on(Cmd::MoveField(field.id.clone(), 1), cx)))
                            .child(ui::ghost_button(("field-edit", i), Some("pen-line"), None).on_click(self.on(Cmd::EditField(field.id.clone()), cx)))
                            .child(ui::ghost_button(("field-delete", i), Some("bin"), None).on_click(self.on(Cmd::DeleteField(field.id.clone()), cx)))
                            .into_any_element()
                    })
                    .collect();
                ui::dialog(560.)
                    .child(ui::dialog_title(t("自定义词条")))
                    .child(
                        body()
                            .child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child(t("词条是这个文献库给每篇文献加的字段，可以填写、筛选，也可以作为表格的一列。它们只属于当前文献库。删除词条会同时删除各文献里它的值。")))
                            .child(if rows.is_empty() {
                                div().flex().items_center().gap(px(10.)).p(px(12.)).rounded(px(8.)).bg(Theme::bg_sunken()).child(div().flex_1().text_color(Theme::text_secondary()).child(t("还没有词条。可以从一组预设开始，再按自己的主题修改。"))).child(ui::secondary_button("field-preset", t("添加单细胞文献预设")).on_click(self.on(Cmd::AddPreset, cx)))
                            } else {
                                div().flex().flex_col().gap(px(2.)).children(rows)
                            })
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(10.))
                                    .p(px(12.))
                                    .rounded(px(10.))
                                    .border_1()
                                    .border_color(Theme::line())
                                    .child(ui::caption(if editing.is_some() { t("修改词条") } else { t("新增词条") }))
                                    .child(ui::field(name, window, cx))
                                    .child(div().flex().items_center().gap(px(6.)).children(FieldKind::ALL.iter().map(|k| option_chip(k.as_str(), t(k.label()), k == kind).on_click(self.on(Cmd::PickKind(*k), cx)))).child(div().flex_1()).child(option_chip("new-in-table", t("显示为表格列"), *in_table).on_click(self.on(Cmd::ToggleInTable, cx))))
                                    .when(*kind != FieldKind::Text, |d| d.child(ui::field(options, window, cx)))
                                    .child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child(match kind {
                                        FieldKind::Text => t("文本：一行自由文字，例如“实验室”“备注编号”。"),
                                        FieldKind::Tags => t("多值：一篇文献可以有多个值，例如物种、方法；可按值筛选。"),
                                        FieldKind::Select => t("单选：从固定选项里选一个，例如“证据等级：高 / 中 / 低”。"),
                                    }))
                                    .child(error),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .justify_end()
                            .gap(px(8.))
                            .px(px(20.))
                            .py(px(14.))
                            .child(ui::secondary_button("modal-cancel", t("完成")).on_click(self.on(Cmd::CloseModal, cx)))
                            .child(ui::primary_button("modal-ok", if editing.is_some() { t("保存修改") } else { t("添加词条") }).on_click(self.on(Cmd::SubmitModal, cx))),
                    )
            }
        };
        // The top strip still drags the window while a dialog is open.
        Some(div().absolute().top_0().left_0().size_full().occlude().flex().items_center().justify_center().bg(Theme::scrim()).child(ui::title_strip().absolute().top_0().left_0()).child(card).into_any_element())
    }
}
