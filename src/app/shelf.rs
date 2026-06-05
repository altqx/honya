//! The Shelf (1 書架): home / project picker; `i` launches the import wizard, `Enter` opens a project.

use std::path::PathBuf;

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::model::{ChapterKind, ChapterStatus, Project};
use crate::theme::{self, Theme, status_glyph};
use crate::ui::text::{col_width, pad_to_cols, truncate_cols};

use super::Action;
use super::overlay::Overlay;

/// Selection covers `projects.len()` project rows plus the trailing import row.
pub struct ShelfScreen {
    list: ListState,
    unimported: Vec<(PathBuf, u64)>,
}

impl ShelfScreen {
    pub fn new() -> Self {
        let mut list = ListState::default();
        list.select(Some(0));
        Self {
            list,
            unimported: Vec::new(),
        }
    }

    /// Re-discover unimported epubs in the working root.
    pub fn rescan(&mut self, root: &std::path::Path) {
        self.unimported = crate::workspace::scan::find_unimported_epubs(root);
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

    fn epub_paths(&self) -> Vec<PathBuf> {
        self.unimported.iter().map(|(p, _)| p.clone()).collect()
    }

    pub fn handle_key(&mut self, key: KeyEvent, projects: &[Project]) -> Action {
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
            KeyCode::Enter => {
                if sel == self.import_row_index(projects) {
                    Action::show_overlay(Overlay::import(self.epub_paths()))
                } else if let Some(p) = projects.get(sel) {
                    Action::OpenProject(p.id.clone())
                } else {
                    Action::None
                }
            }
            KeyCode::Char('i') => Action::show_overlay(Overlay::import(self.epub_paths())),
            KeyCode::Char('d') => {
                if let Some(p) = projects.get(sel) {
                    Action::show_overlay(Overlay::confirm(
                        "Delete project?",
                        format!("This removes {} from the shelf list only.", p.title),
                        Action::None,
                    ))
                } else {
                    Action::None
                }
            }
            KeyCode::Char('R') => {
                if let Some(p) = projects.get(sel) {
                    Action::show_overlay(Overlay::confirm(
                        "Rename",
                        format!("Rename {} — edit PROJECT.md title on disk.", p.title),
                        Action::None,
                    ))
                } else {
                    Action::None
                }
            }
            KeyCode::Char('r') => {
                // Project rescan is the App's job; here we only rescan local epubs.
                self.rescan(&working_root());
                Action::None
            }
            _ => Action::None,
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, projects: &[Project], theme: &Theme) {
        let rows = self.row_count(projects);
        if self.list.selected().is_none_or(|s| s >= rows) {
            self.list.select(Some(rows.saturating_sub(1)));
        }

        let block = Block::default()
            .borders(Borders::NONE)
            .style(Style::default().bg(theme.bg));
        let inner = block.inner(area);
        f.render_widget(block, area);

        if inner.height < 2 {
            return;
        }

        let title = Line::from(vec![
            Span::styled(
                "  書架 ",
                Style::default().fg(theme.ink).add_modifier(Modifier::BOLD),
            ),
            Span::styled("— your shelf", Style::default().fg(theme.ink_soft)),
        ]);
        let count = format!(
            "./  ({} project{} · {} epub{})",
            projects.len(),
            plural(projects.len()),
            self.unimported.len(),
            plural(self.unimported.len()),
        );
        f.render_widget(
            Paragraph::new(title).style(Style::default().bg(theme.bg)),
            Rect {
                x: inner.x,
                y: inner.y,
                width: inner.width,
                height: 1,
            },
        );
        let cw = col_width(&count) as u16;
        if inner.width > cw + 2 {
            f.render_widget(
                Paragraph::new(Span::styled(count, Style::default().fg(theme.ink_faint)))
                    .style(Style::default().bg(theme.bg)),
                Rect {
                    x: inner.x + inner.width - cw - 1,
                    y: inner.y,
                    width: cw,
                    height: 1,
                },
            );
        }

        let list_area = Rect {
            x: inner.x,
            y: inner.y + 2,
            width: inner.width,
            height: inner.height.saturating_sub(2),
        };

        let selected = self.list.selected().unwrap_or(0);
        let mut items: Vec<ListItem> = Vec::new();
        let name_w = list_area.width.saturating_sub(48).max(20) as usize;

        for (i, p) in projects.iter().enumerate() {
            items.push(project_row(p, i == selected, name_w, theme));
        }

        // Separator is folded into the import ListItem (not standalone) so the
        // ListState index maps 1:1 to projects.len().
        let separator_line = Line::from(Span::styled(
            format!(
                "     {}",
                "┄".repeat(list_area.width.saturating_sub(6) as usize)
            ),
            Style::default().fg(theme.rule),
        ));

        let import_selected = selected == self.import_row_index(projects);
        let import_bar = if import_selected {
            theme::SELECT_BAR.to_string()
        } else {
            " ".to_string()
        };
        let import_style = if import_selected {
            Style::default()
                .fg(theme.accent)
                .bg(theme.accent_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.accent)
        };
        let epub_note = format!("{} unimported .epub in this folder", self.unimported.len());
        let mut import_line = vec![
            Span::styled(format!(" {import_bar} "), Style::default().fg(theme.accent)),
            Span::styled("＋ Import EPUB …", import_style),
        ];
        let used = col_width("  ＋ Import EPUB …") + 2;
        let note_w = col_width(&epub_note);
        if (list_area.width as usize) > used + note_w + 4 {
            let gap = list_area.width as usize - used - note_w - 2;
            import_line.push(Span::raw(" ".repeat(gap)));
            import_line.push(Span::styled(
                epub_note,
                Style::default().fg(theme.ink_faint),
            ));
        }
        items.push(ListItem::new(ratatui::text::Text::from(vec![
            separator_line,
            Line::from(import_line),
        ])));

        for (path, size) in &self.unimported {
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("?");
            let size_h = human_size(*size);
            let label = format!("        · {}", pad_to_cols(name, 28),);
            items.push(ListItem::new(Line::from(vec![
                Span::styled(label, Style::default().fg(theme.ink_soft)),
                Span::styled(size_h, Style::default().fg(theme.ink_faint)),
            ])));
        }

        let list = List::new(items).style(Style::default().bg(theme.bg));
        // Selection visuals are baked into the rows; state is kept only for scrolling.
        f.render_stateful_widget(list, list_area, &mut self.list);
    }

    pub fn hints(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("↵", "open"),
            ("i", "import"),
            ("d", "delete"),
            ("R", "rename"),
            ("r", "rescan"),
        ]
    }
}

