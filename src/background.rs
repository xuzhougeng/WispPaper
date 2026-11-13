//! Staying in the background: while `Settings::keep_running` is on, closing
//! the window does not quit. The window and everything under way in it (a
//! screening run, an import, the scheduled searches that `watch_searches`
//! looks at once a minute) stay as they are, only out of sight: on macOS the
//! app is hidden and its Dock icon brings it back; on Windows the window is
//! hidden and an icon in the notification area brings it back or quits. ⌘Q
//! (macOS) and the tray's 退出 quit. Elsewhere closing quits as before.

use gpui::*;

actions!(app, [Quit, HideApp, HideOthers, ShowAll]);

/// Whether this platform has a place for an app without a window to come back from.
pub const SUPPORTED: bool = cfg!(any(target_os = "macos", target_os = "windows"));

/// The app-wide part, before any window opens: the app menu's actions.
pub fn install(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &HideApp, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
}

/// macOS: the Dock icon of a hidden app (or the app launched again) brings the window back.
pub fn on_reopen(app: &Application) {
    app.on_reopen(show);
}

/// The macOS app menu, in the interface language; made again when that changes.
pub fn set_menus(cx: &mut App) {
    if !cfg!(target_os = "macos") { return }
    use crate::i18n::t;
    cx.set_menus(vec![Menu {
        name: "Wisp Paper".into(),
        items: vec![
            MenuItem::action(t("设置…"), crate::shortcuts::ShowSettings),
            MenuItem::separator(),
            MenuItem::action(t("隐藏 Wisp Paper"), HideApp),
            MenuItem::action(t("隐藏其他"), HideOthers),
            MenuItem::action(t("全部显示"), ShowAll),
            MenuItem::separator(),
            MenuItem::action(t("退出 Wisp Paper"), Quit),
        ],
    }]);
}

/// The fixed keys of the app menu, added to every rebuild of the bindings.
pub fn bindings() -> Vec<KeyBinding> {
    if !cfg!(target_os = "macos") { return vec![] }
    vec![KeyBinding::new("cmd-q", Quit, None), KeyBinding::new("cmd-h", HideApp, None), KeyBinding::new("alt-cmd-h", HideOthers, None)]
}

/// The window's part, once the workspace has set the interface language: the menus, and closing
/// the window puts the app away instead of quitting while the setting says so.
pub fn watch(workspace: Entity<crate::app::Workspace>, window: &mut Window, cx: &mut App) {
    set_menus(cx);
    #[cfg(target_os = "windows")]
    tray::start(window, cx);
    window.on_window_should_close(cx, move |window, cx| {
        let keep = workspace.read(cx).settings.keep_running;
        close(keep, window, cx)
    });
}

/// What the window's close button does; true lets the window close (and the app quit with it).
pub fn close(keep_running: bool, _window: &mut Window, _cx: &mut App) -> bool {
    if !SUPPORTED || !keep_running { return true }
    #[cfg(target_os = "macos")]
    _cx.hide();
    #[cfg(target_os = "windows")]
    tray::hide(_window);
    false
}

/// Brings the window back to the front.
fn show(cx: &mut App) {
    cx.activate(true);
    for handle in cx.windows() {
        handle
            .update(cx, |_, window, _| {
                #[cfg(target_os = "windows")]
                tray::reveal(window);
                window.activate_window();
                window.refresh();
            })
            .ok();
    }
}

/// Windows: before the app starts, hand over to an instance already running for the same
/// settings file (its window brought back) and say so; such an instance may be in the tray.
#[cfg(target_os = "windows")]
pub fn hand_over() -> bool {
    tray::hand_over()
}

