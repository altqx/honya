//! Spacing, density and width breakpoints.
//!
//! Replaces the ad-hoc integers and one-off width tests (`width >= 56`,
//! `width.saturating_sub(34)`, `saturating_sub(26)`) scattered through the
//! screens with one scale every component measures against, so panels line up
//! across screens instead of each inventing its own padding.

use ratatui::layout::Rect;

/// One column of breathing room. The smallest unit anything indents by.
pub const GUTTER: u16 = 1;
/// Inside a panel, between its edge and its content.
pub const PAD: u16 = 2;
/// A blank row between sections of a form or card.
pub const SECTION_GAP: u16 = 1;
/// Column reserved on the right of a scrollable pane for its scrollbar.
pub const SCROLLBAR_COLS: u16 = 1;

/// Terminal width bands. Screens branch on these instead of testing raw columns,
/// so "what collapses when" is decided once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Breakpoint {
    /// Under 80 columns: single pane, side panels collapse away.
    Narrow,
    /// 80–119: two panes.
    Medium,
    /// 120+: the full three-zone layout.
    Wide,
}

impl Breakpoint {
    pub const MEDIUM_MIN: u16 = 80;
    pub const WIDE_MIN: u16 = 120;

    pub fn of(width: u16) -> Self {
        match width {
            w if w >= Self::WIDE_MIN => Breakpoint::Wide,
            w if w >= Self::MEDIUM_MIN => Breakpoint::Medium,
            _ => Breakpoint::Narrow,
        }
    }

    /// How many side-by-side panes this width can carry.
    pub fn panes(self) -> u8 {
        match self {
            Breakpoint::Narrow => 1,
            Breakpoint::Medium => 2,
            Breakpoint::Wide => 3,
        }
    }

    pub fn is_narrow(self) -> bool {
        matches!(self, Breakpoint::Narrow)
    }
}

/// Vertical breathing room. Compact drops every optional blank row so a short
/// terminal spends its height on content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Density {
    Comfortable,
    Compact,
}

impl Density {
    /// Below this many rows the layout compacts itself without being asked.
    pub const AUTO_COMPACT_MAX_ROWS: u16 = 28;
    /// The band just above the threshold, where advertising compact mode is
    /// worth a one-shot tip: below it compacting already happened, above it the
    /// tip is noise.
    pub const TIP_BAND_MIN_ROWS: u16 = 29;
    pub const TIP_BAND_MAX_ROWS: u16 = 34;

    /// Resolve the effective density: an explicit opt-in wins, otherwise a short
    /// terminal compacts itself.
    pub fn resolve(forced: bool, height: u16) -> Self {
        if forced || height <= Self::AUTO_COMPACT_MAX_ROWS {
            Density::Compact
        } else {
            Density::Comfortable
        }
    }

    /// True when `height` sits in the band where the compact-mode tip earns its
    /// place — auto-compact has not kicked in yet, but the screen is small
    /// enough that the user would benefit.
    pub fn tip_band_contains(height: u16) -> bool {
        (Self::TIP_BAND_MIN_ROWS..=Self::TIP_BAND_MAX_ROWS).contains(&height)
    }

    pub fn is_compact(self) -> bool {
        matches!(self, Density::Compact)
    }

    /// Vertical padding at the outer edge of a screen body.
    pub fn outer_vpad(self) -> u16 {
        match self {
            Density::Comfortable => 1,
            Density::Compact => 0,
        }
    }

    /// Horizontal padding at the outer edge of a screen body.
    pub fn outer_hpad(self) -> u16 {
        match self {
            Density::Comfortable => PAD,
            Density::Compact => GUTTER,
        }
    }

    /// A blank row between sections, or none when compact.
    pub fn section_gap(self) -> u16 {
        match self {
            Density::Comfortable => SECTION_GAP,
            Density::Compact => 0,
        }
    }

    /// Rows a card spends on padding, top and bottom combined.
    pub fn card_vpad(self) -> u16 {
        match self {
            Density::Comfortable => 1,
            Density::Compact => 0,
        }
    }
}

/// Layout metrics for one frame: the width band, the density, and the padding
/// that follows from them. Threaded to components so none of them re-derives it.
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    pub breakpoint: Breakpoint,
    pub density: Density,
}

impl Metrics {
    pub fn new(area: Rect, forced_compact: bool) -> Self {
        Self {
            breakpoint: Breakpoint::of(area.width),
            density: Density::resolve(forced_compact, area.height),
        }
    }

    pub fn is_compact(self) -> bool {
        self.density.is_compact()
    }

    pub fn is_narrow(self) -> bool {
        self.breakpoint.is_narrow()
    }

    pub fn panes(self) -> u8 {
        self.breakpoint.panes()
    }

    /// Inset `area` by the outer padding for this density.
    pub fn inset_outer(self, area: Rect) -> Rect {
        inset(area, self.density.outer_hpad(), self.density.outer_vpad())
    }
}

/// Shrink `area` by `h` columns on each side and `v` rows top and bottom,
/// saturating to an empty rect rather than underflowing.
pub fn inset(area: Rect, h: u16, v: u16) -> Rect {
    let dx = h.min(area.width / 2);
    let dy = v.min(area.height / 2);
    Rect {
        x: area.x.saturating_add(dx),
        y: area.y.saturating_add(dy),
        width: area.width.saturating_sub(dx * 2),
        height: area.height.saturating_sub(dy * 2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breakpoints_partition_the_width_range() {
        assert_eq!(Breakpoint::of(0), Breakpoint::Narrow);
        assert_eq!(Breakpoint::of(79), Breakpoint::Narrow);
        assert_eq!(Breakpoint::of(80), Breakpoint::Medium);
        assert_eq!(Breakpoint::of(119), Breakpoint::Medium);
        assert_eq!(Breakpoint::of(120), Breakpoint::Wide);
        assert_eq!(Breakpoint::of(400), Breakpoint::Wide);
    }

    #[test]
    fn short_terminals_compact_themselves_and_opt_in_still_wins() {
        assert!(Density::resolve(false, 24).is_compact(), "short: auto");
        assert!(!Density::resolve(false, 50).is_compact(), "tall: comfortable");
        assert!(Density::resolve(true, 50).is_compact(), "opt-in overrides");
    }

    #[test]
    fn the_tip_band_sits_above_auto_compact_not_inside_it() {
        // Below the threshold compacting already happened, so the tip is moot.
        assert!(!Density::tip_band_contains(Density::AUTO_COMPACT_MAX_ROWS));
        assert!(Density::tip_band_contains(Density::TIP_BAND_MIN_ROWS));
        assert!(Density::tip_band_contains(Density::TIP_BAND_MAX_ROWS));
        // Well above it, the hint would just be noise.
        assert!(!Density::tip_band_contains(Density::TIP_BAND_MAX_ROWS + 1));
    }

    #[test]
    fn inset_saturates_instead_of_underflowing() {
        let tiny = Rect {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        };
        let got = inset(tiny, 10, 10);
        assert_eq!(got.width, 0);
        assert_eq!(got.height, 1, "an odd single row cannot be inset away");
        // And the origin never walks outside the source rect.
        assert!(got.x >= tiny.x && got.y >= tiny.y);
    }

    #[test]
    fn inset_outer_is_symmetric() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 40,
        };
        let m = Metrics::new(area, false);
        let got = m.inset_outer(area);
        assert_eq!(got.x - area.x, area.x + area.width - (got.x + got.width));
    }
}
