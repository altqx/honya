//! Refine chat agent backend: cross-volume tools plus the streaming loop.
//! Chapter-text edits archive the prior version before overwriting.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app::refine::{MentionTarget, parse_scope};
use crate::llm::client::LlmClient;
use crate::llm::tool_loop::ToolExecutor;
use crate::llm::{ChatRequest, Message, Role, Tool};
use crate::model::{
    AppEvent, Character, EventTx, GlossaryTerm, RefineRequest, Relationship, StyleExample,
    TermPolicy, ToolResult,
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
            "description":"Read a chapter's Japanese source and/or its current Thai translation (prose only). Use before editing so you work from the real text.",
            "parameters":{"type":"object","additionalProperties":false,"required":["ch"],
                "properties":{
                    "vol":{"type":"integer","description":"Volume number; defaults to the active volume."},
                    "ch":{"type":"integer"},
                    "include_jp":{"type":"boolean","description":"Include the Japanese source (default true)."},
                    "include_th":{"type":"boolean","description":"Include the Thai translation (default true)."}
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
            "name":"replace_chapter_text",
            "description":"Replace a chapter's ENTIRE Thai translation with new_text. The prior version is archived first (reversible via /undo and the Reader diff). Use for a full rewrite; for a small edit prefer find_replace_in_chapter.",
            "parameters":{"type":"object","additionalProperties":false,"required":["ch","new_text"],
                "properties":{"vol":{"type":"integer"},"ch":{"type":"integer"},"new_text":{"type":"string"}}}
        }},
        {"type":"function","function":{
            "name":"find_replace_in_chapter",
            "description":"Replace occurrences of `find` with `replace` in a chapter's Thai. The prior version is archived first. Set all=true to replace every occurrence (default: first only).",
            "parameters":{"type":"object","additionalProperties":false,"required":["ch","find","replace"],
                "properties":{
                    "vol":{"type":"integer"},"ch":{"type":"integer"},
                    "find":{"type":"string"},"replace":{"type":"string"},
                    "all":{"type":"boolean"}
                }}
        }},
        {"type":"function","function":{
            "name":"retranslate_chapter",
            "description":"Re-run the full Translator→Reviewer pipeline on whole chapters (a fresh, high-quality regeneration). Use this for 'redo this chapter properly'; for a small targeted fix prefer find_replace_in_chapter. The current translation is archived first.",
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
        }}
    ])
}

const MAX_TOOL_ROUNDS: usize = 16;

pub struct RefineCtx {
    pub client: Arc<dyn LlmClient>,
    pub root: PathBuf,
    pub default_vol: u32,
    pub model: String,
    pub tx: EventTx,
    /// Flipped to stop the in-flight turn between rounds (also set by `/clear`).
    pub cancel: Arc<AtomicBool>,
    pub session_id: String,
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
    Shutdown,
}

fn seed_messages(root: &Path, id: &str) -> Vec<Message> {
    crate::workspace::refine_session::load(root, id)
        .map(|s| s.messages)
        .unwrap_or_default()
}

fn refine_system_prompt() -> String {
    "You are honya's Refine agent, an expert assistant for a Japanese→Thai light-novel \
translation project. You help the user read, fix, and refine anything in the project: \
any volume or chapter's Thai translation, the character roster, the glossary, the style \
guide, the volume recap/synopsis, and chapter summaries.\n\n\
Tools let you read and edit the project on disk. Always read the relevant text (read_chapter, \
read_lexicon, search_project) before you change it, so you edit the real content. Edits are \
applied immediately and are reversible (chapter-text edits archive the prior version), so be \
precise and surgical: for a small wording fix use find_replace_in_chapter; only use \
replace_chapter_text for a full rewrite you have actually produced. Keep Thai natural and \
consistent with the established glossary and character voices.\n\n\
Tools without an explicit `vol` act on the active volume. When the user tags a chapter like \
@v1/c3, operate on that chapter. Reply concisely in the user's language, and after making \
edits briefly say what you changed."
        .to_string()
}

fn refine_tools_vec() -> Vec<Tool> {
    serde_json::from_value(refine_tools_schema()).unwrap_or_default()
}

