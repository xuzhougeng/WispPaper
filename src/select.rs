//! Text of the window that can be selected with the pointer and copied: a
//! paper's title, authors and abstract, a question asked. (An answer has its
//! own selection over its Markdown, `markdown.rs`; a text field its own.)
//! One text at a time: drag for a stretch, click twice for a word, three
//! times for the whole, ⌘C / Ctrl+C or a right click to copy.

use crate::app::{MenuKind, Workspace};
use crate::theme::Theme;
use gpui::*;
use std::ops::Range;

/// What is selected: in which text (a key and the text itself, so that
/// another paper's abstract under the same key is not marked), from where
/// the button went down to where the pointer is.
pub struct Picked {
    pub key: SharedString,
    pub text: SharedString,
    anchor: usize,
    head: usize,
    /// The button is still down.
    pub held: bool,
    /// The text as it was laid out when the button went down: where the pointer is in it.
    layout: TextLayout,
}

impl Picked {
    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    pub fn chosen(&self) -> &str {
        self.text.get(self.range()).unwrap_or("")
    }
}

/// The byte of `text` under a point of the window; past an end, that end.
fn index_at(layout: &TextLayout, text: &str, position: Point<Pixels>) -> usize {
    // A text not laid out in this frame has no place to be found in.
    let index = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| layout.index_for_position(position).unwrap_or_else(|index| index))).unwrap_or(0);
    floor(text, index)
}

fn floor(text: &str, index: usize) -> usize {
    (0..=index.min(text.len())).rev().find(|at| text.is_char_boundary(*at)).unwrap_or(0)
}

/// The word around a byte: letters and digits with the marks that join them.
pub fn word_at(text: &str, index: usize) -> Range<usize> {
    let index = floor(text, index);
    let part = |c: char| c.is_alphanumeric() || matches!(c, '-' | '_' | '\'' | '’');
    let start = text[..index].char_indices().rev().take_while(|(_, c)| part(*c)).last().map_or(index, |(at, _)| at);
    let end = index + text[index..].char_indices().find(|(_, c)| !part(*c)).map_or(text.len() - index, |(at, _)| at);
    // On a space or a mark: that one character.
    if start == end { return index..index + text[index..].chars().next().map_or(0, char::len_utf8) }
    start..end
}

impl Workspace {
    /// `text` drawn so that it can be selected. `key` tells this text from the others on show.
    pub fn selectable(&self, key: impl Into<SharedString>, text: impl Into<SharedString>, cx: &mut Context<Self>) -> Div {
        let (key, text): (SharedString, SharedString) = (key.into(), text.into());
        let selected = self.picked.as_ref().filter(|picked| picked.key == key && picked.text == text).map(Picked::range).filter(|range| !range.is_empty());
        let styled = StyledText::new(text.clone()).with_highlights(selected.map(|range| (range, HighlightStyle { background_color: Some(Theme::selection()), ..Default::default() })));
        let layout = styled.layout().clone();
        div()
            .cursor_text()
            .on_mouse_down(MouseButton::Left, cx.listener({
                let (key, text, layout) = (key.clone(), text.clone(), layout.clone());
                move |this, event: &MouseDownEvent, window, cx| {
                    let at = index_at(&layout, &text, event.position);
                    let (range, held) = match event.click_count {
                        0 | 1 => (at..at, true),
                        2 => (word_at(&text, at), false),
                        _ => (0..text.len(), false),
                    };
                    // One selection in the window: an answer's goes. The keyboard goes to the window, so that copying finds this one and not a text box.
                    if let Some(lib) = this.lib.as_mut() { (lib.paper_chat.picked, lib.library_chat.picked) = (None, None) }
                    window.focus(&this.focus);
                    this.picked = Some(Picked { key: key.clone(), text: text.clone(), anchor: range.start, head: range.end, held, layout: layout.clone() });
                    cx.notify();
                }
            }))
            // A click is over before the layer that follows the pointer is drawn (home.rs): the text hears the end of it itself.
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.pick_end(cx)))
            .on_mouse_down(MouseButton::Right, self.on_context(MenuKind::Text(text.to_string()), cx))
            .child(styled)
    }

    /// The pointer moved with the button held, anywhere in the window: the selection reaches to it.
    pub fn pick_drag(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if event.pressed_button != Some(MouseButton::Left) { return self.pick_end(cx) }
        let Some(picked) = self.picked.as_mut().filter(|picked| picked.held) else { return };
        let head = index_at(&picked.layout, &picked.text, event.position);
        if head != picked.head {
            picked.head = head;
            cx.notify();
        }
    }

    /// The button is up: a click that selected nothing leaves nothing.
    pub fn pick_end(&mut self, cx: &mut Context<Self>) {
        let Some(picked) = self.picked.as_mut().filter(|picked| picked.held) else { return };
        picked.held = false;
        if picked.range().is_empty() { self.picked = None }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that would bring in gpui's own `test` attribute.
    use super::word_at;

    #[test]
    fn words() {
        let text = "Single-cell RNA-seq（单细胞）of root, 2024.";
        assert_eq!(&text[word_at(text, 3)], "Single-cell");
        assert_eq!(&text[word_at(text, 0)], "Single-cell");
        // Just after a word: that word. On a mark between two others: the mark.
        assert_eq!(&text[word_at(text, 11)], "Single-cell");
        assert_eq!(&"a, b"[word_at("a, b", 2)], " ");
        // Inside a Chinese letter: the run it stands in.
        let at = text.find('细').unwrap() + 1;
        assert_eq!(&text[word_at(text, at)], "单细胞");
        assert_eq!(&text[word_at(text, text.len())], "");
        assert_eq!(&text[word_at(text, text.find("2024").unwrap() + 4)], "2024");
    }
}
