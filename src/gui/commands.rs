//! The screen's own commands, drawn from the table it declares them in.
//!
//! `action_table.rs` says it plainly: a screen declares each command once, and
//! "`handle_key` dispatches from it, the toolbar and the context menu draw
//! from it, help lists it." The GUI read none of it — every button was a
//! hand-written literal, so the GUI was a second copy of a surface that
//! already exists as data, kept in step by hand and 23 actions behind.
//!
//! Drawing from `Act` means the key printed on a control *is* the key that
//! runs it, a disabled command greys rather than vanishing, and a command
//! added to a screen appears here without anyone remembering to add it.

use egui::{RichText, Ui};

use crate::app::App;
use crate::app::action_table::{Act, Kind, Placement};

use super::theme_map::GuiPalette;

/// How an action's accelerator is written for a GUI tooltip. The TUI prints
/// `^G`; a desktop window says `Ctrl+G`.
pub fn accel_text(act: &Act) -> String {
    let shown = act.accel.shown();
    let bare = shown.trim_start_matches('^');
    if act.accel.ctrl {
        // `Ctrl+g` reads as a different key from `Ctrl+G`; a chord is written
        // with the capital even though the keycode is lowercase.
        format!("Ctrl+{}", bare.to_uppercase())
    } else {
        bare.to_string()
    }
}

/// What a control says. A toggle or a cycle shows its value rather than its
/// name, and a live count rides along.
fn label_of(act: &Act) -> String {
    let base = act.value.unwrap_or(act.label);
    let mut out = match act.kind {
        Kind::Toggle(on) => format!("{} {base}", if on { "◉" } else { "○" }),
        Kind::Verb | Kind::Cycle => base.to_string(),
    };
    if let Some(n) = act.count {
        out.push_str(&format!("  {n}"));
    }
    out
}

/// How a screen's declared commands are split between the surfaces that draw
/// them. Every act lands in exactly one, which is what the parity test checks:
/// a command in neither would be one the window silently does not have.
pub struct Surfaces {
    /// Drawn inline in the screen's toolbar.
    pub toolbar: Vec<Act>,
    /// Behind the `⋯` menu — row commands and the long tail.
    pub menu: Vec<Act>,
}

pub fn surfaces(acts: Vec<Act>) -> Surfaces {
    let (toolbar, menu) = acts
        .into_iter()
        .partition(|a| a.placement == Placement::Toolbar);
    Surfaces { toolbar, menu }
}

/// Draw the screen's toolbar commands inline. Returns the id the user ran.
///
/// Replaces the hand-written button literals: the key printed on a control is
/// the key that runs it, an unavailable command greys rather than vanishing,
/// and a command added to a screen appears without anyone remembering to add
/// it here.
pub fn toolbar(ui: &mut Ui, app: &App, pal: &GuiPalette) -> Option<u16> {
    let mut ran = None;
    for act in surfaces(app.screen_actions()).toolbar {
        ui.push_id(("act", act.id), |ui| {
            let selected = matches!(act.kind, Kind::Toggle(true));
            let text = RichText::new(label_of(&act))
                .small()
                .color(if act.enabled { pal.ink } else { pal.ink_faint });
            if ui
                .add_enabled(act.enabled, egui::Button::selectable(selected, text))
                .on_hover_text(format!("{}  ·  {}", act.label, accel_text(&act)))
                .clicked()
            {
                ran = Some(act.id);
            }
        });
    }
    ran
}

