//! Terminal color-depth detection, and mapping truecolor palettes down to it.
//!
//! Palettes are authored in 24-bit RGB because that is how a designer picks
//! colors. Not every terminal can show them: an old `TERM`, a remote session, a
//! restricted CI shell, or a user who set `NO_COLOR` all cap what will actually
//! arrive on screen. Sending 24-bit escapes to such a terminal does not
//! gracefully approximate — it mis-renders or prints nothing useful.
//!
//! So the depth is detected **once at startup** and every palette color is
//! mapped through [`quantize`] on the way in. Colors blended at runtime
//! (hover states, elevated surfaces) go through the same function, so a derived
//! color can never out-range the palette it came from.
//!
//! [`Color::Reset`] and [`Color::Indexed`] already name what the terminal itself
//! will pick, so they pass through untouched at every depth above `Mono`.

// Scaffolding note: consumed by the theme pipeline as the redesign lands; the
// allow comes off with the kit's.
#![allow(dead_code)]

use ratatui::style::Color;

/// What the terminal can actually display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
#[repr(u8)]
pub enum ColorDepth {
    /// No color at all: `NO_COLOR`, or `TERM=dumb`.
    Mono,
    /// The 16 ANSI slots.
    Ansi16,
    /// The xterm 256-color cube.
    Indexed256,
    /// 24-bit RGB.
    #[default]
    TrueColor,
}

impl ColorDepth {
    /// Detect from the environment, in precedence order:
    ///
    /// 1. `NO_COLOR` (any non-empty value) forces [`ColorDepth::Mono`] — the
    ///    informal standard, honoured before anything else.
    /// 2. `HONYA_COLOR_DEPTH` lets a user override the guess outright.
    /// 3. `COLORTERM` of `truecolor`/`24bit` is the usual positive signal.
    /// 4. `TERM` containing `256color`, or a known-truecolor terminal name.
    /// 5. `TERM` of `dumb`/unset means no color.
    pub fn detect() -> Self {
        Self::detect_from(|k| std::env::var(k).ok())
    }

    /// The detection logic, with the environment injected so it can be tested.
    pub fn detect_from(env: impl Fn(&str) -> Option<String>) -> Self {
        if env("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return ColorDepth::Mono;
        }
        if let Some(forced) = env("HONYA_COLOR_DEPTH").and_then(|v| Self::parse(&v)) {
            return forced;
        }
        let colorterm = env("COLORTERM").unwrap_or_default().to_ascii_lowercase();
        if colorterm.contains("truecolor") || colorterm.contains("24bit") {
            return ColorDepth::TrueColor;
        }
        let term = env("TERM").unwrap_or_default().to_ascii_lowercase();
        if term.is_empty() || term == "dumb" {
            return ColorDepth::Mono;
        }
        // Terminals that support truecolor but do not always advertise it.
        const TRUECOLOR_TERMS: [&str; 5] = ["kitty", "ghostty", "wezterm", "alacritty", "foot"];
        if TRUECOLOR_TERMS.iter().any(|t| term.contains(t)) {
            return ColorDepth::TrueColor;
        }
        if term.contains("256color") {
            return ColorDepth::Indexed256;
        }
        if term.contains("color") || term.contains("xterm") || term.contains("screen") {
            return ColorDepth::Ansi16;
        }
        ColorDepth::Ansi16
    }

    /// Round-trip for the cached depth. Only values this enum produced are ever
    /// passed back in, so an unknown byte means the cache was never set.
    pub(crate) fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(ColorDepth::Mono),
            1 => Some(ColorDepth::Ansi16),
            2 => Some(ColorDepth::Indexed256),
            3 => Some(ColorDepth::TrueColor),
            _ => None,
        }
    }

    fn parse(v: &str) -> Option<Self> {
        match v.trim().to_ascii_lowercase().as_str() {
            "truecolor" | "24bit" | "rgb" => Some(ColorDepth::TrueColor),
            "256" | "256color" | "indexed" => Some(ColorDepth::Indexed256),
            "16" | "ansi" | "ansi16" => Some(ColorDepth::Ansi16),
            "mono" | "none" | "0" | "1" => Some(ColorDepth::Mono),
            _ => None,
        }
    }

    /// Whether a truecolor palette survives this depth with its distinctions
    /// intact. Palettes that do not are hidden from the picker rather than
    /// offered as a flattened near-duplicate of one another.
    pub fn is_truecolor(self) -> bool {
        matches!(self, ColorDepth::TrueColor)
    }
}

