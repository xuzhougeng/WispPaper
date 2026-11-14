//! 图片库: the figures cut out of the library's PDFs (`wisp_core::figure`).
//! It is entered from 收藏, which holds the starred papers and the figures
//! side by side (`render_collection_switch`). The figures are a grid of
//! cards that can be searched and narrowed to a tag of their own; a card
//! opens the figure with its number, legend, tags and notes to change, and
//! leads back to the page it was cut from. A piece cut out in the PDF
//! viewer comes here through the same dialog, its number and legend read
//! off the page (`figure::caption`) for the person to look over. The
//! sparkles by the dialog's fields have a model that sees pictures write
//! the legend, tags or a reading of the figure (`figure::Ask`, with the
//! model questions are put to); what it writes is in the field to change,
//! and kept only with the rest.
//!
//! 生成图片 on the page opens the same dialog with a box under the empty
//! stage (`FigureForm::draw`, a `Drawing`): what is written there goes to
//! the model that draws (`Task::Image`, `wisp_core::imagegen`), the picture
//! that comes back is the dialog's, to draw again or to keep. A figure kept
//! that way has no paper: it says so where the others say where they are from.

use crate::i18n::{t, tc};
use crate::app::{Cmd, MenuItem, MenuKind, Modal, Page, Picture, Tab, Workspace};
use crate::input::{InputEvent, TextInput};
use crate::pdf::Piece;
use crate::theme::{Theme, text_px};
use crate::ui;
use gpui::{prelude::FluentBuilder, *};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use wisp_core::ai;
use wisp_core::figure::{self, Ask, Figure};
use wisp_core::imagegen::{self, Shape};
use wisp_core::settings::Task;
use wisp_core::filter::View;
use wisp_core::model::{Paper, new_id, split_list};

/// The narrowest a card is drawn, the room between cards, and a card's height with its picture's.
const CARD_MIN: f32 = 232.;
const GAP: f32 = 14.;
const CARD_H: f32 = 292.;
const PICTURE_H: f32 = 164.;

/// The longest edge of a card's picture in pixels, how many are kept, and how many are read at a time.
const THUMB_EDGE: u32 = 560;
const KEEP_THUMBS: usize = 160;
const THUMB_WORKERS: usize = 4;

/// The longest edge the dialog's picture is drawn from: beyond it a screen shows no more.
const LARGE_EDGE: u32 = 2800;

/// The width of the dialog's column of fields.
const SIDE_W: f32 = 348.;

/// What stands under the dialog's picture: a line of tools, or the box a picture is drawn from.
const TOOLS_H: f32 = 52.;
const DRAW_H: f32 = 172.;

/// The tag a picture a model drew is given, so that such pictures can be told from those cut out of papers.
const DRAWN_TAG: &str = "AI 生成";

/// The figures of the open library, and what the page and the dialog need to show them.
pub struct Gallery {
    /// Every figure, the newest first.
    pub items: Vec<Figure>,
    pub search: Entity<TextInput>,
    /// Only the figures with this tag; empty for all of them.
    pub tag: String,
    pub scroll: UniformListScrollHandle,
    /// The cards' pictures read so far, the least lately shown first in `recent`.
    thumbs: HashMap<String, Arc<RenderImage>>,
    recent: VecDeque<String>,
    loading: HashSet<String>,
    /// Pictures that could not be read.
    broken: HashSet<String>,
    /// The picture of the dialog on show, by the dialog's key.
    large: Option<(String, Arc<RenderImage>)>,
}

/// A figure as the dialog holds it: one of the library, or a piece just cut out that is not kept yet.
pub struct FigureForm {
    /// The figure looked at; `None` for a new one.
    pub id: Option<String>,
    /// What the dialog's picture is kept under.
    key: String,
    pub paper_id: String,
    resource_id: String,
    pub page: usize,
    bbox: Option<[f32; 4]>,
    /// A new figure's picture, as it will be kept.
    png: Option<Arc<Vec<u8>>>,
    pub label: Entity<TextInput>,
    pub legend: Entity<TextInput>,
    pub tags: Entity<TextInput>,
    pub notes: Entity<TextInput>,
    /// The number and the legend were read off the page.
    pub guessed: bool,
    /// What a model is writing right now.
    asking: HashSet<Ask>,
    /// Not drawn yet: with the first frame the keyboard goes to a field.
    fresh: bool,
    /// 生图: the picture is a model's, drawn from what is written here.
    pub draw: Option<Drawing>,
}

/// The box a picture is drawn from, in the dialog of a figure that is not kept yet.
pub struct Drawing {
    /// What the picture should show.
    pub prompt: Entity<TextInput>,
    pub shape: Shape,
    /// A picture is being drawn.
    busy: bool,
    /// Counts the requests, and the stops: an answer to an earlier one is dropped.
    run: u64,
    /// What the notes were given with the last picture, replaced by the next one's while they still say that.
    noted: String,
}

/// How a card's picture stands.
enum Thumb {
    Ready(Arc<RenderImage>),
    Loading,
    Broken,
}

/// A picture's pixels the way GPUI draws them (BGRA), scaled down to `longest` when larger.
fn pixels(bytes: &[u8], longest: u32) -> Option<image::RgbaImage> {
    let picture = image::load_from_memory(bytes).ok()?;
    let picture = if picture.width().max(picture.height()) > longest { picture.resize(longest, longest, image::imageops::FilterType::Triangle) } else { picture };
    let mut pixels = picture.into_rgba8();
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Some(pixels)
}

/// The size a picture of `natural` pixels is drawn at inside `room`: whole, and never larger than it is.
fn fit(natural: (f32, f32), room: (f32, f32)) -> (f32, f32) {
    if natural.0 <= 0. || natural.1 <= 0. { return (0., 0.) }
    let scale = (room.0 / natural.0).min(room.1 / natural.1).min(1.);
    ((natural.0 * scale).max(1.), (natural.1 * scale).max(1.))
}

/// How many cards stand in a row `width` wide, and how wide each is.
fn grid(width: f32) -> (usize, f32) {
    let inner = (width - 32.).max(CARD_MIN);
    let columns = (((inner + GAP) / (CARD_MIN + GAP)).floor() as usize).max(1);
    (columns, (inner - GAP * (columns - 1) as f32) / columns as f32)
}

/// Where a figure is from, in a line: who and when, then the title.
fn source_line(paper: Option<&Paper>) -> String {
    let Some(paper) = paper else { return t("文献已不在库里").into() };
    let mut who = crate::i18n::short_authors(paper);
    if let Some(year) = paper.year { who = if who.is_empty() { year.to_string() } else { format!("{who} {year}") } }
    if who.is_empty() { paper.title.clone() } else { format!("{who} · {}", paper.title) }
}

/// The name a figure's picture is saved under elsewhere: who, when and its number.
fn file_name(paper: Option<&Paper>, figure: &Figure) -> String {
    let mut parts = vec![];
    if let Some(paper) = paper {
        let first = paper.authors.split([';', ',']).next().unwrap_or("").trim().to_string();
        if !first.is_empty() { parts.push(first) }
        if let Some(year) = paper.year { parts.push(year.to_string()) }
    }
    if !figure.label.is_empty() { parts.push(figure.label.clone()) }
    let name = if parts.is_empty() { "figure".to_string() } else { parts.join(" ") };
    let extension = figure.local_path.rsplit('.').next().unwrap_or("png");
    format!("{}.{extension}", name.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "-").trim_end_matches('.'))
}

