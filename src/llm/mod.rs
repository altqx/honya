//! OpenRouter / OpenAI-compatible chat wire-format structs.
//!
//! Two serde subtleties are load-bearing: `Message.content` serializes as JSON
//! `null` (not skipped) on a tool-call turn per the OpenAI/OpenRouter contract;
//! `FunctionCall.arguments` is a JSON *string* decoded again by `parse_args`.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub mod client;
/// Test-only canned-response client for the offline e2e suite (not in the binary).
#[cfg(test)]
pub mod mock;
pub mod structured;
pub mod tool_loop;

pub use client::LlmError;

/// Body of a POST to `{base_url}/chat/completions`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// `flex`/`priority` request tier. Normally left `None` here and stamped from
    /// [`ClientConfig`](client::ClientConfig) at send time so it applies uniformly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<crate::model::ServiceTier>,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            messages,
            tools: None,
            tool_choice: None,
            response_format: None,
            temperature: None,
            max_tokens: None,
            stream: None,
            service_tier: None,
        }
    }
}

/// One chat message. Role-dependent fields are all optional on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    /// Serialized as `null` (not skipped) so a tool-call turn keeps the key.
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Set on a `role: "tool"` message to bind the result to its call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// A `tool` result message keyed back to its originating call.
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
            name: None,
        }
    }
}

/// Chat roles. Serialized lowercase (`system`/`user`/`assistant`/`tool`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A tool the model may call — currently only `type: "function"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionDef,
}

impl Tool {
    pub fn function(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            kind: "function".to_string(),
            function: FunctionDef {
                name: name.into(),
                description: Some(description.into()),
                parameters,
            },
        }
    }
}

/// The schema half of a function tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDef {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema for the function arguments.
    pub parameters: serde_json::Value,
}

/// `response_format` discriminated on `type`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseFormat {
    /// `{"type":"json_object"}` — free-form JSON.
    JsonObject,
    /// `{"type":"json_schema","json_schema":{...}}` — strict schema.
    JsonSchema { json_schema: JsonSchemaSpec },
}

/// The `json_schema` payload of a strict structured-output request.
#[derive(Debug, Clone, Serialize)]
pub struct JsonSchemaSpec {
    pub name: String,
    pub strict: bool,
    pub schema: serde_json::Value,
}

/// Body of a `/chat/completions` response.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatResponse {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    pub choices: Vec<Choice>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

/// One completion choice.
#[derive(Debug, Clone, Deserialize)]
pub struct Choice {
    #[serde(default)]
    pub index: u32,
    pub message: ResponseMessage,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

/// The assistant message inside a choice.
#[derive(Debug, Clone, Deserialize)]
pub struct ResponseMessage {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
}

/// A single tool call requested by the assistant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "default_tool_call_type")]
    pub kind: String,
    pub function: FunctionCall,
}

fn default_tool_call_type() -> String {
    "function".to_string()
}

/// The function-call payload of a [`ToolCall`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// The arguments encoded as a JSON **string** (decode with [`Self::parse_args`]).
    pub arguments: String,
}

impl FunctionCall {
    /// Decode `arguments` without using the crate's `Result<T>` alias.
    pub fn parse_args<T: DeserializeOwned>(&self) -> std::result::Result<T, serde_json::Error> {
        serde_json::from_str(&self.arguments)
    }
}

/// Token + cost accounting reported by the API.
///
/// OpenRouter now returns cost inline on every response (the old
/// `usage:{include:true}` toggle is implied), so we always decode it. The mock
/// client and plain OpenAI-compatible endpoints omit the cost fields, which then
/// default to 0.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
    /// USD deducted from the OpenRouter account for this request. On a **BYOK**
    /// request this is only OpenRouter's surcharge — the provider's own charge
    /// (billed to your provider key) lands in `cost_details.upstream_inference_cost`.
    #[serde(default, deserialize_with = "de_f64_lenient")]
    pub cost: f64,
    #[serde(default)]
    pub cost_details: Option<CostDetails>,
}

/// The `usage.cost_details` breakdown OpenRouter returns alongside `cost`.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct CostDetails {
    /// USD billed directly by the upstream provider — present (non-null) only on
    /// BYOK requests; OpenRouter sends `null` here for its own-key requests.
    #[serde(default, deserialize_with = "de_f64_lenient")]
    pub upstream_inference_cost: f64,
}

