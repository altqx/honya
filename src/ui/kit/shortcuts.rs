//! The shortcuts bar along the bottom of every screen.
//!
//! The existing footer packs `(key, label)` pairs until they stop fitting and
//! then ellipsises. That is almost right, but it can trim away the binding that
//! tells you where the rest went — on a narrow terminal you are left with a
//! handful of arbitrary hints and no route to the full list. So two things are
//! reserved before anything else is packed:
//!
//! * **the help binding**, always, because it is the way out of not knowing;
//! * **a pinned hint**, when focus has been parked outside a modal that is still
//!   on screen, because it names the only way back into it.
//!
//! Everything else competes for what is left. Hints are also clickable, which
//! they have never been despite already being structured data with exact
//! column arithmetic behind them.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::ctx::Ui;
use super::style;
use super::zones::ZoneId;
use crate::ui::glyphs;
use crate::ui::text::col_width;

/// Columns between one hint and the next.
const GAP: usize = 3;
/// The most rows the bar will grow to before it starts trimming.
const MAX_ROWS: u16 = 3;

/// One key binding advertised in the bar.
#[derive(Debug, Clone)]
pub struct Hint {
    pub key: String,
    pub label: String,
    /// Registered when set, making the hint clickable.
    pub id: Option<ZoneId>,
}

impl Hint {
    pub fn new(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            id: None,
        }
    }

    pub fn id(mut self, id: ZoneId) -> Self {
        self.id = Some(id);
        self
    }

    /// Columns this hint occupies including its trailing gap.
    fn cols(&self) -> usize {
        col_width(&self.key) + 1 + col_width(&self.label) + GAP
    }
}

/// Build hints from the `(key, label)` pairs screens already produce.
pub fn hints_from(pairs: &[(&str, &str)]) -> Vec<Hint> {
    pairs.iter().map(|(k, l)| Hint::new(*k, *l)).collect()
}

/// The bar.
pub struct ShortcutsBar<'a> {
    pub hints: &'a [Hint],
    /// Reserved first and never dropped.
    pub help: Option<Hint>,
    /// Reserved second and never dropped — names the way back to a modal that
    /// focus has stepped out of.
    pub pinned: Option<Hint>,
}

impl<'a> ShortcutsBar<'a> {
    pub fn new(hints: &'a [Hint]) -> Self {
        Self {
            hints,
            help: None,
            pinned: None,
        }
    }

    pub fn help(mut self, hint: Hint) -> Self {
        self.help = Some(hint);
        self
    }

    pub fn pinned(mut self, hint: Option<Hint>) -> Self {
        self.pinned = hint;
        self
    }

    /// Columns the reserved hints claim on the last row.
    fn reserved_cols(&self) -> usize {
        self.help.as_ref().map(|h| h.cols()).unwrap_or(0)
            + self.pinned.as_ref().map(|h| h.cols()).unwrap_or(0)
    }

    /// How many leading hints fit in `budget` columns.
    fn pack(&self, from: usize, budget: usize) -> usize {
        let mut used = 1usize; // the leading space
        let mut n = 0usize;
        for h in &self.hints[from.min(self.hints.len())..] {
            if used + h.cols() > budget {
                break;
            }
            used += h.cols();
            n += 1;
        }
        n
    }

    /// Rows needed at `width`: one normally, growing to [`MAX_ROWS`] rather than
    /// trimming while there is still vertical room to spend.
    pub fn height(&self, width: u16) -> u16 {
        if width == 0 || self.hints.is_empty() {
            return 1;
        }
        let total = width as usize;
        let last_budget = total.saturating_sub(self.reserved_cols() + 1);

        if self.pack(0, last_budget) == self.hints.len() {
            return 1;
        }
        let mut placed = 0usize;
        for row in 1..MAX_ROWS {
            placed += self.pack(placed, total);
            if placed >= self.hints.len() {
                return row;
            }
            if self.pack(placed, last_budget) + placed >= self.hints.len() {
                return row + 1;
            }
        }
        MAX_ROWS
    }

