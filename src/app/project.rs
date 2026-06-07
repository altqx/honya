//! The Project Dashboard (2 棚): chapter tree + context files + detail card; launches runs (`t`/`T`).

use std::collections::HashSet;

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::model::{Chapter, ChapterKind, ChapterStatus, Project, UsageStats, Volume};
use crate::theme::{self, Theme, status_glyph};
use crate::ui::text::{col_width, pad_to_cols, thai_display_safe, truncate_cols};
use crate::ui::widgets::{render_line_gauge, status_cell};

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

    fn marked_chapters(&self, active: &ActiveProject) -> Vec<u32> {
        let known: HashSet<u32> = active
            .project
            .volumes
            .iter()
            .flat_map(|v| v.chapters.iter().map(|ch| ch.number))
            .collect();
        let mut chapters: Vec<u32> = self
            .selected
            .iter()
            .copied()
            .filter(|ch| known.contains(ch))
            .collect();
        chapters.sort_unstable();
        chapters
    }

    pub fn handle_key(&mut self, key: KeyEvent, active: Option<&ActiveProject>) -> Action {
        let Some(active) = active else {
            // No project: `e` (go to lexicon) and `Q` (QA inbox, shows the
            // "no project" state) are the only live keys.
            return match key.code {
                KeyCode::Char('e') => Action::Goto(Screen::Lexicon),
                KeyCode::Char('Q') => Action::show_overlay(Overlay::qa_placeholder()),
                _ => Action::None,
            };
        };

        let rows = self.rows(active);
        let n = rows.len();
        let sel = self.tree.selected().unwrap_or(0).min(n.saturating_sub(1));

        let action = match key.code {
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
                let marked = self.marked_chapters(active);
                if !marked.is_empty() {
                    self.selected.clear();
                    Action::StartTranslation { chapters: marked }
                } else if let Some(ch) = self.selected_chapter(active) {
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
                let chapters = self.marked_chapters(active);
                if chapters.is_empty() {
                    Action::None
                } else {
                    self.selected.clear();
                    Action::StartTranslation { chapters }
                }
            }
            KeyCode::Char('e') => Action::Goto(Screen::Lexicon),
            // Re-open the synopsis editor for the active volume (raw → translate → reroll).
            KeyCode::Char('y') => {
                let data = crate::workspace::volume::load(&active.workspace);
                Action::show_overlay(Overlay::synopsis_edit(data.synopsis_raw, data.synopsis_th))
            }
            // Add a volume to this project (import wizard pre-targeted at it).
            KeyCode::Char('V') => Action::AddVolume,
            // Export the volume under the cursor to deliverable formats.
            KeyCode::Char('x') => {
                let vol = self.selected_volume(active).unwrap_or(active.vol);
                Action::show_overlay(Overlay::export(vol))
            }
            // Translation QA inbox (App rebuilds the report from the live project).
            KeyCode::Char('Q') => Action::show_overlay(Overlay::qa_placeholder()),
            _ => Action::None,
        };

        // Auto-follow the cursor: when navigation lands in a different volume and no
        // concrete action fired, switch the active volume to match — so Reader /
        // Translate / synopsis / QA all operate on the volume under the cursor.
        // Marks key off per-volume chapter numbers, so leaving a volume clears them.
        if matches!(action, Action::None)
            && let Some(v) = self.selected_volume(active)
            && v != active.vol
        {
            self.selected.clear();
            return Action::SetActiveVolume { vol: v };
        }
        action
    }

    /// Move the tree cursor onto a volume's header (expanding it), so the App can
    /// land the user on a freshly-added volume after import.
    pub fn focus_volume(&mut self, active: &ActiveProject, vol: u32) {
        self.collapsed.remove(&vol);
        if let Some(idx) = self
            .rows(active)
            .iter()
            .position(|r| matches!(r, Row::Volume(v) if v.number == vol))
        {
            self.tree.select(Some(idx));
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

        // Project dashboard band on top (title · active volume · overall progress),
        // then the chapter tree + context/detail panels below it.
        let panes = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(0)])
            .split(area);
        self.render_dashboard(f, panes[0], active, theme);

        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(40), Constraint::Length(34)])
            .split(panes[1]);

        self.render_tree(f, cols[0], active, theme);
        self.render_side(f, cols[1], active, theme);
    }

    /// The dashboard band: `棚 title` with the active volume + volume count on the
    /// right, and a project-wide chapter-completion gauge below.
    fn render_dashboard(&self, f: &mut Frame, area: Rect, active: &ActiveProject, theme: &Theme) {
        if area.height < 2 {
            return;
        }
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(area);

        // Title line: 棚 + project title (left), active volume + count (right).
        let prefix = " 棚  ";
        let title = truncate_cols(
            &thai_display_safe(&active.project.title),
            (area.width as usize).saturating_sub(34).max(10),
        );
        let nvols = active.project.volumes.len();
        let vol_label = active
            .project
            .volumes
            .iter()
            .find(|v| v.number == active.vol)
            .and_then(|v| v.label.as_deref());
        let right = match vol_label {
            Some(l) => format!(
                "Vol.{:02} {} · {} vol{}",
                active.vol,
                thai_display_safe(l),
                nvols,
                if nvols == 1 { "" } else { "s" }
            ),
            None => format!(
                "Vol.{:02} · {} vol{}",
                active.vol,
                nvols,
                if nvols == 1 { "" } else { "s" }
            ),
        };
        let mut spans = vec![
            Span::styled(prefix, Style::default().fg(theme.accent)),
            Span::styled(
                title.clone(),
                Style::default().fg(theme.ink).add_modifier(Modifier::BOLD),
            ),
        ];
        let used = col_width(prefix) + col_width(&title);
        let rw = col_width(&right);
        if (area.width as usize) > used + rw + 2 {
            let gap = area.width as usize - used - rw - 1;
            spans.push(Span::raw(" ".repeat(gap)));
            spans.push(Span::styled(right, Style::default().fg(theme.accent_soft)));
        }
        f.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(theme.bg)),
            rows[0],
        );

        // Progress line: overall chapter completion across every volume.
        let (done, total) = project_progress(&active.project);
        let pct = if total == 0 {
            0
        } else {
            ((done as f64 / total as f64) * 100.0).round() as u16
        };
        let ratio = if total == 0 {
            0.0
        } else {
            done as f64 / total as f64
        };
        let label = format!(" {done}/{total} chapters · {pct}%");
        render_line_gauge(f, rows[1], ratio, &label, theme);
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
        // Always-visible project usage roll-up (sum of every volume's chapters).
        let pu = active.project.usage_total();
        lines.push(Line::from(vec![
            Span::styled(" Σ ", Style::default().fg(theme.accent)),
            Span::styled(pad_to_cols("project", 15), Style::default().fg(theme.ink)),
            Span::styled(
                format!("${:.4} · {} tok", pu.cost_usd, human_num(pu.tokens.total)),
                Style::default().fg(theme.ink_faint),
            ),
        ]));
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
                format!(
                    " Selected — {} ",
                    truncate_cols(&thai_display_safe(&c.title), 16)
                )
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
            let chunk_progress = if c.total_chunks == 0 {
                format!("{} done", c.committed_chunks)
            } else {
                format!(
                    "{} / {} done",
                    c.committed_chunks.min(c.total_chunks),
                    c.total_chunks
                )
            };
            lines.push(Line::from(vec![
                Span::styled(" chunks  ", Style::default().fg(theme.ink_faint)),
                Span::styled(chunk_progress, Style::default().fg(theme.ink_soft)),
            ]));
            lines.push(Line::from(vec![
                Span::styled(" source  ", Style::default().fg(theme.ink_faint)),
                Span::styled(
                    format!("{} 句", c.source_segments),
                    Style::default().fg(theme.ink_soft),
                ),
            ]));
            // Lifetime usage at all three levels (chapter → volume → project).
            lines.push(Line::raw(""));
            lines.push(usage_line("chapter", &c.usage, theme));
            if let Some(vol) = find_volume(active, c.number) {
                lines.push(usage_line("volume", &vol.usage_total(), theme));
            }
            lines.push(usage_line("project", &active.project.usage_total(), theme));
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
            lines.push(usage_line("project", &active.project.usage_total(), theme));
            lines.push(Line::raw(""));
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
            ("t", "marked/current"),
            ("T", "whole vol"),
            ("V", "add vol"),
            ("x", "export"),
            ("Space", "mark"),
            ("h/l", "tree/focus"),
            ("e", "edit ctx"),
            ("y", "synopsis"),
            ("Q", "QA"),
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
        Some(l) => format!("Vol.{:02} {}", v.number, thai_display_safe(l)),
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
    let name = pad_to_cols(
        &truncate_cols(&thai_display_safe(&ch.title), name_w),
        name_w,
    );
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

