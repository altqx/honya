//! Color themes. One semantic contract across every palette: a single `accent`
//! for focus/nav, `status_failed` the ONLY red, `status_done`/`status_warn` read
//! success/caution, `status_working` is the live pulse. [`ThemeId`] (model.rs)
//! picks the palette; `ThemeId::build` maps it to a concrete [`Theme`].
pub mod color;
pub mod quantize;

use crate::model::{AgentRole, ChapterKind, ChapterStatus, ThemeId};
use ratatui::style::Color;
use ratatui::symbols;

/// Pure extremes, used only as a legibility fallback when a palette's own
/// colors cannot carry text on its accent.
const WHITE: Color = Color::Rgb(255, 255, 255);
const BLACK: Color = Color::Rgb(0, 0, 0);

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
    pub translated_text: Color,
    pub stream_cursor: Color,

    // --- derived interaction slots (see `Base::into_theme`) ---
    /// A surface one step above `bg_panel`: modals, popovers, raised cards.
    pub bg_elevated: Color,
    /// The row under the pointer. Weaker than a selection on purpose.
    pub bg_hover: Color,
    /// A pressed or currently-activated control.
    pub bg_active: Color,
    /// The keyboard focus ring — distinct from `accent`, which also marks the
    /// active tab and other non-focused chrome.
    pub border_focus: Color,
    /// Text drawn *on* an `accent` fill (buttons, the active tab pill).
    pub accent_fg: Color,
    /// The wash behind a modal, so the backdrop recedes.
    pub scrim: Color,
    /// Text on any `ink`-filled surface.
    pub ink_inverse: Color,
}

/// A palette's authored colors: the decisions that are genuinely aesthetic.
/// The interaction states are derived from these by [`Base::into_theme`], so
/// adding a palette means picking these and nothing else.
struct Base {
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
    pub translated_text: Color,
    pub stream_cursor: Color,
}

impl Base {
    /// Derive the interaction slots and produce the full [`Theme`].
    ///
    /// Every derivation degrades correctly for the adaptive `terminal` palette,
    /// whose colors are `Reset`/`Indexed` and carry no channels: the blends pass
    /// their input straight through, which is exactly right there — that palette
    /// paints no fills at all, and signals hover with a reverse-video modifier
    /// instead.
    fn into_theme(self) -> Theme {
        let bg = self.bg;
        let accent = self.accent;
        let ink = self.ink;

        Theme {
            // One step further from the ground than `bg_panel` already sits,
            // in whichever direction "away from the background" means here.
            bg_elevated: color::elevate(self.bg_panel, bg, 0.06),
            // Hover is a hint, not a selection: a tenth of the way to accent.
            bg_hover: color::mix(bg, accent, 0.10),
            // Active is the same gesture at selection weight.
            bg_active: color::mix(bg, accent, 0.20),
            // The ring must read against accent-colored chrome beside it, so it
            // is the accent lifted away from the ground rather than the accent.
            border_focus: color::elevate(accent, bg, 0.18),
            // Text on an accent fill, measured rather than assumed: accents
            // span very light (Catppuccin lavender) to mid (Solarized blue).
            // The palette's own extremes are preferred so a button stays on
            // -palette, but a label has to be readable first — Solarized's mid
            // blue reaches only 3.4:1 against both its own extremes, so when
            // neither clears AA we widen the search to pure white/black.
            accent_fg: {
                let own = color::best_contrast(accent, &[bg, ink]);
                match color::contrast_ratio(accent, own) {
                    Some(r) if r < color::AA_CONTRAST => {
                        color::best_contrast(accent, &[bg, ink, WHITE, BLACK])
                    }
                    _ => own,
                }
            },
            // A scrim always darkens, on light grounds as much as dark ones.
            scrim: color::darken(bg, 0.35),
            ink_inverse: color::best_contrast(ink, &[bg, ink]),

            bg: self.bg,
            bg_panel: self.bg_panel,
            bg_inset: self.bg_inset,
            ink: self.ink,
            ink_soft: self.ink_soft,
            ink_faint: self.ink_faint,
            rule: self.rule,
            accent: self.accent,
            accent_soft: self.accent_soft,
            accent_bg: self.accent_bg,
            status_pending: self.status_pending,
            status_working: self.status_working,
            status_done: self.status_done,
            status_failed: self.status_failed,
            status_warn: self.status_warn,
            status_image: self.status_image,
            ja_text: self.ja_text,
            translated_text: self.translated_text,
            stream_cursor: self.stream_cursor,
        }
    }
}

