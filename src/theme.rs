//! Color themes. One semantic contract across every palette: a single `accent`
//! for focus/nav, `status_failed` the ONLY red, `status_done`/`status_warn` read
//! success/caution, `status_working` is the live pulse. [`ThemeId`] (model.rs)
//! picks the palette; `ThemeId::build` maps it to a concrete [`Theme`].
use crate::model::{AgentRole, ChapterKind, ChapterStatus, ThemeId};
use ratatui::style::Color;
use ratatui::symbols;

/// Shorthand for an opaque RGB color.
const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

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
            bg_inset: Color::Rgb(218, 211, 195), // deepened so the gauge track reads on paper
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

    /// Sumi (墨) — honya-native dark: warm ink ground, the 藍 accent lifted for dark.
    pub fn sumi() -> Self {
        Self {
            bg: rgb(24, 23, 28),
            bg_panel: rgb(31, 30, 37),
            bg_inset: rgb(40, 38, 47),
            ink: rgb(232, 228, 220),
            ink_soft: rgb(176, 170, 160),
            ink_faint: rgb(120, 114, 106),
            rule: rgb(58, 56, 66),
            accent: rgb(132, 156, 204), // 藍, lifted
            accent_soft: rgb(100, 124, 168),
            accent_bg: rgb(40, 44, 62), // indigo-tinted selection wash
            status_pending: rgb(120, 114, 106),
            status_working: rgb(132, 156, 204), // 藍
            status_done: rgb(146, 176, 124),    // 苔
            status_failed: rgb(214, 110, 92),   // 朱
            status_warn: rgb(212, 176, 110),    // 琥珀
            status_image: rgb(186, 152, 120),   // clay
            ja_text: rgb(232, 228, 220),
            th_text: rgb(214, 224, 236),
            stream_cursor: rgb(132, 156, 204),
        }
    }

    /// Terminal — adaptive: `Reset` fg/bg inherit the host scheme and accents use
    /// the 16 ANSI slots, so honya matches the terminal's own colors (dark-tuned).
    pub fn terminal() -> Self {
        Self {
            bg: Color::Reset,
            bg_panel: Color::Reset,
            bg_inset: Color::Indexed(8), // bright black: gauge track
            ink: Color::Reset,
            ink_soft: Color::Indexed(7),
            ink_faint: Color::Indexed(8),
            rule: Color::Indexed(8),
            accent: Color::Indexed(4),
            accent_soft: Color::Indexed(12),
            // Blue band (a non-gray slot) keeps the gray status glyphs/hairlines
            // visible on a selected row; only accent's decorative bar blends in.
            accent_bg: Color::Indexed(4),
            status_pending: Color::Indexed(8),
            status_working: Color::Indexed(4),
            status_done: Color::Indexed(2),
            status_failed: Color::Indexed(1),
            status_warn: Color::Indexed(3),
            status_image: Color::Indexed(5), // magenta (no ANSI brown)
            ja_text: Color::Reset,
            th_text: Color::Reset,
            stream_cursor: Color::Indexed(6),
        }
    }

    pub fn gruvbox() -> Self {
        Self {
            bg: rgb(40, 40, 40),
            bg_panel: rgb(50, 48, 47),
            bg_inset: rgb(60, 56, 54),
            ink: rgb(235, 219, 178),
            ink_soft: rgb(213, 196, 161),
            ink_faint: rgb(146, 131, 116),
            rule: rgb(80, 73, 69),
            accent: rgb(131, 165, 152),
            accent_soft: rgb(69, 133, 136),
            accent_bg: rgb(60, 56, 54),
            status_pending: rgb(146, 131, 116),
            status_working: rgb(131, 165, 152),
            status_done: rgb(184, 187, 38),
            status_failed: rgb(251, 73, 52),
            status_warn: rgb(250, 189, 47),
            status_image: rgb(254, 128, 25),
            ja_text: rgb(235, 219, 178),
            th_text: rgb(235, 219, 178),
            stream_cursor: rgb(131, 165, 152),
        }
    }

    pub fn nord() -> Self {
        Self {
            bg: rgb(46, 52, 64),     // nord0
            bg_panel: rgb(53, 60, 74),
            bg_inset: rgb(67, 76, 94), // nord2 (distinct from accent_bg, visible track)
            ink: rgb(236, 239, 244),   // nord6
            ink_soft: rgb(216, 222, 233), // nord4
            ink_faint: rgb(123, 136, 161),
            rule: rgb(67, 76, 94), // nord2
            accent: rgb(136, 192, 208),  // nord8 frost
            accent_soft: rgb(129, 161, 193), // nord9
            accent_bg: rgb(59, 66, 82),
            status_pending: rgb(123, 136, 161),
            status_working: rgb(136, 192, 208),
            status_done: rgb(163, 190, 140),  // nord14
            status_failed: rgb(191, 97, 106), // nord11
            status_warn: rgb(235, 203, 139),  // nord13
            status_image: rgb(208, 135, 112), // nord12
            ja_text: rgb(236, 239, 244),
            th_text: rgb(229, 233, 240),       // nord5
            stream_cursor: rgb(136, 192, 208),
        }
    }

    pub fn tokyo_night() -> Self {
        Self {
            bg: rgb(26, 27, 38),
            bg_panel: rgb(31, 35, 53),
            bg_inset: rgb(41, 46, 66),
            ink: rgb(192, 202, 245),
            ink_soft: rgb(169, 177, 214),
            ink_faint: rgb(86, 95, 137),
            rule: rgb(59, 66, 97),
            accent: rgb(122, 162, 247),
            accent_soft: rgb(125, 207, 255),
            accent_bg: rgb(41, 46, 66),
            status_pending: rgb(86, 95, 137),
            status_working: rgb(122, 162, 247),
            status_done: rgb(158, 206, 106),
            status_failed: rgb(247, 118, 142),
            status_warn: rgb(224, 175, 104),
            status_image: rgb(255, 158, 100),
            ja_text: rgb(192, 202, 245),
            th_text: rgb(192, 202, 245),
            stream_cursor: rgb(122, 162, 247),
        }
    }

    pub fn dracula() -> Self {
        Self {
            bg: rgb(40, 42, 54),
            bg_panel: rgb(45, 47, 61),
            bg_inset: rgb(68, 72, 92), // lifted so the gauge track is visible
            ink: rgb(248, 248, 242),
            ink_soft: rgb(197, 198, 208),
            ink_faint: rgb(98, 114, 164),  // comment
            rule: rgb(68, 71, 90),         // current line
            accent: rgb(189, 147, 249),    // purple (signature)
            accent_soft: rgb(255, 121, 198),
            accent_bg: rgb(68, 71, 90),    // canonical selection
            status_pending: rgb(98, 114, 164),
            status_working: rgb(139, 233, 253), // cyan (live)
            status_done: rgb(80, 250, 123),
            status_failed: rgb(255, 85, 85),
            status_warn: rgb(241, 250, 140),
            status_image: rgb(255, 184, 108),
            ja_text: rgb(248, 248, 242),
            th_text: rgb(248, 248, 242),
            stream_cursor: rgb(189, 147, 249),
        }
    }

    pub fn catppuccin() -> Self {
        Self {
            bg: rgb(30, 30, 46),     // base
            bg_panel: rgb(37, 37, 57),
            bg_inset: rgb(49, 50, 68), // surface0
            ink: rgb(205, 214, 244),   // text
            ink_soft: rgb(186, 194, 222), // subtext1
            ink_faint: rgb(108, 112, 134), // overlay0
            rule: rgb(69, 71, 90),     // surface1
            accent: rgb(137, 180, 250),
            accent_soft: rgb(180, 190, 254), // lavender
            accent_bg: rgb(49, 50, 68),
            status_pending: rgb(108, 112, 134),
            status_working: rgb(137, 180, 250),
            status_done: rgb(166, 227, 161),
            status_failed: rgb(243, 139, 168),
            status_warn: rgb(249, 226, 175),
            status_image: rgb(250, 179, 135), // peach
            ja_text: rgb(205, 214, 244),
            th_text: rgb(205, 214, 244),
            stream_cursor: rgb(137, 180, 250),
        }
    }

    pub fn solarized_dark() -> Self {
        Self {
            bg: rgb(0, 43, 54),      // base03
            bg_panel: rgb(3, 48, 59),
            bg_inset: rgb(12, 62, 75), // lifted above base02 so the gauge track reads
            ink: rgb(147, 161, 161),  // base1
            ink_soft: rgb(131, 148, 150), // base0
            ink_faint: rgb(88, 110, 117), // base01
            rule: rgb(45, 72, 80),    // visible hairline (base02 alone is ~invisible on base03)
            accent: rgb(38, 139, 210),
            accent_soft: rgb(42, 161, 152),
            accent_bg: rgb(16, 68, 82), // visible selection band
            status_pending: rgb(88, 110, 117),
            status_working: rgb(38, 139, 210),
            status_done: rgb(133, 153, 0),
            status_failed: rgb(220, 50, 47),
            status_warn: rgb(181, 137, 0),
            status_image: rgb(203, 75, 22),
            ja_text: rgb(147, 161, 161),
            th_text: rgb(147, 161, 161),
            stream_cursor: rgb(38, 139, 210),
        }
    }

    pub fn solarized_light() -> Self {
        Self {
            bg: rgb(253, 246, 227),  // base3
            bg_panel: rgb(238, 232, 213), // base2
            bg_inset: rgb(227, 220, 196),
            ink: rgb(88, 110, 117),   // base01 (primary on light)
            ink_soft: rgb(101, 123, 131), // base00
            ink_faint: rgb(147, 161, 161), // base1
            rule: rgb(214, 205, 178),
            accent: rgb(38, 139, 210),
            accent_soft: rgb(42, 161, 152),
            accent_bg: rgb(221, 231, 238), // pale-blue selection wash
            status_pending: rgb(147, 161, 161),
            status_working: rgb(38, 139, 210),
            status_done: rgb(133, 153, 0),
            status_failed: rgb(220, 50, 47),
            status_warn: rgb(181, 137, 0),
            status_image: rgb(203, 75, 22),
            ja_text: rgb(88, 110, 117),
            th_text: rgb(71, 91, 98),
            stream_cursor: rgb(38, 139, 210),
        }
    }

    pub fn everforest() -> Self {
        Self {
            bg: rgb(45, 53, 59),     // bg0
            bg_panel: rgb(52, 63, 68), // bg1
            bg_inset: rgb(61, 72, 77), // bg2
            ink: rgb(211, 198, 170),   // fg
            ink_soft: rgb(157, 169, 160), // grey2
            ink_faint: rgb(122, 132, 120), // grey0
            rule: rgb(71, 82, 88),     // bg3
            accent: rgb(127, 187, 179),
            accent_soft: rgb(131, 192, 146), // aqua
            accent_bg: rgb(61, 72, 77),
            status_pending: rgb(122, 132, 120),
            status_working: rgb(127, 187, 179),
            status_done: rgb(167, 192, 128),
            status_failed: rgb(230, 126, 128),
            status_warn: rgb(219, 188, 127),
            status_image: rgb(230, 152, 117),
            ja_text: rgb(211, 198, 170),
            th_text: rgb(211, 198, 170),
            stream_cursor: rgb(127, 187, 179),
        }
    }

    /// Rosé Pine. No true green in the scheme, so success reads as `foam` cyan.
    pub fn rose_pine() -> Self {
        Self {
            bg: rgb(25, 23, 36),     // base
            bg_panel: rgb(31, 29, 46), // surface
            bg_inset: rgb(50, 47, 72), // lifted above overlay so the gauge track reads
            ink: rgb(224, 222, 244),   // text
            ink_soft: rgb(144, 140, 170), // subtle
            ink_faint: rgb(110, 106, 134), // muted
            rule: rgb(64, 61, 82),     // highlight med
            accent: rgb(196, 167, 231),  // iris (signature)
            accent_soft: rgb(212, 191, 240),
            accent_bg: rgb(38, 35, 58),
            status_pending: rgb(110, 106, 134),
            status_working: rgb(196, 167, 231), // iris
            status_done: rgb(156, 207, 216),    // foam
            status_failed: rgb(235, 111, 146),  // love
            status_warn: rgb(246, 193, 119),    // gold
            status_image: rgb(235, 188, 186),   // rose
            ja_text: rgb(224, 222, 244),
            th_text: rgb(224, 222, 244),
            stream_cursor: rgb(196, 167, 231),
        }
    }
}