    pub fn render(&self, ui: &mut Ui, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        ui.fill(area, Style::default().bg(ui.theme.bg));
        let total = area.width as usize;
        let rows = area.height.min(MAX_ROWS);
        let last_budget = total.saturating_sub(self.reserved_cols() + 1);

        let mut index = 0usize;
        // Every row above the last gets the full width.
        for r in 0..rows.saturating_sub(1) {
            let take = self.pack(index, total);
            self.draw_row(
                ui,
                Rect {
                    y: area.y + r,
                    height: 1,
                    ..area
                },
                index,
                take,
                false,
                false,
            );
            index += take;
        }

        // The last row shares its width with the reserved hints.
        let take = self.pack(index, last_budget);
        let trimmed = index + take < self.hints.len();
        self.draw_row(
            ui,
            Rect {
                y: area.y + rows - 1,
                height: 1,
                ..area
            },
            index,
            take,
            trimmed,
            true,
        );
    }

    /// Draw `count` hints starting at `from`. The reserved cluster is drawn
    /// only on the last row, where it is right-aligned.
    fn draw_row(
        &self,
        ui: &mut Ui,
        row: Rect,
        from: usize,
        count: usize,
        trimmed: bool,
        last: bool,
    ) {
        let reserved = if last { self.reserved_cols() } else { 0 };
        let mut x = row.x + 1;
        let right = row.x + row.width;

        for h in self.hints.iter().skip(from).take(count) {
            let w = (h.cols() - GAP) as u16;
            if x + w > right {
                break;
            }
            self.draw_hint(ui, strip(x, row.y, w), h);
            x += w + GAP as u16;
        }

        if trimmed && x + 1 < right {
            ui.text(
                strip(x, row.y, 1),
                glyphs::ELLIPSIS.as_str().to_string(),
                style::key_label(ui.theme).bg(ui.theme.bg),
            );
        }

        // The reserved cluster is right-aligned, so it sits in the same place
        // regardless of how much was packed before it.
        if reserved > 0 {
            let mut rx = right.saturating_sub(reserved as u16 - GAP as u16);
            for h in [self.pinned.as_ref(), self.help.as_ref()].into_iter().flatten() {
                let w = (h.cols() - GAP) as u16;
                if rx + w > right {
                    break;
                }
                self.draw_hint(ui, strip(rx, row.y, w), h);
                rx += w + GAP as u16;
            }
        }
    }

    fn draw_hint(&self, ui: &mut Ui, rect: Rect, hint: &Hint) {
        let st = match hint.id {
            Some(id) => ui.interactive(rect, id, false),
            None => super::style::State::IDLE,
        };
        let mut key = style::key_cap(ui.theme).bg(ui.theme.bg);
        let mut label = style::key_label(ui.theme).bg(ui.theme.bg);
        if st.hovered || st.focused {
            key = key.fg(ui.theme.accent);
            label = label.fg(ui.theme.ink_soft);
        }
        ui.line(
            rect,
            Line::from(vec![
                Span::styled(hint.key.clone(), key),
                Span::styled(" ", label),
                Span::styled(hint.label.clone(), label),
            ]),
            label,
        );
    }
}

