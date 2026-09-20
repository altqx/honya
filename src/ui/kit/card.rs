//! Cards: a block set apart by a left accent rail and padding, not a box.
//!
//! A rail costs one column where a border costs two rows and two columns, and it
//! can carry state — accent when the pane is focused, faint when it is not, a
//! status colour when the block is reporting one.
//!
//! Modals keep their borders: there a hard edge is the point.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::ctx::Ui;
use super::style::State;
use super::tokens::GUTTER;
use super::zones::ZoneId;
use crate::ui::glyphs;
use crate::ui::text::{col_width, truncate_cols};

/// A titled block.
pub struct Card<'a> {
    pub title: Option<&'a str>,
    /// Right-aligned on the title row — a count, a status, a timestamp.
    pub meta: Option<String>,
    /// Overrides the rail colour, for a block reporting a status of its own.
    pub accent: Option<ratatui::style::Color>,
    /// Registered when set, making the whole card a click target.
    pub id: Option<ZoneId>,
    pub selected: bool,
    /// Draw the rail. Off for a card that fills its pane and needs no edge.
    pub rail: bool,
}

impl<'a> Default for Card<'a> {
    fn default() -> Self {
        Self {
            title: None,
            meta: None,
            accent: None,
            id: None,
            selected: false,
            rail: true,
        }
    }
}

impl<'a> Card<'a> {
    pub fn new(title: &'a str) -> Self {
        Self {
            title: Some(title),
            ..Self::default()
        }
    }

    pub fn meta(mut self, meta: impl Into<String>) -> Self {
        self.meta = Some(meta.into());
        self
    }

    pub fn accent(mut self, color: ratatui::style::Color) -> Self {
        self.accent = Some(color);
        self
    }

    /// Draw the card's frame and return the rect its content goes in.
    pub fn render(&self, ui: &mut Ui, area: Rect) -> Rect {
        if area.width == 0 || area.height == 0 {
            return area;
        }
        let st = match self.id {
            Some(id) => ui.interactive(area, id, self.selected),
            None => State::selected(self.selected),
        };
        let bg = ui.surface_of(st);
        ui.fill(area, Style::default().bg(bg));

        let rail_cols = if self.rail { GUTTER } else { 0 };
        if rail_cols > 0 {
            self.draw_rail(ui, area, st, bg);
        }

        let mut body = Rect {
            x: area.x + rail_cols + GUTTER,
            width: area.width.saturating_sub(rail_cols + GUTTER),
            ..area
        };

        if self.title.is_some() || self.meta.is_some() {
            self.draw_title(ui, body, st, bg);
            body.y = body.y.saturating_add(1);
            body.height = body.height.saturating_sub(1);
        }
        body
    }

    /// The rail: stronger when the card is focused or selected, faint at rest,
    /// and carrying the card's own accent when it has one.
    fn draw_rail(&self, ui: &mut Ui, area: Rect, st: State, bg: ratatui::style::Color) {
        let (glyph, color) = if st.focused {
            (glyphs::SELECT_BAR, ui.theme.border_focus)
        } else if st.selected {
            (glyphs::SELECT_BAR, self.accent.unwrap_or(ui.theme.accent))
        } else if st.hovered {
            (
                glyphs::ACCENT_RAIL_STRONG,
                self.accent.unwrap_or(ui.theme.accent_soft),
            )
        } else {
            (
                glyphs::ACCENT_RAIL,
                self.accent.unwrap_or(ui.theme.rule),
            )
        };
        let sty = Style::default().fg(color).bg(bg);
        for y in area.y..area.y + area.height {
            ui.line(
                Rect {
                    x: area.x,
                    y,
                    width: GUTTER,
                    height: 1,
                },
                Line::from(Span::styled(glyph.as_str().to_string(), sty)),
                sty,
            );
        }
    }

