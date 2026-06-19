//! Refine screen: transcript, chat input, `@` mentions, and slash commands.
//! The focused input captures globals until `Esc` releases it.

use std::hash::{Hash, Hasher};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::llm::{Message, Role};
use crate::model::{AppEvent, PlanStep, PlanStepStatus, Project};
use crate::theme::{self, Theme};
use crate::ui::input::{self, EditOpts, Edited};
use crate::ui::mouse::{MouseGesture, MouseInput};
use crate::ui::text::truncate_cols;
use crate::workspace::refine_session::SessionMeta;

use super::Action;
use super::overlay::Overlay;

/// Structured `@` mention used as both context and default tool scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MentionTarget {
    Volume { vol: u32 },
    Chapter { vol: u32, ch: u32 },
    Lexicon,
    Characters,
    Glossary,
    Style,
    Project,
    Recap,
}

impl MentionTarget {
    fn token(&self) -> String {
        match self {
            MentionTarget::Volume { vol } => format!("@v{vol}"),
            MentionTarget::Chapter { vol, ch } => format!("@v{vol}/c{ch}"),
            MentionTarget::Lexicon => "@lexicon".into(),
            MentionTarget::Characters => "@characters".into(),
            MentionTarget::Glossary => "@glossary".into(),
            MentionTarget::Style => "@style".into(),
            MentionTarget::Project => "@project".into(),
            MentionTarget::Recap => "@recap".into(),
        }
    }
}

fn resource_target(name: &str) -> Option<MentionTarget> {
    match name {
        "lexicon" => Some(MentionTarget::Lexicon),
        "characters" | "cast" => Some(MentionTarget::Characters),
        "glossary" | "terms" => Some(MentionTarget::Glossary),
        "style" => Some(MentionTarget::Style),
        "project" => Some(MentionTarget::Project),
        "recap" | "synopsis" => Some(MentionTarget::Recap),
        _ => None,
    }
}

fn parse_mention(tok: &str) -> Option<MentionTarget> {
    let tok = tok.trim();
    if tok.is_empty() {
        return None;
    }
    if let Some(rest) = tok.strip_prefix(['v', 'V']) {
        let mut parts = rest.splitn(2, '/');
        let vol: u32 = parts.next().unwrap_or("").parse().ok()?;
        if let Some(cpart) = parts.next() {
            let ch: u32 = cpart.trim_start_matches(['c', 'C']).parse().ok()?;
            return Some(MentionTarget::Chapter { vol, ch });
        }
        return Some(MentionTarget::Volume { vol });
    }
    resource_target(&tok.to_lowercase())
}

pub fn parse_scope(input: &str) -> Vec<MentionTarget> {
    let mut out = Vec::new();
    for raw in input.split_whitespace() {
        let Some(tok) = raw.strip_prefix('@') else {
            continue;
        };
        let tok = tok.trim_end_matches([',', '.', ';', ':', '!', '?']);
        if let Some(t) = parse_mention(tok)
            && !out.contains(&t)
        {
            out.push(t);
        }
    }
    out
}

const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/help", "list commands"),
    ("/clear", "clear this conversation"),
    ("/cancel", "stop the in-flight reply"),
    ("/new", "start a new conversation"),
    ("/sessions", "switch between conversations"),
    ("/rename", "rename this conversation"),
    ("/delete", "delete this conversation"),
    ("/model", "set the refine model"),
    ("/undo", "restore the last chapter edit"),
    ("/diff", "diff the last chapter edit"),
];

const RESOURCE_CANDS: &[(&str, &str)] = &[
    ("@lexicon", "cast + glossary"),
    ("@characters", "the cast"),
    ("@glossary", "terminology"),
    ("@style", "style guide"),
    ("@project", "project metadata"),
    ("@recap", "synopsis / recap"),
];

struct MentionCandidate {
    insert: String,
    label: String,
}

