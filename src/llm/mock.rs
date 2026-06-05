//! src/llm/mock.rs — an offline `LlmClient` so honya runs with no API key.
//!
//! The mock keys off the requested `response_format`'s `json_schema.name` and
//! returns schema-shaped JSON that deserializes cleanly into `TranslatorOut` /
//! `ReviewerOut` (the FINAL shapes in `agent_prompts.md`):
//!   * `translation_result` → a `TranslatorOut` with the canned Thai text and
//!     empty discovery arrays.
//!   * `review_result`      → `{"status":"approve","feedback":[]}` (always
//!     approves, so the pipeline commits the canned text and moves on).
//!   * anything else (e.g. an Orchestrator tools turn with no `response_format`)
//!     → a plain assistant `stop` with no tool calls, so the tool loop ends.

use async_trait::async_trait;

use super::client::{LlmClient, Result};
use super::{
    ChatRequest, ChatResponse, Choice, ResponseFormat, ResponseMessage, Usage,
};

/// Offline canned-response client.
pub struct MockClient {
    /// The Thai text returned as `translated_text` for every translation turn.
    pub canned_thai: String,
}

impl Default for MockClient {
    fn default() -> Self {
        Self {
            canned_thai: "(ข้อความแปลจำลอง — ไม่มีคีย์ API จึงใช้ไคลเอนต์จำลอง)".to_string(),
        }
    }
}

impl MockClient {
    /// Build a mock that returns a specific Thai string.
    pub fn new(canned_thai: impl Into<String>) -> Self {
        Self { canned_thai: canned_thai.into() }
    }

    /// Wrap a content string in a one-choice `stop` response.
    fn stop_with(content: String) -> ChatResponse {
        ChatResponse {
            id: Some("mock-completion".to_string()),
            model: Some("honya/mock".to_string()),
            choices: vec![Choice {
                index: 0,
                message: ResponseMessage {
                    role: Some("assistant".to_string()),
                    content: Some(content),
                    tool_calls: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: Some(Usage::default()),
        }
    }
}

#[async_trait]
impl LlmClient for MockClient {
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
        // Inspect the strict-schema name when present.
        let schema_name = match &req.response_format {
            Some(ResponseFormat::JsonSchema { json_schema }) => Some(json_schema.name.as_str()),
            _ => None,
        };

        let content = match schema_name {
            Some("translation_result") => {
                // Shape matches model::TranslatorOut exactly.
                let payload = serde_json::json!({
                    "thought_process": {
                        "scene_analysis": "(mock)",
                        "glossary_check": "(mock)"
                    },
                    "translated_text": self.canned_thai,
                    "new_characters": [],
                    "new_terms": [],
                    "continuity_notes": []
                });
                payload.to_string()
            }
            Some("review_result") => {
                // Shape matches model::ReviewerOut exactly; always approves.
                let payload = serde_json::json!({
                    "status": "approve",
                    "feedback": []
                });
                payload.to_string()
            }
            // Orchestrator tools turn (or any other call): finish cleanly with
            // no tool calls so the tool loop terminates after one round.
            _ => "(mock orchestrator: nothing to record)".to_string(),
        };

        Ok(Self::stop_with(content))
    }
}
