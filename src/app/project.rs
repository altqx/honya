//! The Project Dashboard (2 棚): chapter tree + context files + detail card; launches runs (`t`/`T`).

use std::collections::HashSet;

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::model::{Chapter, ChapterKind, ChapterStatus, Volume};
use crate::theme::{self, Theme, status_glyph};
use crate::ui::text::{col_width, pad_to_cols, truncate_cols};
use crate::ui::widgets::status_cell;

use super::Screen;
use super::overlay::Overlay;
use super::{Action, ActiveProject};

/// A flattened tree row: either a volume header or a chapter.
enum Row<'a> {
    Volume(&'a Volume),
    Chapter { vol: u32, ch: &'a Chapter },
}

pub struct ProjectScreen {
    tree: ListState,
    collapsed: HashSet<u32>,
    /// 0 = tree, 1 = context/detail panel (h/l moves focus between them).
    focus_panel: u8,
    /// Multi-select set of chapter numbers (Space toggles).
    selected: HashSet<u32>,
}

impl ProjectScreen {
    pub fn new() -> Self {
        let mut tree = ListState::default();
        tree.select(Some(0));
        Self {
            tree,
            collapsed: HashSet::new(),
            focus_panel: 0,
            selected: HashSet::new(),
        }
    }

    /// Build the visible (non-collapsed) row list for the active project.
    fn rows<'a>(&self, active: &'a ActiveProject) -> Vec<Row<'a>> {
        let mut rows = Vec::new();
        for vol in &active.project.volumes {
            rows.push(Row::Volume(vol));
            if !self.collapsed.contains(&vol.number) {
                for ch in &vol.chapters {
                    rows.push(Row::Chapter {
                        vol: vol.number,
                        ch,
                    });
                }
            }
        }
        rows
    }

    /// The chapter number under the cursor, if the selected row is a chapter.
    fn selected_chapter(&self, active: &ActiveProject) -> Option<u32> {
        let rows = self.rows(active);
        let idx = self.tree.selected()?;
        match rows.get(idx)? {
            Row::Chapter { ch, .. } => Some(ch.number),
            Row::Volume(_) => None,
        }
    }

    fn selected_volume(&self, active: &ActiveProject) -> Option<u32> {
        let rows = self.rows(active);
        let idx = self.tree.selected()?;
        match rows.get(idx)? {
            Row::Chapter { vol, .. } => Some(*vol),
            Row::Volume(v) => Some(v.number),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, active: Option<&ActiveProject>) -> Action {
        let Some(active) = active else {
            // No project: only `e` (go to lexicon) is live.
            if matches!(key.code, KeyCode::Char('e')) {
                return Action::Goto(Screen::Lexicon);
            }
            return Action::None;
        };

        let rows = self.rows(active);
        let n = rows.len();
        let sel = self.tree.selected().unwrap_or(0).min(n.saturating_sub(1));

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                let next = if sel == 0 {
                    n.saturating_sub(1)
                } else {
                    sel - 1
                };
                self.tree.select(Some(next));
                Action::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let next = if n == 0 { 0 } else { (sel + 1) % n };
                self.tree.select(Some(next));
                Action::None
            }
            KeyCode::Char('h') | KeyCode::Left => {
                if self.focus_panel == 1 {
                    self.focus_panel = 0;
                } else if let Some(vol) = self.selected_volume(active) {
                    self.collapsed.insert(vol);
                }
                Action::None
            }
            KeyCode::Char('l') | KeyCode::Right => {
                if let Some(Row::Volume(v)) = rows.get(sel) {
                    self.collapsed.remove(&v.number);
                } else {
                    self.focus_panel = 1;
                }
                Action::None
            }
            KeyCode::Enter => {
                if let Some(ch) = self.selected_chapter(active) {
                    Action::OpenChapter { chapter: ch }
                } else if let Some(Row::Volume(v)) = rows.get(sel) {
                    if self.collapsed.contains(&v.number) {
                        self.collapsed.remove(&v.number);
                    } else {
                        self.collapsed.insert(v.number);
                    }
                    Action::None
                } else {
                    Action::None
                }
            }
            KeyCode::Char(' ') => {
                if let Some(ch) = self.selected_chapter(active)
                    && !self.selected.insert(ch)
                {
                    self.selected.remove(&ch);
                }
                Action::None
            }
            KeyCode::Char('t') => {
                if let Some(ch) = self.selected_chapter(active) {
                    Action::StartTranslation { chapters: vec![ch] }
                } else {
                    Action::None
                }
            }
            KeyCode::Char('T') => {
                if let Some(vol) = self.selected_volume(active) {
                    let chapters: Vec<u32> = active
                        .project
                        .volumes
                        .iter()
                        .find(|v| v.number == vol)
                        .map(|v| {
                            v.chapters
                                .iter()
                                .filter(|c| translatable(c))
                                .map(|c| c.number)
                                .collect()
                        })
                        .unwrap_or_default();
                    if chapters.is_empty() {
                        Action::None
                    } else {
                        Action::show_overlay(Overlay::confirm(
                            "Translate whole volume?",
                            format!("Queue {} chapter(s) in Vol.{:02}.", chapters.len(), vol),
                            Action::StartTranslation { chapters },
                        ))
                    }
                } else {
                    Action::None
                }
            }
            KeyCode::Char('a') => {
                if self.selected.is_empty() {
                    Action::None
                } else {
                    let mut chapters: Vec<u32> = self.selected.iter().copied().collect();
                    chapters.sort_unstable();
                    Action::StartTranslation { chapters }
                }
            }
            KeyCode::Char('e') => Action::Goto(Screen::Lexicon),
            _ => Action::None,
        }
    }

    pub fn render(
        &mut self,
        f: &mut Frame,
        area: Rect,
        active: Option<&ActiveProject>,
        theme: &Theme,
    ) {
        let Some(active) = active else {
            empty_state(f, area, theme);
            return;
        };

        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(40), Constraint::Length(34)])
            .split(area);

        self.render_tree(f, cols[0], active, theme);
        self.render_side(f, cols[1], active, theme);
    }

    fn render_tree(&mut self, f: &mut Frame, area: Rect, active: &ActiveProject, theme: &Theme) {
        let focused = self.focus_panel == 0;
        let border_color = if focused { theme.accent } else { theme.rule };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(border_color))
            .title(Span::styled(
                " Chapters ",
                Style::default().fg(theme.ink_soft),
            ))
            .style(Style::default().bg(theme.bg_panel));
        let inner = block.inner(area);
        f.render_widget(block, area);

        let rows = self.rows(active);
        let n = rows.len();
        if self.tree.selected().is_none_or(|s| s >= n) {
            self.tree.select(Some(n.saturating_sub(1)));
        }
        let sel = self.tree.selected().unwrap_or(0);
        let name_w = inner.width.saturating_sub(40).max(12) as usize;

        let mut items = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            items.push(match row {
                Row::Volume(v) => volume_row(
                    v,
                    self.collapsed.contains(&v.number),
                    i == sel,
                    inner.width,
                    theme,
                ),
                Row::Chapter { ch, .. } => chapter_row(
                    ch,
                    i == sel,
                    self.selected.contains(&ch.number),
                    name_w,
                    theme,
                ),
            });
        }

        let list = List::new(items).style(Style::default().bg(theme.bg_panel));
        f.render_stateful_widget(list, inner, &mut self.tree);
    }

    fn render_side(&self, f: &mut Frame, area: Rect, active: &ActiveProject, theme: &Theme) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(7), Constraint::Min(0)])
            .split(area);

        self.render_context(f, rows[0], active, theme);
        self.render_detail(f, rows[1], active, theme);
    }

    fn render_context(&self, f: &mut Frame, area: Rect, active: &ActiveProject, theme: &Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(theme.rule))
            .title(Span::styled(
                " Context 文脈 ",
                Style::default().fg(theme.ink_soft),
            ))
            .style(Style::default().bg(theme.bg_panel));
        let inner = block.inner(area);
        f.render_widget(block, area);

        let chars = crate::workspace::characters::load(&active.workspace).len();
        let terms = crate::workspace::glossary::load(&active.workspace).len();

        let files = [
            ("●", "PROJECT.md", "synopsis".to_string(), theme.status_done),
            (
                "●",
                "CHARACTERS.md",
                format!("{chars} entries"),
                theme.status_done,
            ),
            (
                "●",
                "GLOSSARY.md",
                format!("{terms} terms"),
                theme.status_done,
            ),
            ("◐", "STYLE.md", "draft".to_string(), theme.status_working),
        ];
        let mut lines = Vec::new();
        for (glyph, name, note, color) in files {
            lines.push(Line::from(vec![
                Span::styled(format!(" {glyph} "), Style::default().fg(color)),
                Span::styled(pad_to_cols(name, 15), Style::default().fg(theme.ink)),
                Span::styled(note, Style::default().fg(theme.ink_faint)),
            ]));
        }
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme.bg_panel)),
            inner,
        );
    }

    fn render_detail(&self, f: &mut Frame, area: Rect, active: &ActiveProject, theme: &Theme) {
        let ch_no = self.selected_chapter(active);
        let chapter = ch_no.and_then(|n| find_chapter(active, n));

        let title = match &chapter {
            Some(c) if !c.title.is_empty() => {
                format!(" Selected — {} ", truncate_cols(&c.title, 16))
            }
            _ => " Selected ".to_string(),
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(theme.rule))
            .title(Span::styled(title, Style::default().fg(theme.ink_soft)))
            .style(Style::default().bg(theme.bg_panel));
        let inner = block.inner(area);
        f.render_widget(block, area);

        let mut lines = Vec::new();
        if let Some(c) = chapter {
            let (glyph, color) = status_glyph(c.kind, c.status, theme);
            lines.push(Line::from(vec![
                Span::styled(" status  ", Style::default().fg(theme.ink_faint)),
                Span::styled(glyph.to_string(), Style::default().fg(color)),
                Span::raw(" "),
                Span::styled(status_word(c.status), Style::default().fg(color)),
            ]));
            lines.push(Line::from(vec![
                Span::styled(" chunks  ", Style::default().fg(theme.ink_faint)),
                Span::styled(
                    format!("{} · {} done", c.total_chunks, c.committed_chunks),
                    Style::default().fg(theme.ink_soft),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled(" source  ", Style::default().fg(theme.ink_faint)),
                Span::styled(
                    format!("{} 句", c.source_segments),
                    Style::default().fg(theme.ink_soft),
                ),
            ]));
            if !self.selected.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled(" marked  ", Style::default().fg(theme.ink_faint)),
                    Span::styled(
                        format!("{} chapter(s)", self.selected.len()),
                        Style::default().fg(theme.accent),
                    ),
                ]));
            }
            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                Span::styled(" t translate ", Style::default().fg(theme.accent)),
                Span::styled("  e edit context ", Style::default().fg(theme.ink_soft)),
            ]));
        } else {
            lines.push(Line::from(Span::styled(
                " Select a chapter to see its detail.",
                Style::default().fg(theme.ink_faint),
            )));
        }
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme.bg_panel)),
            inner,
        );
    }

    pub fn hints(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("↵", "read"),
            ("t", "translate"),
            ("T", "whole vol"),
            ("Space", "select"),
            ("e", "edit ctx"),
        ]
    }
}

