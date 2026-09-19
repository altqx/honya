//! An anchored popover listing actions — what a right-click on a row, or a
//! toolbar's `⋯`, opens.
//!
//! It takes [`Modal`](super::modal)'s discipline without its shape: a
//! full-frame backdrop registered first, so a click beside it dismisses rather
//! than reaching the screen, and [`Zones::begin_trap`] so Tab cycles inside the
//! menu and cannot wander onto the rows behind it. What differs is placement —
//! a menu belongs beside the thing it was opened on, and flips above the anchor
//! when it would otherwise run off the bottom.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear};

use super::ctx::Ui;
use super::style::State;
use super::zones::{ZoneId, ZoneKind};
use crate::app::action_table::{Act, Placement};
use crate::theme;
use crate::ui::text::{col_width, pad_to_cols, truncate_cols};

/// Columns between a label and its accelerator.
const ACCEL_GAP: u16 = 3;
/// Columns of padding inside the border.
const H_PAD: u16 = 1;

/// Which group an entry belongs to. Row actions are about the thing that was
/// clicked; everything else is about the screen, and the two read better with a
/// rule between them.
fn group(a: &Act) -> u8 {
    u8::from(a.placement != Placement::Row)
}

pub struct Menu<'a> {
    items: &'a [Act],
    /// The cell the menu hangs from — where the pointer was, or the control
    /// that opened it.
    anchor: (u16, u16),
    /// Keyboard selection.
    sel: usize,
}

impl<'a> Menu<'a> {
    pub fn new(items: &'a [Act], anchor: (u16, u16)) -> Self {
        Self {
            items,
            anchor,
            sel: 0,
        }
    }

    pub fn sel(mut self, sel: usize) -> Self {
        self.sel = sel;
        self
    }

    /// Columns the widest entry needs, border and padding included.
    pub fn width(&self) -> u16 {
        let label = self
            .items
            .iter()
            .map(|a| col_width(a.label) as u16)
            .max()
            .unwrap_or(0);
        let accel = self
            .items
            .iter()
            .map(|a| col_width(&a.accel.shown()) as u16)
            .max()
            .unwrap_or(0);
        label + ACCEL_GAP + accel + H_PAD * 2 + 2
    }

    /// Rows the menu needs, separators and border included.
    pub fn height(&self) -> u16 {
        self.items.len() as u16 + self.separators() + 2
    }

    fn separators(&self) -> u16 {
        self.items
            .windows(2)
            .filter(|w| group(&w[0]) != group(&w[1]))
            .count() as u16
    }

