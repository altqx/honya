//! Which region has the keyboard, and where focus goes when a surface closes.
//!
//! The window had no notion of focus at all: `Escape`, the digits and the
//! arrows meant whatever the last-added branch of one `match` decided, and
//! every new pane made that worse. Naming the regions is what lets a rule be
//! stated once — the palette keeps the keyboard while it is open and hands it
//! back to wherever it came from; a digit addresses whatever surface is open
//! rather than switching tabs underneath it; the drawer takes focus when you
//! open it and not when it merely fills with output.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Region {
    Tree,
    Tabs,
    #[default]
    Main,
    Inspector,
    Drawer,
    /// A text field that owns everything typable while it has the caret.
    Composer,
}

impl Region {
    /// Whether this region swallows plain characters. A screen command bound
    /// to a bare letter must not fire into a field someone is typing in.
    pub fn takes_text(self) -> bool {
        matches!(self, Region::Composer)
    }
}

/// Modal surfaces that take the keyboard whole and give it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Palette,
    Overlay,
}

#[derive(Debug, Default)]
pub struct Focus {
    region: Region,
    /// What had the keyboard before a surface took it.
    parked: Option<Region>,
    surface: Option<Surface>,
}

impl Focus {
    pub fn region(&self) -> Region {
        self.region
    }

    #[cfg(test)]
    pub fn surface(&self) -> Option<Surface> {
        self.surface
    }

    /// Move the keyboard because the user asked — a click, or a chord that
    /// names a region. Ignored while a surface holds it, so a stray click
    /// behind a modal cannot steal the caret out of it.
    pub fn set(&mut self, region: Region) {
        if self.surface.is_none() {
            self.region = region;
        }
    }

    /// A surface opened. The region under it is remembered, once: a second
    /// surface opening over the first must not overwrite what to return to.
    pub fn enter(&mut self, surface: Surface) {
        if self.surface.is_none() {
            self.parked = Some(self.region);
        }
        self.surface = Some(surface);
    }

    /// A surface closed. Focus returns to where it came from rather than to
    /// whatever happens to be under the pointer.
    pub fn leave(&mut self) {
        self.surface = None;
        if let Some(back) = self.parked.take() {
            self.region = back;
        }
    }

    /// True when a plain digit means "the row in the open surface" rather than
    /// "switch to that tab".
    pub fn digits_belong_to_surface(&self) -> bool {
        self.surface.is_some()
    }

    /// True when a bare-letter command may fire: nothing modal is up and the
    /// keyboard is not in a text field.
    pub fn accepts_command_keys(&self, text_focused: bool) -> bool {
        self.surface.is_none() && !text_focused && !self.region.takes_text()
    }

    /// Keep this in step with what is actually open. A surface dismissed by
    /// something other than us — Escape handled elsewhere, an action that
    /// closes an overlay — still has to hand the keyboard back.
    pub fn sync(&mut self, open: Option<Surface>) {
        match (self.surface, open) {
            (Some(_), None) => self.leave(),
            (None, Some(s)) => self.enter(s),
            (Some(a), Some(b)) if a != b => self.surface = Some(b),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_surface_hands_the_keyboard_back_to_where_it_came_from() {
        let mut f = Focus::default();
        f.set(Region::Tree);
        assert_eq!(f.region(), Region::Tree);

        f.enter(Surface::Palette);
        assert_eq!(f.surface(), Some(Surface::Palette));

        f.leave();
        assert_eq!(
            f.region(),
            Region::Tree,
            "closing returns the keyboard, it does not drop it"
        );
    }

    #[test]
    fn a_second_surface_does_not_overwrite_what_to_return_to() {
        let mut f = Focus::default();
        f.set(Region::Composer);
        f.enter(Surface::Palette);
        f.enter(Surface::Overlay);
        f.leave();
        assert_eq!(f.region(), Region::Composer);
    }

    #[test]
    fn a_click_behind_a_modal_cannot_steal_the_caret_out_of_it() {
        let mut f = Focus::default();
        f.set(Region::Composer);
        f.enter(Surface::Palette);
        f.set(Region::Tree);
        f.leave();
        assert_eq!(f.region(), Region::Composer);
    }

    #[test]
    fn digits_address_the_open_surface_rather_than_the_tabs_under_it() {
        let mut f = Focus::default();
        assert!(!f.digits_belong_to_surface());
        f.enter(Surface::Palette);
        assert!(f.digits_belong_to_surface());
        f.leave();
        assert!(!f.digits_belong_to_surface());
    }

    #[test]
    fn a_bare_letter_command_waits_for_the_field_to_let_go() {
        let mut f = Focus::default();
        assert!(f.accepts_command_keys(false));
        assert!(!f.accepts_command_keys(true), "a field is being typed in");

        f.set(Region::Composer);
        assert!(
            !f.accepts_command_keys(false),
            "the composer takes text even before egui reports the caret"
        );

        f.set(Region::Main);
        f.enter(Surface::Overlay);
        assert!(!f.accepts_command_keys(false), "a modal has the keyboard");
    }

    #[test]
    fn syncing_notices_a_surface_that_closed_without_telling_us() {
        let mut f = Focus::default();
        f.set(Region::Drawer);
        f.sync(Some(Surface::Overlay));
        assert_eq!(f.surface(), Some(Surface::Overlay));

        // An action closed the overlay; nobody called `leave`.
        f.sync(None);
        assert_eq!(f.surface(), None);
        assert_eq!(f.region(), Region::Drawer);
    }

    #[test]
    fn swapping_one_surface_for_another_keeps_the_original_return() {
        let mut f = Focus::default();
        f.set(Region::Inspector);
        f.sync(Some(Surface::Palette));
        f.sync(Some(Surface::Overlay));
        f.sync(None);
        assert_eq!(f.region(), Region::Inspector);
    }
}
