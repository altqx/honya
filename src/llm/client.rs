//! src/llm/client.rs — the live OpenRouter HTTP client, the `LlmClient` trait,
//! the crate-shared `LlmError`, and configuration/env wiring.
//!
//! `LlmError` is `pub` + `thiserror` so `error.rs` can `#[from]` it. The module
//! also defines `pub type Result<T> = std::result::Result<T, LlmError>`, which
//! shadows std inside this module — callers in `mod.rs` already account for that
//! by spelling out `std::result::Result` where they need the real std type.

use std::time::Duration;

use async_trait::async_trait;
use reqwest::StatusCode;

use crate::config;
use crate::model::AppConfig;

use super::{ChatRequest, ChatResponse};

// ============================================================================
// ERROR
// ============================================================================

/// All failure modes of the LLM layer.
#[derive(thiserror::Error, Debug)]
pub enum LlmError {
    /// No API key was discovered and the live client was nonetheless requested.
    #[error("no OpenRouter API key found (set HONYA_API_KEY or OPENROUTER_API_KEY)")]
    MissingApiKey,

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

// ============================================================================
// CONFIG
// ============================================================================

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
    /// Build a config from the persisted [`AppConfig`] + the discovered API key.
    ///
    /// Reads `base_url`/`referer`/`title` from the saved config (or its defaults)
    /// and the bearer token from [`config::api_key`]. Errors with
    /// [`LlmError::MissingApiKey`] when no key is set.
    pub fn from_env() -> Result<Self> {
        let cfg = config::load();
        let api_key = config::api_key().ok_or(LlmError::MissingApiKey)?;
        Ok(Self::from_app_config(&cfg, api_key))
    }

    /// Build a config from an explicit [`AppConfig`] + key (no env/file reads).
    pub fn from_app_config(cfg: &AppConfig, api_key: String) -> Self {
        Self {
            base_url: cfg.base_url.clone(),
            api_key,
            referer: cfg.referer.clone(),
            title: cfg.title.clone(),
            timeout: Duration::from_secs(120),
        }
    }

    /// The fully-qualified chat-completions endpoint.
    fn endpoint(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!("{base}/chat/completions")
    }
}

// ============================================================================
// CLIENT TRAIT
// ============================================================================

/// The single capability every backend (live or mock) exposes: one chat call.
#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse>;
}

// ============================================================================
// OPENROUTER CLIENT
// ============================================================================

/// Live OpenRouter chat client over reqwest + rustls.
pub struct OpenRouterClient {
    http: reqwest::Client,
    cfg: ClientConfig,
}

impl OpenRouterClient {
    /// Build a client from an explicit [`ClientConfig`].
    pub fn new(cfg: ClientConfig) -> Result<Self> {
        let http = reqwest::Client::builder().timeout(cfg.timeout).build()?;
        Ok(Self { http, cfg })
    }

    /// Build a client from env/config discovery (see [`ClientConfig::from_env`]).
    pub fn from_env() -> Result<Self> {
        Self::new(ClientConfig::from_env()?)
    }

    /// Issue one POST and classify the response into `Result<ChatResponse>`.
    ///
    /// On a non-success status this reads the body text for the error message
    /// and, for 429, extracts the `Retry-After` header seconds. The buffered
    /// body is consumed by `text()`, so success is parsed from that same string.
    async fn send_once(&self, req: &ChatRequest) -> Result<ChatResponse> {
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
}

/// Read the `Retry-After` header as whole seconds, defaulting to 1.
fn parse_retry_after(resp: &reqwest::Response) -> u64 {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(1)
}
