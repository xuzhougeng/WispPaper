//! The text field: one line, or several with wrapping. Handles selection,
//! the clipboard and IME composition (marked text), which GPUI leaves to the
//! application.

use crate::theme::Theme;
use gpui::*;
use std::ops::Range;
use wisp_core::selection::{self, Unit};
use unicode_segmentation::UnicodeSegmentation;

actions!(text_input, [Backspace, Delete, Left, Right, SelectLeft, SelectRight, Home, End, SelectHome, SelectEnd, SelectAll, Copy, Cut, Paste, Enter, ShiftEnter, Escape, Up, Down, Complete, Undo, Redo]);

pub fn bind_keys(cx: &mut App) {
    let c = Some("TextInput");
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, c),
        KeyBinding::new("delete", Delete, c),
        KeyBinding::new("left", Left, c),
        KeyBinding::new("right", Right, c),
        KeyBinding::new("shift-left", SelectLeft, c),
        KeyBinding::new("shift-right", SelectRight, c),
        KeyBinding::new("home", Home, c),
        KeyBinding::new("end", End, c),
        KeyBinding::new("shift-home", SelectHome, c),
        KeyBinding::new("shift-end", SelectEnd, c),
        KeyBinding::new("secondary-a", SelectAll, c),
        KeyBinding::new("secondary-c", Copy, c),
        KeyBinding::new("secondary-x", Cut, c),
        KeyBinding::new("secondary-v", Paste, c),
        KeyBinding::new("enter", Enter, c),
        KeyBinding::new("shift-enter", ShiftEnter, c),
        KeyBinding::new("escape", Escape, c),
        KeyBinding::new("up", Up, c),
        KeyBinding::new("down", Down, c),
        KeyBinding::new("tab", Complete, c),
        KeyBinding::new("secondary-z", Undo, c),
        KeyBinding::new("secondary-shift-z", Redo, c),
    ]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    Changed,
    /// Enter in a single-line field, or in a multi-line one that sends on Enter.
    Submit,
    Cancel,
    /// While `choosing`: the arrow keys and Tab go to the list the owner shows, not to the caret.
    Up,
    Down,
    Tab,
    /// Pictures were pasted (a field that `accepts_images`); the owner takes them with `take_images`.
    Images,
    /// A click (not a drag) inside one of the stretches `set_marks` gave: its place in that list.
    MarkClicked(usize),
}

/// What the last paint laid out, for hit-testing and the IME.
struct Layout {
    rows: Vec<Row>,
    /// How the drawn text stands to the text.
    map: DisplayMap,
    bounds: Bounds<Pixels>,
    scroll_x: Pixels,
}

/// A paragraph as drawn: its wrapped line, where it starts in the drawn
/// text, its top, how far in from the left it is set, the height of each of
/// its lines, and what is drawn behind it.
struct Row {
    line: WrappedLine,
    start: usize,
    top: Pixels,
    x: Pixels,
    line_height: Pixels,
    decor: Decor,
}

impl Row {
    fn height(&self) -> Pixels {
        self.line.size(self.line_height).height
    }
}

/// What a paragraph of Markdown has behind its text.
#[derive(Clone, Copy, PartialEq)]
enum Decor {
    None,
    /// A bar at the left for each level.
    Quote(u8),
    /// A line across, where `---` is not shown.
    Rule,
    /// A ground, for fenced code.
    Code,
}

/// How the drawn text stands to the text: stretches of each in order, and
/// whether a stretch is drawn as written (else it is hidden, `**`, or drawn
/// as something else, a bullet). With none, the text is drawn as written.
#[derive(Default)]
struct DisplayMap {
    segs: Vec<Seg>,
}

struct Seg {
    src: Range<usize>,
    shown: Range<usize>,
    same: bool,
}

impl DisplayMap {
    /// Where byte `o` of the text is drawn.
    fn to_shown(&self, o: usize) -> usize {
        if self.segs.is_empty() { return o }
        let Some(seg) = self.segs[..self.segs.partition_point(|seg| seg.src.start <= o)].last() else { return 0 };
        if seg.same { seg.shown.start + (o - seg.src.start).min(seg.shown.len()) } else if o <= seg.src.start { seg.shown.start } else { seg.shown.end }
    }

    /// The byte of the text drawn at `d`: among stretches that meet there,
    /// one drawn as written, the first; so a click by a bold word puts the
    /// caret outside its `**`, and one at a heading's start after its `# `.
    fn to_source(&self, d: usize) -> usize {
        if self.segs.is_empty() { return d }
        let first = self.segs.partition_point(|seg| seg.shown.end < d);
        let mut at = first;
        while let Some(seg) = self.segs.get(at).filter(|seg| seg.shown.start <= d) {
            if seg.same { return seg.src.start + (d - seg.shown.start) }
            at += 1;
        }
        match self.segs.get(first).or(self.segs.last()) {
            Some(seg) if d <= seg.shown.start => seg.src.start,
            Some(seg) => seg.src.end,
            None => d,
        }
    }
}

