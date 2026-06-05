//! Strict `json_schema` chat with fence-stripping and retry-on-parse-failure.

use serde::de::DeserializeOwned;

use super::client::{LlmClient, LlmError, Result};
use super::{ChatRequest, JsonSchemaSpec, ResponseFormat, Usage};

/// Run a strict structured-output chat and deserialize the assistant content.
///
/// `retries` is extra attempts on parse failure, so total attempts = 1 + retries.
pub async fn chat_structured<T: DeserializeOwned>(
    client: &dyn LlmClient,
    mut req: ChatRequest,
    schema_name: &'static str,
    schema: serde_json::Value,
    retries: usize,
) -> Result<(T, Usage)> {
    // Structured output and tool_choice are mutually exclusive, so clear tool_choice.
    req.response_format = Some(ResponseFormat::JsonSchema {
        json_schema: JsonSchemaSpec {
            name: schema_name.to_string(),
            strict: true,
            schema,
        },
    });
    req.tool_choice = None;

    let mut last_err: Option<LlmError> = None;

    for _ in 0..=retries {
        let resp = client.chat(&req).await?;
        let usage = resp.usage.unwrap_or_default();
        let choice = resp.choices.first().ok_or(LlmError::EmptyChoices)?;
        let raw = choice.message.content.clone().unwrap_or_default();
        let cleaned = strip_fences(&raw);

        match serde_json::from_str::<T>(cleaned) {
            Ok(value) => return Ok((value, usage)),
            Err(source) => {
                last_err = Some(LlmError::Parse {
                    target: schema_name,
                    source,
                    raw,
                });
            }
        }
    }

    Err(last_err.unwrap_or(LlmError::EmptyChoices))
}

/// Strip a single ```json/```/~~~ fence wrapping JSON; returns a trimmed borrowed slice.
pub fn strip_fences(s: &str) -> &str {
    let trimmed = s.trim();

    let fence: Option<&str> = if trimmed.starts_with("```") {
        Some("```")
    } else if trimmed.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    };

    let Some(fence) = fence else {
        return trimmed;
    };

    // Drop the opening fence line, which may carry a language tag like `json`.
    let after_open = &trimmed[fence.len()..];
    let body_start = match after_open.find('\n') {
        Some(nl) => &after_open[nl + 1..],
        // Single-line fenced blob with no newline: just strip the marker.
        None => after_open,
    };

    let body = body_start.trim_end();
    let inner = body.strip_suffix(fence).unwrap_or(body);

    inner.trim()
}

/// Strict `translation_result` schema (verbatim from `agent_prompts.md`) → `model::TranslatorOut`.
pub fn translator_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["thought_process", "translated_text", "new_characters", "new_terms", "continuity_notes"],
        "properties": {
            "thought_process": {
                "type": "object",
                "additionalProperties": false,
                "required": ["scene_analysis", "glossary_check"],
                "properties": {
                    "scene_analysis": { "type": "string" },
                    "glossary_check": { "type": "string" }
                }
            },
            "translated_text": { "type": "string" },
            "new_characters": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["jp_name", "thai_name", "gender", "notes"],
                    "properties": {
                        "jp_name": { "type": "string" },
                        "thai_name": { "type": "string" },
                        "gender": { "type": "string" },
                        "notes": { "type": "string" }
                    }
                }
            },
            "new_terms": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["jp_term", "thai_term", "category", "gloss"],
                    "properties": {
                        "jp_term": { "type": "string" },
                        "thai_term": { "type": "string" },
                        "category": { "type": "string" },
                        "gloss": { "type": "string" }
                    }
                }
            },
            "continuity_notes": {
                "type": "array",
                "items": { "type": "string" }
            }
        }
    })
}

/// Strict `review_result` schema (verbatim from `agent_prompts.md`) → `model::ReviewerOut`.
pub fn reviewer_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["status", "feedback"],
        "properties": {
            "status": { "type": "string", "enum": ["approve", "reject"] },
            "feedback": {
                "type": "array",
                "items": { "type": "string" }
            }
        }
    })
}
