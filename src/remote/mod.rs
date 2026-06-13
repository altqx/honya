//! GitHub account linking and web remote control.
//!
//! Background tasks follow the `update.rs` pattern: own their clients, never
//! touch `App`, and report only by sending [`crate::model::AppEvent`]s.

pub mod auth;
pub mod protocol;
pub mod relay;

/// Public GitHub OAuth App client id; Device Flow uses no client secret.
/// CI bakes in the real value, while the placeholder keeps local builds working.
pub const GITHUB_CLIENT_ID: &str =
    match option_env!("HONYA_GITHUB_CLIENT_ID") {
        Some(id) => id,
        None => "Iv1.honya-placeholder",
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
        .or_else(|| base.strip_prefix("http://").map(|rest| format!("ws://{rest}")))
        .unwrap_or_else(|| base.to_string());
    format!("{ws}/relay")
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
}
