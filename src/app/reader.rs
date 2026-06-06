//! src/app/reader.rs — the Reader / Diff view (4 読).
//!
//! Synced side-by-side JA source vs TH translation for proofreading. A single
//! shared scroll position drives both panes; `z` decouples; `[`/`]` move between
//! chapters; `o` cycles split / JA-only / TH-only; `w` toggles wrap.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::model::ReaderAnnotation;
use crate::theme::{self, Theme};
use crate::workspace::Workspace;

use super::Action;
use super::overlay::Overlay;

/// Layout modes for `o`.
const MODE_SPLIT: u8 = 0;
const MODE_JA: u8 = 1;
const MODE_TH: u8 = 2;

pub struct ReaderScreen {
    scroll: u16,
    /// Second (Thai) scroll offset, used only when sync is off.
    th_scroll: u16,
    sync: bool,
    wrap: bool,
    layout_mode: u8,
    ja: String,
    th: String,
    annotations: Vec<ReaderAnnotation>,
    show_annotations: bool,
    chapter: u32,
}

impl ReaderScreen {
    pub fn new() -> Self {
        Self {
            scroll: 0,
            th_scroll: 0,
            sync: true,
            wrap: true,
            layout_mode: MODE_SPLIT,
            ja: String::new(),
            th: String::new(),
            annotations: Vec::new(),
            show_annotations: true,
            chapter: 0,
        }
    }

    /// Load raw/ (JA) + translated/ (TH) for a chapter. Kept synchronous to match
    /// the locked signature; uses std::fs directly (the async helpers wrap the
    /// same files but `load` is called from the synchronous render/apply path).
    pub fn load(&mut self, ws: &Workspace, chapter: u32) {
        self.chapter = chapter;
        self.scroll = 0;
        self.th_scroll = 0;
        self.ja = std::fs::read_to_string(ws.raw(chapter))
            .unwrap_or_else(|_| "（原文がまだありません — raw not found）".to_string());
        let th = std::fs::read_to_string(ws.translated(chapter))
            .unwrap_or_else(|_| "（ยังไม่มีคำแปล — not translated yet）".to_string());
        // Decompose Thai SARA AM so it never lands as a width-2 single cell that
        // desyncs the terminal and smears ำ across the screen on the next redraw.
        self.th = crate::ui::text::thai_display_safe(&th);
        self.reload_annotations(ws);
    }

