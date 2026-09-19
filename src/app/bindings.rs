//! Every key binding, in one place.
//!
//! The same facts used to be written down three times — once in the help
//! overlay's pre-formatted lines, once in each screen's footer hints, and once
//! in the routing that actually implements them. Three copies of a list that
//! only ever grows is three chances to document a key that no longer exists,
//! and no chance at all of noticing.
//!
//! This is the documentation copy: what a user is told. The routing still
//! decides what a key *does* — a binding table that dispatched as well as
//! described would have to model every conditional a screen applies, which is
//! more coupling than it would buy. What it does guarantee is that the help
//! overlay and the command bar cannot drift apart from each other.

use super::Screen;

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
#[derive(Debug, Clone, Copy)]
pub struct Binding {
    /// How the key is written for a reader, not how it is matched.
    pub keys: &'static str,
    pub what: &'static str,
    pub scope: Scope,
}

const fn b(keys: &'static str, what: &'static str, scope: Scope) -> Binding {
    Binding { keys, what, scope }
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

/// Every binding worth telling someone about.
pub const BINDINGS: &[Binding] = &[
    // --- Global. Tab moves between controls, not between screens: with
    // everything clickable, "next control" is the more useful thing for the
    // key nearest the home row to mean.
    b("Tab / Shift-Tab", "next · previous control", Scope::Global),
    b("] / [", "next · previous screen", Scope::Global),
    b("1–6", "jump to a screen", Scope::Global),
    b("Space", "activate the focused control", Scope::Global),
    b("Enter", "activate the default control", Scope::Global),
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
    b("right-click", "back · dismiss", Scope::Mouse),

    // --- Shelf.
    b("Enter", "open project", Scope::On(Screen::Shelf)),
    b("i", "import a source file", Scope::On(Screen::Shelf)),
    b("d / R / r", "delete · rename · rescan", Scope::On(Screen::Shelf)),

    // --- Project.
    b("Enter", "read chapter", Scope::On(Screen::Project)),
    b("Space", "mark chapter (cross-volume ok)", Scope::On(Screen::Project)),
    b("t / a", "translate · queue marked or current", Scope::On(Screen::Project)),
    b("T / A", "translate volume · whole project", Scope::On(Screen::Project)),
    b("V / i", "add volume · add chapters", Scope::On(Screen::Project)),
    b("h / l", "collapse · expand, focus panel", Scope::On(Screen::Project)),
    b("z / Z", "collapse · expand all volumes", Scope::On(Screen::Project)),
    b("x / Q", "export · QA report", Scope::On(Screen::Project)),
    b("e / y", "edit title · synopsis", Scope::On(Screen::Project)),
    b("M / d", "update images · delete chapters", Scope::On(Screen::Project)),

    // --- Translate.
    b("p / s", "pause · stop the run", Scope::On(Screen::Translate)),
    b("J / K", "move queued chapter down · up", Scope::On(Screen::Translate)),

    // --- Reader.
    b("/", "search both panes", Scope::On(Screen::Reader)),
    b("g", "jump to chapter, section or bookmark", Scope::On(Screen::Reader)),
    b("w / y", "wrap · sync the panes", Scope::On(Screen::Reader)),
    b("b / n", "bookmark · note this line", Scope::On(Screen::Reader)),
    b("Enter", "inspect the chunk under the cursor", Scope::On(Screen::Reader)),

    // --- Lexicon.
    b("Tab", "next section", Scope::On(Screen::Lexicon)),
    b("Enter / d", "edit · delete entry", Scope::On(Screen::Lexicon)),
    b("/", "filter", Scope::On(Screen::Lexicon)),

    // --- Refine.
    b("Ctrl-R", "new session", Scope::On(Screen::Refine)),
    b("Ctrl-C", "cancel the in-flight turn", Scope::On(Screen::Refine)),
    b("/ · @", "slash command · mention a chapter", Scope::On(Screen::Refine)),
];

/// The bindings in one scope, in declaration order.
pub fn in_scope(scope: Scope) -> impl Iterator<Item = &'static Binding> {
    BINDINGS.iter().filter(move |b| b.scope == scope)
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
        for b in BINDINGS {
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
        for b in BINDINGS {
            assert!(!b.keys.is_empty(), "a binding has no key");
            assert!(!b.what.trim().is_empty(), "{:?} has no description", b.keys);
        }
    }

    #[test]
    fn a_key_is_not_documented_twice_in_one_scope() {
        for s in SECTIONS {
            let mut keys: Vec<&str> = in_scope(*s).map(|b| b.keys).collect();
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
            (Screen::Shelf, "Enter"),
            (Screen::Lexicon, "Tab"),
        ] {
            assert!(
                in_scope(Scope::On(screen)).any(|b| b.keys.starts_with(key)),
                "{screen:?} overrides {key} without documenting it"
            );
        }
    }
}
