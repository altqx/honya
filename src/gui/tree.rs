//! The workspace tree: projects, volumes, chapters and Refine sessions.
//!
//! The sidebar used to be six buttons that changed which screen was showing.
//! What a workspace wants there instead is the work itself — so this is what
//! you navigate, and the views are what you navigate *to*.

use std::collections::HashMap;

use egui::{RichText, Ui};

use crate::app::{Action, App};
use crate::model::{Chapter, ChapterStatus, Project};

use super::theme_map::GuiPalette;

/// What the tree is pointing at. The inspector reads this, so detail follows
/// the selection rather than following whichever view happens to be showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    Project(String),
    Volume { vol: u32 },
    Chapter { vol: u32, ch: u32 },
    Session(String),
    /// A running or finished sub-agent, picked in the tasks pane.
    Subagent(String),
    /// A lexicon entry, picked on the Lexicon screen. The id is the JP surface,
    /// which is what both kinds are keyed by.
    Character(String),
    Term(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum NodeKey {
    Project(String),
    Volume(String, u32),
}

#[derive(Default)]
pub struct TreeState {
    pub filter: String,
    /// Folds the user has set by hand. Absent means the default, which is that
    /// the open project and its active volume are expanded and nothing else is
    /// — so opening a project shows you its chapters without a second click,
    /// and eight other projects do not bury them.
    folds: HashMap<NodeKey, bool>,
}

impl TreeState {
    fn is_open(&self, key: &NodeKey, default_open: bool) -> bool {
        self.folds.get(key).copied().unwrap_or(default_open)
    }

    fn toggle(&mut self, key: NodeKey, default_open: bool) {
        let now = self.is_open(&key, default_open);
        self.folds.insert(key, !now);
    }

    /// While a filter is live every branch is open: a match you cannot see is
    /// the same as no match.
    fn filtering(&self) -> bool {
        !self.filter.trim().is_empty()
    }
}

/// Does this chapter match the filter? Number or title, case-insensitively.
fn chapter_matches(ch: &Chapter, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    ch.title.to_lowercase().contains(needle)
        || format!("c{}", ch.number).contains(needle)
        || ch.number.to_string() == needle
}

fn project_matches(p: &Project, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    p.title.to_lowercase().contains(needle)
        || p.id.to_lowercase().contains(needle)
        || p.volumes
            .iter()
            .any(|v| v.chapters.iter().any(|c| chapter_matches(c, needle)))
}

/// One clickable row. `depth` indents, `fold` draws a caret when foldable.
///
/// `key` identifies the row by *what it is*, not by where it landed. Folding a
/// volume shifts every row below it into rects other rows just vacated, and
/// egui flags a widget whose id changes at a rect it already drew — with a red
/// outline around it. Keyed ids move with their row, so nothing is flagged.
struct Row {
    depth: usize,
    /// `Some(open)` draws a caret; `None` is a leaf.
    fold: Option<bool>,
    glyph: Option<(char, egui::Color32)>,
    label: RichText,
    selected: bool,
}

fn row(ui: &mut Ui, pal: &GuiPalette, key: impl std::hash::Hash, r: Row) -> (bool, bool) {
    let Row {
        depth,
        fold,
        glyph,
        label,
        selected,
    } = r;
    let mut toggled = false;
    let mut activated = false;
    ui.push_id(key, |ui| {
        ui.horizontal(|ui| {
            ui.add_space(depth as f32 * 12.0);
            match fold {
                Some(open) => {
                    let caret = if open { "▾" } else { "▸" };
                    if ui
                        .add(
                            egui::Button::new(RichText::new(caret).color(pal.ink_faint).small())
                                .frame(false),
                        )
                        .clicked()
                    {
                        toggled = true;
                    }
                }
                None => ui.add_space(14.0),
            }
            if let Some((g, color)) = glyph {
                ui.label(
                    RichText::new(g.to_string())
                        .color(color)
                        .monospace()
                        .small(),
                );
            }
            let resp = ui.add(
                egui::Button::selectable(selected, label)
                    .frame(false)
                    .min_size(egui::vec2(ui.available_width(), 0.0)),
            );
            if resp.clicked() {
                activated = true;
            }
        });
    });
    (toggled, activated)
}

fn section(ui: &mut Ui, pal: &GuiPalette, title: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(title).color(pal.ink_faint).small().strong());
    ui.add_space(2.0);
}

