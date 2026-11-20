//! Slides (.pptx), written by hand the way `manuscript::docx` writes a Word
//! file: a zip of XML parts, stored without compression.
//!
//! A deck is a title and slides; a slide is a title, a few points (two
//! levels), a picture, a caption, a line saying where it is from and notes
//! for the speaker. Whoever makes the deck, a person or a model, says what
//! is on a slide and never where: `lay` decides the places. A slide with a
//! title alone opens a part of the talk; a picture stands beside the points,
//! or under them when it is wide, or alone.
//!
//! A text box of a .pptx does not shrink its words to fit, so the fitting
//! is done here: the size of the points is the largest of a few at which
//! they fit (`line_count` estimates the lines, no font being at hand), and
//! points that fit at none go on to a slide of the same title. A caption
//! longer than three lines is cut, and then stands whole in the notes.
//!
//! The title and the points are placeholders of the one layout, so that
//! PowerPoint's outline shows them and a theme chosen later reaches them;
//! every size, colour and place is written on the slide itself, so the
//! deck looks the same in PowerPoint, Keynote and LibreOffice.

use crate::manuscript::{crc32, stored_zip, xml_escape as escape};
use crate::markdown;
use anyhow::{Result, bail};

#[derive(Clone, Debug, Default)]
pub struct Deck {
    /// What the first slide says; empty for a deck without one.
    pub title: String,
    /// Under the title of the first slide: who wrote the paper, where and when.
    pub subtitle: String,
    /// `zh` or `en`; empty reads it off the titles.
    pub language: String,
    /// When it was made (ISO); empty leaves the file undated.
    pub date: String,
    pub slides: Vec<Slide>,
}

#[derive(Clone, Debug, Default)]
pub struct Slide {
    pub title: String,
    pub points: Vec<Point>,
    pub picture: Option<Picture>,
    /// Under the picture.
    pub caption: String,
    /// At the foot, small: the paper and the page this slide is from.
    pub source: String,
    /// For the speaker, not shown on the slide.
    pub notes: String,
}

/// A point of a slide. Its text may have `**bold**` and `*italic*`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Point {
    pub text: String,
    /// 0, or 1 for a point under the one before.
    pub level: usize,
}

impl Point {
    /// A point as a model writes it: a list marker in front is dropped, and
    /// a line set in (two spaces, a tab) is a point under the one before.
    pub fn read(line: &str) -> Point {
        let body = line.trim_start();
        let indent = &line[..line.len() - body.len()];
        let text = ["- ", "* ", "• ", "– ", "· "].iter().find_map(|marker| body.strip_prefix(marker)).unwrap_or(body);
        Point { text: text.trim().to_string(), level: (indent.contains('\t') || indent.len() >= 2) as usize }
    }
}

/// A picture PowerPoint can show, with the size it says it has.
#[derive(Clone, Debug, PartialEq)]
pub struct Picture {
    pub bytes: Vec<u8>,
    /// `png` or `jpg`.
    pub kind: &'static str,
    pub width: u32,
    pub height: u32,
}

impl Picture {
    pub fn read(bytes: Vec<u8>) -> Result<Picture> {
        let (kind, size) = match crate::figure::picture_kind(&bytes) {
            Some("png") => ("png", crate::figure::png_size(&bytes)),
            Some("jpg") => ("jpg", jpeg_size(&bytes)),
            Some("webp") => bail!("WebP 图片放不进幻灯片（旧版 PowerPoint 读不了），请换成 PNG 或 JPEG"),
            _ => bail!("不是 PNG 或 JPEG 图片"),
        };
        let Some((width, height)) = size.filter(|(width, height)| *width > 0 && *height > 0) else { bail!("读不出图片的尺寸") };
        Ok(Picture { bytes, kind, width, height })
    }
}

/// The width and height a JPEG says it has: those of its frame header.
fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut at = 2;
    loop {
        let (marker, length) = (*bytes.get(at + 1)?, u16::from_be_bytes([*bytes.get(at + 2)?, *bytes.get(at + 3)?]) as usize);
        if bytes[at] != 0xff { return None }
        // A start of frame, of any kind of coding; C4, C8 and CC among them are tables.
        if matches!(marker, 0xc0..=0xcf) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
            let number = |at: usize| Some(u16::from_be_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]) as u32);
            return Some((number(at + 7)?, number(at + 5)?));
        }
        at += 2 + length;
    }
}

/// What was written, and what `lay` did that the maker of the deck did not ask for.
pub struct Written {
    pub bytes: Vec<u8>,
    /// The slides of the file: the first one and those points went on to are among them.
    pub pages: usize,
    /// The deck's slides (from 1) whose points went on to another slide.
    pub continued: Vec<usize>,
    /// The deck's slides (from 1) whose caption was cut; it stands whole in the notes.
    pub clipped: Vec<usize>,
}

// ---- the places

/// A slide is 960 by 540 points (16:9); everything here is in points.
const WIDE: f32 = 960.0;
const HIGH: f32 = 540.0;
const SIDE: f32 = 56.0;
const TOP: f32 = 44.0;
/// Where what is under the title begins, and where it ends above the foot.
const BODY_TOP: f32 = 122.0;
const BODY_END: f32 = 480.0;
const FOOT: f32 = 500.0;
const GAP: f32 = 28.0;
/// A line's height in its letters' size. Chinese fonts stand taller than Latin ones: this is between.
const LINE: f32 = 1.25;
/// The sizes the points may have, the largest that fits first.
const SIZES: [f32; 5] = [24.0, 22.0, 20.0, 18.0, 16.0];
/// The size at which points that fit at none are counted out over several slides.
const SPLIT: f32 = 18.0;
const CAPTION: f32 = 12.0;
const CAPTION_LINES: usize = 3;
const SMALL: f32 = 11.0;
/// Bold letters are wider.
const BOLD: f32 = 1.06;

const INK: &str = "1F2328";
const SOFT: &str = "656D76";
const ACCENT: &str = "2F5BA8";

