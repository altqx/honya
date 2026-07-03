//! Codex identity/token helpers: PKCE OAuth, refresh, and Codex CLI auth import.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub mod auth;
pub mod models;
pub mod usage_log;

/// Persisted Codex credentials (stored inside honya's `config.json`, mode 0600).
/// `expires_at` is the access-token JWT `exp` (Unix seconds; 0 = unknown).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodexAuth {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub account_id: String,
    #[serde(default)]
    pub expires_at: i64,
}

impl CodexAuth {
    pub fn from_tokens(access_token: String, refresh_token: String, id_token: &str) -> Self {
        let account_id = account_id_from_id_token(id_token).unwrap_or_default();
        let expires_at = jwt_exp(&access_token).unwrap_or(0);
        Self {
            access_token,
            refresh_token,
            account_id,
            expires_at,
        }
    }

    /// Whether the access token is expired (or within 60s of expiring).
    pub fn is_expired(&self, now_unix: i64) -> bool {
        self.expires_at != 0 && now_unix >= self.expires_at - 60
    }
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `$CODEX_HOME`, else `$HOME/.codex` (or `%USERPROFILE%\.codex` on Windows).
pub fn codex_home() -> Option<PathBuf> {
    if let Some(h) = std::env::var_os("CODEX_HOME") {
        return Some(PathBuf::from(h));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".codex"))
}

/// Import the Codex CLI's `~/.codex/auth.json` if present and it holds OAuth
/// tokens (not just an `OPENAI_API_KEY`). Returns `None` when absent/unusable.
pub fn import_codex_cli_auth() -> Option<CodexAuth> {
    let path = codex_home()?.join("auth.json");
    let raw = std::fs::read_to_string(path).ok()?;
    parse_codex_auth_json(&raw)
}

fn parse_codex_auth_json(raw: &str) -> Option<CodexAuth> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let tokens = v.get("tokens")?;
    let access_token = tokens.get("access_token")?.as_str()?.to_string();
    let refresh_token = tokens
        .get("refresh_token")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string();
    let id_token = tokens
        .get("id_token")
        .and_then(|x| x.as_str())
        .unwrap_or_default();
    let account_id = tokens
        .get("account_id")
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .or_else(|| account_id_from_id_token(id_token))
        .unwrap_or_default();
    Some(CodexAuth {
        access_token,
        refresh_token,
        account_id,
        expires_at: jwt_exp(tokens.get("access_token")?.as_str()?).unwrap_or(0),
    })
}

/// Decode a JWT's payload (middle segment) as JSON without verifying the signature.
fn jwt_payload(token: &str) -> Option<serde_json::Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64url_decode(payload).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The ChatGPT account id from an `id_token`: `["https://api.openai.com/auth"]`
/// → `chatgpt_account_id` (falling back to `organization_id`).
pub fn account_id_from_id_token(id_token: &str) -> Option<String> {
    let claims = jwt_payload(id_token)?;
    let auth = claims.get("https://api.openai.com/auth")?;
    auth.get("chatgpt_account_id")
        .or_else(|| auth.get("organization_id"))
        .and_then(|x| x.as_str())
        .map(str::to_string)
}

/// The `exp` (Unix seconds) claim of a JWT access token.
fn jwt_exp(token: &str) -> Option<i64> {
    jwt_payload(token)?.get("exp")?.as_i64()
}

/// A PKCE `(code_verifier, code_challenge)` pair. The challenge is
/// `base64url-nopad(SHA-256(verifier))`, method `S256`.
pub fn pkce_pair() -> (String, String) {
    let verifier = base64url_encode(&random_bytes(32));
    let challenge = base64url_encode(&Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// A random opaque token (base64url) for the OAuth `state` parameter.
pub fn random_state() -> String {
    base64url_encode(&random_bytes(24))
}

/// A random UUIDv4 string for the Responses API `session_id` header.
pub fn uuid_v4() -> String {
    let b = random_bytes(16);
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-4{:01x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0],
        b[1],
        b[2],
        b[3],
        b[4],
        b[5],
        b[6] & 0x0f,
        b[7],
        (b[8] & 0x3f) | 0x80,
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15],
    )
}

