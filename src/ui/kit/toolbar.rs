//! A screen's toolbar row, laid out from its action table.
//!
//! Everything here is drawn from [`Act`] declarations rather than assembled by
//! hand, so a toolbar cannot offer a control the screen has no handler for, and
//! the accelerator printed on a button is the one `handle_key` matches.
//!
//! Width is spent in a fixed order: full labels if they fit, then icon-only,
//! then dropping controls from the right into a trailing `⋯` that opens the
//! rest as a menu. Controls are dropped from the *right* because a screen
//! declares its most-reached-for action first.

use ratatui::layout::Rect;

use super::badge::Chip;
use super::button::Button;
use super::ctx::Ui;
use super::zones::ZoneId;
use crate::app::action_table::{Act, Kind, OVERFLOW_ID, Placement};
use crate::ui::glyphs;

/// How wide the overflow control is: a glyph with a space either side.
fn more_width() -> u16 {
    glyphs::ELLIPSIS.cols() + 2
}

/// What a toolbar decided to draw, so a caller can tell whether the `⋯` it
/// registered is reachable and what it stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// How many of the toolbar's actions were drawn.
    pub drawn: usize,
    /// Labels were dropped for icons.
    pub iconified: bool,
    /// The `⋯` control was drawn.
    pub more: bool,
}

pub struct Toolbar<'a> {
    acts: &'a [Act],
    /// Draw `⋯` even when everything fits — the screen has menu-only actions
    /// behind it.
    has_menu: bool,
    gap: u16,
}

impl<'a> Toolbar<'a> {
    pub fn new(acts: &'a [Act]) -> Self {
        Self {
            acts,
            has_menu: false,
            gap: 1,
        }
    }

    /// The screen keeps actions behind the `⋯`, so draw it unconditionally.
    pub fn has_menu(mut self, yes: bool) -> Self {
        self.has_menu = yes;
        self
    }

    pub fn gap(mut self, gap: u16) -> Self {
        self.gap = gap;
        self
    }

    fn items(&self) -> impl Iterator<Item = &Act> {
        self.acts
            .iter()
            .filter(|a| a.placement == Placement::Toolbar)
    }

    fn total(&self, widths: &[u16]) -> u16 {
        if widths.is_empty() {
            return 0;
        }
        widths.iter().sum::<u16>() + self.gap * (widths.len() as u16 - 1)
    }

    /// Draw into `area` (one row) and register every control.
    pub fn render(&self, ui: &mut Ui, area: Rect) -> Layout {
        let items: Vec<&Act> = self.items().collect();
        if area.width == 0 || area.height == 0 {
            return Layout {
                drawn: 0,
                iconified: false,
                more: false,
            };
        }
        let row = Rect { height: 1, ..area };

        let full: Vec<u16> = items.iter().map(|a| control_width(a, false)).collect();
        let icons: Vec<u16> = items.iter().map(|a| control_width(a, true)).collect();
        let more_w = more_width();
        let reserve = if self.has_menu { more_w + self.gap } else { 0 };

        // Full labels while they fit; otherwise icons, and a tail dropped from
        // those if even they do not.
        let fits = self.total(&full) + reserve <= row.width;
        let (mut widths, iconified) = if fits { (full, false) } else { (icons, true) };

        let mut drawn = items.len();
        let mut more = self.has_menu;
        if self.total(&widths) + reserve > row.width {
            // Something has to go, so the `⋯` is what holds it.
            more = true;
            while drawn > 0 {
                let w = self.total(&widths[..drawn]) + more_w + self.gap;
                if w <= row.width {
                    break;
                }
                drawn -= 1;
            }
            widths.truncate(drawn);
        }

        let mut x = row.x;
        let right = row.x + row.width;
        for (a, w) in items.iter().take(drawn).zip(widths.iter()) {
            if x + w > right {
                break;
            }
            let cell = Rect {
                x,
                y: row.y,
                width: *w,
                height: 1,
            };
            draw_control(ui, a, cell, iconified);
            x += w + self.gap;
        }

        if more && x + more_w <= right {
            let cell = Rect {
                x,
                y: row.y,
                width: more_w,
                height: 1,
            };
            Button::new(ZoneId::action(OVERFLOW_ID), glyphs::ELLIPSIS.as_str()).render(ui, cell);
        } else {
            more = false;
        }

        Layout {
            drawn,
            iconified,
            more,
        }
    }
}

