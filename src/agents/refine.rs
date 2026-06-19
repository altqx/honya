//! Refine chat agent backend: cross-volume tools plus the streaming loop.
//! Chapter-text edits archive the prior version before overwriting.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::oneshot;

use crate::app::refine::{MentionTarget, parse_scope};
use crate::llm::client::LlmClient;
use crate::llm::tool_loop::ToolExecutor;
use crate::llm::{ChatRequest, Message, Role, Tool};
use crate::model::{
    AppEvent, Character, ContinuityNote, EventTx, GlossaryTerm, LogLevel, PlanStep, RefineRequest,
    Relationship, StyleExample, TermPolicy, ToolResult,
};
use crate::workspace::{Workspace, characters, glossary, style, translation, volume};

const READ_CAP: usize = 12_000;

pub fn refine_tools_schema() -> serde_json::Value {
    json!([
        {"type":"function","function":{
            "name":"list_volumes",
            "description":"List every volume in the project with its label and chapter count. Call this first to learn the project's structure.",
            "parameters":{"type":"object","additionalProperties":false,"properties":{}}
        }},
        {"type":"function","function":{
            "name":"list_chapters",
            "description":"List a volume's chapters with number, title, kind, status, and chunk progress.",
            "parameters":{"type":"object","additionalProperties":false,"required":["vol"],
                "properties":{"vol":{"type":"integer"}}}
        }},
        {"type":"function","function":{
            "name":"read_chapter",
            "description":"Read a chapter's Japanese source and/or its current Thai translation (prose only). The Thai is returned with `N│ ` line-number prefixes and a total line count; long chapters are windowed (use offset/limit to page). NEVER copy the `N│ ` prefix into an edit. Read before editing so you work from the real text.",
            "parameters":{"type":"object","additionalProperties":false,"required":["ch"],
                "properties":{
                    "vol":{"type":"integer","description":"Volume number; defaults to the active volume."},
                    "ch":{"type":"integer"},
                    "include_jp":{"type":"boolean","description":"Include the Japanese source (default true)."},
                    "include_th":{"type":"boolean","description":"Include the Thai translation (default true)."},
                    "offset":{"type":"integer","description":"1-based first Thai line to return (default 1)."},
                    "limit":{"type":"integer","description":"Max Thai lines to return (default 400)."}
                }}
        }},
        {"type":"function","function":{
            "name":"grep_chapter",
            "description":"Find a substring inside one chapter and return matching lines with their line numbers, so you can locate the exact text to pass to edit_chapter. Searches the Thai translation by default.",
            "parameters":{"type":"object","additionalProperties":false,"required":["ch","query"],
                "properties":{
                    "vol":{"type":"integer"},
                    "ch":{"type":"integer"},
                    "query":{"type":"string"},
                    "side":{"type":"string","enum":["th","jp","both"],"description":"Which text to search (default th)."},
                    "ignore_case":{"type":"boolean","description":"Case-insensitive match (default true)."}
                }}
        }},
        {"type":"function","function":{
            "name":"read_meta",
            "description":"Read the project/volume metadata you can edit: style notes, the running recap, the synopsis, chapter summaries, continuity notes, and the project title. Use before editing any of them.",
            "parameters":{"type":"object","additionalProperties":false,
                "properties":{
                    "vol":{"type":"integer"},
                    "kind":{"type":"string","enum":["all","style","recap","synopsis","summaries","notes","project"],"description":"What to read (default all)."}
                }}
        }},
        {"type":"function","function":{
            "name":"list_flagged_chunks",
            "description":"List chunks still flagged [REVIEW NEEDED] (auto-translation that did not pass QA) with the reviewer's reason, for a chapter, a volume, or the whole project. Use to find exactly what needs fixing, then edit_chapter or refine_chapter_with_feedback those spots.",
            "parameters":{"type":"object","additionalProperties":false,
                "properties":{
                    "vol":{"type":"integer","description":"Restrict to one volume; omit for the whole project."},
                    "ch":{"type":"integer","description":"Restrict to one chapter (uses vol, else the active volume)."}
                }}
        }},
        {"type":"function","function":{
            "name":"update_plan",
            "description":"Record your working plan as a short checklist the user sees live. Call it for any multi-step or multi-chapter task, then call it again to update status as you progress: keep exactly one step in_progress, flip finished steps to completed, and add steps you discover. Each call REPLACES the whole list. Skip it for a single trivial edit.",
            "parameters":{"type":"object","additionalProperties":false,"required":["steps"],
                "properties":{
                    "steps":{"type":"array","items":{"type":"object","additionalProperties":false,
                        "required":["step"],
                        "properties":{
                            "step":{"type":"string"},
                            "status":{"type":"string","enum":["pending","in_progress","completed"]}
                        }}}
                }}
        }},
        {"type":"function","function":{
            "name":"read_lexicon",
            "description":"Read characters and/or glossary terms, optionally filtered by a query. Use to check existing names/terminology before editing.",
            "parameters":{"type":"object","additionalProperties":false,
                "properties":{
                    "vol":{"type":"integer"},
                    "query":{"type":"string"},
                    "kind":{"type":"string","enum":["characters","glossary","both"]}
                }}
        }},
        {"type":"function","function":{
            "name":"search_project",
            "description":"Search the translated Thai of every chapter (or one volume) for a substring; returns matching chapters with snippets.",
            "parameters":{"type":"object","additionalProperties":false,"required":["query"],
                "properties":{
                    "query":{"type":"string"},
                    "vol":{"type":"integer","description":"Restrict to one volume; omit to search the whole project."}
                }}
        }},
        {"type":"function","function":{
            "name":"upsert_character",
            "description":"Create or update a character in CHARACTERS.md (cross-volume; pass vol for the volume whose lexicon to edit).",
            "parameters":{"type":"object","additionalProperties":false,"required":["jp_name","thai_name"],
                "properties":{
                    "vol":{"type":"integer"},
                    "id":{"type":"string"},
                    "jp_name":{"type":"string"},
                    "thai_name":{"type":"string"},
                    "romaji":{"type":"string"},
                    "gender":{"type":"string","enum":["male","female","nonbinary","unknown"]},
                    "honorific":{"type":"string"},
                    "speech_style":{"type":"string"},
                    "relationships":{"type":"array","items":{"type":"object","additionalProperties":false,
                        "required":["target_id","relation"],
                        "properties":{"target_id":{"type":"string"},"relation":{"type":"string"}}}},
                    "aliases":{"type":"array","items":{"type":"string"}},
                    "notes":{"type":"string"},
                    "first_seen_chapter":{"type":"integer"}
                }}
        }},
        {"type":"function","function":{
            "name":"merge_character",
            "description":"Merge two CHARACTERS.md entries that are the same person; keeps into_id, removes from_id.",
            "parameters":{"type":"object","additionalProperties":false,"required":["from_id","into_id"],
                "properties":{"vol":{"type":"integer"},"from_id":{"type":"string"},"into_id":{"type":"string"}}}
        }},
        {"type":"function","function":{
            "name":"remove_character",
            "description":"Remove a character from CHARACTERS.md by id.",
            "parameters":{"type":"object","additionalProperties":false,"required":["id"],
                "properties":{"vol":{"type":"integer"},"id":{"type":"string"}}}
        }},
        {"type":"function","function":{
            "name":"upsert_glossary_term",
            "description":"Create or update a glossary term in GLOSSARY.md. The refine agent is human-directed, so this overwrites existing terms (echo what changed).",
            "parameters":{"type":"object","additionalProperties":false,"required":["jp_term","thai_term"],
                "properties":{
                    "vol":{"type":"integer"},
                    "jp_term":{"type":"string"},
                    "thai_term":{"type":"string"},
                    "romaji":{"type":"string"},
                    "category":{"type":"string","enum":["skill","place","org","item","title","concept","sfx","other"]},
                    "gloss":{"type":"string"},
                    "policy":{"type":"string","enum":["hard_locked","preferred","forbidden","context_dependent"]},
                    "forbidden_thai":{"type":"array","items":{"type":"string"}},
                    "context_rule":{"type":"string"},
                    "do_not_translate":{"type":"boolean"},
                    "first_seen_chapter":{"type":"integer"}
                }}
        }},
        {"type":"function","function":{
            "name":"remove_glossary_term",
            "description":"Remove a glossary term from GLOSSARY.md by its Japanese form.",
            "parameters":{"type":"object","additionalProperties":false,"required":["jp_term"],
                "properties":{"vol":{"type":"integer"},"jp_term":{"type":"string"}}}
        }},
        {"type":"function","function":{
            "name":"set_recap",
            "description":"Set the volume's running cross-chapter recap in VOLUME.md.",
            "parameters":{"type":"object","additionalProperties":false,"required":["recap"],
                "properties":{"vol":{"type":"integer"},"recap":{"type":"string"}}}
        }},
        {"type":"function","function":{
            "name":"set_chapter_summary",
            "description":"Set a chapter's one-line summary in VOLUME.md.",
            "parameters":{"type":"object","additionalProperties":false,"required":["chapter","summary"],
                "properties":{"vol":{"type":"integer"},"chapter":{"type":"integer"},"summary":{"type":"string"}}}
        }},
        {"type":"function","function":{
            "name":"set_synopsis",
            "description":"Set the volume synopsis. Provide the Thai synopsis (and optionally the raw source synopsis).",
            "parameters":{"type":"object","additionalProperties":false,"required":["thai"],
                "properties":{"vol":{"type":"integer"},"raw":{"type":"string"},"thai":{"type":"string"}}}
        }},
        {"type":"function","function":{
            "name":"append_style_note",
            "description":"Append a translation-style note (tone, register, conventions) to STYLE.md.",
            "parameters":{"type":"object","additionalProperties":false,"required":["note"],
                "properties":{"vol":{"type":"integer"},"note":{"type":"string"}}}
        }},
        {"type":"function","function":{
            "name":"add_style_example",
            "description":"Add a JP→TH exemplar pair to the volume's style anchors (guides future translation).",
            "parameters":{"type":"object","additionalProperties":false,"required":["jp","th"],
                "properties":{"vol":{"type":"integer"},"jp":{"type":"string"},"th":{"type":"string"},"note":{"type":"string"}}}
        }},
        {"type":"function","function":{
            "name":"add_continuity_note",
            "description":"Record a continuity observation in VOLUME.md (name/gender drift, term inconsistency, plot or tone concern) for later reference — without changing chapter text.",
            "parameters":{"type":"object","additionalProperties":false,"required":["severity","note"],
                "properties":{
                    "vol":{"type":"integer"},
                    "severity":{"type":"string","enum":["info","warning","conflict"]},
                    "note":{"type":"string"},
                    "chapter":{"type":"integer"},
                    "kind":{"type":"string","enum":["name","gender","term","plot","tone","other"]}
                }}
        }},
        {"type":"function","function":{
            "name":"replace_chapter_text",
            "description":"Replace a chapter's ENTIRE Thai translation with new_text. The prior version is archived first (reversible via /undo and the Reader diff). Use for a full rewrite; for a small edit prefer edit_chapter.",
            "parameters":{"type":"object","additionalProperties":false,"required":["ch","new_text"],
                "properties":{"vol":{"type":"integer"},"ch":{"type":"integer"},"new_text":{"type":"string"}}}
        }},
        {"type":"function","function":{
            "name":"edit_chapter",
            "description":"Surgically replace an exact snippet of a chapter's Thai. `old` must match the file EXACTLY (verbatim from read_chapter, without the `N│ ` line-number prefix) and be UNIQUE — if it appears more than once the edit fails; pass a longer surrounding snippet or set replace_all=true. The prior version is archived first (reversible via /undo and the Reader diff). This is the preferred tool for any targeted wording fix.",
            "parameters":{"type":"object","additionalProperties":false,"required":["ch","old","new"],
                "properties":{
                    "vol":{"type":"integer"},"ch":{"type":"integer"},
                    "old":{"type":"string","description":"Exact text to replace."},
                    "new":{"type":"string","description":"Replacement text."},
                    "replace_all":{"type":"boolean","description":"Replace every occurrence (default: require a unique match)."}
                }}
        }},
        {"type":"function","function":{
            "name":"multi_edit_chapter",
            "description":"Apply several exact edits to ONE chapter's Thai in a single atomic call (edits run in order; each `old` must match — unique unless replace_all). If any edit fails, nothing is written. The prior version is archived once. Prefer this over many edit_chapter calls on the same chapter.",
            "parameters":{"type":"object","additionalProperties":false,"required":["ch","edits"],
                "properties":{
                    "vol":{"type":"integer"},"ch":{"type":"integer"},
                    "edits":{"type":"array","items":{"type":"object","additionalProperties":false,
                        "required":["old","new"],
                        "properties":{
                            "old":{"type":"string"},"new":{"type":"string"},
                            "replace_all":{"type":"boolean"}
                        }}}
                }}
        }},
        {"type":"function","function":{
            "name":"replace_across_project",
            "description":"Project-wide consistency fix: replace an exact Thai string in EVERY chapter (or one volume) — e.g. standardizing a name or term rendering. Set dry_run=true first to preview which chapters and how many matches WITHOUT changing anything, then run for real. Each modified chapter is archived (per-chapter Reader diff). Afterwards update the matching glossary/character entry so future translation stays consistent.",
            "parameters":{"type":"object","additionalProperties":false,"required":["find","replace"],
                "properties":{
                    "vol":{"type":"integer","description":"Restrict to one volume; omit for the whole project."},
                    "find":{"type":"string"},"replace":{"type":"string"},
                    "dry_run":{"type":"boolean","description":"Preview match counts only; write nothing (default false)."}
                }}
        }},
        {"type":"function","function":{
            "name":"retranslate_chapter",
            "description":"Re-run the full Translator→Reviewer pipeline on whole chapters (a fresh, high-quality regeneration). Use this for 'redo this chapter properly'; for a small targeted fix prefer edit_chapter. The current translation is archived first.",
            "parameters":{"type":"object","additionalProperties":false,"required":["chapters"],
                "properties":{
                    "vol":{"type":"integer"},
                    "chapters":{"type":"array","items":{"type":"integer"}}
                }}
        }},
        {"type":"function","function":{
            "name":"refine_chapter_with_feedback",
            "description":"Re-translate one chapter through the pipeline, first recording your feedback as a style note so the new translation follows it. Use when the user wants a chapter redone a specific way.",
            "parameters":{"type":"object","additionalProperties":false,"required":["ch","feedback"],
                "properties":{
                    "vol":{"type":"integer"},"ch":{"type":"integer"},"feedback":{"type":"string"}
                }}
        }},
        {"type":"function","function":{
            "name":"task",
            "description":"Spawn a focused sub-agent to carry out a self-contained chunk of work (e.g. \"normalize every honorific in volume 2\") using the same project tools, then report back. Use for large or parallelizable sub-tasks so the main thread stays focused. The sub-agent runs to completion and returns a summary; it cannot spawn further sub-agents.",
            "parameters":{"type":"object","additionalProperties":false,"required":["description"],
                "properties":{
                    "description":{"type":"string","description":"The complete, self-contained task for the sub-agent."},
                    "scope":{"type":"string","description":"Optional scope hint, e.g. a volume or chapter range."}
                }}
        }},
        {"type":"function","function":{
            "name":"ask_user",
            "description":"Ask the user a question and wait for their answer when a decision is genuinely theirs to make and you cannot resolve it from the project or sensible defaults (e.g. choosing between two valid Thai renderings, or confirming a risky bulk change). Provide options for a multiple-choice decision, or omit them for a free-text answer. Use sparingly — prefer acting on a reasonable default and saying so.",
            "parameters":{"type":"object","additionalProperties":false,"required":["question"],
                "properties":{
                    "question":{"type":"string","description":"The question to put to the user."},
                    "options":{"type":"array","items":{"type":"string"},"description":"Optional choices; the user picks one."}
                }}
        }}
    ])
}

