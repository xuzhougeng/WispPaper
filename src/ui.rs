//! Small shared UI pieces: icons, buttons, chips, field shells, popover and dialog shells.

use crate::input::TextInput;
use crate::theme::{self, Theme};
use gpui::{prelude::FluentBuilder, *};
use std::time::Duration;

/// The same colours and counts in the compact progress bar and its detail dialog.
pub fn fetch_segments(run: &crate::app::FetchRun) -> [(usize, &'static str, Hsla); 5] {
    [
        (run.count("saved"), "已保存", Theme::success()),
        (run.count("not_found"), "未找到地址", Theme::warning()),
        (run.count("failed"), "下载失败", Theme::danger()),
        (run.working.len(), "正在查找", theme::accent_of("blue").strong()),
        (run.remaining(), if run.stopped { "未处理" } else { "等待中" }, Theme::text_placeholder()),
    ]
}

pub fn fetch_progress(run: &crate::app::FetchRun) -> Div {
    progress_bar(run.total, &fetch_segments(run))
}

pub type ProgressSegment = (usize, &'static str, Hsla);

pub fn import_segments(run: &crate::app::ImportRun) -> [ProgressSegment; 6] {
    use crate::app::Imported;
    let mut counts = [0; 4];
    for row in &run.rows {
        counts[match row.outcome {
            Imported::Failed(_) => 3,
            Imported::InTrash => 2,
            _ if row.open() => 1,
            _ => 0,
        }] += 1;
    }
    import_progress(counts, run.working.len(), run.total.saturating_sub(run.rows.len() + run.working.len()))
}

pub fn zotero_segments(run: &crate::app::ZoteroRun) -> [ProgressSegment; 6] {
    let mut counts = [0; 4];
    for row in &run.rows {
        counts[if row.in_trash { 2 } else if row.paper_id.is_none() { 3 } else if !row.problems.is_empty() { 1 } else { 0 }] += 1;
    }
    let remaining = run.total.saturating_sub(run.rows.len());
    let working = usize::from(run.running && remaining > 0);
    import_progress(counts, working, remaining.saturating_sub(working))
}

fn import_progress(counts: [usize; 4], working: usize, waiting: usize) -> [ProgressSegment; 6] {
    [
        (counts[0], "已导入", Theme::success()),
        (counts[1], "待完善", Theme::warning()),
        (counts[2], "已跳过", Theme::text_tertiary()),
        (counts[3], "导入失败", Theme::danger()),
        (working, "处理中", theme::accent_of("blue").strong()),
        (waiting, "等待中", Theme::text_placeholder()),
    ]
}

pub fn progress_bar(total: usize, segments: &[ProgressSegment]) -> Div {
    div().flex().flex_none().w_full().h(px(7.)).rounded_full().overflow_hidden().bg(Theme::groove())
        .children(segments.iter().copied().filter(|(count, _, _)| *count > 0).map(|(count, _, color)| {
            div().flex_none().h_full().w(relative(count as f32 / total.max(1) as f32)).bg(color)
        }))
}

pub fn progress_entry(id: &'static str, label: &'static str, done: usize, total: usize, running: bool, segments: &[ProgressSegment]) -> Stateful<Div> {
    div().id(id).flex().flex_none().flex_col().gap(px(7.)).px(px(8.)).py(px(10.)).rounded(px(6.))
        .cursor_pointer().hover(|s| s.bg(Theme::hover()))
        .child(div().flex().items_center().gap(px(6.)).text_size(px(12.)).line_height(px(17.))
            .when(running, |d| d.child(spinner((id, 0_usize), 12., theme::accent_of("blue").strong())))
            .child(div().flex_1().min_w_0().truncate().text_color(Theme::text_secondary()).child(label))
            .child(div().flex_none().text_color(Theme::text_tertiary()).child(if running && total == 0 { "准备中".into() } else { format!("{done}/{total}") }))
            .child(icon("chevron-right", 12., Theme::text_tertiary())))
        .child(progress_bar(total, segments))
}

pub fn progress_overview(done: usize, total: usize, state: &'static str, running: bool, segments: &[ProgressSegment]) -> Div {
    let color = if running { theme::accent_of("blue").strong() } else { Theme::text_secondary() };
    div().flex().flex_none().flex_col().gap(px(12.)).px(px(20.)).py(px(14.))
        .child(div().flex().items_center().gap(px(8.))
            .child(div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).child(if running && total == 0 { "正在准备…".into() } else { format!("{done} / {total}") }))
            .when(total > 0, |d| d.child(div().text_color(Theme::text_tertiary()).child("项已处理")))
            .child(div().flex_1())
            .when(running, |d| d.child(spinner("progress-modal-working", 13., color)))
            .child(div().text_color(color).child(state)))
        .child(progress_bar(total, segments).h(px(9.)))
        .child(div().flex().flex_wrap().gap(px(14.)).children(segments.iter().map(|(count, label, color)| {
            div().flex().items_center().gap(px(5.)).text_size(px(12.))
                .child(div().flex_none().size(px(7.)).rounded_full().bg(*color))
                .child(div().text_color(Theme::text_secondary()).child(format!("{label} {count}")))
        })))
}

pub fn icon(name: &str, size: f32, color: Hsla) -> Svg {
    svg().path(SharedString::from(format!("icons/{name}.svg"))).size(px(size)).flex_none().text_color(color)
}

/// A turning mark: something is being waited for.
pub fn spinner(id: impl Into<ElementId>, size: f32, color: Hsla) -> impl IntoElement {
    icon("loader", size, color).with_animation(id, Animation::new(Duration::from_millis(900)).repeat(), |mark, turn| mark.with_transformation(Transformation::rotate(percentage(turn))))
}

/// The element appears over a moment instead of at once. It plays once,
/// when the element with this id is first drawn.
pub fn fade_in(id: impl Into<ElementId>, element: Div) -> impl IntoElement {
    element.with_animation(id, Animation::new(Duration::from_millis(400)).with_easing(ease_out_quint()), |element, shown| element.opacity(shown))
}

/// The filled button of a dialog or toolbar, in the library's accent.
pub fn primary_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .h(px(32.))
        .px(px(14.))
        .rounded(px(8.))
        .bg(Theme::accent())
        .text_color(Theme::on_accent())
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .hover(|s| s.opacity(0.9))
        .active(|s| s.opacity(0.8))
        .child(label.into())
}