/// The actions that belong to one row, drawn at its right end.
///
/// The row rectangle comes from the registry the list wrote while drawing, so
/// there is no second copy of the row layout here to fall out of step with it.
pub struct RowActions<'a> {
    acts: &'a [Act],
    gap: u16,
}

impl<'a> RowActions<'a> {
    pub fn new(acts: &'a [Act]) -> Self {
        Self { acts, gap: 1 }
    }

    fn items(&self) -> Vec<&Act> {
        self.acts
            .iter()
            .filter(|a| a.placement == Placement::Row)
            .collect()
    }

    fn width(&self, items: &[&Act], narrow: bool) -> u16 {
        if items.is_empty() {
            return 0;
        }
        items
            .iter()
            .map(|a| control_width(a, narrow))
            .sum::<u16>()
            + self.gap * (items.len() as u16 - 1)
    }

    /// Draw right-aligned inside `row`, claiming at most half of it — past that
    /// the buttons would be covering the thing they act on. Returns the columns
    /// claimed, or 0 when there was no room and the context menu is the only
    /// way to them.
    pub fn render(&self, ui: &mut Ui, row: Rect) -> u16 {
        let items = self.items();
        if items.is_empty() || row.width == 0 || row.height == 0 {
            return 0;
        }
        let budget = row.width / 2;
        let full = self.width(&items, false);
        let narrow = self.width(&items, true);
        let (want, iconified) = if full <= budget {
            (full, false)
        } else if narrow <= budget {
            (narrow, true)
        } else {
            return 0;
        };

        let mut x = row.x + row.width - want;
        for a in &items {
            let w = control_width(a, iconified);
            draw_control(
                ui,
                a,
                Rect {
                    x,
                    y: row.y,
                    width: w,
                    height: 1,
                },
                iconified,
            );
            x += w + self.gap;
        }
        want
    }
}

/// Columns one control wants. Chips and buttons measure themselves, so a
/// toolbar never guesses a width the control then disagrees with.
pub fn control_width(a: &Act, narrow: bool) -> u16 {
    match a.kind {
        Kind::Verb => button_for(a, narrow).width(),
        _ => chip_for(a, narrow).width(),
    }
}

fn button_for(a: &Act, narrow: bool) -> Button {
    let b = Button::new(a.zone(), label_for(a, narrow)).disabled(!a.enabled);
    if narrow { b } else { b.accel(a.accel.shown()) }
}

fn chip_for(a: &Act, narrow: bool) -> Chip {
    let on = matches!(a.kind, Kind::Toggle(true));
    let chip = Chip::new(a.zone(), label_for(a, narrow), on).disabled(!a.enabled);
    // A cycling control shows its current value, so an on/off mark would be a
    // second, contradictory reading of the same chip.
    if matches!(a.kind, Kind::Cycle) {
        chip.plain()
    } else {
        chip
    }
}

fn label_for(a: &Act, narrow: bool) -> String {
    if narrow {
        a.icon_label()
    } else {
        a.control_label()
    }
}

