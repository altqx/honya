//! One declaration per screen *command*, read by every consumer.
//!
//! A screen used to write each of its keys down three times — the `handle_key`
//! match, the `hints()` array and `bindings.rs` — with nothing checking the
//! three against each other. They drifted: Project advertised a `Q` that had no
//! handler at all, and `bindings.rs` described three Reader keys that do
//! something else. Adding toolbars and menus would have made it five copies.
//!
//! Here a screen declares an action once — label, accelerator, availability —
//! and everything else reads that declaration: `handle_key` dispatches from it,
//! the toolbar and the context menu draw from it, and the help overlay lists
//! it. The key printed on a control *is* the key that runs it.
//!
//! Navigation is deliberately absent. Scrolling, folding, panel focus and
//! paging stay ordinary `handle_key` arms: the wheel and a click already do
//! them, so they need no button, no menu entry and no footer hint.

// Scaffolding note: the table lands before its consumers, and the screens are
// ported onto it one at a time. This allow comes off with the last one.
#![allow(dead_code)]

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::ui::kit::{ZoneId, ZoneKind};

/// The zone index reserved for a toolbar's overflow control. Far enough out
/// that no screen's own ids reach it.
pub const OVERFLOW_ID: u16 = u16::MAX;

/// The key that runs an action. Printed on its control *and* matched by
/// `handle_key`, so the two cannot disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Accel {
    pub code: KeyCode,
    pub ctrl: bool,
    /// A second key that also runs it, kept off the label. For a binding that
    /// has always had a synonym (`.` beside `>`), not for a second name.
    pub alt: Option<KeyCode>,
}

impl Accel {
    pub const fn key(c: char) -> Self {
        Self {
            code: KeyCode::Char(c),
            ctrl: false,
            alt: None,
        }
    }

    pub const fn code(code: KeyCode) -> Self {
        Self {
            code,
            ctrl: false,
            alt: None,
        }
    }

    pub const fn ctrl(c: char) -> Self {
        Self {
            code: KeyCode::Char(c),
            ctrl: true,
            alt: None,
        }
    }

    /// Accept `code` as well, without printing it.
    pub const fn or(mut self, code: KeyCode) -> Self {
        self.alt = Some(code);
        self
    }

    /// Whether `k` runs this action.
    ///
    /// Only Ctrl is compared. Shift is already folded into the `KeyCode` a
    /// terminal reports (`Q` arrives as `Char('Q')` *and* `SHIFT`), so testing
    /// it separately would reject every capital binding.
    pub fn matches(&self, k: &KeyEvent) -> bool {
        if k.modifiers.contains(KeyModifiers::CONTROL) != self.ctrl {
            return false;
        }
        k.code == self.code || self.alt.is_some_and(|c| c == k.code)
    }

    /// How the accelerator is printed on a control and in help.
    pub fn shown(&self) -> String {
        let base = match self.code {
            KeyCode::Char(' ') => "Space".to_string(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Enter => "↵".to_string(),
            KeyCode::Esc => "Esc".to_string(),
            KeyCode::Tab => "Tab".to_string(),
            KeyCode::BackTab => "⇧Tab".to_string(),
            KeyCode::Backspace => "⌫".to_string(),
            KeyCode::Delete => "Del".to_string(),
            KeyCode::Up => "↑".to_string(),
            KeyCode::Down => "↓".to_string(),
            KeyCode::Left => "←".to_string(),
            KeyCode::Right => "→".to_string(),
            KeyCode::PageUp => "PgUp".to_string(),
            KeyCode::PageDown => "PgDn".to_string(),
            KeyCode::Home => "Home".to_string(),
            KeyCode::End => "End".to_string(),
            other => format!("{other:?}"),
        };
        if self.ctrl {
            format!("^{base}")
        } else {
            base
        }
    }
}

/// Where an action offers itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// A button or chip in the screen's toolbar row.
    Toolbar,
    /// On the hovered or selected row, plus that row's context menu.
    Row,
    /// Only in the context menu and in help — the long tail.
    Menu,
}

impl Placement {
    /// Whether an action in this placement draws a control of its own. What
    /// does not is what the footer is still allowed to advertise.
    pub fn is_drawn(self) -> bool {
        matches!(self, Placement::Toolbar | Placement::Row)
    }
}

/// What running the action does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Runs and is done. Drawn as a button.
    Verb,
    /// Carries state. Drawn as a chip showing it.
    Toggle(bool),
    /// Cycles through values rather than flipping. Drawn as a chip showing the
    /// current one, with no on/off mark.
    Cycle,
}

