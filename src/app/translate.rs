//! src/app/translate.rs — the Translate / Run screen (3 訳): the only live view.
//!
//! Top panel: current chapter + chunk N/M LineGauge + three agent activity lines
//! (role badge, spinner on the active one) + a token/retry meter. Bottom panel: the
//! streaming translation preview side-by-side with the JA source and an indigo caret.

use std::hash::{Hash, Hasher};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::model::{
    AgentRole, AppEvent, ChapterKind, ChapterStatus, ReviewVerdict, ServiceTier, ThoughtProcess,
    ThoughtProcessField, UsageStats,
};
use crate::theme::{self, Theme, agent_badge, agent_spinner_frame, spinner_frame, status_glyph};
use crate::ui::mouse::{MouseGesture, MouseInput};
use crate::ui::text::{col_width, pad_to_cols, truncate_cols, truncate_tail_cols};
use crate::ui::widgets::render_line_gauge;

use super::action_table::{self, Act};
use super::{Action, Screen};

#[derive(Clone)]
pub struct QueueRow {
    pub vol: u32,
    pub number: u32,
    pub title: String,
    pub kind: ChapterKind,
    pub status: ChapterStatus,
    pub source_segments: u32,
    pub running: bool,
    pub est_pos: usize,
}

/// Whether a pipeline run is live, so the screen stops claiming "Now translating"
/// when nothing is running.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RunPhase {
    Idle,
    Preparing,
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
    pub(crate) agent_lines: [String; 3],
    /// Which agent line is "active" (drives the spinner). 0..=2.
    pub(crate) active_agent: usize,
    pub(crate) current_chapter: Option<u32>,
    pub(crate) chapter_title: String,
    /// (current chunk index 1-based, total chunks).
    pub(crate) chunk: (usize, usize),
    /// Accumulated translated preview text.
    pub(crate) preview: String,
    pending_preview_separator: bool,
    pub(crate) thought_reasoning: String,
    pub(crate) thought_scene: String,
    pub(crate) thought_glossary: String,
    thought_chunk: Option<usize>,
    thought_attempt: Option<u32>,
    /// Whole-run cumulative usage (tokens / cost / tool calls), from `UsageUpdate`.
    pub(crate) run: UsageStats,
    /// Current chapter's running usage sub-total, from `UsageUpdate`.
    pub(crate) chapter: UsageStats,
    pub(crate) retries: u32,
    pub(crate) last_note: String,
    /// Idle until a run starts; drives the header, border, and spinners.
    phase: RunPhase,
    /// Mouse hit-test rects, refreshed every frame: the 3-row agent block (click
    /// a line to focus that agent) and the streaming preview pane.
    agent_area: Rect,
    preview_area: Rect,
    pub(crate) queue: Vec<QueueRow>,
    queue_sel: usize,
    queue_focused: bool,
    queue_area: Rect,
    queue_offset: usize,
    /// Memoized Markdown render of the streaming preview, so a static (paused/idle)
    /// preview is not re-parsed on every 100 ms tick. Keyed on the preview's length +
    /// tail, so each streamed append rebuilds but a steady pane reuses the lines.
    preview_cache: crate::ui::markdown::RenderCache,
}

