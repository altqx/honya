//! Progress: bars, percentage readouts, and the wizard step rail.
//!
//! The bar draws at eighth-of-a-cell resolution. On a 30-column bar that is the
//! difference between moving every 3% and moving every 0.4%, which is what makes
//! a long chapter feel like it is progressing rather than stuck.
//!
//! The stepper is the import wizard's rail. Completed steps register zones, so a
//! user can click back to one instead of pressing Esc the right number of times
//! and hoping; the current and future steps do not, because jumping ahead past
//! an unanswered question is not a thing the wizard can honour.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::ctx::Ui;
use super::zones::ZoneId;
use crate::ui::glyphs;
use crate::ui::text::{col_width, truncate_cols};

/// Draw a horizontal bar filling `ratio` (0.0–1.0) of `area`.
///
/// `ratio` is clamped rather than trusted: it is usually `done / total` and a
/// total of zero is a live possibility at the moment a run starts.
pub fn bar(ui: &mut Ui, area: Rect, ratio: f64) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let row = Rect {
        height: 1,
        ..area
    };
    let ratio = ratio.clamp(0.0, 1.0);
    let cells = row.width as usize;
    let total_eighths = (ratio * (cells * 8) as f64).round() as usize;
    let full = total_eighths / 8;
    let remainder = total_eighths % 8;

    let filled = Style::default().fg(ui.theme.accent).bg(ui.surface());
    let track = Style::default().fg(ui.theme.ink_faint).bg(ui.surface());

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(3);
    if full > 0 {
        spans.push(Span::styled(
            glyphs::BLOCK_FULL.as_str().repeat(full.min(cells)),
            filled,
        ));
    }
    if remainder > 0 && full < cells {
        spans.push(Span::styled(
            glyphs::EIGHTHS[remainder - 1].as_str().to_string(),
            filled,
        ));
    }
    let drawn = full.min(cells) + usize::from(remainder > 0 && full < cells);
    if drawn < cells {
        spans.push(Span::styled(
            glyphs::BLOCK_EMPTY.as_str().repeat(cells - drawn),
            track,
        ));
    }
    ui.line(row, Line::from(spans), track);
}

/// A bar with a trailing `done/total  NN%` readout.
pub fn bar_with_label(ui: &mut Ui, area: Rect, done: usize, total: usize) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let ratio = if total == 0 {
        0.0
    } else {
        done as f64 / total as f64
    };
    let label = format!(" {done}/{total}  {}%", (ratio * 100.0).round() as u16);
    let label_cols = col_width(&label) as u16;
    // Below this the bar would be a stub; the numbers are the more useful half.
    if area.width <= label_cols + 4 {
        ui.text(
            Rect {
                height: 1,
                ..area
            },
            truncate_cols(label.trim(), area.width as usize),
            Style::default().fg(ui.theme.ink_soft).bg(ui.surface()),
        );
        return;
    }
    bar(
        ui,
        Rect {
            width: area.width - label_cols,
            height: 1,
            ..area
        },
        ratio,
    );
    ui.text(
        Rect {
            x: area.x + area.width - label_cols,
            y: area.y,
            width: label_cols,
            height: 1,
        },
        label,
        Style::default().fg(ui.theme.ink_soft).bg(ui.surface()),
    );
}

/// Where a step sits relative to the user's position in the wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Done,
    Current,
    Ahead,
    /// An optional step the user may skip.
    Optional,
}

/// One step in a wizard.
#[derive(Debug, Clone)]
pub struct Step {
    pub label: String,
    pub state: StepState,
}

impl Step {
    pub fn new(label: impl Into<String>, state: StepState) -> Self {
        Self {
            label: label.into(),
            state,
        }
    }
}

