//! PDF preview. A worker thread owns the parsed file and rasterises pages
//! with hayro (pure Rust, no system PDF library); the view is a virtualised
//! list that asks for the pages on screen and keeps the latest few. The same
//! thread finds a quoted sentence among the glyphs of a page, so a citation
//! can be shown where it stands, and the words typed into the find bar
//! (the shortcut `search` while the viewer has the keyboard, or 自己搜索
//! when a citation's sentence was not found there).

use crate::i18n::t;
use crate::app::Translated;
use crate::theme::{Theme, text_px};
use crate::ui;
use crate::input::{InputEvent, TextInput};
use gpui::{prelude::FluentBuilder, *};
use wisp_core::annotation::{Annotation, COLORS, Kind};
use wisp_core::pdftext::{self, Mark};

actions!(pdf, [CopySelection, FindInPdf]);

/// What the viewer tells whoever shows it.
pub enum PdfEvent {
    /// The selected text went to the clipboard.
    Copied,
    /// Add what is selected to a conversation: the 1-based page, the text, and whether to the library's (else the paper's own).
    Quote(usize, String, bool),
    /// A piece of a page as a picture (PNG) for the next question, and whether for the library's conversation.
    Picture(Vec<u8>, bool),
    /// A piece of a page to keep in the figure library. It stays marked on the page until `PdfView::cut_kept`.
    Figure(Piece),
    /// Translate the selected text: the text, and the model service (its id) asked for this once, else the one translation is bound to.
    /// The answer comes back through `PdfView::show_translation`.
    Translate(String, Option<String>),
    /// Reading metadata, saved by the owner before the viewer redraws it.
    Annotate { paper: String, resource: String, page: usize, kind: Kind, quote: String, boxes: Vec<[f32; 4]> },
    AnnotationNote { resource: String, id: String, note: String },
    /// A highlight or a box takes another colour, a key of `annotation::COLORS`; its range stays.
    RecolorAnnotation { resource: String, id: String, color: &'static str },
    RemoveAnnotation { resource: String, id: String },
    /// A box drawn on a page, or a sticky note placed there (`rect` its spot twice) with its text.
    AddShape { paper: String, resource: String, page: usize, kind: Kind, rect: [f32; 4], note: String },
    /// A box moved or sized, a note moved, on its page.
    MoveShape { resource: String, id: String, rect: [f32; 4] },
}

/// The names of the colours, in the order of `annotation::COLORS`.
const COLOR_NAMES: [&str; 7] = ["黄色", "绿色", "蓝色", "粉色", "紫色", "红色", "橙色"];

/// What the pointer does on a page besides selecting text and cutting pieces out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Select,
    /// Drag out a hollow box round a figure or a passage.
    Box,
    /// Click to put a sticky note there.
    Note,
}

/// The side of a sticky note's icon on screen, whatever the zoom.
const NOTE_ICON: f32 = 22.;
/// How near the pointer must be to a box's edge, or to one of its corners, in screen pixels.
const EDGE_REACH: f32 = 5.;
const GRIP: f32 = 8.;
/// The note card's width.
const NOTE_W: f32 = 280.;

/// A box or a note held by the pointer: its id, 0-based page and place when it was taken, the corner held
/// (none: the whole of it moves), where on the page it was taken, and whether it has moved yet.
struct ShapeDrag {
    id: String,
    page: usize,
    rect: [f32; 4],
    corner: Option<usize>,
    from: (f32, f32),
    moved: bool,
}

/// A highlight that was clicked, and the menu beside it: the highlight's id, its 0-based page, and in that page's points
/// where the click was across and the top and bottom of the line clicked. A box clicked has the same menu.
struct MarkMenu {
    id: String,
    page: usize,
    x: f32,
    line: (f32, f32),
}

/// A piece cut out of a page, for the figure library.
pub struct Piece {
    pub png: Vec<u8>,
    /// The 1-based page it stands on, and its box there in the page's points.
    pub page: usize,
    pub bbox: [f32; 4],
    /// What the page says about it, when a caption stands by it.
    pub caption: Option<wisp_core::figure::Caption>,
}

/// A paragraph of a page with its translation, set in its place on the facing copy of the page.
pub struct FacingPiece {
    /// Where the paragraph stands: `[left, top, right, bottom]` in the page's points.
    pub rect: [f32; 4],
    /// The size the paragraph is set in.
    pub size: f32,
    /// The translation, once there is one; until then the page shows the paragraph itself.
    pub text: Option<SharedString>,
    /// Being asked for.
    pub busy: bool,
}

/// The colour a paragraph stands on, read off the page's picture just outside its box (left, right and
/// above; what two of the three agree on, else the first), and black or white to write on it.
fn ground(image: &RenderImage, page: (f32, f32), rect: [f32; 4]) -> Option<(Hsla, Hsla)> {
    let (size, bytes) = (image.size(0), image.as_bytes(0)?);
    let (width, height) = (i32::from(size.width) as f32, i32::from(size.height) as f32);
    let at = |x: f32, y: f32| {
        let (column, row) = ((x / page.0 * width).clamp(0., width - 1.) as usize, (y / page.1 * height).clamp(0., height - 1.) as usize);
        // BGRA, as the picture is handed to the window.
        bytes.get((row * width as usize + column) * 4..).and_then(|pixel| Some([*pixel.get(2)?, *pixel.get(1)?, *pixel.first()?]))
    };
    let [x0, y0, x1, y1] = rect;
    let seen = [at(x0 - 3., (y0 + y1) / 2.)?, at(x1 + 3., (y0 + y1) / 2.)?, at((x0 + x1) / 2., y0 - 3.)?];
    let near = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= 6);
    let [r, g, b] = if near(seen[1], seen[2]) && !near(seen[0], seen[1]) { seen[1] } else { seen[0] };
    let light = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32 > 140.;
    Some((gpui::rgb((r as u32) << 16 | (g as u32) << 8 | b as u32).into(), if light { gpui::black() } else { gpui::white() }))
}

/// The translation of the selected text, on a card beside it. It lasts as long as that selection.
struct Translation {
    selection: Selection,
    text: String,
    /// The model service asked for this card alone, by id.
    profile: Option<String>,
    state: Translated,
    /// The model the text is translated with: its id (empty for the default one) and its name.
    model: (String, String),
    /// The list of models to translate it with instead is open.
    menu: bool,
}

/// The translation card's width and the height it may grow to, in logical pixels.
const CARD_W: f32 = 360.;
const CARD_TALL: f32 = 280.;
/// The room kept between the card and the viewer's edges, and the gap between it and the selection.
const CARD_EDGE: f32 = 8.;
const CARD_GAP: f32 = 6.;

/// Where the translation card stands in the viewer.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CardPlace {
    left: f32,
    width: f32,
    /// Its top edge, when it hangs under the selection; else its bottom edge, from the viewer's bottom, when it stands over it.
    top: Option<f32>,
    bottom: Option<f32>,
    /// How tall it may be there.
    max_height: f32,
}

/// The card for a selection whose lines cover `anchor` (left, top, right,
/// bottom in the viewer's pixels): under it when the room there takes a
/// card `tall` high or is the larger, else over it; its height held to the
/// room it has, and nothing of it past the viewer's edges, even for a
/// selection scrolled partly out of view.
fn card_place(anchor: [f32; 4], width: f32, tall: f32, (view_w, view_h): (f32, f32)) -> CardPlace {
    let width = width.min(view_w - 2. * CARD_EDGE).max(0.);
    let left = anchor[0].clamp(CARD_EDGE, (view_w - width - CARD_EDGE).max(CARD_EDGE));
    let (low, high) = (CARD_EDGE, (view_h - CARD_EDGE).max(CARD_EDGE));
    let under = (anchor[3] + CARD_GAP).clamp(low, high);
    let over = (anchor[1] - CARD_GAP).clamp(low, high);
    let (room_under, room_over) = (high - under, over - low);
    // A sliver of room is no place for it: past that it may cover the selection.
    let least = 80f32.min(high - low);
    if room_under >= tall || room_under >= room_over {
        let top = under.min(high - least);
        CardPlace { left, width, top: Some(top), bottom: None, max_height: (high - top).min(tall) }
    } else {
        let edge = over.max(low + least);
        CardPlace { left, width, top: None, bottom: Some(view_h - edge), max_height: (edge - low).min(tall) }
    }
}

/// A stretch of one page's glyphs, from where the pointer went down to where it is.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Selection {
    page: usize,
    anchor: usize,
    focus: usize,
}
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;

/// Rendered pages kept in memory at most, and the bytes they may take together:
/// a page at reading width is about 18 MB, one enlarged four times close to 100.
const KEEP_PAGES: usize = 10;
const KEEP_BYTES: usize = 240 << 20;
/// The pixels of one rendered page at most. Past this an enlarged page is
/// drawn softer rather than larger.
const MAX_PAGE_PIXELS: f32 = 24_000_000.;
/// Device pixels per point of the first, quick picture of a page: a quarter of the work of one for a dense display.
const PREVIEW_SCALE: f32 = 1.5;
const PAGE_GAP: f32 = 12.;
/// The room left at either side of a page.
const MARGIN: f32 = 16.;
/// Logical pixels a PDF point takes at 100 %: a point is 1/72 inch, a pixel 1/96.
const POINT_PX: f32 = 96. / 72.;
const ZOOM_MIN: f32 = 25.;
const ZOOM_MAX: f32 = 500.;
/// The sizes the zoom buttons and keys step through, in percent.
const ZOOM_STEPS: [f32; 16] = [25., 33., 50., 67., 75., 90., 100., 110., 125., 150., 175., 200., 250., 300., 400., 500.];

/// How large the pages are drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Zoom {
    /// Each page as wide as the viewer.
    FitWidth,
    /// Each page whole in the viewer.
    FitPage,
    /// A fixed size, in percent of the page's own.
    Percent(f32),
}

impl Zoom {
    /// Whether the pages follow the viewer's size.
    pub fn fits(self) -> bool {
        !matches!(self, Zoom::Percent(_))
    }

    /// What the next paper opens at, this one being left at `self` and
    /// `kept` the fit before: a fit is how the reader reads and stays; a
    /// percentage was for a figure of this paper, and the fit comes back.
    pub fn carried(self, kept: Zoom) -> Zoom {
        if self.fits() { self } else { kept }
    }
}

/// Whether the file is shown twice, each half scrolled on its own: a figure
/// in one, the page that speaks of it in the other.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Split {
    #[default]
    Off,
    /// Two halves side by side.
    Side,
    /// One half over the other.
    Stacked,
}

/// The band between the two halves, in logical pixels: wide enough to tell a page that both halves show from one page.
const DIVIDER: f32 = 4.;

/// The size of a half of a viewer of this size; all of it while it is not split.
fn pane_size(split: Split, view: (f32, f32)) -> (f32, f32) {
    match split {
        Split::Off => view,
        Split::Side => (((view.0 - DIVIDER) / 2.).max(0.), view.1),
        Split::Stacked => (view.0, ((view.1 - DIVIDER) / 2.).max(0.)),
    }
}

/// The half of a split viewer a point of it is in, the point given from the viewer's top left corner.
fn pane_at(split: Split, view: (f32, f32), at: (f32, f32)) -> usize {
    let half = pane_size(split, view);
    match split {
        Split::Off => 0,
        Split::Side => (at.0 >= half.0 + DIVIDER) as usize,
        Split::Stacked => (at.1 >= half.1 + DIVIDER) as usize,
    }
}

/// One view of the pages: where it is scrolled to.
struct Pane {
    list: ListState,
    /// How far the pages are moved to the left, when they are wider than the view.
    scroll_x: f32,
}

impl Pane {
    fn new() -> Self {
        Pane { list: ListState::new(0, ListAlignment::Top, px(600.)), scroll_x: 0. }
    }
}

/// A change of zoom asked for from the toolbar, the menu or the keyboard.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ZoomTo {
    In,
    Out,
    Set(Zoom),
}

/// Logical pixels per point for a page of this size (in points) in a viewer of this size.
fn page_scale(zoom: Zoom, page: (f32, f32), view: (f32, f32)) -> f32 {
    let fit_width = (view.0 - 2. * MARGIN).max(120.) / page.0.max(1.);
    match zoom {
        Zoom::FitWidth => fit_width,
        // Until the viewer has been laid out its height is not known: the width decides.
        Zoom::FitPage if view.1 > 0. => fit_width.min((view.1 - 2. * PAGE_GAP).max(120.) / page.1.max(1.)),
        Zoom::FitPage => fit_width,
        Zoom::Percent(percent) => percent.clamp(ZOOM_MIN, ZOOM_MAX) / 100. * POINT_PX,
    }
}

/// Device pixels per point a page is rendered at to be shown at `wanted`:
/// the next of a few steps, so that a small change of zoom keeps the
/// picture, within what one picture may take.
fn render_scale(page: (f32, f32), wanted: f32) -> f32 {
    let step = [2., 3., 4., 6., 8., 12., 16.].into_iter().find(|step| *step >= wanted - 0.01).unwrap_or(16.);
    // Pixmaps are at most 65535 px a side; huge posters render smaller.
    step.min((MAX_PAGE_PIXELS / (page.0 * page.1).max(1.)).sqrt()).min(8000. / page.0.max(page.1).max(1.))
}

/// Whether a picture rendered at `at` device pixels per point still serves
/// a page that wants `wanted` (its `render_scale`) and is shown at `shown`:
/// never softer, and up to half again sharper, so that a viewer made a
/// little narrower (a panel dragged, the window resized) does not render
/// its pages again, while one zoomed far out lets the large pictures go.
fn picture_serves(at: f32, wanted: f32, shown: f32) -> bool {
    at >= wanted - 0.01 && at <= wanted.max(shown * 1.5) + 0.01
}

/// The next size up or down from this one, in percent.
/// The bar that moves pages wider than their view sideways: how far its
/// track stands in from the view's edges, how tall the strip that hears the
/// pointer is, and the shortest its thumb gets.
const BAR_INSET: f32 = 8.;
const BAR_HEIGHT: f32 = 14.;
const BAR_THUMB_MIN: f32 = 36.;

/// The thumb of the sideways bar in a view `width` wide whose pages can go
/// `max_x` to the left: its length, and how far along the track it stands.
fn bar_thumb(width: f32, max_x: f32, scroll_x: f32) -> (f32, f32) {
    let track = (width - 2. * BAR_INSET).max(1.);
    let thumb = (track * width / (width + max_x)).max(BAR_THUMB_MIN).min(track);
    (thumb, (track - thumb) * (scroll_x / max_x.max(1.)).clamp(0., 1.))
}

fn zoom_step(percent: f32, up: bool) -> f32 {
    if up { ZOOM_STEPS.into_iter().find(|step| *step > percent + 0.5).unwrap_or(ZOOM_MAX) } else { ZOOM_STEPS.into_iter().rev().find(|step| *step < percent - 0.5).unwrap_or(ZOOM_MIN) }
}

/// The pages a quotation is looked for on before giving up.
const FIND_PAGES: usize = 300;
/// Pages whose glyphs the worker keeps once read, for the next search.
const KEEP_GLYPHS: usize = 120;
/// Room the find bar and the citation's notice leave at the right of the viewer, for 标注's button.
const FIND_CLEAR: f32 = 120.;
/// Words of a sentence not found tried at most, one after another, until one is on some page.
const SEARCH_TRIES: usize = 6;
/// Places the find bar shows at most.
const SEARCH_HITS: usize = 999;
/// Page renders kept from a burst of requests; older ones are handed back.
const KEEP_REQUESTS: usize = 3;

#[derive(Default, Debug, PartialEq)]
enum CitationStatus {
    #[default]
    None,
    PageOnly,
    Searching,
    Found,
    /// Only the beginning of the quotation is on the page.
    Partial,
    Missing,
}

/// The latest citation click, independent of when its PDF or search finishes.
#[derive(Default)]
struct CitationFocus {
    request: u64,
    page: Option<usize>,
    boxes: Vec<[f32; 4]>,
    status: CitationStatus,
    /// The sentence looked for: what 自己搜索 starts from when it was not found.
    quote: String,
}

impl CitationFocus {
    fn clear(&mut self) {
        *self = Self { request: self.request.wrapping_add(1), ..Self::default() };
    }

    fn begin(&mut self, page: usize, quote: &str) -> u64 {
        self.clear();
        self.page = Some(page.saturating_sub(1));
        self.quote = quote.to_string();
        self.status = if quote.trim().is_empty() { CitationStatus::PageOnly } else { CitationStatus::Searching };
        self.request
    }

    fn index(&self, count: usize) -> Option<usize> {
        Some(self.page?.min(count.checked_sub(1)?))
    }

    /// A late search must never replace a newer citation or move back to it.
    fn finish(&mut self, request: u64, found: Option<(usize, Vec<[f32; 4]>, bool)>) -> bool {
        if request != self.request || self.status != CitationStatus::Searching { return false }
        match found.filter(|(_, boxes, _)| !boxes.is_empty()) {
            Some((page, boxes, whole)) => {
                self.page = Some(page);
                self.boxes = boxes;
                self.status = if whole { CitationStatus::Found } else { CitationStatus::Partial };
            }
            None => self.status = CitationStatus::Missing,
        }
        true
    }
}

