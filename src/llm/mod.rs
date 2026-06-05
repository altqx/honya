//! src/llm/mod.rs — OpenRouter / OpenAI-compatible chat wire-format structs.
//!
//! These mirror the `/chat/completions` request and response bodies exactly so
//! they (de)serialize straight onto the OpenRouter API. Nothing here depends on
//! the rest of honya: it is the bottom of the LLM stack. The higher layers
//! ([`client`], [`structured`], [`tool_loop`]) build on these types. A
//! canned-response `mock` client exists only under `cfg(test)` for the e2e
//! suite — honya itself has no offline backend.
//!
//! Two serde subtleties are load-bearing here:
//!  * `Message.content` is `Option<String>` but is serialized as JSON `null`
//!    (NOT skipped) on an assistant tool-call turn — OpenAI/OpenRouter require
//!    the key to be present. Every other optional field uses
//!    `skip_serializing_if = "Option::is_none"`.
//!  * `FunctionCall.arguments` is a JSON *string* (the model's tool arguments
//!    encoded as text), so [`FunctionCall::parse_args`] decodes it a second
//!    time. It returns a fully-qualified `std::result::Result` because the
//!    sibling [`client`] module defines a `Result<T>` alias that shadows std.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub mod client;
/// Test-only canned-response client for the offline e2e suite (not in the binary).
#[cfg(test)]
pub mod mock;
pub mod structured;
pub mod tool_loop;

// Convenience re-exports so other clusters can reach the core LLM surface via
// `crate::llm::*` (notably `error.rs` does `#[from] crate::llm::LlmError`).
pub use client::LlmError;

// ============================================================================
// REQUEST
// ============================================================================

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
}

impl ChatRequest {
    /// Construct a minimal request — `model` + `messages`, everything else None.
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
        }
    }
}

/// One chat message. Role-dependent fields are all optional on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    /// Serialized as `null` (not skipped) so the assistant tool-call turn keeps
    /// the key present, per the OpenAI/OpenRouter contract.
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Set on a `role: "tool"` message to bind the result to its call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Optional name (tool/function name on a tool result, or named author).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Message {
    /// A `system` instruction message.
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// A `user` message.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// An `assistant` text message (no tool calls).
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// A `tool` result message keyed back to the originating tool call.
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

// ============================================================================
// TOOLS (request side)
// ============================================================================

/// A tool the model may call — currently only `type: "function"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionDef,
}

impl Tool {
    /// Build a `function`-type tool from name/description/parameters schema.
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

// ============================================================================
// RESPONSE FORMAT (structured output)
// ============================================================================

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

// ============================================================================
// RESPONSE
// ============================================================================

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
    /// Decode the JSON-string `arguments` into a typed value.
    ///
    /// Returns a fully-qualified [`std::result::Result`]: the sibling `client`
    /// module's `Result<T>` alias shadows `std::result::Result` in this crate.
    pub fn parse_args<T: DeserializeOwned>(&self) -> std::result::Result<T, serde_json::Error> {
        serde_json::from_str(&self.arguments)
    }
}

/// Token accounting reported by the API.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
}