enum Popup {
    None,
    Mention {
        items: Vec<MentionCandidate>,
        sel: usize,
    },
    Slash {
        items: Vec<usize>,
        sel: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnRole {
    User,
    Assistant,
    Reasoning,
    Tool,
}

/// UI mirror of the persisted chat thread.
#[derive(Debug, Clone)]
pub struct Turn {
    pub role: TurnRole,
    pub text: String,
    pub streaming: bool,
}

impl Turn {
    fn user(text: String) -> Self {
        Self {
            role: TurnRole::User,
            text,
            streaming: false,
        }
    }
    fn assistant(text: String) -> Self {
        Self {
            role: TurnRole::Assistant,
            text,
            streaming: false,
        }
    }
    fn tool(text: String) -> Self {
        Self {
            role: TurnRole::Tool,
            text,
            streaming: false,
        }
    }
}

/// Rebuild display turns from stored messages, skipping raw tool results.
pub fn display_turns(messages: &[Message]) -> Vec<Turn> {
    let mut turns = Vec::new();
    for m in messages {
        match m.role {
            Role::User => {
                if let Some(c) = &m.content {
                    turns.push(Turn::user(user_display(c).to_string()));
                }
            }
            Role::Assistant => {
                if let Some(c) = &m.content
                    && !c.is_empty()
                {
                    turns.push(Turn::assistant(c.clone()));
                }
                for call in m.tool_calls.iter().flatten() {
                    // Plan calls render in the pinned panel.
                    if call.function.name != "update_plan" {
                        turns.push(Turn::tool(call.function.name.clone()));
                    }
                }
            }
            Role::Tool | Role::System => {}
        }
    }
    turns
}

fn user_display(content: &str) -> &str {
    content
        .split_once("\n\n(In scope:")
        .map(|(head, _)| head)
        .unwrap_or(content)
}

pub struct RefineScreen {
    pub conversation: Vec<Turn>,
    input: String,
    cursor: usize,
    focused: bool,
    popup: Popup,
    streaming: bool,
    scroll: u16,
    follow: bool,
    last_bottom: u16,
    last_scope: Vec<MentionTarget>,
    sessions: Vec<SessionMeta>,
    picker: Option<usize>,
    active_session: String,
    /// Live checklist from `update_plan`.
    plan: Vec<PlanStep>,
    /// Expands finished reasoning and tool detail; streaming reasoning is always shown.
    expanded: bool,
    /// A turn is in flight (between submit and done/error): drives the working line.
    in_flight: bool,
    turn_started: Option<std::time::Instant>,
    last_turn_elapsed: Option<std::time::Duration>,
    /// (input, output) tokens — `turn` resets each request, `session` accumulates.
    turn_tokens: (u32, u32),
    session_tokens: (u32, u32),
    /// Most recent prompt size = how full the model's context window is now.
    last_context: u32,
    /// The active refine model's context window (set when the session starts).
    context_max: u32,
    transcript_area: Rect,
    input_area: Rect,
    transcript_cache: crate::ui::markdown::RenderCache,
}

impl Default for RefineScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl RefineScreen {
    pub fn new() -> Self {
        Self {
            conversation: Vec::new(),
            input: String::new(),
            cursor: 0,
            focused: true,
            popup: Popup::None,
            streaming: false,
            scroll: 0,
            follow: true,
            last_bottom: 0,
            last_scope: Vec::new(),
            sessions: Vec::new(),
            picker: None,
            active_session: String::new(),
            plan: Vec::new(),
            expanded: false,
            in_flight: false,
            turn_started: None,
            last_turn_elapsed: None,
            turn_tokens: (0, 0),
            session_tokens: (0, 0),
            last_context: 0,
            context_max: 128_000,
            transcript_area: Rect::default(),
            input_area: Rect::default(),
            transcript_cache: crate::ui::markdown::RenderCache::default(),
        }
    }

    /// Consulted by `App::screen_is_capturing()` to suppress single-letter globals.
    pub fn is_capturing(&self) -> bool {
        self.focused || self.picker.is_some()
    }

    pub fn load_turns(&mut self, turns: Vec<Turn>, active_session: String) {
        self.conversation = turns;
        self.active_session = active_session;
        self.input.clear();
        self.cursor = 0;
        self.popup = Popup::None;
        self.picker = None;
        self.streaming = false;
        self.scroll = 0;
        self.follow = true;
        self.plan.clear();
        self.reset_meters();
    }

    /// Mark the start of a dispatched turn (called by App once the request is sent).
    pub fn begin_turn(&mut self) {
        self.in_flight = true;
        self.turn_started = Some(std::time::Instant::now());
        self.turn_tokens = (0, 0);
    }

    fn reset_meters(&mut self) {
        self.in_flight = false;
        self.turn_started = None;
        self.last_turn_elapsed = None;
        self.turn_tokens = (0, 0);
        self.session_tokens = (0, 0);
    }

    /// Record the active refine model's context window for the `ctx X/max` meter.
    pub fn set_context_max(&mut self, max: u32) {
        self.context_max = max.max(1);
    }

    /// End the in-flight turn, banking its elapsed time.
    fn finish_turn(&mut self) {
        if let Some(start) = self.turn_started.take() {
            self.last_turn_elapsed = Some(start.elapsed());
        }
        self.in_flight = false;
    }

    pub fn open_picker(&mut self, sessions: Vec<SessionMeta>, active_session: String) {
        self.active_session = active_session;
        let sel = sessions
            .iter()
            .position(|s| s.id == self.active_session)
            .unwrap_or(0);
        self.sessions = sessions;
        self.picker = Some(sel);
    }

    pub fn picker_open(&self) -> bool {
        self.picker.is_some()
    }

    pub fn clear(&mut self) {
        self.conversation.clear();
        self.input.clear();
        self.cursor = 0;
        self.popup = Popup::None;
        self.streaming = false;
        self.scroll = 0;
        self.follow = true;
        self.plan.clear();
        self.reset_meters();
    }

    pub fn cancel(&mut self) {
        self.streaming = false;
        self.finish_turn();
    }

    pub fn handle_key(&mut self, key: KeyEvent, project: Option<&Project>) -> Action {
        if let Some(sel) = self.picker {
            return self.handle_picker_key(key, sel);
        }
        // Details toggle works even while the input is focused.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r') {
            self.expanded = !self.expanded;
            return Action::None;
        }
        if !self.focused {
            match key.code {
                KeyCode::Char(_) | KeyCode::Enter => self.focused = true,
                KeyCode::Up | KeyCode::PageUp => {
                    self.scroll_up(3);
                    return Action::None;
                }
                KeyCode::Down | KeyCode::PageDown => {
                    self.scroll_down(3);
                    return Action::None;
                }
                _ => return Action::None,
            }
        }

        // An open popup owns navigation / accept / dismiss keys.
        if !matches!(self.popup, Popup::None) {
            match key.code {
                KeyCode::Up => {
                    self.popup_move(-1);
                    return Action::None;
                }
                KeyCode::Down => {
                    self.popup_move(1);
                    return Action::None;
                }
                KeyCode::Tab | KeyCode::Enter => {
                    self.accept_popup();
                    return Action::None;
                }
                KeyCode::Esc => {
                    self.popup = Popup::None;
                    return Action::None;
                }
                _ => {}
            }
        }

        // Text editing swallows any key the field recognizes, then refreshes popups.
        if input::handle(&mut self.input, &mut self.cursor, key, EditOpts::default())
            != Edited::Ignored
        {
            self.refresh_popup(project);
            return Action::None;
        }

        match key.code {
            KeyCode::Enter => self.submit(),
            KeyCode::Esc if self.in_flight => Action::RefineCancel,
            KeyCode::Esc => {
                self.focused = false;
                Action::None
            }
            KeyCode::PageUp => {
                self.scroll_up(5);
                Action::None
            }
            KeyCode::PageDown => {
                self.scroll_down(5);
                Action::None
            }
            _ => Action::None,
        }
    }

    fn handle_picker_key(&mut self, key: KeyEvent, sel: usize) -> Action {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.picker = Some(sel.saturating_sub(1));
                Action::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let max = self.sessions.len().saturating_sub(1);
                self.picker = Some((sel + 1).min(max));
                Action::None
            }
            KeyCode::Enter => {
                let action = self
                    .sessions
                    .get(sel)
                    .map(|s| Action::RefineSwitchSession { id: s.id.clone() })
                    .unwrap_or(Action::None);
                self.picker = None;
                action
            }
            KeyCode::Char('n') => {
                self.picker = None;
                Action::RefineNewSession
            }
            KeyCode::Char('d') | KeyCode::Char('x') => self
                .sessions
                .get(sel)
                .map(|s| Action::RefineDeleteSession { id: s.id.clone() })
                .unwrap_or(Action::None),
            KeyCode::Esc => {
                self.picker = None;
                Action::None
            }
            _ => Action::None,
        }
    }

    pub fn handle_mouse(&mut self, m: MouseInput) -> Action {
        if m.is_scroll() {
            match m.gesture {
                MouseGesture::ScrollUp => self.scroll_up(3),
                _ => self.scroll_down(3),
            }
            return Action::None;
        }
        if m.is_click() {
            self.focused = true;
        }
        Action::None
    }

    fn submit(&mut self) -> Action {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return Action::None;
        }
        self.popup = Popup::None;
        self.input.clear();
        self.cursor = 0;

        if text.starts_with('/') {
            return self.run_slash(&text);
        }

        self.follow = true;
        self.last_scope = parse_scope(&text);
        self.plan.clear(); // a fresh request rebuilds its own plan
        self.conversation.push(Turn::user(text.clone()));
        Action::RefineSubmit { text }
    }

    fn run_slash(&mut self, text: &str) -> Action {
        let cmd = text.split_whitespace().next().unwrap_or("");
        let rest = text.split_once(' ').map(|(_, r)| r).unwrap_or("").trim();
        match cmd {
            "/clear" => Action::RefineClear,
            "/cancel" => Action::RefineCancel,
            "/help" => Action::show_overlay(Overlay::Help(0)),
            "/new" => Action::RefineNewSession,
            "/sessions" => Action::RefineOpenSessions,
            "/model" => Action::RefineSetModel {
                model: rest.to_string(),
            },
            "/undo" => Action::RefineUndo,
            "/diff" => Action::RefineOpenDiff,
            "/delete" => Action::RefineDeleteSession {
                id: self.active_session.clone(),
            },
            "/rename" => {
                if rest.is_empty() {
                    self.conversation
                        .push(Turn::tool("usage: /rename <title>".to_string()));
                    self.follow = true;
                    Action::None
                } else {
                    Action::RefineRenameSession {
                        title: rest.to_string(),
                    }
                }
            }
            _ => {
                self.conversation
                    .push(Turn::tool(format!("unknown command: {cmd}")));
                self.follow = true;
                Action::None
            }
        }
    }

    /// The byte range `[start, cursor)` of the token the caret currently sits in.
    fn current_token(&self) -> (usize, &str) {
        let c = input::clamp_cursor(&self.input, self.cursor);
        let start = self.input[..c]
            .rfind(char::is_whitespace)
            .map(|i| i + 1)
            .unwrap_or(0);
        (start, &self.input[start..c])
    }

    fn refresh_popup(&mut self, project: Option<&Project>) {
        let (start, token) = self.current_token();
        if let Some(q) = token.strip_prefix('@') {
            let items = mention_candidates(project, q);
            self.popup = if items.is_empty() {
                Popup::None
            } else {
                Popup::Mention { items, sel: 0 }
            };
        } else if start == 0 && token.starts_with('/') {
            let q = token;
            let items: Vec<usize> = SLASH_COMMANDS
                .iter()
                .enumerate()
                .filter(|(_, (name, _))| name.starts_with(q))
                .map(|(i, _)| i)
                .collect();
            self.popup = if items.is_empty() {
                Popup::None
            } else {
                Popup::Slash { items, sel: 0 }
            };
        } else {
            self.popup = Popup::None;
        }
    }

    fn popup_move(&mut self, delta: isize) {
        let (len, sel) = match &mut self.popup {
            Popup::Mention { items, sel } => (items.len(), sel),
            Popup::Slash { items, sel } => (items.len(), sel),
            Popup::None => return,
        };
        if len == 0 {
            return;
        }
        let cur = *sel as isize + delta;
        *sel = cur.clamp(0, len as isize - 1) as usize;
    }

    fn accept_popup(&mut self) {
        let insert = match &self.popup {
            Popup::Mention { items, sel } => items.get(*sel).map(|c| c.insert.clone()),
            Popup::Slash { items, sel } => {
                items.get(*sel).map(|i| SLASH_COMMANDS[*i].0.to_string())
            }
            Popup::None => None,
        };
        let Some(insert) = insert else {
            self.popup = Popup::None;
            return;
        };
        let (start, _) = self.current_token();
        let c = input::clamp_cursor(&self.input, self.cursor);
        let mut replacement = insert;
        replacement.push(' ');
        self.input.replace_range(start..c, &replacement);
        self.cursor = start + replacement.len();
        self.popup = Popup::None;
    }

    pub fn on_app_event(&mut self, ev: &AppEvent) {
        match ev {
            AppEvent::RefineReasoning { delta } => self.push_reasoning(delta),
            AppEvent::RefineUsage {
                prompt_tokens,
                completion_tokens,
            } => {
                self.turn_tokens.0 = self.turn_tokens.0.saturating_add(*prompt_tokens);
                self.turn_tokens.1 = self.turn_tokens.1.saturating_add(*completion_tokens);
                self.session_tokens.0 = self.session_tokens.0.saturating_add(*prompt_tokens);
                self.session_tokens.1 = self.session_tokens.1.saturating_add(*completion_tokens);
                // The latest prompt size is the current context-window fill.
                self.last_context = *prompt_tokens;
            }
            AppEvent::RefineDelta { delta } => self.push_delta(delta),
            AppEvent::RefinePlanUpdated { steps } => {
                self.plan = steps.clone();
                self.follow = true;
            }
            AppEvent::RefineToolInvoked { tool, summary } => {
                self.settle_reasoning();
                self.conversation
                    .push(Turn::tool(format!("{tool} — {summary}")));
                self.follow = true;
            }
            AppEvent::RefineEditApplied { kind, summary } => {
                self.settle_reasoning();
                self.conversation
                    .push(Turn::tool(format!("{kind}: {summary}")));
                self.follow = true;
            }
            AppEvent::RefineMessageDone => {
                if let Some(last) = self.conversation.last_mut() {
                    last.streaming = false;
                }
                self.streaming = false;
                self.finish_turn();
            }
            AppEvent::RefineError { msg } => {
                self.settle_reasoning();
                self.conversation.push(Turn::tool(format!("error: {msg}")));
                self.streaming = false;
                self.follow = true;
                self.finish_turn();
            }
            _ => {}
        }
    }

    fn push_delta(&mut self, delta: &str) {
        self.settle_reasoning();
        let need_new = !matches!(
            self.conversation.last(),
            Some(t) if t.role == TurnRole::Assistant && t.streaming
        );
        if need_new {
            self.conversation.push(Turn {
                role: TurnRole::Assistant,
                text: String::new(),
                streaming: true,
            });
        }
        if let Some(last) = self.conversation.last_mut() {
            last.text.push_str(delta);
        }
        self.streaming = true;
        self.follow = true;
    }

    fn push_reasoning(&mut self, delta: &str) {
        let need_new = !matches!(
            self.conversation.last(),
            Some(t) if t.role == TurnRole::Reasoning && t.streaming
        );
        if need_new {
            self.conversation.push(Turn {
                role: TurnRole::Reasoning,
                text: String::new(),
                streaming: true,
            });
        }
        if let Some(last) = self.conversation.last_mut() {
            last.text.push_str(delta);
        }
        self.streaming = true;
        self.follow = true;
    }

    /// Ends streaming reasoning when answer or tool output starts.
    fn settle_reasoning(&mut self) {
        if let Some(last) = self.conversation.last_mut()
            && last.role == TurnRole::Reasoning
            && last.streaming
        {
            last.streaming = false;
        }
    }

    fn scroll_up(&mut self, n: u16) {
        if self.follow {
            self.scroll = self.last_bottom;
            self.follow = false;
        }
        self.scroll = self.scroll.saturating_sub(n);
    }

    fn scroll_down(&mut self, n: u16) {
        let next = self.scroll.saturating_add(n);
        if next >= self.last_bottom {
            self.follow = true;
            self.scroll = self.last_bottom;
        } else {
            self.scroll = next;
            self.follow = false;
        }
    }

    pub fn hints(&self) -> &'static [(&'static str, &'static str)] {
        if self.picker.is_some() {
            &[
                ("↑↓", "select"),
                ("↵", "open"),
                ("n", "new"),
                ("d", "delete"),
                ("esc", "close"),
            ]
        } else if self.in_flight {
            &[("esc", "interrupt"), ("⌃R", "details"), ("↑↓", "scroll")]
        } else if self.focused {
            &[
                ("↵", "send"),
                ("@", "mention"),
                ("/", "cmd"),
                ("⌃R", "details"),
                ("esc", "tabs"),
            ]
        } else {
            &[("type", "focus"), ("↑↓", "scroll"), ("⌃R", "details")]
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, frame: u64, has_project: bool, theme: &Theme) {
        if !has_project {
            self.render_no_project(f, area, theme);
            return;
        }
        let input_h = 3;
        let status = self.status_line(frame, theme);

        let mut constraints = vec![Constraint::Min(3)];
        if !self.plan.is_empty() {
            let plan_h = (self.plan.len() as u16 + 2).clamp(4, 10);
            constraints.push(Constraint::Length(plan_h));
        }
        if status.is_some() {
            constraints.push(Constraint::Length(1));
        }
        constraints.push(Constraint::Length(input_h));

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        self.render_transcript(f, rows[0], frame, theme);
        let input_row = rows[rows.len() - 1];
        let mut next = 1;
        if !self.plan.is_empty() {
            self.render_plan(f, rows[next], theme);
            next += 1;
        }
        if let Some(status) = status {
            self.render_status(f, rows[next], status, theme);
        }
        self.render_input(f, input_row, theme);
        if self.picker.is_some() {
            self.render_session_picker(f, area, theme);
        } else {
            self.render_popup(f, area, input_row.y, theme);
        }
    }

    /// Pinned working/usage row.
    fn render_status(&self, f: &mut Frame, area: Rect, line: Line<'static>, theme: &Theme) {
        if area.height == 0 || area.width <= 2 {
            return;
        }
        let inset = Rect {
            x: area.x + 1,
            width: area.width - 2,
            ..area
        };
        f.render_widget(
            Paragraph::new(line).style(Style::default().bg(theme.bg_panel)),
            inset,
        );
    }

    /// Pinned `update_plan` checklist.
    fn render_plan(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        let done = self
            .plan
            .iter()
            .filter(|s| s.status == PlanStepStatus::Completed)
            .count();
        let title = format!(" ✓ plan · {done}/{} ", self.plan.len());
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(theme.rule))
            .title(Span::styled(title, Style::default().fg(theme.ink_soft)))
            .style(Style::default().bg(theme.bg_panel));
        let inner = block.inner(area);
        f.render_widget(block, area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        let w = inner.width as usize;
        let lines: Vec<Line> = self
            .plan
            .iter()
            .take(inner.height as usize)
            .map(|s| {
                let (mark, style) = match s.status {
                    PlanStepStatus::Completed => (
                        "✓ ",
                        Style::default().fg(theme.ink_faint).add_modifier(Modifier::CROSSED_OUT),
                    ),
                    PlanStepStatus::InProgress => (
                        "▸ ",
                        Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
                    ),
                    PlanStepStatus::Pending => ("◻ ", Style::default().fg(theme.ink_soft)),
                };
                Line::from(Span::styled(
                    truncate_cols(&format!("{mark}{}", s.step.trim()), w),
                    style,
                ))
            })
            .collect();
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme.bg_panel)),
            inner,
        );
    }

    /// Placeholder shown on the Refine tab when no project is open. Refine is
    /// per-project, so there is nothing to chat about until one is opened.
    fn render_no_project(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(theme.rule))
            .title(Span::styled(
                " 推 Refine ",
                Style::default().fg(theme.ink_soft),
            ))
            .style(Style::default().bg(theme.bg_panel));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let lines = vec![
            Line::raw(""),
            Line::from(Span::styled(
                "Open a project to use Refine.",
                Style::default().fg(theme.ink_soft),
            )),
            Line::raw(""),
            Line::from(Span::styled(
                "Refine is per-project — pick a book on 書架 Shelf (1), then come back.",
                Style::default().fg(theme.ink_faint),
            )),
        ];
        f.render_widget(
            Paragraph::new(lines)
                .alignment(Alignment::Center)
                .style(Style::default().bg(theme.bg_panel)),
            inner,
        );
    }

    fn render_session_picker(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        let Some(sel) = self.picker else {
            return;
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(theme.accent))
            .title(Span::styled(
                " conversations ",
                Style::default().fg(theme.ink_soft),
            ))
            .style(Style::default().bg(theme.bg_inset));
        let w = area.width.min(64);
        let h = area
            .height
            .min((self.sessions.len() as u16).saturating_add(4).max(6));
        let modal = Rect {
            x: area.x + (area.width.saturating_sub(w)) / 2,
            y: area.y + (area.height.saturating_sub(h)) / 2,
            width: w,
            height: h,
        };
        let inner = block.inner(modal);
        f.render_widget(Clear, modal);
        f.render_widget(block, modal);

        if self.sessions.is_empty() {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "no saved conversations yet — press n for a new one",
                    Style::default().fg(theme.ink_faint),
                )))
                .style(Style::default().bg(theme.bg_inset)),
                inner,
            );
            return;
        }

        let label_w = inner.width as usize;
        // Window the list so the selected row stays visible past the modal height.
        let visible = (inner.height as usize).max(1);
        let start = if sel >= visible { sel + 1 - visible } else { 0 };
        let lines: Vec<Line> = self
            .sessions
            .iter()
            .enumerate()
            .skip(start)
            .take(visible)
            .map(|(i, s)| {
                let marker = if s.id == self.active_session {
                    "● "
                } else {
                    "  "
                };
                let title = if s.title.is_empty() {
                    "(untitled)".to_string()
                } else {
                    s.title.clone()
                };
                let label = format!(
                    "{marker}{}  · {} msg · {}",
                    title,
                    s.message_count,
                    s.updated.format("%Y-%m-%d %H:%M")
                );
                let style = if i == sel {
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.ink_soft)
                };
                Line::from(Span::styled(truncate_cols(&label, label_w), style))
            })
            .collect();
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme.bg_inset)),
            inner,
        );
    }

    fn render_transcript(&mut self, f: &mut Frame, area: Rect, frame: u64, theme: &Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(theme.rule))
            .title(Span::styled(
                " 推 Refine — chat ",
                Style::default().fg(theme.ink_soft),
            ))
            .style(Style::default().bg(theme.bg_panel));
        let inner = block.inner(area);
        f.render_widget(block, area);
        self.transcript_area = inner;

        let width = inner.width as usize;
        let mut lines: Vec<Line> = if self.conversation.is_empty() {
            vec![
                Line::from(Span::styled(
                    "Ask the refine agent to fix or improve anything in this project.",
                    Style::default().fg(theme.ink_faint),
                )),
                Line::raw(""),
                Line::from(Span::styled(
                    "  @v1/c3 tighten the prose · @glossary rename a term · /help",
                    Style::default().fg(theme.ink_faint),
                )),
            ]
        } else {
            let md = self.transcript_markdown();
            let fg = theme.th_text;
            let mut h = std::collections::hash_map::DefaultHasher::new();
            md.len().hash(&mut h);
            md.as_bytes()
                .iter()
                .rev()
                .take(64)
                .for_each(|b| b.hash(&mut h));
            width.hash(&mut h);
            fg.hash(&mut h);
            self.expanded.hash(&mut h);
            crate::ui::markdown::theme_fingerprint(theme).hash(&mut h);
            let key = h.finish();
            self.transcript_cache
                .lines(key, || crate::ui::markdown::render(&md, fg, theme, width))
                .to_vec()
        };

        if self.streaming {
            let caret = Span::styled(
                theme::spinner_frame(frame),
                Style::default().fg(theme.stream_cursor),
            );
            match lines.last_mut() {
                Some(last) => last.push_span(caret),
                None => lines.push(Line::from(caret)),
            }
        }

        let total_lines = lines.len() as u16;
        let view_h = inner.height;
        self.last_bottom = total_lines.saturating_sub(view_h);
        let scroll = if self.follow {
            self.last_bottom
        } else {
            self.scroll.min(total_lines.saturating_sub(1))
        };

        let para = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .style(Style::default().bg(theme.bg_panel));
        f.render_widget(para, inner);
    }

    /// Claude-Code-style working line while a turn runs; an idle session-usage
    /// summary otherwise. `None` when idle with nothing spent yet.
    fn status_line(&self, frame: u64, theme: &Theme) -> Option<Line<'static>> {
        let faint = Style::default().fg(theme.ink_faint);
        let soft = Style::default().fg(theme.ink_soft);
        if self.in_flight {
            let elapsed = self.turn_started.map(|s| s.elapsed()).unwrap_or_default();
            let (inp, out) = self.turn_tokens;
            let mut spans = vec![
                Span::styled(
                    format!("{} ", theme::refine_spinner_frame(frame)),
                    Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "Working ",
                    Style::default().fg(theme.ink).add_modifier(Modifier::BOLD),
                ),
                Span::styled("(", faint),
                Span::styled(fmt_elapsed(elapsed), soft),
            ];
            if inp > 0 || out > 0 {
                spans.push(Span::styled(format!(" · ↑ {}", fmt_tokens(inp)), soft));
                spans.push(Span::styled(format!(" ↓ {}", fmt_tokens(out)), soft));
                spans.push(Span::styled(" tokens", faint));
            }
            spans.push(Span::styled(" · esc to interrupt)", faint));
            return Some(Line::from(spans));
        }

        let (inp, out) = self.session_tokens;
        if inp == 0 && out == 0 {
            return None;
        }
        let mut text = format!(
            "↑ {} ↓ {} · {} tokens",
            fmt_tokens(inp),
            fmt_tokens(out),
            fmt_tokens(inp.saturating_add(out))
        );
        if self.last_context > 0 {
            text.push_str(&format!(
                " · ctx {}/{}",
                fmt_tokens(self.last_context),
                fmt_tokens(self.context_max)
            ));
        }
        if let Some(d) = self.last_turn_elapsed {
            text.push_str(&format!(" · last {}", fmt_elapsed(d)));
        }
        Some(Line::from(Span::styled(text, faint)))
    }

    fn transcript_markdown(&self) -> String {
        let mut out = String::new();
        let mut i = 0;
        while i < self.conversation.len() {
            let turn = &self.conversation[i];
            match turn.role {
                TurnRole::User => {
                    out.push_str("**› you**\n\n");
                    out.push_str(&turn.text);
                    i += 1;
                }
                TurnRole::Assistant => {
                    out.push_str(&turn.text);
                    i += 1;
                }
                TurnRole::Reasoning => {
                    self.push_reasoning_md(&mut out, turn);
                    i += 1;
                }
                TurnRole::Tool => {
                    // Collapse runs so the answer stays prominent; ⌃R expands detail.
                    let start = i;
                    while i < self.conversation.len()
                        && self.conversation[i].role == TurnRole::Tool
                    {
                        i += 1;
                    }
                    let run = &self.conversation[start..i];
                    if self.expanded || run.len() == 1 {
                        let joined = run
                            .iter()
                            .map(|t| format!("`🔧 {}`", t.text.trim()))
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        out.push_str(&joined);
                    } else {
                        out.push_str(&format!("`🔧 {} actions · ⌃R`", run.len()));
                    }
                }
            }
            out.push_str("\n\n");
        }
        out
    }

    /// Render reasoning expanded while streaming or collapsed otherwise.
    fn push_reasoning_md(&self, out: &mut String, turn: &Turn) {
        let body = turn.text.trim();
        if body.is_empty() {
            return;
        }
        if self.expanded || turn.streaming {
            out.push_str("> 💭 *thinking*\n>\n");
            for line in body.lines() {
                out.push_str("> ");
                out.push_str(line);
                out.push('\n');
            }
        } else {
            let lines = body.lines().count().max(1);
            out.push_str(&format!("> 💭 *thinking — {lines} line(s); ⌃R to expand*"));
        }
    }

    fn render_input(&mut self, f: &mut Frame, area: Rect, theme: &Theme) {
        let border = if self.focused {
            theme.accent
        } else {
            theme.rule
        };
        let title = if self.last_scope.is_empty() {
            " message ".to_string()
        } else {
            let scope = self
                .last_scope
                .iter()
                .map(|t| t.token())
                .collect::<Vec<_>>()
                .join(" ");
            format!(" scope: {scope} ")
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(border))
            .title(Span::styled(title, Style::default().fg(theme.ink_faint)))
            .style(Style::default().bg(theme.bg_panel));
        let inner = block.inner(area);
        f.render_widget(block, area);
        self.input_area = inner;
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let prompt = "› ";
        let field_w = (inner.width as usize).saturating_sub(prompt.len());
        if self.input.is_empty() && !self.focused {
            let para = Paragraph::new(Line::from(vec![
                Span::styled(prompt, Style::default().fg(theme.ink_faint)),
                Span::styled(
                    "type to chat — Esc releases the keyboard",
                    Style::default().fg(theme.ink_faint),
                ),
            ]))
            .style(Style::default().bg(theme.bg_panel));
            f.render_widget(para, inner);
            return;
        }

        let (before, after) = input::caret_halves(&self.input, self.cursor, field_w);
        let mut spans = vec![
            Span::styled(prompt, Style::default().fg(theme.accent)),
            Span::styled(before, Style::default().fg(theme.ink)),
        ];
        if self.focused {
            spans.push(Span::styled(
                "▏",
                Style::default()
                    .fg(theme.stream_cursor)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        spans.push(Span::styled(after, Style::default().fg(theme.ink)));
        f.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(theme.bg_panel)),
            inner,
        );
    }

    fn render_popup(&self, f: &mut Frame, body: Rect, input_top: u16, theme: &Theme) {
        let rows: Vec<(String, bool)> = match &self.popup {
            Popup::None => return,
            Popup::Mention { items, sel } => items
                .iter()
                .enumerate()
                .map(|(i, c)| (c.label.clone(), i == *sel))
                .collect(),
            Popup::Slash { items, sel } => items
                .iter()
                .enumerate()
                .map(|(i, ci)| {
                    let (name, help) = SLASH_COMMANDS[*ci];
                    (format!("{name}  {help}"), i == *sel)
                })
                .collect(),
        };
        if rows.is_empty() {
            return;
        }

        let max_rows = rows.len().min(6) as u16;
        let height = max_rows + 2;
        let width = body.width.min(52);
        let x = body.x + 1;
        let y = input_top.saturating_sub(height);
        let area = Rect {
            x,
            y,
            width,
            height,
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(theme.accent))
            .style(Style::default().bg(theme.bg_inset));
        let inner = block.inner(area);
        f.render_widget(Clear, area);
        f.render_widget(block, area);

        let label_w = inner.width as usize;
        let lines: Vec<Line> = rows
            .iter()
            .take(max_rows as usize)
            .map(|(label, selected)| {
                let style = if *selected {
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.ink_soft)
                };
                Line::from(Span::styled(truncate_cols(label, label_w), style))
            })
            .collect();
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme.bg_inset)),
            inner,
        );
    }
}

