//! Refine chat persistence under `<root>/.honya/refine/<id>.json`.
//! The App is the sole writer for sessions and App writes are atomic. Refine
//! sub-agents also keep atomic checkpoint files under `.honya/refine/subagents/`
//! so interrupted delegated work can resume with its tool context intact.

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentCheckpoint {
    pub id: String,
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<serde_json::Value>,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub tool_call_count: usize,
    #[serde(default)]
    pub max_rounds: usize,
    #[serde(default)]
    pub depth: usize,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct SubagentCheckpointMeta {
    pub id: String,
    pub task: String,
    pub scope: Option<String>,
    pub model: String,
    pub updated: DateTime<Utc>,
    pub tool_call_count: usize,
    pub message_count: usize,
    pub depth: usize,
}

fn sessions_dir(root: &Path) -> PathBuf {
    root.join(".honya").join("refine")
}

fn subagents_dir(root: &Path) -> PathBuf {
    sessions_dir(root).join("subagents")
}

fn session_path(root: &Path, id: &str) -> PathBuf {
    sessions_dir(root).join(format!("{id}.json"))
}

fn subagent_path(root: &Path, id: &str) -> PathBuf {
    subagents_dir(root).join(format!("{}.json", filesystem_id(id)))
}

pub fn new_id() -> String {
    Utc::now().format("%Y%m%d-%H%M%S-%3f").to_string()
}

fn filesystem_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
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

pub fn list_subagents(root: &Path) -> Vec<SubagentCheckpointMeta> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(subagents_dir(root)) else {
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
        let Ok(cp) = serde_json::from_str::<SubagentCheckpoint>(&text) else {
            continue;
        };
        out.push(SubagentCheckpointMeta {
            id: cp.id,
            task: cp.task,
            scope: cp.scope,
            model: cp.model,
            updated: cp.updated,
            tool_call_count: cp.tool_call_count,
            message_count: cp.messages.len(),
            depth: cp.depth,
        });
    }
    out.sort_by_key(|m| std::cmp::Reverse(m.updated));
    out
}

pub fn load_subagent(root: &Path, id: &str) -> Option<SubagentCheckpoint> {
    let text = std::fs::read_to_string(subagent_path(root, id)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn find_subagent(
    root: &Path,
    task: &str,
    scope: Option<&str>,
    model: &str,
) -> Option<SubagentCheckpoint> {
    list_subagents(root)
        .into_iter()
        .find(|m| {
            m.task == task
                && m.model == model
                && m.scope.as_deref().map(str::trim).filter(|s| !s.is_empty())
                    == scope.map(str::trim).filter(|s| !s.is_empty())
        })
        .and_then(|m| load_subagent(root, &m.id))
}

pub fn save_subagent(root: &Path, checkpoint: &SubagentCheckpoint) -> std::io::Result<()> {
    let dir = subagents_dir(root);
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_string_pretty(checkpoint)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    data_block::atomic_write(&subagent_path(root, &checkpoint.id), &json)
}

pub fn delete_subagent(root: &Path, id: &str) -> std::io::Result<()> {
    match std::fs::remove_file(subagent_path(root, id)) {
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
