//! The persistent frame around every screen: breadcrumb+tally header, tab bar,
//! and data-driven footer.
//!
//! Layout math is column-aware (via [`crate::ui::text`]) so the right-aligned
//! tally and global hint cluster never drift on CJK breadcrumbs.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::Screen;
use crate::theme::{self, Theme};
use crate::ui::text::{col_width, thai_display_safe};

/// The five tabs, in `Screen` order — the order is load-bearing (digit routing
/// and the tab bar both depend on it).
const TAB_SCREENS: [Screen; 5] = [
    Screen::Shelf,
    Screen::Project,
    Screen::Translate,
    Screen::Reader,
    Screen::Lexicon,
];

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

    let crumb = thai_display_safe(crumb.trim());
    let left_budget = total_cols.saturating_sub(right_cols + 2);
    let crumb_trunc = crate::ui::text::truncate_cols(&crumb, left_budget);
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
///
/// Rendered span-by-span (rather than via the `Tabs` widget) so each tab's
/// clickable [`Rect`] is known exactly; the returned zones let the App route a
/// click on a tab to its screen. Visual parity with the old widget: one space of
/// padding either side of each title, a ` │ ` divider between tabs.
pub fn render_tabbar(
    f: &mut Frame,
    area: Rect,
    active: Screen,
    run_active: bool,
    frame: u64,
    theme: &Theme,
) -> Vec<(Rect, Screen)> {
    if area.width == 0 {
        return Vec::new();
    }
    f.render_widget(
        Paragraph::new("").style(Style::default().bg(theme.bg)),
        area,
    );

    let translate_glyph: &str = if run_active {
        theme::spinner_frame(frame)
    } else {
        "訳"
    };
    let titles = [
        "1 書架 Shelf".to_string(),
        "2 棚 Project".to_string(),
        format!("3 {translate_glyph} Translate"),
        "4 読 Reader".to_string(),
        "5 辞 Lexicon".to_string(),
    ];

    let mut spans: Vec<Span> = Vec::with_capacity(titles.len() * 4);
    let mut zones: Vec<(Rect, Screen)> = Vec::with_capacity(titles.len());
    let mut x = area.x;
    let right = area.x.saturating_add(area.width);
    for (i, title) in titles.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" │ ", Style::default().fg(theme.rule)));
            x = x.saturating_add(3);
        }
        // Clickable span is the padded ` title `; width is title cols + 2.
        let tab_w = col_width(title) as u16 + 2;
        if x < right {
            let w = tab_w.min(right - x);
            zones.push((
                Rect {
                    x,
                    y: area.y,
                    width: w,
                    height: 1,
                },
                TAB_SCREENS[i],
            ));
        }
        let style = if TAB_SCREENS[i] == active {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.ink_soft)
        };
        spans.push(Span::styled(format!(" {title} "), style));
        x = x.saturating_add(tab_w);
    }

    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(theme.bg)),
        area,
    );
    zones
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
    installed: Option<&str>,
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
    // An auto-installed update shows a green "restart to apply" badge; otherwise a
    // pending update shows an amber "honya update" badge. Both sit before the cluster.
    if let Some(version) = installed {
        let done = Style::default()
            .fg(theme.status_done)
            .add_modifier(Modifier::BOLD);
        global.push(Span::styled("✓ ", done));
        global.push(Span::styled(
            format!("{version} · restart to apply  "),
            Style::default().fg(theme.status_done),
        ));
    } else if let Some(version) = update {
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn tab_zones_cover_each_screen_left_to_right() {
        let theme = crate::model::ThemeId::default().build();
        let area = Rect {
            x: 0,
            y: 1,
            width: 120,
            height: 1,
        };
        let mut term = Terminal::new(TestBackend::new(120, 3)).unwrap();
        let mut zones = Vec::new();
        term.draw(|f| {
            zones = render_tabbar(f, area, Screen::Shelf, false, 0, &theme);
        })
        .unwrap();

        // One zone per tab, in Screen order, non-overlapping and left-to-right.
        assert_eq!(zones.len(), 5);
        let order = [
            Screen::Shelf,
            Screen::Project,
            Screen::Translate,
            Screen::Reader,
            Screen::Lexicon,
        ];
        for (i, (rect, screen)) in zones.iter().enumerate() {
            assert_eq!(*screen, order[i]);
            assert_eq!(rect.y, 1);
            assert!(rect.width > 0);
            if i > 0 {
                let prev = zones[i - 1].0;
                assert!(
                    rect.x >= prev.x + prev.width,
                    "tab {i} overlaps its predecessor"
                );
            }
        }
        // The first tab starts at the bar's left edge.
        assert_eq!(zones[0].0.x, 0);
    }
}
