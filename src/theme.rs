//! Design tokens. Colours are semantic (`Theme::text()`, `Theme::line()`) and
//! come in a light and a dark set; the accent follows the open library, so
//! each library looks like itself.

use gpui::{BoxShadow, Hsla, Pixels, SharedString, hsla, point, px, rgb};
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use wisp_core::settings::{self, DEFAULT_FONT_SIZE, ReadingAppearance};

pub const UI_FONT: &str = if cfg!(target_os = "linux") { "Noto Sans" } else { ".SystemUIFont" };
/// For code in an answer.
pub const MONO_FONT: &str = if cfg!(target_os = "macos") { "Menlo" } else if cfg!(target_os = "windows") { "Consolas" } else { "DejaVu Sans Mono" };

thread_local! {
    static READING: Cell<ReadingAppearance> = Cell::new(ReadingAppearance::default());
    static TYPOGRAPHY: RefCell<(SharedString, f32)> = RefCell::new((UI_FONT.into(), 1.));
}

/// Applied on the UI thread before refreshing the whole window, including cached child views.
pub fn set_typography(family: &str, size: f32) {
    TYPOGRAPHY.with(|value| *value.borrow_mut() = (if family.is_empty() { UI_FONT.into() } else { family.to_string().into() }, settings::font_size(size) / DEFAULT_FONT_SIZE));
}

pub fn ui_font() -> SharedString {
    TYPOGRAPHY.with(|value| value.borrow().0.clone())
}

/// Text sizes, line heights and text controls share a scale. PDF page geometry stays in pixels.
pub fn text_px(value: f32) -> Pixels {
    TYPOGRAPHY.with(|typography| px(value * typography.borrow().1))
}

pub fn set_reading(appearance: ReadingAppearance) {
    READING.with(|value| value.set(appearance));
}

/// Long-form text only: controls retain their line heights and hit areas.
pub fn body_line(value: f32) -> Pixels {
    READING.with(|appearance| text_px(value) * appearance.get().line_spacing.line_factor())
}

pub fn paragraph_factor() -> f32 {
    READING.with(|appearance| appearance.get().paragraph_spacing.paragraph_factor())
}

pub fn paragraph_gap(value: f32) -> Pixels {
    text_px(value) * paragraph_factor()
}

pub fn manuscript_width() -> Pixels {
    READING.with(|appearance| px(appearance.get().width.pixels()))
}

static HIGH_CONTRAST: AtomicBool = AtomicBool::new(false);

pub fn set_high_contrast(enabled: bool) {
    HIGH_CONTRAST.store(enabled, Ordering::Relaxed);
}

fn contrast() -> bool {
    HIGH_CONTRAST.load(Ordering::Relaxed)
}

pub fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

static DARK: AtomicBool = AtomicBool::new(false);

/// Whether the dark set is in use (设置 › 外观, or the system's choice).
pub fn dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

pub fn set_dark(dark: bool) {
    DARK.store(dark, Ordering::Relaxed);
}

/// The light or the dark value of a token.
fn pick(light: u32, dark_hex: u32) -> Hsla {
    c(if dark() { dark_hex } else { light })
}

/// A library colour: the strong tone for marks and buttons, the soft one for
/// fills, and the brighter tone that stands in for `strong` on dark surfaces.
#[derive(Clone, Copy)]
pub struct Accent {
    pub key: &'static str,
    strong: u32,
    soft: u32,
    bright: u32,
}

impl Accent {
    pub fn strong(&self) -> Hsla {
        pick(self.strong, self.bright)
    }

    pub fn soft(&self) -> Hsla {
        if dark() { c(self.bright).opacity(0.18) } else { c(self.soft) }
    }
}

/// Same keys and order as `wisp_core::libraries::COLORS`.
pub const ACCENTS: [Accent; 7] = [
    Accent { key: "blue", strong: 0x2F6BDE, soft: 0xE8F0FE, bright: 0x6C9BF2 },
    Accent { key: "green", strong: 0x2E8B57, soft: 0xE4F4EA, bright: 0x57BA85 },
    Accent { key: "amber", strong: 0xB7791F, soft: 0xFBF1DC, bright: 0xDDA94B },
    Accent { key: "rose", strong: 0xC8456B, soft: 0xFCE8EE, bright: 0xE87C9B },
    Accent { key: "violet", strong: 0x7052C8, soft: 0xEEE9FB, bright: 0x9F8AEA },
    Accent { key: "teal", strong: 0x1F8A8A, soft: 0xDFF3F2, bright: 0x4DBCBC },
    Accent { key: "slate", strong: 0x4A5568, soft: 0xE9ECF0, bright: 0x9BA7BA },
];

static ACCENT: AtomicUsize = AtomicUsize::new(0);

pub fn accent_of(key: &str) -> Accent {
    ACCENTS.iter().copied().find(|a| a.key == key).unwrap_or(ACCENTS[0])
}

/// Take the colour of the library being opened (or the default on the home page).
pub fn set_accent(key: &str) {
    ACCENT.store(ACCENTS.iter().position(|a| a.key == key).unwrap_or(0), Ordering::Relaxed);
}

pub struct Theme;