const MAX_TOOL_ROUNDS: usize = 40;
const COMPACT_FRACTION: f64 = 0.8;
const KEEP_RECENT_TURNS: usize = 3;

/// Conservative context-window guess for arbitrary model ids.
pub fn model_max_context(model: &str) -> u32 {
    let m = model.to_ascii_lowercase();
    if m.contains("[1m]") || m.contains("gemini") {
        1_000_000
    } else if m.contains("gpt-5") || m.contains("gpt-4.1") {
        400_000
    } else if m.contains("claude")
        || m.contains("opus")
        || m.contains("sonnet")
        || m.contains("haiku")
    {
        200_000
    } else {
        128_000
    }
}

/// Rough ~4 chars/token estimate, including tool I/O.
fn estimate_context_tokens(messages: &[Message]) -> u32 {
    let mut chars = 0usize;
    for m in messages {
        chars += m.content.as_deref().map_or(0, str::len);
        for tc in m.tool_calls.iter().flatten() {
            chars += tc.function.name.len() + tc.function.arguments.len();
        }
    }
    (chars / 4).min(u32::MAX as usize) as u32
}

/// Summarize old turns before the thread outgrows the model window.
fn maybe_compact(req: &mut ChatRequest, tx: &EventTx, force: bool) {
    if req.messages.len() <= 2 {
        return;
    }
    let max = model_max_context(&req.model);
    let est = estimate_context_tokens(&req.messages);
    if !force && est < (max as f64 * COMPACT_FRACTION) as u32 {
        return;
    }

    // Start of the kept tail, on a user-message boundary.
    let user_idxs: Vec<usize> = req
        .messages
        .iter()
        .enumerate()
        .filter(|(i, m)| *i > 0 && m.role == Role::User)
        .map(|(i, _)| i)
        .collect();
    let keep_from = user_idxs
        .iter()
        .rev()
        .nth(KEEP_RECENT_TURNS - 1)
        .copied()
        .unwrap_or(1);
    if keep_from <= 1 {
        return;
    }

    let summary = compact_summary(&req.messages[1..keep_from]);
    let dropped = keep_from - 1;
    let mut compacted = Vec::with_capacity(req.messages.len() - dropped + 1);
    compacted.push(req.messages[0].clone());
    compacted.push(Message::user(summary));
    compacted.extend(req.messages[keep_from..].iter().cloned());
    req.messages = compacted;

    tx.send(AppEvent::Log {
        level: LogLevel::Info,
        msg: format!(
            "Refine context compacted: summarized {dropped} earlier messages (~{}k of {}k tokens)",
            est / 1000,
            max / 1000
        ),
    });
}

/// Diff preview clipped for transcript display.
fn edit_diff_snippet(pairs: &[(&str, &str)]) -> String {
    let mut s = String::from("```diff\n");
    for (old, new) in pairs {
        for line in old.lines().take(8) {
            s.push_str("- ");
            s.push_str(line);
            s.push('\n');
        }
        for line in new.lines().take(8) {
            s.push_str("+ ");
            s.push_str(line);
            s.push('\n');
        }
    }
    s.push_str("```");
    s
}