/// The volume that owns `chapter`, for per-volume usage roll-ups.
fn find_volume(active: &ActiveProject, chapter: u32) -> Option<&Volume> {
    active
        .project
        .volumes
        .iter()
        .find(|v| v.chapters.iter().any(|c| c.number == chapter))
}

/// Compact token count: `1.2k` past a thousand, else the raw number.
fn human_num(n: u32) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// One labelled usage line for the detail card: `label  N tok · M tools · $C`.
fn usage_line(label: &str, u: &UsageStats, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" {label:<8}"), Style::default().fg(theme.ink_faint)),
        Span::styled(
            format!(
                "{} tok · {} tools · ${:.4}",
                human_num(u.tokens.total),
                u.tool_calls,
                u.cost_usd
            ),
            Style::default().fg(theme.ink_soft),
        ),
    ])
}

fn translatable(ch: &Chapter) -> bool {
    matches!(ch.kind, ChapterKind::Prose)
        && (!ch.status.is_terminal() || ch.status == ChapterStatus::Failed)
}

/// (done, total) chapter counts across every volume, for the dashboard gauge.
/// NeedsReview counts as done (content exists); Appended too.
fn project_progress(p: &Project) -> (usize, usize) {
    let mut done = 0;
    let mut total = 0;
    for v in &p.volumes {
        for c in &v.chapters {
            total += 1;
            if matches!(
                c.status,
                ChapterStatus::Done | ChapterStatus::Appended | ChapterStatus::NeedsReview
            ) {
                done += 1;
            }
        }
    }
    (done, total)
}

