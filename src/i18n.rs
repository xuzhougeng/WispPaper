//! Desktop UI translations. Chinese source text is the lookup key; unknown
//! keys fall back unchanged. Paper content and model prompts are not looked up.

use std::collections::HashMap;
use std::sync::{LazyLock, atomic::{AtomicBool, Ordering}};
use wisp_core::settings::Language;

static ENGLISH: AtomicBool = AtomicBool::new(false);
static EN: LazyLock<HashMap<String, String>> = LazyLock::new(|| serde_json::from_str(include_str!("../locales/en.json")).expect("valid English UI catalog"));

pub fn set_language(language: Language) {
    ENGLISH.store(language == Language::En, Ordering::Relaxed);
}

pub fn is_english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

pub fn translate(language: Language, text: &str) -> &str {
    if language == Language::En { EN.get(text).map_or(text, String::as_str) } else { text }
}

pub fn t(text: &str) -> &str {
    translate(if is_english() { Language::En } else { Language::Zh }, text)
}

/// A context key for source words with different meanings, such as Off / Close.
pub fn tc<'a>(key: &str, text: &'a str) -> &'a str {
    if is_english() { EN.get(key).map_or_else(|| t(text), String::as_str) } else { text }
}

/// Stored keys are language independent; translate only a known label.
pub fn label<'a>(table: &'a [(&'a str, &'a str)], value: &'a str) -> &'a str {
    table.iter().find(|(key, _)| *key == value).map_or(value, |(_, text)| t(text))
}

/// `Paper::short_authors` in the interface's language: core says "Zhang 等" for three authors or more.
pub fn short_authors(paper: &wisp_core::model::Paper) -> String {
    let short = paper.short_authors();
    match short.strip_suffix(" 等") {
        Some(first) if is_english() && paper.authors.split(';').filter(|name| !name.trim().is_empty()).count() > 2 => format!("{first} et al."),
        _ => short,
    }
}

pub fn manuscript_title(manuscript: &wisp_core::manuscript::Manuscript) -> &str {
    if manuscript.title.trim().is_empty() { t(wisp_core::manuscript::UNTITLED) } else { manuscript.shown_title() }
}

pub fn assist_title(assist: &wisp_core::ai::Assist, language: &str) -> String {
    use wisp_core::ai::Assist;
    match assist {
        Assist::Rewrite => t("仿写").into(),
        Assist::Polish => t("润色").into(),
        Assist::Cite => t("配引用").into(),
        Assist::Translate if is_english() => format!("Translate to {}", t(&wisp_core::ai::language_name(language))),
        _ => assist.title(language),
    }
}

/// Both format literals are checked by Rust, including named captures and
/// precision specifiers. Values are formatted only by the selected branch.
macro_rules! tf {
    ($zh:literal, $en:literal $(, $args:expr)* $(,)?) => {
        if crate::i18n::is_english() { format!($en $(, $args)*) } else { format!($zh $(, $args)*) }
    };
}

#[cfg(test)]
mod tests {
    use super::{translate, Language};

    #[test]
    fn language_lookup_preserves_unknown_and_user_content() {
        assert_eq!(translate(Language::En, "设置"), "Settings");
        assert_eq!(translate(Language::Zh, "设置"), "设置");
        let title = "我的研究：A study of roots";
        assert_eq!(translate(Language::En, title), title);
        assert_eq!(translate(Language::Zh, "Unknown label"), "Unknown label");
        assert_eq!(translate(Language::En, ""), "");
    }

    #[test]
    fn shared_labels_have_english_translations() {
        use wisp_core::{ai, model, settings::Task, structure};
        for table in [&model::SCREENINGS[..], &model::READINGS, &model::FULLTEXTS, &model::PUBLICATION_STATUSES, &structure::SECTIONS, &structure::LANGUAGES, &ai::EFFORTS] {
            for (_, text) in table { assert_ne!(translate(Language::En, text), *text, "missing translation: {text}") }
        }
        for task in Task::ALL { assert_ne!(translate(Language::En, task.label()), task.label()) }
        for shortcut in &crate::shortcuts::SHORTCUTS { assert_ne!(translate(Language::En, shortcut.label), shortcut.label) }
    }
}
