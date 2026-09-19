//! The one modal frame every overlay is an instance of.
//!
//! Twenty overlays currently pick their own `centered_modal(w, h)` literal and
//! then restate it in a separate hit-test function. The two copies have already
//! drifted apart once. Here a modal computes its rectangle from a named size,
//! draws itself, and registers what it drew — so there is nothing left to
//! disagree with.
//!
//! It also owns the focus contract. A modal blocks: while one is open the
//! keyboard belongs to it, Tab wraps inside it and cannot reach the screen
//! behind, and a click beside it dismisses rather than falling through to
//! whatever it is covering. [`Zones::begin_trap`] and the full-frame backdrop
//! are what make both true, and doing them here means no overlay has to
//! remember to.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear};

use super::button::{Align, ButtonRow};
use super::ctx::Ui;
use super::style;
use super::zones::{ZoneId, ZoneKind};
use crate::theme;
use crate::ui::glyphs;
use crate::ui::text::{col_width, truncate_cols};

/// Zone index for a modal's close affordance, under [`ZoneKind::Button`].
/// High enough that no overlay's own button indices reach it.
pub const CLOSE_BUTTON: u32 = 0xFFFF;

/// A named modal size. Presets rather than per-overlay literals, so modals
/// across the app line up instead of each being a slightly different rectangle.
#[derive(Debug, Clone, Copy)]
pub struct Sizing {
    /// Fraction of the host width to occupy, before clamping.
    pub width_pct: f32,
    pub min_width: u16,
    pub max_width: u16,
    /// Rows left clear above and below.
    pub v_margin: u16,
    /// Columns of padding inside the border.
    pub h_pad: u16,
    /// Rows of padding inside the border.
    pub v_pad: u16,
}

impl Sizing {
    /// A working surface: editors, the log, side-by-side inspection.
    pub fn large() -> Self {
        Self {
            width_pct: 0.86,
            min_width: 48,
            max_width: 140,
            v_margin: 3,
            h_pad: 2,
            v_pad: 1,
        }
    }

    /// The default: wizards, settings, pickers.
    pub fn medium() -> Self {
        Self {
            width_pct: 0.66,
            min_width: 44,
            max_width: 96,
            v_margin: 4,
            h_pad: 2,
            v_pad: 1,
        }
    }

    /// A question with a couple of buttons.
    pub fn small() -> Self {
        Self {
            width_pct: 0.5,
            min_width: 36,
            max_width: 64,
            v_margin: 6,
            h_pad: 2,
            v_pad: 1,
        }
    }

    /// Collapse the padding for a short terminal, keeping exactly one column so
    /// the rail and the focus ring still have room to draw.
    pub fn compact(mut self, yes: bool) -> Self {
        if yes {
            self.v_margin = 0;
            self.h_pad = 1;
            self.v_pad = 0;
        }
        self
    }

    /// Fix the height instead of deriving it from the margin — for a wizard
    /// whose modal must not resize as the user advances through its steps.
    pub fn with_height(self, rows: u16, area: Rect) -> Rect {
        let outer = self.rect(area);
        let h = rows.min(area.height);
        let y = area.y + (area.height.saturating_sub(h)) / 2;
        Rect { y, height: h, ..outer }
    }

    /// The outer rectangle this sizing occupies within `area`.
    pub fn rect(&self, area: Rect) -> Rect {
        let want = (area.width as f32 * self.width_pct).round() as u16;
        let w = want
            .clamp(self.min_width, self.max_width)
            .min(area.width);
        let h = area
            .height
            .saturating_sub(self.v_margin.saturating_mul(2))
            .max(3)
            .min(area.height);
        Rect {
            x: area.x + (area.width.saturating_sub(w)) / 2,
            y: area.y + (area.height.saturating_sub(h)) / 2,
            width: w,
            height: h,
        }
    }
}

/// The regions a rendered modal hands back to its caller.
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    /// The full outer rectangle, border included.
    pub outer: Rect,
    /// Where the caller draws its content.
    pub body: Rect,
    /// The row reserved for the footer, empty when there is none.
    pub footer: Rect,
}

/// A modal frame: scrim, border, title, close button, body and footer.
pub struct Modal<'a> {
    pub title: &'a str,
    pub sizing: Sizing,
    /// Drawn right-aligned on the title row — a step counter, a file count.
    pub subtitle: Option<String>,
    /// Rows the footer needs; 0 for none.
    pub footer_rows: u16,
    /// Offer an explicit close affordance. Off for a modal that must be
    /// answered rather than dismissed.
    pub closable: bool,
    /// Override the derived height, for a frame that must not resize.
    pub fixed_height: Option<u16>,
}

