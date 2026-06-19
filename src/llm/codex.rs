//! Codex-backed [`LlmClient`] using the ChatGPT backend Responses API.
//! Translates honya's chat/completions requests into typed Responses input and
//! folds streamed `response.*` events back into [`ChatResponse`].

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use super::client::{LlmClient, LlmError, Result, StreamDelta};
use super::{
    ChatRequest, ChatResponse, Choice, FunctionCall, ResponseFormat, ResponseMessage, Role,
    ToolCall, Usage,
};
use crate::codex::{CodexAuth, auth, now_unix};

const RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";

/// An LLM client backed by a Codex (ChatGPT) account's Responses API.
pub struct CodexClient {
    http: reqwest::Client,
    /// Held behind a mutex so an expired access token can be refreshed mid-run.
    auth: Mutex<CodexAuth>,
}

impl CodexClient {
    pub fn new(auth: CodexAuth) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("honya/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            auth: Mutex::new(auth),
        })
    }

    /// Refresh expired credentials before use.
    async fn fresh_auth(&self) -> Result<CodexAuth> {
        let mut guard = self.auth.lock().await;
        if guard.is_expired(now_unix()) && !guard.refresh_token.is_empty() {
            match auth::refresh(&guard).await {
                Ok(next) => *guard = next,
                Err(e) => {
                    return Err(LlmError::Api {
                        status: 401,
                        message: format!("Codex token refresh failed: {e}"),
                    });
                }
            }
        }
        Ok(guard.clone())
    }

    async fn run(
        &self,
        req: &ChatRequest,
        on_delta: &mut (dyn for<'a> FnMut(StreamDelta<'a>) + Send),
    ) -> Result<ChatResponse> {
        let auth = self.fresh_auth().await?;
        let body = build_body(req);

        let resp = self
            .http
            .post(RESPONSES_URL)
            .bearer_auth(&auth.access_token)
            .header("chatgpt-account-id", &auth.account_id)
            .header("OpenAI-Beta", "responses=experimental")
            .header("originator", "codex_cli_rs")
            .header("session_id", crate::codex::uuid_v4())
            .header("Accept", "text/event-stream")
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if status == StatusCode::TOO_MANY_REQUESTS {
            let message = resp.text().await.unwrap_or_default();
            return Err(LlmError::RateLimited {
                retry_after: 0,
                message,
            });
        }
        if !status.is_success() {
            let message = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api {
                status: status.as_u16(),
                message,
            });
        }

        let mut bytes = resp.bytes_stream();
        let mut line_buf: Vec<u8> = Vec::new();
        let mut acc = Acc::default();
        while let Some(chunk) = bytes.next().await {
            line_buf.extend_from_slice(&chunk?);
            while let Some(pos) = line_buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = line_buf.drain(..=pos).collect();
                handle_event_line(&line, on_delta, &mut acc)?;
            }
        }
        if !line_buf.is_empty() {
            handle_event_line(&line_buf, on_delta, &mut acc)?;
        }

        if let Some(err) = acc.error {
            return Err(LlmError::Api {
                status: 502,
                message: err,
            });
        }
        Ok(acc.into_response())
    }
}

#[async_trait]
impl LlmClient for CodexClient {
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
        self.run(req, &mut |_| {}).await
    }

    async fn chat_stream(
        &self,
        req: &ChatRequest,
        on_delta: &mut (dyn for<'a> FnMut(StreamDelta<'a>) + Send),
    ) -> Result<ChatResponse> {
        self.run(req, on_delta).await
    }
}

