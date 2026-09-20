//! Refine screen: transcript, chat input, `@` mentions, and slash commands.
//! The focused input captures globals until `Esc` releases it.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::model::{
    AppEvent, PlanStep, PlanStepStatus, Project, RefineQuestion, RefineSubagentStatus,
};
use crate::theme::{self, Theme};
use crate::ui::input::{self, EditOpts, Edited};
use crate::ui::mouse::{MouseGesture, MouseInput};
use crate::ui::text::truncate_cols;
use crate::workspace::refine_session::SessionMeta;

use super::action_table::{self, Act};
use super::Action;
use super::overlay::Overlay;
// `Block` here is ratatui's panel; the transcript's is `blocks::Block`.
use super::refine_blocks::{self as blocks, ToolStatus};
pub use super::refine_blocks::{BlockKind, ToolStatus as BlockToolStatus, TurnRole, display_blocks};

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
    ("/grep", "search the project for text"),
    ("/resume", "pick a session to resume"),
];

/// Lines of a sub-agent's own transcript kept in its block. Enough to read
/// what it did; not enough for a long run to grow without bound.
const CHILD_TRANSCRIPT_CAP: usize = 400;

pub const COMPACT_SUMMARY_PREFIX: &str = "[Earlier conversation, compacted to fit the context window]";

const FIX_REVIEW_NEEDED_PROMPT: &str = r#"Investigate and fix `honya:review-needed` chunks across the requested scope.

Use `update_plan`. First list/count the flagged chunks, then categorize them before editing:
- name/honorific/surface: wrong alias, full-name expansion, `さん`/`先輩`.
- dialogue/POV/pronoun/register: speaker attribution, `俺/僕/あたし/自分`, addressee forms, polite vs rough particles.
- source fidelity: mistranslation, wrong subject, missing line, skipped title/credit.
- residue/format/ruby: Japanese punctuation, `（ ）`, furigana/original glosses, Markdown/HTML drift.
- glossary/terminology: hard_locked/preferred/forbidden terms and handles.
- target-language quality/SFX: awkward literal phrasing, tone drift, unnatural onomatopoeia.
- infrastructure: translator stream cutoff, refusal/policy notice, empty or partial output.

For each chunk you edit, read the matching SOURCE_JP, translated chunk, CHARACTERS, GLOSSARY, and STYLE. Treat the reviewer reason as a clue, not as automatic truth. Fix only actionable issues that SOURCE_JP/REFERENCE confirms. If a reviewer note says a point is correct/acceptable/not an issue, leave that point alone and fix the actual remaining issue.

Special dialogue rule: `自分` inside dialogue may mean the speaker or the listener. Resolve it from adjacent turns; when it means the listener, use that listener's established target-language address form from CHARACTERS rather than a generic form.

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

/// One transcript line per question, with what it was answered.
fn ask_record(s: &AskSession, answer: &str) -> String {
    if answer.is_empty() {
        return "ask_user — dismissed without answering".to_string();
    }
    let mut out = String::from("ask_user — answered");
    for (item, value) in s.items.iter().zip(s.values()) {
        let value = if value.is_empty() { "(skipped)" } else { &value };
        out.push_str(&format!("\n· {} → {}", item.question.trim(), value));
    }
    out
}

/// Read-only view of a blocking refine prompt, for the GUI's prompt card.
#[derive(Debug, Clone)]
pub struct PendingPromptView {
    pub id: u64,
    /// The first question, kept so a caller that only ever showed one still
    /// compiles and still shows something true.
    pub question: String,
    /// Diff text for approval prompts (empty otherwise).
    pub detail: String,
    /// Every question on the card, in order. Empty for an approval.
    pub questions: Vec<RefineQuestion>,
    pub is_approval: bool,
}

/// Blocking prompt awaiting user input.
#[derive(Debug, Clone)]
enum RefinePending {
    Approval {
        id: u64,
        summary: String,
        diff: String,
        scroll: usize,
    },
    Ask(AskSession),
}

impl RefinePending {
    fn id(&self) -> u64 {
        match self {
            RefinePending::Approval { id, .. } => *id,
            RefinePending::Ask(s) => s.id,
        }
    }
}

/// One `ask_user` card: every question the agent asked in that one call, and
/// what has been answered so far.
#[derive(Debug, Clone)]
struct AskSession {
    id: u64,
    items: Vec<RefineQuestion>,
    answers: Vec<Answer>,
    /// Which question the card is showing.
    at: usize,
    /// Which rung of the answer list has the keyboard. `options.len()` is the
    /// free-text box, which is always the last rung.
    row: usize,
    /// Esc was pressed with something typed; a second Esc discards.
    confirm_dismiss: bool,
}

#[derive(Debug, Clone, Default)]
struct Answer {
    text: String,
    cursor: usize,
    /// Chosen options. A `Vec` because multi-select is per question; a single
    /// answer holds at most one.
    picked: Vec<usize>,
}

impl AskSession {
    /// The only constructor, so `answers` cannot drift from `items`.
    fn new(id: u64, items: Vec<RefineQuestion>) -> Self {
        let answers = vec![Answer::default(); items.len()];
        let mut s = Self {
            id,
            items,
            answers,
            at: 0,
            row: 0,
            confirm_dismiss: false,
        };
        s.row = s.default_row();
        s
    }

    fn item(&self) -> &RefineQuestion {
        &self.items[self.at.min(self.items.len() - 1)]
    }

    /// The box's rung: after the options, always reachable.
    fn box_row(&self) -> usize {
        self.item().options.len()
    }

    /// Where the keyboard lands on arriving at a question: the first option —
    /// which is the box itself when the question offers none.
    fn default_row(&self) -> usize {
        0
    }

    fn answer(&self) -> &Answer {
        &self.answers[self.at.min(self.answers.len() - 1)]
    }

    fn answer_mut(&mut self) -> &mut Answer {
        let at = self.at.min(self.answers.len() - 1);
        &mut self.answers[at]
    }

    /// One rule for "options and a box, always": what was typed wins, because
    /// typing it is the more specific act.
    fn value_of(item: &RefineQuestion, answer: &Answer) -> String {
        if !answer.text.trim().is_empty() {
            return answer.text.trim().to_string();
        }
        answer
            .picked
            .iter()
            .filter_map(|i| item.options.get(*i))
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn values(&self) -> Vec<String> {
        self.items
            .iter()
            .zip(&self.answers)
            .map(|(item, a)| Self::value_of(item, a))
            .collect()
    }

    fn is_dirty(&self) -> bool {
        self.answers
            .iter()
            .any(|a| !a.text.trim().is_empty() || !a.picked.is_empty())
    }

    /// Enter on an option is a commitment about that option, so it replaces
    /// what was typed. Moving the selection never does — that would let a
    /// mis-arrow destroy a sentence.
    fn commit_row(&mut self) {
        let row = self.row;
        let multiple = self.item().multiple;
        let options = self.item().options.len();
        if row >= options {
            return;
        }
        let a = self.answer_mut();
        if multiple {
            match a.picked.iter().position(|p| *p == row) {
                Some(i) => {
                    a.picked.remove(i);
                }
                None => a.picked.push(row),
            }
            a.picked.sort_unstable();
        } else {
            a.picked = vec![row];
        }
        a.text.clear();
        a.cursor = 0;
    }

    fn go_to(&mut self, at: usize) {
        self.at = at.min(self.items.len().saturating_sub(1));
        self.row = self.default_row();
        self.confirm_dismiss = false;
    }

    fn is_last(&self) -> bool {
        self.at + 1 >= self.items.len()
    }
}

/// One run, as a caller outside this module sees it.
pub struct SubagentView<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub role: &'a str,
    pub model: &'a str,
    pub background: bool,
    pub status: RefineSubagentStatus,
    pub activity: &'a str,
    pub summary: &'a str,
    pub elapsed: std::time::Duration,
    pub plan: &'a [PlanStep],
}

#[derive(Debug, Clone)]
struct SubagentRun {
    id: String,
    depth: usize,
    title: String,
    role: String,
    model: String,
    /// Still running after the turn that spawned it ended.
    background: bool,
    status: RefineSubagentStatus,
    activity: String,
    summary: String,
    plan: Vec<PlanStep>,
    /// Kept here rather than sent on every event, so the elapsed time ticks
    /// between updates instead of freezing at the last one.
    started: std::time::Instant,
    ran_for: Option<std::time::Duration>,
}

impl SubagentRun {
    /// A run the UI heard about sideways — a plan or an activity line for an
    /// id no `Started` announced. Better a row with gaps than a silent drop.
    fn unannounced(id: &str) -> Self {
        Self {
            id: id.to_string(),
            depth: id.matches('/').count(),
            title: "sub-agent".to_string(),
            role: String::new(),
            model: String::new(),
            background: false,
            status: RefineSubagentStatus::Running,
            activity: String::new(),
            summary: String::new(),
            plan: Vec::new(),
            started: std::time::Instant::now(),
            ran_for: None,
        }
    }

    fn elapsed(&self) -> std::time::Duration {
        self.ran_for.unwrap_or_else(|| self.started.elapsed())
    }
}

/// One block's rendered lines, and the state they were rendered from.
struct BlockRender {
    key: Option<u64>,
    lines: Vec<Line<'static>>,
    /// Display rows at the cached width, so the row index costs no re-measure.
    rows: u16,
}

pub struct RefineScreen {
    pub blocks: Vec<blocks::Block>,
    /// Parallel to `blocks`; a streamed delta rebuilds one entry, not all.
    renders: Vec<BlockRender>,
    next_block: u32,
    /// Which block the transcript cursor is on. `None` while the input has the
    /// keyboard — in the transcript, the cursor *is* the focus.
    selected: Option<usize>,
    /// Where each block landed in the last frame: `(start row, rows)`. Zones
    /// and scroll-into-view both read this, so what you can click and what was
    /// drawn cannot drift.
    placed: Vec<(u16, u16)>,
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
    /// A queue, not a slot: two sub-agents can each raise a question, and the
    /// second used to overwrite the first and strand its oneshot for good.
    pending: std::collections::VecDeque<RefinePending>,
    approval_mode: crate::agents::refine::ApprovalMode,
    plan: Vec<PlanStep>,
    subagents: Vec<SubagentRun>,
    in_flight: bool,
    turn_started: Option<std::time::Instant>,
    last_turn_elapsed: Option<std::time::Duration>,
    turn_tokens: (u32, u32),
    session_tokens: (u32, u32),
    last_context: u32,
    context_max: u32,
    transcript_area: Rect,
    input_area: Rect,
    /// Session-picker list geometry (inner rect + window start), refreshed on
    /// render while the picker is open, for click hit-testing.
    picker_area: Rect,
    picker_start: usize,
    /// The tasks pane's selection while it is open. Like the session picker,
    /// it owns the keyboard while it is up, which is what makes bare letters
    /// safe there and nowhere else on this screen.
    tasks: Option<usize>,
    tasks_hide_done: bool,
    tasks_area: Rect,
    tasks_start: usize,
    /// A sub-agent's own transcript, opened full-frame. Read-only: the way to
    /// talk to a child is the agent's `message_subagent`, not this.
    child: Option<String>,
    child_scroll: u16,
    /// Slash/mention popup list geometry, refreshed on render while open.
    popup_area: Rect,
    popup_offset: usize,
}

impl Default for RefineScreen {
    fn default() -> Self {
        Self::new()
    }
}

/// Action ids for this screen's table. Stable within the screen: they are also
/// the zone index every one of its controls registers under.
const R_NEW: u16 = 0;
const R_SESSIONS: u16 = 1;
const R_APPROVAL: u16 = 2;
const R_COMPACT: u16 = 3;
const R_EXPORT: u16 = 4;
const R_UNDO: u16 = 5;
const R_COPY: u16 = 6;
const R_TASKS: u16 = 7;

impl RefineScreen {
    pub fn new() -> Self {
        Self {
            blocks: Vec::new(),
            renders: Vec::new(),
            next_block: 0,
            selected: None,
            placed: Vec::new(),
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
            pending: std::collections::VecDeque::new(),
            approval_mode: crate::agents::refine::ApprovalMode::default(),
            plan: Vec::new(),
            subagents: Vec::new(),
            in_flight: false,
            turn_started: None,
            last_turn_elapsed: None,
            turn_tokens: (0, 0),
            session_tokens: (0, 0),
            last_context: 0,
            context_max: 128_000,
            transcript_area: Rect::default(),
            input_area: Rect::default(),
            picker_area: Rect::default(),
            picker_start: 0,
            tasks: None,
            tasks_hide_done: false,
            tasks_area: Rect::default(),
            tasks_start: 0,
            child: None,
            child_scroll: 0,
            popup_area: Rect::default(),
            popup_offset: 0,
        }
    }

    /// The next block id. Ids are never reused within a session, so a cached
    /// render can never be handed to a different block.
    fn block_id(&mut self) -> u32 {
        self.next_block += 1;
        self.next_block
    }

    fn push_block(&mut self, block: blocks::Block) {
        self.blocks.push(block);
    }

    /// Append a one-line notice — something that happened to the conversation
    /// rather than in it.
    fn notice(&mut self, text: String) {
        let id = self.block_id();
        self.push_block(blocks::Block::notice(id, text));
    }

