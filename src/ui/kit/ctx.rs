//! The render context every component draws through.
//!
//! Bundles the four things a component always needs — somewhere to draw, the
//! registry to record itself in, the palette, and the current interaction state
//! — so a component's signature stays about what it *is* rather than what it
//! needs carried in. It also makes the kit's one rule structural: you cannot
//! draw through a [`Ui`] without having the [`Zones`] registry in hand.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::focus::{Focus, Hover};
use super::style::State;
use super::tokens::Metrics;
use super::zones::{ZoneId, Zones};
use crate::theme::Theme;

/// Everything a component draws with.
pub struct Ui<'a, 'f> {
    pub frame: &'a mut Frame<'f>,
    pub zones: &'a mut Zones,
    pub theme: &'a Theme,
    pub metrics: Metrics,
    /// A snapshot, not a handle: components read focus and hover to decide how
    /// to draw, and must never move either while rendering.
    focus: Option<ZoneId>,
    hover: Option<ZoneId>,
    /// Animation clock, for spinners.
    pub frame_count: u64,
    /// The background of the container currently being drawn into.
    ///
    /// A control has to know what it sits on. Everything used to assume the
    /// screen background, which is wrong the moment it is inside a modal — the
    /// row fills with one colour while the spans on it carry another, and the
    /// difference shows as a box around every piece of text. A modal sets this
    /// for its body; nested containers set it in turn.
    surface: ratatui::style::Color,
}

impl<'a, 'f> Ui<'a, 'f> {
    pub fn new(
        frame: &'a mut Frame<'f>,
        zones: &'a mut Zones,
        theme: &'a Theme,
        metrics: Metrics,
        focus: &Focus,
        hover: Hover,
        frame_count: u64,
    ) -> Self {
        Self {
            frame,
            zones,
            theme,
            metrics,
            focus: focus.get(),
            hover: hover.get(),
            frame_count,
            surface: theme.bg,
        }
    }

    /// The background of the container being drawn into.
    pub fn surface(&self) -> ratatui::style::Color {
        self.surface
    }

    /// Draw `body` with `surface` as the container background, restoring the
    /// previous one afterwards so sibling containers are unaffected.
    pub fn on_surface<R>(
        &mut self,
        surface: ratatui::style::Color,
        body: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let prev = std::mem::replace(&mut self.surface, surface);
        let out = body(self);
        self.surface = prev;
        out
    }

    /// Set the container background for everything drawn from here on.
    ///
    /// For a container that hands its body rect back rather than taking a
    /// closure — a modal, say, whose caller draws the contents.
    pub fn set_surface(&mut self, surface: ratatui::style::Color) {
        self.surface = surface;
    }

    /// The background a control in `state` sits on, over this container.
    pub fn surface_of(&self, state: State) -> ratatui::style::Color {
        super::style::surface(state, self.theme, self.surface)
    }

    /// The full style for a row or control label in `state`.
    pub fn row_style(&self, state: State) -> Style {
        super::style::row(state, self.theme, self.surface)
    }

    /// The left rail for a row in `state`, if it has one.
    pub fn rail_of(&self, state: State) -> Option<(crate::ui::glyphs::Glyph, Style)> {
        super::style::rail(state, self.theme, self.surface)
    }

    pub fn is_focused(&self, id: ZoneId) -> bool {
        self.focus == Some(id)
    }

    pub fn is_hovered(&self, id: ZoneId) -> bool {
        self.hover == Some(id)
    }

    /// The interaction state of `id`, folding in a caller-supplied selection.
    pub fn state_of(&self, id: ZoneId, selected: bool) -> State {
        State::selected(selected)
            .with_focus(self.is_focused(id))
            .with_hover(self.is_hovered(id))
    }

    /// Register `rect` as interactive and return its state in one step — the
    /// shape nearly every component wants, and the reason drawing and
    /// hit-testing cannot drift apart.
    pub fn interactive(&mut self, rect: Rect, id: ZoneId, selected: bool) -> State {
        self.zones.push(rect, id);
        self.state_of(id, selected)
    }

    /// Register `rect` as clickable but skipped by the focus ring.
    pub fn hit_only(&mut self, rect: Rect, id: ZoneId) -> State {
        self.zones.push_hit(rect, id);
        self.state_of(id, false)
    }

    /// Paint `area` with a flat background. Terminal cells are opaque, so a
    /// panel has to lay its ground down before drawing onto it.
    pub fn fill(&mut self, area: Rect, style: Style) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        self.frame.render_widget(Paragraph::new("").style(style), area);
    }

    /// Draw one line of spans at `area`, clipped to it.
    pub fn line(&mut self, area: Rect, line: Line<'static>, style: Style) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        self.frame
            .render_widget(Paragraph::new(line).style(style), area);
    }

    /// Draw `text` as a single styled span at `area`.
    pub fn text(&mut self, area: Rect, text: impl Into<String>, style: Style) {
        self.line(area, Line::from(Span::styled(text.into(), style)), style);
    }
}

/// A horizontal run of cells at `row`, `cols` wide, starting at `x`.
pub fn strip(x: u16, row: u16, cols: u16) -> Rect {
    Rect {
        x,
        y: row,
        width: cols,
        height: 1,
    }
}

/// The single row `n` rows down from `area`'s top, full width. Returns an empty
/// rect once `n` runs past the area rather than drawing outside it.
pub fn row_at(area: Rect, n: u16) -> Rect {
    if n >= area.height {
        return Rect {
            x: area.x,
            y: area.y,
            width: 0,
            height: 0,
        };
    }
    Rect {
        x: area.x,
        y: area.y + n,
        width: area.width,
        height: 1,
    }
}

/// Render through a [`Ui`] in a test and hand back what was painted plus the
/// zones registered.
///
/// Shared by every screen's tests: building a `Ui` needs a theme, a registry,
/// focus and hover, and repeating that in six test modules is how they drift
/// apart from what the app actually constructs.
#[cfg(test)]
pub fn draw_test(
    w: u16,
    h: u16,
    draw: impl FnOnce(&mut Ui, Rect),
) -> (Vec<String>, super::zones::Zones) {
    use super::focus::{Focus, Hover};
    use super::tokens::Metrics;
    use super::zones::Zones;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let theme = crate::model::ThemeId::default().build();
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
        draw(&mut ui, area);
    })
    .unwrap();
    let buf = term.backend().buffer().clone();
    let lines = (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
        .collect();
    (lines, zones)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_at_stays_inside_its_area() {
        let area = Rect {
            x: 2,
            y: 3,
            width: 10,
            height: 4,
        };
        for n in 0..4 {
            let r = row_at(area, n);
            assert_eq!(r.y, area.y + n);
            assert_eq!(r.height, 1);
            assert!(r.y < area.y + area.height);
        }
        // Past the end yields nothing to draw into, not a row outside the area.
        let past = row_at(area, 4);
        assert_eq!(past.height, 0);
        assert_eq!(past.width, 0);
    }
}