impl Default for ProjectScreen {
    fn default() -> Self {
        Self::new()
    }
}

fn volume_row(
    v: &Volume,
    collapsed: bool,
    selected: bool,
    width: u16,
    theme: &Theme,
) -> ListItem<'static> {
    let caret = if collapsed { "▸" } else { "▾" };
    let tally = vol_tally(v);
    let bar = if selected {
        theme::SELECT_BAR.to_string()
    } else {
        " ".to_string()
    };
    let row_bg = if selected {
        theme.accent_bg
    } else {
        theme.bg_panel
    };
    let label = match &v.label {
        Some(l) => format!("Vol.{:02} {l}", v.number),
        None => format!("Vol.{:02}", v.number),
    };
    let tally_str = format!("●{} ◐{} ○{} ✗{}", tally.0, tally.1, tally.2, tally.3);
    let used = col_width(&format!(" {bar} {caret} {label}  ")) + col_width(&tally_str);
    let fill = (width as usize).saturating_sub(used + 2);
    let dots = "┄".repeat(fill.min(width as usize));

    ListItem::new(Line::from(vec![
        Span::styled(
            format!(" {bar} "),
            Style::default().fg(theme.accent).bg(row_bg),
        ),
        Span::styled(
            format!("{caret} {label} "),
            Style::default()
                .fg(theme.ink)
                .bg(row_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(dots, Style::default().fg(theme.rule).bg(row_bg)),
        Span::styled(
            format!("  {tally_str}"),
            Style::default().fg(theme.ink_soft).bg(row_bg),
        ),
    ]))
}

fn chapter_row(
    ch: &Chapter,
    selected: bool,
    marked: bool,
    name_w: usize,
    theme: &Theme,
) -> ListItem<'static> {
    let cell = status_cell(ch.kind, ch.status, theme);
    let bar = if selected { theme::SELECT_BAR } else { ' ' };
    let mark = if marked { '◆' } else { ' ' };
    let row_bg = if selected {
        theme.accent_bg
    } else {
        theme.bg_panel
    };
    let name = pad_to_cols(&truncate_cols(&ch.title, name_w), name_w);
    let status = status_word(ch.status);
    let time = ch
        .last_run
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_default();

    ListItem::new(Line::from(vec![
        Span::styled(
            format!(" {bar}"),
            Style::default().fg(theme.accent).bg(row_bg),
        ),
        Span::styled(
            mark.to_string(),
            Style::default().fg(theme.accent).bg(row_bg),
        ),
        // status_cell already carries its own fg color; layer the row bg under it.
        cell.patch_style(Style::default().bg(row_bg)),
        Span::styled(
            format!(" {:03}  ", ch.number),
            Style::default().fg(theme.ink_faint).bg(row_bg),
        ),
        Span::styled(name, Style::default().fg(theme.ink).bg(row_bg)),
        Span::styled(
            format!("{:>5} 句 ", ch.source_segments),
            Style::default().fg(theme.ink_faint).bg(row_bg),
        ),
        Span::styled(
            pad_to_cols(status, 10),
            Style::default().fg(theme.ink_soft).bg(row_bg),
        ),
        Span::styled(time, Style::default().fg(theme.ink_faint).bg(row_bg)),
    ]))
}