    /// Consulted by `App::screen_is_capturing()` to suppress single-letter globals.
    /// True when a pane that owns the whole keyboard is up.
    pub fn owns_keyboard(&self) -> bool {
        self.child.is_some() || self.tasks.is_some()
    }

    pub fn is_capturing(&self) -> bool {
        self.focused || self.picker.is_some() || !self.pending.is_empty() || self.owns_keyboard()
    }

    pub fn approval_mode(&self) -> crate::agents::refine::ApprovalMode {
        self.approval_mode
    }

    pub fn active_session_id(&self) -> &str {
        &self.active_session
    }

    /// (used, max) context tokens after the last turn (GUI meter).
    pub fn context_meter(&self) -> (u32, u32) {
        (self.last_context, self.context_max)
    }

    /// Read-only projection of the blocking prompt awaiting the user, if any.
    /// Empty `options` means a free-text answer; `is_approval` prompts carry a
    /// diff and answer with `"approve"` / `""` (reject).
    pub fn pending_prompt(&self) -> Option<PendingPromptView> {
        self.pending.front().map(|p| match p {
            RefinePending::Approval {
                id, summary, diff, ..
            } => PendingPromptView {
                id: *id,
                question: summary.clone(),
                detail: diff.clone(),
                questions: Vec::new(),
                is_approval: true,
            },
            RefinePending::Ask(s) => PendingPromptView {
                id: s.id,
                question: s
                    .items
                    .first()
                    .map(|q| q.question.clone())
                    .unwrap_or_default(),
                detail: String::new(),
                questions: s.items.clone(),
                is_approval: false,
            },
        })
    }

