//! PDF preview. A worker thread owns the parsed file and rasterises pages
//! with hayro (pure Rust, no system PDF library); the view is a virtualised
//! list that asks for the pages on screen and keeps the latest few. The same
//! thread finds a quoted sentence among the glyphs of a page, so a citation
//! can be shown where it stands.

use crate::i18n::t;
use crate::app::Translated;
use crate::theme::Theme;
use crate::ui;
use crate::input::TextInput;
use gpui::{prelude::FluentBuilder, *};
use wisp_core::annotation::{Annotation, Kind};
use wisp_core::pdftext::{self, Mark};

actions!(pdf, [CopySelection]);

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
    RemoveAnnotation { resource: String, id: String },
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

/// The next size up or down from this one, in percent.
fn zoom_step(percent: f32, up: bool) -> f32 {
    if up { ZOOM_STEPS.into_iter().find(|step| *step > percent + 0.5).unwrap_or(ZOOM_MAX) } else { ZOOM_STEPS.into_iter().rev().find(|step| *step < percent - 0.5).unwrap_or(ZOOM_MIN) }
}

/// The pages a quotation is looked for on before giving up.
const FIND_PAGES: usize = 300;
/// Page renders kept from a burst of requests; older ones are handed back.
const KEEP_REQUESTS: usize = 3;

