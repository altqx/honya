//! src/app/translate.rs — the Translate / Run screen (3 訳): the only live view.
//!
//! Top panel: current chapter + chunk N/M LineGauge + three agent activity lines
//! (role badge, spinner on the active one) + a token/retry meter. Bottom panel: the
//! streaming Thai preview side-by-side with the JA source and an indigo caret.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::model::{AgentRole, AppEvent, ReviewVerdict, UsageStats};
use crate::theme::{self, Theme, agent_badge, spinner_frame};
use crate::ui::text::truncate_cols;
use crate::ui::widgets::render_line_gauge;

use super::{Action, Screen};

/// Whether a pipeline run is live, so the screen stops claiming "Now translating"
/// when nothing is running.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RunPhase {
    Idle,
    Running,
    Paused,
}

pub struct TranslateScreen {
    follow: bool,
    scroll: u16,
    /// Last rendered bottom offset, used to normalize the `u16::MAX` tail-follow
    /// sentinel into a concrete offset the first time the user scrolls up.
    last_bottom: u16,
    /// [orchestrator, translator, reviewer] one-line activity strings.
    agent_lines: [String; 3],
    /// Which agent line is "active" (drives the spinner). 0..=2.
    active_agent: usize,
    current_chapter: Option<u32>,
    chapter_title: String,
    /// (current chunk index 1-based, total chunks).
    chunk: (usize, usize),
    /// Accumulated Thai preview text.
    preview: String,
    pending_preview_separator: bool,
    /// Whole-run cumulative usage (tokens / cost / tool calls), from `UsageUpdate`.
    run: UsageStats,
    /// Current chapter's running usage sub-total, from `UsageUpdate`.
    chapter: UsageStats,
    retries: u32,
    last_note: String,
    /// Idle until a run starts; drives the header, border, and spinners.
    phase: RunPhase,
}

impl TranslateScreen {
    pub fn new() -> Self {
        Self {
            follow: true,
            scroll: 0,
            last_bottom: 0,
            agent_lines: ["idle".to_string(), "idle".to_string(), "idle".to_string()],
            active_agent: 1,
            current_chapter: None,
            chapter_title: String::new(),
            chunk: (0, 0),
            preview: String::new(),
            pending_preview_separator: false,
            run: UsageStats::default(),
            chapter: UsageStats::default(),
            retries: 0,
            last_note: String::new(),
            phase: RunPhase::Idle,
        }
    }

    /// Set the live header's chapter title (resolved by the App from the active
    /// project when a `ChapterStarted` event arrives).
    pub fn set_chapter_title(&mut self, title: String) {
        self.chapter_title = title;
    }

