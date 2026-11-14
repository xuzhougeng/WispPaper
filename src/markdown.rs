//! Draws an answer of the assistant: the blocks `wisp_core::markdown` reads
//! from its Markdown, one under the other. Lists are set tight, tables as a
//! grid of equal columns, links open in the browser. Where the answer
//! cites (`[P3 p.5 "…"]`), a click is handed to whoever asked for it.

use crate::theme::{self, Theme, text_px};
use gpui::{prelude::FluentBuilder, *};
use std::rc::Rc;
use wisp_core::ai::{self, Citation};
use wisp_core::markdown::{Align, Block, Marker, Rich, parse};

/// What a click on a citation does.
pub type OnCite = Rc<dyn Fn(&Citation, &mut Window, &mut App)>;

/// A place in an answer's text: which of its pieces (in the order `texts` gives them), and the byte in it.
pub type Place = (usize, usize);

/// What the pointer did over an answer's text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pointer {
    Down,
    /// Moved with the button held.
    Drag,
    Up,
}

/// Selecting in an answer: the stretch selected (from, to, in order), and
/// who is told where the pointer goes.
#[derive(Clone)]
pub struct Selecting {
    pub selected: Option<(Place, Place)>,
    pub pointer: Rc<dyn Fn(Pointer, Place, &mut Window, &mut App)>,
}

/// What is drawn with: an id that keeps the links of this text apart from
/// those of another, a count of the links so far, what a citation does,
/// and the layouts of the pieces drawn so far, for finding the pointer in them.
struct Draw<'a> {
    id: &'a str,
    links: usize,
    cite: Option<OnCite>,
    selected: Option<(Place, Place)>,
    layouts: Vec<TextLayout>,
}

/// `text` as elements.
pub fn render(id: &str, text: &str, cite: Option<OnCite>, selecting: Option<Selecting>) -> Div {
    let mut draw = Draw { id, links: 0, cite, selected: selecting.as_ref().and_then(|selecting| selecting.selected), layouts: vec![] };
    let drawn = div().flex().flex_col().gap(theme::paragraph_gap(10.)).children(blocks(&parse(text), &mut draw));
    let Some(selecting) = selecting else { return drawn };
    // Where the pointer is among the pieces: in the one it is over, else at the near end of the one nearest above or below.
    let layouts = Rc::new(draw.layouts);
    let place = move |position: Point<Pixels>| -> Option<Place> {
        let mut nearest: Option<(Pixels, Place)> = None;
        for (at, layout) in layouts.iter().enumerate() {
            // A piece not laid out in this frame has no place to be found in.
            let Ok(bounds) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| layout.bounds())) else { continue };
            let (away, index) = if position.y < bounds.top() {
                (bounds.top() - position.y, 0)
            } else if position.y >= bounds.bottom() {
                (position.y - bounds.bottom(), layout.len())
            } else {
                (px(0.), layout.index_for_position(position).unwrap_or_else(|index| index))
            };
            if nearest.is_none_or(|(best, _)| away < best) { nearest = Some((away, (at, index))) }
        }
        nearest.map(|(_, place)| place)
    };
    let (down, drag, up) = (selecting.pointer.clone(), selecting.pointer.clone(), selecting.pointer);
    let (place_down, place_drag, place_up) = (place.clone(), place.clone(), place);
    drawn
        .cursor_text()
        .on_mouse_down(MouseButton::Left, move |event, window, cx| {
            if let Some(place) = place_down(event.position) { down(Pointer::Down, place, window, cx) }
        })
        .on_mouse_move(move |event, window, cx| {
            if event.pressed_button == Some(MouseButton::Left) && let Some(place) = place_drag(event.position) { drag(Pointer::Drag, place, window, cx) }
        })
        .on_mouse_up(MouseButton::Left, move |event, window, cx| {
            if let Some(place) = place_up(event.position) { up(Pointer::Up, place, window, cx) }
        })
}

/// The pieces of an answer's text as they are drawn, in order: what a
/// `Place` counts. One for a paragraph, a heading, an item's own line, a
/// table cell, a block of code.
pub fn texts(text: &str) -> Vec<String> {
    fn walk(blocks: &[Block], out: &mut Vec<String>) {
        for block in blocks {
            match block {
                Block::Paragraph(text) | Block::Heading(_, text) => out.push(text.text.clone()),
                Block::Item { text, children, .. } => {
                    out.push(text.text.clone());
                    walk(children, out);
                }
                Block::Code { text, .. } => out.push(text.clone()),
                Block::Quote(inner) => walk(inner, out),
                Block::Rule => {}
                Block::Table { rows, .. } => out.extend(rows.iter().flatten().map(|cell| cell.text.clone())),
            }
        }
    }
    let mut out = vec![];
    walk(&parse(text), &mut out);
    out
}

