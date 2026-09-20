//! An egui key event as the one `action_table` matches against.
//!
//! The screens already resolve a key to a command from their declared table —
//! `Accel::matches` over a crossterm `KeyEvent`. Translating the window's key
//! events into that shape means the GUI runs the *same* resolution as the
//! terminal, so a chord cannot mean two different things in two front ends and
//! a command added to a screen is bound in both at once.
//!
//! Only commands route this way. Scrolling, paging and folding stay native to
//! each pane, because `action_table.rs` deliberately keeps navigation out of
//! the declaration and because a pane here is measured in pixels, not cells.

use egui::Key;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// `None` for a key the terminal vocabulary has no word for.
pub fn to_crossterm(key: Key, mods: egui::Modifiers) -> Option<KeyEvent> {
    let code = match key {
        Key::Enter => KeyCode::Enter,
        Key::Tab if mods.shift => KeyCode::BackTab,
        Key::Tab => KeyCode::Tab,
        Key::Space => KeyCode::Char(' '),
        Key::Escape => KeyCode::Esc,
        Key::Backspace => KeyCode::Backspace,
        Key::Delete => KeyCode::Delete,
        Key::Insert => KeyCode::Insert,
        Key::Home => KeyCode::Home,
        Key::End => KeyCode::End,
        Key::PageUp => KeyCode::PageUp,
        Key::PageDown => KeyCode::PageDown,
        Key::ArrowUp => KeyCode::Up,
        Key::ArrowDown => KeyCode::Down,
        Key::ArrowLeft => KeyCode::Left,
        Key::ArrowRight => KeyCode::Right,
        other => {
            let name = other.name();
            let mut chars = name.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                // `F5`, `Minus`, `OpenBracket` — egui's own names, which the
                // table below turns into the character a terminal would send.
                return punctuation(name, mods.shift).map(|code| event(code, mods));
            }
            // egui reports letters as uppercase names. A terminal sends the
            // lowercase codepoint unless shift is down, and `Accel::matches`
            // compares Ctrl only — so the case here *is* the shift state.
            let c = if mods.shift {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            };
            KeyCode::Char(c)
        }
    };
    Some(event(code, mods))
}

fn event(code: KeyCode, mods: egui::Modifiers) -> KeyEvent {
    let mut m = KeyModifiers::empty();
    // Command on macOS and Ctrl elsewhere both mean "the chord modifier",
    // which is the only one `Accel` compares.
    if mods.ctrl || mods.command || mods.mac_cmd {
        m |= KeyModifiers::CONTROL;
    }
    if mods.shift {
        m |= KeyModifiers::SHIFT;
    }
    if mods.alt {
        m |= KeyModifiers::ALT;
    }
    KeyEvent::new(code, m)
}

