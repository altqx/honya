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
use crate::ui::mouse::{MouseGesture, MouseInput, row_index};
use crate::ui::text::{col_width, pad_to_cols, thai_display_safe, truncate_cols};
use crate::ui::widgets::{render_line_gauge, status_cell};

use super::overlay::Overlay;
use super::{Action, ActiveProject};

/// A flattened tree row: either a volume header or a chapter.
enum Row<'a> {
    Volume(&'a Volume),
    Chapter { vol: u32, ch: &'a Chapter },
}

/// The owned identity of a clicked tree row, copied out so the borrow of the
/// (immutable) row list ends before `handle_mouse` mutates screen state.
enum TreeHit {
    Volume(u32),
    Chapter { vol: u32, ch: u32 },
}

pub struct ProjectScreen {
    tree: ListState,
    collapsed: HashSet<u32>,
    /// 0 = tree, 1 = context/detail panel (h/l moves focus between them).
    focus_panel: u8,
    /// Multi-select set of `(volume, chapter)` ids (Space toggles). Keyed by
    /// volume too so same-numbered chapters in other volumes don't ghost-mark.
    selected: HashSet<(u32, u32)>,
    /// Mouse hit-test rects, refreshed every frame: the chapter tree's inner area
    /// and the right-hand context/detail column.
    tree_area: Rect,
    side_area: Rect,
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
            tree_area: Rect::default(),
            side_area: Rect::default(),
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

    /// The `(volume, chapter)` id under the cursor, if the row is a chapter.
    fn selected_chapter_id(&self, active: &ActiveProject) -> Option<(u32, u32)> {
        let rows = self.rows(active);
        let idx = self.tree.selected()?;
        match rows.get(idx)? {
            Row::Chapter { vol, ch } => Some((*vol, ch.number)),
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

    fn marked_ids(&self, active: &ActiveProject) -> Vec<(u32, u32)> {
        let known: HashSet<(u32, u32)> = active
            .project
            .volumes
            .iter()
            .flat_map(|v| v.chapters.iter().map(|ch| (v.number, ch.number)))
            .collect();
        let mut ids: Vec<(u32, u32)> = self
            .selected
            .iter()
            .copied()
            .filter(|id| known.contains(id))
            .collect();
        ids.sort_unstable();
        ids
    }

    fn marked_chapters_in_vol(&self, vol: u32) -> Vec<u32> {
        let mut chapters: Vec<u32> = self
            .selected
            .iter()
            .filter(|(v, _)| *v == vol)
            .map(|(_, ch)| *ch)
            .collect();
        chapters.sort_unstable();
        chapters
    }

    fn volume_header_index(&self, active: &ActiveProject, vol: u32) -> Option<usize> {
        self.rows(active).iter().position(|r| {
            matches!(r, Row::Volume(v) if v.number == vol)
        })
    }

    fn collapse_all_volumes(&mut self, active: &ActiveProject) {
        let follow = self.selected_volume(active);
        for vol in &active.project.volumes {
            self.collapsed.insert(vol.number);
        }
        if let Some(vol) = follow
            && let Some(idx) = self.volume_header_index(active, vol)
        {
            self.tree.select(Some(idx));
        }
    }

    fn expand_all_volumes(&mut self) {
        self.collapsed.clear();
    }

    pub fn handle_key(&mut self, key: KeyEvent, active: Option<&ActiveProject>) -> Action {
        let Some(active) = active else {
            return Action::None;
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
                if let Some(id) = self.selected_chapter_id(active)
                    && !self.selected.insert(id)
                {
                    self.selected.remove(&id);
                }
                Action::None
            }
            KeyCode::Char('t') | KeyCode::Char('a') => {
                let marked = self.marked_ids(active);
                if !marked.is_empty() {
                    self.selected.clear();
                    Action::EnqueueChapters { chapters: marked }
                } else if let Some(id) = self.selected_chapter_id(active) {
                    Action::EnqueueChapters {
                        chapters: vec![id],
                    }
                } else {
                    Action::None
                }
            }
            KeyCode::Char('T') => {
                // Chapter selection (incl. the disk-completeness check that catches
                // partial files scanning as Done) happens in apply, which has cfg.
                match self.selected_volume(active) {
                    Some(vol) => Action::StartVolumeTranslation { vol },
                    None => Action::None,
                }
            }
            KeyCode::Char('A') => Action::StartProjectTranslation,
            KeyCode::Char('y') => {
                let data = crate::workspace::volume::load(&active.workspace);
                Action::show_overlay(Overlay::synopsis_edit(
                    data.synopsis_raw,
                    data.synopsis_th,
                    active.vol,
                    active.project.title.clone(),
                ))
            }
            KeyCode::Char('e') => Action::show_overlay(Overlay::project_title_edit(
                active.project.id.clone(),
                active.project.title.clone(),
                active.project.title_th.clone(),
            )),
            KeyCode::Char('V') => Action::AddVolume,
            KeyCode::Char('i') => {
                let vol = self.selected_volume(active).unwrap_or(active.vol);
                Action::AddChapters { vol }
            }
            KeyCode::Char('M') => {
                let vol = self.selected_volume(active).unwrap_or(active.vol);
                Action::show_overlay(Overlay::confirm(
                    "Update volume images",
                    format!(
                        "Re-import the source EPUB for Vol.{vol:02}, copy images as vol{vol}_*, and rewrite image links in raw/ and translated/ Markdown. Translation prose stays unchanged."
                    ),
                    Action::RefreshVolumeImages { vol },
                ))
            }
            KeyCode::Char('d') => {
                let vol = self.selected_volume(active).unwrap_or(active.vol);
                let marked = self.marked_chapters_in_vol(vol);
                let chapters = if !marked.is_empty() {
                    marked
                } else if let Some(ch) = self.selected_chapter(active) {
                    vec![ch]
                } else {
                    Vec::new()
                };
                if chapters.is_empty() {
                    Action::None
                } else {
                    let body = if chapters.len() == 1 {
                        format!("Delete chapter {:03} from Vol.{vol:02}?", chapters[0])
                    } else {
                        format!("Delete {} chapters from Vol.{vol:02}?", chapters.len())
                    };
                    for ch in &chapters {
                        self.selected.remove(&(vol, *ch));
                    }
                    Action::show_overlay(Overlay::confirm(
                        "Delete chapters",
                        body,
                        Action::DeleteChapters { vol, chapters },
                    ))
                }
            }
            KeyCode::Char('x') => {
                let vol = self.selected_volume(active).unwrap_or(active.vol);
                Action::show_overlay(Overlay::export(vol))
            }
            KeyCode::Char('z') => {
                self.collapse_all_volumes(active);
                Action::None
            }
            KeyCode::Char('Z') => {
                self.expand_all_volumes();
                Action::None
            }
            _ => Action::None,
        };

        // Auto-follow volume changes when navigation lands without another action.
        if matches!(action, Action::None)
            && let Some(v) = self.selected_volume(active)
            && v != active.vol
        {
            return Action::SetActiveVolume { vol: v };
        }
        action
    }

    /// Mouse: the wheel walks the tree (auto-following the volume under the
    /// cursor, like the keyboard does); a click selects a row; a double-click (or a
    /// click on the selected row) opens a chapter or toggles a volume's collapse.
    /// Clicking the right column focuses it (so `h` steps back to the tree).
    pub fn handle_mouse(&mut self, m: MouseInput, active: Option<&ActiveProject>) -> Action {
        let Some(active) = active else {
            return Action::None;
        };
        match m.gesture {
            MouseGesture::ScrollUp => {
                self.move_tree(active, -1);
                self.follow_volume(active)
            }
            MouseGesture::ScrollDown => {
                self.move_tree(active, 1);
                self.follow_volume(active)
            }
            MouseGesture::Click { double } => {
                if !m.in_rect(self.tree_area) {
                    if m.in_rect(self.side_area) {
                        self.focus_panel = 1;
                    }
                    return Action::None;
                }
                self.focus_panel = 0;
                let rows = self.rows(active);
                let Some(idx) = row_index(self.tree_area, self.tree.offset(), rows.len(), m.row)
                else {
                    return Action::None;
                };
                let activate = double || self.tree.selected() == Some(idx);
                self.tree.select(Some(idx));
                // Copy the row's identity out so the `rows` borrow ends before we
                // mutate `self.collapsed` / `self.selected`.
                let hit = match &rows[idx] {
                    Row::Chapter { vol, ch } => TreeHit::Chapter {
                        vol: *vol,
                        ch: ch.number,
                    },
                    Row::Volume(v) => TreeHit::Volume(v.number),
                };
                drop(rows);
                match hit {
                    TreeHit::Chapter { vol, ch } => {
                        // Opening resolves against the active volume, so a cross-
                        // volume click first switches volumes (a second click opens).
                        if vol != active.vol {
                            return Action::SetActiveVolume { vol };
                        }
                        if activate {
                            return Action::OpenChapter { chapter: ch };
                        }
                        Action::None
                    }
                    TreeHit::Volume(vnum) => {
                        if activate {
                            if self.collapsed.contains(&vnum) {
                                self.collapsed.remove(&vnum);
                            } else {
                                self.collapsed.insert(vnum);
                            }
                        }
                        if vnum != active.vol {
                            return Action::SetActiveVolume { vol: vnum };
                        }
                        Action::None
                    }
                }
            }
            MouseGesture::RightClick => Action::None,
        }
    }

    /// Move the tree cursor by `delta`, clamped (no wrap, so scrolling can't loop).
    fn move_tree(&mut self, active: &ActiveProject, delta: i32) {
        let n = self.rows(active).len();
        if n == 0 {
            return;
        }
        let cur = self.tree.selected().unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, n as i32 - 1) as usize;
        self.tree.select(Some(next));
    }

    /// Auto-follow the cursor's volume (mirrors `handle_key`'s tail).
    fn follow_volume(&mut self, active: &ActiveProject) -> Action {
        if let Some(v) = self.selected_volume(active)
            && v != active.vol
        {
            return Action::SetActiveVolume { vol: v };
        }
        Action::None
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
            .constraints([Constraint::Min(24), Constraint::Min(28)])
            .split(panes[1]);

        self.side_area = cols[1];
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
        let title_src = if active.project.title_th.trim().is_empty() {
            active.project.title.clone()
        } else {
            format!("{} · {}", active.project.title, active.project.title_th)
        };
        let title = truncate_cols(
            &thai_display_safe(&title_src),
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
        self.tree_area = inner;

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
                Row::Chapter { vol, ch } => chapter_row(
                    ch,
                    i == sel,
                    self.selected.contains(&(*vol, ch.number)),
                    name_w,
                    theme,
                ),
            });
        }

        let list = List::new(items).style(Style::default().bg(theme.bg_panel));
        f.render_stateful_widget(list, inner, &mut self.tree);
        // The stateful render just updated the offset to keep the selection visible.
        crate::ui::widgets::render_panel_scrollbar(f, area, n, self.tree.offset(), theme);
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
        let inner_w = inner.width as usize;

        let chars = crate::workspace::characters::load(&active.workspace).len();
        let terms = crate::workspace::glossary::load(&active.workspace).len();

        // Live translation status (across all volumes) replaces the old hardcoded
        // STYLE.md "draft" stub, so the panel reflects real progress in realtime.
        let progress = active.project.translation_progress();
        let (style_glyph, style_color) = match progress.status {
            crate::model::ProjectStatus::Done => ("●", theme.status_done),
            crate::model::ProjectStatus::InProgress => ("◐", theme.status_working),
            crate::model::ProjectStatus::Draft => ("○", theme.ink_faint),
        };
        let style_note = if progress.total > 0 {
            format!(
                "{} · {}/{} ch",
                progress.status.label_en(),
                progress.done,
                progress.total
            )
        } else {
            progress.status.label_en().to_string()
        };

        // PROJECT.md is project-level, so its note mirrors the project's scope (volume
        // count) rather than a static "synopsis" word — the editable synopsis is
        // per-volume (VOLUME.md), not a PROJECT.md field.
        let vols = active.project.volumes.len();
        let vol_note = format!("{vols} vol{}", if vols == 1 { "" } else { "s" });

        let files: [(&str, &str, String, ratatui::style::Color); 4] = [
            ("●", "PROJECT.md", vol_note, theme.status_done),
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
            (style_glyph, "STYLE.md", style_note, style_color),
        ];
        let mut lines = Vec::new();
        for (glyph, name, note, color) in files {
            lines.push(context_file_line(inner_w, glyph, name, &note, color, theme));
        }
        // Always-visible project usage roll-up (sum of every volume's chapters).
        let pu = active.project.usage_total();
        lines.push(context_file_line(
            inner_w,
            "Σ",
            "project",
            &format!("${:.4} · {} tok", pu.cost_usd, human_num(pu.tokens.total)),
            theme.accent,
            theme,
        ));
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme.bg_panel)),
            inner,
        );
    }

    fn render_detail(&self, f: &mut Frame, area: Rect, active: &ActiveProject, theme: &Theme) {
        let ch_no = self.selected_chapter(active);
        let chapter = ch_no.and_then(|n| find_chapter(active, n));
        let inner_w = block_inner_width(area);

        let title = match &chapter {
            Some(c) if !c.title.is_empty() => {
                let body = truncate_cols(
                    &thai_display_safe(&c.title),
                    area.width.saturating_sub(14) as usize,
                );
                format!(" Selected — {body} ")
            }
            _ => " Selected ".to_string(),
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(theme.rule))
            .title(Span::styled(
                truncate_cols(&title, area.width.saturating_sub(2) as usize),
                Style::default().fg(theme.ink_soft),
            ))
            .style(Style::default().bg(theme.bg_panel));
        let inner = block.inner(area);
        f.render_widget(block, area);

        let faint = Style::default().fg(theme.ink_faint);
        let soft = Style::default().fg(theme.ink_soft);
        let mut lines = Vec::new();
        if let Some(c) = chapter {
            let (glyph, color) = status_glyph(c.kind, c.status, theme);
            let mut status = format!("{glyph} {}", status_word(c.status));
            if c.is_partial_review() {
                status.push_str(" · partial");
            }
            lines.push(side_field_line(
                "status",
                &status,
                inner_w,
                faint,
                Style::default().fg(color),
            ));
            let chunk_progress = if c.total_chunks == 0 {
                format!("{} done", c.committed_chunks)
            } else if c.skipped_chunks > 0 {
                format!(
                    "{} / {} translated · {} skipped",
                    c.translated_chunks().min(c.total_chunks),
                    c.total_chunks,
                    c.skipped_chunks
                )
            } else {
                format!(
                    "{} / {} done",
                    c.committed_chunks.min(c.total_chunks),
                    c.total_chunks
                )
            };
            lines.push(side_field_line("chunks", &chunk_progress, inner_w, faint, soft));
            lines.push(side_field_line(
                "source",
                &format!("{} 句", c.source_segments),
                inner_w,
                faint,
                soft,
            ));
            // Lifetime usage at all three levels (chapter → volume → project).
            lines.push(Line::raw(""));
            lines.push(usage_line("chapter", &c.usage, inner_w, theme));
            if let Some(vol) = find_volume(active, c.number) {
                lines.push(usage_line("volume", &vol.usage_total(), inner_w, theme));
            }
            lines.push(usage_line("project", &active.project.usage_total(), inner_w, theme));
            if !self.selected.is_empty() {
                let nvols = self
                    .selected
                    .iter()
                    .map(|(v, _)| *v)
                    .collect::<HashSet<_>>()
                    .len();
                let label = if nvols > 1 {
                    format!(
                        "{} chapter(s) · {} vols",
                        self.selected.len(),
                        nvols
                    )
                } else {
                    format!("{} chapter(s)", self.selected.len())
                };
                lines.push(side_field_line(
                    "marked",
                    &label,
                    inner_w,
                    faint,
                    Style::default().fg(theme.accent),
                ));
            }
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                truncate_cols(" t queue ", inner_w),
                Style::default().fg(theme.accent),
            )));
            lines.push(Line::from(Span::styled(
                truncate_cols(" M images ", inner_w),
                Style::default().fg(theme.accent),
            )));
        } else {
            lines.push(usage_line("project", &active.project.usage_total(), inner_w, theme));
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                truncate_cols(" Select a chapter to see its detail.", inner_w),
                faint,
            )));
            lines.push(Line::from(Span::styled(
                truncate_cols(" M update volume images.", inner_w),
                Style::default().fg(theme.accent),
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
            ("Space", "mark"),
            ("t", "queue"),
            ("T", "vol"),
            ("A", "project"),
            ("d", "delete"),
            ("h/l", "nav"),
            ("z/Z", "fold"),
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
    let status = if ch.is_partial_review() {
        "partial"
    } else {
        status_word(ch.status)
    };
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
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// Inner width of a bordered side-panel block (left + right border).
fn block_inner_width(area: Rect) -> usize {
    area.width.saturating_sub(2) as usize
}

fn context_name_cols(inner_w: usize) -> usize {
    let lead_w = col_width(" ● ");
    let avail = inner_w.saturating_sub(lead_w);
    if avail <= 18 {
        avail.saturating_sub(6).clamp(6, 10)
    } else {
        13.min(avail.saturating_sub(8))
    }
}

fn context_file_line(
    inner_w: usize,
    glyph: &str,
    name: &str,
    note: &str,
    color: ratatui::style::Color,
    theme: &Theme,
) -> Line<'static> {
    let lead = format!(" {glyph} ");
    let lead_w = col_width(&lead);
    let name_w = context_name_cols(inner_w);
    let note_w = inner_w.saturating_sub(lead_w + name_w);
    Line::from(vec![
        Span::styled(lead, Style::default().fg(color)),
        Span::styled(pad_to_cols(name, name_w), Style::default().fg(theme.ink)),
        Span::styled(
            truncate_cols(note, note_w),
            Style::default().fg(theme.ink_faint),
        ),
    ])
}

fn side_field_line(
    label: &str,
    value: &str,
    width: usize,
    label_style: Style,
    value_style: Style,
) -> Line<'static> {
    let prefix = format!(" {label:<7}");
    let value = truncate_cols(value, width.saturating_sub(col_width(&prefix)));
    Line::from(vec![
        Span::styled(prefix, label_style),
        Span::styled(value, value_style),
    ])
}

/// One labelled usage line for the detail card: `label  N tok · M tools · $C`.
fn usage_line(label: &str, u: &UsageStats, width: usize, theme: &Theme) -> Line<'static> {
    let label_style = Style::default().fg(theme.ink_faint);
    let value_style = Style::default().fg(theme.ink_soft);
    let prefix = format!(" {label:<7}");
    let budget = width.saturating_sub(col_width(&prefix));
    let full = format!(
        "{} tok · {} tools · ${:.4}",
        human_num(u.tokens.total),
        u.tool_calls,
        u.cost_usd
    );
    let value = if col_width(&full) <= budget {
        full
    } else {
        let mid = format!(
            "{} tok · {} tools",
            human_num(u.tokens.total),
            u.tool_calls,
        );
        if col_width(&mid) <= budget {
            truncate_cols(&mid, budget)
        } else {
            truncate_cols(
                &format!(
                    "{}·{}·${:.1}",
                    human_num(u.tokens.total),
                    u.tool_calls,
                    u.cost_usd
                ),
                budget,
            )
        }
    };
    Line::from(vec![
        Span::styled(prefix, label_style),
        Span::styled(value, value_style),
    ])
}