/// Every command the active screen declares, with the key that runs it.
///
/// A menu rather than a second button row: each screen still designs its own
/// toolbar, and this guarantees that nothing a screen declares is unreachable
/// — which is what 23 of the 89 actions were.
pub fn menu(ui: &mut Ui, app: &App, pal: &GuiPalette) -> Option<u16> {
    let acts = app.screen_actions();
    if acts.is_empty() {
        return None;
    }
    let mut ran = None;
    let running = acts.iter().filter(|a| a.enabled).count();
    let total = acts.len();
    let Surfaces { menu, .. } = surfaces(acts);
    ui.menu_button(RichText::new("⋯").color(pal.ink_soft), |ui| {
        ui.label(
            RichText::new(format!("{running} of {total} available"))
                .color(pal.ink_faint)
                .small(),
        );
        ui.separator();
        let mut placement = None;
        for act in &menu {
            // The half the toolbar does not draw: row commands and the long
            // tail. Between the two surfaces, nothing declared is unreachable.
            if placement.is_some_and(|p| p != act.placement) {
                ui.separator();
            }
            placement = Some(act.placement);
            let hint = accel_text(act);
            if ui
                .add_enabled(
                    act.enabled,
                    egui::Button::new(format!("{}      {hint}", label_of(act))),
                )
                .clicked()
            {
                ran = Some(act.id);
                ui.close();
            }
        }
    });
    ran
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::action_table::{Accel, Placement};
    use ratatui::crossterm::event::KeyCode;

    fn act(id: u16, label: &'static str, accel: Accel) -> Act {
        Act::new(id, label, accel, Placement::Toolbar)
    }

    #[test]
    fn an_accelerator_is_written_the_way_a_window_writes_it() {
        assert_eq!(accel_text(&act(0, "tasks", Accel::ctrl('g'))), "Ctrl+G");
        assert_eq!(accel_text(&act(0, "delete", Accel::key('d'))), "d");
        assert_eq!(
            accel_text(&act(0, "open", Accel::code(KeyCode::Enter))),
            "↵"
        );
    }

    #[test]
    fn a_toggle_shows_whether_it_is_on_and_a_cycle_shows_its_value() {
        let mut on = act(1, "wrap", Accel::key('w'));
        on.kind = Kind::Toggle(true);
        assert!(label_of(&on).starts_with('◉'));

        let mut off = on;
        off.kind = Kind::Toggle(false);
        assert!(label_of(&off).starts_with('○'));

        let mut cycle = act(2, "mode", Accel::key('o'));
        cycle.kind = Kind::Cycle;
        cycle.value = Some("split");
        assert_eq!(label_of(&cycle), "split", "a cycle reads as its value");
    }

    #[test]
    fn a_live_count_rides_on_the_label() {
        let mut a = act(3, "notes", Accel::key('n'));
        a.count = Some(4);
        assert!(label_of(&a).contains('4'));
    }

    #[test]
    fn every_screen_declares_commands_the_gui_can_draw() {
        // The parity ratchet: a screen whose table the GUI cannot render is a
        // screen whose commands the GUI silently does not have.
        for screen in [
            crate::app::Screen::Shelf,
            crate::app::Screen::Project,
            crate::app::Screen::Translate,
            crate::app::Screen::Reader,
            crate::app::Screen::Lexicon,
            crate::app::Screen::Refine,
        ] {
            for a in crate::app::bindings::commands_for(screen) {
                assert!(!a.label.is_empty(), "{screen:?} has an unnamed command");
                assert!(
                    !accel_text(&a).is_empty(),
                    "{screen:?}/{} has no accelerator to print",
                    a.label
                );
                assert!(!label_of(&a).is_empty());
            }
        }
    }

    /// The ratchet the plan asks for: every command a screen declares has to
    /// land on a surface the window draws. One in neither is one the window
    /// silently does not have — which is how 23 of 89 actions went missing.
    #[test]
    fn every_screen_action_is_reachable_from_the_gui() {
        for screen in [
            crate::app::Screen::Shelf,
            crate::app::Screen::Project,
            crate::app::Screen::Translate,
            crate::app::Screen::Reader,
            crate::app::Screen::Lexicon,
            crate::app::Screen::Refine,
        ] {
            let declared = crate::app::bindings::commands_for(screen);
            let s = surfaces(declared.clone());
            for act in &declared {
                let on_toolbar = s.toolbar.iter().any(|a| a.id == act.id);
                let in_menu = s.menu.iter().any(|a| a.id == act.id);
                assert!(
                    on_toolbar || in_menu,
                    "{screen:?}/{} is declared but reaches no surface",
                    act.label
                );
                assert!(
                    !(on_toolbar && in_menu),
                    "{screen:?}/{} is drawn twice",
                    act.label
                );
            }
            assert_eq!(s.toolbar.len() + s.menu.len(), declared.len());
        }
    }
}