fn strip(x: u16, y: u16, w: u16) -> Rect {
    Rect {
        x,
        y,
        width: w,
        height: 1,
    }
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

    fn help_hint() -> Hint {
        Hint::new("?", "help").id(ZoneId::hint(9999))
    }

    /// The Project screen's hint set: the widest in the app.
    fn project_hints() -> Vec<Hint> {
        hints_from(&[
            ("enter", "read"),
            ("space", "mark"),
            ("t/a", "queue"),
            ("T", "vol"),
            ("A", "all"),
            ("V", "add vol"),
            ("i", "add ch"),
            ("M", "images"),
            ("x", "export"),
            ("e", "title"),
            ("y", "synopsis"),
            ("d", "del"),
            ("h/l", "nav"),
            ("z/Z", "fold"),
            ("Q", "QA"),
        ])
    }

    fn paint(w: u16, bar: &ShortcutsBar<'_>) -> (Vec<String>, Zones) {
        let h = bar.height(w);
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
        term.draw(|f| {
            let metrics = Metrics::new(area, false);
            let mut ui = Ui::new(f, &mut zones, &theme, metrics, &focus, Hover::default(), 0);
            bar.render(&mut ui, area);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let lines = (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
            .collect();
        (lines, zones)
    }

    #[test]
    fn the_help_binding_survives_every_width() {
        // The whole point: however cramped the bar gets, the route to the full
        // list of bindings is still on screen.
        let hints = project_hints();
        for w in [20u16, 30, 40, 60, 80, 100, 160] {
            let bar = ShortcutsBar::new(&hints).help(help_hint());
            let (lines, zones) = paint(w, &bar);
            let joined = lines.join("\n");
            assert!(
                joined.contains("help"),
                "at {w} columns the escape hatch was trimmed away:\n{joined}"
            );
            assert!(
                zones.contains(ZoneId::hint(9999)),
                "at {w} the help hint was not clickable"
            );
        }
    }

    #[test]
    fn a_pinned_hint_also_survives_every_width() {
        // Focus parked outside a modal: this names the only way back into it.
        let hints = project_hints();
        let pin = Hint::new("tab", "question").id(ZoneId::hint(8888));
        for w in [24u16, 40, 80, 160] {
            let bar = ShortcutsBar::new(&hints)
                .help(help_hint())
                .pinned(Some(pin.clone()));
            let (lines, _) = paint(w, &bar);
            let joined = lines.join("\n");
            assert!(
                joined.contains("question"),
                "at {w} the way back to the modal was trimmed:\n{joined}"
            );
        }
    }

    #[test]
    fn a_roomy_bar_is_one_row_and_shows_everything() {
        let hints = project_hints();
        let bar = ShortcutsBar::new(&hints).help(help_hint());
        assert_eq!(bar.height(200), 1);
        let (lines, _) = paint(200, &bar);
        for needle in ["read", "export", "synopsis", "QA", "help"] {
            assert!(lines[0].contains(needle), "missing {needle}: {:?}", lines[0]);
        }
    }

    #[test]
    fn the_bar_grows_before_it_trims() {
        // Vertical room is cheaper than dropping a binding, so at a width where
        // one row cannot hold the Project set, the bar wraps.
        let hints = project_hints();
        let bar = ShortcutsBar::new(&hints).help(help_hint());
        assert!(bar.height(80) >= 2, "should wrap at 80 columns");
        assert!(bar.height(200) == 1, "should not wrap when it fits");
        assert!(bar.height(30) <= MAX_ROWS, "must not grow without bound");
    }

    #[test]
    fn hints_are_clickable_and_land_on_themselves() {
        let hints: Vec<Hint> = vec![
            Hint::new("enter", "read").id(ZoneId::hint(0)),
            Hint::new("d", "delete").id(ZoneId::hint(1)),
        ];
        let bar = ShortcutsBar::new(&hints).help(help_hint());
        let (_, zones) = paint(80, &bar);
        for i in 0..2 {
            let r = zones
                .rect_of(ZoneId::hint(i))
                .unwrap_or_else(|| panic!("hint {i} not registered"));
            assert_eq!(zones.at(r.x, r.y), Some(ZoneId::hint(i)));
        }
    }

    #[test]
    fn nothing_is_drawn_outside_the_bar_at_any_width() {
        let hints = project_hints();
        for w in [12u16, 20, 37, 64, 81, 120] {
            let bar = ShortcutsBar::new(&hints).help(help_hint());
            let (lines, zones) = paint(w, &bar);
            for l in &lines {
                assert_eq!(l.chars().count(), w as usize, "row overflowed at {w}");
            }
            let h = bar.height(w);
            for (rect, id) in zones.all() {
                assert!(
                    rect.x + rect.width <= w && rect.y + rect.height <= h,
                    "at {w}: {id:?} at {rect:?} escaped a {w}x{h} bar"
                );
            }
        }
    }

    #[test]
    fn the_reserved_cluster_is_drawn_once_not_on_every_row() {
        let hints = project_hints();
        let bar = ShortcutsBar::new(&hints).help(help_hint());
        let w = 80u16;
        assert!(bar.height(w) >= 2, "this test needs a wrapped bar");
        let (lines, _) = paint(w, &bar);
        let rows_with_help = lines.iter().filter(|l| l.contains("help")).count();
        assert_eq!(
            rows_with_help, 1,
            "the help binding should appear once, not on every row:\n{}",
            lines.join("\n")
        );
    }

    #[test]
    fn an_empty_hint_set_still_shows_the_way_out() {
        let bar = ShortcutsBar::new(&[]).help(help_hint());
        let (lines, _) = paint(40, &bar);
        assert!(lines[0].contains("help"), "got {:?}", lines[0]);
    }
}
