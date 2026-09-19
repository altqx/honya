//! The persistent frame around every screen: breadcrumb header, tab bar, toast
//! row and shortcuts bar.
//!
//! All four are built from the kit, so everything in them that looks like a
//! control is one. The breadcrumb is a row of clickable segments rather than a
//! single blob that homes to the Shelf; the tally counts are badges you can
//! click to go to the chapters they count; the tab bar is a segmented control;
//! and the toast has an explicit close affordance instead of being a dismiss
//! target with nothing to say so.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::Screen;
use crate::model::{AgentRole, LogLevel};
use crate::theme::Theme;
use crate::ui::glyphs;
use crate::ui::kit::badge::StatusDot;
use crate::ui::kit::shortcuts::{Hint, ShortcutsBar};
use crate::ui::kit::tabs::{Segment, SegmentedControl};
use crate::ui::kit::{TallySlot, Ui, ZoneId, ZoneKind};
use crate::ui::text::{col_width, thai_display_safe, truncate_cols};

/// The six tabs, in `Screen` order — the order is load-bearing (digit routing
/// and the tab bar both depend on it).
pub const TAB_SCREENS: [Screen; 6] = [
    Screen::Shelf,
    Screen::Project,
    Screen::Translate,
    Screen::Reader,
    Screen::Lexicon,
    Screen::Refine,
];

/// Zone index for the help hint, high enough not to collide with screen hints.
pub const HELP_HINT: usize = 0xF000;
/// Zone index for the update badge when one is showing.
pub const UPDATE_HINT: usize = 0xF001;

/// Aggregate chapter counts shown in the header.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatusTally {
    pub done: u32,
    pub working: u32,
    pub pending: u32,
    pub failed: u32,
}

impl StatusTally {
    pub fn total(&self) -> u32 {
        self.done + self.working + self.pending + self.failed
    }

    /// Percent of chapters fully done (0 when empty).
    pub fn percent(&self) -> u16 {
        let t = self.total();
        if t == 0 {
            0
        } else {
            ((self.done as f64 / t as f64) * 100.0).round() as u16
        }
    }

    pub fn count(&self, slot: TallySlot) -> u32 {
        match slot {
            TallySlot::Done => self.done,
            TallySlot::Working => self.working,
            TallySlot::Pending => self.pending,
            TallySlot::Failed => self.failed,
        }
    }
}

/// One clickable step in the breadcrumb, and where clicking it goes.
#[derive(Debug, Clone)]
pub struct Crumb {
    pub label: String,
    pub target: Screen,
}

impl Crumb {
    pub fn new(label: impl Into<String>, target: Screen) -> Self {
        Self {
            label: label.into(),
            target,
        }
    }
}

/// Glyph and colour for one tally slot.
fn tally_look(slot: TallySlot, theme: &Theme) -> (glyphs::Glyph, ratatui::style::Color) {
    match slot {
        TallySlot::Done => (glyphs::MOON_FULL, theme.status_done),
        TallySlot::Working => (glyphs::MOON_FIRST_QUARTER, theme.status_working),
        TallySlot::Pending => (glyphs::MOON_NEW, theme.status_pending),
        TallySlot::Failed => (glyphs::CROSS, theme.status_failed),
    }
}

/// Render the header: breadcrumb on the left, tally and remote chip on the
/// right. The breadcrumb is truncated to whatever the right side leaves, so the
/// two halves never collide.
pub fn render_header(
    ui: &mut Ui,
    area: Rect,
    crumbs: &[Crumb],
    tally: &StatusTally,
    remote: (crate::remote::protocol::RemoteState, u32),
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let row = Rect {
        height: 1,
        ..area
    };
    ui.fill(row, Style::default().bg(ui.theme.bg));

    let right_cols = render_header_right(ui, row, tally, remote);
    let budget = row.width.saturating_sub(right_cols + 1);
    render_crumbs(ui, Rect { width: budget, ..row }, crumbs);
}

