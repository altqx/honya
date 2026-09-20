//! The Shelf (1 書架): home / project picker; `i` launches the import wizard, `Enter` opens a project.

use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::ListState;

use crate::model::{ChapterKind, ChapterStatus, Project};
use crate::theme::{Theme, status_glyph};
use crate::ui::mouse::{MouseGesture, MouseInput};
use crate::ui::text::{col_width, pad_to_cols, thai_display_safe, truncate_cols};

use super::action_table::{self, Act};
use super::Action;
use super::overlay::Overlay;

/// Selection covers `projects.len()` project rows plus the trailing import row.
pub struct ShelfScreen {
    list: ListState,
    unimported: Vec<(PathBuf, u64)>,
    /// The list's drawn rectangle, captured each frame for mouse hit-testing.
    list_area: Rect,
}

/// One row of the shelf list. Only projects and the import row are selectable;
/// the rule and the loose files below it are there to be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShelfRow {
    Project(usize),
    Rule,
    Import,
    File(usize),
}

/// Action ids for this screen's table. Stable within the screen: they are also
/// the zone index every one of its controls registers under.
const S_IMPORT: u16 = 0;
const S_RESCAN: u16 = 1;
const S_OPEN: u16 = 2;
const S_RENAME: u16 = 3;
const S_DELETE: u16 = 4;

impl ShelfScreen {
    pub fn new() -> Self {
        let mut list = ListState::default();
        list.select(Some(0));
        Self {
            list,
            unimported: Vec::new(),
            list_area: Rect::default(),
        }
    }

    /// Re-discover importable source files in the working root.
    pub fn rescan(&mut self, root: &std::path::Path) {
        self.unimported = crate::workspace::scan::find_importable_files(root);
    }

    pub fn select_first(&mut self) {
        self.list.select(Some(0));
    }

    /// Total selectable rows: every project plus the trailing import row.
    fn row_count(&self, projects: &[Project]) -> usize {
        projects.len() + 1
    }

    fn import_row_index(&self, projects: &[Project]) -> usize {
        projects.len()
    }

