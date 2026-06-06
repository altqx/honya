//! The live OpenRouter HTTP client, the `LlmClient` trait, and the crate-shared `LlmError`.
//!
//! The `Result<T>` alias here shadows `std::result::Result` within `crate::llm::*`.

use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::StatusCode;
use serde::Deserialize;

use crate::model::AppConfig;

use super::{ChatRequest, ChatResponse, Choice, ResponseMessage, Usage};

/// All failure modes of the LLM layer.
#[derive(thiserror::Error, Debug)]
pub enum LlmError {
    /// Underlying HTTP/transport failure (connection, TLS, decode, timeout).
    #[error("HTTP transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// HTTP 429 — rate limited; `retry_after` is the seconds hint if present.
    #[error("rate limited (retry after {retry_after}s): {message}")]
    RateLimited { retry_after: u64, message: String },

    /// Any non-success HTTP status other than 429.
    #[error("API error (status {status}): {message}")]
    Api { status: u16, message: String },

    /// The response carried zero choices.
    #[error("the model returned no choices")]
    EmptyChoices,

    /// A structured response failed to deserialize into the target type.
    #[error("failed to parse {target}: {source} — raw: {raw}")]
    Parse {
        target: &'static str,
        #[source]
        source: serde_json::Error,
        raw: String,
    },
}

/// LLM-layer result alias. Shadows `std::result::Result` within `crate::llm::*`.
pub type Result<T> = std::result::Result<T, LlmError>;

/// Everything the HTTP client needs to talk to OpenRouter.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Base URL, e.g. `https://openrouter.ai/api/v1` (no trailing slash needed).
    pub base_url: String,
    /// Bearer token.
    pub api_key: String,
    /// `HTTP-Referer` ranking header (optional).
    pub referer: Option<String>,
    /// `X-Title` ranking header (optional).
    pub title: Option<String>,
    /// Per-request timeout.
    pub timeout: Duration,
}

impl ClientConfig {
    /// Build a config from an [`AppConfig`] + the key resolved once at startup.
    pub fn from_app_config(cfg: &AppConfig, api_key: String) -> Self {
        Self {
            base_url: cfg.base_url.clone(),
            api_key,
            referer: cfg.referer.clone(),
            title: cfg.title.clone(),
            timeout: Duration::from_secs(120),
        }
    }

    fn endpoint(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!("{base}/chat/completions")
    }
}

/// The single capability every backend (live or mock) exposes: one chat call.
#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse>;

    async fn chat_stream(
        &self,
        req: &ChatRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<ChatResponse> {
        let resp = self.chat(req).await?;
        let content = resp
            .choices
            .first()
            .and_then(|choice| choice.message.content.clone())
            .unwrap_or_default();
        if !content.is_empty() {
            on_delta(&content);
        }
        Ok(resp)
    }
}

/// Live OpenRouter chat client over reqwest + rustls.
pub struct OpenRouterClient {
    http: reqwest::Client,
    cfg: ClientConfig,
}

impl OpenRouterClient {
    pub fn new(cfg: ClientConfig) -> Result<Self> {
        let http = reqwest::Client::builder().timeout(cfg.timeout).build()?;
        Ok(Self { http, cfg })
    }

