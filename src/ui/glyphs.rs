//! Every glyph the UI draws, with a documented fallback and a pinned width.
//!
//! Three problems this module exists to solve, all of which used to be spread
//! across `theme.rs` and a dozen inline string literals:
//!
//! 1. **Fonts vary.** A terminal without a Unicode-complete font renders a
//!    missing glyph as a replacement box, which is both ugly and the wrong
//!    width. Every glyph here carries an ASCII fallback.
//! 2. **Width must be known, not discovered.** honya lays out in display
//!    columns ([`crate::ui::text`]), so a glyph whose width surprises the layout
//!    shifts everything after it. Each glyph declares its column count and a
//!    test pins that declaration against `unicode-width`.
//! 3. **Ambiguous width is a real hazard here.** Most of the geometric shapes
//!    below are East Asian *Ambiguous*: one column in a Latin locale, two in a
//!    CJK-configured terminal. honya translates Japanese, so its users are
//!    unusually likely to run exactly that configuration. [`GlyphSet::Ascii`]
//!    exists as the escape hatch, since its glyphs are unambiguously one column.
//!
//! The invariant worth stating outright: **every animation frame is exactly one
//! column**, so a spinner never shifts the label that follows it.

// Scaffolding note: this module is the component kit being built ahead of its
// callers. Screens and overlays are ported onto it one at a time, so parts of
// the surface are legitimately unused between those steps. This allow comes off
// once the last screen is ported — it must not outlive the redesign.
#![allow(dead_code)]
use std::sync::atomic::{AtomicU8, Ordering};

/// Which repertoire to draw from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GlyphSet {
    /// The full repertoire.
    #[default]
    Unicode,
    /// Unambiguously single-column ASCII, for terminals with a narrow font or an
    /// East Asian locale that would widen the geometric shapes.
    Ascii,
}

/// Process-wide glyph repertoire. An atomic rather than a threaded parameter
/// because glyphs are read from deep inside render code on the UI thread and
/// the value is set once at startup.
static ACTIVE: AtomicU8 = AtomicU8::new(0);

pub fn set_glyph_set(set: GlyphSet) {
    ACTIVE.store(
        match set {
            GlyphSet::Unicode => 0,
            GlyphSet::Ascii => 1,
        },
        Ordering::Relaxed,
    );
}

pub fn glyph_set() -> GlyphSet {
    match ACTIVE.load(Ordering::Relaxed) {
        1 => GlyphSet::Ascii,
        _ => GlyphSet::Unicode,
    }
}

/// One glyph: a preferred form, an ASCII fallback, and the columns both occupy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyph {
    unicode: &'static str,
    ascii: &'static str,
    cols: u16,
}

impl Glyph {
    const fn new(unicode: &'static str, ascii: &'static str, cols: u16) -> Self {
        Self {
            unicode,
            ascii,
            cols,
        }
    }

    /// The form to draw under the active repertoire.
    pub fn as_str(self) -> &'static str {
        match glyph_set() {
            GlyphSet::Unicode => self.unicode,
            GlyphSet::Ascii => self.ascii,
        }
    }

    /// Columns this glyph occupies. Declared, not measured, so layout math can
    /// be `const`-friendly and is pinned by a test.
    pub const fn cols(self) -> u16 {
        self.cols
    }
}

impl std::fmt::Display for Glyph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<Glyph> for String {
    fn from(g: Glyph) -> Self {
        g.as_str().to_string()
    }
}

// ---------------------------------------------------------------------------
// Chapter status — a waxing moon from untouched to done.
// ---------------------------------------------------------------------------