#[derive(Clone, Copy, Debug, PartialEq)]
struct Frame {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

struct Words {
    frame: Frame,
    size: f32,
    text: String,
}

/// A slide of the file, everything in its place.
#[derive(Default)]
struct Page<'a> {
    /// Which of the deck's slides it shows (from 0); none for the first slide, the deck's title.
    slide: Option<usize>,
    /// It goes on with the points of the slide before it.
    continues: bool,
    rule: Option<Frame>,
    title: Option<Words>,
    subtitle: Option<Words>,
    points: Option<(Frame, f32, &'a [Point])>,
    picture: Option<(Frame, &'a Picture)>,
    caption: Option<Words>,
    clipped: bool,
    source: &'a str,
    notes: String,
}

/// A letter of Chinese, Japanese or Korean or one of their marks: as wide as it is high, and a line may break after it.
fn wide(c: char) -> bool {
    matches!(c as u32, 0x1100..=0x11ff | 0x2e80..=0xa4cf | 0xac00..=0xd7af | 0xf900..=0xfaff | 0xfe30..=0xfe4f | 0xff00..=0xff60 | 0xffe0..=0xffe6 | 0x20000..=0x3ffff)
}

/// How wide a letter is in its size, about: Arial's widths by kind, rounded up.
fn advance(c: char) -> f32 {
    match c {
        c if wide(c) => 1.0,
        ' ' | 'i' | 'j' | 'l' | 'I' | 't' | 'f' | 'r' | '.' | ',' | ':' | ';' | '\'' | '|' | '!' | '(' | ')' | '[' | ']' => 0.32,
        'm' | 'w' | 'M' | 'W' | '—' | '…' | '%' | '@' => 0.9,
        'A'..='Z' => 0.7,
        _ => 0.56,
    }
}

/// How many lines a text takes at a size in a width. An estimate, no font
/// being at hand, that errs on the side of more: a word goes to the next
/// line whole, Chinese breaks after any letter.
fn line_count(text: &str, size: f32, width: f32) -> usize {
    let limit = (width / size).max(1.0);
    let mut lines = 0;
    for paragraph in text.split('\n') {
        let (mut used, mut word) = (0.0f32, 0.0f32);
        lines += 1;
        for c in paragraph.chars().chain([' ']) {
            if c != ' ' && !wide(c) {
                word += advance(c);
                continue;
            }
            if word > 0.0 {
                if used > 0.0 && used + word > limit { (lines, used) = (lines + 1, 0.0) }
                // A word longer than a line runs on over the next ones.
                lines += (word / limit) as usize;
                used += word % limit;
                word = 0.0;
            }
            // A space at a line's end takes no room.
            if c != ' ' && used + 1.0 > limit { (lines, used) = (lines + 1, 0.0) }
            used += advance(c);
        }
    }
    lines
}

/// A text without the Markdown around its words: what is measured.
fn plain(text: &str) -> String {
    text.split('\n').map(|line| markdown::inline(line).text).collect::<Vec<_>>().join("\n")
}

/// The size of a point's letters and how far its words stand in, for points of `size`.
fn level(size: f32, level: usize) -> (f32, f32) {
    let under = (size - 2.0).max(14.0);
    if level == 0 { (size, size * 0.9) } else { (under, size * 0.9 + under * 0.9) }
}

/// The room above a point that is not the first.
fn before(size: f32, level: usize) -> f32 {
    if level == 0 { size * 0.5 } else { size * 0.2 }
}

fn points_height(points: &[Point], size: f32, width: f32) -> f32 {
    points.iter().enumerate().map(|(n, point)| {
        let (letters, inset) = level(size, point.level);
        (if n == 0 { 0.0 } else { before(size, point.level) }) + line_count(&plain(&point.text), letters, width - inset) as f32 * letters * LINE
    }).sum()
}

/// The largest size at which the points fit a box.
fn fitted(points: &[Point], width: f32, height: f32) -> Option<f32> {
    SIZES.into_iter().find(|size| points_height(points, *size, width) <= height)
}

/// How many of the points stay on this slide when not all fit: those that
/// fit at a size easy to read, never none, and none without the point it stands under.
fn taken(points: &[Point], width: f32, height: f32) -> usize {
    let fit = (1..=points.len()).take_while(|n| points_height(&points[..*n], SPLIT, width) <= height).last().unwrap_or(1);
    let whole = (1..=fit).rev().find(|n| points.get(*n).is_none_or(|next| next.level == 0));
    whole.unwrap_or(fit)
}

/// The points of a slide that go in a box, their size, and how many they are.
fn placed(points: &[Point], width: f32, height: f32) -> (f32, usize) {
    match fitted(points, width, height) {
        Some(size) => (size, points.len()),
        None => {
            let count = taken(points, width, height);
            (fitted(&points[..count], width, height).unwrap_or(SIZES[SIZES.len() - 1]), count)
        }
    }
}

/// A heading's size and lines: on one line at the largest size it can be, else on two below the largest, else at the smallest.
fn heading(text: &str, sizes: &[f32], width: f32) -> (f32, usize) {
    let lines = |size: f32| line_count(&plain(text), size * BOLD, width);
    let size = sizes.iter().copied().find(|size| lines(*size) <= 1).or_else(|| sizes[1..].iter().copied().find(|size| lines(*size) <= 2)).unwrap_or(sizes[sizes.len() - 1]);
    (size, lines(size))
}

/// The largest frame of a picture's shape in a box, in the middle across and at its top.
fn fit(picture: &Picture, inside: Frame) -> Frame {
    let scale = (inside.w / picture.width as f32).min(inside.h / picture.height as f32);
    let (w, h) = (picture.width as f32 * scale, picture.height as f32 * scale);
    Frame { x: inside.x + (inside.w - w) / 2.0, y: inside.y, w, h }
}

/// A caption for a column: cut to the lines a caption may have, and whether it was.
fn caption(text: &str, width: f32) -> (String, usize, bool) {
    let text = text.trim();
    let lines = line_count(&plain(text), CAPTION, width);
    if lines <= CAPTION_LINES { return (text.to_string(), if text.is_empty() { 0 } else { lines }, false) }
    let letters: Vec<char> = text.chars().collect();
    let cut = |count: usize| format!("{}…", letters[..count].iter().collect::<String>().trim_end());
    // The most letters that still fit, found by halving.
    let (mut low, mut high) = (0, letters.len());
    while low + 1 < high {
        let middle = (low + high) / 2;
        if line_count(&plain(&cut(middle)), CAPTION, width) <= CAPTION_LINES { low = middle } else { high = middle }
    }
    (cut(low), CAPTION_LINES, true)
}

fn caption_height(lines: usize) -> f32 {
    if lines == 0 { 0.0 } else { 8.0 + lines as f32 * CAPTION * LINE }
}

/// Whether a deck is written in Chinese: said, or most of its titles' letters are.
fn chinese(deck: &Deck) -> bool {
    if !deck.language.is_empty() { return deck.language.starts_with("zh") }
    let titles: String = std::iter::once(deck.title.as_str()).chain(deck.slides.iter().map(|slide| slide.title.as_str())).collect();
    titles.chars().filter(|c| wide(*c)).count() * 3 > titles.chars().filter(|c| c.is_alphabetic()).count()
}

/// The slides of the file for a deck.
fn lay(deck: &Deck) -> Vec<Page<'_>> {
    let across = WIDE - 2.0 * SIDE;
    let mut pages = vec![];
    if !deck.title.trim().is_empty() {
        let (size, lines) = heading(deck.title.trim(), &[40.0, 34.0, 28.0], across);
        let (top, height) = (196.0, lines as f32 * size * LINE);
        let subtitle = deck.subtitle.trim();
        pages.push(Page {
            rule: Some(Frame { x: SIDE, y: top - 22.0, w: 56.0, h: 4.0 }),
            title: Some(Words { frame: Frame { x: SIDE, y: top, w: across, h: height }, size, text: deck.title.trim().to_string() }),
            subtitle: (!subtitle.is_empty()).then(|| Words { frame: Frame { x: SIDE, y: top + height + 18.0, w: across, h: line_count(subtitle, 18.0, across) as f32 * 18.0 * LINE }, size: 18.0, text: subtitle.to_string() }),
            ..Default::default()
        });
    }
    for (n, slide) in deck.slides.iter().enumerate() {
        let title = slide.title.trim();
        if slide.points.is_empty() && slide.picture.is_none() {
            // A title alone opens a part of the talk.
            if title.is_empty() { continue }
            let (size, lines) = heading(title, &[36.0, 30.0, 26.0], across);
            pages.push(Page {
                slide: Some(n),
                rule: Some(Frame { x: SIDE, y: 210.0, w: 56.0, h: 4.0 }),
                title: Some(Words { frame: Frame { x: SIDE, y: 232.0, w: across, h: lines as f32 * size * LINE }, size, text: title.to_string() }),
                source: slide.source.trim(),
                notes: slide.notes.trim().to_string(),
                ..Default::default()
            });
            continue;
        }
        let mut rest = &slide.points[..];
        loop {
            let first = rest.len() == slide.points.len();
            let mut page = Page { slide: Some(n), continues: !first, source: slide.source.trim(), ..Default::default() };
            let mut top = TOP;
            if !title.is_empty() {
                let text = if first { title.to_string() } else { format!("{title}{}", if chinese(deck) { "（续）" } else { " (cont.)" }) };
                let (size, lines) = heading(&text, &[30.0, 26.0, 22.0], across);
                let height = lines as f32 * size * LINE;
                page.title = Some(Words { frame: Frame { x: SIDE, y: TOP, w: across, h: height }, size, text });
                top = BODY_TOP.max(TOP + height + 22.0);
            }
            let body = Frame { x: SIDE, y: top, w: across, h: BODY_END - top };
            let picture = slide.picture.as_ref().filter(|_| first);
            let mut count = rest.len();
            // The column the picture stands in, and how far down its box may reach.
            let mut column = None;
            match picture {
                None => {
                    let (size, fits) = placed(rest, body.w, body.h);
                    count = fits;
                    page.points = Some((body, size, &rest[..count]));
                }
                Some(_) if rest.is_empty() => column = Some(body),
                Some(picture) => {
                    let shape = picture.width as f32 / picture.height as f32;
                    // A wide picture goes under the points, when they leave it most of the slide.
                    let above = SIZES.into_iter().find(|size| points_height(rest, *size, body.w) <= body.h * 0.42).filter(|_| shape >= 2.0);
                    if let Some(size) = above {
                        let height = points_height(rest, size, body.w);
                        page.points = Some((Frame { h: height, ..body }, size, rest));
                        column = Some(Frame { y: body.y + height + 18.0, h: body.h - height - 18.0, ..body });
                    } else {
                        // Beside them otherwise; a tall picture needs less of the width.
                        let beside = if shape < 0.85 { 320.0 } else { 452.0 };
                        let width = body.w - beside - GAP;
                        let (size, fits) = placed(rest, width, body.h);
                        count = fits;
                        page.points = Some((Frame { w: width, ..body }, size, &rest[..count]));
                        column = Some(Frame { x: body.x + width + GAP, w: beside, ..body });
                    }
                }
            }
            if let (Some(column), Some(picture)) = (column, picture) {
                let (text, lines, clipped) = caption(&slide.caption, column.w);
                let frame = fit(picture, Frame { h: column.h - caption_height(lines), ..column });
                page.picture = Some((frame, picture));
                if lines > 0 { page.caption = Some(Words { frame: Frame { x: column.x, y: frame.y + frame.h + 8.0, w: column.w, h: lines as f32 * CAPTION * LINE }, size: CAPTION, text }) }
                page.clipped = clipped;
            }
            if first {
                page.notes = slide.notes.trim().to_string();
                // A caption that was cut stands whole where the speaker reads.
                if page.clipped { page.notes = [page.notes.as_str(), slide.caption.trim()].iter().filter(|part| !part.is_empty()).copied().collect::<Vec<_>>().join("\n\n") }
            }
            pages.push(page);
            rest = &rest[count..];
            if rest.is_empty() { break }
        }
    }
    pages
}

// ---- the parts

const HEAD: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";
const SPACES: &str = "xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\"";
const RELATION: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
/// What every tree of shapes begins with: the group all of them are in.
const GROUP: &str = "<p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/><a:chOff x=\"0\" y=\"0\"/><a:chExt cx=\"0\" cy=\"0\"/></a:xfrm></p:grpSpPr>";
const COLOUR_MAP: &str = "<p:clrMap bg1=\"lt1\" tx1=\"dk1\" bg2=\"lt2\" tx2=\"dk2\" accent1=\"accent1\" accent2=\"accent2\" accent3=\"accent3\" accent4=\"accent4\" accent5=\"accent5\" accent6=\"accent6\" hlink=\"hlink\" folHlink=\"folHlink\"/>";
const NO_MARGIN: &str = "lIns=\"0\" tIns=\"0\" rIns=\"0\" bIns=\"0\"";

/// Points as the file counts lengths: 12700 to a point.
fn emu(points: f32) -> i64 {
    (points * 12700.0).round() as i64
}

fn place(frame: Frame) -> String {
    format!("<a:xfrm><a:off x=\"{}\" y=\"{}\"/><a:ext cx=\"{}\" cy=\"{}\"/></a:xfrm>", emu(frame.x), emu(frame.y), emu(frame.w), emu(frame.h))
}

/// How a text's letters look.
struct Look<'a> {
    size: f32,
    colour: &'a str,
    bold: bool,
    lang: &'a str,
}