/// The xterm 6x6x6 color cube's channel levels.
const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// The 16 ANSI colors as rendered by a typical terminal. Approximate by nature —
/// a terminal is free to remap them, which is exactly why this mapping is only
/// used when nothing better is available.
const ANSI_RGB: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (170, 0, 0),
    (0, 170, 0),
    (170, 85, 0),
    (0, 0, 170),
    (170, 0, 170),
    (0, 170, 170),
    (170, 170, 170),
    (85, 85, 85),
    (255, 85, 85),
    (85, 255, 85),
    (255, 255, 85),
    (85, 85, 255),
    (255, 85, 255),
    (85, 255, 255),
    (255, 255, 255),
];

fn dist2(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
    let d = |x: u8, y: u8| {
        let d = x as i32 - y as i32;
        (d * d) as u32
    };
    d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
}

/// The nearest xterm-256 index for an RGB triple.
///
/// Searches every candidate index rather than rounding each channel to its
/// nearest cube level independently. Per-channel rounding is greedy: it cannot
/// notice that a grayscale-ramp entry, or a cube entry that is slightly off on
/// one channel, is closer overall. The search runs once per palette color at
/// startup, so exactness is free here.
pub fn nearest_256(r: u8, g: u8, b: u8) -> u8 {
    let target = (r, g, b);
    (16u8..=255)
        .min_by_key(|&i| dist2(target, indexed_to_rgb(i)))
        .unwrap_or(16)
}

/// Saturation above which a color is treated as carrying hue rather than
/// lightness. Tuned so muted palette colors — Washi's moss green at 0.32,
/// Catppuccin's pastel green at 0.29 — land on the chromatic side.
const CHROMATIC_SATURATION: f32 = 0.15;

/// Value above which the bright variant of an ANSI hue is chosen.
const BRIGHT_VALUE: f32 = 0.70;

/// HSV saturation and value: how far from gray, and how light.
fn saturation_value(r: u8, g: u8, b: u8) -> (f32, f32) {
    let max = r.max(g).max(b) as f32;
    let min = r.min(g).min(b) as f32;
    let sat = if max <= 0.0 { 0.0 } else { (max - min) / max };
    (sat, max / 255.0)
}

/// HSV hue in degrees, or `None` for a gray.
fn hue(r: u8, g: u8, b: u8) -> Option<f32> {
    let (rf, gf, bf) = (r as f32, g as f32, b as f32);
    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let delta = max - min;
    if delta <= f32::EPSILON {
        return None;
    }
    let h = if max == rf {
        60.0 * (((gf - bf) / delta) % 6.0)
    } else if max == gf {
        60.0 * (((bf - rf) / delta) + 2.0)
    } else {
        60.0 * (((rf - gf) / delta) + 4.0)
    };
    Some(if h < 0.0 { h + 360.0 } else { h })
}

/// The six chromatic ANSI hues, paired with their normal and bright slots.
const ANSI_HUES: [(f32, u8, u8); 6] = [
    (0.0, 1, 9),     // red
    (60.0, 3, 11),   // yellow
    (120.0, 2, 10),  // green
    (180.0, 6, 14),  // cyan
    (240.0, 4, 12),  // blue
    (300.0, 5, 13),  // magenta
];

/// Shortest distance between two hue angles, accounting for the wrap at 360.
fn hue_distance(a: f32, b: f32) -> f32 {
    let d = (a - b).abs() % 360.0;
    if d > 180.0 { 360.0 - d } else { d }
}

