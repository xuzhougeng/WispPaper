//! The text field: one line, or several with wrapping. Handles selection,
//! the clipboard and IME composition (marked text), which GPUI leaves to the
//! application.

use crate::theme::Theme;
use gpui::*;
use std::ops::Range;
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
}

/// What the last paint laid out, for hit-testing and the IME.
struct Layout {
    /// Each paragraph with its byte offset in the displayed text and its top.
    lines: Vec<(WrappedLine, usize, Pixels)>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
    scroll_x: Pixels,
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
    pasted: Vec<Image>,
    layout: Option<Layout>,
    selecting: bool,
    scroll_x: Pixels,
    /// The text and selection before each change, to go back to; what was undone, to go forward to.
    undo: Vec<(String, Range<usize>)>,
    redo: Vec<(String, Range<usize>)>,
    /// The last change was a character typed: the next one joins its step.
    typing: bool,
    /// Stretches of the text drawn on a tint (bytes, and the tint): the words a comment is about.
    marks: Vec<(Range<usize>, Hsla)>,
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
            pasted: vec![],
            layout: None,
            selecting: false,
            scroll_x: px(0.),
            undo: vec![],
            redo: vec![],
            typing: false,
            marks: vec![],
        }
    }

    pub fn multiline(mut self, min_lines: usize) -> Self {
        self.multiline = true;
        self.min_lines = min_lines;
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

    /// How far below the top of the text byte `at` was last drawn; none before the first frame.
    pub fn top_of(&self, at: usize) -> Option<Pixels> {
        let layout = self.layout.as_ref()?;
        Some(self.position_in(layout, self.to_display(at.min(self.text.len())))?.y)
    }

    /// What is selected, in bytes; empty at the caret.
    pub fn selection(&self) -> Range<usize> {
        self.sel.clone()
    }

    /// Select `range` (bytes, kept within the text).
    pub fn select_range(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
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
        self.typing = false;
        self.sel = o..o;
        self.reversed = false;
        cx.notify();
    }

    fn select_to(&mut self, o: usize, cx: &mut Context<Self>) {
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
        let Some(at) = self.position_in(l, self.to_display(self.cursor())) else { return };
        let y = at.y + l.line_height * (0.5 + dir);
        let bottom = l.lines.last().map_or(px(0.), |(line, _, top)| *top + line.size(l.line_height).height);
        let target = if y < px(0.) {
            0
        } else if y > bottom {
            self.text.len()
        } else {
            self.text_offset(self.index_in(l, point(at.x, y)))
        };
        self.move_to(target, cx);
    }

    // ---- displayed text (bullets when masked)

    fn display_text(&self) -> String {
        if self.masked { BULLET.repeat(self.text.chars().count()) } else { self.text.clone() }
    }

    fn to_display(&self, o: usize) -> usize {
        if self.masked { self.text[..o.min(self.text.len())].chars().count() * BULLET.len() } else { o }
    }

    fn text_offset(&self, d: usize) -> usize {
        if !self.masked { return d.min(self.text.len()) }
        self.text.char_indices().nth(d / BULLET.len()).map_or(self.text.len(), |(i, _)| i)
    }

    /// Where display offset `o` is drawn, relative to the text origin.
    fn position_in(&self, l: &Layout, o: usize) -> Option<Point<Pixels>> {
        let (line, start, top) = l.lines.iter().rev().find(|(_, start, _)| *start <= o)?;
        let p = line.position_for_index((o - start).min(line.len()), l.line_height)?;
        Some(point(p.x, p.y + *top))
    }

    /// The display offset closest to a point relative to the text origin.
    fn index_in(&self, l: &Layout, p: Point<Pixels>) -> usize {
        let Some((line, start, top)) = l.lines.iter().rev().find(|(_, _, top)| *top <= p.y).or(l.lines.first()) else { return 0 };
        let local = point(p.x.max(px(0.)), (p.y - *top).max(px(0.)).min(line.size(l.line_height).height - px(1.)));
        start + match line.closest_index_for_position(local, l.line_height) {
            Ok(i) | Err(i) => i,
        }
    }

    fn index_for_mouse(&self, p: Point<Pixels>) -> usize {
        let Some(l) = &self.layout else { return 0 };
        let local = point(p.x - l.bounds.left() + l.scroll_x, p.y - l.bounds.top());
        self.text_offset(self.index_in(l, local))
    }

    fn on_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        if ev.click_count >= 2 {
            self.sel = 0..self.text.len();
            self.reversed = false;
            return cx.notify();
        }
        let i = self.index_for_mouse(ev.position);
        self.selecting = true;
        if ev.modifiers.shift { self.select_to(i, cx) } else { self.move_to(i, cx) }
    }

    fn on_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting && ev.pressed_button == Some(MouseButton::Left) {
            let i = self.index_for_mouse(ev.position);
            self.select_to(i, cx);
        }
    }

    fn on_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
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
        let a = self.position_in(l, self.to_display(r.start))?;
        let b = self.position_in(l, self.to_display(r.end))?;
        Some(Bounds::from_corners(origin + a, origin + point(b.x, b.y + l.line_height)))
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