impl Look<'_> {
    fn properties(&self, bold: bool, italic: bool) -> String {
        format!("lang=\"{}\" sz=\"{}\"{}{} dirty=\"0\"><a:solidFill><a:srgbClr val=\"{}\"/></a:solidFill>", self.lang, (self.size * 100.0) as i32, if self.bold || bold { " b=\"1\"" } else { "" }, if italic { " i=\"1\"" } else { "" }, self.colour)
    }
}

/// A text as the runs of a paragraph: its `**bold**` and `*italic*` are looks, a line break in it is one in the paragraph.
fn runs(text: &str, look: &Look) -> String {
    let mut out = String::new();
    for (n, line) in text.split('\n').enumerate() {
        if n > 0 { out.push_str("<a:br/>") }
        let rich = markdown::inline(line);
        for (range, marks) in rich.spans() {
            out.push_str(&format!("<a:r><a:rPr {}</a:rPr><a:t>{}</a:t></a:r>", look.properties(marks.bold, marks.italic), escape(&rich.text[range])));
        }
    }
    out
}

/// The shapes of a slide, numbered as they are added.
struct Shapes {
    xml: String,
    last: usize,
}

impl Shapes {
    fn id(&mut self) -> usize {
        self.last += 1;
        self.last
    }

    /// A box of text. `holder` names the placeholder of the layout it fills (`<p:ph …/>`); empty for a box of its own.
    fn text(&mut self, name: &str, holder: &str, frame: Frame, paragraphs: &str) {
        let id = self.id();
        let (kind, fitting) = if holder.is_empty() { ("<p:cNvSpPr txBox=\"1\"/><p:nvPr/>".to_string(), "") } else { (format!("<p:cNvSpPr><a:spLocks noGrp=\"1\"/></p:cNvSpPr><p:nvPr>{holder}</p:nvPr>"), "<a:normAutofit/>") };
        self.xml.push_str(&format!(
            "<p:sp><p:nvSpPr><p:cNvPr id=\"{id}\" name=\"{name} {}\"/>{kind}</p:nvSpPr><p:spPr>{}<a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom><a:noFill/></p:spPr><p:txBody><a:bodyPr wrap=\"square\" {NO_MARGIN} rtlCol=\"0\" anchor=\"t\">{fitting}</a:bodyPr><a:lstStyle/>{paragraphs}</p:txBody></p:sp>",
            id - 1,
            place(frame)
        ));
    }

    fn rule(&mut self, frame: Frame) {
        let id = self.id();
        self.xml.push_str(&format!("<p:sp><p:nvSpPr><p:cNvPr id=\"{id}\" name=\"Rule {}\"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr>{}<a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom><a:solidFill><a:srgbClr val=\"{ACCENT}\"/></a:solidFill><a:ln><a:noFill/></a:ln></p:spPr></p:sp>", id - 1, place(frame)));
    }

    fn picture(&mut self, frame: Frame, relation: &str, about: &str) {
        let id = self.id();
        self.xml.push_str(&format!(
            "<p:pic><p:nvPicPr><p:cNvPr id=\"{id}\" name=\"Picture {}\" descr=\"{}\"/><p:cNvPicPr><a:picLocks noChangeAspect=\"1\"/></p:cNvPicPr><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed=\"{relation}\"/><a:stretch><a:fillRect/></a:stretch></p:blipFill><p:spPr>{}<a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></p:spPr></p:pic>",
            id - 1,
            escape(about),
            place(frame)
        ));
    }
}