#[cfg(target_os = "windows")]
mod tray {
    use super::show;
    use gpui::*;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::path::PathBuf;
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetClassNameW, IsWindow, GetWindowThreadProcessId, IsIconic, SW_HIDE, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow};

    enum Event {
        Show,
        Quit,
    }

    fn hwnd(window: &Window) -> Option<HWND> {
        // GPUI's own `Window::window_handle` (an `AnyWindowHandle`) would win over the trait's.
        match HasWindowHandle::window_handle(window).ok()?.as_raw() {
            RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as HWND),
            _ => None,
        }
    }

    pub fn hide(window: &Window) {
        if let Some(hwnd) = hwnd(window) {
            unsafe { ShowWindow(hwnd, SW_HIDE) };
        }
    }

    pub fn reveal(window: &Window) {
        if let Some(hwnd) = hwnd(window) {
            bring_up(hwnd);
        }
    }

    fn bring_up(hwnd: HWND) {
        unsafe {
            ShowWindow(hwnd, if IsIconic(hwnd) != 0 { SW_RESTORE } else { SW_SHOW });
            SetForegroundWindow(hwnd);
        }
    }

    /// The tray icon with its menu, and the file that tells a second launch where this window is.
    pub fn start(window: &Window, cx: &mut App) {
        if let Some(hwnd) = hwnd(window) {
            std::fs::write(marker(), format!("{} {}", std::process::id(), hwnd as isize)).ok();
        }
        let (send, receive) = async_channel::unbounded::<Event>();
        let open = MenuItem::new(crate::i18n::t("打开 Wisp Paper"), true, None);
        let quit = MenuItem::new(crate::i18n::t("退出 Wisp Paper"), true, None);
        let menu = Menu::new();
        let (open_id, quit_id) = (open.id().clone(), quit.id().clone());
        if menu.append_items(&[&open, &PredefinedMenuItem::separator(), &quit]).is_err() { return }
        let Some(icon) = icon() else { return };
        let Ok(tray) = TrayIconBuilder::new().with_tooltip("Wisp Paper").with_icon(icon).with_menu(Box::new(menu)).with_menu_on_left_click(false).build() else { return };
        // The handlers run inside the tray's own window procedure; the app hears them through the channel.
        let clicks = send.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            let shown = matches!(event, TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } | TrayIconEvent::DoubleClick { button: MouseButton::Left, .. });
            if shown { clicks.try_send(Event::Show).ok(); }
        }));
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if event.id == open_id { send.try_send(Event::Show).ok(); }
            if event.id == quit_id { send.try_send(Event::Quit).ok(); }
        }));
        cx.spawn(async move |cx| {
            // The icon lives as long as this task: dropping it takes it out of the tray.
            let _tray: TrayIcon = tray;
            while let Ok(event) = receive.recv().await {
                let done = cx
                    .update(|cx| match event {
                        Event::Show => show(cx),
                        Event::Quit => {
                            std::fs::remove_file(marker()).ok();
                            cx.quit()
                        }
                    })
                    .is_err();
                if done { break }
            }
        })
        .detach();
    }

    fn icon() -> Option<tray_icon::Icon> {
        use gpui::AssetSource;
        let bytes = crate::assets::Assets.load("branding/app-icon.png").ok()??;
        let small = image::imageops::resize(&image::load_from_memory(&bytes).ok()?.into_rgba8(), 32, 32, image::imageops::FilterType::Lanczos3);
        tray_icon::Icon::from_rgba(small.into_raw(), 32, 32).ok()
    }

    /// One file per settings file (`WISP_PAPER_HOME` makes another), so a demo runs beside the real library.
    fn marker() -> PathBuf {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::hash::DefaultHasher::new();
        wisp_core::settings::settings_file().hash(&mut hasher);
        std::env::temp_dir().join(format!("wisp-paper-{:016x}.window", hasher.finish()))
    }

    pub fn hand_over() -> bool {
        let Ok(text) = std::fs::read_to_string(marker()) else { return false };
        let mut words = text.split_whitespace().map(|w| w.parse::<isize>().ok());
        let (Some(Some(pid)), Some(Some(handle))) = (words.next(), words.next()) else { return false };
        let hwnd = handle as HWND;
        unsafe {
            if IsWindow(hwnd) == 0 { return false }
            // The window must still be a GPUI window of that process: a handle can be given out again.
            let mut owner = 0u32;
            GetWindowThreadProcessId(hwnd, &mut owner);
            if owner as isize != pid || owner == std::process::id() { return false }
            let mut class = [0u16; 64];
            let length = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
            if String::from_utf16_lossy(&class[..length.max(0) as usize]) != "Zed::Window" { return false }
        }
        bring_up(hwnd);
        true
    }
}
