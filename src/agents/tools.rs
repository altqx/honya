//! src/agents/tools.rs — the Orchestrator's 7 backend tools.
//!
//! The Orchestrator runs as a metadata turn after each committed chunk. It
//! emits OpenAI-style tool calls; `dispatch_tool` parses the JSON arguments,
//! mutates the on-disk workspace metadata (CHARACTERS.md / GLOSSARY.md /
//! VOLUME.md / translated/), and emits the corresponding `AppEvent` so the UI
//! reflects the change. `WorkspaceTools` adapts this to the generic
//! `ToolExecutor` driven by `llm::tool_loop::run_tool_loop`.
//!
//! The 7 tools (verbatim from the design):
//!   1. upsert_character        2. upsert_glossary_term   3. update_volume_recap
//!   4. append_translation      5. flag_continuity_note   6. get_glossary (read)
//!   7. get_character (read)

use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::llm::tool_loop::ToolExecutor;
use crate::model::{
    AppEvent, Character, ContinuityNote, EventTx, GlossaryTerm, Relationship, ToolResult,
};
use crate::workspace::{Workspace, characters, glossary, translation, volume};

/// The full `tools` array advertised to the Orchestrator. Each entry is an
/// OpenAI-style `{"type":"function","function":{name,description,parameters}}`
/// where `parameters` is a strict JSON Schema (`additionalProperties:false`).
pub fn orchestrator_tools() -> serde_json::Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "upsert_character",
                "description": "Create or update a character in CHARACTERS.md. Use whenever a new character appears or an existing character's name, gender, honorific, speech style, or relationships change.",
                "parameters": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["jp_name", "thai_name"],
                    "properties": {
                        "id": {"type": "string", "description": "Stable slug; if omitted it is derived from jp_name."},
                        "jp_name": {"type": "string", "description": "Japanese name as written in the source."},
                        "thai_name": {"type": "string", "description": "Canonical Thai rendering of the name."},
                        "romaji": {"type": "string"},
                        "gender": {"type": "string", "enum": ["male", "female", "nonbinary", "unknown"]},
                        "honorific": {"type": "string", "description": "Honorific or suffix convention, e.g. さん / -kun."},
                        "speech_style": {"type": "string", "description": "Register / pronoun usage notes for dialogue."},
                        "relationships": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "additionalProperties": false,
                                "required": ["target_id", "relation"],
                                "properties": {
                                    "target_id": {"type": "string"},
                                    "relation": {"type": "string"}
                                }
                            }
                        },
                        "notes": {"type": "string"},
                        "first_seen_chapter": {"type": "integer"}
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "upsert_glossary_term",
                "description": "Create or update a glossary term in GLOSSARY.md (skills, places, organizations, items, titles, concepts, sound effects).",
                "parameters": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["jp_term", "thai_term"],
                    "properties": {
                        "jp_term": {"type": "string"},
                        "thai_term": {"type": "string"},
                        "romaji": {"type": "string"},
                        "category": {"type": "string", "enum": ["skill", "place", "org", "item", "title", "concept", "sfx", "other"]},
                        "gloss": {"type": "string", "description": "Short clarifying note about meaning or usage."},
                        "do_not_translate": {"type": "boolean", "description": "True to keep the term verbatim / romanized."},
                        "first_seen_chapter": {"type": "integer"}
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "update_volume_recap",
                "description": "Update VOLUME.md: set this chapter's one-line summary and/or the running cross-chapter recap.",
                "parameters": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["chapter"],
                    "properties": {
                        "chapter": {"type": "integer"},
                        "chapter_summary": {"type": "string", "description": "One-line summary for this chapter."},
                        "running_recap": {"type": "string", "description": "Updated cross-chapter running recap."}
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "append_translation",
                "description": "Append approved Thai text to translated/ch_NNN.md. The app appends deterministically; this tool is a secondary, idempotent path keyed by chunk_index.",
                "parameters": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["chapter", "chunk_index", "thai_text"],
                    "properties": {
                        "chapter": {"type": "integer"},
                        "chunk_index": {"type": "integer"},
                        "thai_text": {"type": "string"}
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "flag_continuity_note",
                "description": "Record a continuity observation in VOLUME.md (name/gender drift, term inconsistency, plot or tone concern).",
                "parameters": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["severity", "note"],
                    "properties": {
                        "severity": {"type": "string", "enum": ["info", "warning", "conflict"]},
                        "note": {"type": "string"},
                        "chapter": {"type": "integer"},
                        "kind": {"type": "string", "enum": ["name", "gender", "term", "plot", "tone", "other"]}
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "get_glossary",
                "description": "Read glossary terms from GLOSSARY.md, optionally filtered by query/category, to check existing locked terminology before inventing new terms.",
                "parameters": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": [],
                    "properties": {
                        "query": {"type": "string"},
                        "category": {"type": "string", "enum": ["skill", "place", "org", "item", "title", "concept", "sfx", "other"]},
                        "limit": {"type": "integer"}
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "get_character",
                "description": "Read characters from CHARACTERS.md, optionally filtered by query or id, to confirm an existing character before adding a duplicate.",
                "parameters": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": [],
                    "properties": {
                        "query": {"type": "string"},
                        "id": {"type": "string"}
                    }
                }
            }
        }
    ])
}

