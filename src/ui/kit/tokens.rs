//! Spacing, density and width breakpoints.
//!
//! Replaces the ad-hoc integers and one-off width tests (`width >= 56`,
//! `width.saturating_sub(34)`, `saturating_sub(26)`) scattered through the
//! screens with one scale every component measures against, so panels line up
//! across screens instead of each inventing its own padding.

use ratatui::layout::Rect;

/// One column of breathing room. The smallest unit anything indents by.
pub const GUTTER: u16 = 1;
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
    /// Resolve the effective density: an explicit opt-in wins, otherwise a short
    /// terminal compacts itself.
    pub fn resolve(forced: bool, height: u16) -> Self {
        if forced || height <= Self::AUTO_COMPACT_MAX_ROWS {
            Density::Compact
        } else {
            Density::Comfortable
        }
    }

    pub fn is_compact(self) -> bool {
        matches!(self, Density::Compact)
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

}