impl Usage {
    /// Total USD this request actually costs the user: the OpenRouter charge plus,
    /// for BYOK requests, the upstream provider's own charge. With your own Google
    /// key, `cost` is just OpenRouter's fee and the real spend is the upstream
    /// cost, so both must be summed — using `cost` alone would undercount badly.
    pub fn cost_usd(&self) -> f64 {
        let upstream = self
            .cost_details
            .map(|d| d.upstream_inference_cost)
            .unwrap_or(0.0);
        self.cost + upstream
    }

    /// Fold another usage record into this running total (saturating token adds,
    /// summed USD). Keeps `cost_usd()` correct across the multiple API calls a
    /// single logical step can make (e.g. the Orchestrator's multi-round tool loop).
    pub fn add(&mut self, other: &Usage) {
        self.prompt_tokens = self.prompt_tokens.saturating_add(other.prompt_tokens);
        self.completion_tokens = self
            .completion_tokens
            .saturating_add(other.completion_tokens);
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
        self.cost += other.cost;
        let upstream = self.cost_details.map_or(0.0, |d| d.upstream_inference_cost)
            + other
                .cost_details
                .map_or(0.0, |d| d.upstream_inference_cost);
        if upstream != 0.0 {
            self.cost_details = Some(CostDetails {
                upstream_inference_cost: upstream,
            });
        }
    }
}

/// Deserialize an `f64` that may arrive as JSON `null` (→ 0.0). OpenRouter sends
/// `upstream_inference_cost: null` on non-BYOK requests, which a plain `f64` would
/// reject and so fail the entire response parse.
fn de_f64_lenient<'de, D>(deserializer: D) -> std::result::Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<f64>::deserialize(deserializer)?.unwrap_or(0.0))
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn byok_cost_sums_openrouter_fee_and_upstream() {
        // BYOK response shape: `cost` is OpenRouter's surcharge, the real provider
        // charge is in cost_details.upstream_inference_cost — both must be summed.
        let json = r#"{
            "prompt_tokens": 194,
            "completion_tokens": 2,
            "total_tokens": 196,
            "cost": 0.95,
            "cost_details": { "upstream_inference_cost": 19 }
        }"#;
        let u: Usage = serde_json::from_str(json).unwrap();
        assert_eq!(u.total_tokens, 196);
        assert!(approx(u.cost, 0.95));
        assert!(approx(u.cost_usd(), 19.95));
    }

    #[test]
    fn null_upstream_cost_is_tolerated() {
        // Non-BYOK responses send upstream_inference_cost: null.
        let json = r#"{
            "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15,
            "cost": 0.0001,
            "cost_details": { "upstream_inference_cost": null }
        }"#;
        let u: Usage = serde_json::from_str(json).unwrap();
        assert!(approx(u.cost_usd(), 0.0001));
    }

    #[test]
    fn absent_cost_defaults_to_zero() {
        // Mock / OpenAI-compatible endpoints omit cost entirely.
        let json = r#"{ "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }"#;
        let u: Usage = serde_json::from_str(json).unwrap();
        assert!(approx(u.cost_usd(), 0.0));
    }

    #[test]
    fn add_accumulates_tokens_and_cost() {
        let mut acc = Usage::default();
        acc.add(&Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            cost: 0.5,
            cost_details: Some(CostDetails {
                upstream_inference_cost: 2.0,
            }),
        });
        acc.add(&Usage {
            prompt_tokens: 4,
            completion_tokens: 6,
            total_tokens: 10,
            cost: 0.25,
            cost_details: Some(CostDetails {
                upstream_inference_cost: 3.0,
            }),
        });
        assert_eq!(acc.total_tokens, 25);
        assert!(approx(acc.cost, 0.75));
        // 0.75 OpenRouter fees + 5.0 upstream.
        assert!(approx(acc.cost_usd(), 5.75));
    }
}

#[cfg(test)]
mod service_tier_tests {
    use super::*;
    use crate::model::ServiceTier;

    #[test]
    fn service_tier_omitted_when_unset() {
        let json = serde_json::to_value(ChatRequest::new("m", vec![])).unwrap();
        assert!(json.get("service_tier").is_none());
    }

    #[test]
    fn service_tier_serializes_as_lowercase_string() {
        let req = ChatRequest {
            service_tier: Some(ServiceTier::Priority),
            ..ChatRequest::new("m", vec![])
        };
        let json = serde_json::to_value(req).unwrap();
        assert_eq!(json["service_tier"], "priority");
    }
}