    fn import_files(&self) -> Vec<(PathBuf, u64)> {
        self.unimported.clone()
    }

    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        projects: &[Project],
        preferred_language: crate::model::TargetLanguage,
    ) -> Action {
        // Commands come from the table; only navigation is left here.
        let acts = self.actions(projects);
        match action_table::hit(&acts, &key) {
            action_table::KeyHit::Run(id) => {
                return self
                    .run(id, projects, preferred_language)
                    .unwrap_or(Action::None);
            }
            action_table::KeyHit::Blocked => return Action::None,
            action_table::KeyHit::Miss => {}
        }

        let rows = self.row_count(projects);
        let sel = self
            .list
            .selected()
            .unwrap_or(0)
            .min(rows.saturating_sub(1));
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                let next = if sel == 0 {
                    rows.saturating_sub(1)
                } else {
                    sel - 1
                };
                self.list.select(Some(next));
                Action::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let next = if rows == 0 { 0 } else { (sel + 1) % rows };
                self.list.select(Some(next));
                Action::None
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.list.select(Some(0));
                Action::None
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.list.select(Some(rows.saturating_sub(1)));
                Action::None
            }
            _ => Action::None,
        }
    }

    /// Mouse: the wheel moves the cursor; a click selects the row under it; a
    /// double-click (or a click on the already-selected row) opens it — opening a
    /// project or, on the import row, the import wizard.
    pub fn handle_mouse(
        &mut self,
        m: MouseInput,
        zone: Option<crate::ui::kit::ZoneId>,
        projects: &[Project],
        preferred_language: crate::model::TargetLanguage,
    ) -> Action {
        match m.gesture {
            MouseGesture::ScrollUp => {
                self.select_delta(projects, -1);
                Action::None
            }
            MouseGesture::ScrollDown => {
                self.select_delta(projects, 1);
                Action::None
            }
            MouseGesture::Click { double } => {
                // The clicked row comes from the registry the list wrote while
                // drawing, so there is no second copy of the row layout here to
                // fall out of step with it.
                let Some(row) = zone.and_then(|z| z.row_index()) else {
                    return Action::None;
                };
                let Some(target) = self.selection_at_row(projects, row) else {
                    return Action::None;
                };
                let import_idx = self.import_row_index(projects);
                let already = self.list.selected() == Some(target);
                self.list.select(Some(target));
                if double || already {
                    if target == import_idx {
                        return Action::show_overlay(Overlay::import(
                            self.import_files(),
                            projects,
                            preferred_language,
                        ));
                    }
                    if let Some(p) = projects.get(target) {
                        return Action::OpenProject(p.id.clone());
                    }
                }
                Action::None
            }
            // The router opens this row's menu straight after, so the
            // selection has to be on the row the menu is about.
            MouseGesture::RightClick => {
                if let Some(target) = zone
                    .and_then(|z| z.row_index())
                    .and_then(|row| self.selection_at_row(projects, row))
                {
                    self.list.select(Some(target));
                }
                Action::None
            }
        }
    }

    /// Move the cursor by `delta`, clamped (no wrap — scroll shouldn't loop).
    fn select_delta(&mut self, projects: &[Project], delta: i32) {
        let rows = self.row_count(projects);
        if rows == 0 {
            return;
        }
        let cur = self.list.selected().unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, rows as i32 - 1) as usize;
        self.list.select(Some(next));
    }

    /// The rows this shelf shows, in screen order.
    ///
    /// Shared with the click handler: the list interleaves a rule and the
    /// loose source files among the selectable rows, so a clicked row index is
    /// not a selection index, and the mapping is only right while both sides
    /// agree about where the extras went.
    pub(crate) fn rows(&self, projects: &[Project]) -> Vec<ShelfRow> {
        let mut rows: Vec<ShelfRow> = (0..projects.len()).map(ShelfRow::Project).collect();
        rows.push(ShelfRow::Rule);
        rows.push(ShelfRow::Import);
        rows.extend((0..self.unimported.len()).map(ShelfRow::File));
        rows
    }

    /// The selection index a clicked row stands for, if it is selectable.
    pub(crate) fn selection_at_row(&self, projects: &[Project], row: usize) -> Option<usize> {
        match self.rows(projects).get(row)? {
            ShelfRow::Project(i) => Some(*i),
            // A loose file is not its own destination — clicking one is a
            // request to import it, which is the row above.
            ShelfRow::Import | ShelfRow::File(_) => Some(self.import_row_index(projects)),
            ShelfRow::Rule => None,
        }
    }

    pub fn render(
        &mut self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        projects: &[Project],
        foreign_busy: &[std::path::PathBuf],
    ) {
        use crate::ui::kit::list::{self, ListState, Row};
        use crate::ui::kit::toolbar::{RowActions, Toolbar};
        use crate::ui::kit::{ZoneId, ZoneKind, ctx::row_at};

        let count = self.row_count(projects);
        if self.list.selected().is_none_or(|s| s >= count) {
            self.list.select(Some(count.saturating_sub(1)));
        }
        ui.fill(area, Style::default().bg(ui.theme.bg));
        if area.height < 2 {
            return;
        }

        let summary = format!(
            "./  ({} project{} · {} source file{})",
            projects.len(),
            plural(projects.len()),
            self.unimported.len(),
            plural(self.unimported.len()),
        );
        ui.line(
            row_at(area, 0),
            Line::from(vec![
                Span::styled(
                    "  書架 ",
                    Style::default()
                        .fg(ui.theme.ink)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("— your shelf", Style::default().fg(ui.theme.ink_soft)),
            ]),
            Style::default().bg(ui.theme.bg),
        );
        let cw = col_width(&summary) as u16;
        if area.width > cw + 2 {
            ui.text(
                Rect {
                    x: area.x + area.width - cw - 1,
                    y: area.y,
                    width: cw,
                    height: 1,
                },
                summary,
                Style::default().fg(ui.theme.ink_faint).bg(ui.theme.bg),
            );
        }

        // The row between the title and the list was already blank, so the
        // toolbar costs this screen nothing.
        let acts = self.actions(projects);
        Toolbar::new(&acts).render(
            ui,
            Rect {
                x: area.x + 2,
                y: area.y + 1,
                width: area.width.saturating_sub(3),
                height: 1,
            },
        );

        let list_area = Rect {
            x: area.x,
            y: area.y + 2,
            width: area.width,
            height: area.height.saturating_sub(2),
        };
        self.list_area = list_area;

        let rows = self.rows(projects);
        let import_idx = self.import_row_index(projects);
        let selected_row = self.list.selected().and_then(|sel| {
            rows.iter().position(|r| match r {
                ShelfRow::Project(i) => *i == sel,
                ShelfRow::Import => sel == import_idx,
                _ => false,
            })
        });

        // Built before the list borrows `ui`: the row closure only needs
        // strings and colours, not the render context.
        let name_w = list_area.width.saturating_sub(48).max(20) as usize;
        let rule_color = ui.theme.rule;
        let accent = ui.theme.accent;
        let dim = Style::default().fg(ui.theme.ink_faint);
        let cells: Vec<ProjectCells> = projects
            .iter()
            .map(|p| {
                let busy = foreign_busy.iter().any(|d| {
                    crate::workspace::session::same_project_dir(d.as_path(), p.dir.as_path())
                });
                project_cells(p, name_w, busy, ui.theme)
            })
            .collect();
        let files: Vec<(String, String)> = self
            .unimported
            .iter()
            .map(|(path, size)| {
                (
                    thai_display_safe(path.file_name().and_then(|s| s.to_str()).unwrap_or("?")),
                    human_size(*size),
                )
            })
            .collect();
        let import_note = format!(
            "{} importable file{} in this folder",
            self.unimported.len(),
            plural(self.unimported.len())
        );
        let list_w = list_area.width;

        let mut state = ListState::new();
        state.select(selected_row);
        list::render(
            ui,
            list_area,
            &mut state,
            rows.len(),
            list::Opts {
                rail: true,
                scrollbar: true,
                kind: ZoneKind::Row,
                id_base: 0,
            },
            |i| match &rows[i] {
                ShelfRow::Project(n) => Row::new(cells[*n].line()),
                ShelfRow::Rule => Row::header(Line::from(Span::styled(
                    format!("   {}", "┄".repeat(list_w.saturating_sub(5) as usize)),
                    Style::default().fg(rule_color),
                ))),
                ShelfRow::Import => {
                    let label = "＋ Import file …";
                    let used = col_width(label) + 3;
                    let note_w = col_width(&import_note);
                    let mut spans = vec![Span::styled(
                        label.to_string(),
                        Style::default().fg(accent).add_modifier(Modifier::BOLD),
                    )];
                    if (list_w as usize) > used + note_w + 4 {
                        spans.push(Span::raw(" ".repeat(list_w as usize - used - note_w - 2)));
                        spans.push(Span::styled(import_note.clone(), dim));
                    }
                    Row::new(Line::from(spans))
                }
                ShelfRow::File(n) => {
                    let (name, size) = &files[*n];
                    Row::header(Line::from(vec![
                        Span::styled(format!("   {name}"), dim),
                        Span::styled(format!("   {size}"), dim),
                    ]))
                }
            },
        );

        // The selected row's own verbs, drawn over the right end of the row the
        // list just registered — so the buttons act on the same row the keys
        // do, and their geometry comes from the registry rather than a second
        // copy of the row layout.
        if let Some(i) = selected_row
            && matches!(rows.get(i), Some(ShelfRow::Project(_)))
            && let Some(rect) = ui.zones.rect_of(ZoneId::row(i))
        {
            RowActions::new(&acts).render(ui, rect);
        }
    }

    /// This screen's commands, availability resolved for this frame. Drawn as
    /// the toolbar row and the selected row's buttons.
    pub fn actions(&self, projects: &[Project]) -> Vec<Act> {
        use action_table::Accel;

        let sel = self.list.selected().unwrap_or(0);
        let on_project = projects.get(sel).is_some();
        vec![
            Act::toolbar(S_IMPORT, "import", Accel::key('i')),
            Act::toolbar(S_RESCAN, "rescan", Accel::key('r')),
            Act::row(S_OPEN, "open", Accel::code(KeyCode::Enter)),
            Act::row(S_RENAME, "rename", Accel::key('R')).when(on_project),
            Act::row(S_DELETE, "delete", Accel::key('d')).when(on_project),
        ]
    }

    /// Run the action `id` stands for, however it was reached. `None` means
    /// no such action here.
    pub fn run(
        &mut self,
        id: u16,
        projects: &[Project],
        preferred_language: crate::model::TargetLanguage,
    ) -> Option<Action> {
        let sel = self
            .list
            .selected()
            .unwrap_or(0)
            .min(self.row_count(projects).saturating_sub(1));
        let import = |screen: &Self| {
            Action::show_overlay(Overlay::import(
                screen.import_files(),
                projects,
                preferred_language,
            ))
        };
        Some(match id {
            S_IMPORT => import(self),
            S_RESCAN => {
                // Project rescan is the App's job; here we only rescan local
                // source files.
                self.rescan(&working_root());
                Action::None
            }
            S_OPEN => {
                if sel == self.import_row_index(projects) {
                    import(self)
                } else if let Some(p) = projects.get(sel) {
                    Action::OpenProject(p.id.clone())
                } else {
                    Action::None
                }
            }
            S_RENAME => match projects.get(sel) {
                Some(p) => Action::show_overlay(Overlay::project_title_edit(
                    p.id.clone(),
                    p.title.clone(),
                    p.translated_title.clone(),
                    p.target_language,
                )),
                None => Action::None,
            },
            S_DELETE => match projects.get(sel) {
                Some(p) => Action::show_overlay(Overlay::confirm(
                    "Delete project?",
                    format!(
                        "Permanently delete “{}” and ALL its files (raw, translations, glossary, characters)? This cannot be undone.",
                        p.title
                    ),
                    Action::DeleteProject { id: p.id.clone() },
                )),
                None => Action::None,
            },
            _ => return None,
        })
    }

    /// Navigation only. Every command this screen has is a control now — a
    /// toolbar button or a button on the selected row — so the footer no longer
    /// restates them.
    pub fn hints(&self) -> &'static [(&'static str, &'static str)] {
        &[("↑↓", "move"), ("Home/End", "ends")]
    }
}