pub fn secondary_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .h(px(32.))
        .px(px(12.))
        .rounded(px(8.))
        .border_1()
        .border_color(Theme::line_strong())
        .bg(Theme::bg_body())
        .text_color(Theme::text())
        .text_size(px(13.))
        .cursor_pointer()
        .hover(|s| s.bg(Theme::bg_sunken()))
        .active(|s| s.bg(Theme::pressed()))
        .child(label.into())
}

/// A borderless button: an icon, a label, or both.
pub fn ghost_button(id: impl Into<ElementId>, icon_name: Option<&str>, label: Option<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(px(5.))
        .h(px(28.))
        .min_w(px(28.))
        .when(label.is_some(), |d| d.px(px(8.)))
        .rounded(px(6.))
        .text_color(Theme::text_secondary())
        .text_size(px(13.))
        .cursor_pointer()
        .hover(|s| s.bg(Theme::hover()))
        .active(|s| s.bg(Theme::pressed()))
        .when_some(icon_name, |d, name| d.child(icon(name, 16., Theme::text_secondary())))
        .when_some(label, |d, label| d.child(label))
}

/// A small rounded label: statuses, tags, field values.
pub fn chip(label: impl Into<SharedString>, text: Hsla, fill: Hsla) -> Div {
    div().flex().flex_none().items_center().h(px(22.)).px(px(8.)).rounded(px(11.)).bg(fill).text_color(text).text_size(px(12.)).whitespace_nowrap().child(label.into())
}

pub fn status_chip(value: &str, label: &str) -> Div {
    let (text, fill) = theme::status_colors(value);
    chip(label.to_string(), text, fill)
}