/// Approval summary/diff for mutating chapter-edit tools.
fn approval_preview(name: &str, args: &str) -> Option<(String, String)> {
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    let s = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let ch = || v.get("ch").and_then(|x| x.as_u64()).unwrap_or(0);
    match name {
        "edit_chapter" => {
            let (old, new) = (s("old"), s("new"));
            Some((
                format!("edit chapter {}", ch()),
                edit_diff_snippet(&[(old.as_str(), new.as_str())]),
            ))
        }
        "multi_edit_chapter" => {
            let pairs: Vec<(String, String)> = v
                .get("edits")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .map(|e| {
                            (
                                e.get("old").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                                e.get("new").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            let refs: Vec<(&str, &str)> =
                pairs.iter().map(|(o, n)| (o.as_str(), n.as_str())).collect();
            Some((
                format!("apply {} edit(s) to chapter {}", pairs.len(), ch()),
                edit_diff_snippet(&refs),
            ))
        }
        "replace_chapter_text" => {
            let preview: String = s("new_text").chars().take(800).collect();
            Some((
                format!("replace ALL text of chapter {}", ch()),
                format!("```\n{preview}\n```"),
            ))
        }
        "replace_across_project" => {
            if v.get("dry_run").and_then(|x| x.as_bool()).unwrap_or(false) {
                return None;
            }
            Some((
                format!("replace across project: \"{}\" → \"{}\"", s("find"), s("replace")),
                String::new(),
            ))
        }
        _ => None,
    }
}

/// Compact old turns while dropping verbose tool noise.
fn compact_summary(dropped: &[Message]) -> String {
    let mut s = String::from("[Earlier conversation, compacted to fit the context window]\n");
    for m in dropped {
        let label = match m.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
            _ => continue,
        };
        let Some(text) = m.content.as_deref().map(str::trim).filter(|t| !t.is_empty()) else {
            continue;
        };
        let clipped: String = text.chars().take(280).collect();
        s.push_str(label);
        s.push_str(": ");
        s.push_str(&clipped);
        if text.chars().count() > 280 {
            s.push('…');
        }
        s.push('\n');
    }
    s
}

pub struct RefineCtx {
    pub client: Arc<dyn LlmClient>,
    pub root: PathBuf,
    pub default_vol: u32,
    pub model: crate::model::AgentModel,
    pub tx: EventTx,
    /// Stops the in-flight turn between rounds.
    pub cancel: Arc<AtomicBool>,
    pub session_id: String,
    pub interact: RefineInteract,
}

/// Chapter-edit approval policy cycled by Ctrl+Tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApprovalMode {
    /// Apply routine fixes directly; ask before risky, bulky, or subjective edits.
    #[default]
    Auto,
    /// Pause every mutating edit for user approval.
    Ask,
    /// Apply edits directly without approval prompts.
    Always,
}

impl ApprovalMode {
    fn from_u8(n: u8) -> Self {
        match n {
            1 => ApprovalMode::Ask,
            2 => ApprovalMode::Always,
            _ => ApprovalMode::Auto,
        }
    }
    fn to_u8(self) -> u8 {
        match self {
            ApprovalMode::Auto => 0,
            ApprovalMode::Ask => 1,
            ApprovalMode::Always => 2,
        }
    }
    /// Ctrl+Tab order.
    pub fn cycled(self) -> Self {
        match self {
            ApprovalMode::Always => ApprovalMode::Ask,
            ApprovalMode::Ask => ApprovalMode::Auto,
            ApprovalMode::Auto => ApprovalMode::Always,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            ApprovalMode::Auto => "auto",
            ApprovalMode::Ask => "ask",
            ApprovalMode::Always => "always-approve",
        }
    }
}

/// Shared interaction channel for edit approval and `ask_user`.
#[derive(Clone, Default)]
pub struct RefineInteract {
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<String>>>>,
    seq: Arc<AtomicU64>,
    mode: Arc<AtomicU8>,
}

impl RefineInteract {
    pub fn mode(&self) -> ApprovalMode {
        ApprovalMode::from_u8(self.mode.load(Ordering::Relaxed))
    }

    pub fn cycle_mode(&self) -> ApprovalMode {
        let next = self.mode().cycled();
        self.mode.store(next.to_u8(), Ordering::Relaxed);
        next
    }

    /// Register a pending interaction and receiver.
    fn open(&self) -> (u64, oneshot::Receiver<String>) {
        let id = self.seq.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        if let Ok(mut map) = self.pending.lock() {
            map.insert(id, tx);
        }
        (id, rx)
    }

    pub fn resolve(&self, id: u64, answer: String) {
        if let Ok(mut map) = self.pending.lock()
            && let Some(tx) = map.remove(&id)
        {
            let _ = tx.send(answer);
        }
    }
}

pub struct UserTurn {
    pub text: String,
}

pub enum RefineControl {
    Submit(UserTurn),
    Clear,
    SetModel(String),
    /// Swap the live thread to another (possibly new/empty) session id.
    SwitchSession(String),
    Compact,
    Shutdown,
}

fn seed_messages(root: &Path, id: &str) -> Vec<Message> {
    crate::workspace::refine_session::load(root, id)
        .map(|s| s.messages)
        .unwrap_or_default()
}

const SUBAGENT_SYSTEM: &str = "You are a focused sub-agent inside honya's Refine system, completing ONE self-contained task delegated by the main agent. You have the same project tools (read/search chapters and the lexicon; edit chapter text; update characters, glossary, style, recaps, summaries). Work autonomously — do not ask questions. Gather the context you need, make the surgical changes the task requires, verify them, then reply with a concise report of exactly what you did: chapters touched, terms/characters changed, and anything the main agent should know. Keep Thai idiomatic and preserve scene breaks, image links, and Markdown. You cannot spawn further sub-agents.";

fn refine_system_prompt() -> String {
    r#"You are honya's Refine agent — an autonomous, expert engineering assistant for a Japanese→Thai light-novel translation project, working through a chat tab inside the honya TUI. You read, fix, and refine anything in the project: any volume or chapter's Thai translation, the character roster, the glossary, the style guide, the volume recap/synopsis, and chapter summaries. Your tools read and write the project on disk; treat the on-disk files as the single source of truth.

# Tone and style
Be concise, direct, and grounded — the user reads your replies in a terminal pane. Skip preamble ("Great question!", "Sure, I can help") and filler. Lead with the result. Don't restate the user's request back to them or narrate routine tool calls ("Now I'll read the chapter"); just do the work and report what you found or changed. Match the user's language (reply in Thai if they write Thai). When you finish, give a short summary of what changed (which chapters/files, what edits) — a couple of sentences or a tight bullet list, not an essay. Reference chapters as `vN/cM`.

Your replies are rendered as Markdown, so format them for a terminal: short paragraphs, `-` bullet lists for multiple points, **bold** for the key takeaway, `inline code` for terms/filenames/Thai snippets you are discussing, and fenced code blocks for any longer before/after text. Don't paste an entire chapter back to the user; quote only the lines that changed.

# Autonomy and persistence
Keep working until the user's request is fully resolved before yielding the turn. Do not stop at a plan or ask for confirmation when the next step is obvious and reversible — gather the context you need and make the change. Only stop early to ask the user when you are genuinely blocked: the request is ambiguous in a way that changes the outcome, or an action is destructive and irreversible. Don't guess about file contents — read them. If you say you will do something, do it in the same turn.

# Workflow
1. Understand the request and its scope. The user may tag targets with `@` (e.g. `@v1/c3`, `@glossary`, `@characters`, `@style`, `@recap`); a scope hint is appended to their message. Tools without an explicit `vol` act on the active volume.
2. Gather context BEFORE editing. Use list_volumes / list_chapters to learn structure, read_chapter to see real text, grep_chapter to locate exact strings, search_project to find a term across chapters, read_lexicon and read_meta to check established names/terminology/style, and list_flagged_chunks to find spots that failed QA ([REVIEW NEEDED]). Issue independent read-only calls together rather than one at a time.
3. For any multi-step or multi-chapter task, call update_plan to lay out the steps, then keep it current: mark exactly one step in_progress as you work it, flip it to completed when done, and add steps you discover. The plan is a persistent TODO list that survives across turns — at the start of a turn, resume from any unfinished steps rather than restarting, and call update_plan with an empty list once the whole task is finished. Skip the plan for a single trivial edit.
4. For a large, self-contained, or parallelizable chunk of work (e.g. "normalize every honorific across volume 2", "rewrite the action scenes in chapters 5–9"), delegate it with the `task` tool — a focused sub-agent runs it to completion with the same tools and reports back, keeping this thread clean. Do the work inline for anything small.
5. Make changes surgically. Prefer the smallest edit that fixes the issue:
   - edit_chapter for a targeted wording fix (exact unique match; the safe default).
   - multi_edit_chapter to apply several fixes to one chapter atomically in a single call.
   - replace_across_project to standardize a name/term in EVERY chapter — run it with dry_run first to preview the blast radius, then for real, and also update the matching glossary/character entry.
   - replace_chapter_text only for a full rewrite you have actually produced.
   - retranslate_chapter / refine_chapter_with_feedback to regenerate a chapter through the Translator→Reviewer pipeline (heavier; for "redo this chapter properly").
   Chapter-text edits archive the prior version automatically and are reversible (the user can /undo or view the Reader diff).
5. Verify after editing — re-read or grep the changed region to confirm the edit landed and reads naturally; fix it if not.
6. When a choice is genuinely the user's to make and you can't resolve it from the project or a sensible default (e.g. two equally valid Thai renderings, or confirming a risky bulk change), use the ask_user tool to put it to them — with options for a pick, or open-ended for free text. Otherwise act on a reasonable default and say what you chose.

# Editing discipline
- read_chapter shows Thai with `N│ ` line-number prefixes. NEVER include that prefix in edit_chapter's `old`/`new`; pass only the real text.
- edit_chapter requires `old` to match the file EXACTLY (including punctuation and spacing) and be UNIQUE. If it is not unique, pass a longer surrounding snippet, or set replace_all to change every occurrence. Always work from text you have actually read, never from memory.
- Keep Thai natural, idiomatic, and consistent with the established glossary policies and each character's voice/pronouns. Preserve scene-break dividers (`---`), image links, and Markdown. Don't introduce raw Japanese kana or `(furigana)` parentheticals.
- When you fix a term or name across the project, also update the glossary/character entry so future translation stays consistent — and use search_project to catch every occurrence.

# Safety
Mature source material may appear; handle characters, terms, and text neutrally and faithfully without moralizing or censoring. Do not invent facts about the story that the source/translation does not support."#
        .to_string()
}

fn refine_tools_vec() -> Vec<Tool> {
    serde_json::from_value(refine_tools_schema()).unwrap_or_default()
}

/// Owns the live chat thread so multi-turn history persists.
pub async fn run_refine_agent(ctx: RefineCtx, mut rx: UnboundedReceiver<RefineControl>) {
    let tools = RefineTools::with_agent(
        ctx.root.clone(),
        ctx.default_vol,
        ctx.tx.clone(),
        ctx.client.clone(),
        ctx.model.clone(),
        ctx.interact.clone(),
    );
    let mut req = ChatRequest::new(
        ctx.model.model.clone(),
        vec![Message::system(refine_system_prompt())],
    );
    req.tools = Some(refine_tools_vec());
    // Keep configured effort; otherwise ask reasoning models to stream thinking.
    req.reasoning = ctx
        .model
        .reasoning_param()
        .or_else(|| Some(json!({"enabled": true})));
    let mut current_id = ctx.session_id.clone();
    req.messages.extend(seed_messages(&ctx.root, &current_id));

    while let Some(ctrl) = rx.recv().await {
        match ctrl {
            RefineControl::Submit(turn) => {
                ctx.cancel.store(false, Ordering::Relaxed);
                let mut msg = build_user_message(&ctx, &turn);
                msg.push_str(approval_directive(ctx.interact.mode()));
                req.messages.push(Message::user(msg));
                run_refine_turn(ctx.client.as_ref(), &mut req, &tools, &ctx.tx, &ctx.cancel).await;
                // Persist through App, tagged so stale session updates are dropped.
                ctx.tx.send(AppEvent::RefineThreadUpdated {
                    session: current_id.clone(),
                    messages: req.messages[1..].to_vec(),
                });
            }
            RefineControl::Clear => {
                req.messages.truncate(1); // keep the system turn
                ctx.cancel.store(true, Ordering::Relaxed);
                ctx.tx.send(AppEvent::RefineThreadUpdated {
                    session: current_id.clone(),
                    messages: vec![],
                });
            }
            RefineControl::SetModel(m) => req.model = m,
            RefineControl::SwitchSession(id) => {
                ctx.cancel.store(true, Ordering::Relaxed);
                current_id = id.clone();
                req.messages.truncate(1);
                req.messages.extend(seed_messages(&ctx.root, &id));
            }
            RefineControl::Compact => {
                maybe_compact(&mut req, &ctx.tx, true);
                ctx.tx.send(AppEvent::RefineThreadUpdated {
                    session: current_id.clone(),
                    messages: req.messages[1..].to_vec(),
                });
            }
            RefineControl::Shutdown => break,
        }
    }
}

/// Streams one user turn and executes tool calls until the model stops.
async fn run_refine_turn(
    client: &dyn LlmClient,
    req: &mut ChatRequest,
    tools: &dyn ToolExecutor,
    tx: &EventTx,
    cancel: &AtomicBool,
) {
    for _round in 0..MAX_TOOL_ROUNDS {
        if cancel.load(Ordering::Relaxed) {
            tx.send(AppEvent::RefineMessageDone);
            return;
        }

        maybe_compact(req, tx, false);

        let mut on_delta = |d: crate::llm::StreamDelta| match d {
            crate::llm::StreamDelta::Content(s) => tx.send(AppEvent::RefineDelta {
                delta: s.to_string(),
            }),
            crate::llm::StreamDelta::Reasoning(s) => tx.send(AppEvent::RefineReasoning {
                delta: s.to_string(),
            }),
        };
        let resp = match client.chat_stream(req, &mut on_delta).await {
            Ok(r) => r,
            Err(e) => {
                tx.send(AppEvent::RefineError { msg: e.to_string() });
                return;
            }
        };

        if let Some(u) = resp.usage {
            tx.send(AppEvent::RefineUsage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
            });
        }

        let Some(choice) = resp.choices.first() else {
            tx.send(AppEvent::RefineError {
                msg: "the model returned no choices".to_string(),
            });
            return;
        };
        let content = choice.message.content.clone();
        let tool_calls = choice.message.tool_calls.clone().unwrap_or_default();

        if tool_calls.is_empty() {
            if let Some(c) = content.filter(|c| !c.is_empty()) {
                req.messages.push(Message::assistant(c));
            }
            tx.send(AppEvent::RefineMessageDone);
            return;
        }

        // Assistant tool-call turn (content stays null on a tool turn).
        req.messages.push(Message {
            role: Role::Assistant,
            content,
            tool_calls: Some(tool_calls.clone()),
            tool_call_id: None,
            name: None,
        });

        for call in &tool_calls {
            // Plan calls render in the pinned panel, not the transcript.
            if call.function.name != "update_plan" {
                tx.send(AppEvent::RefineToolInvoked {
                    tool: call.function.name.clone(),
                    summary: summarize_args(&call.function.arguments),
                });
            }
            let result = match tools
                .execute(&call.function.name, &call.function.arguments)
                .await
            {
                Ok(p) => p,
                Err(e) => json!({
                    "ok": false,
                    "message": format!("tool '{}' failed: {e}", call.function.name)
                })
                .to_string(),
            };
            req.messages
                .push(Message::tool_result(call.id.clone(), result));
        }
    }

    tx.send(AppEvent::RefineError {
        msg: format!("stopped after {MAX_TOOL_ROUNDS} tool rounds"),
    });
    tx.send(AppEvent::RefineMessageDone);
}