/// Draw the breadcrumb segments left to right, registering each. Segments are
/// dropped from the *left* when short of room, because the rightmost names
/// where you actually are.
fn render_crumbs(ui: &mut Ui, area: Rect, crumbs: &[Crumb]) {
    if area.width == 0 || crumbs.is_empty() {
        return;
    }
    let sep = format!(" {} ", glyphs::CRUMB_SEP.as_str());
    let sep_cols = col_width(&sep) as u16;

    // Find the longest tail that fits.
    let mut start = 0usize;
    while start < crumbs.len() {
        let rest = &crumbs[start..];
        let want: u16 = rest
            .iter()
            .map(|c| col_width(&thai_display_safe(&c.label)) as u16)
            .sum::<u16>()
            + sep_cols * (rest.len().saturating_sub(1)) as u16
            + 1;
        if want <= area.width || start + 1 == crumbs.len() {
            break;
        }
        start += 1;
    }

    let mut x = area.x + 1;
    let right = area.x + area.width;
    for (n, crumb) in crumbs.iter().enumerate().skip(start) {
        if x >= right {
            break;
        }
        if n > start {
            let w = sep_cols.min(right - x);
            ui.text(
                Rect {
                    x,
                    y: area.y,
                    width: w,
                    height: 1,
                },
                sep.clone(),
                Style::default().fg(ui.theme.ink_faint).bg(ui.theme.bg),
            );
            x += w;
            if x >= right {
                break;
            }
        }
        let label = thai_display_safe(&crumb.label);
        let label = truncate_cols(&label, (right - x) as usize);
        let w = col_width(&label) as u16;
        if w == 0 {
            break;
        }
        let rect = Rect {
            x,
            y: area.y,
            width: w,
            height: 1,
        };
        let st = ui.interactive(rect, ZoneId::crumb(n), false);
        // The last segment is where you are, so it reads as the title; the
        // earlier ones are places you can go back to.
        let last = n + 1 == crumbs.len();
        let mut sty = Style::default().bg(ui.theme.bg).fg(if last {
            ui.theme.ink
        } else {
            ui.theme.ink_soft
        });
        if last {
            sty = sty.add_modifier(Modifier::BOLD);
        }
        if st.hovered || st.focused {
            sty = sty.fg(ui.theme.accent).add_modifier(Modifier::UNDERLINED);
        }
        ui.text(rect, label, sty);
        x += w;
    }
}

/// Draw the right-hand cluster and return the columns it used.
fn render_header_right(
    ui: &mut Ui,
    row: Rect,
    tally: &StatusTally,
    remote: (crate::remote::protocol::RemoteState, u32),
) -> u16 {
    use crate::remote::protocol::RemoteState;

    // Measure first, then place, so everything is right-aligned as one block.
    let pct = format!("{}%", tally.percent());
    let mut widths: Vec<(TallySlot, u16)> = Vec::with_capacity(4);
    for slot in TallySlot::ALL {
        let (glyph, _) = tally_look(slot, ui.theme);
        let n = tally.count(slot);
        widths.push((slot, glyph.cols() + digits(n)));
    }
    let (state, watchers) = remote;
    let remote_label = (state != RemoteState::Disconnected).then(|| {
        if watchers > 0 {
            format!("{}{watchers}", glyphs::REMOTE_LINK.as_str())
        } else {
            glyphs::REMOTE_LINK.as_str().to_string()
        }
    });

    const GAP: u16 = 2;
    let mut total = col_width(&pct) as u16 + 1;
    total += widths.iter().map(|&(_, w)| w + GAP).sum::<u16>();
    if let Some(r) = &remote_label {
        total += col_width(r) as u16 + GAP;
    }
    if total >= row.width {
        return 0;
    }

    let mut x = row.x + row.width - total;
    if let Some(label) = remote_label {
        let w = col_width(&label) as u16;
        let rect = Rect {
            x,
            y: row.y,
            width: w,
            height: 1,
        };
        let st = ui.interactive(rect, ZoneId::bare(ZoneKind::RemoteChip), false);
        let color = match state {
            RemoteState::Connected => ui.theme.status_done,
            RemoteState::Connecting | RemoteState::Pairing => ui.theme.status_working,
            RemoteState::Error => ui.theme.status_failed,
            RemoteState::Disconnected => ui.theme.ink_faint,
        };
        let mut sty = Style::default().fg(color).bg(ui.theme.bg);
        if st.hovered || st.focused {
            sty = sty.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        }
        ui.text(rect, label, sty);
        x += w + GAP;
    }

    for (slot, w) in widths {
        let (glyph, color) = tally_look(slot, ui.theme);
        StatusDot::new(glyph, tally.count(slot), color)
            .id(ZoneId::tally(slot))
            // Failure is the one count that must not shout when it is zero.
            .emphasize_zero(!matches!(slot, TallySlot::Failed))
            .render(
                ui,
                Rect {
                    x,
                    y: row.y,
                    width: w,
                    height: 1,
                },
            );
        x += w + GAP;
    }

    ui.text(
        Rect {
            x,
            y: row.y,
            width: col_width(&pct) as u16,
            height: 1,
        },
        pct,
        Style::default()
            .fg(ui.theme.ink_soft)
            .bg(ui.theme.bg)
            .add_modifier(Modifier::BOLD),
    );
    total
}

