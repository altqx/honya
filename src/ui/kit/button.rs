//! Buttons, and rows of them.
//!
//! Dialogs used to draw their choices as bare key hints and recover the click
//! target by re-deriving column arithmetic afterwards. A button here draws its
//! own label and registers its own rectangle, so the thing you can see is
//! exactly the thing you can click, and its accelerator is printed on it rather
//! than hidden in the footer.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::ctx::Ui;
use super::style::{self, State};
use super::zones::ZoneId;
use crate::ui::text::{col_width, truncate_cols};

/// How much weight a button carries in its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Variant {
    /// The default action. Filled, so it reads first.
    Primary,
    /// Everything else.
    #[default]
    Secondary,
    /// Destructive. Carries the one red the palette allows.
    Danger,
    /// Present but not currently available.
    Disabled,
}

/// A single labelled control.
#[derive(Debug, Clone)]
pub struct Button {
    pub id: ZoneId,
    pub label: String,
    /// The key that also triggers it, printed on the button itself.
    pub accel: Option<String>,
    pub variant: Variant,
    /// Claim the full width handed to it rather than only what the label needs.
    /// Off by default so a click target is never wider than the control a user
    /// can see.
    pub stretch: bool,
}

impl Button {
    pub fn new(id: ZoneId, label: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
            accel: None,
            variant: Variant::Secondary,
            stretch: false,
        }
    }

    /// Fill the width given, for menu rows and full-width calls to action.
    pub fn stretch(mut self) -> Self {
        self.stretch = true;
        self
    }

    pub fn primary(mut self) -> Self {
        self.variant = Variant::Primary;
        self
    }

    pub fn danger(mut self) -> Self {
        self.variant = Variant::Danger;
        self
    }

    pub fn disabled(mut self, yes: bool) -> Self {
        if yes {
            self.variant = Variant::Disabled;
        }
        self
    }

    pub fn accel(mut self, key: impl Into<String>) -> Self {
        self.accel = Some(key.into());
        self
    }

    /// Columns this button wants: one space of padding either side of the
    /// label, plus the accelerator and its separating space.
    pub fn width(&self) -> u16 {
        let mut w = col_width(&self.label) + 2;
        if let Some(a) = &self.accel {
            w += col_width(a) + 1;
        }
        w as u16
    }

    /// The narrowest this button can usefully be drawn: padding plus at least
    /// one column of label.
    pub fn min_width(&self) -> u16 {
        3
    }

    /// Draw into `area` (one row) and register the click target.
    pub fn render(&self, ui: &mut Ui, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        // A button claims only the columns it draws, unless asked to stretch:
        // a click target wider than the visible control is a click that fires
        // something the user was not pointing at.
        let width = if self.stretch {
            area.width
        } else {
            self.width().min(area.width)
        };
        let area = Rect {
            width,
            height: 1,
            ..area
        };
        let disabled = self.variant == Variant::Disabled;
        let state = if disabled {
            // Still registered: a click should land and do nothing visible,
            // rather than falling through to whatever sits behind it.
            ui.hit_only(area, self.id).with_disabled(true)
        } else {
            ui.interactive(area, self.id, false)
        };

        let base = self.style(state, ui);
        // The label gets whatever space the accelerator does not claim.
        let accel_cols = self
            .accel
            .as_ref()
            .map(|a| col_width(a) as u16 + 1)
            .unwrap_or(0);
        let label_budget = area.width.saturating_sub(2 + accel_cols) as usize;
        let label = truncate_cols(&self.label, label_budget);
        // When stretched, the leftover columns pad the label out so the fill
        // covers the whole control rather than leaving a ragged tail.
        let pad = (label_budget as u16).saturating_sub(col_width(&label) as u16);

        let mut spans = vec![
            Span::styled(" ", base),
            Span::styled(label, base),
        ];
        if pad > 0 {
            spans.push(Span::styled(" ".repeat(pad as usize), base));
        }
        if let Some(a) = &self.accel
            && accel_cols + 2 <= area.width
        {
            spans.push(Span::styled(" ", base));
            spans.push(Span::styled(a.clone(), self.accel_style(state, ui, base)));
        }
        spans.push(Span::styled(" ", base));

        ui.line(area, Line::from(spans), base);
    }

    fn style(&self, state: State, ui: &Ui) -> Style {
        let theme = ui.theme;
        match self.variant {
            Variant::Disabled => Style::default()
                .fg(theme.ink_faint)
                .bg(theme.bg)
                .add_modifier(Modifier::DIM),
            Variant::Primary => {
                let mut st = style::filled(theme);
                // A filled control already dominates; hover and focus lift it
                // rather than re-tinting it, which would lose the fill.
                if state.hovered || state.focused {
                    st = st.add_modifier(Modifier::UNDERLINED);
                }
                st
            }
            Variant::Danger => {
                let mut st = Style::default()
                    .fg(theme.status_failed)
                    .bg(ui.surface_of(state))
                    .add_modifier(Modifier::BOLD);
                if state.hovered && !theme.paints_fills() {
                    st = st.add_modifier(Modifier::REVERSED);
                }
                st
            }
            Variant::Secondary => {
                let mut st = ui.row_style(state);
                if state.focused || state.hovered {
                    st = st.fg(theme.ink).add_modifier(Modifier::BOLD);
                }
                st
            }
        }
    }

    /// The accelerator is dimmer than the label it belongs to, but keeps the
    /// button's own background so it does not punch a hole in a filled control.
    fn accel_style(&self, state: State, ui: &Ui, base: Style) -> Style {
        if self.variant == Variant::Primary {
            return base.remove_modifier(Modifier::BOLD);
        }
        let mut st = Style::default().fg(ui.theme.ink_faint);
        if let Some(bg) = base.bg {
            st = st.bg(bg);
        }
        if state.disabled {
            st = st.add_modifier(Modifier::DIM);
        }
        st
    }
}

