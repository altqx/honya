//! The Orchestrator's backend tools.
//!
//! The Orchestrator emits OpenAI-style tool calls; `dispatch_tool` parses the
//! JSON arguments, mutates the on-disk workspace metadata (CHARACTERS.md /
//! GLOSSARY.md / VOLUME.md / translated/), and emits the matching `AppEvent`.
//! `WorkspaceTools` adapts this to the generic `ToolExecutor`.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::llm::tool_loop::ToolExecutor;
use crate::model::{
    AppEvent, Character, ContinuityNote, EventTx, GlossaryTerm, Relationship, TermPolicy,
    ToolResult,
};
use crate::workspace::{Workspace, characters, glossary, translation, volume};

/// The OpenAI-style `tools` array advertised to the Orchestrator (strict JSON Schema).
pub fn orchestrator_tools() -> serde_json::Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "upsert_character",
                "description": "Create or update a character in CHARACTERS.md. Use whenever a new character appears or an existing character's name, gender, honorific, speech style, or relationships change. To avoid duplicates, prefer the FULL name (surname + given name) as jp_name and list shorter/variant surface forms in `aliases`; call get_character first to check whether the character already exists.",
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
                        "aliases": {"type": "array", "items": {"type": "string"}, "description": "Alternate JP surface forms of THIS SAME character (e.g. bare given name, alternate kanji). Used to fold name variants into one entry instead of creating duplicates."},
                        "notes": {"type": "string"},
                        "first_seen_chapter": {"type": "integer"}
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "merge_character",
                "description": "Consolidate two CHARACTERS.md entries that are the SAME person (e.g. a bare given name recorded separately from the full name, or the same character under different kanji). Keeps `into_id` as canonical, folds the other entry's fields/relationships in, records its name forms as aliases, repoints references, and removes `from_id`. Use this when get_character or an upsert result shows duplicate entries for one character.",
                "parameters": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["from_id", "into_id"],
                    "properties": {
                        "from_id": {"type": "string", "description": "Id of the duplicate entry to remove."},
                        "into_id": {"type": "string", "description": "Id of the surviving canonical entry (prefer the one with the fuller name)."}
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "upsert_glossary_term",
                "description": "Create or update a glossary term in GLOSSARY.md (skills, places, organizations, items, titles, concepts, sound effects). Existing controlled/protected terms (hard_locked, forbidden, context_dependent, or protected=true) will not be overwritten by automatic Orchestrator updates.",
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
                        "policy": {"type": "string", "enum": ["hard_locked", "preferred", "forbidden", "context_dependent"], "description": "Terminology policy. Use preferred for normal discoveries; hard_locked/forbidden/context_dependent are human controls and block automatic overwrites."},
                        "forbidden_thai": {"type": "array", "items": {"type": "string"}, "description": "Thai renderings that must not be used for this Japanese term."},
                        "context_rule": {"type": "string", "description": "Rule for context_dependent terms (when to use each rendering)."},
                        "protected": {"type": "boolean", "description": "Back-compat human protection flag; protected existing terms cannot be automatically overwritten."},
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
                "description": "Read glossary terms from GLOSSARY.md, optionally filtered by query/category/policy/protected_only, to check existing terminology controls before inventing new terms.",
                "parameters": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": [],
                    "properties": {
                        "query": {"type": "string"},
                        "category": {"type": "string", "enum": ["skill", "place", "org", "item", "title", "concept", "sfx", "other"]},
                        "policy": {"type": "string", "enum": ["hard_locked", "preferred", "forbidden", "context_dependent"]},
                        "protected_only": {"type": "boolean", "description": "True to return only terms that block automatic overwrite (hard locked, forbidden, context dependent, or protected)."},
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

// Per-tool argument structs deserialized from the JSON `arguments` string.

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
    aliases: Vec<String>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    first_seen_chapter: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct MergeCharacterArgs {
    from_id: String,
    into_id: String,
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
    policy: Option<TermPolicy>,
    #[serde(default)]
    forbidden_thai: Vec<String>,
    #[serde(default)]
    context_rule: Option<String>,
    #[serde(default)]
    protected: Option<bool>,
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
    policy: Option<TermPolicy>,
    #[serde(default)]
    protected_only: Option<bool>,
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

/// Derive a stable id from a name when the model omits `id`; non-alphanumeric runs collapse to `-`, CJK kept verbatim.
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

/// Execute one tool call, emit the matching `AppEvent`, return a `ToolResult`.
/// Bad args or unknown tool yield `ToolResult::err` so the loop can recover.
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
                aliases: a.aliases,
                notes: a.notes,
                first_seen_chapter: a.first_seen_chapter.or(Some(chapter)),
            };
            match characters::upsert(ws, character) {
                Ok(outcome) => {
                    tx.send(AppEvent::CharacterUpserted {
                        id: id.clone(),
                        jp_name: a.jp_name.clone(),
                        thai_name: a.thai_name.clone(),
                    });
                    let summary = match &outcome {
                        characters::CharacterUpsertOutcome::Merged { into_id } => {
                            format!("character {} merged into {}", a.jp_name, into_id)
                        }
                        _ => format!("character {} ({} → {})", id, a.jp_name, a.thai_name),
                    };
                    tx.send(AppEvent::ToolInvoked {
                        chapter,
                        tool: name.to_string(),
                        summary,
                    });
                    match outcome {
                        characters::CharacterUpsertOutcome::InsertedWithCandidates {
                            id,
                            candidates,
                        } => ToolResult::data(
                            format!(
                                "Saved character {} ({}), but it may duplicate existing entr{}: {}. If any is the same person, call merge_character(from_id, into_id) to consolidate (keep the fuller name as into_id).",
                                a.thai_name,
                                id,
                                if candidates.len() == 1 { "y" } else { "ies" },
                                candidates.join(", "),
                            ),
                            json!({ "id": id, "merge_candidates": candidates }),
                        ),
                        characters::CharacterUpsertOutcome::Merged { into_id } => ToolResult::ok(
                            format!("Merged character {} into {}", a.thai_name, into_id),
                        ),
                        characters::CharacterUpsertOutcome::Inserted => {
                            ToolResult::ok(format!("Upserted character {} ({})", a.thai_name, id))
                        }
                    }
                }
                Err(e) => ToolResult::err(format!("failed to write character: {e}")),
            }
        }

        "merge_character" => {
            let a: MergeCharacterArgs = match serde_json::from_str(args_json) {
                Ok(a) => a,
                Err(e) => return ToolResult::err(format!("invalid merge_character args: {e}")),
            };
            match characters::merge(ws, &a.from_id, &a.into_id) {
                Ok(true) => {
                    tx.send(AppEvent::ToolInvoked {
                        chapter,
                        tool: name.to_string(),
                        summary: format!("merged character {} into {}", a.from_id, a.into_id),
                    });
                    ToolResult::ok(format!("Merged character {} into {}", a.from_id, a.into_id))
                }
                Ok(false) => ToolResult::data(
                    format!(
                        "No merge performed: ids equal or not found (from_id={}, into_id={}).",
                        a.from_id, a.into_id
                    ),
                    json!({ "merged": false }),
                ),
                Err(e) => ToolResult::err(format!("failed to merge characters: {e}")),
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
                policy: a.policy,
                forbidden_thai: a.forbidden_thai,
                context_rule: a.context_rule,
                protected: a.protected,
                do_not_translate: a.do_not_translate,
                first_seen_chapter: a.first_seen_chapter.or(Some(chapter)),
            };
            match glossary::upsert_from_orchestrator(ws, term) {
                Ok(glossary::GlossaryUpsertOutcome::Inserted)
                | Ok(glossary::GlossaryUpsertOutcome::Updated) => {
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
                Ok(glossary::GlossaryUpsertOutcome::Protected { existing, conflict }) => {
                    let policy = glossary::effective_policy(&existing);
                    let summary = format!(
                        "controlled term {} kept by {} policy",
                        existing.jp_term,
                        glossary::policy_label(policy)
                    );
                    tx.send(AppEvent::ToolInvoked {
                        chapter,
                        tool: name.to_string(),
                        summary: summary.clone(),
                    });
                    ToolResult::data(
                        if conflict {
                            format!(
                                "Term {} is controlled by '{}' policy; no automatic update was applied. Flag a term conflict if this chunk needs a different rendering.",
                                existing.jp_term,
                                glossary::policy_label(policy)
                            )
                        } else {
                            format!(
                                "Term {} is controlled by '{}' policy; no automatic update was applied.",
                                existing.jp_term,
                                glossary::policy_label(policy)
                            )
                        },
                        json!({
                            "protected": true,
                            "policy": policy,
                            "conflict": conflict,
                            "existing": existing,
                        }),
                    )
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
            let protected_only = a.protected_only.unwrap_or(false);
            let terms = glossary::get(
                ws,
                a.query.as_deref(),
                a.category.as_deref(),
                a.policy,
                protected_only,
                limit,
            );
            tx.send(AppEvent::ToolInvoked {
                chapter,
                tool: name.to_string(),
                summary: if protected_only {
                    format!("read {} protected glossary term(s)", terms.len())
                } else {
                    format!("read {} glossary term(s)", terms.len())
                },
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

/// A `ToolExecutor` bound to one workspace/channel/chapter. `Workspace` isn't
/// `Clone`, so we store `root` + `vol_number` and rebuild one per call.
pub struct WorkspaceTools {
    root: PathBuf,
    vol_number: u32,
    tx: EventTx,
    chapter: u32,
}

impl WorkspaceTools {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_ws(tag: &str) -> (std::path::PathBuf, Workspace) {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "honya_tools_{tag}_{}_{}",
            std::process::id(),
            stamp
        ));
        std::fs::create_dir_all(&base).unwrap();
        let ws = Workspace::new(base.clone(), 1);
        (base, ws)
    }

    fn term(jp: &str, th: &str, protected: Option<bool>) -> GlossaryTerm {
        GlossaryTerm {
            jp_term: jp.to_string(),
            thai_term: th.to_string(),
            romaji: None,
            category: Some("item".to_string()),
            gloss: None,
            policy: protected.map(|is_protected| {
                if is_protected {
                    TermPolicy::HardLocked
                } else {
                    TermPolicy::Preferred
                }
            }),
            forbidden_thai: Vec::new(),
            context_rule: None,
            protected,
            do_not_translate: Some(false),
            first_seen_chapter: Some(1),
        }
    }

    #[tokio::test]
    async fn orchestrator_upsert_respects_protected_glossary_term() {
        let (base, ws) = temp_ws("protected_upsert");
        glossary::upsert(&ws, term("聖剣", "ดาบศักดิ์สิทธิ์", Some(true))).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let result = dispatch_tool(
            &ws,
            &EventTx(tx),
            3,
            "upsert_glossary_term",
            r#"{"jp_term":"聖剣","thai_term":"ดาบเทพ","do_not_translate":true}"#,
        )
        .await;

        assert!(
            result.ok,
            "protected skip should be a recoverable tool result"
        );
        assert!(result.message.contains("controlled"));
        assert_eq!(
            result
                .data
                .as_ref()
                .and_then(|d| d.get("conflict"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );

        let terms = glossary::load(&ws);
        let saved = terms.iter().find(|t| t.jp_term == "聖剣").unwrap();
        assert_eq!(saved.thai_term, "ดาบศักดิ์สิทธิ์");
        assert_eq!(saved.do_not_translate, Some(false));
        assert_eq!(saved.protected, Some(true));
        assert_eq!(glossary::effective_policy(saved), TermPolicy::HardLocked);

        let mut saw_protected_log = false;
        let mut saw_upsert_event = false;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AppEvent::ToolInvoked { summary, .. } => {
                    saw_protected_log |= summary.contains("controlled term");
                }
                AppEvent::GlossaryUpserted { .. } => saw_upsert_event = true,
                _ => {}
            }
        }
        assert!(saw_protected_log);
        assert!(
            !saw_upsert_event,
            "blocked controlled terms must not emit upsert UI events"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn get_glossary_filters_protected_only() {
        let (base, ws) = temp_ws("protected_filter");
        glossary::upsert(&ws, term("聖剣", "ดาบศักดิ์สิทธิ์", Some(true))).unwrap();
        glossary::upsert(&ws, term("王都", "ราชธานี", None)).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let result = dispatch_tool(
            &ws,
            &EventTx(tx),
            3,
            "get_glossary",
            r#"{"protected_only":true,"limit":10}"#,
        )
        .await;

        assert!(result.ok);
        let terms = result
            .data
            .as_ref()
            .and_then(|d| d.get("terms"))
            .and_then(|v| v.as_array())
            .expect("terms array");
        assert_eq!(terms.len(), 1);
        assert_eq!(
            terms[0].get("jp_term").and_then(|v| v.as_str()),
            Some("聖剣")
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    fn character(id: &str, jp: &str, thai: &str, romaji: Option<&str>) -> Character {
        Character {
            id: id.into(),
            jp_name: jp.into(),
            thai_name: thai.into(),
            romaji: romaji.map(|s| s.to_string()),
            gender: None,
            honorific: None,
            speech_style: None,
            relationships: Vec::new(),
            aliases: Vec::new(),
            notes: None,
            first_seen_chapter: None,
        }
    }

    #[tokio::test]
    async fn upsert_character_with_aliases_persists() {
        let (base, ws) = temp_ws("char_aliases");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let result = dispatch_tool(
            &ws,
            &EventTx(tx),
            3,
            "upsert_character",
            r#"{"id":"yuu","jp_name":"有月勇","thai_name":"อาริทสึกิ ยู","aliases":["勇"]}"#,
        )
        .await;
        assert!(result.ok, "{}", result.message);

        let chars = characters::load(&ws);
        let yuu = chars.iter().find(|c| c.id == "yuu").expect("saved");
        assert!(yuu.aliases.iter().any(|a| a == "勇"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn upsert_character_suggests_merge_candidate() {
        let (base, ws) = temp_ws("char_suggest");
        characters::upsert(&ws, character("miya", "みや", "มิยะ", Some("Miya"))).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        // Same reading, different kanji → suggest-only, must not auto-merge.
        let result = dispatch_tool(
            &ws,
            &EventTx(tx),
            3,
            "upsert_character",
            r#"{"id":"miya2","jp_name":"未夜","thai_name":"มิยะ","romaji":"Miya"}"#,
        )
        .await;
        assert!(result.ok);

        assert_eq!(characters::load(&ws).len(), 2, "homophone kept separate");
        let cands = result
            .data
            .as_ref()
            .and_then(|d| d.get("merge_candidates"))
            .and_then(|v| v.as_array())
            .expect("merge_candidates reported");
        assert!(cands.iter().any(|v| v.as_str() == Some("miya")));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn merge_character_tool_repoints_and_logs() {
        let (base, ws) = temp_ws("char_merge");
        characters::upsert(
            &ws,
            character("yuu", "有月勇", "อาริทสึกิ ยู", Some("Aritsuki Yuu")),
        )
        .unwrap();
        // Bare stub with no romaji → does not auto-merge; consolidate via the tool.
        characters::upsert(&ws, character("yuu-bare", "勇", "ยู", None)).unwrap();
        let mut miya = character("miya", "みや", "มิยะ", Some("Miya"));
        miya.relationships = vec![Relationship {
            target_id: "yuu-bare".into(),
            relation: "neighbor".into(),
        }];
        characters::upsert(&ws, miya).unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let result = dispatch_tool(
            &ws,
            &EventTx(tx),
            5,
            "merge_character",
            r#"{"from_id":"yuu-bare","into_id":"yuu"}"#,
        )
        .await;
        assert!(result.ok, "{}", result.message);

        let chars = characters::load(&ws);
        assert!(chars.iter().all(|c| c.id != "yuu-bare"), "from removed");
        let miya = chars.iter().find(|c| c.id == "miya").unwrap();
        assert_eq!(miya.relationships[0].target_id, "yuu", "ref repointed");

        let mut saw_tool = false;
        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::ToolInvoked { tool, .. } = ev {
                saw_tool |= tool == "merge_character";
            }
        }
        assert!(saw_tool, "merge emits a ToolInvoked log");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn get_character_by_alias() {
        let (base, ws) = temp_ws("char_get_alias");
        let mut yuu = character("yuu", "有月勇", "อาริทสึกิ ยู", Some("Aritsuki Yuu"));
        yuu.aliases = vec!["勇".into()];
        characters::upsert(&ws, yuu).unwrap();

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let result =
            dispatch_tool(&ws, &EventTx(tx), 3, "get_character", r#"{"query":"勇"}"#).await;
        assert!(result.ok);
        let arr = result
            .data
            .as_ref()
            .and_then(|d| d.get("characters"))
            .and_then(|v| v.as_array())
            .expect("characters array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].get("id").and_then(|v| v.as_str()), Some("yuu"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
