//! Mouse plumbing shared by the App, its screens, and its overlays.
//!
//! Raw crossterm [`MouseEvent`]s are normalized into a small [`MouseInput`] (a
//! gesture at a cell) before they reach any UI code. Motion / drag / button-up
//! and the horizontal-scroll kinds are dropped here so handlers only ever see the
//! four gestures they act on. Double-click timing lives in `App` (it owns the
//! clock); this module is otherwise pure geometry so it stays unit-testable.

use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

/// A normalized mouse gesture the UI reacts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseGesture {
    /// Left button pressed; `double` is set on a quick second press at the same cell.
    Click { double: bool },
    /// Right button pressed — used everywhere as "back / dismiss".
    RightClick,
    /// Wheel up (away from the user).
    ScrollUp,
    /// Wheel down (towards the user).
    ScrollDown,
}

/// A gesture located at a terminal cell (absolute, frame coordinates).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseInput {
    pub gesture: MouseGesture,
    pub col: u16,
    pub row: u16,
}

impl MouseInput {
    /// Translate a raw crossterm event into a [`MouseInput`], or `None` for the
    /// kinds the UI ignores (motion, drag, button-up, middle/horizontal). The
    /// caller supplies `double` because double-click detection needs a clock.
    pub fn from_event(ev: &MouseEvent, double: bool) -> Option<Self> {
        let gesture = match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => MouseGesture::Click { double },
            MouseEventKind::Down(MouseButton::Right) => MouseGesture::RightClick,
            MouseEventKind::ScrollUp => MouseGesture::ScrollUp,
            MouseEventKind::ScrollDown => MouseGesture::ScrollDown,
            _ => return None,
        };
        Some(Self {
            gesture,
            col: ev.column,
            row: ev.row,
        })
    }

    /// True when this gesture is a wheel scroll (either direction).
    pub fn is_scroll(self) -> bool {
        matches!(
            self.gesture,
            MouseGesture::ScrollUp | MouseGesture::ScrollDown
        )
    }

    /// True when this gesture is a left click (single or double).
    pub fn is_click(self) -> bool {
        matches!(self.gesture, MouseGesture::Click { .. })
    }

    /// True when this is a double left click.
    pub fn is_double(self) -> bool {
        matches!(self.gesture, MouseGesture::Click { double: true })
    }

    /// True when the gesture lands inside `rect`.
    pub fn in_rect(self, rect: Rect) -> bool {
        hit(rect, self.col, self.row)
    }
}

/// True when cell `(col, row)` lies inside `rect` (empty rects never hit).
pub fn hit(rect: Rect, col: u16, row: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && col >= rect.x
        && col < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

/// Map a click `row` inside a list of uniform 1-line rows to its item index,
/// honoring the widget's scroll `offset`. `None` when the row is outside `area`
/// or past the last item.
pub fn row_index(area: Rect, offset: usize, len: usize, row: u16) -> Option<usize> {
    if row < area.y || row >= area.y.saturating_add(area.height) {
        return None;
    }
    let idx = offset + (row - area.y) as usize;
    (idx < len).then_some(idx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyModifiers;

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    fn ev(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::empty(),
        }
    }

    #[test]
    fn hit_respects_bounds_and_empty_rects() {
        let r = rect(2, 3, 4, 2); // covers x 2..6, y 3..5
        assert!(hit(r, 2, 3));
        assert!(hit(r, 5, 4));
        assert!(!hit(r, 6, 4), "x past the right edge misses");
        assert!(!hit(r, 3, 5), "y past the bottom edge misses");
        assert!(!hit(r, 1, 3), "x left of the rect misses");
        assert!(!hit(rect(0, 0, 0, 5), 0, 0), "zero-width never hits");
    }

    #[test]
    fn row_index_accounts_for_offset_and_length() {
        let area = rect(0, 5, 10, 4); // 4 visible rows starting at y=5
        // Offset 0: row 5 → item 0, row 8 → item 3.
        assert_eq!(row_index(area, 0, 20, 5), Some(0));
        assert_eq!(row_index(area, 0, 20, 8), Some(3));
        // Scrolled down by 2: the top visible row is item 2.
        assert_eq!(row_index(area, 2, 20, 5), Some(2));
        assert_eq!(row_index(area, 2, 20, 8), Some(5));
        // Past the last item (len 3) → None even though the cell is in-area.
        assert_eq!(row_index(area, 0, 3, 8), None);
        // Outside the area entirely → None.
        assert_eq!(row_index(area, 0, 20, 4), None);
        assert_eq!(row_index(area, 0, 20, 9), None);
    }

    #[test]
    fn from_event_normalizes_only_the_four_gestures() {
        let single =
            MouseInput::from_event(&ev(MouseEventKind::Down(MouseButton::Left), 3, 7), false)
                .unwrap();
        assert_eq!(single.gesture, MouseGesture::Click { double: false });
        assert_eq!((single.col, single.row), (3, 7));
        assert!(single.is_click() && !single.is_double());

        let dbl = MouseInput::from_event(&ev(MouseEventKind::Down(MouseButton::Left), 3, 7), true)
            .unwrap();
        assert!(dbl.is_double());

        assert_eq!(
            MouseInput::from_event(&ev(MouseEventKind::Down(MouseButton::Right), 0, 0), false)
                .unwrap()
                .gesture,
            MouseGesture::RightClick
        );
        assert!(
            MouseInput::from_event(&ev(MouseEventKind::ScrollUp, 0, 0), false)
                .unwrap()
                .is_scroll()
        );
        // Ignored kinds yield nothing.
        assert!(MouseInput::from_event(&ev(MouseEventKind::Moved, 0, 0), false).is_none());
        assert!(
            MouseInput::from_event(&ev(MouseEventKind::Up(MouseButton::Left), 0, 0), false)
                .is_none()
        );
        assert!(
            MouseInput::from_event(&ev(MouseEventKind::Drag(MouseButton::Left), 0, 0), false)
                .is_none()
        );
    }
}
