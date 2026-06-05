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

use crate::theme::{self, Theme};
use crate::workspace::Workspace;

use super::Action;

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
        self.th = std::fs::read_to_string(ws.translated(chapter))
            .unwrap_or_else(|_| "（ยังไม่มีคำแปล — not translated yet）".to_string());
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
            KeyCode::Char('y') => {
                // Copy is environment-dependent; we acknowledge via a no-op Action
                // and let the App surface a toast through its normal channels. We
                // keep it a no-op here so the build never depends on a clipboard dep.
                Action::None
            }
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

        let lines: Vec<Line> = content
            .split('\n')
            .map(|l| Line::from(Span::styled(l.to_string(), Style::default().fg(fg))))
            .collect();

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
            ("y", "copy"),
        ]
    }
}

impl Default for ReaderScreen {
    fn default() -> Self {
        Self::new()
    }
}
