//! TypeSafe **System One** (Jev) decisions transport.
//!
//! A System One model does not generate text: it takes a *state* plus typed
//! *questions* and answers with typed decisions carrying calibrated
//! probabilities. That is a different wire contract from chat/completions, so
//! this deliberately does **not** implement [`LlmClient`](super::client::LlmClient)
//! — a decisions backend can never stand in for a translator.
//!
//! OpenRouter and TypeSafe speak the same request/response shape and differ only
//! in URL, key and model id, so one client serves both (the same arrangement as
//! OpenRouter/Tokenrouter sharing `OpenRouterClient`). Note the OpenRouter route
//! sits under `/api/`, not `/api/v1/`, so it cannot be derived from
//! `OPENROUTER_BASE_URL`.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use super::Usage;
use super::client::{
    LlmError, Result, RetryPolicy, parse_error_envelope, parse_retry_after, retry_after_hint,
};

pub const OPENROUTER_DECISIONS_URL: &str = "https://openrouter.ai/api/alpha/decisions";
pub const TYPESAFE_SYSTEMONE_URL: &str = "https://api.typesafe.ai/v1/systemone";

/// Send attempts (initial + retries) per decisions call. Deliberately shallower
/// than the chat client's: the gate exists to be fast, and its fallback (the LLM
/// reviewer) is right there, so spending a long retry ladder to save one
/// reviewer call is a bad trade. One retry absorbs a single blip; anything
/// worse defers. The configured budget still applies when it is *shallower*
/// than this, and the configured cooldown always does.
const MAX_SEND_ATTEMPTS: u32 = 2;

/// Per-request timeout. Jev answers in 70-500ms, so this is ~30x the expected
/// worst case and still bounds how long the gate can delay a chunk.
pub const DECISIONS_TIMEOUT: Duration = Duration::from_secs(15);

/// One typed question. Serializes internally tagged, e.g.
/// `{"type":"noul","instructions":"…"}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    /// Pick one option from a named set; `criteria` maps option → description.
    Choice {
        instructions: String,
        criteria: BTreeMap<String, String>,
    },
    /// Rate against ordered levels; the answer indexes into `criteria`.
    Score {
        instructions: String,
        criteria: Vec<String>,
    },
    /// A yes/no question answered with the probability of "yes".
    Noul { instructions: String },
}

impl Question {
    pub fn choice(instructions: impl Into<String>, criteria: &[(&str, &str)]) -> Self {
        Question::Choice {
            instructions: instructions.into(),
            criteria: criteria
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        }
    }

    pub fn score(instructions: impl Into<String>, levels: &[&str]) -> Self {
        Question::Score {
            instructions: instructions.into(),
            criteria: levels.iter().map(|l| (*l).to_string()).collect(),
        }
    }

    pub fn noul(instructions: impl Into<String>) -> Self {
        Question::Noul {
            instructions: instructions.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DecisionsRequest {
    pub model: String,
    /// A plain string, or a JSON object/array of related context.
    pub state: serde_json::Value,
    pub questions: BTreeMap<String, Question>,
}

/// One typed answer. Unknown fields (`legend`, `probabilities` on a score) are
/// ignored; only what the gate actually reads is declared.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    Choice {
        choice: String,
        #[serde(default)]
        confidence: Option<f64>,
    },
    Score {
        score: f64,
        #[serde(default)]
        confidence: Option<f64>,
    },
    /// The probability that the answer is "yes". Carries **no** `confidence`
    /// field, unlike the other two primitives.
    Noul { noul: f64 },
}

impl Answer {
    /// Confidence in `0.0..=1.0`. A Noul answer reports no confidence, so its
    /// distance from a coin flip stands in for one: 0.5 → 0.0, 0.0/1.0 → 1.0.
    pub fn confidence(&self) -> f64 {
        match self {
            Answer::Choice { confidence, .. } | Answer::Score { confidence, .. } => {
                confidence.unwrap_or(0.0)
            }
            Answer::Noul { noul } => (noul - 0.5).abs() * 2.0,
        }
    }

    /// The selected option, for a Choice answer.
    pub fn as_choice(&self) -> Option<&str> {
        match self {
            Answer::Choice { choice, .. } => Some(choice.as_str()),
            _ => None,
        }
    }