fn digits(n: u32) -> u16 {
    if n == 0 { 1 } else { n.ilog10() as u16 + 1 }
}

/// Render the tab bar. When a run is live, the Translate tab's mark becomes the
/// spinner of whichever agent is working, so the bar reports the run without
/// needing a separate indicator.
pub fn render_tabbar(
    ui: &mut Ui,
    area: Rect,
    active: Screen,
    run_active: bool,
    active_agent: AgentRole,
) {
    let translate_mark = if run_active {
        let set = match active_agent {
            AgentRole::Orchestrator => &glyphs::SPINNER_ORCHESTRATOR,
            AgentRole::Translator => &glyphs::SPINNER_TRANSLATOR,
            AgentRole::Reviewer => &glyphs::SPINNER_REVIEWER,
        };
        glyphs::frame_of(set, ui.frame_count).as_str().to_string()
    } else {
        "訳".to_string()
    };

    let segments = [
        Segment::new("Shelf").mark("書"),
        Segment::new("Project").mark("棚"),
        Segment::new("Translate").mark(translate_mark),
        Segment::new("Reader").mark("読"),
        Segment::new("Lexicon").mark("辞"),
        Segment::new("Refine").mark("推"),
    ];
    let active_index = TAB_SCREENS.iter().position(|&s| s == active).unwrap_or(0);
    SegmentedControl::new(&segments, active_index)
        .ids(ZoneKind::Tab, 0)
        .render(ui, area);
}

/// A full-width hairline.
pub fn render_rule(ui: &mut Ui, area: Rect) {
    crate::ui::kit::card::rule(ui, area);
}

/// Render a toast or the quit prompt. The whole row dismisses, but the close
/// glyph on the right says so.
pub fn render_toast(ui: &mut Ui, area: Rect, level: LogLevel, msg: &str, closable: bool) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let row = Rect {
        height: 1,
        ..area
    };
    let (glyph, color) = match level {
        LogLevel::Trace => (glyphs::DOT, ui.theme.ink_faint),
        LogLevel::Info => (glyphs::CHECK, ui.theme.status_done),
        LogLevel::Warn => (glyphs::FLAG, ui.theme.status_warn),
        LogLevel::Error => (glyphs::CROSS, ui.theme.status_failed),
    };
    let st = ui.hit_only(row, ZoneId::bare(ZoneKind::ToastBody));
    let bg = if st.hovered && ui.theme.paints_fills() {
        ui.theme.bg_hover
    } else {
        ui.theme.bg
    };
    ui.fill(row, Style::default().bg(bg));

    let close_cols: u16 = if closable { 3 } else { 0 };
    let body = truncate_cols(
        &thai_display_safe(msg),
        row.width.saturating_sub(close_cols + 4) as usize,
    );
    ui.line(
        row,
        Line::from(vec![
            Span::styled(" ", Style::default().bg(bg)),
            Span::styled(
                glyph.as_str().to_string(),
                Style::default().fg(color).bg(bg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ", Style::default().bg(bg)),
            Span::styled(body, Style::default().fg(ui.theme.ink_soft).bg(bg)),
        ]),
        Style::default().bg(bg),
    );

    if closable && row.width > close_cols {
        let rect = Rect {
            x: row.x + row.width - close_cols,
            y: row.y,
            width: close_cols,
            height: 1,
        };
        let cst = ui.interactive(rect, ZoneId::bare(ZoneKind::ToastClose), false);
        let fg = if cst.hovered || cst.focused {
            ui.theme.ink
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
    }
}

/// Build the shortcuts bar for a screen's hints, with the help binding reserved
/// and an update badge pinned when one is waiting.
pub fn build_bar<'a>(
    hints: &'a [Hint],
    update: Option<&str>,
    installed: Option<&str>,
) -> ShortcutsBar<'a> {
    let pinned = match (installed, update) {
        (Some(v), _) => Some(
            Hint::new(glyphs::CHECK.as_str(), format!("{v} · restart"))
                .id(ZoneId::hint(UPDATE_HINT)),
        ),
        (None, Some(v)) => Some(
            Hint::new(glyphs::UPGRADE.as_str(), format!("{v} · update"))
                .id(ZoneId::hint(UPDATE_HINT)),
        ),
        (None, None) => None,
    };
    ShortcutsBar::new(hints)
        .help(Hint::new("?", "help").id(ZoneId::hint(HELP_HINT)))
        .pinned(pinned)
}

