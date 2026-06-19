//! PKCE OAuth login for Codex "Sign in with ChatGPT".
//! Runs off-thread and reports only [`AppEvent`](crate::model::AppEvent)s; the
//! browser returns the code to the local Codex CLI port.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Context;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::{CodexAuth, pkce_pair, random_state};
use crate::model::{AppEvent, EventTx};

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const REDIRECT_PORT: u16 = 1455;
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const SCOPE: &str = "openid profile email offline_access";
/// Total time the local listener waits for the browser redirect before giving up.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

/// Spawn the Codex sign-in flow and report URL/success/error events.
pub fn spawn_codex_login(tx: EventTx) {
    tokio::spawn(async move {
        if let Err(e) = run_login(&tx).await {
            tx.send(AppEvent::CodexAuthError { msg: e.to_string() });
        }
    });
}

async fn run_login(tx: &EventTx) -> anyhow::Result<()> {
    let (verifier, challenge) = pkce_pair();
    let state = random_state();

    let listener = TcpListener::bind(("127.0.0.1", REDIRECT_PORT))
        .await
        .with_context(|| format!("could not bind port {REDIRECT_PORT} (is the Codex CLI running?)"))?;

    let url = authorize_url(&challenge, &state);
    let _ = crate::remote::open_url(&url);
    tx.send(AppEvent::CodexAuthUrl { url });

    let (code, got_state) = tokio::time::timeout(LOGIN_TIMEOUT, wait_for_callback(&listener))
        .await
        .context("timed out waiting for the browser sign-in")??;
    anyhow::ensure!(got_state == state, "OAuth state mismatch (possible CSRF)");

    let http = build_client()?;
    let tokens = exchange_code(&http, &code, &verifier).await?;
    let auth = CodexAuth::from_tokens(tokens.access_token, tokens.refresh_token, &tokens.id_token);
    anyhow::ensure!(
        !auth.account_id.is_empty(),
        "signed in but no ChatGPT account id was found in the token"
    );
    tx.send(AppEvent::CodexSignedIn {
        auth: Box::new(auth),
    });
    Ok(())
}

/// Build the OAuth authorize URL with PKCE + the Codex-specific flow params.
fn authorize_url(challenge: &str, state: &str) -> String {
    let q = [
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", REDIRECT_URI),
        ("scope", SCOPE),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
    ];
    let query = q
        .iter()
        .map(|(k, v)| format!("{}={}", k, crate::remote::enc(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{AUTHORIZE_URL}?{query}")
}

/// Accept loopback connections until one carries the `?code=…&state=…` redirect.
async fn wait_for_callback(listener: &TcpListener) -> anyhow::Result<(String, String)> {
    loop {
        let (mut socket, _) = listener.accept().await?;
        let mut buf = vec![0u8; 8192];
        let n = socket.read(&mut buf).await.unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        let target = req
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("");

        let Some(query) = target.split('?').nth(1) else {
            // Ignore favicon/probe requests.
            let _ = socket
                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                .await;
            continue;
        };

        let params = parse_query(query);
        let _ = socket.write_all(success_page().as_bytes()).await;
        let _ = socket.shutdown().await;

        if let Some(err) = params.get("error") {
            anyhow::bail!("authorization denied: {err}");
        }
        let code = params.get("code").cloned().context("callback had no code")?;
        let state = params.get("state").cloned().unwrap_or_default();
        return Ok((code, state));
    }
}

/// Exchange the authorization code for tokens (form-encoded, per OAuth).
async fn exchange_code(
    http: &reqwest::Client,
    code: &str,
    verifier: &str,
) -> anyhow::Result<TokenResponse> {
    let resp = http
        .post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form_body(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", REDIRECT_URI),
            ("client_id", CLIENT_ID),
            ("code_verifier", verifier),
        ]))
        .send()
        .await?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    anyhow::ensure!(status.is_success(), "token exchange failed ({status}): {body}");
    serde_json::from_str(&body).context("could not parse token response")
}

/// Refresh an access token, preserving account id and refresh token fallback.
pub async fn refresh(auth: &CodexAuth) -> anyhow::Result<CodexAuth> {
    let http = build_client()?;
    let resp = http
        .post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form_body(&[
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_ID),
            ("refresh_token", auth.refresh_token.as_str()),
            ("scope", SCOPE),
        ]))
        .send()
        .await?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    anyhow::ensure!(status.is_success(), "token refresh failed ({status}): {body}");
    let tokens: TokenResponse =
        serde_json::from_str(&body).context("could not parse refresh response")?;

    let refresh_token = if tokens.refresh_token.is_empty() {
        auth.refresh_token.clone()
    } else {
        tokens.refresh_token
    };
    let mut next = CodexAuth::from_tokens(tokens.access_token, refresh_token, &tokens.id_token);
    if next.account_id.is_empty() {
        next.account_id = auth.account_id.clone();
    }
    Ok(next)
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    id_token: String,
}

/// Form-encode pairs without reqwest's `urlencoded` feature.
fn form_body(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", crate::remote::enc(k), crate::remote::enc(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn build_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("honya/", env!("CARGO_PKG_VERSION")))
        .build()
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            Some((percent_decode(k), percent_decode(v)))
        })
        .collect()
}

/// Minimal `application/x-www-form-urlencoded` value decode (`%XX` + `+`).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi * 16 + lo) as u8);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn success_page() -> String {
    let body = "<!doctype html><html><head><meta charset=utf-8><title>honya</title></head>\
        <body style=\"font-family:system-ui;background:#1a1a1a;color:#eee;text-align:center;padding-top:18vh\">\
        <h2>本屋 · signed in to Codex</h2><p>You can close this tab and return to honya.</p></body></html>";
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_url_has_pkce_and_codex_params() {
        let url = authorize_url("CHAL", "STATE");
        assert!(url.starts_with(AUTHORIZE_URL));
        assert!(url.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));
        assert!(url.contains("code_challenge=CHAL"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=STATE"));
        assert!(url.contains("codex_cli_simplified_flow=true"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
    }

    #[test]
    fn parses_callback_query() {
        let p = parse_query("code=abc%2F123&state=xyz&extra=1");
        assert_eq!(p.get("code").unwrap(), "abc/123");
        assert_eq!(p.get("state").unwrap(), "xyz");
    }
}
