//! Frame-scoped registry of every interactive rectangle the UI drew.
//!
//! The contract that makes the whole kit work: a component **draws and
//! registers in the same call**, so clicks, hover and keyboard focus resolve
//! against the geometry that is actually on screen. Nothing re-derives a layout
//! a second time to hit-test it — the class of drift that let the Settings modal
//! draw at 76x24 while its click handler claimed 72x26.
//!
//! Registration order is meaningful twice: [`Zones::at`] scans in reverse so the
//! last (topmost) registration wins, and [`Zones::tab_order`] walks forward so
//! the focus ring follows reading order. An overlay therefore only needs to
//! register a full-frame backdrop before its own content for a click beside the
//! modal to resolve as "dismiss" rather than falling through to the screen.

use ratatui::layout::Rect;

use crate::app::Screen;

/// What an interactive rectangle *is*. Fieldless so [`ZoneId`] stays `Copy` and
/// cheap to compare; anything that needs to distinguish instances of the same
/// kind carries it in [`ZoneId::index`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ZoneKind {
    // --- chrome ---
    /// A primary tab; `index` is the [`Screen`] discriminant.
    Tab,
    /// A clickable breadcrumb segment; `index` is its position, outermost first.
    Crumb,
    /// A tally badge in the header; `index` is a `TallySlot`.
    Tally,
    /// The remote-link chip.
    RemoteChip,
    /// A hint in the shortcuts bar; `index` is its position.
    Hint,
    /// The toast's explicit close affordance.
    ToastClose,
    /// The toast body (dismiss, same as clicking close).
    ToastBody,

    // --- generic controls ---
    /// A button; `index` is a caller-assigned `ButtonId`.
    Button,
    /// A row in a list; `index` is the row's index in the full (unwindowed) data.
    Row,
    /// A column header in a table; `index` is the column.
    ColumnHeader,
    /// A form field; `index` is its position in the form.
    Field,
    /// A segment of a segmented control; `index` is the segment.
    Segment,
    /// A step in a wizard's stepper; `index` is the step.
    Step,
    /// A toggle / checkbox; `index` identifies which.
    Toggle,
    /// A scrollbar thumb; `index` distinguishes panes.
    ScrollThumb,
    /// A scrollable surface, for wheel routing; `index` distinguishes panes.
    ScrollSurface,
    /// Editable prose, for click-to-position and drag-select.
    TextSurface,

    // --- structural ---
    /// A modal's full-frame backdrop; clicking it steps back.
    Backdrop,
    /// A modal's own frame, so a click inside it is not a dismiss.
    ModalFrame,
    /// A focusable pane, for click-to-focus; `index` distinguishes panes.
    Pane,
}

/// The address of one interactive rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ZoneId {
    pub kind: ZoneKind,
    pub index: u32,
}

impl ZoneId {
    pub const fn new(kind: ZoneKind, index: u32) -> Self {
        Self { kind, index }
    }

    /// A zone whose kind alone identifies it.
    pub const fn bare(kind: ZoneKind) -> Self {
        Self::new(kind, 0)
    }

    pub const fn tab(screen: Screen) -> Self {
        Self::new(ZoneKind::Tab, screen as u32)
    }

    pub const fn row(index: usize) -> Self {
        Self::new(ZoneKind::Row, index as u32)
    }

    pub const fn field(index: usize) -> Self {
        Self::new(ZoneKind::Field, index as u32)
    }

    pub const fn button(index: u32) -> Self {
        Self::new(ZoneKind::Button, index)
    }

    pub const fn segment(index: usize) -> Self {
        Self::new(ZoneKind::Segment, index as u32)
    }

    pub const fn step(index: usize) -> Self {
        Self::new(ZoneKind::Step, index as u32)
    }

    pub const fn hint(index: usize) -> Self {
        Self::new(ZoneKind::Hint, index as u32)
    }

    pub const fn pane(index: u32) -> Self {
        Self::new(ZoneKind::Pane, index)
    }

    /// The row index this zone addresses, when it is a row.
    pub fn row_index(&self) -> Option<usize> {
        matches!(self.kind, ZoneKind::Row).then_some(self.index as usize)
    }
}