/// A paragraph without a bullet, set left, in the middle or right.
fn line(align: &str, runs: &str) -> String {
    format!("<a:p><a:pPr algn=\"{align}\"><a:buNone/></a:pPr>{runs}</a:p>")
}

/// A slide's XML. `number` is the one it shows at its foot; the first slide shows none.
fn slide_xml(page: &Page, lang: &str, number: Option<usize>) -> String {
    let mut shapes = Shapes { xml: String::new(), last: 1 };
    let look = |size: f32, colour: &'static str, bold: bool| Look { size, colour, bold, lang };
    if let Some(rule) = page.rule { shapes.rule(rule) }
    if let Some(title) = &page.title { shapes.text("Title", "<p:ph type=\"title\"/>", title.frame, &line("l", &runs(&title.text, &look(title.size, INK, true)))) }
    if let Some(subtitle) = &page.subtitle { shapes.text("Subtitle", "", subtitle.frame, &line("l", &runs(&subtitle.text, &look(subtitle.size, SOFT, false)))) }
    if let Some((frame, size, points)) = page.points {
        let paragraphs: String = points.iter().enumerate().map(|(n, point)| {
            let (letters, inset) = level(size, point.level);
            let (hang, mark) = if point.level == 0 { (inset, "•") } else { (letters * 0.9, "–") };
            let above = if n == 0 { 0.0 } else { before(size, point.level) };
            format!(
                "<a:p><a:pPr marL=\"{}\" indent=\"-{}\" lvl=\"{}\" algn=\"l\"><a:lnSpc><a:spcPct val=\"100000\"/></a:lnSpc><a:spcBef><a:spcPts val=\"{}\"/></a:spcBef><a:buClr><a:srgbClr val=\"{SOFT}\"/></a:buClr><a:buFont typeface=\"Arial\"/><a:buChar char=\"{mark}\"/></a:pPr>{}</a:p>",
                emu(inset),
                emu(hang),
                point.level,
                (above * 100.0) as i32,
                runs(&point.text, &look(letters, INK, false))
            )
        }).collect();
        shapes.text("Content", "<p:ph idx=\"1\"/>", frame, &paragraphs);
    }
    if let Some((frame, _)) = page.picture { shapes.picture(frame, "rId2", &plain(page.caption.as_ref().map_or("", |caption| caption.text.as_str()))) }
    if let Some(caption) = &page.caption { shapes.text("Caption", "", caption.frame, &line("ctr", &runs(&caption.text, &look(caption.size, SOFT, false)))) }
    let foot = |x: f32, w: f32| Frame { x, y: FOOT, w, h: SMALL * LINE };
    if !page.source.is_empty() { shapes.text("Source", "", foot(SIDE, WIDE - 2.0 * SIDE - 60.0), &line("l", &runs(page.source, &look(SMALL, SOFT, false)))) }
    if let Some(number) = number {
        // A field, so that the number follows the slide when the person moves it.
        let field = format!("<a:fld id=\"{{7D9F4D5A-2B6E-4E0B-9C5F-3A1B2C4D5E6F}}\" type=\"slidenum\"><a:rPr {}</a:rPr><a:t>{number}</a:t></a:fld>", look(SMALL, SOFT, false).properties(false, false));
        shapes.text("Number", "", foot(WIDE - SIDE - 48.0, 48.0), &line("r", &field));
    }
    format!("{HEAD}<p:sld {SPACES}><p:cSld><p:spTree>{GROUP}{}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>", shapes.xml)
}

/// The page of notes that goes with a slide: the slide's picture, and what the speaker reads.
fn notes_xml(notes: &str, lang: &str) -> String {
    let paragraphs: String = notes.lines().map(|line| format!("<a:p><a:r><a:rPr lang=\"{lang}\" dirty=\"0\"/><a:t>{}</a:t></a:r></a:p>", escape(line))).collect();
    format!(
        "{HEAD}<p:notes {SPACES}><p:cSld><p:spTree>{GROUP}\
<p:sp><p:nvSpPr><p:cNvPr id=\"2\" name=\"Slide Image Placeholder 1\"/><p:cNvSpPr><a:spLocks noGrp=\"1\" noRot=\"1\" noChangeAspect=\"1\"/></p:cNvSpPr><p:nvPr><p:ph type=\"sldImg\"/></p:nvPr></p:nvSpPr><p:spPr/></p:sp>\
<p:sp><p:nvSpPr><p:cNvPr id=\"3\" name=\"Notes Placeholder 2\"/><p:cNvSpPr><a:spLocks noGrp=\"1\"/></p:cNvSpPr><p:nvPr><p:ph type=\"body\" idx=\"1\"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/>{paragraphs}</p:txBody></p:sp>\
</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:notes>"
    )
}

/// The colours and fonts every part falls back on. A theme must be whole to be read: twelve colours, two fonts, three of each kind of fill, line and effect.
fn theme_xml() -> String {
    // No East Asian font is named outright, as in Office's own themes: one named and not installed is drawn as boxes
    // by some programs. PowerPoint takes the font of the text's script, every other program its own.
    let font = |name: &str| format!("<a:{name}><a:latin typeface=\"Arial\"/><a:ea typeface=\"\"/><a:cs typeface=\"\"/><a:font script=\"Hans\" typeface=\"Microsoft YaHei\"/><a:font script=\"Hant\" typeface=\"Microsoft JhengHei\"/><a:font script=\"Jpan\" typeface=\"Yu Gothic\"/></a:{name}>");
    let colours: String = [("dk1", INK), ("lt1", "FFFFFF"), ("dk2", "44546A"), ("lt2", "F3F4F6"), ("accent1", ACCENT), ("accent2", "C0504D"), ("accent3", "5B8F4E"), ("accent4", "8064A2"), ("accent5", "3D96AE"), ("accent6", "DA8137"), ("hlink", "0563C1"), ("folHlink", "954F72")]
        .iter()
        .map(|(name, colour)| format!("<a:{name}><a:srgbClr val=\"{colour}\"/></a:{name}>"))
        .collect();
    let fill = "<a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill>";
    let lines: String = [6350, 12700, 19050].iter().map(|width| format!("<a:ln w=\"{width}\" cap=\"flat\" cmpd=\"sng\" algn=\"ctr\">{fill}<a:prstDash val=\"solid\"/></a:ln>")).collect();
    format!(
        "{HEAD}<a:theme xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" name=\"Wisp Study\"><a:themeElements><a:clrScheme name=\"Wisp Study\">{colours}</a:clrScheme><a:fontScheme name=\"Wisp Study\">{}{}</a:fontScheme>\
<a:fmtScheme name=\"Wisp Study\"><a:fillStyleLst>{fill}{fill}{fill}</a:fillStyleLst><a:lnStyleLst>{lines}</a:lnStyleLst><a:effectStyleLst>{effect}{effect}{effect}</a:effectStyleLst><a:bgFillStyleLst>{fill}{fill}{fill}</a:bgFillStyleLst></a:fmtScheme></a:themeElements><a:objectDefaults/><a:extraClrSchemeLst/></a:theme>",
        font("majorFont"),
        font("minorFont"),
        effect = "<a:effectStyle><a:effectLst/></a:effectStyle>"
    )
}