/// Build the Responses API request body from a chat-completions [`ChatRequest`].
fn build_body(req: &ChatRequest) -> Value {
    let mut instructions = String::new();
    let mut input: Vec<Value> = Vec::new();

    for m in &req.messages {
        match m.role {
            Role::System => {
                if !instructions.is_empty() {
                    instructions.push_str("\n\n");
                }
                instructions.push_str(m.content.as_deref().unwrap_or(""));
            }
            Role::User => input.push(json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": m.content.clone().unwrap_or_default() }],
            })),
            Role::Assistant => {
                if let Some(text) = m.content.as_deref().filter(|t| !t.is_empty()) {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": text }],
                    }));
                }
                for call in m.tool_calls.iter().flatten() {
                    input.push(json!({
                        "type": "function_call",
                        "name": call.function.name,
                        "arguments": call.function.arguments,
                        "call_id": call.id,
                    }));
                }
            }
            Role::Tool => input.push(json!({
                "type": "function_call_output",
                "call_id": m.tool_call_id.clone().unwrap_or_default(),
                "output": m.content.clone().unwrap_or_default(),
            })),
        }
    }

    let mut body = json!({
        "model": req.model,
        "instructions": instructions,
        "input": input,
        "store": false,
        "stream": true,
    });

    if let Some(tools) = &req.tools {
        let mapped: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "name": t.function.name,
                    "description": t.function.description,
                    "parameters": t.function.parameters,
                    "strict": false,
                })
            })
            .collect();
        body["tools"] = json!(mapped);
        body["tool_choice"] = json!("auto");
        body["parallel_tool_calls"] = json!(false);
    }

    // Codex is reasoning-capable; default the effort when config omits it.
    let effort = req
        .reasoning
        .as_ref()
        .and_then(|r| r.get("effort"))
        .and_then(Value::as_str)
        .unwrap_or("medium");
    body["reasoning"] = json!({ "effort": effort, "summary": "auto" });

    if let Some(ResponseFormat::JsonSchema { json_schema }) = &req.response_format {
        body["text"] = json!({
            "format": {
                "type": "json_schema",
                "name": json_schema.name,
                "strict": json_schema.strict,
                "schema": json_schema.schema,
            }
        });
    }

    body
}

/// Streamed reply accumulator, folded into a [`ChatResponse`] at the end.
#[derive(Default)]
struct Acc {
    text: String,
    tool_calls: Vec<ToolCall>,
    usage: Usage,
    id: Option<String>,
    model: Option<String>,
    error: Option<String>,
}

impl Acc {
    fn into_response(self) -> ChatResponse {
        let saw_tool = !self.tool_calls.is_empty();
        let content = (!self.text.is_empty()).then_some(self.text);
        let tool_calls = saw_tool.then_some(self.tool_calls);
        let finish_reason = Some(if saw_tool { "tool_calls" } else { "stop" }.to_string());
        ChatResponse {
            id: self.id,
            model: self.model,
            choices: vec![Choice {
                index: 0,
                message: ResponseMessage {
                    role: Some("assistant".to_string()),
                    content,
                    tool_calls,
                },
                finish_reason,
            }],
            usage: Some(self.usage),
            service_tier: None,
        }
    }
}

/// Parse one SSE line. Only `data:` lines carry JSON; the event type is also in
/// the JSON `type` field, so `event:` lines are ignored.
fn handle_event_line(
    line: &[u8],
    on_delta: &mut (dyn for<'a> FnMut(StreamDelta<'a>) + Send),
    acc: &mut Acc,
) -> Result<()> {
    let text = String::from_utf8_lossy(line);
    let Some(data) = text.trim_end().strip_prefix("data:") else {
        return Ok(());
    };
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(());
    }
    let event: Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(_) => return Ok(()), // tolerate partial/keepalive frames
    };
    apply_event(&event, on_delta, acc);
    Ok(())
}