fn summarize_args(args_json: &str) -> String {
    let flat: String = args_json.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 70 {
        flat
    } else {
        flat.chars().take(70).collect::<String>() + "…"
    }
}

/// Per-turn edit-approval instruction; injected because mode can change live.
fn approval_directive(mode: ApprovalMode) -> &'static str {
    match mode {
        ApprovalMode::Auto => "\n\n[Approval mode: AUTO — apply routine, clearly-wanted fixes directly, but call the ask_user tool to confirm BEFORE any edit the user might not consent to: risky, bulky, subjective, or destructive changes (e.g. rewriting many chapters, changing a character's name everywhere, deleting content).]",
        ApprovalMode::Ask => "\n\n[Approval mode: ASK — the app pauses every edit for the user to accept or reject, so just proceed and make the edits normally.]",
        ApprovalMode::Always => "\n\n[Approval mode: ALWAYS-APPROVE — apply edits directly and do not ask for approval.]",
    }
}

fn build_user_message(ctx: &RefineCtx, turn: &UserTurn) -> String {
    let scope = parse_scope(&turn.text);
    if scope.is_empty() {
        return turn.text.clone();
    }
    let hint = scope
        .iter()
        .map(describe_target)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{}\n\n(In scope: {hint}. Active volume: {}. Use your tools to read before editing.)",
        turn.text, ctx.default_vol
    )
}

fn describe_target(t: &MentionTarget) -> String {
    match t {
        MentionTarget::Volume { vol } => format!("volume {vol}"),
        MentionTarget::Chapter { vol, ch } => format!("volume {vol} chapter {ch}"),
        MentionTarget::Lexicon => "the lexicon".into(),
        MentionTarget::Characters => "the character roster".into(),
        MentionTarget::Glossary => "the glossary".into(),
        MentionTarget::Style => "the style guide".into(),
        MentionTarget::Project => "the project metadata".into(),
        MentionTarget::Recap => "the recap/synopsis".into(),
    }
}

#[derive(Deserialize)]
struct VolArg {
    #[serde(default)]
    vol: Option<u32>,
}
#[derive(Deserialize)]
struct ReadChapterArgs {
    #[serde(default)]
    vol: Option<u32>,
    ch: u32,
    #[serde(default = "default_true")]
    include_jp: bool,
    #[serde(default = "default_true")]
    include_th: bool,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}
#[derive(Deserialize)]
struct GrepChapterArgs {
    #[serde(default)]
    vol: Option<u32>,
    ch: u32,
    query: String,
    #[serde(default)]
    side: Option<String>,
    #[serde(default = "default_true")]
    ignore_case: bool,
}
#[derive(Deserialize)]
struct ReadMetaArgs {
    #[serde(default)]
    vol: Option<u32>,
    #[serde(default)]
    kind: Option<String>,
}
#[derive(Deserialize)]
struct UpdatePlanArgs {
    steps: Vec<PlanStep>,
}
#[derive(Deserialize)]
struct ReadLexiconArgs {
    #[serde(default)]
    vol: Option<u32>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}
#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    #[serde(default)]
    vol: Option<u32>,
}
#[derive(Deserialize)]
struct UpsertCharacterArgs {
    #[serde(default)]
    vol: Option<u32>,
    #[serde(default)]
    id: Option<String>,
    jp_name: String,
    thai_name: String,
    #[serde(default)]
    romaji: Option<String>,
    #[serde(default)]
    gender: Option<String>,
    #[serde(default)]
    honorific: Option<String>,
    #[serde(default)]
    speech_style: Option<String>,
    #[serde(default)]
    relationships: Vec<Relationship>,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    first_seen_chapter: Option<u32>,
}
#[derive(Deserialize)]
struct MergeCharacterArgs {
    #[serde(default)]
    vol: Option<u32>,
    from_id: String,
    into_id: String,
}
#[derive(Deserialize)]
struct RemoveCharacterArgs {
    #[serde(default)]
    vol: Option<u32>,
    id: String,
}
#[derive(Deserialize)]
struct UpsertGlossaryArgs {
    #[serde(default)]
    vol: Option<u32>,
    jp_term: String,
    thai_term: String,
    #[serde(default)]
    romaji: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    gloss: Option<String>,
    #[serde(default)]
    policy: Option<TermPolicy>,
    #[serde(default)]
    forbidden_thai: Vec<String>,
    #[serde(default)]
    context_rule: Option<String>,
    #[serde(default)]
    do_not_translate: Option<bool>,
    #[serde(default)]
    first_seen_chapter: Option<u32>,
}
#[derive(Deserialize)]
struct RemoveGlossaryArgs {
    #[serde(default)]
    vol: Option<u32>,
    jp_term: String,
}
#[derive(Deserialize)]
struct SetRecapArgs {
    #[serde(default)]
    vol: Option<u32>,
    recap: String,
}
#[derive(Deserialize)]
struct SetChapterSummaryArgs {
    #[serde(default)]
    vol: Option<u32>,
    chapter: u32,
    summary: String,
}
#[derive(Deserialize)]
struct SetSynopsisArgs {
    #[serde(default)]
    vol: Option<u32>,
    #[serde(default)]
    raw: Option<String>,
    thai: String,
}
#[derive(Deserialize)]
struct StyleNoteArgs {
    #[serde(default)]
    vol: Option<u32>,
    note: String,
}
#[derive(Deserialize)]
struct StyleExampleArgs {
    #[serde(default)]
    vol: Option<u32>,
    jp: String,
    th: String,
    #[serde(default)]
    note: Option<String>,
}
#[derive(Deserialize)]
struct ReplaceChapterArgs {
    #[serde(default)]
    vol: Option<u32>,
    ch: u32,
    new_text: String,
}
#[derive(Deserialize)]
struct EditChapterArgs {
    #[serde(default)]
    vol: Option<u32>,
    ch: u32,
    old: String,
    new: String,
    #[serde(default)]
    replace_all: bool,
}
#[derive(Deserialize)]
struct EditOp {
    old: String,
    new: String,
    #[serde(default)]
    replace_all: bool,
}
#[derive(Deserialize)]
struct MultiEditArgs {
    #[serde(default)]
    vol: Option<u32>,
    ch: u32,
    edits: Vec<EditOp>,
}
#[derive(Deserialize)]
struct ReplaceAcrossArgs {
    #[serde(default)]
    vol: Option<u32>,
    find: String,
    replace: String,
    #[serde(default)]
    dry_run: bool,
}
#[derive(Deserialize)]
struct ListFlaggedArgs {
    #[serde(default)]
    vol: Option<u32>,
    #[serde(default)]
    ch: Option<u32>,
}
#[derive(Deserialize)]
struct AddNoteArgs {
    #[serde(default)]
    vol: Option<u32>,
    severity: String,
    note: String,
    #[serde(default)]
    chapter: Option<u32>,
    #[serde(default)]
    kind: Option<String>,
}
#[derive(Deserialize)]
struct RetranslateArgs {
    #[serde(default)]
    vol: Option<u32>,
    chapters: Vec<u32>,
}
#[derive(Deserialize)]
struct RefineChapterArgs {
    #[serde(default)]
    vol: Option<u32>,
    ch: u32,
    feedback: String,
}

fn default_true() -> bool {
    true
}

fn slugify(name: &str) -> String {
    crate::workspace::slugify(name)
}

fn cap_to(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…(truncated)", &s[..end])
}

fn cap(s: &str) -> String {
    cap_to(s, READ_CAP)
}

const DEFAULT_LINE_LIMIT: usize = 400;

/// Return a capped, 1-based line window with the `N│ ` prefix the model must strip.
/// Tuple is `(numbered_text, total_lines, returned_to_line)`.
fn numbered_window(text: &str, offset: usize, limit: usize) -> (String, usize, usize) {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let start = offset.saturating_sub(1).min(total);
    let end = start.saturating_add(limit).min(total);
    let width = (end).max(1).to_string().len();
    let mut out = String::new();
    for (i, line) in lines[start..end].iter().enumerate() {
        let n = start + i + 1;
        out.push_str(&format!("{n:>width$}│ {line}\n"));
    }
    (cap(out.trim_end_matches('\n')), total, end)
}

enum EditError {
    NotFound,
    Ambiguous(usize),
    Other(String),
}

impl EditError {
    fn describe(&self) -> String {
        match self {
            EditError::NotFound => "no exact match found".to_string(),
            EditError::Ambiguous(n) => {
                format!("`old` matches {n} places — make it unique or set replace_all")
            }
            EditError::Other(msg) => msg.clone(),
        }
    }
}

/// Exact edit helper: non-empty, changed `old`; unique unless `replace_all`.
fn apply_edit(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<(String, usize), EditError> {
    if old.is_empty() {
        return Err(EditError::Other("`old` is empty".to_string()));
    }
    if old == new {
        return Err(EditError::Other("`old` and `new` are identical".to_string()));
    }
    let count = content.matches(old).count();
    if count == 0 {
        return Err(EditError::NotFound);
    }
    if count > 1 && !replace_all {
        return Err(EditError::Ambiguous(count));
    }
    let updated = if replace_all {
        content.replace(old, new)
    } else {
        content.replacen(old, new, 1)
    };
    Ok((updated, if replace_all { count } else { 1 }))
}

/// Archives current chapter text so `/undo` and Reader diff can recover it.
fn archive_chapter(ws: &Workspace, chapter: u32) -> std::io::Result<()> {
    let path = ws.translated(chapter);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) if !c.trim().is_empty() => c,
        _ => return Ok(()),
    };
    let dir = ws.reruns_dir(chapter);
    std::fs::create_dir_all(&dir)?;
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let fname = format!("{stamp}.md");
    std::fs::write(dir.join(&fname), &content)?;
    let rel = format!("reruns/ch_{chapter:03}/{fname}");

    let review_needed = translation::review_needed_chunk_indices_in(&content).len() as u32;
    let committed = translation::committed_chunk_indices_in(&content).len() as u32;
    let total = translation::total_chunks_in(&content).unwrap_or(committed);
    let finished_at = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .map(chrono::DateTime::<chrono::Utc>::from)
        .unwrap_or_else(|_| chrono::Utc::now());

    volume::archive_prev_version(
        ws,
        chapter,
        &rel,
        volume::PriorVersion {
            finished_at,
            review_needed,
            failed: false,
            total_chunks: total,
            committed_chunks: committed,
        },
    )
}

