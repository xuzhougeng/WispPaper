//! The text of a PDF's pages, read with hayro (the interpreter the preview
//! draws with) from where each glyph stands: glyphs become lines, lines
//! become paragraphs, and a blank line is put wherever the page shows a
//! break that plain text would lose: a larger gap, a change of size or
//! weight (a heading), a first-line indent, a jump to the next column. A
//! bold title that runs into its paragraph gets a line of its own, and the
//! raised citation numbers of a sentence are left out. `structure` reads
//! paragraphs and headings off that text.

use hayro_interpret::font::{Glyph, GlyphRun};
use hayro_interpret::hayro_cmap::BfString;
use hayro_interpret::hayro_syntax::page::Page;
use hayro_interpret::hayro_syntax::{DecryptionError, LoadPdfError, Pdf};
use hayro_interpret::kurbo::{BezPath, Point, Rect};
use hayro_interpret::{BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps, InterpreterCache, InterpreterSettings, SoftMask, TransformExt, interpret_page};

/// One glyph as the page draws it, in points from the top left.
#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    pub text: String,
    /// Where it starts and ends along the line.
    pub x0: f32,
    pub x1: f32,
    /// Its baseline.
    pub y: f32,
    /// The size of its font.
    pub size: f32,
    pub bold: bool,
}

#[derive(Default)]
struct Marks {
    marks: Vec<Mark>,
    /// Glyphs the font gives no character for.
    unreadable: usize,
}

impl<'a> Device<'a> for Marks {
    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, _: &DrawMode) {
        // A run is set in one font: its weight is read once.
        let bold = run.glyphs().iter().find_map(|glyph| match &**glyph {
            Glyph::Outline(outline) => outline.font_data().map(|font| font.weight.is_some_and(|w| w >= 600) || font.postscript_name.is_some_and(|name| ["bold", "black", "heavy", "semibold", "demi"].iter().any(|word| name.to_lowercase().contains(word)))),
            Glyph::Type3(_) => None,
        });
        for glyph in run.glyphs() {
            let text = match glyph.as_unicode() {
                // A ligature is spelled out, so a word reads the same however it was set.
                Some(BfString::Char(c)) => match c {
                    'ﬀ' => "ff".into(),
                    'ﬁ' => "fi".into(),
                    'ﬂ' => "fl".into(),
                    'ﬃ' => "ffi".into(),
                    'ﬄ' => "ffl".into(),
                    // The non-breaking hyphen some journals break words with is a hyphen to a reader.
                    '\u{2011}' => "-".into(),
                    c => c.to_string(),
                },
                Some(BfString::String(text)) => text,
                None => {
                    self.unreadable += 1;
                    continue;
                }
            };
            let advance = match &**glyph {
                Glyph::Outline(outline) => outline.advance_width(),
                Glyph::Type3(type3) => type3.advance_width(),
            };
            // Glyph space is a thousandth of the font size.
            let place = props.transform * glyph.transform();
            let (origin, right, up) = (place * Point::new(0., 0.), place * Point::new(advance.unwrap_or(500.) as f64, 0.), place * Point::new(0., 1000.));
            let size = (up - origin).hypot() as f32;
            // Text set sideways or upside down is a figure's, or a notice in the margin.
            if (right.y - origin.y).abs() > (right.x - origin.x).abs() || right.x < origin.x || !(1. ..400.).contains(&size) { continue }
            self.marks.push(Mark { text, x0: origin.x as f32, x1: right.x as f32, y: origin.y as f32, size, bold: bold.unwrap_or(false) });
        }
    }
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}
}

/// The text of every page of a PDF. An error when the file cannot be
/// parsed, or when too many of its glyphs name no character for the text
/// to be trusted.
pub fn pages(data: Vec<u8>) -> Result<Vec<String>, String> {
    let pdf = Pdf::new(data).map_err(|err| match err {
        LoadPdfError::Decryption(DecryptionError::PasswordProtected) => crate::fulltext::LOCKED.to_string(),
        LoadPdfError::Decryption(other) => format!("这个 PDF 加了密，用的方式这里解不开（{other:?}）。"),
        LoadPdfError::Invalid => "无法解析这个 PDF 的结构。".to_string(),
    })?;
    let cache = InterpreterCache::new();
    let mut out = vec![];
    let (mut read, mut unreadable) = (0, 0);
    for page in pdf.pages().iter() {
        let marks = read_marks(page, &cache);
        read += marks.marks.len();
        unreadable += marks.unreadable;
        out.push(page_text(&marks.marks));
    }
    if unreadable > read { return Err(crate::fulltext::NO_ENCODING.into()) }
    Ok(out)
}

