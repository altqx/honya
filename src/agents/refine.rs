//! Refine chat agent backend: cross-volume tools plus the streaming loop.
//! Chapter-text edits archive the prior version before overwriting.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::{StreamExt, stream};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::oneshot;

use crate::agents::subagent::{
    ChildMessage, Collected, Delivery, MAX_LIVE_SUBAGENTS, SubagentRegistry, SubagentRole,
    SubagentSpec,
};
use crate::app::refine::{MentionTarget, parse_scope};
use crate::llm::client::LlmClient;
use crate::llm::tool_loop::{ToolExecutor, ToolLoopOutcome};
use crate::llm::{
    ChatRequest, ChatResponse, Choice, LlmError, Message, ResponseMessage, Role, Tool, ToolCall,
    Usage,
};
use crate::model::{
    AppEvent, Character, ContinuityNote, EventTx, GlossaryTerm, LogLevel, PlanStep, RefineRequest,
    RefineQuestion, RefineSubagentStatus, Relationship, StyleExample, TermPolicy, ToolResult,
};
use crate::workspace::{Workspace, characters, glossary, style, translation, volume};

const READ_CAP: usize = 12_000;
const CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(50);
const SUBAGENT_RATE_LIMIT_MAX_SLEEP_SECS: u64 = 60;
const SUBAGENT_RATE_LIMIT_MAX_RETRIES: u32 = 10;

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
            "description":"Read a chapter's Japanese source and/or current target-language translation (prose only). The translation is returned with `N│ ` line-number prefixes and a total line count; long chapters are windowed (use offset/limit to page). NEVER copy the `N│ ` prefix into an edit.",
            "parameters":{"type":"object","additionalProperties":false,"required":["ch"],
                "properties":{
                    "vol":{"type":"integer","description":"Volume number; defaults to the active volume."},
                    "ch":{"type":"integer"},
                    "include_jp":{"type":"boolean","description":"Include the Japanese source (default true)."},
                    "include_translation":{"type":"boolean","description":"Include the target-language translation (default true)."},
                    "offset":{"type":"integer","description":"1-based first translated line to return (default 1)."},
                    "limit":{"type":"integer","description":"Max translated lines to return (default 400)."}
                }}
        }},
        {"type":"function","function":{
            "name":"grep_chapter",
            "description":"Find a substring inside one chapter and return matching lines with line numbers. Searches the target-language translation by default.",
            "parameters":{"type":"object","additionalProperties":false,"required":["ch","query"],
                "properties":{
                    "vol":{"type":"integer"},
                    "ch":{"type":"integer"},
                    "query":{"type":"string"},
                    "side":{"type":"string","enum":["translation","jp","both"],"description":"Which text to search (default translation)."},
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
            "description":"Record your working plan as a short checklist the user sees live. Call it for any multi-step or multi-chapter task, then call it again to update status as you progress: keep exactly one step in_progress, flip finished steps to completed, and add steps you discover. Each call REPLACES the whole list; pass an empty steps array to clear it when done. Skip it for a single trivial edit.",
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
            "description":"Search the target-language translation of every chapter (or one volume) for a substring; returns matching chapters with snippets.",
            "parameters":{"type":"object","additionalProperties":false,"required":["query"],
                "properties":{
                    "query":{"type":"string"},
                    "vol":{"type":"integer","description":"Restrict to one volume; omit to search the whole project."}
                }}
        }},
        {"type":"function","function":{
            "name":"upsert_character",
            "description":"Create or update a character in CHARACTERS.md (cross-volume; pass vol for the volume whose lexicon to edit).",
            "parameters":{"type":"object","additionalProperties":false,"required":["jp_name","translated_name"],
                "properties":{
                    "vol":{"type":"integer"},
                    "id":{"type":"string"},
                    "jp_name":{"type":"string"},
                    "translated_name":{"type":"string"},
                    "romaji":{"type":"string"},
                    "gender":{"type":"string","enum":["male","female","nonbinary","unknown"]},
                    "honorific":{"type":"string"},
                    "speech_style":{"type":"string"},
                    "relationships":{"type":"array","items":{"type":"object","additionalProperties":false,
                        "required":["target_id","relation"],
                        "properties":{"target_id":{"type":"string"},"relation":{"type":"string"}}}},
                    "aliases":{"type":"array","items":{"type":"string"},"description":"Variant JP spellings of the SAME name that share one target rendering."},
                    "also_called":{"type":"array","description":"Distinct names others use for this character, each with its own target rendering.","items":{"type":"object","additionalProperties":false,"required":["jp","translated_name"],"properties":{"jp":{"type":"string"},"translated_name":{"type":"string"},"by":{"type":"string"}}}},
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
            "parameters":{"type":"object","additionalProperties":false,"required":["jp_term","translated_term"],
                "properties":{
                    "vol":{"type":"integer"},
                    "jp_term":{"type":"string"},
                    "translated_term":{"type":"string"},
                    "romaji":{"type":"string"},
                    "category":{"type":"string","enum":["skill","place","org","item","title","concept","sfx","other"]},
                    "gloss":{"type":"string"},
                    "policy":{"type":"string","enum":["hard_locked","preferred","forbidden","context_dependent"]},
                    "forbidden_translations":{"type":"array","items":{"type":"string"}},
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
            "description":"Set the volume synopsis, optionally retaining the raw source.",
            "parameters":{"type":"object","additionalProperties":false,"required":["translated_synopsis"],
                "properties":{"vol":{"type":"integer"},"raw":{"type":"string"},"translated_synopsis":{"type":"string"}}}
        }},
        {"type":"function","function":{
            "name":"append_style_note",
            "description":"Append a translation-style note (tone, register, conventions) to STYLE.md.",
            "parameters":{"type":"object","additionalProperties":false,"required":["note"],
                "properties":{"vol":{"type":"integer"},"note":{"type":"string"}}}
        }},
        {"type":"function","function":{
            "name":"add_style_example",
            "description":"Add a Japanese→target-language exemplar pair.",
            "parameters":{"type":"object","additionalProperties":false,"required":["jp","translated_text"],
                "properties":{"vol":{"type":"integer"},"jp":{"type":"string"},"translated_text":{"type":"string"},"note":{"type":"string"}}}
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
            "description":"Replace a chapter's ENTIRE target-language translation with new_text. The prior version is archived first. Use for a full rewrite; for a small edit prefer edit_chapter.",
            "parameters":{"type":"object","additionalProperties":false,"required":["ch","new_text"],
                "properties":{"vol":{"type":"integer"},"ch":{"type":"integer"},"new_text":{"type":"string"}}}
        }},
        {"type":"function","function":{
            "name":"edit_chapter",
            "description":"Surgically replace an exact snippet of a chapter's target-language translation. `old` must match exactly without the `N│ ` prefix and be unique unless replace_all=true. The prior version is archived first.",
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
            "description":"Apply several exact edits to one chapter's target-language translation atomically. Each `old` must match; if any edit fails, nothing is written.",
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
            "description":"Project-wide consistency fix: replace an exact target-language string in every chapter (or one volume). Preview with dry_run=true, then update matching glossary/character metadata after applying.",
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
            "description":"Spawn a focused sub-agent to carry out a self-contained chunk of work (e.g. \"normalize every honorific in volume 2\") using the same project tools, then report back. Use for large or parallelizable sub-tasks so the current thread stays focused. Multiple task calls in the same assistant turn run in parallel; use disjoint scopes. Pick the narrowest `role` that can finish the job: a role it was not given is a tool it cannot call, which is what stops a survey from turning into an edit. Sub-agents may delegate smaller self-contained work to their own sub-agents; nesting is bounded by the app.",
            "parameters":{"type":"object","additionalProperties":false,"required":["description"],
                "properties":{
                    "description":{"type":"string","description":"The complete, self-contained task for the sub-agent."},
                    "scope":{"type":"string","description":"Optional scope hint, e.g. a volume or chapter range."},
                    "role":{"type":"string","enum":["explore","editor","lexicon","general"],"description":"What the sub-agent is allowed to do. `explore` reads, searches and reports but cannot change anything; `editor` also rewrites chapter prose; `lexicon` also maintains characters, glossary, style notes and recaps; `general` (the default) has everything and may delegate further."},
                    "background":{"type":"boolean","description":"Return a sub-agent id immediately instead of its report, leaving it to run while you carry on. Collect it later with subagent_output — a background sub-agent you never collect is work the user never hears about. Use it when you have something useful to do meanwhile; leave it false when the next thing you do depends on the answer."}
                }}
        }},
        {"type":"function","function":{
            "name":"subagent_output",
            "description":"Collect the reports of sub-agents started with `background: true`. Pass timeout_ms 0 for an immediate snapshot of who has finished, or a positive value to wait for them (capped at one hour). Collect every background sub-agent before you finish your turn.",
            "parameters":{"type":"object","additionalProperties":false,"required":["ids"],
                "properties":{
                    "ids":{"type":"array","items":{"type":"string"},"description":"Sub-agent ids returned by background task calls; at most 20."},
                    "timeout_ms":{"type":"integer","description":"0 for a non-blocking snapshot, otherwise how long to wait in milliseconds."}
                }}
        }},
        {"type":"function","function":{
            "name":"cancel_subagent",
            "description":"Stop a running sub-agent. It stops at its next round boundary and keeps a resumable checkpoint, so anything it already wrote to disk stays written and the rest can be picked up later with resume_subagent. Use it when a delegated task turns out to be wrong, redundant, or no longer wanted.",
            "parameters":{"type":"object","additionalProperties":false,"required":["id"],
                "properties":{"id":{"type":"string","description":"The sub-agent id."}}}
        }},
        {"type":"function","function":{
            "name":"message_subagent",
            "description":"Send a message to a running sub-agent, or upward to your own parent with the id \"parent\". Use it to redirect a child heading the wrong way, to add a requirement it should also satisfy, or to report something the parent needs before you finish. `delivery` is `steer` (default: the child sees it at its next round), `queue` (held until the child would otherwise finish, turning \"done\" into \"now also do this\"), or `interject` (abandons the model call already in flight so the child sees it immediately).",
            "parameters":{"type":"object","additionalProperties":false,"required":["id","message"],
                "properties":{
                    "id":{"type":"string","description":"A sub-agent id, or \"parent\"."},
                    "message":{"type":"string","description":"What to tell it."},
                    "delivery":{"type":"string","enum":["steer","queue","interject"]}
                }}
        }},
        {"type":"function","function":{
            "name":"list_interrupted_subagents",
            "description":"List durable checkpoints for sub-agents interrupted by user cancellation, process exit, power loss, or provider failure. Use before resume_subagent when the user asks to continue previous delegated work.",
            "parameters":{"type":"object","additionalProperties":false,"properties":{}}
        }},
        {"type":"function","function":{
            "name":"resume_subagent",
            "description":"Resume an interrupted sub-agent checkpoint by id, preserving its accumulated messages and tool results. Use after list_interrupted_subagents or when the user asks to continue a specific saved sub-agent.",
            "parameters":{"type":"object","additionalProperties":false,"required":["id"],
                "properties":{"id":{"type":"string","description":"Checkpoint id returned by list_interrupted_subagents."}}}
        }},
        {"type":"function","function":{
            "name":"ask_user",
            "description":"Ask the user one or more questions and wait for their answers, when a decision is genuinely theirs to make and you cannot resolve it from the project or sensible defaults (e.g. choosing between two valid Thai renderings, or confirming a risky bulk change). Put EVERY related question in ONE call via `questions` — they are shown together as a single card the user moves through, so asking one at a time costs them a round trip each. `options` are offered as choices; the user can always type something else instead, so offering options never narrows the answer. Set `multiple` when several answers may apply. Use sparingly — prefer acting on a reasonable default and saying so.",
            "parameters":{"type":"object","additionalProperties":false,
                "properties":{
                    "questions":{"type":"array","description":"The questions to put to the user, asked together.","items":{
                        "type":"object","additionalProperties":false,"required":["question"],
                        "properties":{
                            "question":{"type":"string","description":"The question to put to the user."},
                            "options":{"type":"array","items":{"type":"string"},"description":"Offered choices; the user may still answer freely."},
                            "multiple":{"type":"boolean","description":"Several of the options may apply."}
                        }}},
                    "question":{"type":"string","description":"Shorthand for a single question; ignored when `questions` is given."},
                    "options":{"type":"array","items":{"type":"string"},"description":"Offered choices for the shorthand form."}
                }}
        }}
    ])
}

const MAX_TOOL_ROUNDS: usize = 40;
const SUBAGENT_MAX_TOOL_ROUNDS: usize = 120;
const SUBAGENT_FINALIZE_ROUNDS: usize = 3;
const MAX_SUBAGENT_DEPTH: usize = 3;
const COMPACT_FRACTION: f64 = 0.8;
const KEEP_RECENT_TURNS: usize = 3;
const KEEP_RECENT_TOOL_ROUNDS: usize = 2;
const COMPACT_SUMMARY_PREFIX: &str = "[Earlier conversation, compacted to fit the context window]";

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

    let Some(keep_from) = compact_keep_from(&req.messages) else {
        return;
    };
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
    tx.send(AppEvent::RefineContextCompacted {
        dropped_messages: dropped,
        token_estimate: est,
        context_max: max,
    });
}

fn compact_keep_from(messages: &[Message]) -> Option<usize> {
    // Prefer user-message boundaries for the main Refine thread.
    let user_idxs: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(i, m)| *i > 0 && m.role == Role::User)
        .map(|(i, _)| i)
        .collect();
    if let Some(keep_from) = user_idxs
        .iter()
        .rev()
        .nth(KEEP_RECENT_TURNS - 1)
        .copied()
        .filter(|i| *i > 1)
    {
        return Some(keep_from);
    }

    // Sub-agents usually have one user task followed by many assistant/tool
    // rounds. Keep the latest complete tool rounds instead of never compacting.
    messages
        .iter()
        .enumerate()
        .filter(|(i, m)| *i > 1 && m.role == Role::Assistant)
        .map(|(i, _)| i)
        .collect::<Vec<_>>()
        .iter()
        .rev()
        .nth(KEEP_RECENT_TOOL_ROUNDS - 1)
        .copied()
        .filter(|i| *i > 1)
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
                                e.get("old")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                e.get("new")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            let refs: Vec<(&str, &str)> = pairs
                .iter()
                .map(|(o, n)| (o.as_str(), n.as_str()))
                .collect();
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
                format!(
                    "replace across project: \"{}\" → \"{}\"",
                    s("find"),
                    s("replace")
                ),
                String::new(),
            ))
        }
        _ => None,
    }
}

