//! src/llm/tool_loop.rs — the generic tool-call driver for the Orchestrator.
//!
//! [`run_tool_loop`] runs the OpenAI/OpenRouter tool-use loop:
//!   1. send the request,
//!   2. if the assistant replied with `tool_calls`, echo that assistant message
//!      back into the conversation, execute each call via the [`ToolExecutor`],
//!      and append a `role: "tool"` message keyed by `tool_call_id`,
//!   3. repeat until the model finishes without tool calls,
//!   4. abort with [`LlmError::Api`]`{status:0,..}` if `max_rounds` is exceeded.
//!
//! The executor is async + dyn-compatible (via `async_trait`) and returns the
//! tool result as a JSON string (the wire `content` of the tool message).

use async_trait::async_trait;

use super::client::{LlmClient, LlmError, Result};
use super::{ChatRequest, ChatResponse, Message, Role, ToolCall};

/// Backend that actually runs a named tool with its JSON-string arguments.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Execute `name` with `arguments_json` (the raw JSON-string from the model)
    /// and return the result payload as a JSON string for the tool message.
    async fn execute(&self, name: &str, arguments_json: &str) -> anyhow::Result<String>;
}

/// Drive the tool-call loop to completion.
///
/// Returns the final [`ChatResponse`] (the one with no tool calls). The passed
/// `req` is mutated in-place as the conversation grows; ownership is taken so
/// callers don't accidentally reuse a stale request.
pub async fn run_tool_loop(
    client: &dyn LlmClient,
    mut req: ChatRequest,
    executor: &dyn ToolExecutor,
    max_rounds: usize,
) -> Result<ChatResponse> {
    for _round in 0..max_rounds {
        let resp = client.chat(&req).await?;

        let choice = resp.choices.first().ok_or(LlmError::EmptyChoices)?;
        let tool_calls: Vec<ToolCall> = choice.message.tool_calls.clone().unwrap_or_default();

        // No tool calls → the model is done; return the final response.
        if tool_calls.is_empty() {
            return Ok(resp);
        }

        // Echo the assistant tool-call message back into the conversation.
        // content is null on a tool-call turn (serialized as null, not skipped).
        req.messages.push(Message {
            role: Role::Assistant,
            content: choice.message.content.clone(),
            tool_calls: Some(tool_calls.clone()),
            tool_call_id: None,
            name: None,
        });

        // Execute each tool call and push its result keyed by tool_call_id.
        for call in &tool_calls {
            let result = match executor
                .execute(&call.function.name, &call.function.arguments)
                .await
            {
                Ok(payload) => payload,
                // Surface executor errors to the model as a tool result so it
                // can react, rather than aborting the whole loop.
                Err(e) => serde_json::json!({
                    "ok": false,
                    "message": format!("tool '{}' failed: {e}", call.function.name)
                })
                .to_string(),
            };

            req.messages
                .push(Message::tool_result(call.id.clone(), result));
        }
    }

    Err(LlmError::Api {
        status: 0,
        message: format!("tool loop exceeded {max_rounds} rounds without finishing"),
    })
}