impl Default for ShelfScreen {
    fn default() -> Self {
        Self::new()
    }
}

fn project_row(p: &Project, selected: bool, name_w: usize, theme: &Theme) -> ListItem<'static> {
    let (glyph, gcolor) = overall_glyph(p, theme);
    let tally = tally_of(p);
    let touched = touched_label(p);
    let vol = p.volumes.first().map(|v| v.number).unwrap_or(1);

    let bar = if selected {
        theme::SELECT_BAR.to_string()
    } else {
        " ".to_string()
    };
    let row_bg = if selected { theme.accent_bg } else { theme.bg };
    let name_style = if selected {
        Style::default()
            .fg(theme.ink)
            .bg(row_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.ink).bg(row_bg)
    };

    let name = truncate_cols(&p.title, name_w);
    let name_padded = pad_to_cols(&name, name_w);

    let spans = vec![
        Span::styled(
            format!(" {bar} "),
            Style::default().fg(theme.accent).bg(row_bg),
        ),
        Span::styled(glyph.to_string(), Style::default().fg(gcolor).bg(row_bg)),
        Span::styled("  ", Style::default().bg(row_bg)),
        Span::styled(name_padded, name_style),
        Span::styled(
            format!("Vol.{vol}  "),
            Style::default().fg(theme.ink_soft).bg(row_bg),
        ),
        Span::styled(
            format!("●{} ◐{} ○{} ✗{}  ", tally.0, tally.1, tally.2, tally.3),
            Style::default().fg(theme.ink_soft).bg(row_bg),
        ),
        Span::styled(touched, Style::default().fg(theme.ink_faint).bg(row_bg)),
    ];
    ListItem::new(Line::from(spans))
}

/// Project-level glyph: the "least finished" interesting state wins (failed > working > pending > done).
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

fn human_size(bytes: u64) -> String {
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