/// The text between two places of an answer: the pieces a line apart.
pub fn text_between(text: &str, from: Place, to: Place) -> String {
    let pieces = texts(text);
    let mut out = vec![];
    for (at, piece) in pieces.iter().enumerate().take(to.0 + 1).skip(from.0) {
        let cut = |byte: usize| (0..=byte.min(piece.len())).rev().find(|byte| piece.is_char_boundary(*byte)).unwrap_or(0);
        let (start, end) = (if at == from.0 { cut(from.1) } else { 0 }, if at == to.0 { cut(to.1) } else { piece.len() });
        if start < end { out.push(piece[start..end].to_string()) }
    }
    out.join("\n")
}

/// The part of the piece now being drawn that is selected. The ends are
/// moved back to the start of a character: an answer still being written
/// may have changed under a selection made a moment ago.
fn selected_in(draw: &Draw, piece: &str) -> Option<std::ops::Range<usize>> {
    let (from, to) = draw.selected?;
    let at = draw.layouts.len();
    if at < from.0 || at > to.0 { return None }
    let whole = |byte: usize| (0..=byte.min(piece.len())).rev().find(|byte| piece.is_char_boundary(*byte)).unwrap_or(0);
    let (start, end) = (if at == from.0 { whole(from.1) } else { 0 }, if at == to.0 { whole(to.1) } else { piece.len() });
    (start < end).then_some(start..end)
}

/// Marks that do not overlap, with the selection laid over them: every
/// stretch between two edges gets the mark it stands in, and the
/// selection's ground where it is selected.
fn with_selection(marks: Vec<(std::ops::Range<usize>, HighlightStyle)>, selected: Option<std::ops::Range<usize>>) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
    let Some(selected) = selected else { return marks };
    let mut edges: Vec<usize> = marks.iter().flat_map(|(range, _)| [range.start, range.end]).chain([selected.start, selected.end]).collect();
    edges.sort();
    edges.dedup();
    let mut out = vec![];
    for pair in edges.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let mut style = marks.iter().find(|(range, _)| range.start <= start && end <= range.end).map(|(_, style)| *style).unwrap_or_default();
        if selected.start <= start && end <= selected.end { style.background_color = Some(Theme::selection()) }
        if style != HighlightStyle::default() { out.push((start..end, style)) }
    }
    out
}

fn blocks(blocks: &[Block], links: &mut Draw) -> Vec<AnyElement> {
    let mut out: Vec<AnyElement> = vec![];
    let mut at = 0;
    while at < blocks.len() {
        // The items of one list stand closer together than paragraphs do.
        let items = blocks[at..].iter().take_while(|block| matches!(block, Block::Item { .. })).count();
        if items > 0 {
            out.push(div().flex().flex_col().gap(px(4.)).children(blocks[at..at + items].iter().map(|item| block(item, links))).into_any_element());
            at += items;
        } else {
            out.push(block(&blocks[at], links));
            at += 1;
        }
    }
    out
}

fn block(block: &Block, links: &mut Draw) -> AnyElement {
    match block {
        Block::Paragraph(text) => div().line_height(theme::body_line(22.)).child(rich(text, links)).into_any_element(),
        Block::Heading(level, text) => {
            let (size, line) = match level {
                1 => (19., 28.),
                2 => (17., 26.),
                3 => (15., 24.),
                _ => (14., 22.),
            };
            div().pt(px(4.)).text_size(text_px(size)).line_height(theme::body_line(line)).font_weight(FontWeight::SEMIBOLD).child(rich(text, links)).into_any_element()
        }
        Block::Item { marker, text, children } => {
            let mark = match marker {
                Marker::Bullet => "•".to_string(),
                Marker::Number(n) => format!("{n}."),
                Marker::Todo(true) => "☑".to_string(),
                Marker::Todo(false) => "☐".to_string(),
            };
            div()
                .flex()
                .items_start()
                .gap(px(6.))
                .line_height(theme::body_line(22.))
                .child(div().flex_none().min_w(px(16.)).text_right().text_color(Theme::text_secondary()).child(mark))
                .child(div().flex().flex_col().flex_1().min_w_0().gap(px(4.)).child(rich(text, links)).children(blocks(children, links)))
                .into_any_element()
        }
        Block::Code { text, .. } => {
            let code = StyledText::new(text.clone()).with_highlights(with_selection(vec![], selected_in(links, text)));
            links.layouts.push(code.layout().clone());
            div().px(px(12.)).py(px(10.)).rounded(px(6.)).border_1().border_color(Theme::line()).bg(Theme::bg_sunken()).font_family(theme::MONO_FONT).text_size(text_px(12.5)).line_height(theme::body_line(19.)).child(code).into_any_element()
        }
        Block::Quote(inner) => div().flex().flex_col().gap(theme::paragraph_gap(8.)).pl(px(12.)).border_l_2().border_color(Theme::line_strong()).text_color(Theme::text_secondary()).children(blocks(inner, links)).into_any_element(),
        Block::Rule => div().h(px(1.)).bg(Theme::line()).into_any_element(),
        Block::Table { aligns, rows } => {
            let cells = rows.iter().enumerate().flat_map(|(r, row)| row.iter().enumerate().map(move |(c, cell)| (r, c, cell))).map(|(r, c, cell)| {
                div()
                    .min_w_0()
                    .px(px(10.))
                    .py(px(6.))
                    .text_size(text_px(13.))
                    .line_height(theme::body_line(20.))
                    .border_color(Theme::line())
                    .when(c > 0, |d| d.border_l_1())
                    .when(r > 0, |d| d.border_t_1())
                    .when(r == 0, |d| d.bg(Theme::bg_sunken()).font_weight(FontWeight::MEDIUM))
                    .map(|d| match aligns.get(c) {
                        Some(Align::Center) => d.text_center(),
                        Some(Align::Right) => d.text_right(),
                        _ => d,
                    })
                    .child(rich(cell, links))
            });
            let cells: Vec<Div> = cells.collect();
            div().grid().grid_cols(aligns.len() as u16).rounded(px(8.)).border_1().border_color(Theme::line()).overflow_hidden().children(cells).into_any_element()
        }
    }
}

