//! The paper's conversation as a window floating over its PDF: where the
//! window and its folded-up button may stand, how large the window may be,
//! and when a press on the button is a drag rather than a click. Plain
//! arithmetic in logical pixels, measured from the PDF area's top left; the
//! window draws whatever these give.

use serde::{Deserialize, Serialize};

/// The smallest window: a title bar, a few lines and the question box.
pub const MIN_WIDTH: f32 = 300.;
pub const MIN_HEIGHT: f32 = 280.;
/// The size a window opens at the first time, where the area has room.
pub const OPEN_WIDTH: f32 = 400.;
pub const OPEN_HEIGHT: f32 = 560.;
/// How far a window or button keeps from the area's edges.
pub const MARGIN: f32 = 12.;
/// The folded-up conversation: a round button this wide.
pub const BUTTON: f32 = 48.;
/// A press on the button that moves less than this is a click.
pub const DRAG_SLOP: f32 = 4.;

/// A window's place and size.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// What `settings.json` keeps of the floating conversation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Floating {
    /// The paper's conversation floats over the PDF instead of standing beside it.
    pub on: bool,
    /// Where the window was left; none until it is moved or resized.
    pub rect: Option<Rect>,
    /// Folded up into the round button.
    pub folded: bool,
    /// Where the button was left (its top left); none is the area's bottom right.
    pub button: Option<(f32, f32)>,
}

fn finite(value: f32, otherwise: f32) -> f32 {
    if value.is_finite() { value } else { otherwise }
}

/// Keep `at` (the start of something `size` long) within `0..area`, `MARGIN`
/// from either end where there is room, less of a margin where there is not.
fn within(at: f32, size: f32, area: f32) -> f32 {
    let margin = MARGIN.min(((area - size) / 2.).max(0.));
    let high = (area - size - margin).max(0.);
    finite(at, margin).clamp(margin.min(high), high)
}

/// The window as it is drawn in an area of this size: no smaller than the
/// least size, no larger than the area, and wholly inside it. The area
/// shrinking moves the window in and then makes it smaller; the stored
/// place is left as it was, so a window grown back finds it again.
pub fn fit(rect: Rect, area: (f32, f32)) -> Rect {
    let (area_w, area_h) = (area.0.max(0.), area.1.max(0.));
    let room = |area: f32, least: f32| (area - 2. * MARGIN).max(least.min(area));
    let width = finite(rect.width, OPEN_WIDTH).max(MIN_WIDTH).min(room(area_w, MIN_WIDTH));
    let height = finite(rect.height, OPEN_HEIGHT).max(MIN_HEIGHT).min(room(area_h, MIN_HEIGHT));
    Rect { x: within(rect.x, width, area_w), y: within(rect.y, height, area_h), width, height }
}

/// Where a window opens the first time: at the area's top right.
pub fn opening(area: (f32, f32)) -> Rect {
    fit(Rect { x: area.0 - OPEN_WIDTH - MARGIN, y: MARGIN, width: OPEN_WIDTH, height: OPEN_HEIGHT }, area)
}

/// The window on show: where it was left, or where it opens.
pub fn shown(stored: Option<Rect>, area: (f32, f32)) -> Rect {
    stored.map_or_else(|| opening(area), |rect| fit(rect, area))
}

/// The window held by its title bar and moved by `delta` since the press.
pub fn moved(start: Rect, delta: (f32, f32), area: (f32, f32)) -> Rect {
    fit(Rect { x: start.x + delta.0, y: start.y + delta.1, ..start }, area)
}

/// The window held by its bottom-right corner and pulled by `delta`: the
/// top left stays, the corner follows within the area.
pub fn resized(start: Rect, delta: (f32, f32), area: (f32, f32)) -> Rect {
    let width = (start.width + delta.0).min(area.0 - MARGIN - start.x);
    let height = (start.height + delta.1).min(area.1 - MARGIN - start.y);
    fit(Rect { width, height, ..start }, area)
}

/// The round button's top left in an area of this size.
pub fn button_at(stored: Option<(f32, f32)>, area: (f32, f32)) -> (f32, f32) {
    let (x, y) = stored.unwrap_or((area.0 - BUTTON - MARGIN - 8., area.1 - BUTTON - MARGIN - 8.));
    (within(x, BUTTON, area.0.max(0.)), within(y, BUTTON, area.1.max(0.)))
}