/// The glyphs of a page as it is interpreted; none when the interpreter gives up on it.
fn read_marks<'a>(page: &Page<'a>, cache: &InterpreterCache<'a>) -> Marks {
    let (width, height) = page.render_dimensions();
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut marks = Marks::default();
        let mut context = Context::new(page.initial_transform(true).to_kurbo(), Rect::new(0., 0., width as f64, height as f64), cache, page.xref(), InterpreterSettings::default());
        interpret_page(page, &mut context, &mut marks);
        marks
    }))
    .unwrap_or_default()
}

/// The glyphs of one page in the order they are drawn, for the preview:
/// what a pointer dragged over the page selects.
pub fn page_marks<'a>(page: &Page<'a>, cache: &InterpreterCache<'a>) -> Vec<Mark> {
    read_marks(page, cache).marks
}

/// Whether a mark starts another line than the one before it stands on.
fn starts_line(last: &Mark, next: &Mark) -> bool {
    let size = last.size.max(next.size);
    // Another baseline, or back to the left: a new line.
    (next.y - last.y).abs() > size * 0.55 || next.x0 < last.x1 - size * 2.
}

/// The box a mark takes on the page: from above its capitals to below its descenders.
fn mark_box(mark: &Mark) -> [f32; 4] {
    [mark.x0, mark.y - mark.size * 0.8, mark.x1, mark.y + mark.size * 0.25]
}

/// The mark a point of the page is on, or the nearest to it: the line
/// counts for more than the place along it, so a pointer in the margin
/// takes the end of the line beside it.
pub fn mark_at(marks: &[Mark], x: f32, y: f32) -> Option<usize> {
    let away = |low: f32, high: f32, at: f32| if at < low { low - at } else if at > high { at - high } else { 0. };
    let distance = |mark: &Mark| {
        let [x0, y0, x1, y1] = mark_box(mark);
        away(y0, y1, y) * 4. + away(x0, x1, x)
    };
    (0..marks.len()).min_by(|a, b| distance(&marks[*a]).total_cmp(&distance(&marks[*b])))
}

/// One box per line for a stretch of marks: what a selection covers.
pub fn mark_boxes(marks: &[Mark]) -> Vec<[f32; 4]> {
    let mut boxes: Vec<[f32; 4]> = vec![];
    for (at, mark) in marks.iter().enumerate() {
        let own = mark_box(mark);
        match boxes.last_mut() {
            Some(line) if at > 0 && !starts_line(&marks[at - 1], mark) => *line = [line[0].min(own[0]), line[1].min(own[1]), line[2].max(own[2]), line[3].max(own[3])],
            _ => boxes.push(own),
        }
    }
    boxes
}