/// A placeholder of a master or a layout, with the words PowerPoint shows in an empty one.
fn holder(id: usize, name: &str, kind: &str, frame: Option<Frame>, prompt: &str) -> String {
    let (shape, body) = match frame {
        Some(frame) => (format!("<p:spPr>{}<a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></p:spPr>", place(frame)), format!("<a:bodyPr vert=\"horz\" {NO_MARGIN} rtlCol=\"0\" anchor=\"t\"><a:normAutofit/></a:bodyPr>")),
        None => ("<p:spPr/>".to_string(), "<a:bodyPr/>".to_string()),
    };
    format!("<p:sp><p:nvSpPr><p:cNvPr id=\"{id}\" name=\"{name}\"/><p:cNvSpPr><a:spLocks noGrp=\"1\"/></p:cNvSpPr><p:nvPr><p:ph {kind}/></p:nvPr></p:nvSpPr>{shape}<p:txBody>{body}<a:lstStyle/><a:p><a:r><a:rPr lang=\"en-US\"/><a:t>{prompt}</a:t></a:r></a:p></p:txBody></p:sp>")
}

/// The master: where a title and points stand and how they look when a slide does not say.
fn master_xml() -> String {
    let across = WIDE - 2.0 * SIDE;
    let letters = |size: i32, bold: &str, font: &str| format!("<a:defRPr sz=\"{size}\"{bold} kern=\"1200\"><a:solidFill><a:schemeClr val=\"tx1\"/></a:solidFill><a:latin typeface=\"+{font}-lt\"/><a:ea typeface=\"+{font}-ea\"/><a:cs typeface=\"+{font}-cs\"/></a:defRPr>");
    let common = "algn=\"l\" defTabSz=\"914400\" rtl=\"0\" eaLnBrk=\"1\" latinLnBrk=\"0\" hangingPunct=\"1\"";
    let point = |name: &str, inset: f32, hang: f32, size: i32, mark: &str| format!("<a:{name} marL=\"{}\" indent=\"-{}\" {common}><a:spcBef><a:spcPts val=\"1000\"/></a:spcBef><a:buFont typeface=\"Arial\"/><a:buChar char=\"{mark}\"/>{}</a:{name}>", emu(inset), emu(hang), letters(size, "", "mn"));
    format!(
        "{HEAD}<p:sldMaster {SPACES}><p:cSld><p:bg><p:bgRef idx=\"1001\"><a:schemeClr val=\"bg1\"/></p:bgRef></p:bg><p:spTree>{GROUP}{}{}</p:spTree></p:cSld>{COLOUR_MAP}<p:sldLayoutIdLst><p:sldLayoutId id=\"2147483649\" r:id=\"rId1\"/></p:sldLayoutIdLst>\
<p:txStyles><p:titleStyle><a:lvl1pPr {common}><a:spcBef><a:spcPct val=\"0\"/></a:spcBef><a:buNone/>{}</a:lvl1pPr></p:titleStyle><p:bodyStyle>{}{}</p:bodyStyle><p:otherStyle><a:lvl1pPr marL=\"0\" {common}>{}</a:lvl1pPr></p:otherStyle></p:txStyles></p:sldMaster>",
        holder(2, "Title Placeholder 1", "type=\"title\"", Some(Frame { x: SIDE, y: TOP, w: across, h: 30.0 * LINE }), "Click to edit Master title style"),
        holder(3, "Text Placeholder 2", "type=\"body\" idx=\"1\"", Some(Frame { x: SIDE, y: BODY_TOP, w: across, h: BODY_END - BODY_TOP }), "Click to edit Master text styles"),
        letters(3000, " b=\"1\"", "mj"),
        point("lvl1pPr", 18.0, 18.0, 2000, "•"),
        point("lvl2pPr", 34.2, 16.2, 1800, "–"),
        letters(1800, "", "mn")
    )
}

/// The one layout: a title and what is under it.
fn layout_xml() -> String {
    format!(
        "{HEAD}<p:sldLayout {SPACES} type=\"obj\" preserve=\"1\"><p:cSld name=\"Title and Content\"><p:spTree>{GROUP}{}{}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>",
        holder(2, "Title 1", "type=\"title\"", None, "Click to edit Master title style"),
        holder(3, "Content Placeholder 2", "idx=\"1\"", None, "Click to edit Master text styles")
    )
}

/// The master of the pages of notes: a picture of the slide over what the speaker reads.
fn notes_master_xml() -> String {
    let picture = format!("<p:sp><p:nvSpPr><p:cNvPr id=\"2\" name=\"Slide Image Placeholder 1\"/><p:cNvSpPr><a:spLocks noGrp=\"1\" noRot=\"1\" noChangeAspect=\"1\"/></p:cNvSpPr><p:nvPr><p:ph type=\"sldImg\" idx=\"2\"/></p:nvPr></p:nvSpPr><p:spPr>{}<a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom><a:noFill/><a:ln w=\"12700\"><a:solidFill><a:prstClr val=\"black\"/></a:solidFill></a:ln></p:spPr></p:sp>", place(Frame { x: 54.0, y: 90.0, w: 432.0, h: 243.0 }));
    let words = format!("<p:sp><p:nvSpPr><p:cNvPr id=\"3\" name=\"Notes Placeholder 2\"/><p:cNvSpPr><a:spLocks noGrp=\"1\"/></p:cNvSpPr><p:nvPr><p:ph type=\"body\" sz=\"quarter\" idx=\"3\"/></p:nvPr></p:nvSpPr><p:spPr>{}<a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></p:spPr><p:txBody><a:bodyPr vert=\"horz\" lIns=\"91440\" tIns=\"45720\" rIns=\"91440\" bIns=\"45720\" rtlCol=\"0\"/><a:lstStyle/><a:p><a:r><a:rPr lang=\"en-US\"/><a:t>Click to edit Master text styles</a:t></a:r></a:p></p:txBody></p:sp>", place(Frame { x: 54.0, y: 346.5, w: 432.0, h: 283.5 }));
    format!(
        "{HEAD}<p:notesMaster {SPACES}><p:cSld><p:bg><p:bgRef idx=\"1001\"><a:schemeClr val=\"bg1\"/></p:bgRef></p:bg><p:spTree>{GROUP}{picture}{words}</p:spTree></p:cSld>{COLOUR_MAP}\
<p:notesStyle><a:lvl1pPr marL=\"0\" algn=\"l\" defTabSz=\"914400\" rtl=\"0\" eaLnBrk=\"1\" latinLnBrk=\"0\" hangingPunct=\"1\"><a:defRPr sz=\"1200\" kern=\"1200\"><a:solidFill><a:schemeClr val=\"tx1\"/></a:solidFill><a:latin typeface=\"+mn-lt\"/><a:ea typeface=\"+mn-ea\"/><a:cs typeface=\"+mn-cs\"/></a:defRPr></a:lvl1pPr></p:notesStyle></p:notesMaster>"
    )
}

/// The relations of a part: (id, kind, target).
fn relations(of: &[(String, &str, String)]) -> String {
    let all: String = of.iter().map(|(id, kind, target)| format!("<Relationship Id=\"{id}\" Type=\"{RELATION}/{kind}\" Target=\"{target}\"/>")).collect();
    format!("{HEAD}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{all}</Relationships>")
}

