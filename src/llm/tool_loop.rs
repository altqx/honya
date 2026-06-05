//! The generic tool-call driver for the Orchestrator.

use async_trait::async_trait;

use super::client::{LlmClient, LlmError, Result};
use super::{ChatRequest, ChatResponse, Message, Role, ToolCall, Usage};

/// Backend that actually runs a named tool with its JSON-string arguments.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Run `name` with the model's raw JSON-string args; return a JSON-string result.
    async fn execute(&self, name: &str, arguments_json: &str) -> anyhow::Result<String>;
}

/// Drive the tool-call loop to completion, returning the final tool-call-free
/// response paired with the token/cost usage summed across **every** round (each
/// round is its own API call, so the caller would otherwise miss all but the last).
///
/// Takes ownership of `req` (mutated as the conversation grows) to prevent reuse of a stale request.
pub async fn run_tool_loop(
    client: &dyn LlmClient,
    mut req: ChatRequest,
    executor: &dyn ToolExecutor,
    max_rounds: usize,
) -> Result<(ChatResponse, Usage)> {
    let mut usage = Usage::default();
    for _round in 0..max_rounds {
        let resp = client.chat(&req).await?;
        if let Some(u) = &resp.usage {
            usage.add(u);
        }

        let choice = resp.choices.first().ok_or(LlmError::EmptyChoices)?;
        let tool_calls: Vec<ToolCall> = choice.message.tool_calls.clone().unwrap_or_default();

        if tool_calls.is_empty() {
            return Ok((resp, usage));
        }

        // content is null on a tool-call turn (serialized as null, not skipped).
        req.messages.push(Message {
            role: Role::Assistant,
            content: choice.message.content.clone(),
            tool_calls: Some(tool_calls.clone()),
            tool_call_id: None,
            name: None,
        });

        for call in &tool_calls {
            let result = match executor
                .execute(&call.function.name, &call.function.arguments)
                .await
            {
                Ok(payload) => payload,
                // Surface executor errors to the model as a tool result so it can react, not abort.
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