/// A stretch of marks as it reads: words apart where the page sets them
/// apart, a space where a line ends, and a word broken at a line's end
/// put together again.
pub fn marks_text(marks: &[Mark]) -> String {
    let mut text = String::new();
    for (at, mark) in marks.iter().enumerate() {
        if at > 0 {
            let last = &marks[at - 1];
            if starts_line(last, mark) {
                // `devel-` / `opment`: one word. `single-` / `Cell`, or a dash after a space, stays as set.
                let broken = text.ends_with('-') && text.chars().rev().nth(1).is_some_and(char::is_alphabetic) && mark.text.chars().next().is_some_and(char::is_lowercase);
                if broken { text.pop(); } else if !text.ends_with(' ') { text.push(' ') }
            } else {
                let gap = mark.x0 - last.x1;
                if (gap > last.size.max(mark.size) * 0.11 || gap < -last.size) && !text.ends_with(' ') && !mark.text.starts_with(' ') { text.push(' ') }
            }
        }
        text.push_str(&mark.text);
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A line of a page: its text, where it stands and how it is set.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Line {
    pub(crate) text: String,
    pub(crate) x0: f32,
    pub(crate) x1: f32,
    pub(crate) y: f32,
    pub(crate) size: f32,
    bold: bool,
    /// When the line opens in bold and goes on in regular: the bytes of the bold opening.
    bold_opening: usize,
}

/// Whether a raised run of small glyphs is a citation: numbers, commas and dashes only.
fn is_citation(text: &str) -> bool {
    text.chars().any(|c| c.is_ascii_digit()) && text.chars().all(|c| c.is_ascii_digit() || matches!(c, ',' | '-' | '–' | ' ' | '\u{a0}'))
}

/// The marks of one line made into its text.
fn line_of(marks: &[Mark]) -> Option<Line> {
    // The size most of the line is set in, and the baseline of that text.
    let mut sizes: Vec<(f32, usize)> = vec![];
    for mark in marks {
        match sizes.iter_mut().find(|(size, _)| (*size - mark.size).abs() < 0.3) {
            Some((_, count)) => *count += mark.text.len(),
            None => sizes.push((mark.size, mark.text.len())),
        }
    }
    let size = sizes.iter().max_by_key(|(_, count)| *count)?.0;
    let body: Vec<&Mark> = marks.iter().filter(|m| (m.size - size).abs() < 0.3).collect();
    let y = body.iter().map(|m| m.y).sum::<f32>() / body.len() as f32;
    let raised = |m: &Mark| m.size < size * 0.82 && m.y < y - size * 0.12;

    let mut text = String::new();
    let (mut bold_bytes, mut plain_bytes, mut bold_opening, mut opening_over) = (0, 0, 0, false);
    // Where the glyph before ended, a left-out citation included.
    let mut last: Option<f32> = None;
    let mut at = 0;
    while at < marks.len() {
        let mark = &marks[at];
        if raised(mark) {
            let end = (at..marks.len()).find(|i| !raised(&marks[*i])).unwrap_or(marks.len());
            let run: String = marks[at..end].iter().map(|m| m.text.as_str()).collect();
            let before = text.trim_end().chars().last();
            // After a word or a clause, raised numbers cite; after a digit they are a power.
            if is_citation(&run) && before.is_none_or(|c| !c.is_ascii_digit()) {
                last = Some(marks[end - 1].x1);
                at = end;
                continue;
            }
        }
        if let Some(last) = last {
            let gap = mark.x0 - last;
            if (gap > size * 0.11 || gap < -size) && !text.ends_with(' ') && !mark.text.starts_with(' ') { text.push(' ') }
        }
        if !mark.text.trim().is_empty() {
            if mark.bold { bold_bytes += mark.text.len() } else { plain_bytes += mark.text.len() }
            if !mark.bold { opening_over = true }
        }
        text.push_str(&mark.text);
        if !opening_over { bold_opening = text.len() }
        last = Some(mark.x1);
        at += 1;
    }
    let trimmed = text.trim_end();
    if trimmed.trim().is_empty() { return None }
    let bold_opening = if opening_over && bold_opening < trimmed.len() { bold_opening } else { 0 };
    Some(Line { text: trimmed.to_string(), x0: marks.iter().map(|m| m.x0).fold(f32::MAX, f32::min), x1: marks.iter().map(|m| m.x1).fold(f32::MIN, f32::max), y, size, bold: bold_bytes > plain_bytes * 3, bold_opening })
}

/// The marks of a page, in the order they are drawn, as its lines.
pub(crate) fn lines(marks: &[Mark]) -> Vec<Line> {
    let mut out = vec![];
    let mut start = 0;
    for at in 1..=marks.len() {
        let ends = at == marks.len() || starts_line(&marks[at - 1], &marks[at]);
        if ends {
            out.extend(line_of(&marks[start..at]));
            start = at;
        }
    }
    out
}

/// The first letter of a line, past the spaces set before it.
fn first_letter(text: &str) -> Option<char> {
    text.trim_start().chars().next()
}

/// A short line that opens with a section number (`1.2`, `2.1.3`) and a
/// word, and does not end as a sentence does.
fn numbered_title(line: &Line, size: f32) -> bool {
    let text = line.text.trim();
    let number = text.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(text.len());
    let (digits, rest) = text.split_at(number);
    let dotted = digits.matches('.').count();
    // `1.2` and `2.1.3`, not `1` alone (a list, a count) or `3.5` and a unit (`3.5 mM`) set on a line's start.
    let section = dotted >= 1 && !digits.ends_with('.') && digits.split('.').all(|part| !part.is_empty() && part.len() <= 2) && (dotted >= 2 || !digits.starts_with('0'));
    let rest = rest.trim_start_matches(|c: char| c.is_whitespace());
    // A word (`Plant`), not a unit or a symbol (`mM`, `PCR`), unless the title is in Chinese or Japanese (`CCK-8检测`).
    let starts = rest.chars().next().is_some_and(|c| capital(c) && (!c.is_ascii() || rest.chars().nth(1).is_some_and(char::is_lowercase) || rest.chars().any(|c| !c.is_ascii() && c.is_alphabetic() && !c.is_lowercase() && !c.is_uppercase())));
    section && rest.len() > 1 && starts && line.x1 - line.x0 < size * 22. && !text.ends_with(['.', ',', ';', ':', '。', '，', '；', '：', '、'])
}

/// A letter a sentence or a title may open with: a capital, or one of a script without capitals.
fn capital(c: char) -> bool {
    c.is_alphabetic() && !c.is_lowercase()
}

/// How far apart the lines of a page's text usually are, by the size they
/// are set in: Chinese and Japanese are set with more space between lines
/// (1.75 times the size and more), which is no break between paragraphs.
fn usual_leading(lines: &[Line]) -> impl Fn(f32) -> f32 {
    // (size, distance to the next line in tenths of a point, how many such pairs)
    let mut seen: Vec<(f32, i32, usize)> = vec![];
    for pair in lines.windows(2) {
        let (last, line) = (&pair[0], &pair[1]);
        let gap = line.y - last.y;
        if (line.size - last.size).abs() >= 0.3 || gap <= last.size * 0.9 || gap > last.size * 3. { continue }
        let step = (gap * 10.).round() as i32;
        match seen.iter_mut().find(|(size, at, _)| (*size - last.size).abs() < 0.3 && (*at - step).abs() <= 2) {
            Some((_, _, count)) => *count += 1,
            None => seen.push((last.size, step, 1)),
        }
    }
    move |size: f32| {
        // A spacing seen a few times at least, not a pair of lines that happen to stand apart.
        seen.iter().filter(|(at, _, count)| (*at - size).abs() < 0.3 && *count >= 4).max_by_key(|(_, _, count)| *count).map_or(0., |(_, step, _)| *step as f32 / 10.)
    }
}

/// The text of a page from its marks: one line of text per line of the
/// page, a blank line wherever the page shows a break.
pub fn page_text(marks: &[Mark]) -> String {
    let lines = lines(marks);
    let leading = usual_leading(&lines);
    let mut out = String::new();
    let mut opens = true;
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            let last = &lines[index - 1];
            let size = last.size.max(line.size);
            let breaks = line.y < last.y - size * 0.5
                || line.y - last.y > (size * 1.75).max(leading(size) * 1.25)
                || (line.size - last.size).abs() > 0.6
                || line.bold != last.bold
                || (line.x0 > last.x0 + size * 0.9 && line.x0 < last.x0 + size * 4. && last.x1 - last.x0 > size * 12.)
                || (line.x0 - last.x0).abs() > size * 5.
                // A short line that opens a paragraph and stops without a mark, before a full line: a title set like the text.
                // (A capital, or a letter of a script without capitals: `1.1 材料` / `CAL27耐药细胞株…`.)
                || (opens && last.x1 - last.x0 < (line.x1 - line.x0) * 0.75 && !last.text.ends_with(['.', ',', ';', ':', '-', '‑', '。', '，', '；', '：', '、']) && first_letter(&line.text).is_some_and(capital) && first_letter(&last.text).is_some_and(|c| capital(c) || c.is_ascii_digit()))
                // A numbered title set like the text (`1.2 方法`, `2.1 Plant material`): it stands alone, before and after.
                || numbered_title(line, size)
                || numbered_title(last, size);
            opens = breaks;
            out.push_str(if breaks { "\n\n" } else { "\n" });
        }
        // A bold title run into its paragraph: `Plant materials. Seeds were sown…`.
        let opening = &line.text[..line.bold_opening];
        if opens && opening.trim().chars().count() >= 8 && opening.split_whitespace().count() >= 2 && line.text.len() - line.bold_opening >= 20 {
            out.push_str(opening.trim());
            out.push_str("\n\n");
            out.push_str(line.text[line.bold_opening..].trim());
        } else {
            out.push_str(&line.text);
        }
    }
    out
}

