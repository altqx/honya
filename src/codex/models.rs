//! Fetch Codex model ids for the Settings picker, with static fallbacks on failure.

use std::time::Duration;

use super::CodexAuth;
use crate::model::{AppEvent, EventTx, LogLevel};

const MODELS_URL: &str = "https://chatgpt.com/backend-api/codex/models";
/// Backend-required client version, mirrored from Codex CLI.
const CLIENT_VERSION: &str = "0.141.0";

/// Fetch models and log fallback reasons for the activity log.
pub fn spawn_fetch_models(auth: CodexAuth, tx: EventTx) {
    tokio::spawn(async move {
        match fetch_models(&auth).await {
            Ok(models) if !models.is_empty() => {
                tx.send(AppEvent::Log {
                    level: LogLevel::Info,
                    msg: format!("Codex models: {}", models.join(", ")),
                });
                tx.send(AppEvent::CodexModels { models });
            }
            Ok(_) => tx.send(AppEvent::Log {
                level: LogLevel::Warn,
                msg: "Codex models: backend returned an empty list (using built-in fallback)"
                    .to_string(),
            }),
            Err(e) => tx.send(AppEvent::Log {
                level: LogLevel::Warn,
                msg: format!("Codex models fetch failed (using built-in fallback): {e}"),
            }),
        }
    });
}

pub async fn fetch_models(auth: &CodexAuth) -> anyhow::Result<Vec<String>> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("honya/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let resp = http
        .get(format!("{MODELS_URL}?client_version={CLIENT_VERSION}"))
        .bearer_auth(&auth.access_token)
        .header("chatgpt-account-id", &auth.account_id)
        .send()
        .await?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    anyhow::ensure!(
        status.is_success(),
        "{status}: {}",
        body.chars().take(200).collect::<String>()
    );
    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| anyhow::anyhow!("bad JSON: {e}"))?;
    Ok(parse_models(&json))
}

/// Accept both `{data:[{id}]}` and `{models:[{id|slug}]}` response shapes.
fn parse_models(v: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    for key in ["data", "models"] {
        let Some(arr) = v.get(key).and_then(|x| x.as_array()) else {
            continue;
        };
        for m in arr {
            let id = m
                .get("id")
                .or_else(|| m.get("slug"))
                .or_else(|| m.get("model"))
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty());
            if let Some(id) = id {
                let id = id.to_string();
                if !out.contains(&id) {
                    out.push(id);
                }
            }
        }
        if !out.is_empty() {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_data_and_models_shapes() {
        let data = serde_json::json!({"data":[{"id":"gpt-5"},{"id":"gpt-5-codex"},{"id":"gpt-5"}]});
        assert_eq!(parse_models(&data), vec!["gpt-5", "gpt-5-codex"]);
        let models = serde_json::json!({"models":[{"slug":"gpt-5.5"},{"id":"gpt-5"}]});
        assert_eq!(parse_models(&models), vec!["gpt-5.5", "gpt-5"]);
        assert!(parse_models(&serde_json::json!({"nope":1})).is_empty());
    }
}
