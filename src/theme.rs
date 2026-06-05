// src/theme.rs — washi-paper / sumi-ink palette. ONE indigo accent (focus/nav),
// vermilion reserved for failure only. All values are concrete Color::Rgb.
use crate::model::{AgentRole, ChapterKind, ChapterStatus};
use ratatui::style::Color;
use ratatui::symbols;

pub struct Theme {
    // surfaces (3 depths)
    pub bg: Color,       // washi paper
    pub bg_panel: Color, // recessed list panels
    pub bg_inset: Color, // deeper: gutters, modal backing, gauge track
    // text (3 weights)
    pub ink: Color,       // primary
    pub ink_soft: Color,  // secondary / labels
    pub ink_faint: Color, // hints / inactive / hairline text
    pub rule: Color,      // all hairlines & borders
    // accent (focus / active / selection) — 藍 indigo
    pub accent: Color,
    pub accent_soft: Color,
    pub accent_bg: Color, // selection wash
    // semantic status (muted, earthen)
    pub status_pending: Color,
    pub status_working: Color, // 藍 indigo (the live color)
    pub status_done: Color,    // 苔 sage
    pub status_failed: Color,  // 朱 vermilion — the ONLY red anywhere
    pub status_warn: Color,    // 琥珀 amber
    pub status_image: Color,   // clay
    // reader / diff
    pub ja_text: Color,
    pub th_text: Color,
    pub stream_cursor: Color,
}

impl Theme {
    pub fn washi() -> Self {
        Self {
            bg: Color::Rgb(243, 239, 230),             // #F3EFE6
            bg_panel: Color::Rgb(236, 231, 220),       // #ECE7DC
            bg_inset: Color::Rgb(229, 223, 210),       // #E5DFD2
            ink: Color::Rgb(45, 42, 38),               // #2D2A26
            ink_soft: Color::Rgb(92, 86, 78),          // #5C564E
            ink_faint: Color::Rgb(150, 142, 130),      // #968E82
            rule: Color::Rgb(206, 198, 184),           // #CEC6B8
            accent: Color::Rgb(58, 80, 120),           // #3A5078 藍
            accent_soft: Color::Rgb(108, 128, 162),    // #6C80A2
            accent_bg: Color::Rgb(222, 224, 232),      // #DEE0E8
            status_pending: Color::Rgb(150, 142, 130), // #968E82
            status_working: Color::Rgb(70, 96, 140),   // #46608C 藍
            status_done: Color::Rgb(106, 130, 88),     // #6A8258 苔
            status_failed: Color::Rgb(178, 74, 58),    // #B24A3A 朱
            status_warn: Color::Rgb(176, 138, 74),     // #B08A4A 琥珀
            status_image: Color::Rgb(150, 120, 96),    // #967860 clay
            ja_text: Color::Rgb(45, 42, 38),           // = ink
            th_text: Color::Rgb(38, 46, 58),           // #262E3A (a hair cooler)
            stream_cursor: Color::Rgb(58, 80, 120),    // = accent
        }
    }
}

// --- spinner (Braille bloom, ~10fps) ---
pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
pub fn spinner_frame(frame: u64) -> &'static str {
    SPINNER[(frame as usize) % SPINNER.len()]
}
// --- status glyphs (waxing-moon metaphor: ○ → ◐/◑ → ●) ---
// returns (glyph, semantic color) — used by list rows, header tally, detail card.
pub fn status_glyph(kind: ChapterKind, status: ChapterStatus, t: &Theme) -> (char, Color) {
    if matches!(kind, ChapterKind::ImageOnly) {
        return ('▣', t.status_image);
    } // U+25A3
    if matches!(kind, ChapterKind::Empty) {
        return ('–', t.ink_faint);
    } // U+2013
    match status {
        ChapterStatus::Pending => ('○', t.status_pending), // U+25CB
        ChapterStatus::Chunking => ('◔', t.status_working), // U+25D4
        ChapterStatus::Translating => ('◐', t.status_working), // U+25D0
        ChapterStatus::Reviewing => ('◑', t.status_working), // U+25D1
        ChapterStatus::Appended => ('◕', t.status_working), // U+25D5
        ChapterStatus::Done => ('●', t.status_done),       // U+25CF
        ChapterStatus::Failed => ('✗', t.status_failed),   // U+2717
        ChapterStatus::Paused => ('‖', t.status_warn),     // U+2016
    }
}

// --- agent role badges (Run view) ---
pub fn agent_badge(role: AgentRole, t: &Theme) -> (&'static str, Color) {
    match role {
        AgentRole::Orchestrator => ("◆ Orch", t.accent), // U+25C6
        AgentRole::Translator => ("▲ Trans", t.status_working), // U+25B2
        AgentRole::Reviewer => ("■ Review", t.accent_soft), // U+25A0
    }
}

// --- hairline border set: rounded, single-line, delicate (╭╮╰╯ ─ │) ---
pub fn hairline_set() -> symbols::border::Set<'static> {
    symbols::border::ROUNDED
}
// LineGauge fill char ▰ on track ▱; selection bar ▌ (U+258C).
pub const GAUGE_FILLED: &str = "▰";
pub const GAUGE_TRACK: &str = "▱";
pub const SELECT_BAR: char = '▌';