/// The find bar: words typed to look for in the whole file, the places found, and the one gone to.
struct Finder {
    input: Entity<TextInput>,
    _heard: Subscription,
    request: u64,
    hits: Vec<(usize, Vec<[f32; 4]>)>,
    at: Option<usize>,
    searching: bool,
    /// Opened because a citation's sentence was not found in the file: said under the bar.
    missed: bool,
    /// More words of that sentence to try while the one in the bar is on no page.
    tries: Vec<String>,
}

/// Words of a sentence the PDF did not have word for word, to look for instead, the likeliest first: its
/// longer words of letters (terms, names) by length, then pieces of four of its Chinese characters.
pub(crate) fn search_words(quote: &str) -> Vec<String> {
    let is_cjk = wisp_core::structure::is_cjk;
    let mut words: Vec<String> = vec![];
    for word in quote.split(|c: char| !c.is_alphanumeric() && c != '-').map(|w| w.trim_matches('-')).filter(|w| w.chars().count() >= 4 && !w.chars().any(is_cjk)) {
        if !words.iter().any(|known| known.eq_ignore_ascii_case(word)) { words.push(word.to_string()) }
    }
    // Longest first; of the same length, in the sentence's order.
    words.sort_by_key(|w| std::cmp::Reverse(w.chars().count()));
    let letters: Vec<char> = quote.chars().collect();
    for run in letters.split(|c| !is_cjk(*c)).filter(|run| run.len() >= 2) {
        words.extend(run.chunks(4).filter(|piece| piece.len() >= 2).map(|piece| piece.iter().collect::<String>()));
    }
    words.truncate(SEARCH_TRIES);
    words
}

enum Request {
    /// Render a page at so many device pixels per point.
    Page(usize, f32),
    /// Look for a quoted sentence, starting at this 0-based page.
    Find(u64, Option<usize>, String),
    /// Look for words typed into the find bar, on every page.
    Search(u64, String),
    /// The glyphs of a page, for selecting text on it.
    Marks(usize),
}

enum Reply {
    /// Page sizes in points, once, after the file parsed.
    Opened(Vec<(f32, f32)>),
    Failed(String),
    /// A rendered page: its index, the scale it was rendered at, its size in pixels and the pixels.
    Page(usize, f32, u32, u32, Vec<u8>),
    /// A page whose render was dropped for newer ones: ask again when it is in view.
    Skipped(usize),
    /// Where the quotation is: the page, one box per line in page points, and whether that is all of it.
    Found(u64, usize, Vec<[f32; 4]>, bool),
    NotFound(u64),
    /// Where the words of the find bar stand: each place's page and one box per line, in reading order.
    Hits(u64, Vec<(usize, Vec<[f32; 4]>)>),
    Marks(usize, Vec<Mark>),
}

/// Collects the glyphs of a page as it is interpreted: what each one says
/// and where it stands, in page points with the origin at the top left.
#[derive(Default)]
struct Glyphs {
    texts: Vec<String>,
    boxes: Vec<[f32; 4]>,
}

mod device {
    use super::Glyphs;
    use hayro::hayro_interpret::font::GlyphRun;
    use hayro::hayro_interpret::hayro_cmap::BfString;
    use hayro::hayro_interpret::{BlendMode, ClipPath, Device, DrawMode, DrawProps, Image, ImageDrawProps, SoftMask};
    use hayro::kurbo::{BezPath, Point};

    impl<'a> Device<'a> for Glyphs {
        fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, _: &DrawMode) {
            for glyph in run.glyphs() {
                let text = match glyph.as_unicode() {
                    Some(BfString::Char(c)) => c.to_string(),
                    Some(BfString::String(text)) => text,
                    None => continue,
                };
                // Glyph space is a thousandth of the font size. The advance is not known
                // here: half an em from the origin, from below the baseline to the cap height.
                let place = props.transform * glyph.transform();
                let corners = [(0., -220.), (0., 780.), (500., -220.), (500., 780.)].map(|(x, y)| place * Point::new(x, y));
                let (xs, ys) = (corners.map(|p| p.x as f32), corners.map(|p| p.y as f32));
                self.texts.push(text);
                self.boxes.push([xs.iter().copied().fold(f32::MAX, f32::min), ys.iter().copied().fold(f32::MAX, f32::min), xs.iter().copied().fold(f32::MIN, f32::max), ys.iter().copied().fold(f32::MIN, f32::max)]);
            }
        }
        fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}
        fn push_clip_path(&mut self, _: &ClipPath) {}
        fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
        fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}
        fn pop_clip(&mut self) {}
        fn pop_transparency_group(&mut self) {}
    }
}

/// The pages of a PDF from `skip` on, at most `limit` of them, as PNG
/// pictures for a model to read a file that gives no text of its own, and
/// how many pages the file has. One and a half pixels per point: enough
/// to read print by, and small enough to send.
pub fn page_pictures(path: &Path, skip: usize, limit: usize) -> Result<(Vec<Vec<u8>>, usize), String> {
    use hayro::hayro_interpret::InterpreterSettings;
    use hayro::hayro_syntax::Pdf;
    use hayro::vello_cpu::color::palette::css::WHITE;
    use hayro::{PixmapSettings, RenderCache, RenderSettings, render};
    let data = std::fs::read(path).map_err(|err| tf!("无法读取文件：{err}", "Cannot read file: {err}"))?;
    let pdf = Pdf::new(data).map_err(|err| tf!("无法解析这个 PDF（{err:?}）。", "Cannot parse this PDF ({err:?})."))?;
    let (cache, interpreter, settings) = (RenderCache::new(), InterpreterSettings::default(), RenderSettings::default());
    let mut out = vec![];
    let count = pdf.pages().len();
    for page in pdf.pages().iter().skip(skip).take(limit) {
        let (w, h) = page.render_dimensions();
        let scale = 1.5_f32.min(2000. / w.max(h).max(1.));
        let pixmap = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| render(page, &cache, &interpreter, &settings, &PixmapSettings { x_scale: scale, y_scale: scale, bg_color: WHITE }))).map_err(|_| t("这个 PDF 的页面画不出来。").to_string())?;
        // On an opaque white background premultiplied colour is the colour itself.
        let picture = image::RgbaImage::from_raw(pixmap.width() as u32, pixmap.height() as u32, pixmap.data_as_u8_slice().to_vec()).ok_or(t("这个 PDF 的页面画不出来。"))?;
        let mut png = vec![];
        image::DynamicImage::ImageRgba8(picture).to_rgb8().write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).map_err(|err| tf!("页面图像写不出来：{err}", "Cannot write page image: {err}"))?;
        out.push(png);
    }
    if out.is_empty() { return Err(t("这个 PDF 没有页面。").into()) }
    Ok((out, count))
}

fn worker(path: PathBuf, requests: mpsc::Receiver<Request>, replies: async_channel::Sender<Reply>) {
    use hayro::hayro_interpret::{Context, InterpreterCache, InterpreterSettings, TransformExt, interpret_page};
    use hayro::hayro_syntax::Pdf;
    use hayro::kurbo::Rect;
    use hayro::vello_cpu::color::palette::css::WHITE;
    use hayro::{PixmapSettings, RenderCache, RenderSettings, render};
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(err) => {
            let _ = replies.send_blocking(Reply::Failed(tf!("无法读取文件：{err}", "Cannot read file: {err}")));
            return;
        }
    };
    let pdf = match Pdf::new(data) {
        Ok(pdf) => pdf,
        Err(err) => {
            let _ = replies.send_blocking(Reply::Failed(tf!("无法解析这个 PDF（{err:?}）。可以改用系统应用打开。", "Cannot parse this PDF ({err:?}). Try opening it with a system app.")));
            return;
        }
    };
    let pages = pdf.pages();
    if replies.send_blocking(Reply::Opened(pages.iter().map(|p| p.render_dimensions()).collect())).is_err() { return }
    let cache = RenderCache::new();
    let text_cache = InterpreterCache::new();
    let (interpreter, settings) = (InterpreterSettings::default(), RenderSettings::default());
    // The glyphs of pages read for a quotation or a search, kept for the next one (of a long file, the first pages read).
    let mut read: HashMap<usize, Arc<Glyphs>> = HashMap::new();
    let glyphs_of = |index: usize, read: &mut HashMap<usize, Arc<Glyphs>>| -> Option<Arc<Glyphs>> {
        if let Some(known) = read.get(&index) { return Some(known.clone()) }
        let page = pages.get(index)?;
        let (w, h) = page.render_dimensions();
        let glyphs = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut glyphs = Glyphs::default();
            let mut context = Context::new(page.initial_transform(true).to_kurbo(), Rect::new(0., 0., w as f64, h as f64), &text_cache, page.xref(), interpreter.clone());
            interpret_page(page, &mut context, &mut glyphs);
            glyphs
        }))
        .ok()
        .map(Arc::new)?;
        if read.len() < KEEP_GLYPHS { read.insert(index, glyphs.clone()); }
        Some(glyphs)
    };
    while let Ok(first) = requests.recv() {
        let mut waiting = vec![first];
        waiting.extend(requests.try_iter());
        // Of several quotations asked for in a row, only the last still matters.
        let find = waiting.iter().rev().find_map(|request| match request {
            Request::Find(request, page, quote) => Some((*request, *page, quote.clone())),
            Request::Page(..) | Request::Marks(_) | Request::Search(..) => None,
        });
        let search = waiting.iter().rev().find_map(|request| match request {
            Request::Search(request, words) => Some((*request, words.clone())),
            _ => None,
        });
        // Scrolling queues pages faster than they render: the latest few are drawn,
        // newest first, and the view is told about the rest so it can ask again.
        let mut wanted: Vec<(usize, f32)> = vec![];
        for request in waiting.iter().rev() {
            if let Request::Page(index, scale) = request && !wanted.iter().any(|(known, _)| known == index) { wanted.push((*index, *scale)) }
        }
        for (skipped, _) in wanted.split_off(wanted.len().min(KEEP_REQUESTS)) {
            if replies.send_blocking(Reply::Skipped(skipped)).is_err() { return }
        }

        if let Some((request, start, quote)) = find {
            // The page named first, then its neighbours, then the rest in order.
            let mut order: Vec<usize> = start.into_iter().flat_map(|page| [Some(page), page.checked_sub(1), Some(page + 1)]).flatten().filter(|page| *page < pages.len()).collect();
            let rest: Vec<usize> = (0..pages.len().min(FIND_PAGES)).filter(|page| !order.contains(page)).collect();
            order.extend(rest);
            let mut reply = Reply::NotFound(request);
            for index in order {
                let Some(glyphs) = glyphs_of(index, &mut read) else { continue };
                // The whole quotation on a later page is better than its beginning on this one.
                if let Some((range, whole)) = wisp_core::fulltext::find_quote(&glyphs.texts, &glyphs.boxes, &quote) {
                    if whole || matches!(reply, Reply::NotFound(_)) { reply = Reply::Found(request, index, wisp_core::fulltext::line_boxes(&glyphs.boxes[range]), whole) }
                    if whole { break }
                }
            }
            if replies.send_blocking(reply).is_err() { return }
        }

        if let Some((request, words)) = search {
            let mut hits = vec![];
            for index in 0..pages.len().min(FIND_PAGES) {
                if hits.len() >= SEARCH_HITS { break }
                let Some(glyphs) = glyphs_of(index, &mut read) else { continue };
                hits.extend(wisp_core::fulltext::find_all(&glyphs.texts, &glyphs.boxes, &words).into_iter().map(|range| (index, wisp_core::fulltext::line_boxes(&glyphs.boxes[range]))));
            }
            hits.truncate(SEARCH_HITS);
            if replies.send_blocking(Reply::Hits(request, hits)).is_err() { return }
        }

        for (index, scale) in wanted {
            let Some(page) = pages.get(index) else { continue };
            let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| render(page, &cache, &interpreter, &settings, &PixmapSettings { x_scale: scale, y_scale: scale, bg_color: WHITE })));
            let reply = match rendered {
                Ok(pixmap) => {
                    let (width, height) = (pixmap.width() as u32, pixmap.height() as u32);
                    // Opaque white background, so premultiplied equals straight; GPUI wants BGRA.
                    let mut bytes = pixmap.data_as_u8_slice().to_vec();
                    for pixel in bytes.as_chunks_mut::<4>().0 {
                        pixel.swap(0, 2);
                    }
                    Reply::Page(index, scale, width, height, bytes)
                }
                Err(_) => Reply::Page(index, scale, 0, 0, vec![]),
            };
            if replies.send_blocking(reply).is_err() { return }
        }

        // The glyphs of the pages asked for, once the pages themselves are drawn: a page is looked at before it is selected in.
        let mut marked: Vec<usize> = vec![];
        for request in &waiting {
            if let Request::Marks(index) = request && !marked.contains(index) { marked.push(*index) }
        }
        for index in marked {
            let Some(page) = pages.get(index) else { continue };
            if replies.send_blocking(Reply::Marks(index, pdftext::page_marks(page, &text_cache))).is_err() { return }
        }
    }
}

pub struct PdfView {
    sizes: Vec<(f32, f32)>,
    /// The rendered pages: the picture, the scale it was rendered at, the bytes it takes.
    images: HashMap<usize, (Arc<RenderImage>, f32, usize)>,
    /// Pages that failed to render.
    broken: HashSet<usize>,
    /// Most recently shown last.
    recent: VecDeque<usize>,
    pending: HashSet<usize>,
    /// The views of the pages: the second is on show only while the viewer is split.
    panes: [Pane; 2],
    split: Split,
    /// 译文: the second half shows the pages with their paragraphs translated in place, and the two halves
    /// scroll as one. By page (0-based), what stands in place of what (`Workspace::sync_facing`).
    facing: Option<HashMap<usize, Vec<FacingPiece>>>,
    /// How the viewer was split before the translation took its second half.
    facing_was: Split,
    /// The half the pointer last worked in: the one the page number, the keys and a citation speak of.
    active: usize,
    /// The half the selection, the piece cut out or the highlight clicked was made in: what is offered for it stands there.
    held: usize,
    requests: mpsc::Sender<Request>,
    citation: CitationFocus,
    error: Option<String>,
    /// The viewer's size in logical pixels; the height is known once it has been laid out.
    width: f32,
    height: f32,
    zoom: Zoom,
    /// How far the pages can be moved to the left, when they are wider than the view.
    max_x: f32,
    /// The sideways bar held by the pointer: its view, and how far from the thumb's left end it is held.
    sliding: Option<(usize, f32)>,
    /// Device pixels per logical pixel of the window the viewer is in.
    device_scale: f32,
    /// A page to jump to once the file has opened.
    goto: Option<usize>,
    /// A reading place to come back to once the file has opened (`place`).
    resume: Option<(usize, f32)>,
    /// The glyphs of the pages seen lately, and the pages they were asked for.
    marks: HashMap<usize, Arc<Vec<Mark>>>,
    marks_pending: HashSet<usize>,
    selection: Option<Selection>,
    /// The pointer is held down and drawing the selection out.
    selecting: bool,
    /// Whether the library's conversation is on show beside the paper's, to offer both for a quotation.
    pub library_chat: bool,
    /// The pointer cuts a piece out of a page instead of selecting its text.
    cutting: bool,
    /// The piece being cut or cut out: its page, and two corners in the page's points.
    cut: Option<(usize, [f32; 4])>,
    /// The translation asked for the selection.
    translation: Option<Translation>,
    /// The model services the card offers, by id and name.
    models: Vec<(String, String)>,
    paper_id: String,
    resource_id: String,
    annotations: Vec<Annotation>,
    annotations_open: bool,
    annotation_edit: Option<String>,
    annotation_note: Entity<TextInput>,
    /// The highlight the pointer went down on, and where in the window: a release close by is a click on it.
    pressed_mark: Option<(MarkMenu, Point<Pixels>)>,
    /// The menu of a highlight or a box clicked: its colours and 删除.
    mark_menu: Option<MarkMenu>,
    /// What the pointer does on a page; cutting is apart from it (`cutting`).
    tool: Tool,
    /// The box being drawn: its page, and two corners in the page's points.
    drawing: Option<(usize, [f32; 4])>,
    /// A box or note held by the pointer, and where it is shown meanwhile (until the owner saves it).
    shape_drag: Option<ShapeDrag>,
    shape_shown: Option<(String, [f32; 4])>,
    /// The sticky note whose card is open, and the one being written before it is kept: its page and spot.
    note_open: Option<String>,
    note_draft: Option<(usize, f32, f32)>,
    /// The open card's text is being written.
    note_editing: bool,
    note_input: Entity<TextInput>,
    focus: FocusHandle,
    finder: Option<Finder>,
    /// Counts the searches asked for, so a late answer to an older one is dropped.
    searches: u64,
}

impl EventEmitter<PdfEvent> for PdfView {}