/// One declared command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Act {
    /// Stable within a screen; also the zone index it registers under.
    pub id: u16,
    pub label: &'static str,
    pub accel: Accel,
    pub placement: Placement,
    pub kind: Kind,
    /// Resolved per frame. A disabled action still draws — greyed — so the
    /// interface does not change shape as state moves under the pointer.
    pub enabled: bool,
    /// Shown instead of `label` when the toolbar has collapsed to icons.
    /// Falls back to the accelerator when unset.
    pub icon: Option<&'static str>,
    /// Replaces `label` on the control for this frame, for a chip whose text is
    /// its value (`split` / `JA` / `TR`) or that carries a count.
    pub value: Option<&'static str>,
}

impl Act {
    pub const fn new(id: u16, label: &'static str, accel: Accel, placement: Placement) -> Self {
        Self {
            id,
            label,
            accel,
            placement,
            kind: Kind::Verb,
            enabled: true,
            icon: None,
            value: None,
        }
    }

    pub const fn toolbar(id: u16, label: &'static str, accel: Accel) -> Self {
        Self::new(id, label, accel, Placement::Toolbar)
    }

    pub const fn row(id: u16, label: &'static str, accel: Accel) -> Self {
        Self::new(id, label, accel, Placement::Row)
    }

    pub const fn menu(id: u16, label: &'static str, accel: Accel) -> Self {
        Self::new(id, label, accel, Placement::Menu)
    }

    pub const fn toggle(mut self, on: bool) -> Self {
        self.kind = Kind::Toggle(on);
        self
    }

    pub const fn cycle(mut self) -> Self {
        self.kind = Kind::Cycle;
        self
    }

    pub const fn icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
        self
    }

    pub const fn value(mut self, value: &'static str) -> Self {
        self.value = Some(value);
        self
    }

    /// Resolve availability. Chained on the declaration so a screen reads as a
    /// table rather than a sequence of pushes.
    pub const fn when(mut self, yes: bool) -> Self {
        self.enabled = yes;
        self
    }

    pub const fn zone(&self) -> ZoneId {
        ZoneId::action(self.id)
    }

    /// The text drawn on the control: the live value when one is set.
    pub fn control_label(&self) -> &str {
        self.value.unwrap_or(self.label)
    }

    /// The narrow form: an explicit icon, else the accelerator.
    pub fn icon_label(&self) -> String {
        match self.icon {
            Some(i) => i.to_string(),
            None => self.accel.shown(),
        }
    }
}

/// What a key press meant against a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyHit {
    /// Run this action.
    Run(u16),
    /// Declared here but not available right now. The key is still claimed, so
    /// it does nothing rather than falling through to a navigation arm.
    Blocked,
    /// Not in this table.
    Miss,
}

/// Resolve `k` against `acts`.
pub fn hit(acts: &[Act], k: &KeyEvent) -> KeyHit {
    match acts.iter().find(|a| a.accel.matches(k)) {
        Some(a) if a.enabled => KeyHit::Run(a.id),
        Some(_) => KeyHit::Blocked,
        None => KeyHit::Miss,
    }
}

/// The action a zone addresses, when the zone is one of this table's controls.
pub fn from_zone(acts: &[Act], id: ZoneId) -> Option<&Act> {
    if id.kind != ZoneKind::Action {
        return None;
    }
    acts.iter().find(|a| a.id as u32 == id.index)
}

/// Whether any action in `acts` declares `k`, available or not. Used by the
/// router to decide whether a screen has claimed a key the chrome would
/// otherwise take.
pub fn claims(acts: &[Act], k: &KeyEvent) -> bool {
    !matches!(hit(acts, k), KeyHit::Miss)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn shift_is_not_compared_separately() {
        // A terminal reports `Q` as Char('Q') plus SHIFT. Comparing the
        // modifier as well would reject every capital binding.
        let a = Accel::key('Q');
        let shifted = KeyEvent::new(KeyCode::Char('Q'), KeyModifiers::SHIFT);
        assert!(a.matches(&shifted));
        assert!(!a.matches(&press(KeyCode::Char('q'))));
    }

    #[test]
    fn ctrl_must_agree_in_both_directions() {
        let plain = Accel::key('r');
        let ctrl = Accel::ctrl('r');
        let with = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert!(plain.matches(&press(KeyCode::Char('r'))));
        assert!(!plain.matches(&with));
        assert!(ctrl.matches(&with));
        assert!(!ctrl.matches(&press(KeyCode::Char('r'))));
    }

    #[test]
    fn an_alternate_runs_it_without_being_printed() {
        let a = Accel::key('>').or(KeyCode::Char('.'));
        assert!(a.matches(&press(KeyCode::Char('>'))));
        assert!(a.matches(&press(KeyCode::Char('.'))));
        assert_eq!(a.shown(), ">");
    }

    #[test]
    fn a_disabled_action_still_claims_its_key() {
        // Otherwise the key falls through to a navigation arm and does
        // something the user did not ask for.
        let acts = [Act::toolbar(1, "export", Accel::key('x')).when(false)];
        assert_eq!(hit(&acts, &press(KeyCode::Char('x'))), KeyHit::Blocked);
        assert!(claims(&acts, &press(KeyCode::Char('x'))));
        assert_eq!(hit(&acts, &press(KeyCode::Char('z'))), KeyHit::Miss);
    }

    #[test]
    fn zones_round_trip_through_the_table() {
        let acts = [
            Act::toolbar(0, "import", Accel::key('i')),
            Act::row(7, "delete", Accel::key('d')),
        ];
        assert_eq!(from_zone(&acts, acts[1].zone()).map(|a| a.id), Some(7));
        // A button zone with the same index is a different control entirely.
        assert!(from_zone(&acts, ZoneId::button(7)).is_none());
    }

    #[test]
    fn a_value_overrides_the_label_on_the_control_only() {
        let a = Act::toolbar(0, "mode", Accel::key('o')).cycle().value("split");
        assert_eq!(a.control_label(), "split");
        assert_eq!(a.label, "mode", "help still names the action, not its value");
    }

    #[test]
    fn the_narrow_form_falls_back_to_the_accelerator() {
        assert_eq!(Act::toolbar(0, "import", Accel::key('i')).icon_label(), "i");
        assert_eq!(
            Act::toolbar(0, "import", Accel::key('i')).icon("↧").icon_label(),
            "↧"
        );
    }
}