pub async fn dispatch_refine_tool(
    root: &Path,
    default_vol: u32,
    tx: &EventTx,
    name: &str,
    args_json: &str,
) -> ToolResult {
    macro_rules! parse {
        ($t:ty) => {
            match serde_json::from_str::<$t>(args_json) {
                Ok(a) => a,
                Err(e) => return ToolResult::err(format!("invalid {name} args: {e}")),
            }
        };
    }
    let ws = |vol: Option<u32>| Workspace::new(root.to_path_buf(), vol.unwrap_or(default_vol));

    match name {
        "list_volumes" => {
            let _ = parse!(VolArg); // tolerate {} or stray vol
            let Some(project) = crate::workspace::scan::scan_one_project(root) else {
                return ToolResult::err("could not scan the project");
            };
            let vols: Vec<_> = project
                .volumes
                .iter()
                .map(|v| {
                    json!({
                        "vol": v.number,
                        "label": v.label,
                        "chapters": v.chapters.len(),
                    })
                })
                .collect();
            ToolResult::data(
                format!("{} volume(s)", vols.len()),
                json!({ "volumes": vols }),
            )
        }

        "list_chapters" => {
            let a = parse!(VolArg);
            let vol = a.vol.unwrap_or(default_vol);
            let Some(project) = crate::workspace::scan::scan_one_project(root) else {
                return ToolResult::err("could not scan the project");
            };
            let Some(v) = project.volumes.iter().find(|v| v.number == vol) else {
                return ToolResult::err(format!("volume {vol} not found"));
            };
            let chapters: Vec<_> = v
                .chapters
                .iter()
                .map(|c| {
                    json!({
                        "ch": c.number,
                        "title": c.title,
                        "kind": format!("{:?}", c.kind),
                        "status": format!("{:?}", c.status),
                        "committed_chunks": c.committed_chunks,
                        "total_chunks": c.total_chunks,
                    })
                })
                .collect();
            ToolResult::data(
                format!("vol {vol}: {} chapter(s)", chapters.len()),
                json!({ "vol": vol, "chapters": chapters }),
            )
        }

        "read_chapter" => {
            let a = parse!(ReadChapterArgs);
            let w = ws(a.vol);
            let offset = a.offset.unwrap_or(1).max(1);
            let limit = a.limit.unwrap_or(DEFAULT_LINE_LIMIT).clamp(1, 4000);
            let mut data = serde_json::Map::new();
            if a.include_jp {
                let jp = std::fs::read_to_string(w.raw(a.ch)).unwrap_or_default();
                data.insert("japanese".into(), json!(cap(&jp)));
            }
            if a.include_th {
                let th = translation::prose_only(&translation::read_translated(&w, a.ch).await);
                let (numbered, total, to) = numbered_window(&th, offset, limit);
                data.insert("thai".into(), json!(numbered));
                data.insert("thai_total_lines".into(), json!(total));
                data.insert("thai_from_line".into(), json!(offset.min(total.max(1))));
                data.insert("thai_to_line".into(), json!(to));
                data.insert("thai_truncated".into(), json!(to < total));
            }
            ToolResult::data(
                format!(
                    "read chapter {} of vol {}",
                    a.ch,
                    a.vol.unwrap_or(default_vol)
                ),
                serde_json::Value::Object(data),
            )
        }

        "grep_chapter" => {
            let a = parse!(GrepChapterArgs);
            let w = ws(a.vol);
            if a.query.is_empty() {
                return ToolResult::err("empty query");
            }
            let side = a.side.as_deref().unwrap_or("th");
            let needle = if a.ignore_case {
                a.query.to_lowercase()
            } else {
                a.query.clone()
            };
            let matches_line = |line: &str| {
                if a.ignore_case {
                    line.to_lowercase().contains(&needle)
                } else {
                    line.contains(&needle)
                }
            };
            let grep = |text: &str| -> Vec<serde_json::Value> {
                text.lines()
                    .enumerate()
                    .filter(|(_, l)| matches_line(l))
                    .take(50)
                    .map(|(i, l)| json!({"line": i + 1, "text": l}))
                    .collect()
            };
            let mut data = serde_json::Map::new();
            if matches!(side, "th" | "both") {
                let th = translation::prose_only(&translation::read_translated(&w, a.ch).await);
                data.insert("thai".into(), json!(grep(&th)));
            }
            if matches!(side, "jp" | "both") {
                let jp = std::fs::read_to_string(w.raw(a.ch)).unwrap_or_default();
                data.insert("japanese".into(), json!(grep(&jp)));
            }
            ToolResult::data(format!("grep chapter {}", a.ch), serde_json::Value::Object(data))
        }

        "read_meta" => {
            let a = parse!(ReadMetaArgs);
            let w = ws(a.vol);
            let kind = a.kind.as_deref().unwrap_or("all");
            let vd = volume::load(&w);
            let mut data = serde_json::Map::new();
            if matches!(kind, "all" | "style") {
                let style_md = std::fs::read_to_string(w.style_md()).unwrap_or_default();
                let notes = match style_md.find("<!-- honya:data") {
                    Some(i) => style_md[..i].trim().to_string(),
                    None => style_md.trim().to_string(),
                };
                data.insert("style_notes".into(), json!(cap(&notes)));
                data.insert("style_examples".into(), json!(vd.style_examples));
            }
            if matches!(kind, "all" | "recap") {
                data.insert("recap".into(), json!(vd.running_recap));
            }
            if matches!(kind, "all" | "synopsis") {
                data.insert("synopsis_raw".into(), json!(vd.synopsis_raw));
                data.insert("synopsis_th".into(), json!(vd.synopsis_th));
            }
            if matches!(kind, "all" | "summaries") {
                data.insert("chapter_summaries".into(), json!(vd.chapters));
            }
            if matches!(kind, "all" | "notes") {
                data.insert("continuity_notes".into(), json!(vd.notes));
            }
            if matches!(kind, "all" | "project") {
                let title = crate::workspace::scan::scan_one_project(root)
                    .map(|p| p.title)
                    .unwrap_or_default();
                data.insert("project_title".into(), json!(title));
            }
            ToolResult::data("metadata", serde_json::Value::Object(data))
        }

        "update_plan" => {
            let a = parse!(UpdatePlanArgs);
            let steps: Vec<PlanStep> = a
                .steps
                .into_iter()
                .filter(|s| !s.step.trim().is_empty())
                .take(20)
                .collect();
            if steps.is_empty() {
                return ToolResult::err("no plan steps given");
            }
            tx.send(AppEvent::RefinePlanUpdated {
                steps: steps.clone(),
            });
            let rendered = steps
                .iter()
                .map(|s| {
                    let mark = match s.status {
                        crate::model::PlanStepStatus::Completed => "[x]",
                        crate::model::PlanStepStatus::InProgress => "[~]",
                        crate::model::PlanStepStatus::Pending => "[ ]",
                    };
                    format!("{mark} {}", s.step.trim())
                })
                .collect::<Vec<_>>()
                .join("\n");
            ToolResult::ok(format!("plan updated:\n{rendered}"))
        }

        "read_lexicon" => {
            let a = parse!(ReadLexiconArgs);
            let w = ws(a.vol);
            let kind = a.kind.as_deref().unwrap_or("both");
            let q = a.query.as_deref();
            let mut data = serde_json::Map::new();
            if matches!(kind, "characters" | "both") {
                let chars = characters::get(&w, q, None);
                data.insert("characters".into(), json!(chars));
            }
            if matches!(kind, "glossary" | "both") {
                let terms = glossary::get(&w, q, None, None, false, 0);
                data.insert("glossary".into(), json!(terms));
            }
            ToolResult::data("lexicon", serde_json::Value::Object(data))
        }

        "search_project" => {
            let a = parse!(SearchArgs);
            let Some(project) = crate::workspace::scan::scan_one_project(root) else {
                return ToolResult::err("could not scan the project");
            };
            let needle = a.query.to_lowercase();
            let mut hits = Vec::new();
            'outer: for v in &project.volumes {
                if let Some(only) = a.vol
                    && v.number != only
                {
                    continue;
                }
                let w = Workspace::new(root.to_path_buf(), v.number);
                for c in &v.chapters {
                    let th =
                        translation::prose_only(&translation::read_translated(&w, c.number).await);
                    // Find AND slice on the SAME (lowercased) string: a byte index
                    // from the lowercased text is not a valid boundary in the original
                    // when lowercasing changes a char's byte length (would panic).
                    let th_low = th.to_lowercase();
                    if let Some(pos) = th_low.find(&needle) {
                        let start = th_low[..pos]
                            .char_indices()
                            .rev()
                            .nth(40)
                            .map(|(i, _)| i)
                            .unwrap_or(0);
                        let snippet: String = th_low[start..].chars().take(100).collect();
                        hits.push(json!({"vol": v.number, "ch": c.number, "snippet": snippet}));
                        if hits.len() >= 30 {
                            break 'outer;
                        }
                    }
                }
            }
            ToolResult::data(format!("{} match(es)", hits.len()), json!({ "hits": hits }))
        }

        "upsert_character" => {
            let a = parse!(UpsertCharacterArgs);
            let w = ws(a.vol);
            let id = a.id.clone().unwrap_or_else(|| slugify(&a.jp_name));
            let character = Character {
                id: id.clone(),
                jp_name: a.jp_name.clone(),
                thai_name: a.thai_name.clone(),
                romaji: a.romaji,
                gender: a.gender,
                honorific: a.honorific,
                speech_style: a.speech_style,
                relationships: a.relationships,
                aliases: a.aliases,
                notes: a.notes,
                first_seen_chapter: a.first_seen_chapter,
            };
            match characters::upsert(&w, character) {
                Ok(outcome) => {
                    emit_edit(tx, "character", &format!("{} → {}", a.jp_name, a.thai_name));
                    let msg = match outcome {
                        characters::CharacterUpsertOutcome::Merged { into_id } => {
                            format!("merged {} into {into_id}", a.thai_name)
                        }
                        characters::CharacterUpsertOutcome::InsertedWithCandidates {
                            id,
                            candidates,
                        } => format!(
                            "saved {} ({id}); possible duplicates: {}",
                            a.thai_name,
                            candidates.join(", ")
                        ),
                        characters::CharacterUpsertOutcome::Inserted => {
                            format!("saved character {} ({id})", a.thai_name)
                        }
                    };
                    ToolResult::ok(msg)
                }
                Err(e) => ToolResult::err(format!("failed to write character: {e}")),
            }
        }

        "merge_character" => {
            let a = parse!(MergeCharacterArgs);
            let w = ws(a.vol);
            match characters::merge(&w, &a.from_id, &a.into_id) {
                Ok(true) => {
                    emit_edit(
                        tx,
                        "character",
                        &format!("merged {} into {}", a.from_id, a.into_id),
                    );
                    ToolResult::ok(format!("merged {} into {}", a.from_id, a.into_id))
                }
                Ok(false) => ToolResult::ok("nothing to merge".to_string()),
                Err(e) => ToolResult::err(format!("merge failed: {e}")),
            }
        }

        "remove_character" => {
            let a = parse!(RemoveCharacterArgs);
            let w = ws(a.vol);
            match characters::remove(&w, &a.id) {
                Ok(()) => {
                    emit_edit(tx, "character", &format!("removed {}", a.id));
                    ToolResult::ok(format!("removed character {}", a.id))
                }
                Err(e) => ToolResult::err(format!("remove failed: {e}")),
            }
        }

        "upsert_glossary_term" => {
            let a = parse!(UpsertGlossaryArgs);
            let w = ws(a.vol);
            let term = GlossaryTerm {
                jp_term: a.jp_term.clone(),
                thai_term: a.thai_term.clone(),
                romaji: a.romaji,
                category: a.category,
                gloss: a.gloss,
                policy: a.policy,
                forbidden_thai: a.forbidden_thai,
                context_rule: a.context_rule,
                protected: None,
                do_not_translate: a.do_not_translate,
                first_seen_chapter: a.first_seen_chapter,
            };
            match glossary::upsert(&w, term) {
                Ok(()) => {
                    emit_edit(tx, "glossary", &format!("{} → {}", a.jp_term, a.thai_term));
                    ToolResult::ok(format!("saved term {} → {}", a.jp_term, a.thai_term))
                }
                Err(e) => ToolResult::err(format!("failed to write term: {e}")),
            }
        }

        "remove_glossary_term" => {
            let a = parse!(RemoveGlossaryArgs);
            let w = ws(a.vol);
            match glossary::remove(&w, &a.jp_term) {
                Ok(()) => {
                    emit_edit(tx, "glossary", &format!("removed {}", a.jp_term));
                    ToolResult::ok(format!("removed term {}", a.jp_term))
                }
                Err(e) => ToolResult::err(format!("remove failed: {e}")),
            }
        }

        "set_recap" => {
            let a = parse!(SetRecapArgs);
            let w = ws(a.vol);
            match volume::set_recap(&w, &a.recap) {
                Ok(()) => {
                    emit_edit(tx, "recap", "updated running recap");
                    ToolResult::ok("recap updated".to_string())
                }
                Err(e) => ToolResult::err(format!("set_recap failed: {e}")),
            }
        }

        "set_chapter_summary" => {
            let a = parse!(SetChapterSummaryArgs);
            let w = ws(a.vol);
            match volume::set_chapter_summary(&w, a.chapter, &a.summary) {
                Ok(()) => {
                    emit_edit(tx, "summary", &format!("chapter {}", a.chapter));
                    ToolResult::ok(format!("summary set for chapter {}", a.chapter))
                }
                Err(e) => ToolResult::err(format!("set_chapter_summary failed: {e}")),
            }
        }

        "set_synopsis" => {
            let a = parse!(SetSynopsisArgs);
            let w = ws(a.vol);
            let raw = a.raw.unwrap_or_else(|| volume::load(&w).synopsis_raw);
            match volume::set_synopsis(&w, &raw, &a.thai) {
                Ok(()) => {
                    emit_edit(tx, "synopsis", "updated volume synopsis");
                    ToolResult::ok("synopsis updated".to_string())
                }
                Err(e) => ToolResult::err(format!("set_synopsis failed: {e}")),
            }
        }

        "append_style_note" => {
            let a = parse!(StyleNoteArgs);
            let w = ws(a.vol);
            match style::append_note(&w, &a.note) {
                Ok(()) => {
                    emit_edit(tx, "style", "appended style note");
                    ToolResult::ok("style note added".to_string())
                }
                Err(e) => ToolResult::err(format!("append_style_note failed: {e}")),
            }
        }

        "add_style_example" => {
            let a = parse!(StyleExampleArgs);
            let w = ws(a.vol);
            let ex = StyleExample {
                jp: a.jp,
                th: a.th,
                note: a.note,
            };
            match volume::add_style_examples(&w, vec![ex]) {
                Ok(()) => {
                    emit_edit(tx, "style", "added style example");
                    ToolResult::ok("style example added".to_string())
                }
                Err(e) => ToolResult::err(format!("add_style_example failed: {e}")),
            }
        }

        "add_continuity_note" => {
            let a = parse!(AddNoteArgs);
            let w = ws(a.vol);
            let note = ContinuityNote {
                chapter: a.chapter,
                severity: a.severity.clone(),
                kind: a.kind.clone(),
                note: a.note.clone(),
            };
            match volume::add_note(&w, note) {
                Ok(()) => {
                    emit_edit(tx, "continuity", &format!("{} note", a.severity));
                    ToolResult::ok("continuity note recorded".to_string())
                }
                Err(e) => ToolResult::err(format!("add_continuity_note failed: {e}")),
            }
        }

        "replace_chapter_text" => {
            let a = parse!(ReplaceChapterArgs);
            let w = ws(a.vol);
            if let Err(e) = archive_chapter(&w, a.ch) {
                return ToolResult::err(format!("could not archive prior version: {e}"));
            }
            if let Err(e) = translation::reset_chapter(&w, a.ch) {
                return ToolResult::err(format!("reset failed: {e}"));
            }
            if let Err(e) = translation::record_total_chunks(&w, a.ch, 1).await {
                return ToolResult::err(format!("write failed: {e}"));
            }
            if let Err(e) = translation::append_chunk(&w, a.ch, 0, a.new_text.trim()).await {
                return ToolResult::err(format!("write failed: {e}"));
            }
            emit_edit(tx, "chapter text", &format!("replaced chapter {}", a.ch));
            tx.send(AppEvent::RefineChapterEdited {
                vol: a.vol.unwrap_or(default_vol),
                ch: a.ch,
            });
            ToolResult::ok(format!(
                "replaced chapter {} (prior version archived)",
                a.ch
            ))
        }

        "edit_chapter" => {
            let a = parse!(EditChapterArgs);
            let w = ws(a.vol);
            let path = w.translated(a.ch);
            let content = translation::read_translated(&w, a.ch).await;
            if content.is_empty() {
                return ToolResult::err(format!("chapter {} has no translation yet", a.ch));
            }
            let (updated, count) = match apply_edit(&content, &a.old, &a.new, a.replace_all) {
                Ok(r) => r,
                Err(EditError::NotFound) => {
                    return ToolResult::err(format!(
                        "could not find that exact text in chapter {}. Re-read the chapter (use grep_chapter to locate it) and pass an exact substring without the `N│ ` prefix.",
                        a.ch
                    ));
                }
                Err(EditError::Ambiguous(n)) => {
                    return ToolResult::err(format!(
                        "`old` matches {n} places in chapter {}; pass a longer unique snippet or set replace_all=true.",
                        a.ch
                    ));
                }
                Err(EditError::Other(msg)) => return ToolResult::err(msg),
            };
            if let Err(e) = archive_chapter(&w, a.ch) {
                return ToolResult::err(format!("could not archive prior version: {e}"));
            }
            if let Err(e) = tokio::fs::write(&path, updated).await {
                return ToolResult::err(format!("write failed: {e}"));
            }
            emit_edit(
                tx,
                "chapter text",
                &format!("chapter {}: {count} edit(s)", a.ch),
            );
            tx.send(AppEvent::RefineChapterEdited {
                vol: a.vol.unwrap_or(default_vol),
                ch: a.ch,
            });
            ToolResult::ok(format!(
                "edited chapter {}: {count} replacement(s)\n{}",
                a.ch,
                edit_diff_snippet(&[(&a.old, &a.new)])
            ))
        }

        "multi_edit_chapter" => {
            let a = parse!(MultiEditArgs);
            let w = ws(a.vol);
            if a.edits.is_empty() {
                return ToolResult::err("no edits given");
            }
            let path = w.translated(a.ch);
            let mut content = translation::read_translated(&w, a.ch).await;
            if content.is_empty() {
                return ToolResult::err(format!("chapter {} has no translation yet", a.ch));
            }
            let mut total = 0usize;
            for (i, e) in a.edits.iter().enumerate() {
                match apply_edit(&content, &e.old, &e.new, e.replace_all) {
                    Ok((updated, n)) => {
                        content = updated;
                        total += n;
                    }
                    Err(err) => {
                        return ToolResult::err(format!(
                            "edit #{} failed ({}); nothing was written",
                            i + 1,
                            err.describe()
                        ));
                    }
                }
            }
            if let Err(e) = archive_chapter(&w, a.ch) {
                return ToolResult::err(format!("could not archive prior version: {e}"));
            }
            if let Err(e) = tokio::fs::write(&path, content).await {
                return ToolResult::err(format!("write failed: {e}"));
            }
            emit_edit(
                tx,
                "chapter text",
                &format!("chapter {}: {} edit(s)", a.ch, a.edits.len()),
            );
            tx.send(AppEvent::RefineChapterEdited {
                vol: a.vol.unwrap_or(default_vol),
                ch: a.ch,
            });
            let pairs: Vec<(&str, &str)> = a
                .edits
                .iter()
                .map(|e| (e.old.as_str(), e.new.as_str()))
                .collect();
            ToolResult::ok(format!(
                "applied {} edit(s) ({total} replacement(s)) to chapter {}\n{}",
                a.edits.len(),
                a.ch,
                edit_diff_snippet(&pairs)
            ))
        }

        "replace_across_project" => {
            let a = parse!(ReplaceAcrossArgs);
            if a.find.is_empty() {
                return ToolResult::err("`find` is empty");
            }
            if a.find == a.replace {
                return ToolResult::err("`find` and `replace` are identical");
            }
            let Some(project) = crate::workspace::scan::scan_one_project(root) else {
                return ToolResult::err("could not scan the project");
            };
            let mut hits = Vec::new();
            let mut total = 0usize;
            let mut chapters_changed = 0usize;
            for v in &project.volumes {
                if let Some(only) = a.vol
                    && v.number != only
                {
                    continue;
                }
                let w = Workspace::new(root.to_path_buf(), v.number);
                for c in &v.chapters {
                    let content = translation::read_translated(&w, c.number).await;
                    let count = content.matches(&a.find).count();
                    if count == 0 {
                        continue;
                    }
                    total += count;
                    chapters_changed += 1;
                    if hits.len() < 60 {
                        hits.push(json!({"vol": v.number, "ch": c.number, "matches": count}));
                    }
                    if !a.dry_run {
                        if let Err(e) = archive_chapter(&w, c.number) {
                            return ToolResult::err(format!(
                                "archive failed for vol {} ch {}: {e}",
                                v.number, c.number
                            ));
                        }
                        let updated = content.replace(&a.find, &a.replace);
                        if let Err(e) = tokio::fs::write(w.translated(c.number), updated).await {
                            return ToolResult::err(format!(
                                "write failed for vol {} ch {}: {e}",
                                v.number, c.number
                            ));
                        }
                        tx.send(AppEvent::RefineChapterEdited {
                            vol: v.number,
                            ch: c.number,
                        });
                    }
                }
            }
            if !a.dry_run && chapters_changed > 0 {
                emit_edit(
                    tx,
                    "chapter text",
                    &format!("project replace: {total} in {chapters_changed} chapter(s)"),
                );
            }
            let suffix = if a.dry_run { " (dry run)" } else { "" };
            let verb = if a.dry_run { "would change" } else { "changed" };
            ToolResult::data(
                format!("{verb} {total} occurrence(s) in {chapters_changed} chapter(s){suffix}"),
                json!({ "dry_run": a.dry_run, "total_matches": total, "chapters": hits }),
            )
        }

        "list_flagged_chunks" => {
            let a = parse!(ListFlaggedArgs);
            let Some(project) = crate::workspace::scan::scan_one_project(root) else {
                return ToolResult::err("could not scan the project");
            };
            // A bare `ch` scopes to the active volume; an explicit `vol` always wins.
            let only_vol = a.vol.or(a.ch.map(|_| default_vol));
            let mut out = Vec::new();
            let mut total = 0usize;
            for v in &project.volumes {
                if let Some(only) = only_vol
                    && v.number != only
                {
                    continue;
                }
                let w = Workspace::new(root.to_path_buf(), v.number);
                for c in &v.chapters {
                    if let Some(only) = a.ch
                        && c.number != only
                    {
                        continue;
                    }
                    let content = translation::read_translated(&w, c.number).await;
                    let flagged = translation::review_needed_details_in(&content);
                    if flagged.is_empty() {
                        continue;
                    }
                    let items: Vec<_> = flagged
                        .iter()
                        .map(|(idx, reason)| json!({"chunk": idx + 1, "reason": reason}))
                        .collect();
                    total += items.len();
                    out.push(json!({"vol": v.number, "ch": c.number, "flagged": items}));
                }
            }
            ToolResult::data(
                format!("{total} flagged chunk(s) in {} chapter(s)", out.len()),
                json!({ "chapters": out }),
            )
        }

        "retranslate_chapter" => {
            let a = parse!(RetranslateArgs);
            let vol = a.vol.unwrap_or(default_vol);
            if a.chapters.is_empty() {
                return ToolResult::err("no chapters given".to_string());
            }
            tx.send(AppEvent::RefineRequest(RefineRequest::Retranslate {
                vol,
                chapters: a.chapters.clone(),
            }));
            ToolResult::ok(format!(
                "queued a full re-translation of vol {vol} chapter(s) {:?}",
                a.chapters
            ))
        }

        "refine_chapter_with_feedback" => {
            let a = parse!(RefineChapterArgs);
            let vol = a.vol.unwrap_or(default_vol);
            tx.send(AppEvent::RefineRequest(RefineRequest::RefineChapter {
                vol,
                ch: a.ch,
                feedback: a.feedback,
            }));
            ToolResult::ok(format!(
                "queued a focused re-translation of vol {vol} chapter {} with your feedback",
                a.ch
            ))
        }

        other => ToolResult::err(format!("unknown tool: {other}")),
    }
}

