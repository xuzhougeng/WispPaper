//! The keyboard shortcuts of the app (not the text field's editing keys,
//! which are fixed): their defaults, the changes kept in settings.json, and
//! how a key is shown. 设置 › 快捷键 records new keys (settings.rs).

use gpui::*;
use std::collections::BTreeMap;

actions!(workspace, [Dismiss, FocusSearch, AddPaper, ImportPdf, AskLibrary, ToggleDetail, ToggleSidebar, ShowSettings, ShowHome, NewLibrary, ZoomIn, ZoomOut, ZoomFit, CopyPicked, FindCandidates]);

pub struct Shortcut {
    pub id: &'static str,
    pub label: &'static str,
    /// Index into `GROUPS`.
    pub group: usize,
    /// `secondary` is ⌘ on macOS and Ctrl elsewhere.
    default: &'static str,
}

pub const GROUPS: [&str; 2] = ["应用", "文献库"];

/// What each one does is in `Workspace::shortcut` (app.rs).
pub const SHORTCUTS: [Shortcut; 13] = [
    Shortcut { id: "settings", label: "打开设置", group: 0, default: "secondary-," },
    Shortcut { id: "home", label: "回到所有文献库", group: 0, default: "secondary-shift-h" },
    Shortcut { id: "new-library", label: "新建文献库", group: 0, default: "secondary-shift-n" },
    Shortcut { id: "search", label: "搜索文献", group: 1, default: "secondary-f" },
    Shortcut { id: "add", label: "通过 DOI / PMID 添加文献", group: 1, default: "secondary-n" },
    Shortcut { id: "import-pdf", label: "导入 PDF 文件", group: 1, default: "secondary-o" },
    Shortcut { id: "ask", label: "向文献库提问", group: 1, default: "secondary-l" },
    Shortcut { id: "detail", label: "展开 / 收起文献详情", group: 1, default: "secondary-e" },
    Shortcut { id: "sidebar", label: "显示 / 收起侧边栏", group: 1, default: "secondary-b" },
    Shortcut { id: "zoom-in", label: "放大 PDF", group: 1, default: "secondary-=" },
    Shortcut { id: "zoom-out", label: "缩小 PDF", group: 1, default: "secondary--" },
    Shortcut { id: "zoom-fit", label: "PDF 适应宽度", group: 1, default: "secondary-0" },
    Shortcut { id: "write-find", label: "写作：查找候选句", group: 1, default: "secondary-enter" },
];

/// The text field's own keys (input.rs): a shortcut on one of them would
/// never fire while typing, and would surprise when it did.
const EDITING: [&str; 4] = ["secondary-a", "secondary-c", "secondary-x", "secondary-v"];

fn find(id: &str) -> Option<&'static Shortcut> {
    SHORTCUTS.iter().find(|s| s.id == id)
}

/// The key in force: the changed one (empty when removed), else the default.
pub fn key_of(id: &str, custom: &BTreeMap<String, String>) -> String {
    custom.get(id).cloned().unwrap_or_else(|| find(id).map_or(String::new(), |s| s.default.to_string()))
}

fn same(a: &str, b: &str) -> bool {
    match (Keystroke::parse(a), Keystroke::parse(b)) {
        (Ok(a), Ok(b)) => a.modifiers == b.modifiers && a.key == b.key,
        _ => false,
    }
}

pub fn is_default(id: &str, key: &str) -> bool {
    find(id).is_some_and(|s| same(s.default, key))
}

/// Why `key` cannot become the shortcut `id`, if it cannot.
pub fn refusal(id: &str, key: &str, custom: &BTreeMap<String, String>) -> Option<String> {
    let Ok(stroke) = Keystroke::parse(key) else { return Some("没有认出这个按键。".into()) };
    let m = stroke.modifiers;
    let function_key = stroke.key.len() > 1 && stroke.key.starts_with('f') && stroke.key[1..].parse::<u8>().is_ok();
    if !(m.control || m.alt || m.platform || function_key) {
        return Some(if cfg!(target_os = "macos") { "快捷键需要带上 ⌘、⌃ 或 ⌥，或者使用 F1–F12。".into() } else { "快捷键需要带上 Ctrl 或 Alt，或者使用 F1–F12。".into() });
    }
    if EDITING.iter().any(|e| same(e, key)) { return Some(format!("{} 用于文本编辑（全选、复制、剪切、粘贴），不能改作他用。", pretty(key))) }
    SHORTCUTS.iter().find(|s| s.id != id && same(&key_of(s.id, custom), key)).map(|other| format!("{} 已用于「{}」。先修改或移除那个快捷键。", pretty(key), other.label))
}

