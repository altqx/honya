//! OpenRouter client, `LlmClient`, and `LlmError`.
//!
//! The `Result<T>` alias here shadows `std::result::Result` within `crate::llm::*`.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::StatusCode;
use serde::Deserialize;

use crate::model::{AgentModel, AppConfig, Provider, ServiceTier};

pub const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";
pub const TOKENROUTER_BASE_URL: &str = "https://api.tokenrouter.com/v1";

use super::{ChatRequest, ChatResponse, Choice, FunctionCall, ResponseMessage, ToolCall, Usage};

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

    /// A choice came back with empty/`null` content — usually a reasoning model
    /// hitting `finish_reason=length` (its whole token budget went to hidden
    /// reasoning before any answer). Distinct from [`Self::Parse`] so the message
    /// reads as "empty content", not a bogus JSON error on an empty string.
    #[error("the model returned empty content for {target} (finish_reason={finish_reason})")]
    EmptyContent {
        target: &'static str,
        finish_reason: String,
    },

    /// A structured response failed to deserialize into the target type.
    #[error("failed to parse {target}: {source} — raw: {raw}")]
    Parse {
        target: &'static str,
        #[source]
        source: serde_json::Error,
        raw: String,
    },
}

impl LlmError {
    /// True when this is a provider **content-policy block** rather than a real
    /// transport/HTTP fault — e.g. Gemini's `PROHIBITED_CONTENT`/`SAFETY`/`RECITATION`
    /// finish reasons, which OpenRouter surfaces as an `Api`/stream error (often
    /// `status 0`). These won't clear on a verbatim replay, so the pipeline retries
    /// them with a de-escalation prompt instead of the generic "retrying" feedback.
    pub fn is_content_policy_block(&self) -> bool {
        let LlmError::Api { message, .. } = self else {
            return false;
        };
        let m = message.to_ascii_uppercase();
        [
            "PROHIBITED_CONTENT",
            "SAFETY",
            "RECITATION",
            "BLOCKLIST",
            "CONTENT_POLICY",
            "CONTENT POLICY",
            "CONTENT_FILTER",
            "CONTENT FILTER",
        ]
        .iter()
        .any(|needle| m.contains(needle))
    }

    /// True for a **transient** error injected into the SSE stream — e.g.
    /// OpenRouter's "JSON error injected into SSE stream" or a mid-stream
    /// `finish_reason=error` from a flaky upstream provider. These carry the
    /// synthetic `status 0` (a real HTTP fault has a non-zero status) and clear on
    /// a verbatim replay, so [`OpenRouterClient::chat_stream`] retries them once
    /// before the failure reaches the pipeline. A content-policy block also has
    /// `status 0` but is explicitly excluded — it needs the pipeline's
    /// de-escalation retry, not a verbatim replay.
    pub fn is_transient_stream_error(&self) -> bool {
        matches!(self, LlmError::Api { status: 0, .. }) && !self.is_content_policy_block()
    }

    /// True for a no-content `finish_reason=length`; replaying usually fails the same way.
    pub fn is_length_truncation(&self) -> bool {
        matches!(
            self,
            LlmError::EmptyContent { finish_reason, .. } if finish_reason == "length"
        )
    }

    /// True for a fault a verbatim replay might clear — the classifier the
    /// transport-level retry loop uses. Excludes deterministic failures (other
    /// `4xx`, content-policy blocks, empty/parse results). Absorbing these here
    /// keeps the flex tier's frequent transient faults from each burning a
    /// per-chunk pipeline attempt.
    pub fn is_retryable(&self) -> bool {
        match self {
            LlmError::RateLimited { .. } | LlmError::Transport(_) => true,
            LlmError::Api { status: 0, .. } => self.is_transient_stream_error(),
            LlmError::Api { status, .. } => {
                matches!(
                    status,
                    408 | 425 | 429 | 500 | 502 | 503 | 504 | 520..=524 | 529
                )
            }
            LlmError::EmptyChoices | LlmError::EmptyContent { .. } | LlmError::Parse { .. } => {
                false
            }
        }
    }
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
    /// `service_tier` stamped onto every request when set (`flex`/`priority`).
    pub service_tier: Option<ServiceTier>,
    /// Per-request timeout.
    pub timeout: Duration,
}