    pub fn on_app_event(&mut self, ev: &AppEvent) {
        match ev {
            AppEvent::ChapterStarted { chapter } => {
                // A fresh run (we were idle): zero the run meter + retries so they
                // don't carry over from the last run.
                if self.phase == RunPhase::Idle {
                    self.run = UsageStats::default();
                    self.retries = 0;
                }
                // The chapter sub-total always resets at a chapter boundary.
                self.chapter = UsageStats::default();
                self.phase = RunPhase::Running;
                self.current_chapter = Some(*chapter);
                self.preview.clear();
                self.pending_preview_separator = false;
                self.scroll = 0;
                self.chunk = (0, 0);
                self.agent_lines = [
                    "queued".to_string(),
                    "queued".to_string(),
                    "queued".to_string(),
                ];
                self.last_note = format!("chapter {chapter} started");
            }
            AppEvent::ChapterChunked {
                chapter,
                total_chunks,
                ..
            } => {
                self.current_chapter = Some(*chapter);
                self.chunk = (self.chunk.0, *total_chunks);
                self.agent_lines[0] = format!("chunked into {total_chunks}");
                self.active_agent = 0;
            }
            AppEvent::ChunkStarted {
                chapter,
                chunk,
                total,
                ..
            } => {
                self.current_chapter = Some(*chapter);
                self.chunk = (chunk + 1, *total);
            }
            AppEvent::TranslatorRequested { chunk, attempt, .. } => {
                self.active_agent = 1;
                self.agent_lines[1] = format!("requesting chunk {} (attempt {attempt})", chunk + 1);
                self.pending_preview_separator =
                    !self.preview.is_empty() && !self.preview.ends_with('\n');
            }
            AppEvent::TranslatorReturned {
                thai_preview,
                tokens,
                ..
            } => {
                self.active_agent = 1;
                self.agent_lines[1] =
                    format!("returned · {} tok", tokens.completion.max(tokens.total));
                if !thai_preview.is_empty() {
                    // `thai_preview` now carries the chunk's full multi-line Thai.
                    // Separate successive chunks with a blank line so the preview
                    // reads as flowing prose instead of one run-on paragraph.
                    if self.pending_preview_separator
                        || (!self.preview.is_empty() && !self.preview.ends_with('\n'))
                    {
                        self.preview.push_str("\n\n");
                    }
                    self.pending_preview_separator = false;
                    self.append_preview(thai_preview);
                }
                // The authoritative running total arrives via UsageUpdate (emitted
                // right after this event), so don't accumulate here.
            }
            AppEvent::ReviewerRequested { .. } => {
                self.active_agent = 2;
                self.agent_lines[2] = "reviewing …".to_string();
            }
            AppEvent::ReviewerReturned {
                verdict, feedback, ..
            } => {
                self.active_agent = 2;
                self.agent_lines[2] = match verdict {
                    ReviewVerdict::Approve => "✓ approved".to_string(),
                    ReviewVerdict::Reject => {
                        let fb = feedback.as_deref().unwrap_or("revisions requested");
                        format!("✗ rejected · {}", truncate_one_line(fb, 60))
                    }
                };
            }
            AppEvent::ChunkRetry {
                attempt,
                max,
                feedback,
                ..
            } => {
                self.retries = self.retries.saturating_add(1);
                self.agent_lines[2] = format!(
                    "retry {attempt}/{max} · {}",
                    truncate_one_line(feedback, 50)
                );
            }
            AppEvent::ChunkCommitted {
                chunk,
                bytes_written,
                ..
            } => {
                self.last_note = format!("chunk {} committed · {bytes_written} B", chunk + 1);
            }
            AppEvent::ChunkNeedsReview {
                chunk,
                attempts,
                reason,
                ..
            } => {
                self.agent_lines[2] = format!(
                    "⚑ chunk {} committed unreviewed after {attempts} · {}",
                    chunk + 1,
                    truncate_one_line(reason, 36)
                );
                self.last_note = format!("chunk {} needs review", chunk + 1);
            }
            AppEvent::ToolInvoked { tool, summary, .. } => {
                self.active_agent = 0;
                self.agent_lines[0] = format!("{tool} · {}", truncate_one_line(summary, 50));
            }
            AppEvent::StreamDelta { role, delta, .. } => {
                match role {
                    AgentRole::Orchestrator => self.active_agent = 0,
                    AgentRole::Translator => self.active_agent = 1,
                    AgentRole::Reviewer => self.active_agent = 2,
                }
                if matches!(role, AgentRole::Translator) {
                    self.consume_pending_preview_separator();
                    self.append_preview(delta);
                }
            }
            AppEvent::UsageUpdate { run, chapter } => {
                self.run = *run;
                self.chapter = *chapter;
            }
            AppEvent::ChapterCompleted { chapter } => {
                self.last_note = format!("chapter {chapter} done");
            }
            AppEvent::PipelinePaused => {
                self.phase = RunPhase::Paused;
            }
            AppEvent::PipelineResumed => {
                self.phase = RunPhase::Running;
            }
            AppEvent::PipelineFinished {
                chapters_done,
                chapters_failed,
                chapters_need_review,
            } => {
                self.phase = RunPhase::Idle;
                let review = if *chapters_need_review > 0 {
                    format!(" · {chapters_need_review} need review")
                } else {
                    String::new()
                };
                self.last_note = format!(
                    "run finished · {chapters_done} done · {chapters_failed} failed{review}"
                );
            }
            _ => {}
        }
    }

    fn append_preview(&mut self, s: &str) {
        // Decompose Thai SARA AM up front so the streaming preview never renders a
        // width-2 single cell that desyncs the terminal (see ui::text).
        self.preview
            .push_str(&crate::ui::text::thai_display_safe(s));
        // Bound the preview so it never grows unbounded across a long run.
        if self.preview.len() > 16_384 {
            let cut = self.preview.len() - 12_288;
            // Cut on a char boundary.
            let mut idx = cut;
            while idx < self.preview.len() && !self.preview.is_char_boundary(idx) {
                idx += 1;
            }
            self.preview = self.preview[idx..].to_string();
        }
        if self.follow {
            // Keep the view pinned to the tail when following.
            self.scroll = u16::MAX;
        }
    }

