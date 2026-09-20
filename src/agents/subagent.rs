//! Sub-agent roles: what a delegated child is allowed to do, and what it is
//! told it is for.
//!
//! A role is a capability gate, not a hint. The role decides which tools the
//! child's request advertises *and* is checked again when a call arrives, so a
//! child that invents a tool name it was never offered gets a refusal rather
//! than an edit.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, oneshot};

use crate::llm::Tool;
use crate::model::{RefineSubagentStatus, TargetLanguage};

/// Tools every role gets: reading, searching, planning, asking.
const READ: &[&str] = &[
    "list_volumes",
    "list_chapters",
    "read_chapter",
    "grep_chapter",
    "read_meta",
    "list_flagged_chunks",
    "read_lexicon",
    "search_project",
    "update_plan",
    "ask_user",
    "message_subagent",
];

/// Chapter prose.
const EDIT: &[&str] = &[
    "replace_chapter_text",
    "edit_chapter",
    "multi_edit_chapter",
    "replace_across_project",
    "retranslate_chapter",
    "refine_chapter_with_feedback",
];

/// Reference data: who people are, what terms mean, how the book reads.
const LEXICON: &[&str] = &[
    "upsert_character",
    "merge_character",
    "remove_character",
    "upsert_glossary_term",
    "remove_glossary_term",
    "set_recap",
    "set_chapter_summary",
    "set_synopsis",
    "append_style_note",
    "add_style_example",
    "add_continuity_note",
];

/// Delegation. Only `General` may fan out further.
const SPAWN: &[&str] = &[
    "task",
    "list_interrupted_subagents",
    "resume_subagent",
    "subagent_output",
    "cancel_subagent",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SubagentRole {
    /// Reads and reports. Cannot change anything.
    Explore,
    /// Reads and rewrites chapter prose.
    Editor,
    /// Reads and maintains characters, glossary, style and recaps.
    Lexicon,
    #[default]
    General,
}

impl SubagentRole {
    /// Unknown names fall back to `General` rather than failing the call: a
    /// wrong role should cost capability breadth, not the whole task.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "explore" => Self::Explore,
            "editor" => Self::Editor,
            "lexicon" => Self::Lexicon,
            _ => Self::General,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Explore => "explore",
            Self::Editor => "editor",
            Self::Lexicon => "lexicon",
            Self::General => "general",
        }
    }

    #[cfg(test)]
    pub const ALL: [SubagentRole; 4] = [
        SubagentRole::Explore,
        SubagentRole::Editor,
        SubagentRole::Lexicon,
        SubagentRole::General,
    ];

    pub fn allows(self, tool: &str) -> bool {
        if READ.contains(&tool) {
            return true;
        }
        match self {
            Self::Explore => false,
            Self::Editor => EDIT.contains(&tool),
            Self::Lexicon => LEXICON.contains(&tool),
            Self::General => EDIT.contains(&tool) || LEXICON.contains(&tool) || SPAWN.contains(&tool),
        }
    }

    /// The child's advertised tool list.
    pub fn tools(self) -> Vec<Tool> {
        super::refine::refine_tools_vec()
            .into_iter()
            .filter(|t| self.allows(&t.function.name))
            .collect()
    }

    /// Why a tool was refused, in terms the model can act on.
    pub fn refusal(self, tool: &str) -> String {
        format!(
            "tool `{tool}` is not available to a `{}` sub-agent; report what you found and let the parent make the change",
            self.label()
        )
    }

    pub fn system_prompt(self, target: TargetLanguage) -> String {
        let base = match target {
            TargetLanguage::Thai => SUBAGENT_SYSTEM_THAI,
            TargetLanguage::English => SUBAGENT_SYSTEM_ENGLISH,
        };
        format!("{base}\n\n{}", self.charge())
    }

    fn charge(self) -> &'static str {
        match self {
            Self::Explore => "Your role is `explore`: gather evidence and report it. You have no editing tools at all — not for chapters and not for reference data — so do not plan an edit, do not promise one, and do not ask for one. Read widely, quote what you find with chapter and line, and end with findings the parent can act on.",
            Self::Editor => "Your role is `editor`: change chapter prose and nothing else. You cannot touch characters, glossary, style or recaps, so when an edit implies a reference-data change, make the prose edit and name the metadata change in your report for the parent to apply.",
            Self::Lexicon => "Your role is `lexicon`: maintain characters, glossary, style notes, recaps and summaries. You cannot edit chapter text, so when the reference data you fix implies a prose change, record the reference change and name the affected chapters in your report.",
            Self::General => "Your role is `general`: you hold the full tool set, including delegation. Spawn sub-agents only for large independent disjoint scopes; nesting is app-bounded.",
        }
    }
}

/// What a delegated child is, as one value: it travels from the `task` call
/// into the checkpoint, and back out again on resume.
#[derive(Debug, Clone)]
pub struct SubagentSpec {
    pub task: String,
    pub scope: Option<String>,
    pub role: SubagentRole,
    pub depth: usize,
}

