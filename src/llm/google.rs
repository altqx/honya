//! Google Gemini Interactions API transport.

use std::collections::HashMap;

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::model::ServiceTier;

use super::client::{
    ClientConfig, LlmClient, LlmError, Result, StreamDelta, parse_retry_after, retry_after_hint,
    retry_backoff,
};
use super::{
    ChatRequest, ChatResponse, Choice, FunctionCall, Message, ResponseFormat, ResponseMessage,
    Role, Tool, ToolCall, Usage,
};

const API_REVISION: &str = "2026-05-20";

pub struct GoogleInteractionsClient {
    http: reqwest::Client,
    cfg: ClientConfig,
}

impl GoogleInteractionsClient {
    pub fn new(cfg: ClientConfig) -> Result<Self> {
        let http = reqwest::Client::builder().timeout(cfg.timeout).build()?;
        Ok(Self { http, cfg })
    }

    async fn send_once(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let resp = self.request_builder(req, false).send().await?;
        let status = resp.status();

        if status == StatusCode::TOO_MANY_REQUESTS {
            let retry_after = parse_retry_after(&resp);
            let message = google_error_message(resp.text().await.unwrap_or_default());
            return Err(LlmError::RateLimited {
                retry_after,
                message,
            });
        }

        if !status.is_success() {
            let message = google_error_message(resp.text().await.unwrap_or_default());
            return Err(LlmError::Api {
                status: status.as_u16(),
                message,
            });
        }

        let raw = resp.text().await?;
        let parsed: InteractionResponse =
            serde_json::from_str(&raw).map_err(|source| LlmError::Parse {
                target: "Google Interaction",
                source,
                raw,
            })?;
        interaction_to_chat(parsed)
    }

    async fn send_stream_once(
        &self,
        req: &ChatRequest,
        on_delta: &mut (dyn for<'a> FnMut(StreamDelta<'a>) + Send),
    ) -> Result<ChatResponse> {
        let resp = self.request_builder(req, true).send().await?;
        let status = resp.status();

        if status == StatusCode::TOO_MANY_REQUESTS {
            let retry_after = parse_retry_after(&resp);
            let message = google_error_message(resp.text().await.unwrap_or_default());
            return Err(LlmError::RateLimited {
                retry_after,
                message,
            });
        }

        if !status.is_success() {
            let message = google_error_message(resp.text().await.unwrap_or_default());
            return Err(LlmError::Api {
                status: status.as_u16(),
                message,
            });
        }

        let mut bytes = resp.bytes_stream();
        let mut line_buf: Vec<u8> = Vec::new();
        let mut stream = GoogleStreamResponse::default();

        while let Some(chunk) = bytes.next().await {
            line_buf.extend_from_slice(&chunk?);
            while let Some(pos) = line_buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = line_buf.drain(..=pos).collect();
                handle_sse_line(&line, on_delta, &mut stream)?;
            }
        }

        if !line_buf.is_empty() {
            handle_sse_line(&line_buf, on_delta, &mut stream)?;
        }

        stream.into_response()
    }

    fn request_builder(&self, req: &ChatRequest, stream: bool) -> reqwest::RequestBuilder {
        self.http
            .post(self.endpoint())
            .header("x-goog-api-key", &self.cfg.api_key)
            .header("Api-Revision", API_REVISION)
            .json(&build_body(req, self.cfg.service_tier, stream))
    }

    fn endpoint(&self) -> String {
        let base = self.cfg.base_url.trim_end_matches('/');
        format!("{base}/interactions")
    }
}

#[async_trait]
impl LlmClient for GoogleInteractionsClient {
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let max = self.cfg.max_send_attempts();
        let mut retry = 0u32;
        loop {
            match self.send_once(req).await {
                Err(e) if retry + 1 < max && e.is_retryable() => {
                    retry += 1;
                    tokio::time::sleep(retry_backoff(retry, retry_after_hint(&e))).await;
                }
                other => return other,
            }
        }
    }

    async fn chat_stream(
        &self,
        req: &ChatRequest,
        on_delta: &mut (dyn for<'a> FnMut(StreamDelta<'a>) + Send),
    ) -> Result<ChatResponse> {
        let emitted = std::sync::atomic::AtomicBool::new(false);
        let mut tracked = |delta: StreamDelta| {
            emitted.store(true, std::sync::atomic::Ordering::Relaxed);
            on_delta(delta);
        };
        let max = self.cfg.max_send_attempts();
        let mut retry = 0u32;
        loop {
            match self.send_stream_once(req, &mut tracked).await {
                Err(e)
                    if retry + 1 < max
                        && e.is_retryable()
                        && !emitted.load(std::sync::atomic::Ordering::Relaxed) =>
                {
                    retry += 1;
                    tokio::time::sleep(retry_backoff(retry, retry_after_hint(&e))).await;
                }
                other => return other,
            }
        }
    }
}

