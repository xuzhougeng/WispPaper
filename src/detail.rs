//! The detail panel of the selected paper: 信息 (record, statuses, custom
//! fields, notes, files), PDF (preview) and 提问 (questions about this paper).

use crate::i18n::{label, t};
use crate::app::{Cmd, Edge, MenuKind, Tab, Talk, Translated, Workspace};
use crate::pdf::{Split, Tool, Zoom, ZoomTo};
use crate::theme::{self, Theme, text_px};
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use wisp_core::model::*;
use wisp_core::store::translation_key;
use wisp_core::{ai, time};
use wisp_core::floating as geo;

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
        // Beside the narrow list the one-line header is the title strip itself, level with the list's way back.
        let in_strip = asking && edged && ui::custom_titlebar();
        let has_pdf = lib.pdf_file(&p.id).is_some();
        let title = if lib.show_translation && let Some(Translated::Done(text)) = lib.translations.get(&translation_key(&p.title, ai::translation_prompt(&self.settings.translation_prompt))) { text } else { &p.title };
        // Asking about a paper: the conversation, and the paper's PDF to its right where the panel has room for both
        // (to its left once the reader swapped them, `Settings::pdf_first`).
        // A paper without one keeps the layout it is asked about in, with the ways to get its PDF in the PDF's place.
        let split = lib.split && tab == Tab::Ask && (has_pdf || asking) && width >= SPLIT_MIN;
        let pdf_first = self.settings.pdf_first;
        // Two panes side by side can change places; a conversation floating over the PDF has no side.
        // The button stays where it is whichever side each pane is on: a second click undoes the first. An id of
        // each order: the tooltip under the pointer is made anew after the click, saying what the next click does.
        let swap = (split && !(has_pdf && self.settings.floating.on)).then(|| {
            ui::ghost_button(if pdf_first { "split-swap-back" } else { "split-swap" }, Some("arrow-left-right"), None).h(text_px(26.)).min_w(text_px(26.)).occlude()
                .tooltip(ui::tip(if pdf_first { t("把 PDF 移回右边") } else { t("把 PDF 移到左边") }))
                .on_click(self.on(Cmd::SwapPanes, cx))
        });
        let tab_button = |id: &'static str, label: &'static str, value: Tab, this: &Self, cx: &mut Context<Self>| {
            let active = tab == value;
            div()
                .id(id)
                .flex()
                .items_center()
                .h_full()
                .px(px(4.))
                .border_b_2()
                .cursor_pointer()
                .occlude()
                .border_color(if active { Theme::accent() } else { gpui::transparent_black() })
                .text_color(if active { Theme::text() } else { Theme::text_tertiary() })
                .when(active, |d| d.font_weight(FontWeight::MEDIUM))
                .hover(|s| s.text_color(Theme::text()))
                .on_click(this.on(Cmd::SetTab(value), cx))
                .child(label)
        };
        // Reading mode, and a paper that is asked about, keep one line over the paper: what is read, the three tabs, and the way back.
        let header = if reading || asking {
            (if in_strip { ui::title_strip() } else { div().flex().flex_none().items_center().h(text_px(36.)) })
                .gap(px(16.))
                .px(px(16.))
                .when(in_strip && cfg!(target_os = "windows"), |d| d.pr(px(3. * ui::CONTROL_W + 8.)))
                .border_b_1()
                .border_color(Theme::line())
                .child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::MEDIUM).child(title.clone()))
                .child(tab_button("tab-info", t("信息"), Tab::Info, self, cx))
                .child(tab_button("tab-pdf", if has_pdf { "PDF" } else { t("PDF（无）") }, Tab::Pdf, self, cx))
                .child(tab_button("tab-ask", t("提问"), Tab::Ask, self, cx))
                .when(tab == Tab::Ask && has_pdf && !(lib.split && width >= SPLIT_MIN), |d| d.child(ui::ghost_button("beside-pdf", Some("file-text"), Some(t("对照 PDF").into())).h(text_px(26.)).text_size(text_px(12.)).occlude().on_click(self.on(Cmd::BesidePdf(true), cx))))
                .when_some(swap, |d, swap| d.child(swap))
                .child(if reading {
                    ui::ghost_button("reading-leave", Some("x"), Some(tf!("退出阅读 {}", "Leave reading {}", crate::shortcuts::pretty("escape")).into())).h(text_px(26.)).text_size(text_px(12.)).occlude().on_click(self.on(Cmd::ToggleReading, cx))
                } else {
                    ui::ghost_button("ask-leave", Some("x"), Some(tf!("退出提问 {}", "Leave chat {}", crate::shortcuts::pretty("escape")).into())).h(text_px(26.)).text_size(text_px(12.)).occlude().on_click(self.on(Cmd::LeaveAsk, cx))
                })
        } else {
            // The title fills the band the column beside it uses for its header, and the tabs the band of the
            // table's column names under that: the lines under both run on across the window.
            let beside_table = lib.page == crate::app::Page::Papers && !lib.wide;
            // The table's column names have a line over them as well as under: beside them the title's line is that
            // upper one and the tabs, one pixel shorter, end on the lower one.
            let (band, tabs) = if beside_table { (Self::papers_head() + px(1.), text_px(32.) - px(1.)) } else { (ui::head_h(), text_px(32.)) };
            div()
            .flex()
            .flex_col()
            .flex_none()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(band)
                    .px(px(16.))
                    .border_b_1()
                    .border_color(Theme::line())
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .items_start()
                            .gap(px(6.))
                            .child(self.selectable("detail-title", title.clone(), cx).flex_1().min_w_0().line_clamp(if beside_table { 3 } else { 2 }).text_size(text_px(ui::HEAD_TITLE)).line_height(text_px(22.)).font_weight(FontWeight::SEMIBOLD))
                            .child(ui::ghost_button("detail-wide", Some(if lib.wide { "panel-left-open" } else { "expand" }), None).on_click(self.on(Cmd::ToggleWide, cx)))
                            .child(ui::ghost_button("detail-close", Some("x"), None).on_click(self.on(Cmd::CloseDetail, cx))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(16.))
                    .h(tabs)
                    .px(px(16.))
                    .border_b_1()
                    .border_color(Theme::line())
                    .bg(Theme::bg_sunken())
                    .child(tab_button("tab-info", t("信息"), Tab::Info, self, cx))
                    .child(tab_button("tab-pdf", if has_pdf { "PDF" } else { t("PDF（无）") }, Tab::Pdf, self, cx))
                    .child(tab_button("tab-ask", t("提问"), Tab::Ask, self, cx))
                    // Asking with the PDF put away, or in a panel too narrow for both: one click brings it beside the conversation.
                    .when(tab == Tab::Ask && has_pdf && !(lib.split && width >= SPLIT_MIN), |d| d.child(div().flex_1()).child(ui::ghost_button("beside-pdf", Some("file-text"), Some(t("对照 PDF").into())).h(text_px(26.)).text_size(text_px(12.)).occlude().on_click(self.on(Cmd::BesidePdf(true), cx))))
                    .when_some(swap, |d, swap| d.child(div().flex_1()).child(swap)),
            ) };
        let body = match tab {
            Tab::Ask if split && has_pdf && self.settings.floating.on => self.render_floating(width, window, cx),
            Tab::Ask if split => {
                // The conversation keeps its width on either side; the PDF has the rest.
                let chat_width = self.chat_width(width);
                let chat = self.render_chat(Talk::Paper, window, cx);
                let pdf = self.render_pdf_tab(width - chat_width, cx);
                // The edge between the two: drag it to give either more room, click it twice for the widths they came with.
                let held = self.resizing == Some(Edge::Split);
                let between = div()
                    .id("split-edge")
                    .group("split-edge")
                    .absolute()
                    .top_0()
                    .left(px(if pdf_first { width - chat_width } else { chat_width } - 5.))
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
                // Swapped, only the order changes: the PDF and the conversation are the same entities, drawn at the other side.
                let chat = div().flex_none().w(px(chat_width)).h_full().border_color(Theme::line()).map(|d| if pdf_first { d.border_l_1() } else { d.border_r_1() }).child(chat);
                let pdf = div().flex_1().min_w_0().h_full().child(pdf);
                div().relative().flex().size_full().map(|d| if pdf_first { d.child(pdf).child(chat) } else { d.child(chat).child(pdf) }).child(between).into_any_element()
            }
            Tab::Info => self.render_info(window, cx),
            Tab::Pdf => self.render_pdf_tab(width, cx),
            Tab::Ask => self.render_chat(Talk::Paper, window, cx),
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
        div().relative().flex().flex_col().flex_none().w(px(width)).h_full().bg(Theme::bg_body()).when(edged, |d| d.border_l_1().border_color(Theme::line())).when(!in_strip, |d| d.child(ui::title_strip())).child(header).child(div().flex().flex_col().flex_1().min_h_0().child(body)).when(edged, |d| d.child(edge)).into_any_element()
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
            self.selectable("detail-abstract", abstract_text.clone(), cx).text_color(Theme::text_secondary()).line_height(theme::body_line(21.))
        });
        if texts.iter().any(|(_, state)| matches!(state, Translated::Busy)) {
            card = card.child(div().flex().items_center().gap(px(6.)).text_size(text_px(12.)).text_color(Theme::accent()).child(ui::spinner("translating", 13., Theme::accent())).child(t("正在翻译…")));
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
                .h(text_px(24.))
                .pl(px(9.))
                .pr(px(6.))
                .rounded(px(12.))
                .bg(fill)
                .text_color(color)
                .text_size(text_px(12.))
                .cursor_pointer()
                .hover(|s| s.opacity(0.85))
                .on_click(this.on_menu(kind, cx))
                .child(text.to_string())
                .child(ui::icon("chevron-down", 11., color))
        };
        let link = p.link();
        let mut source_line = vec![];
        // `Nature 585(7825), 357–362`: where in the journal, when the record says.
        if !p.journal.is_empty() { source_line.push(format!("{} {}", p.journal, p.placed()).trim().to_string()) }
        if let Some(year) = p.year { source_line.push(year.to_string()) }
        if p.publication_status != "unknown" { source_line.push(label(&PUBLICATION_STATUSES, &p.publication_status).to_string()) }
        let identifiers: Vec<AnyElement> = [("DOI", &p.doi, format!("https://doi.org/{}", p.doi)), ("PMID", &p.pmid, format!("https://pubmed.ncbi.nlm.nih.gov/{}/", p.pmid))]
            .into_iter()
            .filter(|(_, value, _)| !value.is_empty())
            .map(|(name, value, url)| div().flex().gap(px(6.)).text_size(text_px(12.)).child(div().flex_none().w(px(40.)).text_color(Theme::text_tertiary()).child(name)).child(self.selectable_link(name, value.clone(), url, cx).min_w_0()).into_any_element())
            .collect();
        let field_rows = self.render_field_rows(p, cx);
        let files = lib.paper_files(&id);
        // The file read in the viewer, and the numbers the attachments are cited by.
        let (shown, attachments) = (lib.pdf_file(&id).map(|f| f.id.clone()), lib.attachments(&id).iter().map(|f| f.id.clone()).collect::<Vec<_>>());
        let (main_rows, attachment_rows): (Vec<(bool, AnyElement)>, Vec<(bool, AnyElement)>) = files
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
                    Some(Err(error)) => tf!("没读出文字：{}", "No text extracted: {}", t(wisp_core::fulltext::failure_brief(error))),
                    None => String::new(),
                };
                // A fetched file says where it is from.
                let note = match lib.origins.get(&file.id) {
                    Some((_, source, version)) => tf!("{note} · 来自 {}", "{note} · From {}", [source.as_str(), t(wisp_core::openaccess::version_label(version))].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" ")),
                    None => note,
                };
                let small = |id: (&'static str, usize), text: &'static str| ui::secondary_button(id, text).h(text_px(24.)).px(px(8.)).text_size(text_px(12.));
                let row = div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .p(px(8.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(Theme::line())
                    .when(shown.as_deref() == Some(file.id.as_str()) && files.iter().filter(|f| f.format == "pdf").count() > 1, |d| d.border_color(Theme::accent()))
                    .on_mouse_down(MouseButton::Right, self.on_context(MenuKind::File(file.id.clone()), cx))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(if reading || by_eye { ui::spinner(SharedString::from(format!("file-busy-{}", file.id)), 16., Theme::accent()).into_any_element() } else { ui::icon("file-text", 18., Theme::accent()).into_any_element() })
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().truncate().child(file.title.clone()))
                                    .child(div().truncate().text_size(text_px(12.)).text_color(if failure.is_some() { Theme::warning() } else { Theme::text_tertiary() }).child(format!("{} · {}{}", match attachments.iter().position(|a| a == &file.id) { Some(n) => format!("S{}", n + 1), None if file.kind == "fulltext" => t("正文").to_string(), None => t("附件").to_string() }, file_size(file.size_bytes), if note.is_empty() { String::new() } else { format!(" · {note}") }))),
                            )
                            // A PDF is read in the viewer here, an attachment as well as the main text.
                            .when(file.format == "pdf", |d| d.child(small(("file-read", i), t("阅读")).on_click(self.on(Cmd::ReadFile(file.id.clone()), cx))))
                            .child(ui::ghost_button(("file-open", i), Some("link"), None).tooltip(ui::tip(t("用系统应用打开"))).on_click(self.on(Cmd::OpenFile(file.id.clone()), cx)))
                            .child(ui::ghost_button(("file-remove", i), Some("x"), None).on_click(self.on(Cmd::RemoveFile(file.id.clone()), cx))),
                    )
                    // A file without text: why, what it means for questions, and the two things to try.
                    .when_some(failure, |d, error| {
                        d.child(div().text_size(text_px(12.)).line_height(text_px(18.)).text_color(Theme::text_secondary()).child(tf!("{error}在读出文字之前，提问只能用到这篇文献的记录和摘要。", "{error}Until text is extracted, questions use only this paper's record and abstract.")))
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
                    .into_any_element();
                (file.kind == "fulltext", row)
            })
            .partition(|(main, _)| *main);
        let (main_rows, attachment_rows): (Vec<AnyElement>, Vec<AnyElement>) = (main_rows.into_iter().map(|(_, row)| row).collect(), attachment_rows.into_iter().map(|(_, row)| row).collect());
        let has_fields = !lib.fields.is_empty();
        let translating = lib.show_translation;
        let abstract_body = self.render_translation(p, cx).unwrap_or_else(|| {
            if p.abstract_text.is_empty() { div().text_color(Theme::text_placeholder()).child(t("没有摘要")) } else { self.selectable("detail-abstract", p.abstract_text.clone(), cx).text_color(Theme::text_secondary()).line_height(theme::body_line(21.)) }.into_any_element()
        });
        let abstract_head = div()
            .flex()
            .items_center()
            .justify_between()
            .child(ui::caption(t("摘要")))
            .child(ui::ghost_button("translate", None, Some(if translating { t("显示原文").into() } else { t("显示译文").into() })).h(text_px(22.)).text_size(text_px(12.)).when(translating, |d| d.text_color(Theme::accent())).on_click(self.on(Cmd::ToggleTranslation, cx)));
        let notes_head = div()
            .flex()
            .items_center()
            .gap(px(4.))
            .child(div().flex_1().min_w_0().child(ui::caption(t("我的笔记"))))
            .when_some(lib.note_links.get(&id).cloned(), |d, url| d.child(
                ui::ghost_button("note-link", Some("link"), Some("Notion".into()))
                    .h(text_px(22.)).px(px(6.)).text_size(text_px(12.))
                    .tooltip(|_, cx| cx.new(|_| crate::write_assist::Tip(t("打开 Notion").into())).into())
                    .on_click(self.on(Cmd::OpenUrl(url), cx)),
            ))
            .child(ui::ghost_button("note-export", None, Some(t("保存 / 导出").into()))
                .h(text_px(22.)).text_size(text_px(12.))
                .on_click(self.on(Cmd::NoteOpen(None, true), cx)));
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
                    .when(!source_line.is_empty(), |d| d.child(self.selectable("detail-source", source_line.join(" · "), cx).text_size(text_px(12.)).text_color(Theme::text_tertiary())))
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
                    .child(ui::ghost_button("detail-trash", Some("bin"), Some(t("移到回收站").into())).h(text_px(32.)).on_click(self.on(Cmd::TrashPaper(id.clone()), cx))),
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
            .child(div().flex().flex_col().gap(px(6.)).child(notes_head).child(ui::field(&lib.notes, window, cx)))
            // The paper itself and what comes with it, each with its own way to add one.
            .child(section(t("正文 PDF")).child(div().flex().flex_col().gap(px(6.)).children(main_rows).child(div().flex().child(ui::secondary_button("attach-main", t("添加正文 PDF")).on_click(self.on(Cmd::AttachAs(id.clone(), "fulltext"), cx))))))
            .child(section(t("附件／补充材料")).child(div().flex().flex_col().gap(px(6.)).children(attachment_rows).when(files.iter().all(|f| f.kind == "fulltext"), |d| d.child(div().text_size(text_px(12.)).text_color(Theme::text_placeholder()).child(t("补图、补表、补充方法等文件放在这里；PDF 附件可以在阅读器里打开，提问时会和正文一起读。")))).child(div().flex().child(ui::secondary_button("attach-more", t("添加附件")).on_click(self.on(Cmd::AttachAs(id.clone(), "supplement"), cx))))))
            .child(div().text_size(text_px(12.)).text_color(Theme::text_placeholder()).child(tf!("来源：{} · 添加于 {}", "Source: {} · Added {}", p.source, time::date_text(&p.added_at))))
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
                .when_some(last, |d, detail| d.child(div().max_w(px(420.)).text_size(text_px(12.)).line_height(text_px(18.)).text_color(Theme::text_tertiary()).child(detail)))
                .size_full()
                .into_any_element();
        };
        let Some(pdf) = lib.pdf.clone() else { return div().into_any_element() };
        let (file_id, file_title) = (file.id.clone(), file.title.clone());
        // The paper's PDFs beyond the one on show: the title becomes a menu of them, saying which is read.
        let attachments = lib.attachments(&p.id);
        let number = attachments.iter().position(|f| f.id == file_id).map(|i| i + 1);
        let others = lib.paper_files(&p.id).iter().filter(|f| f.format == "pdf").count() > 1;
        let file_title = match number {
            Some(n) => tf!("附件 S{n} · {file_title}", "Attachment S{n} · {file_title}"),
            None if others => tf!("正文 · {file_title}", "Main text · {file_title}"),
            None => file_title,
        };
        let beside = lib.split && lib.tab == Tab::Ask;
        let reading = lib.reading;
        // With the library's conversation on show as well, a selection can go to either.
        let library_chat = lib.page == crate::app::Page::Ask;
        pdf.update(cx, |pdf, _| {
            pdf.set_width(width);
            pdf.library_chat = library_chat;
        });
        let (page, count, percent, zoom, cutting, split, pen, marks_open, pdf_height) = {
            let view = pdf.read(cx);
            (view.current_page(), view.page_count(), view.percent(), view.zoom(), view.cutting(), view.split(), view.tool(), view.annotations_open(), view.height())
        };
        // The tools stand in the toolbar, or float at the page's right edge: a habit, kept for every paper.
        let floating = self.settings.pdf_tools_floating;
        let docked = count > 0 && !floating;
        let split_label = match split { Split::Off => t("分屏"), Split::Side => t("左右分屏"), Split::Stacked => t("上下分屏") };
        // The fit the reader reads at, which a zoom by hand goes back to.
        let fit = lib.zoom;
        let fit_label = |zoom: Zoom| if zoom == Zoom::FitPage { t("适应整页") } else { t("适应宽度") };
        let tool = |id: &'static str, icon: &'static str| ui::ghost_button(id, Some(icon), None).h(text_px(24.)).min_w(px(24.));
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
                    .text_size(text_px(12.))
                    .text_color(Theme::text_tertiary())
                    .child(if others {
                        // Which file is read, and the way to the others: 正文 and each attachment.
                        div().id("pdf-files").flex().flex_shrink().min_w_0().max_w(px(320.)).items_center().gap(px(4.)).cursor_pointer().rounded(px(5.)).hover(|s| s.text_color(Theme::text_secondary())).child(div().min_w_0().truncate().child(file_title)).child(ui::icon("chevron-down", 11., Theme::text_tertiary())).tooltip(ui::tip(t("切换文件"))).on_click(self.on_menu(MenuKind::PdfFiles, cx))
                    } else {
                        div().id("pdf-files").flex_shrink().min_w_0().truncate().child(file_title)
                    })
                    // The title is only as wide as it reads; the space after it is no button.
                    .child(div().flex_1())
                    .when(count > 0, |d| d.child(div().flex_none().child(tf!("{page} / {count} 页", "{page} / {count} pages"))))
                    // Smaller, the size in force (a menu of sizes), larger.
                    .when(count > 0, |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_none()
                                .items_center()
                                .child(tool("pdf-zoom-out", "minus").on_click(self.on(Cmd::PdfZoom(ZoomTo::Out), cx)))
                                .child(ui::ghost_button("pdf-zoom", None, Some(if zoom.fits() { format!("{} · {}%", fit_label(zoom), percent.round()).into() } else { format!("{}%", percent.round()).into() })).h(text_px(24.)).px(px(6.)).text_size(text_px(12.)).on_click(self.on_menu(MenuKind::PdfZoom, cx)))
                                .child(tool("pdf-zoom-in", "plus").on_click(self.on(Cmd::PdfZoom(ZoomTo::In), cx)))
                                // The whole page in one click from any size; in force it is marked, and a click brings the page's top back.
                                .child(ui::ghost_button("pdf-zoom-page", None, None).h(text_px(24.)).min_w(px(24.)).child(ui::icon("scan", 16., if zoom == Zoom::FitPage { Theme::accent() } else { Theme::text_secondary() })).tooltip(ui::tip(t("适应整页"))).on_click(self.on(Cmd::PdfZoom(ZoomTo::Set(Zoom::FitPage)), cx)))
                                // Zoomed by hand: one click back to the fit the reader reads at, offered only then (the whole page has its own button).
                                .when(!zoom.fits() && fit != Zoom::FitPage, |d| d.child(ui::ghost_button("pdf-zoom-fit", None, Some(fit_label(fit).into())).h(text_px(24.)).px(px(6.)).text_size(text_px(12.)).on_click(self.on(Cmd::PdfZoom(ZoomTo::Set(fit)), cx)))),
                        )
                    })
                    // The same file twice, side by side or one over the other: a figure in one half, the page that speaks of it in the other.
                    .when(docked, |d| d.child(ui::ghost_button("pdf-split", None, Some(split_label.into())).h(text_px(24.)).px(px(6.)).text_size(text_px(12.)).when(split != Split::Off, |d| d.text_color(Theme::accent())).on_click(self.on_menu(MenuKind::PdfSplit, cx))))
                    // Cut a piece out of a page (a figure, a table): to keep in the figure library, or as a picture for the next question.
                    .when(docked, |d| d.child(ui::ghost_button("pdf-cut", None, Some(if cutting { t("取消截图").into() } else { t("截图").into() })).h(text_px(24.)).px(px(6.)).text_size(text_px(12.)).when(cutting, |d| d.text_color(Theme::accent())).on_click(self.on(Cmd::PdfCut, cx))))
                    // Marks placed on the page rather than its text: a box round a figure or a passage, a sticky note anywhere.
                    .when(docked, |d| d.child(ui::ghost_button("pdf-box", Some("square"), Some(t("方框").into())).h(text_px(24.)).px(px(6.)).text_size(text_px(12.)).when(pen == Tool::Box, |d| d.text_color(Theme::accent())).tooltip(ui::tip(t("在页面上拖出方框，框住图表或文字"))).on_click(self.on(Cmd::PdfTool(Tool::Box), cx))))
                    .when(docked, |d| d.child(ui::ghost_button("pdf-note", Some("sticky-note"), Some(t("便签").into())).h(text_px(24.)).px(px(6.)).text_size(text_px(12.)).when(pen == Tool::Note, |d| d.text_color(Theme::accent())).tooltip(ui::tip(t("点击页面任意位置添加便签"))).on_click(self.on(Cmd::PdfTool(Tool::Note), cx))))
                    // The whole window for this paper; in reading mode the way back is at the top.
                    // Its shortcut in the tooltip: the way in from the list without opening the paper first.
                    .when(!reading, |d| {
                        let key = crate::shortcuts::key_of("reading", &self.settings.shortcuts);
                        d.child(ui::ghost_button("pdf-reading", None, Some(t("阅读模式").into())).h(text_px(24.)).px(px(6.)).text_size(text_px(12.)).when(!key.is_empty(), |d| d.tooltip(ui::tip(tf!("阅读模式 {}", "Reading mode {}", crate::shortcuts::pretty(&key))))).on_click(self.on(Cmd::ToggleReading, cx)))
                    })
                    // Floating, the column has it; a file that could not be read keeps it here, the way to read it elsewhere.
                    .when(!floating || count == 0, |d| d.child(ui::ghost_button("pdf-open", Some("link"), (width >= 700.).then(|| t("用系统应用打开").into())).h(text_px(24.)).on_click(self.on(Cmd::OpenFile(file_id.clone()), cx))))
                    // Where the tools stand; always the bar's last button, so the same click in the same place puts them back.
                    .when(count > 0, |d| d.child(ui::ghost_button("pdf-tools-float", None, None).h(text_px(24.)).min_w(px(24.)).child(ui::icon("panel-right", 16., if floating { Theme::accent() } else { Theme::text_secondary() })).tooltip(ui::tip(if floating { t("把工具放回工具栏") } else { t("工具悬浮在页面右侧") })).on_click(self.on(Cmd::FloatPdfTools(!floating), cx))))
                    // Beside the conversation: put the PDF away again.
                    .when(beside, |d| d.child(tool("pdf-beside-close", "x").on_click(self.on(Cmd::BesidePdf(false), cx)))),
            )
            .child(div().flex_1().min_h_0().relative().child(pdf).when(floating && count > 0, |d| d.child(self.pdf_tool_column(file_id, width, pdf_height, cutting, split, pen, marks_open, cx))))
            .into_any_element()
    }

    /// The PDF's tools as a column of icons at the page's right edge, under 标注 (or beside its list
    /// while that is open): at hand wherever the page is read, the names in the tooltips.
    #[allow(clippy::too_many_arguments)]
    fn pdf_tool_column(&self, file_id: String, width: f32, pdf_height: f32, cutting: bool, split: Split, pen: Tool, marks_open: bool, cx: &mut Context<Self>) -> AnyElement {
        let tool = |id: &'static str, icon: &'static str, lit: bool, tip: &'static str| ui::ghost_button(id, None, None).size(px(28.)).child(ui::icon(icon, 16., if lit { Theme::accent() } else { Theme::text_secondary() })).tooltip(ui::tip(t(tip)));
        let line = || div().flex_none().h(px(1.)).mx(px(4.)).my(px(2.)).bg(Theme::line());
        // Where the reader dragged it, or under 标注; there, it stands to the left of the list of marks
        // while that is open and leaves room (as wide as `PdfView::annotation_panel` makes it).
        let area = (width, pdf_height);
        let (mut right, top) = geo::tools_at(self.settings.pdf_tools_at, area);
        let list = (width - 16.).clamp(0., 360.);
        if self.settings.pdf_tools_at.is_none() && marks_open && width - list - 16. >= geo::TOOLS.0 + 8. { right = list + 16. }
        let held = self.lib.as_ref().and_then(|lib| lib.float_drag).is_some_and(|drag| drag.held_tools());
        let split_tip = match split { Split::Off => "分屏", Split::Side => "左右分屏", Split::Stacked => "上下分屏" };
        div()
            .absolute()
            .top(px(top))
            .right(px(right))
            .w(px(geo::TOOLS.0))
            .flex()
            .flex_col()
            .gap(px(2.))
            .p(px(3.))
            .rounded(px(8.))
            .bg(Theme::bg_body())
            .border_1()
            .border_color(Theme::line_strong())
            // A press on the column is not a press on the page: no selection, cut or mark starts under it.
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            // The grip: drag the column anywhere over the page; twice, it goes home under 标注.
            .child(
                div()
                    .id("pdf-float-grip")
                    .flex()
                    .flex_none()
                    .justify_center()
                    .items_center()
                    .h(px(10.))
                    .rounded(px(4.))
                    .cursor(if held { CursorStyle::ClosedHand } else { CursorStyle::OpenHand })
                    .hover(|s| s.bg(Theme::hover()))
                    .child(div().w(px(14.)).h(px(2.)).rounded(px(1.)).bg(Theme::line_strong()))
                    .tooltip(ui::tip(t("拖动以移动，双击放回原处")))
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.tools_press(event, cx);
                    })),
            )
            .child(tool("pdf-float-split", "columns-2", split != Split::Off, split_tip).on_click(self.on_menu(MenuKind::PdfSplit, cx)))
            .child(tool("pdf-float-cut", "scissors", cutting, if cutting { "取消截图" } else { "截图" }).on_click(self.on(Cmd::PdfCut, cx)))
            .child(tool("pdf-float-box", "square", pen == Tool::Box, "在页面上拖出方框，框住图表或文字").on_click(self.on(Cmd::PdfTool(Tool::Box), cx)))
            .child(tool("pdf-float-note", "sticky-note", pen == Tool::Note, "点击页面任意位置添加便签").on_click(self.on(Cmd::PdfTool(Tool::Note), cx)))
            .child(line())
            .child(tool("pdf-float-open", "link", false, "用系统应用打开").on_click(self.on(Cmd::OpenFile(file_id), cx)))
            .into_any_element()
    }
}