    fn consume_pending_preview_separator(&mut self) {
        if self.pending_preview_separator {
            if !self.preview.is_empty() && !self.preview.ends_with('\n') {
                self.preview.push_str("\n\n");
            }
            self.pending_preview_separator = false;
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Char('p') => Action::PauseRun,
            KeyCode::Char('s') => Action::show_overlay(super::overlay::Overlay::confirm(
                "Stop the run?",
                "The current chunk finishes, then the pipeline halts.".to_string(),
                Action::StopRun,
            )),
            KeyCode::Char('f') => {
                self.follow = !self.follow;
                // Leaving follow-mode: resolve the tail sentinel to a real offset.
                if !self.follow && self.scroll == u16::MAX {
                    self.scroll = self.last_bottom;
                }
                Action::None
            }
            KeyCode::Char('c') => {
                self.active_agent = (self.active_agent + 1) % 3;
                Action::None
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.follow = false;
                if self.scroll == u16::MAX {
                    self.scroll = self.last_bottom;
                }
                self.scroll = self.scroll.saturating_add(1);
                Action::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.follow = false;
                if self.scroll == u16::MAX {
                    self.scroll = self.last_bottom;
                }
                self.scroll = self.scroll.saturating_sub(1);
                Action::None
            }
            KeyCode::Enter => {
                if let Some(ch) = self.current_chapter {
                    Action::OpenChapter { chapter: ch }
                } else {
                    Action::Goto(Screen::Reader)
                }
            }
            _ => Action::None,
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, frame: u64, theme: &Theme) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(9), Constraint::Min(6)])
            .split(area);

        self.render_pipeline(f, rows[0], frame, theme);
        self.render_preview(f, rows[1], theme);
    }

    fn render_pipeline(&self, f: &mut Frame, area: Rect, frame: u64, theme: &Theme) {
        let (title, accent) = match self.phase {
            RunPhase::Running => (
                match self.current_chapter {
                    Some(ch) => format!(" いま訳しているところ — Now translating · ch {ch} "),
                    None => " いま訳しているところ — Now translating ".to_string(),
                },
                theme.accent,
            ),
            RunPhase::Paused => (
                match self.current_chapter {
                    Some(ch) => format!(" 一時停止 — Paused · ch {ch} "),
                    None => " 一時停止 — Paused ".to_string(),
                },
                theme.status_warn,
            ),
            RunPhase::Idle => (
                match self.current_chapter {
                    Some(ch) => format!(" 訳 Translate — idle · last ch {ch} "),
                    None => " 訳 Translate — no active run ".to_string(),
                },
                theme.ink_faint,
            ),
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(accent))
            .title(Span::styled(
                title,
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.bg_panel));
        let inner = block.inner(area);
        f.render_widget(block, area);

        if inner.height < 5 {
            return;
        }

        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // chunk header
                Constraint::Length(1), // gauge
                Constraint::Length(3), // 3 agent lines
                Constraint::Length(2), // usage meter (run + chapter)
                Constraint::Min(0),
            ])
            .split(inner);

        // Chunk header: title + "chunk N / M  ⠹ working".
        let (cur, total) = self.chunk;
        let working = match self.phase {
            RunPhase::Idle => "idle".to_string(),
            RunPhase::Paused => "paused".to_string(),
            RunPhase::Running => {
                if total > 0 && cur > 0 && cur <= total {
                    format!("{} working", spinner_frame(frame))
                } else {
                    "starting…".to_string()
                }
            }
        };
        let head = Line::from(vec![
            Span::styled(
                truncate_cols(
                    if self.chapter_title.is_empty() {
                        "current chapter"
                    } else {
                        &self.chapter_title
                    },
                    (inner.width as usize).saturating_sub(28),
                ),
                Style::default().fg(theme.ink),
            ),
            Span::raw("  "),
            Span::styled(
                format!("chunk {cur} / {total}   "),
                Style::default().fg(theme.ink_soft),
            ),
            Span::styled(working, Style::default().fg(theme.status_working)),
        ]);
        f.render_widget(
            Paragraph::new(head).style(Style::default().bg(theme.bg_panel)),
            parts[0],
        );

        // Chunk LineGauge.
        let ratio = if total > 0 {
            cur as f64 / total as f64
        } else {
            0.0
        };
        render_line_gauge(f, parts[1], ratio.clamp(0.0, 1.0), "", theme);

        // Three agent lines.
        let roles = [
            AgentRole::Orchestrator,
            AgentRole::Translator,
            AgentRole::Reviewer,
        ];
        let agent_area = parts[2];
        for (i, role) in roles.iter().enumerate() {
            let (badge, color) = agent_badge(*role, theme);
            let line_area = Rect {
                x: agent_area.x,
                y: agent_area.y + i as u16,
                width: agent_area.width,
                height: 1,
            };
            if line_area.y >= agent_area.y + agent_area.height {
                break;
            }
            let active = self.phase == RunPhase::Running && i == self.active_agent;
            let spin = if active {
                format!("{} ", spinner_frame(frame))
            } else {
                "  ".to_string()
            };
            let body = truncate_cols(
                &self.agent_lines[i],
                (line_area.width as usize).saturating_sub(14),
            );
            let body_style = if active {
                Style::default().fg(theme.ink)
            } else {
                Style::default().fg(theme.ink_faint)
            };
            let line = Line::from(vec![
                Span::styled(format!(" {badge:<9} "), Style::default().fg(color)),
                Span::styled(spin, Style::default().fg(theme.status_working)),
                Span::styled(body, body_style),
            ]);
            f.render_widget(
                Paragraph::new(line).style(Style::default().bg(theme.bg_panel)),
                line_area,
            );
        }

        // Usage meter: whole-run total (row 1) + current-chapter sub-total (row 2),
        // each showing tokens (in/out/total), tool calls, and BYOK-aware USD.
        let sep = || Span::styled("   ·   ", Style::default().fg(theme.rule));
        let usage_spans = |label: &str, u: &UsageStats| {
            vec![
                Span::styled(format!(" {label:<7}"), Style::default().fg(theme.ink_faint)),
                Span::styled(
                    format!(
                        "in {} · out {} · total {}",
                        human_tok(u.tokens.prompt),
                        human_tok(u.tokens.completion),
                        human_tok(u.tokens.total)
                    ),
                    Style::default().fg(theme.ink_soft),
                ),
                sep(),
                Span::styled(
                    format!("tools {}", u.tool_calls),
                    Style::default().fg(theme.ink_soft),
                ),
                sep(),
                Span::styled(
                    format!("${:.4}", u.cost_usd),
                    Style::default().fg(theme.ink_faint),
                ),
            ]
        };

        let mut run_spans = usage_spans("run", &self.run);
        run_spans.push(sep());
        run_spans.push(Span::styled(
            format!("retries {}", self.retries),
            Style::default().fg(if self.retries > 0 {
                theme.status_warn
            } else {
                theme.ink_soft
            }),
        ));

        let meter = vec![
            Line::from(run_spans),
            Line::from(usage_spans("chap", &self.chapter)),
        ];
        f.render_widget(
            Paragraph::new(meter).style(Style::default().bg(theme.bg_panel)),
            parts[3],
        );
    }

    fn render_preview(&mut self, f: &mut Frame, area: Rect, theme: &Theme) {
        let follow_note = if self.follow {
            "f: following"
        } else {
            "f: paused"
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(theme.rule))
            .title(Span::styled(
                format!(" Thai preview — streaming  ({follow_note}) "),
                Style::default().fg(theme.ink_soft),
            ))
            .style(Style::default().bg(theme.bg_panel));
        let inner = block.inner(area);
        f.render_widget(block, area);

        // Compose the preview lines: a faint placeholder when there's nothing yet,
        // otherwise the streaming Thai rendered as Markdown. A trailing indigo
        // caret marks the live tail.
        let mut lines: Vec<Line> = if self.preview.is_empty() {
            let msg = match self.phase {
                RunPhase::Idle => {
                    "No active run — start one from 棚 Project (t: chapter · T: volume)."
                }
                RunPhase::Paused => "Paused.",
                RunPhase::Running => "…waiting for the first chunk…",
            };
            vec![Line::from(Span::styled(
                msg.to_string(),
                Style::default().fg(theme.ink_faint),
            ))]
        } else {
            crate::ui::markdown::render(&self.preview, theme.th_text, theme, inner.width as usize)
        };

        let caret = Span::styled("▏", Style::default().fg(theme.stream_cursor));
        match lines.last_mut() {
            Some(last) => last.push_span(caret),
            None => lines.push(Line::from(caret)),
        }

        // Resolve the "tail-follow" sentinel scroll into a concrete offset.
        let total_lines = lines.len() as u16;
        let view_h = inner.height;
        // Remember the real bottom so the key handlers can normalize the sentinel.
        self.last_bottom = total_lines.saturating_sub(view_h);
        let scroll = if self.follow {
            self.last_bottom
        } else {
            self.scroll.min(total_lines.saturating_sub(1))
        };

        let para = Paragraph::new(lines)
            // Thai has no inter-word spaces: trim:false keeps runs intact (risks.txt).
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .style(Style::default().bg(theme.bg_panel));
        f.render_widget(para, inner);

        // Last-note line at the very bottom, if there's room.
        if inner.height >= 2 && !self.last_note.is_empty() {
            let note_area = Rect {
                x: inner.x,
                y: inner.y + inner.height - 1,
                width: inner.width,
                height: 1,
            };
            f.render_widget(
                Paragraph::new(Span::styled(
                    format!(
                        " ✓ {}",
                        truncate_cols(&self.last_note, inner.width.saturating_sub(4) as usize)
                    ),
                    Style::default().fg(theme.status_done),
                ))
                .style(Style::default().bg(theme.bg_panel)),
                note_area,
            );
        }
    }

    pub fn hints(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("p", "pause"),
            ("s", "stop"),
            ("f", "follow"),
            ("↵", "open result"),
            ("c", "cycle agent"),
        ]
    }
}

impl Default for TranslateScreen {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// HELPERS
// ============================================================================

fn truncate_one_line(s: &str, max: usize) -> String {
    let one = s.replace('\n', " ");
    truncate_cols(&one, max)
}

fn human_tok(n: u32) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}