    pub fn load_blocks(&mut self, blocks: Vec<blocks::Block>, active_session: String) {
        self.next_block = blocks.iter().map(|b| b.id).max().unwrap_or(0);
        self.blocks = blocks;
        self.renders.clear();
        self.selected = None;
        self.placed.clear();
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

    /// Which row the picker is on, for a front end that draws its own list.
    pub fn picker_selection(&self) -> Option<usize> {
        self.picker
    }

    pub fn set_picker_selection(&mut self, sel: usize) {
        if self.picker.is_some() {
            self.picker = Some(sel);
        }
    }

    pub fn close_picker(&mut self) {
        self.picker = None;
    }

    pub fn clear(&mut self) {
        self.blocks.clear();
        self.renders.clear();
        self.selected = None;
        self.placed.clear();
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
        self.pending.clear();
        for run in &mut self.subagents {
            if run.status == RefineSubagentStatus::Running {
                run.status = RefineSubagentStatus::Canceled;
                run.summary = "cancelled by the user".to_string();
            }
        }
        self.streaming = false;
        self.finish_turn();
    }

    /// The card answers to the pointer too: an option is a row the list
    /// registered while drawing it, so nothing here recomputes where one went.
    fn handle_pending_mouse(
        &mut self,
        m: MouseInput,
        zone: Option<crate::ui::kit::ZoneId>,
    ) -> Action {
        use crate::ui::kit::ZoneKind;

        if matches!(self.pending.front(), Some(RefinePending::Approval { .. })) {
            return match m.gesture {
                MouseGesture::ScrollUp => self.scroll_approval(-3),
                MouseGesture::ScrollDown => self.scroll_approval(3),
                _ => Action::None,
            };
        }
        let Some(RefinePending::Ask(s)) = self.pending.front_mut() else {
            return Action::None;
        };
        match m.gesture {
            MouseGesture::ScrollUp => s.row = s.row.saturating_sub(1),
            MouseGesture::ScrollDown => s.row = (s.row + 1).min(s.box_row()),
            MouseGesture::RightClick => {}
            MouseGesture::Click { .. } => match zone.map(|z| (z.kind, z.index)) {
                Some((ZoneKind::Row, i)) if (i as usize) < s.box_row() => {
                    s.row = i as usize;
                    s.commit_row();
                }
                Some((ZoneKind::TextSurface, _)) => s.row = s.box_row(),
                _ => {}
            },
        }
        Action::None
    }

    /// Hand the answer back to the agent and take the card down.
    ///
    /// The card is gone once it is answered, so what was asked and what was
    /// decided is left in the transcript — a decision is part of the
    /// conversation, not a dialog that happened to it.
    fn resolve_front(&mut self, id: u64, answer: String) -> Action {
        if let Some(RefinePending::Ask(s)) = self.pending.front() {
            let record = ask_record(s, &answer);
            self.notice(record);
        }
        self.pending.pop_front();
        self.follow = true;
        Action::RefineRespondInteraction { id, answer }
    }

    fn handle_pending_key(&mut self, key: KeyEvent) -> Action {
        let Some(front) = self.pending.front() else {
            return Action::None;
        };
        let id = front.id();
        if matches!(front, RefinePending::Approval { .. }) {
            return match key.code {
                KeyCode::Enter | KeyCode::Char('y') => self.resolve_front(id, "approve".to_string()),
                KeyCode::Esc | KeyCode::Char('r') | KeyCode::Char('n') => {
                    self.resolve_front(id, String::new())
                }
                KeyCode::Up | KeyCode::Char('k') => self.scroll_approval(-1),
                KeyCode::Down | KeyCode::Char('j') => self.scroll_approval(1),
                KeyCode::PageUp => self.scroll_approval(-10),
                KeyCode::PageDown => self.scroll_approval(10),
                _ => Action::None,
            };
        }
        let done = match self.pending.front_mut() {
            Some(RefinePending::Ask(s)) => Self::fold_ask_key(s, key),
            _ => None,
        };
        match done {
            Some(answer) => self.resolve_front(id, answer),
            None => Action::None,
        }
    }

    fn scroll_approval(&mut self, delta: isize) -> Action {
        if let Some(RefinePending::Approval { scroll, .. }) = self.pending.front_mut() {
            *scroll = scroll.saturating_add_signed(delta);
        }
        Action::None
    }

    /// Fold one key into the card. `Some` is the reply to send; an empty reply
    /// is a dismissal, which is what the agent already treats it as.
    ///
    /// Only keys that cannot be typed navigate. That is the whole point of the
    /// card: the box is always live, so a letter is always a letter — there is
    /// no mode to notice being in. The caret keeps Home/End and the word-wise
    /// Ctrl-arrows; bare ←/→ move between questions instead.
    fn fold_ask_key(s: &mut AskSession, key: KeyEvent) -> Option<String> {
        use ratatui::crossterm::event::KeyModifiers;

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('s') {
            return Some(serde_json::to_string(&s.values()).unwrap_or_default());
        }
        if key.code != KeyCode::Esc {
            s.confirm_dismiss = false;
        }
        match key.code {
            KeyCode::Esc => {
                // Losing a typed answer to a stray Esc is worse than pressing
                // it twice, and a second Esc is cheaper than a dialog over a
                // card that deliberately is not one.
                if s.is_dirty() && !s.confirm_dismiss {
                    s.confirm_dismiss = true;
                    return None;
                }
                return Some(String::new());
            }
            KeyCode::Up => {
                s.row = s.row.saturating_sub(1);
                return None;
            }
            KeyCode::Down => {
                s.row = (s.row + 1).min(s.box_row());
                return None;
            }
            KeyCode::Left | KeyCode::BackTab if !ctrl => {
                let at = s.at.saturating_sub(1);
                s.go_to(at);
                return None;
            }
            KeyCode::Right | KeyCode::Tab if !ctrl => {
                let at = s.at + 1;
                s.go_to(at);
                return None;
            }
            KeyCode::Enter => {
                let on_option = s.row < s.box_row();
                let multiple = s.item().multiple;
                if on_option {
                    s.commit_row();
                    // Several answers may apply, so Enter accumulates instead
                    // of moving on; the user leaves when they are done.
                    if multiple {
                        return None;
                    }
                }
                if s.is_last() {
                    return Some(serde_json::to_string(&s.values()).unwrap_or_default());
                }
                let at = s.at + 1;
                s.go_to(at);
                return None;
            }
            _ => {}
        }

        let box_row = s.box_row();
        let a = s.answer_mut();
        let mut text = std::mem::take(&mut a.text);
        let mut cursor = a.cursor.min(text.len());
        let edited = input::handle(&mut text, &mut cursor, key, EditOpts::default());
        a.text = text;
        a.cursor = cursor;
        if edited == Edited::Changed {
            a.picked.clear();
            // Typing moves the keyboard onto the box, so what Enter will do is
            // visible before it is pressed — and arrowing back up to an option
            // stays an explicit choice to use that option instead.
            s.row = box_row;
        }
        None
    }

    pub fn handle_key(&mut self, key: KeyEvent, project: Option<&Project>) -> Action {
        if self.in_flight
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c')
        {
            return Action::RefineCancel;
        }

        // Each of these owns the keyboard while it is up, which is what makes
        // bare letters safe inside them and nowhere else on this screen.
        if self.child.is_some() {
            return self.handle_child_key(key);
        }
        if let Some(sel) = self.tasks {
            return self.handle_tasks_key(key, sel);
        }
        if let Some(sel) = self.picker {
            return self.handle_picker_key(key, sel);
        }
        // Unconditional: a free-text answer used to fall through to the chat
        // input and the action table, so a typed answer landed in the wrong
        // buffer and a `/` opened the command popup over a blocking question.
        if !self.pending.is_empty() {
            return self.handle_pending_key(key);
        }
        // Commands come from the table, ahead of the input, so a chord the
        // screen declares is never eaten by the field being typed into.
        let acts = self.acts(project.is_some());
        match action_table::hit(&acts, &key) {
            action_table::KeyHit::Run(id) => return self.run_action(id).unwrap_or(Action::None),
            action_table::KeyHit::Blocked => return Action::None,
            action_table::KeyHit::Miss => {}
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r') {
            self.fold_all();
            return Action::None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::End {
            self.jump_bottom();
            return Action::None;
        }
        // With the input unfocused the transcript has the keyboard, and what
        // focus *is* there is a block cursor. Typing still starts a message,
        // so nothing single-letter can be bound here.
        if !self.focused {
            let shift = key.modifiers.contains(KeyModifiers::SHIFT);
            match key.code {
                KeyCode::Enter
                    if self
                        .selected
                        .and_then(|i| self.blocks.get(i))
                        .and_then(|b| b.subagent_id())
                        .is_some() =>
                {
                    self.child = self
                        .selected
                        .and_then(|i| self.blocks.get(i))
                        .and_then(|b| b.subagent_id())
                        .map(str::to_string);
                    self.child_scroll = 0;
                    return Action::None;
                }
                KeyCode::Char(_) | KeyCode::Enter => self.focused = true,
                KeyCode::Up if shift => {
                    self.jump_turn(-1);
                    return Action::None;
                }
                KeyCode::Down if shift => {
                    self.jump_turn(1);
                    return Action::None;
                }
                KeyCode::Up => {
                    self.move_cursor(-1);
                    return Action::None;
                }
                KeyCode::Down => {
                    self.move_cursor(1);
                    return Action::None;
                }
                KeyCode::Left => {
                    self.fold_selected(false);
                    return Action::None;
                }
                KeyCode::Right => {
                    self.fold_selected(true);
                    return Action::None;
                }
                KeyCode::PageUp => {
                    self.selected = None;
                    self.scroll_up(5);
                    return Action::None;
                }
                KeyCode::PageDown => {
                    self.selected = None;
                    self.scroll_down(5);
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

        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Enter => self.submit(),
            KeyCode::Esc => {
                self.focused = false;
                Action::None
            }
            // Nothing is typed, so an arrow has nothing to do in the field —
            // it may as well be the block cursor, and then the cursor needs no
            // separate step to reach.
            KeyCode::Up if self.input.is_empty() => {
                if shift {
                    self.jump_turn(-1);
                } else {
                    self.move_cursor(-1);
                }
                Action::None
            }
            KeyCode::Down if self.input.is_empty() => {
                if shift {
                    self.jump_turn(1);
                } else {
                    self.move_cursor(1);
                }
                Action::None
            }
            KeyCode::Left if self.input.is_empty() && self.selected.is_some() => {
                self.fold_selected(false);
                Action::None
            }
            KeyCode::Right if self.input.is_empty() && self.selected.is_some() => {
                self.fold_selected(true);
                Action::None
            }
            KeyCode::PageUp => {
                self.selected = None;
                self.scroll_up(5);
                Action::None
            }
            KeyCode::PageDown => {
                self.selected = None;
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

    /// Mouse: while the session picker is open it owns every gesture (wheel walks
    /// the list, click selects / opens, right-click closes); while a slash/mention
    /// popup is open the wheel moves its selection and a click on a row accepts
    /// it. Otherwise the wheel scrolls the transcript and a click focuses input.
    pub fn handle_mouse(
        &mut self,
        m: MouseInput,
        zone: Option<crate::ui::kit::ZoneId>,
    ) -> Action {
        if self.child.is_some() {
            match m.gesture {
                MouseGesture::ScrollUp => {
                    self.child_scroll = self.child_scroll.saturating_sub(3)
                }
                MouseGesture::ScrollDown => {
                    self.child_scroll = self.child_scroll.saturating_add(3)
                }
                MouseGesture::RightClick => self.child = None,
                _ => {}
            }
            return Action::None;
        }
        if let Some(sel) = self.tasks {
            let rows = self.task_rows();
            match m.gesture {
                MouseGesture::ScrollUp => self.tasks = Some(sel.saturating_sub(1)),
                MouseGesture::ScrollDown => {
                    self.tasks = Some((sel + 1).min(rows.len().saturating_sub(1)))
                }
                MouseGesture::RightClick => self.tasks = None,
                MouseGesture::Click { .. } if m.in_rect(self.tasks_area) => {
                    if let Some(i) = zone.and_then(|z| z.row_index())
                        && i < rows.len()
                    {
                        self.tasks = Some(i);
                    }
                }
                MouseGesture::Click { .. } => self.tasks = None,
            }
            return Action::None;
        }
        if let Some(sel) = self.picker {
            return self.handle_picker_mouse(m, sel);
        }
        if !self.pending.is_empty() {
            return self.handle_pending_mouse(m, zone);
        }
        if !matches!(self.popup, Popup::None) {
            match m.gesture {
                MouseGesture::ScrollUp => {
                    self.popup_move(-1);
                    return Action::None;
                }
                MouseGesture::ScrollDown => {
                    self.popup_move(1);
                    return Action::None;
                }
                MouseGesture::Click { .. } if m.in_rect(self.popup_area) => {
                    // The entry's index comes from the registry rather than
                    // from the popup origin plus the scroll offset.
                    if let Some(idx) = zone.and_then(|z| z.row_index())
                        && self.popup_select(idx)
                    {
                        self.accept_popup();
                    }
                    return Action::None;
                }
                MouseGesture::RightClick => {
                    self.popup = Popup::None;
                    return Action::None;
                }
                _ => {}
            }
        }
        if m.is_scroll() {
            self.selected = None;
            match m.gesture {
                MouseGesture::ScrollUp => self.scroll_up(3),
                _ => self.scroll_down(3),
            }
            return Action::None;
        }
        // A click in the transcript selects the block it landed on, and folds
        // it if there is anything behind the fold. The rect comes from the
        // registry, so it is the one that was drawn.
        if m.is_click() && m.in_rect(self.transcript_area) {
            if let Some(i) = zone.and_then(|z| z.row_index())
                && i < self.blocks.len()
            {
                self.selected = Some(i);
                self.follow = false;
                self.blocks[i].toggle();
                return Action::None;
            }
            return Action::None;
        }
        if m.is_click() {
            self.focused = true;
            self.selected = None;
        }
        Action::None
    }

    fn handle_picker_mouse(&mut self, m: MouseInput, sel: usize) -> Action {
        match m.gesture {
            MouseGesture::ScrollUp => {
                self.picker = Some(sel.saturating_sub(1));
                Action::None
            }
            MouseGesture::ScrollDown => {
                let max = self.sessions.len().saturating_sub(1);
                self.picker = Some((sel + 1).min(max));
                Action::None
            }
            MouseGesture::Click { double } => {
                if !m.in_rect(self.picker_area) {
                    self.picker = None;
                    return Action::None;
                }
                let idx = self.picker_start + (m.row - self.picker_area.y) as usize;
                if idx >= self.sessions.len() {
                    return Action::None;
                }
                let already = sel == idx;
                self.picker = Some(idx);
                if double || already {
                    let action = self
                        .sessions
                        .get(idx)
                        .map(|s| Action::RefineSwitchSession { id: s.id.clone() })
                        .unwrap_or(Action::None);
                    self.picker = None;
                    return action;
                }
                Action::None
            }
            MouseGesture::RightClick => {
                self.picker = None;
                Action::None
            }
        }
    }

    /// Point the open popup's selection at `idx`; false when out of range.
    fn popup_select(&mut self, idx: usize) -> bool {
        match &mut self.popup {
            Popup::Mention { items, sel } if idx < items.len() => {
                *sel = idx;
                true
            }
            Popup::Slash { items, sel } if idx < items.len() => {
                *sel = idx;
                true
            }
            _ => false,
        }
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
        let id = self.block_id();
        self.push_block(blocks::Block::user(id, text.clone()));
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
                    self.notice("usage: /grep <text>".to_string());
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
            "/fix-review-needed" | "/fix-needed-reviews" => {
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
                self.notice(format!(
                    "context {} / {} tokens ({pct}% full) · {} blocks · auto-compacts at 80%",
                    fmt_tokens(self.last_context),
                    fmt_tokens(self.context_max),
                    self.blocks.len()
                ));
                self.follow = true;
                Action::None
            }
            "/delete" => Action::RefineDeleteSession {
                id: self.active_session.clone(),
            },
            "/rename" => {
                if rest.is_empty() {
                    self.notice("usage: /rename <title>".to_string());
                    self.follow = true;
                    Action::None
                } else {
                    Action::RefineRenameSession {
                        title: rest.to_string(),
                    }
                }
            }
            _ => {
                self.notice(format!("unknown command: {cmd}"));
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
                self.notice(compaction_notice(
                    *dropped_messages,
                    *token_estimate,
                    *context_max,
                ));
                self.follow = true;
            }
            AppEvent::RefineDelta { delta } => self.push_delta(delta),
            AppEvent::RefinePlanUpdated { steps } => {
                self.plan = steps.clone();
                self.follow = true;
            }
            AppEvent::RefineSubagentPlanUpdated { id, steps } => {
                self.update_subagent_plan(id, steps);
                self.follow = true;
            }
            AppEvent::RefineApprovalRequest { id, summary, diff } => {
                self.pending.push_back(RefinePending::Approval {
                    id: *id,
                    summary: summary.clone(),
                    diff: diff.clone(),
                    scroll: 0,
                });
                self.follow = true;
            }
            AppEvent::RefineDecisionRequest { id, questions } => {
                self.pending
                    .push_back(RefinePending::Ask(AskSession::new(*id, questions.clone())));
                self.follow = true;
            }
            AppEvent::RefineToolInvoked {
                id,
                tool,
                summary,
                args,
            } => {
                self.settle_reasoning();
                let block_id = self.block_id();
                let mut block = blocks::Block::tool(
                    block_id,
                    tool.clone(),
                    id.clone(),
                    summary.clone(),
                );
                block.set_detail(blocks::tool_detail(args, None));
                self.push_block(block);
                self.follow = true;
            }
            AppEvent::RefineToolReturned { id, ok, detail } => {
                if let Some(b) = self
                    .blocks
                    .iter_mut()
                    .rev()
                    .find(|b| b.tool_call() == Some(id.as_str()))
                {
                    b.set_tool_status(if *ok { ToolStatus::Ok } else { ToolStatus::Failed });
                    let args = b.detail.clone();
                    let args = args.split_once("\n→ ").map_or(args.as_str(), |(a, _)| a);
                    b.set_detail(blocks::tool_detail(args, Some(detail)));
                }
            }
            AppEvent::RefineSubagentStarted {
                id,
                depth,
                title,
                role,
                model,
                background,
            } => {
                let run = self.subagent_mut(id);
                run.depth = *depth;
                run.title = title.clone();
                run.role = role.clone();
                run.model = model.clone();
                run.background = *background;
                run.status = RefineSubagentStatus::Running;
                run.activity = "starting".to_string();
                run.summary.clear();
                run.started = std::time::Instant::now();
                run.ran_for = None;
                // ...and a row in the conversation, because a delegated run is
                // something that happened in it, not beside it.
                if !self.blocks.iter().any(|b| b.subagent_id() == Some(id.as_str())) {
                    let block_id = self.block_id();
                    let head = format!("{title} · {role}");
                    self.push_block(blocks::Block::subagent(block_id, id.clone(), head));
                }
                self.follow = true;
            }
            AppEvent::RefineSubagentActivity { id, activity } => {
                // Only for a run we were told about: a stray activity line is
                // not enough to invent a row from.
                if let Some(run) = self.subagents.iter_mut().find(|r| r.id == *id)
                    && run.status == RefineSubagentStatus::Running
                {
                    run.activity = activity.clone();
                }
            }
            AppEvent::RefineSubagentTurn { id, text } => {
                if let Some(b) = self.blocks.iter_mut().find(|b| b.subagent_id() == Some(id.as_str()))
                {
                    let mut detail = b.detail.clone();
                    if !detail.is_empty() {
                        detail.push('\n');
                    }
                    detail.push_str(text.trim());
                    b.set_detail(blocks::cap_tail(&detail, CHILD_TRANSCRIPT_CAP));
                }
            }
            AppEvent::RefineSubagentFinished {
                id,
                status,
                summary,
            } => {
                if let Some(run) = self.subagents.iter_mut().find(|r| r.id == *id) {
                    run.status = *status;
                    run.summary = summary.clone();
                    run.activity.clear();
                    run.ran_for = Some(run.started.elapsed());
                }
                if let Some(b) = self.blocks.iter_mut().find(|b| b.subagent_id() == Some(id.as_str()))
                {
                    let mut detail = b.detail.clone();
                    if !detail.is_empty() {
                        detail.push('\n');
                    }
                    detail.push_str(summary.trim());
                    b.set_detail(blocks::cap_tail(&detail, CHILD_TRANSCRIPT_CAP));
                }
                self.follow = true;
            }
            AppEvent::RefineEditApplied { kind, summary } => {
                self.settle_reasoning();
                self.notice(format!("{kind}: {summary}"));
                self.follow = true;
            }
            AppEvent::RefineMessageDone => {
                if let Some(last) = self.blocks.last_mut() {
                    last.settle();
                }
                self.streaming = false;
                self.finish_turn();
            }
            AppEvent::RefineError { msg } => {
                self.settle_reasoning();
                self.notice(format!("error: {msg}"));
                self.streaming = false;
                self.follow = true;
                self.finish_turn();
            }
            _ => {}
        }
    }

    fn subagent_mut(&mut self, id: &str) -> &mut SubagentRun {
        if let Some(i) = self.subagents.iter().position(|run| run.id == id) {
            return &mut self.subagents[i];
        }
        self.subagents.push(SubagentRun::unannounced(id));
        self.subagents.last_mut().expect("just pushed")
    }

    fn update_subagent_plan(&mut self, id: &str, steps: &[PlanStep]) {
        let run = self.subagent_mut(id);
        run.plan = steps.to_vec();
    }

    fn push_delta(&mut self, delta: &str) {
        let keep_following = self.at_bottom();
        self.settle_reasoning();
        let open = matches!(
            self.blocks.last(),
            Some(b) if b.role() == TurnRole::Assistant && b.streaming
        );
        if !open {
            let id = self.block_id();
            self.push_block(blocks::Block::assistant(id, String::new()).streaming());
        }
        if let Some(last) = self.blocks.last_mut() {
            last.push_body(delta);
        }
        self.streaming = true;
        if keep_following {
            self.jump_bottom();
        }
    }

    fn push_reasoning(&mut self, delta: &str) {
        let keep_following = self.at_bottom();
        let open = matches!(
            self.blocks.last(),
            Some(b) if b.role() == TurnRole::Reasoning && b.streaming
        );
        if !open {
            let id = self.block_id();
            self.push_block(blocks::Block::reasoning(id, String::new()).streaming());
        }
        if let Some(last) = self.blocks.last_mut() {
            last.push_body(delta);
        }
        self.streaming = true;
        if keep_following {
            self.jump_bottom();
        }
    }

    /// Move the block cursor. The first press lands on the last block rather
    /// than stepping off it — arriving somewhere is the point.
    fn move_cursor(&mut self, delta: isize) {
        if self.blocks.is_empty() {
            return;
        }
        let last = self.blocks.len() - 1;
        let next = match self.selected {
            None => last,
            Some(cur) => (cur as isize + delta).clamp(0, last as isize) as usize,
        };
        self.selected = Some(next);
        self.follow = false;
        self.scroll_to_selected();
    }

    /// To the previous or next thing *you* said — the landmarks in a
    /// conversation are your own turns, not the agent's.
    fn jump_turn(&mut self, dir: isize) {
        let mine: Vec<usize> = self
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| b.role() == TurnRole::User)
            .map(|(i, _)| i)
            .collect();
        if mine.is_empty() {
            return;
        }
        let cur = self.selected.unwrap_or(self.blocks.len().saturating_sub(1));
        let next = if dir < 0 {
            mine.iter().rev().find(|i| **i < cur).copied()
        } else {
            mine.iter().find(|i| **i > cur).copied()
        };
        if let Some(next) = next {
            self.selected = Some(next);
            self.follow = false;
            self.scroll_to_selected();
        }
    }

    fn fold_selected(&mut self, open: bool) {
        if self.selected.is_none() {
            self.move_cursor(0);
        }
        if let Some(i) = self.selected
            && let Some(b) = self.blocks.get_mut(i)
        {
            b.set_open(open);
        }
        self.scroll_to_selected();
    }

    /// Open everything, or — when everything already is — close it.
    fn fold_all(&mut self) {
        let any_closed = self
            .blocks
            .iter()
            .any(|b| b.collapsible() && !b.open);
        for b in &mut self.blocks {
            b.set_open(any_closed);
        }
    }

    fn copy_selected(&self) -> Option<String> {
        let i = self.selected?;
        Some(self.blocks.get(i)?.to_markdown())
    }

    /// Bring the cursor's block into view, using the placement the last frame
    /// actually drew rather than a second guess at it.
    fn scroll_to_selected(&mut self) {
        let Some(i) = self.selected else { return };
        let Some(&(start, rows)) = self.placed.get(i) else {
            return;
        };
        let h = self.transcript_area.height.max(1);
        if start < self.scroll {
            self.scroll = start;
        } else if start.saturating_add(rows) > self.scroll.saturating_add(h) {
            self.scroll = start.saturating_add(rows).saturating_sub(h);
        }
        self.scroll = self.scroll.min(self.last_bottom);
        self.follow = self.scroll >= self.last_bottom;
    }

    fn at_bottom(&self) -> bool {
        self.follow || self.scroll >= self.last_bottom
    }

    fn jump_bottom(&mut self) {
        self.follow = true;
        self.scroll = self.last_bottom;
    }

    /// Reasoning folds itself away once the answer it was working towards
    /// starts arriving: watching it is useful live, and noise afterwards.
    fn settle_reasoning(&mut self) {
        if let Some(last) = self.blocks.last_mut()
            && last.role() == TurnRole::Reasoning
            && last.streaming
        {
            last.settle();
            last.set_open(false);
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

    /// This screen's commands, availability resolved for this frame.
    ///
    /// Every accelerator here is a chord: the input field owns the plain keys,
    /// so a single-letter binding would be swallowed the moment the transcript
    /// has focus. See [`super::action_table`].
    pub fn actions(&self, project: Option<&Project>) -> Vec<Act> {
        self.acts(project.is_some())
    }

    /// The table, from the one thing its availability turns on. `render` knows
    /// only whether a project is open, not which one.
    fn acts(&self, has_project: bool) -> Vec<Act> {
        use action_table::Accel;
        use crate::agents::refine::ApprovalMode;

        // While the session picker or an approval prompt is up, the keyboard
        // belongs to it. The controls grey out rather than vanishing.
        let live = has_project && self.picker.is_none() && self.pending.is_empty();
        let mode = match self.approval_mode {
            ApprovalMode::Auto => "auto",
            ApprovalMode::Ask => "ask",
            ApprovalMode::Always => "always",
        };
        vec![
            Act::toolbar(R_NEW, "new", Accel::ctrl('n')).when(live),
            Act::toolbar(R_SESSIONS, "sessions", Accel::ctrl('o'))
                .count(self.sessions.len() as u32)
                .when(live),
            Act::toolbar(R_APPROVAL, "approve", Accel::ctrl_code(KeyCode::Tab))
                .cycle()
                .value(mode)
                .when(live),
            Act::menu(R_COMPACT, "compact conversation", Accel::ctrl('y')).when(live),
            Act::menu(R_EXPORT, "export to markdown", Accel::ctrl('e')).when(live),
            Act::menu(R_UNDO, "undo last chapter edit", Accel::ctrl('u')).when(live),
            Act::menu(R_COPY, "copy selected block", Accel::ctrl('b'))
                .when(live && self.selected.is_some()),
            Act::menu(R_TASKS, "sub-agent tasks", Accel::ctrl('g'))
                .count(self.running_subagents() as u32)
                .when(has_project),
        ]
    }

    /// Run the action `id` stands for, however it was reached. `None` means
    /// no such action here.
    pub fn run(&mut self, id: u16, project: Option<&Project>) -> Option<Action> {
        let _ = project;
        self.run_action(id)
    }

    fn run_action(&mut self, id: u16) -> Option<Action> {
        Some(match id {
            R_NEW => Action::RefineNewSession,
            R_SESSIONS => Action::RefineOpenSessions,
            // Cycles always-approve → ask → auto.
            R_APPROVAL => Action::RefineCycleApprovalMode,
            R_COMPACT => Action::RefineCompact,
            R_EXPORT => Action::RefineExport,
            R_UNDO => Action::RefineUndo,
            R_COPY => Action::RefineCopyBlock {
                text: self.copy_selected()?,
            },
            R_TASKS => {
                self.toggle_tasks();
                return None;
            }
            _ => return None,
        })
    }

    pub fn hints(&self) -> &'static [(&'static str, &'static str)] {
        if self.child.is_some() {
            &[
                ("↑↓", "scroll"),
                ("⌃C", "stop it"),
                ("q", "close"),
            ]
        } else if self.tasks.is_some() {
            &[
                ("↑↓", "select"),
                ("↵", "open"),
                ("x", "stop"),
                ("h", "hide done"),
                ("esc", "close"),
            ]
        } else if self.picker.is_some() {
            &[
                ("↑↓", "select"),
                ("↵", "open"),
                ("n", "new"),
                ("d", "delete"),
                ("esc", "close"),
            ]
        } else if self.in_flight {
            &[("⌃C", "interrupt"), ("⌃End", "bottom"), ("↑↓", "scroll")]
        } else if self.focused {
            &[
                ("↵", "send"),
                ("@", "mention"),
                ("/", "cmd"),
                ("↑↓", "blocks"),
            ]
        } else {
            &[
                ("↑↓", "block"),
                ("←→", "fold"),
                ("⇧↑↓", "your turns"),
                ("⌃R", "fold all"),
                ("type", "write"),
            ]
        }
    }

    pub fn render(
        &mut self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        has_project: bool,
    ) {
        let theme: &Theme = ui.theme;
        let frame = ui.frame_count;
        if !has_project {
            let f: &mut Frame = ui.frame;
            self.render_no_project(f, area, theme);
            return;
        }
        let input_h = 3;
        let status = self.status_line(frame, theme);

        let card_h = self.ask_card_height(area);
        let mut constraints = vec![Constraint::Min(3)];
        if card_h > 0 {
            constraints.push(Constraint::Length(card_h));
        }
        if !self.plan.is_empty() {
            let plan_h = (self.plan.len() as u16 + 2).clamp(4, 10);
            constraints.push(Constraint::Length(plan_h));
        }
        if !self.subagents.is_empty() {
            let subagent_h = (self.subagents.len() as u16 + 2).clamp(3, 7);
            constraints.push(Constraint::Length(subagent_h));
        }
        // The status band is always there now: it is where this screen's
        // controls live, and a row that comes and go with the status line
        // would take them with it.
        constraints.push(Constraint::Length(1));
        constraints.push(Constraint::Length(input_h));

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);
        let band = rows[rows.len() - 2];

        let acts = self.acts(has_project);
        let toolbar = crate::ui::kit::toolbar::Toolbar::new(&acts).has_menu(true).render(
            ui,
            Rect {
                x: band.x + 1,
                width: band.width.saturating_sub(2),
                height: 1,
                ..band
            },
        );

        let mut next = 1;
        if card_h > 0 {
            self.render_ask_card(ui, rows[next]);
            next += 1;
        }

        // Disjoint field borrows: the frame to draw into, the registry to
        // record interactive rects in.
        let zones: &mut crate::ui::kit::Zones = ui.zones;
        let f: &mut Frame = ui.frame;

        self.render_transcript(f, zones, rows[0], frame, theme);
        let input_row = rows[rows.len() - 1];
        if !self.plan.is_empty() {
            self.render_plan(f, rows[next], theme);
            next += 1;
        }
        if !self.subagents.is_empty() {
            self.render_subagents(f, rows[next], frame, theme);
        }
        if let Some(status) = status {
            // Whatever the toolbar left of the band, right of it.
            let x = band.x + 1 + toolbar.cols.saturating_add(2);
            self.render_status(
                f,
                Rect {
                    x,
                    width: (band.x + band.width).saturating_sub(x),
                    ..band
                },
                status,
                theme,
            );
        }
        self.render_input(f, input_row, theme);
        if let Some(id) = self.child.clone() {
            self.render_child(f, area, &id, theme);
        } else if self.tasks.is_some() {
            self.render_tasks(f, zones, area, frame, theme);
        } else if self.picker.is_some() {
            self.render_session_picker(f, area, theme);
        } else if matches!(self.pending.front(), Some(RefinePending::Approval { .. })) {
            self.render_approval(f, area, theme);
        } else if self.pending.is_empty() {
            self.render_popup(f, zones, area, input_row.y, theme);
        }
    }

    /// The approval gate stays a modal: it is a yes/no over an edit that is
    /// already written, and a hard edge is the point. The question card is not,
    /// because the conversation is what the question is about.
    fn render_approval(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        let Some(RefinePending::Approval {
            summary,
            diff,
            scroll,
            ..
        }) = self.pending.front()
        else {
            return;
        };
        let mut lines: Vec<Line> = vec![
            Line::from(Span::styled(
                summary.clone(),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::raw(""),
        ];
        for l in diff.lines() {
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

        let w = area.width.min(72);
        let h = (lines.len() as u16 + 4)
            .min(area.height.saturating_sub(2))
            .max(7);
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
            .title(" Approve edit? ")
            .style(Style::default().bg(theme.bg_panel));
        let inner = block.inner(modal);
        f.render_widget(block, modal);

        let hint_row = Rect {
            y: inner.y + inner.height.saturating_sub(1),
            height: 1,
            ..inner
        };
        let body = Rect {
            height: inner.height.saturating_sub(1),
            ..inner
        };
        // The diff scrolls now rather than stopping at twenty lines, so what is
        // being approved can actually be read before approving it.
        let max = (lines.len() as u16).saturating_sub(body.height);
        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll(((*scroll as u16).min(max), 0)),
            body,
        );
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "↵ accept   ·   r / Esc reject   ·   ↑↓ scroll",
                Style::default().fg(theme.ink_faint),
            ))),
            hint_row,
        );
    }

    /// Rows the question card takes out of the chat column.
    ///
    /// It is laid out beside the transcript rather than over it: the card is
    /// where the conversation has got to, so covering the conversation with it
    /// would hide exactly the context the question is about.
    fn ask_card_height(&self, area: Rect) -> u16 {
        let Some(RefinePending::Ask(s)) = self.pending.front() else {
            return 0;
        };
        let width = area.width.saturating_sub(8).max(8);
        let prompt = crate::ui::kit::editor::wrap(&s.item().question, width).len() as u16;
        let options = (s.item().options.len() as u16).min(6);
        let want = 1 + prompt.clamp(1, 4) + options + 2;
        want.min(area.height.saturating_sub(8).max(5))
    }

    fn render_ask_card(&self, ui: &mut crate::ui::kit::Ui, area: Rect) {
        use crate::ui::kit::card::Card;
        use crate::ui::kit::list::{self, ListState, Row};
        use crate::ui::kit::{ZoneId, ZoneKind};

        let Some(RefinePending::Ask(s)) = self.pending.front() else {
            return;
        };
        if area.height == 0 {
            return;
        }
        let item = s.item();
        let answer = s.answer();
        let accent = ui.theme.accent;
        let ink = ui.theme.ink;
        let soft = ui.theme.ink_soft;
        let faint = ui.theme.ink_faint;

        let meta = if s.items.len() > 1 {
            format!("{}/{}  ← →", s.at + 1, s.items.len())
        } else {
            String::new()
        };
        let mut card = Card::new("The agent needs a decision").accent(accent);
        if !meta.is_empty() {
            card.meta = Some(meta);
        }
        let body = card.render(ui, area);
        if body.height == 0 {
            return;
        }

        let prompt_lines = crate::ui::kit::editor::wrap(&item.question, body.width);
        let prompt_h = (prompt_lines.len() as u16).min(body.height.saturating_sub(2)).max(1);
        for (n, range) in prompt_lines.iter().take(prompt_h as usize).enumerate() {
            ui.text(
                Rect {
                    y: body.y + n as u16,
                    height: 1,
                    ..body
                },
                item.question[range.clone()].to_string(),
                Style::default().fg(ink).add_modifier(Modifier::BOLD),
            );
        }

        let rest = Rect {
            y: body.y + prompt_h,
            height: body.height.saturating_sub(prompt_h),
            ..body
        };
        if rest.height < 2 {
            return;
        }
        // The hint owns the last row, the box the one above it, and whatever is
        // left goes to the options — which window themselves.
        let hint_row = Rect {
            y: rest.y + rest.height - 1,
            height: 1,
            ..rest
        };
        let box_row = Rect {
            y: hint_row.y.saturating_sub(1),
            height: 1,
            ..rest
        };
        let opts_area = Rect {
            height: rest.height.saturating_sub(2),
            ..rest
        };

        if !item.options.is_empty() && opts_area.height > 0 {
            let mut state = ListState::new();
            if s.row < item.options.len() {
                state.select(Some(s.row));
            }
            let picked = answer.picked.clone();
            let options = item.options.clone();
            let multiple = item.multiple;
            list::render(
                ui,
                opts_area,
                &mut state,
                options.len(),
                list::Opts::default(),
                |i| {
                    let mark = if picked.contains(&i) {
                        if multiple { "[x] " } else { "◉ " }
                    } else if multiple {
                        "[ ] "
                    } else {
                        "○ "
                    };
                    Row::new(Line::from(vec![
                        Span::styled(mark.to_string(), Style::default().fg(accent)),
                        Span::styled(options[i].clone(), Style::default().fg(ink)),
                    ]))
                },
            );
        }

        let on_box = s.row >= item.options.len();
        let st = ui.interactive(box_row, ZoneId::new(ZoneKind::TextSurface, 0), on_box);
        let bg = ui.surface_of(st);
        ui.fill(box_row, Style::default().bg(bg));
        let width = box_row.width.saturating_sub(2) as usize;
        let mut spans = vec![Span::styled(
            "› ".to_string(),
            Style::default().fg(if on_box { accent } else { faint }).bg(bg),
        )];
        if answer.text.is_empty() && !on_box {
            spans.push(Span::styled(
                truncate_cols("or type your own answer", width),
                Style::default().fg(faint).bg(bg),
            ));
        } else {
            let (before, after) = input::caret_halves(&answer.text, answer.cursor, width);
            spans.push(Span::styled(before, Style::default().fg(ink).bg(bg)));
            if on_box {
                spans.push(Span::styled(
                    "▏".to_string(),
                    Style::default().fg(ui.theme.stream_cursor).bg(bg),
                ));
            }
            spans.push(Span::styled(after, Style::default().fg(ink).bg(bg)));
        }
        ui.line(box_row, Line::from(spans), Style::default().bg(bg));

        let hint = if s.confirm_dismiss {
            "Esc again to dismiss without answering".to_string()
        } else {
            let enter = if item.multiple {
                "↵ toggle"
            } else if s.is_last() {
                "↵ submit"
            } else {
                "↵ next"
            };
            let nav = if s.items.len() > 1 { " · ←→ question" } else { "" };
            format!("↑↓ choose · type to answer{nav} · {enter} · ^s send · Esc dismiss")
        };
        ui.text(
            hint_row,
            truncate_cols(&hint, hint_row.width as usize),
            Style::default().fg(soft),
        );
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
                let detail = if run.status == RefineSubagentStatus::Running {
                    let activity = run.activity.trim();
                    let mut parts = vec![status.to_string()];
                    if run.background {
                        parts.push("bg".to_string());
                    }
                    if !activity.is_empty() {
                        parts.push(activity.to_string());
                    }
                    if !run.plan.is_empty() {
                        let done = run
                            .plan
                            .iter()
                            .filter(|step| step.status == PlanStepStatus::Completed)
                            .count();
                        let current = run
                            .plan
                            .iter()
                            .find(|step| step.status == PlanStepStatus::InProgress)
                            .or_else(|| {
                                run.plan
                                    .iter()
                                    .find(|step| step.status == PlanStepStatus::Pending)
                            })
                            .map(|step| step.step.trim())
                            .filter(|step| !step.is_empty());
                        let progress = match current {
                            Some(step) => format!("plan {done}/{} · {step}", run.plan.len()),
                            None => format!("plan {done}/{}", run.plan.len()),
                        };
                        parts.push(progress);
                    }
                    parts.join(" · ")
                } else if run.summary.trim().is_empty() {
                    status.to_string()
                } else {
                    format!("{status} · {}", run.summary.trim())
                };
                let indent = "  ".repeat(run.depth.min(4));
                // Role and model belong on the row: a reader deciding whether
                // to cancel a run wants to know what it is allowed to do and
                // what it is costing.
                let mut tags = Vec::new();
                if !run.role.is_empty() {
                    tags.push(run.role.clone());
                }
                if !run.model.is_empty() {
                    tags.push(short_model(&run.model));
                }
                tags.push(fmt_elapsed(run.elapsed()));
                Line::from(Span::styled(
                    truncate_cols(
                        &format!(
                            "{indent}{mark} {} [{}] — {detail}",
                            run.title.trim(),
                            tags.join(" · ")
                        ),
                        w,
                    ),
                    style,
                ))
            })
            .collect();
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme.bg_panel)),
            inner,
        );
    }

    /// Every run this session started, with what it is allowed to do, what it
    /// costs and what it is doing — the things you need before deciding to
    /// stop one.
    fn render_tasks(
        &mut self,
        f: &mut Frame,
        zones: &mut crate::ui::kit::Zones,
        area: Rect,
        frame: u64,
        theme: &Theme,
    ) {
        let Some(sel) = self.tasks else { return };
        let rows = self.task_rows();
        let running = self.running_subagents();
        let title = if self.tasks_hide_done {
            format!(" ◇ tasks · {running} running · hiding done ")
        } else {
            format!(" ◇ tasks · {running} running ")
        };
        let panel = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(theme.accent))
            .title(Span::styled(title, Style::default().fg(theme.ink_soft)))
            .style(Style::default().bg(theme.bg_inset));
        let w = area.width.min(78);
        let h = area.height.min((rows.len() as u16).saturating_add(4).max(6));
        let modal = Rect {
            x: area.x + (area.width.saturating_sub(w)) / 2,
            y: area.y + (area.height.saturating_sub(h)) / 2,
            width: w,
            height: h,
        };
        let inner = panel.inner(modal);
        f.render_widget(Clear, modal);
        f.render_widget(panel, modal);
        self.tasks_area = inner;
        if inner.width == 0 || inner.height < 2 {
            return;
        }

        let list_h = inner.height - 1;
        self.tasks_start = popup_window_start(rows.len(), sel, list_h as usize);
        let cols = inner.width as usize;
        for (n, idx) in rows
            .iter()
            .skip(self.tasks_start)
            .take(list_h as usize)
            .enumerate()
        {
            let run = &self.subagents[*idx];
            let (mark, style) = match run.status {
                RefineSubagentStatus::Running => (
                    theme::refine_spinner_frame(frame),
                    Style::default().fg(theme.accent),
                ),
                RefineSubagentStatus::Succeeded => ("✓", Style::default().fg(theme.status_done)),
                RefineSubagentStatus::Failed => ("!", Style::default().fg(theme.status_failed)),
                RefineSubagentStatus::Canceled => ("×", Style::default().fg(theme.ink_faint)),
            };
            let chosen = self.tasks_start + n == sel;
            let style = if chosen {
                style.add_modifier(Modifier::BOLD).bg(theme.bg_panel)
            } else {
                style
            };
            let detail = if run.activity.trim().is_empty() {
                run.summary.trim()
            } else {
                run.activity.trim()
            };
            let line = format!(
                "{} {mark} {} · {} · {} · {}  {}",
                if chosen { "▌" } else { " " },
                truncate_cols(run.title.trim(), 26),
                if run.role.is_empty() { "—" } else { &run.role },
                short_model(&run.model),
                fmt_elapsed(run.elapsed()),
                truncate_cols(detail, 24),
            );
            let rect = Rect {
                y: inner.y + n as u16,
                height: 1,
                ..inner
            };
            zones.push(
                rect,
                crate::ui::kit::ZoneId::new(
                    crate::ui::kit::ZoneKind::Row,
                    (self.tasks_start + n) as u32,
                ),
            );
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(truncate_cols(&line, cols), style))),
                rect,
            );
        }

        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_cols(
                    "↑↓ select · ↵ open · x stop · h hide done · esc close",
                    cols,
                ),
                Style::default().fg(theme.ink_faint),
            ))),
            Rect {
                y: inner.y + inner.height - 1,
                height: 1,
                ..inner
            },
        );
    }

    /// One sub-agent's own conversation, full frame and read-only. The way to
    /// talk to a child is the agent's `message_subagent`; a panel that looked
    /// like a chat but could not send would be a worse lie than no panel.
    fn render_child(&mut self, f: &mut Frame, area: Rect, id: &str, theme: &Theme) {
        let run = self.subagents.iter().find(|r| r.id == id);
        let (title, status, elapsed) = match run {
            Some(r) => (
                r.title.clone(),
                match r.status {
                    RefineSubagentStatus::Running => "running",
                    RefineSubagentStatus::Succeeded => "done",
                    RefineSubagentStatus::Failed => "failed",
                    RefineSubagentStatus::Canceled => "cancelled",
                },
                fmt_elapsed(r.elapsed()),
            ),
            None => (id.to_string(), "gone", String::new()),
        };
        let tags = run
            .map(|r| format!("{} · {}", r.role, short_model(&r.model)))
            .unwrap_or_default();
        let panel = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(theme.accent))
            .title(Span::styled(
                format!(" ⟩ {} · {tags} · {status} {elapsed} ", truncate_cols(&title, 40)),
                Style::default().fg(theme.ink_soft),
            ))
            .style(Style::default().bg(theme.bg_panel));
        let inner = panel.inner(area);
        f.render_widget(Clear, area);
        f.render_widget(panel, area);
        if inner.width == 0 || inner.height < 2 {
            return;
        }

        let body = Rect {
            height: inner.height - 1,
            ..inner
        };
        let text = self.child_transcript(id);
        let lines: Vec<Line> = if text.trim().is_empty() {
            vec![Line::from(Span::styled(
                "nothing reported yet",
                Style::default().fg(theme.ink_faint),
            ))]
        } else {
            text.lines()
                .map(|l| {
                    Line::from(Span::styled(
                        l.to_string(),
                        Style::default().fg(theme.translated_text),
                    ))
                })
                .collect()
        };
        let total = wrapped_line_count(&lines, body.width as usize);
        let max = total.saturating_sub(body.height);
        self.child_scroll = self.child_scroll.min(max);
        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((self.child_scroll, 0))
                .style(Style::default().bg(theme.bg_panel)),
            body,
        );
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "↑↓ scroll · ⌃C stop this sub-agent · q close",
                Style::default().fg(theme.ink_faint),
            ))),
            Rect {
                y: inner.y + inner.height - 1,
                height: 1,
                ..inner
            },
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

    fn render_session_picker(&mut self, f: &mut Frame, area: Rect, theme: &Theme) {
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
        self.picker_area = inner;

        if self.sessions.is_empty() {
            self.picker_start = 0;
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
        self.picker_start = start;
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

    /// Re-render only the blocks whose state actually changed.
    ///
    /// The transcript used to be hashed and rendered whole, so every streamed
    /// delta re-rendered every message that had ever been said. Per block, a
    /// delta rebuilds the tail and nothing else.
    fn rebuild_renders(&mut self, width: u16, theme: &Theme) {
        if self.renders.len() != self.blocks.len() {
            self.renders
                .resize_with(self.blocks.len(), || BlockRender {
                    key: None,
                    lines: Vec::new(),
                    rows: 0,
                });
        }
        for (i, b) in self.blocks.iter().enumerate() {
            let key = blocks::cache_key(b, width, theme);
            if self.renders[i].key == Some(key) {
                continue;
            }
            let lines = blocks::render(b, width, theme);
            let rows = wrapped_line_count(&lines, width as usize);
            self.renders[i] = BlockRender {
                key: Some(key),
                lines,
                rows,
            };
        }
    }

    fn render_transcript(
        &mut self,
        f: &mut Frame,
        zones: &mut crate::ui::kit::Zones,
        area: Rect,
        frame: u64,
        theme: &Theme,
    ) {
        let panel = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(theme.rule))
            .title(Span::styled(
                " 推 Refine — chat ",
                Style::default().fg(theme.ink_soft),
            ))
            .style(Style::default().bg(theme.bg_panel));
        let inner = panel.inner(area);
        f.render_widget(panel, area);
        self.transcript_area = inner;
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        if self.blocks.is_empty() {
            self.placed.clear();
            self.last_bottom = 0;
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(Span::styled(
                        "Ask the refine agent to fix or improve anything in this project.",
                        Style::default().fg(theme.ink_faint),
                    )),
                    Line::raw(""),
                    Line::from(Span::styled(
                        "  @v1/c3 tighten the prose · @glossary rename a term · /help",
                        Style::default().fg(theme.ink_faint),
                    )),
                ])
                .style(Style::default().bg(theme.bg_panel)),
                inner,
            );
            return;
        }

        // A two-column gutter carries the selection rail. It is painted per
        // frame rather than rendered into a block, so moving the cursor costs
        // no re-render.
        let rail = inner.width.min(2);
        let body = Rect {
            x: inner.x + rail,
            width: inner.width - rail,
            ..inner
        };
        self.rebuild_renders(body.width, theme);

        let mut lines: Vec<Line<'static>> = Vec::new();
        self.placed.clear();
        let mut row = 0u16;
        for r in &self.renders {
            self.placed.push((row, r.rows));
            lines.extend(r.lines.iter().cloned());
            row = row.saturating_add(r.rows);
        }
        let total = row;

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
        self.last_bottom = total.saturating_sub(body.height);
        let scroll = if self.follow || was_at_bottom {
            self.jump_bottom();
            self.last_bottom
        } else {
            self.scroll = self.scroll.min(self.last_bottom);
            self.scroll
        };

        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((scroll, 0))
                .style(Style::default().bg(theme.bg_panel)),
            body,
        );
        crate::ui::widgets::render_panel_scrollbar(f, area, total as usize, scroll as usize, theme);

        // One pass registers what can be clicked and paints what is selected,
        // from the same placement the draw above used.
        for (i, &(start, rows)) in self.placed.iter().enumerate() {
            let Some(vis) = visible_rows(start, rows, scroll, body.height) else {
                continue;
            };
            let hit = Rect {
                x: inner.x,
                y: inner.y + vis.0,
                width: inner.width,
                height: vis.1,
            };
            zones.push(
                hit,
                crate::ui::kit::ZoneId::new(crate::ui::kit::ZoneKind::Row, i as u32),
            );
            if self.selected == Some(i) {
                let bar = Rect { width: rail, ..hit };
                f.render_widget(
                    Paragraph::new(vec![
                        Line::from(Span::styled(
                            "\u{258c}",
                            Style::default().fg(theme.accent),
                        ));
                        vis.1 as usize
                    ]),
                    bar,
                );
            }
        }
    }

    /// What the transcript is showing right now, flattened. Reading the drawn
    /// lines rather than a parallel string is the point: a test that passes
    /// against a second renderer proves nothing about the first.
    #[cfg(test)]
    fn visible_text(&mut self, width: u16) -> String {
        let theme = crate::model::ThemeId::default().build();
        self.rebuild_renders(width, &theme);
        self.renders
            .iter()
            .flat_map(|r| &r.lines)
            .map(|l| {
                l.spans
                    .iter()
                    .map(|sp| sp.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn toggle_tasks(&mut self) {
        self.tasks = match self.tasks {
            Some(_) => None,
            None => Some(0),
        };
        self.tasks_start = 0;
    }

    /// The rows the pane is showing, newest first, as indices into
    /// `subagents` — so a selection survives the hide-completed toggle
    /// meaning something different.
    fn task_rows(&self) -> Vec<usize> {
        self.subagents
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, r)| {
                !self.tasks_hide_done || r.status == RefineSubagentStatus::Running
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn handle_tasks_key(&mut self, key: KeyEvent, sel: usize) -> Action {
        let rows = self.task_rows();
        let last = rows.len().saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.tasks = Some(sel.saturating_sub(1));
                Action::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.tasks = Some((sel + 1).min(last));
                Action::None
            }
            KeyCode::Char('h') => {
                self.tasks_hide_done = !self.tasks_hide_done;
                self.tasks = Some(0);
                Action::None
            }
            KeyCode::Enter => {
                if let Some(run) = rows.get(sel).and_then(|i| self.subagents.get(*i)) {
                    self.child = Some(run.id.clone());
                    self.child_scroll = 0;
                    self.tasks = None;
                }
                Action::None
            }
            KeyCode::Char('x') | KeyCode::Delete => rows
                .get(sel)
                .and_then(|i| self.subagents.get(*i))
                .filter(|r| r.status == RefineSubagentStatus::Running)
                .map(|r| Action::RefineCancelSubagent { id: r.id.clone() })
                .unwrap_or(Action::None),
            KeyCode::Esc | KeyCode::Char('q') => {
                self.tasks = None;
                Action::None
            }
            _ => Action::None,
        }
    }

    fn handle_child_key(&mut self, key: KeyEvent) -> Action {
        let id = self.child.clone().unwrap_or_default();
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Action::RefineCancelSubagent { id }
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.child = None;
                Action::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.child_scroll = self.child_scroll.saturating_sub(1);
                Action::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.child_scroll = self.child_scroll.saturating_add(1);
                Action::None
            }
            KeyCode::PageUp => {
                self.child_scroll = self.child_scroll.saturating_sub(10);
                Action::None
            }
            KeyCode::PageDown => {
                self.child_scroll = self.child_scroll.saturating_add(10);
                Action::None
            }
            _ => Action::None,
        }
    }

    /// The child's own conversation, as it has reached us so far.
    fn child_transcript(&self, id: &str) -> String {
        self.blocks
            .iter()
            .find(|b| b.subagent_id() == Some(id))
            .map(|b| b.detail.clone())
            .unwrap_or_default()
    }

    /// A read-only view of every run this session started, newest first with
    /// the running ones ahead of the rest — the order the tasks pane wants and
    /// the same one the TUI's own pane uses.
    pub fn subagent_views(&self) -> Vec<SubagentView<'_>> {
        let mut out: Vec<_> = self.subagents.iter().enumerate().collect();
        out.sort_by_key(|(i, r)| (r.status != RefineSubagentStatus::Running, std::cmp::Reverse(*i)));
        out.into_iter()
            .map(|(_, r)| SubagentView {
                id: &r.id,
                title: &r.title,
                role: &r.role,
                model: &r.model,
                background: r.background,
                status: r.status,
                activity: &r.activity,
                summary: &r.summary,
                elapsed: r.elapsed(),
                plan: &r.plan,
            })
            .collect()
    }

    /// Whether anything is still moving on this screen — a spinner to animate.
    pub fn has_running_subagents(&self) -> bool {
        self.running_subagents() > 0
    }

    pub fn running_subagent_count(&self) -> usize {
        self.running_subagents()
    }

    fn running_subagents(&self) -> usize {
        self.subagents
            .iter()
            .filter(|r| r.status == RefineSubagentStatus::Running)
            .count()
    }

    /// Working line while a turn runs; idle usage summary otherwise.
    fn status_line(&self, frame: u64, theme: &Theme) -> Option<Line<'static>> {
        let faint = Style::default().fg(theme.ink_faint);
        let soft = Style::default().fg(theme.ink_soft);
        // A background sub-agent outlives the turn that spawned it, so between
        // turns this band is the only thing that says work is still happening.
        let running = self.running_subagents();
        let still = |spans: &mut Vec<Span<'static>>| {
            if running > 0 {
                spans.push(Span::styled(
                    format!(
                        " · ◎ {running} sub-agent{} running",
                        if running == 1 { "" } else { "s" }
                    ),
                    Style::default().fg(theme.accent),
                ));
            }
        };
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
            still(&mut spans);
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
        let mut spans = vec![Span::styled(text, faint)];
        still(&mut spans);
        Some(Line::from(spans))
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

    fn render_popup(
        &mut self,
        f: &mut Frame,
        zones: &mut crate::ui::kit::Zones,
        body: Rect,
        input_top: u16,
        theme: &Theme,
    ) {
        use crate::ui::kit::{ZoneId, ZoneKind};

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
        self.popup_area = inner;
        self.popup_offset = offset;

        // Each visible entry registers its own index, so a click resolves from
        // the registry rather than from the popup origin plus the scroll
        // offset — the arithmetic that has to be kept in step by hand.
        let label_w = inner.width as usize;
        for (n, (label, selected)) in rows.iter().skip(offset).take(max_rows).enumerate() {
            let rect = Rect {
                x: inner.x,
                y: inner.y + n as u16,
                width: inner.width,
                height: 1,
            };
            if rect.y >= inner.y + inner.height {
                break;
            }
            zones.push(rect, ZoneId::new(ZoneKind::Row, (offset + n) as u32));
            let style = if *selected {
                Style::default()
                    .fg(theme.accent)
                    .bg(theme.bg_inset)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.ink_soft).bg(theme.bg_inset)
            };
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    truncate_cols(label, label_w),
                    style,
                )))
                .style(Style::default().bg(theme.bg_inset)),
                rect,
            );
        }
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