/// Every theme in picker order: lights, native dark + adaptive, then schemes.
pub const ALL_THEMES: &[ThemeId] = &[
    ThemeId::Washi,
    ThemeId::SolarizedLight,
    ThemeId::Sumi,
    ThemeId::Terminal,
    ThemeId::Gruvbox,
    ThemeId::Nord,
    ThemeId::TokyoNight,
    ThemeId::Dracula,
    ThemeId::Catppuccin,
    ThemeId::SolarizedDark,
    ThemeId::Everforest,
    ThemeId::RosePine,
];

impl ThemeId {
    pub fn build(self) -> Theme {
        match self {
            ThemeId::Washi => Theme::washi(),
            ThemeId::SolarizedLight => Theme::solarized_light(),
            ThemeId::Sumi => Theme::sumi(),
            ThemeId::Terminal => Theme::terminal(),
            ThemeId::Gruvbox => Theme::gruvbox(),
            ThemeId::Nord => Theme::nord(),
            ThemeId::TokyoNight => Theme::tokyo_night(),
            ThemeId::Dracula => Theme::dracula(),
            ThemeId::Catppuccin => Theme::catppuccin(),
            ThemeId::SolarizedDark => Theme::solarized_dark(),
            ThemeId::Everforest => Theme::everforest(),
            ThemeId::RosePine => Theme::rose_pine(),
        }
    }

    /// Human-readable name for the picker / toasts.
    pub fn label(self) -> &'static str {
        match self {
            ThemeId::Washi => "Washi 和紙",
            ThemeId::SolarizedLight => "Solarized Light",
            ThemeId::Sumi => "Sumi 墨",
            ThemeId::Terminal => "Terminal (adaptive)",
            ThemeId::Gruvbox => "Gruvbox",
            ThemeId::Nord => "Nord",
            ThemeId::TokyoNight => "Tokyo Night",
            ThemeId::Dracula => "Dracula",
            ThemeId::Catppuccin => "Catppuccin Mocha",
            ThemeId::SolarizedDark => "Solarized Dark",
            ThemeId::Everforest => "Everforest",
            ThemeId::RosePine => "Rosé Pine",
        }
    }

    /// A one-word tonal tag shown beside the name.
    pub fn tone(self) -> &'static str {
        match self {
            ThemeId::Washi | ThemeId::SolarizedLight => "light",
            ThemeId::Terminal => "adaptive",
            _ => "dark",
        }
    }

    /// Index of this id within [`ALL_THEMES`] (0 if somehow absent).
    pub fn index(self) -> usize {
        ALL_THEMES.iter().position(|&t| t == self).unwrap_or(0)
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