/// What a click in a text leads to.
enum Target {
    Url(String),
    Cite(Citation),
}

/// Text with its marks; links in it open on a click, citations go to whoever draws the answer.
fn rich(rich: &Rich, draw: &mut Draw) -> AnyElement {
    let mut highlights = vec![];
    let (mut ranges, mut targets) = (vec![], vec![]);
    for (range, marks) in rich.spans() {
        let mut style = HighlightStyle::default();
        if marks.bold { style.font_weight = Some(FontWeight::SEMIBOLD) }
        if marks.italic { style.font_style = Some(FontStyle::Italic) }
        if marks.strike { style.strikethrough = Some(StrikethroughStyle { thickness: px(1.), color: None }) }
        if marks.code { style.background_color = Some(Theme::groove()) }
        if let Some(url) = &marks.link {
            style.color = Some(Theme::accent());
            style.underline = Some(UnderlineStyle { thickness: px(1.), color: None, wavy: false });
            ranges.push(range.clone());
            targets.push(Target::Url(url.clone()));
        }
        if style != HighlightStyle::default() { highlights.push((range, style)) }
    }
    // Where the text cites: in the accent, and a click away from the page cited.
    if draw.cite.is_some() {
        for (range, cited) in ai::citation_spans(&rich.text) {
            // Marks never overlap: a citation that runs into one (a bold word in its quotation) stays as it is.
            if highlights.iter().any(|(marked, _)| marked.start < range.end && range.start < marked.end) { continue }
            let Some(first) = cited.into_iter().next() else { continue };
            highlights.push((range.clone(), HighlightStyle { color: Some(Theme::accent()), ..Default::default() }));
            ranges.push(range);
            targets.push(Target::Cite(first));
        }
    }
    highlights.sort_by_key(|(range, _)| range.start);
    let text = StyledText::new(rich.text.clone()).with_highlights(with_selection(highlights, selected_in(draw, &rich.text)));
    draw.layouts.push(text.layout().clone());
    if targets.is_empty() { return text.into_any_element() }
    draw.links += 1;
    let cite = draw.cite.clone();
    InteractiveText::new(SharedString::from(format!("{}-link-{}", draw.id, draw.links)), text)
        .on_click(ranges, move |at, window, cx| match &targets[at] {
            Target::Url(url) => cx.open_url(url),
            Target::Cite(citation) => {
                if let Some(cite) = &cite { cite(citation, window, cx) }
            }
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that would bring in gpui's own `test` attribute.
    use super::{text_between, texts, with_selection};
    use gpui::{FontWeight, HighlightStyle};

    #[test]
    fn pieces_and_what_lies_between() {
        let answer = "# 结论\n\n根尖用了 **10x** 平台。\n\n- 第一条\n  - 里面一条\n- 第二条\n\n| 文献 | 平台 |\n|---|---|\n| P1 | 10x |\n\n```\nlet x = 1;\n```";
        assert_eq!(texts(answer), ["结论", "根尖用了 10x 平台。", "第一条", "里面一条", "第二条", "文献", "平台", "P1", "10x", "let x = 1;"]);
        // Within a piece; from the middle of one to the middle of a later one; a byte inside a character is moved back to its start.
        assert_eq!(text_between(answer, (1, 0), (1, 6)), "根尖");
        assert_eq!(text_between(answer, (1, 13), (3, 6)), "10x 平台。\n第一条\n里面");
        assert_eq!(text_between(answer, (1, 1), (1, 6)), "根尖");
        assert_eq!(text_between(answer, (9, 0), (40, 0)), "let x = 1;");
    }

    #[test]
    fn selection_over_marks() {
        let bold = HighlightStyle { font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() };
        let laid = with_selection(vec![(4..8, bold)], Some(2..6));
        assert_eq!(laid.iter().map(|(range, _)| range.clone()).collect::<Vec<_>>(), [2..4, 4..6, 6..8]);
        assert!(laid[0].1.background_color.is_some() && laid[0].1.font_weight.is_none());
        assert!(laid[1].1.background_color.is_some() && laid[1].1.font_weight.is_some());
        assert!(laid[2].1.background_color.is_none() && laid[2].1.font_weight.is_some());
        assert_eq!(with_selection(vec![(4..8, bold)], None).len(), 1);
    }
}
