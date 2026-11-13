//! An open library: the sidebar (collect, views, tags), the paper table with
//! its search and filters, and the trash.

use crate::i18n::{label, t};
use crate::app::{Cmd, Edge, FilterKey, Library, MenuKind, Page, Tab, Translated, Workspace};
use crate::settings::SettingsPage;
use crate::theme::{self, Theme};
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use std::ops::Range;
use wisp_core::filter::{self, View};
use wisp_core::model::*;
use wisp_core::time;

const SIDEBAR_W: f32 = 224.;
/// How narrow and how wide the sidebar is dragged. Pulled in past half its narrowest, it is put away.
const SIDEBAR_MIN: f32 = 176.;
const SIDEBAR_MAX: f32 = 420.;
pub const DETAIL_W: f32 = 448.;
/// The narrowest the detail panel is dragged to, and what the list beside it keeps.
const DETAIL_MIN: f32 = 320.;
const MAIN_MIN: f32 = 260.;

/// The narrowest a paper's conversation is dragged to beside its PDF, and what the PDF keeps.
const SPLIT_CHAT_MIN: f32 = 320.;
const SPLIT_PDF_MIN: f32 = 360.;

/// How wide the conversation is in a panel `width` wide, when the reader asked for `own` (0 or less: no wish).
fn split_chat_width(own: f32, width: f32) -> f32 {
    let wished = if own > 0. { own } else { (width * 0.45).clamp(360., 520.) };
    wished.min(width - SPLIT_PDF_MIN).max(SPLIT_CHAT_MIN)
}

/// The list of papers beside a paper that is asked about: the width it comes with, and how narrow and how wide it is dragged.
const ASK_LIST_W: f32 = 280.;
const ASK_LIST_MIN: f32 = 200.;
const ASK_LIST_MAX: f32 = 420.;

/// How wide that list is in a window `viewport` wide, when the reader asked for `own` (0 or less: no wish).
/// The conversation and the PDF keep `detail::SPLIT_MIN` between them; a window without room for the list has none.
fn ask_list_width(own: f32, viewport: f32) -> f32 {
    let room = viewport - crate::detail::SPLIT_MIN;
    if room < ASK_LIST_MIN { return 0. }
    (if own > 0. { own } else { ASK_LIST_W }).clamp(ASK_LIST_MIN, ASK_LIST_MAX).min(room)
}

fn view_label(view: View) -> &'static str {
    VIEWS.iter().find(|(_, _, _, other)| *other == view).map_or(t("文献"), |(_, _, label, _)| t(label))
}

/// The views of a library, as the sidebar lists them: element id, icon, name.
const VIEWS: [(&str, &str, &str, View); 6] = [
    ("view-all", "library", "全部文献", View::All),
    ("view-recent", "plus", "最近添加", View::Recent),
    ("view-unread", "eye", "未读", View::Unread),
    ("view-starred", "star", "收藏", View::Starred),
    ("view-pending", "list-todo", "待判断", View::Pending),
    ("view-nopdf", "file-text", "缺 PDF", View::NoPdf),
];

/// A column of the paper table: fixed width, or the flexible title column.
struct Column {
    label: String,
    width: Option<f32>,
    sort: Option<&'static str>,
    /// What the cells show.
    kind: ColumnKind,
}

#[derive(Clone)]
enum ColumnKind {
    Star,
    Title,
    Journal,
    Year,
    Field(String),
    Fulltext,
    Reading,
    Screening,
}

impl Workspace {
    /// The sidebar's width for this window: none while it is put away, else
    /// the width the reader dragged it to, or the one it comes with.
    pub fn sidebar_width(&self, window: &Window) -> f32 {
        if self.settings.sidebar_hidden { return 0. }
        let own = if self.settings.sidebar_width > 0. { self.settings.sidebar_width } else { SIDEBAR_W };
        own.min(Self::sidebar_max(window)).max(SIDEBAR_MIN)
    }

    fn sidebar_max(window: &Window) -> f32 {
        SIDEBAR_MAX.min(f32::from(window.viewport_size().width) * 0.4)
    }

    /// The sidebar's edge is being dragged: the sidebar reaches from the
    /// window's left edge to the pointer, and is put away once the pointer
    /// is well inside its narrowest width (back out, and it is there again).
    pub fn drag_sidebar(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.resizing != Some(Edge::Sidebar) { return }
        if event.pressed_button != Some(MouseButton::Left) { return self.end_drag(cx) }
        let x = f32::from(event.position.x);
        let hidden = x < SIDEBAR_MIN / 2.;
        let width = x.min(Self::sidebar_max(window)).max(SIDEBAR_MIN);
        if hidden == self.settings.sidebar_hidden && (hidden || (width - self.settings.sidebar_width).abs() < 0.5) { return }
        self.settings.sidebar_hidden = hidden;
        if !hidden { self.settings.sidebar_width = width }
        cx.notify();
    }

    /// A paper is asked about with its PDF wanted beside the conversation:
    /// the window is laid out for that. The sidebar is away, the papers are
    /// a narrow list at the left (`ask_list_width`), and the conversation
    /// and the PDF have the rest. Nothing is kept for it: it follows from
    /// the tab and `Library::split`, so 信息, the PDF put away, Esc
    /// (`Cmd::LeaveAsk`) and closing the paper all end it.
    pub fn asking(&self, window: &Window) -> bool {
        let Some(lib) = &self.lib else { return false };
        lib.selected.is_some() && lib.tab == Tab::Ask && lib.split && !lib.reading && !matches!(lib.page, Page::Trash | Page::Sources | Page::Guide) && f32::from(window.viewport_size().width) >= crate::detail::SPLIT_MIN
    }

    /// The width of the list of papers beside a paper that is asked about: none on a page whose
    /// list is not the papers', nor in a window too narrow for three columns (the paper then has it all).
    pub fn ask_list_width(&self, window: &Window) -> f32 {
        if !self.asking(window) || self.lib.as_ref().is_none_or(|lib| lib.page != Page::Papers) { return 0. }
        ask_list_width(self.settings.ask_list_width, f32::from(window.viewport_size().width))
    }

