//! Crash/power-loss recovery checkpoint for an in-flight translation run.
//!
//! The translated files are already the real recovery substrate: approved Thai is
//! flushed to `translated/ch_NNN.md` chunk-by-chunk (`translation::append_chunk`),
//! and `run_pipeline` skips already-committed chunks on a re-run. What a crash
//! actually loses is the *orchestration* — which project / volume / chapters were
//! being translated. This module persists just that, so the next launch can offer
//! a one-keystroke resume back into the existing chunk-level resume path.
//!
//! Invariant: the checkpoint file exists **iff** a run is in flight. It is written
//! when a run starts ([`save`]) and removed when the pipeline finishes ([`clear`]).
//! A checkpoint that survives to the next launch therefore means the previous run
//! was interrupted (crash, power loss, or a hard kill) and is resumable.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Bumped if the on-disk shape changes incompatibly; an older/newer file is
/// treated as absent rather than mis-deserialized.
const SCHEMA_VERSION: u32 = 1;

/// The single in-flight run honya can resume. honya runs one pipeline at a time
/// (enforced by `App::run_active`), so a single global checkpoint is sufficient.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCheckpoint {
    /// On-disk schema version (see [`SCHEMA_VERSION`]).
    pub version: u32,
    /// Absolute path to the project directory — used to reopen it on resume even
    /// when honya is relaunched from a different working directory.
    pub project_dir: PathBuf,
    /// Project slug (directory name), for lookup + display.
    pub project_id: String,
    /// Project display title, shown in the recovery prompt.
    pub project_title: String,
    /// Active volume number the run targeted.
    pub vol: u32,
    /// The chapter queue the run was processing (in order).
    pub chapters: Vec<u32>,
    /// When the run started (informational / display).
    pub started_at: DateTime<Utc>,
    /// Stable run-history id shared with `VOLUME.md`. Default keeps older
    /// checkpoints loadable; resume will synthesize one if absent.
    #[serde(default)]
    pub run_id: String,
    /// honya version that wrote the checkpoint (forward-compat / debugging).
    pub honya_version: String,
}

impl SessionCheckpoint {
    /// Build a checkpoint stamped with the current schema version, time, and
    /// honya version.
    pub fn new(
        project_dir: PathBuf,
        project_id: String,
        project_title: String,
        vol: u32,
        chapters: Vec<u32>,
    ) -> Self {
        let started_at = Utc::now();
        Self {
            version: SCHEMA_VERSION,
            project_dir,
            project_id,
            project_title,
            vol,
            chapters,
            started_at,
            run_id: make_run_id(started_at),
            honya_version: crate::update::current_version().to_string(),
        }
    }

    /// True when this checkpoint still points at a real project with chapters to
    /// run. A stale checkpoint (project deleted/moved, or empty queue) is not
    /// resumable and should be cleared instead of prompted.
    pub fn is_resumable(&self) -> bool {
        !self.chapters.is_empty() && self.project_dir.join("PROJECT.md").is_file()
    }

    /// Fill in a run id for checkpoints written by older compatible versions.
    pub fn ensure_run_id(&mut self) {
        if self.run_id.trim().is_empty() {
            self.run_id = make_run_id(self.started_at);
        }
    }
}

/// Deterministic, filesystem-safe id for a run started at `started_at`. The
/// process id suffix avoids collisions when multiple honya instances start in
/// the same millisecond (even though only one run is allowed per app instance).
pub fn make_run_id(started_at: DateTime<Utc>) -> String {
    format!(
        "run-{}-{}",
        started_at.format("%Y%m%dT%H%M%S%.3fZ"),
        std::process::id()
    )
}

/// Path to the recovery checkpoint. Honors `HONYA_SESSION_FILE` (an absolute path
/// override, mostly for tests / relocating state); otherwise lives alongside the
/// config at `<config dir>/session.json`.
pub fn path() -> PathBuf {
    if let Some(p) = std::env::var_os("HONYA_SESSION_FILE") {
        let p = PathBuf::from(p);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    crate::config::config_dir().join("session.json")
}

/// Persist `cp` to the recovery file (atomic temp+rename so a crash mid-write
/// never leaves a torn file).
pub fn save(cp: &SessionCheckpoint) -> std::io::Result<()> {
    save_at(&path(), cp)
}

/// Load the recovery checkpoint, or `None` when absent / unreadable / schema
/// mismatch (all treated as "nothing to recover").
pub fn load() -> Option<SessionCheckpoint> {
    load_at(&path())
}

/// Remove the recovery checkpoint (best-effort; a missing file is success).
pub fn clear() {
    clear_at(&path());
}

fn save_at(path: &Path, cp: &SessionCheckpoint) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(cp)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    crate::workspace::data_block::atomic_write(path, &json)
}

fn load_at(path: &Path) -> Option<SessionCheckpoint> {
    let text = std::fs::read_to_string(path).ok()?;
    let cp: SessionCheckpoint = serde_json::from_str(&text).ok()?;
    if cp.version != SCHEMA_VERSION {
        return None;
    }
    Some(cp)
}

fn clear_at(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("honya_session_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample(project_dir: PathBuf) -> SessionCheckpoint {
        SessionCheckpoint::new(
            project_dir,
            "re-zero".into(),
            "Re:Zero".into(),
            1,
            vec![3, 4, 5],
        )
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = scratch("roundtrip");
        let file = dir.join("session.json");
        let cp = sample(dir.clone());

        save_at(&file, &cp).unwrap();
        let loaded = load_at(&file).expect("checkpoint loads back");
        assert_eq!(loaded, cp, "checkpoint survives a save/load round-trip");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn absent_or_corrupt_or_versioned_yields_none() {
        let dir = scratch("none");
        let file = dir.join("session.json");

        // Absent.
        assert!(load_at(&file).is_none(), "missing file → None");

        // Corrupt JSON.
        std::fs::write(&file, "{not json").unwrap();
        assert!(load_at(&file).is_none(), "bad json → None");

        // Wrong schema version is rejected (treated as nothing to recover).
        let mut cp = sample(dir.clone());
        cp.version = SCHEMA_VERSION + 1;
        let json = serde_json::to_string_pretty(&cp).unwrap();
        std::fs::write(&file, json).unwrap();
        assert!(load_at(&file).is_none(), "version mismatch → None");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_removes_the_file_and_is_idempotent() {
        let dir = scratch("clear");
        let file = dir.join("session.json");
        save_at(&file, &sample(dir.clone())).unwrap();
        assert!(file.is_file());

        clear_at(&file);
        assert!(!file.exists(), "clear removes the checkpoint");
        clear_at(&file); // second clear is a no-op, not an error

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resumable_requires_project_and_chapters() {
        let dir = scratch("resumable");

        // No PROJECT.md yet → not resumable.
        let cp = sample(dir.clone());
        assert!(!cp.is_resumable(), "no PROJECT.md → not resumable");

        // With PROJECT.md present and a non-empty queue → resumable.
        std::fs::write(dir.join("PROJECT.md"), "# Re:Zero\n").unwrap();
        assert!(cp.is_resumable(), "project + chapters → resumable");

        // Empty queue is never resumable.
        let empty = SessionCheckpoint::new(dir.clone(), "x".into(), "X".into(), 1, vec![]);
        assert!(!empty.is_resumable(), "empty chapter queue → not resumable");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
