//! A scrolling, selectable list that registers a zone per visible row.
//!
//! Every screen currently reimplements this: windowing the data to the
//! viewport, keeping the selection visible, drawing a selection bar, and
//! separately recovering which row a click landed on. Doing it once removes
//! that duplication and, because rows register as they draw, removes the second
//! copy of the arithmetic entirely.
//!
//! Rows are built through a closure that is **only called for visible indices**,
//! so a hundred-thousand-line activity log costs the same as a ten-item menu.
//! That matters: honya's lists span both extremes.

use std::ops::Range;

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::ctx::Ui;
use super::style::State;
use super::tokens::SCROLLBAR_COLS;
use super::zones::{ZoneId, ZoneKind};
use crate::ui::glyphs;

/// Selection and scroll position. Owned by the screen, not the component, so it
/// survives across frames.
#[derive(Debug, Clone, Default)]
pub struct ListState {
    selected: Option<usize>,
    offset: usize,
    /// Stick to the end as items are appended — the activity log and the live
    /// translation preview both want this.
    follow: bool,
}

impl ListState {
    pub fn new() -> Self {
        Self::default()
    }

    /// A list that starts pinned to its tail.
    pub fn following() -> Self {
        Self {
            follow: true,
            ..Self::default()
        }
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub fn offset(&self) -> usize {
        self.offset
    }

    pub fn is_following(&self) -> bool {
        self.follow
    }

    pub fn set_follow(&mut self, follow: bool) {
        self.follow = follow;
    }

    pub fn select(&mut self, index: Option<usize>) {
        self.selected = index;
    }

    /// Move the selection by `delta`, clamped to `len`. Selecting anything by
    /// hand drops follow mode — the user has taken over.
    pub fn move_by(&mut self, delta: isize, len: usize) {
        if len == 0 {
            self.selected = None;
            return;
        }
        self.follow = false;
        let cur = self.selected.unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, len as isize - 1) as usize;
        self.selected = Some(next);
    }

    pub fn select_first(&mut self, len: usize) {
        self.follow = false;
        self.selected = (len > 0).then_some(0);
    }

    pub fn select_last(&mut self, len: usize) {
        self.follow = false;
        self.selected = len.checked_sub(1);
    }

    /// Scroll without moving the selection, for the wheel.
    pub fn scroll_by(&mut self, delta: isize, viewport: u16, len: usize) {
        let max = max_offset(viewport, len);
        let next = (self.offset as isize + delta).clamp(0, max as isize) as usize;
        self.offset = next;
        // Scrolling away from the tail means the user wants to read, not follow.
        if delta < 0 {
            self.follow = false;
        } else if next == max {
            self.follow = true;
        }
    }

    /// The visible index range for a viewport of `viewport` rows over `len`
    /// items, after settling the offset so the selection stays on screen.
    pub fn window(&mut self, viewport: u16, len: usize) -> Range<usize> {
        let view = viewport as usize;
        if view == 0 || len == 0 {
            self.offset = 0;
            return 0..0;
        }
        let max = max_offset(viewport, len);
        if self.follow {
            self.offset = max;
        } else if let Some(sel) = self.selected {
            // Scroll the minimum needed to bring the selection into view.
            if sel < self.offset {
                self.offset = sel;
            } else if sel >= self.offset + view {
                self.offset = sel + 1 - view;
            }
        }
        self.offset = self.offset.min(max);
        let end = (self.offset + view).min(len);
        self.offset..end
    }
}

/// The largest offset that still fills the viewport.
fn max_offset(viewport: u16, len: usize) -> usize {
    len.saturating_sub(viewport as usize)
}

/// One rendered row.
pub struct Row {
    pub line: Line<'static>,
    /// Rows that cannot be picked — section headers interleaved in a list.
    pub selectable: bool,
}

impl Row {
    pub fn new(line: Line<'static>) -> Self {
        Self {
            line,
            selectable: true,
        }
    }

    /// A non-selectable divider or heading.
    pub fn header(line: Line<'static>) -> Self {
        Self {
            line,
            selectable: false,
        }
    }
}

