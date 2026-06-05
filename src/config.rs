//! src/config.rs — load/save AppConfig from disk + env API-key discovery.
//! Depends only on model.rs. Tolerant: a missing/corrupt config falls back to
//! AppConfig::default() so the app always launches (and runs the MockClient).

use std::path::PathBuf;

use directories::ProjectDirs;

use crate::model::AppConfig;

/// Where the persisted config lives: <platform config dir>/honya/config.json.
/// Falls back to ./honya-config.json if the platform dir can't be resolved.
pub fn config_path() -> PathBuf {
    if let Some(dirs) = ProjectDirs::from("", "", "honya") {
        dirs.config_dir().join("config.json")
    } else {
        PathBuf::from("honya-config.json")
    }
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
pub fn save(cfg: &AppConfig) -> std::io::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(cfg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, json)
}

/// Discover the OpenRouter API key: HONYA_API_KEY wins, then OPENROUTER_API_KEY.
/// Empty/whitespace values are treated as absent so the MockClient stays active.
pub fn api_key() -> Option<String> {
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