impl PdfView {
    pub fn new(path: &Path, zoom: Zoom, file: &wisp_core::model::PaperFile, annotations: Vec<Annotation>, cx: &mut Context<Self>) -> Self {
        let (requests, request_rx) = mpsc::channel();
        let (reply_tx, replies) = async_channel::unbounded();
        let worker_file = path.to_path_buf();
        std::thread::Builder::new().name("pdf".into()).spawn(move || worker(worker_file, request_rx, reply_tx)).ok();
        cx.spawn(async move |this, cx| {
            while let Ok(reply) = replies.recv().await {
                if this.update(cx, |this, cx| this.receive(reply, cx)).is_err() { break }
            }
        })
        .detach();
        // A page's picture stays in the window's atlas until it is let go of there: dropping the view alone would leave every page it drew.
        cx.on_release(|this: &mut Self, cx| {
            for (_, (image, ..)) in this.images.drain() { cx.drop_image(image, None) }
        })
        .detach();
        PdfView {
            sizes: vec![],
            images: HashMap::new(),
            broken: HashSet::new(),
            recent: VecDeque::new(),
            pending: HashSet::new(),
            panes: [Pane::new(), Pane::new()],
            split: Split::Off,
            facing: None,
            facing_was: Split::Off,
            active: 0,
            held: 0,
            requests,
            error: None,
            width: 0.,
            height: 0.,
            zoom,
            max_x: 0.,
            sliding: None,
            device_scale: 2.,
            goto: None,
            resume: None,
            citation: CitationFocus::default(),
            marks: HashMap::new(),
            marks_pending: HashSet::new(),
            selection: None,
            selecting: false,
            library_chat: false,
            cutting: false,
            cut: None,
            translation: None,
            models: vec![],
            paper_id: file.paper_id.clone(),
            resource_id: file.id.clone(),
            annotations,
            annotations_open: false,
            annotation_edit: None,
            annotation_note: cx.new(|cx| TextInput::new("写下这段原文的笔记…", cx).multiline(3)),
            pressed_mark: None,
            mark_menu: None,
            tool: Tool::Select,
            drawing: None,
            shape_drag: None,
            shape_shown: None,
            note_open: None,
            note_draft: None,
            note_editing: false,
            note_input: cx.new(|cx| TextInput::new("写下想法…", cx).multiline(4)),
            focus: cx.focus_handle(),
            finder: None,
            searches: 0,
        }
    }

    pub fn tool(&self) -> Tool {
        self.tool
    }

    /// Draw boxes, place notes, or go back to selecting text; a second click on the same tool goes back too.
    pub fn set_tool(&mut self, tool: Tool, cx: &mut Context<Self>) {
        self.tool = if self.tool == tool { Tool::Select } else { tool };
        (self.cutting, self.cut, self.drawing, self.selection, self.selecting, self.translation, self.mark_menu) = (false, None, None, None, false, None, None);
        self.close_note();
        cx.notify();
    }

    /// Where a box or note stands now: moved by the pointer, or as kept.
    fn rect_of(&self, a: &Annotation) -> [f32; 4] {
        match &self.shape_shown {
            Some((id, rect)) if *id == a.id => *rect,
            _ => a.boxes.first().copied().unwrap_or_default(),
        }
    }