fn draw_control(ui: &mut Ui, a: &Act, cell: Rect, narrow: bool) {
    match a.kind {
        Kind::Verb => button_for(a, narrow).render(ui, cell),
        _ => chip_for(a, narrow).render(ui, cell),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::action_table::Accel;
    use crate::ui::kit::ctx::draw_test;
    use crate::ui::kit::zones::ZoneKind;

    fn acts() -> Vec<Act> {
        vec![
            Act::toolbar(0, "import", Accel::key('i')),
            Act::toolbar(1, "rescan", Accel::key('r')),
            Act::toolbar(2, "follow", Accel::key('f')).toggle(true),
            Act::row(9, "delete", Accel::key('d')),
        ]
    }

    #[test]
    fn only_toolbar_actions_are_drawn() {
        let acts = acts();
        let (_, zones) = draw_test(80, 1, |ui, area| {
            Toolbar::new(&acts).render(ui, area);
        });
        assert!(zones.contains(ZoneId::action(0)));
        assert!(zones.contains(ZoneId::action(2)));
        assert!(
            !zones.contains(ZoneId::action(9)),
            "a row action belongs on its row, not in the toolbar"
        );
    }

    #[test]
    fn every_control_claims_exactly_what_it_draws() {
        // A target wider than the control fires something the pointer was not
        // on. Measured against each control's own declared width rather than
        // the painted row, whose trailing pad cell a trim would swallow.
        let acts = acts();
        let (_, zones) = draw_test(80, 1, |ui, area| {
            Toolbar::new(&acts).render(ui, area);
        });
        for a in acts.iter().filter(|a| a.placement == Placement::Toolbar) {
            let rect = zones.rect_of(a.zone()).expect("registered");
            assert_eq!(
                rect.width,
                control_width(a, false),
                "{} claims more or less than it draws",
                a.label
            );
        }
        // …and they do not overlap.
        let mut spans: Vec<(u16, u16)> = zones
            .all()
            .filter(|(_, id)| id.kind == ZoneKind::Action)
            .map(|(r, _)| (r.x, r.x + r.width))
            .collect();
        spans.sort();
        for pair in spans.windows(2) {
            assert!(pair[0].1 <= pair[1].0, "overlapping targets: {spans:?}");
        }
    }

    #[test]
    fn narrow_collapses_to_icons_before_it_drops_anything() {
        let acts = acts();
        let wide = draw_test(80, 1, |ui, area| {
            Toolbar::new(&acts).render(ui, area);
        });
        assert!(wide.0[0].contains("import"));

        let (lines, zones) = draw_test(24, 1, |ui, area| {
            Toolbar::new(&acts).render(ui, area);
        });
        assert!(!lines[0].contains("import"), "labels should have gone first");
        for id in [0u16, 1, 2] {
            assert!(
                zones.contains(ZoneId::action(id)),
                "action {id} should still be reachable as an icon: {:?}",
                lines[0]
            );
        }
    }

    #[test]
    fn what_will_not_fit_goes_behind_the_ellipsis() {
        let acts = acts();
        let (_, zones) = draw_test(8, 1, |ui, area| {
            let out = Toolbar::new(&acts).render(ui, area);
            assert!(out.more, "8 columns cannot hold three controls");
            assert!(out.drawn < 3);
        });
        assert!(
            zones.contains(ZoneId::action(OVERFLOW_ID)),
            "the overflow control must stay reachable, or the actions are lost"
        );
    }

    #[test]
    fn the_ellipsis_is_drawn_whenever_the_screen_keeps_a_menu() {
        let acts = acts();
        let (_, zones) = draw_test(80, 1, |ui, area| {
            let out = Toolbar::new(&acts).has_menu(true).render(ui, area);
            assert_eq!(out.drawn, 3, "nothing needed dropping at 80 columns");
            assert!(out.more);
        });
        assert!(zones.contains(ZoneId::action(OVERFLOW_ID)));
    }

    #[test]
    fn a_disabled_control_is_still_a_registered_target() {
        // Falling through to whatever sits behind a greyed button is worse than
        // a click that visibly does nothing.
        let acts = vec![Act::toolbar(0, "export", Accel::key('x')).when(false)];
        let (_, zones) = draw_test(40, 1, |ui, area| {
            Toolbar::new(&acts).render(ui, area);
        });
        assert!(zones.contains(ZoneId::action(0)));
    }

    #[test]
    fn row_actions_sit_at_the_right_end_of_their_row() {
        let acts = acts();
        let row = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 1,
        };
        let (_, zones) = draw_test(60, 1, |ui, _| {
            let used = RowActions::new(&acts).render(ui, row);
            assert!(used > 0);
        });
        let rect = zones.rect_of(ZoneId::action(9)).expect("the row action");
        assert_eq!(rect.x + rect.width, 60, "not flush right: {rect:?}");
        assert!(
            !zones.contains(ZoneId::action(0)),
            "a toolbar action does not belong on a row"
        );
    }

    #[test]
    fn row_actions_give_up_rather_than_cover_the_row() {
        // Past half the row the buttons would hide the thing they act on, and
        // the context menu still reaches them.
        let acts = vec![
            Act::row(0, "rename", Accel::key('R')),
            Act::row(1, "delete", Accel::key('d')),
        ];
        let (_, zones) = draw_test(12, 1, |ui, area| {
            assert_eq!(RowActions::new(&acts).render(ui, area), 0);
        });
        assert!(zones.is_empty());
    }

    #[test]
    fn nothing_is_drawn_into_a_zero_row() {
        let acts = acts();
        let (_, zones) = draw_test(40, 1, |ui, area| {
            let out = Toolbar::new(&acts).render(
                ui,
                Rect {
                    width: 0,
                    ..area
                },
            );
            assert_eq!(out.drawn, 0);
        });
        assert!(zones.is_empty());
    }
}