    pub fn reload_annotations(&mut self, ws: &Workspace) {
        self.annotations = if self.chapter == 0 {
            Vec::new()
        } else {
            crate::workspace::volume::reader_annotations(ws, self.chapter)
        };
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.scroll_by(1);
                Action::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.scroll_by(-1);
                Action::None
            }
            KeyCode::Char(' ') => {
                self.scroll_by(10);
                Action::None
            }
            KeyCode::Char('b') | KeyCode::PageUp => {
                self.scroll_by(-10);
                Action::None
            }
            KeyCode::Char('[') => {
                // Previous chapter: the App owns the workspace, so signal via
                // OpenChapter with the decremented number (App reloads us).
                if self.chapter > 1 {
                    Action::OpenChapter {
                        chapter: self.chapter - 1,
                    }
                } else {
                    Action::None
                }
            }
            KeyCode::Char(']') => Action::OpenChapter {
                chapter: self.chapter + 1,
            },
            KeyCode::Char('z') => {
                self.sync = !self.sync;
                if self.sync {
                    self.th_scroll = self.scroll;
                }
                Action::None
            }
            KeyCode::Char('w') => {
                self.wrap = !self.wrap;
                Action::None
            }
            KeyCode::Char('o') => {
                self.layout_mode = (self.layout_mode + 1) % 3;
                Action::None
            }
            KeyCode::Char('n') => {
                if self.chapter == 0 {
                    Action::None
                } else {
                    let line = self.current_annotation_line();
                    Action::show_overlay(Overlay::reader_note(self.chapter, line))
                }
            }
            KeyCode::Char('N') => {
                self.show_annotations = !self.show_annotations;
                Action::None
            }
            KeyCode::Char('y') => {
                // Copy is environment-dependent; we acknowledge via a no-op Action
                // and let the App surface a toast through its normal channels. We
                // keep it a no-op here so the build never depends on a clipboard dep.
                Action::None
            }
            // Translation QA inbox (App rebuilds the report from the live project).
            KeyCode::Char('Q') => Action::show_overlay(Overlay::qa_placeholder()),
            _ => Action::None,
        }
    }

    fn scroll_by(&mut self, delta: i32) {
        let apply = |v: u16| -> u16 {
            if delta >= 0 {
                v.saturating_add(delta as u16)
            } else {
                v.saturating_sub((-delta) as u16)
            }
        };
        self.scroll = apply(self.scroll);
        if self.sync {
            self.th_scroll = self.scroll;
        } else {
            self.th_scroll = apply(self.th_scroll);
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, theme: &Theme) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(4), Constraint::Length(1)])
            .split(area);
        let body = rows[0];

        match self.layout_mode {
            MODE_JA => {
                self.render_pane(
                    f,
                    body,
                    theme,
                    "日本語 (raw)",
                    &self.ja,
                    theme.ja_text,
                    self.scroll,
                    false,
                    None,
                );
            }
            MODE_TH => {
                self.render_pane(
                    f,
                    body,
                    theme,
                    "ไทย (translated)",
                    &self.th,
                    theme.th_text,
                    self.effective_th_scroll(),
                    true,
                    Some(&self.annotations),
                );
            }
            _ => {
                let cols = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(48), Constraint::Percentage(52)])
                    .split(body);
                self.render_pane(
                    f,
                    cols[0],
                    theme,
                    "日本語 (raw)",
                    &self.ja,
                    theme.ja_text,
                    self.scroll,
                    false,
                    None,
                );
                self.render_pane(
                    f,
                    cols[1],
                    theme,
                    "ไทย (translated)",
                    &self.th,
                    theme.th_text,
                    self.effective_th_scroll(),
                    true,
                    Some(&self.annotations),
                );
            }
        }

        self.render_status(f, rows[1], theme);
    }

    fn effective_th_scroll(&self) -> u16 {
        if self.sync {
            self.scroll
        } else {
            self.th_scroll
        }
    }

    fn current_annotation_line(&self) -> u32 {
        let line_count = self.th.lines().count().max(1) as u32;
        (u32::from(self.effective_th_scroll()) + 1).clamp(1, line_count)
    }

    #[allow(clippy::too_many_arguments)]
    fn render_pane(
        &self,
        f: &mut Frame,
        area: Rect,
        theme: &Theme,
        title: &str,
        content: &str,
        fg: ratatui::style::Color,
        scroll: u16,
        is_thai: bool,
        annotations: Option<&[ReaderAnnotation]>,
    ) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(theme.rule))
            .title(Span::styled(
                format!(" {title} "),
                Style::default().fg(theme.ink_soft),
            ))
            .style(Style::default().bg(theme.bg_panel));
        let inner = block.inner(area);
        f.render_widget(block, area);

        // Render the chapter Markdown as styled prose (bold/italic, headings,
        // image chips, …) rather than leaking raw `**`/`![]()` syntax.
        let annotated;
        let render_content = if is_thai && self.show_annotations {
            if let Some(annotations) = annotations.filter(|notes| !notes.is_empty()) {
                annotated = annotate_markdown(content, annotations);
                annotated.as_str()
            } else {
                content
            }
        } else {
            content
        };
        let lines = crate::ui::markdown::render(render_content, fg, theme, inner.width as usize);

        let mut para = Paragraph::new(lines)
            .scroll((scroll, 0))
            .style(Style::default().bg(theme.bg_panel));
        if self.wrap {
            // Always trim:false — Thai has no inter-word spaces so trim:true would
            // produce long unbroken runs (risks.txt); JA leading spaces in dialogue
            // blocks are likewise intentional. `is_thai` is kept for callers' intent.
            let _ = is_thai;
            para = para.wrap(Wrap { trim: false });
        }
        f.render_widget(para, inner);
    }

    fn render_status(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        let sync_glyph = if self.sync { "●" } else { "○" };
        let wrap_glyph = if self.wrap { "●" } else { "○" };
        let note_glyph = if self.show_annotations { "●" } else { "○" };
        let mode = match self.layout_mode {
            MODE_JA => "JA",
            MODE_TH => "TH",
            _ => "split",
        };
        let line = Line::from(vec![
            Span::styled("  sync ", Style::default().fg(theme.ink_faint)),
            Span::styled(
                sync_glyph,
                Style::default().fg(if self.sync {
                    theme.status_done
                } else {
                    theme.ink_faint
                }),
            ),
            Span::styled(" · wrap ", Style::default().fg(theme.ink_faint)),
            Span::styled(
                wrap_glyph,
                Style::default().fg(if self.wrap {
                    theme.status_done
                } else {
                    theme.ink_faint
                }),
            ),
            Span::styled(" · layout ", Style::default().fg(theme.ink_faint)),
            Span::styled(mode, Style::default().fg(theme.accent_soft)),
            Span::styled(" · notes ", Style::default().fg(theme.ink_faint)),
            Span::styled(
                format!("{} {}", note_glyph, self.annotations.len()),
                Style::default().fg(if self.show_annotations {
                    theme.status_done
                } else {
                    theme.ink_faint
                }),
            ),
            Span::styled(" · line ", Style::default().fg(theme.ink_faint)),
            Span::styled(
                self.current_annotation_line().to_string(),
                Style::default().fg(theme.accent_soft),
            ),
            Span::styled(" · ch ", Style::default().fg(theme.ink_faint)),
            Span::styled(
                format!("{:03}", self.chapter),
                Style::default()
                    .fg(theme.ink_soft)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        f.render_widget(
            Paragraph::new(line).style(Style::default().bg(theme.bg)),
            area,
        );
    }

    pub fn hints(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("↑↓", "scroll"),
            ("[ ]", "chapter"),
            ("z", "sync"),
            ("o", "layout"),
            ("w", "wrap"),
            ("n", "note"),
            ("N", "notes"),
            ("Q", "QA"),
        ]
    }
}

fn annotate_markdown(content: &str, annotations: &[ReaderAnnotation]) -> String {
    let mut by_line: std::collections::BTreeMap<u32, Vec<&ReaderAnnotation>> =
        std::collections::BTreeMap::new();
    for annotation in annotations {
        by_line
            .entry(annotation.line.max(1))
            .or_default()
            .push(annotation);
    }

    let mut out = String::new();
    let mut line_no = 1u32;
    for line in content.lines() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
        push_annotations_for_line(&mut out, line_no, &mut by_line);
        line_no = line_no.saturating_add(1);
    }

    if content.is_empty() {
        push_annotations_for_line(&mut out, 1, &mut by_line);
    }

    // Notes anchored past EOF (for example after a hand edit shrank the file) stay
    // visible at the tail with their original line number.
    for (line, notes) in by_line {
        for note in notes {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("> 📝 L{line}: {}", inline_note_text(&note.note)));
        }
    }

    out
}

fn push_annotations_for_line(
    out: &mut String,
    line: u32,
    by_line: &mut std::collections::BTreeMap<u32, Vec<&ReaderAnnotation>>,
) {
    let Some(notes) = by_line.remove(&line) else {
        return;
    };
    for note in notes {
        out.push('\n');
        out.push_str("> 📝 ");
        out.push_str(&inline_note_text(&note.note));
    }
}

fn inline_note_text(note: &str) -> String {
    note.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl Default for ReaderScreen {
    fn default() -> Self {
        Self::new()
    }
}