/// How a list decorates its rows.
#[derive(Debug, Clone, Copy)]
pub struct Opts {
    /// Draw the left rail for selected, focused and hovered rows.
    pub rail: bool,
    /// Reserve a column on the right for the scrollbar when content overflows.
    pub scrollbar: bool,
    /// Which zone kind rows register under, so several lists on one screen do
    /// not collide.
    pub kind: ZoneKind,
    /// Offset added to a row's index when forming its `ZoneId`, for a screen
    /// with more than one list of the same kind.
    pub id_base: u32,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            rail: true,
            scrollbar: true,
            kind: ZoneKind::Row,
            id_base: 0,
        }
    }
}

impl Opts {
    fn id_for(&self, index: usize) -> ZoneId {
        ZoneId::new(self.kind, self.id_base + index as u32)
    }
}

/// Render `len` items into `area`, building only the visible ones.
///
/// Returns the range actually drawn, which a caller occasionally needs (the
/// Reader pairs it with a gutter).
pub fn render<F>(
    ui: &mut Ui,
    area: Rect,
    state: &mut ListState,
    len: usize,
    opts: Opts,
    mut row_at: F,
) -> Range<usize>
where
    F: FnMut(usize) -> Row,
{
    if area.width == 0 || area.height == 0 {
        return 0..0;
    }
    let overflowing = len > area.height as usize;
    let bar_cols = if opts.scrollbar && overflowing {
        SCROLLBAR_COLS
    } else {
        0
    };
    let body = Rect {
        width: area.width.saturating_sub(bar_cols),
        ..area
    };
    let win = state.window(body.height, len);

    ui.fill(area, Style::default().bg(ui.surface()));

    let rail_cols: u16 = if opts.rail { 1 } else { 0 };
    for (n, index) in win.clone().enumerate() {
        let y = body.y + n as u16;
        let row_rect = Rect {
            x: body.x,
            y,
            width: body.width,
            height: 1,
        };
        let row = row_at(index);
        let id = opts.id_for(index);

        let st = if row.selectable {
            ui.interactive(row_rect, id, state.selected == Some(index))
        } else {
            State::IDLE
        };

        let base = ui.row_style(st);
        ui.fill(row_rect, base);

        let mut x = row_rect.x;
        if rail_cols > 0 {
            let (glyph, rail_style) = match ui.rail_of(st) {
                Some(r) => (r.0.as_str().to_string(), r.1),
                None => (" ".to_string(), base),
            };
            ui.line(
                Rect {
                    x,
                    y,
                    width: rail_cols,
                    height: 1,
                },
                Line::from(Span::styled(glyph, rail_style)),
                base,
            );
            x += rail_cols;
        }

        let text_rect = Rect {
            x,
            y,
            width: row_rect.width.saturating_sub(rail_cols),
            height: 1,
        };
        ui.line(text_rect, row.line, base);
    }

    if bar_cols > 0 {
        render_scrollbar(
            ui,
            Rect {
                x: area.x + area.width - bar_cols,
                y: area.y,
                width: bar_cols,
                height: area.height,
            },
            len,
            state.offset,
        );
    }
    win
}