/// Not started.
pub const MOON_NEW: Glyph = Glyph::new("○", "-", 1);
/// Chunking.
pub const MOON_CRESCENT: Glyph = Glyph::new("◔", ".", 1);
/// Translating.
pub const MOON_FIRST_QUARTER: Glyph = Glyph::new("◐", "o", 1);
/// Reviewing.
pub const MOON_LAST_QUARTER: Glyph = Glyph::new("◑", "o", 1);
/// Appended, not yet sealed.
pub const MOON_GIBBOUS: Glyph = Glyph::new("◕", "O", 1);
/// Done.
pub const MOON_FULL: Glyph = Glyph::new("●", "#", 1);
/// Partially complete.
pub const MOON_HALF_DOWN: Glyph = Glyph::new("◒", "o", 1);

/// An image-only page, which skips the agents entirely.
pub const IMAGE_PAGE: Glyph = Glyph::new("▣", "I", 1);
/// An empty page.
pub const EMPTY_PAGE: Glyph = Glyph::new("–", "~", 1);
/// Flagged for review.
pub const FLAG: Glyph = Glyph::new("⚑", "!", 1);
/// Failed.
pub const CROSS: Glyph = Glyph::new("✗", "x", 1);
/// Paused.
pub const PAUSE: Glyph = Glyph::new("‖", "=", 1);
/// Succeeded.
pub const CHECK: Glyph = Glyph::new("✓", "v", 1);

// ---------------------------------------------------------------------------
// Chrome
// ---------------------------------------------------------------------------

/// The selection bar down the left of a focused row.
pub const SELECT_BAR: Glyph = Glyph::new("▌", ">", 1);
/// A left accent rail — the primary way a block is set apart, in place of a box.
pub const ACCENT_RAIL: Glyph = Glyph::new("▏", "|", 1);
/// A heavier rail, for the focused or active block.
pub const ACCENT_RAIL_STRONG: Glyph = Glyph::new("▎", "|", 1);
/// Hairline rules and vertical dividers.
pub const RULE_H: Glyph = Glyph::new("─", "-", 1);
pub const RULE_V: Glyph = Glyph::new("│", "|", 1);
/// Scrollbar track and thumb.
pub const SCROLL_TRACK: Glyph = Glyph::new("│", "|", 1);
pub const SCROLL_THUMB: Glyph = Glyph::new("┃", "#", 1);
/// Gauge cells.
pub const GAUGE_FILLED: Glyph = Glyph::new("▰", "#", 1);
pub const GAUGE_TRACK: Glyph = Glyph::new("▱", ".", 1);
/// The remote-link indicator.
pub const REMOTE_LINK: Glyph = Glyph::new("⇄", "=", 1);
/// An available update.
pub const UPGRADE: Glyph = Glyph::new("⬆", "^", 1);
/// A modal's close affordance.
pub const CLOSE: Glyph = Glyph::new("×", "x", 1);
/// Disclosure arrows for a collapsible block.
pub const CHEVRON_DOWN: Glyph = Glyph::new("▾", "v", 1);
pub const CHEVRON_RIGHT: Glyph = Glyph::new("▸", ">", 1);
/// A separator between breadcrumb segments.
pub const CRUMB_SEP: Glyph = Glyph::new("›", ">", 1);
/// A neutral bullet.
pub const DOT: Glyph = Glyph::new("·", ".", 1);
/// Checkbox states.
pub const CHECKBOX_ON: Glyph = Glyph::new("◼", "x", 1);
pub const CHECKBOX_OFF: Glyph = Glyph::new("◻", " ", 1);
/// Toggle states.
pub const TOGGLE_ON: Glyph = Glyph::new("◉", "*", 1);
pub const TOGGLE_OFF: Glyph = Glyph::new("○", "o", 1);
/// Ellipsis, when content is trimmed.
pub const ELLIPSIS: Glyph = Glyph::new("…", ".", 1);

// ---------------------------------------------------------------------------
// Agents
// ---------------------------------------------------------------------------

pub const BADGE_ORCHESTRATOR: Glyph = Glyph::new("◆", "*", 1);
pub const BADGE_TRANSLATOR: Glyph = Glyph::new("▲", "^", 1);
pub const BADGE_REVIEWER: Glyph = Glyph::new("■", "#", 1);