// --- per-tool argument structs (deserialized from the JSON `arguments` string)

#[derive(Debug, Deserialize)]
struct UpsertCharacterArgs {
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
    notes: Option<String>,
    #[serde(default)]
    first_seen_chapter: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct UpsertGlossaryArgs {
    jp_term: String,
    thai_term: String,
    #[serde(default)]
    romaji: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    gloss: Option<String>,
    #[serde(default)]
    do_not_translate: Option<bool>,
    #[serde(default)]
    first_seen_chapter: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct UpdateRecapArgs {
    chapter: u32,
    #[serde(default)]
    chapter_summary: Option<String>,
    #[serde(default)]
    running_recap: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AppendTranslationArgs {
    chapter: u32,
    chunk_index: u32,
    thai_text: String,
}

#[derive(Debug, Deserialize)]
struct FlagNoteArgs {
    severity: String,
    note: String,
    #[serde(default)]
    chapter: Option<u32>,
    #[serde(default)]
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GetGlossaryArgs {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct GetCharacterArgs {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    id: Option<String>,
}

/// Slugify a Japanese (or any) name into a stable ascii-ish id when the model
/// omits an explicit `id`. Non-alphanumeric runs collapse to `-`; CJK chars are
/// kept verbatim (lowercased ascii where applicable) so ids stay unique.
fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in name.trim().chars() {
        if c.is_alphanumeric() {
            for lc in c.to_lowercase() {
                out.push(lc);
            }
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("char");
    }
    out
}

/// Execute one Orchestrator tool call against the workspace, emit the matching
/// `AppEvent`, and return a `ToolResult` to feed back to the model. A malformed
/// argument string or unknown tool name yields a `ToolResult::err` (the loop
/// keeps going so the model can recover).
pub async fn dispatch_tool(
    ws: &Workspace,
    tx: &EventTx,
    chapter: u32,
    name: &str,
    args_json: &str,
) -> ToolResult {
    match name {
        "upsert_character" => {
            let a: UpsertCharacterArgs = match serde_json::from_str(args_json) {
                Ok(a) => a,
                Err(e) => return ToolResult::err(format!("invalid upsert_character args: {e}")),
            };
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
                notes: a.notes,
                first_seen_chapter: a.first_seen_chapter.or(Some(chapter)),
            };
            match characters::upsert(ws, character) {
                Ok(()) => {
                    tx.send(AppEvent::CharacterUpserted {
                        id: id.clone(),
                        jp_name: a.jp_name.clone(),
                        thai_name: a.thai_name.clone(),
                    });
                    tx.send(AppEvent::ToolInvoked {
                        chapter,
                        tool: name.to_string(),
                        summary: format!("character {} ({} → {})", id, a.jp_name, a.thai_name),
                    });
                    ToolResult::ok(format!("Upserted character {} ({})", a.thai_name, id))
                }
                Err(e) => ToolResult::err(format!("failed to write character: {e}")),
            }
        }

        "upsert_glossary_term" => {
            let a: UpsertGlossaryArgs = match serde_json::from_str(args_json) {
                Ok(a) => a,
                Err(e) => {
                    return ToolResult::err(format!("invalid upsert_glossary_term args: {e}"));
                }
            };
            let term = GlossaryTerm {
                jp_term: a.jp_term.clone(),
                thai_term: a.thai_term.clone(),
                romaji: a.romaji,
                category: a.category,
                gloss: a.gloss,
                do_not_translate: a.do_not_translate,
                first_seen_chapter: a.first_seen_chapter.or(Some(chapter)),
            };
            match glossary::upsert(ws, term) {
                Ok(()) => {
                    tx.send(AppEvent::GlossaryUpserted {
                        jp_term: a.jp_term.clone(),
                        thai_term: a.thai_term.clone(),
                    });
                    tx.send(AppEvent::ToolInvoked {
                        chapter,
                        tool: name.to_string(),
                        summary: format!("term {} → {}", a.jp_term, a.thai_term),
                    });
                    ToolResult::ok(format!("Upserted term {} → {}", a.jp_term, a.thai_term))
                }
                Err(e) => ToolResult::err(format!("failed to write glossary term: {e}")),
            }
        }

        "update_volume_recap" => {
            let a: UpdateRecapArgs = match serde_json::from_str(args_json) {
                Ok(a) => a,
                Err(e) => return ToolResult::err(format!("invalid update_volume_recap args: {e}")),
            };
            let mut touched = false;
            if let Some(summary) = a.chapter_summary.as_deref() {
                if let Err(e) = volume::set_chapter_summary(ws, a.chapter, summary) {
                    return ToolResult::err(format!("failed to set chapter summary: {e}"));
                }
                touched = true;
            }
            if let Some(recap) = a.running_recap.as_deref() {
                if let Err(e) = volume::set_recap(ws, recap) {
                    return ToolResult::err(format!("failed to set running recap: {e}"));
                }
                touched = true;
            }
            if !touched {
                return ToolResult::ok("Nothing to update (no summary or recap provided)");
            }
            tx.send(AppEvent::VolumeRecapUpdated { chapter: a.chapter });
            tx.send(AppEvent::ToolInvoked {
                chapter,
                tool: name.to_string(),
                summary: format!("volume recap updated for chapter {}", a.chapter),
            });
            ToolResult::ok(format!("Updated volume recap for chapter {}", a.chapter))
        }

        "append_translation" => {
            let a: AppendTranslationArgs = match serde_json::from_str(args_json) {
                Ok(a) => a,
                Err(e) => return ToolResult::err(format!("invalid append_translation args: {e}")),
            };
            match translation::append_chunk(ws, a.chapter, a.chunk_index, &a.thai_text).await {
                Ok(bytes) => {
                    tx.send(AppEvent::ToolInvoked {
                        chapter,
                        tool: name.to_string(),
                        summary: format!(
                            "appended chunk {} to chapter {} ({} bytes)",
                            a.chunk_index, a.chapter, bytes
                        ),
                    });
                    ToolResult::ok(format!(
                        "Appended chunk {} to chapter {} ({bytes} bytes)",
                        a.chunk_index, a.chapter
                    ))
                }
                Err(e) => ToolResult::err(format!("failed to append translation: {e}")),
            }
        }

        "flag_continuity_note" => {
            let a: FlagNoteArgs = match serde_json::from_str(args_json) {
                Ok(a) => a,
                Err(e) => {
                    return ToolResult::err(format!("invalid flag_continuity_note args: {e}"));
                }
            };
            let note = ContinuityNote {
                chapter: a.chapter.or(Some(chapter)),
                severity: a.severity.clone(),
                kind: a.kind.clone(),
                note: a.note.clone(),
            };
            match volume::add_note(ws, note) {
                Ok(()) => {
                    tx.send(AppEvent::ContinuityFlag {
                        chapter: a.chapter.unwrap_or(chapter),
                        severity: a.severity.clone(),
                        kind: a.kind.clone().unwrap_or_else(|| "other".to_string()),
                        note: a.note.clone(),
                    });
                    tx.send(AppEvent::ToolInvoked {
                        chapter,
                        tool: name.to_string(),
                        summary: format!("continuity [{}] {}", a.severity, a.note),
                    });
                    ToolResult::ok(format!("Flagged continuity note ({})", a.severity))
                }
                Err(e) => ToolResult::err(format!("failed to write continuity note: {e}")),
            }
        }

        "get_glossary" => {
            let a: GetGlossaryArgs = match serde_json::from_str(args_json) {
                Ok(a) => a,
                Err(e) => return ToolResult::err(format!("invalid get_glossary args: {e}")),
            };
            let limit = a.limit.unwrap_or(50);
            let terms = glossary::get(ws, a.query.as_deref(), a.category.as_deref(), limit);
            tx.send(AppEvent::ToolInvoked {
                chapter,
                tool: name.to_string(),
                summary: format!("read {} glossary term(s)", terms.len()),
            });
            let data = serde_json::to_value(&terms).unwrap_or(serde_json::Value::Null);
            ToolResult::data(
                format!("Found {} glossary term(s)", terms.len()),
                json!({ "terms": data }),
            )
        }

        "get_character" => {
            let a: GetCharacterArgs = match serde_json::from_str(args_json) {
                Ok(a) => a,
                Err(e) => return ToolResult::err(format!("invalid get_character args: {e}")),
            };
            let chars = characters::get(ws, a.query.as_deref(), a.id.as_deref());
            tx.send(AppEvent::ToolInvoked {
                chapter,
                tool: name.to_string(),
                summary: format!("read {} character(s)", chars.len()),
            });
            let data = serde_json::to_value(&chars).unwrap_or(serde_json::Value::Null);
            ToolResult::data(
                format!("Found {} character(s)", chars.len()),
                json!({ "characters": data }),
            )
        }

        other => ToolResult::err(format!("unknown tool: {other}")),
    }
}

/// A `ToolExecutor` bound to one project workspace, event channel, and chapter.
///
/// `Workspace` is not assumed to be `Clone`, so we store its constituent parts
/// (`root` + `vol_number`) and rebuild a fresh `Workspace` for each call.
pub struct WorkspaceTools {
    root: PathBuf,
    vol_number: u32,
    tx: EventTx,
    chapter: u32,
}

impl WorkspaceTools {
    /// Build a tool executor for `chapter` in the given project root + volume.
    pub fn new(root: PathBuf, vol_number: u32, tx: EventTx, chapter: u32) -> Self {
        Self {
            root,
            vol_number,
            tx,
            chapter,
        }
    }

    fn workspace(&self) -> Workspace {
        Workspace::new(self.root.clone(), self.vol_number)
    }
}

#[async_trait]
impl ToolExecutor for WorkspaceTools {
    async fn execute(&self, name: &str, arguments_json: &str) -> anyhow::Result<String> {
        let ws = self.workspace();
        let result = dispatch_tool(&ws, &self.tx, self.chapter, name, arguments_json).await;
        Ok(serde_json::to_string(&result)?)
    }
}