/// Owns the live chat thread so multi-turn history persists.
pub async fn run_refine_agent(ctx: RefineCtx, mut rx: UnboundedReceiver<RefineControl>) {
    let tools = RefineTools::new(ctx.root.clone(), ctx.default_vol, ctx.tx.clone());
    let mut req = ChatRequest::new(
        ctx.model.clone(),
        vec![Message::system(refine_system_prompt())],
    );
    req.tools = Some(refine_tools_vec());
    let mut current_id = ctx.session_id.clone();
    req.messages.extend(seed_messages(&ctx.root, &current_id));

    while let Some(ctrl) = rx.recv().await {
        match ctrl {
            RefineControl::Submit(turn) => {
                ctx.cancel.store(false, Ordering::Relaxed);
                let msg = build_user_message(&ctx, &turn);
                req.messages.push(Message::user(msg));
                run_refine_turn(ctx.client.as_ref(), &mut req, &tools, &ctx.tx, &ctx.cancel).await;
                // Hand the updated thread to the App to persist (sole writer), tagged
                // with the session it belongs to so a stale update is dropped.
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

        let mut on_delta = |s: &str| {
            tx.send(AppEvent::RefineDelta {
                delta: s.to_string(),
            })
        };
        let resp = match client.chat_stream(req, &mut on_delta).await {
            Ok(r) => r,
            Err(e) => {
                tx.send(AppEvent::RefineError { msg: e.to_string() });
                return;
            }
        };

        let Some(choice) = resp.choices.first() else {
            tx.send(AppEvent::RefineError {
                msg: "the model returned no choices".to_string(),
            });
            return;
        };
        let content = choice.message.content.clone();
        let tool_calls = choice.message.tool_calls.clone().unwrap_or_default();

        if tool_calls.is_empty() {
            // Final text turn: persist it so the next user turn sees the reply.
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
            tx.send(AppEvent::RefineToolInvoked {
                tool: call.function.name.clone(),
                summary: summarize_args(&call.function.arguments),
            });
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

/// Persist only a short scope hint, not full chapter text.
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
struct FindReplaceArgs {
    #[serde(default)]
    vol: Option<u32>,
    ch: u32,
    find: String,
    replace: String,
    #[serde(default)]
    all: bool,
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
            let mut data = serde_json::Map::new();
            if a.include_jp {
                let jp = std::fs::read_to_string(w.raw(a.ch)).unwrap_or_default();
                data.insert("japanese".into(), json!(cap(&jp)));
            }
            if a.include_th {
                let th = translation::read_translated(&w, a.ch).await;
                data.insert("thai".into(), json!(cap(&translation::prose_only(&th))));
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

        "find_replace_in_chapter" => {
            let a = parse!(FindReplaceArgs);
            let w = ws(a.vol);
            let path = w.translated(a.ch);
            let content = translation::read_translated(&w, a.ch).await;
            if content.is_empty() {
                return ToolResult::err(format!("chapter {} has no translation yet", a.ch));
            }
            if a.find.is_empty() || !content.contains(&a.find) {
                return ToolResult::ok(format!(
                    "no occurrences of the search text in chapter {}",
                    a.ch
                ));
            }
            if let Err(e) = archive_chapter(&w, a.ch) {
                return ToolResult::err(format!("could not archive prior version: {e}"));
            }
            let (updated, n) = if a.all {
                (
                    content.replace(&a.find, &a.replace),
                    content.matches(&a.find).count(),
                )
            } else {
                (content.replacen(&a.find, &a.replace, 1), 1)
            };
            if let Err(e) = tokio::fs::write(&path, updated).await {
                return ToolResult::err(format!("write failed: {e}"));
            }
            emit_edit(
                tx,
                "chapter text",
                &format!("chapter {}: {n} replacement(s)", a.ch),
            );
            tx.send(AppEvent::RefineChapterEdited {
                vol: a.vol.unwrap_or(default_vol),
                ch: a.ch,
            });
            ToolResult::ok(format!("replaced {n} occurrence(s) in chapter {}", a.ch))
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
}

impl RefineTools {
    pub fn new(root: PathBuf, default_vol: u32, tx: EventTx) -> Self {
        Self {
            root,
            default_vol,
            tx,
        }
    }
}

#[async_trait]
impl ToolExecutor for RefineTools {
    async fn execute(&self, name: &str, arguments_json: &str) -> anyhow::Result<String> {
        let result =
            dispatch_refine_tool(&self.root, self.default_vol, &self.tx, name, arguments_json)
                .await;
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
        assert_eq!(crate::model::ModelSet::default().refine, "openai/gpt-5.5");
    }

    #[tokio::test]
    async fn find_replace_edits_prose_in_place() {
        let root = temp_root("findrep");
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
            "find_replace_in_chapter",
            r#"{"ch":1,"find":"แมว","replace":"สุนัข","all":true}"#,
        )
        .await;
        assert!(r.ok, "{}", r.message);
        let now = translation::prose_only(&translation::read_translated(&ws, 1).await);
        assert_eq!(now, "สุนัขสีดำ และ สุนัขสีขาว");

        let _ = std::fs::remove_dir_all(&root);
    }
}
