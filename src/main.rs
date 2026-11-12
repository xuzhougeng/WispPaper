//! Wisp Paper: a local-first literature library. Collect papers, organise
//! them with tags and custom fields, preview PDFs and ask questions over a
//! library. The engine is `wisp_core` (crates/core, no GPUI); this crate is
//! the GPUI application.
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod assets;
mod chat;
mod cite;
mod context;
mod detail;
mod dialogs;
mod guide;
mod home;
mod input;
mod library;
mod manuscripts;
mod markdown;
mod pdf;
mod picture;
mod screen;
mod searches;
mod select;
mod settings;
mod shortcuts;
mod theme;
mod ui;
mod write;
mod write_find;
mod write_assist;

use gpui::*;

pub(crate) const APP_ID: &str = "com.wisp.paper";

fn main() {
    Application::new().with_assets(assets::Assets).run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1360.), px(860.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                app_id: Some(APP_ID.into()),
                titlebar: Some(TitlebarOptions { title: Some("Wisp Paper".into()), appears_transparent: ui::custom_titlebar(), traffic_light_position: Some(point(px(14.), px(11.))) }),
                window_min_size: Some(size(px(900.), px(560.))),
                focus: std::env::var_os("WISP_PAPER_DEV").is_none(),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| app::Workspace::new(window, cx)),
        )
        .expect("open window")
        .update(cx, |_workspace, _window, _cx| {
            #[cfg(debug_assertions)]
            _workspace.dev_start(_window, _cx);
        })
        .ok();
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() { cx.quit() }
        })
        .detach();
        // A scripted debug start must not take the keyboard from whatever the user is doing.
        if std::env::var_os("WISP_PAPER_DEV").is_none() { cx.activate(true) }
    });
}