/// The nearest of the 16 ANSI slots for an RGB triple.
///
/// Euclidean RGB distance is the wrong metric at this depth, because it is
/// dominated by lightness — the very thing 16 colors can least afford to spend
/// its distinctions on. Two examples from honya's own palettes: Catppuccin's
/// pastel green and pastel pink are both "near white" in RGB, and Washi's muted
/// moss green and vermilion are both nearer ANSI brown than either is to pure
/// green or red. In both cases success and failure would become the same slot.
///
/// So a color with real saturation is matched by **hue angle**, which is what
/// actually carries the meaning, with value choosing the normal or bright
/// variant. Near-grays keep straight distance matching over all sixteen slots,
/// since for them lightness *is* the signal.
pub fn nearest_ansi16(r: u8, g: u8, b: u8) -> u8 {
    let (sat, val) = saturation_value(r, g, b);
    if sat >= CHROMATIC_SATURATION
        && let Some(h) = hue(r, g, b)
    {
        let (_, normal, bright) = ANSI_HUES
            .iter()
            .copied()
            .min_by(|(a, _, _), (c, _, _)| {
                hue_distance(h, *a).total_cmp(&hue_distance(h, *c))
            })
            .unwrap_or((0.0, 1, 9));
        return if val >= BRIGHT_VALUE { bright } else { normal };
    }
    (0u8..16)
        .min_by_key(|&i| dist2((r, g, b), ANSI_RGB[i as usize]))
        .unwrap_or(7)
}

/// Map `c` into what `depth` can display.
///
/// At `Mono` everything collapses to the terminal's own foreground/background:
/// a color that reads as "dark" becomes `Reset` (the background) and everything
/// else becomes `Reset` too — with no color available, distinctions must be
/// carried by the bold/reverse modifiers the renderer already applies, never by
/// an invented gray that may be invisible.
pub fn quantize(c: Color, depth: ColorDepth) -> Color {
    match depth {
        ColorDepth::TrueColor => c,
        ColorDepth::Mono => Color::Reset,
        ColorDepth::Indexed256 => match c {
            Color::Rgb(r, g, b) => Color::Indexed(nearest_256(r, g, b)),
            other => other,
        },
        ColorDepth::Ansi16 => match c {
            Color::Rgb(r, g, b) => Color::Indexed(nearest_ansi16(r, g, b)),
            Color::Indexed(i) if i >= 16 => {
                let (r, g, b) = indexed_to_rgb(i);
                Color::Indexed(nearest_ansi16(r, g, b))
            }
            other => other,
        },
    }
}