impl Theme {
    pub fn washi() -> Self {
        Base {
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
            translated_text: Color::Rgb(38, 46, 58), // a hair cooler than ink
            stream_cursor: Color::Rgb(58, 80, 120),  // = accent
        }
        .into_theme()
    }

    /// Sumi (墨) — honya-native dark: warm ink ground, the 藍 accent lifted for dark.
    pub fn sumi() -> Self {
        Base {
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
            translated_text: rgb(214, 224, 236),
            stream_cursor: rgb(132, 156, 204),
        }
        .into_theme()
    }

    /// Terminal — adaptive: `Reset` fg/bg inherit the host scheme and accents use
    /// the 16 ANSI slots, so honya matches the terminal's own colors on *both*
    /// light and dark backgrounds. Two rules make it readable everywhere, since a
    /// fixed palette can't know the host's background: (1) secondary text uses
    /// bright-black (8), the one gray that stays legible on white *and* black —
    /// never white (7), which vanishes on a light terminal; (2) no solid fill sits
    /// behind `Reset`/gray text — selection is the `▌` bar + bold every list draws,
    /// not a colored band that would bury the gray columns sitting on it.
    pub fn terminal() -> Self {
        Base {
            bg: Color::Reset,
            bg_panel: Color::Reset,
            bg_inset: Color::Reset, // no fill: gauge fill + code text carry on their fg
            ink: Color::Reset,
            ink_soft: Color::Indexed(8),
            ink_faint: Color::Indexed(8),
            rule: Color::Indexed(8),
            accent: Color::Indexed(4),
            accent_soft: Color::Indexed(12),
            accent_bg: Color::Reset, // selection = the ▌ bar + bold, never a band
            status_pending: Color::Indexed(8),
            status_working: Color::Indexed(4),
            status_done: Color::Indexed(2),
            status_failed: Color::Indexed(1),
            status_warn: Color::Indexed(3),
            status_image: Color::Indexed(5), // magenta (no ANSI brown)
            ja_text: Color::Reset,
            translated_text: Color::Reset,
            stream_cursor: Color::Indexed(6),
        }
        .into_theme()
    }

    pub fn gruvbox() -> Self {
        Base {
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
            translated_text: rgb(235, 219, 178),
            stream_cursor: rgb(131, 165, 152),
        }
        .into_theme()
    }

    pub fn nord() -> Self {
        Base {
            bg: rgb(46, 52, 64), // nord0
            bg_panel: rgb(53, 60, 74),
            bg_inset: rgb(67, 76, 94), // nord2 (distinct from accent_bg, visible track)
            ink: rgb(236, 239, 244),   // nord6
            ink_soft: rgb(216, 222, 233), // nord4
            ink_faint: rgb(123, 136, 161),
            rule: rgb(67, 76, 94),           // nord2
            accent: rgb(136, 192, 208),      // nord8 frost
            accent_soft: rgb(129, 161, 193), // nord9
            accent_bg: rgb(59, 66, 82),
            status_pending: rgb(123, 136, 161),
            status_working: rgb(136, 192, 208),
            status_done: rgb(163, 190, 140),  // nord14
            status_failed: rgb(191, 97, 106), // nord11
            status_warn: rgb(235, 203, 139),  // nord13
            status_image: rgb(208, 135, 112), // nord12
            ja_text: rgb(236, 239, 244),
            translated_text: rgb(229, 233, 240), // nord5
            stream_cursor: rgb(136, 192, 208),
        }
        .into_theme()
    }

    pub fn tokyo_night() -> Self {
        Base {
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
            translated_text: rgb(192, 202, 245),
            stream_cursor: rgb(122, 162, 247),
        }
        .into_theme()
    }