    /// The note or box under a point of a 0-based page, and the corner of a box held when it is the one chosen.
    /// A note is its icon; a box is its edge, or anywhere within it once chosen. The last made is on top.
    fn shape_at(&self, page: usize, x: f32, y: f32) -> Option<(String, Option<usize>)> {
        let scale = self.scale_of(page);
        let chosen = self.mark_menu.as_ref().map(|menu| menu.id.as_str());
        self.annotations.iter().rev().filter(|a| a.page == page + 1 && a.kind.shape()).find_map(|a| {
            let [x0, y0, x1, y1] = self.rect_of(a);
            if a.kind == Kind::Note {
                let side = NOTE_ICON / scale;
                return (x >= x0 && x <= x0 + side && y >= y0 && y <= y0 + side).then(|| (a.id.clone(), None));
            }
            let (reach, grip) = (EDGE_REACH / scale, GRIP / scale);
            if chosen == Some(a.id.as_str()) {
                let corners = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)];
                if let Some(corner) = corners.iter().position(|(cx, cy)| (x - cx).abs() <= grip && (y - cy).abs() <= grip) { return Some((a.id.clone(), Some(corner))) }
                if x >= x0 - reach && x <= x1 + reach && y >= y0 - reach && y <= y1 + reach { return Some((a.id.clone(), None)) }
                return None;
            }
            let inside = |x: f32, y: f32, grow: f32| x >= x0 - grow && x <= x1 + grow && y >= y0 - grow && y <= y1 + grow;
            (inside(x, y, reach) && !inside(x, y, -reach)).then(|| (a.id.clone(), None))
        })
    }

    /// Put the note card away, and a note not yet kept with it.
    fn close_note(&mut self) {
        (self.note_open, self.note_draft, self.note_editing) = (None, None, false);
    }

    /// Open a note's card, to read or (`edit`) to write.
    fn open_note(&mut self, id: String, edit: bool, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.annotations.iter().find(|a| a.id == id).map(|a| a.note.clone()).unwrap_or_default();
        (self.note_open, self.note_draft, self.note_editing, self.mark_menu) = (Some(id), None, edit, None);
        self.note_input.update(cx, |input, cx| input.set_text(&text, cx));
        if edit { self.note_input.read(cx).focus(window) }
        cx.notify();
    }

    /// Keep what the open card says: a new note, or the text of one there.
    fn save_note(&mut self, cx: &mut Context<Self>) {
        let note = self.note_input.read(cx).text().trim().to_string();
        match (self.note_draft, self.note_open.clone()) {
            (Some((page, x, y)), _) => {
                // A note with nothing written is not kept.
                if note.is_empty() { return self.close_note() }
                cx.emit(PdfEvent::AddShape { paper: self.paper_id.clone(), resource: self.resource_id.clone(), page: page + 1, kind: Kind::Note, rect: [x, y, x, y], note });
            }
            (None, Some(id)) => cx.emit(PdfEvent::AnnotationNote { resource: self.resource_id.clone(), id, note }),
            (None, None) => {}
        }
        cx.notify();
    }

    /// The sticky note's card: beside the page where there is room, so it covers no text, else beside its icon.
    fn note_card(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (page, spot, text) = match (self.note_draft, &self.note_open) {
            (Some((page, x, y)), _) => (page, (x, y), String::new()),
            (None, Some(id)) => {
                let a = self.annotations.iter().find(|a| &a.id == id)?;
                let [x, y, ..] = self.rect_of(a);
                (a.page - 1, (x, y), a.note.clone())
            }
            (None, None) => return None,
        };
        let scale = self.scale_of(page);
        let (sheet_x, sheet_y) = self.sheet_place(page)?;
        let sheet_w = self.sizes.get(page)?.0 * scale;
        let (icon_x, icon_y) = (sheet_x + spot.0 * scale, sheet_y + spot.1 * scale);
        let room_right = self.width - (sheet_x + sheet_w);
        let left = if room_right >= NOTE_W + 16. {
            sheet_x + sheet_w + 8.
        } else if sheet_x >= NOTE_W + 16. {
            sheet_x - NOTE_W - 8.
        } else if icon_x + NOTE_ICON + 6. + NOTE_W <= self.width - 8. {
            icon_x + NOTE_ICON + 6.
        } else {
            icon_x - NOTE_W - 6.
        };
        let left = left.clamp(8., (self.width - NOTE_W - 8.).max(8.));
        let top = icon_y.clamp(8., (self.height - 200.).max(8.));
        let button = |id: &'static str, label: &'static str| div().id(id).flex().flex_none().items_center().h(text_px(24.)).px(px(8.)).rounded(px(6.)).cursor_pointer().hover(|s| s.bg(Theme::hover())).child(label);
        let existing = self.note_open.clone().filter(|_| self.note_draft.is_none());
        let body = if self.note_editing {
            div().flex().flex_col().gap(px(8.))
                .child(ui::field(&self.note_input, window, cx))
                .child(div().flex().items_center().gap(px(4.))
                    .child(button("pdf-note-save", t("保存")).text_color(Theme::accent()).on_click(cx.listener(|this, _, _, cx| this.save_note(cx))))
                    .child(button("pdf-note-cancel", t("取消")).on_click(cx.listener(|this, _, _, cx| {
                        if this.note_draft.is_some() { this.close_note() } else { this.note_editing = false }
                        cx.notify();
                    }))))
        } else {
            let edit = existing.clone();
            div().flex().flex_col().gap(px(8.))
                .child(div().line_height(text_px(20.)).text_color(if text.is_empty() { Theme::text_placeholder() } else { Theme::text() }).child(if text.is_empty() { t("（空白便签）").to_string() } else { text }))
                .child(div().flex().items_center().gap(px(4.))
                    .child(button("pdf-note-edit", t("编辑")).text_color(Theme::accent()).on_click(cx.listener(move |this, _, window, cx| if let Some(id) = edit.clone() { this.open_note(id, true, window, cx) })))
                    .child(div().flex_1())
                    .when_some(existing.clone(), |d, id| d.child(button("pdf-note-remove", t("删除")).text_color(Theme::danger()).on_click(cx.listener(move |this, _, _, cx| {
                        this.close_note();
                        cx.emit(PdfEvent::RemoveAnnotation { resource: this.resource_id.clone(), id: id.clone() });
                        cx.notify();
                    })))))
        };
        Some(
            div()
                .id("pdf-note-card")
                .absolute()
                .left(px(left))
                .top(px(top))
                .w(px(NOTE_W))
                .max_h(px((self.height - 16.).max(0.)))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(8.))
                .p(px(12.))
                .rounded(px(10.))
                .bg(Theme::bg_body())
                .border_1()
                .border_color(Theme::line_strong())
                .shadow(crate::theme::shadow_card())
                .text_size(text_px(13.))
                .occlude()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(div().flex().items_center().gap(px(6.)).text_size(text_px(12.)).text_color(Theme::text_tertiary())
                    .child(ui::icon("sticky-note", 14., Theme::warning()))
                    .child(div().flex_1().child(tf!("第 {} 页 · 便签", "Page {} · Note", page + 1)))
                    .child(ui::ghost_button("pdf-note-close", Some("x"), None).h(text_px(20.)).on_click(cx.listener(|this, _, _, cx| { this.close_note(); cx.notify(); }))))
                .child(body)
                .into_any_element(),
        )
    }

    pub fn page_count(&self) -> usize {
        self.sizes.len()
    }

    /// Called only after a successful database write. Failed writes leave the
    /// selection or draft intact, so the user can retry without losing text.
    pub fn annotations_saved(&mut self, annotations: Vec<Annotation>, cx: &mut Context<Self>) {
        self.annotations = annotations;
        self.selection = None;
        self.annotation_edit = None;
        self.mark_menu = None;
        (self.shape_drag, self.shape_shown, self.drawing) = (None, None, None);
        // A note just kept, or one whose text was saved, stays open to read; one gone goes with its card.
        if self.note_open.as_ref().is_some_and(|id| !self.annotations.iter().any(|a| &a.id == id)) { self.note_open = None }
        (self.note_draft, self.note_editing) = (None, false);
        cx.notify();
    }

    pub fn annotate(&mut self, kind: Kind, cx: &mut Context<Self>) {
        let Some((page, quote)) = self.selected() else { return };
        let Some(selection) = self.selection else { return };
        let Some(marks) = self.marks.get(&selection.page).and_then(|marks| marks.get(selection.anchor.min(selection.focus)..=selection.anchor.max(selection.focus))) else { return };
        let boxes = pdftext::mark_boxes(marks);
        if boxes.is_empty() { return }
        cx.emit(PdfEvent::Annotate { paper: self.paper_id.clone(), resource: self.resource_id.clone(), page, kind, quote, boxes });
    }

    /// The page at the top of the view, 1-based: of a split viewer, the half last worked in.
    pub fn current_page(&self) -> usize {
        self.list().logical_scroll_top().item_ix + 1
    }

    /// The pages as the half last worked in shows them.
    /// The find bar, with its words selected: those given, else what is selected on a page, else what was looked for last.
    /// For a citation not found, `words` is the first of `search_words` and the rest are tried after it when it finds nothing.
    pub fn open_finder(&mut self, words: Option<String>, missed: bool, window: &mut Window, cx: &mut Context<Self>) {
        let words = words.or_else(|| self.selected().map(|(_, text)| text).filter(|text| text.chars().count() <= 80 && !text.contains('\n')));
        if self.finder.is_none() {
            let input = cx.new(|cx| TextInput::new("在 PDF 里查找", cx));
            let heard = cx.subscribe(&input, |this, _, event: &InputEvent, cx| match event {
                // The person's own words: no more of the sentence's are tried.
                InputEvent::Changed => {
                    if let Some(finder) = this.finder.as_mut() { finder.tries.clear() }
                    this.finder_search(cx)
                }
                InputEvent::Submit => this.finder_step(true, cx),
                _ => {}
            });
            self.finder = Some(Finder { input, _heard: heard, request: 0, hits: vec![], at: None, searching: false, missed, tries: vec![] });
        }
        let Some(finder) = self.finder.as_mut() else { return };
        finder.missed = missed;
        finder.tries = if missed { search_words(&self.citation.quote).into_iter().skip(1).collect() } else { vec![] };
        let input = finder.input.clone();
        let given = words.is_some();
        input.update(cx, |input, cx| {
            if let Some(words) = &words { input.set_text(words, cx) }
            let end = input.text().len();
            input.select_range(0..end, cx);
        });
        input.read(cx).focus(window);
        if given || finder.hits.is_empty() { self.finder_search(cx) }
        cx.notify();
    }

    fn close_finder(&mut self, window: &mut Window) {
        self.finder = None;
        window.focus(&self.focus);
    }

    /// Ask the worker for the words now in the bar.
    fn finder_search(&mut self, cx: &mut Context<Self>) {
        self.searches += 1;
        let request = self.searches;
        let Some(finder) = self.finder.as_mut() else { return };
        let words = finder.input.read(cx).text().to_string();
        (finder.request, finder.hits, finder.at) = (request, vec![], None);
        finder.searching = !wisp_core::fulltext::bare(&words).is_empty();
        if finder.searching { let _ = self.requests.send(Request::Search(request, words)); }
        cx.notify();
    }

    /// The next place found (true) or the one before; past the end, around to the beginning.
    fn finder_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(finder) = self.finder.as_ref() else { return };
        let count = finder.hits.len();
        if count == 0 { return }
        let next = match finder.at {
            Some(n) if forward => (n + 1) % count,
            Some(n) => (n + count - 1) % count,
            None => 0,
        };
        self.finder_go(next, cx);
    }

    /// Bring a place found below the find bar, and into the view sideways.
    fn finder_go(&mut self, n: usize, cx: &mut Context<Self>) {
        let Some(finder) = self.finder.as_mut() else { return };
        let Some((index, boxes)) = finder.hits.get(n).cloned() else { return };
        finder.at = Some(n);
        let below = if finder.missed { 160. } else { 110. };
        if let Some(first) = boxes.first() {
            let scale = self.scale_of(index);
            // Near the top of its page the place is brought down by showing the foot of the page before.
            let down = first[1] * scale - below;
            let top = match index.checked_sub(1).filter(|_| down < 0.) {
                Some(before) => ListOffset { item_ix: before, offset_in_item: px((self.sizes.get(before).map_or(842., |size| size.1) * self.scale_of(before) + PAGE_GAP + down).max(0.)) },
                None => ListOffset { item_ix: index, offset_in_item: px(down.max(0.)) },
            };
            self.list().scroll_to(top);
            if self.max_x > 0. { self.panes[self.active].scroll_x = (first[0] * scale - 40.).clamp(0., self.max_x) }
        }
        cx.notify();
    }

    /// The find bar at the top of the viewer: the words, how many places and which is gone to, the arrows and ×;
    /// opened for a citation not found, what to do about it.
    fn finder_bar(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let finder = self.finder.as_ref()?;
        let typed = !wisp_core::fulltext::bare(finder.input.read(cx).text()).is_empty();
        let (count, color) = match (typed, finder.searching, finder.hits.len(), finder.at) {
            (false, ..) => (String::new(), Theme::text_tertiary()),
            (true, true, ..) => (t("查找中…").to_string(), Theme::text_tertiary()),
            (true, false, 0, _) => (t("没有找到").to_string(), Theme::danger()),
            (true, false, n, Some(at)) => (format!("{}/{n}", at + 1), Theme::text_secondary()),
            (true, false, n, None) => (tf!("{n} 处", "{n} found"), Theme::text_secondary()),
        };
        let able = !finder.hits.is_empty();
        let arrow = |id: &'static str, icon: &str, tip: &'static str, forward: bool, cx: &mut Context<Self>| ui::icon_action(id, icon, Theme::text_secondary(), t(tip)).when(!able, |d| d.opacity(0.4).cursor_default()).when(able, |d| d.on_click(cx.listener(move |this, _, window, cx| { this.finder_step(forward, cx); if let Some(finder) = this.finder.as_ref() { finder.input.read(cx).focus(window) } })));
        // At the top left: the right corner has 标注's button and its panel.
        Some(
            div()
                .absolute()
                .top(px(8.))
                .left(px(12.))
                .w(px(340f32.min(self.width - FIND_CLEAR).max(200.)))
                .flex()
                .flex_col()
                .gap(px(6.))
                .p(px(6.))
                .rounded(px(8.))
                .bg(Theme::bg_body())
                .border_1()
                .border_color(Theme::line())
                .shadow(crate::theme::shadow_card())
                .occlude()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .child(div().flex_1().min_w_0().child(ui::field(&finder.input, window, cx).py(px(4.))))
                        .child(div().flex_none().min_w(px(32.)).px(px(2.)).text_size(text_px(12.)).text_color(color).child(count))
                        .child(arrow("pdf-find-previous", "chevron-up", "上一处", false, cx))
                        .child(arrow("pdf-find-next", "chevron-down", "下一处（Enter）", true, cx))
                        .child(ui::icon_action("pdf-find-close", "x", Theme::text_tertiary(), t("关闭（Esc）")).on_click(cx.listener(|this, _, window, cx| { this.close_finder(window); cx.notify() }))),
                )
                .when(finder.missed, |d| d.child(div().px(px(4.)).text_size(text_px(12.)).line_height(text_px(18.)).text_color(Theme::text_secondary()).child(t("引用的这句话没能在 PDF 里逐字对上：可能是改写、翻译，或排版把句子拆开了。已用句中的词查找；也可以换成别的词、数字或术语。")))),
        )
    }

    fn list(&self) -> &ListState {
        &self.panes[self.active].list
    }

    pub fn split(&self) -> Split {
        self.split
    }

    /// The size of one view of the pages: the viewer's, or a half of it.
    fn pane_size(&self) -> (f32, f32) {
        pane_size(self.split, (self.width, self.height))
    }

    /// Show the file once, or twice side by side or one over the other. The
    /// second half starts where the first is; put away, the half last worked
    /// in is the one that stays.
    pub fn set_split(&mut self, split: Split, cx: &mut Context<Self>) {
        // The translation keeps its half: side by side or one over the other, as chosen.
        if self.facing.is_some() && split == Split::Off { return }
        if split == self.split { return }
        let was = self.split;
        if split == Split::Off && self.active == 1 { self.panes.swap(0, 1) }
        if split == Split::Off { (self.active, self.held) = (0, 0) }
        self.resize((0., 0.), |this| this.split = split);
        if was == Split::Off {
            let top = self.panes[0].list.logical_scroll_top();
            self.panes[1].list.reset(self.sizes.len());
            self.panes[1].list.scroll_to(top);
            self.panes[1].scroll_x = self.panes[0].scroll_x;
        }
        cx.notify();
    }

    pub fn zoom(&self) -> Zoom {
        self.zoom
    }

    /// Show the pages' translation in the second half (`Some`: the paragraphs of each page, 0-based), or
    /// put it away. The viewer is split for it, side by side where it is wide enough for two pages, and
    /// goes back to how it was split before.
    pub fn set_facing(&mut self, pieces: Option<HashMap<usize, Vec<FacingPiece>>>, cx: &mut Context<Self>) {
        match (self.facing.is_some(), pieces.is_some()) {
            (false, true) => {
                self.facing_was = self.split;
                if self.split == Split::Off { self.set_split(if self.width >= crate::facing::FACING_MIN { Split::Side } else { Split::Stacked }, cx) }
                self.facing = pieces;
            }
            (true, false) => {
                self.facing = None;
                // The original is the half that stays.
                self.active = 0;
                self.set_split(self.facing_was, cx);
            }
            _ => self.facing = pieces,
        }
        cx.notify();
    }

    pub fn facing(&self) -> bool {
        self.facing.is_some()
    }

    /// How large the page in view is drawn, in percent of its own size.
    pub fn percent(&self) -> f32 {
        self.scale_of(self.list().logical_scroll_top().item_ix) / POINT_PX * 100.
    }

    /// Go to a 1-based page and look for a sentence quoted from it; when it
    /// is found, there or on another page, it is highlighted and scrolled
    /// into view. A citation without a quotation marks the page itself.
    pub fn find(&mut self, page: usize, quote: &str, cx: &mut Context<Self>) {
        self.go_to(page, cx);
        let request = self.citation.begin(page, quote);
        if self.citation.status == CitationStatus::Searching {
            let _ = self.requests.send(Request::Find(request, page.checked_sub(1), quote.to_string()));
        }
    }

    /// How many logical pixels a point of this page takes.
    fn scale_of(&self, index: usize) -> f32 {
        page_scale(self.zoom, self.sizes.get(index).copied().unwrap_or((595., 842.)), self.pane_size())
    }

    /// Where a page's left edge stands in its view: in the middle while
    /// the page fits, else moved by that view's sideways scroll.
    fn sheet_left(&self, pane: usize, sheet_width: f32) -> f32 {
        let width = self.pane_size().0;
        if sheet_width + 2. * MARGIN <= width { (width - sheet_width) / 2. } else { MARGIN - self.panes[pane].scroll_x }
    }

    /// Change what decides the size of the pages (the zoom, the viewer's
    /// size, the split), and keep in view what was: the point `anchor`
    /// pixels from the top left corner of the half last worked in stays
    /// under it, and the other half keeps its top.
    fn resize(&mut self, anchor: (f32, f32), change: impl FnOnce(&mut Self)) {
        let before = [0, 1].map(|pane| {
            let top = self.panes[pane].list.logical_scroll_top();
            let scale = self.scale_of(top.item_ix);
            (top, scale, self.sheet_left(pane, self.sizes.get(top.item_ix).map_or(595., |size| size.0) * scale))
        });
        change(self);
        let widest = self.sizes.iter().enumerate().map(|(index, (width, _))| width * self.scale_of(index)).fold(0., f32::max);
        self.max_x = (widest + 2. * MARGIN - self.pane_size().0).max(0.);
        for (pane, (top, before, left)) in before.into_iter().enumerate() {
            let anchor = if pane == self.active { anchor } else { (anchor.0, 0.) };
            let page_height = self.sizes.get(top.item_ix).map_or(842., |size| size.1);
            let after = self.scale_of(top.item_ix);
            let ratio = after / before.max(0.001);
            // Sideways: the point of the page under the anchor comes back under it.
            self.panes[pane].scroll_x = (MARGIN - (anchor.0 - (anchor.0 - left) * ratio)).clamp(0., self.max_x);
            if (ratio - 1.).abs() < 0.0005 { continue }
            // Page heights follow the scale: measure the list again.
            let offset = ((f32::from(top.offset_in_item) + anchor.1) * ratio - anchor.1).clamp(0., (page_height * after - 1.).max(0.));
            self.panes[pane].list.reset(self.sizes.len());
            self.panes[pane].list.scroll_to(ListOffset { item_ix: top.item_ix, offset_in_item: px(offset) });
        }
    }

    /// Draw the pages at another size. `anchor` is the point of the half
    /// last worked in that stays where it is (the pointer's place); without one, the top.
    pub fn set_zoom(&mut self, zoom: Zoom, anchor: Option<(f32, f32)>, cx: &mut Context<Self>) {
        let zoom = match zoom {
            Zoom::Percent(percent) => Zoom::Percent(percent.clamp(ZOOM_MIN, ZOOM_MAX)),
            fit => fit,
        };
        if zoom == self.zoom && zoom != Zoom::FitPage { return }
        if zoom != self.zoom { self.resize(anchor.unwrap_or((self.pane_size().0 / 2., 0.)), |this| this.zoom = zoom) }
        // The whole page: the one each half is on comes to its top, so all four edges are in view; asked again, it comes back there.
        if zoom == Zoom::FitPage {
            for pane in &mut self.panes {
                let page = pane.list.logical_scroll_top().item_ix;
                pane.list.scroll_to(ListOffset { item_ix: page, offset_in_item: px(0.) });
            }
        }
        cx.notify();
    }

    pub fn zoom_to(&mut self, to: ZoomTo, cx: &mut Context<Self>) {
        let zoom = match to {
            ZoomTo::In => Zoom::Percent(zoom_step(self.percent(), true)),
            ZoomTo::Out => Zoom::Percent(zoom_step(self.percent(), false)),
            ZoomTo::Set(zoom) => zoom,
        };
        self.set_zoom(zoom, None, cx);
    }

    /// The wheel turned over the viewer, before the list of pages hears of
    /// it. With ⌘ or Ctrl it changes the zoom about the pointer; sideways
    /// (or with Shift) it moves pages wider than the viewer. Anything else
    /// is left to the list, which scrolls down the pages.
    fn wheel(&mut self, event: &ScrollWheelEvent, bounds: Bounds<Pixels>, cx: &mut Context<Self>) -> bool {
        let delta = event.delta.pixel_delta(px(20.));
        let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
        // The half under the pointer is the one worked in from here on.
        let at = (f32::from(event.position.x - bounds.origin.x), f32::from(event.position.y - bounds.origin.y));
        let view = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        let pane = pane_at(self.split, view, at);
        if pane != self.active {
            self.active = pane;
            cx.notify();
        }
        if event.modifiers.secondary() || event.modifiers.control {
            if dy != 0. {
                let half = pane_size(self.split, view);
                let anchor = match (self.split, pane) {
                    (Split::Side, 1) => (at.0 - half.0 - DIVIDER, at.1),
                    (Split::Stacked, 1) => (at.0, at.1 - half.1 - DIVIDER),
                    _ => at,
                };
                self.set_zoom(Zoom::Percent(self.percent() * (dy / 240.).exp()), Some(anchor), cx);
            }
            return true;
        }
        // A wheel that only turns one way moves sideways with Shift held.
        let sideways = if event.modifiers.shift && dx == 0. { dy } else { dx };
        if self.max_x <= 0. || sideways == 0. || (sideways.abs() <= dy.abs() && !event.modifiers.shift) { return false }
        self.panes[pane].scroll_x = (self.panes[pane].scroll_x - sideways).clamp(0., self.max_x);
        cx.notify();
        true
    }

    /// Scroll to a 1-based page.
    /// The pointer went down on a view's sideways bar: the thumb is held where it was pressed,
    /// and a press beside it brings its middle under the pointer first.
    fn bar_press(&mut self, pane: usize, x: Pixels, cx: &mut Context<Self>) {
        let (thumb, left) = bar_thumb(self.pane_size().0, self.max_x, self.panes[pane].scroll_x);
        let at = f32::from(x - self.panes[pane].list.viewport_bounds().left()) - BAR_INSET;
        self.sliding = Some((pane, if at >= left && at <= left + thumb { at - left } else { thumb / 2. }));
        self.slide(x, cx);
    }

    /// The thumb of the bar held follows the pointer, at `x` in the window, and the pages go the other way.
    fn slide(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some((pane, grab)) = self.sliding else { return };
        let width = self.pane_size().0;
        let room = (width - 2. * BAR_INSET - bar_thumb(width, self.max_x, 0.).0).max(1.);
        let left = f32::from(x - self.panes[pane].list.viewport_bounds().left()) - BAR_INSET - grab;
        self.panes[pane].scroll_x = (left / room * self.max_x).clamp(0., self.max_x);
        cx.notify();
    }

    /// The bar at the foot of a view whose pages are wider than it: it shows that there is more
    /// to either side and how much, and its thumb is dragged to get there.
    fn sideways_bar(&self, pane: usize, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        if self.max_x <= 0. { return None }
        let (thumb, left) = bar_thumb(self.pane_size().0, self.max_x, self.panes[pane].scroll_x);
        let held = self.sliding.is_some_and(|(held, _)| held == pane);
        Some(
            div().id(("pdf-sideways", pane)).group("pdf-sideways").absolute().left(px(BAR_INSET)).right(px(BAR_INSET)).bottom(px(2.)).h(px(BAR_HEIGHT)).cursor_default()
                // The pages under the bar hear nothing of the press: no selection starts there.
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.bar_press(pane, event.position.x, cx);
                }))
                .child(div().absolute().left(px(left)).top(px(3.)).w(px(thumb)).h(px(BAR_HEIGHT - 6.)).rounded(px((BAR_HEIGHT - 6.) / 2.)).bg(Theme::text_tertiary().opacity(if held { 1. } else { 0.55 })).group_hover("pdf-sideways", |s| s.bg(Theme::text_tertiary()))),
        )
    }

    pub fn go_to(&mut self, page: usize, cx: &mut Context<Self>) {
        self.citation.clear();
        self.scroll_to_page(page, cx);
    }

    fn scroll_to_page(&mut self, page: usize, cx: &mut Context<Self>) {
        if self.sizes.is_empty() {
            self.goto = Some(page);
        } else {
            self.list().scroll_to(ListOffset { item_ix: page.clamp(1, self.sizes.len()) - 1, offset_in_item: px(0.) });
        }
        cx.notify();
    }

    /// Where the view is read at: the 0-based page at its top, and how far down that page in its points.
    /// Kept by the owner when another file of the paper is shown, and handed back by `resume`.
    pub fn place(&self) -> (usize, f32) {
        let top = self.list().logical_scroll_top();
        (top.item_ix, f32::from(top.offset_in_item) / self.scale_of(top.item_ix).max(0.01))
    }

    /// Come back to a place `place` gave, now or once the file has opened.
    pub fn resume(&mut self, (index, down): (usize, f32), cx: &mut Context<Self>) {
        if self.sizes.is_empty() { return self.resume = Some((index, down)) }
        let index = index.min(self.sizes.len() - 1);
        self.list().scroll_to(ListOffset { item_ix: index, offset_in_item: px(down * self.scale_of(index)) });
        cx.notify();
    }

    /// The file on show.
    pub fn resource_id(&self) -> &str {
        &self.resource_id
    }

    /// Go to a place on a 1-based page: what stands `top` points below the page's top comes a little under the top of the view.
    pub fn go_to_place(&mut self, page: usize, top: f32, cx: &mut Context<Self>) {
        self.citation.clear();
        if self.sizes.is_empty() { return self.go_to(page, cx) }
        let index = page.clamp(1, self.sizes.len()) - 1;
        self.list().scroll_to(ListOffset { item_ix: index, offset_in_item: px((top * self.scale_of(index) - 48.).max(0.)) });
        cx.notify();
    }

    fn receive(&mut self, reply: Reply, cx: &mut Context<Self>) {
        match reply {
            Reply::Opened(sizes) => {
                self.sizes = sizes;
                for pane in &self.panes { pane.list.reset(self.sizes.len()) }
                self.resize((0., 0.), |_| {});
                if let Some(page) = self.goto.take() { self.scroll_to_page(page, cx) }
                if let Some(place) = self.resume.take() { self.resume(place, cx) }
            }
            Reply::Failed(error) => self.error = Some(error),
            Reply::Skipped(index) => {
                self.pending.remove(&index);
            }
            Reply::Found(request, index, boxes, whole) => {
                if !self.citation.finish(request, Some((index, boxes, whole))) { return }
                // Bring the first line of the quotation a little below the top of the view, and into it sideways.
                if let Some(first) = self.citation.boxes.first() {
                    let scale = self.scale_of(index);
                    self.list().scroll_to(ListOffset { item_ix: index, offset_in_item: px((first[1] * scale - 80.).max(0.)) });
                    if self.max_x > 0. { self.panes[self.active].scroll_x = (first[0] * scale - 40.).clamp(0., self.max_x) }
                }
            }
            Reply::NotFound(request) => { self.citation.finish(request, None); }
            Reply::Hits(request, hits) => {
                let Some(finder) = self.finder.as_mut().filter(|finder| finder.request == request) else { return };
                // A word of a sentence not found that is on no page either: the next one.
                if hits.is_empty() && !finder.tries.is_empty() {
                    let next = finder.tries.remove(0);
                    let input = finder.input.clone();
                    input.update(cx, |input, cx| {
                        input.set_text(&next, cx);
                        input.select_range(0..next.len(), cx);
                    });
                    return self.finder_search(cx);
                }
                (finder.hits, finder.searching, finder.at) = (hits, false, None);
                // The first place from the page in view on.
                let top = self.list().logical_scroll_top().item_ix;
                let first = self.finder.as_ref().and_then(|finder| finder.hits.iter().position(|(page, _)| *page >= top).or((!finder.hits.is_empty()).then_some(0)));
                if let Some(n) = first { self.finder_go(n, cx) }
            }
            Reply::Marks(index, marks) => {
                self.marks_pending.remove(&index);
                // Of a long paper, the glyphs of the pages read last are enough.
                if self.marks.len() > 40 {
                    let keep = self.selection.map(|selection| selection.page);
                    self.marks.retain(|page, _| Some(*page) == keep || page.abs_diff(index) <= 4);
                }
                self.marks.insert(index, Arc::new(marks));
            }
            Reply::Page(index, scale, width, height, bytes) => {
                self.pending.remove(&index);
                match image::RgbaImage::from_raw(width, height, bytes).filter(|_| width > 0) {
                    Some(buffer) => {
                        let size = width as usize * height as usize * 4;
                        // A page rendered again at another size takes the place of the one it had.
                        if let Some((old, ..)) = self.images.insert(index, (Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])), scale, size)) { cx.drop_image(old, None) }
                        self.touch(index);
                        // Two halves show twice the pages.
                        let keep = if self.split == Split::Off { KEEP_PAGES } else { KEEP_PAGES + 6 };
                        while self.recent.len() > keep || (self.recent.len() > 2 && self.images.values().map(|(.., size)| size).sum::<usize>() > KEEP_BYTES) {
                            if let Some(old) = self.recent.pop_front() && let Some((image, ..)) = self.images.remove(&old) {
                                cx.drop_image(image, None);
                            }
                        }
                    }
                    None => {
                        self.broken.insert(index);
                    }
                }
            }
        }
        cx.notify();
    }

    fn touch(&mut self, index: usize) {
        self.recent.retain(|i| *i != index);
        self.recent.push_back(index);
    }

    /// A page as one of the views draws it. The two share its picture and what is marked on it.
    fn page(&mut self, pane: usize, index: usize) -> AnyElement {
        let (w, h) = self.sizes.get(index).copied().unwrap_or((595., 842.));
        let scale = self.scale_of(index);
        let (width, height) = (w * scale, h * scale);
        // The picture this size wants. One made for another size is shown, stretched, until its own arrives.
        let wanted = render_scale((w, h), scale * self.device_scale);
        let image = self.images.get(&index).map(|(image, at, _)| (image.clone(), *at));
        let stale = image.as_ref().is_none_or(|(_, at)| !picture_serves(*at, wanted, scale * self.device_scale));
        if image.is_some() { self.touch(index) }
        if stale && !self.broken.contains(&index) && self.pending.insert(index) {
            // A page not drawn yet comes quickly and coarse first, then at the size it is shown at.
            let _ = self.requests.send(Request::Page(index, if image.is_none() { wanted.min(PREVIEW_SCALE) } else { wanted }));
        }
        if !self.marks.contains_key(&index) && self.marks_pending.insert(index) {
            let _ = self.requests.send(Request::Marks(index));
        }
        // What is selected on this page, under the quotation's marker.
        let selected: Vec<Div> = self
            .selection
            .filter(|selection| selection.page == index)
            .and_then(|selection| Some(pdftext::mark_boxes(self.marks.get(&index)?.get(selection.anchor.min(selection.focus)..=selection.anchor.max(selection.focus))?)))
            .unwrap_or_default()
            .into_iter()
            .map(|line| div().absolute().left(px(line[0] * scale)).top(px(line[1] * scale)).w(px((line[2] - line[0]) * scale)).h(px((line[3] - line[1]) * scale)).bg(Theme::selection()))
            .collect();
        // The piece being cut out of this page.
        let cut = self.cut.filter(|(page, _)| *page == index).map(|(_, [x0, y0, x1, y1])| div().absolute().left(px(x0.min(x1) * scale)).top(px(y0.min(y1) * scale)).w(px((x1 - x0).abs() * scale)).h(px((y1 - y0).abs() * scale)).border_2().border_color(Theme::accent()).bg(Theme::selection()));
        let cutting = self.cutting;
        // Metadata is painted over the current raster at the same scale as
        // the glyphs, independently of both selection and citation marks.
        // A highlight can be clicked for its menu, so the pointer says so over it.
        let annotations: Vec<Div> = self.annotations.iter().filter(|a| a.page == index + 1 && !a.kind.shape()).flat_map(|a| a.boxes.iter().map(move |line| {
            let (top, height, color) = match a.kind {
                Kind::Underline => (line[3] * scale - (1.3 * scale).max(1.), (1.3 * scale).max(1.), Theme::underline_mark()),
                _ => (line[1] * scale, (line[3] - line[1]) * scale, Theme::highlight(&a.color)),
            };
            div().absolute().left(px(line[0] * scale)).top(px(top)).w(px((line[2] - line[0]) * scale)).h(px(height)).bg(color).when(a.kind == Kind::Highlight && !cutting, |d| d.cursor_pointer())
        })).collect();
        // Boxes, hollow so what they frame stays clear; the one chosen with its four corners to size it by.
        let chosen = self.mark_menu.as_ref().map(|menu| menu.id.clone());
        let free = self.tool == Tool::Select && !cutting;
        // A piece marked to be cut out later is a dashed frame in the accent, and says it can be clicked.
        let shapes: Vec<Div> = self.annotations.iter().filter(|a| a.page == index + 1 && matches!(a.kind, Kind::Box | Kind::Cut)).map(|a| {
            let [x0, y0, x1, y1] = self.rect_of(a);
            let marked = a.kind == Kind::Cut;
            let color = if marked { Theme::accent() } else { Theme::highlight_swatch(&a.color) };
            let picked = chosen.as_deref() == Some(a.id.as_str());
            let grips = picked.then(|| [(0., 0.), (1., 0.), (1., 1.), (0., 1.)].map(|(fx, fy)| {
                div().absolute().left(px((x1 - x0) * scale * fx - GRIP / 2. + 1.)).top(px((y1 - y0) * scale * fy - GRIP / 2. + 1.)).size(px(GRIP)).rounded(px(2.)).bg(gpui::white()).border_1().border_color(color)
            }));
            div().absolute().left(px(x0 * scale - 1.)).top(px(y0 * scale - 1.)).w(px((x1 - x0) * scale + 2.)).h(px((y1 - y0) * scale + 2.)).border_2().border_color(color).rounded(px(2.))
                .when(marked, |d| d.border_dashed().when(self.tool == Tool::Select, |d| d.cursor_pointer()).when(picked, |d| d.bg(Theme::selection())))
                .children(grips.into_iter().flatten())
        }).collect();
        // A box being drawn.
        let drawing = self.drawing.filter(|(page, _)| *page == index).map(|(_, [x0, y0, x1, y1])| div().absolute().left(px(x0.min(x1) * scale)).top(px(y0.min(y1) * scale)).w(px((x1 - x0).abs() * scale)).h(px((y1 - y0).abs() * scale)).border_2().border_color(Theme::highlight_swatch(wisp_core::annotation::BOX_COLOR)).rounded(px(2.)));
        // Sticky notes, folded to an icon at their spot; the open one ringed. A note being written shows where it will go.
        let open = self.note_open.clone();
        let mut notes: Vec<Div> = self.annotations.iter().filter(|a| a.page == index + 1 && a.kind == Kind::Note).map(|a| {
            let [x, y, ..] = self.rect_of(a);
            let lit = open.as_deref() == Some(a.id.as_str());
            note_icon(x * scale, y * scale, lit).when(free, |d| d.cursor_pointer())
        }).collect();
        if let Some((page, x, y)) = self.note_draft && page == index { notes.push(note_icon(x * scale, y * scale, true)) }
        // The quotation on this page: a translucent marker over each of its lines.
        let cited = self.citation.index(self.sizes.len()) == Some(index);
        let marks: Vec<Div> = self
            .citation.boxes
            .iter()
            .filter(|_| cited)
            .map(|line| div().absolute().left(px(line[0] * scale - 1.)).top(px(line[1] * scale)).w(px((line[2] - line[0]) * scale + 2.)).h(px((line[3] - line[1]) * scale)).rounded(px(2.)).bg(hsla(50. / 360., 1., 0.5, 0.35)))
            .collect();
        // What the find bar found on this page: the place gone to stronger.
        let found: Vec<Div> = self
            .finder
            .iter()
            .flat_map(|finder| finder.hits.iter().enumerate().filter(|(_, (page, _))| *page == index).flat_map(move |(n, (_, lines))| lines.iter().map(move |line| (finder.at == Some(n), *line))))
            .map(|(current, line)| div().absolute().left(px(line[0] * scale - 1.)).top(px(line[1] * scale)).w(px((line[2] - line[0]) * scale + 2.)).h(px((line[3] - line[1]) * scale)).rounded(px(2.)).bg(if current { Theme::found_current() } else { Theme::found() }))
            .collect();
        let sheet = div().relative().flex_none().ml(px(self.sheet_left(pane, width))).w(px(width)).h(px(height)).bg(gpui::white()).border_1().border_color(Theme::line()).overflow_hidden();
        // The facing copy of the page: each paragraph translated stands in the paragraph's own box, on the
        // page's white, set no larger than the paragraph and as small as it takes to stand in it
        // (`translation::fit`). One being asked for is tinted; the rest of the page is as it was.
        if pane == 1 && let Some(facing) = &self.facing {
            let picture = image.as_ref().map(|(image, _)| image.clone());
            let pieces = facing.get(&index).map(Vec::as_slice).unwrap_or_default().iter().filter_map(|piece| {
                let [x0, y0, x1, y1] = piece.rect;
                // The colour the paragraph stands on (a tinted box, a table's band), read off the page beside it.
                let (paper, ink) = picture.as_ref().and_then(|image| ground(image, (w, h), piece.rect)).unwrap_or((gpui::white(), gpui::black()));
                let place = div().absolute().left(px((x0 - 1.) * scale)).top(px((y0 - 0.5) * scale)).w(px((x1 - x0 + 2.) * scale));
                match &piece.text {
                    Some(text) => {
                        let size = wisp_core::translation::fit(text, x1 - x0, y1 - y0, piece.size);
                        Some(place.min_h(px((y1 - y0 + 1.) * scale)).max_h(px((y1 - y0).max(size * wisp_core::translation::LEADING) * scale + size * scale)).overflow_hidden().bg(paper).text_color(ink).text_size(px(size * scale)).line_height(px(size * wisp_core::translation::LEADING * scale)).child(text.clone()))
                    }
                    None if piece.busy => Some(place.h(px((y1 - y0 + 1.) * scale)).rounded(px(2.)).bg(Theme::accent().opacity(0.10))),
                    None => None,
                }
            });
            let sheet = match image {
                Some((image, _)) => sheet.child(img(image).w(px(width)).h(px(height))).children(pieces),
                None => sheet.flex().items_center().justify_center().text_size(text_px(12.)).text_color(Theme::text_placeholder()).child(tf!("第 {} 页", "Page {}", index + 1)),
            };
            return div().w_full().flex().pb(px(PAGE_GAP)).child(sheet).into_any_element();
        }
        let sheet = match image {
            Some((image, _)) => sheet.when(cutting || !free, |d| d.cursor_crosshair()).when(free, |d| d.cursor_text()).child(img(image).w(px(width)).h(px(height))).children(annotations).children(selected).children(marks).children(found).children(cut).children(shapes).children(drawing).children(notes),
            None => sheet.flex().items_center().justify_center().text_size(text_px(12.)).text_color(Theme::text_placeholder()).child(if self.broken.contains(&index) { tf!("第 {} 页无法渲染", "Cannot render page {}", index + 1) } else { tf!("第 {} 页", "Page {}", index + 1) }),
        };
        // Draw over the image without changing its size or covering the text.
        // This also makes page-only citations visible while a page is loading.
        let sheet = sheet.when(cited, |d| d.child(div().absolute().inset_0().border_2().border_color(Theme::accent())));
        div().w_full().flex().pb(px(PAGE_GAP)).child(sheet).into_any_element()
    }

    /// Where a page stands in the window, in one of the views. The list leaves out its own top padding from what it says, though it draws every item below it.
    fn item_bounds(&self, pane: usize, index: usize) -> Option<Bounds<Pixels>> {
        self.panes[pane].list.bounds_for_item(index).map(|bounds| Bounds { origin: point(bounds.origin.x, bounds.origin.y + px(PAGE_GAP)), size: bounds.size })
    }

    /// The view a point of the window is in.
    fn pane_under(&self, position: Point<Pixels>) -> Option<usize> {
        let shown = if self.split == Split::Off { 1 } else { 2 };
        (0..shown).find(|pane| self.panes[*pane].list.viewport_bounds().contains(&position))
    }

    /// Where a page's sheet stands in the viewer, in the half its selection, cut or highlight was made in: its top left corner.
    fn sheet_place(&self, page: usize) -> Option<(f32, f32)> {
        let (item, view) = (self.item_bounds(self.held, page)?, self.panes[0].list.viewport_bounds());
        let left = self.sheet_left(self.held, self.sizes.get(page)?.0 * self.scale_of(page));
        Some((f32::from(item.left() - view.left()) + left, f32::from(item.top() - view.top())))
    }

    /// The page under a point of the window in one of the views, and the point in that page's own points.
    fn locate(&self, pane: usize, position: Point<Pixels>) -> Option<(usize, f32, f32)> {
        let top = self.panes[pane].list.logical_scroll_top().item_ix;
        (top..self.sizes.len().min(top + 8)).find_map(|index| {
            let bounds = self.item_bounds(pane, index)?;
            if position.y < bounds.top() || position.y >= bounds.bottom() { return None }
            let scale = self.scale_of(index);
            let left = f32::from(bounds.left()) + self.sheet_left(pane, self.sizes[index].0 * scale);
            Some((index, (f32::from(position.x) - left) / scale, f32::from(position.y - bounds.top()) / scale))
        })
    }

    /// The pointer went down on a page: a selection starts at the glyph under it; with a tool, a box or a note
    /// starts there; on a box or a note, that is taken hold of.
    fn press(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        let chosen = self.mark_menu.take();
        (self.selection, self.selecting, self.translation, self.pressed_mark, self.shape_drag) = (None, false, None, None, None);
        // The half pressed in is the one worked in; a drag stays in it. The line between the two is neither.
        let Some(pane) = self.pane_under(event.position) else {
            if self.cutting { self.cut = None }
            return cx.notify();
        };
        (self.active, self.held) = (pane, pane);
        if self.cutting {
            // A piece of the page is cut out from here.
            self.cut = self.locate(pane, event.position).map(|(page, x, y)| (page, [x, y, x, y]));
            self.selecting = self.cut.is_some();
            // A click on a piece marked before is for that mark, not a new piece.
            self.pressed_mark = self.cut.and_then(|(page, [x, y, ..])| Some((self.cut_mark_at(page, x, y)?, event.position)));
            return cx.notify();
        }
        let Some((page, x, y)) = self.locate(pane, event.position) else {
            self.close_note();
            return cx.notify();
        };
        match self.tool {
            Tool::Box => {
                self.drawing = Some((page, [x, y, x, y]));
                self.selecting = true;
                return cx.notify();
            }
            Tool::Note => {
                // The icon's top left at the pointer, the card open to write in. One note a click of the tool:
                // a click on the page afterwards is for reading again, not for another note.
                (self.note_open, self.note_draft, self.note_editing, self.tool) = (None, Some((page, x, y)), true, Tool::Select);
                self.note_input.update(cx, |input, cx| input.set_text("", cx));
                self.note_input.read(cx).focus(window);
                return cx.notify();
            }
            Tool::Select => {}
        }
        // A box or a note is taken hold of: dragged it moves (or, by a corner, changes size), clicked it opens.
        self.mark_menu = chosen;
        if let Some((id, corner)) = self.shape_at(page, x, y) && let Some(a) = self.annotations.iter().find(|a| a.id == id) {
            let rect = self.rect_of(a);
            if self.mark_menu.as_ref().is_none_or(|menu| menu.id != id) { self.mark_menu = None }
            self.shape_drag = Some(ShapeDrag { id, page, rect, corner, from: (x, y), moved: false });
            self.selecting = true;
            return cx.notify();
        }
        self.mark_menu = None;
        if self.note_draft.is_none() && !self.note_editing { self.note_open = None }
        self.pressed_mark = self.highlight_at(page, x, y).map(|(id, line)| MarkMenu { id, page, x, line }).or_else(|| self.cut_mark_at(page, x, y)).map(|menu| (menu, event.position));
        if let Some((marks, at)) = self.marks.get(&page).and_then(|marks| Some((marks, pdftext::mark_at(marks, x, y)?))) {
            self.selection = Some(Selection { page, anchor: at, focus: at });
            self.selecting = true;
            // A double click takes the word, done at once: its bar shows, and no highlight's menu opens.
            if event.click_count == 2 {
                let (anchor, focus) = pdftext::word_marks(marks, at);
                // A space alone is nothing to select.
                let word = (!pdftext::marks_text(&marks[anchor..=focus]).is_empty()).then_some(Selection { page, anchor, focus });
                (self.selection, self.selecting, self.pressed_mark) = (word, false, None);
            }
        }
        cx.notify();
    }

    /// The highlight under a point of a 0-based page, and the top and bottom of its line there. Where
    /// highlights overlap it is the one drawn on top, the one made last.
    fn highlight_at(&self, page: usize, x: f32, y: f32) -> Option<(String, (f32, f32))> {
        self.annotations.iter().rev().filter(|a| a.kind == Kind::Highlight && a.page == page + 1).find_map(|a| {
            let line = a.boxes.iter().find(|b| x >= b[0] - 1. && x <= b[2] + 1. && y >= b[1] - 1. && y <= b[3] + 1.)?;
            Some((a.id.clone(), (line[1], line[3])))
        })
    }

    /// The piece marked to be cut out under a point of a 0-based page, as the menu a click on it opens: the
    /// one made last where they overlap. Its edge is taken hold of by `shape_at`; this is the whole of it.
    fn cut_mark_at(&self, page: usize, x: f32, y: f32) -> Option<MarkMenu> {
        self.annotations.iter().rev().filter(|a| a.kind == Kind::Cut && a.page == page + 1).find_map(|a| {
            let [x0, y0, x1, y1] = self.rect_of(a);
            (x >= x0 && x <= x1 && y >= y0 && y <= y1).then(|| MarkMenu { id: a.id.clone(), page, x: x0, line: (y0, y1) })
        })
    }

    /// The pointer moved with the button held: the selection reaches to the glyph under it, on the page it began on.
    fn drag(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.selecting { return }
        if event.pressed_button != Some(MouseButton::Left) { return self.release(None, cx) }
        let Some((page, x, y)) = self.locate(self.held, event.position) else { return };
        // A box or note held follows the pointer on its own page, kept inside the page.
        let (scale, (w, h)) = (self.scale_of(page), self.sizes.get(page).copied().unwrap_or((595., 842.)));
        if let Some(held) = self.shape_drag.as_mut() {
            if page != held.page { return }
            let (dx, dy) = (x - held.from.0, y - held.from.1);
            if !held.moved && dx.abs().max(dy.abs()) * scale < 3. { return }
            held.moved = true;
            let [x0, y0, x1, y1] = held.rect;
            let rect = match held.corner {
                // The corner held goes with the pointer, the one across from it stays.
                Some(corner) => {
                    let (x, y) = (x.clamp(0., w), y.clamp(0., h));
                    match corner { 0 => [x, y, x1, y1], 1 => [x0, y, x, y1], 2 => [x0, y0, x, y], _ => [x, y0, x1, y] }
                }
                None => {
                    let dx = dx.clamp(-x0, (w - x1).max(-x0));
                    let dy = dy.clamp(-y0, (h - y1).max(-y0));
                    [x0 + dx, y0 + dy, x1 + dx, y1 + dy]
                }
            };
            self.shape_shown = Some((held.id.clone(), [rect[0].min(rect[2]), rect[1].min(rect[3]), rect[0].max(rect[2]), rect[1].max(rect[3])]));
            return cx.notify();
        }
        if let Some((draw_page, corners)) = self.drawing.as_mut() {
            if *draw_page == page {
                (corners[2], corners[3]) = (x, y);
                cx.notify();
            }
            return;
        }
        if let Some((cut_page, corners)) = self.cut.as_mut() {
            if *cut_page == page {
                (corners[2], corners[3]) = (x, y);
                cx.notify();
            }
            return;
        }
        let Some(at) = self.marks.get(&page).and_then(|marks| pdftext::mark_at(marks, x, y)) else { return };
        if let Some(selection) = self.selection.as_mut().filter(|selection| selection.page == page && selection.focus != at) {
            selection.focus = at;
            cx.notify();
        }
    }

    /// The button came up, at `at` in the window when it is known: a pointer that never left its glyph
    /// selected nothing, and one that went down on a highlight and came up close by clicked it.
    fn release(&mut self, at: Option<Point<Pixels>>, cx: &mut Context<Self>) {
        let pressed = self.pressed_mark.take();
        if !std::mem::take(&mut self.selecting) && pressed.is_none() { return }
        // A box or note let go of: moved, it is kept where it is now (shown there until the owner has saved it);
        // clicked, a note opens and a box shows its menu.
        if let Some(held) = self.shape_drag.take() {
            match (held.moved, self.shape_shown.clone()) {
                (true, Some((id, rect))) if id == held.id => cx.emit(PdfEvent::MoveShape { resource: self.resource_id.clone(), id, rect }),
                (true, _) => {}
                (false, _) => match self.annotations.iter().find(|a| a.id == held.id).map(|a| a.kind) {
                    Some(Kind::Note) => {
                        if self.note_open.as_deref() == Some(held.id.as_str()) { self.close_note() } else { (self.note_open, self.note_draft, self.note_editing) = (Some(held.id.clone()), None, false) }
                    }
                    Some(_) => {
                        let [x0, y0, _, y1] = held.rect;
                        self.mark_menu = Some(MarkMenu { id: held.id, page: held.page, x: x0, line: (y0, y1) });
                    }
                    None => {}
                },
            }
            return cx.notify();
        }
        // A box drawn: one too small to frame anything was a slip of the hand. The tool stays on for the next one.
        if let Some((page, [x0, y0, x1, y1])) = self.drawing.take() && (x1 - x0).abs() >= 8. && (y1 - y0).abs() >= 8. {
            cx.emit(PdfEvent::AddShape { paper: self.paper_id.clone(), resource: self.resource_id.clone(), page: page + 1, kind: Kind::Box, rect: [x0, y0, x1, y1], note: String::new() });
            return cx.notify();
        }
        if self.selection.is_some_and(|selection| selection.anchor == selection.focus) { self.selection = None }
        // A piece too small to show anything was a slip of the hand.
        if self.cut.is_some_and(|(_, [x0, y0, x1, y1])| (x1 - x0).abs() < 8. || (y1 - y0).abs() < 8.) { self.cut = None }
        if let (Some((menu, from)), Some(at)) = (pressed, at) && self.selection.is_none() && self.cut.is_none() && f32::from((at.x - from.x).abs()).max(f32::from((at.y - from.y).abs())) <= 4. {
            self.mark_menu = Some(menu);
        }
        cx.notify();
    }

    /// The highlight of the menu takes another colour.
    fn recolor(&mut self, color: &'static str, cx: &mut Context<Self>) {
        let Some(menu) = self.mark_menu.take() else { return };
        if self.annotations.iter().any(|a| a.id == menu.id && a.color != color) {
            cx.emit(PdfEvent::RecolorAnnotation { resource: self.resource_id.clone(), id: menu.id, color });
        }
        cx.notify();
    }

    /// The highlight of the menu goes, and only that one.
    fn remove_mark(&mut self, cx: &mut Context<Self>) {
        let Some(menu) = self.mark_menu.take() else { return };
        cx.emit(PdfEvent::RemoveAnnotation { resource: self.resource_id.clone(), id: menu.id });
        cx.notify();
    }

    /// Cut pieces out of pages with the pointer, or go back to selecting text.
    pub fn toggle_cutting(&mut self, cx: &mut Context<Self>) {
        self.cutting = !self.cutting;
        (self.tool, self.drawing, self.mark_menu) = (Tool::Select, None, None);
        self.close_note();
        (self.cut, self.selection, self.selecting, self.translation) = (None, None, false, None);
        cx.notify();
    }

    /// The list of marks is open under its button at the top right, where the floating tools would stand.
    pub fn annotations_open(&self) -> bool {
        self.annotations_open
    }

    /// The viewer's width, as `set_width` was last told.
    pub fn width(&self) -> f32 {
        self.width
    }

    /// The viewer's height, as it was last laid out.
    pub fn height(&self) -> f32 {
        self.height
    }

    pub fn cutting(&self) -> bool {
        self.cutting
    }

    /// The piece cut out, as a PNG.
    fn cut_picture(&self) -> Option<Vec<u8>> {
        let (page, rect) = self.cut?;
        self.picture_of(page, rect)
    }

    /// A piece of a 0-based page as a PNG: taken from the picture the page is drawn with.
    fn picture_of(&self, page: usize, [x0, y0, x1, y1]: [f32; 4]) -> Option<Vec<u8>> {
        let (image, scale, _) = self.images.get(&page)?;
        let (width, height) = (image.size(0).width.0 as u32, image.size(0).height.0 as u32);
        let edge = |at: f32, most: u32| ((at * scale).round().max(0.) as u32).min(most);
        let (left, right, top, bottom) = (edge(x0.min(x1), width), edge(x0.max(x1), width), edge(y0.min(y1), height), edge(y0.max(y1), height));
        if right <= left || bottom <= top { return None }
        let pixels = image.as_bytes(0)?;
        let mut piece = image::RgbaImage::new(right - left, bottom - top);
        for (x, y, pixel) in piece.enumerate_pixels_mut() {
            let at = (((top + y) * width + left + x) * 4) as usize;
            // The page is kept as BGRA.
            *pixel = image::Rgba([pixels[at + 2], pixels[at + 1], pixels[at], 255]);
        }
        let mut png = vec![];
        image::DynamicImage::ImageRgba8(piece).to_rgb8().write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).ok()?;
        Some(png)
    }

    /// Hand the piece cut out to a conversation as a picture, and go back to selecting text.
    fn send_cut(&mut self, to_library: bool, cx: &mut Context<Self>) {
        let Some(png) = self.cut_picture() else { return };
        (self.cut, self.cutting) = (None, false);
        cx.emit(PdfEvent::Picture(png, to_library));
        cx.notify();
    }

    /// Hand the piece cut out to the figure library, with the caption the page gives it. The piece stays
    /// marked, so a dialog put away leaves it to be used otherwise; `cut_kept` lets go of it.
    pub fn collect_cut(&mut self, cx: &mut Context<Self>) {
        let (Some(png), Some((page, [x0, y0, x1, y1]))) = (self.cut_picture(), self.cut) else { return };
        let bbox = [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)];
        let caption = self.marks.get(&page).and_then(|marks| wisp_core::figure::caption(marks, bbox));
        cx.emit(PdfEvent::Figure(Piece { png, page: page + 1, bbox, caption }));
    }

    /// Leave the piece cut out marked on its page, a dashed frame to click later: for the figure library or
    /// a question then. Cutting stays on for the next one.
    fn mark_cut(&mut self, cx: &mut Context<Self>) {
        let Some((page, rect)) = self.cut.take() else { return };
        cx.emit(PdfEvent::AddShape { paper: self.paper_id.clone(), resource: self.resource_id.clone(), page: page + 1, kind: Kind::Cut, rect, note: String::new() });
        cx.notify();
    }

    /// The piece of the menu's mark, with its 0-based page and its place there.
    fn marked_piece(&self) -> Option<(Vec<u8>, usize, [f32; 4])> {
        let menu = self.mark_menu.as_ref()?;
        let mark = self.annotations.iter().find(|a| a.id == menu.id && a.kind == Kind::Cut)?;
        let (page, rect) = (mark.page.checked_sub(1)?, self.rect_of(mark));
        Some((self.picture_of(page, rect)?, page, rect))
    }

    /// Hand the piece of the menu's mark to the figure library, as `collect_cut` does a piece just cut. The mark stays.
    fn collect_mark(&mut self, cx: &mut Context<Self>) {
        let Some((png, page, bbox)) = self.marked_piece() else { return };
        self.mark_menu = None;
        let caption = self.marks.get(&page).and_then(|marks| wisp_core::figure::caption(marks, bbox));
        cx.emit(PdfEvent::Figure(Piece { png, page: page + 1, bbox, caption }));
        cx.notify();
    }

    /// Hand the piece of the menu's mark to a conversation as a picture, and go back to selecting text. The mark stays.
    fn send_mark(&mut self, to_library: bool, cx: &mut Context<Self>) {
        let Some((png, ..)) = self.marked_piece() else { return };
        (self.mark_menu, self.cut, self.cutting) = (None, None, false);
        cx.emit(PdfEvent::Picture(png, to_library));
        cx.notify();
    }

    /// The piece cut out is in the figure library: the next one can be cut.
    pub fn cut_kept(&mut self, cx: &mut Context<Self>) {
        self.cut = None;
        cx.notify();
    }

    /// Mark a piece of a 0-based page as cut out, as a drag would.
    pub fn set_cut(&mut self, page: usize, bbox: [f32; 4], cx: &mut Context<Self>) {
        (self.cutting, self.cut, self.selection, self.selecting, self.translation) = (true, Some((page, bbox)), None, false, None);
        self.held = self.active;
        cx.notify();
    }

    /// The selected text as it reads, and the 1-based page it is on.
    pub fn selected(&self) -> Option<(usize, String)> {
        let selection = self.selection?;
        let marks = self.marks.get(&selection.page)?;
        let text = pdftext::marks_text(marks.get(selection.anchor.min(selection.focus)..=selection.anchor.max(selection.focus))?);
        (!text.is_empty()).then_some((selection.page + 1, text))
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        let Some((_, text)) = self.selected() else { return };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        cx.emit(PdfEvent::Copied);
    }

    /// Hand what is selected to a conversation, and let go of it.
    fn quote(&mut self, to_library: bool, cx: &mut Context<Self>) {
        let Some((page, text)) = self.selected() else { return };
        self.selection = None;
        cx.emit(PdfEvent::Quote(page, text, to_library));
        cx.notify();
    }

    /// Select glyphs `from` to `to` of a 0-based page, as a drag would; nothing when that page's glyphs are not at hand.
    pub fn select_marks(&mut self, page: usize, from: usize, to: usize, cx: &mut Context<Self>) -> bool {
        let Some(marks) = self.marks.get(&page) else { return false };
        if marks.is_empty() { return false }
        let last = marks.len() - 1;
        (self.selection, self.selecting, self.translation, self.cut) = (Some(Selection { page, anchor: from.min(last), focus: to.min(last) }), false, None, None);
        self.held = self.active;
        cx.notify();
        true
    }

    /// Translate the selected text, with the model service `profile` for this once or the usual one.
    pub fn translate(&mut self, profile: Option<String>, cx: &mut Context<Self>) {
        let (Some(selection), Some((_, text))) = (self.selection, self.selected()) else { return };
        // The model on show stays until the window says which one was asked.
        let model = self.translation.take().map(|t| t.model).unwrap_or_default();
        self.translation = Some(Translation { selection, text: text.clone(), profile: profile.clone(), state: Translated::Busy, model, menu: false });
        cx.emit(PdfEvent::Translate(text, profile));
        cx.notify();
    }

    /// The text of the translation card on show, and the model asked for it alone, for the window to say how it stands.
    pub fn translation_asked(&self) -> Option<(String, Option<String>)> {
        self.translation.as_ref().map(|t| (t.text.clone(), t.profile.clone()))
    }

    /// How the card's translation stands, the model it is made with (id and name), and the models it can be asked of instead.
    pub fn show_translation(&mut self, state: Translated, model: (String, String), models: Vec<(String, String)>, cx: &mut Context<Self>) {
        let Some(card) = self.translation.as_mut() else { return };
        (card.state, card.model) = (state, model);
        self.models = models;
        cx.notify();
    }

    fn copy_translation(&mut self, cx: &mut Context<Self>) {
        let Some(Translated::Done(text)) = self.translation.as_ref().map(|t| &t.state) else { return };
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        cx.emit(PdfEvent::Copied);
    }

    /// The translation of the selection, on a card under it (over it when the room is there), kept inside the viewer.
    fn translation_card(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.selecting { return None }
        let card = self.translation.as_ref().filter(|t| Some(t.selection) == self.selection)?;
        let selection = card.selection;
        let boxes = pdftext::mark_boxes(self.marks.get(&selection.page)?.get(selection.anchor.min(selection.focus)..=selection.anchor.max(selection.focus))?);
        let (first, last) = (boxes.first()?, boxes.last()?);
        let scale = self.scale_of(selection.page);
        let (x, y) = self.sheet_place(selection.page)?;
        let (left, right) = boxes.iter().fold((f32::MAX, f32::MIN), |(l, r), line| (l.min(line[0]), r.max(line[2])));
        let place = card_place([x + left * scale, y + first[1] * scale, x + right * scale, y + last[3] * scale], CARD_W, CARD_TALL, (self.width, self.height));
        let button = |id: &'static str, label: &'static str| div().id(id).flex().flex_none().items_center().h(text_px(24.)).px(px(8.)).rounded(px(6.)).cursor_pointer().text_color(Theme::accent()).hover(|s| s.bg(Theme::hover())).child(label);
        let body = match &card.state {
            Translated::Busy => div().flex().items_center().gap(px(8.)).text_color(Theme::text_secondary()).child(ui::spinner("pdf-translation-busy", 14., Theme::text_tertiary())).child(t("正在翻译…")),
            Translated::Done(text) => div().text_color(Theme::text()).child(text.clone()),
            Translated::Failed(error) => div().text_color(Theme::danger()).child(error.clone()),
        };
        let chosen = &card.model.0;
        let menu = card.menu.then(|| {
            let rows: Vec<AnyElement> = self
                .models
                .iter()
                .enumerate()
                .map(|(n, (id, name))| {
                    let id = id.clone();
                    ui::menu_row(("pdf-translation-with", n), name.clone(), id == *chosen, None).on_click(cx.listener(move |this, _, _, cx| this.translate(Some(id.clone()), cx))).into_any_element()
                })
                .collect();
            div()
                .flex()
                .flex_col()
                .flex_none()
                .p(px(4.))
                .border_b_1()
                .border_color(Theme::line())
                .text_size(text_px(13.))
                .child(div().px(px(8.)).py(px(4.)).text_size(text_px(11.)).text_color(Theme::text_tertiary()).child(t("只这一次换用，不改设置")))
                .when(rows.is_empty(), |d| d.child(div().px(px(8.)).py(px(6.)).text_color(Theme::text_secondary()).child(t("还没有配置模型：在 设置 › AI 里添加。"))))
                .children(rows)
        });
        let footer = match &card.state {
            Translated::Done(_) => Some(button("pdf-translation-copy", t("复制译文")).on_click(cx.listener(|this, _, _, cx| this.copy_translation(cx)))),
            Translated::Failed(_) => Some(button("pdf-translation-retry", t("重试")).on_click(cx.listener(|this, _, _, cx| {
                let profile = this.translation.as_ref().and_then(|t| t.profile.clone());
                this.translate(profile, cx);
            }))),
            Translated::Busy => None,
        };
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .h(text_px(34.))
            .pl(px(12.))
            .pr(px(4.))
            .border_b_1()
            .border_color(Theme::line())
            .child(div().flex_1().text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(t("译文")))
            .child(
                div()
                    .id("pdf-translation-model")
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap(px(4.))
                    .h(text_px(24.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_size(text_px(12.))
                    .text_color(Theme::text_secondary())
                    .hover(|s| s.bg(Theme::hover()))
                    .when(card.menu, |d| d.bg(Theme::pressed()))
                    .child(div().max_w(px(180.)).truncate().child(card.model.1.clone()))
                    .child(ui::icon("chevron-down", 12., Theme::text_tertiary()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(card) = this.translation.as_mut() { card.menu = !card.menu }
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("pdf-translation-close")
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(24.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|s| s.bg(Theme::hover()))
                    .child(ui::icon("x", 13., Theme::text_tertiary()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.translation = None;
                        cx.notify();
                    })),
            );
        Some(
            div()
                .id("pdf-translation")
                .absolute()
                .left(px(place.left))
                .when_some(place.top, |d, top| d.top(px(top)))
                .when_some(place.bottom, |d, bottom| d.bottom(px(bottom)))
                .w(px(place.width))
                .max_h(px(place.max_height))
                .flex()
                .flex_col()
                .overflow_hidden()
                .rounded(px(10.))
                .bg(Theme::bg_body())
                .border_1()
                .border_color(Theme::line_strong())
                .shadow(crate::theme::shadow_card())
                .font(crate::theme::ui_font())
                .text_size(text_px(13.))
                .line_height(text_px(20.))
                .occlude()
                // A press on the card is not a press on the page.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(header)
                .children(menu)
                .child(div().id("pdf-translation-text").flex_shrink().min_h_0().overflow_y_scroll().px(px(12.)).py(px(10.)).child(body))
                .children(footer.map(|button| div().flex().flex_none().justify_end().px(px(4.)).pb(px(4.)).child(button)))
                .into_any_element(),
        )
    }

    /// What can be done with the selection, beside where it ends.
    fn selection_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.selecting { return None }
        // The selection's translation takes the bar's place while it is on show.
        if self.cut.is_none() && self.translation.as_ref().is_some_and(|t| Some(t.selection) == self.selection) { return None }
        // What the bar is about: the piece cut out of a page, or the text selected; and the box it stands under.
        let (page, last, piece) = match (self.cut, self.selection) {
            (Some((page, [x0, y0, x1, y1])), _) => (page, [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)], true),
            (None, Some(selection)) => {
                let marks = self.marks.get(&selection.page)?;
                (selection.page, pdftext::mark_boxes(marks.get(selection.anchor.min(selection.focus)..=selection.anchor.max(selection.focus))?).pop()?, false)
            }
            (None, None) => return None,
        };
        let scale = self.scale_of(page);
        let (x, y) = self.sheet_place(page)?;
        // Under the last line of the selection, kept inside the viewer.
        let (left, top) = (x + last[0] * scale, y + last[3] * scale + 6.);
        let button = |id: &'static str, label: &'static str| div().id(id).flex().flex_none().items_center().h(text_px(26.)).px(px(9.)).rounded(px(6.)).cursor_pointer().hover(|s| s.bg(Theme::hover())).child(label);
        Some(
            div()
                .absolute()
                .left(px(left.clamp(8., (self.width - 520.).max(8.))))
                .top(px(top.clamp(8., (self.height - 150.).max(8.))))
                .max_w(px((self.width - 16.).max(0.)))
                .max_h(px((self.height - 16.).clamp(0., 140.)))
                .id("pdf-selection-bar")
                .overflow_y_scroll()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(2.))
                .p(px(3.))
                .rounded(px(8.))
                .bg(Theme::bg_body())
                .border_1()
                .border_color(Theme::line_strong())
                .shadow(crate::theme::shadow_card())
                .text_size(text_px(12.))
                .occlude()
                // A press on the bar is not a press on the page.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .when(!piece, |d| d.child(button("pdf-copy", t("复制")).on_click(cx.listener(|this, _, _, cx| this.copy(cx)))))
                .when(!piece, |d| d.child(button("pdf-underline", t("下划线")).on_click(cx.listener(|this, _, _, cx| this.annotate(Kind::Underline, cx)))).child(button("pdf-highlight", t("高亮")).on_click(cx.listener(|this, _, _, cx| this.annotate(Kind::Highlight, cx)))))
                .when(!piece && self.library_chat, |d| d.child(button("pdf-quote-library", t("加到文献库对话")).text_color(Theme::accent()).on_click(cx.listener(|this, _, _, cx| this.quote(true, cx)))).child(button("pdf-quote-paper", t("加到这篇的对话")).text_color(Theme::accent()).on_click(cx.listener(|this, _, _, cx| this.quote(false, cx)))))
                .when(!piece && !self.library_chat, |d| d.child(button("pdf-quote-paper", t("添加到对话")).text_color(Theme::accent()).on_click(cx.listener(|this, _, _, cx| this.quote(false, cx)))))
                .when(!piece, |d| d.child(button("pdf-translate", t("翻译")).on_click(cx.listener(|this, _, _, cx| this.translate(None, cx)))))
                // A piece of the page: a figure to keep, or a picture for the next question.
                .when(piece, |d| d.child(button("pdf-cut-keep", t("收藏到图片库")).text_color(Theme::accent()).on_click(cx.listener(|this, _, _, cx| this.collect_cut(cx)))))
                .when(piece && self.library_chat, |d| d.child(button("pdf-cut-library", t("截图加到文献库对话")).on_click(cx.listener(|this, _, _, cx| this.send_cut(true, cx)))).child(button("pdf-cut-paper", t("截图加到这篇的对话")).on_click(cx.listener(|this, _, _, cx| this.send_cut(false, cx)))))
                .when(piece && !self.library_chat, |d| d.child(button("pdf-cut-paper", t("用这张截图提问")).on_click(cx.listener(|this, _, _, cx| this.send_cut(false, cx)))))
                // Or left on the page as a mark, to come back to.
                .when(piece, |d| d.child(button("pdf-cut-mark", t("标记")).tooltip(ui::tip(t("在页面上留一圈虚线，之后点它再收藏或提问"))).on_click(cx.listener(|this, _, _, cx| this.mark_cut(cx)))))
                .when(piece, |d| d.child(button("pdf-cut-cancel", t("取消")).on_click(cx.listener(|this, _, _, cx| this.toggle_cutting(cx)))))
                .into_any_element(),
        )
    }

    /// What can be done with a highlight clicked, under the line clicked (over it at the viewer's foot):
    /// its colours, the one it has ringed, and 删除高亮. The same small bar as the selection's. A piece marked
    /// to be cut out has what a piece just cut has instead of colours: the figure library, a question.
    fn mark_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.mark_menu.as_ref()?;
        let mark = self.annotations.iter().find(|a| a.id == menu.id)?;
        let scale = self.scale_of(menu.page);
        let (x, y) = self.sheet_place(menu.page)?;
        const HEIGHT: f32 = 34.;
        let marked = mark.kind == Kind::Cut;
        let button = |id: &'static str, label: &'static str| div().id(id).flex().flex_none().items_center().h(text_px(26.)).px(px(9.)).rounded(px(6.)).cursor_pointer().hover(|s| s.bg(Theme::hover())).child(label);
        let below = y + menu.line.1 * scale + 6.;
        let top = if below + HEIGHT + 8. <= self.height { below } else { y + menu.line.0 * scale - 6. - HEIGHT };
        let swatches = COLORS.iter().zip(COLOR_NAMES).map(|(&key, name)| {
            div()
                .id(SharedString::from(format!("pdf-mark-{key}")))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(26.))
                .rounded_full()
                .cursor_pointer()
                .hover(|s| s.bg(Theme::hover()))
                .when(mark.color == key, |d| d.border_2().border_color(Theme::text_secondary()))
                .child(div().size(px(14.)).rounded_full().bg(Theme::highlight_swatch(key)))
                .tooltip(ui::tip(t(name)))
                .on_click(cx.listener(move |this, _, _, cx| this.recolor(key, cx)))
        });
        Some(
            div()
                .id("pdf-mark-menu")
                .absolute()
                .left(px((x + menu.x * scale - if marked { 0. } else { 16. }).clamp(8., (self.width - if marked { 420. } else { 240. }).max(8.))))
                .top(px(top.clamp(8., (self.height - HEIGHT - 8.).max(8.))))
                .flex()
                .items_center()
                .gap(px(1.))
                .p(px(3.))
                .rounded(px(8.))
                .bg(Theme::bg_body())
                .border_1()
                .border_color(Theme::line_strong())
                .shadow(crate::theme::shadow_card())
                .text_size(text_px(12.))
                .occlude()
                // A press on the menu is not a press on the page; one anywhere else puts it away.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down_out(cx.listener(|this, _, _, cx| if this.mark_menu.take().is_some() { cx.notify() }))
                .when(!marked, |d| d.children(swatches))
                .when(marked, |d| d.child(button("pdf-mark-keep", t("收藏到图片库")).text_color(Theme::accent()).on_click(cx.listener(|this, _, _, cx| this.collect_mark(cx)))))
                .when(marked && self.library_chat, |d| d.child(button("pdf-mark-library", t("截图加到文献库对话")).on_click(cx.listener(|this, _, _, cx| this.send_mark(true, cx)))).child(button("pdf-mark-paper", t("截图加到这篇的对话")).on_click(cx.listener(|this, _, _, cx| this.send_mark(false, cx)))))
                .when(marked && !self.library_chat, |d| d.child(button("pdf-mark-paper", t("用这张截图提问")).on_click(cx.listener(|this, _, _, cx| this.send_mark(false, cx)))))
                .child(div().flex_none().w(px(1.)).h(px(16.)).mx(px(4.)).bg(Theme::line()))
                .child(div().id("pdf-mark-remove").flex().flex_none().items_center().h(text_px(26.)).px(px(9.)).rounded(px(6.)).cursor_pointer().text_color(Theme::danger()).hover(|s| s.bg(Theme::hover())).child(match mark.kind { Kind::Box => t("删除方框"), Kind::Cut => t("删除标记"), _ => t("删除高亮") }).on_click(cx.listener(|this, _, _, cx| this.remove_mark(cx))))
                .into_any_element(),
        )
    }

    /// A bounded, scrollable list also lets a mark be found after reopening
    /// the file, without depending on a hit target over the PDF text.
    fn annotation_panel(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let button = |id, label: &'static str| div().id(id).px(px(8.)).py(px(4.)).rounded(px(5.)).cursor_pointer().hover(|s| s.bg(Theme::hover())).child(label);
        let width = (self.width - 16.).clamp(0., 360.);
        let list = self.annotations_open.then(|| {
            let rows = self.annotations.iter().map(|annotation| {
                let (id, edit_id, remove_id) = (annotation.id.clone(), annotation.id.clone(), annotation.id.clone());
                let (page, top) = (annotation.page, annotation.boxes.first().map(|b| b[1]).unwrap_or(0.));
                let note = annotation.note.clone();
                let editing = self.annotation_edit.as_deref() == Some(id.as_str());
                let kind = match annotation.kind { Kind::Underline => t("下划线"), Kind::Highlight => t("高亮"), Kind::Box => t("方框"), Kind::Note => t("便签"), Kind::Cut => t("截图标记") };
                div().id(SharedString::from(id)).p(px(10.)).border_b_1().border_color(Theme::line()).flex().flex_col().gap(px(7.))
                    .child(div().flex().flex_wrap().items_center().gap(px(4.))
                        .children(matches!(annotation.kind, Kind::Highlight | Kind::Box).then(|| div().flex_none().size(px(8.)).mr(px(2.)).rounded_full().bg(Theme::highlight_swatch(&annotation.color))))
                        .child(div().flex_1().text_color(Theme::text_secondary()).child(tf!("第 {page} 页 · {kind}", "Page {page} · {kind}")))
                        .child(button("annotation-locate", t("定位")).on_click(cx.listener(move |this, _, _, cx| this.go_to_place(page, top, cx))))
                        .child(button("annotation-edit", t("笔记")).on_click(cx.listener(move |this, _, _, cx| {
                            this.annotation_edit = Some(edit_id.clone());
                            this.annotation_note.update(cx, |input, cx| input.set_text(&note, cx));
                            cx.notify();
                        })))
                        .child(button("annotation-delete", t("删除")).on_click(cx.listener(move |this, _, _, cx| cx.emit(PdfEvent::RemoveAnnotation { resource: this.resource_id.clone(), id: remove_id.clone() })))))
                    .when(!annotation.quote.is_empty(), |d| d.child(div().max_h(px(100.)).overflow_hidden().child(annotation.quote.clone())))
                    .when(!editing && !annotation.note.is_empty(), |d| d.child(div().text_color(Theme::text_secondary()).child(annotation.note.clone())))
                    .when(editing, |d| d.child(ui::field(&self.annotation_note, window, cx)).child(div().flex().gap(px(5.))
                        .child(button("annotation-save", t("保存笔记")).text_color(Theme::accent()).on_click(cx.listener(|this, _, _, cx| {
                            if let Some(id) = this.annotation_edit.clone() { cx.emit(PdfEvent::AnnotationNote { resource: this.resource_id.clone(), id, note: this.annotation_note.read(cx).text().to_string() }) }
                        })))
                        .child(button("annotation-cancel", t("取消")).on_click(cx.listener(|this, _, _, cx| { this.annotation_edit = None; cx.notify(); })))))
            });
            div().id("pdf-annotations-list").w_full().max_h(px((self.height - 70.).max(0.))).overflow_y_scroll().bg(Theme::bg_body()).border_1().border_color(Theme::line_strong()).rounded(px(8.))
                .when(self.annotations.is_empty(), |d| d.child(div().p(px(12.)).text_color(Theme::text_secondary()).child(t("选中 PDF 文字后，可添加下划线或高亮；「方框」「便签」两个工具可以框出区域、在任意位置记笔记。"))))
                .children(rows)
        });
        div().absolute().top(px(8.)).right(px(8.)).max_w(px(width)).flex().flex_col().items_end().gap(px(6.)).text_size(text_px(12.)).line_height(text_px(18.)).text_color(Theme::text()).occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().id("pdf-annotations-toggle").px(px(10.)).py(px(5.)).rounded(px(6.)).bg(Theme::bg_body()).border_1().border_color(Theme::line_strong()).cursor_pointer()
                .child(tf!("标注 ({})", "Annotations ({})", self.annotations.len()))
                .on_click(cx.listener(|this, _, _, cx| { this.annotations_open = !this.annotations_open; cx.notify(); })))
            .children(list.map(|list| div().w(px(width)).child(list)))
            .into_any_element()
    }

    /// The viewer, `width` logical pixels wide.
    pub fn set_width(&mut self, width: f32) {
        if (self.width - width).abs() > 0.5 { self.resize((0., 0.), |this| this.width = width) }
    }

    /// The viewer's height, as it was laid out.
    fn set_height(&mut self, height: f32, cx: &mut Context<Self>) {
        if (self.height - height).abs() <= 0.5 { return }
        self.resize((0., 0.), |this| this.height = height);
        cx.notify();
    }
}