/// The deck as a .pptx file.
pub fn write(deck: &Deck) -> Written {
    let pages = lay(deck);
    let lang = if chinese(deck) { "zh-CN" } else { "en-US" };
    let noted = pages.iter().any(|page| !page.notes.is_empty());
    let one = |id: &str, kind: &'static str, target: String| (id.to_string(), kind, target);
    let mut files: Vec<(String, Vec<u8>)> = vec![];
    let mut add = |name: String, text: String| files.push((name, text.into_bytes()));
    let mut types = String::new();
    let mut part = |name: &str, kind: &str| types.push_str(&format!("<Override PartName=\"/{name}\" ContentType=\"application/vnd.openxmlformats-{kind}+xml\"/>"));

    // The presentation: its master, its theme, the notes' master, then the slides in their order.
    let mut listed = vec![one("rId1", "slideMaster", "slideMasters/slideMaster1.xml".into()), one("rId2", "theme", "theme/theme1.xml".into())];
    if noted { listed.push(one("rId3", "notesMaster", "notesMasters/notesMaster1.xml".into())) }
    let slide_ids: String = (0..pages.len()).map(|n| format!("<p:sldId id=\"{}\" r:id=\"rId{}\"/>", 256 + n, 4 + n)).collect();
    listed.extend((0..pages.len()).map(|n| (format!("rId{}", 4 + n), "slide", format!("slides/slide{}.xml", n + 1))));
    part("ppt/presentation.xml", "officedocument.presentationml.presentation.main");
    add(
        "ppt/presentation.xml".into(),
        format!(
            "{HEAD}<p:presentation {SPACES}><p:sldMasterIdLst><p:sldMasterId id=\"2147483648\" r:id=\"rId1\"/></p:sldMasterIdLst>{}<p:sldIdLst>{slide_ids}</p:sldIdLst><p:sldSz cx=\"{}\" cy=\"{}\"/><p:notesSz cx=\"6858000\" cy=\"9144000\"/></p:presentation>",
            if noted { "<p:notesMasterIdLst><p:notesMasterId r:id=\"rId3\"/></p:notesMasterIdLst>" } else { "" },
            emu(WIDE),
            emu(HIGH)
        ),
    );
    add("ppt/_rels/presentation.xml.rels".into(), relations(&listed));
    part("ppt/theme/theme1.xml", "officedocument.theme");
    add("ppt/theme/theme1.xml".into(), theme_xml());
    part("ppt/slideMasters/slideMaster1.xml", "officedocument.presentationml.slideMaster");
    add("ppt/slideMasters/slideMaster1.xml".into(), master_xml());
    add("ppt/slideMasters/_rels/slideMaster1.xml.rels".into(), relations(&[one("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml".into()), one("rId2", "theme", "../theme/theme1.xml".into())]));
    part("ppt/slideLayouts/slideLayout1.xml", "officedocument.presentationml.slideLayout");
    add("ppt/slideLayouts/slideLayout1.xml".into(), layout_xml());
    add("ppt/slideLayouts/_rels/slideLayout1.xml.rels".into(), relations(&[one("rId1", "slideMaster", "../slideMasters/slideMaster1.xml".into())]));
    if noted {
        // The notes' master has a theme of its own: two masters do not share one.
        part("ppt/theme/theme2.xml", "officedocument.theme");
        add("ppt/theme/theme2.xml".into(), theme_xml());
        part("ppt/notesMasters/notesMaster1.xml", "officedocument.presentationml.notesMaster");
        add("ppt/notesMasters/notesMaster1.xml".into(), notes_master_xml());
        add("ppt/notesMasters/_rels/notesMaster1.xml.rels".into(), relations(&[one("rId1", "theme", "../theme/theme2.xml".into())]));
    }

    // A picture shown on several slides is in the file once.
    let mut media: Vec<(u32, &Picture, String)> = vec![];
    for (n, page) in pages.iter().enumerate() {
        let number = n + 1;
        let mut related = vec![one("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml".into())];
        if let Some((_, picture)) = page.picture {
            let sum = crc32(&picture.bytes);
            let known = media.iter().position(|(other, held, _)| *other == sum && held.bytes == picture.bytes);
            let at = known.unwrap_or_else(|| {
                media.push((sum, picture, format!("image{}.{}", media.len() + 1, picture.kind)));
                media.len() - 1
            });
            related.push(one("rId2", "image", format!("../media/{}", media[at].2)));
        }
        if !page.notes.is_empty() {
            related.push(one("rId3", "notesSlide", format!("../notesSlides/notesSlide{number}.xml")));
            part(&format!("ppt/notesSlides/notesSlide{number}.xml"), "officedocument.presentationml.notesSlide");
            add(format!("ppt/notesSlides/notesSlide{number}.xml"), notes_xml(&page.notes, lang));
            add(format!("ppt/notesSlides/_rels/notesSlide{number}.xml.rels"), relations(&[one("rId1", "notesMaster", "../notesMasters/notesMaster1.xml".into()), one("rId2", "slide", format!("../slides/slide{number}.xml"))]));
        }
        part(&format!("ppt/slides/slide{number}.xml"), "officedocument.presentationml.slide");
        add(format!("ppt/slides/slide{number}.xml"), slide_xml(page, lang, page.slide.map(|_| number)));
        add(format!("ppt/slides/_rels/slide{number}.xml.rels"), relations(&related));
    }

    // What the file is called and when it was made, for the finder of files.
    let dated = deck.date.get(..19).map(|date| format!("<dcterms:created xsi:type=\"dcterms:W3CDTF\">{date}Z</dcterms:created><dcterms:modified xsi:type=\"dcterms:W3CDTF\">{date}Z</dcterms:modified>")).unwrap_or_default();
    part("docProps/core.xml", "package.core-properties");
    add(
        "docProps/core.xml".into(),
        format!(
            "{HEAD}<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\" xmlns:dcmitype=\"http://purl.org/dc/dcmitype/\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"><dc:title>{}</dc:title><dc:creator>Wisp Study</dc:creator>{dated}</cp:coreProperties>",
            escape(deck.title.trim())
        ),
    );
    add(
        "_rels/.rels".into(),
        format!("{HEAD}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"{RELATION}/officeDocument\" Target=\"ppt/presentation.xml\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/></Relationships>"),
    );
    let content_types = format!("{HEAD}<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Default Extension=\"png\" ContentType=\"image/png\"/><Default Extension=\"jpg\" ContentType=\"image/jpeg\"/>{types}</Types>");

    let mut all: Vec<(&str, &[u8])> = vec![("[Content_Types].xml", content_types.as_bytes())];
    all.extend(files.iter().map(|(name, bytes)| (name.as_str(), bytes.as_slice())));
    let pictures: Vec<(String, &[u8])> = media.iter().map(|(_, picture, name)| (format!("ppt/media/{name}"), picture.bytes.as_slice())).collect();
    all.extend(pictures.iter().map(|(name, bytes)| (name.as_str(), *bytes)));
    let numbers = |of: fn(&Page) -> bool| {
        let mut found: Vec<usize> = pages.iter().filter(|page| of(page)).filter_map(|page| page.slide).map(|n| n + 1).collect();
        found.dedup();
        found
    };
    Written {
        bytes: stored_zip(&all),
        pages: pages.len(),
        continued: numbers(|page| page.continues),
        clipped: numbers(|page| page.clipped),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docx::Zip;

    fn picture(width: u32, height: u32) -> Picture {
        Picture { bytes: format!("a picture of {width} by {height}").into_bytes(), kind: "png", width, height }
    }

    fn points(lines: &[&str]) -> Vec<Point> {
        lines.iter().map(|line| Point::read(line)).collect()
    }

    /// Every tag of a part closes the one opened last.
    fn well_formed(xml: &str) -> bool {
        let mut open = vec![];
        let mut rest = xml;
        while let Some(at) = rest.find('<') {
            let Some(end) = rest[at..].find('>') else { return false };
            let tag = &rest[at + 1..at + end];
            rest = &rest[at + end + 1..];
            if tag.starts_with('?') || tag.ends_with('/') { continue }
            match tag.strip_prefix('/') {
                Some(name) => if open.pop() != Some(name) { return false },
                None => open.push(tag.split(' ').next().unwrap_or(tag)),
            }
        }
        open.is_empty()
    }

    /// The values an attribute has in a part.
    fn values<'a>(xml: &'a str, attribute: &str) -> Vec<&'a str> {
        let opening = format!(" {attribute}=\"");
        xml.match_indices(&opening).map(|(at, _)| &xml[at + opening.len()..]).filter_map(|rest| rest.split('"').next()).collect()
    }

    /// The part a relation's target names, said from the part that has the relation.
    fn target(relations: &str, to: &str) -> String {
        let mut folders: Vec<&str> = relations.split('/').collect();
        folders.truncate(folders.len() - 2);
        for step in to.split('/') {
            if step == ".." { folders.pop(); } else { folders.push(step) }
        }
        folders.join("/")
    }

    fn talk() -> Deck {
        let cell = picture(1200, 900);
        Deck {
            title: "根尖干细胞微环境的单细胞图谱".into(),
            subtitle: "Zhang et al., 2021 · Plant Cell".into(),
            date: "2026-11-20T08:00:00.000Z".into(),
            slides: vec![
                Slide { title: "背景".into(), ..Default::default() },
                Slide { title: "WOX5 维持静止中心".into(), points: points(&["**WOX5** 只在静止中心表达", "  敲除后干细胞分化 & 根变短", "与 <SCR> 通路相互独立"]), source: "Zhang et al., 2021 · p. 3".into(), notes: "先讲表达，再讲敲除。\n第二段。".into(), ..Default::default() },
                Slide { title: "单细胞图谱".into(), points: points(&["共 12 个细胞群"]), picture: Some(cell.clone()), caption: "Fig. 1 | UMAP of root tip cells.".into(), ..Default::default() },
                Slide { picture: Some(cell), ..Default::default() },
                Slide { title: "Western blot".into(), picture: Some(Picture { kind: "jpg", ..picture(900, 300) }), ..Default::default() },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn a_deck_is_a_package_whose_parts_are_all_there() {
        let written = write(&talk());
        let zip = Zip::read(&written.bytes).unwrap();
        let names: Vec<&str> = zip.names().collect();
        assert_eq!((written.pages, written.continued.len(), written.clipped.len()), (6, 0, 0));
        assert_eq!(names[0], "[Content_Types].xml");
        let types = zip.text("[Content_Types].xml").unwrap();
        for name in &names {
            let text = zip.text(name).unwrap();
            if name.ends_with(".xml") || name.ends_with(".rels") { assert!(well_formed(&text), "{name}") }
            // Every part says what it is, by its name or by its ending.
            if name.starts_with("ppt/") && name.ends_with(".xml") { assert!(types.contains(&format!("PartName=\"/{name}\"")), "{name}") }
            if !name.ends_with(".rels") { continue }
            // Every relation leads to a part of the file.
            for to in values(&text, "Target") { assert!(zip.has(&target(name, to)), "{name} → {to}") }
            // And every relation a part uses is one it has.
            let part = name.replace("_rels/", "").replace(".rels", "");
            let (held, used) = (values(&text, "Id"), zip.text(&part).unwrap());
            for id in values(&used, "r:id").into_iter().chain(values(&used, "r:embed")) { assert!(held.contains(&id), "{part} uses {id}") }
        }
        // Six slides in their order; the picture shown twice is in the file once.
        let presentation = zip.text("ppt/presentation.xml").unwrap();
        assert_eq!((values(&presentation, "r:id").len(), names.iter().filter(|name| name.starts_with("ppt/media/")).count()), (1 + 1 + 6, 2));
        assert!(zip.has("ppt/media/image1.png") && zip.has("ppt/media/image2.jpg"));
        assert!(presentation.contains("<p:sldSz cx=\"12192000\" cy=\"6858000\"/>"));
        // The words are escaped, the bold ones bold, the point under another set in.
        let slide = zip.text("ppt/slides/slide3.xml").unwrap();
        assert!(slide.contains("b=\"1\" dirty=\"0\"><a:solidFill><a:srgbClr val=\"1F2328\"/></a:solidFill></a:rPr><a:t>WOX5</a:t>") && slide.contains("<a:t>敲除后干细胞分化 &amp; 根变短</a:t>") && slide.contains("<a:t>与 &lt;SCR&gt; 通路相互独立</a:t>"));
        assert!(slide.contains("lvl=\"1\"") && slide.contains("<p:ph type=\"title\"/>") && slide.contains("<p:ph idx=\"1\"/>") && slide.contains("lang=\"zh-CN\""));
        assert!(slide.contains("<a:t>Zhang et al., 2021 · p. 3</a:t>") && slide.contains("type=\"slidenum\"") && slide.contains("<a:t>3</a:t>"));
        // The first slide has the title and no number; the notes are with the slide they are for, a paragraph a line.
        let first = zip.text("ppt/slides/slide1.xml").unwrap();
        assert!(first.contains("<a:t>根尖干细胞微环境的单细胞图谱</a:t>") && first.contains("<a:t>Zhang et al., 2021 · Plant Cell</a:t>") && !first.contains("slidenum"));
        let notes = zip.text("ppt/notesSlides/notesSlide3.xml").unwrap();
        assert!(notes.contains("<a:t>先讲表达，再讲敲除。</a:t></a:r></a:p><a:p>") && notes.contains("<a:t>第二段。</a:t>"));
        assert!(zip.has("ppt/notesMasters/notesMaster1.xml") && !zip.has("ppt/notesSlides/notesSlide2.xml"));
        assert!(zip.text("docProps/core.xml").unwrap().contains("<dcterms:created xsi:type=\"dcterms:W3CDTF\">2026-11-20T08:00:00Z</dcterms:created>"));
        // A deck nobody wrote notes for has no notes' master, and one without a title no first slide.
        let plain = write(&Deck { slides: vec![Slide { title: "Only this".into(), points: points(&["one"]), ..Default::default() }], ..Default::default() });
        let zip = Zip::read(&plain.bytes).unwrap();
        assert!(plain.pages == 1 && !zip.names().any(|name| name.contains("notes")) && zip.text("ppt/slides/slide1.xml").unwrap().contains("lang=\"en-US\""));
    }

    #[test]
    fn points_that_do_not_fit_go_on_to_another_slide() {
        let long = "根尖静止中心的干细胞微环境由 WOX5 与生长素梯度共同维持，敲除任何一方都会让周围的干细胞提前分化";
        let mut many: Vec<Point> = (0..14).map(|_| Point { text: long.into(), level: 0 }).collect();
        // A few points are set large, many small, and what fits at no size is counted out over slides.
        let size = |count: usize| lay(&Deck { slides: vec![Slide { title: "要点".into(), points: many[..count].to_vec(), ..Default::default() }], ..Default::default() })[0].points.unwrap().1;
        assert_eq!((size(2), size(6), size(8)), (24.0, 20.0, 16.0));
        // The point under another is never the first of a slide.
        let cut = taken(&many, 848.0, 358.0);
        many[cut] = Point { text: long.repeat(3), level: 1 };
        let deck = Deck { slides: vec![Slide { title: "要点".into(), points: many.clone(), notes: "讲稿".into(), ..Default::default() }, Slide { title: "下一页".into(), points: points(&["一条"]), ..Default::default() }], ..Default::default() };
        let pages = lay(&deck);
        let counts: Vec<usize> = pages.iter().map(|page| page.points.unwrap().2.len()).collect();
        assert_eq!((counts[0], counts.iter().sum::<usize>(), pages.len()), (cut - 1, 15, counts.len()));
        assert!(pages.len() >= 3 && pages[1].points.unwrap().2[0].level == 0 && pages[1].points.unwrap().2[1].level == 1);
        assert_eq!((pages[0].title.as_ref().unwrap().text.as_str(), pages[1].title.as_ref().unwrap().text.as_str(), pages[1].continues), ("要点", "要点（续）", true));
        // The notes are said once, and every page of the slide fits under its title.
        assert_eq!((pages[0].notes.as_str(), pages[1].notes.as_str()), ("讲稿", ""));
        for page in &pages {
            let (frame, size, points) = page.points.unwrap();
            assert!(points_height(points, size, frame.w) <= frame.h, "{size}");
        }
        let written = write(&deck);
        assert_eq!((written.pages, written.continued), (pages.len(), vec![1]));
        // An English deck says it in English; one point too long for a slide still gets one.
        let english = Deck { slides: vec![Slide { title: "Points".into(), points: (0..30).map(|_| Point { text: "The quiescent centre keeps the surrounding stem cells from differentiating.".into(), level: 0 }).collect(), ..Default::default() }], ..Default::default() };
        assert_eq!(lay(&english)[1].title.as_ref().unwrap().text, "Points (cont.)");
        let endless = Deck { slides: vec![Slide { points: vec![Point { text: long.repeat(40), level: 0 }], ..Default::default() }], ..Default::default() };
        assert_eq!(lay(&endless).len(), 1);
    }

    #[test]
    fn everything_stays_on_the_slide_and_a_picture_keeps_its_shape() {
        let apart = |a: Frame, b: Frame| a.x + a.w <= b.x + 0.01 || b.x + b.w <= a.x + 0.01 || a.y + a.h <= b.y + 0.01 || b.y + b.h <= a.y + 0.01;
        let few = points(&["WOX5 is expressed in the quiescent centre only", "Its loss makes the stem cells differentiate", "  seen in three independent lines"]);
        let legend = "Fig. 2 | Expression of WOX5 in the root tip. ".repeat(12);
        for (width, height) in [(1600, 400), (1200, 900), (800, 800), (500, 1400), (4000, 300)] {
            for with in [&few[..], &[]] {
                for caption in ["", "Fig. 2 | WOX5 in the root tip.", legend.as_str()] {
                    let deck = Deck { slides: vec![Slide { title: "A title that is long enough to need a second line when it is set in large letters on a slide".into(), points: with.to_vec(), picture: Some(picture(width, height)), caption: caption.into(), source: "Zhang et al., 2021".into(), ..Default::default() }], ..Default::default() };
                    let pages = lay(&deck);
                    let (page, about) = (&pages[0], format!("{width}x{height}, {} points, caption of {}", with.len(), caption.len()));
                    let (frame, shown) = page.picture.unwrap();
                    let mut frames = vec![page.title.as_ref().unwrap().frame, frame];
                    frames.extend(page.points.map(|(frame, _, _)| frame));
                    frames.extend(page.caption.as_ref().map(|caption| caption.frame));
                    for (n, one) in frames.iter().enumerate() {
                        assert!(one.x >= SIDE - 0.01 && one.x + one.w <= WIDE - SIDE + 0.01 && one.y >= TOP - 0.01 && one.y + one.h <= BODY_END + 0.01 && one.w > 0.0 && one.h > 0.0, "{about}: {one:?}");
                        for other in &frames[n + 1..] { assert!(apart(*one, *other), "{about}: {one:?} over {other:?}") }
                    }
                    assert!((frame.w / frame.h - shown.width as f32 / shown.height as f32).abs() < 0.01, "{about}");
                    // A picture is not a stamp: it has a fair part of the slide whatever stands beside it.
                    assert!(frame.w * frame.h > 10_000.0, "{about}: {frame:?}");
                    assert_eq!((page.caption.is_some(), page.clipped), (!caption.is_empty(), caption.len() > 100), "{about}");
                    if page.clipped { assert!(page.caption.as_ref().unwrap().text.ends_with('…') && page.notes == caption.trim()) }
                }
            }
        }
        // A wide picture stands under the points, another beside them.
        let under = |width: u32, height: u32| {
            let deck = Deck { slides: vec![Slide { title: "T".into(), points: few.clone(), picture: Some(picture(width, height)), ..Default::default() }], ..Default::default() };
            let page = &lay(&deck)[0];
            page.picture.unwrap().0.y > page.points.unwrap().0.y + 1.0
        };
        assert_eq!((under(1600, 400), under(1200, 900), under(500, 1400)), (true, false, false));
        // The first slide and one that opens a part stay inside the slide too, however long their titles.
        let title = "A single-cell atlas of the root stem cell niche and its maintenance by auxin gradients under compacted soil ".repeat(2);
        for page in lay(&Deck { title: title.clone(), subtitle: "Zhang et al., 2021".into(), slides: vec![Slide { title, ..Default::default() }], ..Default::default() }) {
            for words in [page.title.as_ref(), page.subtitle.as_ref()].into_iter().flatten() { assert!(words.frame.y + words.frame.h <= FOOT, "{}", words.frame.y + words.frame.h) }
        }
    }

    #[test]
    fn lines_are_counted_by_the_width_of_letters() {
        // Thirty Chinese letters of 20 points are 600 points wide.
        let chinese = "根".repeat(30);
        assert_eq!((line_count(&chinese, 20.0, 600.0), line_count(&chinese, 20.0, 599.0), line_count(&chinese, 20.0, 200.0)), (1, 2, 3));
        // A word is not broken, a line break is kept, an empty text is a line.
        assert_eq!((line_count("stem cells", 20.0, 90.0), line_count("stem cells", 20.0, 130.0), line_count("a\nb", 20.0, 500.0), line_count("", 20.0, 500.0)), (2, 1, 2, 1));
        // What is measured is the words, not the Markdown around them.
        assert_eq!(plain("**WOX5** in *vivo*"), "WOX5 in vivo");
        // A long English sentence in the body's width at 20 points: about a hundred letters to a line.
        let sentence = "The quiescent centre keeps the surrounding stem cells from differentiating. ".repeat(4);
        assert_eq!(line_count(sentence.trim(), 20.0, 848.0), 4);
    }

    #[test]
    fn a_point_and_a_picture_are_read_as_given() {
        assert_eq!(points(&["- one", "  - under it", "\tunder too", "• dot", "**bold** start", "* star"]), vec![Point { text: "one".into(), level: 0 }, Point { text: "under it".into(), level: 1 }, Point { text: "under too".into(), level: 1 }, Point { text: "dot".into(), level: 0 }, Point { text: "**bold** start".into(), level: 0 }, Point { text: "star".into(), level: 0 }]);
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x02\x80\x00\x00\x01\xe0\x08\x02\x00\x00\x00".to_vec();
        let read = Picture::read(png).unwrap();
        assert_eq!((read.kind, read.width, read.height), ("png", 640, 480));
        // A JPEG says its size in its frame header, after whatever comes before it.
        let jpeg = [&b"\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00"[..], &b"\xff\xdb\x00\x04\x00\x00"[..], &b"\xff\xc2\x00\x11\x08\x01\x2c\x03\x84\x03\x01\x22\x00\x02\x11\x01\x03\x11\x01"[..]].concat();
        let read = Picture::read(jpeg.clone()).unwrap();
        assert_eq!((read.kind, read.width, read.height), ("jpg", 900, 300));
        assert!(Picture::read(jpeg[..24].to_vec()).unwrap_err().to_string().contains("尺寸"));
        assert!(Picture::read(b"RIFF\x00\x00\x00\x00WEBPVP8 ".to_vec()).unwrap_err().to_string().contains("WebP"));
        assert!(Picture::read(b"GIF89a".to_vec()).is_err());
    }
}
