//! User keybindings, resolved against the same declarations both front ends
//! dispatch from.
//!
//! A screen declares its commands once, each with an accelerator
//! (`action_table::Act`). Rather than intercepting dispatch, a user binding
//! *rewrites the declaration* before anything reads it — so one file moves the
//! key in the TUI's `handle_key`, in the toolbar label, in help, and in the
//! GUI's menu, because all four read the same `Act`.
//!
//! The rewrite happens inside [`super::App::screen_actions`], the one place
//! that serves the table. Applying it at a call site instead left the other
//! seven reading the unmodified declarations.
//!
//! `~/.config/honya/keybindings.json`:
//!
//! ```json
//! [{ "key": "ctrl+j", "command": "refine.sub-agent-tasks", "when": "screen.refine" }]
//! ```
//!
//! Last matching rule wins, so a later rule overrides an earlier one — which
//! is what lets a file append an exception without editing what came before.

use ratatui::crossterm::event::KeyCode;

use super::Screen;
use super::action_table::{Accel, Act};

/// One user rule. `when` is optional; absent means "wherever the command is".
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct Rule {
    pub key: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Bindings {
    rules: Vec<Rule>,
}

fn bindings_path() -> std::path::PathBuf {
    crate::config::config_dir().join("keybindings.json")
}

impl Bindings {
    /// Read the user's file. A missing or unreadable one is simply no rules —
    /// a broken keymap must not be a broken app.
    pub fn load() -> Self {
        let rules = std::fs::read_to_string(bindings_path())
            .ok()
            .and_then(|s| serde_json::from_str::<Vec<Rule>>(&s).ok())
            .unwrap_or_default();
        Self { rules }
    }

    #[cfg(test)]
    pub fn from_rules(rules: Vec<Rule>) -> Self {
        Self { rules }
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Rewrite each act's accelerator where the user bound its command.
    ///
    /// Applied to the table, not to the dispatch, so the key printed on a
    /// control stays the key that runs it. Reached only through
    /// `App::screen_actions`, which is what makes that true of every surface
    /// rather than of whichever caller remembered to ask.
    pub(crate) fn apply(&self, screen: Screen, acts: &mut [Act]) {
        if self.rules.is_empty() {
            return;
        }
        for act in acts.iter_mut() {
            let id = command_id(screen, act);
            // Last match wins: a later rule is an exception to an earlier one.
            let bound = self
                .rules
                .iter()
                .rfind(|r| r.command == id && when_matches(r.when.as_deref(), screen))
                .and_then(|r| parse_key(&r.key));
            if let Some(accel) = bound {
                act.accel = accel;
            }
        }
    }
}

/// Does a rule's `when` apply on this screen? Only `screen.<name>` is
/// understood; an unknown condition never matches, so a typo disables the rule
/// rather than applying it everywhere.
fn when_matches(when: Option<&str>, screen: Screen) -> bool {
    match when {
        None => true,
        Some(cond) => cond
            .trim()
            .strip_prefix("screen.")
            .is_some_and(|name| name.eq_ignore_ascii_case(screen_slug(screen))),
    }
}

pub fn screen_slug(screen: Screen) -> &'static str {
    match screen {
        Screen::Shelf => "shelf",
        Screen::Project => "project",
        Screen::Translate => "translate",
        Screen::Reader => "reader",
        Screen::Lexicon => "lexicon",
        Screen::Refine => "refine",
    }
}

/// What a user writes to name a command: the screen, then the label it is
/// declared with, lowercased and hyphenated.
pub fn command_id(screen: Screen, act: &Act) -> String {
    format!("{}.{}", screen_slug(screen), slug(act.label))
}

fn slug(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut hyphen = false;
    for c in label.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            hyphen = false;
        } else if !out.is_empty() && !hyphen {
            out.push('-');
            hyphen = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// `ctrl+g`, `shift+tab`, `enter`, `f5`, `]`. Case-insensitive.
pub fn parse_key(spec: &str) -> Option<Accel> {
    let mut ctrl = false;
    let mut shift = false;
    for part in spec.split('+') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "mod" | "cmd" | "command" => ctrl = true,
            // Shift is folded into the keycode a terminal reports, the way
            // `Accel::matches` expects, so `shift+q` is simply `Q`.
            "shift" => shift = true,
            _ => {
                if shift {
                    return named_key(part).map(|code| shifted(code, ctrl));
                }
                return named_key(part).map(|code| Accel {
                    // A terminal reports a ctrl chord with the lowercase key,
                    // so `CTRL+G` and `ctrl+g` are the same binding. Without
                    // ctrl the case is the user's: bare `Q` *is* shift-q.
                    code: if ctrl { lowered(code) } else { code },
                    ctrl,
                    alt: None,
                });
            }
        }
    }
    None
}

fn lowered(code: KeyCode) -> KeyCode {
    match code {
        KeyCode::Char(c) => KeyCode::Char(c.to_ascii_lowercase()),
        other => other,
    }
}

fn shifted(code: KeyCode, ctrl: bool) -> Accel {
    let code = match code {
        KeyCode::Char(c) => KeyCode::Char(c.to_ascii_uppercase()),
        other => other,
    };
    Accel {
        code,
        ctrl,
        alt: None,
    }
}

