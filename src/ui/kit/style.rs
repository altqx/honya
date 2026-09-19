//! How an interactive element *looks* in each of its states.
//!
//! Centralised so every component agrees: a hovered row in a list, a hovered
//! button and a hovered tab must read as the same gesture, or hover stops
//! meaning anything. Components ask for a state and get a style; none of them
//! decide what "focused" looks like on their own.
//!
//! One palette needs care. The adaptive `terminal` theme paints **no
//! backgrounds at all** — every background slot is `Color::Reset` so terminal
//! translucency and background images show through, which is the whole point of
//! it. There, a background wash is not merely subtle, it is invisible. So state
//! is carried by the reverse-video and bold modifiers instead, and
//! [`Theme::paints_fills`] is the test that decides which language to speak.

use ratatui::style::{Color, Modifier, Style};

use crate::theme::Theme;
use crate::ui::glyphs::{self, Glyph};

impl Theme {
    /// Whether this palette paints solid backgrounds. False for the adaptive
    /// `terminal` theme, whose grounds are all `Color::Reset`.
    pub fn paints_fills(&self) -> bool {
        self.bg_hover != self.bg
    }
}

/// The interaction state of one element, as the owning screen understands it.
///
/// `selected` and `focused` are distinct: a list keeps a selected row while the
/// keyboard is somewhere else entirely, and that row should stay marked without
/// claiming to hold focus.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct State {
    pub selected: bool,
    pub focused: bool,
    pub hovered: bool,
    pub disabled: bool,
    /// Mid-press, or the moment an action fires.
    pub active: bool,
}

impl State {
    pub const IDLE: State = State {
        selected: false,
        focused: false,
        hovered: false,
        disabled: false,
        active: false,
    };

    pub fn selected(selected: bool) -> Self {
        Self {
            selected,
            ..Self::IDLE
        }
    }

    pub fn with_focus(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    pub fn with_hover(mut self, hovered: bool) -> Self {
        self.hovered = hovered;
        self
    }

    pub fn with_disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn with_active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Whether anything about this state should be drawn differently from idle.
    pub fn is_marked(self) -> bool {
        self.selected || self.focused || self.hovered || self.active || self.disabled
    }
}

/// The background a row or control sits on, given its state.
///
/// Precedence is active over selected over hover, because they answer different
/// questions and the most immediate one wins: "I am pressing this" beats "this
/// is the current item" beats "my pointer is here".
/// `base` is the container's own background — the screen's, or a modal's
/// raised surface. An idle control keeps it; the marked states replace it.
pub fn surface(state: State, theme: &Theme, base: Color) -> Color {
    if !theme.paints_fills() {
        return base;
    }
    match state {
        s if s.disabled => base,
        s if s.active => theme.bg_active,
        s if s.selected => theme.accent_bg,
        s if s.hovered => theme.bg_hover,
        _ => base,
    }
}

/// The foreground for body text in a given state.
pub fn ink(state: State, theme: &Theme) -> Color {
    match state {
        s if s.disabled => theme.ink_faint,
        s if s.selected || s.focused => theme.ink,
        _ => theme.ink_soft,
    }
}

/// The full style for one row of a list or one control's label.
pub fn row(state: State, theme: &Theme, base: Color) -> Style {
    let mut st = Style::default()
        .fg(ink(state, theme))
        .bg(surface(state, theme, base));
    if state.selected || state.focused {
        st = st.add_modifier(Modifier::BOLD);
    }
    // A palette with no fills has no wash to signal hover with, so it borrows
    // the terminal's own reverse video instead.
    if state.hovered && !theme.paints_fills() && !state.disabled {
        st = st.add_modifier(Modifier::REVERSED);
    }
    if state.disabled {
        st = st.add_modifier(Modifier::DIM);
    }
    st
}

/// The rail drawn down the left edge of a row or block, if any.
///
/// The rail is the primary way honya sets a block apart — a box around
/// everything reads as clutter at this density, and it costs two rows and two
/// columns of the very space a terminal has least of.
pub fn rail(state: State, theme: &Theme, base: Color) -> Option<(Glyph, Style)> {
    let (glyph, color) = match state {
        s if s.disabled => return None,
        s if s.focused => (glyphs::SELECT_BAR, theme.border_focus),
        s if s.selected => (glyphs::SELECT_BAR, theme.accent),
        s if s.hovered => (glyphs::ACCENT_RAIL_STRONG, theme.accent_soft),
        _ => return None,
    };
    Some((glyph, Style::default().fg(color).bg(surface(state, theme, base))))
}

/// The border color for a panel in a given state.
pub fn border(state: State, theme: &Theme) -> Color {
    match state {
        s if s.focused => theme.border_focus,
        s if s.hovered => theme.accent_soft,
        _ => theme.rule,
    }
}

/// A filled control — the primary button, the active tab's pill.
pub fn filled(theme: &Theme) -> Style {
    if theme.paints_fills() {
        Style::default()
            .fg(theme.accent_fg)
            .bg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        // No fills available: reverse video is the terminal's own "filled".
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::REVERSED | Modifier::BOLD)
    }
}