enum Request {
    /// Render a page at so many device pixels per point.
    Page(usize, f32),
    /// Look for a quoted sentence, starting at this 0-based page.
    Find(Option<usize>, String),
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
    /// Where the quotation is: the page, and one box per line in page points.
    Found(usize, Vec<[f32; 4]>),
    NotFound,
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
    while let Ok(first) = requests.recv() {
        let mut waiting = vec![first];
        waiting.extend(requests.try_iter());
        // Of several quotations asked for in a row, only the last still matters.
        let find = waiting.iter().rev().find_map(|request| match request {
            Request::Find(page, quote) => Some((*page, quote.clone())),
            Request::Page(..) | Request::Marks(_) => None,
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

        if let Some((start, quote)) = find {
            // The page named first, then its neighbours, then the rest in order.
            let mut order: Vec<usize> = start.into_iter().flat_map(|page| [Some(page), page.checked_sub(1), Some(page + 1)]).flatten().filter(|page| *page < pages.len()).collect();
            let rest: Vec<usize> = (0..pages.len().min(FIND_PAGES)).filter(|page| !order.contains(page)).collect();
            order.extend(rest);
            let mut reply = Reply::NotFound;
            for index in order {
                let page = &pages[index];
                let (w, h) = page.render_dimensions();
                let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut glyphs = Glyphs::default();
                    let mut context = Context::new(page.initial_transform(true).to_kurbo(), Rect::new(0., 0., w as f64, h as f64), &text_cache, page.xref(), interpreter.clone());
                    interpret_page(page, &mut context, &mut glyphs);
                    glyphs
                }));
                let Ok(glyphs) = read else { continue };
                if let Some(range) = wisp_core::fulltext::find_quote(&glyphs.texts, &quote) {
                    reply = Reply::Found(index, wisp_core::fulltext::line_boxes(&glyphs.boxes[range]));
                    break;
                }
            }
            if replies.send_blocking(reply).is_err() { return }
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
    list: ListState,
    requests: mpsc::Sender<Request>,
    /// The quotation on show: its 0-based page, and one box per line in page points.
    highlight: Option<(usize, Vec<[f32; 4]>)>,
    /// A quotation was looked for and is not in this PDF.
    missing: bool,
    error: Option<String>,
    /// The viewer's size in logical pixels; the height is known once it has been laid out.
    width: f32,
    height: f32,
    zoom: Zoom,
    /// How far the pages are moved to the left, when they are wider than the viewer.
    scroll_x: f32,
    /// How far they can be.
    max_x: f32,
    /// Device pixels per logical pixel of the window the viewer is in.
    device_scale: f32,
    /// A page to jump to once the file has opened.
    goto: Option<usize>,
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
    focus: FocusHandle,
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
        PdfView {
            sizes: vec![],
            images: HashMap::new(),
            broken: HashSet::new(),
            recent: VecDeque::new(),
            pending: HashSet::new(),
            list: ListState::new(0, ListAlignment::Top, px(600.)),
            requests,
            error: None,
            width: 0.,
            height: 0.,
            zoom,
            scroll_x: 0.,
            max_x: 0.,
            device_scale: 2.,
            goto: None,
            highlight: None,
            missing: false,
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
            focus: cx.focus_handle(),
        }
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

    /// The page at the top of the view, 1-based.
    pub fn current_page(&self) -> usize {
        self.list.logical_scroll_top().item_ix + 1
    }

    pub fn zoom(&self) -> Zoom {
        self.zoom
    }

    /// How large the page in view is drawn, in percent of its own size.
    pub fn percent(&self) -> f32 {
        self.scale_of(self.list.logical_scroll_top().item_ix) / POINT_PX * 100.
    }

    /// Go to a 1-based page and look for a sentence quoted from it; when it
    /// is found, there or on another page, it is highlighted and scrolled
    /// into view.
    pub fn find(&mut self, page: usize, quote: &str, cx: &mut Context<Self>) {
        self.highlight = None;
        self.missing = false;
        self.go_to(page, cx);
        let _ = self.requests.send(Request::Find(page.checked_sub(1), quote.to_string()));
    }

    /// How many logical pixels a point of this page takes.
    fn scale_of(&self, index: usize) -> f32 {
        page_scale(self.zoom, self.sizes.get(index).copied().unwrap_or((595., 842.)), (self.width, self.height))
    }

    /// Where a page's left edge stands in the viewer: in the middle while
    /// the page fits, else moved by the sideways scroll.
    fn sheet_left(&self, sheet_width: f32) -> f32 {
        if sheet_width + 2. * MARGIN <= self.width { (self.width - sheet_width) / 2. } else { MARGIN - self.scroll_x }
    }

    /// Change what decides the size of the pages (the zoom, the viewer's
    /// size), and keep in view what was: the point `anchor` pixels from the
    /// viewer's top left corner stays under it.
    fn resize(&mut self, anchor: (f32, f32), change: impl FnOnce(&mut Self)) {
        let top = self.list.logical_scroll_top();
        let before = self.scale_of(top.item_ix);
        let (page_width, page_height) = self.sizes.get(top.item_ix).copied().unwrap_or((595., 842.));
        let left = self.sheet_left(page_width * before);
        change(self);
        let after = self.scale_of(top.item_ix);
        let widest = self.sizes.iter().enumerate().map(|(index, (width, _))| width * self.scale_of(index)).fold(0., f32::max);
        self.max_x = (widest + 2. * MARGIN - self.width).max(0.);
        let ratio = after / before.max(0.001);
        // Sideways: the point of the page under the anchor comes back under it.
        self.scroll_x = (MARGIN - (anchor.0 - (anchor.0 - left) * ratio)).clamp(0., self.max_x);
        if (ratio - 1.).abs() < 0.0005 { return }
        // Page heights follow the scale: measure the list again.
        let offset = ((f32::from(top.offset_in_item) + anchor.1) * ratio - anchor.1).clamp(0., (page_height * after - 1.).max(0.));
        self.list.reset(self.sizes.len());
        self.list.scroll_to(ListOffset { item_ix: top.item_ix, offset_in_item: px(offset) });
    }

    /// Draw the pages at another size. `anchor` is the point of the viewer
    /// that stays where it is (the pointer's place); without one, the top.
    pub fn set_zoom(&mut self, zoom: Zoom, anchor: Option<(f32, f32)>, cx: &mut Context<Self>) {
        let zoom = match zoom {
            Zoom::Percent(percent) => Zoom::Percent(percent.clamp(ZOOM_MIN, ZOOM_MAX)),
            fit => fit,
        };
        if zoom == self.zoom { return }
        self.resize(anchor.unwrap_or((self.width / 2., 0.)), |this| this.zoom = zoom);
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
        if event.modifiers.secondary() || event.modifiers.control {
            if dy != 0. {
                let anchor = (f32::from(event.position.x - bounds.origin.x), f32::from(event.position.y - bounds.origin.y));
                self.set_zoom(Zoom::Percent(self.percent() * (dy / 240.).exp()), Some(anchor), cx);
            }
            return true;
        }
        // A wheel that only turns one way moves sideways with Shift held.
        let sideways = if event.modifiers.shift && dx == 0. { dy } else { dx };
        if self.max_x <= 0. || sideways == 0. || (sideways.abs() <= dy.abs() && !event.modifiers.shift) { return false }
        self.scroll_x = (self.scroll_x - sideways).clamp(0., self.max_x);
        cx.notify();
        true
    }

    /// Scroll to a 1-based page.
    pub fn go_to(&mut self, page: usize, cx: &mut Context<Self>) {
        if self.sizes.is_empty() {
            self.goto = Some(page);
        } else {
            self.list.scroll_to(ListOffset { item_ix: page.clamp(1, self.sizes.len()) - 1, offset_in_item: px(0.) });
        }
        cx.notify();
    }

    /// Go to a place on a 1-based page: what stands `top` points below the page's top comes a little under the top of the view.
    pub fn go_to_place(&mut self, page: usize, top: f32, cx: &mut Context<Self>) {
        if self.sizes.is_empty() { return self.go_to(page, cx) }
        let index = page.clamp(1, self.sizes.len()) - 1;
        self.list.scroll_to(ListOffset { item_ix: index, offset_in_item: px((top * self.scale_of(index) - 48.).max(0.)) });
        cx.notify();
    }

    fn receive(&mut self, reply: Reply, cx: &mut Context<Self>) {
        match reply {
            Reply::Opened(sizes) => {
                self.sizes = sizes;
                self.list.reset(self.sizes.len());
                self.resize((0., 0.), |_| {});
                if let Some(page) = self.goto.take() { self.go_to(page, cx) }
            }
            Reply::Failed(error) => self.error = Some(error),
            Reply::Skipped(index) => {
                self.pending.remove(&index);
            }
            Reply::Found(index, boxes) => {
                // Bring the first line of the quotation a little below the top of the view, and into it sideways.
                if let Some(first) = boxes.first() {
                    let scale = self.scale_of(index);
                    self.list.scroll_to(ListOffset { item_ix: index, offset_in_item: px((first[1] * scale - 80.).max(0.)) });
                    if self.max_x > 0. { self.scroll_x = (first[0] * scale - 40.).clamp(0., self.max_x) }
                }
                self.highlight = Some((index, boxes));
            }
            Reply::NotFound => self.missing = true,
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
                        while self.recent.len() > KEEP_PAGES || (self.recent.len() > 2 && self.images.values().map(|(.., size)| size).sum::<usize>() > KEEP_BYTES) {
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

    fn page(&mut self, index: usize) -> AnyElement {
        let (w, h) = self.sizes.get(index).copied().unwrap_or((595., 842.));
        let scale = self.scale_of(index);
        let (width, height) = (w * scale, h * scale);
        // The picture this size wants. One made for another size is shown, stretched, until its own arrives.
        let wanted = render_scale((w, h), scale * self.device_scale);
        let image = self.images.get(&index).map(|(image, at, _)| (image.clone(), *at));
        let stale = image.as_ref().is_none_or(|(_, at)| (at - wanted).abs() > 0.01);
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
        let annotations: Vec<Div> = self.annotations.iter().filter(|a| a.page == index + 1).flat_map(|a| a.boxes.iter().map(move |line| {
            let (top, height, color) = match a.kind {
                Kind::Underline => (line[3] * scale - (1.3 * scale).max(1.), (1.3 * scale).max(1.), hsla(215. / 360., 0.85, 0.48, 0.9)),
                Kind::Highlight => (line[1] * scale, (line[3] - line[1]) * scale, hsla(48. / 360., 1., 0.5, 0.3)),
            };
            div().absolute().left(px(line[0] * scale)).top(px(top)).w(px((line[2] - line[0]) * scale)).h(px(height)).bg(color)
        })).collect();
        // The quotation on this page: a translucent marker over each of its lines.
        let marks: Vec<Div> = self
            .highlight
            .iter()
            .filter(|(page, _)| *page == index)
            .flat_map(|(_, boxes)| boxes.iter())
            .map(|line| div().absolute().left(px(line[0] * scale - 1.)).top(px(line[1] * scale)).w(px((line[2] - line[0]) * scale + 2.)).h(px((line[3] - line[1]) * scale)).rounded(px(2.)).bg(hsla(50. / 360., 1., 0.5, 0.35)))
            .collect();
        let sheet = div().relative().flex_none().ml(px(self.sheet_left(width))).w(px(width)).h(px(height)).bg(gpui::white()).border_1().border_color(Theme::line()).overflow_hidden();
        let sheet = match image {
            Some((image, _)) => sheet.when(cutting, |d| d.cursor_crosshair()).when(!cutting, |d| d.cursor_text()).child(img(image).w(px(width)).h(px(height))).children(annotations).children(selected).children(marks).children(cut),
            None => sheet.flex().items_center().justify_center().text_size(px(12.)).text_color(Theme::text_placeholder()).child(if self.broken.contains(&index) { tf!("第 {} 页无法渲染", "Cannot render page {}", index + 1) } else { tf!("第 {} 页", "Page {}", index + 1) }),
        };
        div().w_full().flex().pb(px(PAGE_GAP)).child(sheet).into_any_element()
    }

    /// The page under a point of the window, and the point in that page's own points.
    fn locate(&self, position: Point<Pixels>) -> Option<(usize, f32, f32)> {
        let top = self.list.logical_scroll_top().item_ix;
        (top..self.sizes.len().min(top + 8)).find_map(|index| {
            let bounds = self.list.bounds_for_item(index)?;
            if position.y < bounds.top() || position.y >= bounds.bottom() { return None }
            let scale = self.scale_of(index);
            let left = f32::from(bounds.left()) + self.sheet_left(self.sizes[index].0 * scale);
            Some((index, (f32::from(position.x) - left) / scale, f32::from(position.y - bounds.top()) / scale))
        })
    }

    /// The pointer went down on a page: a selection starts at the glyph under it.
    fn press(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        (self.selection, self.selecting, self.translation) = (None, false, None);
        if self.cutting {
            // A piece of the page is cut out from here.
            self.cut = self.locate(event.position).map(|(page, x, y)| (page, [x, y, x, y]));
            self.selecting = self.cut.is_some();
            return cx.notify();
        }
        if let Some((page, x, y)) = self.locate(event.position) && let Some(at) = self.marks.get(&page).and_then(|marks| pdftext::mark_at(marks, x, y)) {
            self.selection = Some(Selection { page, anchor: at, focus: at });
            self.selecting = true;
        }
        cx.notify();
    }

    /// The pointer moved with the button held: the selection reaches to the glyph under it, on the page it began on.
    fn drag(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.selecting { return }
        if event.pressed_button != Some(MouseButton::Left) { return self.release(cx) }
        let Some((page, x, y)) = self.locate(event.position) else { return };
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

    /// The button came up: a pointer that never left its glyph selected nothing.
    fn release(&mut self, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.selecting) { return }
        if self.selection.is_some_and(|selection| selection.anchor == selection.focus) { self.selection = None }
        // A piece too small to show anything was a slip of the hand.
        if self.cut.is_some_and(|(_, [x0, y0, x1, y1])| (x1 - x0).abs() < 8. || (y1 - y0).abs() < 8.) { self.cut = None }
        cx.notify();
    }

    /// Cut pieces out of pages with the pointer, or go back to selecting text.
    pub fn toggle_cutting(&mut self, cx: &mut Context<Self>) {
        self.cutting = !self.cutting;
        (self.cut, self.selection, self.selecting, self.translation) = (None, None, false, None);
        cx.notify();
    }

    pub fn cutting(&self) -> bool {
        self.cutting
    }

    /// The piece cut out, as a PNG: taken from the picture the page is drawn with.
    fn cut_picture(&self) -> Option<Vec<u8>> {
        let (page, [x0, y0, x1, y1]) = self.cut?;
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

    /// The piece cut out is in the figure library: the next one can be cut.
    pub fn cut_kept(&mut self, cx: &mut Context<Self>) {
        self.cut = None;
        cx.notify();
    }

    /// Mark a piece of a 0-based page as cut out, as a drag would.
    pub fn set_cut(&mut self, page: usize, bbox: [f32; 4], cx: &mut Context<Self>) {
        (self.cutting, self.cut, self.selection, self.selecting, self.translation) = (true, Some((page, bbox)), None, false, None);
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
        let (item, view) = (self.list.bounds_for_item(selection.page)?, self.list.viewport_bounds());
        let scale = self.scale_of(selection.page);
        let (x, y) = (f32::from(item.left() - view.left()) + self.sheet_left(self.sizes.get(selection.page)?.0 * scale), f32::from(item.top() - view.top()));
        let (left, right) = boxes.iter().fold((f32::MAX, f32::MIN), |(l, r), line| (l.min(line[0]), r.max(line[2])));
        let place = card_place([x + left * scale, y + first[1] * scale, x + right * scale, y + last[3] * scale], CARD_W, CARD_TALL, (self.width, self.height));
        let button = |id: &'static str, label: &'static str| div().id(id).flex().flex_none().items_center().h(px(24.)).px(px(8.)).rounded(px(6.)).cursor_pointer().text_color(Theme::accent()).hover(|s| s.bg(Theme::hover())).child(label);
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
                .text_size(px(13.))
                .child(div().px(px(8.)).py(px(4.)).text_size(px(11.)).text_color(Theme::text_tertiary()).child(t("只这一次换用，不改设置")))
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
            .h(px(34.))
            .pl(px(12.))
            .pr(px(4.))
            .border_b_1()
            .border_color(Theme::line())
            .child(div().flex_1().text_size(px(12.)).text_color(Theme::text_tertiary()).child(t("译文")))
            .child(
                div()
                    .id("pdf-translation-model")
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap(px(4.))
                    .h(px(24.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_size(px(12.))
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
                .font_family(crate::theme::UI_FONT)
                .text_size(px(13.))
                .line_height(px(20.))
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
        let (item, view) = (self.list.bounds_for_item(page)?, self.list.viewport_bounds());
        let scale = self.scale_of(page);
        // Under the last line of the selection, kept inside the viewer.
        let left = f32::from(item.left() - view.left()) + self.sheet_left(self.sizes.get(page)?.0 * scale) + last[0] * scale;
        let top = f32::from(item.top() - view.top()) + last[3] * scale + 6.;
        let button = |id: &'static str, label: &'static str| div().id(id).flex().flex_none().items_center().h(px(26.)).px(px(9.)).rounded(px(6.)).cursor_pointer().hover(|s| s.bg(Theme::hover())).child(label);
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
                .text_size(px(12.))
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
                .when(piece, |d| d.child(button("pdf-cut-cancel", t("取消")).on_click(cx.listener(|this, _, _, cx| this.toggle_cutting(cx)))))
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
                let kind = match annotation.kind { Kind::Underline => t("下划线"), Kind::Highlight => t("高亮") };
                div().id(SharedString::from(id)).p(px(10.)).border_b_1().border_color(Theme::line()).flex().flex_col().gap(px(7.))
                    .child(div().flex().flex_wrap().items_center().gap(px(4.))
                        .child(div().flex_1().text_color(Theme::text_secondary()).child(tf!("第 {page} 页 · {kind}", "Page {page} · {kind}")))
                        .child(button("annotation-locate", t("定位")).on_click(cx.listener(move |this, _, _, cx| this.go_to_place(page, top, cx))))
                        .child(button("annotation-edit", t("笔记")).on_click(cx.listener(move |this, _, _, cx| {
                            this.annotation_edit = Some(edit_id.clone());
                            this.annotation_note.update(cx, |input, cx| input.set_text(&note, cx));
                            cx.notify();
                        })))
                        .child(button("annotation-delete", t("删除")).on_click(cx.listener(move |this, _, _, cx| cx.emit(PdfEvent::RemoveAnnotation { resource: this.resource_id.clone(), id: remove_id.clone() })))))
                    .child(div().max_h(px(100.)).overflow_hidden().child(annotation.quote.clone()))
                    .when(!editing && !annotation.note.is_empty(), |d| d.child(div().text_color(Theme::text_secondary()).child(annotation.note.clone())))
                    .when(editing, |d| d.child(ui::field(&self.annotation_note, window, cx)).child(div().flex().gap(px(5.))
                        .child(button("annotation-save", t("保存笔记")).text_color(Theme::accent()).on_click(cx.listener(|this, _, _, cx| {
                            if let Some(id) = this.annotation_edit.clone() { cx.emit(PdfEvent::AnnotationNote { resource: this.resource_id.clone(), id, note: this.annotation_note.read(cx).text().to_string() }) }
                        })))
                        .child(button("annotation-cancel", t("取消")).on_click(cx.listener(|this, _, _, cx| { this.annotation_edit = None; cx.notify(); })))))
            });
            div().id("pdf-annotations-list").w_full().max_h(px((self.height - 70.).max(0.))).overflow_y_scroll().bg(Theme::bg_body()).border_1().border_color(Theme::line_strong()).rounded(px(8.))
                .when(self.annotations.is_empty(), |d| d.child(div().p(px(12.)).text_color(Theme::text_secondary()).child(t("选中 PDF 文字后，可添加下划线或高亮。"))))
                .children(rows)
        });
        div().absolute().top(px(8.)).right(px(8.)).max_w(px(width)).flex().flex_col().items_end().gap(px(6.)).text_size(px(12.)).line_height(px(18.)).text_color(Theme::text()).occlude()
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

impl Drop for PdfView {
    fn drop(&mut self) {
        // The textures of dropped images are released with the window's atlas entries.
        self.images.clear();
    }
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
        let entity = cx.entity();
        let (measured, wheel) = (cx.entity(), cx.entity());
        // A card for a selection that has since gone goes with it.
        if self.translation.as_ref().is_some_and(|t| Some(t.selection) != self.selection) { self.translation = None }
        let bar = self.selection_bar(cx);
        let card = self.translation_card(cx);
        div()
            .id("pdf-viewer")
            .key_context("PdfView")
            .track_focus(&self.focus)
            .size_full()
            .bg(Theme::bg_sunken())
            .relative()
            .on_action(cx.listener(|this, _: &CopySelection, _, cx| this.copy(cx)))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::press))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| this.drag(event, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.release(cx)))
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
                    },
                )
                .absolute()
                .size_full(),
            )
            .child(list(self.list.clone(), move |index, _, cx| entity.update(cx, |this, _| this.page(index))).size_full().pt(px(PAGE_GAP)))
            // Said plainly: the page is the cited one, the sentence was not found on it or anywhere else.
            .children(bar)
            .children(card)
            .children(self.missing.then(|| div().absolute().top(px(8.)).left_0().w_full().flex().justify_center().child(div().px(px(10.)).py(px(5.)).rounded(px(6.)).bg(Theme::bg_body()).border_1().border_color(Theme::line()).text_size(px(12.)).text_color(Theme::text_secondary()).child(t("已跳到引用的页，但没有在 PDF 里找到这句原文。")))))
            .child(self.annotation_panel(window, cx))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that would bring in gpui's own `test` attribute.
    use super::{CARD_EDGE, CardPlace, MARGIN, POINT_PX, Zoom, card_place, page_scale, render_scale, zoom_step};

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
}