/// A key as it is on the keyboard, beside what it does.
pub fn keycap(key: impl Into<SharedString>) -> Div {
    div().flex().flex_none().items_center().justify_center().min_w(px(18.)).h(px(18.)).px(px(4.)).rounded(px(4.)).border_1().border_color(Theme::line_strong()).bg(Theme::bg_sunken()).text_size(px(11.)).line_height(px(14.)).font_weight(FontWeight::MEDIUM).text_color(Theme::text_secondary()).child(key.into())
}

/// The bordered shell around a text input.
pub fn field(input: &Entity<TextInput>, window: &Window, cx: &App) -> Div {
    let focused = input.focus_handle(cx).is_focused(window);
    let multiline = input.read(cx).multiline;
    div()
        .flex()
        .w_full()
        .px(px(10.))
        .py(px(if multiline { 8. } else { 6. }))
        .rounded(px(8.))
        .border_1()
        .border_color(if focused { Theme::accent() } else { Theme::line_strong() })
        .bg(Theme::bg_body())
        .text_size(px(13.))
        .line_height(px(20.))
        .text_color(Theme::text())
        .child(input.clone())
}

/// A labelled row of a form.
pub fn form_row(label: impl Into<SharedString>, content: impl IntoElement) -> Div {
    div().flex().flex_col().gap(px(5.)).child(div().text_size(px(12.)).text_color(Theme::text_secondary()).child(label.into())).child(content)
}

/// The floating shell of a menu.
pub fn popover() -> Div {
    div()
        .occlude()
        .flex()
        .flex_col()
        .min_w(px(168.))
        .max_w(px(320.))
        .p(px(4.))
        .rounded(px(10.))
        .bg(Theme::bg_body())
        .border_1()
        .border_color(Theme::line())
        .shadow(theme::shadow_panel())
        .font_family(theme::UI_FONT)
        .text_size(px(13.))
        .text_color(Theme::text())
}

pub fn menu_row(id: impl Into<ElementId>, label: impl Into<SharedString>, checked: bool, note: Option<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(8.))
        .h(px(30.))
        .px(px(8.))
        .rounded(px(6.))
        .cursor_pointer()
        .hover(|s| s.bg(Theme::hover()))
        .child(div().w(px(14.)).flex_none().when(checked, |d| d.child(icon("check", 14., Theme::accent()))))
        .child(div().flex_1().min_w_0().truncate().child(label.into()))
        .when_some(note, |d, note| d.child(div().flex_none().text_size(px(12.)).text_color(Theme::text_tertiary()).child(note)))
}

/// The white card of a dialog, centred by the caller over a scrim.
pub fn dialog(width: f32) -> Div {
    div()
        .occlude()
        .flex()
        .flex_col()
        .w(px(width))
        .max_h(relative(0.9))
        .rounded(px(14.))
        .bg(Theme::bg_body())
        .shadow(theme::shadow_panel())
        .font_family(theme::UI_FONT)
        .text_color(Theme::text())
        .text_size(px(13.))
}

pub fn dialog_title(title: impl Into<SharedString>) -> Div {
    div().px(px(20.)).pt(px(18.)).pb(px(6.)).text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(title.into())
}

pub fn error_line(message: &str) -> Div {
    div().when(!message.is_empty(), |d| d.px(px(10.)).py(px(7.)).rounded(px(8.)).bg(Theme::danger_soft()).text_color(Theme::danger()).text_size(px(12.)).child(message.to_string()))
}

/// The centred message of an empty list.
pub fn empty_state(icon_name: &str, title: &str, hint: &str) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .p(px(32.))
        .child(icon(icon_name, 30., Theme::text_placeholder()))
        .child(div().text_size(px(14.)).text_color(Theme::text_secondary()).child(title.to_string()))
        .child(div().max_w(px(360.)).text_size(px(12.)).text_color(Theme::text_tertiary()).child(hint.to_string()))
}

/// A section caption inside a panel.
pub fn caption(text: impl Into<SharedString>) -> Div {
    div().text_size(px(11.)).font_weight(FontWeight::MEDIUM).text_color(Theme::text_tertiary()).child(text.into())
}