impl Default for ShelfScreen {
    fn default() -> Self {
        Self::new()
    }
}

/// A project row's pieces. Selection styling is the list's job now, so nothing
/// here knows whether the row is selected — which is what lets the cells be
/// built outside the render closure, where borrowing the palette would clash
/// with the list's own borrow.
pub(crate) struct ProjectCells {
    glyph: String,
    glyph_color: ratatui::style::Color,
    name: String,
    vol: String,
    tally: String,
    touched: String,
    soft: ratatui::style::Color,
    faint: ratatui::style::Color,
}

impl ProjectCells {
    fn line(&self) -> Line<'static> {
        Line::from(vec![
            Span::styled(self.glyph.clone(), Style::default().fg(self.glyph_color)),
            Span::raw("  "),
            Span::raw(self.name.clone()),
            Span::styled(self.vol.clone(), Style::default().fg(self.soft)),
            Span::styled(self.tally.clone(), Style::default().fg(self.soft)),
            Span::styled(self.touched.clone(), Style::default().fg(self.faint)),
        ])
    }
}

fn project_cells(p: &Project, name_w: usize, foreign_busy: bool, theme: &Theme) -> ProjectCells {
    let (glyph, glyph_color) = if foreign_busy {
        ('↻', theme.status_working)
    } else {
        overall_glyph(p, theme)
    };
    let tally = tally_of(p);
    let name_src = if p.translated_title.trim().is_empty() {
        p.title.clone()
    } else {
        format!("{} · {}", p.title, p.translated_title)
    };
    ProjectCells {
        glyph: glyph.to_string(),
        glyph_color,
        name: pad_to_cols(&truncate_cols(&thai_display_safe(&name_src), name_w), name_w),
        vol: format!("Vol.{}  ", p.volumes.first().map(|v| v.number).unwrap_or(1)),
        tally: format!("●{} ◐{} ○{} ✗{}  ", tally.0, tally.1, tally.2, tally.3),
        touched: if foreign_busy {
            "running elsewhere".to_string()
        } else {
            touched_label(p)
        },
        soft: theme.ink_soft,
        faint: theme.ink_faint,
    }
}