/// Action ids for this screen's table. Stable within the screen: they are also
/// the zone index every one of its controls registers under.
const T_PAUSE: u16 = 0;
const T_STOP: u16 = 1;
const T_FOLLOW: u16 = 2;
const T_AGENT: u16 = 3;
const T_MOVE_UP: u16 = 4;
const T_MOVE_DOWN: u16 = 5;
const T_REMOVE: u16 = 6;
const T_SORT: u16 = 7;
const T_OPEN: u16 = 8;

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
            thought_reasoning: String::new(),
            thought_scene: String::new(),
            thought_glossary: String::new(),
            thought_chunk: None,
            thought_attempt: None,
            run: UsageStats::default(),
            chapter: UsageStats::default(),
            retries: 0,
            last_note: String::new(),
            phase: RunPhase::Idle,
            agent_area: Rect::default(),
            preview_area: Rect::default(),
            queue: Vec::new(),
            queue_sel: 0,
            queue_focused: false,
            queue_area: Rect::default(),
            queue_offset: 0,
            preview_cache: crate::ui::markdown::RenderCache::default(),
        }
    }

    pub fn usage_snapshots(
        &self,
    ) -> (
        crate::remote::protocol::UsageSnapshot,
        crate::remote::protocol::UsageSnapshot,
    ) {
        ((&self.run).into(), (&self.chapter).into())
    }

    pub fn set_queue(&mut self, rows: Vec<QueueRow>) {
        self.queue = rows;
        let pending = self.pending_count();
        if pending == 0 {
            self.queue_sel = 0;
            self.queue_focused = false;
        } else if self.queue_sel >= pending {
            self.queue_sel = pending - 1;
        }
    }

    fn pending_count(&self) -> usize {
        self.queue.iter().filter(|r| !r.running).count()
    }

    fn pending_identity(&self, i: usize) -> Option<(u32, u32)> {
        self.queue
            .iter()
            .filter(|r| !r.running)
            .nth(i)
            .map(|r| (r.vol, r.number))
    }

    /// Set the live header's chapter title (resolved by the App from the active
    /// project when a `ChapterStarted` event arrives).
    pub fn set_chapter_title(&mut self, title: String) {
        self.chapter_title = title;
    }

    /// GUI-facing run phase label (Idle / Preparing / Running / Paused).
    pub(crate) fn phase_label(&self) -> &'static str {
        match self.phase {
            RunPhase::Idle => "Idle",
            RunPhase::Preparing => "Preparing",
            RunPhase::Running => "Running",
            RunPhase::Paused => "Paused",
        }
    }

    pub(crate) fn is_running(&self) -> bool {
        matches!(self.phase, RunPhase::Running | RunPhase::Preparing)
    }

    pub(crate) fn is_paused(&self) -> bool {
        matches!(self.phase, RunPhase::Paused)
    }

    /// The agent currently doing work, so the tab bar can mirror its spinner.
    pub fn active_agent_role(&self) -> AgentRole {
        match self.active_agent {
            0 => AgentRole::Orchestrator,
            2 => AgentRole::Reviewer,
            _ => AgentRole::Translator,
        }
    }

    pub fn on_app_event(&mut self, ev: &AppEvent) {
        match ev {
            AppEvent::PrepassStarted { vol } => {
                if self.phase == RunPhase::Idle {
                    self.run = UsageStats::default();
                    self.retries = 0;
                }
                self.chapter = UsageStats::default();
                self.phase = RunPhase::Preparing;
                self.current_chapter = None;
                self.chapter_title = format!("Vol.{vol:02} pre-scan");
                self.preview.clear();
                self.pending_preview_separator = false;
                self.clear_thought_process();
                self.scroll = 0;
                self.chunk = (0, 0);
                self.active_agent = 1;
                self.agent_lines = [
                    "sampling raw chapters".to_string(),
                    "extracting cast, terms, and style".to_string(),
                    "queued".to_string(),
                ];
                self.last_note = format!("Vol.{vol:02} pre-scan started");
            }
            AppEvent::PrepassFinished {
                vol,
                characters,
                terms,
                examples,
            } => {
                self.active_agent = 1;
                self.agent_lines[0] = "references ready".to_string();
                self.agent_lines[1] =
                    format!("seeded {characters} char · {terms} term · {examples} style");
                self.last_note =
                    format!("Vol.{vol:02} pre-scan seeded {characters}/{terms}/{examples}");
            }
            AppEvent::PrepassFailed { vol, reason } => {
                self.active_agent = 1;
                self.agent_lines[1] =
                    format!("pre-scan skipped · {}", truncate_one_line(reason, 52));
                self.last_note = format!("Vol.{vol:02} pre-scan skipped");
            }
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
                self.clear_thought_process();
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
                self.clear_thought_process();
                self.thought_chunk = Some(chunk + 1);
                self.thought_attempt = Some(*attempt);
            }
            AppEvent::TranslatorReturned {
                thought_process,
                translated_preview,
                tokens,
                ..
            } => {
                self.active_agent = 1;
                self.agent_lines[1] =
                    format!("returned · {} tok", tokens.completion.max(tokens.total));
                self.set_thought_process(thought_process);
                if !translated_preview.is_empty() {
                    // `translated_preview` now carries the chunk's full multi-line translation.
                    // Separate successive chunks with a blank line so the preview
                    // reads as flowing prose instead of one run-on paragraph.
                    if self.pending_preview_separator
                        || (!self.preview.is_empty() && !self.preview.ends_with('\n'))
                    {
                        self.preview.push_str("\n\n");
                    }
                    self.pending_preview_separator = false;
                    self.append_preview(translated_preview);
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
            AppEvent::ThoughtProcessDelta {
                field,
                delta,
                chunk,
                attempt,
                ..
            } => {
                self.active_agent = 1;
                self.thought_chunk = Some(chunk + 1);
                self.thought_attempt = Some(*attempt);
                self.append_thought_process(*field, delta);
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
                ..
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

    fn clear_thought_process(&mut self) {
        self.thought_reasoning.clear();
        self.thought_scene.clear();
        self.thought_glossary.clear();
        self.thought_chunk = None;
        self.thought_attempt = None;
    }

    fn set_thought_process(&mut self, thought: &ThoughtProcess) {
        self.thought_scene = crate::ui::text::thai_display_safe(&thought.scene_analysis);
        self.thought_glossary = crate::ui::text::thai_display_safe(&thought.glossary_check);
    }

    fn append_thought_process(&mut self, field: ThoughtProcessField, delta: &str) {
        let safe = crate::ui::text::thai_display_safe(delta);
        let target = match field {
            ThoughtProcessField::ModelReasoning => &mut self.thought_reasoning,
            ThoughtProcessField::SceneAnalysis => &mut self.thought_scene,
            ThoughtProcessField::GlossaryCheck => &mut self.thought_glossary,
        };
        target.push_str(&safe);
        trim_string_tail(target, 4096, 3072);
    }

    fn thought_process_is_empty(&self) -> bool {
        self.thought_reasoning.trim().is_empty()
            && self.thought_scene.trim().is_empty()
            && self.thought_glossary.trim().is_empty()
    }

    /// `acts` is the resolved table the App serves, user bindings already
    /// folded in — the screen must not rebuild it, or a rebound key would work
    /// at the App level and the original would keep working here.
    pub fn handle_key(&mut self, key: KeyEvent, acts: &[Act]) -> Action {
        // Commands come from the table; panel focus, scrolling and the queue
        // cursor are navigation and stay here.
        match action_table::hit(acts, &key) {
            action_table::KeyHit::Run(id) => return self.run(id).unwrap_or(Action::None),
            action_table::KeyHit::Blocked => return Action::None,
            action_table::KeyHit::Miss => {}
        }
        match key.code {
            KeyCode::Char('g') => {
                if self.pending_count() > 0 {
                    self.queue_focused = !self.queue_focused;
                }
                Action::None
            }
            KeyCode::Esc if self.queue_focused => {
                self.queue_focused = false;
                Action::None
            }
            KeyCode::Char('j') | KeyCode::Down if self.queue_focused => {
                let pc = self.pending_count();
                if pc > 0 {
                    self.queue_sel = (self.queue_sel + 1).min(pc - 1);
                }
                Action::None
            }
            KeyCode::Char('k') | KeyCode::Up if self.queue_focused => {
                self.queue_sel = self.queue_sel.saturating_sub(1);
                Action::None
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.scroll_preview(1);
                Action::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.scroll_preview(-1);
                Action::None
            }
            _ => Action::None,
        }
    }

    /// This screen's commands, availability resolved for this frame. Drawn as
    /// the band under the pipeline and the queue row's verbs.
    pub fn actions(&self) -> Vec<Act> {
        use action_table::Accel;

        let pc = self.pending_count();
        let i = self.queue_sel.min(pc.saturating_sub(1));
        let agent = match self.active_agent {
            0 => "orchestrator",
            2 => "reviewer",
            _ => "translator",
        };
        vec![
            Act::toolbar(T_PAUSE, "pause", Accel::key('p')),
            Act::toolbar(T_STOP, "stop", Accel::key('s')),
            Act::toolbar(T_FOLLOW, "follow", Accel::key('f')).toggle(self.follow),
            Act::toolbar(T_AGENT, "agent", Accel::key('c'))
                .cycle()
                .value(agent),
            Act::row(T_MOVE_UP, "up", Accel::key('K'))
                .icon("▲")
                .when(pc > 0 && i > 0),
            Act::row(T_MOVE_DOWN, "down", Accel::key('J'))
                .icon("▼")
                .when(pc > 0 && i + 1 < pc),
            Act::row(T_REMOVE, "remove", Accel::key('x')).when(pc > 0),
            Act::menu(T_SORT, "sort queue", Accel::key('S')).when(pc > 0),
            Act::menu(T_OPEN, "open result", Accel::code(KeyCode::Enter)),
        ]
    }

    /// Run the action `id` stands for, however it was reached. `None` means
    /// no such action here.
    pub fn run(&mut self, id: u16) -> Option<Action> {
        let pc = self.pending_count();
        Some(match id {
            T_PAUSE => Action::PauseRun,
            T_STOP => Action::show_overlay(super::overlay::Overlay::confirm(
                "Stop the run?",
                "The current chunk finishes, then the pipeline halts.".to_string(),
                Action::StopRun,
            )),
            T_FOLLOW => {
                self.follow = !self.follow;
                // Leaving follow-mode: resolve the tail sentinel to a real offset.
                if !self.follow && self.scroll == u16::MAX {
                    self.scroll = self.last_bottom;
                }
                Action::None
            }
            T_AGENT => {
                self.active_agent = (self.active_agent + 1) % 3;
                Action::None
            }
            T_MOVE_DOWN => {
                if pc == 0 {
                    return Some(Action::None);
                }
                self.queue_focused = true;
                let i = self.queue_sel.min(pc - 1);
                match (i + 1 < pc, self.pending_identity(i)) {
                    (true, Some((vol, ch))) => {
                        self.queue_sel = i + 1; // follow the moved item
                        Action::QueueMoveDown { vol, ch }
                    }
                    _ => Action::None,
                }
            }
            T_MOVE_UP => {
                if pc == 0 {
                    return Some(Action::None);
                }
                self.queue_focused = true;
                let i = self.queue_sel.min(pc - 1);
                match (i > 0, self.pending_identity(i)) {
                    (true, Some((vol, ch))) => {
                        self.queue_sel = i - 1;
                        Action::QueueMoveUp { vol, ch }
                    }
                    _ => Action::None,
                }
            }
            T_REMOVE => {
                if pc == 0 {
                    return Some(Action::None);
                }
                self.queue_focused = true;
                let i = self.queue_sel.min(pc - 1);
                match self.pending_identity(i) {
                    Some((vol, ch)) => Action::DequeueChapter { vol, ch },
                    None => Action::None,
                }
            }
            T_SORT => {
                if pc == 0 {
                    return Some(Action::None);
                }
                self.queue_focused = true;
                self.queue_sel = 0;
                Action::SortQueue
            }
            T_OPEN => match self.current_chapter {
                Some(ch) => Action::OpenChapter { chapter: ch },
                None => Action::Goto(Screen::Reader),
            },
            _ => return None,
        })
    }

    /// Mouse: the wheel scrolls the preview (leaving follow-mode) — or, over the
    /// queue panel, walks the queue selection; clicking an agent line focuses that
    /// agent (its spinner moves there); double-clicking the preview opens the
    /// result in the Reader, matching Enter; right-click drops queue focus (Esc).
    pub fn handle_mouse(
        &mut self,
        m: MouseInput,
        zone: Option<crate::ui::kit::ZoneId>,
    ) -> Action {
        match m.gesture {
            MouseGesture::ScrollUp => {
                if m.in_rect(self.queue_area) {
                    self.queue_scroll(-1);
                } else {
                    self.scroll_preview(-3);
                }
                Action::None
            }
            MouseGesture::ScrollDown => {
                if m.in_rect(self.queue_area) {
                    self.queue_scroll(1);
                } else {
                    self.scroll_preview(3);
                }
                Action::None
            }
            MouseGesture::Click { double } => {
                if m.in_rect(self.queue_area) {
                    self.queue_focused = true;
                    // Pending rows register their own index, so nothing here
                    // re-derives one from the panel origin, the scroll offset
                    // and whether a running head is pinned above them.
                    if let Some(idx) = zone.and_then(|z| z.row_index()) {
                        let pc = self.pending_count();
                        if pc > 0 {
                            self.queue_sel = idx.min(pc - 1);
                        }
                    }
                    return Action::None;
                }
                if m.in_rect(self.agent_area) {
                    let i = (m.row - self.agent_area.y) as usize;
                    if i < 3 {
                        self.active_agent = i;
                    }
                    return Action::None;
                }
                if double && m.in_rect(self.preview_area) {
                    return match self.current_chapter {
                        Some(ch) => Action::OpenChapter { chapter: ch },
                        None => Action::Goto(Screen::Reader),
                    };
                }
                Action::None
            }
            MouseGesture::RightClick => {
                // On a queue row the router opens that row's menu straight
                // after, so the selection moves to it; anywhere else
                // right-click keeps its back-out meaning.
                match zone.and_then(|z| z.row_index()) {
                    Some(idx) => {
                        self.queue_focused = true;
                        let pc = self.pending_count();
                        if pc > 0 {
                            self.queue_sel = idx.min(pc - 1);
                        }
                    }
                    None => self.queue_focused = false,
                }
                Action::None
            }
        }
    }

    /// Move the queue cursor by `delta`, focusing the panel (so the highlight is
    /// visible) and clamping like the j/k key handlers.
    fn queue_scroll(&mut self, delta: i32) {
        let pc = self.pending_count();
        if pc == 0 {
            return;
        }
        self.queue_focused = true;
        let cur = self.queue_sel.min(pc - 1) as i32;
        self.queue_sel = (cur + delta).clamp(0, pc as i32 - 1) as usize;
    }

    /// Scroll the preview by `delta` lines, dropping follow-mode and resolving the
    /// tail sentinel first (mirrors the j/k key handlers).
    fn scroll_preview(&mut self, delta: i32) {
        self.follow = false;
        if self.scroll == u16::MAX {
            self.scroll = self.last_bottom;
        }
        self.scroll = if delta >= 0 {
            self.scroll.saturating_add(delta as u16)
        } else {
            self.scroll.saturating_sub((-delta) as u16)
        };
    }

    pub fn render(
        &mut self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        service_tier: Option<ServiceTier>,
        acts: &[Act],
    ) {
        // The band under the pipeline header carries the run controls, and the
        // tier disclaimer shares it when one is configured — the trade-off
        // stays visible mid-run rather than only back in Settings.
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(9),
                Constraint::Length(1),
                Constraint::Min(6),
            ])
            .split(area);
        let band = rows[1];
        let body = rows[2];

        let toolbar = crate::ui::kit::toolbar::Toolbar::new(acts)
            .has_menu(true)
            .render(ui, Rect { x: band.x + 1, ..band });

        // Hide the queue on narrow terminals so the preview stays usable.
        let (preview_col, queue_col) = if !self.queue.is_empty() && body.width >= 56 {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(40), Constraint::Length(34)])
                .split(body);
            (cols[0], Some(cols[1]))
        } else {
            self.queue_area = Rect::default();
            (body, None)
        };

        // The queue draws through the kit; everything else is still on the old
        // path and takes the frame back out afterwards.
        if let Some(col) = queue_col {
            self.render_queue(ui, col);
        }

        // The queue's selected row carries its own verbs, over the rectangle
        // the panel registered while drawing it.
        if queue_col.is_some()
            && let Some(rect) = ui.zones.rect_of(crate::ui::kit::ZoneId::row(self.queue_sel))
        {
            crate::ui::kit::toolbar::RowActions::new(acts).render(ui, rect);
        }

        let theme: &Theme = ui.theme;
        let frame = ui.frame_count;
        let f: &mut Frame = ui.frame;
        self.render_pipeline(f, rows[0], frame, theme);
        if let Some(tier) = service_tier {
            // Whatever the toolbar left, with a gap; nothing when it took the
            // row, rather than the two writing over each other.
            let x = band.x + 1 + toolbar.cols.saturating_add(2);
            let rest = Rect {
                x,
                width: (band.x + band.width).saturating_sub(x),
                ..band
            };
            self.render_tier_disclaimer(f, rest, tier, theme);
        }
        self.render_translation_body(f, preview_col, theme);
    }

    /// One-line, full-width banner naming the active OpenRouter tier and its
    /// speed/cost trade-off (flex = cheaper/slower, priority = faster/pricier).
    fn render_tier_disclaimer(&self, f: &mut Frame, area: Rect, tier: ServiceTier, theme: &Theme) {
        if area.height == 0 {
            return;
        }
        let (label, color) = match tier {
            ServiceTier::Flex => ("Flex", theme.status_warn),
            ServiceTier::Priority => ("Priority", theme.accent),
        };
        let head = format!(" ⚑ {label} tier");
        let desc_budget = (area.width as usize).saturating_sub(col_width(&head));
        let desc = truncate_cols(
            &format!(" · {}", ServiceTier::desc(Some(tier))),
            desc_budget,
        );
        let line = Line::from(vec![
            Span::styled(
                head,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(desc, Style::default().fg(theme.ink_faint)),
        ]);
        f.render_widget(
            Paragraph::new(line).style(Style::default().bg(theme.bg_panel)),
            area,
        );
    }

    /// The run queue: the active chapter pinned at the top, pending rows
    /// scrolling below it.
    ///
    /// Only pending rows register zones. The running chapter is not a thing the
    /// queue can reorder or remove — it has already started — so offering a
    /// click target for it would promise something the pipeline cannot honour.
    fn render_queue(&mut self, ui: &mut crate::ui::kit::Ui, area: Rect) {
        use crate::ui::kit::ZoneKind;
        use crate::ui::kit::card::Card;
        use crate::ui::kit::list::{self, ListState, Row as KitRow};

        let pending = self.pending_count();
        let inner = Card::new(" คิว · Queue ")
            .meta(format!("{pending}"))
            .accent(if self.queue_focused {
                ui.theme.border_focus
            } else {
                ui.theme.rule
            })
            .render(ui, area);
        self.queue_area = inner;
        if inner.height == 0 || inner.width < 6 {
            return;
        }
        if self.queue.is_empty() {
            ui.text(
                crate::ui::kit::ctx::row_at(inner, 0),
                " no queue",
                Style::default()
                    .fg(ui.theme.ink_faint)
                    .bg(ui.theme.bg_panel),
            );
            return;
        }

        let multi_vol = self.queue.iter().any(|r| r.vol != self.queue[0].vol);
        let width = inner.width as usize;
        let mut body = inner;

        if let Some(row) = self.queue.first().filter(|r| r.running).cloned() {
            let line = self.queue_line(&row, false, multi_vol, width, ui.theme);
            ui.line(
                crate::ui::kit::ctx::row_at(inner, 0),
                line,
                Style::default().bg(ui.theme.bg_panel),
            );
            body = Rect {
                y: inner.y + 1,
                height: inner.height.saturating_sub(1),
                ..inner
            };
        }
        if body.height == 0 {
            return;
        }

        let pend: Vec<QueueRow> = self
            .queue
            .iter()
            .filter(|r| !r.running)
            .cloned()
            .collect();
        let theme = ui.theme;
        let lines: Vec<Line<'static>> = pend
            .iter()
            .map(|r| self.queue_line(r, false, multi_vol, width, theme))
            .collect();

        let mut state = ListState::new();
        state.select(self.queue_focused.then_some(self.queue_sel));
        list::render(
            ui,
            body,
            &mut state,
            lines.len(),
            list::Opts {
                rail: true,
                scrollbar: true,
                kind: ZoneKind::Row,
                id_base: 0,
            },
            |i| KitRow::new(lines[i].clone()),
        );
        self.queue_offset = state.offset();
    }

    /// Queue row, width-budgeted in display columns for CJK/Thai titles.
    /// One queue row. Every span is built from an owned `format!`, so the
    /// line does not borrow the row it describes and can outlive it — which is
    /// what lets the rows be built before the list takes its borrow.
    fn queue_line(
        &self,
        row: &QueueRow,
        selected: bool,
        multi_vol: bool,
        width: usize,
        theme: &Theme,
    ) -> Line<'static> {
        let caret = if selected { "›" } else { " " };
        let pos = if row.running {
            " ▶ ".to_string()
        } else {
            format!("{:>2} ", row.est_pos)
        };
        let (glyph, glyph_color) = status_glyph(row.kind, row.status, theme);
        let vol = if multi_vol {
            format!("V{} ", row.vol)
        } else {
            String::new()
        };
        let chap = format!("ch{:03} ", row.number);
        let size = if matches!(row.kind, ChapterKind::ImageOnly) {
            "IMG".to_string()
        } else if row.source_segments > 0 {
            format!("{}句", row.source_segments)
        } else {
            String::new()
        };

        let prefix_w = col_width(caret) + col_width(&pos) + 2 + col_width(&vol) + col_width(&chap);
        let mut size_w = col_width(&size);
        let mut title_budget = width.saturating_sub(prefix_w + size_w + 1);
        if title_budget < 4 {
            size_w = 0;
            title_budget = width.saturating_sub(prefix_w);
        }
        let title = pad_to_cols(&truncate_cols(&row.title, title_budget), title_budget);

        let text_color = if row.running || selected {
            theme.ink
        } else {
            theme.ink_soft
        };
        let mut spans = vec![
            Span::styled(
                caret.to_string(),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                pos,
                Style::default().fg(if row.running {
                    theme.accent
                } else {
                    theme.ink_faint
                }),
            ),
            Span::styled(format!("{glyph} "), Style::default().fg(glyph_color)),
        ];
        if !vol.is_empty() {
            spans.push(Span::styled(vol, Style::default().fg(theme.ink_faint)));
        }
        spans.push(Span::styled(chap, Style::default().fg(theme.ink_faint)));
        spans.push(Span::styled(title, Style::default().fg(text_color)));
        if size_w > 0 {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(size, Style::default().fg(theme.ink_faint)));
        }
        Line::from(spans)
    }

    fn render_translation_body(&mut self, f: &mut Frame, area: Rect, theme: &Theme) {
        if self.show_thought_panel(area) {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(6), Constraint::Min(4)])
                .split(area);
            self.render_thought_panel(f, rows[0], theme);
            self.render_preview(f, rows[1], theme);
        } else {
            self.render_preview(f, area, theme);
        }
    }

    fn show_thought_panel(&self, area: Rect) -> bool {
        matches!(self.phase, RunPhase::Running | RunPhase::Paused)
            && area.width >= 34
            && area.height >= 12
    }

    fn render_thought_panel(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        let border = if self.active_agent == 1 && matches!(self.phase, RunPhase::Running) {
            theme.accent
        } else {
            theme.rule
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(border))
            .title(Span::styled(
                " 翻訳者 · live reasoning ",
                Style::default().fg(theme.ink_soft),
            ))
            .style(Style::default().bg(theme.bg_panel));
        let inner = block.inner(area);
        f.render_widget(block, area);
        if inner.height == 0 || inner.width < 8 {
            return;
        }

        let mut lines = Vec::new();
        let width = inner.width as usize;
        if let Some(context) = self.thought_context_line(width, theme) {
            lines.push(context);
        }
        if self.thought_process_is_empty() {
            lines.push(Line::from(Span::styled(
                " waiting for translator reasoning",
                Style::default().fg(theme.ink_faint),
            )));
        } else {
            if !self.thought_reasoning.trim().is_empty() {
                lines.push(reasoning_row(
                    "current",
                    &self.thought_reasoning,
                    width,
                    theme.accent,
                    theme.ink_soft,
                    theme,
                ));
            }
            lines.push(thought_row(
                "scene",
                &self.thought_scene,
                width,
                theme.accent,
                theme.ink_soft,
            ));
            lines.push(thought_row(
                "glossary",
                &self.thought_glossary,
                width,
                theme.status_working,
                theme.ink_soft,
            ));
        }

        lines.truncate(inner.height as usize);
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme.bg_panel)),
            inner,
        );
    }

    fn thought_context_line(&self, width: usize, theme: &Theme) -> Option<Line<'static>> {
        let chunk = self.thought_chunk?;
        let attempt = self
            .thought_attempt
            .map(|n| format!(" · attempt {n}"))
            .unwrap_or_default();
        Some(Line::from(Span::styled(
            format!(
                " chunk {} / {}{}",
                chunk,
                self.chunk.1.max(chunk),
                truncate_cols(&attempt, width.saturating_sub(16))
            ),
            Style::default().fg(theme.ink_faint),
        )))
    }

    fn render_pipeline(&mut self, f: &mut Frame, area: Rect, frame: u64, theme: &Theme) {
        let (title, accent) = match self.phase {
            RunPhase::Preparing => (
                " 下準備 — Preparing volume pre-scan ".to_string(),
                theme.accent,
            ),
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
        let status_title = self.pipeline_status_title(area.width, &title, theme);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(accent))
            .title(Span::styled(
                title,
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.bg_panel));
        let block = if let Some(status_title) = status_title {
            block.title(status_title)
        } else {
            block
        };
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
            RunPhase::Preparing => format!("{} pre-scan", spinner_frame(frame)),
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
        self.agent_area = agent_area;
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
            let active = matches!(self.phase, RunPhase::Preparing | RunPhase::Running)
                && i == self.active_agent;
            let spin = if active {
                format!("{} ", agent_spinner_frame(*role, frame))
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
                Span::styled(spin, Style::default().fg(color)),
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
                format!(" Translation preview — streaming  ({follow_note}) "),
                Style::default().fg(theme.ink_soft),
            ))
            .style(Style::default().bg(theme.bg_panel));
        let inner = block.inner(area);
        f.render_widget(block, area);
        self.preview_area = inner;

        // Compose the preview lines: a faint placeholder when there's nothing yet,
        // otherwise the streaming translation rendered as Markdown. A trailing indigo
        // caret marks the live tail. The Markdown render is memoized so a paused /
        // idle pane is not re-parsed on every 100 ms tick; each streamed append
        // changes the preview's length + tail and so rebuilds.
        let mut lines: Vec<Line> = if self.preview.is_empty() {
            let msg = match self.phase {
                RunPhase::Idle => {
                    "No active run — start one from 棚 Project (t: chapter · T: volume)."
                }
                RunPhase::Paused => "Paused.",
                RunPhase::Preparing => "…pre-scanning this volume before the first chunk…",
                RunPhase::Running => "…waiting for the first chunk…",
            };
            vec![Line::from(Span::styled(
                msg.to_string(),
                Style::default().fg(theme.ink_faint),
            ))]
        } else {
            let preview = &self.preview;
            let width = inner.width as usize;
            let fg = theme.translated_text;
            let mut h = std::collections::hash_map::DefaultHasher::new();
            preview.len().hash(&mut h);
            preview_tail(preview).hash(&mut h);
            width.hash(&mut h);
            fg.hash(&mut h);
            crate::ui::markdown::theme_fingerprint(theme).hash(&mut h);
            let key = h.finish();
            self.preview_cache
                .lines(key, || {
                    crate::ui::markdown::render(preview, fg, theme, width)
                })
                .to_vec()
        };

        let caret = Span::styled("▏", Style::default().fg(theme.stream_cursor));
        match lines.last_mut() {
            Some(last) => last.push_span(caret),
            None => lines.push(Line::from(caret)),
        }

        // Resolve the "tail-follow" sentinel scroll into a concrete offset.
        // Count display rows after soft-wrap, not logical markdown lines.
        let total_lines = crate::ui::markdown::wrapped_rows(&lines, inner.width as usize)
            .min(u16::MAX as usize) as u16;
        let view_h = inner.height;
        // Remember the real bottom so the key handlers can normalize the sentinel.
        self.last_bottom = total_lines.saturating_sub(view_h);
        let scroll = if self.follow {
            self.last_bottom
        } else {
            self.scroll.min(self.last_bottom)
        };

        let para = Paragraph::new(lines)
            // Thai has no inter-word spaces: trim:false keeps runs intact (risks.txt).
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .style(Style::default().bg(theme.bg_panel));
        f.render_widget(para, inner);
        crate::ui::widgets::render_panel_scrollbar(
            f,
            area,
            total_lines as usize,
            scroll as usize,
            theme,
        );
    }

    fn pipeline_status_title(
        &self,
        width: u16,
        left_title: &str,
        theme: &Theme,
    ) -> Option<Line<'static>> {
        if self.last_note.is_empty() {
            return None;
        }

        let title_area = width.saturating_sub(2) as usize;
        let budget = title_area.saturating_sub(col_width(left_title) + 2);
        let note_budget = budget.saturating_sub(col_width(" ✓  "));
        if note_budget < 8 {
            return None;
        }

        Some(
            Line::from(Span::styled(
                format!(" ✓ {} ", truncate_cols(&self.last_note, note_budget)),
                Style::default().fg(theme.status_done),
            ))
            .alignment(Alignment::Right),
        )
    }

    /// Navigation only: the run controls and the queue verbs are drawn now.
    pub fn hints(&self) -> &'static [(&'static str, &'static str)] {
        if self.queue_focused {
            &[("↑↓", "queue"), ("g/esc", "unfocus")]
        } else {
            &[("↑↓", "scroll"), ("g", "queue")]
        }
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