// ---- window chrome
//
// On Windows and macOS the window has no separate title bar: the top strip
// of each column drags the window, and on Windows the minimise / maximise /
// close buttons are drawn at the top right. They are native hit-test areas,
// so Snap Layouts and the system's close handling keep working.

pub const TITLE_H: f32 = 36.;
const CONTROL_W: f32 = 46.;

pub fn custom_titlebar() -> bool {
    cfg!(any(target_os = "windows", target_os = "macos"))
}

/// Room to leave at the top left for macOS's traffic lights.
pub fn traffic_lights_width() -> f32 {
    if cfg!(target_os = "macos") { 72. } else { 0. }
}

/// A strip that works like a title bar: a press drags the window, a double
/// press zooms it. Buttons placed on it call `.occlude()` to stay clickable.
pub fn title_strip() -> Div {
    let strip = div().flex().flex_none().items_center().w_full().h(px(if custom_titlebar() { TITLE_H } else { 0. }));
    if cfg!(target_os = "windows") {
        // Keep the workspace's focus handler from preventing the native caption action
        // (activation, dragging and double-click maximising) on mouse down.
        strip.occlude().window_control_area(WindowControlArea::Drag)
    } else if cfg!(target_os = "macos") {
        strip.on_mouse_down(MouseButton::Left, |ev, window, _| match ev.click_count {
            1 => window.start_window_move(),
            2 => window.titlebar_double_click(),
            _ => {}
        })
    } else {
        strip
    }
}

/// The minimise, maximise and close buttons (Windows only).
pub fn window_controls(window: &Window) -> Option<Div> {
    if !cfg!(target_os = "windows") { return None }
    let button = |id: &'static str, area: WindowControlArea, icon_name: &'static str, close: bool| {
        div()
            .id(id)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .w(px(CONTROL_W))
            .h(px(TITLE_H))
            .occlude()
            .window_control_area(area)
            .group(id)
            .hover(move |s| if close { s.bg(Theme::danger()) } else { s.bg(Theme::hover()) })
            .child(icon(icon_name, 15., Theme::text_secondary()).when(close, |i| i.group_hover(id, |s| s.text_color(gpui::white()))))
    };
    Some(
        div()
            .absolute()
            .top_0()
            .right_0()
            .flex()
            .child(button("window-minimize", WindowControlArea::Min, "minus", false))
            .child(button("window-maximize", WindowControlArea::Max, if window.is_maximized() { "copy" } else { "expand" }, false))
            .child(button("window-close", WindowControlArea::Close, "x", true)),
    )
}

#[cfg(test)]
mod progress_tests {
    use super::{import_segments, zotero_segments};
    use crate::app::{ImportRow, ImportRun, Imported, ZoteroRun};
    use wisp_core::zotero;

    #[test]
    fn progress_categories_are_exclusive_even_when_failed_rows_have_warnings() {
        let row = |outcome, unrecognised| ImportRow { name: "same.pdf".into(), outcome, paper_id: None, unrecognised, textless: String::new() };
        let pdfs = ImportRun { total: 7, working: vec!["same.pdf".into(), "same.pdf".into()], rows: vec![row(Imported::Added, false), row(Imported::Attached, true), row(Imported::InTrash, true), row(Imported::Failed("bad file".into()), true)] };
        assert_eq!(import_segments(&pdfs).map(|(n, _, _)| n), [1, 1, 1, 1, 2, 1]);
        let zotero = ZoteroRun { total: 6, running: true, rows: vec![
            zotero::Row { paper_id: Some("saved".into()), ..Default::default() },
            zotero::Row { paper_id: Some("incomplete".into()), problems: vec!["missing PDF".into()], ..Default::default() },
            zotero::Row { in_trash: true, problems: vec!["skipped".into()], ..Default::default() },
            zotero::Row { problems: vec!["failed".into()], ..Default::default() },
        ], ..Default::default() };
        assert_eq!(zotero_segments(&zotero).map(|(n, _, _)| n), [1, 1, 1, 1, 1, 1]);
    }
}
