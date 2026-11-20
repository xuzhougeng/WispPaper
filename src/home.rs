//! The window's frame (menus, dialogs and toasts over the page) and the
//! home page: one card per library.

use crate::i18n::t;
use crate::app::{Cmd, Edge, MenuKind, Workspace};
use crate::context::Nav;
use crate::settings::{SettingsPage, UpdateState};
use crate::shortcuts::{AskAssistant, DecideIncluded, DecideIrrelevant, DecidePending, DecideRelated, DecideUncertain, StarScreened, TakeSuggestion, TickSuggestion, TranslateScreened, UndoDecision, MarkAll, MarkDown, MarkUp, MoveDown, MoveFirst, MoveLast, MoveLeft, MovePageDown, MovePageUp, MoveRight, MoveUp, OpenSelected, TrashSelected, AddPaper, AskLibrary, FocusSearch, ImportPdf, NewLibrary, ShowHome, ShowPalette, ShowSettings, CopyPicked, FindCandidates, ToggleDetail, ToggleReading, ToggleSidebar, ZoomFit, ZoomIn, ZoomOut};
use crate::theme::{self, Theme, text_px};
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use wisp_core::{libraries, time};

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.filters_drawn = false;
        let page = if self.settings_view.is_some() {
            self.render_settings(window, cx)
        } else if self.lib.is_some() {
            self.render_library(window, cx)
        } else if self.home.open {
            // The assistant's page with nothing said yet: the box of a first question and the conversations (`draft.rs`).
            self.render_start_page(window, cx)
        } else {
            self.render_home(cx)
        };
        // An element says only when the pointer comes or goes: once the folded filters are not drawn, nothing would say it went.
        if !self.filters_drawn { self.filters_over = [false; 2] }
        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .bg(Theme::bg_window())
            .font(theme::ui_font())
            .text_size(text_px(13.))
            .line_height(text_px(20.))
            .text_color(Theme::text())
            // Only while 设置 › 快捷键 waits for a new key.
            .on_key_down(cx.listener(Self::record_key))
            // A press anywhere else ends the selection in an answer, and the bar beside it goes with it (one in an answer is begun before this hears of it).
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                if this.lib.as_mut().is_some_and(crate::app::Library::unpick) { cx.notify() }
            }))
            .on_action(cx.listener(Self::dismiss))
            .on_action(cx.listener(|this, _: &ShowPalette, window, cx| this.shortcut("palette", window, cx)))
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
            // The keys of 筛选 (screen_single.rs).
            .on_action(cx.listener(|this, _: &DecideIncluded, window, cx| this.screen_key("y", window, cx)))
            .on_action(cx.listener(|this, _: &DecideRelated, window, cx| this.screen_key("r", window, cx)))
            .on_action(cx.listener(|this, _: &DecideUncertain, window, cx| this.screen_key("u", window, cx)))
            .on_action(cx.listener(|this, _: &DecideIrrelevant, window, cx| this.screen_key("n", window, cx)))
            .on_action(cx.listener(|this, _: &DecidePending, window, cx| this.screen_key("p", window, cx)))
            .on_action(cx.listener(|this, _: &TakeSuggestion, window, cx| this.screen_key("a", window, cx)))
            .on_action(cx.listener(|this, _: &StarScreened, window, cx| this.screen_key("s", window, cx)))
            .on_action(cx.listener(|this, _: &TranslateScreened, window, cx| this.screen_key("t", window, cx)))
            .on_action(cx.listener(|this, _: &UndoDecision, window, cx| this.screen_key("z", window, cx)))
            .on_action(cx.listener(|this, _: &AskAssistant, window, cx| this.screen_key("g", window, cx)))
            .on_action(cx.listener(|this, _: &TickSuggestion, window, cx| this.screen_key("space", window, cx)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.import_paths(paths.paths().to_vec(), cx)))

            .child(page)
            .children(self.render_modal(window, cx))
            .children(self.render_palette(cx))
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
                        Edge::Manuscript => this.drag_manuscript(event, window, cx),
                        Edge::Column => this.drag_column(event, cx),
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
                    .on_mouse_up(MouseButton::Left, cx.listener(|this, event, _, cx| this.pick_release(event, cx)))
            }))
            // The same while the floating conversation is held by its title bar, its corner or its button (floating.rs).
            .children(self.render_float_layer(cx))
            .children(ui::window_controls(window))
    }
}

/// A library's card on its way to another place on the home page: what is held, and the card drawn under the pointer meanwhile.
#[derive(Clone)]
pub struct LibraryDrag {
    id: String,
    name: String,
    color: String,
    /// Where in its card the pointer took hold of it.
    held: Point<Pixels>,
}

impl Render for LibraryDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let accent = theme::accent_of(&self.color);
        // What is dragged is drawn from the card's corner: the smaller card is moved to the pointer.
        div().pl((self.held.x - px(16.)).max(px(0.))).pt((self.held.y - px(16.)).max(px(0.))).child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .w(px(220.))
                .h(text_px(40.))
                .px(px(10.))
                .rounded(px(10.))
                .border_1()
                .border_color(accent.strong())
                .bg(Theme::bg_body())
                .shadow(theme::shadow_panel())
                .font(theme::ui_font())
                .text_size(text_px(13.))
                .text_color(Theme::text())
                .child(div().flex().flex_none().items_center().justify_center().size(px(24.)).rounded(px(6.)).bg(accent.soft()).child(ui::icon("library", 14., accent.strong())))
                .child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).child(self.name.clone())),
        )
    }
}