    /// How tall a paper's row is: in the table, or in the narrow list beside a paper that is asked
    /// about, where a title has two lines. 设置 › 外观: the compact rows still fit the title and its line of details.
    pub fn row_height(&self, narrow: bool) -> f32 {
        match (narrow, self.settings.density == "compact") {
            (false, false) => 56.,
            (true, false) => 68.,
            (_, true) => 44.,
        }
    }

    /// The detail panel's width for this window: the width the reader
    /// dragged it to, or the one it comes with; most of the window while it
    /// is expanded. The list beside it keeps `MAIN_MIN` where the window allows.
    pub fn detail_width(&self, window: &Window) -> f32 {
        let Some(lib) = &self.lib else { return 0. };
        if lib.selected.is_none() || matches!(lib.page, Page::Trash | Page::Sources | Page::Guide) { return 0. }
        let viewport = f32::from(window.viewport_size().width);
        // Reading mode: the paper has the window.
        if lib.reading { return viewport }
        // Asked about: what the narrow list leaves.
        if self.asking(window) { return viewport - self.ask_list_width(window) }
        let own = if self.settings.detail_width > 0. { self.settings.detail_width } else { DETAIL_W };
        let sidebar = self.sidebar_width(window);
        let main_min = if lib.page == Page::Papers { MAIN_MIN } else { 460. };
        if lib.wide { (viewport * 0.62).max(DETAIL_W).min(viewport - sidebar - 80.) } else { own.min(viewport - sidebar - main_min).max(DETAIL_MIN) }
    }

    /// The edge between the list and the detail panel is being dragged:
    /// the panel reaches from the pointer to the window's right edge.
    pub fn drag_detail(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.resizing != Some(Edge::Detail) { return }
        // Let go outside the window: the button is up by the time the pointer is back.
        if event.pressed_button != Some(MouseButton::Left) { return self.end_drag(cx) }
        let viewport = f32::from(window.viewport_size().width);
        // Beside a paper that is asked about, the edge is the narrow list's: it reaches from the window's left to the pointer.
        if self.asking(window) {
            let width = ask_list_width(f32::from(event.position.x).max(ASK_LIST_MIN), viewport);
            if (width - self.settings.ask_list_width).abs() < 0.5 { return }
            self.settings.ask_list_width = width;
            return cx.notify();
        }
        let width = (viewport - f32::from(event.position.x)).min(viewport - self.sidebar_width(window) - MAIN_MIN).max(DETAIL_MIN);
        if (width - self.settings.detail_width).abs() < 0.5 { return }
        self.settings.detail_width = width;
        if let Some(lib) = self.lib.as_mut() { lib.wide = false }
        cx.notify();
    }

    /// The conversation's width beside the PDF in a detail panel `width` wide: the one the reader
    /// dragged it to, or a good share of the panel; the PDF keeps `SPLIT_PDF_MIN`.
    pub fn chat_width(&self, width: f32) -> f32 {
        split_chat_width(self.settings.chat_width, width)
    }

    /// The edge between a paper's conversation and its PDF is being dragged:
    /// the conversation reaches from the panel's left edge to the pointer.
    pub fn drag_split(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.resizing != Some(Edge::Split) { return }
        if event.pressed_button != Some(MouseButton::Left) { return self.end_drag(cx) }
        let panel = self.detail_width(window);
        let left = f32::from(window.viewport_size().width) - panel;
        let width = split_chat_width(f32::from(event.position.x) - left, panel);
        if (width - self.settings.chat_width).abs() < 0.5 { return }
        self.settings.chat_width = width;
        cx.notify();
    }

