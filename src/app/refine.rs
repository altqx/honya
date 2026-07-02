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
use crate::model::{AppEvent, PlanStep, PlanStepStatus, Project, RefineSubagentStatus};
use crate::theme::{self, Theme};
use crate::ui::input::{self, EditOpts, Edited};
use crate::ui::mouse::{MouseGesture, MouseInput};
use crate::ui::text::{col_width, truncate_cols};
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
    ("/compact", "compact the conversation now"),
    ("/context", "show context-window usage"),
    ("/export", "export this conversation to markdown"),
    ("/fix-review-needed", "triage and fix review-needed chunks"),
    ("/fix-short-names", "fix expanded short name surfaces"),
    ("/grep", "search the project for text"),
    ("/resume", "pick a session to resume"),
];

const COMPACT_SUMMARY_PREFIX: &str = "[Earlier conversation, compacted to fit the context window]";

const FIX_SHORT_NAMES_PROMPT: &str = r#"Fix over-expanded character names across the requested scope.

Problem: some Thai translations use a full canonical character name even when SOURCE_JP uses only a short surface such as a surname, given name, nickname, title, or alias. Example: if CHARACTERS has `天道カレン → เทนโด คาเรน` but the source line says only `天道`, the Thai should use `เทนโด`, not `เทนโด คาเรน`.

Workflow:
1. Use `update_plan`, then read the character roster with `read_lexicon`.
2. Find candidate chapters by searching translated text for full canonical Thai names. For each candidate, read the matching Japanese source and Thai lines together.
3. Only edit when the source mention is actually short and the Thai expanded it to the full canonical name. Do not change cases where SOURCE_JP uses the full name, where the full name is needed for clarity, or where the Thai line is already natural.
4. Prefer existing `also_called` mappings for the replacement. If a verified short source surface lacks one, infer the natural short Thai surface from the canonical Thai name or existing usage, then update CHARACTERS with `also_called` (for example `天道→เทนโด`) so future translation/review preserves that surface.
5. Apply surgical chapter edits with `multi_edit_chapter` where possible. Do not run a blind project-wide replacement.
6. Verify changed regions by re-reading or grepping them, then report every chapter touched and every character mapping added or changed.

Default scope: the whole project unless the user supplied an explicit @volume/@chapter scope."#;

const FIX_REVIEW_NEEDED_PROMPT: &str = r#"Investigate and fix `honya:review-needed` chunks across the requested scope.

Use `update_plan`. First list/count the flagged chunks, then categorize them before editing:
- name/honorific/surface: wrong alias, full-name expansion, `さん`/`先輩`.
- dialogue/POV/pronoun/register: speaker attribution, `俺/僕/あたし/自分`, addressee forms, polite vs rough particles.
- source fidelity: mistranslation, wrong subject, missing line, skipped title/credit.
- residue/format/ruby: Japanese punctuation, `（ ）`, furigana/original glosses, Markdown/HTML drift.
- glossary/terminology: hard_locked/preferred/forbidden terms and handles.
- Thai quality/SFX: awkward literal phrasing, tone drift, unnatural onomatopoeia.
- infrastructure: translator stream cutoff, refusal/policy notice, empty or partial output.

For each chunk you edit, read the matching SOURCE_JP, Thai chunk, CHARACTERS, GLOSSARY, and STYLE. Treat the reviewer reason as a clue, not as automatic truth. Fix only actionable issues that SOURCE_JP/REFERENCE confirms. If a reviewer note says a point is correct/acceptable/not an issue, leave that point alone and fix the actual remaining issue.

Special dialogue rule: `自分` inside dialogue may mean the speaker or the listener. Resolve from adjacent turns; if it means the listener, use that listener's established address form such as `คุณอากุริ` or `อามาโนะคุง`, not generic `เธอ/แก` when the speaker is polite.

Use surgical `multi_edit_chapter`/`edit_chapter`, verify changed regions, and report counts by category plus files changed.

