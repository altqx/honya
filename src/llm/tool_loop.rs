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

/// Outcome of a completed tool loop: the final tool-call-free response, the
/// token/cost usage summed across **every** round (each round is its own API
/// call), and the number of tool calls executed across the whole loop.
pub struct ToolLoopOutcome {
    /// Final tool-call-free response. Part of the driver's general contract; the
    /// Orchestrator caller only needs the side effects + accounting below.
    #[allow(dead_code)]
    pub response: ChatResponse,
    pub usage: Usage,
    pub tool_calls: usize,
}

/// Drive the tool-call loop to completion.
///
/// Takes ownership of `req` (mutated as the conversation grows) to prevent reuse of a stale request.
pub async fn run_tool_loop(
    client: &dyn LlmClient,
    mut req: ChatRequest,
    executor: &dyn ToolExecutor,
    max_rounds: usize,
) -> Result<ToolLoopOutcome> {
    let mut usage = Usage::default();
    let mut tool_call_count = 0usize;
    for _round in 0..max_rounds {
        let resp = client.chat(&req).await?;
        if let Some(u) = &resp.usage {
            usage.add(u);
        }

        let choice = resp.choices.first().ok_or(LlmError::EmptyChoices)?;
        let tool_calls: Vec<ToolCall> = choice.message.tool_calls.clone().unwrap_or_default();

        if tool_calls.is_empty() {
            return Ok(ToolLoopOutcome {
                response: resp,
                usage,
                tool_calls: tool_call_count,
            });
        }
        tool_call_count += tool_calls.len();

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{Choice, CostDetails, FunctionCall, ResponseMessage, Usage};
    use std::sync::Mutex;

    /// Client that replays a fixed script of responses, one per `chat` call.
    struct ScriptedClient {
        responses: Mutex<std::collections::VecDeque<ChatResponse>>,
    }

    #[async_trait]
    impl LlmClient for ScriptedClient {
        async fn chat(&self, _req: &ChatRequest) -> Result<ChatResponse> {
            Ok(self.responses.lock().unwrap().pop_front().expect("script exhausted"))
        }
    }

    struct NoopExecutor;

    #[async_trait]
    impl ToolExecutor for NoopExecutor {
        async fn execute(&self, _name: &str, _args: &str) -> anyhow::Result<String> {
            Ok("{\"ok\":true}".to_string())
        }
    }

    fn usage(cost: f64, upstream: f64) -> Usage {
        Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            cost,
            cost_details: Some(CostDetails {
                upstream_inference_cost: upstream,
            }),
        }
    }

    fn tool_turn(u: Usage) -> ChatResponse {
        ChatResponse {
            id: None,
            model: None,
            usage: Some(u),
            choices: vec![Choice {
                index: 0,
                finish_reason: Some("tool_calls".to_string()),
                message: ResponseMessage {
                    role: Some("assistant".to_string()),
                    content: None,
                    tool_calls: Some(vec![ToolCall {
                        id: "call_1".to_string(),
                        kind: "function".to_string(),
                        function: FunctionCall {
                            name: "upsert_term".to_string(),
                            arguments: "{}".to_string(),
                        },
                    }]),
                },
            }],
        }
    }

    fn stop_turn(u: Usage) -> ChatResponse {
        ChatResponse {
            id: None,
            model: None,
            usage: Some(u),
            choices: vec![Choice {
                index: 0,
                finish_reason: Some("stop".to_string()),
                message: ResponseMessage {
                    role: Some("assistant".to_string()),
                    content: Some("done".to_string()),
                    tool_calls: None,
                },
            }],
        }
    }

    #[tokio::test]
    async fn counts_tool_calls_and_sums_usage_across_rounds() {
        // Round 1: one tool call. Round 2: final stop. Usage accrues on both.
        let script = std::collections::VecDeque::from(vec![
            tool_turn(usage(0.001, 0.01)),
            stop_turn(usage(0.002, 0.02)),
        ]);
        let client = ScriptedClient {
            responses: Mutex::new(script),
        };
        let out = run_tool_loop(&client, ChatRequest::default(), &NoopExecutor, 8)
            .await
            .unwrap();

        assert_eq!(out.tool_calls, 1, "one tool call executed across the loop");
        assert_eq!(out.usage.total_tokens, 30, "tokens summed over both rounds");
        assert!((out.usage.cost_usd() - 0.033).abs() < 1e-9, "fees + upstream over both rounds");
    }
}
