//! The keyboard shortcuts of the app (not the text field's editing keys,
//! which are fixed): their defaults, the changes kept in settings.json, and
//! how a key is shown. 设置 › 快捷键 records new keys (settings.rs).

use crate::i18n::t;
use gpui::*;
use std::collections::BTreeMap;

actions!(workspace, [Dismiss, FocusSearch, AddPaper, ImportPdf, AskLibrary, ToggleDetail, ToggleSidebar, ToggleReading, ShowSettings, ShowHome, NewLibrary, ZoomIn, ZoomOut, ZoomFit, CopyPicked, FindCandidates, MoveUp, MoveDown, MoveLeft, MoveRight, MovePageUp, MovePageDown, MoveFirst, MoveLast, OpenSelected, TrashSelected, MarkUp, MarkDown, MarkAll, DecideIncluded, DecideRelated, DecideUncertain, DecideIrrelevant, DecidePending, TakeSuggestion, StarScreened, TranslateScreened, UndoDecision, AskAssistant, TickSuggestion]);

/// The keys of 筛选, fixed like the arrow keys: what each does is in `Workspace::screen_key`. Bare letters
/// and digits, so they are heard only while no text field has the keyboard.
pub const SCREEN_KEYS: [(&str, &str); 11] = [("Y / 1", "已纳入"), ("R / 2", "相关"), ("U / 3", "证据不足"), ("N / 4", "不相关"), ("P / 0", "留待判断"), ("A", "采纳助手的建议"), ("Z", "逐篇：撤销上一个判断"), ("G", "让助手判断这一篇"), ("S", "加星标 / 取消星标"), ("T", "显示译文 / 显示原文"), ("Space / X", "确认建议：勾选 / 不勾选")];

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
pub const SHORTCUTS: [Shortcut; 14] = [
    Shortcut { id: "settings", label: "打开设置", group: 0, default: "secondary-," },
    Shortcut { id: "home", label: "回到所有文献库", group: 0, default: "secondary-shift-h" },
    Shortcut { id: "new-library", label: "新建文献库", group: 0, default: "secondary-shift-n" },
    Shortcut { id: "search", label: "搜索文献", group: 1, default: "secondary-f" },
    Shortcut { id: "add", label: "通过 DOI / PMID 添加文献", group: 1, default: "secondary-n" },
    Shortcut { id: "import-pdf", label: "导入 PDF 文件", group: 1, default: "secondary-o" },
    Shortcut { id: "ask", label: "打开助手", group: 1, default: "secondary-l" },
    Shortcut { id: "detail", label: "展开 / 收起文献详情", group: 1, default: "secondary-e" },
    Shortcut { id: "sidebar", label: "显示 / 收起侧边栏", group: 1, default: "secondary-b" },
    Shortcut { id: "reading", label: "进入 / 退出阅读模式", group: 1, default: "secondary-shift-f" },
    Shortcut { id: "zoom-in", label: "放大 PDF", group: 1, default: "secondary-=" },
    Shortcut { id: "zoom-out", label: "缩小 PDF", group: 1, default: "secondary--" },
    Shortcut { id: "zoom-fit", label: "PDF 适应宽度", group: 1, default: "secondary-0" },
    Shortcut { id: "write-find", label: "写作：查找候选句", group: 1, default: "secondary-enter" },
];

/// The text field's own keys (input.rs): a shortcut on one of them would
/// never fire while typing, and would surprise when it did.
const EDITING: [&str; 6] = ["secondary-a", "secondary-c", "secondary-x", "secondary-v", "secondary-z", "secondary-shift-z"];
/// Moving a paper to the trash from the list.
const TRASH: &str = "secondary-backspace";

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
    let Ok(stroke) = Keystroke::parse(key) else { return Some(t("没有认出这个按键。").into()) };
    let m = stroke.modifiers;
    let function_key = stroke.key.len() > 1 && stroke.key.starts_with('f') && stroke.key[1..].parse::<u8>().is_ok();
    if !(m.control || m.alt || m.platform || function_key) {
        return Some(if cfg!(target_os = "macos") { t("快捷键需要带上 ⌘、⌃ 或 ⌥，或者使用 F1–F12。").into() } else { t("快捷键需要带上 Ctrl 或 Alt，或者使用 F1–F12。").into() });
    }
    if EDITING.iter().any(|e| same(e, key)) { return Some(tf!("{} 用于文本编辑（全选、复制、剪切、粘贴、撤销、重做），不能改作他用。", "{} is reserved for text editing (select all, copy, cut, paste, undo and redo).", pretty(key))) }
    if same(TRASH, key) { return Some(tf!("{} 用于把选中的文献移到回收站，不能改作他用。", "{} is reserved for moving selected papers to trash.", pretty(key))) }
    SHORTCUTS.iter().find(|s| s.id != id && same(&key_of(s.id, custom), key)).map(|other| tf!("{} 已用于「{}」。先修改或移除那个快捷键。", "{} is already used for “{}”. Change or remove that shortcut first.", pretty(key), t(other.label)))
}

