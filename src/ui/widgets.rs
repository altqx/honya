//! Reusable render primitives shared by every screen.
//!
//! All colors are threaded from [`crate::theme::Theme`]; nothing here inlines a
//! `Color::Rgb`.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Gauge, LineGauge, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};

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

/// Render a slim vertical scrollbar along the right column of `strip` when
/// `total` display rows overflow the viewport (`strip.height`); a fully visible
/// pane draws nothing. `offset` is the first visible row.
pub fn render_scrollbar(f: &mut Frame, strip: Rect, total: usize, offset: usize, theme: &Theme) {
    let view = strip.height as usize;
    if strip.width == 0 || view == 0 || total <= view {
        return;
    }
    let max_off = total - view;
    let mut state = ScrollbarState::new(max_off).position(offset.min(max_off));
    let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .track_symbol(Some("│"))
        .thumb_symbol("┃")
        .track_style(Style::default().fg(theme.rule))
        .thumb_style(Style::default().fg(theme.ink_soft));
    f.render_stateful_widget(bar, strip, &mut state);
}

/// Scrollbar for a bordered panel: drawn over the right border of `outer`,
/// inset one row at each end so the corner glyphs stay intact.
pub fn render_panel_scrollbar(
    f: &mut Frame,
    outer: Rect,
    total: usize,
    offset: usize,
    theme: &Theme,
) {
    if outer.width < 2 || outer.height < 3 {
        return;
    }
    let strip = Rect {
        x: outer.x,
        y: outer.y + 1,
        width: outer.width,
        height: outer.height - 2,
    };
    render_scrollbar(f, strip, total, offset, theme);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// The rightmost-column glyphs of `area` after rendering via `draw`.
    fn right_column(w: u16, h: u16, draw: impl FnOnce(&mut Frame)) -> Vec<String> {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(draw).unwrap();
        let buf = term.backend().buffer().clone();
        (0..h)
            .map(|y| buf[(w - 1, y)].symbol().to_string())
            .collect()
    }

    #[test]
    fn scrollbar_skipped_when_content_fits() {
        let theme = crate::model::ThemeId::default().build();
        let area = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 6,
        };
        let col = right_column(10, 6, |f| render_scrollbar(f, area, 6, 0, &theme));
        assert!(
            col.iter().all(|s| s == " "),
            "no track when everything is visible: {col:?}"
        );
    }

    #[test]
    fn scrollbar_thumb_tracks_offset() {
        let theme = crate::model::ThemeId::default().build();
        let area = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 6,
        };
        // 60 rows in a 6-row viewport: thumb at the top for offset 0…
        let top = right_column(10, 6, |f| render_scrollbar(f, area, 60, 0, &theme));
        assert_eq!(top[0], "┃", "thumb starts at the top: {top:?}");
        assert_eq!(top[5], "│", "track fills the rest: {top:?}");
        // …and at the bottom for the max offset.
        let bottom = right_column(10, 6, |f| render_scrollbar(f, area, 60, 54, &theme));
        assert_eq!(bottom[5], "┃", "thumb ends at the bottom: {bottom:?}");
        assert_eq!(bottom[0], "│");
    }

    #[test]
    fn panel_scrollbar_spares_the_corners() {
        let theme = crate::model::ThemeId::default().build();
        let outer = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 8,
        };
        let col = right_column(10, 8, |f| {
            render_panel_scrollbar(f, outer, 100, 0, &theme);
        });
        assert_eq!(col[0], " ", "top corner row untouched");
        assert_eq!(col[7], " ", "bottom corner row untouched");
        assert!(col[1] == "┃" || col[1] == "│", "bar drawn inside: {col:?}");
    }
}