/// Project glyph: the most urgent state wins.
fn overall_glyph(p: &Project, theme: &Theme) -> (char, ratatui::style::Color) {
    let mut any_working = false;
    let mut any_failed = false;
    let mut any_needs_review = false;
    let mut any_pending = false;
    let mut all_done = true;
    let mut any = false;
    for vol in &p.volumes {
        for ch in &vol.chapters {
            any = true;
            match ch.status {
                ChapterStatus::Failed => any_failed = true,
                ChapterStatus::NeedsReview => any_needs_review = true,
                s if s.is_active() || s == ChapterStatus::Paused => any_working = true,
                ChapterStatus::Done | ChapterStatus::Appended => {}
                _ => any_pending = true,
            }
            // NeedsReview is written content, so it doesn't keep a project looking
            // perpetually incomplete — it surfaces via the warn glyph below.
            if !matches!(
                ch.status,
                ChapterStatus::Done | ChapterStatus::Appended | ChapterStatus::NeedsReview
            ) {
                all_done = false;
            }
        }
    }
    let status = if !any {
        ChapterStatus::Pending
    } else if any_failed {
        ChapterStatus::Failed
    } else if any_working {
        ChapterStatus::Translating
    } else if any_needs_review {
        ChapterStatus::NeedsReview
    } else if all_done {
        ChapterStatus::Done
    } else if any_pending {
        ChapterStatus::Pending
    } else {
        ChapterStatus::Done
    };
    status_glyph(ChapterKind::Prose, status, theme)
}

