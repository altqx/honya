//! GitHub Device Flow plus relay device registration.
//!
//! The task shows GitHub's user code, polls for authorization, then exchanges the
//! GitHub token with the relay for a long-lived device token.

use std::time::Duration;

use serde::Deserialize;

use super::{GITHUB_CLIENT_ID, RELAY_BASE, enc, user_agent};
use crate::model::{AppEvent, EventTx};

const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const SCOPE: &str = "read:user";

#[derive(Debug, Clone, Deserialize)]
struct DeviceCode {
    device_code: String,
    user_code: String,
    verification_uri: String,
    /// GitHub's code-prefilled verification URL, if provided.
    #[serde(default)]
    verification_uri_complete: String,
    #[serde(default = "default_interval")]
    interval: u64,
    #[serde(default)]
    expires_in: u64,
}

fn default_interval() -> u64 {
    5
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TokenPoll {
    Token(String),
    Pending,
    SlowDown(u64),
    Failed(String),
}

#[derive(Debug, Clone, Deserialize)]
struct RegisterResponse {
    device_id: String,
    device_token: String,
    login: String,
}

fn parse_token_poll(body: &serde_json::Value) -> TokenPoll {
    if let Some(token) = body.get("access_token").and_then(|v| v.as_str()) {
        return TokenPoll::Token(token.to_string());
    }
    match body.get("error").and_then(|v| v.as_str()) {
        Some("authorization_pending") => TokenPoll::Pending,
        Some("slow_down") => {
            let extra = body
                .get("interval")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(5);
            TokenPoll::SlowDown(extra)
        }
        Some("expired_token") => TokenPoll::Failed("the sign-in code expired — try again".into()),
        Some("access_denied") => TokenPoll::Failed("sign-in was cancelled".into()),
        Some(other) => TokenPoll::Failed(other.replace('_', " ")),
        None => TokenPoll::Failed("unexpected response from GitHub".into()),
    }
}

pub fn spawn_device_login(tx: EventTx) {
    tokio::spawn(async move {
        if let Err(msg) = run_device_login(&tx).await {
            tx.send(AppEvent::RemoteAuthError { msg });
        }
    });
}

async fn run_device_login(tx: &EventTx) -> Result<(), String> {
    if !super::github_login_configured() {
        return Err("GitHub sign-in is not configured in this build".into());
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("http init failed: {e}"))?;

    let dc = request_device_code(&client).await?;
    tx.send(AppEvent::RemoteAuthCode {
        user_code: dc.user_code.clone(),
        verification_uri: dc.verification_uri.clone(),
        verification_uri_complete: dc.verification_uri_complete.clone(),
    });

    let github_token = poll_for_token(&client, tx, &dc).await?;
    let registered = register_device(&client, &github_token).await?;

    tx.send(AppEvent::RemotePaired {
        login: registered.login,
        device_id: registered.device_id,
        device_token: registered.device_token,
    });
    Ok(())
}

async fn request_device_code(client: &reqwest::Client) -> Result<DeviceCode, String> {
    let url = format!(
        "{DEVICE_CODE_URL}?client_id={}&scope={}",
        enc(GITHUB_CLIENT_ID),
        enc(SCOPE)
    );
    let resp = client
        .post(url)
        .header("Accept", "application/json")
        .header("User-Agent", user_agent())
        .send()
        .await
        .map_err(|e| format!("could not reach GitHub: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "GitHub returned {} starting sign-in",
            resp.status()
        ));
    }
    resp.json::<DeviceCode>()
        .await
        .map_err(|e| format!("unexpected GitHub response: {e}"))
}