    /// Where the menu lands inside `area`.
    pub fn rect(&self, area: Rect) -> Rect {
        let w = self.width().min(area.width);
        let h = self.height().min(area.height);
        let (ax, ay) = self.anchor;

        let max_x = (area.x + area.width).saturating_sub(w);
        let x = ax.clamp(area.x, max_x.max(area.x));

        // Below the anchor by default; above it when there is no room, which is
        // what keeps a menu opened on the last row from being clipped away.
        let below = ay.saturating_add(1);
        let y = if below + h <= area.y + area.height {
            below
        } else if ay >= area.y + h {
            ay - h
        } else {
            (area.y + area.height).saturating_sub(h).max(area.y)
        };
        Rect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    /// Draw the menu and register every entry. Returns its rectangle.
    pub fn render(&self, ui: &mut Ui, area: Rect) -> Rect {
        let outer = self.rect(area);
        if outer.width < 3 || outer.height < 3 {
            return outer;
        }

        // Registered before anything else and covering the whole frame: a click
        // outside resolves to the backdrop rather than to the row underneath,
        // which would both dismiss the menu and do something else.
        ui.hit_only(area, ZoneId::bare(ZoneKind::Backdrop));
        ui.zones.begin_trap();

        ui.frame.render_widget(Clear, outer);
        let border = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(
                Style::default()
                    .fg(ui.theme.accent)
                    .bg(ui.theme.bg_elevated),
            )
            .style(Style::default().bg(ui.theme.bg_elevated));
        let inner = border.inner(outer);
        ui.frame.render_widget(border, outer);
        ui.hit_only(outer, ZoneId::bare(ZoneKind::ModalFrame));

        ui.on_surface(ui.theme.bg_elevated, |ui| {
            self.draw_entries(ui, inner);
        });
        outer
    }

    fn draw_entries(&self, ui: &mut Ui, inner: Rect) {
        let mut y = inner.y;
        let bottom = inner.y + inner.height;
        let mut prev: Option<&Act> = None;

        for (i, act) in self.items.iter().enumerate() {
            if y >= bottom {
                break;
            }
            if let Some(p) = prev
                && group(p) != group(act)
            {
                draw_rule(ui, Rect { y, height: 1, ..inner });
                y += 1;
                if y >= bottom {
                    break;
                }
            }
            let row = Rect {
                y,
                height: 1,
                ..inner
            };
            self.draw_entry(ui, row, act, i == self.sel);
            y += 1;
            prev = Some(act);
        }
    }

    fn draw_entry(&self, ui: &mut Ui, row: Rect, act: &Act, selected: bool) {
        let state = if act.enabled {
            ui.interactive(row, act.zone(), selected)
        } else {
            ui.hit_only(row, act.zone()).with_disabled(true)
        };
        let base = entry_style(ui, state, act.enabled);

        let accel = act.accel.shown();
        let accel_cols = col_width(&accel) as u16;
        let label_budget = row
            .width
            .saturating_sub(H_PAD * 2 + accel_cols + ACCEL_GAP) as usize;
        let label = pad_to_cols(&truncate_cols(act.label, label_budget), label_budget);

        let gap = row
            .width
            .saturating_sub(H_PAD * 2 + col_width(&label) as u16 + accel_cols)
            .max(1) as usize;

        let pad = " ".repeat(H_PAD as usize);
        let spans = vec![
            Span::styled(pad.clone(), base),
            Span::styled(label, base),
            Span::styled(" ".repeat(gap), base),
            Span::styled(accel, accel_style(ui, base, act.enabled)),
            Span::styled(pad, base),
        ];
        ui.line(row, Line::from(spans), base);
    }
}

fn entry_style(ui: &Ui, state: State, enabled: bool) -> Style {
    if !enabled {
        return Style::default()
            .fg(ui.theme.ink_faint)
            .bg(ui.surface())
            .add_modifier(Modifier::DIM);
    }
    let mut st = ui.row_style(state);
    if state.focused || state.hovered || state.selected {
        st = st.fg(ui.theme.ink).add_modifier(Modifier::BOLD);
    }
    st
}

fn accel_style(ui: &Ui, base: Style, enabled: bool) -> Style {
    let mut st = Style::default().fg(ui.theme.ink_faint);
    if let Some(bg) = base.bg {
        st = st.bg(bg);
    }
    if !enabled {
        st = st.add_modifier(Modifier::DIM);
    }
    st
}

fn draw_rule(ui: &mut Ui, row: Rect) {
    let glyph = crate::ui::glyphs::RULE_H.as_str();
    let st = Style::default().fg(ui.theme.rule).bg(ui.surface());
    ui.text(row, glyph.repeat(row.width as usize), st);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::action_table::{Accel, Act};
    use crate::ui::kit::ctx::draw_test;

    fn items() -> Vec<Act> {
        vec![
            Act::row(0, "read", Accel::code(ratatui::crossterm::event::KeyCode::Enter)),
            Act::row(1, "delete", Accel::key('d')),
            Act::menu(2, "export", Accel::key('x')),
            Act::menu(3, "synopsis", Accel::key('y')).when(false),
        ]
    }

    #[test]
    fn every_entry_is_registered_under_its_action_id() {
        let items = items();
        let (_, zones) = draw_test(60, 20, |ui, area| {
            Menu::new(&items, (4, 4)).render(ui, area);
        });
        for id in 0u16..4 {
            assert!(zones.contains(ZoneId::action(id)), "entry {id} unreachable");
        }
    }

    #[test]
    fn a_click_beside_the_menu_lands_on_the_backdrop() {
        let items = items();
        let (_, zones) = draw_test(60, 20, |ui, area| {
            // A row behind the menu, as a screen would have drawn.
            ui.interactive(Rect { x: 0, y: 0, width: 60, height: 1 }, ZoneId::row(0), false);
            Menu::new(&items, (4, 4)).render(ui, area);
        });
        assert_eq!(
            zones.at(58, 0),
            Some(ZoneId::bare(ZoneKind::Backdrop)),
            "dismissing must not also fire the row underneath"
        );
    }

    #[test]
    fn tab_is_confined_to_the_menu() {
        let items = items();
        let (_, zones) = draw_test(60, 20, |ui, area| {
            ui.interactive(Rect { x: 0, y: 0, width: 60, height: 1 }, ZoneId::row(0), false);
            Menu::new(&items, (4, 4)).render(ui, area);
        });
        let ring: Vec<_> = zones.tab_order().collect();
        assert!(!ring.contains(&ZoneId::row(0)));
        assert!(ring.contains(&ZoneId::action(0)));
        assert!(
            !ring.contains(&ZoneId::action(3)),
            "a disabled entry is clickable but not a focus stop"
        );
    }

    #[test]
    fn it_flips_above_an_anchor_near_the_bottom() {
        let items = items();
        let area = Rect { x: 0, y: 0, width: 60, height: 20 };
        let menu = Menu::new(&items, (4, 19));
        let r = menu.rect(area);
        assert!(r.y + r.height <= 20, "clipped off the bottom: {r:?}");
        assert!(r.y < 19, "should have opened upward: {r:?}");

        // With room below, it hangs from the anchor as usual.
        let down = Menu::new(&items, (4, 2)).rect(area);
        assert_eq!(down.y, 3);
    }

    #[test]
    fn it_stays_inside_the_frame_when_anchored_at_the_right_edge() {
        let items = items();
        let area = Rect { x: 0, y: 0, width: 60, height: 20 };
        let r = Menu::new(&items, (59, 2)).rect(area);
        assert!(r.x + r.width <= 60, "ran off the right: {r:?}");
    }

    #[test]
    fn row_actions_are_ruled_off_from_screen_actions() {
        let items = items();
        let (lines, _) = draw_test(60, 20, |ui, area| {
            Menu::new(&items, (4, 4)).render(ui, area);
        });
        let rule = crate::ui::glyphs::RULE_H.as_str();
        let body: Vec<&String> = lines.iter().filter(|l| l.contains("delete")).collect();
        assert_eq!(body.len(), 1);
        assert!(
            lines.iter().filter(|l| l.matches(rule).count() > 4).count() >= 3,
            "expected a separator row between the two groups, plus the border"
        );
    }

    #[test]
    fn an_accelerator_is_printed_beside_every_entry() {
        let items = items();
        let (lines, _) = draw_test(60, 20, |ui, area| {
            Menu::new(&items, (4, 4)).render(ui, area);
        });
        let row = lines.iter().find(|l| l.contains("delete")).unwrap();
        assert!(row.contains('d'), "{row:?}");
        let export = lines.iter().find(|l| l.contains("export")).unwrap();
        assert!(export.contains('x'), "{export:?}");
    }
}