/// Where a block that starts at `start` and is `rows` tall lands inside a
/// viewport scrolled to `scroll`: `(top row, height)`, or `None` when none of
/// it is on screen.
fn visible_rows(start: u16, rows: u16, scroll: u16, height: u16) -> Option<(u16, u16)> {
    if rows == 0 || height == 0 {
        return None;
    }
    let end = start.saturating_add(rows);
    if end <= scroll || start >= scroll.saturating_add(height) {
        return None;
    }
    let top = start.max(scroll) - scroll;
    let bottom = end.min(scroll.saturating_add(height)) - scroll;
    Some((top, bottom - top))
}

fn wrapped_line_count(lines: &[Line<'_>], width: usize) -> u16 {
    crate::ui::markdown::wrapped_rows(lines, width).min(u16::MAX as usize) as u16
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
/// `anthropic/claude-sonnet-5` reads as `claude-sonnet-5` on a crowded row;
/// the vendor prefix is the part nobody is choosing between.
fn short_model(model: &str) -> String {
    model.rsplit('/').next().unwrap_or(model).to_string()
}

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
    fn fix_review_needed_slash_submits_triage_prompt_with_scope() {
        for cmd in ["/fix-review-needed", "/fix-needed-reviews"] {
            let mut s = RefineScreen::new();

            let action = s.run_slash(&format!("{cmd} @v5/c3"));

            match action {
                Action::RefineSubmit { text } => {
                    assert!(text.contains("Investigate and fix `honya:review-needed` chunks"));
                    assert!(text.contains("dialogue/POV/pronoun/register"));
                    assert!(text.contains("`自分` inside dialogue"));
                    assert!(text.contains("Treat the reviewer reason as a clue"));
                    assert!(text.contains("Scope hint: @v5/c3"));
                }
                other => panic!("expected RefineSubmit for {cmd}, got {other:?}"),
            }
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
    fn display_blocks_reconstructs_from_messages() {
        use crate::llm::{FunctionCall, Message, Role, ToolCall};
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
        let out = display_blocks(&messages, COMPACT_SUMMARY_PREFIX);
        assert_eq!(out.len(), 4);
        assert_eq!(out[0].role(), TurnRole::User);
        assert_eq!(out[0].body, "fix the prose"); // scope hint stripped
        assert_eq!(out[1].role(), TurnRole::User);
        assert_eq!(out[1].body, "also update tone"); // steering metadata stripped
        assert_eq!(out[2].role(), TurnRole::Tool);
        assert_eq!(out[2].tool_name(), Some("edit_chapter"));
        assert_eq!(out[3].role(), TurnRole::Assistant);

        // Ids are unique, or a cached render could be handed to another block.
        let mut ids: Vec<_> = out.iter().map(|b| b.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), out.len());
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
    fn display_blocks_shows_compacted_history_as_a_notice() {
        let messages = vec![crate::llm::Message::user(format!(
            "{COMPACT_SUMMARY_PREFIX}\nUser: earlier request"
        ))];
        let turns = display_blocks(&messages, COMPACT_SUMMARY_PREFIX);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].role(), TurnRole::Tool);
        assert!(turns[0].body.contains("context summarized"));
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

    /// The mouse drives the session picker: the wheel walks the list, a click
    /// selects, a second click on the selection opens it, and a right-click (or a
    /// click off the modal) closes the picker.
    #[test]
    fn picker_mouse_selects_opens_and_dismisses() {
        use crate::ui::mouse::{MouseGesture, MouseInput};

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
        let mut s = RefineScreen::new();
        s.open_picker(sessions.clone(), "a".to_string());
        // Geometry normally set on render.
        s.picker_area = Rect {
            x: 10,
            y: 5,
            width: 40,
            height: 4,
        };
        s.picker_start = 0;

        let click = |col, row| MouseInput {
            gesture: MouseGesture::Click { double: false },
            col,
            row,
        };

        // Wheel walks the selection.
        s.handle_mouse(
            MouseInput {
                gesture: MouseGesture::ScrollDown,
                col: 0,
                row: 0,
            },
            None,
        );
        assert_eq!(s.picker, Some(1));

        // Click row 0 selects it; a second click opens that session.
        assert!(matches!(s.handle_mouse(click(12, 5), None), Action::None));
        assert_eq!(s.picker, Some(0));
        match s.handle_mouse(click(12, 5), None) {
            Action::RefineSwitchSession { id } => assert_eq!(id, "a"),
            other => panic!("expected switch to a, got {other:?}"),
        }
        assert!(!s.picker_open());

        // Right-click closes without switching.
        s.open_picker(sessions, "a".to_string());
        s.handle_mouse(
            MouseInput {
                gesture: MouseGesture::RightClick,
                col: 0,
                row: 0,
            },
            None,
        );
        assert!(!s.picker_open());
    }

    #[test]
    fn reasoning_streams_then_folds_itself_when_the_answer_starts() {
        let mut s = RefineScreen::new();
        s.on_app_event(&AppEvent::RefineReasoning {
            delta: "weighing the options\nthen deciding".to_string(),
        });
        assert_eq!(s.blocks.len(), 1);
        assert_eq!(s.blocks[0].role(), TurnRole::Reasoning);
        assert!(s.blocks[0].streaming, "reasoning streams live");
        assert!(s.blocks[0].open, "and is open while it does");
        assert!(s.visible_text(80).contains("weighing the options"));

        s.on_app_event(&AppEvent::RefineDelta {
            delta: "Here is the fix.".to_string(),
        });
        assert!(
            !s.blocks[0].streaming,
            "reasoning stops when the answer starts"
        );
        assert!(!s.blocks[0].open, "and folds itself away");
        assert_eq!(s.blocks[1].role(), TurnRole::Assistant);

        let shown = s.visible_text(80);
        assert!(shown.contains("thinking —"), "{shown}");
        assert!(!shown.contains("weighing the options"), "{shown}");

        // ...and only that block reopens, which is the whole point.
        s.selected = Some(0);
        s.fold_selected(true);
        let shown = s.visible_text(80);
        assert!(shown.contains("weighing the options"), "{shown}");
    }

    #[test]
    fn only_the_selected_block_folds() {
        let mut s = RefineScreen::new();
        for n in 0..3 {
            s.on_app_event(&tool_invoked(&format!("c{n}"), "read_chapter", "ch1"));
        }
        assert!(s.blocks.iter().all(|b| !b.open));

        s.selected = Some(1);
        s.fold_selected(true);
        assert!(!s.blocks[0].open);
        assert!(s.blocks[1].open, "the selected one opened");
        assert!(!s.blocks[2].open, "and only that one");

        // ⌃R is the deliberate all-at-once, and it is a separate gesture.
        s.fold_all();
        assert!(s.blocks.iter().all(|b| b.open));
        s.fold_all();
        assert!(s.blocks.iter().all(|b| !b.open));
    }

    /// Shaped like the real event: the collapsed summary the agent sends is
    /// the flattened arguments, which is what a reload derives it from too.
    fn tool_invoked(id: &str, tool: &str, summary: &str) -> AppEvent {
        let args = format!("{{\"note\":\"{summary}\"}}");
        AppEvent::RefineToolInvoked {
            id: id.to_string(),
            tool: tool.to_string(),
            summary: args.clone(),
            args,
        }
    }

    #[test]
    fn expanded_reasoning_renders_bold_without_comment_dividers() {
        let mut s = RefineScreen::new();
        s.on_app_event(&AppEvent::RefineReasoning {
            delta: "**Inferring character names and traits** <!-- -->**Confirming character presence and POV** <!-- -->".to_string(),
        });

        let theme = crate::model::ThemeId::default().build();
        s.rebuild_renders(100, &theme);
        let lines: Vec<_> = s.renders.iter().flat_map(|r| r.lines.clone()).collect();
        let visible = lines
            .iter()
            .flat_map(|line| &line.spans)
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(!visible.contains("<!--"));
        for expected in [
            "Inferring character names and traits",
            "Confirming character presence and POV",
        ] {
            let span = lines
                .iter()
                .flat_map(|line| &line.spans)
                .find(|span| span.content == expected)
                .expect("bold reasoning span");
            assert!(span.style.add_modifier.contains(Modifier::BOLD));
        }
    }

    #[test]
    fn a_tool_call_shows_its_name_always_and_its_detail_on_request() {
        let mut s = RefineScreen::new();
        for (n, (tool, summary)) in [
            ("read_chapter", "ch1"),
            ("grep_chapter", "ดาบ"),
            ("edit_chapter", "ch1: 1 edit(s)"),
        ]
        .into_iter()
        .enumerate()
        {
            s.on_app_event(&tool_invoked(&format!("c{n}"), tool, summary));
        }
        // What ran is never hidden — it used to coalesce into "3 actions".
        let shown = s.visible_text(80);
        for tool in ["read_chapter", "grep_chapter", "edit_chapter"] {
            assert!(shown.contains(tool), "{tool} missing from: {shown}");
        }
        assert!(!shown.contains("→"), "the result is hidden when folded: {shown}");

        s.on_app_event(&AppEvent::RefineToolReturned {
            id: "c1".to_string(),
            ok: true,
            detail: "9 matches in v1/c3".to_string(),
        });
        s.selected = Some(1);
        s.fold_selected(true);
        let shown = s.visible_text(80);
        assert!(shown.contains("9 matches"), "{shown}");
    }

    #[test]
    fn a_failed_tool_call_is_marked_without_being_opened() {
        let mut s = RefineScreen::new();
        s.on_app_event(&tool_invoked("c1", "edit_chapter", "ch1"));
        s.on_app_event(&AppEvent::RefineToolReturned {
            id: "c1".to_string(),
            ok: false,
            detail: "old text not found".to_string(),
        });
        assert!(!s.blocks[0].open);
        assert!(matches!(
            s.blocks[0].kind,
            blocks::BlockKind::Tool {
                status: ToolStatus::Failed,
                ..
            }
        ));
    }

    #[test]
    fn every_visible_block_registers_a_zone_and_a_click_lands_on_it() {
        let mut s = RefineScreen::new();
        s.on_app_event(&AppEvent::RefineDelta {
            delta: "here is what I found".to_string(),
        });
        s.on_app_event(&AppEvent::RefineMessageDone);
        for n in 0..3 {
            s.on_app_event(&tool_invoked(&format!("c{n}"), "read_chapter", "ch1"));
        }

        let (_, zones) =
            crate::ui::kit::ctx::draw_test(100, 40, |ui, area| s.render(ui, area, true));

        for i in 0..s.blocks.len() {
            let id = crate::ui::kit::ZoneId::new(crate::ui::kit::ZoneKind::Row, i as u32);
            let rect = zones
                .rect_of(id)
                .unwrap_or_else(|| panic!("block {i} drew but registered no zone"));
            let (col, row) = (rect.x + 1, rect.y);
            s.handle_mouse(
                MouseInput {
                    gesture: MouseGesture::Click { double: false },
                    col,
                    row,
                },
                zones.at(col, row),
            );
            assert_eq!(s.selected, Some(i), "clicking block {i} selected something else");
        }
    }

    #[test]
    fn a_click_on_a_folded_block_also_opens_it() {
        let mut s = RefineScreen::new();
        s.on_app_event(&tool_invoked("c0", "grep_chapter", "先輩"));
        let (_, zones) =
            crate::ui::kit::ctx::draw_test(100, 20, |ui, area| s.render(ui, area, true));
        let rect = zones
            .rect_of(crate::ui::kit::ZoneId::new(crate::ui::kit::ZoneKind::Row, 0))
            .expect("the tool block registered a zone");
        let (col, row) = (rect.x + 1, rect.y);
        s.handle_mouse(
            MouseInput {
                gesture: MouseGesture::Click { double: false },
                col,
                row,
            },
            zones.at(col, row),
        );
        assert!(s.blocks[0].open);
    }

    #[test]
    fn shift_arrow_jumps_between_your_own_turns() {
        let mut s = RefineScreen::new();
        for n in 0..3 {
            let id = s.block_id();
            s.push_block(blocks::Block::user(id, format!("ask {n}")));
            s.on_app_event(&tool_invoked(&format!("c{n}"), "read_chapter", "ch1"));
            s.on_app_event(&AppEvent::RefineDelta {
                delta: format!("answer {n}"),
            });
            s.on_app_event(&AppEvent::RefineMessageDone);
        }
        let mine: Vec<usize> = s
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| b.role() == TurnRole::User)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(mine.len(), 3);

        // From the bottom, back up through the three things you said.
        s.selected = None;
        s.handle_key(shift(KeyCode::Up), None);
        assert_eq!(s.selected, Some(mine[2]));
        s.handle_key(shift(KeyCode::Up), None);
        assert_eq!(s.selected, Some(mine[1]));
        s.handle_key(shift(KeyCode::Down), None);
        assert_eq!(s.selected, Some(mine[2]));
        // ...and it stops rather than wrapping round.
        s.handle_key(shift(KeyCode::Down), None);
        assert_eq!(s.selected, Some(mine[2]));
    }

    fn shift(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    #[test]
    fn a_streamed_delta_rerenders_only_the_tail_block() {
        let theme = crate::model::ThemeId::default().build();
        let mut s = RefineScreen::new();
        for n in 0..4 {
            s.on_app_event(&tool_invoked(&format!("c{n}"), "read_chapter", "ch1"));
        }
        s.on_app_event(&AppEvent::RefineDelta {
            delta: "partial".to_string(),
        });
        s.rebuild_renders(80, &theme);
        let before: Vec<_> = s.renders.iter().map(|r| r.key).collect();

        s.on_app_event(&AppEvent::RefineDelta {
            delta: " more".to_string(),
        });
        s.rebuild_renders(80, &theme);
        let after: Vec<_> = s.renders.iter().map(|r| r.key).collect();

        assert_eq!(
            before[..before.len() - 1],
            after[..after.len() - 1],
            "a delta must not invalidate the messages above it"
        );
        assert_ne!(before.last(), after.last(), "the tail did change");
    }

    #[test]
    fn a_reloaded_session_shows_what_the_live_one_did() {
        use crate::llm::{FunctionCall, Message, Role, ToolCall};
        let mut live = RefineScreen::new();
        let id = live.block_id();
        live.push_block(blocks::Block::user(id, "fix chapter 3".to_string()));
        live.on_app_event(&tool_invoked("c1", "read_chapter", "v1/c3"));
        live.on_app_event(&AppEvent::RefineToolReturned {
            id: "c1".to_string(),
            ok: true,
            detail: "read 40 lines".to_string(),
        });
        live.on_app_event(&AppEvent::RefineDelta {
            delta: "tightened it".to_string(),
        });
        live.on_app_event(&AppEvent::RefineMessageDone);

        let messages = vec![
            Message::user("fix chapter 3"),
            Message {
                role: Role::Assistant,
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "c1".to_string(),
                    kind: "function".to_string(),
                    function: FunctionCall {
                        name: "read_chapter".to_string(),
                        arguments: r#"{"note":"v1/c3"}"#.to_string(),
                    },
                }]),
                tool_call_id: None,
                name: None,
            },
            Message::tool_result("c1", r#"{"ok":true,"message":"read 40 lines"}"#),
            Message::assistant("tightened it"),
        ];
        let mut reloaded = RefineScreen::new();
        reloaded.load_blocks(
            display_blocks(&messages, COMPACT_SUMMARY_PREFIX),
            String::new(),
        );

        let roles = |s: &RefineScreen| s.blocks.iter().map(|b| b.role()).collect::<Vec<_>>();
        assert_eq!(roles(&live), roles(&reloaded));
        assert_eq!(
            live.blocks.iter().map(|b| b.body.clone()).collect::<Vec<_>>(),
            reloaded
                .blocks
                .iter()
                .map(|b| b.body.clone())
                .collect::<Vec<_>>()
        );
    }

    fn with_runs() -> RefineScreen {
        let mut s = RefineScreen::new();
        for (n, (title, status)) in [
            ("sweep ch12", RefineSubagentStatus::Running),
            ("glossary pass", RefineSubagentStatus::Succeeded),
            ("synopsis", RefineSubagentStatus::Running),
        ]
        .into_iter()
        .enumerate()
        {
            let id = format!("call_{n}");
            s.on_app_event(&AppEvent::RefineSubagentStarted {
                id: id.clone(),
                depth: 0,
                title: title.to_string(),
                role: "explore".to_string(),
                model: "openrouter/gemini".to_string(),
                background: true,
            });
            if status != RefineSubagentStatus::Running {
                s.on_app_event(&AppEvent::RefineSubagentFinished {
                    id,
                    status,
                    summary: "done".to_string(),
                });
            }
        }
        s
    }

    #[test]
    fn the_tasks_pane_owns_the_keyboard_while_it_is_up() {
        let mut s = with_runs();
        assert!(!s.owns_keyboard());
        s.toggle_tasks();
        assert!(s.owns_keyboard());
        // ...so its single letters are not the screen's globals.
        assert!(s.is_capturing());

        // Newest first, so the selection starts on the most recent run.
        assert_eq!(s.task_rows(), vec![2, 1, 0]);
        s.handle_key(key(KeyCode::Down), None);
        assert_eq!(s.tasks, Some(1));
        s.handle_key(typed('j'), None);
        assert_eq!(s.tasks, Some(2));
        s.handle_key(typed('j'), None);
        assert_eq!(s.tasks, Some(2), "the list stops rather than wrapping");

        s.handle_key(key(KeyCode::Esc), None);
        assert!(s.tasks.is_none());
        assert!(!s.owns_keyboard());
    }

    #[test]
    fn hiding_completed_runs_leaves_the_running_ones() {
        let mut s = with_runs();
        s.toggle_tasks();
        s.handle_key(typed('h'), None);
        assert!(s.tasks_hide_done);
        assert_eq!(s.task_rows(), vec![2, 0], "only the running two");
        assert_eq!(s.tasks, Some(0), "and the selection is valid again");
        s.handle_key(typed('h'), None);
        assert_eq!(s.task_rows(), vec![2, 1, 0]);
    }

    #[test]
    fn stopping_from_the_tasks_pane_targets_that_one_run() {
        let mut s = with_runs();
        s.toggle_tasks();
        // Row 0 is `call_2`, which is running.
        match s.handle_key(typed('x'), None) {
            Action::RefineCancelSubagent { id } => assert_eq!(id, "call_2"),
            other => panic!("expected a cancel, got {other:?}"),
        }
        // Row 1 is `call_1`, which already finished — nothing to stop.
        s.tasks = Some(1);
        assert!(matches!(s.handle_key(typed('x'), None), Action::None));
    }

    #[test]
    fn enter_on_a_subagent_block_opens_its_transcript() {
        let mut s = with_runs();
        s.on_app_event(&AppEvent::RefineSubagentTurn {
            id: "call_0".to_string(),
            text: "· read_chapter {\"ch\":12}".to_string(),
        });
        let i = s
            .blocks
            .iter()
            .position(|b| b.subagent_id() == Some("call_0"))
            .expect("a sub-agent gets a block in the conversation");

        s.focused = false;
        s.selected = Some(i);
        s.handle_key(key(KeyCode::Enter), None);
        assert_eq!(s.child.as_deref(), Some("call_0"));
        assert!(s.child_transcript("call_0").contains("read_chapter"));
        assert!(s.owns_keyboard());

        // Read-only, and ⌃C stops that child rather than the turn.
        match s.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL), None) {
            Action::RefineCancelSubagent { id } => assert_eq!(id, "call_0"),
            other => panic!("expected a cancel, got {other:?}"),
        }
        s.handle_key(typed('q'), None);
        assert!(s.child.is_none());
    }

    #[test]
    fn a_child_transcript_is_capped_and_says_it_was() {
        let mut s = with_runs();
        for n in 0..(CHILD_TRANSCRIPT_CAP + 50) {
            s.on_app_event(&AppEvent::RefineSubagentTurn {
                id: "call_0".to_string(),
                text: format!("line {n}"),
            });
        }
        let kept = s.child_transcript("call_0");
        assert!(kept.lines().count() <= CHILD_TRANSCRIPT_CAP + 1);
        assert!(kept.contains("earlier line(s) dropped"), "{}", &kept[..80]);
        assert!(
            kept.contains(&format!("line {}", CHILD_TRANSCRIPT_CAP + 49)),
            "the newest line is the one worth keeping"
        );
    }

    #[test]
    fn compaction_event_adds_a_visible_notice() {
        let mut s = RefineScreen::new();
        s.on_app_event(&AppEvent::RefineContextCompacted {
            dropped_messages: 4,
            token_estimate: 120_000,
            context_max: 128_000,
        });
        assert_eq!(s.blocks.len(), 1);
        assert_eq!(s.blocks[0].role(), TurnRole::Tool);
        let shown = s.visible_text(80);
        assert!(shown.contains("context summarized"), "{shown}");
        assert!(shown.contains("4 old message"), "{shown}");
    }

    #[test]
    fn subagent_events_populate_panel_state_and_clear() {
        let mut s = RefineScreen::new();

        s.on_app_event(&started("call_1", "audit volume 2"));

        assert_eq!(s.subagents.len(), 1);
        assert_eq!(s.subagents[0].depth, 1);
        assert_eq!(s.subagents[0].title, "audit volume 2");
        assert_eq!(s.subagents[0].role, "explore");
        assert_eq!(s.subagents[0].status, RefineSubagentStatus::Running);
        assert_eq!(s.subagents[0].activity, "starting");

        s.on_app_event(&AppEvent::RefineSubagentActivity {
            id: "call_1".to_string(),
            activity: "reading chapter · {\"ch\":2}".to_string(),
        });

        assert_eq!(s.subagents[0].title, "audit volume 2");
        assert_eq!(s.subagents[0].status, RefineSubagentStatus::Running);
        assert!(s.subagents[0].activity.contains("reading chapter"));

        s.on_app_event(&AppEvent::RefineSubagentFinished {
            id: "call_1".to_string(),
            status: RefineSubagentStatus::Succeeded,
            summary: "sub-agent finished (3 tool call(s))".to_string(),
        });

        assert_eq!(s.subagents.len(), 1);
        assert_eq!(s.subagents[0].title, "audit volume 2");
        assert_eq!(s.subagents[0].status, RefineSubagentStatus::Succeeded);
        assert!(s.subagents[0].summary.contains("3 tool"));
        assert!(s.subagents[0].activity.is_empty());
        // The elapsed time stops at the end rather than ticking forever.
        let settled = s.subagents[0].elapsed();
        assert_eq!(settled, s.subagents[0].elapsed());

        s.clear();
        assert!(s.subagents.is_empty());
    }

    fn started(id: &str, title: &str) -> AppEvent {
        AppEvent::RefineSubagentStarted {
            id: id.to_string(),
            depth: 1,
            title: title.to_string(),
            role: "explore".to_string(),
            model: "m".to_string(),
            background: false,
        }
    }

    #[test]
    fn an_activity_line_for_an_unknown_subagent_invents_nothing() {
        let mut s = RefineScreen::new();
        s.on_app_event(&AppEvent::RefineSubagentActivity {
            id: "ghost".to_string(),
            activity: "thinking".to_string(),
        });
        assert!(s.subagents.is_empty());

        // ...and nor does an ending for one that never started.
        s.on_app_event(&AppEvent::RefineSubagentFinished {
            id: "ghost".to_string(),
            status: RefineSubagentStatus::Canceled,
            summary: "cancelled by the user".to_string(),
        });
        assert!(s.subagents.is_empty());
    }

    #[test]
    fn a_finished_subagent_stops_reporting_activity() {
        let mut s = RefineScreen::new();
        s.on_app_event(&started("call_1", "sweep"));
        s.on_app_event(&AppEvent::RefineSubagentFinished {
            id: "call_1".to_string(),
            status: RefineSubagentStatus::Succeeded,
            summary: "done".to_string(),
        });
        s.on_app_event(&AppEvent::RefineSubagentActivity {
            id: "call_1".to_string(),
            activity: "thinking".to_string(),
        });
        assert!(s.subagents[0].activity.is_empty());
        assert_eq!(s.subagents[0].status, RefineSubagentStatus::Succeeded);
    }

    #[test]
    fn subagents_keep_separate_plans_without_replacing_main_plan() {
        let mut s = RefineScreen::new();
        s.plan = vec![PlanStep {
            step: "main task".to_string(),
            status: PlanStepStatus::InProgress,
        }];

        for id in ["call_1", "call_2"] {
            s.on_app_event(&started(id, &format!("run {id}")));
        }
        s.on_app_event(&AppEvent::RefineSubagentPlanUpdated {
            id: "call_1".to_string(),
            steps: vec![PlanStep {
                step: "first child task".to_string(),
                status: PlanStepStatus::InProgress,
            }],
        });
        s.on_app_event(&AppEvent::RefineSubagentPlanUpdated {
            id: "call_2".to_string(),
            steps: vec![PlanStep {
                step: "second child task".to_string(),
                status: PlanStepStatus::Pending,
            }],
        });

        assert_eq!(s.plan[0].step, "main task");
        assert_eq!(s.subagents[0].plan[0].step, "first child task");
        assert_eq!(s.subagents[1].plan[0].step, "second child task");

        s.on_app_event(&AppEvent::RefineSubagentPlanUpdated {
            id: "call_1".to_string(),
            steps: Vec::new(),
        });
        assert_eq!(s.plan[0].step, "main task");
        assert!(s.subagents[0].plan.is_empty());
        assert_eq!(s.subagents[1].plan[0].step, "second child task");
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

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    fn typed(c: char) -> KeyEvent {
        KeyEvent::from(KeyCode::Char(c))
    }

    fn ask(questions: Vec<RefineQuestion>) -> AppEvent {
        AppEvent::RefineDecisionRequest { id: 7, questions }
    }

    fn q(text: &str, options: &[&str]) -> RefineQuestion {
        RefineQuestion {
            question: text.to_string(),
            options: options.iter().map(|s| s.to_string()).collect(),
            multiple: false,
        }
    }

    /// The fall-through this replaced: a free-text answer used to be typed into
    /// the shared chat buffer, so it survived the card and went to the agent as
    /// the next message.
    #[test]
    fn typing_an_answer_never_reaches_the_chat_input() {
        let mut s = RefineScreen::new();
        s.on_app_event(&ask(vec![q("Which rendering?", &[])]));
        for c in "ทาคาฮาชิ".chars() {
            s.handle_key(typed(c), None);
        }
        assert!(s.input.is_empty(), "the chat buffer must stay untouched");
        let Some(RefinePending::Ask(sess)) = s.pending.front() else {
            panic!("the card should still be up");
        };
        assert_eq!(sess.answer().text, "ทาคาฮาชิ");
    }

    /// `/` used to open the slash-command popup over a blocking question,
    /// because the key gate only claimed Enter and Esc.
    #[test]
    fn a_slash_does_not_open_the_command_popup_while_a_question_is_up() {
        let mut s = RefineScreen::new();
        s.on_app_event(&ask(vec![q("Which rendering?", &[])]));
        s.handle_key(typed('/'), None);
        assert!(
            matches!(s.popup, Popup::None),
            "a blocking question owns the keyboard"
        );
        assert!(s.is_capturing(), "and says so, so globals stay suppressed");
    }

    /// Several questions in one call are one card, and the reply carries every
    /// answer in order — which is the whole reason the agent is told to batch.
    #[test]
    fn every_question_in_one_call_is_asked_and_answered_in_order() {
        let mut s = RefineScreen::new();
        s.on_app_event(&ask(vec![
            q("Rendering for 高橋?", &["ทาคาฮาชิ", "ทากาฮาชิ"]),
            q("Keep the honorific?", &["yes", "no"]),
        ]));

        // First question: pick the second option, which advances.
        s.handle_key(key(KeyCode::Down), None);
        assert!(matches!(s.handle_key(key(KeyCode::Enter), None), Action::None));
        // Second question: type instead of picking.
        for c in "only for elders".chars() {
            s.handle_key(typed(c), None);
        }
        let action = s.handle_key(key(KeyCode::Enter), None);
        let Action::RefineRespondInteraction { id, answer } = action else {
            panic!("the last question should submit, got {action:?}");
        };
        assert_eq!(id, 7);
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&answer).unwrap(),
            vec!["ทากาฮาชิ".to_string(), "only for elders".to_string()]
        );
        assert!(s.pending.is_empty(), "the card goes when it is answered");
    }

    /// ←→ revisit an earlier question instead of a separate review step.
    #[test]
    fn arrows_revisit_an_earlier_question_without_losing_its_answer() {
        let mut s = RefineScreen::new();
        s.on_app_event(&ask(vec![q("One?", &["a", "b"]), q("Two?", &["c", "d"])]));
        s.handle_key(key(KeyCode::Enter), None);
        s.handle_key(key(KeyCode::Left), None);

        let Some(RefinePending::Ask(sess)) = s.pending.front() else {
            panic!("card gone");
        };
        assert_eq!(sess.at, 0, "← should go back a question");
        assert_eq!(sess.answers[0].picked, vec![0], "and keep what was answered");
    }

    /// Two sub-agents can each raise a question. The second used to overwrite
    /// the first, stranding a oneshot the agent was still awaiting.
    #[test]
    fn a_second_request_does_not_orphan_the_first() {
        let mut s = RefineScreen::new();
        s.on_app_event(&ask(vec![q("First?", &[])]));
        s.on_app_event(&AppEvent::RefineDecisionRequest {
            id: 8,
            questions: vec![q("Second?", &[])],
        });
        assert_eq!(s.pending.len(), 2);

        for c in "one".chars() {
            s.handle_key(typed(c), None);
        }
        let first = s.handle_key(key(KeyCode::Enter), None);
        assert!(
            matches!(first, Action::RefineRespondInteraction { id: 7, .. }),
            "the front card answers first, got {first:?}"
        );
        assert_eq!(s.pending.len(), 1, "the queued one is still there");
        let Some(RefinePending::Ask(sess)) = s.pending.front() else {
            panic!("card gone");
        };
        assert_eq!(sess.id, 8);
        assert!(
            sess.answer().text.is_empty(),
            "and did not inherit the first card's typing"
        );
    }

    /// Dismissal is still an empty reply, which is what the agent reads as one.
    #[test]
    fn esc_dismisses_and_confirms_first_when_something_was_typed() {
        let mut s = RefineScreen::new();
        s.on_app_event(&ask(vec![q("Which?", &[])]));
        assert!(
            matches!(s.handle_key(key(KeyCode::Esc), None), Action::RefineRespondInteraction { answer, .. } if answer.is_empty()),
            "an untouched card goes on the first Esc"
        );

        s.on_app_event(&ask(vec![q("Which?", &[])]));
        s.handle_key(typed('x'), None);
        assert!(
            matches!(s.handle_key(key(KeyCode::Esc), None), Action::None),
            "a typed answer is not thrown away by one keystroke"
        );
        assert!(matches!(
            s.handle_key(key(KeyCode::Esc), None),
            Action::RefineRespondInteraction { .. }
        ));
    }

    /// A question that offers options can still be answered in prose, and what
    /// was typed wins — typing it is the more specific act.
    #[test]
    fn a_typed_answer_beats_a_picked_option() {
        let mut s = RefineScreen::new();
        s.on_app_event(&ask(vec![q("Which?", &["a", "b"])]));
        s.handle_key(key(KeyCode::Down), None);
        s.handle_key(key(KeyCode::Down), None);
        for c in "neither".chars() {
            s.handle_key(typed(c), None);
        }
        let Action::RefineRespondInteraction { answer, .. } = s.handle_key(key(KeyCode::Enter), None)
        else {
            panic!("should submit");
        };
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&answer).unwrap(),
            vec!["neither".to_string()]
        );
    }

    /// The agent opts into several answers, and then Enter accumulates rather
    /// than moving on — otherwise the second pick is unreachable.
    #[test]
    fn a_multiple_choice_question_accumulates_picks() {
        let mut s = RefineScreen::new();
        s.on_app_event(&ask(vec![RefineQuestion {
            question: "Which chapters?".into(),
            options: vec!["1".into(), "2".into(), "3".into()],
            multiple: true,
        }]));
        s.handle_key(key(KeyCode::Enter), None);
        s.handle_key(key(KeyCode::Down), None);
        s.handle_key(key(KeyCode::Down), None);
        s.handle_key(key(KeyCode::Enter), None);

        let Some(RefinePending::Ask(sess)) = s.pending.front() else {
            panic!("a multi-select card must not submit on the first Enter");
        };
        assert_eq!(sess.answers[0].picked, vec![0, 2]);
        let Action::RefineRespondInteraction { answer, .. } =
            s.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL), None)
        else {
            panic!("^s should send");
        };
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&answer).unwrap(),
            vec!["1, 3".to_string()]
        );
    }

    /// The diff used to stop at twenty lines with no way to see the rest, which
    /// is a poor basis for approving an edit.
    #[test]
    fn a_long_diff_scrolls_rather_than_truncating() {
        let mut s = RefineScreen::new();
        let diff = (0..60).map(|i| format!("+line {i}")).collect::<Vec<_>>().join("\n");
        s.on_app_event(&AppEvent::RefineApprovalRequest {
            id: 3,
            summary: "rewrite ch.3".into(),
            diff,
        });
        s.handle_key(key(KeyCode::Down), None);
        s.handle_key(key(KeyCode::PageDown), None);
        let Some(RefinePending::Approval { scroll, .. }) = s.pending.front() else {
            panic!("approval gone");
        };
        assert_eq!(*scroll, 11, "the diff scrolls");
    }
}