/// Apply one Responses event to the accumulator.
fn apply_event(
    event: &Value,
    on_delta: &mut (dyn for<'a> FnMut(StreamDelta<'a>) + Send),
    acc: &mut Acc,
) {
    let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "response.output_text.delta" => {
            if let Some(d) = event.get("delta").and_then(Value::as_str) {
                acc.text.push_str(d);
                on_delta(StreamDelta::Content(d));
            }
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            if let Some(d) = event.get("delta").and_then(Value::as_str) {
                on_delta(StreamDelta::Reasoning(d));
            }
        }
        "response.output_item.done" => {
            if let Some(item) = event.get("item")
                && item.get("type").and_then(Value::as_str) == Some("function_call")
            {
                acc.tool_calls.push(ToolCall {
                    id: item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    kind: "function".to_string(),
                    function: FunctionCall {
                        name: item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        arguments: item
                            .get("arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("{}")
                            .to_string(),
                    },
                });
            }
        }
        "response.created" | "response.completed" => {
            if let Some(r) = event.get("response") {
                if acc.id.is_none() {
                    acc.id = r.get("id").and_then(Value::as_str).map(str::to_string);
                }
                if acc.model.is_none() {
                    acc.model = r.get("model").and_then(Value::as_str).map(str::to_string);
                }
                if let Some(u) = r.get("usage") {
                    acc.usage.prompt_tokens =
                        u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0) as u32;
                    acc.usage.completion_tokens =
                        u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0) as u32;
                    acc.usage.total_tokens =
                        u.get("total_tokens").and_then(Value::as_u64).unwrap_or(
                            (acc.usage.prompt_tokens + acc.usage.completion_tokens) as u64,
                        ) as u32;
                }
            }
        }
        "response.failed" | "error" | "response.error" => {
            let msg = event
                .pointer("/response/error/message")
                .or_else(|| event.pointer("/error/message"))
                .or_else(|| event.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("Codex stream error")
                .to_string();
            acc.error = Some(msg);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{Message, Tool};

    #[test]
    fn build_body_maps_system_to_instructions_and_messages_to_input() {
        let req = ChatRequest {
            model: "gpt-5.5".into(),
            reasoning: Some(json!({ "effort": "high" })),
            ..ChatRequest::new(
                "gpt-5.5",
                vec![
                    Message::system("be terse"),
                    Message::user("translate this"),
                ],
            )
        };
        let body = build_body(&req);
        assert_eq!(body["instructions"], "be terse");
        assert_eq!(body["input"][0]["role"], "user");
        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn build_body_flattens_tools_and_tool_results() {
        let mut req = ChatRequest::new(
            "gpt-5.5-codex",
            vec![Message::tool_result("call_1", "{\"ok\":true}")],
        );
        req.tools = Some(vec![Tool::function("do_it", "does it", json!({"type":"object"}))]);
        let body = build_body(&req);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["name"], "do_it");
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(body["input"][0]["type"], "function_call_output");
        assert_eq!(body["input"][0]["call_id"], "call_1");
    }

    #[test]
    fn stream_events_accumulate_text_tools_and_usage() {
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut on = |d: StreamDelta| match d {
            StreamDelta::Content(s) => text.push_str(s),
            StreamDelta::Reasoning(s) => reasoning.push_str(s),
        };
        let mut acc = Acc::default();
        for line in [
            r#"data: {"type":"response.reasoning_summary_text.delta","delta":"think "}"#,
            r#"data: {"type":"response.output_text.delta","delta":"hello "}"#,
            r#"data: {"type":"response.output_text.delta","delta":"world"}"#,
            r#"data: {"type":"response.output_item.done","item":{"type":"function_call","name":"f","arguments":"{}","call_id":"c1"}}"#,
            r#"data: {"type":"response.completed","response":{"id":"r1","model":"gpt-5.5","usage":{"input_tokens":10,"output_tokens":5}}}"#,
        ] {
            handle_event_line(line.as_bytes(), &mut on, &mut acc).unwrap();
        }
        assert_eq!(text, "hello world");
        assert_eq!(reasoning, "think ");
        let resp = acc.into_response();
        assert_eq!(resp.id.as_deref(), Some("r1"));
        let choice = &resp.choices[0];
        assert_eq!(choice.message.content.as_deref(), Some("hello world"));
        assert_eq!(choice.finish_reason.as_deref(), Some("tool_calls"));
        let calls = choice.message.tool_calls.as_ref().unwrap();
        assert_eq!(calls[0].function.name, "f");
        assert_eq!(calls[0].id, "c1");
        let usage = resp.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.total_tokens, 15);
    }
}