/// An open context menu: what it lists, where it hangs from, and where the
/// keyboard is inside it.
///
/// Lives on `App` rather than in `Overlay` because it is not one: it carries no
/// state of its own beyond a selection, it is always about the screen it was
/// opened on, and every entry is already declared in that screen's table.
#[derive(Debug, Clone)]
pub struct OpenMenu {
    pub items: Vec<Act>,
    /// The cell it hangs from — the pointer, or the control that opened it.
    pub anchor: (u16, u16),
    pub sel: usize,
}

impl OpenMenu {
    /// Row actions first, then the rest, so the menu reads from the thing that
    /// was clicked outward to the screen it sits on.
    pub fn new(mut items: Vec<Act>, anchor: (u16, u16)) -> Self {
        items.sort_by_key(|a| u8::from(a.placement != Placement::Row));
        let sel = items.iter().position(|a| a.enabled).unwrap_or(0);
        Self { items, anchor, sel }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Move the selection by `delta`, skipping unavailable entries and wrapping.
    /// A menu of nothing but disabled entries leaves the selection alone rather
    /// than looping forever looking for one.
    pub fn step(&mut self, delta: isize) {
        let n = self.items.len();
        if n == 0 || !self.items.iter().any(|a| a.enabled) {
            return;
        }
        let mut i = self.sel;
        for _ in 0..n {
            i = (i as isize + delta).rem_euclid(n as isize) as usize;
            if self.items[i].enabled {
                self.sel = i;
                return;
            }
        }
    }

    pub fn selected(&self) -> Option<u16> {
        self.items
            .get(self.sel)
            .filter(|a| a.enabled)
            .map(|a| a.id)
    }

    /// The action `k` runs inside the menu, if any.
    pub fn hit(&self, k: &KeyEvent) -> KeyHit {
        hit(&self.items, k)
    }
}

#[cfg(test)]
mod menu_tests {
    use super::*;

    #[test]
    fn row_actions_sort_above_screen_actions() {
        let m = OpenMenu::new(
            vec![
                Act::menu(2, "export", Accel::key('x')),
                Act::row(0, "read", Accel::code(KeyCode::Enter)),
                Act::toolbar(1, "rescan", Accel::key('r')),
                Act::row(3, "delete", Accel::key('d')),
            ],
            (0, 0),
        );
        let ids: Vec<u16> = m.items.iter().map(|a| a.id).collect();
        assert_eq!(&ids[..2], &[0, 3], "row actions lead: {ids:?}");
    }

    #[test]
    fn the_selection_opens_on_something_runnable() {
        let m = OpenMenu::new(
            vec![
                Act::row(0, "read", Accel::key('o')).when(false),
                Act::row(1, "delete", Accel::key('d')),
            ],
            (0, 0),
        );
        assert_eq!(m.selected(), Some(1));
    }

    #[test]
    fn stepping_skips_what_cannot_be_run() {
        let mut m = OpenMenu::new(
            vec![
                Act::row(0, "a", Accel::key('a')),
                Act::row(1, "b", Accel::key('b')).when(false),
                Act::row(2, "c", Accel::key('c')),
            ],
            (0, 0),
        );
        assert_eq!(m.selected(), Some(0));
        m.step(1);
        assert_eq!(m.selected(), Some(2));
        m.step(1);
        assert_eq!(m.selected(), Some(0), "wraps");
        m.step(-1);
        assert_eq!(m.selected(), Some(2));
    }

    #[test]
    fn a_menu_of_nothing_available_does_not_spin() {
        let mut m = OpenMenu::new(
            vec![Act::row(0, "a", Accel::key('a')).when(false)],
            (0, 0),
        );
        m.step(1);
        assert_eq!(m.selected(), None);
    }
}
