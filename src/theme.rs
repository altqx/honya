//! Washi-paper / sumi-ink palette: ONE indigo accent (focus/nav), vermilion reserved for failure only.
use crate::model::{AgentRole, ChapterKind, ChapterStatus};
use ratatui::style::Color;
use ratatui::symbols;

pub struct Theme {
    pub bg: Color,        // washi paper
    pub bg_panel: Color,  // recessed list panels
    pub bg_inset: Color,  // gutters, modal backing, gauge track
    pub ink: Color,       // primary
    pub ink_soft: Color,  // secondary / labels
    pub ink_faint: Color, // hints / inactive / hairline text
    pub rule: Color,      // all hairlines & borders
    pub accent: Color,
    pub accent_soft: Color,
    pub accent_bg: Color, // selection wash
    pub status_pending: Color,
    pub status_working: Color, // the live color
    pub status_done: Color,
    pub status_failed: Color, // the ONLY red anywhere
    pub status_warn: Color,
    pub status_image: Color,
    pub ja_text: Color,
    pub th_text: Color,
    pub stream_cursor: Color,
}

impl Theme {
    pub fn washi() -> Self {
        Self {
            bg: Color::Rgb(243, 239, 230),
            bg_panel: Color::Rgb(236, 231, 220),
            bg_inset: Color::Rgb(229, 223, 210),
            ink: Color::Rgb(45, 42, 38),
            ink_soft: Color::Rgb(92, 86, 78),
            ink_faint: Color::Rgb(150, 142, 130),
            rule: Color::Rgb(206, 198, 184),
            accent: Color::Rgb(58, 80, 120), // 藍
            accent_soft: Color::Rgb(108, 128, 162),
            accent_bg: Color::Rgb(222, 224, 232),
            status_pending: Color::Rgb(150, 142, 130),
            status_working: Color::Rgb(70, 96, 140), // 藍
            status_done: Color::Rgb(106, 130, 88),   // 苔
            status_failed: Color::Rgb(178, 74, 58),  // 朱
            status_warn: Color::Rgb(176, 138, 74),   // 琥珀
            status_image: Color::Rgb(150, 120, 96),  // clay
            ja_text: Color::Rgb(45, 42, 38),         // = ink
            th_text: Color::Rgb(38, 46, 58),         // a hair cooler than ink
            stream_cursor: Color::Rgb(58, 80, 120),  // = accent
        }
    }
}

/// Braille-bloom spinner, ~10fps.
pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
pub fn spinner_frame(frame: u64) -> &'static str {
    SPINNER[(frame as usize) % SPINNER.len()]
}
/// Status glyph + semantic color, using a waxing-moon metaphor (○ → ◐/◑ → ●).
pub fn status_glyph(kind: ChapterKind, status: ChapterStatus, t: &Theme) -> (char, Color) {
    if matches!(kind, ChapterKind::ImageOnly) {
        return ('▣', t.status_image);
    }
    if matches!(kind, ChapterKind::Empty) {
        return ('–', t.ink_faint);
    }
    match status {
        ChapterStatus::Pending => ('○', t.status_pending),
        ChapterStatus::Chunking => ('◔', t.status_working),
        ChapterStatus::Translating => ('◐', t.status_working),
        ChapterStatus::Reviewing => ('◑', t.status_working),
        ChapterStatus::Appended => ('◕', t.status_working),
        ChapterStatus::Done => ('●', t.status_done),
        ChapterStatus::Failed => ('✗', t.status_failed),
        ChapterStatus::Paused => ('‖', t.status_warn),
    }
}

pub fn agent_badge(role: AgentRole, t: &Theme) -> (&'static str, Color) {
    match role {
        AgentRole::Orchestrator => ("◆ Orch", t.accent),
        AgentRole::Translator => ("▲ Trans", t.status_working),
        AgentRole::Reviewer => ("■ Review", t.accent_soft),
    }
}

/// Rounded, single-line, delicate hairline border set.
pub fn hairline_set() -> symbols::border::Set<'static> {
    symbols::border::ROUNDED
}
pub const GAUGE_FILLED: &str = "▰";
pub const GAUGE_TRACK: &str = "▱";
pub const SELECT_BAR: char = '▌';
