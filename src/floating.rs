//! The paper's conversation floating over its PDF (`Settings::floating`):
//! the PDF has the whole of the panel, the conversation is a window over it
//! that is moved by its title bar, sized by its bottom-right corner and
//! folded up into a round button. It is the same `Chat` and the same input
//! as beside the PDF; only where it is drawn differs. Where the window may
//! stand is `wisp_core::floating`.

use crate::i18n::t;
use crate::app::{Cmd, Tab, Talk, Workspace};
use crate::theme::{self, Theme, text_px};
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use wisp_core::floating::{self as geo, Rect};

/// The PDF's own bar of tools stays clear of the window.
const PDF_BAR: f32 = 36.;
const TITLE_H: f32 = 36.;
const CORNER: f32 = 18.;

/// What the pointer holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    /// The title bar: the window moves.
    Title,
    /// The bottom-right corner: the window grows or shrinks.
    Corner,
    /// The folded-up button: it moves once the pointer went far enough, else it is a click.
    Button,
}

/// A press on the window or the button, until the button is let go.
#[derive(Clone, Copy, Debug)]
pub struct Drag {
    pub hold: Hold,
    from: Point<Pixels>,
    rect: Rect,
    button: (f32, f32),
    moved: bool,
}

impl Workspace {
    /// The paper's conversation beside its PDF: the panel is asking, there is a PDF, and room for both.
    pub fn paper_beside(&self, window: &Window) -> bool {
        self.lib.as_ref().is_some_and(|lib| lib.split && lib.tab == Tab::Ask && lib.selected.as_deref().is_some_and(|id| lib.pdf_file(id).is_some())) && self.detail_width(window) >= crate::detail::SPLIT_MIN
    }

    /// The button in the header of the conversation beside the PDF that lets it float instead.
    pub fn render_float_switch(&self, talk: Talk, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if talk != Talk::Paper || self.settings.floating.on || !self.paper_beside(window) { return None }
        Some(ui::ghost_button("chat-float", Some("square"), Some(t("悬浮").into())).on_click(self.on(Cmd::FloatChat(true), cx)).into_any_element())
    }

    /// The panel asking about a paper, with the conversation floating over the PDF.
    pub fn render_floating(&mut self, width: f32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let pdf = self.render_pdf_tab(width, cx);
        let Some(lib) = self.lib.as_ref() else { return pdf };
        let area = lib.float_area;
        let floating = &self.settings.floating;
        // The area's size, learned as it is drawn: the window and the button keep within it.
        let entity = cx.entity();
        let measure = canvas(|_, _, _| (), move |bounds, _, window, cx| {
            let size = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            window.defer(cx, move |_, cx| entity.update(cx, |this, cx| this.float_measured(size, cx)));
        })
        .absolute()
        .size_full();
        let over = if area.0 <= 0. || area.1 <= 0. {
            None
        } else if floating.folded {
            Some(self.render_float_button(area, cx))
        } else {
            Some(self.render_float_window(geo::shown(floating.rect, area), window, cx))
        };
        div()
            .relative()
            .size_full()
            .child(pdf)
            .child(div().absolute().top(px(PDF_BAR)).left_0().right_0().bottom_0().child(measure).children(over))
            .into_any_element()
    }