impl<'a> Modal<'a> {
    pub fn new(title: &'a str) -> Self {
        Self {
            title,
            sizing: Sizing::medium(),
            subtitle: None,
            footer_rows: 0,
            closable: true,
            fixed_height: None,
        }
    }

    pub fn sizing(mut self, sizing: Sizing) -> Self {
        self.sizing = sizing;
        self
    }

    pub fn subtitle(mut self, text: impl Into<String>) -> Self {
        self.subtitle = Some(text.into());
        self
    }

    pub fn footer(mut self, rows: u16) -> Self {
        self.footer_rows = rows;
        self
    }

    pub fn closable(mut self, yes: bool) -> Self {
        self.closable = yes;
        self
    }

    pub fn fixed_height(mut self, rows: u16) -> Self {
        self.fixed_height = Some(rows);
        self
    }

    /// Draw the frame into `area` (the whole terminal) and return its regions.
    pub fn render(&self, ui: &mut Ui, area: Rect) -> Frame {
        let sizing = self.sizing.compact(ui.metrics.is_compact());
        let outer = match self.fixed_height {
            Some(rows) => sizing.with_height(rows, area),
            None => sizing.rect(area),
        };

        // The backdrop is registered before anything else and covers the whole
        // frame, so a click anywhere outside the modal resolves to it rather
        // than reaching the screen underneath.
        ui.hit_only(area, ZoneId::bare(ZoneKind::Backdrop));
        self.draw_scrim(ui, area, outer);

        // From here on is the modal's own content: the focus ring is confined
        // to it, so Tab cannot wander onto the screen behind.
        ui.zones.begin_trap();

        ui.frame.render_widget(Clear, outer);
        let border = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(ui.theme.accent).bg(ui.theme.bg_elevated))
            .style(Style::default().bg(ui.theme.bg_elevated));
        let inner = border.inner(outer);
        ui.frame.render_widget(border, outer);

        // Registered after the border so a click inside the modal is inert
        // rather than a dismiss, but before the controls so they win.
        ui.hit_only(outer, ZoneId::bare(ZoneKind::ModalFrame));

        self.draw_title(ui, outer);

        let pad_x = sizing.h_pad.min(inner.width / 2);
        let pad_y = sizing.v_pad.min(inner.height / 2);
        let padded = Rect {
            x: inner.x + pad_x,
            y: inner.y + pad_y,
            width: inner.width.saturating_sub(pad_x * 2),
            height: inner.height.saturating_sub(pad_y * 2),
        };

        let footer_rows = self.footer_rows.min(padded.height);
        let body = Rect {
            height: padded.height.saturating_sub(footer_rows),
            ..padded
        };
        let footer = Rect {
            y: body.y + body.height,
            height: footer_rows,
            ..padded
        };
        Frame { outer, body, footer }
    }

    /// Dim the area around the modal so the backdrop recedes. Skipped on a
    /// palette that paints no fills, where there is no wash to dim with.
    fn draw_scrim(&self, ui: &mut Ui, area: Rect, outer: Rect) {
        if !ui.theme.paints_fills() {
            return;
        }
        let scrim = Style::default().bg(ui.theme.scrim);
        for y in area.y..area.y + area.height {
            if y >= outer.y && y < outer.y + outer.height {
                // Two side strips, leaving the modal's own rows alone.
                ui.fill(
                    Rect {
                        x: area.x,
                        y,
                        width: outer.x.saturating_sub(area.x),
                        height: 1,
                    },
                    scrim,
                );
                let right_x = outer.x + outer.width;
                ui.fill(
                    Rect {
                        x: right_x,
                        y,
                        width: (area.x + area.width).saturating_sub(right_x),
                        height: 1,
                    },
                    scrim,
                );
            } else {
                ui.fill(
                    Rect {
                        x: area.x,
                        y,
                        width: area.width,
                        height: 1,
                    },
                    scrim,
                );
            }
        }
    }

    /// Title on the top border, with the subtitle and close button to its right.
    fn draw_title(&self, ui: &mut Ui, outer: Rect) {
        if outer.width < 6 {
            return;
        }
        let bg = ui.theme.bg_elevated;
        let title_style = Style::default()
            .fg(ui.theme.accent)
            .bg(bg)
            .add_modifier(Modifier::BOLD);

        // Close button first, so the title knows what room is left.
        let mut right_edge = outer.x + outer.width - 1;
        if self.closable {
            let close_w = 3u16;
            if outer.width > close_w + 6 {
                let rect = Rect {
                    x: right_edge.saturating_sub(close_w),
                    y: outer.y,
                    width: close_w,
                    height: 1,
                };
                let id = ZoneId::button(CLOSE_BUTTON);
                let st = ui.interactive(rect, id, false);
                let fg = if st.hovered || st.focused {
                    ui.theme.status_failed
                } else {
                    ui.theme.ink_faint
                };
                ui.line(
                    rect,
                    Line::from(vec![
                        Span::styled(" ", Style::default().bg(bg)),
                        Span::styled(
                            glyphs::CLOSE.as_str().to_string(),
                            Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" ", Style::default().bg(bg)),
                    ]),
                    Style::default().bg(bg),
                );
                right_edge = rect.x;
            }
        }

        let avail = right_edge.saturating_sub(outer.x + 2) as usize;
        if avail == 0 {
            return;
        }
        let mut spans = vec![Span::styled(" ", Style::default().bg(bg))];
        let mut used = 1usize;

        let title = truncate_cols(self.title, avail.saturating_sub(used + 1));
        used += col_width(&title) + 1;
        spans.push(Span::styled(title, title_style));
        spans.push(Span::styled(" ", Style::default().bg(bg)));

        if let Some(sub) = &self.subtitle {
            let room = avail.saturating_sub(used + 2);
            if room > 2 {
                let sub = truncate_cols(sub, room);
                spans.push(Span::styled(
                    format!("{} ", glyphs::DOT.as_str()),
                    Style::default().fg(ui.theme.ink_faint).bg(bg),
                ));
                spans.push(Span::styled(
                    sub,
                    Style::default().fg(ui.theme.ink_soft).bg(bg),
                ));
                spans.push(Span::styled(" ", Style::default().bg(bg)));
            }
        }

        ui.line(
            Rect {
                x: outer.x + 1,
                y: outer.y,
                width: right_edge.saturating_sub(outer.x + 1),
                height: 1,
            },
            Line::from(spans),
            Style::default().bg(bg),
        );
    }
}