impl Gallery {
    pub fn new(subscriptions: &mut Vec<Subscription>, cx: &mut Context<Workspace>) -> Gallery {
        let search = cx.new(|cx| TextInput::new("搜索图注、图号、标签、备注，或所属文献…", cx));
        subscriptions.push(cx.subscribe(&search, |this, _, event: &InputEvent, cx| {
            if *event != InputEvent::Changed { return }
            // Another set of cards: from its top.
            if let Some(lib) = this.lib.as_mut() { lib.gallery.scroll = UniformListScrollHandle::new() }
            cx.notify();
        }));
        Gallery { items: vec![], search, tag: String::new(), scroll: UniformListScrollHandle::new(), thumbs: HashMap::new(), recent: VecDeque::new(), loading: HashSet::new(), broken: HashSet::new(), large: None }
    }

    pub fn figure(&self, id: &str) -> Option<&Figure> {
        self.items.iter().find(|figure| figure.id == id)
    }

    /// The figures on show, as places in `items`: those with the tag chosen that say every word searched for.
    fn shown(&self, papers: &[Paper], query: &str) -> Vec<usize> {
        let words = figure::search_words(query);
        // The papers are only looked through when something is searched for.
        let by_id: HashMap<&str, &Paper> = if words.is_empty() { HashMap::new() } else { papers.iter().map(|paper| (paper.id.as_str(), paper)).collect() };
        (0..self.items.len())
            .filter(|&at| {
                let figure = &self.items[at];
                (self.tag.is_empty() || figure.tags.contains(&self.tag)) && figure.matches(by_id.get(figure.paper_id.as_str()).copied(), &words)
            })
            .collect()
    }

    fn touch(&mut self, id: &str) {
        if self.recent.back().is_some_and(|last| last == id) { return }
        self.recent.retain(|other| other != id);
        self.recent.push_back(id.to_string());
    }
}

impl Workspace {
    // ---- commands

