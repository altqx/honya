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
        }
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