fn emit_edit(tx: &EventTx, kind: &str, summary: &str) {
    tx.send(AppEvent::RefineEditApplied {
        kind: kind.to_string(),
        summary: summary.to_string(),
    });
}

pub struct RefineTools {
    root: PathBuf,
    default_vol: u32,
    tx: EventTx,
    /// Enables `task`; absent in tests.
    client: Option<std::sync::Arc<dyn LlmClient>>,
    model: crate::model::AgentModel,
    can_spawn: bool,
    interact: RefineInteract,
}

impl RefineTools {
    #[cfg(test)]
    pub fn new(root: PathBuf, default_vol: u32, tx: EventTx) -> Self {
        Self {
            root,
            default_vol,
            tx,
            client: None,
            model: crate::model::AgentModel::openrouter(""),
            can_spawn: false,
            interact: RefineInteract::default(),
        }
    }

    pub fn with_agent(
        root: PathBuf,
        default_vol: u32,
        tx: EventTx,
        client: std::sync::Arc<dyn LlmClient>,
        model: crate::model::AgentModel,
        interact: RefineInteract,
    ) -> Self {
        Self {
            root,
            default_vol,
            tx,
            client: Some(client),
            model,
            can_spawn: true,
            interact,
        }
    }

    fn child(&self) -> Self {
        Self {
            root: self.root.clone(),
            default_vol: self.default_vol,
            tx: self.tx.clone(),
            client: self.client.clone(),
            model: self.model.clone(),
            can_spawn: false,
            interact: self.interact.clone(),
        }
    }

