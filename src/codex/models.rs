//! Fetch Codex model ids for the Settings picker, with static fallbacks on failure.

use std::time::Duration;

use super::CodexAuth;
use crate::model::{AppEvent, EventTx, LogLevel};

const MODELS_URL: &str = "https://chatgpt.com/backend-api/codex/models";
const CODEX_PACKAGE_URL: &str = "https://registry.npmjs.org/@openai/codex/latest";
const FALLBACK_CLIENT_VERSION: &str = "0.144.1";

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
    let client_version = fetch_client_version(&http)
        .await
        .unwrap_or_else(|| FALLBACK_CLIENT_VERSION.to_string());
    let resp = models_request(&http, MODELS_URL, auth, &client_version)?
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

async fn fetch_client_version(http: &reqwest::Client) -> Option<String> {
    let resp = http
        .get(CODEX_PACKAGE_URL)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    let body = resp.text().await.ok()?;
    let json: serde_json::Value = serde_json::from_str(&body).ok()?;
    parse_client_version(&json).map(str::to_string)
}

fn parse_client_version(v: &serde_json::Value) -> Option<&str> {
    v.get("version")
        .and_then(|version| version.as_str())
        .map(str::trim)
        .filter(|version| !version.is_empty())
}

fn models_request(
    http: &reqwest::Client,
    url: &str,
    auth: &CodexAuth,
    client_version: &str,
) -> anyhow::Result<reqwest::RequestBuilder> {
    let mut url = reqwest::Url::parse(url)?;
    url.query_pairs_mut()
        .append_pair("client_version", client_version);
    Ok(http
        .get(url)
        .bearer_auth(&auth.access_token)
        .header("chatgpt-account-id", &auth.account_id))
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