    pub fn dracula() -> Self {
        Base {
            bg: rgb(40, 42, 54),
            bg_panel: rgb(45, 47, 61),
            bg_inset: rgb(68, 72, 92), // lifted so the gauge track is visible
            ink: rgb(248, 248, 242),
            ink_soft: rgb(197, 198, 208),
            ink_faint: rgb(98, 114, 164), // comment
            rule: rgb(68, 71, 90),        // current line
            accent: rgb(189, 147, 249),   // purple (signature)
            accent_soft: rgb(255, 121, 198),
            accent_bg: rgb(68, 71, 90), // canonical selection
            status_pending: rgb(98, 114, 164),
            status_working: rgb(139, 233, 253), // cyan (live)
            status_done: rgb(80, 250, 123),
            status_failed: rgb(255, 85, 85),
            status_warn: rgb(241, 250, 140),
            status_image: rgb(255, 184, 108),
            ja_text: rgb(248, 248, 242),
            translated_text: rgb(248, 248, 242),
            stream_cursor: rgb(189, 147, 249),
        }
        .into_theme()
    }

    pub fn catppuccin() -> Self {
        Base {
            bg: rgb(30, 30, 46), // base
            bg_panel: rgb(37, 37, 57),
            bg_inset: rgb(49, 50, 68),     // surface0
            ink: rgb(205, 214, 244),       // text
            ink_soft: rgb(186, 194, 222),  // subtext1
            ink_faint: rgb(108, 112, 134), // overlay0
            rule: rgb(69, 71, 90),         // surface1
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
            translated_text: rgb(205, 214, 244),
            stream_cursor: rgb(137, 180, 250),
        }
        .into_theme()
    }

    pub fn solarized_dark() -> Self {
        Base {
            bg: rgb(0, 43, 54), // base03
            bg_panel: rgb(3, 48, 59),
            bg_inset: rgb(12, 62, 75), // lifted above base02 so the gauge track reads
            ink: rgb(147, 161, 161),   // base1
            ink_soft: rgb(131, 148, 150), // base0
            ink_faint: rgb(88, 110, 117), // base01
            rule: rgb(45, 72, 80),     // visible hairline (base02 alone is ~invisible on base03)
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
            translated_text: rgb(147, 161, 161),
            stream_cursor: rgb(38, 139, 210),
        }
        .into_theme()
    }

    pub fn solarized_light() -> Self {
        Base {
            bg: rgb(253, 246, 227),       // base3
            bg_panel: rgb(238, 232, 213), // base2
            bg_inset: rgb(227, 220, 196),
            ink: rgb(88, 110, 117),        // base01 (primary on light)
            ink_soft: rgb(101, 123, 131),  // base00
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
            translated_text: rgb(71, 91, 98),
            stream_cursor: rgb(38, 139, 210),
        }
        .into_theme()
    }

    pub fn everforest() -> Self {
        Base {
            bg: rgb(45, 53, 59),           // bg0
            bg_panel: rgb(52, 63, 68),     // bg1
            bg_inset: rgb(61, 72, 77),     // bg2
            ink: rgb(211, 198, 170),       // fg
            ink_soft: rgb(157, 169, 160),  // grey2
            ink_faint: rgb(122, 132, 120), // grey0
            rule: rgb(71, 82, 88),         // bg3
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
            translated_text: rgb(211, 198, 170),
            stream_cursor: rgb(127, 187, 179),
        }
        .into_theme()
    }

    /// Rosé Pine. No true green in the scheme, so success reads as `foam` cyan.
    pub fn rose_pine() -> Self {
        Base {
            bg: rgb(25, 23, 36),           // base
            bg_panel: rgb(31, 29, 46),     // surface
            bg_inset: rgb(50, 47, 72),     // lifted above overlay so the gauge track reads
            ink: rgb(224, 222, 244),       // text
            ink_soft: rgb(144, 140, 170),  // subtle
            ink_faint: rgb(110, 106, 134), // muted
            rule: rgb(64, 61, 82),         // highlight med
            accent: rgb(196, 167, 231),    // iris (signature)
            accent_soft: rgb(212, 191, 240),
            accent_bg: rgb(38, 35, 58),
            status_pending: rgb(110, 106, 134),
            status_working: rgb(196, 167, 231), // iris
            status_done: rgb(156, 207, 216),    // foam
            status_failed: rgb(235, 111, 146),  // love
            status_warn: rgb(246, 193, 119),    // gold
            status_image: rgb(235, 188, 186),   // rose
            ja_text: rgb(224, 222, 244),
            translated_text: rgb(224, 222, 244),
            stream_cursor: rgb(196, 167, 231),
        }
        .into_theme()
    }
}

