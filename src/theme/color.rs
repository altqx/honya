//! Color math: blending, contrast, and the derivation of one palette slot from
//! another.
//!
//! Palettes declare the colors a designer actually chose; the interaction states
//! that follow from them (hover, active, elevated, the text that sits *on* an
//! accent fill) are derived here rather than hand-picked twelve times over. That
//! keeps a new palette to the handful of decisions that are genuinely aesthetic.
//!
//! [`Color::Reset`] and [`Color::Indexed`] carry no RGB — the adaptive
//! `terminal` palette is built from them deliberately, so every function here
//! degrades to a sensible passthrough rather than inventing channel values it
//! cannot know.

use ratatui::style::Color;

/// WCAG AA contrast for normal-size text. The bar a control label must clear
/// to be considered legible on its fill.
pub const AA_CONTRAST: f32 = 4.5;

/// The RGB channels of `c`, or `None` for a color with no known channels
/// (`Reset`, `Indexed`, and the named ANSI variants).
pub fn channels(c: Color) -> Option<(u8, u8, u8)> {
    match c {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    }
}

/// Relative luminance per WCAG 2.x, used for contrast decisions.
pub fn luminance(c: Color) -> Option<f32> {
    let (r, g, b) = channels(c)?;
    let f = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.039_285_71 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    Some(0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b))
}

/// WCAG contrast ratio between two colors, 1.0–21.0. `None` when either side has
/// no known channels.
pub fn contrast_ratio(a: Color, b: Color) -> Option<f32> {
    let (la, lb) = (luminance(a)?, luminance(b)?);
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    Some((hi + 0.05) / (lo + 0.05))
}

/// True when `c` is dark enough that light text belongs on it. Unknown colors
/// are treated as dark, matching the common terminal default.
pub fn is_dark(c: Color) -> bool {
    luminance(c).is_none_or(|l| l < 0.18)
}

/// Blend `a` toward `b` by `t` (0.0 = `a`, 1.0 = `b`). If either side has no
/// channels the blend is meaningless, so `a` passes through unchanged.
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    let (Some((ar, ag, ab)), Some((br, bg, bb))) = (channels(a), channels(b)) else {
        return a;
    };
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    Color::Rgb(lerp(ar, br), lerp(ag, bg), lerp(ab, bb))
}

/// Move `c` toward white by `amt`.
pub fn lighten(c: Color, amt: f32) -> Color {
    mix(c, Color::Rgb(255, 255, 255), amt)
}

/// Move `c` toward black by `amt`.
pub fn darken(c: Color, amt: f32) -> Color {
    mix(c, Color::Rgb(0, 0, 0), amt)
}

/// Shift `c` away from `bg` — lighter on a dark ground, darker on a light one.
/// This is how an elevated surface is built without asking each palette which
/// direction "up" is.
pub fn elevate(c: Color, bg: Color, amt: f32) -> Color {
    if is_dark(bg) {
        lighten(c, amt)
    } else {
        darken(c, amt)
    }
}

/// Whichever of `candidates` contrasts most with `on`. Falls back to the first
/// candidate when contrast cannot be measured, so the adaptive palette still
/// gets a defined answer.
pub fn best_contrast(on: Color, candidates: &[Color]) -> Color {
    let Some(first) = candidates.first().copied() else {
        return on;
    };
    candidates
        .iter()
        .copied()
        .filter_map(|c| contrast_ratio(on, c).map(|r| (c, r)))
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(c, _)| c)
        .unwrap_or(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: Color = Color::Rgb(255, 255, 255);
    const BLACK: Color = Color::Rgb(0, 0, 0);

    #[test]
    fn contrast_ratio_matches_the_wcag_extremes() {
        let r = contrast_ratio(BLACK, WHITE).unwrap();
        assert!((r - 21.0).abs() < 0.05, "black on white should be 21:1, got {r}");
        let same = contrast_ratio(WHITE, WHITE).unwrap();
        assert!((same - 1.0).abs() < 0.001, "a color against itself is 1:1");
    }

    #[test]
    fn contrast_is_symmetric() {
        let a = Color::Rgb(58, 80, 120);
        let b = Color::Rgb(243, 239, 230);
        let ab = contrast_ratio(a, b).unwrap();
        let ba = contrast_ratio(b, a).unwrap();
        assert!((ab - ba).abs() < 0.0001);
    }

    #[test]
    fn mix_hits_both_endpoints_and_the_midpoint() {
        assert_eq!(mix(BLACK, WHITE, 0.0), BLACK);
        assert_eq!(mix(BLACK, WHITE, 1.0), WHITE);
        let mid = mix(BLACK, WHITE, 0.5);
        assert_eq!(mid, Color::Rgb(128, 128, 128));
    }

    #[test]
    fn mix_clamps_out_of_range_t() {
        assert_eq!(mix(BLACK, WHITE, -5.0), BLACK);
        assert_eq!(mix(BLACK, WHITE, 5.0), WHITE);
    }

    #[test]
    fn channelless_colors_pass_through_untouched() {
        // The adaptive `terminal` palette is built from these; they must never
        // be turned into invented RGB.
        for c in [Color::Reset, Color::Indexed(8), Color::Indexed(4)] {
            assert_eq!(mix(c, WHITE, 0.5), c);
            assert_eq!(lighten(c, 0.5), c);
            assert_eq!(darken(c, 0.5), c);
            assert_eq!(elevate(c, Color::Reset, 0.1), c);
            assert!(luminance(c).is_none());
            assert!(contrast_ratio(c, WHITE).is_none());
        }
    }

    #[test]
    fn elevate_moves_away_from_the_ground_in_both_directions() {
        let dark_bg = Color::Rgb(24, 23, 28);
        let light_bg = Color::Rgb(243, 239, 230);
        let panel_dark = Color::Rgb(31, 30, 37);
        let panel_light = Color::Rgb(236, 231, 220);

        let up = elevate(panel_dark, dark_bg, 0.08);
        assert!(
            luminance(up).unwrap() > luminance(panel_dark).unwrap(),
            "on a dark ground, elevated means lighter"
        );
        let down = elevate(panel_light, light_bg, 0.08);
        assert!(
            luminance(down).unwrap() < luminance(panel_light).unwrap(),
            "on a light ground, elevated means darker"
        );
    }

    #[test]
    fn best_contrast_picks_the_legible_side() {
        // Text on a mid-dark accent fill should come out light.
        let accent = Color::Rgb(58, 80, 120);
        assert_eq!(best_contrast(accent, &[WHITE, BLACK]), WHITE);
        // …and dark on a pale one.
        let pale = Color::Rgb(222, 224, 232);
        assert_eq!(best_contrast(pale, &[WHITE, BLACK]), BLACK);
    }

    #[test]
    fn best_contrast_falls_back_rather_than_returning_nothing() {
        assert_eq!(best_contrast(Color::Reset, &[WHITE, BLACK]), WHITE);
        assert_eq!(best_contrast(WHITE, &[]), WHITE);
    }

    #[test]
    fn is_dark_treats_unknown_channels_as_dark() {
        assert!(is_dark(BLACK));
        assert!(!is_dark(WHITE));
        assert!(is_dark(Color::Reset), "the common terminal default");
    }
}
