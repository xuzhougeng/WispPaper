//! The detail panel of the selected paper: 信息 (record, statuses, custom
//! fields, notes, files), PDF (preview) and 提问 (questions about this paper).

use crate::i18n::{label, t};
use crate::app::{Cmd, Edge, MenuKind, Tab, Translated, Workspace};
use crate::pdf::{Zoom, ZoomTo};
use crate::theme::{self, Theme};
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use wisp_core::model::*;
use wisp_core::store::translation_key;
use wisp_core::{ai, time};

/// The narrowest detail panel that shows a PDF and the paper's conversation side by side.
pub const SPLIT_MIN: f32 = 760.;

impl Workspace {
    pub fn render_detail(&mut self, width: f32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let lib = self.lib.as_ref().unwrap();
        let Some(p) = lib.selected_paper() else { return div().into_any_element() };
        let tab = lib.tab;
        let reading = lib.reading;
        // Asked about with the PDF beside: the panel is what the narrow list leaves of the window (`Workspace::asking`).
        let asking = self.asking(window);
        // The edge at the panel's left is there to drag while something stands at its left.
        let edged = !reading && (!asking || self.ask_list_width(window) > 0.);
        let has_pdf = lib.pdf_file(&p.id).is_some();
        let title = if lib.show_translation && let Some(Translated::Done(text)) = lib.translations.get(&translation_key(&p.title, ai::translation_prompt(&self.settings.translation_prompt))) { text } else { &p.title };
        let tab_button = |id: &'static str, label: &'static str, value: Tab, this: &Self, cx: &mut Context<Self>| {
            let active = tab == value;
            div()
                .id(id)
                .flex()
                .items_center()
                .h(px(34.))
                .px(px(4.))
                .border_b_2()
                .cursor_pointer()
                .border_color(if active { Theme::accent() } else { gpui::transparent_black() })
                .text_color(if active { Theme::text() } else { Theme::text_tertiary() })
                .when(active, |d| d.font_weight(FontWeight::MEDIUM))
                .hover(|s| s.text_color(Theme::text()))
                .on_click(this.on(Cmd::SetTab(value), cx))
                .child(label)
        };
        // Reading mode, and a paper that is asked about, keep one line over the paper: what is read, the three tabs, and the way back.
        let header = if reading || asking {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(16.))
                .px(px(16.))
                .border_b_1()
                .border_color(Theme::line())
                .child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::MEDIUM).child(p.title.clone()))
                .child(tab_button("tab-info", t("信息"), Tab::Info, self, cx))
                .child(tab_button("tab-pdf", if has_pdf { "PDF" } else { t("PDF（无）") }, Tab::Pdf, self, cx))
                .child(tab_button("tab-ask", t("提问"), Tab::Ask, self, cx))
                .when(tab == Tab::Ask && has_pdf && !(lib.split && width >= SPLIT_MIN), |d| d.child(ui::ghost_button("beside-pdf", Some("file-text"), Some(t("对照 PDF").into())).h(px(26.)).text_size(px(12.)).on_click(self.on(Cmd::BesidePdf(true), cx))))
                .child(if reading {
                    ui::ghost_button("reading-leave", Some("x"), Some(tf!("退出阅读 {}", "Leave reading {}", crate::shortcuts::pretty("escape")).into())).h(px(26.)).text_size(px(12.)).on_click(self.on(Cmd::ToggleReading, cx))
                } else {
                    ui::ghost_button("ask-leave", Some("x"), Some(tf!("退出提问 {}", "Leave chat {}", crate::shortcuts::pretty("escape")).into())).h(px(26.)).text_size(px(12.)).on_click(self.on(Cmd::LeaveAsk, cx))
                })
        } else { div()
            .flex()
            .flex_col()
            .flex_none()
            .px(px(16.))
            .pt(px(12.))
            .border_b_1()
            .border_color(Theme::line())
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(6.))
                    .child(self.selectable("detail-title", title.clone(), cx).flex_1().min_w_0().line_clamp(3).text_size(px(15.)).line_height(px(22.)).font_weight(FontWeight::SEMIBOLD))
                    .child(ui::ghost_button("detail-wide", Some(if lib.wide { "panel-left-open" } else { "expand" }), None).on_click(self.on(Cmd::ToggleWide, cx)))
                    .child(ui::ghost_button("detail-close", Some("x"), None).on_click(self.on(Cmd::CloseDetail, cx))),
            )
            .child(
                div()
                    .flex()
                    .gap(px(16.))
                    .pt(px(6.))
                    .child(tab_button("tab-info", t("信息"), Tab::Info, self, cx))
                    .child(tab_button("tab-pdf", if has_pdf { "PDF" } else { t("PDF（无）") }, Tab::Pdf, self, cx))
                    .child(tab_button("tab-ask", t("提问"), Tab::Ask, self, cx))
                    // Asking with the PDF put away, or in a panel too narrow for both: one click brings it beside the conversation.
                    .when(tab == Tab::Ask && has_pdf && !(lib.split && width >= SPLIT_MIN), |d| d.child(div().flex_1()).child(ui::ghost_button("beside-pdf", Some("file-text"), Some(t("对照 PDF").into())).h(px(26.)).text_size(px(12.)).on_click(self.on(Cmd::BesidePdf(true), cx)))),
            ) };
        // Asking about a paper: the conversation, and the paper's PDF to its right where the panel has room for both.
        // A paper without one keeps the layout it is asked about in, with the ways to get its PDF at the right.
        let split = lib.split && tab == Tab::Ask && (has_pdf || asking) && width >= SPLIT_MIN;
        let body = match tab {
            Tab::Ask if split && has_pdf && self.settings.floating.on => self.render_floating(width, window, cx),
            Tab::Ask if split => {
                let chat_width = self.chat_width(width);
                let chat = self.render_chat(true, window, cx);
                let pdf = self.render_pdf_tab(width - chat_width, cx);
                // The edge between the two: drag it to give either more room, click it twice for the widths they came with.
                let held = self.resizing == Some(Edge::Split);
                let between = div()
                    .id("split-edge")
                    .group("split-edge")
                    .absolute()
                    .top_0()
                    .left(px(chat_width - 5.))
                    .w(px(9.))
                    .h_full()
                    .flex()
                    .justify_center()
                    .cursor_col_resize()
                    .occlude()
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &MouseDownEvent, _, cx| {
                        if event.click_count >= 2 {
                            this.resizing = None;
                            this.settings.chat_width = 0.;
                            this.save_settings(cx);
                        } else {
                            this.resizing = Some(Edge::Split);
                        }
                        cx.notify();
                    }))
                    .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.end_drag(cx)))
                    .child(div().w(px(2.)).h_full().when(held, |d| d.bg(Theme::accent())).when(!held, |d| d.group_hover("split-edge", |s| s.bg(Theme::accent().opacity(0.5)))));
                div().relative().flex().size_full().child(div().flex_none().w(px(chat_width)).h_full().border_r_1().border_color(Theme::line()).child(chat)).child(div().flex_1().min_w_0().h_full().child(pdf)).child(between).into_any_element()
            }
            Tab::Info => self.render_info(window, cx),
            Tab::Pdf => self.render_pdf_tab(width, cx),
            Tab::Ask => self.render_chat(true, window, cx),
        };
        // The panel's left edge: drag it to give the list or the paper more room, click it twice for the width it came with.
        let resizing = self.resizing == Some(Edge::Detail);
        let edge = div()
            .id("detail-edge")
            .group("detail-edge")
            .absolute()
            .top_0()
            .left(px(-4.))
            .w(px(9.))
            .h_full()
            .flex()
            .justify_center()
            .cursor_col_resize()
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                if event.click_count >= 2 {
                    this.resizing = None;
                    // Beside a paper that is asked about, the edge is the narrow list's.
                    if asking { this.settings.ask_list_width = 0. } else { this.settings.detail_width = 0. }
                    if let Some(lib) = this.lib.as_mut() { lib.wide = false }
                    this.save_settings(cx);
                } else {
                    this.resizing = Some(Edge::Detail);
                }
                cx.notify();
            }))
            // A tap is over before the layer that follows the pointer is drawn: the edge hears the end of it itself.
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.end_drag(cx)))
            .child(div().w(px(2.)).h_full().when(resizing, |d| d.bg(Theme::accent())).when(!resizing, |d| d.group_hover("detail-edge", |s| s.bg(Theme::accent().opacity(0.5)))));
        // Where the panel is the window (reading mode, or asked about without the list): no edge to drag, no line at its left.
        div().relative().flex().flex_col().flex_none().w(px(width)).h_full().bg(Theme::bg_body()).when(edged, |d| d.border_l_1().border_color(Theme::line())).child(ui::title_strip()).child(header).child(div().flex().flex_col().flex_1().min_h_0().child(body)).when(edged, |d| d.child(edge)).into_any_element()
    }

    /// The translated abstract replaces its source in place. Until it is
    /// ready, keep the source readable alongside progress or a retry.
    fn render_translation(&self, p: &Paper, cx: &mut Context<Self>) -> Option<AnyElement> {
        let lib = self.lib.as_ref().filter(|l| l.show_translation)?;
        let prompt = ai::translation_prompt(&self.settings.translation_prompt);
        let texts: Vec<(String, &Translated)> = [&p.title, &p.abstract_text].into_iter().filter(|text| !text.trim().is_empty()).filter_map(|text| {
            let key = translation_key(text, prompt);
            lib.translations.get(&key).map(|state| (key, state))
        }).collect();
        if texts.is_empty() { return None }
        let abstract_key = translation_key(&p.abstract_text, prompt);
        let abstract_text = match lib.translations.get(&abstract_key) {
            Some(Translated::Done(text)) => text,
            _ => &p.abstract_text,
        };
        let mut card = div().flex().flex_col().gap(px(6.)).child(if abstract_text.is_empty() {
            div().text_color(Theme::text_placeholder()).child(t("没有摘要"))
        } else {
            self.selectable("detail-abstract", abstract_text.clone(), cx).text_color(Theme::text_secondary()).line_height(px(21.))
        });
        if texts.iter().any(|(_, state)| matches!(state, Translated::Busy)) {
            card = card.child(div().flex().items_center().gap(px(6.)).text_size(px(12.)).text_color(Theme::accent()).child(ui::spinner("translating", 13., Theme::accent())).child(t("正在翻译…")));
        }
        if let Some(error) = texts.iter().find_map(|(_, state)| match state {
            Translated::Failed(error) => Some(error.clone()),
            _ => None,
        }) {
            card = card.child(div().flex().items_center().gap(px(8.)).child(div().flex_1().min_w_0().child(ui::error_line(&error))).child(ui::ghost_button("translate-retry", None, Some(t("重试").into())).on_click(self.on(Cmd::RetryTranslation, cx))));
        }
        Some(card.into_any_element())
    }

    fn render_info(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let lib = self.lib.as_ref().unwrap();
        let Some(p) = lib.selected_paper() else { return div().into_any_element() };
        let id = p.id.clone();
        let section = |title: &'static str| div().flex().flex_col().gap(px(6.)).child(ui::caption(title));
        // A status the reader changes with one click.
        let pick = |element_id: &'static str, value: &str, text: &str, kind: MenuKind, this: &Self, cx: &mut Context<Self>| {
            let (color, fill) = theme::status_colors(value);
            div()
                .id(element_id)
                .flex()
                .flex_none()
                .items_center()
                .gap(px(3.))
                .h(px(24.))
                .pl(px(9.))
                .pr(px(6.))
                .rounded(px(12.))
                .bg(fill)
                .text_color(color)
                .text_size(px(12.))
                .cursor_pointer()
                .hover(|s| s.opacity(0.85))
                .on_click(this.on_menu(kind, cx))
                .child(text.to_string())
                .child(ui::icon("chevron-down", 11., color))
        };
        let link = p.link();
        let mut source_line = vec![];
        if !p.journal.is_empty() { source_line.push(p.journal.clone()) }
        if let Some(year) = p.year { source_line.push(year.to_string()) }
        if p.publication_status != "unknown" { source_line.push(label(&PUBLICATION_STATUSES, &p.publication_status).to_string()) }
        let identifiers: Vec<AnyElement> = [("DOI", &p.doi, format!("https://doi.org/{}", p.doi)), ("PMID", &p.pmid, format!("https://pubmed.ncbi.nlm.nih.gov/{}/", p.pmid))]
            .into_iter()
            .filter(|(_, value, _)| !value.is_empty())
            .map(|(name, value, url)| div().flex().gap(px(6.)).text_size(px(12.)).child(div().flex_none().w(px(40.)).text_color(Theme::text_tertiary()).child(name)).child(self.selectable_link(name, value.clone(), url, cx).min_w_0()).into_any_element())
            .collect();
        let field_rows = self.render_field_rows(p, cx);
        let files = lib.paper_files(&id);
        let file_rows: Vec<AnyElement> = files
            .iter()
            .enumerate()
            .map(|(i, file)| {
                let reading = lib.extracting.contains(&file.id);
                // By a model's eyes, when the file gave no text of its own.
                let by_eye = lib.writing.sorting.reading && lib.writing.sorting.working.contains(&file.id);
                let text = lib.store.fulltext_pages(&file.id).ok().flatten();
                let failure = match &text {
                    Some(Err(error)) if !reading && !by_eye => Some(error.clone()),
                    _ => None,
                };
                let note = match &text {
                    _ if reading => t("正在提取文字…").to_string(),
                    _ if by_eye => t("模型正在读页面…").to_string(),
                    Some(Ok(pages)) => {
                        // How much of the file gave text: a page without any is a figure, or a page that could not be read.
                        let with_text = pages.iter().filter(|page| !page.trim().is_empty()).count();
                        let count = if with_text == pages.len() { tf!("{} 页文字可用于提问", "{} pages available for chat", pages.len()) } else { tf!("{} 页中 {with_text} 页有文字，可用于提问", "{with_text} of {} pages contain text for chat", pages.len()) };
                        match lib.writing.structures.get(&file.id).filter(|info| !info.text_by.is_empty()) {
                            Some(info) if info.read_in_part() => tf!("{count}（由 {} 读出，全文 {} 页，还有 {} 页没读）", "{count} (read by {}; {} pages total, {} unread)", info.text_by, info.page_count, info.page_count - info.pages_read),
                            Some(info) => tf!("{count}（由 {} 读出）", "{count} (read by {})", info.text_by),
                            None => count,
                        }
                    }
                    Some(Err(error)) => tf!("没读出文字：{}", "No text extracted: {}", wisp_core::fulltext::failure_brief(error)),
                    None => String::new(),
                };
                // A fetched file says where it is from.
                let note = match lib.origins.get(&file.id) {
                    Some((_, source, version)) => tf!("{note} · 来自 {}", "{note} · From {}", [source.as_str(), t(wisp_core::openaccess::version_label(version))].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" ")),
                    None => note,
                };
                let small = |id: (&'static str, usize), text: &'static str| ui::secondary_button(id, text).h(px(24.)).px(px(8.)).text_size(px(12.));
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .p(px(8.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(Theme::line())
                    .on_mouse_down(MouseButton::Right, self.on_context(MenuKind::File(file.id.clone()), cx))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(if reading || by_eye { ui::spinner(("file-busy", i), 16., Theme::accent()).into_any_element() } else { ui::icon("file-text", 18., Theme::accent()).into_any_element() })
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().truncate().child(file.title.clone()))
                                    .child(div().truncate().text_size(px(12.)).text_color(if failure.is_some() { Theme::warning() } else { Theme::text_tertiary() }).child(format!("{} · {}{}", if file.kind == "fulltext" { t("全文") } else { t("附件") }, file_size(file.size_bytes), if note.is_empty() { String::new() } else { format!(" · {note}") }))),
                            )
                            .child(ui::ghost_button(("file-open", i), Some("link"), None).on_click(self.on(Cmd::OpenFile(file.id.clone()), cx)))
                            .child(ui::ghost_button(("file-remove", i), Some("x"), None).on_click(self.on(Cmd::RemoveFile(file.id.clone()), cx))),
                    )
                    // A file without text: why, what it means for questions, and the two things to try.
                    .when_some(failure, |d, error| {
                        d.child(div().text_size(px(12.)).line_height(px(18.)).text_color(Theme::text_secondary()).child(tf!("{error}在读出文字之前，提问只能用到这篇文献的记录和摘要。", "{error}Until text is extracted, questions use only this paper's record and abstract.")))
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap(px(6.))
                                    .child(small(("file-reread", i), t("重新读取")).on_click(self.on(Cmd::RereadFile(file.id.clone()), cx)))
                                    .when(file.format == "pdf" && wisp_core::fulltext::readable_by_eye(&error), |d| d.child(small(("file-by-eye", i), t("让模型读页面")).on_click(self.on(Cmd::ReadPages(file.id.clone()), cx)))),
                            )
                            .when(lib.writing.sorting.reading && !lib.writing.sorting.error.is_empty(), |d| d.child(ui::error_line(&lib.writing.sorting.error)))
                    })
                    // Pages a model read in part: the rest are read from where it stopped.
                    .when(!by_eye && file.format == "pdf" && lib.writing.structures.get(&file.id).is_some_and(|info| info.read_in_part()), |d| d.child(div().flex().child(small(("file-read-rest", i), t("读完剩下的页面")).on_click(self.on(Cmd::ReadPages(file.id.clone()), cx)))))
                    .into_any_element()
            })
            .collect();
        let has_fields = !lib.fields.is_empty();
        let translating = lib.show_translation;
        let abstract_body = self.render_translation(p, cx).unwrap_or_else(|| {
            if p.abstract_text.is_empty() { div().text_color(Theme::text_placeholder()).child(t("没有摘要")) } else { self.selectable("detail-abstract", p.abstract_text.clone(), cx).text_color(Theme::text_secondary()).line_height(px(21.)) }.into_any_element()
        });
        let abstract_head = div()
            .flex()
            .items_center()
            .justify_between()
            .child(ui::caption(t("摘要")))
            .child(ui::ghost_button("translate", None, Some(if translating { t("显示原文").into() } else { t("显示译文").into() })).h(px(22.)).text_size(px(12.)).when(translating, |d| d.text_color(Theme::accent())).on_click(self.on(Cmd::ToggleTranslation, cx)));
        div()
            .id("detail-info")
            .flex()
            .flex_col()
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&lib.detail_scroll)
            .p(px(16.))
            .gap(px(18.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(if p.authors.is_empty() { div().text_color(Theme::text_secondary()).child(t("作者待补")) } else { self.selectable("detail-authors", p.authors.clone(), cx).text_color(Theme::text_secondary()) })
                    .when(!source_line.is_empty(), |d| d.child(self.selectable("detail-source", source_line.join(" · "), cx).text_size(px(12.)).text_color(Theme::text_tertiary())))
                    .children(identifiers),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.))
                    .child(pick("pick-reading", &p.reading, label(&READINGS, &p.reading), MenuKind::Reading(id.clone()), self, cx))
                    .child(pick("pick-screening", &p.screening, label(&SCREENINGS, &p.screening), MenuKind::Screening(id.clone()), self, cx))
                    .child(ui::status_chip(&p.fulltext, label(&FULLTEXTS, &p.fulltext)))
                    .child(ui::ghost_button("detail-star", Some("star"), Some(if p.starred { t("已收藏").into() } else { t("收藏").into() })).when(p.starred, |d| d.text_color(Theme::star())).on_click(self.on(Cmd::ToggleStar(id.clone()), cx))),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(8.))
                    .child(ui::secondary_button("detail-edit", t("编辑信息与词条")).on_click(self.on(Cmd::EditPaper(id.clone()), cx)))
                    // A record without a number of its own can be looked for by its title.
                    .when(p.doi.is_empty() && p.pmid.is_empty(), |d| d.child(ui::secondary_button("detail-complete", t("按题目补全")).on_click(self.on(Cmd::CompletePaper(id.clone()), cx))))
                    .when(!link.is_empty(), |d| d.child(ui::secondary_button("detail-link", t("打开网页")).on_click(self.on(Cmd::OpenLink(id.clone()), cx))))
                    .child(ui::ghost_button("detail-trash", Some("bin"), Some(t("移到回收站").into())).h(px(32.)).on_click(self.on(Cmd::TrashPaper(id.clone()), cx))),
            )
            .child(section(t("标签")).child(if p.tags.is_empty() {
                div().text_color(Theme::text_placeholder()).child(t("没有标签"))
            } else {
                div().flex().flex_wrap().gap(px(4.)).children(p.tags.iter().map(|t| ui::chip(format!("#{t}"), Theme::accent(), Theme::accent_soft())))
            }))
            .child(section(t("自定义词条")).child(if has_fields {
                div().flex().flex_col().gap(px(8.)).children(field_rows)
            } else {
                div().text_color(Theme::text_placeholder()).child(t("这个文献库还没有自定义词条。在左下角「自定义词条」里添加，例如物种、研究方法、所属课题。"))
            }))
            .child(div().flex().flex_col().gap(px(6.)).child(abstract_head).child(abstract_body))
            .child(section(t("我的笔记")).child(ui::field(&lib.notes, window, cx)))
            .child(section(t("文件")).child(div().flex().flex_col().gap(px(6.)).children(file_rows).child(ui::secondary_button("attach", if files.is_empty() { t("添加 PDF 或附件") } else { t("再添加一个文件") }).on_click(self.on(Cmd::AttachPdf(id.clone()), cx)))))
            .child(div().text_size(px(12.)).text_color(Theme::text_placeholder()).child(tf!("来源：{} · 添加于 {}", "Source: {} · Added {}", p.source, time::date_text(&p.added_at))))
            .into_any_element()
    }

    pub fn render_pdf_tab(&mut self, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let lib = self.lib.as_ref().unwrap();
        let Some(p) = lib.selected_paper() else { return div().into_any_element() };
        let Some(file) = lib.pdf_file(&p.id) else {
            let looking = lib.fetching.as_ref().is_some_and(|run| run.running() && (run.working.contains(&p.id)));
            let last = lib.fetching.as_ref().and_then(|run| run.rows.iter().rev().find(|row| row.paper_id == p.id)).map(|row| row.detail.clone()).filter(|detail| !detail.is_empty());
            return ui::empty_state("file-text", t("这篇文献还没有 PDF"), t("添加后可以在这里预览，全文也会用于提问。也可以替你找开放获取的版本：只取能免费合法获取的，查找时会把 DOI 或 PMID 发给 OpenAlex、Europe PMC 和出版商。"))
                .child(div().flex().flex_wrap().gap(px(8.)).child(ui::primary_button("pdf-attach", t("添加 PDF")).on_click(self.on(Cmd::AttachPdf(p.id.clone()), cx))).child(if looking { ui::secondary_button("pdf-find", t("正在查找…")) } else { ui::secondary_button("pdf-find", t("查找开放全文")).on_click(self.on(Cmd::FindPdfs(Some(p.id.clone())), cx)) }))
                .when_some(last, |d, detail| d.child(div().max_w(px(420.)).text_size(px(12.)).line_height(px(18.)).text_color(Theme::text_tertiary()).child(detail)))
                .size_full()
                .into_any_element();
        };
        let Some(pdf) = lib.pdf.clone() else { return div().into_any_element() };
        let (file_id, file_title) = (file.id.clone(), file.title.clone());
        let beside = lib.split && lib.tab == Tab::Ask;
        let reading = lib.reading;
        // With the library's conversation on show as well, a selection can go to either.
        let library_chat = lib.page == crate::app::Page::Ask;
        pdf.update(cx, |pdf, _| {
            pdf.set_width(width);
            pdf.library_chat = library_chat;
        });
        let (page, count, percent, zoom, cutting) = {
            let view = pdf.read(cx);
            (view.current_page(), view.page_count(), view.percent(), view.zoom(), view.cutting())
        };
        // The size chosen here is the one the next paper opens at.
        if let Some(lib) = self.lib.as_mut() { lib.zoom = zoom }
        let fit = match zoom {
            Zoom::FitWidth => t("适应宽度"),
            Zoom::FitPage => t("适应整页"),
            Zoom::Percent(_) => "",
        };
        let tool = |id: &'static str, icon: &'static str| ui::ghost_button(id, Some(icon), None).h(px(24.)).min_w(px(24.));
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.))
                    .flex_wrap()
                    .min_h(px(36.))
                    .py(px(4.))
                    .px(px(12.))
                    .border_b_1()
                    .border_color(Theme::line())
                    .text_size(px(12.))
                    .text_color(Theme::text_tertiary())
                    .child(div().flex_1().min_w_0().truncate().child(file_title))
                    .when(count > 0, |d| d.child(div().flex_none().child(tf!("{page} / {count} 页", "{page} / {count} pages"))))
                    // Smaller, the size in force (a menu of sizes), larger.
                    .when(count > 0, |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_none()
                                .items_center()
                                .child(tool("pdf-zoom-out", "minus").on_click(self.on(Cmd::PdfZoom(ZoomTo::Out), cx)))
                                .child(ui::ghost_button("pdf-zoom", None, Some(if fit.is_empty() { format!("{}%", percent.round()).into() } else { format!("{fit} · {}%", percent.round()).into() })).h(px(24.)).px(px(6.)).text_size(px(12.)).on_click(self.on_menu(MenuKind::PdfZoom, cx)))
                                .child(tool("pdf-zoom-in", "plus").on_click(self.on(Cmd::PdfZoom(ZoomTo::In), cx))),
                        )
                    })
                    // Cut a piece out of a page (a figure, a table): to keep in the figure library, or as a picture for the next question.
                    .when(count > 0, |d| d.child(ui::ghost_button("pdf-cut", None, Some(if cutting { t("取消截图").into() } else { t("截图").into() })).h(px(24.)).px(px(6.)).text_size(px(12.)).when(cutting, |d| d.text_color(Theme::accent())).on_click(self.on(Cmd::PdfCut, cx))))
                    // The whole window for this paper; in reading mode the way back is at the top.
                    .when(!reading, |d| d.child(ui::ghost_button("pdf-reading", None, Some(t("阅读模式").into())).h(px(24.)).px(px(6.)).text_size(px(12.)).on_click(self.on(Cmd::ToggleReading, cx))))
                    .child(ui::ghost_button("pdf-open", Some("link"), (width >= 700.).then(|| t("用系统应用打开").into())).h(px(24.)).on_click(self.on(Cmd::OpenFile(file_id), cx)))
                    // Beside the conversation: put the PDF away again.
                    .when(beside, |d| d.child(tool("pdf-beside-close", "x").on_click(self.on(Cmd::BesidePdf(false), cx)))),
            )
            .child(div().flex_1().min_h_0().child(pdf))
            .into_any_element()
    }
}
