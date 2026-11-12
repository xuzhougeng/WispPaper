//! The window's frame (menus, dialogs and toasts over the page) and the
//! home page: one card per library.

use crate::app::{Cmd, Edge, MenuKind, Workspace};
use crate::context::Nav;
use crate::settings::{SettingsPage, UpdateState};
use crate::shortcuts::{MarkAll, MarkDown, MarkUp, MoveDown, MoveFirst, MoveLast, MoveLeft, MovePageDown, MovePageUp, MoveRight, MoveUp, OpenSelected, TrashSelected, AddPaper, AskLibrary, FocusSearch, ImportPdf, NewLibrary, ShowHome, ShowSettings, CopyPicked, FindCandidates, ToggleDetail, ToggleReading, ToggleSidebar, ZoomFit, ZoomIn, ZoomOut};
use crate::theme::{self, Theme};
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use wisp_core::time;

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = if self.settings_view.is_some() {
            self.render_settings(window, cx)
        } else if self.lib.is_some() {
            self.render_library(window, cx)
        } else {
            self.render_home(cx)
        };
        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .bg(Theme::bg_window())
            .font_family(theme::UI_FONT)
            .text_size(px(13.))
            .line_height(px(20.))
            .text_color(Theme::text())
            // Only while 设置 › 快捷键 waits for a new key.
            .on_key_down(cx.listener(Self::record_key))
            .on_action(cx.listener(Self::dismiss))
            .on_action(cx.listener(|this, _: &ShowSettings, window, cx| this.shortcut("settings", window, cx)))
            .on_action(cx.listener(|this, _: &ShowHome, window, cx| this.shortcut("home", window, cx)))
            .on_action(cx.listener(|this, _: &NewLibrary, window, cx| this.shortcut("new-library", window, cx)))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| this.shortcut("search", window, cx)))
            .on_action(cx.listener(|this, _: &AddPaper, window, cx| this.shortcut("add", window, cx)))
            .on_action(cx.listener(|this, _: &ImportPdf, window, cx| this.shortcut("import-pdf", window, cx)))
            .on_action(cx.listener(|this, _: &AskLibrary, window, cx| this.shortcut("ask", window, cx)))
            .on_action(cx.listener(|this, _: &ToggleDetail, window, cx| this.shortcut("detail", window, cx)))
            .on_action(cx.listener(|this, _: &ToggleSidebar, window, cx| this.shortcut("sidebar", window, cx)))
            .on_action(cx.listener(|this, _: &ToggleReading, window, cx| this.shortcut("reading", window, cx)))
            .on_action(cx.listener(|this, _: &ZoomIn, window, cx| this.shortcut("zoom-in", window, cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, window, cx| this.shortcut("zoom-out", window, cx)))
            .on_action(cx.listener(|this, _: &ZoomFit, window, cx| this.shortcut("zoom-fit", window, cx)))
            .on_action(cx.listener(|this, _: &CopyPicked, window, cx| this.shortcut("copy-picked", window, cx)))
            .on_action(cx.listener(|this, _: &FindCandidates, window, cx| this.shortcut("write-find", window, cx)))
            .on_action(cx.listener(|this, _: &MoveUp, window, cx| this.nav(Nav::Up, window, cx)))
            .on_action(cx.listener(|this, _: &MoveDown, window, cx| this.nav(Nav::Down, window, cx)))
            .on_action(cx.listener(|this, _: &MoveLeft, window, cx| this.nav(Nav::Left, window, cx)))
            .on_action(cx.listener(|this, _: &MoveRight, window, cx| this.nav(Nav::Right, window, cx)))
            .on_action(cx.listener(|this, _: &MovePageUp, window, cx| this.nav(Nav::PageUp, window, cx)))
            .on_action(cx.listener(|this, _: &MovePageDown, window, cx| this.nav(Nav::PageDown, window, cx)))
            .on_action(cx.listener(|this, _: &MoveFirst, window, cx| this.nav(Nav::First, window, cx)))
            .on_action(cx.listener(|this, _: &MoveLast, window, cx| this.nav(Nav::Last, window, cx)))
            .on_action(cx.listener(|this, _: &MarkUp, window, cx| this.nav(Nav::MarkUp, window, cx)))
            .on_action(cx.listener(|this, _: &MarkDown, window, cx| this.nav(Nav::MarkDown, window, cx)))
            .on_action(cx.listener(|this, _: &MarkAll, window, cx| this.nav(Nav::MarkAll, window, cx)))
            .on_action(cx.listener(|this, _: &OpenSelected, window, cx| this.nav(Nav::Open, window, cx)))
            .on_action(cx.listener(|this, _: &TrashSelected, window, cx| this.nav(Nav::Trash, window, cx)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.import_paths(paths.paths().to_vec(), cx)))

            .child(page)
            .children(self.render_modal(window, cx))
            .children(self.render_menu(cx))
            .children(self.render_toast())
            // While an edge is held (the sidebar's, the detail panel's, the one between a conversation and its PDF), a layer over everything hears the pointer
            // wherever it goes: an element only hears the moves made over it.
            .children(self.resizing.map(|edge| {
                div()
                    .id("resize-layer")
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .occlude()
                    .cursor_col_resize()
                    .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, window, cx| match edge {
                        Edge::Sidebar => this.drag_sidebar(event, window, cx),
                        Edge::Detail => this.drag_detail(event, window, cx),
                        Edge::Split => this.drag_split(event, window, cx),
                    }))
                    .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.end_drag(cx)))
            }))
            // The same while a stretch of text is being selected (select.rs).
            .children(self.picked.as_ref().filter(|picked| picked.held).map(|_| {
                div()
                    .id("pick-layer")
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .occlude()
                    .cursor_text()
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| this.pick_drag(event, cx)))
                    .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.pick_end(cx)))
            }))
            .children(ui::window_controls(window))
    }
}