/// Replace every key binding: the text field's, Esc, and the shortcuts as changed by `custom`.
pub fn rebuild(custom: &BTreeMap<String, String>, cx: &mut App) {
    cx.clear_key_bindings();
    crate::input::bind_keys(cx);
    let context = Some("Workspace");
    // Copying what is selected in a PDF or in an answer: the text fields have the same key for their own selection.
    let mut bindings = vec![KeyBinding::new("escape", Dismiss, context), KeyBinding::new("secondary-c", crate::pdf::CopySelection, Some("PdfView")), KeyBinding::new("secondary-c", CopyPicked, context)];
    for shortcut in &SHORTCUTS {
        let key = key_of(shortcut.id, custom);
        // A key from a hand-edited settings.json may not parse; `KeyBinding::new` would panic on it.
        if key.is_empty() || Keystroke::parse(&key).is_err() { continue }
        bindings.push(match shortcut.id {
            "settings" => KeyBinding::new(&key, ShowSettings, context),
            "home" => KeyBinding::new(&key, ShowHome, context),
            "new-library" => KeyBinding::new(&key, NewLibrary, context),
            "search" => KeyBinding::new(&key, FocusSearch, context),
            "add" => KeyBinding::new(&key, AddPaper, context),
            "import-pdf" => KeyBinding::new(&key, ImportPdf, context),
            "ask" => KeyBinding::new(&key, AskLibrary, context),
            "sidebar" => KeyBinding::new(&key, ToggleSidebar, context),
            "zoom-in" => KeyBinding::new(&key, ZoomIn, context),
            "zoom-out" => KeyBinding::new(&key, ZoomOut, context),
            "zoom-fit" => KeyBinding::new(&key, ZoomFit, context),
            "write-find" => KeyBinding::new(&key, FindCandidates, context),
            _ => KeyBinding::new(&key, ToggleDetail, context),
        });
    }
    cx.bind_keys(bindings);
}

/// A key as people read it: `Ctrl + Shift + H`, or `⇧⌘H` on macOS.
pub fn pretty(key: &str) -> String {
    let Ok(stroke) = Keystroke::parse(key) else { return key.to_string() };
    let m = stroke.modifiers;
    let name = match stroke.key.as_str() {
        "escape" => "Esc".to_string(),
        "space" => "Space".to_string(),
        "enter" => "Enter".to_string(),
        "backspace" => "Backspace".to_string(),
        "delete" => "Delete".to_string(),
        "tab" => "Tab".to_string(),
        "up" => "↑".to_string(),
        "down" => "↓".to_string(),
        "left" => "←".to_string(),
        "right" => "→".to_string(),
        "pageup" => "Page Up".to_string(),
        "pagedown" => "Page Down".to_string(),
        "home" => "Home".to_string(),
        "end" => "End".to_string(),
        other => other.to_uppercase(),
    };
    if cfg!(target_os = "macos") {
        let mut text = String::new();
        for (on, symbol) in [(m.control, "⌃"), (m.alt, "⌥"), (m.shift, "⇧"), (m.platform, "⌘")] {
            if on { text.push_str(symbol) }
        }
        text + &name
    } else {
        let mut parts = vec![];
        for (on, word) in [(m.control, "Ctrl"), (m.alt, "Alt"), (m.shift, "Shift"), (m.platform, if cfg!(target_os = "windows") { "Win" } else { "Super" })] {
            if on { parts.push(word.to_string()) }
        }
        parts.push(name);
        parts.join(" + ")
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that would bring in gpui's own `test` attribute.
    use super::{SHORTCUTS, is_default, key_of, pretty, refusal, same};
    use std::collections::BTreeMap;

    #[test]
    fn keys() {
        let mut custom = BTreeMap::new();
        assert!(same(&key_of("search", &custom), "secondary-f") && is_default("search", if cfg!(target_os = "macos") { "cmd-f" } else { "ctrl-f" }));
        assert_eq!(refusal("search", "alt-f", &custom), None);
        // Bare keys, editing keys and keys in use are refused; a function key is fine.
        assert!(refusal("search", "f", &custom).is_some() && refusal("search", "shift-f", &custom).is_some());
        assert!(refusal("search", "secondary-c", &custom).unwrap().contains("文本编辑"));
        assert!(refusal("search", "secondary-n", &custom).unwrap().contains("通过 DOI / PMID 添加文献"));
        assert_eq!(refusal("search", "f3", &custom), None);
        // Its own key is no conflict; a removed shortcut frees its key.
        assert_eq!(refusal("search", "secondary-f", &custom), None);
        custom.insert("add".to_string(), String::new());
        assert_eq!((key_of("add", &custom), refusal("search", "secondary-n", &custom)), (String::new(), None));
        // Every default is distinct and acceptable.
        for s in &SHORTCUTS {
            assert_eq!(refusal(s.id, s.default, &BTreeMap::new()), None, "{}", s.id);
        }
        if !cfg!(target_os = "macos") {
            assert_eq!((pretty("ctrl-shift-h"), pretty("secondary-,"), pretty("f3")), ("Ctrl + Shift + H".to_string(), "Ctrl + ,".to_string(), "F3".to_string()));
        }
    }
}