/// Replace every key binding: the text field's, Esc, and the shortcuts as changed by `custom`.
pub fn rebuild(custom: &BTreeMap<String, String>, cx: &mut App) {
    cx.clear_key_bindings();
    crate::input::bind_keys(cx);
    let context = Some("Workspace");
    // Copying what is selected in a PDF or in an answer: the text fields have the same key for their own selection.
    let mut bindings = vec![KeyBinding::new("escape", Dismiss, context), KeyBinding::new("secondary-c", crate::pdf::CopySelection, Some("PdfView")), KeyBinding::new("secondary-c", CopyPicked, context)];
    // Moving in the list of papers and in an open menu: fixed keys, as the text field's are. A text field
    // with the keyboard hears its own arrows, Enter and Delete first.
    bindings.extend([
        KeyBinding::new("up", MoveUp, context),
        KeyBinding::new("down", MoveDown, context),
        KeyBinding::new("left", MoveLeft, context),
        KeyBinding::new("right", MoveRight, context),
        KeyBinding::new("pageup", MovePageUp, context),
        KeyBinding::new("pagedown", MovePageDown, context),
        KeyBinding::new("home", MoveFirst, context),
        KeyBinding::new("end", MoveLast, context),
        KeyBinding::new("shift-up", MarkUp, context),
        KeyBinding::new("shift-down", MarkDown, context),
        KeyBinding::new("secondary-a", MarkAll, context),
        KeyBinding::new("enter", OpenSelected, context),
        KeyBinding::new("delete", TrashSelected, context),
        KeyBinding::new(TRASH, TrashSelected, context),
    ]);
    // Screening: a decision is one key. Not while typing: a letter there is a letter.
    let screening = Some("Workspace && !TextInput");
    for key in ["y", "1"] { bindings.push(KeyBinding::new(key, DecideIncluded, screening)) }
    for key in ["r", "2"] { bindings.push(KeyBinding::new(key, DecideRelated, screening)) }
    for key in ["u", "3"] { bindings.push(KeyBinding::new(key, DecideUncertain, screening)) }
    for key in ["n", "4"] { bindings.push(KeyBinding::new(key, DecideIrrelevant, screening)) }
    for key in ["p", "0"] { bindings.push(KeyBinding::new(key, DecidePending, screening)) }
    for key in ["space", "x"] { bindings.push(KeyBinding::new(key, TickSuggestion, screening)) }
    bindings.extend([KeyBinding::new("a", TakeSuggestion, screening), KeyBinding::new("s", StarScreened, screening), KeyBinding::new("t", TranslateScreened, screening), KeyBinding::new("z", UndoDecision, screening), KeyBinding::new("g", AskAssistant, screening)]);
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
            "reading" => KeyBinding::new(&key, ToggleReading, context),
            "zoom-in" => KeyBinding::new(&key, ZoomIn, context),
            "zoom-out" => KeyBinding::new(&key, ZoomOut, context),
            "zoom-fit" => KeyBinding::new(&key, ZoomFit, context),
            "write-find" => KeyBinding::new(&key, FindCandidates, context),
            _ => KeyBinding::new(&key, ToggleDetail, context),
        });
    }
    bindings.extend(crate::background::bindings());
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
        "backspace" => if cfg!(target_os = "macos") { "⌫" } else { "Backspace" }.to_string(),
        "delete" => if cfg!(target_os = "macos") { "⌦" } else { "Delete" }.to_string(),
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