    /// Issue one POST and classify the response; 429 extracts the `Retry-After` seconds.
    async fn send_once(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let builder = self.request_builder(req);

        let resp = builder.send().await?;
        let status = resp.status();

        if status == StatusCode::TOO_MANY_REQUESTS {
            let retry_after = parse_retry_after(&resp);
            let message = resp.text().await.unwrap_or_default();
            return Err(LlmError::RateLimited {
                retry_after,
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

        let raw = resp.text().await?;
        let parsed: ChatResponse =
            serde_json::from_str(&raw).map_err(|source| LlmError::Parse {
                target: "ChatResponse",
                source,
                raw,
            })?;

        if parsed.choices.is_empty() {
            return Err(LlmError::EmptyChoices);
        }

        Ok(parsed)
    }

    async fn send_stream_once(
        &self,
        req: &ChatRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<ChatResponse> {
        let mut stream_req = req.clone();
        stream_req.stream = Some(true);

        let resp = self.request_builder(&stream_req).send().await?;
        let status = resp.status();

        if status == StatusCode::TOO_MANY_REQUESTS {
            let retry_after = parse_retry_after(&resp);
            let message = resp.text().await.unwrap_or_default();
            return Err(LlmError::RateLimited {
                retry_after,
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
        let mut stream_resp = StreamResponse::default();

        while let Some(chunk) = bytes.next().await {
            line_buf.extend_from_slice(&chunk?);
            while let Some(pos) = line_buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = line_buf.drain(..=pos).collect();
                handle_sse_line(&line, on_delta, &mut stream_resp)?;
            }
        }

        if !line_buf.is_empty() {
            handle_sse_line(&line_buf, on_delta, &mut stream_resp)?;
        }

        Ok(stream_resp.into_response())
    }

    fn request_builder(&self, req: &ChatRequest) -> reqwest::RequestBuilder {
        let mut builder = self
            .http
            .post(self.cfg.endpoint())
            .bearer_auth(&self.cfg.api_key)
            .json(req);

        if let Some(referer) = &self.cfg.referer {
            builder = builder.header("HTTP-Referer", referer);
        }
        if let Some(title) = &self.cfg.title {
            builder = builder.header("X-Title", title);
        }

        builder
    }
}

#[async_trait]
impl LlmClient for OpenRouterClient {
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
        match self.send_once(req).await {
            // One automatic retry on a polite (<=30s) rate-limit hint.
            Err(LlmError::RateLimited { retry_after, .. }) if retry_after <= 30 => {
                tokio::time::sleep(Duration::from_secs(retry_after)).await;
                self.send_once(req).await
            }
            other => other,
        }
    }

    async fn chat_stream(
        &self,
        req: &ChatRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<ChatResponse> {
        match self.send_stream_once(req, on_delta).await {
            Err(LlmError::RateLimited { retry_after, .. }) if retry_after <= 30 => {
                tokio::time::sleep(Duration::from_secs(retry_after)).await;
                self.send_stream_once(req, on_delta).await
            }
            other => other,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ChatStreamChunk {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<Usage>,
    #[serde(default)]
    error: Option<StreamError>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    #[serde(default)]
    index: u32,
    #[serde(default)]
    delta: StreamDeltaMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct StreamDeltaMessage {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamError {
    message: String,
}

#[derive(Debug, Default)]
struct StreamResponse {
    content: String,
    usage: Option<Usage>,
    id: Option<String>,
    model: Option<String>,
    role: Option<String>,
    finish_reason: Option<String>,
}

impl StreamResponse {
    fn into_response(self) -> ChatResponse {
        ChatResponse {
            id: self.id,
            model: self.model,
            choices: vec![Choice {
                index: 0,
                message: ResponseMessage {
                    role: self.role,
                    content: Some(self.content),
                    tool_calls: None,
                },
                finish_reason: self.finish_reason,
            }],
            usage: self.usage,
        }
    }
}

fn handle_sse_line(
    line: &[u8],
    on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    stream_resp: &mut StreamResponse,
) -> Result<()> {
    let line = String::from_utf8_lossy(line);
    let line = line.trim_end_matches(['\r', '\n']);
    let Some(payload) = line.strip_prefix("data:") else {
        return Ok(());
    };
    let payload = payload.trim_start();
    if payload.is_empty() || payload == "[DONE]" {
        return Ok(());
    }

    let chunk: ChatStreamChunk =
        serde_json::from_str(payload).map_err(|source| LlmError::Parse {
            target: "ChatStreamChunk",
            source,
            raw: payload.to_string(),
        })?;

    if let Some(err) = chunk.error {
        return Err(LlmError::Api {
            status: 0,
            message: err.message,
        });
    }

    if chunk.id.is_some() {
        stream_resp.id = chunk.id;
    }
    if chunk.model.is_some() {
        stream_resp.model = chunk.model;
    }
    if chunk.usage.is_some() {
        stream_resp.usage = chunk.usage;
    }

    for choice in chunk.choices {
        let _ = choice.index;
        if let Some(next_role) = choice.delta.role {
            stream_resp.role = Some(next_role);
        }
        if let Some(next_finish_reason) = choice.finish_reason {
            if next_finish_reason == "error" {
                return Err(LlmError::Api {
                    status: 0,
                    message: "stream ended with finish_reason=error".to_string(),
                });
            }
            stream_resp.finish_reason = Some(next_finish_reason);
        }
        if let Some(delta) = choice.delta.content
            && !delta.is_empty()
        {
            stream_resp.content.push_str(&delta);
            on_delta(&delta);
        }
    }

    Ok(())
}

/// Read the `Retry-After` header as whole seconds, defaulting to 1.
fn parse_retry_after(resp: &reqwest::Response) -> u64 {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_line_appends_content_and_final_usage() {
        let mut seen = String::new();
        let mut on_delta = |delta: &str| seen.push_str(delta);
        let mut stream_resp = StreamResponse::default();

        handle_sse_line(
            br#"data: {"id":"cmpl_1","model":"test/model","choices":[{"index":0,"delta":{"role":"assistant","content":"abc"},"finish_reason":null}],"usage":null}"#,
            &mut on_delta,
            &mut stream_resp,
        )
        .unwrap();
        handle_sse_line(
            br#"data: {"choices":[],"usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3,"cost":0.004}}"#,
            &mut on_delta,
            &mut stream_resp,
        )
        .unwrap();

        assert_eq!(seen, "abc");
        assert_eq!(stream_resp.content, "abc");
        assert_eq!(stream_resp.id.as_deref(), Some("cmpl_1"));
        assert_eq!(stream_resp.model.as_deref(), Some("test/model"));
        assert_eq!(stream_resp.role.as_deref(), Some("assistant"));
        assert_eq!(stream_resp.usage.expect("final usage").total_tokens, 3);
    }
}