impl Theme {
    /// Map every slot into what `depth` can display.
    ///
    /// Applied once when a theme is built rather than per draw: a palette is
    /// authored in truecolor because that is how colours are chosen, but what
    /// reaches the terminal has to be something it can show.
    pub fn quantized(self, depth: quantize::ColorDepth) -> Self {
        let q = |c: Color| quantize::quantize(c, depth);
        Self {
            bg: q(self.bg),
            bg_panel: q(self.bg_panel),
            bg_inset: q(self.bg_inset),
            ink: q(self.ink),
            ink_soft: q(self.ink_soft),
            ink_faint: q(self.ink_faint),
            rule: q(self.rule),
            accent: q(self.accent),
            accent_soft: q(self.accent_soft),
            accent_bg: q(self.accent_bg),
            status_pending: q(self.status_pending),
            status_working: q(self.status_working),
            status_done: q(self.status_done),
            status_failed: q(self.status_failed),
            status_warn: q(self.status_warn),
            status_image: q(self.status_image),
            ja_text: q(self.ja_text),
            translated_text: q(self.translated_text),
            stream_cursor: q(self.stream_cursor),
            bg_elevated: q(self.bg_elevated),
            bg_hover: q(self.bg_hover),
            bg_active: q(self.bg_active),
            border_focus: q(self.border_focus),
            accent_fg: q(self.accent_fg),
            scrim: q(self.scrim),
            ink_inverse: q(self.ink_inverse),
        }
    }
}

/// The cached terminal colour depth. `u8::MAX` means "not detected yet"; every
/// other value is a [`quantize::ColorDepth`] discriminant.
static DEPTH: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(u8::MAX);

/// The terminal's colour depth, detected once. Detection reads the
/// environment, which does not change under a running process.
pub fn terminal_depth() -> quantize::ColorDepth {
    use std::sync::atomic::Ordering;
    if let Some(d) = quantize::ColorDepth::from_u8(DEPTH.load(Ordering::Relaxed)) {
        return d;
    }
    let detected = quantize::ColorDepth::detect();
    DEPTH.store(detected as u8, Ordering::Relaxed);
    detected
}