/// One registered rectangle.
#[derive(Debug, Clone, Copy)]
struct Zone {
    rect: Rect,
    id: ZoneId,
    /// Whether the keyboard focus ring stops here. Backdrops and scroll
    /// surfaces are clickable but not focusable.
    focusable: bool,
}

/// Every interactive rectangle drawn this frame.
///
/// Cleared at the top of each render pass and rebuilt as components draw.
#[derive(Debug, Default, Clone)]
pub struct Zones {
    zones: Vec<Zone>,
    /// Index into `zones` where a focus trap begins, set by a modal as it
    /// renders. While set, the focus ring covers only zones from here on, so
    /// Tab cycles inside the modal and can never land on the screen behind it.
    trap_from: Option<usize>,
}

impl Zones {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop everything from the previous frame. Called once per render.
    pub fn clear(&mut self) {
        self.zones.clear();
        self.trap_from = None;
    }

    pub fn is_empty(&self) -> bool {
        self.zones.is_empty()
    }

    /// Register a clickable, focusable rectangle. Zero-area rects are dropped so
    /// a collapsed pane never swallows a click at its origin.
    pub fn push(&mut self, rect: Rect, id: ZoneId) {
        self.insert(rect, id, true);
    }

    /// Register a clickable rectangle the focus ring skips.
    pub fn push_hit(&mut self, rect: Rect, id: ZoneId) {
        self.insert(rect, id, false);
    }

    fn insert(&mut self, rect: Rect, id: ZoneId, focusable: bool) {
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        self.zones.push(Zone {
            rect,
            id,
            focusable,
        });
    }

    /// The topmost zone containing `(col, row)`, or `None`.
    pub fn at(&self, col: u16, row: u16) -> Option<ZoneId> {
        self.zone_at(col, row).map(|z| z.id)
    }

    fn zone_at(&self, col: u16, row: u16) -> Option<&Zone> {
        self.zones
            .iter()
            .rev()
            .find(|z| crate::ui::mouse::hit(z.rect, col, row))
    }

    /// The rectangle registered for `id`, if any. The last registration wins, to
    /// match [`Zones::at`].
    pub fn rect_of(&self, id: ZoneId) -> Option<Rect> {
        self.zones.iter().rev().find(|z| z.id == id).map(|z| z.rect)
    }

    pub fn contains(&self, id: ZoneId) -> bool {
        self.zones.iter().any(|z| z.id == id)
    }

    /// Begin a focus trap at the current position. Everything registered from
    /// here on forms the focus ring; everything before it is still clickable but
    /// unreachable by Tab. A modal calls this before drawing its own content.
    pub fn begin_trap(&mut self) {
        self.trap_from = Some(self.zones.len());
    }

    pub fn is_trapped(&self) -> bool {
        self.trap_from.is_some()
    }

