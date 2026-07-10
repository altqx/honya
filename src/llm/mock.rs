//! Test-only canned-response `LlmClient` (cfg(test)) so the e2e suite runs without network or key.
//!
//! Keys off the request's `json_schema.name`: `translation_result` and
//! `review_result` return matching shapes; anything else returns a tool-call-free
//! `stop` so the tool loop ends.

use async_trait::async_trait;

use super::client::{LlmClient, Result};
use super::{
    ChatRequest, ChatResponse, Choice, CostDetails, ResponseFormat, ResponseMessage, Usage,
};

/// Offline canned-response client.
pub struct MockClient {
    /// The target-language text returned for every translation turn.
    pub canned_translation: String,
}

impl Default for MockClient {
    fn default() -> Self {
        Self {
            canned_translation: "(ข้อความแปลจำลอง — ไม่มีคีย์ API จึงใช้ไคลเอนต์จำลอง)".to_string(),
        }
    }
}

impl MockClient {
    #[allow(dead_code)]
    pub fn new(canned_translation: impl Into<String>) -> Self {
        Self {
            canned_translation: canned_translation.into(),
        }
    }

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
            // Nonzero, BYOK-shaped usage so the cost-accounting chain is exercised
            // end-to-end (OpenRouter fee in `cost`, provider charge in upstream).
            usage: Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                cost: 0.001,
                cost_details: Some(CostDetails {
                    upstream_inference_cost: 0.01,
                }),
                ..Usage::default()
            }),
            service_tier: None,
        }
    }
}

#[async_trait]
impl LlmClient for MockClient {
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let schema_name = match &req.response_format {
            Some(ResponseFormat::JsonSchema { json_schema }) => Some(json_schema.name.as_str()),
            _ => None,
        };

        let content = match schema_name {
            Some("translation_result") => {
                // Shape must match model::TranslatorOut.
                let payload = serde_json::json!({
                    "thought_process": {
                        "scene_analysis": "(mock)",
                        "glossary_check": "(mock)"
                    },
                    "translated_text": self.canned_translation,
                    "new_characters": [],
                    "new_terms": [],
                    "continuity_notes": []
                });
                payload.to_string()
            }
            Some("review_result") => {
                // Shape must match model::ReviewerOut; always approves.
                let payload = serde_json::json!({
                    "status": "approve",
                    "feedback": []
                });
                payload.to_string()
            }
            Some("prepass_result") => {
                // Shape must match agents::prepass::PrepassOut; seeds nothing.
                let payload = serde_json::json!({
                    "characters": [],
                    "terms": [],
                    "style_examples": []
                });
                payload.to_string()
            }
            Some("coherence_result") => {
                // Shape must match agents::coherence::CoherenceOut; finds nothing.
                let payload = serde_json::json!({ "issues": [] });
                payload.to_string()
            }
            // Any other call: no tool calls, so the tool loop terminates after one round.
            _ => "(mock orchestrator: nothing to record)".to_string(),
        };

        Ok(Self::stop_with(content))
    }
}
