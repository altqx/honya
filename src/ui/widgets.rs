//! Reusable render primitives shared by every screen.
//!
//! All colors are threaded from [`crate::theme::Theme`]; nothing here inlines a
//! `Color::Rgb`.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Gauge, LineGauge, Paragraph};

use crate::model::{ChapterKind, ChapterStatus};
use crate::theme::{self, Theme};

/// Render a Braille bloom spinner followed by `label`; `frame` advances the
/// animation (~10 fps from the main-loop ticker).
#[allow(dead_code)]
pub fn render_spinner(f: &mut Frame, area: Rect, frame: u64, label: &str, theme: &Theme) {
    let glyph = theme::spinner_frame(frame);
    let line = Line::from(vec![
        Span::styled(glyph, Style::default().fg(theme.status_working)),
        Span::raw(" "),
        Span::styled(label.to_string(), Style::default().fg(theme.ink_soft)),
    ]);
    f.render_widget(
        Paragraph::new(line).style(Style::default().bg(theme.bg)),
        area,
    );
}

/// Render a block-style [`Gauge`] for `done`/`total`; a `total` of 0 renders an
/// empty 0% gauge rather than dividing by zero.
pub fn render_gauge(f: &mut Frame, area: Rect, done: usize, total: usize, theme: &Theme) {
    let ratio = if total == 0 {
        0.0
    } else {
        (done as f64 / total as f64).clamp(0.0, 1.0)
    };
    let pct = (ratio * 100.0).round() as u16;
    let label = format!("{done}/{total}  {pct}%");
    let gauge = Gauge::default()
        .ratio(ratio)
        .label(Span::styled(label, Style::default().fg(theme.ink)))
        .use_unicode(true)
        .gauge_style(Style::default().fg(theme.accent_soft).bg(theme.bg_inset))
        .style(Style::default().bg(theme.bg));
    f.render_widget(gauge, area);
}

/// Render a single-line [`LineGauge`] for a 0.0–1.0 `ratio` with a trailing
/// `label`; `ratio` is clamped because `LineGauge::ratio` panics out of range.
pub fn render_line_gauge(f: &mut Frame, area: Rect, ratio: f64, label: &str, theme: &Theme) {
    let ratio = ratio.clamp(0.0, 1.0);
    let gauge = LineGauge::default()
        .ratio(ratio)
        .label(Line::styled(
            label.to_string(),
            Style::default().fg(theme.ink_soft),
        ))
        .filled_symbol(theme::GAUGE_FILLED)
        .unfilled_symbol(theme::GAUGE_TRACK)
        .filled_style(Style::default().fg(theme.accent_soft))
        .unfilled_style(Style::default().fg(theme.ink_faint))
        .style(Style::default().bg(theme.bg));
    f.render_widget(gauge, area);
}

/// The waxing-moon status glyph for a chapter, as an owned `'static` [`Span`]
/// (so it can be cached / stored in owned [`Line`]s).
pub fn status_cell(kind: ChapterKind, status: ChapterStatus, theme: &Theme) -> Span<'static> {
    let (glyph, color) = theme::status_glyph(kind, status, theme);
    Span::styled(glyph.to_string(), Style::default().fg(color))
}