/// Compact old turns while dropping verbose tool noise.
fn compact_summary(dropped: &[Message]) -> String {
    let mut s = String::from(COMPACT_SUMMARY_PREFIX);
    s.push('\n');
    for m in dropped {
        let label = match m.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
            _ => continue,
        };
        let Some(text) = m
            .content
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        else {
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
    pub target_language: crate::model::TargetLanguage,
    pub tx: EventTx,
    /// Stops the in-flight turn between rounds.
    pub cancel: Arc<AtomicBool>,
    pub session_id: String,
    pub interact: RefineInteract,
    pub steering: Arc<Mutex<VecDeque<UserTurn>>>,
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

    pub fn cancel_all(&self) {
        if let Ok(mut map) = self.pending.lock() {
            for (_, tx) in map.drain() {
                let _ = tx.send(String::new());
            }
        }
    }
}

pub struct UserTurn {
    pub text: String,
    /// The sub-agent that sent this upward, when it was not the user.
    pub from: Option<String>,
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

fn refine_system_prompt(target: crate::model::TargetLanguage) -> String {
    if target == crate::model::TargetLanguage::English {
        return r#"You are honya's Refine agent for a Japanese-to-English light-novel translation project. Work directly on the on-disk project through the provided tools and keep going until the user's request is resolved.

Lead with results, stay concise, and match the user's language. Read the Japanese source, current English translation, CHARACTERS, GLOSSARY, STYLE, and relevant context before judging or editing. For multi-step work, maintain a plan; delegate only large independent scopes. Give every `task` the narrowest `role` that can finish it (`explore`, `editor`, `lexicon`, or `general`); set `background: true` only when you have useful work to do meanwhile and always collect the result with `subagent_output` before finishing the turn; redirect a sub-agent with `message_subagent` or stop it with `cancel_subagent` rather than letting wrong work run to completion. Prefer surgical exact edits, verify every changed passage afterward, preserve Markdown/images/scene breaks, and archive-safe tool workflows.

English quality means publication-ready prose for native English-language light-novel readers: faithful meaning and POV, natural dialogue, distinct voices, confident narrative rhythm, idiomatic syntax, restrained honorific/localization choices consistent with project style, and no translationese, intrusive glosses, gratuitous Westernization, invented profanity, censorship, or raw Japanese residue. Treat reviewer notes as evidence to verify, not automatic truth. Resolve speakers and Japanese modifier chains from the source. Keep short name surfaces short and follow exact alternate-address mappings.

All `translated_*` fields contain English target-language text in this mode. When changing a recurring name or term, update both chapters and reference metadata. Handle mature material neutrally and faithfully. Ask the user only when two materially different valid outcomes cannot be resolved from source, context, or a safe default."#
            .to_string();
    }
    r#"You are honya's Refine agent for a Japanese→Thai light-novel project. Work on the on-disk files through the provided tools until the user's request is fully resolved.

Personality: concise and direct in a terminal pane — lead with the result, skip preamble and tool narration, match the user's language, summarize changes briefly (`vN/cM`), and format with short Markdown.

Autonomy:
- for answer/explain/review/diagnose/plan requests: inspect and report; do not edit unless asked
- for change/fix/refine requests: make in-scope local edits and verify without asking first
- require confirmation only for destructive/irreversible bulk risk or when two materially different valid outcomes cannot be resolved from source/context/defaults
- keep working until done; use update_plan for multi-step work; delegate with `task` only for large independent disjoint scopes
- give every `task` the narrowest `role` that can finish it: `explore` to survey and report, `editor` for chapter prose, `lexicon` for characters/glossary/style/recaps, `general` only when it genuinely needs everything — a role it was not given is a tool it cannot call
- set `background: true` when you have useful work to do meanwhile, then collect with `subagent_output` before you finish the turn; a background sub-agent you never collect is work the user never hears about
- `cancel_subagent` when a delegated task turns out wrong or redundant — it keeps a resumable checkpoint — and `message_subagent` to redirect one that is heading the wrong way rather than letting it finish and redoing the work
- after interruption/cancel, call list_interrupted_subagents then resume_subagent before redoing delegated work
- use ask_user sparingly when the choice is genuinely the user's; otherwise act on a safe default and say so
- when you do ask, put every related question in ONE ask_user call: they are shown together as one card, so asking one at a time costs the user a round trip each

Success criteria:
- gather real SOURCE_JP / translation / CHARACTERS / GLOSSARY / STYLE evidence before editing; issue independent reads together
- prefer surgical edits (`edit_chapter` / `multi_edit_chapter`); dry-run `replace_across_project` first; retranslate only when a full redo is needed
- never copy `N│ ` line prefixes into edits; `old` must match exactly and be unique unless replace_all
- keep Thai idiomatic and consistent with glossary + character voice; preserve `---`, image links, Markdown; no raw kana or leftover furigana parentheses
- canonical CHARACTERS names are spelling anchors, not full-name expansion orders — short JP surfaces stay short unless the source uses the full name; treat `also_called` as exact per-surface mappings and add missing short-surface mappings when fixing expansions
- when a female character uses `僕/ぼく/ボク`, Thai form is always `เรา`, never `ผม` or the transliteration `โบคุ` (overrides stale metadata; do not infer gender from `僕` alone)
- treat `[REVIEW NEEDED]` / reviewer notes as evidence to verify, not automatic truth; categorize (name/honorific, POV/pronoun/register, fidelity/modifier-chain, residue/ruby, glossary, Thai quality, infrastructure) before editing; fix only actionable verified problems; if feedback says a form is correct/acceptable, leave that point alone
- resolve speakers and `自分` from adjacent turns; polite speakers addressing a listener need that listener's established form, not generic `เธอ/แก`
- when changing a recurring name/term, update chapters and lexicon together
- handle mature material neutrally and faithfully

Stop when the request is resolved and changed regions have been re-read/grepped."#
        .to_string()
}

pub(crate) fn refine_tools_vec() -> Vec<Tool> {
    serde_json::from_value(refine_tools_schema()).unwrap_or_default()
}

/// Owns the live chat thread so multi-turn history persists.
pub async fn run_refine_agent(ctx: RefineCtx, mut rx: UnboundedReceiver<RefineControl>) {
    let registry = SubagentRegistry::default();
    let tools = RefineTools::for_ctx(&ctx, registry.clone());
    let mut req = ChatRequest::new(
        ctx.model.model.clone(),
        vec![Message::system(refine_system_prompt(ctx.target_language))],
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
                let mut msg = build_user_message(ctx.default_vol, &turn);
                msg.push_str(approval_directive(ctx.interact.mode()));
                req.messages.push(Message::user(msg));
                let runtime = RefineTurnRuntime {
                    tx: &ctx.tx,
                    registry: &registry,
                    cancel: &ctx.cancel,
                    default_vol: ctx.default_vol,
                    interact: &ctx.interact,
                    steering: &ctx.steering,
                };
                run_refine_turn(ctx.client.as_ref(), &mut req, &tools, &runtime).await;
                // Persist through App, tagged so stale session updates are dropped.
                ctx.tx.send(AppEvent::RefineThreadUpdated {
                    session: current_id.clone(),
                    messages: req.messages[1..].to_vec(),
                });
            }
            RefineControl::Clear => {
                req.messages.truncate(1); // keep the system turn
                ctx.cancel.store(true, Ordering::Relaxed);
                registry.cancel_all();
                clear_steering(&ctx.steering);
                ctx.tx.send(AppEvent::RefineThreadUpdated {
                    session: current_id.clone(),
                    messages: vec![],
                });
            }
            RefineControl::SetModel(m) => req.model = m,
            RefineControl::SwitchSession(id) => {
                ctx.cancel.store(true, Ordering::Relaxed);
                registry.cancel_all();
                clear_steering(&ctx.steering);
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
            RefineControl::Shutdown => {
                registry.cancel_all();
                break;
            }
        }
    }
}

struct RefineTurnRuntime<'a> {
    tx: &'a EventTx,
    registry: &'a SubagentRegistry,
    cancel: &'a AtomicBool,
    default_vol: u32,
    interact: &'a RefineInteract,
    steering: &'a Arc<Mutex<VecDeque<UserTurn>>>,
}

struct RefineCancelled;

async fn wait_cancelled(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Relaxed) {
        tokio::time::sleep(CANCEL_POLL).await;
    }
}

async fn cancellable<T>(
    cancel: &AtomicBool,
    fut: impl Future<Output = T>,
) -> Result<T, RefineCancelled> {
    tokio::select! {
        out = fut => Ok(out),
        _ = wait_cancelled(cancel) => Err(RefineCancelled),
    }
}

fn cancelled_tool_result(name: &str) -> String {
    json!({
        "ok": false,
        "message": format!("tool '{name}' was cancelled by the user")
    })
    .to_string()
}

fn push_cancelled_tool_results(
    req: &mut ChatRequest,
    calls: &[crate::llm::ToolCall],
    tx: &EventTx,
    registry: &SubagentRegistry,
    parent_path: &str,
) {
    for call in calls {
        // A foreground child dies with the future tree it is inside, so nothing
        // it owns will report its end — the parent has to close it out here. A
        // background child is not in that tree and is deliberately left alone.
        if call.function.name == "task" {
            let id = subagent_event_id(parent_path, &call.id);
            registry.finish(
                &id,
                RefineSubagentStatus::Canceled,
                serde_json::to_string(&ToolResult::err("cancelled by the user"))
                    .unwrap_or_default(),
            );
            emit_subagent_terminal(
                tx,
                &id,
                RefineSubagentStatus::Canceled,
                "cancelled by the user".to_string(),
            );
        }
        req.messages.push(Message::tool_result(
            call.id.clone(),
            cancelled_tool_result(&call.function.name),
        ));
    }
}

fn subagent_event_id(parent_path: &str, call_id: &str) -> String {
    if parent_path.is_empty() {
        call_id.to_string()
    } else {
        format!("{parent_path}/{call_id}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParallelToolBatch {
    ReadOnly,
    Task,
}

struct ToolExecution {
    call: ToolCall,
    result: String,
}

fn refine_tool_parallelism() -> usize {
    std::env::var("HONYA_REFINE_PARALLELISM")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        })
}

fn parallel_tool_batch(call: &ToolCall) -> Option<ParallelToolBatch> {
    match call.function.name.as_str() {
        "list_volumes"
        | "list_chapters"
        | "read_chapter"
        | "grep_chapter"
        | "read_meta"
        | "list_flagged_chunks"
        | "read_lexicon"
        | "search_project" => Some(ParallelToolBatch::ReadOnly),
        "replace_across_project" if replace_across_project_is_dry_run(&call.function.arguments) => {
            Some(ParallelToolBatch::ReadOnly)
        }
        "task" => Some(ParallelToolBatch::Task),
        _ => None,
    }
}

fn replace_across_project_is_dry_run(args: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(args)
        .ok()
        .and_then(|v| v.get("dry_run").and_then(|x| x.as_bool()))
        .unwrap_or(false)
}

/// One delegated child, packed so foreground and background take the same path.
struct SubagentRun {
    client: std::sync::Arc<dyn LlmClient>,
    /// Already the child's own executor, carrying its role and event path.
    executor: RefineTools,
    event_id: String,
    task_depth: usize,
    req: ChatRequest,
    initial_tool_call_count: usize,
    checkpoint: SubagentCheckpointState,
    handle: crate::agents::subagent::RunHandle,
}

/// What a caller has to decide before a child can start.
struct LaunchSpec {
    event_id: String,
    role: SubagentRole,
    title: String,
    background: bool,
    req: ChatRequest,
    initial_tool_call_count: usize,
    checkpoint: SubagentCheckpointState,
}

/// Run a child to the end and close it out. The registry and the UI learn how
/// it finished from here and nowhere else, so a foreground child and a
/// backgrounded one report identically.
async fn drive_subagent(run: SubagentRun) -> ToolResult {
    let SubagentRun {
        client,
        executor,
        event_id,
        task_depth,
        req,
        initial_tool_call_count,
        checkpoint,
        handle,
    } = run;
    let checkpoint_id = checkpoint.id().to_string();
    let tx = executor.tx.clone();
    let registry = executor.registry.clone();

    let outcome = run_compacting_tool_loop(
        client.as_ref(),
        req,
        &executor,
        &tx,
        SubagentLoopOptions::new(SUBAGENT_MAX_TOOL_ROUNDS, task_depth, &event_id)
            .initial_tool_call_count(initial_tool_call_count)
            .checkpoint(checkpoint)
            .controls(handle),
    )
    .await;

    let (status, result) = match outcome {
        Ok(outcome) => {
            let report = outcome
                .response
                .choices
                .first()
                .and_then(|c| c.message.content.clone())
                .unwrap_or_default();
            (
                RefineSubagentStatus::Succeeded,
                ToolResult::ok(format!(
                    "sub-agent finished ({} tool call(s)):\n{report}",
                    outcome.tool_calls
                )),
            )
        }
        Err(LlmError::Canceled) => (
            RefineSubagentStatus::Canceled,
            ToolResult::err(format!(
                "sub-agent cancelled; checkpoint `{checkpoint_id}` kept for resume"
            )),
        ),
        Err(e) => (
            RefineSubagentStatus::Failed,
            ToolResult::err(format!(
                "sub-agent interrupted; checkpoint `{checkpoint_id}` kept for resume: {e}"
            )),
        ),
    };

    let json = serde_json::to_string(&result).unwrap_or_default();
    let summary = tool_result_first_line(&json);
    registry.finish(&event_id, status, json);
    emit_subagent_terminal(&tx, &event_id, status, summary);
    result
}

fn emit_subagent_activity(tx: &EventTx, parent_path: &str, _task_depth: usize, activity: String) {
    if parent_path.is_empty() {
        return;
    }
    tx.send(AppEvent::RefineSubagentActivity {
        id: parent_path.to_string(),
        activity,
    });
}

fn emit_subagent_terminal(
    tx: &EventTx,
    event_id: &str,
    status: RefineSubagentStatus,
    summary: String,
) {
    if event_id.is_empty() {
        return;
    }
    tx.send(AppEvent::RefineSubagentFinished {
        id: event_id.to_string(),
        status,
        summary,
    });
}

fn tool_call_failure_payload(call: &ToolCall, e: anyhow::Error) -> String {
    json!({
        "ok": false,
        "message": format!("tool '{}' failed: {e}", call.function.name)
    })
    .to_string()
}

async fn execute_tool_call_result(executor: &dyn ToolExecutor, call: &ToolCall) -> String {
    match executor.execute_call(call).await {
        Ok(payload) => payload,
        Err(e) => tool_call_failure_payload(call, e),
    }
}

async fn execute_tool_calls_parallel(
    executor: &dyn ToolExecutor,
    calls: &[ToolCall],
) -> Vec<ToolExecution> {
    let limit = refine_tool_parallelism().min(calls.len()).max(1);
    stream::iter(calls.iter().cloned())
        .map(|call| async move {
            let result = execute_tool_call_result(executor, &call).await;
            ToolExecution { call, result }
        })
        .buffered(limit)
        .collect()
        .await
}

fn tool_activity(call: &ToolCall) -> String {
    let action = match call.function.name.as_str() {
        "list_volumes" => "listing volumes",
        "list_chapters" => "listing chapters",
        "read_chapter" => "reading chapter",
        "grep_chapter" => "searching chapter",
        "read_meta" => "reading metadata",
        "list_flagged_chunks" => "checking review-needed chunks",
        "update_plan" => "updating plan",
        "read_lexicon" => "reading lexicon",
        "search_project" => "searching project",
        "upsert_character" => "updating character roster",
        "merge_character" => "merging characters",
        "remove_character" => "removing character",
        "upsert_glossary_term" => "updating glossary",
        "remove_glossary_term" => "removing glossary term",
        "set_recap" => "updating recap",
        "set_chapter_summary" => "updating chapter summary",
        "set_synopsis" => "updating synopsis",
        "append_style_note" => "updating style notes",
        "add_style_example" => "adding style example",
        "add_continuity_note" => "adding continuity note",
        "replace_chapter_text" => "rewriting chapter",
        "edit_chapter" => "editing chapter",
        "multi_edit_chapter" => "editing chapter",
        "replace_across_project" => "replacing across project",
        "retranslate_chapter" => "queueing retranslation",
        "refine_chapter_with_feedback" => "queueing focused retranslation",
        "task" => "waiting on nested sub-agent",
        "ask_user" => "waiting for user",
        other => other,
    };
    let detail = summarize_args(&call.function.arguments);
    if detail.is_empty() || detail == "{}" {
        action.to_string()
    } else {
        format!("{action} · {detail}")
    }
}

fn tool_batch_activity(calls: &[ToolCall], kind: ParallelToolBatch) -> String {
    if calls.len() == 1 {
        return tool_activity(&calls[0]);
    }
    match kind {
        ParallelToolBatch::ReadOnly => format!("running {} read-only tools", calls.len()),
        ParallelToolBatch::Task => format!("waiting on {} nested sub-agents", calls.len()),
    }
}

fn subagent_rate_limit_wait(retry_after: u64) -> u64 {
    retry_after.min(SUBAGENT_RATE_LIMIT_MAX_SLEEP_SECS)
}