impl ClientConfig {
    /// Build a config for an explicit endpoint + key, inheriting the ranking
    /// headers, service tier, and timeout from an [`AppConfig`]. Used to point the
    /// same OpenAI-compatible client at OpenRouter vs Tokenrouter.
    pub fn for_endpoint(cfg: &AppConfig, base_url: impl Into<String>, api_key: String) -> Self {
        Self {
            base_url: base_url.into(),
            api_key,
            referer: cfg.referer.clone(),
            title: cfg.title.clone(),
            service_tier: cfg.service_tier,
            timeout: Duration::from_secs(120),
        }
    }

    /// Build a config from an [`AppConfig`] + the key resolved once at startup,
    /// pointed at the fixed OpenRouter endpoint.
    pub fn from_app_config(cfg: &AppConfig, api_key: String) -> Self {
        Self::for_endpoint(cfg, OPENROUTER_BASE_URL, api_key)
    }

    fn endpoint(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!("{base}/chat/completions")
    }

    /// Send attempts (initial + retries) per chat call. Flex runs on spare,
    /// deprioritized capacity and faults far more often, so it gets a deeper budget.
    fn max_send_attempts(&self) -> u32 {
        match self.service_tier {
            Some(ServiceTier::Flex) => 5,
            _ => 3,
        }
    }
}

/// Backoff before the `retry`-th retry (1-based): exponential from 1s, capped at
/// 20s. A `Retry-After` hint wins but is still capped, so one chunk can't stall
/// the run for minutes on a single rate-limit reset.
fn retry_backoff(retry: u32, retry_after: Option<u64>) -> Duration {
    const MAX_BACKOFF: u64 = 20;
    let secs = match retry_after {
        Some(hint) => hint.min(MAX_BACKOFF),
        None => (1u64 << (retry.saturating_sub(1)).min(5)).min(MAX_BACKOFF),
    };
    Duration::from_secs(secs)
}

fn retry_after_hint(err: &LlmError) -> Option<u64> {
    match err {
        LlmError::RateLimited { retry_after, .. } => Some(*retry_after),
        _ => None,
    }
}

/// Live clients keyed by [`Provider`], built once per run from the resolved keys.
/// Each agent routes to the client for its configured provider.
#[derive(Clone, Default)]
pub struct ClientSet {
    openrouter: Option<Arc<dyn LlmClient>>,
    tokenrouter: Option<Arc<dyn LlmClient>>,
}

impl ClientSet {
    pub fn build(cfg: &AppConfig) -> Result<Self> {
        let openrouter = match crate::config::resolve_api_key(cfg) {
            Some(key) => Some(
                Arc::new(OpenRouterClient::new(ClientConfig::from_app_config(
                    cfg, key,
                ))?) as Arc<dyn LlmClient>,
            ),
            None => None,
        };
        let tokenrouter = match crate::config::resolve_tokenrouter_key(cfg) {
            Some(key) => Some(Arc::new(OpenRouterClient::new(ClientConfig::for_endpoint(
                cfg,
                TOKENROUTER_BASE_URL,
                key,
            ))?) as Arc<dyn LlmClient>),
            None => None,
        };
        Ok(Self {
            openrouter,
            tokenrouter,
        })
    }

    /// The client for a provider, or `None` when that provider has no key
    /// configured (Codex is always `None` until its auth flow lands).
    pub fn for_provider(&self, provider: Provider) -> Option<Arc<dyn LlmClient>> {
        match provider {
            Provider::OpenRouter => self.openrouter.clone(),
            Provider::Tokenrouter => self.tokenrouter.clone(),
            Provider::Codex => None,
        }
    }

    /// The client for an agent's configured provider.
    pub fn for_agent(&self, agent: &AgentModel) -> Option<Arc<dyn LlmClient>> {
        self.for_provider(agent.provider)
    }

    /// True when no provider has a key configured (fully unconfigured).
    pub fn is_empty(&self) -> bool {
        self.openrouter.is_none() && self.tokenrouter.is_none()
    }

