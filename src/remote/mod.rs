//! GitHub account linking and web remote control.
//!
//! Background tasks follow the `update.rs` pattern: own their clients, never
//! touch `App`, and report only by sending [`crate::model::AppEvent`]s.

pub mod auth;
pub mod protocol;
pub mod relay;

/// Public GitHub OAuth App client id; Device Flow uses no client secret.
/// CI bakes in the real value, while the placeholder keeps local builds working.
pub const GITHUB_CLIENT_ID: &str = match option_env!("HONYA_GITHUB_CLIENT_ID") {
    Some(id) => id,
    None => "Ov23li8xAEUqrNraq98z",
};

/// Relay backend base URL, overridable at build time for staging.
pub const RELAY_BASE: &str = match option_env!("HONYA_RELAY_BASE") {
    Some(url) => url,
    None => "https://api.honya.altqx.com",
};

pub fn github_login_configured() -> bool {
    !GITHUB_CLIENT_ID.contains("placeholder")
}

pub fn relay_ws_url() -> String {
    let base = RELAY_BASE.trim_end_matches('/');
    let ws = base
        .strip_prefix("https://")
        .map(|rest| format!("wss://{rest}"))
        .or_else(|| {
            base.strip_prefix("http://")
                .map(|rest| format!("ws://{rest}"))
        })
        .unwrap_or_else(|| base.to_string());
    format!("{ws}/relay")
}

/// Session-scoped relay URL; `label` is what the dashboard lists.
pub fn session_ws_url(session_id: &str, label: &str) -> String {
    format!(
        "{}?session={}&label={}",
        relay_ws_url(),
        enc(session_id),
        enc(label)
    )
}

/// Per-launch relay id for uniqueness only, not authorization.
pub fn new_session_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = u128::from(std::process::id());
    let count = u128::from(COUNTER.fetch_add(1, Ordering::Relaxed));
    let seed = u128::from(
        std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish(),
    );
    let mixed = nanos ^ (pid << 96) ^ (count << 64) ^ (seed << 32) ^ seed;
    format!("{mixed:032x}")
}

/// Best-effort browser opener: detached, silenced, and non-blocking.
pub fn open_url(url: &str) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let mut cmd;
    #[cfg(target_os = "macos")]
    {
        cmd = Command::new("open");
        cmd.arg(url);
    }
    #[cfg(target_os = "windows")]
    {
        // `explorer <url>` passes the URL as argv, avoiding cmd.exe shell parsing.
        cmd = Command::new("explorer");
        cmd.arg(url);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        cmd = Command::new("xdg-open");
        cmd.arg(url);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Write OSC-52 clipboard text; terminals may silently ignore it.
pub fn copy_to_clipboard(text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let seq = format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes()));
    let mut out = std::io::stdout().lock();
    out.write_all(seq.as_bytes())?;
    out.flush()
}

fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Minimal percent-encoder for hand-built device-flow and relay query strings.
pub(crate) fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// GitHub expects CLI clients to send a User-Agent.
pub fn user_agent() -> String {
    format!("honya/{}", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_ws_url_swaps_scheme() {
        assert!(relay_ws_url().starts_with("wss://"));
        assert!(relay_ws_url().ends_with("/relay"));
    }

    #[test]
    fn session_ws_url_carries_encoded_params() {
        let url = session_ws_url("abc123", "Re:Zero · Vol.03");
        assert!(url.contains("/relay?session=abc123&label="));
        assert!(url.contains("Re%3AZero%20%C2%B7%20Vol.03"));
    }

    #[test]
    fn session_ids_are_distinct_hex() {
        let a = new_session_id();
        let b = new_session_id();
        assert_ne!(a, b);
        assert!(!a.is_empty());
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn base64_matches_known_vector() {
        assert_eq!(base64_encode(b"honya"), "aG9ueWE=");
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
    }
}