// ---------------------------------------------------------------------------
// Animation. Every frame in every set is one column.
// ---------------------------------------------------------------------------

/// The general-purpose braille bloom, ~10fps.
pub const SPINNER: [Glyph; 10] = [
    Glyph::new("⠋", "|", 1),
    Glyph::new("⠙", "/", 1),
    Glyph::new("⠹", "/", 1),
    Glyph::new("⠸", "-", 1),
    Glyph::new("⠼", "-", 1),
    Glyph::new("⠴", "\\", 1),
    Glyph::new("⠦", "\\", 1),
    Glyph::new("⠧", "|", 1),
    Glyph::new("⠇", "/", 1),
    Glyph::new("⠏", "-", 1),
];

/// The Orchestrator's diamond, breathing like a beacon coordinating the run.
pub const SPINNER_ORCHESTRATOR: [Glyph; 4] = [
    Glyph::new("◇", "-", 1),
    Glyph::new("◈", "+", 1),
    Glyph::new("◆", "*", 1),
    Glyph::new("◈", "+", 1),
];

/// The Translator's triangle, turning clockwise as it works through the text.
pub const SPINNER_TRANSLATOR: [Glyph; 4] = [
    Glyph::new("◤", "`", 1),
    Glyph::new("◥", "'", 1),
    Glyph::new("◢", ".", 1),
    Glyph::new("◣", ",", 1),
];

/// The Reviewer's square, sweeping corner to corner as it scans the draft.
pub const SPINNER_REVIEWER: [Glyph; 4] = [
    Glyph::new("◰", "|", 1),
    Glyph::new("◳", "-", 1),
    Glyph::new("◲", "|", 1),
    Glyph::new("◱", "-", 1),
];

/// The Refine agent's quarter-arc, sweeping like a hand polishing the text.
pub const SPINNER_REFINE: [Glyph; 4] = [
    Glyph::new("◜", "`", 1),
    Glyph::new("◝", "'", 1),
    Glyph::new("◞", ".", 1),
    Glyph::new("◟", ",", 1),
];