/// A slim scrollbar whose thumb is proportional to the visible fraction and
/// registers a zone, so it can be dragged rather than only looked at.
pub fn render_scrollbar(ui: &mut Ui, area: Rect, len: usize, offset: usize) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let view = area.height as usize;
    if len <= view {
        return;
    }
    let track = glyphs::SCROLL_TRACK.as_str();
    let thumb = glyphs::SCROLL_THUMB.as_str();
    let max_off = len - view;

    // At least one row, so the thumb never vanishes on a very long list.
    let thumb_h = ((view * view) / len).max(1).min(view);
    let span = view - thumb_h;
    // `max_off` is at least 1: the early return above covers `len <= view`,
    // so the division needs no guard. Rounded to nearest so the thumb reaches
    // both ends of the track rather than stopping a row short at the bottom.
    let thumb_y = (offset * span + max_off / 2) / max_off;

    let track_style = Style::default().fg(ui.theme.rule).bg(ui.surface());
    let thumb_style = Style::default().fg(ui.theme.ink_soft).bg(ui.surface());
    for n in 0..view {
        let on_thumb = n >= thumb_y && n < thumb_y + thumb_h;
        let cell = Rect {
            x: area.x,
            y: area.y + n as u16,
            width: area.width,
            height: 1,
        };
        let (g, st) = if on_thumb {
            (thumb, thumb_style)
        } else {
            (track, track_style)
        };
        ui.line(cell, Line::from(Span::styled(g.to_string(), st)), st);
    }
    ui.hit_only(area, ZoneId::new(ZoneKind::ScrollThumb, 0));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ThemeId;
    use crate::ui::kit::focus::{Focus, Hover};
    use crate::ui::kit::tokens::Metrics;
    use crate::ui::kit::zones::Zones;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Render a list of `len` numbered rows into `w`x`h` and return the painted
    /// lines plus the zones registered.
    fn paint(
        w: u16,
        h: u16,
        state: &mut ListState,
        len: usize,
        opts: Opts,
    ) -> (Vec<String>, Zones, Range<usize>) {
        let theme = ThemeId::Washi.build();
        let mut zones = Zones::new();
        let focus = Focus::new();
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let area = Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        let mut win = 0..0;
        term.draw(|f| {
            let metrics = Metrics::new(area, false);
            let mut ui = Ui::new(f, &mut zones, &theme, metrics, &focus, Hover::default(), 0);
            win = render(&mut ui, area, state, len, opts, |i| {
                Row::new(Line::from(format!("item-{i}")))
            });
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let lines = (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
            .collect();
        (lines, zones, win)
    }

    #[test]
    fn only_visible_rows_are_built_and_each_registers_its_own_zone() {
        let mut st = ListState::new();
        st.select(Some(0));
        let (lines, zones, win) = paint(20, 4, &mut st, 100, Opts::default());

        assert_eq!(win, 0..4, "a 4-row viewport shows 4 of the 100");
        assert!(lines[0].contains("item-0"), "got {:?}", lines[0]);
        assert!(lines[3].contains("item-3"), "got {:?}", lines[3]);

        // Exactly the visible rows are registered — not all hundred.
        for i in 0..4 {
            assert!(zones.contains(ZoneId::row(i)), "row {i} missing");
        }
        assert!(!zones.contains(ZoneId::row(4)), "off-screen row registered");

        // And a click on each visible row resolves to that row.
        for i in 0..4u16 {
            assert_eq!(zones.at(2, i), Some(ZoneId::row(i as usize)));
        }
    }

    #[test]
    fn a_scrolled_list_registers_the_data_index_not_the_screen_row() {
        let mut st = ListState::new();
        st.select(Some(50));
        let (lines, zones, win) = paint(20, 4, &mut st, 100, Opts::default());

        assert_eq!(win, 47..51);
        assert!(lines[0].contains("item-47"), "got {:?}", lines[0]);
        // The top screen row must carry index 47, or clicks act on the wrong item.
        assert_eq!(zones.at(2, 0), Some(ZoneId::row(47)));
        assert_eq!(zones.at(2, 3), Some(ZoneId::row(50)));
    }

    #[test]
    fn a_scrollbar_appears_only_when_content_overflows() {
        let mut fits = ListState::new();
        let (_, zones, _) = paint(20, 10, &mut fits, 4, Opts::default());
        assert!(
            !zones.contains(ZoneId::new(ZoneKind::ScrollThumb, 0)),
            "no bar when everything is visible"
        );

        let mut overflows = ListState::new();
        let (_, zones, _) = paint(20, 4, &mut overflows, 100, Opts::default());
        assert!(zones.contains(ZoneId::new(ZoneKind::ScrollThumb, 0)));
    }

    #[test]
    fn two_lists_on_one_screen_do_not_collide() {
        // The id_base is what keeps a second list's row 0 from overwriting the
        // first list's row 0 in the registry.
        let mut a = ListState::new();
        let (_, zones_a, _) = paint(20, 3, &mut a, 10, Opts::default());
        let mut b = ListState::new();
        let (_, zones_b, _) = paint(
            20,
            3,
            &mut b,
            10,
            Opts {
                id_base: 1000,
                ..Opts::default()
            },
        );
        assert_eq!(zones_a.at(2, 0), Some(ZoneId::row(0)));
        assert_eq!(zones_b.at(2, 0), Some(ZoneId::row(1000)));
    }

    #[test]
    fn header_rows_are_drawn_but_never_selectable() {
        let theme = ThemeId::Washi.build();
        let mut zones = Zones::new();
        let focus = Focus::new();
        let mut st = ListState::new();
        let mut term = Terminal::new(TestBackend::new(20, 4)).unwrap();
        let area = Rect {
            x: 0,
            y: 0,
            width: 20,
            height: 4,
        };
        term.draw(|f| {
            let metrics = Metrics::new(area, false);
            let mut ui = Ui::new(f, &mut zones, &theme, metrics, &focus, Hover::default(), 0);
            render(&mut ui, area, &mut st, 4, Opts::default(), |i| {
                if i == 0 {
                    Row::header(Line::from("SECTION"))
                } else {
                    Row::new(Line::from(format!("item-{i}")))
                }
            });
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let top: String = (0..20).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(top.contains("SECTION"), "header should draw: {top:?}");
        assert!(
            !zones.contains(ZoneId::row(0)),
            "a header must not be clickable or focusable"
        );
        assert!(zones.contains(ZoneId::row(1)), "real rows still register");
    }

    #[test]
    fn a_list_narrower_than_its_content_still_stays_inside_its_area() {
        let mut st = ListState::new();
        let (lines, zones, _) = paint(8, 3, &mut st, 50, Opts::default());
        for l in &lines {
            assert_eq!(l.chars().count(), 8, "a row overflowed its width");
        }
        for (rect, id) in zones.all() {
            assert!(
                rect.x + rect.width <= 8 && rect.y + rect.height <= 3,
                "{id:?} at {rect:?} escaped the area"
            );
        }
    }

    #[test]
    fn a_window_that_fits_shows_everything() {
        let mut s = ListState::new();
        assert_eq!(s.window(10, 4), 0..4);
        assert_eq!(s.offset(), 0);
    }

    #[test]
    fn an_empty_or_collapsed_list_yields_an_empty_window() {
        let mut s = ListState::new();
        assert_eq!(s.window(10, 0), 0..0);
        assert_eq!(s.window(0, 100), 0..0);
    }

    #[test]
    fn the_window_scrolls_the_minimum_needed_to_reveal_the_selection() {
        let mut s = ListState::new();
        s.select(Some(0));
        assert_eq!(s.window(5, 100), 0..5);

        // Just past the bottom edge: scroll by exactly one.
        s.select(Some(5));
        assert_eq!(s.window(5, 100), 1..6);

        // Far below: the selection lands on the last visible row.
        s.select(Some(50));
        let w = s.window(5, 100);
        assert_eq!(w, 46..51);
        assert!(w.contains(&50));

        // Back above the top edge: the selection lands on the first row.
        s.select(Some(10));
        assert_eq!(s.window(5, 100), 10..15);
    }

    #[test]
    fn the_window_never_runs_past_the_end() {
        let mut s = ListState::new();
        s.select(Some(99));
        let w = s.window(10, 100);
        assert_eq!(w.end, 100);
        assert_eq!(w.len(), 10, "the viewport stays full at the tail");
    }

    #[test]
    fn a_shrinking_list_clamps_a_stale_offset() {
        let mut s = ListState::new();
        s.select(Some(90));
        s.window(10, 100);
        assert!(s.offset() > 0);
        // The list is replaced by a much shorter one.
        let w = s.window(10, 3);
        assert_eq!(w, 0..3, "a stale offset must not scroll past the new end");
        assert_eq!(s.offset(), 0);
    }

    #[test]
    fn follow_mode_sticks_to_the_tail_as_items_arrive() {
        let mut s = ListState::following();
        assert_eq!(s.window(5, 10), 5..10);
        assert_eq!(s.window(5, 50), 45..50, "still pinned as the log grows");
    }

    #[test]
    fn taking_over_the_selection_releases_follow_mode() {
        let mut s = ListState::following();
        s.window(5, 50);
        assert!(s.is_following());
        s.move_by(-1, 50);
        assert!(!s.is_following(), "the user has taken over");
    }

    #[test]
    fn scrolling_back_releases_follow_and_reaching_the_tail_restores_it() {
        let mut s = ListState::following();
        s.window(5, 50);
        s.scroll_by(-3, 5, 50);
        assert!(!s.is_following());
        s.scroll_by(100, 5, 50);
        assert!(s.is_following(), "back at the tail, resume following");
    }

    #[test]
    fn move_by_clamps_at_both_ends_without_wrapping() {
        let mut s = ListState::new();
        s.select(Some(0));
        s.move_by(-5, 10);
        assert_eq!(s.selected(), Some(0), "must not wrap to the end");
        s.move_by(100, 10);
        assert_eq!(s.selected(), Some(9), "must not wrap to the start");
    }

    #[test]
    fn an_empty_list_has_no_selection() {
        let mut s = ListState::new();
        s.select(Some(3));
        s.move_by(1, 0);
        assert_eq!(s.selected(), None);
        s.select_last(0);
        assert_eq!(s.selected(), None);
    }
}