    fn draw_title(&self, ui: &mut Ui, body: Rect, st: State, bg: ratatui::style::Color) {
        if body.width == 0 {
            return;
        }
        let row = Rect {
            height: 1,
            ..body
        };
        let title_style = Style::default()
            .fg(if st.selected || st.focused {
                ui.theme.ink
            } else {
                ui.theme.ink_soft
            })
            .bg(bg)
            .add_modifier(Modifier::BOLD);
        let meta_style = Style::default().fg(ui.theme.ink_faint).bg(bg);

        let meta_cols = self
            .meta
            .as_ref()
            .map(|m| col_width(m) as u16 + 2)
            .unwrap_or(0);
        let title_budget = row.width.saturating_sub(meta_cols) as usize;
        let title = truncate_cols(self.title.unwrap_or(""), title_budget);
        let title_cols = col_width(&title) as u16;

        let mut spans = vec![Span::styled(title, title_style)];
        if let Some(m) = &self.meta {
            let gap = row.width.saturating_sub(title_cols + meta_cols) + 2;
            spans.push(Span::styled(" ".repeat(gap as usize), meta_style));
            spans.push(Span::styled(m.clone(), meta_style));
        }
        ui.line(row, Line::from(spans), Style::default().bg(bg));
    }
}

/// A full-width hairline, for the rare place a rule earns its row.
pub fn rule(ui: &mut Ui, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    ui.text(
        Rect {
            height: 1,
            ..area
        },
        glyphs::RULE_H.as_str().repeat(area.width as usize),
        Style::default().fg(ui.theme.rule).bg(ui.surface()),
    );
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

    fn paint(
        w: u16,
        h: u16,
        focus: &Focus,
        draw: impl FnOnce(&mut Ui, Rect) -> Rect,
    ) -> (Vec<String>, Zones, Rect) {
        let theme = ThemeId::Washi.build();
        let mut zones = Zones::new();
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let area = Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        let mut body = area;
        term.draw(|f| {
            let metrics = Metrics::new(area, false);
            let mut ui = Ui::new(f, &mut zones, &theme, metrics, focus, Hover::default(), 0);
            body = draw(&mut ui, area);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let lines = (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
            .collect();
        (lines, zones, body)
    }

    #[test]
    fn a_card_costs_one_column_of_rail_not_a_whole_border() {
        let (_, _, body) = paint(40, 6, &Focus::new(), |ui, area| {
            Card::default().render(ui, area)
        });
        // A bordered panel would lose a row top and bottom; a rail loses none.
        assert_eq!(body.height, 6, "a rail must not cost vertical space");
        assert_eq!(body.x, 2, "one column of rail plus one of padding");
        assert_eq!(body.width, 38);
    }

    #[test]
    fn a_title_takes_one_row_and_the_body_starts_below_it() {
        let (lines, _, body) = paint(40, 6, &Focus::new(), |ui, area| {
            Card::new("Chapters").meta("12 done").render(ui, area)
        });
        assert!(lines[0].contains("Chapters"), "got {:?}", lines[0]);
        assert!(lines[0].contains("12 done"), "meta missing: {:?}", lines[0]);
        assert_eq!(body.y, 1);
        assert_eq!(body.height, 5);
    }

    #[test]
    fn a_long_title_is_trimmed_rather_than_colliding_with_its_meta() {
        let (lines, _, _) = paint(24, 3, &Focus::new(), |ui, area| {
            Card::new("An extremely long chapter title that will not fit")
                .meta("99%")
                .render(ui, area)
        });
        assert_eq!(lines[0].chars().count(), 24, "title row overflowed");
        assert!(lines[0].contains("99%"), "meta must survive: {:?}", lines[0]);
    }

    #[test]
    fn a_collapsed_card_draws_nothing_and_does_not_panic() {
        for (w, h) in [(0u16, 5u16), (5, 0), (1, 1)] {
            let (_, zones, _) = paint(w.max(1), h.max(1), &Focus::new(), |ui, _| {
                Card::new("x").render(
                    ui,
                    Rect {
                        x: 0,
                        y: 0,
                        width: w,
                        height: h,
                    },
                )
            });
            for (rect, _) in zones.all() {
                assert!(rect.width > 0 && rect.height > 0);
            }
        }
    }

}