/// A sticky note folded to its icon, at a spot of a page in screen pixels; `lit` while its card is open.
fn note_icon(left: f32, top: f32, lit: bool) -> Div {
    div()
        .absolute()
        .left(px(left))
        .top(px(top))
        .size(px(NOTE_ICON))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .bg(Theme::note_paper())
        .border_1()
        .border_color(if lit { Theme::accent() } else { Theme::note_edge() })
        .when(lit, |d| d.border_2())
        .shadow(crate::theme::shadow_card())
        .child(ui::icon("sticky-note", 13., Theme::note_ink()))
}

impl Render for PdfView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(error) = &self.error {
            return ui::empty_state("image-broken", t("无法预览"), error).size_full().into_any_element();
        }
        if self.sizes.is_empty() {
            return ui::empty_state("file-text", t("正在打开 PDF…"), "").size_full().into_any_element();
        }
        self.device_scale = window.scale_factor();
        // The page and its translation are read as one: the half last scrolled leads the other.
        if self.facing.is_some() && self.split != Split::Off {
            let (lead, other) = (self.active, 1 - self.active);
            let (top, theirs) = (self.panes[lead].list.logical_scroll_top(), self.panes[other].list.logical_scroll_top());
            if top.item_ix != theirs.item_ix || top.offset_in_item != theirs.offset_in_item { self.panes[other].list.scroll_to(top) }
            self.panes[other].scroll_x = self.panes[lead].scroll_x;
        }
        let (measured, wheel, slid) = (cx.entity(), cx.entity(), cx.entity());
        let bars = [0, 1].map(|pane| self.sideways_bar(pane, cx));
        let mut bars = bars.into_iter();
        // A view's pages, with its sideways bar over their foot.
        let mut pane = |pane: usize| {
            let entity = cx.entity();
            let pages = list(self.panes[pane].list.clone(), move |index, _, cx| entity.update(cx, |this, _| this.page(pane, index))).size_full().pt(px(PAGE_GAP));
            div().size_full().relative().child(pages).children(bars.next().flatten())
        };
        // The pages once, or twice with a line between: each half scrolls on its own.
        let pages = match self.split {
            Split::Off => pane(0).into_any_element(),
            split => {
                let half = |pane: Div| div().flex_1().min_w_0().min_h_0().overflow_hidden().child(pane);
                let line = div().flex_none().bg(Theme::line_strong());
                let line = if split == Split::Side { line.w(px(DIVIDER)).h_full() } else { line.h(px(DIVIDER)).w_full() };
                div().size_full().flex().when(split == Split::Stacked, |d| d.flex_col()).child(half(pane(0))).child(line).child(half(pane(1))).into_any_element()
            }
        };
        // A card for a selection that has since gone goes with it.
        if self.translation.as_ref().is_some_and(|t| Some(t.selection) != self.selection) { self.translation = None }
        let bar = self.selection_bar(cx);
        let card = self.translation_card(cx);
        let mark_menu = self.mark_menu(cx);
        let note_card = self.note_card(window, cx);
        let notice = match self.citation.status {
            CitationStatus::PageOnly => Some(t("此引用仅提供页码，已标出引用页。")),
            CitationStatus::Searching => Some(t("正在定位引用原文…")),
            CitationStatus::Missing => Some(t("已跳到引用的页，但没有在 PDF 里找到这句原文。")),
            CitationStatus::Partial => Some(t("只在 PDF 里找到这句引文的开头，已标出；其余部分与原文对不上，请自行核对。")),
            CitationStatus::None | CitationStatus::Found => None,
        };
        // Where the sentence was not found word for word, the person looks for it: 自己搜索 opens the find bar with a word of it.
        let missed = matches!(self.citation.status, CitationStatus::Missing | CitationStatus::Partial);
        // The find bar, once open, is what is said about it.
        let notice = notice.filter(|_| self.finder.is_none());
        let finder = self.finder_bar(window, cx);
        div()
            .id("pdf-viewer")
            .key_context("PdfView")
            .track_focus(&self.focus)
            .size_full()
            .bg(Theme::bg_sunken())
            .relative()
            .on_action(cx.listener(|this, _: &CopySelection, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &FindInPdf, window, cx| this.open_finder(None, false, window, cx)))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::press))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| this.drag(event, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, event: &MouseUpEvent, _, cx| this.release(Some(event.position), cx)))
            // Esc puts a highlight's menu away before it does anything else.
            // Then a note's card, then a tool in use.
            .on_action(cx.listener(|this, _: &crate::shortcuts::Dismiss, window, cx| {
                if this.mark_menu.take().is_some() {
                } else if this.note_open.is_some() || this.note_draft.is_some() {
                    this.close_note();
                } else if this.finder.is_some() {
                    this.close_finder(window);
                } else if this.tool != Tool::Select {
                    (this.tool, this.drawing) = (Tool::Select, None);
                } else {
                    return cx.propagate();
                }
                cx.notify();
            }))
            // Under the pages: it learns the viewer's height, and hears the wheel before the list does.
            .child(
                canvas(
                    |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
                    move |bounds, hitbox, window, cx| {
                        let height = f32::from(bounds.size.height);
                        window.defer(cx, move |_, cx| measured.update(cx, |this, cx| this.set_height(height, cx)));
                        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
                            if phase == DispatchPhase::Capture && hitbox.is_hovered(window) && wheel.update(cx, |this, cx| this.wheel(event, bounds, cx)) { cx.stop_propagation() }
                        });
                        // A sideways bar held is heard wherever the pointer goes, until the button comes up.
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                            if phase != DispatchPhase::Capture { return }
                            slid.update(cx, |this, cx| {
                                if this.sliding.is_none() { return }
                                if event.pressed_button == Some(MouseButton::Left) { this.slide(event.position.x, cx) } else { this.sliding = None; cx.notify() }
                            });
                        });
                    },
                )
                .absolute()
                .size_full(),
            )
            .child(pages)
            // Explain whether the citation names a page or a passage within it.
            .children(bar)
            .children(card)
            .children(mark_menu)
            .children(note_card)
            .children(notice.map(|notice| {
                let search = ui::ghost_button("pdf-cite-search", Some("search"), Some(t("自己搜索").into())).h(text_px(22.)).px(px(6.)).text_size(text_px(12.)).text_color(Theme::accent()).on_click(cx.listener(|this, _, window, cx| {
                    let word = search_words(&this.citation.quote).into_iter().next().unwrap_or_default();
                    this.open_finder(Some(word), true, window, cx);
                }));
                div().absolute().top(px(8.)).left(px(8.)).right(px(FIND_CLEAR - 12.)).flex().justify_center().child(div().flex().items_center().gap(px(6.)).max_w(px((self.width - FIND_CLEAR - 4.).max(160.))).pl(px(10.)).pr(px(if missed { 4. } else { 10. })).py(px(if missed { 2. } else { 5. })).rounded(px(6.)).bg(Theme::bg_body()).border_1().border_color(Theme::line()).text_size(text_px(12.)).line_height(text_px(18.)).text_color(Theme::text_secondary()).child(div().flex_shrink().min_w_0().child(notice)).when(missed, |d| d.child(search)))
            }))
            .children(finder)
            .child(self.annotation_panel(window, cx))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that would bring in gpui's own `test` attribute.
    use super::{BAR_INSET, BAR_THUMB_MIN, CARD_EDGE, CardPlace, CitationFocus, CitationStatus, DIVIDER, MARGIN, PAGE_GAP, POINT_PX, Split, Zoom, bar_thumb, card_place, page_scale, pane_at, pane_size, picture_serves, render_scale, search_words, zoom_step};

    #[test]
    fn a_sentence_not_found_is_looked_for_by_its_words() {
        // The longer words first, each once, short ones left out.
        assert_eq!(search_words("Libraries were prepared with single-cell Chromium kits; libraries again."), ["single-cell", "Libraries", "prepared", "Chromium", "again", "were"]);
        assert_eq!(search_words("根系发育受到生长素的调控。"), ["根系发育", "受到生长", "素的调控"]);
        // A term in a Chinese sentence before the characters around it.
        assert_eq!(search_words("我们用 Arabidopsis 做了实验"), ["Arabidopsis", "我们用", "做了实验"]);
        assert!(search_words("").is_empty());
    }

    #[test]
    fn a_split_viewer_has_two_equal_halves() {
        let view = (800. + DIVIDER, 600. + DIVIDER);
        assert_eq!(pane_size(Split::Off, view), view);
        assert_eq!((pane_size(Split::Side, view), pane_size(Split::Stacked, view)), ((400., view.1), (view.0, 300.)));
        // Before the viewer is laid out its height is not known: a half is not less than nothing.
        assert_eq!(pane_size(Split::Stacked, (800., 0.)), (800., 0.));
        // A point is in the first half up to the line between them, in the second past it.
        assert_eq!((pane_at(Split::Off, view, (700., 500.)), pane_at(Split::Side, view, (399., 500.)), pane_at(Split::Side, view, (400. + DIVIDER, 10.))), (0, 0, 1));
        assert_eq!((pane_at(Split::Stacked, view, (700., 299.)), pane_at(Split::Stacked, view, (10., 300. + DIVIDER))), (0, 1));
        // At a fit the pages of a half are as wide as the half, side by side; a percentage stays.
        let a4 = (595., 842.);
        assert_eq!(page_scale(Zoom::FitWidth, a4, pane_size(Split::Side, view)), (400. - 2. * MARGIN) / 595.);
        assert_eq!(page_scale(Zoom::Percent(150.), a4, pane_size(Split::Side, view)), page_scale(Zoom::Percent(150.), a4, view));
        // The whole page in each half: with the gap above and below it, a page is no taller and no wider than its half.
        for split in [Split::Off, Split::Side, Split::Stacked] {
            let half = pane_size(split, view);
            let scale = page_scale(Zoom::FitPage, a4, half);
            assert!(a4.1 * scale + 2. * PAGE_GAP <= half.1 + 0.01 && a4.0 * scale + 2. * MARGIN <= half.0 + 0.01, "{split:?}");
        }
        // One over the other, the height decides, and the page fills it.
        assert!((page_scale(Zoom::FitPage, a4, pane_size(Split::Stacked, view)) * 842. + 2. * PAGE_GAP - 300.).abs() < 0.01);
    }

    #[test]
    fn page_only_citation_marks_its_page_before_or_after_pdf_opens() {
        let citation = wisp_core::ai::citations("结果见 [P429 p.8]。").remove(0);
        let mut focus = CitationFocus::default();
        focus.begin(citation.page.unwrap(), &citation.quote);
        assert_eq!(focus.status, CitationStatus::PageOnly);
        assert_eq!(focus.index(0), None);
        assert_eq!(focus.index(12), Some(7));
        assert!(focus.boxes.is_empty()); // A page reference must not invent a passage.
        focus.begin(4, " \n ");
        assert_eq!((&focus.status, focus.index(12)), (&CitationStatus::PageOnly, Some(3)));
        focus.begin(99, "");
        assert_eq!(focus.index(12), Some(11)); // Match navigation's page clamping.
        focus.begin(0, "");
        assert_eq!(focus.index(12), Some(0));
    }

    #[test]
    fn newer_citation_or_navigation_ignores_old_search_results() {
        let mut focus = CitationFocus::default();
        let old = focus.begin(4, "A quoted sentence.");
        let boxes = vec![[10., 20., 90., 30.]];
        focus.begin(8, "");
        assert!(!focus.finish(old, Some((3, boxes.clone(), true))));
        assert!(!focus.finish(old, None));
        assert_eq!((&focus.status, focus.index(12)), (&CitationStatus::PageOnly, Some(7)));

        let old = focus.begin(4, "An earlier quotation.");
        let latest = focus.begin(6, "Another quotation.");
        assert!(!focus.finish(old, None));
        assert!(focus.finish(latest, Some((6, boxes.clone(), true))));
        assert_eq!((&focus.status, focus.index(12), &focus.boxes), (&CitationStatus::Found, Some(6), &boxes));
        assert!(!focus.finish(old, Some((3, boxes, true))));

        let pending = focus.begin(4, "Still searching.");
        focus.clear();
        assert!(!focus.finish(pending, None));
        assert_eq!((&focus.status, focus.index(12)), (&CitationStatus::None, None));
    }

    #[test]
    fn missing_quote_keeps_page_marker_and_next_click_clears_notice() {
        let mut focus = CitationFocus::default();
        let request = focus.begin(4, "Not in the PDF.");
        assert!(focus.finish(request, None));
        assert_eq!((&focus.status, focus.index(12)), (&CitationStatus::Missing, Some(3)));
        focus.begin(8, "");
        assert_eq!((&focus.status, focus.index(12)), (&CitationStatus::PageOnly, Some(7)));
        let request = focus.begin(4, "Text without drawable boxes.");
        assert!(focus.finish(request, Some((3, vec![], true))));
        assert_eq!(focus.status, CitationStatus::Missing);
        // Only the beginning found: marked, and said to be a part.
        let request = focus.begin(4, "A quotation that drifts from the page.");
        assert!(focus.finish(request, Some((3, vec![[10., 20., 90., 30.]], false))));
        assert_eq!((&focus.status, focus.boxes.len()), (&CitationStatus::Partial, 1));
    }

    #[test]
    fn translation_card_stays_in_view() {
        let view = (800., 600.);
        // Under the selection, where it starts, as tall as it may be.
        let under = card_place([100., 100., 400., 140.], 360., 280., view);
        assert_eq!(under, CardPlace { left: 100., width: 360., top: Some(146.), bottom: None, max_height: 280. });
        // Near the right edge it moves in; near the bottom it stands over the selection, its bottom over the first line.
        let over = card_place([700., 500., 780., 540.], 360., 280., view);
        assert_eq!((over.left, over.top, over.bottom, over.max_height), (800. - 360. - CARD_EDGE, None, Some(600. - 494.), 280.));
        // With little room either way it takes the side with more, held to that room.
        let short = card_place([10., 150., 300., 200.], 360., 280., (800., 300.));
        assert_eq!((short.top, short.max_height), (None, 144. - CARD_EDGE));
        // A viewer narrower than the card: the card is narrowed to it.
        let narrow = card_place([0., 10., 50., 20.], 360., 280., (300., 600.));
        assert_eq!((narrow.left, narrow.width), (CARD_EDGE, 300. - 2. * CARD_EDGE));
        // A selection scrolled out of view above or below: the card stays inside, at the edge nearest it.
        let gone_up = card_place([100., -400., 300., -300.], 360., 280., view);
        assert_eq!((gone_up.top, gone_up.max_height), (Some(CARD_EDGE), 280.));
        let gone_down = card_place([100., 900., 300., 950.], 360., 280., view);
        assert_eq!((gone_down.bottom, gone_down.max_height), (Some(CARD_EDGE), 280.));
        // A selection at the very bottom of a short viewer still leaves the card some room, over the text.
        let low = card_place([100., 0., 300., 290.], 360., 280., (800., 300.));
        let bottom = low.bottom.map(|b| 300. - b).or(low.top).unwrap();
        assert!(low.max_height >= 80. && bottom - CARD_EDGE >= 0. && bottom <= 300. - CARD_EDGE);
    }

    #[test]
    fn the_sideways_bar_shows_how_much_is_out_of_view() {
        // A view 800 wide over pages twice as wide: the thumb is half the track, and runs its whole length.
        let track = 800. - 2. * BAR_INSET;
        assert_eq!(bar_thumb(800., 800., 0.), (track / 2., 0.));
        assert_eq!(bar_thumb(800., 800., 400.), (track / 2., track / 4.));
        assert_eq!(bar_thumb(800., 800., 800.), (track / 2., track / 2.));
        // Zoomed far in, the thumb stays long enough to take hold of, and inside its track.
        let (thumb, left) = bar_thumb(300., 30000., 30000.);
        assert_eq!((thumb, left), (BAR_THUMB_MIN, 300. - 2. * BAR_INSET - BAR_THUMB_MIN));
    }

    #[test]
    fn sizes_of_a_page() {
        let (a4, view) = ((595., 842.), (800., 600.));
        // As wide as the viewer less its margins; whole in it; its own size.
        assert_eq!(page_scale(Zoom::FitWidth, a4, view), (800. - 2. * MARGIN) / 595.);
        assert!((page_scale(Zoom::FitPage, a4, view) * 842. - (600. - 24.)).abs() < 0.01);
        assert_eq!(page_scale(Zoom::FitPage, a4, (800., 0.)), page_scale(Zoom::FitWidth, a4, view));
        assert_eq!((page_scale(Zoom::Percent(100.), a4, view), page_scale(Zoom::Percent(9000.), a4, view)), (POINT_PX, 5. * POINT_PX));
        // The buttons step through round sizes, from wherever the wheel left the zoom.
        assert_eq!((zoom_step(100., true), zoom_step(100., false), zoom_step(137.2, true), zoom_step(137.2, false)), (110., 90., 150., 125.));
        assert_eq!((zoom_step(500., true), zoom_step(25., false)), (500., 25.));
    }

    #[test]
    fn pictures_within_bounds() {
        let a4 = (595., 842.);
        // Reading width on a dense display, and a little more of the same: one picture serves both.
        assert_eq!((render_scale(a4, 2.6), render_scale(a4, 2.9), render_scale(a4, 1.3)), (3., 3., 2.));
        // Enlarged five times on a dense display a page would be 400 MB: it is held to what a picture may take.
        let capped = render_scale(a4, 13.3);
        assert!(capped < 7. && capped * capped * 595. * 842. <= 24_000_001.);
        // A poster stays under the longest side a pixmap can have.
        assert!(render_scale((10_000., 4_000.), 2.) * 10_000. <= 8000.5);
    }

    #[test]
    fn a_fit_follows_the_viewer_and_the_reader() {
        let a4 = (595., 842.);
        // At a fit the page grows and shrinks with the reading area; a percentage does not.
        assert!(page_scale(Zoom::FitWidth, a4, (1400., 900.)) > page_scale(Zoom::FitWidth, a4, (700., 900.)));
        assert_eq!(page_scale(Zoom::Percent(150.), a4, (1400., 900.)), page_scale(Zoom::Percent(150.), a4, (700., 900.)));
        // The next paper opens at the fit the reader chose; a percentage was for one figure.
        assert_eq!(Zoom::FitWidth.carried(Zoom::FitWidth), Zoom::FitWidth);
        assert_eq!(Zoom::FitPage.carried(Zoom::FitWidth), Zoom::FitPage);
        assert_eq!(Zoom::Percent(150.).carried(Zoom::FitWidth), Zoom::FitWidth);
        assert_eq!(Zoom::Percent(150.).carried(Zoom::FitPage), Zoom::FitPage);
        assert!(Zoom::FitPage.fits() && !Zoom::Percent(100.).fits());
    }

    #[test]
    fn a_narrower_viewer_keeps_its_pictures() {
        let a4 = (595., 842.);
        // A page shown at 3.2 device pixels a point wants a picture at 4; at 2.9 it would want 3, and the 4 serves.
        let (wide, narrow) = (render_scale(a4, 3.2), render_scale(a4, 2.9));
        assert_eq!((wide, narrow), (4., 3.));
        assert!(picture_serves(wide, narrow, 2.9));
        // Wider again, a softer picture does not.
        assert!(!picture_serves(narrow, wide, 3.2));
        // Zoomed far out, a large picture is let go of.
        assert!(!picture_serves(12., render_scale(a4, 2.), 2.));
        assert!(picture_serves(3., 3., 2.6));
    }
}