async fn poll_for_token(
    client: &reqwest::Client,
    tx: &EventTx,
    dc: &DeviceCode,
) -> Result<String, String> {
    let mut interval = dc.interval.max(1);
    // Respect GitHub's device-code lifetime instead of polling indefinitely.
    let deadline_polls = if dc.expires_in > 0 {
        (dc.expires_in / interval).max(1) + 2
    } else {
        180
    };

    for _ in 0..deadline_polls {
        tokio::time::sleep(Duration::from_secs(interval)).await;

        let url = format!(
            "{ACCESS_TOKEN_URL}?client_id={}&device_code={}&grant_type={}",
            enc(GITHUB_CLIENT_ID),
            enc(&dc.device_code),
            enc("urn:ietf:params:oauth:grant-type:device_code"),
        );
        let resp = client
            .post(url)
            .header("Accept", "application/json")
            .header("User-Agent", user_agent())
            .send()
            .await
            .map_err(|e| format!("could not reach GitHub: {e}"))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("unexpected GitHub response: {e}"))?;

        match parse_token_poll(&body) {
            TokenPoll::Token(t) => return Ok(t),
            TokenPoll::Pending => {
                tx.send(AppEvent::RemoteAuthPending);
            }
            TokenPoll::SlowDown(extra) => {
                interval = interval.saturating_add(extra);
                tx.send(AppEvent::RemoteAuthPending);
            }
            TokenPoll::Failed(msg) => return Err(msg),
        }
    }
    Err("sign-in timed out — try again".into())
}

async fn register_device(
    client: &reqwest::Client,
    github_token: &str,
) -> Result<RegisterResponse, String> {
    let label = device_label();
    let resp = client
        .post(format!("{RELAY_BASE}/device/register"))
        .header("Authorization", format!("Bearer {github_token}"))
        .header("User-Agent", user_agent())
        .json(&serde_json::json!({ "label": label }))
        .send()
        .await
        .map_err(|e| format!("could not reach the honya relay: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("relay rejected sign-in ({})", resp.status()));
    }
    resp.json::<RegisterResponse>()
        .await
        .map_err(|e| format!("unexpected relay response: {e}"))
}

pub(crate) fn device_label() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "honya".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_returns_token() {
        let v = serde_json::json!({ "access_token": "gho_abc", "token_type": "bearer" });
        assert_eq!(parse_token_poll(&v), TokenPoll::Token("gho_abc".into()));
    }

    #[test]
    fn poll_pending_and_slow_down() {
        let p = serde_json::json!({ "error": "authorization_pending" });
        assert_eq!(parse_token_poll(&p), TokenPoll::Pending);
        let s = serde_json::json!({ "error": "slow_down", "interval": 10 });
        assert_eq!(parse_token_poll(&s), TokenPoll::SlowDown(10));
    }

    #[test]
    fn poll_terminal_errors() {
        let e = serde_json::json!({ "error": "expired_token" });
        assert!(matches!(parse_token_poll(&e), TokenPoll::Failed(_)));
        let d = serde_json::json!({ "error": "access_denied" });
        assert!(matches!(parse_token_poll(&d), TokenPoll::Failed(_)));
        let empty = serde_json::json!({});
        assert!(matches!(parse_token_poll(&empty), TokenPoll::Failed(_)));
    }

    #[test]
    fn device_code_parses_with_defaults() {
        let v = serde_json::json!({
            "device_code": "dc",
            "user_code": "ABCD-1234",
            "verification_uri": "https://github.com/login/device"
        });
        let dc: DeviceCode = serde_json::from_value(v).unwrap();
        assert_eq!(dc.user_code, "ABCD-1234");
        assert_eq!(dc.interval, 5);
        assert_eq!(dc.verification_uri_complete, "");
    }

    #[test]
    fn device_code_keeps_complete_uri_when_present() {
        let v = serde_json::json!({
            "device_code": "dc",
            "user_code": "ABCD-1234",
            "verification_uri": "https://github.com/login/device",
            "verification_uri_complete": "https://github.com/login/device?user_code=ABCD-1234"
        });
        let dc: DeviceCode = serde_json::from_value(v).unwrap();
        assert_eq!(
            dc.verification_uri_complete,
            "https://github.com/login/device?user_code=ABCD-1234"
        );
    }
}
