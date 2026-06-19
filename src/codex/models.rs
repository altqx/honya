//! Fetch Codex model ids for the Settings picker, with static fallbacks on failure.

use std::time::Duration;

use super::CodexAuth;
use crate::model::{AppEvent, EventTx};

const MODELS_URL: &str = "https://chatgpt.com/backend-api/codex/models";
/// The backend requires a `client_version`; this mirrors a recent Codex CLI.
const CLIENT_VERSION: &str = "0.50.0";

/// Fetch models in the background; failures leave the static picker list in place.
pub fn spawn_fetch_models(auth: CodexAuth, tx: EventTx) {
    tokio::spawn(async move {
        if let Ok(models) = fetch_models(&auth).await
            && !models.is_empty()
        {
            tx.send(AppEvent::CodexModels { models });
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
        .header("OpenAI-Beta", "responses=experimental")
        .send()
        .await?;
    anyhow::ensure!(
        resp.status().is_success(),
        "models fetch failed: {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().await?;
    Ok(parse_models(&body))
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
