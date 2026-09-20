//! Every key binding, in one place — the documentation copy.
//!
//! The per-screen **command** sections are not written here. They are read out
//! of each screen's [`super::action_table`], the same declaration `handle_key`
//! dispatches from, so help cannot describe a key the screen does not have.
//!
//! What is written by hand is what is not a screen command: the globals, the
//! pointer gestures, and each screen's *navigation* — scrolling, folding,
//! moving between panels — which are deliberately outside the table.

use std::sync::OnceLock;

use super::Screen;
use super::action_table::{Act, Placement};

/// Where a binding applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Works anywhere.
    Global,
    /// Pointer gestures.
    Mouse,
    /// One screen.
    On(Screen),
}

impl Scope {
    pub fn title(self) -> &'static str {
        match self {
            Scope::Global => "Global",
            Scope::Mouse => "Mouse",
            Scope::On(Screen::Shelf) => "Shelf 書架",
            Scope::On(Screen::Project) => "Project 棚",
            Scope::On(Screen::Translate) => "Translate 訳",
            Scope::On(Screen::Reader) => "Reader 読",
            Scope::On(Screen::Lexicon) => "Lexicon 辞",
            Scope::On(Screen::Refine) => "Refine 推",
        }
    }
}

/// One documented binding.
#[derive(Debug, Clone)]
pub struct Binding {
    /// How the key is written for a reader, not how it is matched.
    pub keys: String,
    pub what: String,
    pub scope: Scope,
}

fn b(keys: &str, what: &str, scope: Scope) -> Binding {
    Binding {
        keys: keys.to_string(),
        what: what.to_string(),
        scope,
    }
}

/// The sections, in the order help shows them.
pub const SECTIONS: &[Scope] = &[
    Scope::Global,
    Scope::Mouse,
    Scope::On(Screen::Shelf),
    Scope::On(Screen::Project),
    Scope::On(Screen::Translate),
    Scope::On(Screen::Reader),
    Scope::On(Screen::Lexicon),
    Scope::On(Screen::Refine),
];

/// The bindings that are not screen commands: what works everywhere, what the
/// pointer does, and how you move around inside each screen.
fn hand_written() -> Vec<Binding> {
    use Screen::*;
    vec![
        // --- Global. Tab moves between controls, not between screens: with
        // everything clickable, "next control" is the more useful thing for
        // the key nearest the home row to mean.
        b("Tab / Shift-Tab", "next · previous control", Scope::Global),
        b("] / [", "next · previous screen", Scope::Global),
        b("1–6", "jump to a screen", Scope::Global),
        b("Space / Enter", "activate the focused control", Scope::Global),
        b("Esc", "back one step", Scope::Global),
        b(": / Ctrl-P / Ctrl-K", "command bar", Scope::Global),
        b("?", "this help", Scope::Global),
        b("Ctrl-T", "theme picker", Scope::Global),
        b("` / l", "activity log (Project keeps l)", Scope::Global),
        b("Ctrl-C", "cancel a run · press twice to quit", Scope::Global),
        // --- Mouse.
        b("click", "tabs, rows, buttons, breadcrumb, counts", Scope::Mouse),
        b("double-click", "open the row under the pointer", Scope::Mouse),
        b("wheel", "scroll the pane under the pointer", Scope::Mouse),
        b("right-click", "a row's menu · elsewhere, back", Scope::Mouse),
        // --- Per-screen navigation. Not commands: they have no button and no
        // menu entry, because the wheel and a click already do them.
        b("↑ ↓ / j k", "move the cursor", Scope::On(Shelf)),
        b("Home / End", "first · last row", Scope::On(Shelf)),
        b("↑ ↓ / j k", "move the cursor", Scope::On(Project)),
        b("h / l", "collapse · expand, focus panel", Scope::On(Project)),
        b("z / Z", "collapse · expand all volumes", Scope::On(Project)),
        b("↑ ↓ / j k", "scroll the preview", Scope::On(Translate)),
        b("g", "focus the queue", Scope::On(Translate)),
        b("↑ ↓ / j k", "scroll", Scope::On(Reader)),
        b("Space / b", "page down · up", Scope::On(Reader)),
        b("Tab", "next section", Scope::On(Lexicon)),
        b("↑ ↓ / j k", "move the cursor", Scope::On(Lexicon)),
        b("type", "focus the input", Scope::On(Refine)),
        b("Ctrl-R", "expand the last turn's details", Scope::On(Refine)),
        b("Ctrl-End", "jump to the bottom", Scope::On(Refine)),
        b("@ · /", "mention a chapter · slash command", Scope::On(Refine)),
    ]
}