/// Rows the shortcuts bar needs at `width`.
pub fn footer_height(
    hints: &[Hint],
    width: u16,
    update: Option<&str>,
    installed: Option<&str>,
) -> u16 {
    build_bar(hints, update, installed).height(width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ThemeId;
    use crate::remote::protocol::RemoteState;
    use crate::ui::kit::focus::{Focus, Hover};
    use crate::ui::kit::shortcuts::hints_from;
    use crate::ui::kit::tokens::Metrics;
    use crate::ui::kit::zones::Zones;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn paint(
        w: u16,
        h: u16,
        draw: impl FnOnce(&mut Ui, Rect),
    ) -> (Vec<String>, Zones) {
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
        term.draw(|f| {
            let metrics = Metrics::new(area, false);
            let mut ui = Ui::new(f, &mut zones, &theme, metrics, &focus, Hover::default(), 0);
            draw(&mut ui, area);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let lines = (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
            .collect();
        (lines, zones)
    }

    fn crumbs() -> Vec<Crumb> {
        vec![
            Crumb::new("honya 本屋", Screen::Shelf),
            Crumb::new("ある夏の物語", Screen::Project),
            Crumb::new("Vol.02 夏", Screen::Project),
        ]
    }

    fn tally() -> StatusTally {
        StatusTally {
            done: 12,
            working: 1,
            pending: 7,
            failed: 0,
        }
    }

    #[test]
    fn every_tab_registers_a_zone_in_screen_order() {
        let (_, zones) = paint(120, 1, |ui, area| {
            render_tabbar(ui, area, Screen::Shelf, false, AgentRole::Translator);
        });
        let mut prev_x = 0u16;
        for (i, screen) in TAB_SCREENS.iter().enumerate() {
            let r = zones
                .rect_of(ZoneId::tab(*screen))
                .unwrap_or_else(|| panic!("no zone for {screen:?}"));
            assert_eq!(zones.at(r.x + r.width / 2, 0), Some(ZoneId::tab(*screen)));
            if i > 0 {
                assert!(r.x >= prev_x, "tabs must run left to right");
            }
            prev_x = r.x + r.width;
        }
    }

    #[test]
    fn each_breadcrumb_segment_is_its_own_target() {
        let cs = crumbs();
        let (_, zones) = paint(120, 1, |ui, area| {
            render_header(ui, area, &cs, &tally(), (RemoteState::Disconnected, 0));
        });
        for i in 0..cs.len() {
            let r = zones
                .rect_of(ZoneId::crumb(i))
                .unwrap_or_else(|| panic!("crumb {i} not registered"));
            assert_eq!(zones.at(r.x, 0), Some(ZoneId::crumb(i)));
        }
    }

    #[test]
    fn a_cramped_header_keeps_the_segment_naming_where_you_are() {
        // Segments are dropped from the left, so the rightmost — the one that
        // says where you actually are — is the last to go.
        let cs = crumbs();
        let (_, zones) = paint(46, 1, |ui, area| {
            render_header(ui, area, &cs, &tally(), (RemoteState::Disconnected, 0));
        });
        assert!(
            zones.contains(ZoneId::crumb(cs.len() - 1)),
            "the innermost crumb must survive"
        );
    }

    #[test]
    fn the_header_halves_never_collide() {
        let cs = crumbs();
        for w in [40u16, 60, 80, 120, 200] {
            let (lines, zones) = paint(w, 1, |ui, area| {
                render_header(ui, area, &cs, &tally(), (RemoteState::Connected, 3));
            });
            assert_eq!(lines[0].chars().count(), w as usize);
            let mut rects: Vec<Rect> = zones.all().map(|(r, _)| r).collect();
            rects.sort_by_key(|r| r.x);
            for pair in rects.windows(2) {
                assert!(
                    pair[0].x + pair[0].width <= pair[1].x,
                    "at {w}: {:?} overlaps {:?}",
                    pair[0],
                    pair[1]
                );
            }
        }
    }

    #[test]
    fn every_tally_badge_is_clickable_and_decodes_to_its_slot() {
        let cs = crumbs();
        let (_, zones) = paint(120, 1, |ui, area| {
            render_header(ui, area, &cs, &tally(), (RemoteState::Disconnected, 0));
        });
        for slot in TallySlot::ALL {
            let id = ZoneId::tally(slot);
            let r = zones
                .rect_of(id)
                .unwrap_or_else(|| panic!("{slot:?} not registered"));
            assert_eq!(zones.at(r.x, 0), Some(id));
            assert_eq!(TallySlot::from_index(id.index), Some(slot));
        }
    }

    #[test]
    fn the_remote_chip_appears_only_when_linked() {
        let cs = crumbs();
        let (_, off) = paint(120, 1, |ui, area| {
            render_header(ui, area, &cs, &tally(), (RemoteState::Disconnected, 0));
        });
        assert!(!off.contains(ZoneId::bare(ZoneKind::RemoteChip)));

        let (lines, on) = paint(120, 1, |ui, area| {
            render_header(ui, area, &cs, &tally(), (RemoteState::Connected, 2));
        });
        assert!(on.contains(ZoneId::bare(ZoneKind::RemoteChip)));
        assert!(lines[0].contains('2'), "watcher count: {:?}", lines[0]);
    }

    #[test]
    fn a_toast_offers_a_close_affordance_as_well_as_a_dismiss_target() {
        let (lines, zones) = paint(60, 1, |ui, area| {
            render_toast(ui, area, LogLevel::Info, "project imported", true);
        });
        assert!(lines[0].contains("project imported"), "got {:?}", lines[0]);
        assert!(
            zones.contains(ZoneId::bare(ZoneKind::ToastClose)),
            "there must be something to click, not just a row that reacts"
        );
        assert!(zones.contains(ZoneId::bare(ZoneKind::ToastBody)));
    }

    #[test]
    fn the_quit_prompt_has_no_close_button() {
        let (_, zones) = paint(60, 1, |ui, area| {
            render_toast(ui, area, LogLevel::Warn, "press Ctrl-C again to quit", false);
        });
        assert!(!zones.contains(ZoneId::bare(ZoneKind::ToastClose)));
    }

    #[test]
    fn the_help_binding_and_an_update_badge_both_survive_a_narrow_bar() {
        let hints = hints_from(&[
            ("enter", "read"),
            ("space", "mark"),
            ("t/a", "queue"),
            ("x", "export"),
            ("y", "synopsis"),
            ("d", "del"),
            ("Q", "QA"),
        ]);
        // Help is non-negotiable at any width.
        for w in [24u16, 40, 80, 160] {
            let bar = build_bar(&hints, Some("0.8.0"), None);
            let h = bar.height(w);
            let (lines, zones) = paint(w, h, |ui, area| bar.render(ui, area));
            let joined = lines.join("\n");
            assert!(joined.contains("help"), "at {w}:\n{joined}");
            assert!(zones.contains(ZoneId::hint(HELP_HINT)));
        }
        // The update badge is reserved too, and survives wherever both fit.
        for w in [40u16, 80, 160] {
            let bar = build_bar(&hints, Some("0.8.0"), None);
            let h = bar.height(w);
            let (lines, zones) = paint(w, h, |ui, area| bar.render(ui, area));
            let joined = lines.join("\n");
            assert!(
                joined.contains("update"),
                "the update badge must not be trimmed at {w}:\n{joined}"
            );
            assert!(zones.contains(ZoneId::hint(UPDATE_HINT)));
        }
    }

    #[test]
    fn an_installed_update_reads_as_restart_not_as_another_update() {
        let hints = hints_from(&[("q", "quit")]);
        let bar = build_bar(&hints, None, Some("0.8.1"));
        let (lines, _) = paint(80, bar.height(80), |ui, area| bar.render(ui, area));
        let joined = lines.join("\n");
        assert!(joined.contains("restart"), "got:\n{joined}");
        assert!(!joined.contains("· update"), "got:\n{joined}");
    }

    #[test]
    fn a_zero_failure_count_does_not_read_as_an_alarm() {
        // It keeps its glyph so the column stays in place, but drops to faint
        // ink rather than the one red the palette allows.
        let theme = ThemeId::Washi.build();
        let cs = crumbs();
        let mut zones = Zones::new();
        let focus = Focus::new();
        let mut term = Terminal::new(TestBackend::new(120, 1)).unwrap();
        let area = Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 1,
        };
        term.draw(|f| {
            let metrics = Metrics::new(area, false);
            let mut ui = Ui::new(f, &mut zones, &theme, metrics, &focus, Hover::default(), 0);
            render_header(&mut ui, area, &cs, &tally(), (RemoteState::Disconnected, 0));
        })
        .unwrap();
        let rect = zones.rect_of(ZoneId::tally(TallySlot::Failed)).unwrap();
        let cell = term.backend().buffer()[(rect.x, 0)].clone();
        assert_ne!(
            cell.style().fg,
            Some(theme.status_failed),
            "an empty failure column must not be vermilion"
        );
    }
}