    /// A set with a single client serving the default (OpenRouter) provider.
    /// Convenient for tests and one-client setups.
    pub fn single(client: Arc<dyn LlmClient>) -> Self {
        Self {
            openrouter: Some(client),
            tokenrouter: None,
        }
    }

    /// Override a single provider's client (used by tests to inject a mock).
    #[cfg(test)]
    pub fn with_provider(mut self, provider: Provider, client: Arc<dyn LlmClient>) -> Self {
        match provider {
            Provider::OpenRouter => self.openrouter = Some(client),
            Provider::Tokenrouter => self.tokenrouter = Some(client),
            Provider::Codex => {}
        }
        self
    }
}

/// A streamed fragment: visible answer text or provider-surfaced reasoning.
#[derive(Debug, Clone, Copy)]
pub enum StreamDelta<'a> {
    Content(&'a str),
    Reasoning(&'a str),
}

/// The single capability every backend (live or mock) exposes: one chat call.
#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse>;

    async fn chat_stream(
        &self,
        req: &ChatRequest,
        on_delta: &mut (dyn for<'a> FnMut(StreamDelta<'a>) + Send),
    ) -> Result<ChatResponse> {
        let resp = self.chat(req).await?;
        let content = resp
            .choices
            .first()
            .and_then(|choice| choice.message.content.clone())
            .unwrap_or_default();
        if !content.is_empty() {
            on_delta(StreamDelta::Content(&content));
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

        // OpenRouter can answer HTTP 200 with an error envelope ({"error": {…}})
        // rather than a completion — typically an upstream provider failure. Detect
        // that shape so it surfaces as a real `Api` error (and is classified for the
        // content-policy / transient retry paths) instead of an opaque "missing
        // field `choices`" parse failure. Mirrors the SSE path's `chunk.error`.
        if let Some(err) = parse_error_envelope(&raw) {
            return Err(err);
        }

        let mut parsed: ChatResponse =
            serde_json::from_str(&raw).map_err(|source| LlmError::Parse {
                target: "ChatResponse",
                source,
                raw,
            })?;

        if parsed.choices.is_empty() {
            return Err(LlmError::EmptyChoices);
        }

        // Carry the top-level service-tier echo on the usage record, which is the
        // only part of the response the agents pass back up to the pipeline.
        if let Some(usage) = parsed.usage.as_mut()
            && usage.served_tier.is_none()
        {
            usage.served_tier = parsed.service_tier;
        }

        Ok(parsed)
    }

    async fn send_stream_once(
        &self,
        req: &ChatRequest,
        on_delta: &mut (dyn for<'a> FnMut(StreamDelta<'a>) + Send),
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
        let post = self
            .http
            .post(self.cfg.endpoint())
            .bearer_auth(&self.cfg.api_key);

        // Stamp the configured service tier unless the caller already set one.
        let mut builder = match (self.cfg.service_tier, req.service_tier) {
            (Some(tier), None) => {
                let mut req = req.clone();
                req.service_tier = Some(tier);
                post.json(&req)
            }
            _ => post.json(req),
        };

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
        // Track whether any delta reached the caller: a stream error after partial
        // output can't be replayed here without double-feeding the field-stream
        // parser, so that case is left to the pipeline (the partial-stream path).
        let emitted = std::sync::atomic::AtomicBool::new(false);
        let mut tracked = |delta: StreamDelta| {
            emitted.store(true, Ordering::Relaxed);
            on_delta(delta);
        };
        let max = self.cfg.max_send_attempts();
        let mut retry = 0u32;
        loop {
            match self.send_stream_once(req, &mut tracked).await {
                // Only retry while nothing has reached the caller: replaying after
                // partial output would double-feed the field-stream parser.
                Err(e)
                    if retry + 1 < max && e.is_retryable() && !emitted.load(Ordering::Relaxed) =>
                {
                    retry += 1;
                    tokio::time::sleep(retry_backoff(retry, retry_after_hint(&e))).await;
                }
                other => return other,
            }
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
    #[serde(default, deserialize_with = "super::de_served_tier")]
    service_tier: Option<super::ServedTier>,
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
    /// Provider reasoning; OpenRouter uses `reasoning`, some upstreams use the alias.
    #[serde(default, alias = "reasoning_content")]
    reasoning: Option<String>,
    /// Tool calls arrive as indexed deltas: the first fragment for an index carries
    /// `id` + `function.name`, later fragments append `function.arguments` text.
    #[serde(default)]
    tool_calls: Option<Vec<StreamToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
struct StreamToolCallDelta {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<StreamFnDelta>,
}

#[derive(Debug, Deserialize)]
struct StreamFnDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamError {
    message: String,
}

/// One streamed tool call being accumulated across delta fragments.
#[derive(Debug, Default, Clone)]
struct StreamToolAcc {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Debug, Default)]
struct StreamResponse {
    content: String,
    tool_calls: Vec<StreamToolAcc>,
    usage: Option<Usage>,
    id: Option<String>,
    model: Option<String>,
    role: Option<String>,
    finish_reason: Option<String>,
    service_tier: Option<super::ServedTier>,
}

impl StreamResponse {
    fn into_response(self) -> ChatResponse {
        let mut usage = self.usage;
        if let Some(u) = usage.as_mut()
            && u.served_tier.is_none()
        {
            u.served_tier = self.service_tier;
        }
        // Reassemble accumulated tool-call fragments into whole calls.
        let calls: Vec<ToolCall> = self
            .tool_calls
            .into_iter()
            .enumerate()
            .filter(|(_, a)| !a.name.is_empty() || !a.id.is_empty())
            .map(|(i, a)| ToolCall {
                id: if a.id.is_empty() {
                    format!("call_{i}")
                } else {
                    a.id
                },
                kind: "function".to_string(),
                function: FunctionCall {
                    name: a.name,
                    arguments: if a.arguments.is_empty() {
                        "{}".to_string()
                    } else {
                        a.arguments
                    },
                },
            })
            .collect();
        let tool_calls = (!calls.is_empty()).then_some(calls);
        // Keep `content` null when empty (the wire contract for a tool-call turn).
        let content = (!self.content.is_empty()).then_some(self.content);
        ChatResponse {
            id: self.id,
            model: self.model,
            choices: vec![Choice {
                index: 0,
                message: ResponseMessage {
                    role: self.role,
                    content,
                    tool_calls,
                },
                finish_reason: self.finish_reason,
            }],
            usage,
            service_tier: self.service_tier,
        }
    }
}

fn handle_sse_line(
    line: &[u8],
    on_delta: &mut (dyn for<'a> FnMut(StreamDelta<'a>) + Send),
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
    if chunk.service_tier.is_some() {
        stream_resp.service_tier = chunk.service_tier;
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
        if let Some(reasoning) = choice.delta.reasoning
            && !reasoning.is_empty()
        {
            on_delta(StreamDelta::Reasoning(&reasoning));
        }
        if let Some(delta) = choice.delta.content
            && !delta.is_empty()
        {
            stream_resp.content.push_str(&delta);
            on_delta(StreamDelta::Content(&delta));
        }
        if let Some(deltas) = choice.delta.tool_calls {
            for tc in deltas {
                if stream_resp.tool_calls.len() <= tc.index {
                    stream_resp
                        .tool_calls
                        .resize(tc.index + 1, StreamToolAcc::default());
                }
                let acc = &mut stream_resp.tool_calls[tc.index];
                if let Some(id) = tc.id.filter(|s| !s.is_empty()) {
                    acc.id = id;
                }
                if let Some(f) = tc.function {
                    if let Some(name) = f.name.filter(|s| !s.is_empty()) {
                        acc.name = name;
                    }
                    if let Some(args) = f.arguments {
                        acc.arguments.push_str(&args);
                    }
                }
            }
        }
    }

    Ok(())
}