impl Theme {
    /// The window surface the sidebar sits on.
    pub fn bg_window() -> Hsla { pick(0xF5F4F1, 0x1A1B1D) }
    /// The working plane: tables, panels, dialogs.
    pub fn bg_body() -> Hsla { pick(0xFFFFFF, 0x222326) }
    /// Insets on the working plane: inputs at rest, table header, code.
    pub fn bg_sunken() -> Hsla { pick(0xF7F7F5, 0x2A2B2F) }
    pub fn hover() -> Hsla { if dark() { hsla(0., 0., 1., 0.06) } else { hsla(0., 0., 0., 0.045) } }
    pub fn pressed() -> Hsla { if dark() { hsla(0., 0., 1., 0.11) } else { hsla(0., 0., 0., 0.08) } }
    pub fn text() -> Hsla { pick(0x1F2329, 0xE6E7EA) }
    pub fn text_secondary() -> Hsla { if contrast() { pick(0x333942, 0xD8DBE0) } else { pick(0x5C636E, 0xABAFB7) } }
    pub fn text_tertiary() -> Hsla { if contrast() { pick(0x535B66, 0xBBC0C8) } else { pick(0x8F959E, 0x80858E) } }
    pub fn text_placeholder() -> Hsla { if contrast() { pick(0x626B77, 0xA3ABB7) } else { pick(0xB0B5BD, 0x5E636C) } }
    pub fn line() -> Hsla { if contrast() { pick(0x969C95, 0x747A83) } else { pick(0xE7E7E3, 0x333539) } }
    pub fn line_strong() -> Hsla { if contrast() { pick(0x737B74, 0x979EA8) } else { pick(0xD5D6D2, 0x47494F) } }
    pub fn scrim() -> Hsla { hsla(0., 0., 0., if dark() { 0.5 } else { 0.28 }) }
    pub fn danger() -> Hsla { pick(0xD4483B, 0xEB7468) }
    pub fn danger_soft() -> Hsla { pick(0xFDECEA, 0x3B2523) }
    pub fn success() -> Hsla { pick(0x2E8B57, 0x57BA85) }
    /// Something that still wants the reader's attention, short of an error.
    pub fn warning() -> Hsla { pick(0x9A6410, 0xE0B25A) }
    pub fn star() -> Hsla { pick(0xE0A100, 0xE8B92E) }
    pub fn accent() -> Hsla { ACCENTS[ACCENT.load(Ordering::Relaxed)].strong() }
    pub fn accent_soft() -> Hsla { ACCENTS[ACCENT.load(Ordering::Relaxed)].soft() }
    /// Text and icons on an accent or danger fill: the dark set's fills are
    /// bright, so they carry dark text.
    pub fn on_accent() -> Hsla { pick(0xFFFFFF, 0x141518) }
    /// Under the words a comment of a manuscript is about; stronger where the assistant proposes a change; the line beside a comment's quotation.
    pub fn note() -> Hsla { if dark() { hsla(0.11, 0.85, 0.6, 0.22) } else { hsla(0.11, 0.95, 0.55, 0.2) } }
    pub fn note_strong() -> Hsla { if dark() { hsla(0.11, 0.85, 0.6, 0.38) } else { hsla(0.11, 0.95, 0.55, 0.36) } }
    pub fn note_line() -> Hsla { pick(0xE0A100, 0xE8B92E) }
    pub fn selection() -> Hsla { Self::accent().opacity(if dark() { 0.36 } else { 0.22 }) }
    /// The off state of a switch and the groove of a segmented control.
    pub fn groove() -> Hsla { if dark() { hsla(0., 0., 1., 0.12) } else { hsla(0., 0., 0., 0.07) } }
    /// The chosen segment of a segmented control, lighter than its groove.
    pub fn segment() -> Hsla { pick(0xFFFFFF, 0x5A5C63) }
}

/// The tile a model provider's mark is drawn on (设置 › AI): the service's
/// own colour, from the top of the tile to its bottom, the same in both sets.
/// The mark on it is white.
pub fn provider_tile(preset: &str) -> (Hsla, Hsla) {
    let (top, bottom) = match preset {
        "deepseek" => (0x6A86FF, 0x3D5AF1),
        "kimi" => (0x3A3D45, 0x111216),
        "glm" => (0x7B6BFF, 0x4338E0),
        "claude" => (0xE69477, 0xC9603E),
        "openai" => (0x19B48E, 0x0B7A5F),
        "gemini" => (0x5B9BFF, 0x7A5CF0),
        "ark" => (0x2FC4D8, 0x1677E0),
        "typesafe" => (0x2B2F3A, 0x0F1115),
        _ => (0x9AA0AA, 0x6E737D),
    };
    (c(top), c(bottom))
}

/// The (text, fill) of a status chip.
pub fn status_colors(value: &str) -> (Hsla, Hsla) {
    let (light, fill, bright) = match value {
        "included" | "read" | "stored" | "published" => (0x23764A, 0xE4F4EA, 0x63C48F),
        "excluded" | "skimmed" | "linked" => (0x2A5FC4, 0xE8F0FE, 0x7CA8F5),
        "uncertain" | "preprint" | "textless" => (0x9A6410, 0xFBF1DC, 0xE0B25A),
        "irrelevant" => (0xB23B30, 0xFDECEA, 0xEE877C),
        _ => (0x5C636E, 0xEFEFEC, 0xABAFB7),
    };
    if dark() { (c(bright), c(bright).opacity(0.16)) } else { (c(light), c(fill)) }
}

pub fn shadow_panel() -> Vec<BoxShadow> {
    let strength = if dark() { 3. } else { 1. };
    vec![
        BoxShadow { color: hsla(0., 0., 0., 0.10 * strength), offset: point(px(0.), px(8.)), blur_radius: px(28.), spread_radius: px(0.) },
        BoxShadow { color: hsla(0., 0., 0., 0.06 * strength), offset: point(px(0.), px(1.)), blur_radius: px(3.), spread_radius: px(0.) },
    ]
}

pub fn shadow_card() -> Vec<BoxShadow> {
    vec![BoxShadow { color: hsla(0., 0., 0., if dark() { 0.2 } else { 0.05 }), offset: point(px(0.), px(1.)), blur_radius: px(3.), spread_radius: px(0.) }]
}