fn vol_tally(v: &Volume) -> (u32, u32, u32, u32) {
    let mut done = 0;
    let mut working = 0;
    let mut pending = 0;
    let mut failed = 0;
    for ch in &v.chapters {
        match ch.status {
            // NeedsReview is fully written (content exists), so it counts as done
            // for progress; the per-chapter row still shows the warn glyph/label.
            ChapterStatus::Done | ChapterStatus::Appended | ChapterStatus::NeedsReview => done += 1,
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
        ChapterStatus::NeedsReview => "needs review",
        ChapterStatus::Failed => "failed",
        ChapterStatus::Paused => "paused",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ModelSet, Project};
    use crate::workspace::Workspace;
    use ratatui::crossterm::event::{KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn chapter(number: u32) -> Chapter {
        Chapter {
            number,
            title: format!("Chapter {number:03}"),
            kind: ChapterKind::Prose,
            status: ChapterStatus::Pending,
            source_segments: 1,
            total_chunks: 0,
            committed_chunks: 0,
            last_run: None,
            usage: UsageStats::default(),
        }
    }

    fn active_project() -> ActiveProject {
        let dir = std::env::temp_dir().join(format!("honya_project_screen_{}", std::process::id()));
        ActiveProject {
            project: Project {
                id: "novel".to_string(),
                dir: dir.clone(),
                title: "Novel".to_string(),
                created: None,
                touched: None,
                volumes: vec![Volume {
                    number: 1,
                    dir: dir.join("Vol_01"),
                    label: None,
                    chapters: vec![chapter(1), chapter(2)],
                }],
                models: None,
            },
            workspace: Workspace::new(dir, 1),
            client: Some(std::sync::Arc::new(crate::llm::mock::MockClient::default())
                as std::sync::Arc<dyn crate::llm::client::LlmClient>),
            models: ModelSet::default(),
            vol: 1,
        }
    }

    /// Two-volume project (each volume has chapters 1 & 2), active on Vol.01 —
    /// the fixture for the auto-follow-the-cursor volume-switching tests.
    fn two_vol_project() -> ActiveProject {
        let mut active = active_project();
        let dir = active.project.dir.clone();
        active.project.volumes.push(Volume {
            number: 2,
            dir: dir.join("Vol_02"),
            label: None,
            chapters: vec![chapter(1), chapter(2)],
        });
        active
    }

    #[test]
    fn t_prefers_marked_chapters_and_clears_marks() {
        let active = active_project();
        let mut screen = ProjectScreen::new();

        // Row 0 is the volume header; move to chapter 1 and mark it with Space.
        assert!(matches!(
            screen.handle_key(key(KeyCode::Down), Some(&active)),
            Action::None
        ));
        assert!(matches!(
            screen.handle_key(key(KeyCode::Char(' ')), Some(&active)),
            Action::None
        ));
        assert!(screen.selected.contains(&1));

        // Move the cursor to chapter 2. Pressing `t` must translate the marked
        // chapter, not the cursor row, then clear the mark state.
        assert!(matches!(
            screen.handle_key(key(KeyCode::Down), Some(&active)),
            Action::None
        ));
        match screen.handle_key(key(KeyCode::Char('t')), Some(&active)) {
            Action::StartTranslation { chapters } => assert_eq!(chapters, vec![1]),
            other => panic!("expected StartTranslation, got {other:?}"),
        }
        assert!(
            screen.selected.is_empty(),
            "marks clear after queueing translation"
        );

        // With no marks, `t` remains the single-chapter shortcut for the cursor row.
        match screen.handle_key(key(KeyCode::Char('t')), Some(&active)) {
            Action::StartTranslation { chapters } => assert_eq!(chapters, vec![2]),
            other => panic!("expected StartTranslation, got {other:?}"),
        }
    }

    #[test]
    fn cursor_into_next_volume_switches_active_volume() {
        let active = two_vol_project(); // active.vol == 1, cursor on Vol.01 header
        let mut screen = ProjectScreen::new();

        // Down over Vol.01's two chapters stays in volume 1 (no switch).
        assert!(matches!(
            screen.handle_key(key(KeyCode::Down), Some(&active)),
            Action::None
        ));
        assert!(matches!(
            screen.handle_key(key(KeyCode::Down), Some(&active)),
            Action::None
        ));
        // The next Down lands on the Vol.02 header → auto-switch the active volume.
        match screen.handle_key(key(KeyCode::Down), Some(&active)) {
            Action::SetActiveVolume { vol } => assert_eq!(vol, 2),
            other => panic!("expected SetActiveVolume, got {other:?}"),
        }
    }

    #[test]
    fn switching_volume_clears_marks() {
        let active = two_vol_project();
        let mut screen = ProjectScreen::new();

        // Mark chapter 1 in Vol.01.
        screen.handle_key(key(KeyCode::Down), Some(&active));
        screen.handle_key(key(KeyCode::Char(' ')), Some(&active));
        assert!(screen.selected.contains(&1));

        // Crossing into Vol.02 clears the marks (they key off per-volume numbers).
        screen.handle_key(key(KeyCode::Down), Some(&active));
        let action = screen.handle_key(key(KeyCode::Down), Some(&active));
        assert!(matches!(action, Action::SetActiveVolume { vol: 2 }));
        assert!(
            screen.selected.is_empty(),
            "marks clear when leaving a volume"
        );
    }

    #[test]
    fn v_key_requests_add_volume() {
        let active = active_project();
        let mut screen = ProjectScreen::new();
        assert!(matches!(
            screen.handle_key(key(KeyCode::Char('V')), Some(&active)),
            Action::AddVolume
        ));
    }

    #[test]
    fn focus_volume_moves_cursor_to_the_volume_header() {
        let active = two_vol_project();
        let mut screen = ProjectScreen::new();
        screen.focus_volume(&active, 2);
        // The selected row must now be the Vol.02 header.
        assert_eq!(screen.selected_volume(&active), Some(2));
        assert!(
            screen.selected_chapter(&active).is_none(),
            "cursor sits on the header, not a chapter"
        );
    }
}
