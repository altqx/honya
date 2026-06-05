//! Standard screen skeleton split + centered overlay helpers.
//!
//! Collapsing the toast row to zero height when nothing is showing keeps the
//! body from jumping by a line as toasts come and go.

use ratatui::layout::{Constraint, Flex, Layout, Rect};

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
/// unless `show_toast`, so the body reclaims that line when hidden.
pub fn skeleton(area: Rect, show_toast: bool) -> Skeleton {
    let toast_h = if show_toast { 1 } else { 0 };
    let [header, tabs, rule, body, toast, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(toast_h),
        Constraint::Length(1),
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

/// A `w`×`h` rectangle centered inside `area`; size is clamped to `area` so a
/// modal never exceeds its host.
pub fn centered_modal(w: u16, h: u16, area: Rect) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    let [horiz] = Layout::horizontal([Constraint::Length(w)])
        .flex(Flex::Center)
        .areas(area);
    let [out] = Layout::vertical([Constraint::Length(h)])
        .flex(Flex::Center)
        .areas(horiz);
    out
}

/// A percentage-sized rectangle centered inside `area`; `pct_w`/`pct_h` are
/// 0–100 and values above 100 simply fill the axis.
pub fn centered_pct(pct_w: u16, pct_h: u16, area: Rect) -> Rect {
    let [horiz] = Layout::horizontal([Constraint::Percentage(pct_w)])
        .flex(Flex::Center)
        .areas(area);
    let [out] = Layout::vertical([Constraint::Percentage(pct_h)])
        .flex(Flex::Center)
        .areas(horiz);
    out
}