fn named_key(name: &str) -> Option<KeyCode> {
    let lower = name.to_ascii_lowercase();
    Some(match lower.as_str() {
        "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "space" => KeyCode::Char(' '),
        "esc" | "escape" => KeyCode::Esc,
        "backspace" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        _ => {
            if let Some(n) = lower.strip_prefix('f')
                && let Ok(n) = n.parse::<u8>()
                && (1..=12).contains(&n)
            {
                return Some(KeyCode::F(n));
            }
            let mut chars = name.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            KeyCode::Char(c)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::action_table::Placement;

    fn act(label: &'static str, accel: Accel) -> Act {
        Act::new(1, label, accel, Placement::Toolbar)
    }

    #[test]
    fn a_command_is_named_by_its_screen_and_its_label() {
        let a = act("undo last chapter edit", Accel::ctrl('u'));
        assert_eq!(
            command_id(Screen::Refine, &a),
            "refine.undo-last-chapter-edit"
        );
        assert_eq!(slug("copy selected block"), "copy-selected-block");
        assert_eq!(slug("QA  ·  findings"), "qa-findings");
    }

    #[test]
    fn a_key_spec_parses_the_way_it_is_written() {
        assert_eq!(parse_key("ctrl+g"), Some(Accel::ctrl('g')));
        assert_eq!(parse_key("CTRL+G"), Some(Accel::ctrl('g')));
        assert_eq!(parse_key("d"), Some(Accel::key('d')));
        assert_eq!(parse_key("enter"), Some(Accel::code(KeyCode::Enter)));
        assert_eq!(parse_key("f5"), Some(Accel::code(KeyCode::F(5))));
        assert_eq!(parse_key("]"), Some(Accel::key(']')));
        // Shift is folded into the keycode, the way a terminal reports it.
        assert_eq!(parse_key("shift+q"), Some(Accel::key('Q')));
        assert_eq!(parse_key("nonsense"), None);
        assert_eq!(parse_key(""), None);
    }

    #[test]
    fn a_user_binding_moves_the_key_on_the_declaration() {
        let mut acts = vec![act("sub-agent tasks", Accel::ctrl('g'))];
        let b = Bindings::from_rules(vec![Rule {
            key: "ctrl+j".into(),
            command: "refine.sub-agent-tasks".into(),
            when: None,
        }]);
        b.apply(Screen::Refine, &mut acts);
        assert_eq!(acts[0].accel, Accel::ctrl('j'));
    }

    #[test]
    fn the_last_matching_rule_wins() {
        let mut acts = vec![act("sub-agent tasks", Accel::ctrl('g'))];
        let b = Bindings::from_rules(vec![
            Rule {
                key: "ctrl+j".into(),
                command: "refine.sub-agent-tasks".into(),
                when: None,
            },
            Rule {
                key: "ctrl+k".into(),
                command: "refine.sub-agent-tasks".into(),
                when: None,
            },
        ]);
        b.apply(Screen::Refine, &mut acts);
        assert_eq!(
            acts[0].accel,
            Accel::ctrl('k'),
            "a later rule is an exception to an earlier one"
        );
    }

    #[test]
    fn a_when_condition_scopes_a_rule_to_one_screen() {
        let b = Bindings::from_rules(vec![Rule {
            key: "ctrl+j".into(),
            command: "reader.qa".into(),
            when: Some("screen.reader".into()),
        }]);

        let mut on_reader = vec![act("QA", Accel::key('Q'))];
        b.apply(Screen::Reader, &mut on_reader);
        assert_eq!(on_reader[0].accel, Accel::ctrl('j'));

        // The same command id on another screen is left alone.
        let mut elsewhere = vec![act("QA", Accel::key('Q'))];
        b.apply(Screen::Project, &mut elsewhere);
        assert_eq!(elsewhere[0].accel, Accel::key('Q'));
    }

    #[test]
    fn an_unknown_condition_disables_the_rule_rather_than_widening_it() {
        let b = Bindings::from_rules(vec![Rule {
            key: "ctrl+j".into(),
            command: "reader.qa".into(),
            when: Some("mode.vim".into()),
        }]);
        let mut acts = vec![act("QA", Accel::key('Q'))];
        b.apply(Screen::Reader, &mut acts);
        assert_eq!(
            acts[0].accel,
            Accel::key('Q'),
            "a typo must not bind the key everywhere"
        );
    }

    #[test]
    fn an_unparseable_key_leaves_the_default_alone() {
        let b = Bindings::from_rules(vec![Rule {
            key: "ctrl+nonsense".into(),
            command: "reader.qa".into(),
            when: None,
        }]);
        let mut acts = vec![act("QA", Accel::key('Q'))];
        b.apply(Screen::Reader, &mut acts);
        assert_eq!(acts[0].accel, Accel::key('Q'));
    }

    #[test]
    fn no_rules_is_the_cheap_path_and_changes_nothing() {
        let b = Bindings::default();
        assert!(b.is_empty());
        let mut acts = vec![act("QA", Accel::key('Q'))];
        b.apply(Screen::Reader, &mut acts);
        assert_eq!(acts[0].accel, Accel::key('Q'));
    }

    #[test]
    fn every_declared_command_has_a_name_a_user_could_write() {
        use std::collections::HashSet;
        for screen in [
            Screen::Shelf,
            Screen::Project,
            Screen::Translate,
            Screen::Reader,
            Screen::Lexicon,
            Screen::Refine,
        ] {
            let mut seen = HashSet::new();
            for a in crate::app::bindings::commands_for(screen) {
                let id = command_id(screen, &a);
                assert!(!id.ends_with('.'), "{id} has no name to bind");
                assert!(
                    seen.insert(id.clone()),
                    "{id} names two commands on the same screen"
                );
            }
        }
    }
}
