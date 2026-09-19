//! The keyboard focus ring, and the pointer's hover state.
//!
//! Both resolve against the same [`Zones`] registry the mouse hit-tests, so what
//! Tab reaches and what a click reaches can never disagree. Focus and hover are
//! deliberately separate: the pointer moving across a row must not steal the
//! keyboard's place in a form.
//!
//! Zones are rebuilt every frame, so a focused id can vanish between frames — a
//! list shrinks, a modal closes. [`Focus::reconcile`] runs after each render and
//! settles that, rather than leaving focus pointing at something gone.

use super::zones::{ZoneId, Zones};

/// Where the keyboard is.
#[derive(Debug, Default, Clone)]
pub struct Focus {
    current: Option<ZoneId>,
    /// Set while focus is parked outside a trap that is still on screen, so the
    /// shortcuts bar can pin a hint naming the way back.
    parked: Option<ZoneId>,
}

impl Focus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self) -> Option<ZoneId> {
        self.current
    }

    /// Whether `id` currently holds the keyboard. Components call this to decide
    /// whether to draw a focus ring.
    pub fn is(&self, id: ZoneId) -> bool {
        self.current == Some(id)
    }

    pub fn set(&mut self, id: ZoneId) {
        self.current = Some(id);
        self.parked = None;
    }

    pub fn clear(&mut self) {
        self.current = None;
        self.parked = None;
    }

    /// The zone focus will return to when it is un-parked.
    pub fn parked(&self) -> Option<ZoneId> {
        self.parked
    }

    /// Step focus out of a trap without dismissing it, remembering the way back.
    /// Esc does this before it closes anything, so a user can scroll the content
    /// behind a modal while the modal stays on screen.
    pub fn park(&mut self) {
        if let Some(cur) = self.current.take() {
            self.parked = Some(cur);
        }
    }

    pub fn unpark(&mut self) {
        if let Some(back) = self.parked.take() {
            self.current = Some(back);
        }
    }

    pub fn is_parked(&self) -> bool {
        self.parked.is_some()
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

    /// Settle focus against the zones just rendered. A focused zone that no
    /// longer exists is dropped rather than left dangling; a parked zone that is
    /// gone releases the park, so Esc does not have a phantom rung to climb.
    pub fn reconcile(&mut self, zones: &Zones) {
        if let Some(cur) = self.current
            && !zones.contains(cur)
        {
            self.current = None;
        }
        if let Some(p) = self.parked
            && !zones.contains(p)
        {
            self.parked = None;
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

    pub fn is(self, id: ZoneId) -> bool {
        self.current == Some(id)
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
    fn parking_remembers_the_way_back_and_unparking_restores_it() {
        let z = row_zones(3);
        let mut f = Focus::new();
        f.set(ZoneId::row(1));
        f.park();
        assert!(f.is_parked());
        assert_eq!(f.get(), None, "parked focus is off the ring");
        assert_eq!(f.parked(), Some(ZoneId::row(1)));
        f.unpark();
        assert_eq!(f.get(), Some(ZoneId::row(1)));
        assert!(!f.is_parked());
        // Reconcile against live zones leaves a valid restore alone.
        f.reconcile(&z);
        assert_eq!(f.get(), Some(ZoneId::row(1)));
    }

    #[test]
    fn a_park_whose_target_vanished_releases() {
        let mut f = Focus::new();
        f.set(ZoneId::row(7));
        f.park();
        f.reconcile(&row_zones(2));
        assert!(!f.is_parked(), "Esc must not have a phantom rung to climb");
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