/// Compact token count: `950`, `1.5k`, `12k`, `120k`.
fn fmt_tokens(n: u32) -> String {
    if n < 1000 {
        n.to_string()
    } else if n < 10_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else if n < 1_000_000 {
        format!("{}k", n / 1000)
    } else {
        // >= 1000k → show in millions (e.g. 2044k → 2.0M).
        format!("{:.1}M", n as f64 / 1_000_000.0)
    }
}

/// Human elapsed time: `45s`, `1m 10s`, `1h 2m`.
fn fmt_elapsed(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    }
}

fn mention_candidates(project: Option<&Project>, q: &str) -> Vec<MentionCandidate> {
    let ql = q.to_lowercase();
    let matches = |hay: &str| hay.to_lowercase().contains(&ql);
    let mut items: Vec<MentionCandidate> = Vec::new();

    for (insert, desc) in RESOURCE_CANDS {
        if ql.is_empty() || matches(insert) {
            items.push(MentionCandidate {
                insert: (*insert).to_string(),
                label: format!("{insert}  —  {desc}"),
            });
        }
    }

    if let Some(p) = project {
        for v in &p.volumes {
            let vtok = format!("@v{}", v.number);
            if ql.is_empty() || matches(&vtok) {
                let label = match &v.label {
                    Some(l) => format!("{vtok}  —  Vol.{} {l}", v.number),
                    None => format!("{vtok}  —  Vol.{}", v.number),
                };
                items.push(MentionCandidate {
                    insert: vtok,
                    label,
                });
            }
            for ch in &v.chapters {
                let ctok = format!("@v{}/c{}", v.number, ch.number);
                if ql.is_empty() || matches(&ctok) || matches(&ch.title) {
                    items.push(MentionCandidate {
                        insert: ctok.clone(),
                        label: format!("{ctok}  —  {}", ch.title),
                    });
                }
            }
        }
    }

    items.truncate(40);
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_scope_extracts_chapters_and_resources() {
        let scope = parse_scope("fix @v1/c3 and update @glossary please");
        assert_eq!(
            scope,
            vec![
                MentionTarget::Chapter { vol: 1, ch: 3 },
                MentionTarget::Glossary
            ]
        );
    }

    #[test]
    fn parse_scope_handles_volume_and_trailing_punctuation() {
        let scope = parse_scope("redo @v2, then check @recap.");
        assert_eq!(
            scope,
            vec![MentionTarget::Volume { vol: 2 }, MentionTarget::Recap]
        );
    }

    #[test]
    fn parse_scope_dedups_and_ignores_plain_words() {
        let scope = parse_scope("@v1 @v1 hello world @style");
        assert_eq!(
            scope,
            vec![MentionTarget::Volume { vol: 1 }, MentionTarget::Style]
        );
    }

    #[test]
    fn parse_mention_rejects_garbage() {
        assert_eq!(parse_mention("vabc"), None);
        assert_eq!(parse_mention("nonsense"), None);
        assert_eq!(parse_mention("v3/c"), None);
    }

    #[test]
    fn slash_popup_filters_by_prefix() {
        let mut s = RefineScreen::new();
        s.input = "/cl".to_string();
        s.cursor = s.input.len();
        s.refresh_popup(None);
        match &s.popup {
            Popup::Slash { items, .. } => {
                assert_eq!(items.len(), 1);
                assert_eq!(SLASH_COMMANDS[items[0]].0, "/clear");
            }
            _ => panic!("expected a slash popup"),
        }
    }

    #[test]
    fn mention_popup_opens_for_at_token() {
        let mut s = RefineScreen::new();
        s.input = "fix @gl".to_string();
        s.cursor = s.input.len();
        s.refresh_popup(None);
        match &s.popup {
            Popup::Mention { items, .. } => {
                assert!(items.iter().any(|c| c.insert == "@glossary"));
            }
            _ => panic!("expected a mention popup"),
        }
    }

    #[test]
    fn display_turns_reconstructs_from_messages() {
        use crate::llm::{FunctionCall, ToolCall};
        let messages = vec![
            Message::user("fix the prose\n\n(In scope: volume 1 chapter 1. Active volume: 1.)"),
            Message {
                role: Role::Assistant,
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "c1".to_string(),
                    kind: "function".to_string(),
                    function: FunctionCall {
                        name: "edit_chapter".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]),
                tool_call_id: None,
                name: None,
            },
            Message::tool_result("c1", "{\"ok\":true}"),
            Message::assistant("done — tightened the phrasing"),
        ];
        let turns = display_turns(&messages);
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0].role, TurnRole::User);
        assert_eq!(turns[0].text, "fix the prose"); // scope hint stripped
        assert_eq!(turns[1].role, TurnRole::Tool);
        assert_eq!(turns[1].text, "edit_chapter");
        assert_eq!(turns[2].role, TurnRole::Assistant);
    }

    #[test]
    fn picker_navigates_and_selects() {
        let mut s = RefineScreen::new();
        let sessions = vec![
            SessionMeta {
                id: "a".to_string(),
                title: "first".to_string(),
                updated: chrono::Utc::now(),
                message_count: 2,
            },
            SessionMeta {
                id: "b".to_string(),
                title: "second".to_string(),
                updated: chrono::Utc::now(),
                message_count: 1,
            },
        ];
        s.open_picker(sessions, "a".to_string());
        assert!(s.picker_open() && s.is_capturing());
        let action = s.handle_key(
            ratatui::crossterm::event::KeyEvent::new(
                KeyCode::Down,
                ratatui::crossterm::event::KeyModifiers::empty(),
            ),
            None,
        );
        assert!(matches!(action, Action::None));
        let action = s.handle_key(
            ratatui::crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                ratatui::crossterm::event::KeyModifiers::empty(),
            ),
            None,
        );
        match action {
            Action::RefineSwitchSession { id } => assert_eq!(id, "b"),
            other => panic!("expected switch to b, got {other:?}"),
        }
        assert!(!s.picker_open());
    }

    #[test]
    fn reasoning_streams_then_collapses_and_toggles() {
        let mut s = RefineScreen::new();
        s.on_app_event(&AppEvent::RefineReasoning {
            delta: "weighing the options\nthen deciding".to_string(),
        });
        assert_eq!(s.conversation.len(), 1);
        assert_eq!(s.conversation[0].role, TurnRole::Reasoning);
        assert!(s.conversation[0].streaming, "reasoning streams live");

        s.on_app_event(&AppEvent::RefineDelta {
            delta: "Here is the fix.".to_string(),
        });
        assert!(!s.conversation[0].streaming, "reasoning stops when the answer starts");
        assert_eq!(s.conversation[1].role, TurnRole::Assistant);

        let md = s.transcript_markdown();
        assert!(md.contains("thinking —"), "collapsed reasoning summary shown");
        assert!(!md.contains("weighing the options"), "full reasoning hidden when collapsed");

        s.expanded = true;
        let md = s.transcript_markdown();
        assert!(md.contains("weighing the options"), "expanded reasoning shows the text");
    }

    #[test]
    fn tool_run_collapses_then_expands() {
        let mut s = RefineScreen::new();
        for (tool, summary) in [
            ("read_chapter", "ch1"),
            ("grep_chapter", "ดาบ"),
            ("edit_chapter", "ch1: 1 edit(s)"),
        ] {
            s.on_app_event(&AppEvent::RefineToolInvoked {
                tool: tool.to_string(),
                summary: summary.to_string(),
            });
        }
        let md = s.transcript_markdown();
        assert!(md.contains("3 actions"), "consecutive tool lines coalesce: {md}");
        assert!(!md.contains("grep_chapter"), "details hidden when collapsed");

        s.expanded = true;
        let md = s.transcript_markdown();
        assert!(md.contains("grep_chapter") && md.contains("edit_chapter"));
        assert!(!md.contains("3 actions"));
    }

    #[test]
    fn plan_event_populates_pinned_checklist() {
        let mut s = RefineScreen::new();
        assert!(s.plan.is_empty());
        s.on_app_event(&AppEvent::RefinePlanUpdated {
            steps: vec![
                PlanStep {
                    step: "read ch1".to_string(),
                    status: PlanStepStatus::Completed,
                },
                PlanStep {
                    step: "fix the term".to_string(),
                    status: PlanStepStatus::InProgress,
                },
            ],
        });
        assert_eq!(s.plan.len(), 2);
        s.input = "do something".to_string();
        s.cursor = s.input.len();
        let _ = s.submit();
        assert!(s.plan.is_empty(), "a new request starts with a clean plan");
    }

    #[test]
    fn token_and_elapsed_formatting() {
        assert_eq!(fmt_tokens(950), "950");
        assert_eq!(fmt_tokens(1500), "1.5k");
        assert_eq!(fmt_tokens(12_000), "12k");
        assert_eq!(fmt_tokens(120_000), "120k");
        use std::time::Duration;
        assert_eq!(fmt_elapsed(Duration::from_secs(45)), "45s");
        assert_eq!(fmt_elapsed(Duration::from_secs(70)), "1m 10s");
        assert_eq!(fmt_elapsed(Duration::from_secs(3720)), "1h 2m");
    }

    #[test]
    fn usage_accumulates_and_turn_lifecycle_clears_in_flight() {
        let mut s = RefineScreen::new();
        s.begin_turn();
        assert!(s.in_flight);
        s.on_app_event(&AppEvent::RefineUsage {
            prompt_tokens: 1000,
            completion_tokens: 200,
        });
        s.on_app_event(&AppEvent::RefineUsage {
            prompt_tokens: 1500,
            completion_tokens: 300,
        });
        assert_eq!(s.turn_tokens, (2500, 500));
        assert_eq!(s.session_tokens, (2500, 500));
        s.on_app_event(&AppEvent::RefineMessageDone);
        assert!(!s.in_flight, "done ends the in-flight turn");
        assert!(s.last_turn_elapsed.is_some(), "elapsed is banked");
        assert_eq!(s.session_tokens, (2500, 500), "session total persists across turns");
    }

    #[test]
    fn accept_popup_replaces_partial_token() {
        let mut s = RefineScreen::new();
        s.input = "fix @gl".to_string();
        s.cursor = s.input.len();
        s.refresh_popup(None);
        if let Popup::Mention { items, sel } = &mut s.popup {
            *sel = items.iter().position(|c| c.insert == "@glossary").unwrap();
        }
        s.accept_popup();
        assert_eq!(s.input, "fix @glossary ");
        assert_eq!(s.cursor, s.input.len());
        assert!(matches!(s.popup, Popup::None));
    }
}