pub struct TextInput {
    focus: FocusHandle,
    text: String,
    sel: Range<usize>,
    reversed: bool,
    marked: Option<Range<usize>>,
    placeholder: SharedString,
    /// Wrap onto more lines; Enter inserts a line break.
    pub multiline: bool,
    /// In a multi-line field, Enter submits and Shift+Enter breaks the line.
    pub submit_on_enter: bool,
    /// Draw bullets instead of the characters (API keys).
    pub masked: bool,
    /// The least height of a multi-line field, in lines.
    pub min_lines: usize,
    /// The owner is showing a list to choose from (the slash commands): Up,
    /// Down and Tab are its keys, and Escape closes it and nothing else.
    pub choosing: bool,
    /// Pasting a picture hands it to the owner instead of being ignored.
    pub accepts_images: bool,
    /// Draw the text as the Markdown in it says (headings, emphasis, lists,
    /// quotes, code), its syntax shown only on the lines being edited. The
    /// text itself stays the Markdown as written.
    pub markdown: bool,
    /// While the button is held, the lines that show their syntax stay those
    /// of the press, so the text does not move under a drag.
    reveal_held: Option<Range<usize>>,
    pasted: Vec<Image>,
    layout: Option<Layout>,
    selecting: bool,
    /// A press that selected a word, sentence or paragraph: what it selected,
    /// which a drag then grows by the same unit.
    grain: Option<(Unit, Range<usize>)>,
    scroll_x: Pixels,
    /// The text and selection before each change, to go back to; what was undone, to go forward to.
    undo: Vec<(String, Range<usize>)>,
    redo: Vec<(String, Range<usize>)>,
    /// The last change was a character typed: the next one joins its step.
    typing: bool,
    /// Stretches of the text drawn on a tint (bytes, and the tint): the words a comment is about.
    marks: Vec<(Range<usize>, Hsla)>,
    /// How far below the top of the text each mark begins, as last drawn.
    mark_tops: Vec<Pixels>,
    /// Updated during prepaint, so a selection toolbar follows wrapping and scrolling in this frame.
    selection_geometry: Option<SelectionGeometry>,
    selection_toolbar_visible: bool,
    selection_toolbar_dismissed: bool,
}

/// The most changes a field can go back through.
const UNDO_STEPS: usize = 200;

impl EventEmitter<InputEvent> for TextInput {}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

const BULLET: &str = "•";