/// The RGB an xterm-256 index stands for. Used to step a 256-color value down
/// to 16 without losing the original hue.
pub fn indexed_to_rgb(i: u8) -> (u8, u8, u8) {
    match i {
        0..=15 => ANSI_RGB[i as usize],
        16..=231 => {
            let n = i - 16;
            let (r, g, b) = (n / 36, (n % 36) / 6, n % 6);
            (
                CUBE_LEVELS[r as usize],
                CUBE_LEVELS[g as usize],
                CUBE_LEVELS[b as usize],
            )
        }
        232..=255 => {
            let v = 8 + (i as u16 - 232) * 10;
            let v = v.min(255) as u8;
            (v, v, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k: &str| {
            owned
                .iter()
                .find(|(kk, _)| kk == k)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn no_color_wins_over_every_other_signal() {
        let e = env_of(&[
            ("NO_COLOR", "1"),
            ("COLORTERM", "truecolor"),
            ("TERM", "xterm-256color"),
            ("HONYA_COLOR_DEPTH", "truecolor"),
        ]);
        assert_eq!(ColorDepth::detect_from(e), ColorDepth::Mono);
    }

    #[test]
    fn an_empty_no_color_is_not_a_signal() {
        let e = env_of(&[("NO_COLOR", ""), ("COLORTERM", "truecolor")]);
        assert_eq!(ColorDepth::detect_from(e), ColorDepth::TrueColor);
    }

    #[test]
    fn the_explicit_override_beats_detection() {
        let e = env_of(&[("HONYA_COLOR_DEPTH", "16"), ("COLORTERM", "truecolor")]);
        assert_eq!(ColorDepth::detect_from(e), ColorDepth::Ansi16);
    }

    #[test]
    fn detection_reads_the_usual_signals() {
        assert_eq!(
            ColorDepth::detect_from(env_of(&[("COLORTERM", "truecolor")])),
            ColorDepth::TrueColor
        );
        assert_eq!(
            ColorDepth::detect_from(env_of(&[("TERM", "xterm-256color")])),
            ColorDepth::Indexed256
        );
        assert_eq!(
            ColorDepth::detect_from(env_of(&[("TERM", "xterm-ghostty")])),
            ColorDepth::TrueColor,
            "known-truecolor terminals that do not advertise COLORTERM"
        );
        assert_eq!(
            ColorDepth::detect_from(env_of(&[("TERM", "dumb")])),
            ColorDepth::Mono
        );
        assert_eq!(ColorDepth::detect_from(env_of(&[])), ColorDepth::Mono);
    }

    #[test]
    fn truecolor_is_a_passthrough() {
        let c = Color::Rgb(58, 80, 120);
        assert_eq!(quantize(c, ColorDepth::TrueColor), c);
    }

    #[test]
    fn channelless_colors_survive_every_depth_above_mono() {
        for d in [
            ColorDepth::TrueColor,
            ColorDepth::Indexed256,
            ColorDepth::Ansi16,
        ] {
            assert_eq!(quantize(Color::Reset, d), Color::Reset);
            assert_eq!(
                quantize(Color::Indexed(8), d),
                Color::Indexed(8),
                "an ANSI slot already names what the terminal picks"
            );
        }
    }

    #[test]
    fn mono_erases_all_color() {
        for c in [Color::Rgb(255, 0, 0), Color::Indexed(4), Color::Reset] {
            assert_eq!(quantize(c, ColorDepth::Mono), Color::Reset);
        }
    }

    #[test]
    fn the_256_cube_round_trips_its_own_levels_exactly() {
        // Any color that *is* a cube vertex must map to that vertex.
        for r in CUBE_LEVELS {
            for g in CUBE_LEVELS {
                for b in CUBE_LEVELS {
                    let idx = nearest_256(r, g, b);
                    assert_eq!(
                        indexed_to_rgb(idx),
                        (r, g, b),
                        "cube vertex ({r},{g},{b}) mapped to index {idx}"
                    );
                }
            }
        }
    }

    #[test]
    fn quantizing_to_256_returns_the_genuinely_nearest_index() {
        // The exact invariant, rather than a hand-picked distance threshold:
        // no other index in the 256-color space is closer to the original.
        let samples = [
            (58, 80, 120),
            (243, 239, 230),
            (178, 74, 58),
            (106, 130, 88),
            (196, 167, 231),
            (24, 23, 28),
            (0, 0, 0),
            (255, 255, 255),
        ];
        for (r, g, b) in samples {
            let Color::Indexed(i) = quantize(Color::Rgb(r, g, b), ColorDepth::Indexed256) else {
                panic!("expected an indexed color");
            };
            let got = dist2((r, g, b), indexed_to_rgb(i));
            for other in 16u8..=255 {
                assert!(
                    dist2((r, g, b), indexed_to_rgb(other)) >= got,
                    "({r},{g},{b}) -> idx {i}, but idx {other} is closer"
                );
            }
        }
    }

    #[test]
    fn grayscale_prefers_the_ramp_over_the_cube() {
        // A near-gray has a much closer match on the 24-step ramp than in the
        // coarse cube, and the ramp indices start at 232.
        let Color::Indexed(i) = quantize(Color::Rgb(120, 120, 120), ColorDepth::Indexed256) else {
            panic!("expected indexed");
        };
        assert!(i >= 232, "expected a grayscale-ramp index, got {i}");
    }

    #[test]
    fn muted_and_pastel_pairs_keep_their_hue_instead_of_merging() {
        // The two real cases that plain RGB distance got wrong: Catppuccin's
        // pastel pair both landed on white, Washi's muted pair both on brown.
        for (name, done_rgb, failed_rgb) in [
            ("catppuccin", (166u8, 227u8, 161u8), (243u8, 139u8, 168u8)),
            ("washi", (106, 130, 88), (178, 74, 58)),
        ] {
            let done = quantize(
                Color::Rgb(done_rgb.0, done_rgb.1, done_rgb.2),
                ColorDepth::Ansi16,
            );
            let failed = quantize(
                Color::Rgb(failed_rgb.0, failed_rgb.1, failed_rgb.2),
                ColorDepth::Ansi16,
            );
            assert_ne!(done, failed, "{name}: success and failure merged");
            assert!(matches!(done, Color::Indexed(2 | 10)), "{name}: want green, got {done:?}");
            assert!(matches!(failed, Color::Indexed(1 | 9)), "{name}: want red, got {failed:?}");
        }
    }

    #[test]
    fn hue_distance_wraps_around_the_circle() {
        assert!((hue_distance(350.0, 10.0) - 20.0).abs() < 0.001);
        assert!((hue_distance(10.0, 350.0) - 20.0).abs() < 0.001);
        assert!((hue_distance(0.0, 180.0) - 180.0).abs() < 0.001);
        assert!((hue_distance(90.0, 90.0)).abs() < 0.001);
    }

    #[test]
    fn hue_is_none_for_gray_and_correct_for_primaries() {
        assert!(hue(128, 128, 128).is_none());
        assert!((hue(255, 0, 0).unwrap() - 0.0).abs() < 0.1);
        assert!((hue(0, 255, 0).unwrap() - 120.0).abs() < 0.1);
        assert!((hue(0, 0, 255).unwrap() - 240.0).abs() < 0.1);
    }

    #[test]
    fn near_grays_still_reach_the_achromatic_slots() {
        // The flip side: lightness is the whole signal for a gray, so it must
        // not be forced onto a colored slot.
        for (rgb, allowed) in [
            ((250u8, 250u8, 250u8), [7u8, 15]),
            ((5, 5, 5), [0, 0]),
            ((128, 128, 128), [7, 8]),
        ] {
            let got = quantize(Color::Rgb(rgb.0, rgb.1, rgb.2), ColorDepth::Ansi16);
            let Color::Indexed(i) = got else {
                panic!("expected indexed")
            };
            assert!(allowed.contains(&i), "{rgb:?} -> {i}, want {allowed:?}");
        }
    }

    #[test]
    fn ansi16_keeps_the_broad_hue() {
        let cases = [
            ((250, 40, 40), [1u8, 9]),    // red
            ((40, 220, 40), [2, 10]),     // green
            ((40, 40, 240), [4, 12]),     // blue
            ((250, 250, 250), [7, 15]),   // white
            ((5, 5, 5), [0, 0]),          // black
        ];
        for ((r, g, b), allowed) in cases {
            let Color::Indexed(i) = quantize(Color::Rgb(r, g, b), ColorDepth::Ansi16) else {
                panic!("expected indexed");
            };
            assert!(
                allowed.contains(&i),
                "({r},{g},{b}) -> {i}, expected one of {allowed:?}"
            );
        }
    }

    #[test]
    fn stepping_256_down_to_16_keeps_the_hue() {
        // A 256-color value must not pass through unchanged at Ansi16 depth.
        let red256 = Color::Indexed(196);
        let got = quantize(red256, ColorDepth::Ansi16);
        assert!(matches!(got, Color::Indexed(1 | 9)), "got {got:?}");
    }

    #[test]
    fn indexed_to_rgb_covers_every_index_without_panicking() {
        for i in 0..=255u8 {
            let _ = indexed_to_rgb(i);
        }
        assert_eq!(indexed_to_rgb(0), (0, 0, 0));
        assert_eq!(indexed_to_rgb(255), (238, 238, 238));
    }
}