struct Prepaint {
    lines: Vec<(WrappedLine, usize, Pixels)>,
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

/// The text to draw with its runs: the placeholder when empty, the marked
/// (composing) range underlined.
fn shape(input: &TextInput, style: &TextStyle, window: &mut Window, wrap: Option<Pixels>) -> Vec<(WrappedLine, usize, Pixels)> {
    let display = input.display_text();
    let (text, color) = if display.is_empty() { (crate::i18n::t(&input.placeholder).to_string(), Theme::text_placeholder()) } else { (display, style.color) };
    let run = TextRun { len: text.len(), font: style.font(), color, background_color: None, underline: None, strikethrough: None };
    let marked = input.marked.as_ref().filter(|_| !input.text.is_empty()).map(|m| input.to_display(m.start)..input.to_display(m.end));
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
    let mut lines = vec![];
    let (mut start, mut top) = (0, px(0.));
    for line in shaped {
        let (len, height) = (line.len(), line.size(line_height).height);
        lines.push((line, start, top));
        start += len + 1;
        top += height;
    }
    lines
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
            let lines = shape(entity.read(cx), &text_style, window, width);
            let height = lines.last().map_or(px(0.), |(line, _, top)| *top + line.size(line_height).height);
            size(width.unwrap_or(px(240.)), height.max(min))
        });
        (id, ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) -> Prepaint {
        let input = self.input.read(cx);
        let style = window.text_style();
        let line_height = window.line_height();
        let lines = shape(input, &style, window, input.multiline.then_some(bounds.size.width));
        let probe = Layout { lines, bounds, line_height, scroll_x: px(0.) };
        let caret_at = input.position_in(&probe, input.to_display(input.cursor())).unwrap_or_default();
        // A single line scrolls sideways to keep the caret in view.
        let mut scroll_x = px(0.);
        if !input.multiline {
            let width = bounds.size.width - px(2.);
            // Unfocused, a long value shows its beginning rather than wherever the caret was left.
            scroll_x = if input.focus.is_focused(window) { input.scroll_x.min(caret_at.x).max(caret_at.x - width).max(px(0.)) } else { px(0.) };
        }
        let origin = point(bounds.left() - scroll_x, bounds.top());
        // The rows a stretch of the text covers, line by line.
        let rows = |a: usize, b: usize, color: Hsla| {
            let mut quads = vec![];
            for (line, start, top) in &probe.lines {
                let (from, to) = (a.max(*start), b.min(start + line.len()));
                if from > to || (from == to && !(a < *start && b > *start + line.len())) { continue }
                let (Some(p), Some(q)) = (line.position_for_index(from - start, line_height), line.position_for_index(to - start, line_height)) else { continue };
                let row = |x0: Pixels, x1: Pixels, y: Pixels| fill(Bounds::from_corners(origin + point(x0, *top + y), origin + point(x1, *top + y + line_height)), color);
                if p.y == q.y {
                    quads.push(row(p.x, q.x, p.y));
                } else {
                    quads.push(row(p.x, bounds.size.width, p.y));
                    let mut y = p.y + line_height;
                    while y < q.y {
                        quads.push(row(px(0.), bounds.size.width, y));
                        y += line_height;
                    }
                    quads.push(row(px(0.), q.x, q.y));
                }
            }
            quads
        };
        let selection = if input.sel.is_empty() { vec![] } else { rows(input.to_display(input.sel.start), input.to_display(input.sel.end), Theme::selection()) };
        let marks = input.marks.iter().filter(|(range, _)| range.start < range.end && range.end <= input.text.len() && !input.masked).flat_map(|(range, color)| rows(range.start, range.end, *color)).collect();
        let caret = input.sel.is_empty().then(|| fill(Bounds::new(origin + point(caret_at.x, caret_at.y + px(1.)), size(px(1.5), line_height - px(2.))), Theme::text()));
        Prepaint { lines: probe.lines, marks, selection, caret, scroll_x }
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), prepaint: &mut Prepaint, window: &mut Window, cx: &mut App) {
        let focus = self.input.read(cx).focus.clone();
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.input.clone()), cx);
        let line_height = window.line_height();
        let origin = point(bounds.left() - prepaint.scroll_x, bounds.top());
        let lines = std::mem::take(&mut prepaint.lines);
        let focused = focus.is_focused(window);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for quad in prepaint.marks.drain(..) {
                window.paint_quad(quad);
            }
            if focused {
                for quad in prepaint.selection.drain(..) {
                    window.paint_quad(quad);
                }
            }
            for (line, _, top) in &lines {
                let _ = line.paint(origin + point(px(0.), *top), line_height, TextAlign::Left, None, window, cx);
            }
            if focused && let Some(caret) = prepaint.caret.take() {
                window.paint_quad(caret);
            }
        });
        let scroll_x = prepaint.scroll_x;
        self.input.update(cx, |input, _| {
            input.scroll_x = scroll_x;
            input.layout = Some(Layout { lines, bounds, line_height, scroll_x });
        });
    }
}