    /// The edge was let go: the width is kept for the next time.
    pub fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.resizing.take().is_none() { return }
        self.save_settings(cx);
        cx.notify();
    }

    pub fn render_library(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let detail_width = self.detail_width(window);
        // Reading mode: nothing but the paper.
        if detail_width > 0. && self.lib.as_ref().is_some_and(|l| l.reading) { return div().size_full().child(self.render_detail(detail_width, window, cx)).into_any_element() }
        // Into or out of the layout for asking, the rows change their height: the list finds its paper again.
        let asking = self.asking(window);
        if let Some(lib) = self.lib.as_mut().filter(|lib| lib.ask_drawn != asking) {
            lib.ask_drawn = asking;
            if let Some(at) = lib.selected.as_ref().and_then(|id| lib.shown.iter().position(|&i| lib.papers[i].id == *id)) { lib.table_scroll.scroll_to_item(at, ScrollStrategy::Center) }
        }
        // A paper asked about: the papers as a narrow list, then the conversation and the PDF.
        if asking {
            let list_width = self.ask_list_width(window);
            let list = (list_width > 0.).then(|| self.render_ask_list(list_width, window, cx));
            return div().flex().size_full().children(list).child(self.render_detail(detail_width, window, cx)).into_any_element();
        }
        let sidebar_width = self.sidebar_width(window);
        let main_width = f32::from(window.viewport_size().width) - sidebar_width - detail_width;
        let page = self.lib.as_ref().map_or(Page::Papers, |l| l.page);
        let main = match page {
            Page::Papers => self.render_papers(main_width, window, cx),
            Page::Ask => self.render_chat(false, window, cx),
            Page::Trash => self.render_trash(cx),
            Page::Sources => self.render_searches(cx),
            Page::Guide => self.render_guide(window, cx),
            Page::Screen => self.render_screen(main_width, window, cx),
            Page::Write => self.render_write(main_width, window, cx),
            Page::Figures => self.render_figures(main_width, window, cx),
        };
        let shown = sidebar_width > 0.;
        // The sidebar's right edge: drag it for a wider or narrower sidebar, click it twice to put the sidebar away.
        let dragging = self.resizing == Some(Edge::Sidebar);
        let edge = div()
            .id("sidebar-edge")
            .group("sidebar-edge")
            .absolute()
            .top_0()
            .left(px(-4.))
            .w(px(9.))
            .h_full()
            .flex()
            .justify_center()
            .cursor_col_resize()
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &MouseDownEvent, _, cx| {
                if event.click_count >= 2 {
                    this.resizing = None;
                    this.settings.sidebar_hidden = true;
                    this.save_settings(cx);
                } else {
                    this.resizing = Some(Edge::Sidebar);
                }
                cx.notify();
            }))
            // A tap is over before the layer that follows the pointer is drawn: the edge hears the end of it itself.
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.end_drag(cx)))
            .child(div().w(px(2.)).h_full().when(dragging, |d| d.bg(Theme::accent())).when(!dragging, |d| d.group_hover("sidebar-edge", |s| s.bg(Theme::accent().opacity(0.5)))));
        // With the sidebar put away its two top buttons sit over the list, where they were.
        let strip = if shown { ui::title_strip() } else { self.render_corner(true, cx).mt(px(Self::corner_top())).pl(px(10. + ui::traffic_lights_width())) };
        div()
            .flex()
            .size_full()
            .when(shown, |d| d.child(self.render_sidebar(sidebar_width, cx)))
            .child(div().relative().flex().flex_col().flex_1().min_w_0().h_full().bg(Theme::bg_body()).when(shown, |d| d.border_l_1().border_color(Theme::line())).child(strip).children(self.render_batch_translation(cx)).child(div().flex().flex_col().flex_1().min_h_0().child(main)).when(shown || dragging, |d| d.child(edge)))
            .when(detail_width > 0., |d| d.child(self.render_detail(detail_width, window, cx)))
            .into_any_element()
    }

    /// What is left free above the top-left buttons. Where the window draws its own title bar there is
    /// nothing: the buttons sit in a strip of its height, level with macOS's traffic lights beside them.
    fn corner_top() -> f32 {
        if ui::custom_titlebar() { 0. } else { 10. }
    }

    /// The window's top-left corner in a library: the button that puts the
    /// sidebar away or brings it back, and the way back to the libraries.
    /// A narrow sidebar keeps the arrow and drops its words.
    fn render_corner(&self, roomy: bool, cx: &mut Context<Self>) -> Div {
        ui::title_strip()
            .when(!ui::custom_titlebar(), |d| d.h(px(28.)))
            .pl(px(ui::traffic_lights_width()))
            .child(ui::ghost_button("toggle-sidebar", Some("panel-left"), None).occlude().on_click(self.on(Cmd::ToggleSidebar, cx)))
            .child(ui::ghost_button("back-home", Some("arrow-left"), roomy.then(|| t("所有文献库").into())).occlude().on_click(self.on(Cmd::GoHome, cx)))
    }

    /// Each background job stays compact; its dialog owns the live details.
    fn render_import(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let run = self.lib.as_ref()?.import.as_ref()?;
        Some(ui::progress_entry("import-progress", if run.running() { t("导入 PDF") } else { t("PDF 导入完成") }, run.rows.len(), run.total, run.running(), &ui::import_segments(run))
            .on_click(self.on(Cmd::ShowImport, cx)).into_any_element())
    }

    fn render_fetching(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let run = self.lib.as_ref()?.fetching.as_ref()?;
        Some(ui::progress_entry("fetch-progress", if run.running() { t("查找全文") } else { run.status() }, run.rows.len(), run.total, run.running(), &ui::fetch_segments(run))
            .on_click(self.on(Cmd::ShowFetch, cx)).into_any_element())
    }

    fn render_sidebar(&mut self, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let lib = self.lib.as_ref().unwrap();
        let now = time::now_millis();
        let nav = |id: &'static str, icon: &'static str, label: &'static str, count: Option<usize>, active: bool, cmd: Cmd, this: &Self, cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(8.))
                .h(px(30.))
                .px(px(8.))
                .rounded(px(6.))
                .cursor_pointer()
                .when(active, |d| d.bg(Theme::accent_soft()).text_color(Theme::accent()).font_weight(FontWeight::MEDIUM))
                .when(!active, |d| d.hover(|s| s.bg(Theme::hover())))
                .on_click(this.on(cmd, cx))
                .child(ui::icon(icon, 16., if active { Theme::accent() } else { Theme::text_secondary() }))
                .child(div().flex_1().min_w_0().truncate().child(label))
                .when_some(count, |d, n| d.child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child(n.to_string())))
        };
        let on_papers = lib.page == Page::Papers;
        let view_rows: Vec<AnyElement> = VIEWS
            .into_iter()
            .map(|(id, icon, label, view)| {
                // 收藏 holds the starred papers and the figure library: both count, and either page is its own.
                let collected = if view == View::Starred { lib.gallery.items.len() } else { 0 };
                let active = (on_papers && lib.filters.view == view) || (view == View::Starred && lib.page == Page::Figures);
                nav(id, icon, t(label), Some(filter::view_count(&lib.papers, view, now) + collected), active, Cmd::SetView(view), self, cx).into_any_element()
            })
            .collect();
        let tags = filter::tag_counts(&lib.papers);
        let tag_rows: Vec<AnyElement> = tags
            .iter()
            .take(30)
            .enumerate()
            .map(|(i, (tag, count))| {
                let active = on_papers && &lib.filters.tag == tag;
                div()
                    .id(("tag", i))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(28.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .when(active, |d| d.bg(Theme::accent_soft()).text_color(Theme::accent()))
                    .when(!active, |d| d.hover(|s| s.bg(Theme::hover())))
                    .on_click(self.on(Cmd::SetFilter(FilterKey::Tag, if active { String::new() } else { tag.clone() }), cx))
                    .on_mouse_down(MouseButton::Right, self.on_context(MenuKind::Tag(tag.clone()), cx))
                    .child(div().size(px(6.)).flex_none().rounded_full().bg(if active { Theme::accent() } else { Theme::text_placeholder() }))
                    .child(div().flex_1().min_w_0().truncate().child(tag.clone()))
                    .child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child(count.to_string()))
                    .into_any_element()
            })
            .collect();
        let trash_count = lib.trash.len();
        div()
            .id("library-sidebar")
            .flex()
            .flex_col()
            .flex_none()
            .w(px(width))
            .h_full()
            .overflow_y_scroll()
            .px(px(10.))
            .pb(px(10.))
            .pt(px(Self::corner_top()))
            .gap(px(10.))
            .child(self.render_corner(width >= ui::traffic_lights_width() + 150., cx).when(ui::custom_titlebar(), |d| d.mb(px(4.))))
            .child(
                div()
                    .id("library-head")
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(8.))
                    .cursor_pointer()
                    .hover(|s| s.bg(Theme::hover()))
                    .on_click(self.on(Cmd::EditLibrary(lib.info.id.clone()), cx))
                    .child(div().flex().items_center().gap(px(8.)).child(div().size(px(10.)).flex_none().rounded(px(3.)).bg(Theme::accent())).child(div().flex_1().min_w_0().truncate().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(lib.info.name.clone())))
            )
            .child(ui::primary_button("add-paper", t("添加文献")).w_full().on_click(self.on_menu(MenuKind::Add, cx)))
            .child(div().flex().flex_col().gap(px(1.)).children(view_rows))
            .child(div().flex().flex_col().gap(px(1.)).child(nav("page-ask", "sparkle", t("助手"), None, lib.page == Page::Ask, Cmd::SetPage(Page::Ask), self, cx)).child(nav("page-sources", "search", t("检索与导入"), (!lib.searches.is_empty()).then_some(lib.searches.len()), matches!(lib.page, Page::Sources | Page::Guide), Cmd::SetPage(Page::Sources), self, cx)).child(nav("page-screen", "list-todo", t("筛选"), Some(lib.papers.iter().filter(|p| p.screening == "pending").count()).filter(|n| *n > 0), lib.page == Page::Screen, Cmd::SetPage(Page::Screen), self, cx)).child(nav("page-write", "pen-line", t("写作"), None, lib.page == Page::Write, Cmd::SetPage(Page::Write), self, cx)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(80.))
                    .gap(px(2.))
                    .child(ui::caption(t("标签")).px(px(8.)).py(px(4.)))
                    .child(div().id("tags").flex().flex_col().flex_1().min_h_0().overflow_y_scroll().gap(px(1.)).children(tag_rows).when(tags.is_empty(), |d| d.child(div().px(px(8.)).text_size(px(12.)).text_color(Theme::text_placeholder()).child(t("编辑文献时添加标签")))))
            )
            .children(self.render_import(cx))
            .when_some(lib.zotero.as_ref(), |d, run| d.child(
                ui::progress_entry("zotero-progress", if run.running { t("导入 Zotero") } else if !run.error.is_empty() { t("Zotero 导入失败") } else { t("Zotero 导入完成") }, run.rows.len(), run.total, run.running, &ui::zotero_segments(run))
                    .on_click(self.on(Cmd::ShowZotero, cx))
            ))
            .children(self.render_fetching(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .pt(px(6.))
                    .border_t_1()
                    .border_color(Theme::line())
                    .child(nav("manage-fields", "list", t("自定义词条"), Some(lib.fields.len()), false, Cmd::ManageFields, self, cx))
                    .child(nav("page-trash", "bin", t("回收站"), (trash_count > 0).then_some(trash_count), lib.page == Page::Trash, Cmd::SetPage(Page::Trash), self, cx))
                    .child(nav("open-settings", "settings-gear", t("设置"), None, false, Cmd::OpenSettings(SettingsPage::General), self, cx)),
            )
            .into_any_element()
    }

    fn columns(lib: &Library, width: f32) -> Vec<Column> {
        let column = |label: &str, width: f32, sort: Option<&'static str>, kind: ColumnKind| Column { label: label.to_string(), width: Some(width), sort, kind };
        let mut columns = vec![column("", 34., None, ColumnKind::Star), Column { label: t("文献").into(), width: None, sort: Some("title"), kind: ColumnKind::Title }];
        // The title keeps at least 300px; optional columns join while there is room, in this order.
        let mut room = width - 34. - 300. - 32.;
        let english = crate::i18n::is_english();
        let mut optional = vec![column(t("阅读"), if english { 112. } else { 76. }, None, ColumnKind::Reading), column(t("年份"), 60., Some("year"), ColumnKind::Year), column(t("全文"), if english { 144. } else { 84. }, None, ColumnKind::Fulltext)];
        optional.extend(lib.fields.iter().filter(|f| f.in_table).map(|f| column(&f.name, 132., None, ColumnKind::Field(f.id.clone()))));
        optional.push(column(t("期刊"), 150., Some("journal"), ColumnKind::Journal));
        optional.push(column(t("收录"), if english { 176. } else { 84. }, None, ColumnKind::Screening));
        let mut chosen = vec![];
        for c in optional {
            let w = c.width.unwrap_or(0.);
            if room >= w {
                room -= w;
                chosen.push(c);
            }
        }
        // Shown in reading order rather than the order they were admitted in.
        let rank = |c: &Column| match c.kind {
            ColumnKind::Journal => 0,
            ColumnKind::Year => 1,
            ColumnKind::Field(_) => 2,
            ColumnKind::Fulltext => 3,
            ColumnKind::Reading => 4,
            ColumnKind::Screening => 5,
            _ => 0,
        };
        chosen.sort_by_key(rank);
        columns.extend(chosen);
        columns
    }

    fn render_papers(&mut self, width: f32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let lib = self.lib.as_ref().unwrap();
        let f = &lib.filters;
        // Filter chips: the label alone when unset, "label: value" when set.
        let chip = |id: ElementId, label: &str, value: String, key: FilterKey, this: &Self, cx: &mut Context<Self>| {
            let set = !value.is_empty();
            div()
                .id(id)
                .flex()
                .flex_none()
                .items_center()
                .gap(px(4.))
                .h(px(26.))
                .px(px(9.))
                .rounded(px(13.))
                .border_1()
                .cursor_pointer()
                .text_size(px(12.))
                .when(set, |d| d.bg(Theme::accent_soft()).border_color(Theme::accent_soft()).text_color(Theme::accent()))
                .when(!set, |d| d.border_color(Theme::line_strong()).text_color(Theme::text_secondary()).hover(|s| s.bg(Theme::hover())))
                .on_click(this.on_menu(MenuKind::Filter(key), cx))
                .child(if set { format!("{label}：{value}") } else { label.to_string() })
                .child(ui::icon("chevron-down", 12., if set { Theme::accent() } else { Theme::text_tertiary() }))
        };
        let mut chips: Vec<AnyElement> = vec![
            chip("f-year".into(), t("年份"), f.year.clone(), FilterKey::Year, self, cx).into_any_element(),
            chip("f-reading".into(), t("阅读"), label(&READINGS, &f.reading).to_string(), FilterKey::Reading, self, cx).into_any_element(),
            chip("f-screening".into(), t("收录"), label(&SCREENINGS, &f.screening).to_string(), FilterKey::Screening, self, cx).into_any_element(),
            chip("f-fulltext".into(), t("全文"), label(&FULLTEXTS, &f.fulltext).to_string(), FilterKey::Fulltext, self, cx).into_any_element(),
            chip("f-tag".into(), t("标签"), f.tag.clone(), FilterKey::Tag, self, cx).into_any_element(),
        ];
        for (i, field) in lib.fields.iter().enumerate() {
            if field.kind == FieldKind::Text { continue }
            let value = f.fields.get(&field.id).cloned().unwrap_or_default();
            chips.push(chip(("f-field", i).into(), &field.name, value, FilterKey::Field(field.id.clone()), self, cx).into_any_element());
        }
        chips.push(chip("f-source".into(), t("来源"), f.source.clone(), FilterKey::Source, self, cx).into_any_element());
        chips.push(chip("f-preprint".into(), t("预印本"), match f.preprint.as_str() { "yes" => t("只看").into(), "no" => t("不含").into(), _ => String::new() }, FilterKey::Preprint, self, cx).into_any_element());
        let filtered = f.active() > 0 || !f.search.trim().is_empty();
        if filtered {
            chips.push(ui::ghost_button("f-clear", Some("x"), Some(t("清空筛选").into())).h(px(26.)).on_click(self.on(Cmd::ClearFilters, cx)).into_any_element());
        }

        let columns = Self::columns(lib, width);
        let (sort_key, sort_desc) = filter::sort_parts(&f.sort);
        let header: Vec<AnyElement> = columns
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let active = c.sort == Some(sort_key);
                div()
                    .id(("th", i))
                    .flex()
                    .items_center()
                    .gap(px(3.))
                    .h_full()
                    .px(px(8.))
                    .map(|d| match c.width {
                        Some(w) => d.w(px(w)).flex_none(),
                        None => d.flex_1().min_w_0(),
                    })
                    .when_some(c.sort, |d, key| d.cursor_pointer().hover(|s| s.text_color(Theme::text())).on_click(self.on(Cmd::SetSort(filter::toggle_sort(&f.sort, key)), cx)))
                    .when(active, |d| d.text_color(Theme::text()))
                    .child(c.label.clone())
                    .when(active, |d| d.child(ui::icon("chevron-down", 12., Theme::text()).when(!sort_desc, |s| s.with_transformation(Transformation::rotate(percentage(0.5))))))
                    .into_any_element()
            })
            .collect();

        let total = filter::view_count(&lib.papers, View::All, 0);
        let shown = lib.shown.len();
        // Of the papers on show, those without a PDF that have a number to be looked up by.
        let lookable = if lib.filters.view == View::NoPdf { lib.shown.iter().filter(|&&i| wisp_core::openaccess::lookable(&lib.papers[i])).count() } else { 0 };
        let body = if shown == 0 {
            if lib.papers.is_empty() {
                ui::empty_state("library", t("这个文献库还是空的"), t("点击左侧「添加文献」：粘贴 DOI 或 PMID、导入 BibTeX / RIS，或者直接把 PDF 或整个文件夹拖进窗口（子文件夹的名字会成为标签）。")).into_any_element()
            } else {
                ui::empty_state("search", t("没有匹配的文献"), t("调整搜索词或筛选条件。")).child(ui::secondary_button("empty-clear", t("清空筛选")).on_click(self.on(Cmd::ClearFilters, cx))).into_any_element()
            }
        } else {
            uniform_list("papers", shown, cx.processor(move |this, range: Range<usize>, window, cx| this.render_rows(range, width, window, cx))).track_scroll(lib.table_scroll.clone()).flex_1().into_any_element()
        };
        let marked = lib.marked.len();
        let status = if marked > 0 {
            tf!("已选 {marked} 篇 · 右键批量操作 · Esc 取消", "{marked} selected · Right-click for batch actions · Esc to cancel")
        } else if let Some(run) = lib.import.as_ref().filter(|run| run.running()) {
            tf!("正在导入 PDF，已完成 {} / {}", "Importing PDFs, {} / {} complete", run.rows.len(), run.total)
        } else if !lib.extracting.is_empty() {
            tf!("正在提取 {} 篇全文的文字，用于提问…", "Extracting full text from {} papers for chat…", lib.extracting.len())
        } else {
            tf!("↑↓ 切换文献 · Shift / {} 点选多篇 · 右键更多操作 · {} 搜索", "↑↓ Switch paper · Shift / {} to select multiple · Right-click for more · {} Search", if cfg!(target_os = "macos") { "⌘" } else { "Ctrl" }, crate::shortcuts::pretty(&crate::shortcuts::key_of("search", &self.settings.shortcuts)))
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.))
                    .px(px(16.))
                    .pt(px(14.))
                    // 收藏: the starred papers here, the figure library beside them.
                    .when(lib.filters.view == View::Starred, |d| d.child(self.render_collection_switch(cx)))
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w(px(180.))
                            .items_center()
                            .gap(px(8.))
                            .h(px(34.))
                            .px(px(10.))
                            .rounded(px(8.))
                            .bg(Theme::bg_sunken())
                            .border_1()
                            .border_color(if lib.search.focus_handle(cx).is_focused(window) { Theme::accent() } else { Theme::bg_sunken() })
                            .child(ui::icon("search", 15., Theme::text_tertiary()))
                            .child(lib.search.clone()),
                    )
                    .child(ui::secondary_button("sort", t("排序")).child(ui::icon("sort-down", 14., Theme::text_secondary())).on_click(self.on_menu(MenuKind::Sort, cx)))
                    .child(ui::secondary_button("list-translation", if lib.show_translation { t("显示原文") } else { t("显示译文") }).on_click(self.on(Cmd::ToggleTranslation, cx)))
                    .child(ui::secondary_button("export", t("导出")).child(ui::icon("export", 14., Theme::text_secondary())).on_click(self.on_menu(MenuKind::Export, cx))),
            )
            .child(div().id("filters").flex().flex_wrap().items_center().gap(px(6.)).px(px(16.)).py(px(10.)).children(chips))
            .child(div().flex().flex_none().h(px(32.)).px(px(8.)).border_t_1().border_b_1().border_color(Theme::line()).bg(Theme::bg_sunken()).text_size(px(12.)).text_color(Theme::text_tertiary()).children(header))
            // The papers without a PDF: the open ones among them can be fetched in one go.
            .when(lib.filters.view == View::NoPdf && lookable > 0 && lib.fetching.as_ref().is_none_or(|run| !run.running()), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(10.))
                        .px(px(16.))
                        .py(px(8.))
                        .border_b_1()
                        .border_color(Theme::line())
                        .bg(Theme::accent_soft())
                        .child(div().flex_1().min_w_0().text_size(px(12.)).line_height(px(18.)).text_color(Theme::text_secondary()).child(tf!("这些文献还没有 PDF。可以替你找开放获取的全文（arXiv、开放获取期刊、作者存档的版本），下载后附到各自的条目上；要付费或登录的不会去取。查找时会把 DOI 或 PMID 发给 OpenAlex、Europe PMC 和出版商。{}", "Find open access full text from arXiv, journals and author repositories, then attach it to each paper. Paid or login-only files are skipped. DOI or PMID will be sent to OpenAlex, Europe PMC and publishers.{}", if lookable < shown { tf!("另有 {} 篇没有 DOI、PMID 或 arXiv 编号，查不了。", "Another {} papers have no DOI, PMID or arXiv ID and cannot be searched.", shown - lookable) } else { String::new() })))
                        .child(ui::primary_button("find-pdfs", tf!("为 {lookable} 篇查找开放全文", "Find open full text for {lookable} papers")).h(px(30.)).flex_none().on_click(self.on(Cmd::FindPdfs(None), cx))),
                )
            })
            .child(body)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .h(px(30.))
                    .px(px(16.))
                    .border_t_1()
                    .border_color(Theme::line())
                    .text_size(px(12.))
                    .text_color(Theme::text_tertiary())
                    .gap(px(12.))
                    .child(div().flex_none().child(if filtered || shown != total { tf!("显示 {shown} / {total} 篇", "Showing {shown} / {total} papers") } else { tf!("共 {total} 篇", "{total} papers in total") }))
                    .child(div().min_w_0().truncate().child(status)),
            )
            .into_any_element()
    }

    fn render_rows(&mut self, range: Range<usize>, width: f32, _: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(lib) = &self.lib else { return vec![] };
        let columns = Self::columns(lib, width);
        let row_height = self.row_height(false);
        let muted = |text: &str| div().text_color(Theme::text_placeholder()).child(text.to_string());
        range
            .filter_map(|row| {
                let p = lib.papers.get(*lib.shown.get(row)?)?;
                let selected = lib.selected.as_deref() == Some(p.id.as_str()) || lib.marked.contains(&p.id);
                // The paper a right-click menu is about.
                let pointed = self.menu.as_ref().is_some_and(|menu| menu.paper.as_deref() == Some(p.id.as_str()));
                let cells: Vec<AnyElement> = columns
                    .iter()
                    .map(|c| {
                        let cell = div().flex().items_center().h_full().overflow_hidden().px(px(8.)).map(|d| match c.width {
                            Some(w) => d.w(px(w)).flex_none(),
                            None => d.flex_1().min_w_0(),
                        });
                        match &c.kind {
                            ColumnKind::Star => cell
                                .px(px(4.))
                                .child(
                                    div()
                                        .id(("star", row))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .size(px(26.))
                                        .rounded(px(6.))
                                        .cursor_pointer()
                                        .hover(|s| s.bg(Theme::hover()))
                                        .on_click({
                                            let toggle = self.on(Cmd::ToggleStar(p.id.clone()), cx);
                                            move |ev, window, cx| {
                                                cx.stop_propagation();
                                                toggle(ev, window, cx);
                                            }
                                        })
                                        .child(ui::icon("star", 15., if p.starred { Theme::star() } else { Theme::text_placeholder() })),
                                )
                                .into_any_element(),
                            ColumnKind::Title => {
                                let title = self.listed_title(lib, p);
                                let mut meta = vec![];
                                let authors = p.short_authors();
                                if !authors.is_empty() { meta.push(authors) }
                                if let Some(year) = p.year { meta.push(year.to_string()) }
                                if !p.journal.is_empty() { meta.push(p.journal.clone()) }
                                cell.child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .min_w_0()
                                        .w_full()
                                        .child(div().truncate().font_weight(if p.reading == "unread" { FontWeight::MEDIUM } else { FontWeight::NORMAL }).text_color(if p.reading == "unread" { Theme::text() } else { Theme::text_secondary() }).child(title.clone()))
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap(px(6.))
                                                .text_size(px(12.))
                                                .line_height(px(18.))
                                                .text_color(Theme::text_tertiary())
                                                .child(div().min_w_0().truncate().child(if meta.is_empty() { t("作者、年份待补").to_string() } else { meta.join(" · ") }))
                                                .when(p.publication_status == "preprint", |d| d.child(div().flex_none().text_color(theme::status_colors("preprint").0).child(t("预印本"))))
                                                .children(p.tags.iter().take(2).map(|t| div().flex_none().text_color(Theme::accent()).child(format!("#{t}")))),
                                        ),
                                )
                                .into_any_element()
                            }
                            ColumnKind::Journal => cell.child(if p.journal.is_empty() { muted("—") } else { div().truncate().text_color(Theme::text_secondary()).child(p.journal.clone()) }).into_any_element(),
                            ColumnKind::Year => cell.child(match p.year {
                                Some(year) => div().text_color(Theme::text_secondary()).child(year.to_string()),
                                None => muted("—"),
                            })
                            .into_any_element(),
                            ColumnKind::Field(id) => cell
                                .child(match p.fields.get(id) {
                                    Some(values) => div().truncate().text_color(Theme::text_secondary()).child(values.join("、")),
                                    None => muted("—"),
                                })
                                .into_any_element(),
                            ColumnKind::Fulltext => cell.child(ui::status_chip(&p.fulltext, label(&FULLTEXTS, &p.fulltext))).into_any_element(),
                            ColumnKind::Reading => cell.child(ui::status_chip(&p.reading, label(&READINGS, &p.reading))).into_any_element(),
                            ColumnKind::Screening => cell.child(ui::status_chip(&p.screening, label(&SCREENINGS, &p.screening))).into_any_element(),
                        }
                    })
                    .collect();
                Some(
                    div()
                        .id(("row", row))
                        .flex()
                        .items_center()
                        .h(px(row_height))
                        .w(px(width))
                        .overflow_hidden()
                        .px(px(8.))
                        .border_b_1()
                        .border_color(Theme::line())
                        .cursor_pointer()
                        .when(selected, |d| d.bg(Theme::accent_soft()))
                        .when(!selected && pointed, |d| d.bg(Theme::hover()))
                        .when(!selected && !pointed, |d| d.hover(|s| s.bg(Theme::hover())))
                        .on_mouse_down(MouseButton::Right, self.on_paper_context(p.id.clone(), cx))
                        .on_click(cx.listener({
                            let id = p.id.clone();
                            move |this, ev: &ClickEvent, window, cx| this.row_clicked(&id, ev, window, cx)
                        }))
                        .children(cells)
                        .into_any_element(),
                )
            })
            .collect()
    }

    /// A paper's title as the lists show it: translated while translations are on show and this one is there.
    fn listed_title<'a>(&'a self, lib: &'a Library, p: &'a Paper) -> &'a String {
        if !lib.show_translation { return &p.title }
        match lib.translations.get(&wisp_core::store::translation_key(&p.title, wisp_core::ai::translation_prompt(&self.settings.translation_prompt))) {
            Some(Translated::Done(text)) => text,
            _ => &p.title,
        }
    }

    /// A click on a paper's row, in the table or in the narrow list beside a paper that is asked about.
    fn row_clicked(&mut self, id: &str, ev: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        // With ⌘ / Ctrl or Shift held the click marks papers; alone it shows one.
        let held = ev.modifiers();
        if held.secondary() || held.shift { return self.mark(id, held.shift, window, cx) }
        if let Some(lib) = self.lib.as_mut() { (lib.anchor, lib.cursor) = (Some(id.to_string()), None) }
        // A double click opens the PDF straight away; beside a paper that is asked about, its PDF is on show already.
        if ev.click_count() >= 2 && !self.asking(window) && let Some(lib) = self.lib.as_mut() && lib.pdf_file(id).is_some() { lib.tab = Tab::Pdf }
        self.run(Cmd::Select(id.to_string()), window, cx);
        self.run(Cmd::SetTab(self.lib.as_ref().map_or(Tab::Info, |l| l.tab)), window, cx);
        // The keyboard stays with the list, for the arrow keys; a question about the paper keeps its box.
        if self.lib.as_ref().is_some_and(|l| l.tab != Tab::Ask) { window.focus(&self.focus) }
    }

    /// The papers on show as a narrow list, at the left of a paper that is asked about: the way
    /// back at the top where the sidebar's corner is, the search, then a title and a line for each.
    fn render_ask_list(&mut self, width: f32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let lib = self.lib.as_ref().unwrap();
        let (shown, total) = (lib.shown.len(), filter::view_count(&lib.papers, View::All, 0));
        let filtered = lib.filters.active() > 0 || !lib.search.read(cx).text().trim().is_empty();
        let body = if shown == 0 {
            div().flex_1().px(px(8.)).pt(px(8.)).text_size(px(12.)).line_height(px(18.)).text_color(Theme::text_placeholder()).child(t("没有匹配的文献。换个搜索词，或按 Esc 回到完整列表调整筛选。")).into_any_element()
        } else {
            uniform_list("ask-papers", shown, cx.processor(move |this, range: Range<usize>, _, cx| this.render_ask_rows(range, cx))).track_scroll(lib.table_scroll.clone()).flex_1().into_any_element()
        };
        // The edge between the list and the paper is drawn by the detail panel, which is where it is dragged.
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(width))
            .h_full()
            .px(px(10.))
            .pb(px(8.))
            .pt(px(Self::corner_top()))
            .gap(px(8.))
            .child(self.render_corner(width >= ui::traffic_lights_width() + 150., cx).when(ui::custom_titlebar(), |d| d.mb(px(4.))))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.))
                    .h(px(32.))
                    .px(px(10.))
                    .rounded(px(8.))
                    .bg(Theme::bg_body())
                    .border_1()
                    .border_color(if lib.search.focus_handle(cx).is_focused(window) { Theme::accent() } else { Theme::line() })
                    .child(ui::icon("search", 14., Theme::text_tertiary()))
                    .child(lib.search.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .px(px(8.))
                    .child(ui::caption(if lib.filters.tag.is_empty() { view_label(lib.filters.view).to_string() } else { format!("{} · #{}", view_label(lib.filters.view), lib.filters.tag) }).min_w_0().truncate())
                    .child(div().flex_none().text_size(px(11.)).text_color(Theme::text_tertiary()).child(if filtered || shown != total { tf!("{shown} / {total} 篇", "{shown} / {total} papers") } else { tf!("{total} 篇", "{total} papers") })),
            )
            .child(body)
            .child(div().flex_none().px(px(8.)).truncate().text_size(px(11.)).text_color(Theme::text_placeholder()).child(tf!("↑↓ 切换文献 · {} 回到完整列表", "↑↓ Switch paper · {} Back to full list", crate::shortcuts::pretty("escape"))))
            .into_any_element()
    }

    fn render_ask_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(lib) = &self.lib else { return vec![] };
        let row_height = self.row_height(true);
        let compact = self.settings.density == "compact";
        range
            .filter_map(|row| {
                let p = lib.papers.get(*lib.shown.get(row)?)?;
                let selected = lib.selected.as_deref() == Some(p.id.as_str()) || lib.marked.contains(&p.id);
                let pointed = self.menu.as_ref().is_some_and(|menu| menu.paper.as_deref() == Some(p.id.as_str()));
                let unread = p.reading == "unread";
                let mut meta = vec![];
                let authors = p.short_authors();
                if !authors.is_empty() { meta.push(authors) }
                if let Some(year) = p.year { meta.push(year.to_string()) }
                if meta.is_empty() && !p.journal.is_empty() { meta.push(p.journal.clone()) }
                Some(
                    // A row of the list's own height, and inside it the rounded piece that is pointed at, as in the sidebar.
                    div()
                        .id(("ask-row", row))
                        .h(px(row_height))
                        .w_full()
                        .py(px(1.))
                        .cursor_pointer()
                        .on_mouse_down(MouseButton::Right, self.on_paper_context(p.id.clone(), cx))
                        .on_click(cx.listener({
                            let id = p.id.clone();
                            move |this, ev: &ClickEvent, window, cx| this.row_clicked(&id, ev, window, cx)
                        }))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .justify_center()
                                .size_full()
                                .px(px(8.))
                                .rounded(px(6.))
                                .when(selected, |d| d.bg(Theme::accent_soft()))
                                .when(!selected && pointed, |d| d.bg(Theme::hover()))
                                .when(!selected && !pointed, |d| d.hover(|s| s.bg(Theme::hover())))
                                .child(div().w_full().text_size(px(13.)).line_height(px(19.)).map(|d| if compact { d.truncate() } else { d.line_clamp(2) }).font_weight(if unread || selected { FontWeight::MEDIUM } else { FontWeight::NORMAL }).text_color(if selected { Theme::accent() } else if unread { Theme::text() } else { Theme::text_secondary() }).child(self.listed_title(lib, p).clone()))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(6.))
                                        .text_size(px(12.))
                                        .line_height(px(18.))
                                        .text_color(Theme::text_tertiary())
                                        .child(div().min_w_0().truncate().child(if meta.is_empty() { t("作者、年份待补").to_string() } else { meta.join(" · ") }))
                                        .when(p.starred, |d| d.child(ui::icon("star", 11., Theme::star())))
                                        // Asked about without its PDF, the answer has only the record to go by.
                                        .when(lib.pdf_file(&p.id).is_none(), |d| d.child(div().flex_none().text_color(Theme::text_placeholder()).child(t("无 PDF")))),
                                ),
                        )
                        .into_any_element(),
                )
            })
            .collect()
    }

    fn render_trash(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let lib = self.lib.as_ref().unwrap();
        let rows: Vec<AnyElement> = lib
            .trash
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let p = &entry.paper;
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .px(px(16.))
                    .py(px(10.))
                    .border_b_1()
                    .border_color(Theme::line())
                    .on_mouse_down(MouseButton::Right, self.on_context(MenuKind::Trashed(p.id.clone()), cx))
                    .child(div().flex().flex_col().flex_1().min_w_0().child(div().truncate().child(p.title.clone())).child(div().truncate().text_size(px(12.)).text_color(Theme::text_tertiary()).child(tf!("{} · 删除于 {}", "{} · Deleted {}", if p.authors.is_empty() { t("作者待补").to_string() } else { p.short_authors() }, time::date_text(&entry.deleted_at)))))
                    .child(ui::secondary_button(("restore", i), t("恢复")).on_click(self.on(Cmd::RestorePaper(p.id.clone()), cx)))
                    .child(ui::ghost_button(("delete", i), Some("bin"), Some(t("彻底删除").into())).text_color(Theme::danger()).on_click(self.on(Cmd::AskDeletePaper(p.id.clone()), cx)))
                    .into_any_element()
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(div().px(px(16.)).pt(px(16.)).pb(px(10.)).child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(t("回收站"))).child(div().text_size(px(12.)).text_color(Theme::text_tertiary()).child(t("这里的文献保留着笔记、词条和 PDF，可以恢复。它们仍会阻止同一篇文献被重复导入。"))))
            .child(if rows.is_empty() { ui::empty_state("bin", t("回收站是空的"), "").into_any_element() } else { div().id("trash").flex().flex_col().flex_1().min_h_0().overflow_y_scroll().border_t_1().border_color(Theme::line()).children(rows).into_any_element() })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that would bring in gpui's own `test` attribute.
    use super::{ASK_LIST_MAX, ASK_LIST_MIN, ASK_LIST_W, SPLIT_CHAT_MIN, SPLIT_PDF_MIN, ask_list_width, split_chat_width};
    use crate::detail::SPLIT_MIN;

    #[test]
    fn list_beside_a_paper_asked_about() {
        // No wish: the width it comes with, where the window has room for it.
        assert_eq!(ask_list_width(0., 1360.), ASK_LIST_W);
        // The conversation and the PDF keep their least: the list gives way, down to none.
        assert_eq!(ask_list_width(0., SPLIT_MIN + 240.), 240.);
        assert_eq!(ask_list_width(0., SPLIT_MIN + ASK_LIST_MIN), ASK_LIST_MIN);
        assert_eq!(ask_list_width(0., SPLIT_MIN + ASK_LIST_MIN - 1.), 0.);
        // A wish is kept within the list's own bounds.
        assert_eq!((ask_list_width(340., 1360.), ask_list_width(40., 1360.), ask_list_width(900., 1600.)), (340., ASK_LIST_MIN, ASK_LIST_MAX));
    }

    #[test]
    fn conversation_beside_pdf() {
        // No wish: a good share of the panel, within its own bounds.
        assert_eq!((split_chat_width(0., 760.), split_chat_width(0., 1000.), split_chat_width(0., 1600.)), (360., 450., 520.));
        // A wish is kept where the panel has room, and the PDF keeps its least.
        assert_eq!(split_chat_width(600., 1600.), 600.);
        assert_eq!(split_chat_width(900., 1000.), 1000. - SPLIT_PDF_MIN);
        assert_eq!(split_chat_width(40., 1000.), SPLIT_CHAT_MIN);
        // The narrowest panel that shows both gives each its least.
        assert!(split_chat_width(2000., crate::detail::SPLIT_MIN) >= SPLIT_CHAT_MIN);
    }
}
