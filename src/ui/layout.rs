//! src/ui/layout.rs — the standard screen skeleton split + centered overlay helpers.
//!
//! Every primary screen lays out as six stacked rows: a one-line header, the tab
//! bar, a hairline rule, the flexible body, an optional toast line, and the
//! footer hint bar. Collapsing the toast row to zero height when nothing is
//! showing keeps the body from jumping by a line as toasts come and go.

use ratatui::layout::{Constraint, Flex, Layout, Rect};

/// The six regions every primary screen is composed from.
///
/// `toast` has zero height when `show_toast` was false; rendering into a
/// zero-area `Rect` is a no-op so callers can render unconditionally.
#[derive(Debug, Clone, Copy)]
pub struct Skeleton {
    /// Row 0: breadcrumb + right-aligned status tally.
    pub header: Rect,
    /// Row 1: primary tab bar (書架/棚/訳/読/辞).
    pub tabs: Rect,
    /// Row 2: full-width hairline rule under the tabs.
    pub rule: Rect,
    /// Flexible middle: the screen's actual content.
    pub body: Rect,
    /// One-line toast / status message (height 0 when hidden).
    pub toast: Rect,
    /// Bottom: data-driven footer hints + global cluster.
    pub footer: Rect,
}

/// Split `area` into the standard six-row [`Skeleton`].
///
/// Layout is `[Length(1), Length(1), Length(1), Min(0), toast, Length(1)]`
/// where the toast row is `Length(1)` when `show_toast` and `Length(0)`
/// otherwise — so the body reclaims that line when no toast is visible.
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

/// A `w`×`h` rectangle centered inside `area` via `Flex::Center`.
///
/// The requested size is clamped to `area` (a modal never exceeds its host),
/// so callers can pass generous dimensions and trust they fit on small terminals.
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

/// A percentage-sized rectangle centered inside `area` via `Flex::Center`.
///
/// `pct_w` / `pct_h` are 0–100; values above 100 simply fill the axis.
pub fn centered_pct(pct_w: u16, pct_h: u16, area: Rect) -> Rect {
    let [horiz] = Layout::horizontal([Constraint::Percentage(pct_w)])
        .flex(Flex::Center)
        .areas(area);
    let [out] = Layout::vertical([Constraint::Percentage(pct_h)])
        .flex(Flex::Center)
        .areas(horiz);
    out
}