    fn render_float_window(&mut self, rect: Rect, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let chat = self.render_chat(Talk::Paper, window, cx);
        let held = self.lib.as_ref().and_then(|lib| lib.float_drag).map(|drag| drag.hold);
        // The buttons of the title bar are not a place to take hold of it.
        let keep_press = |_: &MouseDownEvent, _: &mut Window, cx: &mut App| cx.stop_propagation();
        let title = div()
            .id("float-chat-title")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .h(text_px(TITLE_H))
            .pl(px(12.))
            .pr(px(6.))
            .border_b_1()
            .border_color(Theme::line())
            .bg(Theme::bg_sunken())
            .cursor(if held == Some(Hold::Title) { CursorStyle::ClosedHand } else { CursorStyle::OpenHand })
            .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &MouseDownEvent, _, cx| this.float_press(Hold::Title, event.position, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, window, cx| this.float_release(window, cx)))
            .child(ui::icon("sparkle", 14., Theme::accent()))
            .child(div().flex_1().min_w_0().truncate().text_size(text_px(12.)).font_weight(FontWeight::MEDIUM).text_color(Theme::text_secondary()).child(t("就这篇文献提问")))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .cursor_default()
                    .on_mouse_down(MouseButton::Left, keep_press)
                    .child(ui::ghost_button("float-chat-dock", Some("panel-left"), Some(t("分栏").into())).h(text_px(26.)).text_size(text_px(12.)).on_click(self.on(Cmd::FloatChat(false), cx)))
                    .child(ui::ghost_button("float-chat-fold", Some("minus"), None).h(text_px(26.)).min_w(px(26.)).on_click(self.on(Cmd::FoldChat(true), cx))),
            );
        // The corner: two short strokes, as on any window that can be sized.
        let corner = div()
            .id("float-chat-corner")
            .absolute()
            .right_0()
            .bottom_0()
            .size(px(CORNER))
            .cursor(CursorStyle::ResizeUpLeftDownRight)
            .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                this.float_press(Hold::Corner, event.position, cx)
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, window, cx| this.float_release(window, cx)))
            .child(div().absolute().right(px(4.)).bottom(px(4.)).size(px(8.)).border_r_2().border_b_2().rounded_br(px(3.)).border_color(if held == Some(Hold::Corner) { Theme::accent() } else { Theme::line_strong() }));
        let frame = div()
            .id("float-chat")
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .rounded(px(12.))
            .border_1()
            .border_color(Theme::line_strong())
            .bg(Theme::bg_body())
            .overflow_hidden()
            // Nothing under the window hears the pointer: the PDF keeps still while the conversation scrolls.
            .occlude()
            .child(title)
            .child(div().flex().flex_col().flex_1().min_h_0().child(chat))
            .child(corner);
        ui::fade_in("float-chat-in", div().absolute().left(px(rect.x)).top(px(rect.y)).w(px(rect.width)).h(px(rect.height)).rounded(px(12.)).shadow(theme::shadow_panel()).child(frame)).into_any_element()
    }

    /// The conversation folded up: a round button, moved by dragging, opened by a click.
    fn render_float_button(&self, area: (f32, f32), cx: &mut Context<Self>) -> AnyElement {
        let Some(lib) = self.lib.as_ref() else { return div().into_any_element() };
        let (x, y) = geo::button_at(self.settings.floating.button, area);
        let held = lib.float_drag.is_some_and(|drag| drag.hold == Hold::Button && drag.moved);
        let busy = lib.paper_chat.busy;
        // Something waits to be sent: the passages and pictures added while it was folded say so.
        let waiting = !lib.paper_chat.quotes.is_empty() || !lib.paper_chat.attachments.is_empty();
        let button = div()
            .id("float-chat-button")
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .size(px(geo::BUTTON))
            .rounded_full()
            .bg(Theme::accent())
            .cursor(if held { CursorStyle::ClosedHand } else { CursorStyle::PointingHand })
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &MouseDownEvent, _, cx| this.float_press(Hold::Button, event.position, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, window, cx| this.float_release(window, cx)))
            .child(if busy { ui::spinner("float-chat-busy", 20., Theme::on_accent()).into_any_element() } else { ui::icon("sparkle", 20., Theme::on_accent()).into_any_element() })
            .when(waiting, |d| d.child(div().absolute().top(px(2.)).right(px(2.)).size(px(12.)).rounded_full().border_2().border_color(Theme::accent()).bg(Theme::on_accent())));
        ui::fade_in("float-chat-button-in", div().absolute().left(px(x)).top(px(y)).rounded_full().shadow(theme::shadow_panel()).child(button)).into_any_element()
    }

    /// While the window or the button is held, a layer over the whole window
    /// hears the pointer wherever it goes (home.rs draws it last).
    pub fn render_float_layer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let drag = self.lib.as_ref()?.float_drag?;
        let cursor = match drag.hold {
            Hold::Corner => CursorStyle::ResizeUpLeftDownRight,
            Hold::Button if !drag.moved => CursorStyle::PointingHand,
            _ => CursorStyle::ClosedHand,
        };
        Some(
            div()
                .id("float-drag-layer")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .occlude()
                .cursor(cursor)
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| this.float_move(event, window, cx)))
                .on_mouse_up(MouseButton::Left, cx.listener(|this, _, window, cx| this.float_release(window, cx)))
                .into_any_element(),
        )
    }

    fn float_measured(&mut self, size: (f32, f32), cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        if (lib.float_area.0 - size.0).abs() < 0.5 && (lib.float_area.1 - size.1).abs() < 0.5 { return }
        lib.float_area = size;
        cx.notify();
    }

    fn float_press(&mut self, hold: Hold, at: Point<Pixels>, cx: &mut Context<Self>) {
        let floating = &self.settings.floating;
        let Some(lib) = self.lib.as_mut() else { return };
        let area = lib.float_area;
        lib.float_drag = Some(Drag { hold, from: at, rect: geo::shown(floating.rect, area), button: geo::button_at(floating.button, area), moved: false });
        cx.notify();
    }

    fn float_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let Some(mut drag) = lib.float_drag else { return };
        // Let go outside the window: the button is up by the time the pointer is back.
        if event.pressed_button != Some(MouseButton::Left) { return self.float_release(window, cx) }
        let area = lib.float_area;
        let delta = (f32::from(event.position.x - drag.from.x), f32::from(event.position.y - drag.from.y));
        if !drag.moved && !geo::is_drag(delta) { return }
        drag.moved = true;
        lib.float_drag = Some(drag);
        let floating = &mut self.settings.floating;
        match drag.hold {
            Hold::Title => floating.rect = Some(geo::moved(drag.rect, delta, area)),
            Hold::Corner => floating.rect = Some(geo::resized(drag.rect, delta, area)),
            Hold::Button => floating.button = Some(geo::button_at(Some((drag.button.0 + delta.0, drag.button.1 + delta.1)), area)),
        }
        cx.notify();
    }

    /// The pointer let go: where the window or the button is, it stays. A
    /// button pressed and let go without going anywhere opens the conversation.
    fn float_release(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.lib.as_mut().and_then(|lib| lib.float_drag.take()) else { return };
        if drag.hold == Hold::Button && !drag.moved { return self.fold_chat(false, window, cx) }
        if drag.moved { self.save_settings(cx) }
        cx.notify();
    }

    /// The paper's conversation floats over the PDF, or stands beside it again.
    pub fn float_chat(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.floating.on = on;
        if let Some(lib) = self.lib.as_mut() { lib.float_drag = None }
        self.save_settings(cx);
        if let Some(lib) = self.lib.as_ref() && !(on && self.settings.floating.folded) { lib.paper_chat.input.read(cx).focus(window) }
        cx.notify();
    }

    /// Fold the floating conversation into its button, or open it again where it was.
    pub fn fold_chat(&mut self, fold: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.floating.folded = fold;
        self.save_settings(cx);
        if !fold && let Some(lib) = self.lib.as_ref() { lib.paper_chat.input.read(cx).focus(window) }
        cx.notify();
    }

    /// Something was added to the paper's conversation (a passage, a piece
    /// of a page): a folded-up conversation opens to show it.
    pub fn unfold_chat(&mut self, cx: &mut Context<Self>) {
        if !(self.settings.floating.on && self.settings.floating.folded) { return }
        self.settings.floating.folded = false;
        self.save_settings(cx);
    }
}