/// `n` pseudo-random bytes seeded from the OS-seeded `RandomState` mixed with
/// high-resolution time and the pid, whitened through SHA-256.
// std has no public CSPRNG; use `getrandom` if this must become stronger.
fn random_bytes(n: usize) -> Vec<u8> {
    use std::hash::{BuildHasher, Hasher};
    let mut out = Vec::with_capacity(n + 32);
    let mut counter: u64 = 0;
    while out.len() < n {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        h.write_u128(nanos);
        h.write_u64(counter);
        h.write_u32(std::process::id());
        let mut hasher = Sha256::new();
        hasher.update(h.finish().to_le_bytes());
        hasher.update(counter.to_le_bytes());
        out.extend_from_slice(&hasher.finalize());
        counter += 1;
    }
    out.truncate(n);
    out
}

const B64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Base64url-encode without padding (RFC 4648 §5).
pub fn base64url_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64URL[(n >> 18 & 0x3f) as usize] as char);
        out.push(B64URL[(n >> 12 & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64URL[(n >> 6 & 0x3f) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(B64URL[(n & 0x3f) as usize] as char);
        }
    }
    out
}

/// Base64url-decode, tolerating missing padding and standard (`+/`) alphabets.
pub fn base64url_decode(s: &str) -> Result<Vec<u8>, &'static str> {
    let val = |c: u8| -> Result<u32, &'static str> {
        match c {
            b'A'..=b'Z' => Ok((c - b'A') as u32),
            b'a'..=b'z' => Ok((c - b'a' + 26) as u32),
            b'0'..=b'9' => Ok((c - b'0' + 52) as u32),
            b'-' | b'+' => Ok(62),
            b'_' | b'/' => Ok(63),
            _ => Err("invalid base64url char"),
        }
    };
    let bytes: Vec<u8> = s.bytes().filter(|b| *b != b'=').collect();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        if chunk.len() < 2 {
            return Err("truncated base64url");
        }
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= val(c)? << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_round_trips_and_is_url_safe() {
        for case in [
            &b""[..],
            b"f",
            b"fo",
            b"foo",
            b"foob",
            b"\xff\xfe\xfd\x00\x01",
        ] {
            let enc = base64url_encode(case);
            assert!(!enc.contains('=') && !enc.contains('+') && !enc.contains('/'));
            assert_eq!(base64url_decode(&enc).unwrap(), case);
        }
    }

    #[test]
    fn pkce_challenge_is_sha256_of_verifier() {
        let (verifier, challenge) = pkce_pair();
        assert!(verifier.len() >= 43, "verifier must be >= 43 chars");
        let expect = base64url_encode(&Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, expect);
        assert_ne!(pkce_pair().0, pkce_pair().0);
    }

    fn jwt(payload: &serde_json::Value) -> String {
        let body = base64url_encode(serde_json::to_string(payload).unwrap().as_bytes());
        format!("eyJhbGciOiJub25lIn0.{body}.sig")
    }

    #[test]
    fn account_id_extracted_from_id_token_claim() {
        let token = jwt(&serde_json::json!({
            "https://api.openai.com/auth": { "chatgpt_account_id": "acct-123" }
        }));
        assert_eq!(
            account_id_from_id_token(&token).as_deref(),
            Some("acct-123")
        );
    }

    #[test]
    fn imports_codex_cli_auth_json_shape() {
        let access = jwt(&serde_json::json!({ "exp": 9_999_999_999i64 }));
        let id = jwt(&serde_json::json!({
            "https://api.openai.com/auth": { "chatgpt_account_id": "acct-xyz" }
        }));
        let raw = serde_json::json!({
            "OPENAI_API_KEY": serde_json::Value::Null,
            "tokens": { "id_token": id, "access_token": access, "refresh_token": "rt" },
            "last_refresh": "2026-01-01T00:00:00Z"
        })
        .to_string();
        let auth = parse_codex_auth_json(&raw).unwrap();
        assert_eq!(auth.account_id, "acct-xyz");
        assert_eq!(auth.refresh_token, "rt");
        assert_eq!(auth.expires_at, 9_999_999_999);
        assert!(!auth.is_expired(0));
        assert!(auth.is_expired(9_999_999_999));
    }
}