    async fn request_approval(&self, summary: String, diff: String) -> bool {
        let (id, rx) = self.interact.open();
        self.tx
            .send(AppEvent::RefineApprovalRequest { id, summary, diff });
        rx.await.unwrap_or_default() == "approve"
    }

    async fn request_decision(&self, question: String, options: Vec<String>) -> String {
        let (id, rx) = self.interact.open();
        self.tx.send(AppEvent::RefineDecisionRequest {
            id,
            question,
            options,
        });
        rx.await.unwrap_or_default()
    }

    async fn run_ask_user(&self, arguments_json: &str) -> ToolResult {
        #[derive(serde::Deserialize)]
        struct Args {
            question: String,
            #[serde(default)]
            options: Vec<String>,
        }
        let a: Args = match serde_json::from_str(arguments_json) {
            Ok(a) => a,
            Err(e) => return ToolResult::err(format!("bad ask_user args: {e}")),
        };
        let answer = self.request_decision(a.question, a.options).await;
        if answer.is_empty() {
            ToolResult::err("the user dismissed the question without answering")
        } else {
            ToolResult::ok(format!("user answered: {answer}"))
        }
    }

    async fn run_subagent(&self, arguments_json: &str) -> ToolResult {
        if !self.can_spawn {
            return ToolResult::err("nested sub-agents are not allowed");
        }
        let Some(client) = self.client.clone() else {
            return ToolResult::err("sub-agents are unavailable (no client wired)");
        };
        #[derive(serde::Deserialize)]
        struct Args {
            description: String,
            #[serde(default)]
            scope: Option<String>,
        }
        let a: Args = match serde_json::from_str(arguments_json) {
            Ok(a) => a,
            Err(e) => return ToolResult::err(format!("bad task args: {e}")),
        };
        let user = match &a.scope {
            Some(s) if !s.trim().is_empty() => format!("Scope: {s}\n\nTask: {}", a.description),
            _ => a.description.clone(),
        };
        let mut req = ChatRequest::new(
            self.model.model.clone(),
            vec![Message::system(SUBAGENT_SYSTEM), Message::user(user)],
        );
        req.tools = Some(refine_tools_vec());
        req.reasoning = self.model.reasoning_param();

        let preview: String = a.description.chars().take(80).collect();
        self.tx.send(AppEvent::Log {
            level: LogLevel::Info,
            msg: format!("Refine sub-agent started: {preview}"),
        });

        match crate::llm::tool_loop::run_tool_loop(client.as_ref(), req, &self.child(), 30).await {
            Ok(outcome) => {
                let report = outcome
                    .response
                    .choices
                    .first()
                    .and_then(|c| c.message.content.clone())
                    .unwrap_or_default();
                ToolResult::ok(format!(
                    "sub-agent finished ({} tool call(s)):\n{report}",
                    outcome.tool_calls
                ))
            }
            Err(e) => ToolResult::err(format!("sub-agent failed: {e}")),
        }
    }
}