/// Draw the tree and return the actions the user's clicks asked for.
/// Draw the tree. `subject` is what the inspector is about — owned outside,
/// because the tasks pane and the Lexicon set it too and two copies of it
/// would disagree.
pub fn show(
    ui: &mut Ui,
    app: &App,
    st: &mut TreeState,
    subject: &mut Option<Selection>,
    pal: &GuiPalette,
) -> Vec<Action> {
    let mut actions = Vec::new();

    ui.horizontal(|ui| {
        ui.label(RichText::new("⌕").color(pal.ink_faint).small());
        ui.add(
            egui::TextEdit::singleline(&mut st.filter)
                .hint_text("filter")
                .desired_width(ui.available_width()),
        );
    });

    let needle = st.filter.trim().to_lowercase();
    let active_id = app.active.as_ref().map(|a| a.project.id.clone());
    let active_vol = app.active.as_ref().map(|a| a.vol);

    egui::ScrollArea::vertical()
        .id_salt("workspace_tree")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            section(ui, pal, "WORKSPACE");
            if app.projects.is_empty() {
                ui.label(
                    RichText::new("No projects yet — import a source or create the sample.")
                        .color(pal.ink_faint)
                        .small(),
                );
            }

            for project in &app.projects {
                if !project_matches(project, &needle) {
                    continue;
                }
                let is_active = active_id.as_deref() == Some(project.id.as_str());
                let key = NodeKey::Project(project.id.clone());
                let open = st.filtering() || st.is_open(&key, is_active);
                let selected = *subject == Some(Selection::Project(project.id.clone()));
                let title = RichText::new(&project.title)
                    .color(if is_active { pal.ink } else { pal.ink_soft })
                    .strong();
                let (toggled, activated) = row(
                    ui,
                    pal,
                    ("project", project.id.as_str()),
                    Row {
                        depth: 0,
                        fold: Some(open),
                        glyph: None,
                        label: title,
                        selected,
                    },
                );
                if toggled {
                    st.toggle(key.clone(), is_active);
                }
                if activated {
                    *subject = Some(Selection::Project(project.id.clone()));
                    if !is_active {
                        actions.push(Action::OpenProject(project.id.clone()));
                    }
                }
                if !open {
                    continue;
                }

                for vol in &project.volumes {
                    let vkey = NodeKey::Volume(project.id.clone(), vol.number);
                    let vol_is_active = is_active && active_vol == Some(vol.number);
                    let vopen = st.filtering() || st.is_open(&vkey, vol_is_active);
                    let label = match &vol.label {
                        Some(l) if !l.trim().is_empty() => {
                            format!("Vol {} · {l}", vol.number)
                        }
                        _ => format!("Vol {}", vol.number),
                    };
                    let done = vol
                        .chapters
                        .iter()
                        .filter(|c| {
                            matches!(
                                c.status,
                                ChapterStatus::Done
                                    | ChapterStatus::Appended
                                    | ChapterStatus::NeedsReview
                            )
                        })
                        .count();
                    let text = RichText::new(format!("{label}   {done}/{}", vol.chapters.len()))
                        .color(if vol_is_active { pal.ink } else { pal.ink_soft })
                        .small();
                    let selected = *subject == Some(Selection::Volume { vol: vol.number });
                    let (toggled, activated) = row(
                        ui,
                        pal,
                        ("volume", project.id.as_str(), vol.number),
                        Row {
                            depth: 1,
                            fold: Some(vopen),
                            glyph: None,
                            label: text,
                            selected,
                        },
                    );
                    if toggled {
                        st.toggle(vkey.clone(), vol_is_active);
                    }
                    if activated {
                        *subject = Some(Selection::Volume { vol: vol.number });
                        if !is_active {
                            actions.push(Action::OpenProject(project.id.clone()));
                        }
                        actions.push(Action::SetActiveVolume { vol: vol.number });
                    }
                    if !vopen {
                        continue;
                    }

                    for ch in &vol.chapters {
                        if !chapter_matches(ch, &needle) {
                            continue;
                        }
                        let (glyph, color) =
                            crate::theme::status_glyph(ch.kind, ch.status, &app.theme);
                        let title = if ch.title.trim().is_empty() {
                            format!("c{}", ch.number)
                        } else {
                            format!("c{}  {}", ch.number, ch.title.trim())
                        };
                        let selected = *subject
                            == Some(Selection::Chapter {
                                vol: vol.number,
                                ch: ch.number,
                            });
                        let (_, activated) = row(
                            ui,
                            pal,
                            ("chapter", project.id.as_str(), vol.number, ch.number),
                            Row {
                                depth: 2,
                                fold: None,
                                glyph: Some((glyph, super::theme_map::color(color))),
                                label: RichText::new(title).color(pal.ink_soft).small(),
                                selected,
                            },
                        );
                        if activated {
                            *subject = Some(Selection::Chapter {
                                vol: vol.number,
                                ch: ch.number,
                            });
                            if !is_active {
                                actions.push(Action::OpenProject(project.id.clone()));
                            }
                            // The Reader resolves against the active volume, so
                            // the volume has to land before the chapter does.
                            actions.push(Action::SetActiveVolume { vol: vol.number });
                            actions.push(Action::OpenChapter { chapter: ch.number });
                        }
                    }
                }
            }

            if !app.refine_sessions.is_empty() {
                section(ui, pal, "SESSIONS");
                for s in &app.refine_sessions {
                    let title = if s.title.trim().is_empty() {
                        s.id.clone()
                    } else {
                        s.title.clone()
                    };
                    if !needle.is_empty() && !title.to_lowercase().contains(&needle) {
                        continue;
                    }
                    let live = app.refine_session_id == s.id;
                    let selected = *subject == Some(Selection::Session(s.id.clone()));
                    let (_, activated) = row(
                        ui,
                        pal,
                        ("session", s.id.as_str()),
                        Row {
                            depth: 0,
                            fold: None,
                            glyph: Some((if live { '●' } else { '·' }, pal.accent)),
                            label: RichText::new(title)
                                .color(if live { pal.ink } else { pal.ink_soft })
                                .small(),
                            selected,
                        },
                    );
                    if activated {
                        *subject = Some(Selection::Session(s.id.clone()));
                        if !live {
                            actions.push(Action::RefineSwitchSession { id: s.id.clone() });
                        }
                        actions.push(Action::Goto(crate::app::Screen::Refine));
                    }
                }
            }
        });

    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ChapterKind;

    fn chapter(n: u32, title: &str) -> Chapter {
        Chapter {
            number: n,
            title: title.to_string(),
            kind: ChapterKind::Prose,
            status: ChapterStatus::Pending,
            source_segments: 0,
            total_chunks: 0,
            committed_chunks: 0,
            skipped_chunks: 0,
            last_run: None,
            usage: Default::default(),
        }
    }

    #[test]
    fn the_filter_reaches_a_chapter_by_number_or_by_title() {
        let ch = chapter(12, "第十二話 影の庭");
        assert!(chapter_matches(&ch, ""));
        assert!(chapter_matches(&ch, "c12"));
        assert!(chapter_matches(&ch, "12"));
        assert!(chapter_matches(&ch, "影"));
        assert!(!chapter_matches(&ch, "c13"));
    }

    #[test]
    fn a_project_matches_when_one_of_its_chapters_does() {
        let mut p = Project {
            id: "novel".into(),
            dir: std::path::PathBuf::from("/tmp/x"),
            title: "銀の庭".into(),
            translated_title: String::new(),
            target_language: crate::model::TargetLanguage::Thai,
            created: None,
            touched: None,
            volumes: vec![crate::model::Volume {
                number: 1,
                dir: std::path::PathBuf::from("/tmp/x/Vol_01"),
                label: None,
                chapters: vec![chapter(3, "序章")],
            }],
            models: None,
        };
        assert!(project_matches(&p, "序"), "reached through its chapters");
        assert!(project_matches(&p, "銀"), "and by its own title");
        assert!(!project_matches(&p, "zzz"));
        p.volumes.clear();
        assert!(!project_matches(&p, "序"));
    }

    #[test]
    fn folds_default_to_the_open_project_and_leave_the_rest_shut() {
        let st = TreeState::default();
        let active = NodeKey::Project("novel".into());
        let other = NodeKey::Project("other".into());
        assert!(st.is_open(&active, true), "the open project shows its work");
        assert!(!st.is_open(&other, false), "eight others do not bury it");
    }

    #[test]
    fn a_hand_fold_outlives_the_default() {
        let mut st = TreeState::default();
        let key = NodeKey::Project("novel".into());
        st.toggle(key.clone(), true);
        assert!(!st.is_open(&key, true), "closing the active project sticks");
        st.toggle(key.clone(), true);
        assert!(st.is_open(&key, true));
    }

    #[test]
    fn a_live_filter_opens_everything() {
        let mut st = TreeState::default();
        assert!(!st.filtering());
        st.filter = "  ".into();
        assert!(!st.filtering(), "whitespace is not a filter");
        st.filter = "ch12".into();
        assert!(st.filtering());
    }
}
