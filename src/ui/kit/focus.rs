//! The keyboard focus ring, and the pointer's hover state.
//!
//! Both resolve against the same [`Zones`] registry the mouse hit-tests, so what
//! Tab reaches and what a click reaches cannot disagree. They are separate
//! structures on purpose: the pointer crossing a row must not steal the
//! keyboard's place in a form.
//!
//! Zones are rebuilt every frame, so a focused id can vanish between them — a
//! list shrinks, a modal closes. [`Focus::reconcile`] settles that after each
//! render rather than leaving focus pointing at something gone.

use super::zones::{ZoneId, Zones};

/// Where the keyboard is.
#[derive(Debug, Default, Clone)]
pub struct Focus {
    current: Option<ZoneId>,
}

impl Focus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self) -> Option<ZoneId> {
        self.current
    }

    /// Nothing in the app moves focus by id — the ring and the pointer do it —
    /// but a test needs a way to put it somewhere before asserting.
    #[cfg(test)]
    pub fn set(&mut self, id: ZoneId) {
        self.current = Some(id);
    }

    pub fn clear(&mut self) {
        self.current = None;
    }

    /// Move to the next focusable zone, wrapping. With nothing focused this
    /// lands on the first. Within a trap the ring is the trap's, so this wraps
    /// inside the modal and never escapes it.
    pub fn next(&mut self, zones: &Zones) {
        self.step(zones, true);
    }

    /// Move to the previous focusable zone, wrapping.
    pub fn prev(&mut self, zones: &Zones) {
        self.step(zones, false);
    }

    fn step(&mut self, zones: &Zones, forward: bool) {
        let ring: Vec<ZoneId> = zones.tab_order().collect();
        if ring.is_empty() {
            self.current = None;
            return;
        }
        let next = match self.current.and_then(|c| ring.iter().position(|&z| z == c)) {
            Some(i) if forward => (i + 1) % ring.len(),
            Some(i) => (i + ring.len() - 1) % ring.len(),
            // Nothing focused (or focus is off-ring): enter at the near end.
            None if forward => 0,
            None => ring.len() - 1,
        };
        self.current = Some(ring[next]);
    }

    /// Settle focus against the zones just rendered: a focused zone that no
    /// longer exists is dropped rather than left dangling.
    pub fn reconcile(&mut self, zones: &Zones) {
        if let Some(cur) = self.current
            && !zones.contains(cur)
        {
            self.current = None;
        }
    }
}

/// Where the pointer is. Kept apart from [`Focus`] so hovering never moves the
/// keyboard's place.
#[derive(Debug, Default, Clone, Copy)]
pub struct Hover {
    current: Option<ZoneId>,
}

impl Hover {
    pub fn get(self) -> Option<ZoneId> {
        self.current
    }

    /// Record the zone under `(col, row)`. Returns true when the hovered zone
    /// changed — the only case worth a repaint. Motion reporting fires far
    /// faster than the frame budget, so every caller must gate on this.
    pub fn moved_to(&mut self, zones: &Zones, col: u16, row: u16) -> bool {
        let next = zones.at(col, row);
        let changed = next != self.current;
        self.current = next;
        changed
    }

    /// Drop hover, e.g. when mouse reporting is switched off. Returns true when
    /// something actually cleared.
    pub fn clear(&mut self) -> bool {
        self.current.take().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::super::zones::{ZoneKind, Zones};
    use super::*;
    use ratatui::layout::Rect;

    fn row_zones(n: usize) -> Zones {
        let mut z = Zones::new();
        for i in 0..n {
            z.push(
                Rect {
                    x: 0,
                    y: i as u16,
                    width: 10,
                    height: 1,
                },
                ZoneId::row(i),
            );
        }
        z
    }

    #[test]
    fn tab_enters_at_the_top_and_wraps_at_the_bottom() {
        let z = row_zones(3);
        let mut f = Focus::new();
        f.next(&z);
        assert_eq!(f.get(), Some(ZoneId::row(0)));
        f.next(&z);
        f.next(&z);
        assert_eq!(f.get(), Some(ZoneId::row(2)));
        f.next(&z);
        assert_eq!(f.get(), Some(ZoneId::row(0)), "wraps forward");
    }

    #[test]
    fn shift_tab_enters_at_the_bottom_and_wraps() {
        let z = row_zones(3);
        let mut f = Focus::new();
        f.prev(&z);
        assert_eq!(f.get(), Some(ZoneId::row(2)));
        f.prev(&z);
        assert_eq!(f.get(), Some(ZoneId::row(1)));
        f.prev(&z);
        f.prev(&z);
        assert_eq!(f.get(), Some(ZoneId::row(2)), "wraps backward");
    }

    #[test]
    fn a_trap_keeps_tab_inside_the_modal() {
        let mut z = row_zones(2);
        z.begin_trap();
        z.push(
            Rect {
                x: 5,
                y: 5,
                width: 8,
                height: 1,
            },
            ZoneId::button(0),
        );
        z.push(
            Rect {
                x: 5,
                y: 6,
                width: 8,
                height: 1,
            },
            ZoneId::button(1),
        );

        let mut f = Focus::new();
        for _ in 0..6 {
            f.next(&z);
            let got = f.get().unwrap();
            assert!(
                matches!(got.kind, ZoneKind::Button),
                "focus escaped the trap onto {got:?}"
            );
        }
    }

    #[test]
    fn focus_on_a_vanished_zone_is_dropped_not_left_dangling() {
        let mut f = Focus::new();
        f.set(ZoneId::row(9));
        f.reconcile(&row_zones(3));
        assert_eq!(f.get(), None);
        // And the ring is still enterable afterwards.
        f.next(&row_zones(3));
        assert_eq!(f.get(), Some(ZoneId::row(0)));
    }

    #[test]
    fn hover_reports_only_real_changes() {
        let z = row_zones(3);
        let mut h = Hover::default();
        assert!(h.moved_to(&z, 2, 0), "entering a zone is a change");
        assert!(!h.moved_to(&z, 4, 0), "moving within the same zone is not");
        assert!(h.moved_to(&z, 2, 1), "crossing to another zone is");
        assert!(h.moved_to(&z, 50, 50), "leaving every zone is");
        assert!(!h.moved_to(&z, 60, 60), "staying outside is not");
    }
}