#[async_trait]
impl ToolExecutor for RefineTools {
    async fn execute(&self, name: &str, arguments_json: &str) -> anyhow::Result<String> {
        if name == "task" {
            return Ok(serde_json::to_string(&self.run_subagent(arguments_json).await)?);
        }
        if name == "ask_user" {
            return Ok(serde_json::to_string(&self.run_ask_user(arguments_json).await)?);
        }
        // Ask mode gates only mutating tools.
        if self.interact.mode() == ApprovalMode::Ask
            && let Some((summary, diff)) = approval_preview(name, arguments_json)
            && !self.request_approval(summary, diff).await
        {
            return Ok(serde_json::to_string(&ToolResult::err(
                "edit rejected by the user",
            ))?);
        }
        let result =
            dispatch_refine_tool(&self.root, self.default_vol, &self.tx, name, arguments_json).await;
        Ok(serde_json::to_string(&result)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::client::Result as LlmResult;
    use crate::llm::{Choice, FunctionCall, ResponseMessage, ToolCall};
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[test]
    fn model_max_context_by_family() {
        assert_eq!(model_max_context("google/gemini-3-flash"), 1_000_000);
        assert_eq!(model_max_context("claude-opus-4-8[1m]"), 1_000_000);
        assert_eq!(model_max_context("anthropic/claude-sonnet-4-6"), 200_000);
        assert_eq!(model_max_context("openai/gpt-5.5"), 400_000);
        assert_eq!(model_max_context("some/unknown-model"), 128_000);
    }

    #[test]
    fn compaction_summarizes_old_turns_and_keeps_recent_tail() {
        let mut req = ChatRequest::new("claude-test", vec![Message::system("sys")]);
        let big = "x".repeat(40_000); // ~10k tokens each
        for i in 0..30 {
            req.messages.push(Message::user(format!("req {i} {big}")));
            req.messages.push(Message::assistant(format!("ans {i}")));
        }
        let before = req.messages.len();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        maybe_compact(&mut req, &EventTx(tx), false);

        assert!(req.messages.len() < before, "context was compacted");
        assert_eq!(req.messages[0].role, Role::System, "system prompt kept");
        assert_eq!(req.messages[1].role, Role::User);
        assert!(
            req.messages[1]
                .content
                .as_deref()
                .unwrap()
                .contains("compacted")
        );
        assert_eq!(req.messages[2].role, Role::User, "tail starts at a user turn");
        assert!(
            req.messages
                .last()
                .unwrap()
                .content
                .as_deref()
                .unwrap()
                .contains("ans 29")
        );
    }

    #[test]
    fn approval_mode_cycles_and_gates() {
        let i = RefineInteract::default();
        assert_eq!(i.mode(), ApprovalMode::Auto);
        assert_eq!(i.cycle_mode(), ApprovalMode::Always);
        assert_eq!(i.cycle_mode(), ApprovalMode::Ask);
        assert_eq!(i.cycle_mode(), ApprovalMode::Auto);
        assert_eq!(ApprovalMode::Always.cycled(), ApprovalMode::Ask);
    }

    #[tokio::test]
    async fn interact_channel_round_trips() {
        let i = RefineInteract::default();
        let (id, rx) = i.open();
        assert_eq!(id, 0, "ids start at 0");
        let (id2, _rx2) = i.open();
        assert_eq!(id2, 1, "ids increment");
        i.resolve(id, "yes".to_string());
        assert_eq!(rx.await.unwrap(), "yes");
        i.resolve(999, "x".to_string());
    }

    #[test]
    fn approval_preview_builds_diff_for_edits() {
        let (summary, diff) =
            approval_preview("edit_chapter", r#"{"ch":3,"old":"foo","new":"bar"}"#).unwrap();
        assert!(summary.contains("chapter 3"));
        assert!(diff.contains("- foo") && diff.contains("+ bar"));
        assert!(approval_preview("read_chapter", r#"{"ch":1}"#).is_none());
        assert!(
            approval_preview(
                "replace_across_project",
                r#"{"find":"a","replace":"b","dry_run":true}"#
            )
            .is_none()
        );
    }

    #[tokio::test]
    async fn task_tool_blocked_without_agent_wiring() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let tools = RefineTools::new(std::path::PathBuf::from("/tmp"), 1, EventTx(tx));
        let out = tools
            .execute("task", r#"{"description":"do something big"}"#)
            .await
            .unwrap();
        assert!(out.contains("\"ok\":false"));
        assert!(out.contains("sub-agent") || out.contains("nested"));
    }

    #[test]
    fn no_compaction_when_under_budget() {
        let mut req = ChatRequest::new("claude-test", vec![Message::system("sys")]);
        req.messages.push(Message::user("hi"));
        req.messages.push(Message::assistant("hello"));
        let before = req.messages.clone();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        maybe_compact(&mut req, &EventTx(tx), false);
        assert_eq!(req.messages.len(), before.len(), "small chat untouched");
    }

    struct ScriptedClient {
        responses: Mutex<VecDeque<crate::llm::ChatResponse>>,
    }

    #[async_trait]
    impl LlmClient for ScriptedClient {
        async fn chat(&self, _req: &ChatRequest) -> LlmResult<crate::llm::ChatResponse> {
            Ok(self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("script exhausted"))
        }
    }

    fn tool_call_turn(name: &str, args: &str) -> crate::llm::ChatResponse {
        crate::llm::ChatResponse {
            id: None,
            model: None,
            service_tier: None,
            usage: None,
            choices: vec![Choice {
                index: 0,
                finish_reason: Some("tool_calls".to_string()),
                message: ResponseMessage {
                    role: Some("assistant".to_string()),
                    content: None,
                    tool_calls: Some(vec![ToolCall {
                        id: "call_1".to_string(),
                        kind: "function".to_string(),
                        function: FunctionCall {
                            name: name.to_string(),
                            arguments: args.to_string(),
                        },
                    }]),
                },
            }],
        }
    }

    fn stop_turn(text: &str) -> crate::llm::ChatResponse {
        crate::llm::ChatResponse {
            id: None,
            model: None,
            service_tier: None,
            usage: None,
            choices: vec![Choice {
                index: 0,
                finish_reason: Some("stop".to_string()),
                message: ResponseMessage {
                    role: Some("assistant".to_string()),
                    content: Some(text.to_string()),
                    tool_calls: None,
                },
            }],
        }
    }

    #[tokio::test]
    async fn refine_turn_streams_executes_tool_and_finishes() {
        let root = temp_root("turn");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let etx = EventTx(tx);
        let client = ScriptedClient {
            responses: Mutex::new(VecDeque::from(vec![
                tool_call_turn("upsert_character", r#"{"jp_name":"勇","thai_name":"ยู"}"#),
                stop_turn("เพิ่มตัวละครเรียบร้อย"),
            ])),
        };
        let tools = RefineTools::new(root.clone(), 1, etx.clone());
        let mut req = ChatRequest::new("m", vec![Message::system("sys")]);
        req.messages.push(Message::user("add a character"));
        let cancel = AtomicBool::new(false);

        run_refine_turn(&client, &mut req, &tools, &etx, &cancel).await;

        let (mut saw_tool, mut saw_done, mut saw_delta) = (false, false, false);
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AppEvent::RefineToolInvoked { tool, .. } => {
                    saw_tool = true;
                    assert_eq!(tool, "upsert_character");
                }
                AppEvent::RefineMessageDone => saw_done = true,
                AppEvent::RefineDelta { .. } => saw_delta = true,
                _ => {}
            }
        }
        assert!(saw_tool, "the tool call was surfaced");
        assert!(saw_delta, "the final assistant text streamed");
        assert!(saw_done, "the turn finished");

        let ws = Workspace::new(root.clone(), 1);
        assert!(
            characters::load(&ws).iter().any(|c| c.thai_name == "ยู"),
            "the tool mutated the workspace"
        );
        assert!(req.messages.len() >= 4);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn refine_turn_respects_cancel() {
        let root = temp_root("cancel");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let etx = EventTx(tx);
        let client = ScriptedClient {
            responses: Mutex::new(VecDeque::from(vec![stop_turn("unused")])),
        };
        let tools = RefineTools::new(root.clone(), 1, etx.clone());
        let mut req = ChatRequest::new("m", vec![Message::system("sys")]);
        let cancel = AtomicBool::new(true); // pre-cancelled

        run_refine_turn(&client, &mut req, &tools, &etx, &cancel).await;
        let mut saw_done = false;
        while let Ok(ev) = rx.try_recv() {
            if matches!(ev, AppEvent::RefineMessageDone) {
                saw_done = true;
            }
        }
        assert!(saw_done, "a cancelled turn still emits MessageDone");
        let _ = std::fs::remove_dir_all(&root);
    }

    fn temp_root(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "honya_refine_{tag}_{}_{}",
            std::process::id(),
            stamp
        ));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[tokio::test]
    async fn upsert_character_then_read_lexicon_round_trips() {
        let root = temp_root("char");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let tx = EventTx(tx);

        let r = dispatch_refine_tool(
            &root,
            1,
            &tx,
            "upsert_character",
            r#"{"jp_name":"勇","thai_name":"ยู"}"#,
        )
        .await;
        assert!(r.ok, "{}", r.message);

        let r =
            dispatch_refine_tool(&root, 1, &tx, "read_lexicon", r#"{"kind":"characters"}"#).await;
        assert!(r.ok);
        let chars = r.data.unwrap();
        let arr = chars.get("characters").unwrap().as_array().unwrap();
        assert!(
            arr.iter()
                .any(|c| c.get("thai_name").and_then(|v| v.as_str()) == Some("ยู"))
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn replace_chapter_text_archives_and_overwrites() {
        let root = temp_root("replace");
        let ws = Workspace::new(root.clone(), 1);
        std::fs::create_dir_all(ws.translated(1).parent().unwrap()).unwrap();
        translation::append_chunk(&ws, 1, 0, "ข้อความเดิม")
            .await
            .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let tx = EventTx(tx);

        let r = dispatch_refine_tool(
            &root,
            1,
            &tx,
            "replace_chapter_text",
            r#"{"ch":1,"new_text":"ข้อความใหม่"}"#,
        )
        .await;
        assert!(r.ok, "{}", r.message);

        let now = translation::read_translated(&ws, 1).await;
        assert!(now.contains("ข้อความใหม่"));
        assert!(!now.contains("ข้อความเดิม"));
        let runs = volume::chapter_runs(&ws, 1);
        assert!(
            runs.iter().any(|r| r.archived.is_some()),
            "prior version archived"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn retranslate_tool_emits_request() {
        let root = temp_root("retrans");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let etx = EventTx(tx);
        let r = dispatch_refine_tool(
            &root,
            2,
            &etx,
            "retranslate_chapter",
            r#"{"chapters":[3,4]}"#,
        )
        .await;
        assert!(r.ok, "{}", r.message);
        let mut req = None;
        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::RefineRequest(r) = ev {
                req = Some(r);
            }
        }
        match req {
            Some(RefineRequest::Retranslate { vol, chapters }) => {
                assert_eq!(vol, 2, "defaults to the active volume");
                assert_eq!(chapters, vec![3, 4]);
            }
            other => panic!("expected a Retranslate request, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn default_refine_model_is_gpt_5_5() {
        assert_eq!(crate::model::ModelSet::default().refine.model, "openai/gpt-5.5");
    }

    #[tokio::test]
    async fn edit_chapter_replaces_all_when_flagged() {
        let root = temp_root("editall");
        let ws = Workspace::new(root.clone(), 1);
        std::fs::create_dir_all(ws.translated(1).parent().unwrap()).unwrap();
        translation::append_chunk(&ws, 1, 0, "แมวสีดำ และ แมวสีขาว")
            .await
            .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let tx = EventTx(tx);

        let r = dispatch_refine_tool(
            &root,
            1,
            &tx,
            "edit_chapter",
            r#"{"ch":1,"old":"แมว","new":"สุนัข","replace_all":true}"#,
        )
        .await;
        assert!(r.ok, "{}", r.message);
        let now = translation::prose_only(&translation::read_translated(&ws, 1).await);
        assert_eq!(now, "สุนัขสีดำ และ สุนัขสีขาว");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn edit_chapter_rejects_ambiguous_match() {
        let root = temp_root("editambig");
        let ws = Workspace::new(root.clone(), 1);
        std::fs::create_dir_all(ws.translated(1).parent().unwrap()).unwrap();
        translation::append_chunk(&ws, 1, 0, "แมว และ แมว")
            .await
            .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let tx = EventTx(tx);

        let r = dispatch_refine_tool(
            &root,
            1,
            &tx,
            "edit_chapter",
            r#"{"ch":1,"old":"แมว","new":"สุนัข"}"#,
        )
        .await;
        assert!(!r.ok, "ambiguous match must be rejected: {}", r.message);
        let now = translation::prose_only(&translation::read_translated(&ws, 1).await);
        assert_eq!(now, "แมว และ แมว");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn update_plan_emits_plan_event() {
        let root = temp_root("plan");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let etx = EventTx(tx);
        let r = dispatch_refine_tool(
            &root,
            1,
            &etx,
            "update_plan",
            r#"{"steps":[{"step":"read ch1","status":"in_progress"},{"step":"fix term"}]}"#,
        )
        .await;
        assert!(r.ok, "{}", r.message);
        let mut steps = None;
        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::RefinePlanUpdated { steps: s } = ev {
                steps = Some(s);
            }
        }
        let steps = steps.expect("a plan event was emitted");
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].status, crate::model::PlanStepStatus::InProgress);
        assert_eq!(steps[1].status, crate::model::PlanStepStatus::Pending);

        let _ = std::fs::remove_dir_all(&root);
    }

    fn scannable_project(tag: &str) -> (PathBuf, Workspace) {
        let root = temp_root(tag);
        std::fs::write(root.join("PROJECT.md"), "# Test\n").unwrap();
        let ws = Workspace::new(root.clone(), 1);
        std::fs::create_dir_all(ws.raw(1).parent().unwrap()).unwrap();
        std::fs::write(ws.raw(1), "源文").unwrap();
        std::fs::write(ws.raw(2), "源文二").unwrap();
        std::fs::create_dir_all(ws.translated(1).parent().unwrap()).unwrap();
        (root, ws)
    }

    #[tokio::test]
    async fn multi_edit_applies_in_order_and_is_atomic() {
        let root = temp_root("multiedit");
        let ws = Workspace::new(root.clone(), 1);
        std::fs::create_dir_all(ws.translated(1).parent().unwrap()).unwrap();
        translation::append_chunk(&ws, 1, 0, "หนึ่ง สอง สาม")
            .await
            .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let tx = EventTx(tx);

        let r = dispatch_refine_tool(
            &root,
            1,
            &tx,
            "multi_edit_chapter",
            r#"{"ch":1,"edits":[{"old":"หนึ่ง","new":"1"},{"old":"ไม่มี","new":"x"}]}"#,
        )
        .await;
        assert!(!r.ok, "a failing edit must abort the batch");
        assert_eq!(
            translation::prose_only(&translation::read_translated(&ws, 1).await),
            "หนึ่ง สอง สาม",
            "nothing was written on failure"
        );

        let r = dispatch_refine_tool(
            &root,
            1,
            &tx,
            "multi_edit_chapter",
            r#"{"ch":1,"edits":[{"old":"หนึ่ง","new":"1"},{"old":"สอง","new":"2"}]}"#,
        )
        .await;
        assert!(r.ok, "{}", r.message);
        assert_eq!(
            translation::prose_only(&translation::read_translated(&ws, 1).await),
            "1 2 สาม"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn replace_across_project_dry_run_then_writes() {
        let (root, ws) = scannable_project("replall");
        translation::append_chunk(&ws, 1, 0, "ดาบเก่า").await.unwrap();
        translation::append_chunk(&ws, 2, 0, "ดาบเก่า อีกครั้ง")
            .await
            .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let tx = EventTx(tx);

        let r = dispatch_refine_tool(
            &root,
            1,
            &tx,
            "replace_across_project",
            r#"{"find":"ดาบเก่า","replace":"ดาบใหม่","dry_run":true}"#,
        )
        .await;
        assert!(r.ok, "{}", r.message);
        assert_eq!(r.data.unwrap()["total_matches"], 2);
        assert!(
            translation::read_translated(&ws, 1).await.contains("ดาบเก่า"),
            "dry run must not write"
        );

        let r = dispatch_refine_tool(
            &root,
            1,
            &tx,
            "replace_across_project",
            r#"{"find":"ดาบเก่า","replace":"ดาบใหม่"}"#,
        )
        .await;
        assert!(r.ok, "{}", r.message);
        assert!(
            translation::prose_only(&translation::read_translated(&ws, 1).await).contains("ดาบใหม่")
        );
        assert!(
            translation::prose_only(&translation::read_translated(&ws, 2).await).contains("ดาบใหม่")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn list_flagged_chunks_reports_review_needed() {
        let (root, ws) = scannable_project("flagged");
        translation::append_chunk(&ws, 1, 0, "ผ่าน").await.unwrap();
        translation::append_chunk_needs_review(&ws, 1, 1, "ร่าง", 3, "meaning drift")
            .await
            .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let tx = EventTx(tx);

        let r = dispatch_refine_tool(&root, 1, &tx, "list_flagged_chunks", "{}").await;
        assert!(r.ok, "{}", r.message);
        let d = r.data.unwrap();
        let chapters = d["chapters"].as_array().unwrap();
        assert_eq!(chapters.len(), 1);
        assert_eq!(chapters[0]["ch"], 1);
        let flagged = chapters[0]["flagged"].as_array().unwrap();
        assert_eq!(flagged[0]["chunk"], 2, "0-based idx 1 surfaces as chunk 2");
        assert!(
            flagged[0]["reason"]
                .as_str()
                .unwrap()
                .contains("meaning drift")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn add_continuity_note_round_trips_through_read_meta() {
        let root = temp_root("note");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let tx = EventTx(tx);

        let r = dispatch_refine_tool(
            &root,
            1,
            &tx,
            "add_continuity_note",
            r#"{"severity":"warning","note":"name drift on Yuu","kind":"name","chapter":3}"#,
        )
        .await;
        assert!(r.ok, "{}", r.message);

        let r = dispatch_refine_tool(&root, 1, &tx, "read_meta", r#"{"kind":"notes"}"#).await;
        assert!(r.ok, "{}", r.message);
        let notes = r.data.unwrap()["continuity_notes"]
            .as_array()
            .unwrap()
            .clone();
        assert!(
            notes
                .iter()
                .any(|n| n["note"].as_str() == Some("name drift on Yuu"))
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