fn trim_string_tail(s: &mut String, max: usize, keep: usize) {
    if s.len() <= max {
        return;
    }
    let cut = s.len().saturating_sub(keep);
    let mut idx = cut;
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    *s = s[idx..].to_string();
}

fn thought_row(
    label: &str,
    text: &str,
    width: usize,
    label_color: Color,
    text_color: Color,
) -> Line<'static> {
    let prefix = format!(" {label:<8}");
    let body_budget = width.saturating_sub(col_width(&prefix));
    let body = if text.trim().is_empty() {
        "…".to_string()
    } else {
        truncate_one_line(text.trim(), body_budget)
    };
    Line::from(vec![
        Span::styled(
            prefix,
            Style::default()
                .fg(label_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(body, Style::default().fg(text_color)),
    ])
}

fn reasoning_row(
    label: &str,
    text: &str,
    width: usize,
    label_color: Color,
    text_color: Color,
    theme: &Theme,
) -> Line<'static> {
    let prefix = format!(" {label:<8}");
    let body_budget = width.saturating_sub(col_width(&prefix));
    let markdown = reasoning_markdown(text);
    let spans = crate::ui::markdown::render(&markdown, text_color, theme, body_budget)
        .into_iter()
        .next()
        .map(|line| line.spans)
        .unwrap_or_default();
    let mut out = vec![Span::styled(
        prefix,
        Style::default()
            .fg(label_color)
            .add_modifier(Modifier::BOLD),
    )];
    out.extend(truncate_reasoning_spans(
        spans,
        body_budget,
        Style::default().fg(text_color),
    ));
    Line::from(out)
}