impl TextInput {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        TextInput {
            focus: cx.focus_handle(),
            text: String::new(),
            sel: 0..0,
            reversed: false,
            marked: None,
            placeholder: placeholder.into(),
            multiline: false,
            submit_on_enter: false,
            masked: false,
            min_lines: 1,
            choosing: false,
            accepts_images: false,
            markdown: false,
            reveal_held: None,
            pasted: vec![],
            layout: None,
            selecting: false,
            grain: None,
            scroll_x: px(0.),
            undo: vec![],
            redo: vec![],
            typing: false,
            marks: vec![],
            mark_tops: vec![],
            selection_geometry: None,
            selection_toolbar_visible: false,
            selection_toolbar_dismissed: false,
        }
    }

    pub fn multiline(mut self, min_lines: usize) -> Self {
        self.multiline = true;
        self.min_lines = min_lines;
        self
    }

    /// Draw the text as Markdown (`markdown`).
    pub fn markdown(mut self) -> Self {
        self.markdown = true;
        self
    }

    pub fn with_text(mut self, text: &str) -> Self {
        self.text = if self.multiline { text.replace("\r\n", "\n") } else { text.replace(['\n', '\r'], " ") };
        self.sel = self.text.len()..self.text.len();
        self
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.selection_toolbar_dismissed = false;
        self.text = if self.multiline { text.replace("\r\n", "\n") } else { text.replace(['\n', '\r'], " ") };
        self.sel = self.text.len()..self.text.len();
        self.marked = None;
        self.scroll_x = px(0.);
        // Another text altogether (a draft of another part): its changes are not this one's.
        (self.undo, self.redo, self.typing) = (vec![], vec![], false);
        cx.notify();
    }

    /// Draw these stretches of the text on a tint. The owner gives them anew when the text or what they mark changes; nothing is redrawn for it.
    pub fn set_marks(&mut self, marks: Vec<(Range<usize>, Hsla)>) {
        self.marks = marks;
    }

    /// How far below the top of the text each stretch `set_marks` gave begins, as last drawn: one per mark,
    /// or fewer before they were drawn. A frame that moves them draws another, so the owner catches up.
    pub fn mark_tops(&self) -> &[Pixels] {
        &self.mark_tops
    }

    /// How far below the top of the text byte `at` was last drawn; none before the first frame.
    pub fn top_of(&self, at: usize) -> Option<Pixels> {
        let layout = self.layout.as_ref()?;
        Some(self.position_in(layout, self.to_display(&layout.map, at.min(self.text.len())))?.y)
    }

    /// What is selected, in bytes; empty at the caret.
    pub fn selection(&self) -> Range<usize> {
        self.sel.clone()
    }

    /// Select `range` (bytes, kept within the text).
    pub fn select_range(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        self.selection_toolbar_dismissed = false;
        let at = |o: usize| self.text.floor_char_boundary(o.min(self.text.len()));
        (self.sel, self.reversed, self.typing) = (at(range.start)..at(range.end.max(range.start)), false, false);
        cx.notify();
    }

    /// Put `text` in place of `range` as one step that Undo takes back, and select it.
    pub fn replace_range(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        let range = self.text.floor_char_boundary(range.start.min(self.text.len()))..self.text.floor_char_boundary(range.end.min(self.text.len()));
        self.typing = false;
        let start = range.start;
        self.replace(range, text, cx);
        self.typing = false;
        self.sel = start..self.sel.end;
        cx.notify();
    }

    /// Put `text` in place of `range` as one step that Undo takes back, the caret and the selection staying
    /// with the words they were at: a change made by someone else than the person at the keyboard.
    pub fn replace_apart(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        let range = self.text.floor_char_boundary(range.start.min(self.text.len()))..self.text.floor_char_boundary(range.end.min(self.text.len()));
        let (sel, reversed) = (self.sel.clone(), self.reversed);
        self.typing = false;
        self.replace(range.clone(), text, cx);
        self.typing = false;
        // `replace` left the caret after what it put in: that is how long it is as kept.
        let put = self.sel.end - range.start;
        let moved = |at: usize| if at <= range.start { at } else if at >= range.end { at - range.len() + put } else { range.start + put };
        (self.sel, self.reversed) = (moved(sel.start)..moved(sel.end), reversed);
        cx.notify();
    }

    /// The pictures pasted since this was last asked.
    pub fn take_images(&mut self) -> Vec<Image> {
        std::mem::take(&mut self.pasted)
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus);
    }

    /// What is selected; empty when nothing is.
    pub fn selected(&self) -> &str {
        &self.text[self.sel.clone()]
    }

    /// Where the caret stands, in bytes from the start of the text.
    pub fn caret(&self) -> usize {
        self.cursor()
    }

    /// Put text in place of a piece of the text (in bytes), as if it were typed there.
    pub fn replace_text(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        self.marked = None;
        self.replace(range, text, cx);
    }

    /// Put text in at the caret, over what is selected, as if it were typed.
    pub fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        let range = self.marked.take().unwrap_or(self.sel.clone());
        self.replace(range, text, cx);
    }

    fn cursor(&self) -> usize {
        if self.reversed { self.sel.start } else { self.sel.end }
    }

    fn move_to(&mut self, o: usize, cx: &mut Context<Self>) {
        self.selection_toolbar_dismissed = false;
        self.typing = false;
        self.sel = o..o;
        self.reversed = false;
        cx.notify();
    }

    fn select_to(&mut self, o: usize, cx: &mut Context<Self>) {
        self.selection_toolbar_dismissed = false;
        if self.reversed { self.sel.start = o } else { self.sel.end = o }
        if self.sel.end < self.sel.start {
            self.reversed = !self.reversed;
            self.sel = self.sel.end..self.sel.start;
        }
        cx.notify();
    }

    fn prev(&self, o: usize) -> usize {
        self.text.grapheme_indices(true).rev().find_map(|(i, _)| (i < o).then_some(i)).unwrap_or(0)
    }

    fn next(&self, o: usize) -> usize {
        self.text.grapheme_indices(true).find_map(|(i, _)| (i > o).then_some(i)).unwrap_or(self.text.len())
    }

    fn line_start(&self, o: usize) -> usize {
        self.text[..o].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self, o: usize) -> usize {
        self.text[o..].find('\n').map_or(self.text.len(), |i| o + i)
    }

    fn replace(&mut self, range: Range<usize>, s: &str, cx: &mut Context<Self>) {
        self.edit(range, s, true, cx)
    }

    /// Change the text; `keep` makes it a step to undo (a composition was kept when it began).
    fn edit(&mut self, range: Range<usize>, s: &str, keep: bool, cx: &mut Context<Self>) {
        let s = if self.multiline { s.replace("\r\n", "\n").replace('\r', "\n") } else { s.replace(['\n', '\r'], " ") };
        if keep { self.remember(&range, &s) }
        self.text.replace_range(range.clone(), &s);
        let o = range.start + s.len();
        self.sel = o..o;
        self.reversed = false;
        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    /// Keep the text as it is before a change. Letters typed one after
    /// another are one step, until a space, a line break or a move.
    fn remember(&mut self, range: &Range<usize>, s: &str) {
        let typed = range.is_empty() && s.chars().count() == 1 && !s.contains(char::is_whitespace);
        if !(typed && self.typing) {
            self.undo.push((self.text.clone(), self.sel.clone()));
            if self.undo.len() > UNDO_STEPS { self.undo.remove(0); }
        }
        self.redo.clear();
        self.typing = typed;
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        self.undo_last(cx)
    }

    /// Go back to before the last change, as Cmd/Ctrl+Z does.
    pub fn undo_last(&mut self, cx: &mut Context<Self>) {
        let Some((text, sel)) = self.undo.pop() else { return };
        self.redo.push((std::mem::replace(&mut self.text, text), self.sel.clone()));
        self.restore(sel, cx);
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        let Some((text, sel)) = self.redo.pop() else { return };
        self.undo.push((std::mem::replace(&mut self.text, text), self.sel.clone()));
        self.restore(sel, cx);
    }

    fn restore(&mut self, sel: Range<usize>, cx: &mut Context<Self>) {
        let end = self.text.len();
        self.sel = sel.start.min(end)..sel.end.min(end);
        (self.reversed, self.marked, self.typing) = (false, None, false);
        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        if self.sel.is_empty() {
            let p = self.prev(self.cursor());
            self.select_to(p, cx);
        }
        if !self.sel.is_empty() { self.replace(self.sel.clone(), "", cx) }
    }

    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        if self.sel.is_empty() {
            let n = self.next(self.cursor());
            self.select_to(n, cx);
        }
        if !self.sel.is_empty() { self.replace(self.sel.clone(), "", cx) }
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        let o = if self.sel.is_empty() { self.prev(self.cursor()) } else { self.sel.start };
        self.move_to(o, cx);
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        let o = if self.sel.is_empty() { self.next(self.cursor()) } else { self.sel.end };
        self.move_to(o, cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        let p = self.prev(self.cursor());
        self.select_to(p, cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        let n = self.next(self.cursor());
        self.select_to(n, cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        let o = self.line_start(self.cursor());
        self.move_to(o, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        let o = self.line_end(self.cursor());
        self.move_to(o, cx);
    }

    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        let o = self.line_start(self.cursor());
        self.select_to(o, cx);
    }

    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        let o = self.line_end(self.cursor());
        self.select_to(o, cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selection_toolbar_dismissed = false;
        self.sel = 0..self.text.len();
        self.reversed = false;
        cx.notify();
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.sel.is_empty() && !self.masked {
            cx.write_to_clipboard(ClipboardItem::new_string(self.text[self.sel.clone()].to_string()));
        }
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if !self.sel.is_empty() && !self.masked {
            cx.write_to_clipboard(ClipboardItem::new_string(self.text[self.sel.clone()].to_string()));
            self.replace(self.sel.clone(), "", cx);
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else { return };
        // A screenshot may come with a text fallback beside it: where pictures are taken, the picture is what was meant.
        if self.accepts_images {
            let images: Vec<Image> = item.entries().iter().filter_map(|entry| if let ClipboardEntry::Image(image) = entry { Some(image.clone()) } else { None }).collect();
            if !images.is_empty() {
                self.pasted.extend(images);
                return cx.emit(InputEvent::Images);
            }
        }
        if let Some(t) = item.text() { self.replace(self.sel.clone(), &t, cx) }
    }

    fn enter(&mut self, _: &Enter, _: &mut Window, cx: &mut Context<Self>) {
        if self.multiline && !self.submit_on_enter {
            // In a list or a quote the next line goes on with it; Enter on an empty item ends it.
            if self.markdown && self.sel.is_empty() && self.marked.is_none() {
                let (start, at) = (self.line_start(self.cursor()), self.cursor());
                match wisp_core::markdown::continued(&self.text[start..self.line_end(at)]) {
                    Some(next) if at >= start + next.marker && next.empty => return self.replace(start..self.line_end(at), "", cx),
                    Some(next) if at >= start + next.marker => return self.replace(at..at, &format!("\n{}", next.prefix), cx),
                    _ => {}
                }
            }
            return self.replace(self.sel.clone(), "\n", cx);
        }
        cx.emit(InputEvent::Submit);
    }

    fn shift_enter(&mut self, _: &ShiftEnter, _: &mut Window, cx: &mut Context<Self>) {
        if self.multiline {
            return self.replace(self.sel.clone(), "\n", cx);
        }
        cx.emit(InputEvent::Submit);
    }

    fn escape(&mut self, _: &Escape, _: &mut Window, cx: &mut Context<Self>) {
        if self.selection_toolbar_visible { return self.dismiss_selection_toolbar(cx) }
        cx.emit(InputEvent::Cancel);
        // The dialog or panel around the field handles Escape too, unless it only closes the list.
        if !self.choosing { cx.propagate() }
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        if self.choosing { return cx.emit(InputEvent::Up) }
        // Nowhere to go in one line or in an empty field: the key is the window's, which moves in its list.
        if !self.multiline || self.text.is_empty() { return cx.propagate() }
        self.move_line(-1., cx);
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        if self.choosing { return cx.emit(InputEvent::Down) }
        if !self.multiline || self.text.is_empty() { return cx.propagate() }
        self.move_line(1., cx);
    }

    fn tab(&mut self, _: &Complete, _: &mut Window, cx: &mut Context<Self>) {
        if self.choosing { cx.emit(InputEvent::Tab) } else { cx.propagate() }
    }

    /// Move the caret a visual line up or down; past the first or last line it goes to the start or end.
    fn move_line(&mut self, dir: f32, cx: &mut Context<Self>) {
        if !self.multiline {
            return cx.propagate();
        }
        let Some(l) = &self.layout else { return };
        let shown = self.to_display(&l.map, self.cursor());
        let (Some(at), Some(row)) = (self.position_in(l, shown), row_of(l, shown)) else { return };
        let n = l.rows.iter().position(|r| std::ptr::eq(r, row)).unwrap_or(0);
        // The middle of the line above or below: in this paragraph, else in the one before or after it.
        let y = at.y + row.line_height * (0.5 + dir);
        let y = if y < row.top {
            n.checked_sub(1).map(|n| &l.rows[n]).map(|r| r.top + r.height() - r.line_height * 0.5)
        } else if y >= row.top + row.height() {
            l.rows.get(n + 1).map(|r| r.top + r.line_height * 0.5)
        } else {
            Some(y)
        };
        let target = match y {
            None if dir < 0. => 0,
            None => self.text.len(),
            Some(y) => self.text_offset(&l.map, self.index_in(l, point(at.x, y))),
        };
        self.move_to(target, cx);
    }

    // ---- displayed text (bullets when masked, Markdown drawn)

    fn display_text(&self) -> String {
        if self.masked { BULLET.repeat(self.text.chars().count()) } else { self.text.clone() }
    }

    fn to_display(&self, map: &DisplayMap, o: usize) -> usize {
        if self.masked { self.text[..o.min(self.text.len())].chars().count() * BULLET.len() } else { map.to_shown(o.min(self.text.len())) }
    }

    fn text_offset(&self, map: &DisplayMap, d: usize) -> usize {
        if !self.masked { return self.text.floor_char_boundary(map.to_source(d).min(self.text.len())) }
        self.text.char_indices().nth(d / BULLET.len()).map_or(self.text.len(), |(i, _)| i)
    }

    /// Where display offset `o` is drawn, relative to the text origin.
    fn position_in(&self, l: &Layout, o: usize) -> Option<Point<Pixels>> {
        let row = row_of(l, o)?;
        let p = row.line.position_for_index((o - row.start).min(row.line.len()), row.line_height)?;
        Some(point(row.x + p.x, p.y + row.top))
    }

    /// The display offset closest to a point relative to the text origin.
    fn index_in(&self, l: &Layout, p: Point<Pixels>) -> usize {
        let Some(row) = l.rows.iter().rev().find(|row| row.top <= p.y).or(l.rows.first()) else { return 0 };
        let local = point((p.x - row.x).max(px(0.)), (p.y - row.top).max(px(0.)).min(row.height() - px(1.)));
        row.start + match row.line.closest_index_for_position(local, row.line_height) {
            Ok(i) | Err(i) => i,
        }
    }

    fn index_for_mouse(&self, p: Point<Pixels>) -> usize {
        let Some(l) = &self.layout else { return 0 };
        let local = point(p.x - l.bounds.left() + l.scroll_x, p.y - l.bounds.top());
        self.text_offset(&l.map, self.index_in(l, local))
    }

    fn on_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.selection_toolbar_dismissed = false;
        window.focus(&self.focus);
        let i = self.index_for_mouse(ev.position);
        self.selecting = true;
        if self.markdown { self.reveal_held = Some(i..i) }
        // Two clicks a word, three a paragraph, ⌘ or Ctrl with one a sentence, as in Word.
        let unit = match ev.click_count {
            2 => Some(Unit::Word),
            n if n >= 3 => Some(Unit::Paragraph),
            _ => (ev.modifiers.secondary() && !ev.modifiers.shift).then_some(Unit::Sentence),
        };
        self.grain = None;
        let Some(unit) = unit else {
            return if ev.modifiers.shift { self.select_to(i, cx) } else { self.move_to(i, cx) };
        };
        // A hidden text gives nothing away about its words.
        if self.masked {
            self.selecting = false;
            self.sel = 0..self.text.len();
            self.reversed = false;
            return cx.notify();
        }
        let range = selection::around(&self.text, i, unit);
        self.grain = Some((unit, range.clone()));
        self.typing = false;
        self.selection_toolbar_dismissed = false;
        self.sel = range;
        self.reversed = false;
        cx.notify();
    }

    fn on_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting && ev.pressed_button == Some(MouseButton::Left) {
            let i = self.index_for_mouse(ev.position);
            let Some((unit, first)) = &self.grain else { return self.select_to(i, cx) };
            let (sel, reversed) = selection::extend(&self.text, first, i, *unit);
            if (&sel, reversed) != (&self.sel, self.reversed) {
                (self.sel, self.reversed) = (sel, reversed);
                cx.notify();
            }
        }
    }

    fn on_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            self.selecting = false;
            self.grain = None;
            self.reveal_held = None;
            // A click on marked words: the owner says what they are marked for.
            let at = self.cursor();
            if self.sel.is_empty() && let Some(n) = self.marks.iter().position(|(range, _)| range.start <= at && at <= range.end && range.start < range.end) { cx.emit(InputEvent::MarkClicked(n)) }
            cx.notify();
        }
    }

    fn dismiss_selection_toolbar(&mut self, cx: &mut Context<Self>) {
        self.selection_toolbar_dismissed = true;
        self.selection_toolbar_visible = false;
        cx.notify();
    }

    // ---- UTF-16 offsets, which the platform's input methods speak

    fn to16(&self, o: usize) -> usize {
        self.text[..o.min(self.text.len())].encode_utf16().count()
    }

    fn from16(&self, o16: usize) -> usize {
        let mut n = 0;
        for (i, ch) in self.text.char_indices() {
            if n >= o16 { return i }
            n += ch.len_utf16();
        }
        self.text.len()
    }

    fn range16(&self, r: &Range<usize>) -> Range<usize> {
        self.to16(r.start)..self.to16(r.end)
    }

    fn range8(&self, r: &Range<usize>) -> Range<usize> {
        self.from16(r.start)..self.from16(r.end)
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(&mut self, range: Range<usize>, actual: &mut Option<Range<usize>>, _: &mut Window, _: &mut Context<Self>) -> Option<String> {
        let r = self.range8(&range);
        actual.replace(self.range16(&r));
        Some(self.text[r].to_string())
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: self.range16(&self.sel), reversed: self.reversed })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|r| self.range16(r))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(&mut self, range: Option<Range<usize>>, s: &str, _: &mut Window, cx: &mut Context<Self>) {
        let r = range.map(|r| self.range8(&r)).or(self.marked.clone()).unwrap_or(self.sel.clone());
        let composed = self.marked.take().is_some();
        self.edit(r, s, !composed, cx);
    }

    fn replace_and_mark_text_in_range(&mut self, range: Option<Range<usize>>, s: &str, new_sel: Option<Range<usize>>, _: &mut Window, cx: &mut Context<Self>) {
        let r = range.map(|r| self.range8(&r)).or(self.marked.clone()).unwrap_or(self.sel.clone());
        // Undo goes back to before the composition, not to the letters typed for it.
        if self.marked.is_none() {
            self.typing = false;
            self.remember(&r, "");
        }
        self.text.replace_range(r.clone(), s);
        self.marked = (!s.is_empty()).then(|| r.start..r.start + s.len());
        self.sel = new_sel
            .map(|ns| {
                let base16 = self.to16(r.start);
                self.from16(base16 + ns.start)..self.from16(base16 + ns.end)
            })
            .unwrap_or(r.start + s.len()..r.start + s.len());
        self.reversed = false;
        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    fn bounds_for_range(&mut self, range: Range<usize>, _: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let r = self.range8(&range);
        let l = self.layout.as_ref()?;
        let origin = point(l.bounds.left() - l.scroll_x, l.bounds.top());
        let (a, b) = (self.to_display(&l.map, r.start), self.to_display(&l.map, r.end));
        let line_height = row_of(l, b)?.line_height;
        let (a, b) = (self.position_in(l, a)?, self.position_in(l, b)?);
        Some(Bounds::from_corners(origin + a, origin + point(b.x, b.y + line_height)))
    }

    fn character_index_for_point(&mut self, p: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.to16(self.index_for_mouse(p)))
    }
}