fn build_body(req: &ChatRequest, configured_tier: Option<ServiceTier>, stream: bool) -> Value {
    let (system_instruction, input) = messages_to_steps(&req.messages);
    let mut body = json!({
        "model": req.model,
        "input": input,
        "store": false,
    });

    if stream {
        body["stream"] = json!(true);
    }
    if let Some(system_instruction) = system_instruction {
        body["system_instruction"] = json!(system_instruction);
    }
    if let Some(tools) = req.tools.as_deref()
        && !tools.is_empty()
    {
        body["tools"] = Value::Array(tools.iter().map(tool_to_google).collect());
    }
    if let Some(format) = &req.response_format {
        body["response_format"] = response_format_to_google(format);
    }
    if let Some(config) = generation_config(req, stream) {
        body["generation_config"] = config;
    }
    if let Some(tier) = req.service_tier.or(configured_tier) {
        body["service_tier"] = json!(service_tier_value(tier));
    }

    body
}

fn messages_to_steps(messages: &[Message]) -> (Option<String>, Vec<Value>) {
    let mut system = Vec::new();
    let mut steps = Vec::new();

    for message in messages {
        match message.role {
            Role::System => {
                if let Some(content) = message.content.as_deref()
                    && !content.is_empty()
                {
                    system.push(content.to_string());
                }
            }
            Role::User => steps.push(json!({
                "type": "user_input",
                "content": text_content(message.content.as_deref().unwrap_or_default()),
            })),
            Role::Assistant => {
                if let Some(content) = message.content.as_deref()
                    && !content.is_empty()
                {
                    steps.push(json!({
                        "type": "model_output",
                        "content": text_content(content),
                    }));
                }
                if let Some(calls) = &message.tool_calls {
                    for call in calls {
                        let args = serde_json::from_str::<Value>(&call.function.arguments)
                            .unwrap_or_else(|_| json!(call.function.arguments));
                        steps.push(json!({
                            "type": "function_call",
                            "id": call.id,
                            "name": call.function.name,
                            "arguments": args,
                        }));
                    }
                }
            }
            Role::Tool => {
                let mut step = Map::new();
                step.insert("type".to_string(), json!("function_result"));
                step.insert(
                    "call_id".to_string(),
                    json!(message.tool_call_id.as_deref().unwrap_or_default()),
                );
                if let Some(name) = message.name.as_deref()
                    && !name.is_empty()
                {
                    step.insert("name".to_string(), json!(name));
                }
                step.insert(
                    "result".to_string(),
                    json!(text_content(message.content.as_deref().unwrap_or_default())),
                );
                steps.push(Value::Object(step));
            }
        }
    }

    if steps.is_empty() {
        steps.push(json!({
            "type": "user_input",
            "content": text_content(""),
        }));
    }

    let system = (!system.is_empty()).then(|| system.join("\n\n"));
    (system, steps)
}

fn text_content(text: &str) -> Vec<Value> {
    vec![json!({ "type": "text", "text": text })]
}

fn tool_to_google(tool: &Tool) -> Value {
    let mut out = Map::new();
    out.insert("type".to_string(), json!(tool.kind));
    out.insert("name".to_string(), json!(tool.function.name));
    if let Some(description) = &tool.function.description {
        out.insert("description".to_string(), json!(description));
    }
    out.insert("parameters".to_string(), json!(tool.function.parameters));
    Value::Object(out)
}