fn reasoning_markdown(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_reasoning_spans(
    spans: Vec<Span<'static>>,
    max_cols: usize,
    ellipsis_style: Style,
) -> Vec<Span<'static>> {
    let total = spans
        .iter()
        .map(|span| col_width(span.content.as_ref()))
        .sum::<usize>();
    if total <= max_cols {
        return spans;
    }
    if max_cols == 0 {
        return Vec::new();
    }

    let mut remaining = max_cols - 1;
    let mut tail = Vec::new();
    for span in spans.into_iter().rev() {
        let width = col_width(span.content.as_ref());
        if width <= remaining {
            remaining -= width;
            tail.push(span);
            continue;
        }
        let content = truncate_tail_cols(span.content.as_ref(), remaining);
        let content = content.strip_prefix('…').unwrap_or(&content);
        if !content.is_empty() {
            tail.push(Span::styled(content.to_string(), span.style));
        }
        break;
    }
    tail.reverse();
    let mut out = vec![Span::styled("…", ellipsis_style)];
    out.extend(tail);
    out
}

fn human_tok(n: u32) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// The last ~96 bytes of `s`, snapped to a UTF-8 boundary. Combined with the full
/// length, this distinguishes the append-only growth of the streaming preview cheaply
/// — enough to key its render cache without hashing the whole (growing) buffer.
fn preview_tail(s: &str) -> &str {
    let lower = s.len().saturating_sub(96);
    let start = (lower..=s.len())
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(s.len());
    &s[start..]
}