/// (done, working, pending, failed) tally across all volumes.
fn tally_of(p: &Project) -> (u32, u32, u32, u32) {
    let mut done = 0;
    let mut working = 0;
    let mut pending = 0;
    let mut failed = 0;
    for vol in &p.volumes {
        for ch in &vol.chapters {
            match ch.status {
                ChapterStatus::Done | ChapterStatus::Appended | ChapterStatus::NeedsReview => {
                    done += 1
                }
                ChapterStatus::Failed => failed += 1,
                s if s.is_active() || s == ChapterStatus::Paused => working += 1,
                _ => pending += 1,
            }
        }
    }
    (done, working, pending, failed)
}

fn touched_label(p: &Project) -> String {
    match p.touched {
        Some(t) => {
            let now = chrono::Utc::now();
            let delta = now.signed_duration_since(t);
            let secs = delta.num_seconds().max(0);
            let human = if secs < 60 {
                "just now".to_string()
            } else if secs < 3600 {
                format!("{}m ago", secs / 60)
            } else if secs < 86_400 {
                format!("{}h ago", secs / 3600)
            } else if secs < 172_800 {
                "yesterday".to_string()
            } else {
                format!("{}d ago", secs / 86_400)
            };
            format!("touched {human}")
        }
        None => String::new(),
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

pub(crate) fn human_size(bytes: u64) -> String {
    let mb = bytes as f64 / 1_048_576.0;
    if mb >= 1.0 {
        format!("{mb:.1} MB")
    } else {
        let kb = bytes as f64 / 1024.0;
        format!("{kb:.0} KB")
    }
}

fn working_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::mouse::{MouseGesture, MouseInput};

    fn proj(id: &str) -> Project {
        Project {
            id: id.to_string(),
            dir: std::env::temp_dir().join(id),
            title: id.to_string(),
            translated_title: String::new(),
            target_language: crate::model::TargetLanguage::Thai,
            created: None,
            touched: None,
            volumes: Vec::new(),
            models: None,
        }
    }

    fn click(double: bool, col: u16, row: u16) -> MouseInput {
        MouseInput {
            gesture: MouseGesture::Click { double },
            col,
            row,
        }
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(
            KeyCode::Char(c),
            ratatui::crossterm::event::KeyModifiers::NONE,
        )
    }

    /// Every id the table declares has a `run` arm. Without this a screen can
    /// advertise a key that falls through to nothing, which is exactly how
    /// Project ended up printing `Q QA` for an action that did not exist.
    #[test]
    fn every_declared_action_has_a_handler() {
        let projects = vec![proj("alpha")];
        let mut screen = ShelfScreen::new();
        for act in screen.actions(&projects) {
            assert!(
                screen
                    .run(act.id, &projects, crate::model::TargetLanguage::Thai)
                    .is_some(),
                "{} ({}) is advertised with no handler",
                act.label,
                act.accel.shown()
            );
        }
    }

    /// Pressing an action's accelerator and running it by id are the same
    /// thing, because the key router looks the key up in this very table.
    #[test]
    fn a_key_and_its_control_reach_the_same_action() {
        let projects = vec![proj("alpha")];
        let lang = crate::model::TargetLanguage::Thai;
        let mut a = ShelfScreen::new();
        a.list.select(Some(0));
        let mut b = ShelfScreen::new();
        b.list.select(Some(0));

        let by_key = a.handle_key(key('d'), &projects, lang);
        let by_id = b.run(S_DELETE, &projects, lang).unwrap();
        assert_eq!(
            format!("{by_key:?}"),
            format!("{by_id:?}"),
            "the key and the control must produce the same action"
        );
    }

    /// Everything the screen declares is reachable without the keyboard: drawn
    /// as a control, or listed in the row's context menu.
    #[test]
    fn nothing_is_keyboard_only() {
        let projects = vec![proj("alpha")];
        let mut screen = ShelfScreen::new();
        screen.list.select(Some(0));
        let acts = screen.actions(&projects);
        let (_, zones) = crate::ui::kit::ctx::draw_test(100, 24, |ui, area| {
            screen.render(ui, area, &projects, &[]);
        });
        for act in &acts {
            assert!(
                act.placement != action_table::Placement::Menu,
                "{} would be reachable only from the menu",
                act.label
            );
            assert!(
                zones.contains(act.zone()),
                "{} is declared but nothing drew a control for it",
                act.label
            );
        }
    }

    /// A single click selects the row under the pointer; clicking the (now)
    /// selected row again opens the project.
    #[test]
    fn click_selects_then_opens() {
        let projects = vec![proj("alpha"), proj("beta")];
        let mut s = ShelfScreen::new();
        let (_, zones) = crate::ui::kit::ctx::draw_test(90, 20, |ui, area| {
            s.render(ui, area, &projects, &[])
        });
        // Ask the registry where the second project row landed rather than
        // working it out from the list area a second time.
        let rect = zones
            .rect_of(crate::ui::kit::ZoneId::row(1))
            .expect("second project row");
        let at = (rect.x + 4, rect.y);
        let zone = zones.at(at.0, at.1);

        // Row 0 starts selected; clicking another row selects it, no open.
        let a = s.handle_mouse(
            click(false, at.0, at.1),
            zone,
            &projects,
            crate::model::TargetLanguage::Thai,
        );
        assert!(matches!(a, Action::None));
        assert_eq!(s.list.selected(), Some(1));

        // Clicking the already-selected project opens it.
        match s.handle_mouse(
            click(false, at.0, at.1),
            zone,
            &projects,
            crate::model::TargetLanguage::Thai,
        ) {
            Action::OpenProject(id) => assert_eq!(id, "beta"),
            other => panic!("expected OpenProject, got {other:?}"),
        }
    }

    /// The wheel walks the selection (clamped, no wrap past the import row).
    #[test]
    fn wheel_moves_selection_clamped() {
        let projects = vec![proj("alpha"), proj("beta")];
        let mut s = ShelfScreen::new(); // rows = 2 projects + import = 3
        s.handle_mouse(
            MouseInput {
                gesture: MouseGesture::ScrollUp,
                col: 0,
                row: 0,
            },
            None,
            &projects,
            crate::model::TargetLanguage::Thai,
        );
        assert_eq!(s.list.selected(), Some(0), "scroll up at the top stays put");
        for _ in 0..5 {
            s.handle_mouse(
                MouseInput {
                    gesture: MouseGesture::ScrollDown,
                    col: 0,
                    row: 0,
                },
                None,
                &projects,
                crate::model::TargetLanguage::Thai,
            );
        }
        assert_eq!(s.list.selected(), Some(2), "clamps at the import row");
    }
}