/// A press that went this far from where it began is a drag; a shorter one is a click.
pub fn is_drag(delta: (f32, f32)) -> bool {
    delta.0.hypot(delta.1) > DRAG_SLOP
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: (f32, f32) = (900., 700.);

    fn inside(rect: Rect, area: (f32, f32)) -> bool {
        rect.x >= 0. && rect.y >= 0. && rect.x + rect.width <= area.0 && rect.y + rect.height <= area.1
    }

    #[test]
    fn opens_at_the_top_right() {
        let rect = opening(AREA);
        assert_eq!((rect.width, rect.height), (OPEN_WIDTH, OPEN_HEIGHT));
        assert_eq!(rect.x + rect.width, AREA.0 - MARGIN);
        assert_eq!(rect.y, MARGIN);
        // A short area: the window is as tall as it allows.
        let short = opening((900., 400.));
        assert_eq!(short.height, 400. - 2. * MARGIN);
        assert!(inside(short, (900., 400.)));
    }

    #[test]
    fn a_move_stops_at_the_edges() {
        let start = Rect { x: 100., y: 100., width: 400., height: 500. };
        assert_eq!(moved(start, (30., -20.), AREA), Rect { x: 130., y: 80., ..start });
        let far = moved(start, (5000., 5000.), AREA);
        assert_eq!((far.x, far.y), (AREA.0 - 400. - MARGIN, AREA.1 - 500. - MARGIN));
        let back = moved(start, (-5000., -5000.), AREA);
        assert_eq!((back.x, back.y), (MARGIN, MARGIN));
        assert_eq!((back.width, back.height), (400., 500.));
    }

    #[test]
    fn a_resize_keeps_the_top_left_and_the_least_size() {
        let start = Rect { x: 100., y: 50., width: 400., height: 500. };
        assert_eq!(resized(start, (60., 40.), AREA), Rect { width: 460., height: 540., ..start });
        let small = resized(start, (-1000., -1000.), AREA);
        assert_eq!(small, Rect { width: MIN_WIDTH, height: MIN_HEIGHT, ..start });
        // Pulled past the area: the corner stays at its margin, the window does not move.
        let large = resized(start, (2000., 2000.), AREA);
        assert_eq!((large.x, large.y), (100., 50.));
        assert_eq!((large.x + large.width, large.y + large.height), (AREA.0 - MARGIN, AREA.1 - MARGIN));
    }

    #[test]
    fn a_smaller_area_brings_the_window_back_in() {
        let left = Rect { x: 480., y: 120., width: 400., height: 560. };
        let fitted = fit(left, (600., 500.));
        assert!(inside(fitted, (600., 500.)));
        assert_eq!(fitted.width, 400.);
        assert_eq!(fitted.height, 500. - 2. * MARGIN);
        // Narrower than the least width: the least width, from the margin, as far as it reaches.
        let tiny = fit(left, (200., 150.));
        assert_eq!((tiny.width, tiny.height), (200., 150.));
        assert_eq!((tiny.x, tiny.y), (0., 0.));
        // Nonsense from a hand-edited file is not drawn as it is.
        let odd = fit(Rect { x: f32::NAN, y: f32::INFINITY, width: f32::NAN, height: -3. }, AREA);
        assert!(inside(odd, AREA));
        assert_eq!((odd.width, odd.height), (OPEN_WIDTH, MIN_HEIGHT));
    }

    #[test]
    fn the_button_stays_in_the_area() {
        let corner = button_at(None, AREA);
        assert_eq!(corner, (AREA.0 - BUTTON - MARGIN - 8., AREA.1 - BUTTON - MARGIN - 8.));
        assert_eq!(button_at(Some((-50., 5000.)), AREA), (MARGIN, AREA.1 - BUTTON - MARGIN));
        assert_eq!(button_at(Some((200., 300.)), AREA), (200., 300.));
    }

    #[test]
    fn a_short_press_is_a_click() {
        assert!(!is_drag((0., 0.)));
        assert!(!is_drag((3., 2.)));
        assert!(is_drag((4., 3.)));
        assert!(is_drag((-10., 0.)));
    }

    #[test]
    fn settings_read_without_the_section() {
        let floating: Floating = serde_json::from_str("{}").unwrap();
        assert_eq!(floating, Floating::default());
        let kept = Floating { on: true, rect: Some(Rect { x: 1., y: 2., width: 300., height: 400. }), folded: true, button: Some((5., 6.)) };
        assert_eq!(serde_json::from_str::<Floating>(&serde_json::to_string(&kept).unwrap()).unwrap(), kept);
    }
}
