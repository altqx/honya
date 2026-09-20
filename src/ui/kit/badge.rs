//! Small status marks: dots, chips and badges.
//!
//! These are the app's smallest controls, not decoration: the header's status
//! tally filters by what it counts, the Reader's chips flip the mode they name.
//! So they register zones like anything else.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::ctx::Ui;
use super::style::{self, State};
use super::zones::ZoneId;
use crate::ui::glyphs::{self, Glyph};
use crate::ui::text::col_width;

/// A glyph-and-count pair, as the header tally uses.
pub struct StatusDot {
    pub glyph: Glyph,
    pub count: u32,
    pub color: Color,
    /// Registered when set, making the dot a filter control rather than a
    /// readout.
    pub id: Option<ZoneId>,
    /// Draw at full strength even when the count is zero. Off by default, so an
    /// empty failure count does not read as an alarm.
    pub emphasize_zero: bool,
}

impl StatusDot {
    pub fn new(glyph: Glyph, count: u32, color: Color) -> Self {
        Self {
            glyph,
            count,
            color,
            id: None,
            emphasize_zero: false,
        }
    }

    pub fn id(mut self, id: ZoneId) -> Self {
        self.id = Some(id);
        self
    }

    pub fn emphasize_zero(mut self, yes: bool) -> Self {
        self.emphasize_zero = yes;
        self
    }

    pub fn width(&self) -> u16 {
        self.glyph.cols() + count_cols(self.count)
    }

    pub fn render(&self, ui: &mut Ui, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let rect = Rect {
            width: self.width().min(area.width),
            height: 1,
            ..area
        };
        let st = match self.id {
            Some(id) => ui.interactive(rect, id, false),
            None => State::IDLE,
        };
        // A zero count is stated, not shouted: it keeps its glyph but drops to
        // the faint ink so an empty failure column is not a false alarm.
        let fg = if self.count == 0 && !self.emphasize_zero {
            ui.theme.ink_faint
        } else {
            self.color
        };
        let mut sty = Style::default().fg(fg).bg(ui.surface_of(st));
        if st.hovered || st.focused {
            sty = sty.add_modifier(Modifier::BOLD);
        }
        ui.line(
            rect,
            Line::from(vec![Span::styled(
                format!("{}{}", self.glyph.as_str(), self.count),
                sty,
            )]),
            sty,
        );
    }
}

fn count_cols(n: u32) -> u16 {
    if n == 0 {
        1
    } else {
        n.ilog10() as u16 + 1
    }
}

/// A small labelled control that carries an on/off state — the Reader's wrap and
/// sync toggles, a filter tag.
pub struct Chip {
    pub id: ZoneId,
    pub label: String,
    pub on: bool,
    /// Show a leading state glyph. Off for chips whose label alone says it.
    pub show_state: bool,
    pub disabled: bool,
}

impl Chip {
    pub fn new(id: ZoneId, label: impl Into<String>, on: bool) -> Self {
        Self {
            id,
            label: label.into(),
            on,
            show_state: true,
            disabled: false,
        }
    }

    pub fn plain(mut self) -> Self {
        self.show_state = false;
        self
    }

    pub fn disabled(mut self, yes: bool) -> Self {
        self.disabled = yes;
        self
    }

    pub fn width(&self) -> u16 {
        let mut w = col_width(&self.label) as u16 + 2;
        if self.show_state {
            w += glyphs::TOGGLE_ON.cols() + 1;
        }
        w
    }