Default scope: the whole project unless the user supplied an explicit @volume/@chapter scope."#;

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
                    if is_compacted_summary(c) {
                        turns.push(Turn::tool(
                            "context summarized - earlier messages compacted".to_string(),
                        ));
                    } else {
                        turns.push(Turn::user(user_display(c).to_string()));
                    }
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
    let mut end = content.len();
    for marker in [
        "\n\n(In scope:",
        "\n\n[Mid-run steering:",
        "\n\n[Approval mode:",
    ] {
        if let Some((head, _)) = content.split_once(marker) {
            end = end.min(head.len());
        }
    }
    &content[..end]
}

fn is_compacted_summary(content: &str) -> bool {
    content.starts_with(COMPACT_SUMMARY_PREFIX)
}

/// Blocking prompt awaiting user input.
#[derive(Debug, Clone)]
enum RefinePending {
    Approval {
        id: u64,
        summary: String,
        diff: String,
    },
    Decision {
        id: u64,
        question: String,
        options: Vec<String>,
        selected: usize,
    },
}

#[derive(Debug, Clone)]
struct SubagentRun {
    id: String,
    title: String,
    status: RefineSubagentStatus,
    summary: String,
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
    pending: Option<RefinePending>,
    approval_mode: crate::agents::refine::ApprovalMode,
    plan: Vec<PlanStep>,
    subagents: Vec<SubagentRun>,
    expanded: bool,
    in_flight: bool,
    turn_started: Option<std::time::Instant>,
    last_turn_elapsed: Option<std::time::Duration>,
    turn_tokens: (u32, u32),
    session_tokens: (u32, u32),
    last_context: u32,
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
            pending: None,
            approval_mode: crate::agents::refine::ApprovalMode::default(),
            plan: Vec::new(),
            subagents: Vec::new(),
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
        self.subagents.clear();
        self.reset_meters();
    }

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

    pub fn set_context_max(&mut self, max: u32) {
        self.context_max = max.max(1);
    }

    pub fn set_approval_mode(&mut self, mode: crate::agents::refine::ApprovalMode) {
        self.approval_mode = mode;
    }

    pub fn plan(&self) -> &[PlanStep] {
        &self.plan
    }

    pub fn is_in_flight(&self) -> bool {
        self.in_flight
    }

    pub fn set_plan(&mut self, plan: Vec<PlanStep>) {
        self.plan = plan;
    }

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
        self.subagents.clear();
        self.reset_meters();
    }

    pub fn cancel(&mut self) {
        self.pending = None;
        for run in &mut self.subagents {
            if run.status == RefineSubagentStatus::Running {
                run.status = RefineSubagentStatus::Canceled;
                run.summary = "cancelled by the user".to_string();
            }
        }
        self.streaming = false;
        self.finish_turn();
    }

    fn handle_pending_key(&mut self, key: KeyEvent) -> Action {
        let Some(pending) = self.pending.clone() else {
            return Action::None;
        };
        let respond = |me: &mut Self, id: u64, answer: String| {
            me.pending = None;
            me.follow = true;
            Action::RefineRespondInteraction { id, answer }
        };
        match pending {
            RefinePending::Approval { id, .. } => match key.code {
                KeyCode::Enter | KeyCode::Char('y') => respond(self, id, "approve".to_string()),
                KeyCode::Esc | KeyCode::Char('r') | KeyCode::Char('n') => {
                    respond(self, id, String::new())
                }
                _ => Action::None,
            },
            RefinePending::Decision {
                id,
                options,
                selected,
                ..
            } => {
                if options.is_empty() {
                    match key.code {
                        KeyCode::Enter => {
                            let answer = self.input.trim().to_string();
                            if answer.is_empty() {
                                return Action::None;
                            }
                            self.input.clear();
                            self.cursor = 0;
                            respond(self, id, answer)
                        }
                        KeyCode::Esc => respond(self, id, String::new()),
                        _ => Action::None,
                    }
                } else {
                    match key.code {
                        KeyCode::Up => {
                            if let Some(RefinePending::Decision { selected, .. }) =
                                self.pending.as_mut()
                            {
                                *selected = selected.saturating_sub(1);
                            }
                            Action::None
                        }
                        KeyCode::Down => {
                            if let Some(RefinePending::Decision { selected, .. }) =
                                self.pending.as_mut()
                            {
                                *selected = (*selected + 1).min(options.len() - 1);
                            }
                            Action::None
                        }
                        KeyCode::Enter => respond(self, id, options[selected].clone()),
                        KeyCode::Esc => respond(self, id, String::new()),
                        _ => Action::None,
                    }
                }
            }
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, project: Option<&Project>) -> Action {
        if self.in_flight
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c')
        {
            return Action::RefineCancel;
        }

        if let Some(sel) = self.picker {
            return self.handle_picker_key(key, sel);
        }
        if let Some(pending) = &self.pending {
            // Free-text decisions use the input; approvals and choices capture all keys.
            let free_text =
                matches!(pending, RefinePending::Decision { options, .. } if options.is_empty());
            if !free_text || matches!(key.code, KeyCode::Enter | KeyCode::Esc) {
                return self.handle_pending_key(key);
            }
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r') {
            self.expanded = !self.expanded;
            return Action::None;
        }
        // Ctrl+Tab cycles always-approve → ask → auto.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Tab {
            return Action::RefineCycleApprovalMode;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::End {
            self.jump_bottom();
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

        // Text editing swallows recognized keys, then refreshes popups.
        if input::handle(&mut self.input, &mut self.cursor, key, EditOpts::default())
            != Edited::Ignored
        {
            self.refresh_popup(project);
            return Action::None;
        }

        match key.code {
            KeyCode::Enter => self.submit(),
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
        // Plans persist until the agent updates or clears them.
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
            "/compact" => Action::RefineCompact,
            "/export" => Action::RefineExport,
            "/resume" => Action::RefineOpenSessions,
            "/grep" => {
                if rest.is_empty() {
                    self.conversation
                        .push(Turn::tool("usage: /grep <text>".to_string()));
                    self.follow = true;
                    Action::None
                } else {
                    self.input = format!(
                        "Search the whole project for \"{rest}\" and list every chapter and line where it appears."
                    );
                    self.cursor = self.input.len();
                    self.submit()
                }
            }
            "/fix-short-names" => {
                self.input = if rest.is_empty() {
                    FIX_SHORT_NAMES_PROMPT.to_string()
                } else {
                    format!("{FIX_SHORT_NAMES_PROMPT}\n\nScope hint: {rest}")
                };
                self.cursor = self.input.len();
                self.submit()
            }
            "/fix-review-needed" => {
                self.input = if rest.is_empty() {
                    FIX_REVIEW_NEEDED_PROMPT.to_string()
                } else {
                    format!("{FIX_REVIEW_NEEDED_PROMPT}\n\nScope hint: {rest}")
                };
                self.cursor = self.input.len();
                self.submit()
            }
            "/context" => {
                let pct = if self.context_max > 0 {
                    (self.last_context as u64 * 100 / self.context_max as u64) as u32
                } else {
                    0
                };
                self.conversation.push(Turn::tool(format!(
                    "context {} / {} tokens ({pct}% full) · {} turns · auto-compacts at 80%",
                    fmt_tokens(self.last_context),
                    fmt_tokens(self.context_max),
                    self.conversation.len()
                )));
                self.follow = true;
                Action::None
            }
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
                self.last_context = *prompt_tokens;
            }
            AppEvent::RefineContextCompacted {
                dropped_messages,
                token_estimate,
                context_max,
            } => {
                self.settle_reasoning();
                self.conversation.push(Turn::tool(compaction_notice(
                    *dropped_messages,
                    *token_estimate,
                    *context_max,
                )));
                self.follow = true;
            }
            AppEvent::RefineDelta { delta } => self.push_delta(delta),
            AppEvent::RefinePlanUpdated { steps } => {
                self.plan = steps.clone();
                self.follow = true;
            }
            AppEvent::RefineApprovalRequest { id, summary, diff } => {
                self.pending = Some(RefinePending::Approval {
                    id: *id,
                    summary: summary.clone(),
                    diff: diff.clone(),
                });
                self.follow = true;
            }
            AppEvent::RefineDecisionRequest {
                id,
                question,
                options,
            } => {
                self.pending = Some(RefinePending::Decision {
                    id: *id,
                    question: question.clone(),
                    options: options.clone(),
                    selected: 0,
                });
                self.follow = true;
            }
            AppEvent::RefineToolInvoked { tool, summary } => {
                self.settle_reasoning();
                self.conversation
                    .push(Turn::tool(format!("{tool} — {summary}")));
                self.follow = true;
            }
            AppEvent::RefineSubagentUpdated {
                id,
                status,
                summary,
            } => {
                self.update_subagent(id, *status, summary);
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

    fn update_subagent(&mut self, id: &str, status: RefineSubagentStatus, summary: &str) {
        if let Some(run) = self.subagents.iter_mut().find(|run| run.id == id) {
            run.status = status;
            if status == RefineSubagentStatus::Running {
                run.title = summary.to_string();
            } else {
                run.summary = summary.to_string();
            }
            return;
        }

        let (title, run_summary) = if status == RefineSubagentStatus::Running {
            (summary.to_string(), String::new())
        } else {
            ("sub-agent".to_string(), summary.to_string())
        };
        self.subagents.push(SubagentRun {
            id: id.to_string(),
            title,
            status,
            summary: run_summary,
        });
    }

    fn push_delta(&mut self, delta: &str) {
        let keep_following = self.at_bottom();
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
        if keep_following {
            self.jump_bottom();
        }
    }

    fn push_reasoning(&mut self, delta: &str) {
        let keep_following = self.at_bottom();
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
        if keep_following {
            self.jump_bottom();
        }
    }

    fn at_bottom(&self) -> bool {
        self.follow || self.scroll >= self.last_bottom
    }

    fn jump_bottom(&mut self) {
        self.follow = true;
        self.scroll = self.last_bottom;
    }

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
            &[
                ("⌃C", "interrupt"),
                ("⌃End", "bottom"),
                ("⌃R", "details"),
                ("↑↓", "scroll"),
            ]
        } else if self.focused {
            &[
                ("↵", "send"),
                ("@", "mention"),
                ("/", "cmd"),
                ("⌃End", "bottom"),
                ("⌃R", "details"),
                ("esc", "unfocus"),
            ]
        } else {
            &[
                ("type", "focus"),
                ("⌃End", "bottom"),
                ("↑↓", "scroll"),
                ("⌃R", "details"),
            ]
        }
    }

    pub fn render(
        &mut self,
        f: &mut Frame,
        area: Rect,
        frame: u64,
        has_project: bool,
        theme: &Theme,
    ) {
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
        if !self.subagents.is_empty() {
            let subagent_h = (self.subagents.len() as u16 + 2).clamp(3, 7);
            constraints.push(Constraint::Length(subagent_h));
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
        if !self.subagents.is_empty() {
            self.render_subagents(f, rows[next], frame, theme);
            next += 1;
        }
        if let Some(status) = status {
            self.render_status(f, rows[next], status, theme);
        }
        self.render_input(f, input_row, theme);
        if self.picker.is_some() {
            self.render_session_picker(f, area, theme);
        } else if self.pending.is_some() {
            self.render_pending(f, area, theme);
        } else {
            self.render_popup(f, area, input_row.y, theme);
        }
    }

    fn render_pending(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        let Some(pending) = &self.pending else {
            return;
        };
        let mut lines: Vec<Line> = Vec::new();
        let (title, hint) = match pending {
            RefinePending::Approval { summary, diff, .. } => {
                lines.push(Line::from(Span::styled(
                    summary.clone(),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::raw(""));
                for l in diff.lines().take(20) {
                    let color = if l.starts_with('+') {
                        theme.status_done
                    } else if l.starts_with('-') {
                        theme.status_failed
                    } else {
                        theme.ink_soft
                    };
                    lines.push(Line::from(Span::styled(
                        l.to_string(),
                        Style::default().fg(color),
                    )));
                }
                ("Approve edit?", "↵ accept   ·   r / Esc reject")
            }
            RefinePending::Decision {
                question,
                options,
                selected,
                ..
            } => {
                lines.push(Line::from(Span::styled(
                    question.clone(),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::raw(""));
                if options.is_empty() {
                    lines.push(Line::from(Span::styled(
                        "type your answer below, then ↵",
                        Style::default().fg(theme.ink_soft),
                    )));
                } else {
                    for (i, opt) in options.iter().enumerate() {
                        let marker = if i == *selected { "▸ " } else { "  " };
                        let style = if i == *selected {
                            Style::default()
                                .fg(theme.accent)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(theme.ink)
                        };
                        lines.push(Line::from(Span::styled(format!("{marker}{opt}"), style)));
                    }
                }
                (
                    "The agent needs a decision",
                    if options.is_empty() {
                        "↵ submit   ·   Esc cancel"
                    } else {
                        "↑↓ select   ·   ↵ choose   ·   Esc cancel"
                    },
                )
            }
        };
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            hint.to_string(),
            Style::default().fg(theme.ink_faint),
        )));

        let w = area.width.min(72);
        let h = (lines.len() as u16 + 2)
            .min(area.height.saturating_sub(2))
            .max(5);
        let modal = Rect {
            x: area.x + (area.width.saturating_sub(w)) / 2,
            y: area.y + (area.height.saturating_sub(h)) / 2,
            width: w,
            height: h,
        };
        f.render_widget(Clear, modal);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent))
            .title(format!(" {title} "))
            .style(Style::default().bg(theme.bg_panel));
        let inner = block.inner(modal);
        f.render_widget(block, modal);
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }

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
                        Style::default()
                            .fg(theme.ink_faint)
                            .add_modifier(Modifier::CROSSED_OUT),
                    ),
                    PlanStepStatus::InProgress => (
                        "▸ ",
                        Style::default()
                            .fg(theme.accent)
                            .add_modifier(Modifier::BOLD),
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

    fn render_subagents(&self, f: &mut Frame, area: Rect, frame: u64, theme: &Theme) {
        let running = self
            .subagents
            .iter()
            .filter(|run| run.status == RefineSubagentStatus::Running)
            .count();
        let title = if running == 0 {
            format!(" ◇ sub-agents · {} ", self.subagents.len())
        } else {
            format!(" ◇ sub-agents · {running} running ")
        };
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
            .subagents
            .iter()
            .rev()
            .take(inner.height as usize)
            .map(|run| {
                let (mark, status, style) = match run.status {
                    RefineSubagentStatus::Running => (
                        theme::refine_spinner_frame(frame),
                        "running",
                        Style::default()
                            .fg(theme.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    RefineSubagentStatus::Succeeded => {
                        ("✓", "done", Style::default().fg(theme.status_done))
                    }
                    RefineSubagentStatus::Failed => {
                        ("!", "failed", Style::default().fg(theme.status_failed))
                    }
                    RefineSubagentStatus::Canceled => {
                        ("×", "canceled", Style::default().fg(theme.ink_faint))
                    }
                };
                let detail = if run.summary.trim().is_empty() {
                    status.to_string()
                } else {
                    format!("{status} · {}", run.summary.trim())
                };
                Line::from(Span::styled(
                    truncate_cols(&format!("{mark} {} — {detail}", run.title.trim()), w),
                    style,
                ))
            })
            .collect();
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme.bg_panel)),
            inner,
        );
    }

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

        let was_at_bottom = self.at_bottom();
        let total_lines = wrapped_line_count(&lines, width);
        let view_h = inner.height;
        self.last_bottom = total_lines.saturating_sub(view_h);
        let scroll = if self.follow || was_at_bottom {
            self.jump_bottom();
            self.last_bottom
        } else {
            self.scroll = self.scroll.min(self.last_bottom);
            self.scroll
        };

        let para = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .style(Style::default().bg(theme.bg_panel));
        f.render_widget(para, inner);
    }

    /// Working line while a turn runs; idle usage summary otherwise.
    fn status_line(&self, frame: u64, theme: &Theme) -> Option<Line<'static>> {
        let faint = Style::default().fg(theme.ink_faint);
        let soft = Style::default().fg(theme.ink_soft);
        if self.in_flight {
            let elapsed = self.turn_started.map(|s| s.elapsed()).unwrap_or_default();
            let (inp, out) = self.turn_tokens;
            let mut spans = vec![
                Span::styled(
                    format!("{} ", theme::refine_spinner_frame(frame)),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
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
            spans.push(Span::styled(" · Ctrl-C to interrupt)", faint));
            return Some(Line::from(spans));
        }

        let (inp, out) = self.session_tokens;
        let mut text = format!("mode: {} ⌃⇥", self.approval_mode.label());
        if inp != 0 || out != 0 {
            text.push_str(&format!(
                " · ↑ {} ↓ {} · {} tokens",
                fmt_tokens(inp),
                fmt_tokens(out),
                fmt_tokens(inp.saturating_add(out))
            ));
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
                    while i < self.conversation.len() && self.conversation[i].role == TurnRole::Tool
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

        let max_rows = rows.len().min(6);
        let selected = rows.iter().position(|(_, selected)| *selected).unwrap_or(0);
        let offset = popup_window_start(rows.len(), selected, max_rows);
        let height = max_rows as u16 + 2;
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
            .skip(offset)
            .take(max_rows)
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

fn popup_window_start(row_count: usize, selected: usize, max_rows: usize) -> usize {
    if row_count <= max_rows || max_rows == 0 {
        return 0;
    }
    let selected = selected.min(row_count - 1);
    selected
        .saturating_add(1)
        .saturating_sub(max_rows)
        .min(row_count - max_rows)
}

fn wrapped_line_count(lines: &[Line<'_>], width: usize) -> u16 {
    if width == 0 {
        return 0;
    }
    let rows: usize = lines
        .iter()
        .map(|line| {
            let cols: usize = line
                .spans
                .iter()
                .map(|span| col_width(span.content.as_ref()))
                .sum();
            cols.div_ceil(width).max(1)
        })
        .sum();
    rows.min(u16::MAX as usize) as u16
}

/// Transcript notice for context compaction.
fn compaction_notice(dropped: usize, token_estimate: u32, context_max: u32) -> String {
    format!(
        "context summarized - {dropped} old message(s), before ~{} / {} tokens",
        fmt_tokens(token_estimate),
        fmt_tokens(context_max)
    )
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
    fn popup_window_follows_selection_past_first_page() {
        assert_eq!(popup_window_start(17, 0, 6), 0);
        assert_eq!(popup_window_start(17, 5, 6), 0);
        assert_eq!(popup_window_start(17, 6, 6), 1);
        assert_eq!(popup_window_start(17, 16, 6), 11);
    }

    #[test]
    fn fix_short_names_slash_submits_cleanup_prompt_with_scope() {
        let mut s = RefineScreen::new();

        let action = s.run_slash("/fix-short-names @v1/c3");

        match action {
            Action::RefineSubmit { text } => {
                assert!(text.contains("Fix over-expanded character names"));
                assert!(text.contains("read_lexicon"));
                assert!(text.contains("also_called"));
                assert!(text.contains("Scope hint: @v1/c3"));
            }
            other => panic!("expected RefineSubmit, got {other:?}"),
        }
    }

    #[test]
    fn fix_review_needed_slash_submits_triage_prompt_with_scope() {
        let mut s = RefineScreen::new();

        let action = s.run_slash("/fix-review-needed @v5/c3");

        match action {
            Action::RefineSubmit { text } => {
                assert!(text.contains("Investigate and fix `honya:review-needed` chunks"));
                assert!(text.contains("dialogue/POV/pronoun/register"));
                assert!(text.contains("`自分` inside dialogue"));
                assert!(text.contains("Treat the reviewer reason as a clue"));
                assert!(text.contains("Scope hint: @v5/c3"));
            }
            other => panic!("expected RefineSubmit, got {other:?}"),
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
            Message::user(
                "also update tone\n\n[Mid-run steering: hidden note]\n\n[Approval mode: auto]",
            ),
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
        assert_eq!(turns.len(), 4);
        assert_eq!(turns[0].role, TurnRole::User);
        assert_eq!(turns[0].text, "fix the prose"); // scope hint stripped
        assert_eq!(turns[1].role, TurnRole::User);
        assert_eq!(turns[1].text, "also update tone"); // steering metadata stripped
        assert_eq!(turns[2].role, TurnRole::Tool);
        assert_eq!(turns[2].text, "edit_chapter");
        assert_eq!(turns[3].role, TurnRole::Assistant);
    }

    #[test]
    fn in_flight_reflects_running_turn_state() {
        let mut s = RefineScreen::new();
        assert!(!s.is_in_flight());
        s.begin_turn();
        assert!(s.is_in_flight());
        s.on_app_event(&AppEvent::RefineMessageDone);
        assert!(!s.is_in_flight());
    }

    #[test]
    fn display_turns_shows_compacted_history_as_tool_notice() {
        let messages = vec![Message::user(format!(
            "{COMPACT_SUMMARY_PREFIX}\nUser: earlier request"
        ))];
        let turns = display_turns(&messages);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].role, TurnRole::Tool);
        assert!(turns[0].text.contains("context summarized"));
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
        assert!(
            !s.conversation[0].streaming,
            "reasoning stops when the answer starts"
        );
        assert_eq!(s.conversation[1].role, TurnRole::Assistant);

        let md = s.transcript_markdown();
        assert!(
            md.contains("thinking —"),
            "collapsed reasoning summary shown"
        );
        assert!(
            !md.contains("weighing the options"),
            "full reasoning hidden when collapsed"
        );

        s.expanded = true;
        let md = s.transcript_markdown();
        assert!(
            md.contains("weighing the options"),
            "expanded reasoning shows the text"
        );
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
        assert!(
            md.contains("3 actions"),
            "consecutive tool lines coalesce: {md}"
        );
        assert!(
            !md.contains("grep_chapter"),
            "details hidden when collapsed"
        );

        s.expanded = true;
        let md = s.transcript_markdown();
        assert!(md.contains("grep_chapter") && md.contains("edit_chapter"));
        assert!(!md.contains("3 actions"));
    }

    #[test]
    fn compaction_event_adds_visible_tool_notice() {
        let mut s = RefineScreen::new();
        s.on_app_event(&AppEvent::RefineContextCompacted {
            dropped_messages: 4,
            token_estimate: 120_000,
            context_max: 128_000,
        });
        assert_eq!(s.conversation.len(), 1);
        assert_eq!(s.conversation[0].role, TurnRole::Tool);
        let md = s.transcript_markdown();
        assert!(md.contains("context summarized"));
        assert!(md.contains("4 old message"));
    }

    #[test]
    fn subagent_events_populate_panel_state_and_clear() {
        let mut s = RefineScreen::new();

        s.on_app_event(&AppEvent::RefineSubagentUpdated {
            id: "call_1".to_string(),
            status: RefineSubagentStatus::Running,
            summary: "audit volume 2".to_string(),
        });

        assert_eq!(s.subagents.len(), 1);
        assert_eq!(s.subagents[0].title, "audit volume 2");
        assert_eq!(s.subagents[0].status, RefineSubagentStatus::Running);

        s.on_app_event(&AppEvent::RefineSubagentUpdated {
            id: "call_1".to_string(),
            status: RefineSubagentStatus::Succeeded,
            summary: "sub-agent finished (3 tool call(s))".to_string(),
        });

        assert_eq!(s.subagents.len(), 1);
        assert_eq!(s.subagents[0].title, "audit volume 2");
        assert_eq!(s.subagents[0].status, RefineSubagentStatus::Succeeded);
        assert!(s.subagents[0].summary.contains("3 tool"));

        s.clear();
        assert!(s.subagents.is_empty());
    }

    #[test]
    fn end_moves_chat_input_cursor_to_back() {
        let mut s = RefineScreen::new();
        s.input = "draft".to_string();
        s.cursor = 0;
        s.last_bottom = 12;
        s.scroll = 3;
        s.follow = false;

        let action = s.handle_key(
            ratatui::crossterm::event::KeyEvent::new(
                KeyCode::End,
                ratatui::crossterm::event::KeyModifiers::empty(),
            ),
            None,
        );

        assert!(matches!(action, Action::None));
        assert!(!s.follow);
        assert_eq!(s.scroll, 3);
        assert_eq!(s.cursor, s.input.len());
    }

    #[test]
    fn ctrl_end_jumps_to_bottom_even_with_input_text() {
        let mut s = RefineScreen::new();
        s.input = "draft".to_string();
        s.cursor = 0;
        s.last_bottom = 9;
        s.scroll = 2;
        s.follow = false;

        let action = s.handle_key(
            ratatui::crossterm::event::KeyEvent::new(
                KeyCode::End,
                ratatui::crossterm::event::KeyModifiers::CONTROL,
            ),
            None,
        );

        assert!(matches!(action, Action::None));
        assert!(s.follow);
        assert_eq!(s.scroll, 9);
        assert_eq!(s.cursor, 0, "Ctrl+End scrolls instead of moving the caret");
    }

    #[test]
    fn ctrl_c_interrupts_in_flight_refine_turn() {
        let mut s = RefineScreen::new();
        s.begin_turn();

        let action = s.handle_key(
            ratatui::crossterm::event::KeyEvent::new(
                KeyCode::Char('c'),
                ratatui::crossterm::event::KeyModifiers::CONTROL,
            ),
            None,
        );

        assert!(matches!(action, Action::RefineCancel));
    }

    #[test]
    fn esc_unfocuses_input_even_while_refine_turn_is_in_flight() {
        let mut s = RefineScreen::new();
        s.begin_turn();
        assert!(s.focused);

        let action = s.handle_key(
            ratatui::crossterm::event::KeyEvent::new(
                KeyCode::Esc,
                ratatui::crossterm::event::KeyModifiers::empty(),
            ),
            None,
        );

        assert!(matches!(action, Action::None));
        assert!(!s.focused);
        assert!(s.in_flight);
    }

    #[test]
    fn streaming_follows_only_when_already_at_bottom() {
        let mut at_bottom = RefineScreen::new();
        at_bottom.follow = false;
        at_bottom.last_bottom = 10;
        at_bottom.scroll = 10;
        at_bottom.on_app_event(&AppEvent::RefineDelta {
            delta: "new text".to_string(),
        });
        assert!(
            at_bottom.follow,
            "bottom viewport keeps following streamed text"
        );

        let mut scrolled_up = RefineScreen::new();
        scrolled_up.follow = false;
        scrolled_up.last_bottom = 10;
        scrolled_up.scroll = 4;
        scrolled_up.on_app_event(&AppEvent::RefineDelta {
            delta: "new text".to_string(),
        });
        assert!(
            !scrolled_up.follow,
            "scrolled-up viewport is not forced to bottom"
        );
        assert_eq!(scrolled_up.scroll, 4);
    }

    #[test]
    fn wrapped_line_count_matches_display_rows() {
        let lines = vec![Line::raw("abcdefghij"), Line::raw("")];
        assert_eq!(wrapped_line_count(&lines, 4), 4);
        assert_eq!(wrapped_line_count(&lines, 80), 2);
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
        assert_eq!(
            s.plan.len(),
            2,
            "the TODO list is persistent — it survives across turns"
        );
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
        assert_eq!(
            s.session_tokens,
            (2500, 500),
            "session total persists across turns"
        );
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