fn response_format_to_google(format: &ResponseFormat) -> Value {
    match format {
        ResponseFormat::JsonObject => json!({
            "type": "text",
            "mime_type": "application/json",
        }),
        ResponseFormat::JsonSchema { json_schema } => json!({
            "type": "text",
            "mime_type": "application/json",
            "schema": json_schema.schema,
        }),
    }
}

fn generation_config(req: &ChatRequest, include_thought_summaries: bool) -> Option<Value> {
    let mut config = serde_json::Map::new();

    if include_thought_summaries {
        config.insert("thinking_summaries".to_string(), json!("auto"));
    }

    if let Some(temperature) = req.temperature {
        config.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(max_tokens) = req.max_tokens {
        config.insert("max_output_tokens".to_string(), json!(max_tokens));
    }
    if let Some(level) = req
        .reasoning
        .as_ref()
        .and_then(|r| r.get("effort"))
        .and_then(Value::as_str)
        .and_then(google_thinking_level)
    {
        config.insert("thinking_level".to_string(), json!(level));
    }

    (!config.is_empty()).then_some(Value::Object(config))
}

fn google_thinking_level(effort: &str) -> Option<&'static str> {
    match effort {
        "minimal" => Some("minimal"),
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" => Some("high"),
        _ => None,
    }
}

fn service_tier_value(tier: ServiceTier) -> &'static str {
    match tier {
        ServiceTier::Flex => "flex",
        ServiceTier::Priority => "priority",
    }
}