    pub fn render(&self, ui: &mut Ui, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let rect = Rect {
            width: self.width().min(area.width),
            height: 1,
            ..area
        };
        let st = if self.disabled {
            ui.hit_only(rect, self.id).with_disabled(true)
        } else {
            ui.interactive(rect, self.id, self.on)
        };

        let base = if self.disabled {
            Style::default()
                .fg(ui.theme.ink_faint)
                .bg(ui.surface())
                .add_modifier(Modifier::DIM)
        } else if self.on {
            style::filled(ui.theme)
        } else {
            ui.row_style(st)
        };

        let mut spans = vec![Span::styled(" ", base)];
        if self.show_state {
            let g = if self.on {
                glyphs::TOGGLE_ON
            } else {
                glyphs::TOGGLE_OFF
            };
            spans.push(Span::styled(format!("{} ", g.as_str()), base));
        }
        spans.push(Span::styled(self.label.clone(), base));
        spans.push(Span::styled(" ", base));
        ui.line(rect, Line::from(spans), base);
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

    fn paint(w: u16, draw: impl FnOnce(&mut Ui, Rect)) -> (String, Zones) {
        let theme = ThemeId::Washi.build();
        let mut zones = Zones::new();
        let focus = Focus::new();
        let mut term = Terminal::new(TestBackend::new(w, 1)).unwrap();
        let area = Rect {
            x: 0,
            y: 0,
            width: w,
            height: 1,
        };
        term.draw(|f| {
            let metrics = Metrics::new(area, false);
            let mut ui = Ui::new(f, &mut zones, &theme, metrics, &focus, Hover::default(), 0);
            draw(&mut ui, area);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = (0..w).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        (text, zones)
    }

    #[test]
    fn count_width_tracks_the_number_of_digits() {
        assert_eq!(count_cols(0), 1);
        assert_eq!(count_cols(9), 1);
        assert_eq!(count_cols(10), 2);
        assert_eq!(count_cols(999), 3);
        assert_eq!(count_cols(1000), 4);
        assert_eq!(count_cols(u32::MAX), 10);
    }

    #[test]
    fn a_dot_claims_exactly_the_columns_it_prints() {
        for count in [0u32, 7, 42, 1234] {
            let dot = StatusDot::new(glyphs::MOON_FULL, count, Color::Green).id(ZoneId::row(0));
            let (text, zones) = paint(20, |ui, area| dot.render(ui, area));
            let printed = format!("{}{}", glyphs::MOON_FULL.as_str(), count);
            assert!(text.contains(&printed), "got {text:?}");
            let r = zones.rect_of(ZoneId::row(0)).expect("registered");
            assert_eq!(
                r.width,
                col_width(&printed) as u16,
                "the click target must match what was printed for {count}"
            );
        }
    }

    #[test]
    fn a_readout_dot_registers_nothing() {
        let dot = StatusDot::new(glyphs::CROSS, 3, Color::Red);
        let (_, zones) = paint(20, |ui, area| dot.render(ui, area));
        assert!(zones.is_empty(), "a dot with no id is not a control");
    }

    #[test]
    fn a_chip_reports_and_registers_its_state() {
        for on in [false, true] {
            let chip = Chip::new(ZoneId::button(1), "wrap", on);
            let (text, zones) = paint(20, |ui, area| chip.render(ui, area));
            assert!(text.contains("wrap"), "got {text:?}");
            let r = zones.rect_of(ZoneId::button(1)).expect("registered");
            assert_eq!(r.width, chip.width());
            // Every column of the chip is clickable.
            for x in r.x..r.x + r.width {
                assert_eq!(zones.at(x, 0), Some(ZoneId::button(1)));
            }
        }
    }

    #[test]
    fn a_chip_keeps_its_width_when_toggled() {
        // Otherwise a toolbar would reflow every time a toggle is clicked.
        let off = Chip::new(ZoneId::button(0), "sync", false).width();
        let on = Chip::new(ZoneId::button(0), "sync", true).width();
        assert_eq!(off, on);
    }

    #[test]
    fn a_disabled_chip_is_clickable_but_unfocusable() {
        let chip = Chip::new(ZoneId::button(2), "diff", false).disabled(true);
        let (_, zones) = paint(20, |ui, area| chip.render(ui, area));
        assert!(zones.contains(ZoneId::button(2)));
        assert!(!zones.tab_order().any(|z| z == ZoneId::button(2)));
    }

    #[test]
    fn nothing_is_drawn_into_a_collapsed_area() {
        let (_, zones) = paint(0, |ui, area| {
            StatusDot::new(glyphs::MOON_FULL, 1, Color::Green)
                .id(ZoneId::row(0))
                .render(ui, area);
            Chip::new(ZoneId::button(0), "x", true).render(ui, area);
        });
        assert!(zones.is_empty());
    }
}