/// Draw the step rail. Completed steps are clickable so the user can go back to
/// one directly.
pub fn stepper(ui: &mut Ui, area: Rect, steps: &[Step]) {
    if area.width == 0 || area.height == 0 || steps.is_empty() {
        return;
    }
    let row = Rect {
        height: 1,
        ..area
    };
    ui.fill(row, Style::default().bg(ui.surface()));

    // Each step costs its mark, a space, its label, and a separator. The
    // separator is deliberately tight: at five columns apiece it cost a quarter
    // of a six-step rail, which pushed the labels out and left a row of bare
    // marks that say nothing about what each step is.
    let sep = format!(" {} ", glyphs::CHEVRON_RIGHT.as_str());
    let sep_cols = col_width(&sep) as u16;
    let natural: u16 = steps
        .iter()
        .map(|s| col_width(&s.label) as u16 + 2)
        .sum::<u16>()
        + sep_cols * (steps.len() as u16 - 1);
    // Too tight for labels: fall back to marks alone, which still shows how far
    // through the wizard the user is.
    let labels = natural <= row.width;

    let mut x = row.x;
    let right = row.x + row.width;
    for (i, step) in steps.iter().enumerate() {
        if x >= right {
            break;
        }
        if i > 0 {
            let w = sep_cols.min(right - x);
            ui.line(
                Rect {
                    x,
                    y: row.y,
                    width: w,
                    height: 1,
                },
                Line::from(Span::styled(
                    sep.clone(),
                    Style::default().fg(ui.theme.ink_faint).bg(ui.surface()),
                )),
                Style::default().bg(ui.surface()),
            );
            x += w;
            if x >= right {
                break;
            }
        }

        // One consistent mark per state. Numbering the required steps read as
        // broken the moment an optional one sat between them — "2 Name" beside
        // "4 Volume" looks like a step went missing, when position already says
        // the order and the label already says what it is.
        let (mark, color) = match step.state {
            StepState::Done => (glyphs::CHECK, ui.theme.status_done),
            StepState::Current => (glyphs::MOON_FULL, ui.theme.accent),
            StepState::Optional => (glyphs::DOT, ui.theme.ink_faint),
            StepState::Ahead => (glyphs::MOON_NEW, ui.theme.ink_faint),
        };
        let mark = mark.as_str().to_string();

        let label = if labels { step.label.as_str() } else { "" };
        let want = col_width(&mark) as u16 + if labels { col_width(label) as u16 + 1 } else { 0 };
        let w = want.min(right - x);
        let rect = Rect {
            x,
            y: row.y,
            width: w,
            height: 1,
        };

        // Only a completed step is navigable: jumping forward past a question
        // the wizard has not been given an answer to is not honourable.
        let st = if step.state == StepState::Done {
            ui.interactive(rect, ZoneId::step(i), false)
        } else {
            ui.state_of(ZoneId::step(i), step.state == StepState::Current)
        };

        let mut sty = Style::default()
            .fg(color)
            .bg(ui.surface_of(st));
        if step.state == StepState::Current {
            sty = sty.add_modifier(Modifier::BOLD);
        }
        if st.hovered {
            sty = sty.add_modifier(Modifier::UNDERLINED);
        }

        let mut spans = vec![Span::styled(mark, sty)];
        if labels && !label.is_empty() {
            let budget = w.saturating_sub(col_width(&spans[0].content) as u16 + 1) as usize;
            spans.push(Span::styled(
                format!(" {}", truncate_cols(label, budget)),
                sty,
            ));
        }
        ui.line(rect, Line::from(spans), sty);
        x = x.saturating_add(w);
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

    fn filled_cols(text: &str) -> usize {
        text.chars()
            .filter(|c| *c == '\u{2588}' || ('\u{2589}'..='\u{258F}').contains(c))
            .count()
    }

    #[test]
    fn a_bar_always_fills_its_whole_width() {
        for r in [0.0, 0.01, 0.25, 0.5, 0.99, 1.0] {
            let (text, _) = paint(20, |ui, area| bar(ui, area, r));
            assert_eq!(text.chars().count(), 20, "ratio {r} left a ragged bar");
        }
    }

    #[test]
    fn the_extremes_are_exact() {
        let (empty, _) = paint(16, |ui, area| bar(ui, area, 0.0));
        assert_eq!(filled_cols(&empty), 0, "0% must draw nothing filled");
        let (full, _) = paint(16, |ui, area| bar(ui, area, 1.0));
        assert_eq!(filled_cols(&full), 16, "100% must fill every cell");
    }

    #[test]
    fn an_out_of_range_ratio_is_clamped_rather_than_overflowing() {
        for r in [-5.0, 2.0, f64::INFINITY] {
            let (text, _) = paint(10, |ui, area| bar(ui, area, r));
            assert_eq!(text.chars().count(), 10, "ratio {r} overflowed");
        }
        // NaN clamps to the low end rather than panicking.
        let (nan, _) = paint(10, |ui, area| bar(ui, area, f64::NAN));
        assert_eq!(nan.chars().count(), 10);
    }

    #[test]
    fn sub_cell_resolution_distinguishes_ratios_a_whole_cell_bar_could_not() {
        // Within a single cell's worth of progress on a 10-wide bar, three
        // ratios that whole-cell rendering would draw identically.
        let a = paint(10, |ui, area| bar(ui, area, 0.02)).0;
        let b = paint(10, |ui, area| bar(ui, area, 0.05)).0;
        let c = paint(10, |ui, area| bar(ui, area, 0.09)).0;
        assert_ne!(a, b, "0.02 and 0.05 should differ");
        assert_ne!(b, c, "0.05 and 0.09 should differ");
    }

    #[test]
    fn a_zero_total_reads_as_zero_rather_than_dividing_by_it() {
        let (text, _) = paint(30, |ui, area| bar_with_label(ui, area, 0, 0));
        assert!(text.contains("0/0"), "got {text:?}");
        assert!(text.contains("0%"), "got {text:?}");
    }

    #[test]
    fn a_cramped_label_bar_keeps_the_numbers_and_drops_the_bar() {
        let (text, _) = paint(12, |ui, area| bar_with_label(ui, area, 7, 9));
        assert!(text.contains("7/9"), "the numbers are the useful half: {text:?}");
        assert_eq!(text.chars().count(), 12);
    }

    #[test]
    fn only_completed_steps_are_navigable() {
        let steps = [
            Step::new("Pick", StepState::Done),
            Step::new("Name", StepState::Done),
            Step::new("Volume", StepState::Current),
            Step::new("Synopsis", StepState::Optional),
            Step::new("Import", StepState::Ahead),
        ];
        let (_, zones) = paint(70, |ui, area| stepper(ui, area, &steps));
        assert!(zones.contains(ZoneId::step(0)), "a done step is clickable");
        assert!(zones.contains(ZoneId::step(1)));
        assert!(
            !zones.contains(ZoneId::step(2)),
            "the current step is not a destination"
        );
        assert!(
            !zones.contains(ZoneId::step(4)),
            "jumping ahead past an unanswered step is not honourable"
        );
    }

    #[test]
    fn a_narrow_rail_drops_labels_but_keeps_the_marks() {
        let steps = [
            Step::new("Pick a file", StepState::Done),
            Step::new("Project name", StepState::Current),
            Step::new("Volume number", StepState::Ahead),
        ];
        let (wide, _) = paint(70, |ui, area| stepper(ui, area, &steps));
        assert!(wide.contains("Pick a file"), "got {wide:?}");

        let (narrow, zones) = paint(14, |ui, area| stepper(ui, area, &steps));
        assert_eq!(narrow.chars().count(), 14, "the rail overflowed");
        for (rect, id) in zones.all() {
            assert!(rect.x + rect.width <= 14, "{id:?} at {rect:?} escaped");
        }
    }

    #[test]
    fn an_empty_stepper_draws_nothing() {
        let (_, zones) = paint(40, |ui, area| stepper(ui, area, &[]));
        assert!(zones.is_empty());
    }
}
