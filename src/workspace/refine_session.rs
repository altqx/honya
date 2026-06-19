//! Refine chat persistence under `<root>/.honya/refine/<id>.json`.
//! The App is the sole writer; agents only read sessions and App writes are atomic.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::llm::Message;
use crate::workspace::data_block;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefineSession {
    pub id: String,
    #[serde(default)]
    pub title: String,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    #[serde(default)]
    pub model: String,
    /// Chat messages excluding the system turn.
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default)]
    pub plan: Vec<crate::model::PlanStep>,
}

impl RefineSession {
    pub fn new(id: String, model: String) -> Self {
        let now = Utc::now();
        Self {
            id,
            title: String::new(),
            created: now,
            updated: now,
            model,
            messages: Vec::new(),
            plan: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub updated: DateTime<Utc>,
    pub message_count: usize,
}

fn sessions_dir(root: &Path) -> PathBuf {
    root.join(".honya").join("refine")
}

fn session_path(root: &Path, id: &str) -> PathBuf {
    sessions_dir(root).join(format!("{id}.json"))
}

pub fn new_id() -> String {
    Utc::now().format("%Y%m%d-%H%M%S-%3f").to_string()
}

pub fn list(root: &Path) -> Vec<SessionMeta> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(sessions_dir(root)) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(session) = serde_json::from_str::<RefineSession>(&text) else {
            continue;
        };
        out.push(SessionMeta {
            id: session.id,
            title: session.title,
            updated: session.updated,
            message_count: session.messages.len(),
        });
    }
    out.sort_by_key(|m| std::cmp::Reverse(m.updated));
    out
}

pub fn load(root: &Path, id: &str) -> Option<RefineSession> {
    let text = std::fs::read_to_string(session_path(root, id)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn save(root: &Path, session: &RefineSession) -> std::io::Result<()> {
    let dir = sessions_dir(root);
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_string_pretty(session)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    data_block::atomic_write(&session_path(root, &session.id), &json)
}

pub fn delete(root: &Path, id: &str) -> std::io::Result<()> {
    match std::fs::remove_file(session_path(root, id)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::Message;

    fn temp_root(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "honya_session_{tag}_{}_{}",
            std::process::id(),
            stamp
        ));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn save_load_list_delete_round_trip() {
        let root = temp_root("rt");
        let mut a = RefineSession::new("a-id".to_string(), "m".to_string());
        a.title = "first".to_string();
        a.messages.push(Message::user("hi"));
        save(&root, &a).unwrap();

        let b = RefineSession::new("b-id".to_string(), "m".to_string());
        save(&root, &b).unwrap();

        let listed = list(&root);
        assert_eq!(listed.len(), 2);

        let loaded = load(&root, "a-id").unwrap();
        assert_eq!(loaded.title, "first");
        assert_eq!(loaded.messages.len(), 1);

        delete(&root, "a-id").unwrap();
        assert!(load(&root, "a-id").is_none());
        assert_eq!(list(&root).len(), 1);

        let _ = std::fs::remove_dir_all(&root);
    }
}