/// A terminal sends the *shifted* character, not the key plus a modifier —
/// `>` rather than `.` with shift — and the screens bind `>` and `<`. The
/// shifted forms assume a US layout, which is the same assumption the
/// accelerators themselves make.
fn punctuation(name: &str, shift: bool) -> Option<KeyCode> {
    if shift {
        let shifted = match name {
            "Period" => Some('>'),
            "Comma" => Some('<'),
            "Slash" => Some('?'),
            "Semicolon" => Some(':'),
            "Minus" => Some('_'),
            "Equals" => Some('+'),
            "OpenBracket" => Some('{'),
            "CloseBracket" => Some('}'),
            "Backslash" => Some('|'),
            "Quote" => Some('"'),
            "Backtick" => Some('~'),
            _ => None,
        };
        if let Some(c) = shifted {
            return Some(KeyCode::Char(c));
        }
    }
    Some(match name {
        "Minus" => KeyCode::Char('-'),
        "Plus" => KeyCode::Char('+'),
        "Equals" => KeyCode::Char('='),
        "Slash" => KeyCode::Char('/'),
        "Backslash" => KeyCode::Char('\\'),
        "Pipe" => KeyCode::Char('|'),
        "Colon" => KeyCode::Char(':'),
        "Semicolon" => KeyCode::Char(';'),
        "Comma" => KeyCode::Char(','),
        "Period" => KeyCode::Char('.'),
        "Questionmark" => KeyCode::Char('?'),
        "Exclamationmark" => KeyCode::Char('!'),
        "Quote" => KeyCode::Char('\''),
        "Backtick" => KeyCode::Char('`'),
        "OpenBracket" => KeyCode::Char('['),
        "CloseBracket" => KeyCode::Char(']'),
        "OpenCurlyBracket" => KeyCode::Char('{'),
        "CloseCurlyBracket" => KeyCode::Char('}'),
        other => {
            let n = other.strip_prefix('F')?.parse::<u8>().ok()?;
            if (1..=12).contains(&n) {
                KeyCode::F(n)
            } else {
                return None;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::action_table::{self, Accel, Act, KeyHit, Placement};

    fn none() -> egui::Modifiers {
        egui::Modifiers::NONE
    }

    fn ctrl() -> egui::Modifiers {
        egui::Modifiers {
            ctrl: true,
            ..egui::Modifiers::NONE
        }
    }

    fn shift() -> egui::Modifiers {
        egui::Modifiers {
            shift: true,
            ..egui::Modifiers::NONE
        }
    }

    #[test]
    fn a_letter_carries_its_shift_state_in_its_case() {
        let lower = to_crossterm(Key::Q, none()).unwrap();
        assert_eq!(lower.code, KeyCode::Char('q'));
        let upper = to_crossterm(Key::Q, shift()).unwrap();
        assert_eq!(upper.code, KeyCode::Char('Q'));
    }

    #[test]
    fn a_chord_arrives_as_the_lowercase_key_with_control() {
        let ev = to_crossterm(Key::G, ctrl()).unwrap();
        assert_eq!(ev.code, KeyCode::Char('g'));
        assert!(ev.modifiers.contains(KeyModifiers::CONTROL));
        assert!(Accel::ctrl('g').matches(&ev));
    }

    #[test]
    fn shift_tab_is_backtab_the_way_a_terminal_sends_it() {
        assert_eq!(to_crossterm(Key::Tab, shift()).unwrap().code, KeyCode::BackTab);
        assert_eq!(to_crossterm(Key::Tab, none()).unwrap().code, KeyCode::Tab);
    }

    #[test]
    fn the_punctuation_the_screens_bind_all_survives_the_trip() {
        for (key, want) in [
            (Key::OpenBracket, '['),
            (Key::CloseBracket, ']'),
            (Key::Slash, '/'),
            (Key::Comma, ','),
            (Key::Period, '.'),
            (Key::Semicolon, ';'),
        ] {
            let ev = to_crossterm(key, none())
                .unwrap_or_else(|| panic!("{key:?} has no terminal equivalent"));
            assert_eq!(ev.code, KeyCode::Char(want), "{key:?}");
        }
    }

    #[test]
    fn a_named_key_maps_to_its_code() {
        for (key, want) in [
            (Key::Enter, KeyCode::Enter),
            (Key::Escape, KeyCode::Esc),
            (Key::ArrowUp, KeyCode::Up),
            (Key::PageDown, KeyCode::PageDown),
            (Key::Home, KeyCode::Home),
            (Key::Space, KeyCode::Char(' ')),
        ] {
            assert_eq!(to_crossterm(key, none()).unwrap().code, want, "{key:?}");
        }
    }

    /// The point of the whole module: every accelerator a screen declares must
    /// be reachable from a window key event, or that command is TUI-only.
    #[test]
    fn every_declared_accelerator_can_be_produced_from_the_window() {
        for screen in [
            crate::app::Screen::Shelf,
            crate::app::Screen::Project,
            crate::app::Screen::Translate,
            crate::app::Screen::Reader,
            crate::app::Screen::Lexicon,
            crate::app::Screen::Refine,
        ] {
            let acts = crate::app::bindings::commands_for(screen);
            for act in &acts {
                let ev = synthesize(&act.accel).unwrap_or_else(|| {
                    panic!("{screen:?}/{} cannot be typed in the GUI", act.label)
                });
                match action_table::hit(&acts, &ev) {
                    // Blocked is fine: the command is declared but unavailable
                    // in a default screen, which is still the table claiming
                    // the key rather than letting it fall through.
                    KeyHit::Blocked => {}
                    KeyHit::Run(id) => assert_eq!(
                        id, act.id,
                        "{screen:?}/{} resolved to another command",
                        act.label
                    ),
                    KeyHit::Miss => panic!(
                        "{screen:?}/{} is declared but its key reaches nothing",
                        act.label
                    ),
                }
            }
        }
    }

    /// Build the window event that would produce this accelerator.
    fn synthesize(accel: &Accel) -> Option<KeyEvent> {
        let (key, shifted) = match accel.code {
            KeyCode::Char(c) if c.is_ascii_alphabetic() => {
                (Key::from_name(&c.to_ascii_uppercase().to_string())?, c.is_uppercase())
            }
            KeyCode::Char(' ') => (Key::Space, false),
            KeyCode::Char(c) => key_for_punctuation(c)?,
            KeyCode::Enter => (Key::Enter, false),
            KeyCode::Tab => (Key::Tab, false),
            KeyCode::BackTab => (Key::Tab, true),
            KeyCode::Esc => (Key::Escape, false),
            KeyCode::Up => (Key::ArrowUp, false),
            KeyCode::Down => (Key::ArrowDown, false),
            KeyCode::Left => (Key::ArrowLeft, false),
            KeyCode::Right => (Key::ArrowRight, false),
            KeyCode::Home => (Key::Home, false),
            KeyCode::End => (Key::End, false),
            KeyCode::PageUp => (Key::PageUp, false),
            KeyCode::PageDown => (Key::PageDown, false),
            KeyCode::Delete => (Key::Delete, false),
            KeyCode::Backspace => (Key::Backspace, false),
            KeyCode::F(n) => (Key::from_name(&format!("F{n}"))?, false),
            _ => return None,
        };
        to_crossterm(
            key,
            egui::Modifiers {
                ctrl: accel.ctrl,
                shift: shifted,
                ..egui::Modifiers::NONE
            },
        )
    }

    /// The key, and whether shift is held to produce that character.
    fn key_for_punctuation(c: char) -> Option<(Key, bool)> {
        Some(match c {
            '[' => (Key::OpenBracket, false),
            ']' => (Key::CloseBracket, false),
            '/' => (Key::Slash, false),
            ',' => (Key::Comma, false),
            '.' => (Key::Period, false),
            ';' => (Key::Semicolon, false),
            '-' => (Key::Minus, false),
            '=' => (Key::Equals, false),
            '`' => (Key::Backtick, false),
            '>' => (Key::Period, true),
            '<' => (Key::Comma, true),
            '?' => (Key::Slash, true),
            ':' => (Key::Semicolon, true),
            _ => return None,
        })
    }

    #[test]
    fn a_synthesized_toolbar_key_runs_that_toolbar_command() {
        let acts = vec![
            Act::new(1, "tasks", Accel::ctrl('g'), Placement::Toolbar),
            Act::new(2, "delete", Accel::key('d'), Placement::Toolbar),
        ];
        let ev = to_crossterm(Key::D, none()).unwrap();
        assert!(matches!(action_table::hit(&acts, &ev), KeyHit::Run(2)));
    }
}
