//! Wisp Study: a local-first literature library. Collect papers, organise
//! them with tags and custom fields, preview PDFs and ask questions over a
//! library. The engine is `wisp_core` (crates/core, no GPUI); this crate is
//! the GPUI application.
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

#[macro_use]
mod i18n;
mod app;
mod assets;
mod background;
mod chat;
mod cite;
mod comments;
mod context;
mod custom;
mod detail;
mod extract;
mod facing;
mod figures;
mod floating;
mod dialogs;
mod draft;
mod guide;
mod home;
mod input;
mod library;
mod manuscripts;
mod markdown;
mod notes;
mod palette;
mod pdf;
mod picture;
mod revise;
mod screen;
mod screen_confirm;
mod screen_single;
mod searches;
mod select;
mod settings;
mod shortcuts;
mod theme;
mod translate;
mod ui;
mod updater;
mod write;
mod write_find;
mod write_seek;
mod write_agent;
mod write_assist;
mod write_cite;
mod write_source;

use gpui::*;

pub(crate) const APP_ID: &str = "com.wisp.study";

fn main() {
    #[cfg(target_os = "windows")]
    if background::hand_over() { return }
    let app = Application::new().with_assets(assets::Assets);
    background::on_reopen(&app);
    app.run(|cx: &mut App| {
        background::install(cx);
        let bounds = Bounds::centered(None, size(px(1360.), px(860.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                app_id: Some(APP_ID.into()),
                titlebar: Some(TitlebarOptions { title: Some("Wisp Study".into()), appears_transparent: ui::custom_titlebar(), traffic_light_position: Some(point(px(14.), px(11.))) }),
                window_min_size: Some(size(px(900.), px(560.))),
                focus: wisp_core::settings::env("DEV").is_none(),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| app::Workspace::new(window, cx)),
        )
        .expect("open window")
        .update(cx, |workspace, window, cx| {
            background::watch(cx.entity(), window, cx);
            workspace.start_session(window, cx);
            #[cfg(debug_assertions)]
            workspace.dev_start(window, cx);
        })
        .ok();
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() { cx.quit() }
        })
        .detach();
        // A scripted debug start must not take the keyboard from whatever the user is doing.
        if wisp_core::settings::env("DEV").is_none() { cx.activate(true) }
    });
}