#[derive(Debug, Deserialize)]
struct InteractionResponse {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    service_tier: Option<String>,
    #[serde(default)]
    steps: Vec<InteractionStep>,
    #[serde(default)]
    usage: Option<InteractionUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum InteractionStep {
    ModelOutput {
        #[serde(default)]
        content: Vec<InteractionContent>,
    },
    FunctionCall {
        id: String,
        name: String,
        #[serde(default)]
        arguments: Value,
    },
    Error {
        #[serde(default)]
        message: Option<String>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum InteractionContent {
    Text {
        text: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
struct InteractionUsage {
    #[serde(default)]
    total_input_tokens: u32,
    #[serde(default)]
    total_output_tokens: u32,
    #[serde(default)]
    total_tokens: u32,
}

fn interaction_to_chat(interaction: InteractionResponse) -> Result<ChatResponse> {
    let mut content = String::new();
    let mut tool_calls = Vec::new();
    let mut errors = Vec::new();

    for step in &interaction.steps {
        match step {
            InteractionStep::ModelOutput { content: parts } => {
                for part in parts {
                    if let InteractionContent::Text { text } = part {
                        content.push_str(text);
                    }
                }
            }
            InteractionStep::FunctionCall {
                id,
                name,
                arguments,
            } => tool_calls.push(ToolCall {
                id: id.clone(),
                kind: "function".to_string(),
                function: FunctionCall {
                    name: name.clone(),
                    arguments: arguments.to_string(),
                },
            }),
            InteractionStep::Error { message } => {
                errors.push(
                    message
                        .clone()
                        .unwrap_or_else(|| "interaction error".to_string()),
                );
            }
            InteractionStep::Other => {}
        }
    }

    if !errors.is_empty() {
        return Err(LlmError::Api {
            status: 0,
            message: errors.join("; "),
        });
    }

    let finish_reason = match interaction.status.as_deref() {
        Some("requires_action") => Some("tool_calls".to_string()),
        Some("completed") | None => Some("stop".to_string()),
        Some(status) => Some(status.to_string()),
    };
    let usage = interaction.usage.map(|usage| Usage {
        prompt_tokens: usage.total_input_tokens,
        completion_tokens: usage.total_output_tokens,
        total_tokens: usage.total_tokens,
        cost: 0.0,
        cost_details: None,
        served_tier: served_tier(interaction.service_tier.as_deref()),
    });

    Ok(ChatResponse {
        id: interaction.id,
        model: interaction.model,
        choices: vec![Choice {
            index: 0,
            message: ResponseMessage {
                role: Some("assistant".to_string()),
                content: (!content.is_empty()).then_some(content),
                tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
            },
            finish_reason,
        }],
        usage,
        service_tier: served_tier(interaction.service_tier.as_deref()),
    })
}

fn served_tier(value: Option<&str>) -> Option<super::ServedTier> {
    match value {
        Some("flex") => Some(super::ServedTier::Flex),
        Some("priority") => Some(super::ServedTier::Priority),
        Some("standard" | "default") => Some(super::ServedTier::Default),
        _ => None,
    }
}

#[derive(Default)]
struct GoogleStreamResponse {
    content: String,
    latest_interaction: Option<InteractionResponse>,
    step_types: HashMap<u32, String>,
}

impl GoogleStreamResponse {
    fn into_response(self) -> Result<ChatResponse> {
        if let Some(interaction) = self.latest_interaction {
            let mut resp = interaction_to_chat(interaction)?;
            // Streaming delivers the text via `step.delta` events; the terminal
            // `interaction.completed` summary carries no `steps`, so its content
            // is empty. Recover the accumulated stream instead of dropping it.
            if let Some(choice) = resp.choices.first_mut() {
                let has_text = choice
                    .message
                    .content
                    .as_deref()
                    .is_some_and(|c| !c.is_empty());
                if !has_text && !self.content.is_empty() {
                    choice.message.content = Some(self.content);
                }
            }
            return Ok(resp);
        }

        Ok(ChatResponse {
            id: None,
            model: None,
            choices: vec![Choice {
                index: 0,
                message: ResponseMessage {
                    role: Some("assistant".to_string()),
                    content: (!self.content.is_empty()).then_some(self.content),
                    tool_calls: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: None,
            service_tier: None,
        })
    }
}

fn handle_sse_line(
    line: &[u8],
    on_delta: &mut (dyn for<'a> FnMut(StreamDelta<'a>) + Send),
    stream: &mut GoogleStreamResponse,
) -> Result<()> {
    let line = String::from_utf8_lossy(line);
    let line = line.trim();
    let Some(data) = line.strip_prefix("data:") else {
        return Ok(());
    };
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(());
    }

    let event: Value = serde_json::from_str(data).map_err(|source| LlmError::Parse {
        target: "Google SSE event",
        source,
        raw: data.to_string(),
    })?;

    match event.get("event_type").and_then(Value::as_str) {
        Some("error") => {
            let message = event
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("Google Interactions stream error")
                .to_string();
            return Err(LlmError::Api { status: 0, message });
        }
        Some("step.start") => {
            let step = event.get("step");
            if let Some(index) = event.get("index").and_then(Value::as_u64)
                && let Some(kind) = step
                    .and_then(|value| value.get("type"))
                    .and_then(Value::as_str)
            {
                stream.step_types.insert(index as u32, kind.to_string());
                match kind {
                    "model_output" => {
                        emit_text_blocks(step.and_then(|value| value.get("content")), |text| {
                            stream.content.push_str(text);
                            on_delta(StreamDelta::Content(text));
                        })
                    }
                    "thought" => {
                        emit_text_blocks(step.and_then(|value| value.get("summary")), |text| {
                            on_delta(StreamDelta::Reasoning(text))
                        })
                    }
                    _ => {}
                }
            }
        }
        Some("step.delta") => {
            let step_kind = event
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|index| stream.step_types.get(&(index as u32)))
                .map(String::as_str);
            if let Some(delta) = event.get("delta") {
                match delta.get("type").and_then(Value::as_str) {
                    Some("text") if step_kind.is_none_or(|kind| kind == "model_output") => {
                        if let Some(text) = delta_text(delta) {
                            stream.content.push_str(text);
                            on_delta(StreamDelta::Content(text));
                        }
                    }
                    Some("thought_summary" | "thought") => {
                        if let Some(text) = delta_text(delta) {
                            on_delta(StreamDelta::Reasoning(text));
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }

    if let Some(interaction) = event.get("interaction") {
        let parsed = serde_json::from_value::<InteractionResponse>(interaction.clone()).map_err(
            |source| LlmError::Parse {
                target: "Google stream interaction",
                source,
                raw: interaction.to_string(),
            },
        )?;
        stream.latest_interaction = Some(parsed);
    }

    Ok(())
}

fn emit_text_blocks(blocks: Option<&Value>, mut emit: impl FnMut(&str)) {
    let Some(blocks) = blocks.and_then(Value::as_array) else {
        return;
    };
    for block in blocks {
        if block.get("type").and_then(Value::as_str) == Some("text")
            && let Some(text) = block.get("text").and_then(Value::as_str)
            && !text.is_empty()
        {
            emit(text);
        }
    }
}

fn delta_text(delta: &Value) -> Option<&str> {
    delta
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| {
            delta
                .get("content")
                .and_then(|content| content.get("text"))
                .and_then(Value::as_str)
        })
        .filter(|text| !text.is_empty())
}

fn google_error_message(raw: String) -> String {
    serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .filter(|s| !s.is_empty())
        .unwrap_or(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{FunctionDef, JsonSchemaSpec};

    #[test]
    fn build_body_maps_chat_contract_to_interaction_steps() {
        let req = ChatRequest {
            tools: Some(vec![Tool {
                kind: "function".to_string(),
                function: FunctionDef {
                    name: "upsert".to_string(),
                    description: Some("write metadata".to_string()),
                    parameters: json!({"type": "object"}),
                },
            }]),
            response_format: Some(ResponseFormat::JsonSchema {
                json_schema: JsonSchemaSpec {
                    name: "translation_result".to_string(),
                    strict: true,
                    schema: json!({"type": "object", "required": ["translated_text"]}),
                },
            }),
            reasoning: Some(json!({"effort": "xhigh"})),
            service_tier: Some(ServiceTier::Flex),
            ..ChatRequest::new(
                "gemini-3.5-flash",
                vec![
                    Message::system("translate JP to Thai"),
                    Message::user("こんにちは"),
                ],
            )
        };

        let body = build_body(&req, None, false);
        assert_eq!(body["model"], "gemini-3.5-flash");
        assert_eq!(body["store"], false);
        assert_eq!(body["system_instruction"], "translate JP to Thai");
        assert_eq!(body["input"][0]["type"], "user_input");
        assert_eq!(body["input"][0]["content"][0]["text"], "こんにちは");
        assert_eq!(body["tools"][0]["name"], "upsert");
        assert_eq!(body["tools"][0]["parameters"]["type"], "object");
        assert_eq!(body["response_format"]["mime_type"], "application/json");
        assert_eq!(body["generation_config"]["thinking_level"], "high");
        assert_eq!(body["service_tier"], "flex");
    }

    #[test]
    fn body_requests_thought_summaries_without_explicit_effort() {
        let body = build_body(
            &ChatRequest::new("gemini-3.5-flash", vec![Message::user("go")]),
            None,
            true,
        );
        assert_eq!(body["generation_config"]["thinking_summaries"], "auto");
    }

    #[test]
    fn body_flattens_tool_calls_and_results_to_steps() {
        let mut req = ChatRequest::new("gemini-3.5-flash", vec![Message::user("go")]);
        req.messages.push(Message {
            role: Role::Assistant,
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".to_string(),
                kind: "function".to_string(),
                function: FunctionCall {
                    name: "lookup".to_string(),
                    arguments: r#"{"term":"猫"}"#.to_string(),
                },
            }]),
            tool_call_id: None,
            name: None,
        });
        req.messages
            .push(Message::tool_result("call_1", r#"{"ok":true}"#));

        let body = build_body(&req, None, false);
        assert_eq!(body["input"][1]["type"], "function_call");
        assert_eq!(body["input"][1]["arguments"]["term"], "猫");
        assert_eq!(body["input"][2]["type"], "function_result");
        assert_eq!(body["input"][2]["call_id"], "call_1");
    }

    #[test]
    fn interaction_response_maps_text_tool_calls_and_usage() {
        let raw = r#"{
          "id": "i1",
          "model": "gemini-3.5-flash",
          "status": "requires_action",
          "service_tier": "priority",
          "steps": [
            {"type":"model_output","content":[{"type":"text","text":"partial"}]},
            {"type":"function_call","id":"call_1","name":"write","arguments":{"ok":true}}
          ],
          "usage": {
            "total_input_tokens": 10,
            "total_output_tokens": 5,
            "total_tokens": 15
          }
        }"#;
        let parsed: InteractionResponse = serde_json::from_str(raw).unwrap();
        let chat = interaction_to_chat(parsed).unwrap();
        let choice = &chat.choices[0];
        assert_eq!(choice.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(choice.message.content.as_deref(), Some("partial"));
        let call = &choice.message.tool_calls.as_ref().unwrap()[0];
        assert_eq!(call.function.name, "write");
        assert_eq!(call.function.arguments, r#"{"ok":true}"#);
        assert_eq!(chat.usage.unwrap().total_tokens, 15);
        assert_eq!(chat.service_tier, Some(super::super::ServedTier::Priority));
    }

    #[test]
    fn sse_text_delta_streams_and_final_interaction_wins() {
        let mut stream = GoogleStreamResponse::default();
        let mut content = String::new();
        let mut on_delta = |delta: StreamDelta| {
            let StreamDelta::Content(text) = delta else {
                return;
            };
            content.push_str(text);
        };

        handle_sse_line(
            br#"data: {"event_type":"step.start","index":0,"step":{"type":"model_output","content":[{"type":"text","text":"he"}]}}"#,
            &mut on_delta,
            &mut stream,
        )
        .unwrap();
        handle_sse_line(
            br#"data: {"event_type":"step.delta","index":0,"delta":{"type":"text","text":"l"}}"#,
            &mut on_delta,
            &mut stream,
        )
        .unwrap();
        handle_sse_line(
            br#"data: {"event_type":"step.stop","index":0,"interaction":{"id":"i1","status":"completed","steps":[{"type":"model_output","content":[{"type":"text","text":"hello"}]}]}}"#,
            &mut on_delta,
            &mut stream,
        )
        .unwrap();

        assert_eq!(content, "hel");
        let chat = stream.into_response().unwrap();
        assert_eq!(chat.choices[0].message.content.as_deref(), Some("hello"));
    }

    #[test]
    fn sse_thought_summary_streams_as_reasoning() {
        let mut stream = GoogleStreamResponse::default();
        let mut reasoning = String::new();
        let mut on_delta = |delta: StreamDelta| {
            if let StreamDelta::Reasoning(text) = delta {
                reasoning.push_str(text);
            }
        };

        handle_sse_line(
            br#"data: {"event_type":"step.start","index":0,"step":{"type":"thought","summary":[{"type":"text","text":"checking "}]}}"#,
            &mut on_delta,
            &mut stream,
        )
        .unwrap();
        handle_sse_line(
            br#"data: {"event_type":"step.delta","index":0,"delta":{"type":"thought_summary","content":{"type":"text","text":"terms"}}}"#,
            &mut on_delta,
            &mut stream,
        )
        .unwrap();
        handle_sse_line(
            br#"data: {"event_type":"step.delta","index":0,"delta":{"type":"thought_signature","signature":"encrypted"}}"#,
            &mut on_delta,
            &mut stream,
        )
        .unwrap();

        assert_eq!(reasoning, "checking terms");
        assert!(stream.content.is_empty());
    }

    #[test]
    fn stepless_terminal_interaction_keeps_streamed_content() {
        // The live Interactions API streams text via `step.delta` and ends with an
        // `interaction.completed` summary that carries no `steps` — the content must
        // survive instead of collapsing to an empty `finish_reason=stop` response.
        let mut stream = GoogleStreamResponse::default();
        let mut sink = |_: StreamDelta| {};

        handle_sse_line(
            br#"data: {"event_type":"step.start","index":1,"step":{"type":"model_output"}}"#,
            &mut sink,
            &mut stream,
        )
        .unwrap();
        handle_sse_line(
            br#"data: {"event_type":"step.delta","index":1,"delta":{"type":"text","text":"{\"translated_text\":\"hi\"}"}}"#,
            &mut sink,
            &mut stream,
        )
        .unwrap();
        handle_sse_line(
            br#"data: {"event_type":"interaction.completed","interaction":{"id":"i9","status":"completed","model":"gemini-3-flash-preview","usage":{"total_input_tokens":12,"total_output_tokens":8,"total_tokens":20}}}"#,
            &mut sink,
            &mut stream,
        )
        .unwrap();

        let chat = stream.into_response().unwrap();
        assert_eq!(
            chat.choices[0].message.content.as_deref(),
            Some(r#"{"translated_text":"hi"}"#)
        );
        assert_eq!(chat.choices[0].finish_reason.as_deref(), Some("stop"));
        assert_eq!(chat.usage.unwrap().total_tokens, 20);
    }
}
