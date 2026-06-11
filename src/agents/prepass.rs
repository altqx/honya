//! One-time per-volume pre-extraction pass.
//!
//! Before chunk 1 of a volume is translated, sample its raw Japanese chapters and
//! ask the Translator model to extract the cast, recurring terminology, and a few
//! style exemplars — then seed CHARACTERS.md / GLOSSARY.md / VOLUME.md. This lifts
//! early chapters out of the "sparse context" hole (the roster is otherwise built
//! up incrementally as translation proceeds, so chapter 1 is translated nearly
//! blind). Idempotency is the caller's job via `VolumeData.prepass_done`.

use serde::Deserialize;

use crate::agents::prompts::PREPASS_SYSTEM;
use crate::llm::client::{LlmClient, Result};
use crate::llm::structured::{chat_structured, prepass_schema};
use crate::llm::{ChatRequest, Message, Usage};
use crate::model::{Character, GlossaryTerm, StyleExample};
use crate::workspace::{Workspace, characters, glossary, volume};

/// Total raw-source characters fed to the extractor (sampled across chapters).
const SAMPLE_BUDGET_CHARS: usize = 48_000;
/// Per-chapter head sample bounds, so a many-chapter volume still spreads coverage.
const PER_CHAPTER_MIN: usize = 1_500;
const PER_CHAPTER_MAX: usize = 8_000;