/// How far the assistant's button stands from the window's bottom right corner.
const CORNER_GAP: f32 = 24.;

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
        // The conversations not yet made libraries are the assistant's, behind the button in the corner, not among the libraries.
        // In the order the person dragged them to, the rest as they were made (`libraries::shelf`); opening one does not move it.
        let cards: Vec<AnyElement> = libraries::shelf(&self.libraries, &self.settings.library_order)
            .into_iter()
            .map(|info| {
                let accent = theme::accent_of(&info.color);
                // Three lines of the card hold about this much; a longer topic ends in 「…」 on a whole character instead of being clipped mid-glyph.
                let topic = if info.topic.is_empty() { t("还没有写主题描述").to_string() } else if info.topic.chars().count() > 60 { format!("{}…", info.topic.chars().take(58).collect::<String>().trim_end()) } else { info.topic.clone() };
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
                    // Dragged onto another card, it takes that card's place; a press that does not move is still a click.
                    .on_drag(LibraryDrag { id: info.id.clone(), name: info.name.clone(), color: info.color.clone(), held: Point::default() }, |drag, held, _, cx| cx.new(|_| LibraryDrag { held, ..drag.clone() }))
                    .drag_over::<LibraryDrag>(move |style, _, _, _| style.border_color(accent.strong()).bg(accent.soft()))
                    .on_drop(cx.listener({
                        let onto = info.id.clone();
                        move |this, drag: &LibraryDrag, window, cx| this.run(Cmd::MoveLibrary(drag.id.clone(), onto.clone()), window, cx)
                    }))
                    // A 6px quad clamps its radius to 3px and the card clips only to a rectangle, so the strip crops a taller quad that keeps the card's inner curve.
                    .child(div().flex_none().h(px(6.)).w_full().overflow_hidden().child(div().h(px(22.)).w_full().rounded_t(px(11.)).bg(accent.strong())))
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
                                    .child(div().flex_1().min_w_0().truncate().text_size(text_px(15.)).font_weight(FontWeight::SEMIBOLD).child(info.name.clone()))
                                    .child(ui::ghost_button(SharedString::from(format!("library-menu-{}", info.id)), Some("ellipsis"), None).on_click({
                                        let open = self.on_menu(MenuKind::LibraryCard(info.id.clone()), cx);
                                        move |ev, window, cx| {
                                            cx.stop_propagation();
                                            open(ev, window, cx);
                                        }
                                    })),
                            )
                            .child(div().flex_1().min_h_0().overflow_hidden().line_clamp(3).text_size(text_px(12.)).line_height(text_px(18.)).text_color(if info.topic.is_empty() { Theme::text_placeholder() } else { Theme::text_secondary() }).child(topic))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .text_size(text_px(12.))
                                    .text_color(Theme::text_tertiary())
                                    .child(tf!("{} 篇文献", "{} papers", info.paper_count))
                                    // Left while its assistant was answering, it goes on: said in place of when it was opened.
                                    .child(match self.off_screen_mark(&info.id) {
                                        Some(mark) => mark.into_any_element(),
                                        None => div().child(tf!("打开于 {}", "Opened {}", time::date_text(&info.opened_at))).into_any_element(),
                                    }),
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
            .child(t("新建文献库"));
        let empty = self.libraries.iter().all(|info| info.draft);
        let new_version = match &self.update {
            UpdateState::Available(release, _) | UpdateState::Downloading(release) | UpdateState::Ready(release, ..) => Some(release.version.clone()),
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
                                    .child(div().text_size(text_px(24.)).line_height(text_px(32.)).font_weight(FontWeight::SEMIBOLD).child("Wisp Study"))
                                    .child(div().text_color(Theme::text_secondary()).child(t("每个文献库是一个独立的主题：自己的文献、PDF、词条和提问。"))),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap(px(4.))
                                    // Only while a newer release is on offer.
                                    .when_some(new_version, |d, version| d.child(ui::primary_button("home-update", tf!("新版本 {version}", "New version {version}")).h(text_px(28.)).px(px(10.)).rounded_full().text_size(text_px(12.)).on_click(self.on(Cmd::OpenSettings(SettingsPage::About), cx))))
                                    .child(ui::ghost_button("home-import", Some("library"), Some(t("导入文献库").into())).on_click(self.on(Cmd::ImportLibrary, cx)))
                                    .child(ui::ghost_button("home-folder", Some("folder-open"), Some(t("数据目录").into())).on_click(self.on(Cmd::RevealFolder, cx)))
                                    .child(ui::ghost_button("home-settings", Some("settings-gear"), Some(t("设置").into())).on_click(self.on(Cmd::OpenSettings(SettingsPage::General), cx))),
                            ),
                    )
                    .when(empty, |d| {
                        d.child(div().p(px(16.)).rounded(px(10.)).bg(Theme::accent_soft()).text_color(Theme::text_secondary()).child(t("还没有文献库。先为一个研究主题新建文献库，再往里添加文献：粘贴 DOI、拖入 PDF，或导入 BibTeX / RIS。")))
                    })
                    .child(div().flex().flex_wrap().gap(px(16.)).children(cards).child(new_card)),
            )
            ;
        // The assistant: one button in the window's corner, drawn as the floating conversation's is; it does not scroll with the cards.
        let assistant = div()
            .id("home-assistant")
            .flex()
            .items_center()
            .justify_center()
            .size(px(wisp_core::floating::BUTTON))
            .rounded_full()
            .bg(Theme::accent())
            .cursor_pointer()
            .hover(|s| s.opacity(0.9))
            .tooltip(ui::tip(t("助手")))
            .on_click(self.on(Cmd::Draft(crate::draft::Do::Begin), cx))
            .child(ui::icon("sparkle", 20., Theme::on_accent()));
        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .child(ui::title_strip())
            .child(page)
            .child(div().absolute().right(px(CORNER_GAP)).bottom(px(CORNER_GAP)).rounded_full().shadow(theme::shadow_panel()).child(assistant))
            .into_any_element()
    }
}