/// Draw a button row right-aligned in a modal's footer.
pub fn render_footer(ui: &mut Ui, footer: Rect, buttons: ButtonRow) {
    if footer.height == 0 {
        return;
    }
    let row = Rect {
        y: footer.y + footer.height - 1,
        height: 1,
        ..footer
    };
    ui.fill(row, Style::default().bg(ui.theme.bg_elevated));
    buttons.align(Align::Right).render(ui, row);
}

/// A dimmed hint line, for the left of a footer.
pub fn footer_hint(ui: &mut Ui, footer: Rect, text: &str) {
    if footer.height == 0 || footer.width == 0 {
        return;
    }
    let row = Rect {
        y: footer.y + footer.height - 1,
        height: 1,
        ..footer
    };
    let st = style::key_label(ui.theme).bg(ui.theme.bg_elevated);
    let text = truncate_cols(text, row.width as usize);
    ui.text(row, text, st);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ThemeId;
    use crate::ui::kit::button::Button;
    use crate::ui::kit::focus::{Focus, Hover};
    use crate::ui::kit::tokens::Metrics;
    use crate::ui::kit::zones::Zones;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn paint(
        w: u16,
        h: u16,
        compact: bool,
        modal: Modal<'_>,
    ) -> (Vec<String>, Zones, super::Frame) {
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
        let mut out = super::Frame {
            outer: area,
            body: area,
            footer: area,
        };
        term.draw(|f| {
            let metrics = Metrics::new(area, compact);
            let mut ui = Ui::new(f, &mut zones, &theme, metrics, &focus, Hover::default(), 0);
            // A screen row underneath, to prove the backdrop shields it.
            ui.zones.push(
                Rect {
                    x: 0,
                    y: 0,
                    width: w,
                    height: 1,
                },
                ZoneId::row(0),
            );
            out = modal.render(&mut ui, area);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let lines = (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
            .collect();
        (lines, zones, out)
    }

    #[test]
    fn the_frame_stays_inside_its_host_at_every_size() {
        for (w, h) in [(40u16, 12u16), (60, 20), (80, 24), (200, 60)] {
            for sizing in [Sizing::small(), Sizing::medium(), Sizing::large()] {
                let area = Rect {
                    x: 0,
                    y: 0,
                    width: w,
                    height: h,
                };
                let r = sizing.rect(area);
                assert!(
                    r.x + r.width <= w && r.y + r.height <= h,
                    "{w}x{h}: {r:?} escapes its host"
                );
                assert!(r.width > 0 && r.height > 0);
            }
        }
    }

    #[test]
    fn a_modal_registers_its_title_close_and_shields_the_screen_behind_it() {
        let (lines, zones, frame) = paint(80, 24, false, Modal::new("Settings"));
        let joined = lines.join("\n");
        assert!(joined.contains("Settings"), "title should draw");

        // The close button is real and clickable.
        let close = zones
            .rect_of(ZoneId::button(CLOSE_BUTTON))
            .expect("close button registered");
        assert_eq!(zones.at(close.x + 1, close.y), Some(ZoneId::button(CLOSE_BUTTON)));

        // A click well outside the modal hits the backdrop, never the row below.
        assert_eq!(
            zones.at(0, 23),
            Some(ZoneId::bare(ZoneKind::Backdrop)),
            "the screen behind must not be reachable"
        );
        // A click inside but on nothing is inert, not a dismiss.
        let mid = (frame.body.x + 2, frame.body.y + 2);
        assert_eq!(zones.at(mid.0, mid.1), Some(ZoneId::bare(ZoneKind::ModalFrame)));
    }

    #[test]
    fn the_focus_ring_cannot_leave_an_open_modal() {
        let (_, zones, _) = paint(80, 24, false, Modal::new("Confirm"));
        let ring: Vec<ZoneId> = zones.tab_order().collect();
        assert!(!ring.is_empty(), "the modal must be reachable by keyboard");
        assert!(
            !ring.contains(&ZoneId::row(0)),
            "Tab escaped onto the screen behind the modal"
        );
    }

    #[test]
    fn a_modal_that_must_be_answered_offers_no_close_affordance() {
        let (_, zones, _) = paint(80, 24, false, Modal::new("Resume run?").closable(false));
        assert!(!zones.contains(ZoneId::button(CLOSE_BUTTON)));
    }

    #[test]
    fn the_body_and_footer_partition_the_interior_without_overlapping() {
        let (_, _, frame) = paint(80, 24, false, Modal::new("Export").footer(2));
        assert_eq!(
            frame.body.y + frame.body.height,
            frame.footer.y,
            "body and footer must abut, not overlap"
        );
        assert_eq!(frame.footer.height, 2);
        assert!(frame.body.y >= frame.outer.y);
        assert!(
            frame.footer.y + frame.footer.height <= frame.outer.y + frame.outer.height,
            "footer escaped the frame"
        );
    }

    #[test]
    fn compact_mode_reclaims_the_padding_rows() {
        let roomy = paint(80, 40, false, Modal::new("Settings")).2;
        let tight = paint(80, 40, true, Modal::new("Settings")).2;
        assert!(
            tight.outer.height >= roomy.outer.height,
            "compact should give the modal more of the screen, not less"
        );
        assert!(
            tight.body.height >= roomy.body.height,
            "compact should give the body more rows"
        );
    }

    #[test]
    fn a_fixed_height_frame_does_not_resize_between_steps() {
        // The import wizard's requirement: the modal must not jump as the user
        // advances, even though each step's content is a different shape.
        let a = paint(80, 30, false, Modal::new("Import").fixed_height(20)).2;
        let b = paint(80, 30, false, Modal::new("Import").fixed_height(20).footer(2)).2;
        assert_eq!(a.outer.height, 20);
        assert_eq!(b.outer.height, 20);
        assert_eq!(a.outer.y, b.outer.y, "the frame moved between steps");
    }

    #[test]
    fn a_tiny_terminal_still_produces_a_drawable_frame() {
        let (_, _, frame) = paint(24, 6, false, Modal::new("A very long modal title"));
        assert!(frame.outer.width <= 24 && frame.outer.height <= 6);
        assert!(frame.body.width > 0 || frame.body.height == 0);
    }

    #[test]
    fn a_footer_row_lays_its_buttons_inside_the_frame() {
        let theme = ThemeId::Washi.build();
        let mut zones = Zones::new();
        let focus = Focus::new();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        term.draw(|f| {
            let metrics = Metrics::new(area, false);
            let mut ui = Ui::new(f, &mut zones, &theme, metrics, &focus, Hover::default(), 0);
            let frame = Modal::new("Delete project?")
                .sizing(Sizing::small())
                .footer(1)
                .render(&mut ui, area);
            render_footer(
                &mut ui,
                frame.footer,
                ButtonRow::new(vec![
                    Button::new(ZoneId::button(0), "Cancel"),
                    Button::new(ZoneId::button(1), "Delete").danger(),
                ]),
            );
        })
        .unwrap();
        for id in [ZoneId::button(0), ZoneId::button(1)] {
            let r = zones.rect_of(id).unwrap_or_else(|| panic!("{id:?} missing"));
            assert!(r.x + r.width <= 80 && r.y < 24, "{id:?} at {r:?} escaped");
        }
    }
}