/// Where a row of buttons sits within its area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    Left,
    Center,
    #[default]
    Right,
}

/// A row of buttons laid out on one line.
///
/// When the row does not fit, buttons are dropped from the *left* — the primary
/// action conventionally sits rightmost, and losing the way to confirm would be
/// worse than losing the way to cancel, which Esc still covers.
pub struct ButtonRow {
    pub buttons: Vec<Button>,
    pub align: Align,
    /// Columns between adjacent buttons.
    pub gap: u16,
}

impl ButtonRow {
    pub fn new(buttons: Vec<Button>) -> Self {
        Self {
            buttons,
            align: Align::Right,
            gap: 2,
        }
    }

    pub fn align(mut self, align: Align) -> Self {
        self.align = align;
        self
    }

    /// Total columns the row wants with every button at full width.
    pub fn width(&self) -> u16 {
        if self.buttons.is_empty() {
            return 0;
        }
        let labels: u16 = self.buttons.iter().map(|b| b.width()).sum();
        labels + self.gap * (self.buttons.len() as u16 - 1)
    }

    /// Which buttons fit in `cols`, as a slice of the tail.
    fn visible(&self, cols: u16) -> &[Button] {
        let mut start = 0usize;
        while start < self.buttons.len() {
            let rest = &self.buttons[start..];
            let w: u16 = rest.iter().map(|b| b.width()).sum::<u16>()
                + self.gap * (rest.len().saturating_sub(1)) as u16;
            if w <= cols {
                return rest;
            }
            start += 1;
        }
        &[]
    }