impl Render for TextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("TextInput")
            .track_focus(&self.focus)
            .cursor(CursorStyle::IBeam)
            .flex_1()
            .min_w_0()
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::enter))
            .on_action(cx.listener(Self::shift_enter))
            .on_action(cx.listener(Self::escape))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::tab))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_down))
            .on_mouse_move(cx.listener(Self::on_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_up))
            .child(InputElement { input: cx.entity() })
    }
}

struct InputElement {
    input: Entity<TextInput>,
}

#[derive(Clone, Copy)]
struct SelectionGeometry {
    first: Bounds<Pixels>,
    last: Bounds<Pixels>,
    viewport: Bounds<Pixels>,
}

impl SelectionGeometry {
    fn toolbar_origin(self, toolbar: Size<Pixels>) -> Point<Pixels> {
        let inset = px(6.);
        let gap = px(8.);
        // Above the first selected line; below the last when the top has no room.
        let above = self.first.top() - gap - toolbar.height;
        let (line, y) = if above >= self.viewport.top() + inset {
            (self.first, above)
        } else {
            (self.last, self.last.bottom() + gap)
        };
        let left = self.viewport.left() + inset;
        let top = self.viewport.top() + inset;
        point(
            (line.center().x - toolbar.width / 2.).clamp(left, (self.viewport.right() - inset - toolbar.width).max(left)),
            y.clamp(top, (self.viewport.bottom() - inset - toolbar.height).max(top)),
        )
    }
}