    /// The (fractional) level index, for a Score answer.
    pub fn as_score(&self) -> Option<f64> {
        match self {
            Answer::Score { score, .. } => Some(*score),
            _ => None,
        }
    }

    /// The probability of "yes", for a Noul answer.
    pub fn as_noul(&self) -> Option<f64> {
        match self {
            Answer::Noul { noul } => Some(*noul),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DecisionsResponse {
    #[serde(default)]
    pub model: String,
    pub answers: BTreeMap<String, Answer>,
    #[serde(default)]
    pub usage: DecisionsUsage,
}

/// `Option<f64>` for `cost` so both a missing field (TypeSafe) and an explicit
/// `null` decode to zero.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct DecisionsUsage {
    #[serde(default)]
    pub input_tokens: u32,
    #[serde(default)]
    pub output_tokens: u32,
    #[serde(default)]
    pub cost: Option<f64>,
}

impl DecisionsUsage {
    /// Map onto the chat-layer [`Usage`] so gate calls fold into the same running
    /// totals and `UsageUpdate` events as every other agent.
    pub fn to_usage(self) -> Usage {
        Usage {
            prompt_tokens: self.input_tokens,
            completion_tokens: self.output_tokens,
            total_tokens: self.input_tokens.saturating_add(self.output_tokens),
            cost: self.cost.unwrap_or(0.0),
            cost_details: None,
            served_tier: None,
        }
    }
}

/// A ready-to-use System One: the backend paired with the settings that govern
/// it. `Option<SystemOneHandle>` is how every caller carries "a judgement is
/// available, or it is not" — `None` always means run the deterministic path.
#[derive(Clone)]
pub struct SystemOneHandle {
    pub backend: std::sync::Arc<dyn DecisionsBackend>,
    pub config: crate::model::SystemOne,
}

impl SystemOneHandle {
    pub fn new(
        backend: Option<std::sync::Arc<dyn DecisionsBackend>>,
        config: &crate::model::SystemOne,
    ) -> Option<Self> {
        Some(Self {
            backend: backend?,
            config: config.clone(),
        })
    }
}

/// A System One backend. Separate from `LlmClient` because the two wire
/// contracts have nothing in common, and so tests can inject a fake.
#[async_trait]
pub trait DecisionsBackend: Send + Sync {
    async fn decide(&self, req: &DecisionsRequest) -> Result<DecisionsResponse>;
}

/// Live decisions client over reqwest + rustls.
pub struct DecisionsClient {
    http: reqwest::Client,
    url: String,
    api_key: String,
    retry: RetryPolicy,
}

impl DecisionsClient {
    pub fn new(
        url: impl Into<String>,
        api_key: String,
        timeout: Duration,
        retry: RetryPolicy,
    ) -> Result<Self> {
        let http = reqwest::Client::builder().timeout(timeout).build()?;
        Ok(Self {
            http,
            url: url.into(),
            api_key,
            retry: RetryPolicy {
                max_attempts: retry.max_attempts.min(MAX_SEND_ATTEMPTS),
                ..retry
            },
        })
    }

    async fn send_once(&self, req: &DecisionsRequest) -> Result<DecisionsResponse> {
        let resp = self
            .http
            .post(&self.url)
            .bearer_auth(&self.api_key)
            .json(req)
            .send()
            .await?;
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
        // Same 200-with-error-envelope shape as the chat route.
        if let Some(err) = parse_error_envelope(&raw) {
            return Err(err);
        }
        serde_json::from_str(&raw).map_err(|source| LlmError::Parse {
            target: "DecisionsResponse",
            source,
            raw,
        })
    }
}

#[async_trait]
impl DecisionsBackend for DecisionsClient {
    async fn decide(&self, req: &DecisionsRequest) -> Result<DecisionsResponse> {
        let mut sent = 0u32;
        loop {
            sent += 1;
            match self.send_once(req).await {
                Err(e) if self.retry.should_retry(&e, sent) => {
                    tokio::time::sleep(self.retry.backoff(sent, retry_after_hint(&e))).await;
                }
                other => return other,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_request() -> DecisionsRequest {
        DecisionsRequest {
            model: "typesafe/jev-1.13".to_string(),
            state: serde_json::json!({ "source_jp": "猫", "translation": "cat" }),
            questions: BTreeMap::from([
                (
                    "verdict".to_string(),
                    Question::choice(
                        "Approve or revise?",
                        &[("approve", "Faithful."), ("revise", "Has errors.")],
                    ),
                ),
                (
                    "fluency".to_string(),
                    Question::score("How natural?", &["Awkward", "Stiff", "Natural"]),
                ),
                (
                    "residue".to_string(),
                    Question::noul("Untranslated Japanese remains."),
                ),
            ]),
        }
    }

    #[test]
    fn request_serializes_to_the_documented_body() {
        let v = serde_json::to_value(sample_request()).unwrap();
        assert_eq!(v["model"], "typesafe/jev-1.13");
        assert_eq!(v["state"]["translation"], "cat");

        let verdict = &v["questions"]["verdict"];
        assert_eq!(verdict["type"], "choice");
        assert_eq!(verdict["instructions"], "Approve or revise?");
        assert_eq!(verdict["criteria"]["approve"], "Faithful.");

        let fluency = &v["questions"]["fluency"];
        assert_eq!(fluency["type"], "score");
        assert_eq!(
            fluency["criteria"],
            serde_json::json!(["Awkward", "Stiff", "Natural"]),
            "score criteria must stay an ordered array"
        );

        let residue = &v["questions"]["residue"];
        assert_eq!(residue["type"], "noul");
        assert!(
            residue.get("criteria").is_none(),
            "noul criteria is optional and must be omitted, not sent as null"
        );
    }

    /// The exact body the live OpenRouter endpoint returned during development.
    #[test]
    fn openrouter_response_decodes_including_cost() {
        let raw = r#"{"model":"typesafe/jev-1.13-20260917","answers":{
            "accuracy":{"type":"noul","noul":0.97},
            "verdict":{"type":"choice","choice":"approve","probabilities":{"revise":0,"approve":1},"confidence":1},
            "fluency":{"type":"score","score":1.99,"legend":{"0":"a","1":"b","2":"c"},"probabilities":{"0":0,"1":0.01,"2":0.99},"confidence":0.98}},
            "usage":{"input_tokens":426,"output_tokens":64,"cost":0.000017892},
            "id":"gen-dec-1","provider":"TypeSafe"}"#;
        let resp: DecisionsResponse = serde_json::from_str(raw).unwrap();

        assert_eq!(resp.model, "typesafe/jev-1.13-20260917");
        assert_eq!(resp.answers["verdict"].as_choice(), Some("approve"));
        assert_eq!(resp.answers["fluency"].as_score(), Some(1.99));
        assert_eq!(resp.answers["accuracy"].as_noul(), Some(0.97));

        let usage = resp.usage.to_usage();
        assert_eq!(usage.prompt_tokens, 426);
        assert_eq!(usage.completion_tokens, 64);
        assert_eq!(usage.total_tokens, 490);
        assert!((usage.cost - 0.000017892).abs() < f64::EPSILON);
    }

    /// TypeSafe's native response omits `cost` entirely.
    #[test]
    fn typesafe_response_without_cost_decodes_to_zero() {
        let raw = r#"{"model":"jev-latest","answers":{"is_urgent":{"type":"noul","noul":0.999}},
            "usage":{"input_tokens":312,"output_tokens":48}}"#;
        let resp: DecisionsResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(resp.usage.to_usage().cost, 0.0);
        assert_eq!(resp.answers["is_urgent"].as_noul(), Some(0.999));
    }

    #[test]
    fn noul_confidence_is_distance_from_a_coin_flip() {
        let certain_yes = Answer::Noul { noul: 1.0 };
        let certain_no = Answer::Noul { noul: 0.0 };
        let coin_flip = Answer::Noul { noul: 0.5 };
        assert!((certain_yes.confidence() - 1.0).abs() < 1e-9);
        assert!((certain_no.confidence() - 1.0).abs() < 1e-9);
        assert!(coin_flip.confidence().abs() < 1e-9);
    }

    #[test]
    fn choice_answer_without_confidence_reports_zero() {
        // `confidence` is optional in the schema; absent must not read as certain.
        let a: Answer = serde_json::from_str(r#"{"type":"choice","choice":"approve"}"#).unwrap();
        assert_eq!(a.confidence(), 0.0);
    }
}