async fn subagent_chat_resuming_after_rate_limit(
    client: &dyn LlmClient,
    req: &ChatRequest,
    tx: &EventTx,
    task_depth: usize,
    parent_path: &str,
    phase: &str,
) -> crate::llm::client::Result<ChatResponse> {
    let mut retry = 0u32;
    loop {
        match client.chat(req).await {
            Ok(resp) => return Ok(resp),
            Err(LlmError::RateLimited {
                retry_after,
                message,
            }) => {
                retry += 1;
                if retry > SUBAGENT_RATE_LIMIT_MAX_RETRIES {
                    return Err(LlmError::RateLimited {
                        retry_after,
                        message: format!(
                            "rate limited {SUBAGENT_RATE_LIMIT_MAX_RETRIES} times while {phase}: {message}"
                        ),
                    });
                }
                let wait = subagent_rate_limit_wait(retry_after);
                let msg = if message.trim().is_empty() {
                    format!("rate limited while {phase}; retrying in {wait}s")
                } else {
                    format!(
                        "rate limited while {phase}; retrying in {wait}s · {}",
                        summarize_args(&message)
                    )
                };
                emit_subagent_activity(tx, parent_path, task_depth, msg.clone());
                tx.send(AppEvent::Log {
                    level: LogLevel::Warn,
                    msg: format!("Refine sub-agent {msg} (retry {retry}/{SUBAGENT_RATE_LIMIT_MAX_RETRIES})"),
                });
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

struct SubagentCheckpointState {
    root: PathBuf,
    checkpoint: crate::workspace::refine_session::SubagentCheckpoint,
}

impl SubagentCheckpointState {
    fn for_request(
        root: PathBuf,
        id: String,
        spec: SubagentSpec,
        req: &ChatRequest,
        max_rounds: usize,
    ) -> Self {
        let now = chrono::Utc::now();
        Self {
            root,
            checkpoint: crate::workspace::refine_session::SubagentCheckpoint {
                id,
                task: spec.task,
                scope: spec.scope,
                role: spec.role.label().to_string(),
                model: req.model.clone(),
                reasoning: req.reasoning.clone(),
                messages: req.messages.clone(),
                tool_call_count: 0,
                max_rounds,
                depth: spec.depth,
                created: now,
                updated: now,
            },
        }
    }

    fn from_existing(
        root: PathBuf,
        checkpoint: crate::workspace::refine_session::SubagentCheckpoint,
    ) -> Self {
        Self { root, checkpoint }
    }

    fn id(&self) -> &str {
        &self.checkpoint.id
    }

    fn save(&mut self, req: &ChatRequest, tool_call_count: usize, tx: &EventTx) {
        self.checkpoint.model = req.model.clone();
        self.checkpoint.reasoning = req.reasoning.clone();
        self.checkpoint.messages = req.messages.clone();
        self.checkpoint.tool_call_count = tool_call_count;
        self.checkpoint.updated = chrono::Utc::now();
        if let Err(e) =
            crate::workspace::refine_session::save_subagent(&self.root, &self.checkpoint)
        {
            tx.send(AppEvent::Log {
                level: LogLevel::Warn,
                msg: format!("could not save Refine sub-agent checkpoint: {e}"),
            });
        }
    }

    fn delete(self, tx: &EventTx) {
        if let Err(e) =
            crate::workspace::refine_session::delete_subagent(&self.root, &self.checkpoint.id)
        {
            tx.send(AppEvent::Log {
                level: LogLevel::Warn,
                msg: format!("could not remove completed Refine sub-agent checkpoint: {e}"),
            });
        }
    }
}

fn save_subagent_checkpoint(
    checkpoint: Option<&mut SubagentCheckpointState>,
    req: &ChatRequest,
    tool_call_count: usize,
    tx: &EventTx,
) {
    if let Some(checkpoint) = checkpoint {
        checkpoint.save(req, tool_call_count, tx);
    }
}

fn finish_subagent_checkpoint(checkpoint: &mut Option<SubagentCheckpointState>, tx: &EventTx) {
    if let Some(checkpoint) = checkpoint.take() {
        checkpoint.delete(tx);
    }
}

struct SubagentLoopOptions {
    max_rounds: usize,
    task_depth: usize,
    parent_path: String,
    initial_tool_call_count: usize,
    checkpoint: Option<SubagentCheckpointState>,
    /// Stops this child between rounds. A background child is not inside the
    /// parent turn's future tree, so dropping that tree cannot reach it.
    cancel: Option<Arc<AtomicBool>>,
    /// Abandons the model call already in flight, for an interjected message.
    interrupt: Option<Arc<AtomicBool>>,
    inbox: Option<tokio::sync::mpsc::UnboundedReceiver<ChildMessage>>,
}

impl SubagentLoopOptions {
    fn new(max_rounds: usize, task_depth: usize, parent_path: impl Into<String>) -> Self {
        Self {
            max_rounds,
            task_depth,
            parent_path: parent_path.into(),
            initial_tool_call_count: 0,
            checkpoint: None,
            cancel: None,
            interrupt: None,
            inbox: None,
        }
    }

    fn controls(mut self, handle: crate::agents::subagent::RunHandle) -> Self {
        self.cancel = Some(handle.cancel);
        self.interrupt = Some(handle.interrupt);
        self.inbox = Some(handle.inbox);
        self
    }

    fn initial_tool_call_count(mut self, count: usize) -> Self {
        self.initial_tool_call_count = count;
        self
    }

    fn checkpoint(mut self, checkpoint: SubagentCheckpointState) -> Self {
        self.checkpoint = Some(checkpoint);
        self
    }
}

fn subagent_budget_summary(max_rounds: usize, tool_call_count: usize) -> String {
    format!(
        "Sub-agent reached the {max_rounds}-round tool budget after {tool_call_count} tool call(s). It stopped to avoid an infinite loop. Treat any edits already written to disk as applied, then continue with a smaller follow-up task if more work remains."
    )
}

fn synthetic_subagent_budget_response(max_rounds: usize, tool_call_count: usize) -> ChatResponse {
    ChatResponse {
        id: None,
        model: None,
        service_tier: None,
        usage: None,
        choices: vec![Choice {
            index: 0,
            finish_reason: Some("tool_budget".to_string()),
            message: ResponseMessage {
                role: Some("assistant".to_string()),
                content: Some(subagent_budget_summary(max_rounds, tool_call_count)),
                tool_calls: None,
            },
        }],
    }
}

/// Streams one user turn and executes tool calls until the model stops.
async fn run_refine_turn(
    client: &dyn LlmClient,
    req: &mut ChatRequest,
    tools: &dyn ToolExecutor,
    runtime: &RefineTurnRuntime<'_>,
) {
    let tx = runtime.tx;
    let mut turn_tool_summaries = Vec::new();
    let mut latest_plan: Option<Vec<PlanStep>> = None;

    for _round in 0..MAX_TOOL_ROUNDS {
        if runtime.cancel.load(Ordering::Relaxed) {
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
        let resp = match cancellable(runtime.cancel, client.chat_stream(req, &mut on_delta)).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                tx.send(AppEvent::RefineError { msg: e.to_string() });
                return;
            }
            Err(_) => {
                tx.send(AppEvent::RefineMessageDone);
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
            if let Some(c) = content.filter(|c| !c.trim().is_empty()) {
                req.messages.push(Message::assistant(c));
            }
            if drain_steering(
                req,
                runtime.default_vol,
                runtime.interact,
                runtime.steering,
                tx,
            ) > 0
            {
                continue;
            }
            if req
                .messages
                .last()
                .is_some_and(|m| m.role != Role::Assistant)
                && !turn_tool_summaries.is_empty()
            {
                let summary = fallback_final_summary(&turn_tool_summaries);
                tx.send(AppEvent::RefineDelta {
                    delta: summary.clone(),
                });
                req.messages.push(Message::assistant(summary));
            }
            clear_completed_plan(tx, latest_plan.as_deref());
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

        let mut idx = 0;
        while idx < tool_calls.len() {
            if runtime.cancel.load(Ordering::Relaxed) {
                push_cancelled_tool_results(req, &tool_calls[idx..], tx, runtime.registry, "");
                tx.send(AppEvent::RefineMessageDone);
                return;
            }

            if let Some(kind) = parallel_tool_batch(&tool_calls[idx]) {
                let start = idx;
                let mut end = idx + 1;
                while end < tool_calls.len() && parallel_tool_batch(&tool_calls[end]) == Some(kind)
                {
                    end += 1;
                }

                for call in &tool_calls[start..end] {
                    tx.send(AppEvent::RefineToolInvoked {
                        tool: call.function.name.clone(),
                        summary: summarize_args(&call.function.arguments),
                    });
                }

                let results = match cancellable(
                    runtime.cancel,
                    execute_tool_calls_parallel(tools, &tool_calls[start..end]),
                )
                .await
                {
                    Ok(results) => results,
                    Err(_) => {
                        push_cancelled_tool_results(req, &tool_calls[start..], tx, runtime.registry, "");
                        tx.send(AppEvent::RefineMessageDone);
                        return;
                    }
                };

                for ToolExecution { call, result } in results {
                    if let Some(summary) = tool_summary_for_final(&call.function.name, &result) {
                        turn_tool_summaries.push(summary);
                    }
                    req.messages.push(Message::tool_result(call.id, result));
                }

                idx = end;
                continue;
            }

            let call = &tool_calls[idx];
            if call.function.name == "update_plan" {
                latest_plan = parse_plan_steps(&call.function.arguments);
            }
            // Plan calls render in the pinned panel, not the transcript.
            if call.function.name != "update_plan" {
                tx.send(AppEvent::RefineToolInvoked {
                    tool: call.function.name.clone(),
                    summary: summarize_args(&call.function.arguments),
                });
            }
            let result =
                match cancellable(runtime.cancel, execute_tool_call_result(tools, call)).await {
                    Ok(result) => result,
                    Err(_) => {
                        push_cancelled_tool_results(req, &tool_calls[idx..], tx, runtime.registry, "");
                        tx.send(AppEvent::RefineMessageDone);
                        return;
                    }
                };
            if let Some(summary) = tool_summary_for_final(&call.function.name, &result) {
                turn_tool_summaries.push(summary);
            }
            req.messages
                .push(Message::tool_result(call.id.clone(), result));
            idx += 1;
        }

        drain_steering(
            req,
            runtime.default_vol,
            runtime.interact,
            runtime.steering,
            tx,
        );
    }

    tx.send(AppEvent::RefineError {
        msg: format!("stopped after {MAX_TOOL_ROUNDS} tool rounds"),
    });
    tx.send(AppEvent::RefineMessageDone);
}

/// A child's message queue, split by when each message is allowed to land.
struct Inbox {
    rx: Option<tokio::sync::mpsc::UnboundedReceiver<ChildMessage>>,
    /// `Delivery::Queue` messages, held until the child would otherwise stop.
    held: Vec<String>,
}

impl Inbox {
    fn new(rx: Option<tokio::sync::mpsc::UnboundedReceiver<ChildMessage>>) -> Self {
        Self { rx, held: Vec::new() }
    }

    /// Messages to inject at this round boundary.
    fn drain_now(&mut self) -> Vec<String> {
        let Some(rx) = self.rx.as_mut() else {
            return Vec::new();
        };
        let mut now = Vec::new();
        while let Ok(m) = rx.try_recv() {
            match m.delivery {
                Delivery::Queue => self.held.push(m.text),
                Delivery::Steer | Delivery::Interject => now.push(m.text),
            }
        }
        now
    }

    /// Messages that were waiting for the child to think it was finished.
    fn take_held(&mut self) -> Vec<String> {
        std::mem::take(&mut self.held)
    }
}

fn parent_message(text: &str) -> String {
    format!("[Message from the parent agent: {text}]")
}

async fn run_compacting_tool_loop(
    client: &dyn LlmClient,
    mut req: ChatRequest,
    executor: &dyn ToolExecutor,
    tx: &EventTx,
    options: SubagentLoopOptions,
) -> crate::llm::client::Result<ToolLoopOutcome> {
    let mut usage = Usage::default();
    let max_rounds = options.max_rounds;
    let task_depth = options.task_depth;
    let parent_path = options.parent_path;
    let parent_path = parent_path.as_str();
    let mut checkpoint = options.checkpoint;
    let mut tool_call_count = options.initial_tool_call_count;
    let cancel = options.cancel;
    let interrupt = options.interrupt;
    let mut inbox = Inbox::new(options.inbox);
    for _round in 0..max_rounds {
        if cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
            save_subagent_checkpoint(checkpoint.as_mut(), &req, tool_call_count, tx);
            return Err(LlmError::Canceled);
        }
        for text in inbox.drain_now() {
            req.messages.push(Message::user(parent_message(&text)));
        }
        maybe_compact(&mut req, tx, false);
        save_subagent_checkpoint(checkpoint.as_mut(), &req, tool_call_count, tx);

        emit_subagent_activity(tx, parent_path, task_depth, "thinking".to_string());
        let chat = subagent_chat_resuming_after_rate_limit(
            client,
            &req,
            tx,
            task_depth,
            parent_path,
            "thinking",
        );
        let resp = match interrupt.as_deref() {
            // An interjected message abandons the call in flight; the next
            // round picks the message up at the top of the loop.
            Some(flag) => match cancellable(flag, chat).await {
                Ok(r) => r?,
                Err(RefineCancelled) => {
                    flag.store(false, Ordering::Relaxed);
                    continue;
                }
            },
            None => chat.await?,
        };
        if let Some(u) = &resp.usage {
            usage.add(u);
        }

        let choice = resp.choices.first().ok_or(LlmError::EmptyChoices)?;
        let tool_calls: Vec<ToolCall> = choice.message.tool_calls.clone().unwrap_or_default();

        if tool_calls.is_empty() {
            if choice
                .message
                .content
                .as_deref()
                .is_some_and(|c| !c.trim().is_empty())
            {
                // A queued message was waiting for exactly this moment: the
                // child thinks it is done, and is told there is more.
                let held = inbox.take_held();
                if !held.is_empty() {
                    req.messages.push(Message {
                        role: Role::Assistant,
                        content: choice.message.content.clone(),
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                    });
                    for text in held {
                        req.messages.push(Message::user(parent_message(&text)));
                    }
                    continue;
                }
                finish_subagent_checkpoint(&mut checkpoint, tx);
                return Ok(ToolLoopOutcome {
                    response: resp,
                    usage,
                    tool_calls: tool_call_count,
                });
            }
            req.messages.push(Message::user(
                "Return a concise final report without using tools.",
            ));
            continue;
        }
        tool_call_count += tool_calls.len();

        req.messages.push(Message {
            role: Role::Assistant,
            content: choice.message.content.clone(),
            tool_calls: Some(tool_calls.clone()),
            tool_call_id: None,
            name: None,
        });

        let mut idx = 0;
        while idx < tool_calls.len() {
            if let Some(kind) = parallel_tool_batch(&tool_calls[idx]) {
                let start = idx;
                let mut end = idx + 1;
                while end < tool_calls.len() && parallel_tool_batch(&tool_calls[end]) == Some(kind)
                {
                    end += 1;
                }

                emit_subagent_activity(
                    tx,
                    parent_path,
                    task_depth,
                    tool_batch_activity(&tool_calls[start..end], kind),
                );
                let results = execute_tool_calls_parallel(executor, &tool_calls[start..end]).await;
                for ToolExecution { call, result } in results {
                    req.messages.push(Message::tool_result(call.id, result));
                }

                idx = end;
                continue;
            }

            let call = &tool_calls[idx];
            emit_subagent_activity(tx, parent_path, task_depth, tool_activity(call));
            let result = execute_tool_call_result(executor, call).await;
            req.messages
                .push(Message::tool_result(call.id.clone(), result));
            idx += 1;
        }
        save_subagent_checkpoint(checkpoint.as_mut(), &req, tool_call_count, tx);
    }

    tx.send(AppEvent::Log {
        level: LogLevel::Warn,
        msg: format!("Refine sub-agent reached {max_rounds} tool rounds; requesting final report"),
    });
    req.tools = None;
    req.tool_choice = None;
    req.messages.push(Message::user(format!(
        "You have reached the {max_rounds}-round tool budget for this delegated task. Do not call any more tools. Return a concise final report now: what you completed, what files or chapters you changed, and what remains incomplete."
    )));

    for _ in 0..SUBAGENT_FINALIZE_ROUNDS {
        maybe_compact(&mut req, tx, false);
        save_subagent_checkpoint(checkpoint.as_mut(), &req, tool_call_count, tx);

        emit_subagent_activity(
            tx,
            parent_path,
            task_depth,
            "preparing final report".to_string(),
        );
        let resp = subagent_chat_resuming_after_rate_limit(
            client,
            &req,
            tx,
            task_depth,
            parent_path,
            "preparing final report",
        )
        .await?;
        if let Some(u) = &resp.usage {
            usage.add(u);
        }

        let choice = resp.choices.first().ok_or(LlmError::EmptyChoices)?;
        let tool_calls: Vec<ToolCall> = choice.message.tool_calls.clone().unwrap_or_default();

        if tool_calls.is_empty() {
            if choice
                .message
                .content
                .as_deref()
                .is_some_and(|c| !c.trim().is_empty())
            {
                finish_subagent_checkpoint(&mut checkpoint, tx);
                return Ok(ToolLoopOutcome {
                    response: resp,
                    usage,
                    tool_calls: tool_call_count,
                });
            }
            req.messages.push(Message::user(
                "Return a non-empty final report without using tools.",
            ));
            continue;
        }

        req.messages.push(Message {
            role: Role::Assistant,
            content: choice.message.content.clone(),
            tool_calls: Some(tool_calls.clone()),
            tool_call_id: None,
            name: None,
        });
        for call in &tool_calls {
            req.messages.push(Message::tool_result(
                call.id.clone(),
                json!({
                    "ok": false,
                    "message": format!(
                        "tool '{}' was not run because the sub-agent reached its tool-round budget; provide a final report instead",
                        call.function.name
                    )
                })
                .to_string(),
            ));
        }
        req.messages.push(Message::user(
            "Stop using tools. Return the final report in plain text now.",
        ));
    }

    Ok(ToolLoopOutcome {
        response: synthetic_subagent_budget_response(max_rounds, tool_call_count),
        usage,
        tool_calls: tool_call_count,
    })
}

fn summarize_args(args_json: &str) -> String {
    let flat: String = args_json.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 70 {
        flat
    } else {
        flat.chars().take(70).collect::<String>() + "…"
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

fn drain_steering(
    req: &mut ChatRequest,
    default_vol: u32,
    interact: &RefineInteract,
    steering: &Arc<Mutex<VecDeque<UserTurn>>>,
    tx: &EventTx,
) -> usize {
    let mut turns = Vec::new();
    if let Ok(mut queue) = steering.lock() {
        while let Some(turn) = queue.pop_front() {
            turns.push(turn);
        }
    }

    let count = turns.len();
    for turn in turns {
        let mut msg = build_user_message(default_vol, &turn);
        match &turn.from {
            Some(id) => msg.push_str(&format!(
                "\n\n[Mid-run report from sub-agent `{id}`, sent while it was still working. It is evidence from a delegated task, not an instruction from the user.]"
            )),
            None => msg.push_str(
                "\n\n[Mid-run steering: the user sent this while the current turn was running. Apply it to the remaining work if it is still relevant; if completed work cannot be changed, say so in the final summary.]",
            ),
        }
        msg.push_str(approval_directive(interact.mode()));
        req.messages.push(Message::user(msg));
    }

    if count > 0 {
        tx.send(AppEvent::RefineToolInvoked {
            tool: "steering".to_string(),
            summary: format!("added {count} queued user instruction(s)"),
        });
    }

    count
}

fn clear_steering(steering: &Arc<Mutex<VecDeque<UserTurn>>>) {
    if let Ok(mut queue) = steering.lock() {
        queue.clear();
    }
}

fn tool_summary_for_final(name: &str, result_json: &str) -> Option<String> {
    if name == "update_plan" {
        return None;
    }
    let label = name.replace('_', " ");
    let msg = tool_result_message(result_json);
    let first_line = msg.lines().next().unwrap_or("").trim();
    if first_line.is_empty() {
        None
    } else {
        Some(format!("{label}: {first_line}"))
    }
}

/// A finished run's stored tool-result JSON, as a value the report can carry.
fn subagent_report(json: &str) -> serde_json::Value {
    let mut out = serde_json::from_str::<serde_json::Value>(json)
        .unwrap_or_else(|_| json!({ "message": json }));
    if let Some(map) = out.as_object_mut() {
        map.insert("status".into(), json!("finished"));
    }
    out
}

fn tool_result_message(result_json: &str) -> String {
    serde_json::from_str::<ToolResult>(result_json)
        .map(|r| {
            let prefix = if r.ok { "" } else { "failed: " };
            format!("{prefix}{}", r.message)
        })
        .unwrap_or_else(|_| result_json.to_string())
}

fn tool_result_first_line(result_json: &str) -> String {
    let msg = tool_result_message(result_json);
    let first = msg.lines().next().unwrap_or("").trim();
    if first.is_empty() {
        "(no summary returned)".to_string()
    } else {
        truncate_chars(first, 90)
    }
}

fn fallback_final_summary(summaries: &[String]) -> String {
    let mut out = String::from("Done. Summary:\n");
    for item in summaries.iter().take(6) {
        out.push_str("- ");
        out.push_str(item.trim());
        out.push('\n');
    }
    if summaries.len() > 6 {
        out.push_str(&format!(
            "- ...and {} more action(s)\n",
            summaries.len() - 6
        ));
    }
    out.trim_end().to_string()
}

fn normalize_plan_steps(steps: Vec<PlanStep>) -> Vec<PlanStep> {
    steps
        .into_iter()
        .filter(|s| !s.step.trim().is_empty())
        .take(20)
        .collect()
}

fn parse_plan_steps(args_json: &str) -> Option<Vec<PlanStep>> {
    serde_json::from_str::<UpdatePlanArgs>(args_json)
        .ok()
        .map(|a| normalize_plan_steps(a.steps))
}

fn emit_plan_update(tx: &EventTx, owner: Option<&str>, steps: Vec<PlanStep>) {
    match owner {
        Some(id) => tx.send(AppEvent::RefineSubagentPlanUpdated {
            id: id.to_string(),
            steps,
        }),
        None => tx.send(AppEvent::RefinePlanUpdated { steps }),
    }
}

fn clear_completed_plan(tx: &EventTx, steps: Option<&[PlanStep]>) {
    let Some(steps) = steps else {
        return;
    };
    if !steps.is_empty()
        && steps
            .iter()
            .all(|s| s.status == crate::model::PlanStepStatus::Completed)
    {
        tx.send(AppEvent::RefinePlanUpdated { steps: Vec::new() });
    }
}

/// Per-turn edit-approval instruction; injected because mode can change live.
fn approval_directive(mode: ApprovalMode) -> &'static str {
    match mode {
        ApprovalMode::Auto => {
            "\n\n[Approval mode: AUTO — apply routine, clearly-wanted fixes directly, but call the ask_user tool to confirm BEFORE any edit the user might not consent to: risky, bulky, subjective, or destructive changes (e.g. rewriting many chapters, changing a character's name everywhere, deleting content).]"
        }
        ApprovalMode::Ask => {
            "\n\n[Approval mode: ASK — the app pauses every edit for the user to accept or reject, so just proceed and make the edits normally.]"
        }
        ApprovalMode::Always => {
            "\n\n[Approval mode: ALWAYS-APPROVE — apply edits directly and do not ask for approval.]"
        }
    }
}

fn build_user_message(default_vol: u32, turn: &UserTurn) -> String {
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
        turn.text, default_vol
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
    #[serde(default = "default_true", alias = "include_th")]
    include_translation: bool,
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
    #[serde(alias = "thai_name")]
    translated_name: String,
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
    also_called: Vec<crate::model::AltName>,
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
    #[serde(alias = "thai_term")]
    translated_term: String,
    #[serde(default)]
    romaji: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    gloss: Option<String>,
    #[serde(default)]
    policy: Option<TermPolicy>,
    #[serde(default, alias = "forbidden_thai")]
    forbidden_translations: Vec<String>,
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
    #[serde(alias = "thai")]
    translated_synopsis: String,
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
    #[serde(alias = "th")]
    translated_text: String,
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
        return Err(EditError::Other(
            "`old` and `new` are identical".to_string(),
        ));
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
    dispatch_refine_tool_for_owner(root, default_vol, tx, None, name, args_json).await
}

async fn dispatch_refine_tool_for_owner(
    root: &Path,
    default_vol: u32,
    tx: &EventTx,
    plan_owner: Option<&str>,
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
            if a.include_translation {
                let translated =
                    translation::prose_only(&translation::read_translated(&w, a.ch).await);
                let (numbered, total, to) = numbered_window(&translated, offset, limit);
                data.insert("translation".into(), json!(numbered));
                data.insert("translation_total_lines".into(), json!(total));
                data.insert(
                    "translation_from_line".into(),
                    json!(offset.min(total.max(1))),
                );
                data.insert("translation_to_line".into(), json!(to));
                data.insert("translation_truncated".into(), json!(to < total));
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
            let side = match a.side.as_deref().unwrap_or("translation") {
                "translation" | "translated" | "th" => "translation",
                "jp" => "jp",
                "both" => "both",
                other => return ToolResult::err(format!("invalid side: {other}")),
            };
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
            if matches!(side, "translation" | "both") {
                let translated =
                    translation::prose_only(&translation::read_translated(&w, a.ch).await);
                data.insert("translation".into(), json!(grep(&translated)));
            }
            if matches!(side, "jp" | "both") {
                let jp = std::fs::read_to_string(w.raw(a.ch)).unwrap_or_default();
                data.insert("japanese".into(), json!(grep(&jp)));
            }
            ToolResult::data(
                format!("grep chapter {}", a.ch),
                serde_json::Value::Object(data),
            )
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
                data.insert("translated_synopsis".into(), json!(vd.translated_synopsis));
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
            let steps = normalize_plan_steps(a.steps);
            if steps.is_empty() {
                emit_plan_update(tx, plan_owner, steps);
                return ToolResult::ok("plan cleared");
            }
            emit_plan_update(tx, plan_owner, steps.clone());
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
                    let translated =
                        translation::prose_only(&translation::read_translated(&w, c.number).await);
                    // Find AND slice on the SAME (lowercased) string: a byte index
                    // from the lowercased text is not a valid boundary in the original
                    // when lowercasing changes a char's byte length (would panic).
                    let translated_lower = translated.to_lowercase();
                    if let Some(pos) = translated_lower.find(&needle) {
                        let start = translated_lower[..pos]
                            .char_indices()
                            .rev()
                            .nth(40)
                            .map(|(i, _)| i)
                            .unwrap_or(0);
                        let snippet: String = translated_lower[start..].chars().take(100).collect();
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
                translated_name: a.translated_name.clone(),
                romaji: a.romaji,
                gender: a.gender,
                honorific: a.honorific,
                speech_style: a.speech_style,
                relationships: a.relationships,
                aliases: a.aliases,
                also_called: a.also_called,
                notes: a.notes,
                first_seen_chapter: a.first_seen_chapter,
            };
            match characters::upsert(&w, character) {
                Ok(outcome) => {
                    emit_edit(
                        tx,
                        "character",
                        &format!("{} → {}", a.jp_name, a.translated_name),
                    );
                    let msg = match outcome {
                        characters::CharacterUpsertOutcome::Merged { into_id } => {
                            format!("merged {} into {into_id}", a.translated_name)
                        }
                        characters::CharacterUpsertOutcome::InsertedWithCandidates {
                            id,
                            candidates,
                        } => format!(
                            "saved {} ({id}); possible duplicates: {}",
                            a.translated_name,
                            candidates.join(", ")
                        ),
                        characters::CharacterUpsertOutcome::Inserted => {
                            format!("saved character {} ({id})", a.translated_name)
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
                translated_term: a.translated_term.clone(),
                romaji: a.romaji,
                category: a.category,
                gloss: a.gloss,
                policy: a.policy,
                forbidden_translations: a.forbidden_translations,
                context_rule: a.context_rule,
                protected: None,
                do_not_translate: a.do_not_translate,
                first_seen_chapter: a.first_seen_chapter,
            };
            match glossary::upsert(&w, term) {
                Ok(()) => {
                    emit_edit(
                        tx,
                        "glossary",
                        &format!("{} → {}", a.jp_term, a.translated_term),
                    );
                    ToolResult::ok(format!("saved term {} → {}", a.jp_term, a.translated_term))
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
            match volume::set_synopsis(&w, &raw, &a.translated_synopsis) {
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
                translated_text: a.translated_text,
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
            let updated = translation::clear_review_needed_for_changed_chunks(&content, &updated);
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
            let original = content.clone();
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
            content = translation::clear_review_needed_for_changed_chunks(&original, &content);
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
    depth: usize,
    path: String,
    interact: RefineInteract,
    target_language: crate::model::TargetLanguage,
    /// What this executor is allowed to run. The root agent is `General`; a
    /// child carries whatever role spawned it.
    role: SubagentRole,
    /// Every run this session has started. Shared with the agent task, not the
    /// turn, which is what lets a background child outlive its turn.
    registry: SubagentRegistry,
    /// The root agent's mid-run steering queue, so a child can report upward.
    steering: Arc<Mutex<VecDeque<UserTurn>>>,
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
            depth: 0,
            path: String::new(),
            interact: RefineInteract::default(),
            target_language: crate::model::TargetLanguage::Thai,
            role: SubagentRole::General,
            registry: SubagentRegistry::default(),
            steering: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    #[cfg(test)]
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
            depth: 0,
            path: String::new(),
            interact,
            target_language: crate::model::TargetLanguage::Thai,
            role: SubagentRole::General,
            registry: SubagentRegistry::default(),
            steering: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    #[cfg(test)]
    fn sharing(
        mut self,
        registry: SubagentRegistry,
        steering: Arc<Mutex<VecDeque<UserTurn>>>,
    ) -> Self {
        self.registry = registry;
        self.steering = steering;
        self
    }

    /// The root executor for one Refine session.
    pub fn for_ctx(ctx: &RefineCtx, registry: SubagentRegistry) -> Self {
        Self {
            root: ctx.root.clone(),
            default_vol: ctx.default_vol,
            tx: ctx.tx.clone(),
            client: Some(ctx.client.clone()),
            model: ctx.model.clone(),
            can_spawn: true,
            depth: 0,
            path: String::new(),
            interact: ctx.interact.clone(),
            target_language: ctx.target_language,
            role: SubagentRole::General,
            registry,
            steering: ctx.steering.clone(),
        }
    }

    fn child(&self, path: String, role: SubagentRole) -> Self {
        Self {
            root: self.root.clone(),
            default_vol: self.default_vol,
            tx: self.tx.clone(),
            client: self.client.clone(),
            model: self.model.clone(),
            can_spawn: self.can_spawn
                && self.depth + 1 < MAX_SUBAGENT_DEPTH
                && role.allows("task"),
            depth: self.depth + 1,
            path,
            interact: self.interact.clone(),
            target_language: self.target_language,
            role,
            registry: self.registry.clone(),
            steering: self.steering.clone(),
        }
    }

    async fn request_approval(&self, summary: String, diff: String) -> bool {
        let (id, rx) = self.interact.open();
        self.tx
            .send(AppEvent::RefineApprovalRequest { id, summary, diff });
        rx.await.unwrap_or_default() == "approve"
    }

    async fn request_decision(&self, questions: Vec<RefineQuestion>) -> String {
        let (id, rx) = self.interact.open();
        self.tx
            .send(AppEvent::RefineDecisionRequest { id, questions });
        rx.await.unwrap_or_default()
    }

    async fn run_ask_user(&self, arguments_json: &str) -> ToolResult {
        let questions = match parse_ask_user(arguments_json) {
            Ok(q) => q,
            Err(e) => return ToolResult::err(e),
        };
        let single = questions.len() == 1;
        let raw = self.request_decision(questions.clone()).await;
        let answers = decode_answers(&raw, questions.len());
        if answers.iter().all(|a| a.trim().is_empty()) {
            return ToolResult::err("the user dismissed the question without answering");
        }
        // A model reads prose, so the array the UI sends back is formatted here
        // rather than handed over raw. One question keeps the old flat line, so
        // nothing that already worked reads differently.
        if single {
            return ToolResult::ok(format!("user answered: {}", answers[0].trim()));
        }
        let mut out = String::from("user answered:");
        for (i, q) in questions.iter().enumerate() {
            let a = answers.get(i).map(|s| s.trim()).unwrap_or("");
            out.push_str(&format!(
                "\n{}. {}\n   → {}",
                i + 1,
                q.question.trim(),
                if a.is_empty() { "(skipped)" } else { a }
            ));
        }
        ToolResult::ok(out)
    }

    /// The run that spawned this one, or `None` when the parent is the root
    /// agent (whose own inbox is the steering queue).
    fn parent_run_id(&self) -> Option<&str> {
        self.path.rsplit_once('/').map(|(head, _)| head)
    }

    async fn collect_subagents(&self, arguments_json: &str) -> ToolResult {
        #[derive(serde::Deserialize)]
        struct Args {
            ids: Vec<String>,
            #[serde(default)]
            timeout_ms: u64,
        }
        let a: Args = match serde_json::from_str(arguments_json) {
            Ok(a) => a,
            Err(e) => return ToolResult::err(format!("bad subagent_output args: {e}")),
        };
        if a.ids.is_empty() {
            return ToolResult::err("subagent_output needs at least one id");
        }
        let ids: Vec<String> = a.ids.into_iter().take(20).collect();
        let budget = std::time::Duration::from_millis(a.timeout_ms.min(3_600_000));
        let deadline = tokio::time::Instant::now() + budget;

        let mut out = serde_json::Map::new();
        let mut pending = Vec::new();
        for id in ids {
            match self.registry.poll(&id) {
                Collected::Done(json) => {
                    out.insert(id, subagent_report(&json));
                }
                Collected::Running => pending.push(id),
                Collected::Unknown => {
                    out.insert(id, json!({ "status": "unknown" }));
                }
            }
        }

        // One shared deadline across the waits, so `timeout_ms` means what it
        // says however many ids were given.
        for id in pending {
            let done = match self.registry.waiter(&id) {
                None => self.registry.poll(&id),
                Some(rx) => {
                    let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                    match tokio::time::timeout(left, rx).await {
                        Ok(Ok(json)) => Collected::Done(json),
                        _ => Collected::Running,
                    }
                }
            };
            match done {
                Collected::Done(json) => {
                    out.insert(id, subagent_report(&json));
                }
                _ => {
                    out.insert(id, json!({ "status": "running" }));
                }
            }
        }

        let still = out
            .values()
            .filter(|v| v.get("status").and_then(|s| s.as_str()) == Some("running"))
            .count();
        let summary = if still == 0 {
            format!("collected {} sub-agent report(s)", out.len())
        } else {
            format!("{} of {} sub-agent(s) still running", still, out.len())
        };
        ToolResult::data(summary, json!({ "subagents": out }))
    }

    fn cancel_subagent(&self, arguments_json: &str) -> ToolResult {
        #[derive(serde::Deserialize)]
        struct Args {
            id: String,
        }
        let a: Args = match serde_json::from_str(arguments_json) {
            Ok(a) => a,
            Err(e) => return ToolResult::err(format!("bad cancel_subagent args: {e}")),
        };
        if self.registry.cancel(&a.id) {
            ToolResult::ok(format!(
                "sub-agent `{}` will stop at its next round boundary; its checkpoint is kept, so resume_subagent can pick the work up",
                a.id
            ))
        } else {
            ToolResult::err(format!("no running sub-agent `{}`", a.id))
        }
    }

    fn message_subagent(&self, arguments_json: &str) -> ToolResult {
        #[derive(serde::Deserialize)]
        struct Args {
            id: String,
            message: String,
            #[serde(default)]
            delivery: Option<String>,
        }
        let a: Args = match serde_json::from_str(arguments_json) {
            Ok(a) => a,
            Err(e) => return ToolResult::err(format!("bad message_subagent args: {e}")),
        };
        if a.message.trim().is_empty() {
            return ToolResult::err("message_subagent needs a message");
        }
        let delivery = a.delivery.as_deref().map(Delivery::parse).unwrap_or_default();
        let msg = ChildMessage {
            text: a.message,
            delivery,
        };

        if a.id.trim() == "parent" {
            let Some(from) = (!self.path.is_empty()).then(|| self.path.clone()) else {
                return ToolResult::err(
                    "you are the top-level agent; say this in your reply to the user instead",
                );
            };
            return match self.parent_run_id() {
                Some(parent) => {
                    if self.registry.send(parent, msg) {
                        ToolResult::ok(format!("message delivered to parent sub-agent `{parent}`"))
                    } else {
                        ToolResult::err("the parent sub-agent is no longer running")
                    }
                }
                // The top-level agent's inbox is the steering queue.
                None => {
                    if let Ok(mut queue) = self.steering.lock() {
                        queue.push_back(UserTurn {
                            text: msg.text,
                            from: Some(from),
                        });
                        ToolResult::ok("message queued for the top-level agent")
                    } else {
                        ToolResult::err("could not reach the top-level agent")
                    }
                }
            };
        }

        let label = delivery.label();
        if self.registry.send(&a.id, msg) {
            ToolResult::ok(format!("message {label}ed to sub-agent `{}`", a.id))
        } else {
            ToolResult::err(format!("no running sub-agent `{}`", a.id))
        }
    }

    fn list_interrupted_subagents(&self) -> ToolResult {
        let checkpoints = crate::workspace::refine_session::list_subagents(&self.root);
        let items: Vec<_> = checkpoints
            .iter()
            .map(|cp| {
                json!({
                    "id": cp.id,
                    "task": cp.task,
                    "scope": cp.scope,
                    "role": cp.role,
                    "model": cp.model,
                    "updated": cp.updated.to_rfc3339(),
                    "tool_calls": cp.tool_call_count,
                    "messages": cp.message_count,
                    "depth": cp.depth,
                })
            })
            .collect();
        ToolResult::data(
            format!("{} interrupted sub-agent checkpoint(s)", items.len()),
            json!({ "checkpoints": items }),
        )
    }

    async fn run_saved_subagent(&self, call_id: &str, arguments_json: &str) -> ToolResult {
        if !self.can_spawn {
            return ToolResult::err("sub-agent nesting limit reached");
        }
        let Some(client) = self.client.clone() else {
            return ToolResult::err("sub-agents are unavailable (no client wired)");
        };
        #[derive(serde::Deserialize)]
        struct Args {
            id: String,
        }
        let a: Args = match serde_json::from_str(arguments_json) {
            Ok(a) => a,
            Err(e) => return ToolResult::err(format!("bad resume_subagent args: {e}")),
        };
        let Some(checkpoint) = crate::workspace::refine_session::load_subagent(&self.root, &a.id)
        else {
            return ToolResult::err(format!("no interrupted sub-agent checkpoint `{}`", a.id));
        };
        let event_id = subagent_event_id(&self.path, call_id);
        self.continue_subagent_checkpoint(client, event_id, checkpoint, true)
            .await
    }

    async fn run_subagent(&self, call_id: &str, arguments_json: &str) -> ToolResult {
        if !self.can_spawn {
            return ToolResult::err("sub-agent nesting limit reached");
        }
        let Some(client) = self.client.clone() else {
            return ToolResult::err("sub-agents are unavailable (no client wired)");
        };
        #[derive(serde::Deserialize)]
        struct Args {
            description: String,
            #[serde(default)]
            scope: Option<String>,
            #[serde(default)]
            role: Option<String>,
            #[serde(default)]
            background: bool,
        }
        let a: Args = match serde_json::from_str(arguments_json) {
            Ok(a) => a,
            Err(e) => return ToolResult::err(format!("bad task args: {e}")),
        };
        let role = a.role.as_deref().map(SubagentRole::parse).unwrap_or_default();
        let event_id = subagent_event_id(&self.path, call_id);
        let preview: String = a.description.chars().take(80).collect();
        self.tx.send(AppEvent::Log {
            level: LogLevel::Info,
            msg: format!("Refine sub-agent started: {preview}"),
        });

        if let Some(checkpoint) =
            crate::workspace::refine_session::load_subagent(&self.root, &event_id).or_else(|| {
                crate::workspace::refine_session::find_subagent(
                    &self.root,
                    &a.description,
                    a.scope.as_deref(),
                    &self.model.model,
                )
            })
        {
            emit_subagent_activity(
                &self.tx,
                &event_id,
                self.depth + 1,
                format!("resuming saved context · {}", checkpoint.id),
            );
            return self
                .continue_subagent_checkpoint(client, event_id, checkpoint, true)
                .await;
        }

        let user = match &a.scope {
            Some(s) if !s.trim().is_empty() => format!("Scope: {s}\n\nTask: {}", a.description),
            _ => a.description.clone(),
        };
        let mut req = ChatRequest::new(
            self.model.model.clone(),
            vec![
                Message::system(role.system_prompt(self.target_language)),
                Message::user(user),
            ],
        );
        req.tools = Some(role.tools());
        req.reasoning = self.model.reasoning_param();
        let checkpoint = SubagentCheckpointState::for_request(
            self.root.clone(),
            event_id.clone(),
            SubagentSpec {
                task: a.description.clone(),
                scope: a.scope,
                role,
                depth: self.depth + 1,
            },
            &req,
            SUBAGENT_MAX_TOOL_ROUNDS,
        );

        self.launch(
            client,
            LaunchSpec {
                event_id,
                role,
                title: a.description,
                background: a.background,
                req,
                initial_tool_call_count: 0,
                checkpoint,
            },
        )
        .await
    }

    async fn continue_subagent_checkpoint(
        &self,
        client: std::sync::Arc<dyn LlmClient>,
        event_id: String,
        checkpoint: crate::workspace::refine_session::SubagentCheckpoint,
        resumed: bool,
    ) -> ToolResult {
        let role = SubagentRole::parse(&checkpoint.role);
        let mut req = ChatRequest::new(checkpoint.model.clone(), checkpoint.messages.clone());
        req.tools = Some(role.tools());
        req.reasoning = checkpoint
            .reasoning
            .clone()
            .or_else(|| self.model.reasoning_param());
        let initial_tool_call_count = checkpoint.tool_call_count;
        let checkpoint_id = checkpoint.id.clone();
        let title = checkpoint.task.clone();
        let state = SubagentCheckpointState::from_existing(self.root.clone(), checkpoint);
        if resumed {
            emit_subagent_activity(
                &self.tx,
                &event_id,
                self.depth + 1,
                format!("resuming checkpoint · {checkpoint_id}"),
            );
        }
        self.launch(
            client,
            LaunchSpec {
                event_id,
                role,
                title: title.clone(),
                background: false,
                req,
                initial_tool_call_count,
                checkpoint: state,
            },
        )
        .await
    }

    /// Register a child, then either await it or hand it to the runtime.
    async fn launch(&self, client: std::sync::Arc<dyn LlmClient>, spec: LaunchSpec) -> ToolResult {
        let task_depth = self.depth + 1;
        if spec.background && self.registry.live() >= MAX_LIVE_SUBAGENTS {
            return ToolResult::err(format!(
                "{MAX_LIVE_SUBAGENTS} sub-agents are already running; collect one with subagent_output before starting another"
            ));
        }
        let handle = self.registry.register(&spec.event_id);
        self.tx.send(AppEvent::RefineSubagentStarted {
            id: spec.event_id.clone(),
            depth: task_depth.saturating_sub(1),
            title: truncate_chars(&spec.title, 80),
            role: spec.role.label().to_string(),
            model: self.model.model.clone(),
            background: spec.background,
        });

        let run = SubagentRun {
            client,
            executor: self.child(spec.event_id.clone(), spec.role),
            event_id: spec.event_id.clone(),
            task_depth,
            req: spec.req,
            initial_tool_call_count: spec.initial_tool_call_count,
            checkpoint: spec.checkpoint,
            handle,
        };

        if spec.background {
            let id = spec.event_id;
            tokio::spawn(drive_subagent(run));
            return ToolResult::data(
                format!("sub-agent `{id}` started in the background"),
                json!({ "id": id, "status": "running", "background": true }),
            );
        }
        drive_subagent(run).await
    }
}

#[async_trait]
impl ToolExecutor for RefineTools {
    async fn execute(&self, name: &str, arguments_json: &str) -> anyhow::Result<String> {
        // Checked again here, not only when the tool list was built: a model
        // that names a tool it was never offered gets a refusal, not an edit.
        if !self.role.allows(name) {
            return Ok(serde_json::to_string(&ToolResult::err(
                self.role.refusal(name),
            ))?);
        }
        if name == "task" {
            return Ok(serde_json::to_string(
                &self.run_subagent("direct", arguments_json).await,
            )?);
        }
        if name == "list_interrupted_subagents" {
            return Ok(serde_json::to_string(&self.list_interrupted_subagents())?);
        }
        if name == "subagent_output" {
            return Ok(serde_json::to_string(
                &self.collect_subagents(arguments_json).await,
            )?);
        }
        if name == "cancel_subagent" {
            return Ok(serde_json::to_string(&self.cancel_subagent(arguments_json))?);
        }
        if name == "message_subagent" {
            return Ok(serde_json::to_string(&self.message_subagent(arguments_json))?);
        }
        if name == "resume_subagent" {
            return Ok(serde_json::to_string(
                &self.run_saved_subagent("direct", arguments_json).await,
            )?);
        }
        if name == "ask_user" {
            return Ok(serde_json::to_string(
                &self.run_ask_user(arguments_json).await,
            )?);
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
        let result = match (!self.path.is_empty()).then_some(self.path.as_str()) {
            Some(owner) => {
                dispatch_refine_tool_for_owner(
                    &self.root,
                    self.default_vol,
                    &self.tx,
                    Some(owner),
                    name,
                    arguments_json,
                )
                .await
            }
            None => {
                dispatch_refine_tool(&self.root, self.default_vol, &self.tx, name, arguments_json)
                    .await
            }
        };
        Ok(serde_json::to_string(&result)?)
    }

    async fn execute_call(&self, call: &ToolCall) -> anyhow::Result<String> {
        if call.function.name == "task" {
            return Ok(serde_json::to_string(
                &self.run_subagent(&call.id, &call.function.arguments).await,
            )?);
        }
        if call.function.name == "resume_subagent" {
            return Ok(serde_json::to_string(
                &self
                    .run_saved_subagent(&call.id, &call.function.arguments)
                    .await,
            )?);
        }
        self.execute(&call.function.name, &call.function.arguments)
            .await
    }
}

/// `questions` when the model sent it, else the one-question shorthand.
///
/// The schema carries no top-level `required`, so both shapes validate on the
/// wire and the choice is made here — which is what keeps a model that still
/// sends the old shape working.
fn parse_ask_user(arguments_json: &str) -> std::result::Result<Vec<RefineQuestion>, String> {
    #[derive(serde::Deserialize)]
    struct Args {
        #[serde(default)]
        questions: Vec<RefineQuestion>,
        #[serde(default)]
        question: String,
        #[serde(default)]
        options: Vec<String>,
    }
    let a: Args =
        serde_json::from_str(arguments_json).map_err(|e| format!("bad ask_user args: {e}"))?;
    let mut questions = a.questions;
    questions.retain(|q| !q.question.trim().is_empty());
    if questions.is_empty() && !a.question.trim().is_empty() {
        questions.push(RefineQuestion {
            question: a.question,
            options: a.options,
            multiple: false,
        });
    }
    if questions.is_empty() {
        return Err("ask_user needs at least one question".to_string());
    }
    Ok(questions)
}

/// The card answers with a JSON array, one entry per question in order.
///
/// Anything that is not that array is taken as one free-text answer, which is
/// what a dismissal (the empty string) and any older reply both are.
fn decode_answers(raw: &str, n: usize) -> Vec<String> {
    let mut out =
        serde_json::from_str::<Vec<String>>(raw).unwrap_or_else(|_| vec![raw.to_string()]);
    out.resize(n, String::new());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::client::Result as LlmResult;
    use crate::llm::{Choice, FunctionCall, ResponseMessage, ToolCall};
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Arc, Mutex};

    static REFINE_PARALLELISM_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[test]
    fn thai_refine_prompts_map_female_boku_to_rao() {
        let prompt = refine_system_prompt(crate::model::TargetLanguage::Thai);
        assert!(prompt.contains("Thai form is always `เรา`"));
        assert!(prompt.contains("never `ผม` or the transliteration `โบคุ`"));
        assert!(
            SubagentRole::General
                .system_prompt(crate::model::TargetLanguage::Thai)
                .contains("render it as `เรา`, never `ผม` or `โบคุ`")
        );
    }

    #[test]
    fn refine_schema_exposes_only_neutral_target_fields() {
        let schema = refine_tools_schema().to_string();
        for field in [
            "include_translation",
            "translated_name",
            "translated_term",
            "forbidden_translations",
            "translated_synopsis",
            "translated_text",
        ] {
            assert!(schema.contains(field), "missing neutral field {field}");
        }
        for legacy in [
            "include_th",
            "thai_name",
            "thai_term",
            "forbidden_thai",
            "\"thai\"",
            "\"th\"",
        ] {
            assert!(
                !schema.contains(legacy),
                "legacy field leaked into Refine schema: {legacy}"
            );
        }
    }

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
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
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
        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }
        assert!(!events.is_empty(), "compaction emits events");
        assert!(
            events
                .iter()
                .any(|ev| matches!(ev, AppEvent::RefineContextCompacted { .. })),
            "compaction emits a UI-visible event"
        );
        assert_eq!(
            req.messages[2].role,
            Role::User,
            "tail starts at a user turn"
        );
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
    fn compaction_summarizes_tool_rounds_without_extra_user_turns() {
        let mut req = ChatRequest::new("subagent-test", vec![Message::system("sys")]);
        req.messages.push(Message::user("audit a whole volume"));
        let big = "x".repeat(90_000);
        for i in 0..6 {
            req.messages.push(Message {
                role: Role::Assistant,
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: format!("call_{i}"),
                    kind: "function".to_string(),
                    function: FunctionCall {
                        name: "read_chapter".to_string(),
                        arguments: format!(r#"{{"ch":{i}}}"#),
                    },
                }]),
                tool_call_id: None,
                name: None,
            });
            req.messages
                .push(Message::tool_result(format!("call_{i}"), big.clone()));
        }
        let before = req.messages.len();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        maybe_compact(&mut req, &EventTx(tx), false);

        assert!(
            req.messages.len() < before,
            "old tool rounds were compacted"
        );
        assert_eq!(req.messages[0].role, Role::System);
        assert_eq!(req.messages[1].role, Role::User);
        assert!(
            req.messages[1]
                .content
                .as_deref()
                .is_some_and(|c| c.contains("compacted"))
        );
        assert_eq!(
            req.messages[2].role,
            Role::Assistant,
            "tail starts at a complete assistant/tool round"
        );
        assert!(
            rx.try_recv()
                .is_ok_and(|ev| matches!(ev, AppEvent::Log { .. }))
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

    /// A client that parks before answering, so a test can watch a child while
    /// it is genuinely mid-flight instead of racing its completion.
    struct GatedClient {
        gate: Arc<tokio::sync::Semaphore>,
        responses: Mutex<VecDeque<crate::llm::ChatResponse>>,
    }

    impl GatedClient {
        fn new(responses: Vec<crate::llm::ChatResponse>) -> (Arc<Self>, Arc<tokio::sync::Semaphore>) {
            let gate = Arc::new(tokio::sync::Semaphore::new(0));
            (
                Arc::new(Self {
                    gate: gate.clone(),
                    responses: Mutex::new(VecDeque::from(responses)),
                }),
                gate,
            )
        }
    }

    #[async_trait]
    impl LlmClient for GatedClient {
        async fn chat(&self, _req: &ChatRequest) -> LlmResult<crate::llm::ChatResponse> {
            let permit = self
                .gate
                .acquire()
                .await
                .map_err(|_| LlmError::Canceled)?;
            permit.forget();
            let next = self.responses.lock().unwrap().pop_front();
            Ok(next.unwrap_or_else(|| stop_turn("done")))
        }
    }

    fn gated_tools(root: &std::path::Path, client: Arc<dyn LlmClient>) -> RefineTools {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        RefineTools::with_agent(
            root.to_path_buf(),
            1,
            EventTx(tx),
            client,
            crate::model::AgentModel::openrouter("m"),
            RefineInteract::default(),
        )
    }

    /// Give a spawned child a moment to reach its first await point.
    async fn settle() {
        for _ in 0..40 {
            tokio::task::yield_now().await;
        }
    }

    /// Real time, not just yields: a cancel reaches a parked call through
    /// `CANCEL_POLL`, which yielding alone never advances past.
    async fn wait_until(mut done: impl FnMut() -> bool) -> bool {
        for _ in 0..300 {
            if done() {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        done()
    }

    #[tokio::test]
    async fn a_background_task_returns_before_the_child_finishes() {
        let root = temp_root("bgtask");
        let (client, gate) = GatedClient::new(vec![stop_turn("swept ch1")]);
        let tools = gated_tools(&root, client);

        let out = tools
            .execute("task", r#"{"description":"sweep ch1","background":true}"#)
            .await
            .unwrap();
        // The tool answered with an id while the child is still parked.
        assert!(out.contains(r#""status":"running""#), "{out}");
        assert!(out.contains(r#""background":true"#), "{out}");
        settle().await;
        assert_eq!(tools.registry.live(), 1);

        // A snapshot must not block, and must not claim a report it has not got.
        let snap = tools
            .execute("subagent_output", r#"{"ids":["direct"],"timeout_ms":0}"#)
            .await
            .unwrap();
        assert!(snap.contains(r#""status":"running""#), "{snap}");

        gate.add_permits(1);
        let done = tools
            .execute("subagent_output", r#"{"ids":["direct"],"timeout_ms":5000}"#)
            .await
            .unwrap();
        assert!(done.contains("swept ch1"), "{done}");
        assert_eq!(tools.registry.live(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn collecting_an_id_that_was_never_started_says_so() {
        let root = temp_root("bgunknown");
        let (client, _gate) = GatedClient::new(vec![]);
        let tools = gated_tools(&root, client);
        let out = tools
            .execute("subagent_output", r#"{"ids":["nope"],"timeout_ms":0}"#)
            .await
            .unwrap();
        assert!(out.contains(r#""status":"unknown""#), "{out}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn cancelling_one_child_leaves_its_sibling_running() {
        let root = temp_root("bgcancel");
        let (client, gate) = GatedClient::new(vec![]);
        let tools = gated_tools(&root, client);

        for (id, desc) in [("call_a", "sweep ch1"), ("call_b", "sweep ch2")] {
            let call = ToolCall {
                id: id.to_string(),
                kind: "function".to_string(),
                function: crate::llm::FunctionCall {
                    name: "task".to_string(),
                    arguments: format!(r#"{{"description":"{desc}","background":true}}"#),
                },
            };
            tools.execute_call(&call).await.unwrap();
        }
        settle().await;
        assert_eq!(tools.registry.live(), 2);

        let out = tools
            .execute("cancel_subagent", r#"{"id":"call_a"}"#)
            .await
            .unwrap();
        assert!(out.contains("resume_subagent"), "{out}");

        // Neither child is let past its gate: cancelling abandons the call in
        // flight, so the cancelled one stops and the sibling stays parked.
        assert!(
            wait_until(|| tools.registry.live() == 1).await,
            "the cancel never reached the child"
        );
        assert_eq!(tools.registry.live(), 1, "the sibling was cancelled too");
        drop(gate);

        let snap = tools
            .execute(
                "subagent_output",
                r#"{"ids":["call_a","call_b"],"timeout_ms":0}"#,
            )
            .await
            .unwrap();
        assert!(snap.contains("cancelled"), "{snap}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn cancelling_a_child_keeps_a_resumable_checkpoint() {
        let root = temp_root("bgckpt");
        let (client, gate) = GatedClient::new(vec![]);
        let tools = gated_tools(&root, client);
        tools
            .execute("task", r#"{"description":"long sweep","background":true}"#)
            .await
            .unwrap();
        settle().await;
        tools
            .execute("cancel_subagent", r#"{"id":"direct"}"#)
            .await
            .unwrap();
        assert!(wait_until(|| tools.registry.live() == 0).await);
        drop(gate);
        let saved = crate::workspace::refine_session::list_subagents(&root);
        assert!(
            saved.iter().any(|c| c.task == "long sweep"),
            "nothing left to resume: {saved:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn spawning_past_the_live_cap_is_refused_not_queued() {
        let root = temp_root("bgcap");
        let (client, _gate) = GatedClient::new(vec![]);
        let tools = gated_tools(&root, client);

        for n in 0..MAX_LIVE_SUBAGENTS {
            let call = ToolCall {
                id: format!("call_{n}"),
                kind: "function".to_string(),
                function: crate::llm::FunctionCall {
                    name: "task".to_string(),
                    arguments: r#"{"description":"work","background":true}"#.to_string(),
                },
            };
            let out = tools.execute_call(&call).await.unwrap();
            assert!(out.contains(r#""status":"running""#), "{n}: {out}");
        }
        settle().await;
        let out = tools
            .execute("task", r#"{"description":"one too many","background":true}"#)
            .await
            .unwrap();
        assert!(out.contains(r#""ok":false"#), "{out}");
        assert!(out.contains("subagent_output"), "{out}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_message_to_a_running_child_is_taken_and_a_finished_one_is_not() {
        let root = temp_root("bgmsg");
        let (client, gate) = GatedClient::new(vec![stop_turn("done")]);
        let tools = gated_tools(&root, client);
        tools
            .execute("task", r#"{"description":"sweep","background":true}"#)
            .await
            .unwrap();
        settle().await;

        let out = tools
            .execute(
                "message_subagent",
                r#"{"id":"direct","message":"also check chapter 4","delivery":"steer"}"#,
            )
            .await
            .unwrap();
        assert!(out.contains(r#""ok":true"#), "{out}");

        gate.add_permits(4);
        assert!(wait_until(|| tools.registry.live() == 0).await);
        let out = tools
            .execute(
                "message_subagent",
                r#"{"id":"direct","message":"too late"}"#,
            )
            .await
            .unwrap();
        assert!(out.contains(r#""ok":false"#), "{out}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn the_top_level_agent_has_no_parent_to_message() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let tools = RefineTools::new(std::path::PathBuf::from("/tmp"), 1, EventTx(tx));
        let out = tools
            .execute("message_subagent", r#"{"id":"parent","message":"hi"}"#)
            .await
            .unwrap();
        assert!(out.contains(r#""ok":false"#), "{out}");
        assert!(out.contains("top-level"), "{out}");
    }

    #[tokio::test]
    async fn a_child_of_the_root_agent_reports_upward_through_steering() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let steering = empty_steering();
        let tools = RefineTools::with_agent(
            std::path::PathBuf::from("/tmp"),
            1,
            EventTx(tx),
            Arc::new(ScriptedClient {
                responses: Mutex::new(VecDeque::new()),
            }),
            crate::model::AgentModel::openrouter("m"),
            RefineInteract::default(),
        )
        .sharing(SubagentRegistry::default(), steering.clone())
        .child("call_1".to_string(), SubagentRole::Explore);

        let out = tools
            .execute(
                "message_subagent",
                r#"{"id":"parent","message":"chapter 3 is already clean"}"#,
            )
            .await
            .unwrap();
        assert!(out.contains(r#""ok":true"#), "{out}");
        let queued = steering.lock().unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].from.as_deref(), Some("call_1"));
    }

    #[tokio::test]
    async fn an_explore_subagent_cannot_edit_a_chapter() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let child = RefineTools::new(std::path::PathBuf::from("/tmp"), 1, EventTx(tx))
            .child("call_1".to_string(), SubagentRole::Explore);

        // Refused on the way in, not merely absent from the advertised list:
        // the gate has to hold for a name the model invented.
        for tool in ["edit_chapter", "multi_edit_chapter", "upsert_character", "task"] {
            let out = child.execute(tool, "{}").await.unwrap();
            assert!(out.contains("\"ok\":false"), "{tool} was not refused: {out}");
            assert!(out.contains("explore"), "{tool} refusal does not name the role: {out}");
        }
        // And reading still works, or the role would be useless.
        let out = child.execute("list_volumes", "{}").await.unwrap();
        assert!(!out.contains("not available to a `explore`"), "{out}");
    }

    #[tokio::test]
    async fn an_explore_subagent_cannot_delegate_its_way_around_the_gate() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let root = RefineTools::new(std::path::PathBuf::from("/tmp"), 1, EventTx(tx));
        assert!(!root.child("a".into(), SubagentRole::Explore).can_spawn);
        assert!(!root.child("a".into(), SubagentRole::Editor).can_spawn);
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
        tool_call_turn_many(&[(name, args)])
    }

    fn tool_call_turn_many(calls: &[(&str, &str)]) -> crate::llm::ChatResponse {
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
                    tool_calls: Some(
                        calls
                            .iter()
                            .enumerate()
                            .map(|(idx, (name, args))| ToolCall {
                                id: format!("call_{}", idx + 1),
                                kind: "function".to_string(),
                                function: FunctionCall {
                                    name: (*name).to_string(),
                                    arguments: (*args).to_string(),
                                },
                            })
                            .collect(),
                    ),
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

    struct HugeExecutor {
        payload: String,
    }

    #[async_trait]
    impl ToolExecutor for HugeExecutor {
        async fn execute(&self, _name: &str, _arguments_json: &str) -> anyhow::Result<String> {
            Ok(serde_json::to_string(&ToolResult::ok(
                self.payload.clone(),
            ))?)
        }
    }

    struct ConcurrentExecutor {
        active: AtomicUsize,
        max_seen: AtomicUsize,
    }

    impl ConcurrentExecutor {
        fn new() -> Self {
            Self {
                active: AtomicUsize::new(0),
                max_seen: AtomicUsize::new(0),
            }
        }

        fn max_seen(&self) -> usize {
            self.max_seen.load(Ordering::Relaxed)
        }
    }

    #[async_trait]
    impl ToolExecutor for ConcurrentExecutor {
        async fn execute_call(&self, call: &ToolCall) -> anyhow::Result<String> {
            let active = self.active.fetch_add(1, Ordering::Relaxed) + 1;
            self.max_seen.fetch_max(active, Ordering::Relaxed);
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            self.active.fetch_sub(1, Ordering::Relaxed);
            Ok(serde_json::to_string(&ToolResult::ok(format!(
                "finished {}",
                call.id
            )))?)
        }

        async fn execute(&self, _name: &str, _arguments_json: &str) -> anyhow::Result<String> {
            unreachable!("tests call execute_call")
        }
    }

    struct RateLimitedAfterToolClient {
        calls: AtomicUsize,
        seen_lengths: Mutex<Vec<usize>>,
    }

    impl RateLimitedAfterToolClient {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                seen_lengths: Mutex::new(Vec::new()),
            }
        }

        fn seen_lengths(&self) -> Vec<usize> {
            self.seen_lengths.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl LlmClient for RateLimitedAfterToolClient {
        async fn chat(&self, req: &ChatRequest) -> LlmResult<crate::llm::ChatResponse> {
            self.seen_lengths.lock().unwrap().push(req.messages.len());
            match self.calls.fetch_add(1, Ordering::Relaxed) {
                0 => Ok(tool_call_turn("read_chapter", r#"{"ch":1}"#)),
                1 => Err(LlmError::RateLimited {
                    retry_after: 0,
                    message: "usage limit reached".to_string(),
                }),
                _ => Ok(stop_turn("continued after retry")),
            }
        }
    }

    struct FailingAfterToolClient {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl LlmClient for FailingAfterToolClient {
        async fn chat(&self, _req: &ChatRequest) -> LlmResult<crate::llm::ChatResponse> {
            match self.calls.fetch_add(1, Ordering::Relaxed) {
                0 => Ok(tool_call_turn("read_chapter", r#"{"ch":1}"#)),
                _ => Err(LlmError::Api {
                    status: 500,
                    message: "provider unavailable".to_string(),
                }),
            }
        }
    }

    fn empty_steering() -> Arc<Mutex<VecDeque<UserTurn>>> {
        Arc::new(Mutex::new(VecDeque::new()))
    }

    fn test_runtime<'a>(
        tx: &'a EventTx,
        cancel: &'a AtomicBool,
        interact: &'a RefineInteract,
        steering: &'a Arc<Mutex<VecDeque<UserTurn>>>,
    ) -> RefineTurnRuntime<'a> {
        RefineTurnRuntime {
            tx,
            registry: test_registry(),
            cancel,
            default_vol: 1,
            interact,
            steering,
        }
    }

    /// Leaked deliberately: the runtime borrows it for the length of a test,
    /// and one registry per test is cheaper than threading a lifetime through
    /// every call site.
    fn test_registry() -> &'static SubagentRegistry {
        Box::leak(Box::new(SubagentRegistry::default()))
    }

    #[tokio::test]
    async fn refine_turn_streams_executes_tool_and_finishes() {
        let root = temp_root("turn");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let etx = EventTx(tx);
        let client = ScriptedClient {
            responses: Mutex::new(VecDeque::from(vec![
                tool_call_turn(
                    "upsert_character",
                    r#"{"jp_name":"勇","translated_name":"ยู"}"#,
                ),
                stop_turn("เพิ่มตัวละครเรียบร้อย"),
            ])),
        };
        let tools = RefineTools::new(root.clone(), 1, etx.clone());
        let mut req = ChatRequest::new("m", vec![Message::system("sys")]);
        req.messages.push(Message::user("add a character"));
        let cancel = AtomicBool::new(false);
        let interact = RefineInteract::default();
        let steering = empty_steering();
        let runtime = test_runtime(&etx, &cancel, &interact, &steering);

        run_refine_turn(&client, &mut req, &tools, &runtime).await;

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
            characters::load(&ws)
                .iter()
                .any(|c| c.translated_name == "ยู"),
            "the tool mutated the workspace"
        );
        assert!(req.messages.len() >= 4);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn refine_turn_runs_batched_tasks_in_parallel() {
        let _env = REFINE_PARALLELISM_ENV_LOCK.lock().await;
        unsafe {
            std::env::set_var("HONYA_REFINE_PARALLELISM", "2");
        }

        let root = temp_root("paralleltasks");
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let etx = EventTx(tx);
        let client = ScriptedClient {
            responses: Mutex::new(VecDeque::from(vec![
                tool_call_turn_many(&[
                    ("task", r#"{"description":"audit chapter 1"}"#),
                    ("task", r#"{"description":"audit chapter 2"}"#),
                ]),
                stop_turn("done"),
            ])),
        };
        let tools = ConcurrentExecutor::new();
        let mut req = ChatRequest::new("m", vec![Message::system("sys")]);
        req.messages.push(Message::user("audit two chapters"));
        let cancel = AtomicBool::new(false);
        let interact = RefineInteract::default();
        let steering = empty_steering();
        let runtime = test_runtime(&etx, &cancel, &interact, &steering);

        run_refine_turn(&client, &mut req, &tools, &runtime).await;

        assert!(
            tools.max_seen() >= 2,
            "batched task calls should execute concurrently"
        );
        let tool_ids: Vec<_> = req
            .messages
            .iter()
            .filter(|m| m.role == Role::Tool)
            .filter_map(|m| m.tool_call_id.as_deref())
            .collect();
        assert_eq!(tool_ids, vec!["call_1", "call_2"]);
        drop(rx);

        unsafe {
            std::env::remove_var("HONYA_REFINE_PARALLELISM");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn subagent_tool_loop_auto_compacts_large_tool_history() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let etx = EventTx(tx);
        let client = ScriptedClient {
            responses: Mutex::new(VecDeque::from(vec![
                tool_call_turn("read_chapter", r#"{"ch":1}"#),
                tool_call_turn("read_chapter", r#"{"ch":2}"#),
                tool_call_turn("read_chapter", r#"{"ch":3}"#),
                tool_call_turn("read_chapter", r#"{"ch":4}"#),
                stop_turn("done"),
            ])),
        };
        let mut req = ChatRequest::new(
            "subagent-test",
            vec![
                Message::system(SubagentRole::General.system_prompt(crate::model::TargetLanguage::Thai)),
                Message::user("audit the volume"),
            ],
        );
        req.tools = Some(refine_tools_vec());
        let executor = HugeExecutor {
            payload: "x".repeat(180_000),
        };

        let outcome = run_compacting_tool_loop(
            &client,
            req,
            &executor,
            &etx,
            SubagentLoopOptions::new(8, 0, ""),
        )
        .await
        .unwrap();

        assert_eq!(outcome.tool_calls, 4);
        let mut saw_compaction = false;
        while let Ok(ev) = rx.try_recv() {
            if matches!(ev, AppEvent::RefineContextCompacted { .. }) {
                saw_compaction = true;
            }
        }
        assert!(
            saw_compaction,
            "sub-agent loop compacts growing tool history"
        );
    }

    #[tokio::test]
    async fn subagent_tool_loop_finalizes_when_tool_budget_is_reached() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let etx = EventTx(tx);
        let client = ScriptedClient {
            responses: Mutex::new(VecDeque::from(vec![
                tool_call_turn("read_chapter", r#"{"ch":1}"#),
                tool_call_turn("read_chapter", r#"{"ch":1,"offset":400}"#),
                stop_turn("completed the inspected edits; remaining work should be split"),
            ])),
        };
        let mut req = ChatRequest::new(
            "subagent-test",
            vec![
                Message::system(SubagentRole::General.system_prompt(crate::model::TargetLanguage::Thai)),
                Message::user("fix a long review-needed chapter"),
            ],
        );
        req.tools = Some(refine_tools_vec());
        let executor = HugeExecutor {
            payload: "ok".to_string(),
        };

        let outcome = run_compacting_tool_loop(
            &client,
            req,
            &executor,
            &etx,
            SubagentLoopOptions::new(2, 0, ""),
        )
        .await
        .unwrap();

        assert_eq!(outcome.tool_calls, 2);
        let report = outcome.response.choices[0]
            .message
            .content
            .as_deref()
            .unwrap_or_default();
        assert!(report.contains("completed the inspected edits"));
        let mut saw_budget_log = false;
        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::Log { level, msg } = ev
                && level == LogLevel::Warn
                && msg.contains("reached 2 tool rounds")
            {
                saw_budget_log = true;
            }
        }
        assert!(saw_budget_log, "budget finalization should be logged");
    }

    #[tokio::test]
    async fn subagent_loop_emits_terminal_status_on_completion() {
        let root = temp_root("terminalstatus");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let etx = EventTx(tx);
        let client = Arc::new(ScriptedClient {
            responses: Mutex::new(VecDeque::from(vec![stop_turn("all done")])),
        });
        let tools = RefineTools::with_agent(
            root.clone(),
            1,
            etx,
            client,
            crate::model::AgentModel::openrouter("subagent-test"),
            RefineInteract::default(),
        );

        let out = tools
            .execute_call(&ToolCall {
                id: "call_1".to_string(),
                kind: "function".to_string(),
                function: FunctionCall {
                    name: "task".to_string(),
                    arguments: r#"{"description":"quick audit"}"#.to_string(),
                },
            })
            .await
            .unwrap();
        let result: ToolResult = serde_json::from_str(&out).unwrap();
        assert!(result.ok, "{result:?}");

        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|ev| match ev {
                AppEvent::RefineSubagentFinished { id, status, .. } => Some((id, status)),
                _ => None,
            })
            .collect();
        assert!(
            events
                .iter()
                .any(|(id, status)| id == "call_1" && *status == RefineSubagentStatus::Succeeded),
            "sub-agent should report Succeeded when its loop finishes: {events:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn subagent_tool_loop_retries_empty_final_report() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let etx = EventTx(tx);
        let client = ScriptedClient {
            responses: Mutex::new(VecDeque::from(vec![
                stop_turn(""),
                stop_turn("completed the audit"),
            ])),
        };
        let mut req = ChatRequest::new(
            "subagent-test",
            vec![
                Message::system(SubagentRole::General.system_prompt(crate::model::TargetLanguage::Thai)),
                Message::user("audit the volume"),
            ],
        );
        req.tools = Some(refine_tools_vec());
        let executor = HugeExecutor {
            payload: "ok".to_string(),
        };

        let outcome = run_compacting_tool_loop(
            &client,
            req,
            &executor,
            &etx,
            SubagentLoopOptions::new(8, 0, ""),
        )
        .await
        .unwrap();

        let report = outcome.response.choices[0]
            .message
            .content
            .as_deref()
            .unwrap_or_default();
        assert!(report.contains("completed the audit"));
    }

    #[tokio::test]
    async fn parallel_subagents_mark_done_without_waiting_for_siblings() {
        let _env = REFINE_PARALLELISM_ENV_LOCK.lock().await;
        unsafe {
            std::env::set_var("HONYA_REFINE_PARALLELISM", "2");
        }

        struct TaskScriptedClient {
            slow_rounds: AtomicUsize,
        }

        fn task_text(req: &ChatRequest) -> &str {
            req.messages
                .iter()
                .filter(|m| m.role == Role::User)
                .filter_map(|m| m.content.as_deref())
                .next_back()
                .unwrap_or("")
        }

        #[async_trait]
        impl LlmClient for TaskScriptedClient {
            async fn chat(&self, req: &ChatRequest) -> LlmResult<crate::llm::ChatResponse> {
                let task = task_text(req);
                if task.contains("fast audit") {
                    return Ok(stop_turn("fast done"));
                }
                if task.contains("slow audit") {
                    match self.slow_rounds.fetch_add(1, Ordering::Relaxed) {
                        0 => Ok(tool_call_turn("read_chapter", r#"{"ch":1}"#)),
                        _ => Ok(stop_turn("slow done")),
                    }
                } else {
                    panic!("unexpected sub-agent task: {task}");
                }
            }
        }

        let root = temp_root("parallelsubagents");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let etx = EventTx(tx);
        let client = Arc::new(TaskScriptedClient {
            slow_rounds: AtomicUsize::new(0),
        });
        let tools = RefineTools::with_agent(
            root.clone(),
            1,
            etx,
            client,
            crate::model::AgentModel::openrouter("subagent-test"),
            RefineInteract::default(),
        );
        let calls = [
            ToolCall {
                id: "call_1".to_string(),
                kind: "function".to_string(),
                function: FunctionCall {
                    name: "task".to_string(),
                    arguments: r#"{"description":"fast audit"}"#.to_string(),
                },
            },
            ToolCall {
                id: "call_2".to_string(),
                kind: "function".to_string(),
                function: FunctionCall {
                    name: "task".to_string(),
                    arguments: r#"{"description":"slow audit"}"#.to_string(),
                },
            },
        ];

        execute_tool_calls_parallel(&tools, &calls).await;

        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|ev| match ev {
                AppEvent::RefineSubagentFinished {
                    id,
                    status,
                    summary,
                } => Some((id, status, summary)),
                _ => None,
            })
            .collect();
        let call_1_done = events.iter().position(|(id, status, _)| {
            id == "call_1" && *status == RefineSubagentStatus::Succeeded
        });
        let call_2_done = events.iter().position(|(id, status, _)| {
            id == "call_2" && *status == RefineSubagentStatus::Succeeded
        });
        assert!(
            call_1_done.is_some() && call_2_done.is_some(),
            "both sub-agents should finish: {events:?}"
        );
        assert!(
            call_1_done.unwrap() < call_2_done.unwrap(),
            "the fast sub-agent should report done before its slower sibling: {events:?}"
        );

        unsafe {
            std::env::remove_var("HONYA_REFINE_PARALLELISM");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn subagent_tool_loop_retries_rate_limit_without_losing_context() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let etx = EventTx(tx);
        let client = RateLimitedAfterToolClient::new();
        let mut req = ChatRequest::new(
            "subagent-test",
            vec![
                Message::system(SubagentRole::General.system_prompt(crate::model::TargetLanguage::Thai)),
                Message::user("audit the volume"),
            ],
        );
        req.tools = Some(refine_tools_vec());
        let executor = HugeExecutor {
            payload: "ok".to_string(),
        };

        let outcome = run_compacting_tool_loop(
            &client,
            req,
            &executor,
            &etx,
            SubagentLoopOptions::new(8, 1, "call_1"),
        )
        .await
        .unwrap();

        assert_eq!(outcome.tool_calls, 1);
        assert_eq!(
            client.seen_lengths(),
            vec![2, 4, 4],
            "rate-limit retry must resend the same sub-agent context, including tool results"
        );
        let mut saw_rate_limit_activity = false;
        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::RefineSubagentActivity { activity, .. } = ev
                && activity.contains("rate limited")
            {
                saw_rate_limit_activity = true;
            }
        }
        assert!(
            saw_rate_limit_activity,
            "sub-agent panel reports the retry instead of showing a hard failure"
        );
    }

    #[tokio::test]
    async fn subagent_checkpoint_resumes_after_interrupted_tool_loop() {
        let root = temp_root("resumecheckpoint");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let etx = EventTx(tx);
        let client = FailingAfterToolClient {
            calls: AtomicUsize::new(0),
        };
        let mut req = ChatRequest::new(
            "subagent-test",
            vec![
                Message::system(SubagentRole::General.system_prompt(crate::model::TargetLanguage::Thai)),
                Message::user("audit the volume"),
            ],
        );
        req.tools = Some(refine_tools_vec());
        let checkpoint = SubagentCheckpointState::for_request(
            root.clone(),
            "call_1".to_string(),
            SubagentSpec {
                task: "audit the volume".to_string(),
                scope: None,
                role: SubagentRole::General,
                depth: 1,
            },
            &req,
            SUBAGENT_MAX_TOOL_ROUNDS,
        );
        let executor = HugeExecutor {
            payload: "ok".to_string(),
        };

        let err = match run_compacting_tool_loop(
            &client,
            req,
            &executor,
            &etx,
            SubagentLoopOptions::new(8, 1, "call_1").checkpoint(checkpoint),
        )
        .await
        {
            Ok(_) => panic!("interrupted sub-agent should fail before resume"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("provider unavailable"));

        let saved = crate::workspace::refine_session::load_subagent(&root, "call_1")
            .expect("interrupted sub-agent checkpoint remains on disk");
        assert_eq!(saved.messages.len(), 4);
        assert_eq!(saved.tool_call_count, 1);

        let resume_client: Arc<dyn LlmClient> = Arc::new(ScriptedClient {
            responses: Mutex::new(VecDeque::from(vec![stop_turn("resumed and done")])),
        });
        let tools = RefineTools::with_agent(
            root.clone(),
            1,
            etx.clone(),
            resume_client,
            crate::model::AgentModel::openrouter("subagent-test"),
            RefineInteract::default(),
        );
        let out = tools
            .execute_call(&ToolCall {
                id: "resume_call".to_string(),
                kind: "function".to_string(),
                function: FunctionCall {
                    name: "resume_subagent".to_string(),
                    arguments: r#"{"id":"call_1"}"#.to_string(),
                },
            })
            .await
            .unwrap();
        let result: ToolResult = serde_json::from_str(&out).unwrap();
        assert!(result.ok, "{result:?}");
        assert!(
            result.message.contains("resumed and done"),
            "{}",
            result.message
        );
        assert!(
            crate::workspace::refine_session::load_subagent(&root, "call_1").is_none(),
            "completed resume removes the checkpoint"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn subagent_tool_loop_runs_batched_tasks_in_parallel() {
        let _env = REFINE_PARALLELISM_ENV_LOCK.lock().await;
        unsafe {
            std::env::set_var("HONYA_REFINE_PARALLELISM", "2");
        }

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let etx = EventTx(tx);
        let client = ScriptedClient {
            responses: Mutex::new(VecDeque::from(vec![
                tool_call_turn_many(&[
                    ("task", r#"{"description":"nested audit 1"}"#),
                    ("task", r#"{"description":"nested audit 2"}"#),
                ]),
                stop_turn("done"),
            ])),
        };
        let mut req = ChatRequest::new(
            "subagent-test",
            vec![
                Message::system(SubagentRole::General.system_prompt(crate::model::TargetLanguage::Thai)),
                Message::user("audit the volume"),
            ],
        );
        req.tools = Some(refine_tools_vec());
        let executor = ConcurrentExecutor::new();

        let outcome = run_compacting_tool_loop(
            &client,
            req,
            &executor,
            &etx,
            SubagentLoopOptions::new(8, 1, "root_call"),
        )
        .await
        .unwrap();

        assert_eq!(outcome.tool_calls, 2);
        assert!(
            executor.max_seen() >= 2,
            "sub-agent task batches should execute concurrently"
        );

        unsafe {
            std::env::remove_var("HONYA_REFINE_PARALLELISM");
        }
    }

    #[tokio::test]
    async fn subagent_tool_loop_namespaces_nested_task_events() {
        let root = temp_root("nestedtask");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let etx = EventTx(tx);
        let client: Arc<dyn LlmClient> = Arc::new(ScriptedClient {
            responses: Mutex::new(VecDeque::from(vec![
                tool_call_turn("task", r#"{"description":"nested audit"}"#),
                stop_turn("nested done"),
                stop_turn("parent done"),
            ])),
        });
        let mut req = ChatRequest::new(
            "subagent-test",
            vec![
                Message::system(SubagentRole::General.system_prompt(crate::model::TargetLanguage::Thai)),
                Message::user("audit the volume"),
            ],
        );
        req.tools = Some(refine_tools_vec());
        let tools = RefineTools::with_agent(
            root.clone(),
            1,
            etx.clone(),
            client.clone(),
            crate::model::AgentModel::openrouter("m"),
            RefineInteract::default(),
        );
        let executor = tools.child("root_call".to_string(), SubagentRole::General);

        let outcome = run_compacting_tool_loop(
            client.as_ref(),
            req,
            &executor,
            &etx,
            SubagentLoopOptions::new(8, 1, "root_call"),
        )
        .await
        .unwrap();

        assert_eq!(outcome.tool_calls, 1);
        let mut started = Vec::new();
        let mut finished = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AppEvent::RefineSubagentStarted {
                    id, depth, title, ..
                } => started.push((id, depth, title)),
                AppEvent::RefineSubagentFinished {
                    id,
                    status,
                    summary,
                } => finished.push((id, status, summary)),
                _ => {}
            }
        }
        assert!(
            started.iter().any(|(id, depth, title)| {
                id == "root_call/call_1" && *depth == 1 && title.contains("nested audit")
            }),
            "nested task start should be namespaced under the parent path: {started:?}"
        );
        assert!(
            finished.iter().any(|(id, status, summary)| {
                id == "root_call/call_1"
                    && *status == RefineSubagentStatus::Succeeded
                    && summary.contains("sub-agent finished")
            }),
            "nested task completion should close the same namespaced row: {finished:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn refine_turn_synthesizes_summary_after_tool_only_stop() {
        let root = temp_root("fallback");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let etx = EventTx(tx);
        let client = ScriptedClient {
            responses: Mutex::new(VecDeque::from(vec![
                tool_call_turn(
                    "upsert_character",
                    r#"{"jp_name":"勇","translated_name":"ยู"}"#,
                ),
                stop_turn(""),
            ])),
        };
        let tools = RefineTools::new(root.clone(), 1, etx.clone());
        let mut req = ChatRequest::new("m", vec![Message::system("sys")]);
        req.messages.push(Message::user("add a character"));
        let cancel = AtomicBool::new(false);
        let interact = RefineInteract::default();
        let steering = empty_steering();
        let runtime = test_runtime(&etx, &cancel, &interact, &steering);

        run_refine_turn(&client, &mut req, &tools, &runtime).await;

        let mut summary = String::new();
        let mut saw_done = false;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AppEvent::RefineDelta { delta } => summary.push_str(&delta),
                AppEvent::RefineMessageDone => saw_done = true,
                _ => {}
            }
        }
        assert!(summary.contains("Done. Summary"), "{summary}");
        assert!(summary.contains("upsert character"), "{summary}");
        assert!(saw_done, "fallback still finishes the turn");
        assert!(
            req.messages
                .last()
                .and_then(|m| m.content.as_deref())
                .is_some_and(|c| c.contains("Done. Summary")),
            "fallback summary is persisted in chat history"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn refine_turn_clears_fully_completed_plan_on_final_stop() {
        let root = temp_root("planclearturn");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let etx = EventTx(tx);
        let client = ScriptedClient {
            responses: Mutex::new(VecDeque::from(vec![
                tool_call_turn(
                    "update_plan",
                    r#"{"steps":[{"step":"read chapter","status":"completed"},{"step":"apply edit","status":"completed"}]}"#,
                ),
                stop_turn("done"),
            ])),
        };
        let tools = RefineTools::new(root.clone(), 1, etx.clone());
        let mut req = ChatRequest::new("m", vec![Message::system("sys")]);
        req.messages.push(Message::user("finish a task"));
        let cancel = AtomicBool::new(false);
        let interact = RefineInteract::default();
        let steering = empty_steering();
        let runtime = test_runtime(&etx, &cancel, &interact, &steering);

        run_refine_turn(&client, &mut req, &tools, &runtime).await;

        let mut plan_lengths = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::RefinePlanUpdated { steps } = ev {
                plan_lengths.push(steps.len());
            }
        }
        assert_eq!(plan_lengths, vec![2, 0]);

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
        let interact = RefineInteract::default();
        let steering = empty_steering();
        let runtime = test_runtime(&etx, &cancel, &interact, &steering);

        run_refine_turn(&client, &mut req, &tools, &runtime).await;
        let mut saw_done = false;
        while let Ok(ev) = rx.try_recv() {
            if matches!(ev, AppEvent::RefineMessageDone) {
                saw_done = true;
            }
        }
        assert!(saw_done, "a cancelled turn still emits MessageDone");
        let _ = std::fs::remove_dir_all(&root);
    }

    struct BlockingTaskClient {
        calls: AtomicUsize,
        subagent_started: tokio::sync::Notify,
    }

    #[async_trait]
    impl LlmClient for BlockingTaskClient {
        async fn chat(&self, _req: &ChatRequest) -> LlmResult<crate::llm::ChatResponse> {
            if self.calls.fetch_add(1, Ordering::Relaxed) == 0 {
                return Ok(tool_call_turn(
                    "task",
                    r#"{"description":"audit the whole volume"}"#,
                ));
            }
            self.subagent_started.notify_waiters();
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn refine_turn_cancel_unblocks_long_running_task_tool() {
        let root = temp_root("canceltask");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let etx = EventTx(tx);
        let client = Arc::new(BlockingTaskClient {
            calls: AtomicUsize::new(0),
            subagent_started: tokio::sync::Notify::new(),
        });
        let tools = RefineTools::with_agent(
            root.clone(),
            1,
            etx.clone(),
            client.clone(),
            crate::model::AgentModel::openrouter("m"),
            RefineInteract::default(),
        );
        let mut req = ChatRequest::new("m", vec![Message::system("sys")]);
        req.messages.push(Message::user("do a long task"));
        let cancel = AtomicBool::new(false);
        let interact = RefineInteract::default();
        let steering = empty_steering();
        let runtime = test_runtime(&etx, &cancel, &interact, &steering);

        {
            let turn = run_refine_turn(client.as_ref(), &mut req, &tools, &runtime);
            tokio::pin!(turn);
            tokio::select! {
                _ = client.subagent_started.notified() => {}
                _ = &mut turn => panic!("turn finished before sub-agent blocked"),
            }

            cancel.store(true, Ordering::Relaxed);
            tokio::time::timeout(std::time::Duration::from_millis(300), &mut turn)
                .await
                .expect("cancel must unblock a long-running task tool");
        }

        let mut saw_done = false;
        let mut saw_start = false;
        let mut activities = Vec::new();
        let mut finals = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AppEvent::RefineMessageDone => saw_done = true,
                AppEvent::RefineSubagentStarted { .. } => saw_start = true,
                AppEvent::RefineSubagentActivity { activity, .. } => activities.push(activity),
                AppEvent::RefineSubagentFinished { status, .. } => finals.push(status),
                _ => {}
            }
        }
        assert!(saw_done, "cancelled long task still emits MessageDone");
        assert!(saw_start, "UI is told the sub-agent started");
        assert!(
            activities.iter().any(|s| s == "thinking"),
            "UI sees the sub-agent's current activity: {activities:?}"
        );
        assert_eq!(
            finals,
            vec![RefineSubagentStatus::Canceled],
            "UI sees exactly one ending, and it is a cancellation"
        );
        let tool_result = req
            .messages
            .iter()
            .find(|m| m.role == Role::Tool)
            .expect("cancelled tool call is closed with a tool result");
        assert!(
            tool_result
                .content
                .as_deref()
                .is_some_and(|c| c.contains("was cancelled by the user")),
            "tool result records the cancellation"
        );
        let checkpoints = crate::workspace::refine_session::list_subagents(&root);
        assert_eq!(checkpoints.len(), 1);
        assert_eq!(checkpoints[0].id, "call_1");
        let saved = crate::workspace::refine_session::load_subagent(&root, "call_1").unwrap();
        assert_eq!(
            saved.messages.len(),
            2,
            "user cancellation leaves the sub-agent's current context resumable"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn refine_turn_adds_queued_steering_after_tool_batch() {
        let root = temp_root("steeraftertool");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let etx = EventTx(tx);
        let client = ScriptedClient {
            responses: Mutex::new(VecDeque::from(vec![
                tool_call_turn("read_meta", r#"{"kind":"project"}"#),
                stop_turn("checked style too"),
            ])),
        };
        let tools = RefineTools::new(root.clone(), 1, etx.clone());
        let mut req = ChatRequest::new("m", vec![Message::system("sys")]);
        req.messages.push(Message::user("read project"));
        let cancel = AtomicBool::new(false);
        let interact = RefineInteract::default();
        let steering = Arc::new(Mutex::new(VecDeque::from(vec![UserTurn {
            text: "also check @style".to_string(),
            from: None,
        }])));
        let runtime = test_runtime(&etx, &cancel, &interact, &steering);

        run_refine_turn(&client, &mut req, &tools, &runtime).await;

        let tool_idx = req
            .messages
            .iter()
            .position(|m| m.role == Role::Tool)
            .expect("tool result persisted");
        let steering_idx = req
            .messages
            .iter()
            .position(|m| {
                m.role == Role::User
                    && m.content
                        .as_deref()
                        .is_some_and(|c| c.contains("also check @style"))
            })
            .expect("queued steering persisted as a user message");
        assert!(
            steering_idx > tool_idx,
            "steering is injected after the current tool batch"
        );
        let steering_msg = req.messages[steering_idx].content.as_deref().unwrap();
        assert!(steering_msg.contains("[Mid-run steering:"));

        let mut saw_steering_event = false;
        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::RefineToolInvoked { tool, summary } = ev
                && tool == "steering"
            {
                saw_steering_event = true;
                assert!(summary.contains("1 queued"));
            }
        }
        assert!(
            saw_steering_event,
            "the UI is told steering was queued into context"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn refine_turn_continues_at_stop_when_steering_is_queued() {
        let root = temp_root("steeratstop");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let etx = EventTx(tx);
        let client = ScriptedClient {
            responses: Mutex::new(VecDeque::from(vec![
                stop_turn("initial answer"),
                stop_turn("after steering"),
            ])),
        };
        let tools = RefineTools::new(root.clone(), 1, etx.clone());
        let mut req = ChatRequest::new("m", vec![Message::system("sys")]);
        req.messages.push(Message::user("start"));
        let cancel = AtomicBool::new(false);
        let interact = RefineInteract::default();
        let steering = Arc::new(Mutex::new(VecDeque::from(vec![UserTurn {
            text: "one more thing".to_string(),
            from: None,
        }])));
        let runtime = test_runtime(&etx, &cancel, &interact, &steering);

        run_refine_turn(&client, &mut req, &tools, &runtime).await;

        let contents: Vec<&str> = req
            .messages
            .iter()
            .filter_map(|m| m.content.as_deref())
            .collect();
        assert!(contents.contains(&"initial answer"));
        assert!(
            contents
                .iter()
                .any(|c| c.contains("one more thing") && c.contains("[Mid-run steering:"))
        );
        assert!(contents.contains(&"after steering"));

        let done_count = std::iter::from_fn(|| rx.try_recv().ok())
            .filter(|ev| matches!(ev, AppEvent::RefineMessageDone))
            .count();
        assert_eq!(done_count, 1);

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
    async fn refine_accepts_legacy_target_field_arguments() {
        let root = temp_root("legacy_target_args");
        let ws = Workspace::new(root.clone(), 1);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let tx = EventTx(tx);

        let character = dispatch_refine_tool(
            &root,
            1,
            &tx,
            "upsert_character",
            r#"{"jp_name":"勇","thai_name":"ยู","also_called":[{"jp":"兄ちゃん","thai":"พี่"}]}"#,
        )
        .await;
        assert!(character.ok, "{}", character.message);
        let saved_character = characters::load(&ws).pop().unwrap();
        assert_eq!(saved_character.translated_name, "ยู");
        assert_eq!(saved_character.also_called[0].translated_name, "พี่");

        let term = dispatch_refine_tool(
            &root,
            1,
            &tx,
            "upsert_glossary_term",
            r#"{"jp_term":"魔法","thai_term":"เวทมนตร์","forbidden_thai":["มายากล"]}"#,
        )
        .await;
        assert!(term.ok, "{}", term.message);
        let saved_term = glossary::load(&ws).pop().unwrap();
        assert_eq!(saved_term.translated_term, "เวทมนตร์");
        assert_eq!(saved_term.forbidden_translations, ["มายากล"]);

        let synopsis = dispatch_refine_tool(
            &root,
            1,
            &tx,
            "set_synopsis",
            r#"{"raw":"粗筋","thai":"เรื่องย่อ"}"#,
        )
        .await;
        assert!(synopsis.ok, "{}", synopsis.message);

        let example = dispatch_refine_tool(
            &root,
            1,
            &tx,
            "add_style_example",
            r#"{"jp":"彼は笑った。","th":"เขาหัวเราะ"}"#,
        )
        .await;
        assert!(example.ok, "{}", example.message);
        let volume = volume::load(&ws);
        assert_eq!(volume.translated_synopsis, "เรื่องย่อ");
        assert_eq!(volume.style_examples[0].translated_text, "เขาหัวเราะ");

        let _ = std::fs::remove_dir_all(&root);
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
            r#"{"jp_name":"勇","translated_name":"ยู"}"#,
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
                .any(|c| c.get("translated_name").and_then(|v| v.as_str()) == Some("ยู"))
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
        assert_eq!(
            crate::model::ModelSet::default().refine.model,
            "openai/gpt-5.5"
        );
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
    async fn edit_chapter_clears_review_marker_for_touched_chunk_only() {
        let root = temp_root("editflag");
        let ws = Workspace::new(root.clone(), 1);
        std::fs::create_dir_all(ws.translated(1).parent().unwrap()).unwrap();
        translation::append_chunk_needs_review(&ws, 1, 0, "ร่างเก่า", 3, "tone drift")
            .await
            .unwrap();
        translation::append_chunk_needs_review(&ws, 1, 1, "อีกจุด", 3, "missing line")
            .await
            .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let tx = EventTx(tx);

        let r = dispatch_refine_tool(
            &root,
            1,
            &tx,
            "edit_chapter",
            r#"{"ch":1,"old":"ร่างเก่า","new":"แก้แล้ว"}"#,
        )
        .await;
        assert!(r.ok, "{}", r.message);
        let now = translation::read_translated(&ws, 1).await;
        assert_eq!(
            translation::review_needed_chunk_indices_in(&now),
            std::collections::BTreeSet::from([1])
        );
        assert_eq!(
            translation::chunk_prose_in(&now, 0).as_deref(),
            Some("แก้แล้ว")
        );
        assert_eq!(
            crate::workspace::scan::derive_status(&ws.vol_dir, 1, crate::model::ChapterKind::Prose),
            crate::model::ChapterStatus::NeedsReview
        );

        let r = dispatch_refine_tool(
            &root,
            1,
            &tx,
            "edit_chapter",
            r#"{"ch":1,"old":"อีกจุด","new":"แก้ครบแล้ว"}"#,
        )
        .await;
        assert!(r.ok, "{}", r.message);
        let now = translation::read_translated(&ws, 1).await;
        assert!(translation::review_needed_chunk_indices_in(&now).is_empty());
        assert_eq!(
            crate::workspace::scan::derive_status(&ws.vol_dir, 1, crate::model::ChapterKind::Prose),
            crate::model::ChapterStatus::Done
        );

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

    #[tokio::test]
    async fn update_plan_accepts_empty_steps_to_clear() {
        let root = temp_root("planclear");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let etx = EventTx(tx);
        let r = dispatch_refine_tool(&root, 1, &etx, "update_plan", r#"{"steps":[]}"#).await;
        assert!(r.ok, "{}", r.message);
        assert_eq!(r.message, "plan cleared");
        let mut saw_empty = false;
        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::RefinePlanUpdated { steps } = ev {
                saw_empty = steps.is_empty();
            }
        }
        assert!(saw_empty, "empty plan event clears the UI checklist");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn subagent_update_plan_does_not_replace_main_plan() {
        let root = temp_root("subagentplan");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let tools = RefineTools::new(root.clone(), 1, EventTx(tx)).child("call_1".to_string(), SubagentRole::General);

        let result = tools
            .execute(
                "update_plan",
                r#"{"steps":[{"step":"read chapter","status":"in_progress"}]}"#,
            )
            .await
            .unwrap();
        assert!(result.contains("plan updated"));

        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, AppEvent::RefinePlanUpdated { .. })),
            "a sub-agent plan must not be emitted as the main plan: {events:?}"
        );
        assert!(events.iter().any(|event| matches!(
            event,
            AppEvent::RefineSubagentPlanUpdated { id, steps }
                if id == "call_1"
                    && steps.len() == 1
                    && steps[0].step == "read chapter"
        )));

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
        translation::append_chunk(&ws, 1, 0, "ดาบเก่า")
            .await
            .unwrap();
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
            translation::read_translated(&ws, 1)
                .await
                .contains("ดาบเก่า"),
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

    /// Both wire shapes reach the same card, so a model that still sends the
    /// one-question form keeps working while the batched form is the one the
    /// tool description asks for.
    #[test]
    fn ask_user_accepts_the_batch_and_the_single_question_shorthand() {
        let batch = parse_ask_user(
            r#"{"questions":[
                {"question":"Which rendering?","options":["a","b"]},
                {"question":"Which chapters?","options":["1","2"],"multiple":true}
            ]}"#,
        )
        .unwrap();
        assert_eq!(batch.len(), 2);
        assert!(batch[1].multiple);
        assert!(!batch[0].multiple, "multiple is opt-in per question");

        let single = parse_ask_user(r#"{"question":"Which rendering?","options":["a"]}"#).unwrap();
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].options, vec!["a".to_string()]);

        // `questions` wins, and a blank entry is not a question.
        let mixed = parse_ask_user(
            r#"{"question":"ignored","questions":[{"question":"real"},{"question":"  "}]}"#,
        )
        .unwrap();
        assert_eq!(mixed.len(), 1);
        assert_eq!(mixed[0].question, "real");

        assert!(parse_ask_user(r#"{"questions":[]}"#).is_err());
    }

    /// The card replies with an array; anything else is one free-text answer,
    /// which is what a dismissal and an older reply both are.
    #[test]
    fn answers_decode_from_the_array_and_fall_back_to_one_string() {
        assert_eq!(decode_answers(r#"["a","b"]"#, 2), vec!["a", "b"]);
        assert_eq!(decode_answers("plain", 1), vec!["plain"]);
        assert_eq!(decode_answers("", 2), vec!["", ""], "a dismissal stays empty");
        assert_eq!(
            decode_answers(r#"["only"]"#, 3),
            vec!["only", "", ""],
            "a short reply is padded, never panics"
        );
    }
}