pub(crate) fn translatable(ch: &Chapter) -> bool {
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
        ChapterStatus::Partial => "partial",
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
            skipped_chunks: 0,
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
                title_th: String::new(),
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
            clients: Some(crate::llm::ClientSet::single(std::sync::Arc::new(
                crate::llm::mock::MockClient::default(),
            )
                as std::sync::Arc<dyn crate::llm::client::LlmClient>)),
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
        assert!(screen.selected.contains(&(1, 1)));

        // Move the cursor to chapter 2. Pressing `t` must translate the marked
        // chapter, not the cursor row, then clear the mark state.
        assert!(matches!(
            screen.handle_key(key(KeyCode::Down), Some(&active)),
            Action::None
        ));
        match screen.handle_key(key(KeyCode::Char('t')), Some(&active)) {
            Action::EnqueueChapters { chapters, .. } => assert_eq!(chapters, vec![(1, 1)]),
            other => panic!("expected EnqueueChapters, got {other:?}"),
        }
        assert!(
            screen.selected.is_empty(),
            "marks clear after queueing translation"
        );

        // With no marks, `t` remains the single-chapter shortcut for the cursor row.
        match screen.handle_key(key(KeyCode::Char('t')), Some(&active)) {
            Action::EnqueueChapters { chapters, .. } => assert_eq!(chapters, vec![(1, 2)]),
            other => panic!("expected EnqueueChapters, got {other:?}"),
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
    fn switching_volume_preserves_marks() {
        let active = two_vol_project();
        let mut screen = ProjectScreen::new();

        // Mark chapter 1 in Vol.01.
        screen.handle_key(key(KeyCode::Down), Some(&active));
        screen.handle_key(key(KeyCode::Char(' ')), Some(&active));
        assert!(screen.selected.contains(&(1, 1)));

        // Crossing into Vol.02 keeps the marks so cross-volume queueing works.
        screen.handle_key(key(KeyCode::Down), Some(&active));
        let action = screen.handle_key(key(KeyCode::Down), Some(&active));
        assert!(matches!(action, Action::SetActiveVolume { vol: 2 }));
        assert!(
            screen.selected.contains(&(1, 1)),
            "marks persist when leaving a volume"
        );
    }

    #[test]
    fn cross_volume_mark_and_queue() {
        let active = two_vol_project();
        let mut screen = ProjectScreen::new();

        // Mark Vol.01 ch 1.
        screen.handle_key(key(KeyCode::Down), Some(&active));
        screen.handle_key(key(KeyCode::Char(' ')), Some(&active));

        // Move to Vol.02 ch 1 and mark it too.
        screen.handle_key(key(KeyCode::Down), Some(&active));
        screen.handle_key(key(KeyCode::Down), Some(&active));
        screen.handle_key(key(KeyCode::Down), Some(&active));
        screen.handle_key(key(KeyCode::Char(' ')), Some(&active));

        match screen.handle_key(key(KeyCode::Char('t')), Some(&active)) {
            Action::EnqueueChapters { chapters, .. } => {
                assert_eq!(chapters, vec![(1, 1), (2, 1)]);
            }
            other => panic!("expected EnqueueChapters, got {other:?}"),
        }
        assert!(
            screen.selected.is_empty(),
            "marks clear after queueing translation"
        );
    }

    #[test]
    fn marks_are_scoped_to_their_volume() {
        // Marking Vol.01's chapter 1 must not ghost-mark Vol.02's chapter 1
        // (the tree renders every volume, and they share chapter numbers).
        let active = two_vol_project();
        let mut screen = ProjectScreen::new();

        screen.handle_key(key(KeyCode::Down), Some(&active));
        screen.handle_key(key(KeyCode::Char(' ')), Some(&active));

        assert!(screen.selected.contains(&(1, 1)));
        assert!(
            !screen.selected.contains(&(2, 1)),
            "same-numbered chapter in another volume must not be marked"
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

    fn click(double: bool, col: u16, row: u16) -> crate::ui::mouse::MouseInput {
        crate::ui::mouse::MouseInput {
            gesture: crate::ui::mouse::MouseGesture::Click { double },
            col,
            row,
        }
    }

    /// Clicking a chapter row selects it; a double-click (or a click on the
    /// selected row) opens it in the Reader.
    #[test]
    fn clicking_a_chapter_selects_then_opens() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let active = active_project(); // Vol.01 with chapters 1 & 2
        let mut screen = ProjectScreen::new();
        let theme = crate::model::ThemeId::default().build();
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| screen.render(f, f.area(), Some(&active), &theme))
            .unwrap();
        let ta = screen.tree_area;

        // Rows: 0 = Vol header, 1 = ch 1, 2 = ch 2. Click ch 1.
        let a = screen.handle_mouse(click(false, ta.x + 4, ta.y + 1), Some(&active));
        assert!(matches!(a, Action::None));
        assert_eq!(screen.tree.selected(), Some(1));

        // Clicking the selected chapter row opens it.
        match screen.handle_mouse(click(false, ta.x + 4, ta.y + 1), Some(&active)) {
            Action::OpenChapter { chapter } => assert_eq!(chapter, 1),
            other => panic!("expected OpenChapter, got {other:?}"),
        }
    }

    /// Clicking a row in another volume switches the active volume first (the same
    /// auto-follow the keyboard does), rather than opening across volumes.
    #[test]
    fn clicking_into_another_volume_follows_it() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let active = two_vol_project(); // active.vol == 1
        let mut screen = ProjectScreen::new();
        let theme = crate::model::ThemeId::default().build();
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| screen.render(f, f.area(), Some(&active), &theme))
            .unwrap();
        let ta = screen.tree_area;

        // Rows: 0 Vol.01, 1 ch1, 2 ch2, 3 Vol.02, 4 ch1, 5 ch2. Click into Vol.02.
        match screen.handle_mouse(click(false, ta.x + 4, ta.y + 3), Some(&active)) {
            Action::SetActiveVolume { vol } => assert_eq!(vol, 2),
            other => panic!("expected SetActiveVolume, got {other:?}"),
        }
    }

    #[test]
    fn chapter_tree_draws_a_scrollbar_when_overflowing() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut active = active_project();
        for i in 3..=40 {
            active
                .project
                .volumes
                .iter_mut()
                .next()
                .unwrap()
                .chapters
                .push(chapter(i));
        }
        let mut screen = ProjectScreen::new();
        let theme = crate::model::ThemeId::default().build();
        let mut term = Terminal::new(TestBackend::new(100, 12)).unwrap();
        term.draw(|f| screen.render(f, f.area(), Some(&active), &theme))
            .unwrap();

        let outer_right = screen.tree_area.x + screen.tree_area.width;
        let mut saw_bar = false;
        for row in screen.tree_area.y..screen.tree_area.y + screen.tree_area.height {
            let cell = term.backend().buffer()[(outer_right, row)].symbol();
            if cell == "┃" || cell == "│" {
                saw_bar = true;
                break;
            }
        }
        assert!(saw_bar, "overflowing chapter tree should render a scrollbar");
    }

    #[test]
    fn z_collapses_all_volumes_and_snaps_to_volume_header() {
        let active = two_vol_project();
        let mut screen = ProjectScreen::new();

        screen.focus_volume(&active, 2);
        screen.handle_key(key(KeyCode::Down), Some(&active));
        assert_eq!(screen.selected_chapter(&active), Some(1));
        assert_eq!(screen.selected_volume(&active), Some(2));

        let action = screen.handle_key(key(KeyCode::Char('z')), Some(&active));
        assert!(
            matches!(action, Action::None | Action::SetActiveVolume { vol: 2 }),
            "unexpected action: {action:?}"
        );
        assert!(
            active
                .project
                .volumes
                .iter()
                .all(|v| screen.collapsed.contains(&v.number)),
            "every volume should be collapsed"
        );
        assert_eq!(
            screen.rows(&active).len(),
            active.project.volumes.len(),
            "only volume headers remain visible"
        );
        assert_eq!(
            screen.selected_volume(&active),
            Some(2),
            "cursor snaps to the current volume header"
        );
        assert!(screen.selected_chapter(&active).is_none());
    }

    #[test]
    fn shift_z_expands_all_volumes() {
        let active = two_vol_project();
        let mut screen = ProjectScreen::new();

        screen.handle_key(key(KeyCode::Char('z')), Some(&active));
        assert_eq!(screen.rows(&active).len(), 2);

        assert!(matches!(
            screen.handle_key(key(KeyCode::Char('Z')), Some(&active)),
            Action::None
        ));
        assert!(screen.collapsed.is_empty());
        assert_eq!(screen.rows(&active).len(), 6, "both volumes show chapters again");
    }

    #[test]
    fn usage_line_truncates_for_narrow_side_panel() {
        let theme = crate::model::ThemeId::default().build();
        let u = UsageStats {
            tokens: crate::model::TokenUsage {
                total: 164_600_000,
                ..Default::default()
            },
            tool_calls: 4709,
            cost_usd: 70.3614,
        };
        let line = usage_line("project", &u, 30, &theme);
        let rendered: String = line
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            col_width(&rendered) <= 30,
            "usage line should fit the panel: {rendered} ({})",
            col_width(&rendered)
        );
    }

    #[test]
    fn context_file_line_truncates_long_notes() {
        let theme = crate::model::ThemeId::default().build();
        let line = context_file_line(
            30,
            "●",
            "STYLE.md",
            "in progress · 999/999 ch",
            theme.status_working,
            &theme,
        );
        let rendered: String = line
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            col_width(&rendered) <= 30,
            "context row should fit the panel: {rendered} ({})",
            col_width(&rendered)
        );
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