    /// The focus ring, in registration (reading) order, honouring an active trap.
    pub fn tab_order(&self) -> impl Iterator<Item = ZoneId> + '_ {
        self.zones[self.trap_from.unwrap_or(0)..]
            .iter()
            .filter(|z| z.focusable)
            .map(|z| z.id)
    }

    /// Every registered id, focusable or not, in registration order.
    pub fn all(&self) -> impl Iterator<Item = (Rect, ZoneId)> + '_ {
        self.zones.iter().map(|z| (z.rect, z.id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn topmost_registration_wins() {
        let mut z = Zones::new();
        z.push(r(0, 0, 20, 10), ZoneId::bare(ZoneKind::Pane));
        z.push(r(5, 5, 4, 1), ZoneId::row(3));
        // Inside the later, smaller rect.
        assert_eq!(z.at(6, 5), Some(ZoneId::row(3)));
        // Inside only the earlier one.
        assert_eq!(z.at(1, 1), Some(ZoneId::bare(ZoneKind::Pane)));
        // Outside both.
        assert_eq!(z.at(40, 40), None);
    }

    #[test]
    fn a_backdrop_catches_what_the_modal_does_not() {
        // The shape every overlay registers: screen content, then a full-frame
        // backdrop, then the modal itself on top.
        let mut z = Zones::new();
        z.push(r(0, 0, 80, 24), ZoneId::row(0));
        z.push_hit(r(0, 0, 80, 24), ZoneId::bare(ZoneKind::Backdrop));
        z.push_hit(r(20, 6, 40, 12), ZoneId::bare(ZoneKind::ModalFrame));
        z.push(r(22, 8, 10, 1), ZoneId::button(1));

        assert_eq!(z.at(23, 8), Some(ZoneId::button(1)), "the control wins");
        assert_eq!(
            z.at(50, 10),
            Some(ZoneId::bare(ZoneKind::ModalFrame)),
            "inside the modal but on no control: inert, not a dismiss"
        );
        assert_eq!(
            z.at(2, 2),
            Some(ZoneId::bare(ZoneKind::Backdrop)),
            "beside the modal: the backdrop, never the screen row beneath it"
        );
    }

    #[test]
    fn tab_order_skips_unfocusable_and_keeps_reading_order() {
        let mut z = Zones::new();
        z.push_hit(r(0, 0, 80, 24), ZoneId::bare(ZoneKind::Backdrop));
        z.push(r(0, 1, 10, 1), ZoneId::field(0));
        z.push(r(0, 2, 10, 1), ZoneId::field(1));
        z.push_hit(r(0, 3, 10, 5), ZoneId::bare(ZoneKind::ScrollSurface));
        z.push(r(0, 9, 6, 1), ZoneId::button(0));

        let ring: Vec<_> = z.tab_order().collect();
        assert_eq!(ring, vec![ZoneId::field(0), ZoneId::field(1), ZoneId::button(0)]);
    }

    #[test]
    fn a_trap_confines_the_focus_ring_to_the_modal() {
        let mut z = Zones::new();
        z.push(r(0, 1, 10, 1), ZoneId::row(0));
        z.push(r(0, 2, 10, 1), ZoneId::row(1));
        // The modal opens: everything from here on is the ring.
        z.begin_trap();
        z.push_hit(r(0, 0, 80, 24), ZoneId::bare(ZoneKind::Backdrop));
        z.push(r(22, 8, 10, 1), ZoneId::field(0));
        z.push(r(22, 9, 10, 1), ZoneId::button(0));

        let ring: Vec<_> = z.tab_order().collect();
        assert_eq!(
            ring,
            vec![ZoneId::field(0), ZoneId::button(0)],
            "the screen rows behind the modal must be unreachable by Tab"
        );
        // The rows stay registered — the trap confines the ring, not the
        // registry — but the backdrop above them is what a click resolves to.
        assert!(z.contains(ZoneId::row(0)));
        assert_eq!(z.at(1, 1), Some(ZoneId::bare(ZoneKind::Backdrop)));
    }

    #[test]
    fn clearing_releases_the_trap() {
        let mut z = Zones::new();
        z.begin_trap();
        assert!(z.is_trapped());
        z.clear();
        assert!(!z.is_trapped(), "a stale trap would strand focus next frame");
        z.push(r(0, 0, 4, 1), ZoneId::row(0));
        assert_eq!(z.tab_order().count(), 1);
    }

    #[test]
    fn zero_area_zones_are_dropped() {
        let mut z = Zones::new();
        z.push(r(4, 4, 0, 1), ZoneId::row(0));
        z.push(r(4, 4, 3, 0), ZoneId::row(1));
        assert!(z.is_empty());
        assert_eq!(z.at(4, 4), None);
    }

    #[test]
    fn rect_of_round_trips_every_registered_zone() {
        let mut z = Zones::new();
        let rects = [r(0, 0, 5, 1), r(0, 1, 7, 1), r(0, 2, 9, 1)];
        for (i, rect) in rects.iter().enumerate() {
            z.push(*rect, ZoneId::row(i));
        }
        for (i, rect) in rects.iter().enumerate() {
            assert_eq!(z.rect_of(ZoneId::row(i)), Some(*rect));
            // The center of a registered rect must resolve back to it.
            let (cx, cy) = (rect.x + rect.width / 2, rect.y + rect.height / 2);
            assert_eq!(z.at(cx, cy), Some(ZoneId::row(i)));
        }
    }
}