/// Draw the editor normally, then place its toolbar using this frame's selection geometry.
/// Deferring the toolbar keeps it above the text and outside the scroll area's clipping layer.
pub fn with_selection_toolbar(input: &Entity<TextInput>, toolbar: impl IntoElement) -> impl IntoElement {
    SelectionToolbar { input: input.clone(), editor: input.clone().into_any_element(), toolbar: Some(toolbar.into_any_element()) }
}

struct SelectionToolbar {
    input: Entity<TextInput>,
    editor: AnyElement,
    toolbar: Option<AnyElement>,
}

impl IntoElement for SelectionToolbar {
    type Element = Self;
    fn into_element(self) -> Self { self }
}

impl Element for SelectionToolbar {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> { None }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> { None }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        (self.editor.request_layout(window, cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) {
        self.editor.prepaint(window, cx);
        let input = self.input.read(cx);
        if input.selecting || input.selection_toolbar_dismissed || !input.focus.is_focused(window) || input.selected().trim().is_empty() { return }
        let Some(geometry) = input.selection_geometry else { return };
        let available = geometry.viewport.size - size(px(12.), px(12.));
        if available.width <= px(0.) || available.height <= px(0.) { return }
        let Some(toolbar) = self.toolbar.take() else { return };
        let outside = self.input.clone();
        let clicked = self.input.clone();
        let mut toolbar = div()
            .id("text-selection-toolbar")
            .max_w(available.width)
            .max_h(available.height)
            .overflow_y_scroll()
            .rounded(px(10.))
            .border_1()
            .border_color(Theme::line_strong())
            .bg(Theme::bg_body())
            .shadow(crate::theme::shadow_panel())
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .on_mouse_down_out(move |_, _, cx| outside.update(cx, |input, cx| input.dismiss_selection_toolbar(cx)))
            .on_click(move |_, _, cx| clicked.update(cx, |input, cx| input.dismiss_selection_toolbar(cx)))
            .child(toolbar)
            .into_any_element();
        let measured = toolbar.layout_as_root(size(AvailableSpace::MaxContent, AvailableSpace::MaxContent), window, cx);
        window.defer_draw(toolbar, geometry.toolbar_origin(measured), 1);
        self.input.update(cx, |input, _| input.selection_toolbar_visible = true);
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) {
        self.editor.paint(window, cx);
    }
}

struct Prepaint {
    rows: Vec<Row>,
    map: DisplayMap,
    /// Behind the text: quote bars, rules, the ground of code.
    decor: Vec<PaintQuad>,
    /// Under the text whether the field has the keyboard or not.
    marks: Vec<PaintQuad>,
    selection: Vec<PaintQuad>,
    caret: Option<PaintQuad>,
    scroll_x: Pixels,
}

impl IntoElement for InputElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

/// The row a display offset is drawn in.
fn row_of(l: &Layout, o: usize) -> Option<&Row> {
    l.rows.iter().rev().find(|row| row.start <= o).or(l.rows.first())
}

/// The text to draw with its runs: the placeholder when empty, the marked
/// (composing) range underlined; Markdown drawn as it says (`shape_markdown`).
fn shape(input: &TextInput, style: &TextStyle, window: &mut Window, wrap: Option<Pixels>) -> (Vec<Row>, DisplayMap) {
    if input.markdown && !input.text.is_empty() && !input.masked { return shape_markdown(input, style, window, wrap) }
    let display = input.display_text();
    let (text, color) = if display.is_empty() { (crate::i18n::t(&input.placeholder).to_string(), Theme::text_placeholder()) } else { (display, style.color) };
    let run = TextRun { len: text.len(), font: style.font(), color, background_color: None, underline: None, strikethrough: None };
    let plain = DisplayMap::default();
    let marked = input.marked.as_ref().filter(|_| !input.text.is_empty()).map(|m| input.to_display(&plain, m.start)..input.to_display(&plain, m.end));
    let runs: Vec<TextRun> = match marked {
        Some(m) => vec![
            TextRun { len: m.start, ..run.clone() },
            TextRun { len: m.end - m.start, underline: Some(UnderlineStyle { color: Some(color), thickness: px(1.), wavy: false }), ..run.clone() },
            TextRun { len: text.len() - m.end, ..run },
        ]
        .into_iter()
        .filter(|r| r.len > 0)
        .collect(),
        None => vec![run],
    };
    let font_size = style.font_size.to_pixels(window.rem_size());
    let line_height = window.line_height();
    let shaped = window.text_system().shape_text(text.into(), font_size, &runs, wrap, None).unwrap_or_default();
    let mut rows = vec![];
    let (mut start, mut top) = (0, px(0.));
    for line in shaped {
        let len = line.len();
        let row = Row { line, start, top, x: px(0.), line_height, decor: Decor::None };
        top += row.height();
        rows.push(row);
        start += len + 1;
    }
    (rows, plain)
}

/// How much larger a heading is set than the text, by its level.
fn heading_scale(level: u8) -> f32 {
    match level {
        1 => 1.4,
        2 => 1.25,
        3 => 1.12,
        _ => 1.,
    }
}

/// How far a quote's text is set in, per level.
const QUOTE_INDENT: f32 = 16.;
/// How far code is set in from its ground's edge.
const CODE_INDENT: f32 = 10.;

/// Markdown drawn as it says, a paragraph at a time: headings larger,
/// emphasis, code and links in their style, a bullet for `-`, quotes set in
/// behind a bar. The syntax is hidden but on the lines the selection or the
/// caret is on, where it is drawn faintly so it can be edited.
fn shape_markdown(input: &TextInput, style: &TextStyle, window: &mut Window, wrap: Option<Pixels>) -> (Vec<Row>, DisplayMap) {
    use wisp_core::markdown::{self, Part};
    let text = &input.text;
    let focused = input.focus.is_focused(window);
    let reveal = input.reveal_held.clone().unwrap_or(input.sel.clone());
    let font_size = style.font_size.to_pixels(window.rem_size());
    let base_height = window.line_height();
    let mut map = DisplayMap::default();
    let mut rows: Vec<Row> = vec![];
    let (mut start, mut top) = (0usize, px(0.));
    let lines = markdown::lines(text);
    for (n, line) in lines.iter().enumerate() {
        let revealed = focused && line.range.start <= reveal.end && reveal.start <= line.range.end;
        let mut shown = String::new();
        let mut runs: Vec<TextRun> = vec![];
        let base_color = if line.quote > 0 { Theme::text_secondary() } else { style.color };
        for piece in &line.pieces {
            let written = &text[piece.range.clone()];
            let hidden = piece.part == Part::Syntax && !revealed;
            let drawn: &str = if piece.part == Part::Bullet && !revealed { "•" } else if hidden { "" } else { written };
            let from = start + shown.len();
            map.segs.push(Seg { src: piece.range.clone(), shown: from..from + drawn.len(), same: drawn.len() == written.len() && !hidden && drawn == written });
            if drawn.is_empty() { continue }
            shown.push_str(drawn);
            let marks = &piece.marks;
            let mut font = style.font();
            if marks.bold || line.heading > 0 { font.weight = FontWeight::SEMIBOLD }
            if marks.italic { font.style = FontStyle::Italic }
            if marks.code || line.code { font.family = crate::theme::MONO_FONT.into() }
            let color = match piece.part {
                Part::Syntax => Theme::text_tertiary(),
                Part::Bullet => Theme::text_secondary(),
                Part::Text if marks.link.is_some() || piece.cite => Theme::accent(),
                Part::Text => base_color,
            };
            let run = TextRun {
                len: drawn.len(),
                font,
                color,
                background_color: (marks.code && !line.code).then(Theme::groove),
                underline: marks.link.as_ref().map(|_| UnderlineStyle { color: Some(Theme::accent().opacity(0.5)), thickness: px(1.), wavy: false }),
                strikethrough: marks.strike.then(|| StrikethroughStyle { thickness: px(1.), color: None }),
            };
            // The letters being composed are underlined; they are always drawn as typed.
            match input.marked.as_ref().filter(|m| piece.part != Part::Bullet && m.start < piece.range.end && piece.range.start < m.end && drawn == written) {
                Some(m) => {
                    let (a, b) = (m.start.max(piece.range.start) - piece.range.start, m.end.min(piece.range.end) - piece.range.start);
                    let underline = Some(UnderlineStyle { color: Some(color), thickness: px(1.), wavy: false });
                    for (len, underline) in [(a, run.underline), (b - a, underline), (drawn.len() - b, run.underline)] {
                        if len > 0 { runs.push(TextRun { len, underline, ..run.clone() }) }
                    }
                }
                None => runs.push(run),
            }
        }
        let scale = heading_scale(line.heading);
        let (size, mut line_height) = if line.code { (font_size * 0.9, base_height) } else { (font_size * scale, (base_height * scale).round()) };
        let x = px(QUOTE_INDENT * line.quote as f32) + if line.code { px(CODE_INDENT) } else { px(0.) };
        let decor = if line.code {
            Decor::Code
        } else if line.rule && shown.is_empty() {
            Decor::Rule
        } else if line.quote > 0 {
            Decor::Quote(line.quote)
        } else {
            Decor::None
        };
        // Room between paragraphs, more over a heading; none inside a list, a quote or code.
        let empty = |n: usize| text[lines[n].range.clone()].trim().is_empty();
        let gap = if n == 0 || empty(n) || empty(n - 1) {
            px(0.)
        } else if line.heading > 0 {
            (base_height * 0.5).round()
        } else if (line.item.is_some() && lines[n - 1].item.is_some()) || (line.code && lines[n - 1].code) || (line.quote > 0 && lines[n - 1].quote > 0) {
            px(0.)
        } else {
            (base_height * 0.3).round()
        };
        // A blank line is the paragraph separator in Markdown. Its visual height can change
        // without inserting/removing characters, and the caret/selection use the same row metrics.
        let paragraph_factor = crate::theme::paragraph_factor();
        if empty(n) && !line.code { line_height *= paragraph_factor }
        top += gap * paragraph_factor;
        let len = shown.len();
        let wrap = wrap.map(|w| (w - x).max(px(40.)));
        let shaped = window.text_system().shape_text(shown.into(), size, &runs, wrap, None).unwrap_or_default();
        let Some(shaped) = shaped.into_iter().next() else { continue };
        let row = Row { line: shaped, start, top, x, line_height, decor };
        top += row.height();
        rows.push(row);
        start += len + 1;
        // The line break, drawn as written.
        if line.range.end < text.len() { map.segs.push(Seg { src: line.range.end..line.range.end + 1, shown: start - 1..start, same: true }) }
    }
    (rows, map)
}

impl Element for InputElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let line_height = window.line_height();
        let input = self.input.read(cx);
        if !input.multiline {
            style.size.height = line_height.into();
            return (window.request_layout(style, [], cx), ());
        }
        let entity = self.input.clone();
        let text_style = window.text_style();
        let min = line_height * input.min_lines as f32;
        let id = window.request_measured_layout(style, move |known, available, window, cx| {
            let width = known.width.or(match available.width {
                AvailableSpace::Definite(w) => Some(w),
                _ => None,
            });
            let (rows, _) = shape(entity.read(cx), &text_style, window, width);
            let height = rows.last().map_or(px(0.), |row| row.top + row.height());
            size(width.unwrap_or(px(240.)), height.max(min))
        });
        (id, ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) -> Prepaint {
        let input = self.input.read(cx);
        let style = window.text_style();
        let (rows, map) = shape(input, &style, window, input.multiline.then_some(bounds.size.width));
        let probe = Layout { rows, map, bounds, scroll_x: px(0.) };
        let cursor = input.to_display(&probe.map, input.cursor());
        let caret_at = input.position_in(&probe, cursor).unwrap_or_default();
        // A single line scrolls sideways to keep the caret in view.
        let mut scroll_x = px(0.);
        if !input.multiline {
            let width = bounds.size.width - px(2.);
            // Unfocused, a long value shows its beginning rather than wherever the caret was left.
            scroll_x = if input.focus.is_focused(window) { input.scroll_x.min(caret_at.x).max(caret_at.x - width).max(px(0.)) } else { px(0.) };
        }
        let origin = point(bounds.left() - scroll_x, bounds.top());
        // The rows a stretch of the drawn text covers, line by line.
        let quads = |a: usize, b: usize, color: Hsla| {
            let mut quads = vec![];
            for row in &probe.rows {
                let (line, start, top, line_height) = (&row.line, row.start, row.top, row.line_height);
                let (from, to) = (a.max(start), b.min(start + line.len()));
                if from > to || (from == to && !(a < start && b > start + line.len())) { continue }
                let (Some(mut p), Some(q)) = (line.position_for_index(from - start, line_height), line.position_for_index(to - start, line_height)) else { continue };
                // A stretch that begins where the line wraps begins on the next line, not at the end of this one.
                if from < to {
                    let next = (from - start + 1..=line.text.len()).find(|at| line.text.is_char_boundary(*at));
                    if let Some(after) = next.and_then(|at| line.position_for_index(at, line_height)) && after.y > p.y { p = point(px(0.), after.y) }
                }
                let x0 = row.x;
                let quad = |l: Pixels, r: Pixels, y: Pixels| fill(Bounds::from_corners(origin + point(l, top + y), origin + point(r, top + y + line_height)), color);
                if p.y == q.y {
                    quads.push(quad(x0 + p.x, x0 + q.x, p.y));
                } else {
                    quads.push(quad(x0 + p.x, bounds.size.width, p.y));
                    let mut y = p.y + line_height;
                    while y < q.y {
                        quads.push(quad(x0, bounds.size.width, y));
                        y += line_height;
                    }
                    quads.push(quad(x0, x0 + q.x, q.y));
                }
            }
            quads
        };
        let selection = if input.sel.is_empty() { vec![] } else { quads(input.to_display(&probe.map, input.sel.start), input.to_display(&probe.map, input.sel.end), Theme::selection()) };
        let marks = input.marks.iter().filter(|(range, _)| range.start < range.end && range.end <= input.text.len() && !input.masked).flat_map(|(range, color)| quads(input.to_display(&probe.map, range.start), input.to_display(&probe.map, range.end), *color)).collect();
        let line_height = row_of(&probe, cursor).map_or(window.line_height(), |row| row.line_height);
        let caret = input.sel.is_empty().then(|| fill(Bounds::new(origin + point(caret_at.x, caret_at.y + px(1.)), size(px(1.5), line_height - px(2.))), Theme::text()));
        let decor = probe
            .rows
            .iter()
            .flat_map(|row| {
                let (top, height) = (row.top, row.height());
                match row.decor {
                    Decor::None => vec![],
                    Decor::Quote(depth) => (0..depth).map(|n| fill(Bounds::new(origin + point(px(QUOTE_INDENT * n as f32 + 2.), top), size(px(3.), height)), Theme::line_strong())).collect(),
                    Decor::Rule => vec![fill(Bounds::new(origin + point(px(0.), top + (height / 2.).round()), size(bounds.size.width, px(1.))), Theme::line_strong())],
                    Decor::Code => vec![fill(Bounds::new(origin + point(row.x - px(CODE_INDENT), top), size(bounds.size.width - row.x + px(CODE_INDENT), height)), Theme::bg_sunken())],
                }
            })
            .collect();
        let viewport = bounds.intersect(&window.content_mask().bounds);
        let visible: Vec<_> = selection.iter().map(|quad| quad.bounds.intersect(&viewport)).filter(|bounds| !bounds.is_empty()).collect();
        let geometry = visible.first().zip(visible.last()).map(|(&first, &last)| SelectionGeometry { first, last, viewport });
        self.input.update(cx, |input, _| {
            input.selection_geometry = geometry;
            input.selection_toolbar_visible = false;
        });
        Prepaint { rows: probe.rows, map: probe.map, decor, marks, selection, caret, scroll_x }
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), prepaint: &mut Prepaint, window: &mut Window, cx: &mut App) {
        let focus = self.input.read(cx).focus.clone();
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.input.clone()), cx);
        let origin = point(bounds.left() - prepaint.scroll_x, bounds.top());
        let rows = std::mem::take(&mut prepaint.rows);
        let focused = focus.is_focused(window);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for quad in prepaint.decor.drain(..).chain(prepaint.marks.drain(..)) {
                window.paint_quad(quad);
            }
            if focused {
                for quad in prepaint.selection.drain(..) {
                    window.paint_quad(quad);
                }
            }
            for row in &rows {
                let _ = row.line.paint(origin + point(row.x, row.top), row.line_height, TextAlign::Left, None, window, cx);
            }
            if focused && let Some(caret) = prepaint.caret.take() {
                window.paint_quad(caret);
            }
        });
        let scroll_x = prepaint.scroll_x;
        let map = std::mem::take(&mut prepaint.map);
        self.input.update(cx, |input, _| {
            input.scroll_x = scroll_x;
            let layout = Layout { rows, map, bounds, scroll_x };
            let tops: Vec<Pixels> = input.marks.iter().map(|(range, _)| input.position_in(&layout, input.to_display(&layout.map, range.start.min(input.text.len()))).map_or(px(0.), |p| p.y)).collect();
            // What the owner drew beside the marks was placed by the last frame's: draw again with these.
            if tops != input.mark_tops {
                input.mark_tops = tops;
                window.request_animation_frame();
            }
            input.layout = Some(layout);
        });
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that would bring in gpui's own `test` attribute.
    use super::{DisplayMap, Seg};

    /// `a **b**` drawn as `a b`, then a line break and `- c` drawn as `• c`.
    fn map() -> DisplayMap {
        let seg = |src: std::ops::Range<usize>, shown: std::ops::Range<usize>, same: bool| Seg { src, shown, same };
        DisplayMap { segs: vec![seg(0..2, 0..2, true), seg(2..4, 2..2, false), seg(4..5, 2..3, true), seg(5..7, 3..3, false), seg(7..8, 3..4, true), seg(8..9, 4..7, false), seg(9..11, 7..9, true)] }
    }

    #[test]
    fn drawn_and_written_places_meet() {
        let map = map();
        // Inside the hidden `**` the caret is drawn where the stars would be.
        assert_eq!([0, 2, 3, 4, 5, 6, 7, 8, 9, 11].map(|o| map.to_shown(o)), [0, 2, 2, 2, 3, 3, 3, 4, 7, 9]);
        // A click beside the bold word lands outside its stars; one after the bullet, after the marker.
        assert_eq!([0, 2, 3, 4, 7, 9].map(|d| map.to_source(d)), [0, 2, 5, 8, 9, 11]);
        assert_eq!(DisplayMap::default().to_source(5), 5);
    }
}
