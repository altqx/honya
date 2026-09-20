//! Standard screen skeleton split + centered overlay helpers.
//!
//! Collapsing the toast row to zero height when nothing is showing keeps the
//! body from jumping by a line as toasts come and go.

use ratatui::layout::{Constraint, Layout, Rect};

/// The six regions every primary screen is composed from.
///
/// `toast` has zero height when hidden; rendering into a zero-area `Rect` is a
/// no-op so callers can render unconditionally.
#[derive(Debug, Clone, Copy)]
pub struct Skeleton {
    pub header: Rect,
    pub tabs: Rect,
    pub rule: Rect,
    pub body: Rect,
    pub toast: Rect,
    pub footer: Rect,
}

/// Split `area` into the standard six-row [`Skeleton`]; toast row is height 0
/// unless `show_toast`, so the body reclaims that line when hidden. `footer_h`
/// is 1 normally, or 2–3 when the hint bar wraps.
pub fn skeleton(area: Rect, show_toast: bool, footer_h: u16) -> Skeleton {
    let toast_h = if show_toast { 1 } else { 0 };
    let footer_h = footer_h.clamp(1, 3);
    let [header, tabs, rule, body, toast, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(toast_h),
        Constraint::Length(footer_h),
    ])
    .areas(area);

    Skeleton {
        header,
        tabs,
        rule,
        body,
        toast,
        footer,
    }
}