/// Pin the depth, for tests that assert on colour.
///
/// The detected depth is cached on first use, so a test cannot steer it through
/// the environment: under `cargo test` another test has usually triggered
/// detection first. At [`quantize::ColorDepth::Mono`] every palette collapses
/// to `Color::Reset`, which would make such assertions compare Reset to Reset.
#[cfg(test)]
pub fn pin_terminal_depth(depth: quantize::ColorDepth) {
    DEPTH.store(depth as u8, std::sync::atomic::Ordering::Relaxed);
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
    /// Build this palette and map it to what the terminal can show.
    ///
    /// The app uses this; [`ThemeId::build`] stays exact so tests and the GUI
    /// see the authored colours rather than whatever the test runner's
    /// environment happens to advertise.
    pub fn build_adaptive(self) -> Theme {
        self.build().quantized(terminal_depth())
    }

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

/// Per-agent spinner, ~10fps. Each echoes its agent's badge glyph so the live
/// indicator reads as that agent's signature: the Orchestrator's diamond
/// breathes like a beacon coordinating the run (◇→◆), the Translator's triangle
/// turns clockwise as it churns through the text (▲), and the Reviewer's square
/// sweeps corner-to-corner as it scans the draft (■).
pub fn agent_spinner_frame(role: AgentRole, frame: u64) -> &'static str {
    let frames: &[&str] = match role {
        AgentRole::Orchestrator => &["◇", "◈", "◆", "◈"],
        AgentRole::Translator => &["◤", "◥", "◢", "◣"],
        AgentRole::Reviewer => &["◰", "◳", "◲", "◱"],
    };
    frames[(frame as usize) % frames.len()]
}
/// Refine-agent spinner, ~10fps: a quarter-arc sweeping clockwise like a hand
/// polishing the text it is refining. Distinct from the braille run spinner.
pub const REFINE_SPINNER: [&str; 4] = ["◜", "◝", "◞", "◟"];
pub fn refine_spinner_frame(frame: u64) -> &'static str {
    REFINE_SPINNER[(frame as usize) % REFINE_SPINNER.len()]
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
        ChapterStatus::NeedsReview => ('⚑', t.status_warn),
        ChapterStatus::Failed => ('✗', t.status_failed),
        ChapterStatus::Paused => ('‖', t.status_warn),
        ChapterStatus::Partial => ('◒', t.status_warn),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::quantize::{ColorDepth, quantize};

    /// Squared RGB distance, for "is this shift bigger than that one".
    fn dist2(a: Color, b: Color) -> u32 {
        let (Some((ar, ag, ab)), Some((br, bg, bb))) = (color::channels(a), color::channels(b))
        else {
            return 0;
        };
        let d = |x: u8, y: u8| {
            let d = x as i32 - y as i32;
            (d * d) as u32
        };
        d(ar, br) + d(ag, bg) + d(ab, bb)
    }

    /// Every palette except the adaptive one, which has no channels to measure.
    fn rgb_palettes() -> impl Iterator<Item = (ThemeId, Theme)> {
        ALL_THEMES
            .iter()
            .copied()
            .filter(|id| !matches!(id, ThemeId::Terminal))
            .map(|id| (id, id.build()))
    }

    #[test]
    fn a_label_on_an_accent_fill_is_legible_in_every_palette() {
        for (id, t) in rgb_palettes() {
            let r = color::contrast_ratio(t.accent, t.accent_fg).unwrap();
            assert!(
                r >= color::AA_CONTRAST,
                "{}: accent_fg reaches only {r:.2}:1 on accent",
                id.label()
            );
        }
    }

    #[test]
    fn hover_is_perceptible_and_active_is_stronger_still() {
        for (id, t) in rgb_palettes() {
            let hover = dist2(t.bg, t.bg_hover);
            let active = dist2(t.bg, t.bg_active);
            assert!(hover > 0, "{}: hover is invisible against bg", id.label());
            assert!(
                active > hover,
                "{}: active ({active}) must read stronger than hover ({hover})",
                id.label()
            );
        }
    }

    #[test]
    fn an_elevated_surface_separates_from_the_panel_beneath_it() {
        for (id, t) in rgb_palettes() {
            assert!(
                dist2(t.bg_panel, t.bg_elevated) > 0,
                "{}: bg_elevated is indistinguishable from bg_panel",
                id.label()
            );
        }
    }

    #[test]
    fn the_semantic_contract_survives_in_every_palette() {
        // The module contract: status_failed is the only red, and success and
        // failure must never be confusable.
        for (id, t) in rgb_palettes() {
            assert!(
                dist2(t.status_done, t.status_failed) > 900,
                "{}: done and failed are too close to tell apart",
                id.label()
            );
            assert!(
                dist2(t.bg, t.ink) > 900,
                "{}: primary text does not separate from the ground",
                id.label()
            );
        }
    }

    #[test]
    fn success_and_failure_stay_distinct_after_quantization() {
        // A 256-color terminal is the realistic fallback; the palette must not
        // collapse into ambiguity there.
        for (id, t) in rgb_palettes() {
            for depth in [ColorDepth::Indexed256, ColorDepth::Ansi16] {
                let done = quantize(t.status_done, depth);
                let failed = quantize(t.status_failed, depth);
                assert_ne!(
                    done,
                    failed,
                    "{} at {depth:?}: done and failed quantize to the same slot",
                    id.label()
                );
            }
        }
    }

    #[test]
    fn the_adaptive_palette_keeps_its_derived_slots_channelless() {
        // It paints no fills on purpose, so nothing may invent RGB for it.
        let t = ThemeId::Terminal.build();
        for (name, c) in [
            ("bg_elevated", t.bg_elevated),
            ("bg_hover", t.bg_hover),
            ("bg_active", t.bg_active),
            ("scrim", t.scrim),
            ("accent_fg", t.accent_fg),
            ("ink_inverse", t.ink_inverse),
            ("border_focus", t.border_focus),
        ] {
            assert!(
                color::channels(c).is_none(),
                "{name} gained invented channels: {c:?}"
            );
        }
    }

    #[test]
    fn every_theme_id_builds_and_is_listed_once() {
        for &id in ALL_THEMES {
            let _ = id.build();
            assert_eq!(ALL_THEMES[id.index()], id, "index() disagrees with the list");
        }
        let mut seen = ALL_THEMES.to_vec();
        seen.sort_by_key(|t| format!("{t:?}"));
        seen.dedup();
        assert_eq!(seen.len(), ALL_THEMES.len(), "a palette is listed twice");
    }
}