/// Detect an OpenRouter error envelope (`{"error": {"message", "code", …}}`)
/// returned with an HTTP-success status. Returns `None` for a normal completion
/// (no top-level `error`), so a valid response never false-positives. The `code`
/// becomes the `Api` status when it's a plausible HTTP code, else `0` (treated as
/// a transient stream-style fault, matching the SSE path).
fn parse_error_envelope(raw: &str) -> Option<LlmError> {
    #[derive(Deserialize)]
    struct Envelope {
        error: ErrBody,
    }
    #[derive(Deserialize)]
    struct ErrBody {
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        code: Option<serde_json::Value>,
    }

    let env: Envelope = serde_json::from_str(raw).ok()?;
    let status = env
        .error
        .code
        .as_ref()
        .and_then(|c| match c {
            serde_json::Value::Number(n) => n.as_u64().and_then(|v| u16::try_from(v).ok()),
            serde_json::Value::String(s) => s.trim().parse::<u16>().ok(),
            _ => None,
        })
        .filter(|s| (100..=599).contains(s))
        .unwrap_or(0);
    let message = env
        .error
        .message
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| raw.trim().to_string());
    Some(LlmError::Api { status, message })
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
    use crate::model::{AgentModel, Provider};

    #[test]
    fn for_endpoint_overrides_base_url_and_keeps_config() {
        let cfg = AppConfig::default();
        let cc = ClientConfig::for_endpoint(&cfg, TOKENROUTER_BASE_URL, "k".into());
        assert_eq!(cc.base_url, TOKENROUTER_BASE_URL);
        assert_eq!(cc.api_key, "k");
        assert_eq!(cc.service_tier, cfg.service_tier);
        assert_eq!(cc.referer, cfg.referer);
    }

    #[test]
    fn client_set_routes_by_provider() {
        let mock: Arc<dyn LlmClient> = Arc::new(crate::llm::mock::MockClient::default());
        let set = ClientSet::default().with_provider(Provider::Tokenrouter, mock);
        assert!(set.for_provider(Provider::Tokenrouter).is_some());
        assert!(set.for_provider(Provider::OpenRouter).is_none());
        assert!(
            set.for_provider(Provider::Codex).is_none(),
            "Codex unwired in phase 1"
        );
        assert!(!set.is_empty());
        // for_agent dispatches on the agent's configured provider.
        let agent = AgentModel::new(Provider::Tokenrouter, "m", None);
        assert!(set.for_agent(&agent).is_some());
        assert!(set.for_agent(&AgentModel::openrouter("m")).is_none());
        assert!(ClientSet::default().is_empty());
    }

    #[test]
    fn sse_line_appends_content_and_final_usage() {
        let mut seen = String::new();
        let mut reasoning = String::new();
        let mut on_delta = |delta: StreamDelta| match delta {
            StreamDelta::Content(s) => seen.push_str(s),
            StreamDelta::Reasoning(s) => reasoning.push_str(s),
        };
        let mut stream_resp = StreamResponse::default();

        handle_sse_line(
            br#"data: {"choices":[{"index":0,"delta":{"role":"assistant","reasoning":"hmm "}}]}"#,
            &mut on_delta,
            &mut stream_resp,
        )
        .unwrap();
        handle_sse_line(
            br#"data: {"id":"cmpl_1","model":"test/model","choices":[{"index":0,"delta":{"role":"assistant","content":"abc"},"finish_reason":null}],"usage":null}"#,
            &mut on_delta,
            &mut stream_resp,
        )
        .unwrap();
        handle_sse_line(
            br#"data: {"choices":[],"service_tier":"flex","usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3,"cost":0.004}}"#,
            &mut on_delta,
            &mut stream_resp,
        )
        .unwrap();

        assert_eq!(seen, "abc");
        assert_eq!(reasoning, "hmm ", "reasoning deltas surface separately");
        assert_eq!(
            stream_resp.content, "abc",
            "reasoning is not mixed into content"
        );
        assert_eq!(stream_resp.content, "abc");
        assert_eq!(stream_resp.id.as_deref(), Some("cmpl_1"));
        assert_eq!(stream_resp.model.as_deref(), Some("test/model"));
        assert_eq!(stream_resp.role.as_deref(), Some("assistant"));
        assert_eq!(stream_resp.usage.expect("final usage").total_tokens, 3);

        let resp = stream_resp.into_response();
        assert_eq!(resp.service_tier, Some(crate::llm::ServedTier::Flex));
        assert_eq!(
            resp.usage.expect("usage").served_tier,
            Some(crate::llm::ServedTier::Flex)
        );
    }

    #[test]
    fn sse_stream_accumulates_tool_calls() {
        let mut sink = |_: StreamDelta| {};
        let mut sr = StreamResponse::default();
        let lines: [&[u8]; 4] = [
            br#"data: {"choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call_1","function":{"name":"upsert_character","arguments":"{\"jp_"}}]}}]}"#,
            br#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"name\":\"yuu\"}"}}]}}]}"#,
            br#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            b"data: [DONE]",
        ];
        for l in lines {
            handle_sse_line(l, &mut sink, &mut sr).unwrap();
        }
        let resp = sr.into_response();
        let msg = &resp.choices[0].message;
        let calls = msg.tool_calls.as_ref().expect("tool calls preserved");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].function.name, "upsert_character");
        assert_eq!(calls[0].function.arguments, r#"{"jp_name":"yuu"}"#);
        // A tool-call turn carries no visible text → content is null.
        assert!(msg.content.is_none());
    }

    fn client_with_tier(service_tier: Option<ServiceTier>) -> OpenRouterClient {
        OpenRouterClient::new(ClientConfig {
            base_url: "https://example.test/api/v1".into(),
            api_key: "k".into(),
            referer: None,
            title: None,
            service_tier,
            timeout: Duration::from_secs(1),
        })
        .unwrap()
    }

    fn body_json(client: &OpenRouterClient, req: &ChatRequest) -> serde_json::Value {
        let built = client.request_builder(req).build().unwrap();
        let bytes = built.body().unwrap().as_bytes().unwrap();
        serde_json::from_slice(bytes).unwrap()
    }

    #[test]
    fn configured_service_tier_is_stamped_onto_requests() {
        let client = client_with_tier(Some(ServiceTier::Flex));
        let json = body_json(&client, &ChatRequest::new("m", vec![]));
        assert_eq!(json["service_tier"], "flex");
    }

    #[test]
    fn caller_service_tier_overrides_config() {
        let client = client_with_tier(Some(ServiceTier::Flex));
        let req = ChatRequest {
            service_tier: Some(ServiceTier::Priority),
            ..ChatRequest::new("m", vec![])
        };
        assert_eq!(body_json(&client, &req)["service_tier"], "priority");
    }

    #[test]
    fn content_policy_block_detected_across_provider_phrasings() {
        for msg in [
            "PROHIBITED_CONTENT",
            "blocked: SAFETY",
            "finish_reason=recitation",
            "request violates our content policy",
            "content_filter triggered",
        ] {
            let err = LlmError::Api {
                status: 0,
                message: msg.to_string(),
            };
            assert!(err.is_content_policy_block(), "should flag: {msg}");
        }
    }

    #[test]
    fn injected_sse_error_is_transient_but_not_a_policy_block() {
        let err = LlmError::Api {
            status: 0,
            message: "JSON error injected into SSE stream".to_string(),
        };
        assert!(err.is_transient_stream_error());
        assert!(!err.is_content_policy_block());
    }

    #[test]
    fn policy_block_and_real_http_errors_are_not_transient() {
        // A content-policy block carries status 0 too, but must NOT be replayed
        // verbatim — it routes to the pipeline's de-escalation retry instead.
        assert!(
            !LlmError::Api {
                status: 0,
                message: "PROHIBITED_CONTENT".into(),
            }
            .is_transient_stream_error()
        );
        // A real HTTP fault has a non-zero status.
        assert!(
            !LlmError::Api {
                status: 503,
                message: "service unavailable".into(),
            }
            .is_transient_stream_error()
        );
    }

    #[test]
    fn error_envelope_on_http_200_becomes_api_error() {
        // OpenRouter answers 200 with an error envelope on an upstream failure;
        // the numeric `code` carries through as the Api status.
        let err =
            parse_error_envelope(r#"{"error":{"message":"Provider returned error","code":502}}"#)
                .expect("envelope should be detected");
        match err {
            LlmError::Api { status, message } => {
                assert_eq!(status, 502);
                assert_eq!(message, "Provider returned error");
            }
            other => panic!("expected Api, got {other:?}"),
        }
    }

    #[test]
    fn error_envelope_carries_content_policy_classification() {
        let err = parse_error_envelope(r#"{"error":{"message":"PROHIBITED_CONTENT"}}"#)
            .expect("envelope should be detected");
        assert!(err.is_content_policy_block());
    }

    #[test]
    fn error_envelope_without_usable_code_is_transient_status_zero() {
        let err = parse_error_envelope(r#"{"error":{"message":"upstream hiccup","code":"foo"}}"#)
            .expect("envelope should be detected");
        assert!(err.is_transient_stream_error());
    }

    #[test]
    fn normal_completion_is_not_an_error_envelope() {
        assert!(
            parse_error_envelope(
                r#"{"choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#,
            )
            .is_none()
        );
        // A literal `error: null` is not an envelope either.
        assert!(parse_error_envelope(r#"{"error":null,"choices":[]}"#).is_none());
    }

    #[test]
    fn transport_and_plain_api_errors_are_not_policy_blocks() {
        assert!(
            !LlmError::Api {
                status: 500,
                message: "internal server error".into(),
            }
            .is_content_policy_block()
        );
        assert!(
            !LlmError::EmptyChoices.is_content_policy_block(),
            "non-Api errors are never policy blocks"
        );
    }

    #[test]
    fn no_service_tier_field_when_unconfigured() {
        let client = client_with_tier(None);
        let json = body_json(&client, &ChatRequest::new("m", vec![]));
        assert!(json.get("service_tier").is_none());
    }

    #[test]
    fn transient_faults_are_retryable() {
        for status in [408, 425, 429, 500, 502, 503, 504, 520, 524, 529] {
            assert!(
                LlmError::Api {
                    status,
                    message: "x".into(),
                }
                .is_retryable(),
                "status {status} should be retryable"
            );
        }
        assert!(
            LlmError::RateLimited {
                retry_after: 99,
                message: "x".into(),
            }
            .is_retryable()
        );
        assert!(
            LlmError::Api {
                status: 0,
                message: "JSON error injected into SSE stream".into(),
            }
            .is_retryable()
        );
    }

    #[test]
    fn deterministic_faults_are_not_retryable() {
        // 4xx client errors, content-policy blocks, and empty/parse failures
        // will never clear on a verbatim replay.
        for status in [400, 401, 403, 404, 422] {
            assert!(
                !LlmError::Api {
                    status,
                    message: "x".into(),
                }
                .is_retryable(),
                "status {status} should not be retryable"
            );
        }
        assert!(
            !LlmError::Api {
                status: 0,
                message: "PROHIBITED_CONTENT".into(),
            }
            .is_retryable(),
            "content-policy block must route to the pipeline, not a verbatim replay"
        );
        assert!(!LlmError::EmptyChoices.is_retryable());
        assert!(
            !LlmError::EmptyContent {
                target: "translator",
                finish_reason: "length".into(),
            }
            .is_retryable()
        );
    }

    #[test]
    fn flex_tier_gets_a_deeper_retry_budget() {
        assert_eq!(
            client_with_tier(Some(ServiceTier::Flex))
                .cfg
                .max_send_attempts(),
            5
        );
        assert_eq!(
            client_with_tier(Some(ServiceTier::Priority))
                .cfg
                .max_send_attempts(),
            3
        );
        assert_eq!(client_with_tier(None).cfg.max_send_attempts(), 3);
    }

    #[test]
    fn backoff_is_exponential_capped_and_honors_retry_after() {
        // Exponential from 1s, doubling, capped at 20s.
        assert_eq!(retry_backoff(1, None), Duration::from_secs(1));
        assert_eq!(retry_backoff(2, None), Duration::from_secs(2));
        assert_eq!(retry_backoff(3, None), Duration::from_secs(4));
        assert_eq!(retry_backoff(4, None), Duration::from_secs(8));
        assert_eq!(retry_backoff(9, None), Duration::from_secs(20));
        // A server hint takes precedence but is still capped.
        assert_eq!(retry_backoff(1, Some(3)), Duration::from_secs(3));
        assert_eq!(retry_backoff(1, Some(600)), Duration::from_secs(20));
    }
}