/// The title a first page shows: the largest run of lines that reads like
/// one (a few words, a few lines at most), which leaves out the journal's
/// name over it and the text under it. A guess for a person to correct.
pub fn title_of(marks: &[Mark]) -> Option<String> {
    let lines = lines(marks);
    // Runs of lines set alike, one under the other.
    let mut runs: Vec<Vec<&Line>> = vec![];
    for line in &lines {
        match runs.last_mut().filter(|run| run.last().is_some_and(|last| (last.size - line.size).abs() < 0.6 && last.bold == line.bold && line.y > last.y && line.y - last.y < last.size * 2.2)) {
            Some(run) => run.push(line),
            None => runs.push(vec![line]),
        }
    }
    let reads_like_one = |run: &Vec<&Line>| {
        let text: String = run.iter().map(|line| line.text.as_str()).collect::<Vec<_>>().join(" ");
        run.len() <= 6 && text.split_whitespace().count() >= 3 && text.chars().filter(|c| c.is_alphabetic()).count() >= 20
    };
    let run = runs.iter().filter(|run| reads_like_one(run)).max_by(|a, b| a[0].size.total_cmp(&b[0].size))?;
    // Set no larger than most of the page: there is no telling it from the text.
    let larger = lines.iter().filter(|line| line.size > run[0].size - 0.6).map(|line| line.text.len()).sum::<usize>();
    if larger * 2 > lines.iter().map(|line| line.text.len()).sum::<usize>() { return None }
    let mut title = String::new();
    for line in run {
        let broken = title.ends_with('-') && title.chars().rev().nth(1).is_some_and(char::is_alphabetic) && line.text.chars().next().is_some_and(char::is_lowercase);
        if broken { title.pop(); } else if !title.is_empty() { title.push(' ') }
        title.push_str(line.text.trim());
    }
    Some(title.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// The glyphs of every page of a PDF, in the order each page draws them; none when the file cannot be read.
pub fn file_marks(data: Vec<u8>) -> Option<Vec<Vec<Mark>>> {
    std::panic::catch_unwind(|| {
        let pdf = Pdf::new(data).ok()?;
        let cache = InterpreterCache::new();
        Some(pdf.pages().iter().map(|page| read_marks(page, &cache).marks).collect())
    })
    .ok()
    .flatten()
}

/// The title on the first page of a PDF, when the page can be read and shows one.
pub fn title_guess(data: Vec<u8>) -> Option<String> {
    std::panic::catch_unwind(|| {
        let pdf = Pdf::new(data).ok()?;
        let cache = InterpreterCache::new();
        let pages = pdf.pages();
        title_of(&read_marks(pages.iter().next()?, &cache).marks)
    })
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line of text set glyph by glyph from `x`, each half its size wide.
    fn set(text: &str, x: f32, y: f32, size: f32, bold: bool) -> Vec<Mark> {
        text.chars().enumerate().map(|(i, c)| Mark { text: c.to_string(), x0: x + i as f32 * size * 0.5, x1: x + (i + 1) as f32 * size * 0.5, y, size, bold }).collect()
    }

    #[test]
    fn the_title_of_a_first_page() {
        let mut marks = set("Molecular Plant", 50., 40., 22., true);
        marks.extend(set("Research Article", 50., 70., 9., false));
        marks.extend(set("A single-cell atlas of the devel-", 50., 100., 18., true));
        marks.extend(set("oping Arabidopsis root", 50., 122., 18., true));
        marks.extend(set("Tian-Qi Zhang, Jia-Wei Wang", 50., 150., 11., false));
        for row in 0..12 {
            marks.extend(set("Roots take up water and anchor the plant in the soil it grows in.", 50., 180. + row as f32 * 12., 9., false));
        }
        assert_eq!(title_of(&marks).as_deref(), Some("A single-cell atlas of the developing Arabidopsis root"));
        // A page set in one size throughout shows no title to tell from its text.
        let plain: Vec<Mark> = (0..8).flat_map(|row| set("Roots take up water and anchor the plant.", 50., 100. + row as f32 * 12., 9., false)).collect();
        assert_eq!(title_of(&plain), None);
        assert_eq!((title_of(&[]), title_guess(b"not a pdf".to_vec())), (None, None));
    }

    #[test]
    fn a_stretch_of_a_page() {
        let mut marks = set("Roots adapt to devel-", 50., 100., 10., false);
        marks.extend(set("opment in soil", 50., 114., 10., false));
        // A word set apart by a gap and not by a space glyph.
        marks.extend(set("deeply", 150., 114., 10., false));
        assert_eq!(marks_text(&marks), "Roots adapt to development in soil deeply");
        assert_eq!(marks_text(&marks[6..11]), "adapt");
        // The pointer on a letter, between the lines, and out in the right margin of the first.
        assert_eq!(mark_at(&marks, 51., 97.), Some(0));
        assert_eq!(marks[mark_at(&marks, 76., 113.).unwrap()].text, "t");
        assert_eq!(mark_at(&marks, 400., 99.), Some(20));
        assert_eq!(mark_at(&[], 1., 1.), None);
        // One box a line, each as wide as what is taken of that line.
        let boxes = mark_boxes(&marks[15..25]);
        assert_eq!(boxes.len(), 2);
        assert_eq!((boxes[0][0], boxes[0][2], boxes[1][0], boxes[1][2]), (125., 155., 50., 70.));
        assert!(boxes[0][1] < 100. && boxes[0][3] > 100.);
    }

    #[test]
    fn lines_and_breaks() {
        let mut marks = vec![];
        marks.extend(set("Results", 50., 100., 12., true));
        marks.extend(set("Roots adapt to compacted soil in many ways", 50., 116., 10., false));
        // A raised citation after the word, then the sentence goes on.
        marks.extend(set("1,2", 260., 112., 6., false));
        marks.extend(set(". They grow.", 269., 116., 10., false));
        marks.extend(set("and they thicken as the soil gets harder", 50., 128., 10., false));
        // An indented first line opens the next paragraph.
        marks.extend(set("A second paragraph starts here.", 62., 140., 10., false));
        // The next column starts higher up.
        marks.extend(set("The other column.", 320., 100., 10., false));
        assert_eq!(page_text(&marks), "Results\n\nRoots adapt to compacted soil in many ways. They grow.\nand they thicken as the soil gets harder\n\nA second paragraph starts here.\n\nThe other column.");
    }

    #[test]
    fn chinese_set_with_wide_lines() {
        // 四川大学学报（医学版）: 9.5 point text 16.7 points apart, a numbered title set like the text.
        let mut marks = set("1 材料与方法", 308., 100., 12., false);
        marks.extend(set("\u{a0}1.1 \u{a0} \u{a0}材料", 308., 124.7, 9.5, false));
        let lines = ["CAL27耐药细胞株由四川大学华西口腔医院构建；", "南美胎牛血清（FBS-PAN）购自成都宝科生物科技有限公", "司；DMEM高糖培养基、Opti-MEM培养基购自美国", "Gibco公司；PBS缓冲液、CCK-8试剂盒及青霉素-链霉素"];
        for (row, line) in lines.iter().enumerate() {
            marks.extend(set(line, if row == 0 { 327. } else { 308. }, 141.4 + row as f32 * 16.7, 9.5, false));
        }
        marks.extend(set("1.2 方法", 308., 141.4 + 4. * 16.7, 9.5, false));
        marks.extend(set("CAL27细胞采用含10%胎牛血清的DMEM高糖培养基进行培养。", 308., 141.4 + 5. * 16.7, 9.5, false));
        let text = page_text(&marks);
        assert_eq!(text.split("\n\n").map(str::trim).collect::<Vec<_>>(), ["1 材料与方法", "1.1 \u{a0} \u{a0}材料", &lines.join("\n"), "1.2 方法", "CAL27细胞采用含10%胎牛血清的DMEM高糖培养基进行培养。"]);
        // In English a number at a line's start is not a title: `2.5 mM`, `3.5 cm`, or a line ending a sentence.
        let english = |text: &str| Line { text: text.into(), x0: 50., x1: 50. + text.len() as f32 * 5., y: 100., size: 10., bold: false, bold_opening: 0 };
        assert!(numbered_title(&english("2.1 Plant material"), 10.) && numbered_title(&english("3.2.1 Total RNA extraction"), 10.));
        for not in ["2.5 mM MgCl2 and", "1 Department of Biology", "3.5 cm long roots were cut.", "0.5 Units of enzyme"] {
            assert!(!numbered_title(&english(not), 10.), "{not}");
        }
    }

    #[test]
    fn spaces_powers_and_run_in_titles() {
        // Words set apart by a gap and no space glyph; a power stays, unlike a citation.
        let mut marks = set("about", 50., 100., 10., false);
        marks.extend(set("10", 80., 100., 10., false));
        marks.extend(set("5", 90., 96., 6., false));
        marks.extend(set("cells", 98., 100., 10., false));
        assert_eq!(page_text(&marks), "about 105 cells");
        let mut marks = set("Plant materials.", 50., 100., 10., true);
        marks.extend(set(" Seeds were sown on soil.", 130., 100., 10., false));
        assert_eq!(page_text(&marks), "Plant materials.\n\nSeeds were sown on soil.");
        assert_eq!(page_text(&[]), "");
    }
}
