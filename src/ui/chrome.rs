//! The persistent frame around every screen: breadcrumb+tally header, tab bar,
//! and data-driven footer.
//!
//! Layout math is column-aware (via [`crate::ui::text`]) so the right-aligned
//! tally and global hint cluster never drift on CJK breadcrumbs.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Tabs};

use crate::app::Screen;
use crate::theme::{self, Theme};
use crate::ui::text::col_width;

/// Aggregate chapter counts shown in the header's right-aligned tally.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatusTally {
    pub done: u32,
    pub working: u32,
    pub pending: u32,
    pub failed: u32,
}

impl StatusTally {
    fn total(&self) -> u32 {
        self.done + self.working + self.pending + self.failed
    }

    /// Percent of chapters fully done (0 when empty).
    fn percent(&self) -> u16 {
        let t = self.total();
        if t == 0 {
            0
        } else {
            ((self.done as f64 / t as f64) * 100.0).round() as u16
        }
    }
}

/// Render the top header row: left breadcrumb, right
/// `●done ◐working ○pending ✗failed  NN%`. The breadcrumb is truncated to
/// whatever space the tally leaves so the two halves never collide.
pub fn render_header(f: &mut Frame, area: Rect, crumb: &str, tally: &StatusTally, theme: &Theme) {
    if area.width == 0 {
        return;
    }
    f.render_widget(
        Paragraph::new("").style(Style::default().bg(theme.bg)),
        area,
    );

    let total_cols = area.width as usize;

    let pct = tally.percent();
    let mut right: Vec<Span> = Vec::new();
    let push_stat = |spans: &mut Vec<Span>, glyph: char, count: u32, color| {
        spans.push(Span::styled(
            format!("{glyph}{count}"),
            Style::default().fg(color),
        ));
        spans.push(Span::styled("  ", Style::default().fg(theme.ink_faint)));
    };
    push_stat(&mut right, '●', tally.done, theme.status_done);
    push_stat(&mut right, '◐', tally.working, theme.status_working);
    push_stat(&mut right, '○', tally.pending, theme.status_pending);
    // Vermilion only when there are failures, else faint (no false alarm).
    let fail_color = if tally.failed > 0 {
        theme.status_failed
    } else {
        theme.ink_faint
    };
    right.push(Span::styled(
        format!("✗{}", tally.failed),
        Style::default().fg(fail_color),
    ));
    right.push(Span::styled("   ", Style::default().fg(theme.ink_faint)));
    right.push(Span::styled(
        format!("{pct}%"),
        Style::default()
            .fg(theme.ink_soft)
            .add_modifier(Modifier::BOLD),
    ));

    let right_cols: usize = right.iter().map(|s| col_width(s.content.as_ref())).sum();

    let crumb = crumb.trim();
    let left_budget = total_cols.saturating_sub(right_cols + 2);
    let crumb_trunc = crate::ui::text::truncate_cols(crumb, left_budget);
    let crumb_cols = col_width(&crumb_trunc);

    let mut spans: Vec<Span> = Vec::with_capacity(right.len() + 2);
    spans.push(Span::styled(
        format!(" {crumb_trunc}"),
        Style::default().fg(theme.ink).add_modifier(Modifier::BOLD),
    ));
    let gap = total_cols.saturating_sub(crumb_cols + 1 + right_cols);
    if gap > 0 {
        spans.push(Span::raw(" ".repeat(gap)));
    }
    spans.extend(right);

    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(theme.bg)),
        area,
    );
}

/// Render the primary tab bar; when `run_active`, tab 3's `訳` glyph is swapped
/// for the live spinner frame so the bar itself signals a running translation.
pub fn render_tabbar(
    f: &mut Frame,
    area: Rect,
    active: Screen,
    run_active: bool,
    frame: u64,
    theme: &Theme,
) {
    if area.width == 0 {
        return;
    }
    let translate_glyph: &str = if run_active {
        theme::spinner_frame(frame)
    } else {
        "訳"
    };

    let titles: Vec<Line> = vec![
        Line::from("1 書架 Shelf"),
        Line::from("2 棚 Project"),
        Line::from(format!("3 {translate_glyph} Translate")),
        Line::from("4 読 Reader"),
        Line::from("5 辞 Lexicon"),
    ];

    let tabs = Tabs::new(titles)
        .select(active.index())
        .style(Style::default().fg(theme.ink_soft).bg(theme.bg))
        .highlight_style(
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
        .divider(Span::styled(" │ ", Style::default().fg(theme.rule)))
        .padding(" ", " ");

    f.render_widget(tabs, area);
}

/// Render the footer hint bar: each `(key, label)` then the always-present
/// right-aligned global cluster `?help  :cmd  q quit`. When the row is too
/// narrow, screen-specific hints are dropped before the global cluster — the
/// contract every screen relies on.
pub fn render_footer(
    f: &mut Frame,
    area: Rect,
    hints: &[(&str, &str)],
    update: Option<&str>,
    theme: &Theme,
) {
    if area.width == 0 {
        return;
    }
    f.render_widget(
        Paragraph::new("").style(Style::default().bg(theme.bg)),
        area,
    );

    let total_cols = area.width as usize;

    let key_style = Style::default()
        .fg(theme.ink_soft)
        .add_modifier(Modifier::BOLD);
    let lbl_style = Style::default().fg(theme.ink_faint);

    let mut global: Vec<Span> = Vec::new();
    // A pending self-update shows as a persistent amber badge before the cluster.
    if let Some(version) = update {
        let warn = Style::default()
            .fg(theme.status_warn)
            .add_modifier(Modifier::BOLD);
        global.push(Span::styled("⬆ ", warn));
        global.push(Span::styled(
            format!("{version} · honya update  "),
            Style::default().fg(theme.status_warn),
        ));
    }
    global.extend([
        Span::styled("?", key_style),
        Span::styled("help  ", lbl_style),
        Span::styled(":", key_style),
        Span::styled("cmd  ", lbl_style),
        Span::styled("q", key_style),
        Span::styled(" quit", lbl_style),
    ]);
    let global_cols: usize = global.iter().map(|s| col_width(s.content.as_ref())).sum();

    // Screen-specific hints, dropped first when the row is cramped.
    let left_budget = total_cols.saturating_sub(global_cols + 2);
    let mut left: Vec<Span> = Vec::new();
    let mut left_cols = 0usize;
    left.push(Span::raw(" "));
    left_cols += 1;
    for (key, label) in hints {
        // Measure the "<key> <label>   " piece before committing so it never
        // overflows into the global cluster.
        let piece_cols = col_width(key) + 1 + col_width(label) + 3;
        if left_cols + piece_cols > left_budget {
            break;
        }
        left.push(Span::styled((*key).to_string(), key_style));
        left.push(Span::raw(" "));
        left.push(Span::styled((*label).to_string(), lbl_style));
        left.push(Span::raw("   "));
        left_cols += piece_cols;
    }

    // Filler gap pins the global cluster to the right edge.
    let gap = total_cols.saturating_sub(left_cols + global_cols);
    let mut spans = left;
    if gap > 0 {
        spans.push(Span::raw(" ".repeat(gap)));
    }
    spans.append(&mut global);

    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(theme.bg)),
        area,
    );
}