/// How many children may be live at once. A model asked to parallelise will
/// happily ask for fifty; past this it is told to collect some first.
pub const MAX_LIVE_SUBAGENTS: usize = 8;

/// Finished runs stay listed so their output can still be collected, but not
/// forever — the oldest finished entry is dropped past this.
const MAX_REMEMBERED: usize = 64;

/// When a message reaches a running child.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Delivery {
    /// At the child's next round boundary. Redirects work in progress.
    #[default]
    Steer,
    /// Held until the child would otherwise finish, turning "done" into "now
    /// also do this". Dropped unread if the child stops for any other reason.
    Queue,
    /// Like `Steer`, but abandons the model call already in flight so the
    /// child sees it now rather than after the current round.
    Interject,
}

impl Delivery {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "queue" => Self::Queue,
            "interject" => Self::Interject,
            _ => Self::Steer,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Steer => "steer",
            Self::Queue => "queue",
            Self::Interject => "interject",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChildMessage {
    pub text: String,
    pub delivery: Delivery,
}

/// The controls handed to a child when it registers.
pub struct RunHandle {
    pub cancel: Arc<AtomicBool>,
    /// Set when a message should abandon the model call already in flight.
    pub interrupt: Arc<AtomicBool>,
    pub inbox: mpsc::UnboundedReceiver<ChildMessage>,
}

/// What looking a run up finds.
pub enum Collected {
    /// Finished; here is its tool-result JSON.
    Done(String),
    /// Started but not finished.
    Running,
    /// No such run.
    Unknown,
}

struct Slot {
    status: RefineSubagentStatus,
    cancel: Arc<AtomicBool>,
    interrupt: Arc<AtomicBool>,
    inbox: mpsc::UnboundedSender<ChildMessage>,
    result: Option<String>,
    waiters: Vec<oneshot::Sender<String>>,
    seq: u64,
}

/// Every sub-agent this Refine session has started, running or finished.
///
/// It is created once per agent task rather than per turn, which is what lets
/// a background child outlive the turn that spawned it.
#[derive(Clone, Default)]
pub struct SubagentRegistry {
    inner: Arc<Mutex<Registry>>,
}

#[derive(Default)]
struct Registry {
    slots: HashMap<String, Slot>,
    seq: u64,
}

impl SubagentRegistry {
    /// Register a run and take its controls. Replaces any entry under the same
    /// id, which is how a resumed child reuses its own slot.
    pub fn register(&self, id: &str) -> RunHandle {
        let cancel = Arc::new(AtomicBool::new(false));
        let interrupt = Arc::new(AtomicBool::new(false));
        let (tx, inbox) = mpsc::unbounded_channel();
        if let Ok(mut reg) = self.inner.lock() {
            reg.seq += 1;
            let seq = reg.seq;
            reg.slots.insert(
                id.to_string(),
                Slot {
                    status: RefineSubagentStatus::Running,
                    cancel: cancel.clone(),
                    interrupt: interrupt.clone(),
                    inbox: tx,
                    result: None,
                    waiters: Vec::new(),
                    seq,
                },
            );
            reg.trim(id);
        }
        RunHandle {
            cancel,
            interrupt,
            inbox,
        }
    }

    pub fn finish(&self, id: &str, status: RefineSubagentStatus, result: String) {
        let Ok(mut reg) = self.inner.lock() else { return };
        let Some(slot) = reg.slots.get_mut(id) else {
            return;
        };
        slot.status = status;
        slot.result = Some(result.clone());
        for w in slot.waiters.drain(..) {
            let _ = w.send(result.clone());
        }
    }

    /// A look, with no side effect — polling must not leave a waiter behind.
    pub fn poll(&self, id: &str) -> Collected {
        let Ok(reg) = self.inner.lock() else {
            return Collected::Unknown;
        };
        match reg.slots.get(id) {
            None => Collected::Unknown,
            Some(slot) => match &slot.result {
                Some(result) => Collected::Done(result.clone()),
                None => Collected::Running,
            },
        }
    }

    /// Park a waiter on a run that has not finished. `None` when it already
    /// has, or was never known.
    pub fn waiter(&self, id: &str) -> Option<oneshot::Receiver<String>> {
        let mut reg = self.inner.lock().ok()?;
        let slot = reg.slots.get_mut(id)?;
        if slot.result.is_some() {
            return None;
        }
        let (tx, rx) = oneshot::channel();
        slot.waiters.push(tx);
        Some(rx)
    }

    pub fn live(&self) -> usize {
        self.inner
            .lock()
            .map(|reg| {
                reg.slots
                    .values()
                    .filter(|s| s.status == RefineSubagentStatus::Running)
                    .count()
            })
            .unwrap_or(0)
    }

    /// True when the run existed and was running.
    pub fn cancel(&self, id: &str) -> bool {
        let Ok(reg) = self.inner.lock() else {
            return false;
        };
        let Some(slot) = reg.slots.get(id) else {
            return false;
        };
        if slot.status != RefineSubagentStatus::Running {
            return false;
        }
        slot.cancel.store(true, Ordering::Relaxed);
        slot.interrupt.store(true, Ordering::Relaxed);
        true
    }