fn empty_state(f: &mut Frame, area: Rect, theme: &Theme) {
    let lines = vec![
        Line::raw(""),
        Line::raw(""),
        Line::from(Span::styled(
            "  No project open.",
            Style::default().fg(theme.ink_soft),
        )),
        Line::raw(""),
        Line::from(Span::styled(
            "  Press 1 to return to the Shelf and open one with ↵.",
            Style::default().fg(theme.ink_faint),
        )),
    ];
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme.bg)),
        area,
    );
}

fn find_chapter(active: &ActiveProject, number: u32) -> Option<&Chapter> {
    for vol in &active.project.volumes {
        for ch in &vol.chapters {
            if ch.number == number {
                return Some(ch);
            }
        }
    }
    None
}

fn translatable(ch: &Chapter) -> bool {
    matches!(ch.kind, ChapterKind::Prose) && !ch.status.is_terminal()
}

fn vol_tally(v: &Volume) -> (u32, u32, u32, u32) {
    let mut done = 0;
    let mut working = 0;
    let mut pending = 0;
    let mut failed = 0;
    for ch in &v.chapters {
        match ch.status {
            ChapterStatus::Done | ChapterStatus::Appended => done += 1,
            ChapterStatus::Failed => failed += 1,
            s if s.is_active() || s == ChapterStatus::Paused => working += 1,
            _ => pending += 1,
        }
    }
    (done, working, pending, failed)
}

fn status_word(s: ChapterStatus) -> &'static str {
    match s {
        ChapterStatus::Pending => "pending",
        ChapterStatus::Chunking => "chunking",
        ChapterStatus::Translating => "translating",
        ChapterStatus::Reviewing => "reviewing",
        ChapterStatus::Appended => "appended",
        ChapterStatus::Done => "done",
        ChapterStatus::Failed => "failed",
        ChapterStatus::Paused => "paused",
    }
}