impl Workspace {
    fn render_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.menu.as_ref()?;
        let mut rows: Vec<AnyElement> = vec![];
        for (i, item) in menu.items.iter().enumerate() {
            // A line between two groups of a longer menu.
            if menu.breaks.contains(&i) { rows.push(div().flex_none().h(px(1.)).mx(px(6.)).my(px(4.)).bg(Theme::line()).into_any_element()) }
            rows.push(ui::menu_row(("menu", i), item.label.clone(), item.checked, item.note.clone()).when(menu.active == Some(i), |d| d.bg(Theme::hover())).on_click(self.on(item.cmd.clone(), cx)).into_any_element());
        }
        let at = menu.at;
        let close = |this: &mut Self, _: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>| {
            this.menu = None;
            cx.notify();
        };
        Some(
            div()
                .id("menu-layer")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .occlude()
                .on_mouse_down(MouseButton::Left, cx.listener(close))
                .on_mouse_down(MouseButton::Right, cx.listener(close))
                .child(anchored().position(at).snap_to_window_with_margin(px(8.)).child(ui::popover().id("menu").max_h(px(520.)).overflow_y_scroll().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation()).children(rows)))
                .into_any_element(),
        )
    }

    fn render_toast(&self) -> Option<AnyElement> {
        let toast = self.toast.as_ref()?;
        let (mark, color) = if toast.error { ("x", Theme::danger()) } else { ("check", Theme::success()) };
        Some(
            div()
                .absolute()
                .top(px(18.))
                .left_0()
                .w_full()
                .flex()
                .justify_center()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .max_w(px(640.))
                        .px(px(14.))
                        .py(px(9.))
                        .rounded(px(10.))
                        .bg(Theme::bg_body())
                        .border_1()
                        .border_color(Theme::line())
                        .shadow(theme::shadow_panel())
                        .child(ui::icon(mark, 14., color))
                        .child(div().min_w_0().child(toast.message.clone())),
                )
                .into_any_element(),
        )
    }

    fn render_home(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let cards: Vec<AnyElement> = self
            .libraries
            .iter()
            .map(|info| {
                let accent = theme::accent_of(&info.color);
                let topic = if info.topic.is_empty() { "还没有写主题描述".to_string() } else { info.topic.clone() };
                div()
                    .id(SharedString::from(format!("library-{}", info.id)))
                    .flex()
                    .flex_col()
                    .w(px(288.))
                    .h(px(168.))
                    .rounded(px(12.))
                    .bg(Theme::bg_body())
                    .border_1()
                    .border_color(Theme::line())
                    .shadow(theme::shadow_card())
                    .overflow_hidden()
                    .cursor_pointer()
                    .hover(|s| s.border_color(accent.strong()))
                    .on_click(self.on(Cmd::OpenLibrary(info.id.clone()), cx))
                    .on_mouse_down(MouseButton::Right, self.on_context(MenuKind::LibraryCard(info.id.clone()), cx))
                    .child(div().h(px(6.)).w_full().rounded_t(px(11.)).bg(accent.strong()))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h_0()
                            .p(px(16.))
                            .gap(px(6.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(div().flex().flex_none().items_center().justify_center().size(px(28.)).rounded(px(8.)).bg(accent.soft()).child(ui::icon("library", 16., accent.strong())))
                                    .child(div().flex_1().min_w_0().truncate().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(info.name.clone()))
                                    .child(ui::ghost_button(SharedString::from(format!("library-menu-{}", info.id)), Some("ellipsis"), None).on_click({
                                        let open = self.on_menu(MenuKind::LibraryCard(info.id.clone()), cx);
                                        move |ev, window, cx| {
                                            cx.stop_propagation();
                                            open(ev, window, cx);
                                        }
                                    })),
                            )
                            .child(div().flex_1().min_h_0().overflow_hidden().line_clamp(3).text_size(px(12.)).line_height(px(18.)).text_color(if info.topic.is_empty() { Theme::text_placeholder() } else { Theme::text_secondary() }).child(topic))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .text_size(px(12.))
                                    .text_color(Theme::text_tertiary())
                                    .child(format!("{} 篇文献", info.paper_count))
                                    .child(format!("打开于 {}", time::date_text(&info.opened_at))),
                            ),
                    )
                    .into_any_element()
            })
            .collect();
        let new_card = div()
            .id("library-new")
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.))
            .w(px(288.))
            .h(px(168.))
            .rounded(px(12.))
            .border_1()
            .border_dashed()
            .border_color(Theme::line_strong())
            .text_color(Theme::text_secondary())
            .cursor_pointer()
            .hover(|s| s.bg(Theme::hover()))
            .on_click(self.on(Cmd::NewLibrary, cx))
            .child(ui::icon("plus", 20., Theme::text_secondary()))
            .child("新建文献库");
        let empty = self.libraries.is_empty();
        let new_version = match &self.update {
            UpdateState::Available(release) => Some(release.version.clone()),
            _ => None,
        };
        let page = div()
            .id("home")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .track_scroll(&self.home_scroll)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .max_w(px(1280.))
                    .mx_auto()
                    .px(px(40.))
                    .py(px(32.))
                    .gap(px(24.))
                    .child(
                        div()
                            .flex()
                            .items_end()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.))
                                    .child(div().text_size(px(24.)).line_height(px(32.)).font_weight(FontWeight::SEMIBOLD).child("Wisp Paper"))
                                    .child(div().text_color(Theme::text_secondary()).child("每个文献库是一个独立的主题：自己的文献、PDF、词条和提问。")),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap(px(4.))
                                    // Only while a newer release is on offer.
                                    .when_some(new_version, |d, version| d.child(ui::primary_button("home-update", format!("新版本 {version}")).h(px(28.)).px(px(10.)).rounded_full().text_size(px(12.)).on_click(self.on(Cmd::OpenSettings(SettingsPage::About), cx))))
                                    .child(ui::ghost_button("home-folder", Some("folder-open"), Some("数据目录".into())).on_click(self.on(Cmd::RevealFolder, cx)))
                                    .child(ui::ghost_button("home-settings", Some("settings-gear"), Some("设置".into())).on_click(self.on(Cmd::OpenSettings(SettingsPage::General), cx))),
                            ),
                    )
                    .when(empty, |d| {
                        d.child(div().p(px(16.)).rounded(px(10.)).bg(Theme::accent_soft()).text_color(Theme::text_secondary()).child("还没有文献库。先为一个研究主题新建文献库，再往里添加文献：粘贴 DOI、拖入 PDF，或导入 BibTeX / RIS。"))
                    })
                    .child(div().flex().flex_wrap().gap(px(16.)).children(cards).child(new_card)),
            )
            ;
        div().flex().flex_col().size_full().child(ui::title_strip()).child(page).into_any_element()
    }
}
