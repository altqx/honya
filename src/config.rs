//! Load/save AppConfig from disk + OpenRouter API-key discovery.
//! Tolerant: a missing/corrupt config falls back to AppConfig::default().

use std::path::PathBuf;

use crate::model::AppConfig;

/// The honya config directory: `$XDG_CONFIG_HOME/honya`, Windows app data, then `~/.config/honya`.
pub fn config_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let xdg = PathBuf::from(xdg);
        if xdg.is_absolute() {
            return xdg.join("honya");
        }
    }
    // Prefer roaming app data on Windows, where HOME is often unset.
    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("honya");
        }
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            return PathBuf::from(profile).join(".config").join("honya");
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(local).join("honya");
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".config").join("honya");
    }
    // Last resort: a local ./.config/honya.
    PathBuf::from(".config").join("honya")
}

/// Where the persisted config lives: `<config dir>/config.json`.
pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

/// Load config from disk, falling back to defaults on any error.
pub fn load() -> AppConfig {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => AppConfig::default(),
    }
}

/// Persist config as pretty JSON, creating the parent directory if needed.
/// The file may hold the API key, so it is tightened to 0600 on Unix.
pub fn save(cfg: &AppConfig) -> std::io::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(cfg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, json)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Resolve the OpenRouter API key. The environment wins (HONYA_API_KEY, then
/// OPENROUTER_API_KEY); otherwise the key persisted in the config is used.
/// Empty/whitespace values are treated as absent.
pub fn resolve_api_key(cfg: &AppConfig) -> Option<String> {
    api_key_from_env().or_else(|| {
        cfg.api_key
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    })
}

/// The API key supplied via the environment (HONYA_API_KEY, then
/// OPENROUTER_API_KEY), if any. When this is set it overrides the saved config
/// key, so the in-app Settings editor surfaces it as read-only.
pub fn api_key_from_env() -> Option<String> {
    for var in ["HONYA_API_KEY", "OPENROUTER_API_KEY"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}