/// Pick the frame for `frame` from any animation set.
pub fn frame_of<const N: usize>(set: &[Glyph; N], frame: u64) -> Glyph {
    set[(frame as usize) % N]
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    /// Every glyph declared in this module, so the pins below cannot miss one.
    fn every_glyph() -> Vec<(&'static str, Glyph)> {
        let mut v: Vec<(&'static str, Glyph)> = vec![
            ("MOON_NEW", MOON_NEW),
            ("MOON_CRESCENT", MOON_CRESCENT),
            ("MOON_FIRST_QUARTER", MOON_FIRST_QUARTER),
            ("MOON_LAST_QUARTER", MOON_LAST_QUARTER),
            ("MOON_GIBBOUS", MOON_GIBBOUS),
            ("MOON_FULL", MOON_FULL),
            ("MOON_HALF_DOWN", MOON_HALF_DOWN),
            ("IMAGE_PAGE", IMAGE_PAGE),
            ("EMPTY_PAGE", EMPTY_PAGE),
            ("FLAG", FLAG),
            ("CROSS", CROSS),
            ("PAUSE", PAUSE),
            ("CHECK", CHECK),
            ("SELECT_BAR", SELECT_BAR),
            ("ACCENT_RAIL", ACCENT_RAIL),
            ("ACCENT_RAIL_STRONG", ACCENT_RAIL_STRONG),
            ("RULE_H", RULE_H),
            ("RULE_V", RULE_V),
            ("SCROLL_TRACK", SCROLL_TRACK),
            ("SCROLL_THUMB", SCROLL_THUMB),
            ("GAUGE_FILLED", GAUGE_FILLED),
            ("GAUGE_TRACK", GAUGE_TRACK),
            ("REMOTE_LINK", REMOTE_LINK),
            ("UPGRADE", UPGRADE),
            ("CLOSE", CLOSE),
            ("CHEVRON_DOWN", CHEVRON_DOWN),
            ("CHEVRON_RIGHT", CHEVRON_RIGHT),
            ("CRUMB_SEP", CRUMB_SEP),
            ("DOT", DOT),
            ("CHECKBOX_ON", CHECKBOX_ON),
            ("CHECKBOX_OFF", CHECKBOX_OFF),
            ("TOGGLE_ON", TOGGLE_ON),
            ("TOGGLE_OFF", TOGGLE_OFF),
            ("ELLIPSIS", ELLIPSIS),
            ("BADGE_ORCHESTRATOR", BADGE_ORCHESTRATOR),
            ("BADGE_TRANSLATOR", BADGE_TRANSLATOR),
            ("BADGE_REVIEWER", BADGE_REVIEWER),
        ];
        for (name, set) in [
            ("SPINNER", &SPINNER[..]),
            ("SPINNER_ORCHESTRATOR", &SPINNER_ORCHESTRATOR[..]),
            ("SPINNER_TRANSLATOR", &SPINNER_TRANSLATOR[..]),
            ("SPINNER_REVIEWER", &SPINNER_REVIEWER[..]),
            ("SPINNER_REFINE", &SPINNER_REFINE[..]),
        ] {
            v.extend(set.iter().map(|g| (name, *g)));
        }
        v
    }

    #[test]
    fn declared_width_matches_measured_width_in_both_repertoires() {
        for (name, g) in every_glyph() {
            assert_eq!(
                g.unicode.width() as u16,
                g.cols(),
                "{name}: unicode form {:?} measures {} but declares {}",
                g.unicode,
                g.unicode.width(),
                g.cols()
            );
            assert_eq!(
                g.ascii.width() as u16,
                g.cols(),
                "{name}: ascii fallback {:?} must occupy the same {} column(s)",
                g.ascii,
                g.cols()
            );
        }
    }

    #[test]
    fn every_animation_frame_is_exactly_one_column() {
        // The invariant that keeps a spinner from shifting its trailing label.
        for (name, set) in [
            ("SPINNER", &SPINNER[..]),
            ("SPINNER_ORCHESTRATOR", &SPINNER_ORCHESTRATOR[..]),
            ("SPINNER_TRANSLATOR", &SPINNER_TRANSLATOR[..]),
            ("SPINNER_REVIEWER", &SPINNER_REVIEWER[..]),
            ("SPINNER_REFINE", &SPINNER_REFINE[..]),
        ] {
            for g in set {
                assert_eq!(g.cols(), 1, "{name}: {:?} is not one column", g.unicode);
            }
        }
    }

    #[test]
    fn ascii_fallbacks_are_pure_ascii() {
        // The point of the fallback set is that it cannot be East Asian
        // Ambiguous, so it must contain nothing outside ASCII.
        for (name, g) in every_glyph() {
            assert!(
                g.ascii.is_ascii(),
                "{name}: fallback {:?} is not ASCII",
                g.ascii
            );
        }
    }

    #[test]
    fn the_active_repertoire_selects_the_form() {
        let prior = glyph_set();
        set_glyph_set(GlyphSet::Unicode);
        assert_eq!(MOON_FULL.as_str(), "●");
        set_glyph_set(GlyphSet::Ascii);
        assert_eq!(MOON_FULL.as_str(), "#");
        set_glyph_set(prior);
    }

    #[test]
    fn frame_of_wraps_without_panicking() {
        for f in [0u64, 3, 9, 10, 1_000, u64::MAX] {
            let _ = frame_of(&SPINNER, f);
            let _ = frame_of(&SPINNER_ORCHESTRATOR, f);
        }
        assert_eq!(frame_of(&SPINNER, 0), SPINNER[0]);
        assert_eq!(frame_of(&SPINNER, 10), SPINNER[0], "wraps at the set length");
    }
}