#[cfg(test)]
mod queue_panel_tests {
    use super::*;

    /// Press a key the way `App` does: resolve the table first, then dispatch
    /// from it. The screen no longer builds its own.
    fn press(screen: &mut TranslateScreen, k: KeyEvent) -> Action {
        let acts = screen.actions();
        screen.handle_key(k, &acts)
    }
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty())
    }

    fn rows() -> Vec<QueueRow> {
        vec![
            QueueRow {
                vol: 1,
                number: 3,
                title: "影の中で".into(),
                kind: ChapterKind::Prose,
                status: ChapterStatus::Translating,
                source_segments: 120,
                running: true,
                est_pos: 1,
            },
            QueueRow {
                vol: 1,
                number: 4,
                title: "黎明とそのさき、とても長い章のタイトル".into(),
                kind: ChapterKind::Prose,
                status: ChapterStatus::Pending,
                source_segments: 88,
                running: false,
                est_pos: 2,
            },
            QueueRow {
                vol: 2,
                number: 7,
                title: "写真".into(),
                kind: ChapterKind::ImageOnly,
                status: ChapterStatus::Pending,
                source_segments: 0,
                running: false,
                est_pos: 3,
            },
        ]
    }

    #[test]
    fn renders_panel_at_several_widths_without_panic() {
        let tiers = [None, Some(ServiceTier::Flex), Some(ServiceTier::Priority)];
        for (w, h) in [(90u16, 24u16), (60, 16), (120, 40)] {
            for tier in tiers {
                let mut screen = TranslateScreen::new();
                screen.phase = RunPhase::Running;
                screen.queue_focused = true;
                screen.queue_sel = 1;
                screen.set_queue(rows());
                let acts = screen.actions();
                crate::ui::kit::ctx::draw_test(w, h, |ui, area| {
                    screen.render(ui, area, tier, &acts)
                });
            }
        }
    }

    #[test]
    fn prepass_events_show_preparation_and_preserve_run_usage() {
        let mut screen = TranslateScreen::new();
        screen.run.tokens.total = 99;

        screen.on_app_event(&AppEvent::PrepassStarted { vol: 2 });

        assert!(matches!(screen.phase, RunPhase::Preparing));
        assert_eq!(
            screen.run.tokens.total, 0,
            "fresh prepass starts a fresh run"
        );
        assert_eq!(screen.current_chapter, None);
        assert_eq!(screen.chapter_title, "Vol.02 pre-scan");
        assert!(screen.agent_lines[1].contains("extracting"));

        let mut run = UsageStats::default();
        run.tokens.prompt = 7;
        run.tokens.completion = 5;
        run.tokens.total = 12;
        screen.on_app_event(&AppEvent::UsageUpdate {
            run,
            chapter: UsageStats::default(),
        });
        screen.on_app_event(&AppEvent::PrepassFinished {
            vol: 2,
            characters: 3,
            terms: 4,
            examples: 2,
        });

        assert!(screen.last_note.contains("Vol.02 pre-scan seeded"));
        screen.on_app_event(&AppEvent::ChapterStarted { chapter: 1 });

        assert!(matches!(screen.phase, RunPhase::Running));
        assert_eq!(
            screen.run.tokens.total, 12,
            "chapter start must not discard prepass usage"
        );
        assert_eq!(screen.chapter.tokens.total, 0);
        assert_eq!(screen.current_chapter, Some(1));
    }

    #[test]
    fn thought_process_events_fill_panel_state() {
        let mut screen = TranslateScreen::new();
        screen.on_app_event(&AppEvent::ChapterStarted { chapter: 1 });
        screen.on_app_event(&AppEvent::ChunkStarted {
            chapter: 1,
            chunk: 0,
            total: 3,
            est_tokens: 120,
        });
        screen.on_app_event(&AppEvent::TranslatorRequested {
            chapter: 1,
            chunk: 0,
            attempt: 1,
        });
        screen.on_app_event(&AppEvent::ThoughtProcessDelta {
            chapter: 1,
            chunk: 0,
            attempt: 1,
            field: ThoughtProcessField::ModelReasoning,
            delta: "checking context ".into(),
        });
        screen.on_app_event(&AppEvent::ThoughtProcessDelta {
            chapter: 1,
            chunk: 0,
            attempt: 1,
            field: ThoughtProcessField::SceneAnalysis,
            delta: "tone".into(),
        });
        screen.on_app_event(&AppEvent::ThoughtProcessDelta {
            chapter: 1,
            chunk: 0,
            attempt: 1,
            field: ThoughtProcessField::GlossaryCheck,
            delta: "term".into(),
        });

        assert_eq!(screen.thought_reasoning, "checking context ");
        assert_eq!(screen.thought_scene, "tone");
        assert_eq!(screen.thought_glossary, "term");
        assert_eq!(screen.thought_chunk, Some(1));
        assert_eq!(screen.thought_attempt, Some(1));

        screen.on_app_event(&AppEvent::TranslatorReturned {
            chapter: 1,
            chunk: 0,
            attempt: 1,
            thought_process: ThoughtProcess {
                scene_analysis: "final tone".into(),
                glossary_check: "final term".into(),
            },
            translated_preview: String::new(),
            tokens: crate::model::TokenUsage::default(),
        });
        assert_eq!(screen.thought_reasoning, "checking context ");
        assert_eq!(screen.thought_scene, "final tone");
        assert_eq!(screen.thought_glossary, "final term");

        let acts = screen.actions();
        crate::ui::kit::ctx::draw_test(90, 24, |ui, area| screen.render(ui, area, None, &acts));
    }

    #[test]
    fn current_reasoning_row_keeps_the_live_tail() {
        let theme = crate::model::ThemeId::default().build();
        let line = reasoning_row(
            "current",
            "first thought\nlatest reasoning",
            20,
            Color::Cyan,
            Color::White,
            &theme,
        );
        let visible = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(visible.contains('…'));
        assert!(visible.ends_with("reasoning"));
        assert!(col_width(&visible) <= 20);
    }

    #[test]
    fn reasoning_row_renders_bold_and_hides_comment_dividers() {
        let theme = crate::model::ThemeId::default().build();
        let line = reasoning_row(
            "current",
            "**Inferring character names and traits** <!-- -->**Confirming character presence and POV** <!-- -->",
            120,
            Color::Cyan,
            Color::White,
            &theme,
        );
        let visible = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(!visible.contains("**"));
        assert!(!visible.contains("<!--"));
        assert!(line.spans.iter().any(|span| {
            span.content.contains("Inferring character names")
                && span.style.add_modifier.contains(Modifier::BOLD)
        }));
        assert!(line.spans.iter().any(|span| {
            span.content.contains("Confirming character presence")
                && span.style.add_modifier.contains(Modifier::BOLD)
        }));
    }

    #[test]
    fn set_queue_clamps_selection_and_counts_pending() {
        let mut screen = TranslateScreen::new();
        screen.set_queue(rows());
        assert_eq!(screen.pending_count(), 2, "running head is not pending");
        screen.queue_focused = true;
        screen.queue_sel = 9;
        screen.set_queue(rows());
        assert_eq!(screen.queue_sel, 1);
        screen.set_queue(Vec::new());
        assert!(!screen.queue_focused);
        assert_eq!(screen.queue_sel, 0);
    }

    #[test]
    fn keys_focus_move_sort_and_remove_pending_only() {
        let mut screen = TranslateScreen::new();
        screen.set_queue(rows());
        assert!(matches!(press(&mut screen, key('g')), Action::None));
        assert!(screen.queue_focused);
        match press(&mut screen, key('J')) {
            Action::QueueMoveDown { vol, ch } => assert_eq!((vol, ch), (1, 4)),
            other => panic!("expected QueueMoveDown, got {other:?}"),
        }
        assert_eq!(screen.queue_sel, 1);
        match press(&mut screen, key('K')) {
            Action::QueueMoveUp { vol, ch } => assert_eq!((vol, ch), (2, 7)),
            other => panic!("expected QueueMoveUp, got {other:?}"),
        }
        assert_eq!(screen.queue_sel, 0);
        assert!(matches!(press(&mut screen, key('S')), Action::SortQueue));
        match press(&mut screen, key('x')) {
            Action::DequeueChapter { vol, ch } => assert_eq!((vol, ch), (1, 4)),
            other => panic!("expected DequeueChapter, got {other:?}"),
        }
        // Esc unfocuses; j then scrolls the preview rather than the queue.
        assert!(matches!(
            press(&mut screen, KeyEvent::new(KeyCode::Esc, KeyModifiers::empty())),
            Action::None
        ));
        assert!(!screen.queue_focused);
    }

    #[test]
    fn jk_scroll_preview_when_queue_unfocused() {
        let mut screen = TranslateScreen::new();
        screen.set_queue(rows());
        assert!(!screen.queue_focused);
        assert!(matches!(press(&mut screen, key('j')), Action::None));
        assert!(matches!(press(&mut screen, key('k')), Action::None));
        assert!(!screen.queue_focused);
    }

    /// The wheel over the queue panel walks (and focuses) the queue selection;
    /// a right-click drops the focus again, like Esc.
    #[test]
    fn wheel_over_queue_moves_selection_and_right_click_unfocuses() {
        use crate::ui::mouse::{MouseGesture, MouseInput};

        let mut screen = TranslateScreen::new();
        screen.set_queue(rows());
        screen.queue_area = Rect {
            x: 60,
            y: 2,
            width: 20,
            height: 10,
        };
        let at = |gesture| MouseInput {
            gesture,
            col: 65,
            row: 4,
        };

        screen.handle_mouse(at(MouseGesture::ScrollDown), None);
        assert!(screen.queue_focused, "wheel over the queue focuses it");
        assert_eq!(screen.queue_sel, 1);
        screen.handle_mouse(at(MouseGesture::ScrollDown), None);
        assert_eq!(screen.queue_sel, 1, "clamps at the last pending row");
        screen.handle_mouse(at(MouseGesture::ScrollUp), None);
        assert_eq!(screen.queue_sel, 0);

        screen.handle_mouse(at(MouseGesture::RightClick), None);
        assert!(!screen.queue_focused, "right-click backs out of the queue");
    }
}