#[derive(Debug, Clone, Deserialize)]
pub struct PrepassOut {
    #[serde(default)]
    pub characters: Vec<PrepassCharacter>,
    #[serde(default)]
    pub terms: Vec<PrepassTerm>,
    #[serde(default)]
    pub style_examples: Vec<PrepassExample>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PrepassCharacter {
    pub jp_name: String,
    pub thai_name: String,
    #[serde(default)]
    pub romaji: String,
    #[serde(default)]
    pub gender: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub honorific: String,
    #[serde(default)]
    pub speech_style: String,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PrepassTerm {
    pub jp_term: String,
    pub thai_term: String,
    #[serde(default)]
    pub romaji: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub gloss: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PrepassExample {
    pub jp: String,
    pub th: String,
    #[serde(default)]
    pub note: String,
}

/// What a pass seeded, for the caller's log line + usage accounting.
pub struct PrepassSeeded {
    pub characters: usize,
    pub terms: usize,
    pub examples: usize,
    pub usage: Usage,
}

/// Run the extraction over `ws`'s sampled raw chapters and seed the project files.
/// Returns `Ok(None)` when there is no raw source to sample (nothing to do).
pub async fn run_prepass(
    client: &dyn LlmClient,
    model: &str,
    ws: &Workspace,
) -> Result<Option<PrepassSeeded>> {
    let sample = sample_volume_raw(ws);
    if sample.trim().is_empty() {
        return Ok(None);
    }

    let user = format!(
        "<<VOLUME_RAW_SAMPLE: ตัวอย่างเนื้อหาต้นฉบับภาษาญี่ปุ่นจากหลายบทของเล่มนี้ ใช้สกัดข้อมูลอ้างอิงก่อนเริ่มแปล>>\n{sample}\n<<END_VOLUME_RAW_SAMPLE>>"
    );
    let req = ChatRequest {
        model: model.to_string(),
        messages: vec![Message::system(PREPASS_SYSTEM), Message::user(user)],
        temperature: Some(0.2),
        ..ChatRequest::default()
    };

    let (out, usage) =
        chat_structured::<PrepassOut>(client, req, "prepass_result", prepass_schema(), 1).await?;

    let mut characters_added = 0usize;
    for c in &out.characters {
        if c.jp_name.trim().is_empty() {
            continue;
        }
        let character = Character {
            id: String::new(),
            jp_name: c.jp_name.trim().to_string(),
            thai_name: c.thai_name.trim().to_string(),
            romaji: non_empty(&c.romaji),
            gender: non_empty(&c.gender),
            honorific: non_empty(&c.honorific),
            speech_style: non_empty(&c.speech_style),
            relationships: Vec::new(),
            aliases: c
                .aliases
                .iter()
                .map(|a| a.trim().to_string())
                .filter(|a| !a.is_empty())
                .collect(),
            notes: non_empty(&c.notes),
            first_seen_chapter: None,
        };
        // Best-effort: a single bad row must not sink the whole seed.
        if characters::upsert(ws, character).is_ok() {
            characters_added += 1;
        }
    }

    let mut terms_added = 0usize;
    for t in &out.terms {
        if t.jp_term.trim().is_empty() {
            continue;
        }
        // Seeded unprotected (effective policy = preferred) so the Orchestrator can
        // still refine the rendering as the real translation proceeds.
        let term = GlossaryTerm {
            jp_term: t.jp_term.trim().to_string(),
            thai_term: t.thai_term.trim().to_string(),
            romaji: non_empty(&t.romaji),
            category: non_empty(&t.category),
            gloss: non_empty(&t.gloss),
            policy: None,
            forbidden_thai: Vec::new(),
            context_rule: None,
            protected: None,
            do_not_translate: None,
            first_seen_chapter: None,
        };
        if glossary::upsert(ws, term).is_ok() {
            terms_added += 1;
        }
    }

    let examples: Vec<StyleExample> = out
        .style_examples
        .iter()
        .filter(|e| !e.jp.trim().is_empty() && !e.th.trim().is_empty())
        .map(|e| StyleExample {
            jp: e.jp.trim().to_string(),
            th: e.th.trim().to_string(),
            note: non_empty(&e.note),
        })
        .collect();
    let examples_added = examples.len();
    if !examples.is_empty() {
        let _ = volume::add_style_examples(ws, examples);
    }

    Ok(Some(PrepassSeeded {
        characters: characters_added,
        terms: terms_added,
        examples: examples_added,
        usage,
    }))
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Concatenate a head sample from each raw chapter (in numeric order) under a global
/// budget, so names/terms introduced later in the volume still get represented.
fn sample_volume_raw(ws: &Workspace) -> String {
    let mut chapters = raw_chapter_numbers(ws);
    chapters.sort_unstable();
    if chapters.is_empty() {
        return String::new();
    }

    let per_chapter =
        (SAMPLE_BUDGET_CHARS / chapters.len()).clamp(PER_CHAPTER_MIN, PER_CHAPTER_MAX);
    let mut out = String::new();
    let mut used = 0usize;
    for ch in chapters {
        if used >= SAMPLE_BUDGET_CHARS {
            break;
        }
        let Ok(raw) = std::fs::read_to_string(ws.raw(ch)) else {
            continue;
        };
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let take = per_chapter.min(SAMPLE_BUDGET_CHARS - used);
        let head: String = raw.chars().take(take).collect();
        used += head.chars().count();
        out.push_str(&format!("\n## บท {ch}\n{head}\n"));
    }
    out
}

/// Chapter numbers with a `raw/ch_NNN.md` source on disk.
fn raw_chapter_numbers(ws: &Workspace) -> Vec<u32> {
    let raw_dir = ws.vol_dir.join("raw");
    let Ok(entries) = std::fs::read_dir(&raw_dir) else {
        return Vec::new();
    };
    entries
        .filter_map(std::result::Result::ok)
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_str()?;
            let digits = name.strip_prefix("ch_")?.strip_suffix(".md")?;
            digits.trim_start_matches('0').parse::<u32>().ok().or({
                // "ch_000.md" trims to empty → chapter 0.
                if digits.chars().all(|c| c == '0') {
                    Some(0)
                } else {
                    None
                }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::client::Result as LlmResult;
    use crate::llm::{ChatRequest, ChatResponse, Choice, ResponseFormat, ResponseMessage, Usage};
    use async_trait::async_trait;

    /// Returns one character / term / exemplar for a `prepass_result` request.
    struct SeedingClient;

    #[async_trait]
    impl LlmClient for SeedingClient {
        async fn chat(&self, req: &ChatRequest) -> LlmResult<ChatResponse> {
            let schema = match &req.response_format {
                Some(ResponseFormat::JsonSchema { json_schema }) => json_schema.name.as_str(),
                _ => "",
            };
            let content = if schema == "prepass_result" {
                serde_json::json!({
                    "characters": [{
                        "jp_name": "有月勇", "thai_name": "อาริทสึกิ ยู", "romaji": "Aritsuki Yuu",
                        "gender": "male", "aliases": ["勇"], "honorific": "", "speech_style": "ห้วน",
                        "notes": ""
                    }],
                    "terms": [{
                        "jp_term": "聖剣", "thai_term": "ดาบศักดิ์สิทธิ์", "romaji": "Seiken",
                        "category": "item", "gloss": "canonical weapon"
                    }],
                    "style_examples": [{
                        "jp": "彼は笑った。", "th": "เขาหัวเราะออกมา", "note": "สบาย ๆ"
                    }]
                })
                .to_string()
            } else {
                "{}".to_string()
            };
            Ok(ChatResponse {
                id: None,
                model: None,
                usage: Some(Usage {
                    prompt_tokens: 5,
                    completion_tokens: 5,
                    total_tokens: 10,
                    ..Usage::default()
                }),
                service_tier: None,
                choices: vec![Choice {
                    index: 0,
                    message: ResponseMessage {
                        role: Some("assistant".into()),
                        content: Some(content),
                        tool_calls: None,
                    },
                    finish_reason: Some("stop".into()),
                }],
            })
        }
    }

    #[tokio::test]
    async fn prepass_seeds_characters_terms_and_examples() {
        let base = std::env::temp_dir().join(format!("honya_prepass_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let ws = Workspace::new(base.clone(), 1);
        crate::workspace::translation::write_raw(&ws, 1, "有月勇は聖剣を抜いた。彼は笑った。")
            .unwrap();

        let seeded = run_prepass(&SeedingClient, "mock", &ws)
            .await
            .expect("run_prepass ok")
            .expect("had raw to sample");
        assert_eq!(seeded.characters, 1);
        assert_eq!(seeded.terms, 1);
        assert_eq!(seeded.examples, 1);

        let chars = characters::load(&ws);
        assert!(chars.iter().any(|c| c.jp_name == "有月勇"));
        let terms = glossary::load(&ws);
        assert!(terms.iter().any(|t| t.jp_term == "聖剣"));
        // Seeded unprotected so the Orchestrator can still refine it.
        assert!(!terms.iter().any(glossary::blocks_automatic_update));
        assert_eq!(volume::load(&ws).style_examples.len(), 1);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn prepass_returns_none_without_raw() {
        let base = std::env::temp_dir().join(format!("honya_prepass_empty_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let ws = Workspace::new(base.clone(), 1);
        let out = run_prepass(&SeedingClient, "mock", &ws).await.unwrap();
        assert!(out.is_none(), "no raw chapters → nothing to do");
        let _ = std::fs::remove_dir_all(&base);
    }
}