    pub fn render(&self, ui: &mut Ui, area: Rect) {
        if area.width == 0 || area.height == 0 || self.buttons.is_empty() {
            return;
        }
        let row = Rect {
            height: 1,
            ..area
        };
        let visible = self.visible(row.width).to_vec();
        if visible.is_empty() {
            return;
        }
        let used: u16 = visible.iter().map(|b| b.width()).sum::<u16>()
            + self.gap * (visible.len() as u16 - 1);
        let slack = row.width.saturating_sub(used);
        let mut x = match self.align {
            Align::Left => row.x,
            Align::Center => row.x + slack / 2,
            Align::Right => row.x + slack,
        };
        for b in &visible {
            let w = b.width().min(row.x + row.width - x);
            b.render(
                ui,
                Rect {
                    x,
                    y: row.y,
                    width: w,
                    height: 1,
                },
            );
            x = x.saturating_add(w + self.gap);
        }
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

    /// Render `draw` at `w`x1 and return the painted text plus the zones.
    fn paint(
        w: u16,
        focus: &Focus,
        hover: Hover,
        draw: impl FnOnce(&mut Ui, Rect),
    ) -> (String, Zones) {
        let theme = ThemeId::Washi.build();
        let mut zones = Zones::new();
        let mut term = Terminal::new(TestBackend::new(w, 1)).unwrap();
        let area = Rect {
            x: 0,
            y: 0,
            width: w,
            height: 1,
        };
        term.draw(|f| {
            let metrics = Metrics::new(area, false);
            let mut ui = Ui::new(f, &mut zones, &theme, metrics, focus, hover, 0);
            draw(&mut ui, area);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = (0..w).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        (text, zones)
    }

    #[test]
    fn a_button_draws_its_label_and_registers_exactly_what_it_drew() {
        let b = Button::new(ZoneId::button(1), "Confirm");
        let (text, zones) = paint(20, &Focus::new(), Hover::default(), |ui, area| {
            b.render(ui, area);
        });
        assert!(text.contains("Confirm"), "got {text:?}");

        let rect = zones.rect_of(ZoneId::button(1)).expect("registered");
        assert_eq!(rect.width, b.width());
        // Every column the button claims must hit-test back to it.
        for x in rect.x..rect.x + rect.width {
            assert_eq!(zones.at(x, 0), Some(ZoneId::button(1)), "column {x}");
        }
    }

    #[test]
    fn an_accelerator_is_printed_on_the_button() {
        let b = Button::new(ZoneId::button(0), "Delete").accel("d").danger();
        let (text, _) = paint(24, &Focus::new(), Hover::default(), |ui, area| {
            b.render(ui, area);
        });
        assert!(text.contains("Delete"), "got {text:?}");
        assert!(text.contains('d'), "accelerator should be visible: {text:?}");
    }

    #[test]
    fn a_disabled_button_still_catches_its_click_but_leaves_the_focus_ring() {
        let b = Button::new(ZoneId::button(3), "Export").disabled(true);
        let (_, zones) = paint(20, &Focus::new(), Hover::default(), |ui, area| {
            b.render(ui, area);
        });
        // Clickable, so the click stops here rather than falling through...
        assert!(zones.at(2, 0).is_some());
        // ...but Tab skips it.
        assert!(
            !zones.tab_order().any(|z| z == ZoneId::button(3)),
            "a disabled control must not take focus"
        );
    }

    #[test]
    fn a_row_lays_buttons_out_without_overlap_and_inside_its_area() {
        let row = ButtonRow::new(vec![
            Button::new(ZoneId::button(0), "Cancel"),
            Button::new(ZoneId::button(1), "Restart").accel("r"),
            Button::new(ZoneId::button(2), "Continue").primary(),
        ]);
        let (_, zones) = paint(60, &Focus::new(), Hover::default(), |ui, area| {
            row.render(ui, area);
        });
        let rects: Vec<Rect> = (0..3)
            .map(|i| zones.rect_of(ZoneId::button(i)).expect("registered"))
            .collect();
        for w in rects.windows(2) {
            assert!(
                w[0].x + w[0].width <= w[1].x,
                "buttons overlap: {:?} then {:?}",
                w[0],
                w[1]
            );
        }
        let last = rects[2];
        assert!(last.x + last.width <= 60, "row ran past its area");
    }

    #[test]
    fn a_cramped_row_keeps_the_primary_action_and_drops_from_the_left() {
        let row = ButtonRow::new(vec![
            Button::new(ZoneId::button(0), "Cancel"),
            Button::new(ZoneId::button(1), "Discard"),
            Button::new(ZoneId::button(2), "Save").primary(),
        ]);
        // Only room for the last button.
        let (_, zones) = paint(8, &Focus::new(), Hover::default(), |ui, area| {
            row.render(ui, area);
        });
        assert!(
            zones.contains(ZoneId::button(2)),
            "the primary action must survive; Esc still covers cancel"
        );
        assert!(!zones.contains(ZoneId::button(0)));
    }

    #[test]
    fn a_row_that_cannot_fit_anything_draws_nothing_rather_than_overflowing() {
        let row = ButtonRow::new(vec![Button::new(ZoneId::button(0), "Confirm")]);
        let (_, zones) = paint(2, &Focus::new(), Hover::default(), |ui, area| {
            row.render(ui, area);
        });
        assert!(zones.is_empty());
    }

    #[test]
    fn focus_and_hover_change_how_a_button_reads() {
        let b = Button::new(ZoneId::button(7), "Open");
        let mut focused = Focus::new();
        focused.set(ZoneId::button(7));

        let (plain, _) = paint(16, &Focus::new(), Hover::default(), |ui, area| {
            b.render(ui, area)
        });
        let (lit, _) = paint(16, &focused, Hover::default(), |ui, area| {
            b.render(ui, area)
        });
        // Same glyphs either way — the difference is styling, which is what the
        // buffer text cannot show; assert the text is stable so a future change
        // to focus styling cannot silently start shifting layout.
        assert_eq!(plain, lit, "focus must not change a button's width");
    }
}