/// A screen's commands, as it declares them. Availability is irrelevant here —
/// help documents what a screen *has*, not what happens to be reachable right
/// now — so each table is read from a screen in its default state.
fn screen_commands(screen: Screen) -> Vec<Act> {
    match screen {
        Screen::Shelf => super::shelf::ShelfScreen::new().actions(&[]),
        Screen::Project => super::project::ProjectScreen::default().actions(None),
        Screen::Translate => super::translate::TranslateScreen::new().actions(),
        Screen::Reader => super::reader::ReaderScreen::new().actions(),
        Screen::Lexicon => super::lexicon::LexiconScreen::new().actions(None),
        Screen::Refine => super::refine::RefineScreen::new().actions(None),
    }
}

/// How an action reads in help: its name, and where it can be reached.
fn documented(act: &Act) -> String {
    match act.placement {
        Placement::Toolbar => act.label.to_string(),
        Placement::Row => format!("{} (this row)", act.label),
        Placement::Menu => format!("{} (right-click)", act.label),
    }
}

/// Every binding worth telling someone about: the hand-written ones, plus each
/// screen's commands read out of its action table.
pub fn bindings() -> &'static [Binding] {
    static ALL: OnceLock<Vec<Binding>> = OnceLock::new();
    ALL.get_or_init(|| {
        let mut all = hand_written();
        for screen in crate::ui::chrome::TAB_SCREENS {
            for act in screen_commands(screen) {
                all.push(Binding {
                    keys: act.accel.shown(),
                    what: documented(&act),
                    scope: Scope::On(screen),
                });
            }
        }
        all
    })
}

/// The bindings in one scope, in declaration order.
pub fn in_scope(scope: Scope) -> impl Iterator<Item = &'static Binding> {
    bindings().iter().filter(move |b| b.scope == scope)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_section_has_bindings_and_every_binding_has_a_section() {
        for s in SECTIONS {
            assert!(
                in_scope(*s).next().is_some(),
                "{:?} is listed but empty",
                s.title()
            );
        }
        for b in bindings() {
            assert!(
                SECTIONS.contains(&b.scope),
                "{:?} is in a scope help never shows",
                b.keys
            );
        }
    }

    #[test]
    fn every_screen_is_documented() {
        // A screen with no bindings listed is a screen nobody can learn.
        for screen in crate::ui::chrome::TAB_SCREENS {
            assert!(
                in_scope(Scope::On(screen)).next().is_some(),
                "{screen:?} has no documented bindings"
            );
        }
    }

    #[test]
    fn nothing_is_blank() {
        for b in bindings() {
            assert!(!b.keys.is_empty(), "a binding has no key");
            assert!(!b.what.trim().is_empty(), "{:?} has no description", b.keys);
        }
    }

    /// The assertion this table never had: what help prints for a screen is
    /// exactly what that screen declares it can do. A `Q` documented with no
    /// handler cannot survive this, because the handler is what declares it.
    #[test]
    fn help_prints_exactly_what_each_screen_declares() {
        for screen in crate::ui::chrome::TAB_SCREENS {
            let declared: Vec<String> = screen_commands(screen)
                .iter()
                .map(|a| format!("{} {}", a.accel.shown(), documented(a)))
                .collect();
            let printed: Vec<String> = in_scope(Scope::On(screen))
                .map(|b| format!("{} {}", b.keys, b.what))
                .filter(|line| declared.iter().any(|d| d == line))
                .collect();
            assert_eq!(
                printed, declared,
                "{screen:?}'s help and its action table disagree"
            );
        }
    }

    #[test]
    fn a_key_is_not_documented_twice_in_one_scope() {
        for s in SECTIONS {
            let mut keys: Vec<&str> = in_scope(*s).map(|b| b.keys.as_str()).collect();
            let total = keys.len();
            keys.sort_unstable();
            keys.dedup();
            assert_eq!(
                keys.len(),
                total,
                "{} documents the same key twice",
                s.title()
            );
        }
    }

    #[test]
    fn a_screen_binding_that_shadows_a_global_says_so_in_its_own_section() {
        // Space and Enter are global "activate" keys, and two screens give them
        // a local meaning. That is allowed, but the local meaning has to be
        // documented where someone on that screen will look for it.
        for (screen, key) in [
            (Screen::Project, "Space"),
            (Screen::Shelf, "↵"),
            (Screen::Lexicon, "Tab"),
        ] {
            assert!(
                in_scope(Scope::On(screen)).any(|b| b.keys.starts_with(key)),
                "{screen:?} overrides {key} without documenting it"
            );
        }
    }
}