    pub(crate) fn run_figures(&mut self, cmd: Cmd, window: &mut Window, cx: &mut Context<Self>) {
        match cmd {
            Cmd::FigureTag(tag) => {
                let Some(lib) = self.lib.as_mut() else { return };
                lib.gallery.tag = if lib.gallery.tag == tag { String::new() } else { tag };
                lib.gallery.scroll = UniformListScrollHandle::new();
            }
            Cmd::OpenFigure(id) => self.figure_dialog(&id, cx),
            Cmd::FigureSource(id) => {
                let Some(figure) = self.lib.as_ref().and_then(|lib| lib.gallery.figure(&id)).cloned() else { return };
                if figure.drawn() { return }
                self.run(Cmd::CloseModal, window, cx);
                if self.lib.as_ref().is_some_and(|lib| lib.paper(&figure.paper_id).is_none()) { return self.fail(t("这张图的文献已不在库里。"), cx) }
                self.run(Cmd::GotoSource(figure.paper_id.clone(), (figure.page > 0).then_some(figure.page), String::new()), window, cx);
                // Where on the page it was cut from, once the page is at hand.
                if let (Some(bbox), Some(pdf)) = (figure.bbox, self.lib.as_ref().and_then(|lib| lib.pdf.clone())) && figure.page > 0 {
                    pdf.update(cx, |pdf, cx| pdf.go_to_place(figure.page, bbox[1], cx));
                }
            }
            Cmd::AskFigure(id) => {
                let Some((paper_id, path)) = self.lib.as_ref().and_then(|lib| lib.gallery.figure(&id).filter(|figure| !figure.drawn()).map(|figure| (figure.paper_id.clone(), lib.store.figure_path(figure)))) else { return };
                self.run(Cmd::CloseModal, window, cx);
                self.run(Cmd::OpenPaper(paper_id, Tab::Ask), window, cx);
                self.attach_images(crate::app::Talk::Paper, vec![Picture::File(path)], cx);
            }
            Cmd::CopyFigure(id) => {
                let Some(path) = self.lib.as_ref().and_then(|lib| lib.gallery.figure(&id).map(|figure| lib.store.figure_path(figure))) else { return };
                let format = match path.extension().and_then(|e| e.to_str()) {
                    Some("jpg") => ImageFormat::Jpeg,
                    Some("webp") => ImageFormat::Webp,
                    _ => ImageFormat::Png,
                };
                match std::fs::read(&path) {
                    Ok(bytes) => {
                        cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(format, bytes)));
                        self.say(t("图片已复制"), cx);
                    }
                    Err(err) => self.fail(tf!("无法读取 {}：{err}", "Cannot read {}: {err}", path.display()), cx),
                }
            }
            Cmd::SaveFigure(id) => self.save_figure_as(&id, cx),
            Cmd::AskRemoveFigure(id) => {
                let Some(figure) = self.lib.as_ref().and_then(|lib| lib.gallery.figure(&id)) else { return };
                let name = if figure.headline().is_empty() { t("这张图片").to_string() } else { format!("「{}」", figure.headline().chars().take(40).collect::<String>()) };
                self.modal_subscriptions.clear();
                self.modal_error.clear();
                self.modal = Some(Modal::Confirm { title: tf!("把{name}移出图片库？", "Remove {name} from figures?"), message: t("它的图注、标签和备注会一起删除。图片文件不会被删除，会移到文献库文件夹下的 removed 文件夹。PDF 和文献不动。").into(), action: t("移出图片库").into(), cmd: Cmd::RemoveFigure(id) });
            }
            Cmd::RemoveFigure(id) => {
                self.run(Cmd::CloseModal, window, cx);
                if !self.change(cx, |lib| lib.store.remove_figure(&id)) { return }
                if let Some(lib) = self.lib.as_mut() {
                    lib.gallery.recent.retain(|other| *other != id);
                    lib.gallery.broken.remove(&id);
                    if let Some(image) = lib.gallery.thumbs.remove(&id) { cx.drop_image(image, Some(window)) }
                }
                self.say(t("已移出图片库。"), cx);
            }
            Cmd::FigureInfer(ask) => self.infer_figure(ask, cx),
            Cmd::DrawFigure => self.draw_dialog(cx),
            Cmd::FigureDraw => self.draw_figure(cx),
            Cmd::FigureShape(shape) => {
                if let Some(Modal::Figure(form)) = self.modal.as_mut() && let Some(draw) = form.draw.as_mut() { draw.shape = shape }
            }
            Cmd::FigureAddTag(tag) => {
                let Some(Modal::Figure(form)) = self.modal.as_ref() else { return };
                form.tags.update(cx, |input, cx| {
                    let mut tags = split_list(input.text());
                    if !tags.contains(&tag) { tags.push(tag) }
                    input.set_text(&tags.join(", "), cx);
                });
            }
            _ => {}
        }
    }

    /// A figure's picture written where the person says.
    fn save_figure_as(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_ref() else { return };
        let Some(figure) = lib.gallery.figure(id) else { return };
        let (from, name) = (lib.store.figure_path(figure), file_name(lib.paper(&figure.paper_id), figure));
        let directory = dirs::picture_dir().or_else(dirs::document_dir).or_else(dirs::home_dir).unwrap_or_else(|| self.root.clone());
        let picked = cx.prompt_for_new_path(&directory, Some(&name));
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(mut path))) = picked.await {
                if path.extension().is_none() && let Some(extension) = from.extension() { path.set_extension(extension); }
                let result = std::fs::copy(&from, &path);
                this.update(cx, |this, cx| match result {
                    Ok(_) => this.say(tf!("已保存到 {}", "Saved to {}", path.display()), cx),
                    Err(err) => this.fail(tf!("无法写入 {}：{err}", "Cannot write {}: {err}", path.display()), cx),
                })
                .ok();
            }
        })
        .detach();
    }

    // ---- the dialog

    fn figure_form(&mut self, figure: Option<&Figure>, cx: &mut Context<Self>) -> FigureForm {
        let input = |placeholder: &'static str, lines: usize, text: &str, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let input = TextInput::new(placeholder, cx);
                if lines > 1 { input.multiline(lines).with_text(text) } else { input.with_text(text) }
            })
        };
        let empty = Figure::default();
        let from = figure.unwrap_or(&empty);
        FigureForm {
            id: figure.map(|figure| figure.id.clone()),
            key: figure.map_or_else(|| new_id("N"), |figure| figure.id.clone()),
            paper_id: from.paper_id.clone(),
            resource_id: from.resource_id.clone(),
            page: from.page,
            bbox: from.bbox,
            png: None,
            label: input("例如 Fig. 2", 1, &from.label, cx),
            legend: input("图注：这张图画的是什么", 5, &from.legend, cx),
            tags: input("用逗号隔开，例如 配色, UMAP, 示意图", 1, &from.tags.join(", "), cx),
            notes: input("为什么收藏它、想借鉴什么", 3, &from.notes, cx),
            guessed: false,
            asking: HashSet::new(),
            fresh: true,
            draw: None,
        }
    }

    /// 生成图片: the dialog with nothing in it yet but the box a picture is drawn from.
    fn draw_dialog(&mut self, cx: &mut Context<Self>) {
        if self.lib.is_none() { return }
        let mut form = self.figure_form(None, cx);
        let prompt = cx.new(|cx| TextInput::new("要画什么：例如「生长素在根尖的极性运输示意图，扁平风格，白底，英文标注」", cx).multiline(3));
        form.draw = Some(Drawing { prompt, shape: Shape::default(), busy: false, run: 0, noted: String::new() });
        self.modal_subscriptions.clear();
        self.modal_error.clear();
        self.modal = Some(Modal::Figure(Box::new(form)));
        cx.notify();
    }

    /// Have the model that draws make the dialog's picture from what its box says. While one is
    /// being drawn this stops it: the answer, when it comes, is dropped.
    fn draw_figure(&mut self, cx: &mut Context<Self>) {
        let profile = self.settings.profile_for(Task::Image);
        let Some(library) = self.lib.as_ref().map(|lib| lib.info.id.clone()) else { return };
        let Some(Modal::Figure(form)) = self.modal.as_mut() else { return };
        let key = form.key.clone();
        let Some(draw) = form.draw.as_mut() else { return };
        draw.run += 1;
        if std::mem::take(&mut draw.busy) { return cx.notify() }
        let (prompt, shape, run) = (draw.prompt.read(cx).text().trim().to_string(), draw.shape, draw.run);
        let asked = match profile {
            None => Err(t("还没有生图模型。在「设置 › AI › 模型」里给一个服务添加生图模型（例如 OpenAI 的 gpt-image、Gemini 的图像模型、火山方舟的 Seedream），填好它的 API Key。").to_string()),
            Some(profile) => imagegen::request(&profile, &prompt, shape).map(|request| (request, imagegen::interface(&profile), profile.title())),
        };
        let (request, interface, model) = match asked {
            Ok(asked) => asked,
            Err(err) => {
                self.modal_error = t(&err).to_string();
                return cx.notify();
            }
        };
        draw.busy = true;
        self.modal_error.clear();
        cx.notify();
        let work = cx.background_executor().spawn(async move { wisp_net::generate_image(request, interface) });
        cx.spawn(async move |this, cx| {
            let drawn = work.await;
            this.update(cx, |this, cx| {
                if this.lib.as_ref().is_none_or(|lib| lib.info.id != library) { return }
                // Another dialog by now, or a later request, or stopped: this picture was for what is gone.
                let Some(Modal::Figure(form)) = this.modal.as_mut().filter(|modal| matches!(modal, Modal::Figure(form) if form.key == key && form.draw.as_ref().is_some_and(|draw| draw.run == run))) else { return };
                let Some(draw) = form.draw.as_mut() else { return };
                draw.busy = false;
                match drawn {
                    Err(err) => this.modal_error = tf!("没有画出来：{err}", "No picture came of it: {err}"),
                    Ok(picture) => {
                        // Said with the figure: that a model drew it, which one, and from what. The person's own notes are not written over.
                        let noted = tf!("{model} 按描述生成：{prompt}", "Drawn by {model} from: {prompt}");
                        let before = form.notes.read(cx).text().trim().to_string();
                        if before.is_empty() || before == draw.noted { form.notes.update(cx, |input, cx| input.replace_range(0..input.text().len(), &noted, cx)) }
                        draw.noted = noted;
                        let mut tags = split_list(form.tags.read(cx).text());
                        if !tags.iter().any(|tag| tag == DRAWN_TAG) {
                            tags.push(DRAWN_TAG.to_string());
                            form.tags.update(cx, |input, cx| input.replace_range(0..input.text().len(), &tags.join(", "), cx));
                        }
                        let png = Arc::new(picture);
                        form.png = Some(png.clone());
                        this.modal_error.clear();
                        this.load_large(key, move || Some(png.to_vec()), cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Show a figure of the library, to look at and to change.
    fn figure_dialog(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_ref() else { return };
        let Some(figure) = lib.gallery.figure(id).cloned() else { return };
        let path = lib.store.figure_path(&figure);
        let form = self.figure_form(Some(&figure), cx);
        let key = form.key.clone();
        self.modal_subscriptions.clear();
        self.modal_error.clear();
        self.modal = Some(Modal::Figure(Box::new(form)));
        self.load_large(key, move || std::fs::read(&path).ok(), cx);
        cx.notify();
    }

    /// A piece was cut out of the PDF on show for the figure library: the dialog, with what the page says about it.
    pub(crate) fn collect_piece(&mut self, piece: &Piece, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_ref() else { return };
        let Some(paper_id) = lib.selected.clone() else { return };
        let resource_id = lib.pdf_file(&paper_id).map(|file| file.id.clone()).unwrap_or_default();
        let caption = piece.caption.clone().unwrap_or_default();
        let figure = Figure { paper_id, resource_id, page: piece.page, bbox: Some(piece.bbox), label: caption.label, legend: caption.legend, ..Default::default() };
        let mut form = self.figure_form(Some(&figure), cx);
        let png = Arc::new(piece.png.clone());
        (form.id, form.key, form.png, form.guessed) = (None, new_id("N"), Some(png.clone()), piece.caption.is_some());
        let key = form.key.clone();
        self.modal_subscriptions.clear();
        self.modal_error.clear();
        self.modal = Some(Modal::Figure(Box::new(form)));
        self.load_large(key, move || Some(png.to_vec()), cx);
        cx.notify();
    }

    /// Read the dialog's picture off the main thread; it is shown when it is the dialog's still.
    fn load_large(&mut self, key: String, read: impl FnOnce() -> Option<Vec<u8>> + Send + 'static, cx: &mut Context<Self>) {
        let Some(library) = self.lib.as_ref().map(|lib| lib.info.id.clone()) else { return };
        let work = cx.background_executor().spawn(async move { read().and_then(|bytes| pixels(&bytes, LARGE_EDGE)) });
        cx.spawn(async move |this, cx| {
            let picture = work.await;
            this.update(cx, |this, cx| {
                let wanted = matches!(&this.modal, Some(Modal::Figure(form)) if form.key == key);
                let Some(lib) = this.lib.as_mut().filter(|lib| lib.info.id == library && wanted) else { return };
                let Some(picture) = picture else {
                    this.modal_error = t("这张图片读不出来，文件可能被移走了。").into();
                    return cx.notify();
                };
                if let Some((_, old)) = lib.gallery.large.replace((key, Arc::new(RenderImage::new(vec![image::Frame::new(picture)])))) { cx.drop_image(old, None) }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Have the model that questions are put to look at the dialog's picture and write one of its fields.
    /// It is told the paper and what is written so far; its answer goes into the field, as one step to undo.
    fn infer_figure(&mut self, ask: Ask, cx: &mut Context<Self>) {
        let profile = self.settings.profile_for_or_default(Task::Chat);
        let Some(lib) = self.lib.as_ref() else { return };
        let Some(Modal::Figure(form)) = self.modal.as_mut() else { return };
        if form.asking.contains(&ask) { return }
        if profile.missing().is_some() {
            self.modal_error = t("还没有配置模型。在「设置 › AI」里填好一个能看图的模型，它就可以看着图写图注、标签和解读。").into();
            return cx.notify();
        }
        let text = |input: &Entity<TextInput>| input.read(cx).text().trim().to_string();
        let title = lib.paper(&form.paper_id).map(|paper| paper.title.clone()).unwrap_or_default();
        let known: Vec<String> = figure::tag_counts(&lib.gallery.items).into_iter().take(40).map(|(tag, _)| tag).collect();
        let prompt = figure::ask_prompt(ask, &title, &text(&form.label), &text(&form.legend), &known);
        let (png, path) = (form.png.clone(), form.id.as_deref().and_then(|id| lib.gallery.figure(id)).map(|figure| lib.store.figure_path(figure)));
        let (key, protocol) = (form.key.clone(), profile.protocol);
        form.asking.insert(ask);
        self.modal_error.clear();
        cx.notify();
        let work = cx.background_executor().spawn(async move {
            let bytes = match (png, path) {
                (Some(png), _) => png.to_vec(),
                (None, Some(path)) => std::fs::read(&path).map_err(|err| tf!("无法读取 {}：{err}", "Cannot read {}: {err}", path.display()))?,
                (None, None) => return Err(t("没有可以给模型看的图片。").to_string()),
            };
            let picture = crate::picture::prepare(bytes)?;
            let request = ai::picture_request(&profile, figure::ASK_SYSTEM, &prompt, ai::ImageData { media_type: picture.format.mime_type().to_string(), data: ai::base64(&picture.bytes) })?;
            wisp_net::complete(request, protocol)
        });
        cx.spawn(async move |this, cx| {
            let reply = work.await;
            this.update(cx, |this, cx| {
                // Another dialog by now: the answer was for the one that is gone.
                let Some(Modal::Figure(form)) = this.modal.as_mut().filter(|modal| matches!(modal, Modal::Figure(form) if form.key == key)) else { return };
                form.asking.remove(&ask);
                let whole = |input: &Entity<TextInput>, text: &str, cx: &mut Context<Self>| input.update(cx, |input, cx| input.replace_range(0..input.text().len(), text, cx));
                let failed = match reply {
                    Err(err) => Some(tf!("模型没有答上来：{err}", "Model request failed: {err}")),
                    Ok(reply) => match ask {
                        Ask::Legend => {
                            let caption = figure::read_legend(&reply);
                            // The number read off the page, or typed, stays; the model's fills an empty one.
                            if !caption.label.is_empty() && form.label.read(cx).text().trim().is_empty() { whole(&form.label, &caption.label, cx) }
                            if !caption.legend.is_empty() { whole(&form.legend, &caption.legend, cx) }
                            caption.legend.is_empty().then(|| t("模型没有写出图注。").to_string())
                        }
                        Ask::Tags => {
                            let mut tags = split_list(form.tags.read(cx).text());
                            let offered = figure::read_tags(&reply);
                            for tag in &offered { if !tags.contains(tag) { tags.push(tag.clone()) } }
                            whole(&form.tags, &tags.join(", "), cx);
                            offered.is_empty().then(|| t("模型没有给出标签。").to_string())
                        }
                        Ask::Notes => {
                            let (before, reading) = (form.notes.read(cx).text().trim_end().to_string(), figure::read_notes(&reply));
                            // After what is there: the person's own notes are not written over.
                            if !reading.is_empty() { whole(&form.notes, &if before.is_empty() { reading.clone() } else { format!("{before}\n\n{reading}") }, cx) }
                            reading.is_empty().then(|| t("模型没有写出解读。").to_string())
                        }
                    },
                };
                if let Some(failed) = failed { this.modal_error = failed }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Before the dialogs are drawn: a figure's dialog just opened gets the keyboard, and the picture
    /// of one that is gone, however it was closed, is let go of.
    pub(crate) fn tend_figure_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = match self.modal.as_mut() {
            Some(Modal::Figure(form)) => {
                if std::mem::take(&mut form.fresh) {
                    // A picture to be drawn: what it should show. A piece just cut out: its tags are what is left to give; with no legend read, the legend.
                    let field = if let Some(draw) = form.draw.as_ref() { Some(&draw.prompt) } else if form.id.is_some() { None } else if form.legend.read(cx).text().is_empty() { Some(&form.legend) } else { Some(&form.tags) };
                    if let Some(field) = field { field.read(cx).focus(window) }
                }
                Some(form.key.clone())
            }
            _ => None,
        };
        let Some(lib) = self.lib.as_mut() else { return };
        if lib.gallery.large.as_ref().is_some_and(|(held, _)| Some(held) != key.as_ref()) && let Some((_, image)) = lib.gallery.large.take() {
            cx.drop_image(image, Some(window));
        }
    }

    pub(crate) fn submit_figure(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::Figure(form)) = self.modal.as_ref() else { return };
        let text = |input: &Entity<TextInput>| input.read(cx).text().trim().to_string();
        let (label, legend, tags, notes) = (text(&form.label), text(&form.legend), split_list(&text(&form.tags)), text(&form.notes));
        let (id, png, drawing) = (form.id.clone(), form.png.clone(), form.draw.is_some());
        if id.is_none() && png.is_none() {
            self.modal_error = t("还没有图片：先写下要画什么，点「生成」。").into();
            return cx.notify();
        }
        let mut figure = Figure { paper_id: form.paper_id.clone(), resource_id: form.resource_id.clone(), page: form.page, bbox: form.bbox, label, legend, tags, notes, ..Default::default() };
        let mut failed = String::new();
        let kept = self.change(cx, |lib| {
            let result = match (&id, &png) {
                (Some(id), _) => lib.store.get_figure(id).and_then(|held| {
                    figure = Figure { label: figure.label.clone(), legend: figure.legend.clone(), tags: figure.tags.clone(), notes: figure.notes.clone(), ..held };
                    lib.store.save_figure(&mut figure)
                }),
                (None, Some(png)) => lib.store.add_figure(&mut figure, png),
                (None, None) => Ok(()),
            };
            // Said in the dialog, which stays open with what was typed.
            if let Err(err) = &result { failed = format!("{err:#}") }
            Ok(())
        });
        if !failed.is_empty() { return self.modal_error = failed }
        if !kept { return }
        self.run(Cmd::CloseModal, window, cx);
        if drawing {
            self.say(t("已收藏到图片库。"), cx);
        } else if id.is_none() {
            // The piece is kept: the next one can be cut.
            if let Some(pdf) = self.lib.as_ref().and_then(|lib| lib.pdf.clone()) { pdf.update(cx, |pdf, cx| pdf.cut_kept(cx)) }
            self.say(t("已收藏到图片库，在左侧「收藏」里的「图片」下。"), cx);
        }
    }

    pub(crate) fn render_figure_dialog(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        let Some(Modal::Figure(form)) = self.modal.as_ref() else { return None };
        let lib = self.lib.as_ref()?;
        let viewport = window.viewport_size();
        let (width, height) = ((f32::from(viewport.width) * 0.92).min(1060.), (f32::from(viewport.height) * 0.88).min(700.));
        let stage = (width - SIDE_W, height - if form.draw.is_some() { DRAW_H } else { TOOLS_H });
        let paper = lib.paper(&form.paper_id);
        let held = form.id.as_deref().and_then(|id| lib.gallery.figure(id));
        // A model's picture, here or kept: it was cut from no paper.
        let drawn = form.paper_id.is_empty();
        let drawing = form.draw.as_ref().is_some_and(|draw| draw.busy);

        // The picture, whole, on the white of the page it came from.
        let picture = match lib.gallery.large.as_ref().filter(|(key, _)| *key == form.key) {
            Some((_, image)) => {
                let natural = image.size(0);
                // A piece of a page is drawn at the screen's density: at its own size it is as sharp as the page was.
                // A model's picture has no size of its own on a page: it is given the room there is.
                let density = if drawn { 1. } else { window.scale_factor().min(2.) };
                let (w, h) = fit((natural.width.0 as f32 / density, natural.height.0 as f32 / density), (stage.0 - 40., stage.1 - 40.));
                div().flex_none().bg(gpui::white()).border_1().border_color(Theme::line()).shadow(crate::theme::shadow_card()).when(drawing, |d| d.opacity(0.45)).child(img(image.clone()).w(px(w)).h(px(h))).into_any_element()
            }
            None if drawing => div().flex().flex_col().items_center().gap(px(10.)).text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(ui::spinner("figure-drawing", 18., Theme::accent())).child(t("正在画，大图可能要一两分钟…")).into_any_element(),
            None if form.draw.is_some() && form.png.is_none() => div().flex().flex_col().items_center().gap(px(8.)).max_w(px(320.)).text_color(Theme::text_tertiary()).child(ui::icon("image", 30., Theme::text_placeholder())).child(div().text_color(Theme::text_secondary()).child(t("在下面写下要画什么"))).child(div().text_size(text_px(12.)).line_height(text_px(18.)).text_center().child(t("比如一张机制示意图、图文摘要的草图，或者报告里的配图。画好以后可以再画，满意了再收藏。"))).into_any_element(),
            None if self.modal_error.is_empty() => ui::spinner("figure-large", 18., Theme::text_tertiary()).into_any_element(),
            None => ui::icon("image-broken", 30., Theme::text_placeholder()).into_any_element(),
        };
        let mut where_from = if drawn { t("模型生成的图片，不出自任何文献").to_string() } else { source_line(paper) };
        if form.page > 0 { where_from = tf!("第 {} 页 · {where_from}", "Page {} · {where_from}", form.page) }
        let tool = |id: &'static str, label: &'static str, cmd: Cmd, this: &Self, cx: &mut Context<Self>| ui::ghost_button(id, None, Some(label.into())).flex_none().h(text_px(26.)).text_size(text_px(12.)).on_click(this.on(cmd, cx));
        let under = div()
            .flex()
            .flex_wrap()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .min_h(px(52.))
            .py(px(8.))
            .px(px(16.))
            .border_t_1()
            .border_color(Theme::line())
            .text_size(text_px(12.))
            .text_color(Theme::text_secondary())
            .child(div().flex_1().min_w(px(140.)).truncate().child(where_from))
            .when_some(held, |d, figure| {
                let id = figure.id.clone();
                d.when(!drawn, |d| d.child(tool("figure-source", t("打开原文"), Cmd::FigureSource(id.clone()), self, cx).text_color(Theme::accent())).child(tool("figure-ask", t("用这张图提问"), Cmd::AskFigure(id.clone()), self, cx)))
                    .child(tool("figure-copy", t("复制图片"), Cmd::CopyFigure(id.clone()), self, cx))
                    .child(tool("figure-save", t("另存为…"), Cmd::SaveFigure(id), self, cx))
            });
        // 生图: what the picture should show, its shape, who draws it, and the button that has it drawn.
        let under = match form.draw.as_ref() {
            None => under,
            Some(draw) => {
                let wide = stage.0 - 32.;
                let focused = draw.prompt.focus_handle(cx).is_focused(window);
                let model = self.settings.profile_for(Task::Image);
                let shapes: Vec<(&'static str, Cmd)> = imagegen::SHAPES.iter().map(|(shape, name)| (t(name), Cmd::FigureShape(*shape))).collect();
                let go = if draw.busy {
                    ui::secondary_button("figure-draw-go", t("停止"))
                } else {
                    ui::primary_button("figure-draw-go", if form.png.is_some() { t("重新生成") } else { t("生成") })
                };
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(px(8.))
                    .h(px(DRAW_H))
                    .p(px(16.))
                    .border_t_1()
                    .border_color(Theme::line())
                    .child(div().id("figure-prompt").w(px(wide)).h(px(94.)).overflow_y_scroll().px(px(10.)).py(px(8.)).rounded(px(8.)).border_1().border_color(if focused { Theme::accent() } else { Theme::line_strong() }).bg(Theme::bg_body()).line_height(text_px(20.)).child(div().w(px(wide - 22.)).child(draw.prompt.clone())))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .w(px(wide))
                            .child(crate::settings::segmented("figure-shape", shapes, imagegen::SHAPES.iter().position(|(shape, _)| *shape == draw.shape).unwrap_or(0), self, cx))
                            .child(div().flex_1().min_w_0().truncate().text_size(text_px(12.)).text_color(Theme::text_tertiary()).child(model.map_or_else(|| t("还没有生图模型").to_string(), |profile| profile.labelled())))
                            .child(go.flex_none().on_click(self.on(Cmd::FigureDraw, cx))),
                    )
            }
        };
        let left = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(stage.0))
            .h_full()
            .rounded_l(px(14.))
            .bg(Theme::bg_sunken())
            .overflow_hidden()
            .child(div().flex().flex_1().min_h_0().items_center().justify_center().child(picture))
            .child(under);

        // The tags the library's figures already carry, to take with a click.
        let typed = split_list(form.tags.read(cx).text());
        let offered: Vec<AnyElement> = figure::tag_counts(&lib.gallery.items)
            .into_iter()
            .filter(|(tag, _)| !typed.contains(tag))
            .take(10)
            .enumerate()
            .map(|(i, (tag, _))| {
                div()
                    .id(("figure-offer", i))
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(text_px(22.))
                    .px(px(8.))
                    .rounded(px(11.))
                    .border_1()
                    .border_color(Theme::line_strong())
                    .text_size(text_px(12.))
                    .text_color(Theme::text_secondary())
                    .cursor_pointer()
                    .hover(|s| s.bg(Theme::hover()).border_color(Theme::accent()))
                    .on_click(self.on(Cmd::FigureAddTag(tag.clone()), cx))
                    .child(format!("+ {tag}"))
                    .into_any_element()
            })
            .collect();
        let wide = SIDE_W - 40.;
        // By a field's name, the sparkle that has a model write it; a spinner while it does. No words: the field says what.
        let sparkle = |id: &'static str, ask: Ask, tip: &'static str, this: &Self, cx: &mut Context<Self>| {
            let busy = form.asking.contains(&ask);
            div()
                .id(id)
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(22.))
                .rounded(px(6.))
                .when(busy, |d| d.child(ui::spinner((id, 1usize), 13., Theme::accent())))
                .when(!busy, |d| d.cursor_pointer().hover(|s| s.bg(Theme::accent_soft())).active(|s| s.bg(Theme::pressed())).child(ui::icon("sparkle", 14., Theme::accent())).on_click(this.on(Cmd::FigureInfer(ask), cx)))
                .tooltip(move |_, cx| cx.new(|_| crate::write_assist::Tip(tip.to_string())).into())
        };
        let named = |label: &'static str, sparkle: Stateful<Div>, field: Div| div().flex().flex_col().gap(px(3.)).w(px(wide)).child(div().flex().items_center().justify_between().h(text_px(22.)).child(div().text_size(text_px(12.)).text_color(Theme::text_secondary()).child(label)).child(sparkle)).child(field);
        let sparkles = [sparkle("figure-ask-legend", Ask::Legend, t("让模型看图写图注"), self, cx), sparkle("figure-ask-tags", Ask::Tags, t("让模型看图建议标签"), self, cx), sparkle("figure-ask-notes", Ask::Notes, t("让模型看图写一段解读"), self, cx)];
        let [ask_legend, ask_tags, ask_notes] = sparkles;
        // A wrapping field under percentages is measured too narrow: each gets the column's width.
        let row = |label: &'static str, input: &Entity<TextInput>| ui::form_row(label, div().w(px(wide)).child(ui::field(input, window, cx)));
        // A legend can run to half a page: its field keeps a height and scrolls, so the tags stay in view under it.
        let tall = |id: &'static str, input: &Entity<TextInput>, most: f32| {
            let focused = input.focus_handle(cx).is_focused(window);
            div().child(div().id(id).w(px(wide)).max_h(px(most)).overflow_y_scroll().px(px(10.)).py(px(8.)).rounded(px(8.)).border_1().border_color(if focused { Theme::accent() } else { Theme::line_strong() }).bg(Theme::bg_body()).line_height(text_px(20.)).child(div().w(px(wide - 22.)).child(input.clone())))
        };
        let fresh = form.id.is_none();
        let hint = if !fresh {
            None
        } else if form.draw.is_some() {
            Some(t("模型画的图会带上「AI 生成」的标签，备注里记下是哪个模型按什么描述画的。它不出自任何文献，用到稿件里时请照期刊的要求说明。"))
        } else if form.guessed {
            Some(t("图号和图注是从这一页上读出来的，收藏前看一眼，不对的地方改掉。"))
        } else {
            Some(t("这一页上没有找到挨着它的图注，可以自己填，也可以先留空。"))
        };
        let fields = div()
            .id("figure-fields")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap(px(12.))
            .px(px(20.))
            .py(px(8.))
            .when_some(hint, |d, hint| d.child(div().w(px(wide)).text_size(text_px(12.)).line_height(text_px(18.)).text_color(Theme::text_tertiary()).child(hint)))
            .child(row(t("图号"), &form.label))
            .child(named(t("图注"), ask_legend, tall("figure-legend", &form.legend, 190.)))
            .child(named(t("标签"), ask_tags, div().w(px(wide)).child(ui::field(&form.tags, window, cx))).when(!offered.is_empty(), |d| d.child(div().flex().flex_wrap().gap(px(5.)).pt(px(3.)).w(px(wide)).children(offered))))
            .child(named(t("备注"), ask_notes, tall("figure-notes", &form.notes, 130.)))
            .child(div().w(px(wide)).child(ui::error_line(&self.modal_error)));
        let footer = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .px(px(20.))
            .py(px(14.))
            .when_some(form.id.clone(), |d, id| d.child(ui::ghost_button("figure-remove", Some("bin"), Some(t("移出").into())).text_color(Theme::danger()).on_click(self.on(Cmd::AskRemoveFigure(id), cx))))
            .child(div().flex_1())
            .child(ui::secondary_button("modal-cancel", if fresh { t("取消") } else { crate::i18n::tc("dialog.close", "关闭") }).on_click(self.on(Cmd::CloseModal, cx)))
            .child(ui::primary_button("modal-ok", if fresh { tc("action.save_figure", "收藏") } else { t("保存") }).on_click(self.on(Cmd::SubmitModal, cx)));
        let right = div().flex().flex_col().flex_none().w(px(SIDE_W)).h_full().child(ui::dialog_title(if form.draw.is_some() { t("生成图片") } else if fresh { t("收藏到图片库") } else { t("图片") })).child(fields).child(footer);
        Some(ui::dialog(width).h(px(height)).flex_row().child(left).child(right))
    }

    // ---- the page

    /// A card's picture: the one read before, or its reading started (a few at a time; the rest when these are back).
    fn figure_thumb(&mut self, id: &str, cx: &mut Context<Self>) -> Thumb {
        let Some(lib) = self.lib.as_mut() else { return Thumb::Broken };
        let gallery = &mut lib.gallery;
        if let Some(image) = gallery.thumbs.get(id).cloned() {
            gallery.touch(id);
            return Thumb::Ready(image);
        }
        if gallery.broken.contains(id) { return Thumb::Broken }
        if gallery.loading.contains(id) || gallery.loading.len() >= THUMB_WORKERS { return Thumb::Loading }
        let Some(path) = gallery.figure(id).map(|figure| lib.store.figure_path(figure)) else { return Thumb::Broken };
        gallery.loading.insert(id.to_string());
        let (library, id) = (lib.info.id.clone(), id.to_string());
        let work = cx.background_executor().spawn(async move { std::fs::read(&path).ok().and_then(|bytes| pixels(&bytes, THUMB_EDGE)) });
        cx.spawn(async move |this, cx| {
            let picture = work.await;
            this.update(cx, |this, cx| {
                let Some(lib) = this.lib.as_mut().filter(|lib| lib.info.id == library) else { return };
                let gallery = &mut lib.gallery;
                gallery.loading.remove(&id);
                match picture {
                    Some(picture) => {
                        gallery.thumbs.insert(id.clone(), Arc::new(RenderImage::new(vec![image::Frame::new(picture)])));
                        gallery.touch(&id);
                        while gallery.recent.len() > KEEP_THUMBS {
                            if let Some(old) = gallery.recent.pop_front() && let Some(image) = gallery.thumbs.remove(&old) { cx.drop_image(image, None) }
                        }
                    }
                    None => {
                        gallery.broken.insert(id);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        Thumb::Loading
    }

    /// 文献 and 图片: the two things 收藏 holds.
    pub(crate) fn render_collection_switch(&self, cx: &mut Context<Self>) -> Div {
        let Some(lib) = self.lib.as_ref() else { return div() };
        let figures = lib.page == Page::Figures;
        let side = |id: &'static str, label: String, chosen: bool, cmd: Cmd, this: &Self, cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex()
                .items_center()
                .justify_center()
                .h_full()
                .px(px(12.))
                .rounded(px(7.))
                .cursor_pointer()
                .whitespace_nowrap()
                .when(chosen, |d| d.bg(Theme::segment()).shadow(crate::theme::shadow_card()))
                .when(!chosen, |d| d.text_color(Theme::text_secondary()).hover(|s| s.text_color(Theme::text())))
                .on_click(this.on(cmd, cx))
                .child(label)
        };
        div()
            .flex()
            .flex_none()
            .gap(px(2.))
            .h(text_px(34.))
            .p(px(3.))
            .rounded(px(9.))
            .bg(Theme::groove())
            .child(side("collect-papers", tf!("文献 {}", "Papers {}", lib.papers.iter().filter(|p| p.starred).count()), !figures, Cmd::SetView(View::Starred), self, cx))
            .child(side("collect-figures", tf!("图片 {}", "Figures {}", lib.gallery.items.len()), figures, Cmd::SetPage(Page::Figures), self, cx))
    }

    pub fn render_figures(&mut self, width: f32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let lib = self.lib.as_ref().unwrap();
        let gallery = &lib.gallery;
        let query = gallery.search.read(cx).text().to_string();
        let shown = Rc::new(gallery.shown(&lib.papers, &query));
        let (total, count) = (gallery.items.len(), shown.len());
        let (columns, card) = grid(width);

        let tags = figure::tag_counts(&gallery.items);
        let tag_chip = |id: ElementId, label: String, chosen: bool, cmd: Cmd, this: &Self, cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex()
                .flex_none()
                .items_center()
                .h(text_px(26.))
                .px(px(10.))
                .rounded(px(13.))
                .border_1()
                .cursor_pointer()
                .text_size(text_px(12.))
                .when(chosen, |d| d.bg(Theme::accent_soft()).border_color(Theme::accent_soft()).text_color(Theme::accent()))
                .when(!chosen, |d| d.border_color(Theme::line_strong()).text_color(Theme::text_secondary()).hover(|s| s.bg(Theme::hover())))
                .on_click(this.on(cmd, cx))
                .child(label)
        };
        let mut chips = vec![tag_chip("figure-tag-all".into(), tf!("全部 {total}", "All {total}"), gallery.tag.is_empty(), Cmd::FigureTag(gallery.tag.clone()), self, cx).into_any_element()];
        for (i, (tag, n)) in tags.iter().take(24).enumerate() {
            chips.push(tag_chip(("figure-tag", i).into(), format!("{tag} {n}"), *tag == gallery.tag, Cmd::FigureTag(tag.clone()), self, cx).into_any_element());
        }

        let body = if total == 0 {
            ui::empty_state("image", t("还没有收藏图片"), t("打开一篇文献的 PDF，点工具栏上的「截图」，在页面上框出一张图或一个表，再点「收藏到图片库」。图号和图注会从页面上读出来，标签和备注可以自己加。也可以点右上角的「生成图片」，让生图模型按描述画一张。")).into_any_element()
        } else if count == 0 {
            ui::empty_state("search", t("没有匹配的图片"), t("换一个搜索词，或者点「全部」看所有图片。")).into_any_element()
        } else {
            let rows = count.div_ceil(columns);
            let shown = shown.clone();
            uniform_list("figures", rows, cx.processor(move |this, range: Range<usize>, _, cx| this.render_figure_rows(range, &shown, columns, card, cx))).track_scroll(gallery.scroll.clone()).flex_1().into_any_element()
        };
        let papers = shown.iter().map(|&at| gallery.items[at].paper_id.as_str()).filter(|id| !id.is_empty()).collect::<HashSet<_>>().len();
        let drawn = shown.iter().filter(|&&at| gallery.items[at].drawn()).count();
        let focused = gallery.search.focus_handle(cx).is_focused(window);
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(16.))
                    .pt(px(14.))
                    .child(self.render_collection_switch(cx))
                    .child(div().flex().flex_1().items_center().gap(px(8.)).h(text_px(34.)).px(px(10.)).rounded(px(8.)).bg(Theme::bg_sunken()).border_1().border_color(if focused { Theme::accent() } else { Theme::bg_sunken() }).child(ui::icon("search", 15., Theme::text_tertiary())).child(gallery.search.clone()))
                    .child(ui::ghost_button("figure-draw", Some("sparkle"), Some(t("生成图片").into())).flex_none().h(text_px(34.)).on_click(self.on(Cmd::DrawFigure, cx))),
            )
            .child(div().id("figure-tags").flex().flex_wrap().items_center().gap(px(6.)).px(px(16.)).py(px(10.)).when(total > 0, |d| d.children(chips)))
            .child(div().flex().flex_col().flex_1().min_h_0().border_t_1().border_color(Theme::line()).pt(px(GAP)).child(body))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .h(text_px(30.))
                    .px(px(16.))
                    .border_t_1()
                    .border_color(Theme::line())
                    .text_size(text_px(12.))
                    .text_color(Theme::text_tertiary())
                    .gap(px(12.))
                    .child(div().flex_none().child(if total == 0 {
                        t("图片库是空的").to_string()
                    } else if count != total {
                        tf!("显示 {count} / {total} 张", "Showing {count} / {total} images")
                    } else if drawn == 0 {
                        tf!("共 {total} 张，来自 {papers} 篇文献", "{total} images from {papers} papers")
                    } else if papers == 0 {
                        tf!("共 {total} 张，都是模型生成的", "{total} images, all drawn by a model")
                    } else {
                        tf!("共 {total} 张，来自 {papers} 篇文献，其中 {drawn} 张是模型生成的", "{total} images from {papers} papers, {drawn} of them drawn by a model")
                    }))
                    .child(div().min_w_0().truncate().child(t("点开图片查看和修改 · 右键更多操作 · 在 PDF 工具栏点「截图」收藏新的"))),
            )
            .into_any_element()
    }

    fn render_figure_rows(&mut self, range: Range<usize>, shown: &[usize], columns: usize, card: f32, cx: &mut Context<Self>) -> Vec<AnyElement> {
        range
            .map(|row| {
                let cards: Vec<AnyElement> = shown.iter().skip(row * columns).take(columns).map(|&at| self.render_figure_card(at, card, cx)).collect();
                div().flex().flex_none().gap(px(GAP)).w_full().h(px(PICTURE_H + GAP) + text_px(CARD_H - PICTURE_H)).px(px(16.)).pb(px(GAP)).children(cards).into_any_element()
            })
            .collect()
    }

    fn render_figure_card(&mut self, at: usize, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let Some(figure) = self.lib.as_ref().and_then(|lib| lib.gallery.items.get(at)).cloned() else { return div().into_any_element() };
        let thumb = self.figure_thumb(&figure.id, cx);
        let lib = self.lib.as_ref().unwrap();
        let paper = lib.paper(&figure.paper_id);
        let picture = match thumb {
            Thumb::Ready(image) => {
                let natural = image.size(0);
                let (w, h) = fit((natural.width.0 as f32, natural.height.0 as f32), (width - 18., PICTURE_H - 16.));
                img(image).w(px(w)).h(px(h)).into_any_element()
            }
            Thumb::Loading => div().into_any_element(),
            Thumb::Broken => ui::icon("image-broken", 24., Theme::text_placeholder()).into_any_element(),
        };
        // The number the paper gives it; without one, where it is from stands in its place.
        let source = if figure.drawn() { t("模型生成").to_string() } else { source_line(paper) };
        let (head, below) = if figure.label.is_empty() { (source.clone(), String::new()) } else { (figure.label.clone(), source) };
        let said = if !figure.legend.is_empty() { figure.legend.clone() } else { figure.notes.clone() };
        let tags: Vec<Div> = figure.tags.iter().take(4).map(|tag| ui::chip(tag.clone(), Theme::text_secondary(), Theme::bg_sunken()).h(text_px(20.)).text_size(text_px(11.))).collect();
        div()
            .id(SharedString::from(format!("figure-{}", figure.id)))
            .flex()
            .flex_col()
            .flex_none()
            .w(px(width))
            .h(px(PICTURE_H) + text_px(CARD_H - PICTURE_H))
            .rounded(px(10.))
            .border_1()
            .border_color(Theme::line())
            .bg(Theme::bg_body())
            .overflow_hidden()
            .cursor_pointer()
            .hover(|s| s.border_color(Theme::accent()))
            .on_click(self.on(Cmd::OpenFigure(figure.id.clone()), cx))
            .on_mouse_down(MouseButton::Right, self.on_context(MenuKind::Figure(figure.id.clone()), cx))
            .child(div().flex().flex_none().items_center().justify_center().h(px(PICTURE_H)).bg(gpui::white()).border_b_1().border_color(Theme::line()).child(picture))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .gap(px(4.))
                    .px(px(11.))
                    .py(px(9.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::MEDIUM).child(head))
                            .when(figure.page > 0, |d| d.child(div().flex_none().text_size(text_px(11.)).text_color(Theme::text_tertiary()).child(tf!("第 {} 页", "Page {}", figure.page)))),
                    )
                    .child(div().h(text_px(36.)).overflow_hidden().text_size(text_px(12.)).line_height(text_px(18.)).line_clamp(2).text_color(if said.is_empty() { Theme::text_placeholder() } else { Theme::text_secondary() }).child(if said.is_empty() { t("还没有图注").to_string() } else { said }))
                    .when(!below.is_empty(), |d| d.child(div().truncate().text_size(text_px(11.)).text_color(Theme::text_tertiary()).child(below)))
                    .child(div().flex().flex_none().gap(px(4.)).h(text_px(20.)).overflow_hidden().children(tags)),
            )
            .into_any_element()
    }

    /// What a right click on a card offers.
    pub(crate) fn figure_menu(&self, id: &str) -> (Vec<MenuItem>, Vec<usize>) {
        let item = |label: &str, cmd: Cmd| MenuItem { label: label.to_string().into(), checked: false, note: None, cmd };
        let Some(figure) = self.lib.as_ref().and_then(|lib| lib.gallery.figure(id)) else { return (vec![], vec![]) };
        let id = id.to_string();
        let mut items = vec![item(t("查看与修改…"), Cmd::OpenFigure(id.clone()))];
        // A picture a model drew leads back to no paper, and there is none to ask about with it.
        if !figure.drawn() { items.push(item(&if figure.page > 0 { tf!("打开原文第 {} 页", "Open source at page {}", figure.page) } else { t("打开所属文献").to_string() }, Cmd::FigureSource(id.clone()))) }
        let mut breaks = vec![items.len()];
        if !figure.drawn() { items.push(item(t("用这张图提问"), Cmd::AskFigure(id.clone()))) }
        items.push(item(t("复制图片"), Cmd::CopyFigure(id.clone())));
        if !figure.legend.is_empty() { items.push(item(t("复制图注"), Cmd::CopyText(figure.legend.clone()))) }
        items.push(item(t("另存为…"), Cmd::SaveFigure(id.clone())));
        breaks.push(items.len());
        items.push(item(t("移出图片库…"), Cmd::AskRemoveFigure(id)));
        (items, breaks)
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that would bring in gpui's own `test` attribute.
    use super::{CARD_MIN, GAP, file_name, fit, grid, pixels, source_line};
    use wisp_core::figure::Figure;
    use wisp_core::model::Paper;

    #[test]
    fn cards_fill_the_width() {
        // One card in a narrow column, more as the page widens; together with their gaps they take the width less its margins.
        assert_eq!(grid(200.).0, 1);
        for width in [300., 520., 760., 1100., 1600.] {
            let (columns, card) = grid(width);
            assert!(card >= CARD_MIN - 0.01 && ((card * columns as f32 + GAP * (columns - 1) as f32) - (width - 32.).max(CARD_MIN)).abs() < 0.01, "{width}");
        }
        assert_eq!((grid(760.).0, grid(1100.).0), (3, 4));
    }

    #[test]
    fn pictures_are_drawn_whole() {
        // Scaled down to the room, by the tighter side; never drawn larger than it is.
        assert_eq!(fit((1000., 500.), (200., 200.)), (200., 100.));
        assert_eq!(fit((500., 1000.), (200., 200.)), (100., 200.));
        assert_eq!(fit((80., 60.), (200., 200.)), (80., 60.));
        assert_eq!(fit((0., 60.), (200., 200.)), (0., 0.));
        // A picture's pixels come back in GPUI's order, scaled down when large.
        let mut png = vec![];
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(400, 100, image::Rgb([200, 40, 10]))).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let small = pixels(&png, 100).unwrap();
        assert_eq!((small.width(), small.height(), small.get_pixel(0, 0).0), (100, 25, [10, 40, 200, 255]));
        assert!(pixels(b"not a picture", 100).is_none());
    }

    #[test]
    fn where_a_figure_is_from() {
        let paper = Paper { title: "Root stem cell niche".into(), authors: "Zhang Wei; Li Na; Wang Fang".into(), year: Some(2021), ..Default::default() };
        assert_eq!(source_line(Some(&paper)), "Zhang Wei 等 2021 · Root stem cell niche");
        assert_eq!(source_line(Some(&Paper { title: "Untitled scan".into(), ..Default::default() })), "Untitled scan");
        assert_eq!(source_line(None), "文献已不在库里");
        let figure = Figure { label: "Fig. 2".into(), local_path: "figures/G-1.png".into(), ..Default::default() };
        assert_eq!(file_name(Some(&paper), &figure), "Zhang Wei 2021 Fig. 2.png");
        assert_eq!(file_name(None, &Figure { local_path: "figures/G-2.jpg".into(), ..Default::default() }), "figure.jpg");
        assert_eq!(file_name(None, &Figure { label: "Fig. 1: a/b".into(), local_path: "figures/G-3.png".into(), ..Default::default() }), "Fig. 1- a-b.png");
    }
}