/// A key cap in a hint or on a control's accelerator.
pub fn key_cap(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.ink_soft)
        .add_modifier(Modifier::BOLD)
}

/// The label beside a key cap.
pub fn key_label(theme: &Theme) -> Style {
    Style::default().fg(theme.ink_faint)
}

/// A section heading inside a panel or form.
pub fn section(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.ink_faint)
        .add_modifier(Modifier::BOLD)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ThemeId;

    fn washi() -> Theme {
        ThemeId::Washi.build()
    }

    fn terminal() -> Theme {
        ThemeId::Terminal.build()
    }

    #[test]
    fn only_the_adaptive_palette_declines_to_paint_fills() {
        assert!(washi().paints_fills());
        assert!(!terminal().paints_fills());
        for &id in crate::theme::ALL_THEMES {
            let t = id.build();
            assert_eq!(
                t.paints_fills(),
                !matches!(id, ThemeId::Terminal),
                "{} disagrees about painting fills",
                id.label()
            );
        }
    }

    #[test]
    fn the_more_immediate_state_wins_the_surface() {
        let t = washi();
        let hovered = State::IDLE.with_hover(true);
        let selected = State::selected(true);
        let both = State::selected(true).with_hover(true);
        let pressed = State::selected(true).with_hover(true).with_active(true);

        assert_eq!(surface(hovered, &t, t.bg), t.bg_hover);
        assert_eq!(surface(selected, &t, t.bg), t.accent_bg);
        assert_eq!(
            surface(both, &t, t.bg),
            t.accent_bg,
            "selection outranks hover"
        );
        assert_eq!(
            surface(pressed, &t, t.bg),
            t.bg_active,
            "a press outranks everything"
        );
    }

    #[test]
    fn a_disabled_control_never_lights_up() {
        let t = washi();
        let disabled = State::selected(true).with_hover(true).with_disabled(true);
        assert_eq!(surface(disabled, &t, t.bg), t.bg, "no wash");
        assert_eq!(ink(disabled, &t), t.ink_faint);
        assert!(rail(disabled, &t, t.bg).is_none(), "no rail");
        assert!(row(disabled, &t, t.bg).add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn the_adaptive_palette_signals_hover_with_reverse_video() {
        let t = terminal();
        let hovered = State::IDLE.with_hover(true);
        // There is no wash to use...
        assert_eq!(surface(hovered, &t, t.bg), t.bg);
        // ...so the modifier carries it instead.
        assert!(
            row(hovered, &t, t.bg).add_modifier.contains(Modifier::REVERSED),
            "hover would be invisible on a palette that paints no fills"
        );
        // And a palette that does paint fills must not also reverse.
        assert!(!row(hovered, &washi(), washi().bg).add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn focus_and_selection_are_distinguishable_rails() {
        let t = washi();
        let (_, focused) = rail(State::IDLE.with_focus(true), &t, t.bg).expect("focus rail");
        let (_, selected) = rail(State::selected(true), &t, t.bg).expect("selection rail");
        assert_eq!(focused.fg, Some(t.border_focus));
        assert_eq!(selected.fg, Some(t.accent));
        assert_ne!(
            focused.fg, selected.fg,
            "a focused row must be tellable from a merely selected one"
        );
        assert!(rail(State::IDLE, &t, t.bg).is_none(), "idle rows carry no rail");
    }

    #[test]
    fn a_filled_control_is_legible_in_every_palette() {
        for &id in crate::theme::ALL_THEMES {
            let t = id.build();
            let st = filled(&t);
            if t.paints_fills() {
                assert_eq!(st.fg, Some(t.accent_fg), "{}", id.label());
                assert_eq!(st.bg, Some(t.accent), "{}", id.label());
            } else {
                assert!(
                    st.add_modifier.contains(Modifier::REVERSED),
                    "{}: no fill available, so it must reverse",
                    id.label()
                );
            }
        }
    }
}