    pub fn cancel_all(&self) {
        let Ok(reg) = self.inner.lock() else { return };
        for slot in reg.slots.values() {
            slot.cancel.store(true, Ordering::Relaxed);
            slot.interrupt.store(true, Ordering::Relaxed);
        }
    }

    /// True when the run existed, was running, and took the message.
    pub fn send(&self, id: &str, msg: ChildMessage) -> bool {
        let Ok(reg) = self.inner.lock() else {
            return false;
        };
        let Some(slot) = reg.slots.get(id) else {
            return false;
        };
        if slot.status != RefineSubagentStatus::Running {
            return false;
        }
        if msg.delivery == Delivery::Interject {
            slot.interrupt.store(true, Ordering::Relaxed);
        }
        slot.inbox.send(msg).is_ok()
    }

}

impl Registry {
    fn trim(&mut self, keep: &str) {
        while self.slots.len() > MAX_REMEMBERED {
            let oldest = self
                .slots
                .iter()
                .filter(|(id, s)| {
                    s.status != RefineSubagentStatus::Running && id.as_str() != keep
                })
                .min_by_key(|(_, s)| s.seq)
                .map(|(id, _)| id.clone());
            match oldest {
                Some(id) => {
                    self.slots.remove(&id);
                }
                // Everything left is running; dropping one would strand it.
                None => break,
            }
        }
    }
}

const SUBAGENT_SYSTEM_THAI: &str = "You are a focused sub-agent inside honya's Refine system completing ONE self-contained parent-delegated task. Use the project tools to gather evidence, make surgical changes, verify them, then report chapters/terms/characters touched. Keep Thai idiomatic; preserve scene breaks, image links, and Markdown. When a female character uses `僕/ぼく/ボク` as her self-pronoun, render it as `เรา`, never `ผม` or `โบคุ`; identify the character from context rather than inferring gender from `僕` alone. If the parent needs something before you finish — a blocker, a decision only it can make, or a finding that changes its plan — send it with message_subagent using the id \"parent\" instead of saving it for your final report.";

const SUBAGENT_SYSTEM_ENGLISH: &str = "You are a focused sub-agent inside honya's Refine system, completing one self-contained task delegated by a parent agent. Read the real Japanese source, English translation, and reference data before editing. Make only evidence-backed surgical changes, keep the English idiomatic and publication-ready for native light-novel readers, preserve scene breaks, image links, and Markdown, verify the result, then report the chapters and metadata changed. If the parent needs something before you finish — a blocker, a decision only it can make, or a finding that changes its plan — send it with message_subagent using the id \"parent\" instead of saving it for your final report.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explore_subagent_cannot_edit_anything() {
        let r = SubagentRole::Explore;
        assert!(r.allows("read_chapter"));
        assert!(r.allows("grep_chapter"));
        assert!(!r.allows("edit_chapter"));
        assert!(!r.allows("multi_edit_chapter"));
        assert!(!r.allows("upsert_character"));
        assert!(!r.allows("task"));
        let names: Vec<_> = r.tools().into_iter().map(|t| t.function.name).collect();
        assert!(names.contains(&"read_chapter".to_string()));
        assert!(!names.contains(&"edit_chapter".to_string()));
    }

    #[test]
    fn editor_and_lexicon_do_not_overlap_outside_reads() {
        assert!(SubagentRole::Editor.allows("edit_chapter"));
        assert!(!SubagentRole::Editor.allows("upsert_character"));
        assert!(SubagentRole::Lexicon.allows("upsert_character"));
        assert!(!SubagentRole::Lexicon.allows("edit_chapter"));
        // Reading is common ground; neither can work blind.
        for r in [SubagentRole::Editor, SubagentRole::Lexicon] {
            assert!(r.allows("read_chapter"));
            assert!(r.allows("read_lexicon"));
        }
    }

    #[test]
    fn only_general_delegates_further() {
        for r in SubagentRole::ALL {
            assert_eq!(r.allows("task"), r == SubagentRole::General);
        }
    }

    #[test]
    fn every_advertised_tool_is_one_the_role_allows() {
        for r in SubagentRole::ALL {
            for t in r.tools() {
                assert!(r.allows(&t.function.name), "{} advertised {}", r.label(), t.function.name);
            }
        }
    }

    #[test]
    fn an_unknown_role_name_is_general() {
        assert_eq!(SubagentRole::parse("wizard"), SubagentRole::General);
        assert_eq!(SubagentRole::parse("EXPLORE"), SubagentRole::Explore);
        assert_eq!(SubagentRole::parse(" editor "), SubagentRole::Editor);
    }

    #[test]
    fn a_role_says_what_it_is_for_in_its_prompt() {
        for r in SubagentRole::ALL {
            let p = r.system_prompt(TargetLanguage::Thai);
            assert!(p.contains("sub-agent"));
            assert!(p.contains(r.label()), "{} prompt omits its own role", r.label());
        }
    }
}
