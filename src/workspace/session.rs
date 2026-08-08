//! Crash/power-loss recovery checkpoint for an in-flight translation run.
//!
//! The translated files are already the real recovery substrate: approved Thai is
//! flushed to `translated/ch_NNN.md` chunk-by-chunk (`translation::append_chunk`),
//! and `run_pipeline` skips already-committed chunks on a re-run. What a crash
//! actually loses is the *orchestration* — which project / volume / chapters were
//! being translated. This module persists just that, so the next launch can offer
//! a one-keystroke resume back into the existing chunk-level resume path.
//!
//! Invariant: a checkpoint file exists for a project **iff** a run is in flight
//! (or was interrupted before a clean finish) on that project. It is written when
//! a run starts ([`save`]) and removed when the pipeline finishes
//! ([`clear_for`]). The owner fields [`SessionCheckpoint::pid`] and
//! [`SessionCheckpoint::heartbeat_at`] distinguish a **live** run in another
//! honya window from a true interrupt: fresh heartbeat and an alive foreign PID
//! mean leave the file alone and block opening that project; dead or stale means
//! the normal resume prompt.
//!
//! Storage is one file per project under [`dir`], so multiple honya PIDs can
//! translate **different** projects in parallel without clobbering each other.
//! The same project is still single-owner (second window cannot open/start it).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Bumped if the on-disk shape changes incompatibly; an older/newer file is
/// treated as absent rather than mis-deserialized.
const SCHEMA_VERSION: u32 = 1;

/// Heartbeats older than this mean the owner is gone or hung → treat as interrupt.
const HEARTBEAT_STALE_SECS: i64 = 90;

/// One in-flight (or interrupted) run, scoped to a single project.
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
    /// True when this run was the one-click auto project-translate (every volume).
    /// On resume the queue is recomputed from disk across all volumes rather than
    /// read from `vol`/`chapters`, which only record the segment in flight at
    /// checkpoint time. Default keeps older checkpoints loadable.
    #[serde(default)]
    pub whole_project: bool,
    /// Process id of the honya instance that owns this run. `0` = legacy checkpoint
    /// without an owner (treated as interrupted, not live elsewhere).
    #[serde(default)]
    pub pid: u32,
    /// Last time the owner refreshed this file while the run was active.
    #[serde(default = "epoch")]
    pub heartbeat_at: DateTime<Utc>,
}

fn epoch() -> DateTime<Utc> {
    DateTime::<Utc>::UNIX_EPOCH
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
        let now = started_at;
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
            whole_project: false,
            pid: std::process::id(),
            heartbeat_at: now,
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

    /// Claim this checkpoint for the current process (pid + fresh heartbeat).
    /// Call before saving when this instance starts or resumes a run.
    pub fn claim_owner(&mut self) {
        self.pid = std::process::id();
        self.touch_heartbeat();
    }

    /// Refresh the live-run heartbeat timestamp.
    pub fn touch_heartbeat(&mut self) {
        self.heartbeat_at = Utc::now();
    }

    /// True when another honya process still owns this run (alive PID + fresh
    /// heartbeat). Used to suppress the false "interrupted" resume prompt and to
    /// lock the project in a second window.
    pub fn is_live_elsewhere(&self) -> bool {
        #[cfg(test)]
        if let Some(forced) = live_elsewhere_override() {
            return forced && self.pid != 0 && self.pid != std::process::id();
        }

        let me = std::process::id();
        if self.pid == 0 || self.pid == me {
            return false;
        }
        if !heartbeat_fresh(self.heartbeat_at) {
            return false;
        }
        process_alive(self.pid)
    }
}

fn heartbeat_fresh(at: DateTime<Utc>) -> bool {
    let age = Utc::now().signed_duration_since(at);
    age.num_seconds() >= 0 && age.num_seconds() <= HEARTBEAT_STALE_SECS
}

fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        // Signal 0: existence check only (no signal delivered).
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(windows)]
    {
        windows_process_alive(pid)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

#[cfg(windows)]
fn windows_process_alive(pid: u32) -> bool {
    type Handle = *mut core::ffi::c_void;
    // Edition 2024: extern blocks are unsafe.
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        fn CloseHandle(handle: Handle) -> i32;
        fn GetExitCodeProcess(handle: Handle, exit_code: *mut u32) -> i32;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code);
        CloseHandle(h);
        ok != 0 && code == STILL_ACTIVE
    }
}

/// True when `a` and `b` name the same directory, even across symlink aliases
/// (macOS `/var` → `/private/var`, Windows `\\?\` prefixes after canonicalize).
pub fn same_project_dir(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

/// Deterministic, filesystem-safe id for a run started at `started_at`. The
/// process id suffix avoids collisions when multiple honya instances start in
/// the same millisecond.
pub fn make_run_id(started_at: DateTime<Utc>) -> String {
    format!(
        "run-{}-{}",
        started_at.format("%Y%m%dT%H%M%S%.3fZ"),
        std::process::id()
    )
}

/// Directory holding per-project recovery checkpoints.
///
/// Honors `HONYA_SESSION_DIR` (absolute directory override). For older tests /
/// scripts, `HONYA_SESSION_FILE` pointing at a `…/session.json` path redirects
/// into that file's parent directory (the basename is ignored).
pub fn dir() -> PathBuf {
    if let Some(p) = std::env::var_os("HONYA_SESSION_DIR") {
        let p = PathBuf::from(p);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    if let Some(p) = std::env::var_os("HONYA_SESSION_FILE") {
        let p = PathBuf::from(p);
        if !p.as_os_str().is_empty() {
            if let Some(parent) = p.parent()
                && !parent.as_os_str().is_empty()
            {
                return parent.to_path_buf();
            }
            return p;
        }
    }
    crate::config::config_dir().join("sessions")
}

/// Legacy single-file checkpoint path (`<config>/session.json`), still read on
/// startup for migration into the per-project directory.
pub fn legacy_path() -> PathBuf {
    crate::config::config_dir().join("session.json")
}

/// Stable filename slot for `project_dir` inside [`dir`].
pub fn path_for(project_dir: &Path) -> PathBuf {
    dir().join(format!("{}.json", slot_key(project_dir)))
}

fn slot_key(project_dir: &Path) -> String {
    let canon = std::fs::canonicalize(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
    let mut hasher = DefaultHasher::new();
    canon.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Persist `cp` into its project slot (atomic temp+rename so a crash mid-write
/// never leaves a torn file). Other projects' slots are left untouched.
pub fn save(cp: &SessionCheckpoint) -> std::io::Result<()> {
    let path = path_for(&cp.project_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    save_at(&path, cp)
}

/// Load every on-disk checkpoint (per-project slots + a one-shot legacy migrate).
pub fn load_all() -> Vec<SessionCheckpoint> {
    migrate_legacy_session_file();
    let dir = dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Some(cp) = load_at(&path) {
            out.push(cp);
        }
    }
    out
}

/// Load the checkpoint for `project_dir`, if any.
pub fn load_for(project_dir: &Path) -> Option<SessionCheckpoint> {
    migrate_legacy_session_file();
    let path = path_for(project_dir);
    if let Some(cp) = load_at(&path)
        && same_project_dir(&cp.project_dir, project_dir)
    {
        return Some(cp);
    }
    // Hash collision or moved project: fall through to a scan.
    load_all()
        .into_iter()
        .find(|cp| same_project_dir(&cp.project_dir, project_dir))
}

/// Live foreign runs (another PID, fresh heartbeat), newest first.
pub fn load_live_foreign() -> Vec<SessionCheckpoint> {
    let mut live: Vec<_> = load_all()
        .into_iter()
        .filter(|cp| cp.is_live_elsewhere())
        .collect();
    live.sort_by_key(|cp| std::cmp::Reverse(cp.started_at));
    live
}

/// Interrupted (not live elsewhere) checkpoints that are still resumable, newest first.
pub fn load_resumable() -> Vec<SessionCheckpoint> {
    let mut out = Vec::new();
    for mut cp in load_all() {
        if cp.is_live_elsewhere() {
            continue;
        }
        cp.ensure_run_id();
        if cp.is_resumable() {
            out.push(cp);
        } else {
            clear_for(&cp.project_dir);
        }
    }
    out.sort_by_key(|cp| std::cmp::Reverse(cp.started_at));
    out
}

/// Remove the recovery checkpoint for `project_dir` (best-effort).
pub fn clear_for(project_dir: &Path) {
    clear_at(&path_for(project_dir));
    // Also drop any slot whose embedded project_dir matches (hash mismatch / rename).
    let dir = dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Some(cp) = load_at(&path)
            && same_project_dir(&cp.project_dir, project_dir)
        {
            clear_at(&path);
        }
    }
}

/// If a pre-multi-slot `session.json` still exists, move it into a per-project
/// slot once and delete the legacy file.
fn migrate_legacy_session_file() {
    let legacy = legacy_path();
    // When tests redirect via HONYA_SESSION_FILE/DIR, skip touching the real
    // config-dir legacy path — only migrate when we're on the default layout
    // and the legacy file sits beside the new sessions directory.
    if std::env::var_os("HONYA_SESSION_DIR").is_some()
        || std::env::var_os("HONYA_SESSION_FILE").is_some()
    {
        // Still accept a bare SessionCheckpoint written at the redirected path
        // basename used by older tests (`…/session.json`).
        if let Some(p) = std::env::var_os("HONYA_SESSION_FILE") {
            let p = PathBuf::from(p);
            if p.is_file()
                && let Some(cp) = load_at(&p)
            {
                let _ = save(&cp);
                let _ = std::fs::remove_file(&p);
            }
        }
        return;
    }
    if !legacy.is_file() {
        return;
    }
    if let Some(cp) = load_at(&legacy) {
        let _ = save(&cp);
    }
    let _ = std::fs::remove_file(&legacy);
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
thread_local! {
    static LIVE_ELSEWHERE_OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn live_elsewhere_override() -> Option<bool> {
    LIVE_ELSEWHERE_OVERRIDE.with(|c| c.get())
}

/// Test-only: force [`SessionCheckpoint::is_live_elsewhere`] to `Some(true/false)`,
/// or `None` to restore real PID/heartbeat checks. Nested calls restore the prior value.
#[cfg(test)]
pub fn with_live_elsewhere_override<R>(value: Option<bool>, f: impl FnOnce() -> R) -> R {
    LIVE_ELSEWHERE_OVERRIDE.with(|c| {
        let prev = c.replace(value);
        let out = f();
        c.set(prev);
        out
    })
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
        assert_eq!(loaded.pid, std::process::id());
        assert!(heartbeat_fresh(loaded.heartbeat_at));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn distinct_project_dirs_get_distinct_slot_files() {
        let root = scratch("slots");
        let a = root.join("proj_a");
        let b = root.join("proj_b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        assert_ne!(
            slot_key(&a),
            slot_key(&b),
            "different projects must not share a checkpoint filename"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn whole_project_flag_round_trips() {
        let dir = scratch("whole_project");
        let file = dir.join("session.json");
        let mut cp = sample(dir.clone());
        cp.whole_project = true;

        save_at(&file, &cp).unwrap();
        let loaded = load_at(&file).expect("checkpoint loads back");
        assert!(
            loaded.whole_project,
            "whole_project flag survives a round-trip"
        );
        assert_eq!(loaded, cp);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_checkpoint_defaults_pid_and_epoch_heartbeat() {
        let dir = scratch("legacy");
        let file = dir.join("session.json");
        // Pre-liveness shape (no pid / heartbeat_at). Build via Value so Windows
        // backslashes in project_dir are JSON-escaped correctly.
        let json = serde_json::json!({
            "version": 1,
            "project_dir": dir,
            "project_id": "re-zero",
            "project_title": "Re:Zero",
            "vol": 1,
            "chapters": [1],
            "started_at": "2024-01-01T00:00:00Z",
            "run_id": "run-old",
            "honya_version": "0.1.0",
            "whole_project": false
        });
        std::fs::write(&file, serde_json::to_string_pretty(&json).unwrap()).unwrap();
        let loaded = load_at(&file).expect("legacy loads");
        assert_eq!(loaded.pid, 0);
        assert_eq!(loaded.heartbeat_at, DateTime::<Utc>::UNIX_EPOCH);
        assert!(
            !loaded.is_live_elsewhere(),
            "legacy checkpoints are interrupted, not live elsewhere"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn own_pid_is_never_live_elsewhere() {
        let cp = sample(scratch("own_pid"));
        assert_eq!(cp.pid, std::process::id());
        assert!(!cp.is_live_elsewhere());
    }

    #[test]
    fn live_elsewhere_respects_override_and_foreign_pid() {
        let mut cp = sample(scratch("override"));
        cp.pid = std::process::id().wrapping_add(999_991).max(1);
        with_live_elsewhere_override(Some(true), || {
            assert!(cp.is_live_elsewhere());
        });
        with_live_elsewhere_override(Some(false), || {
            assert!(!cp.is_live_elsewhere());
        });
    }

    #[test]
    fn touch_heartbeat_advances_timestamp() {
        let mut cp = sample(scratch("touch"));
        let before = cp.heartbeat_at;
        std::thread::sleep(std::time::Duration::from_millis(5));
        cp.touch_heartbeat();
        assert!(cp.heartbeat_at > before);
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
    fn same_project_dir_follows_symlink_aliases() {
        let dir = scratch("same_dir");
        let real = dir.join("real_project");
        std::fs::create_dir_all(&real).unwrap();
        let alias = dir.join("alias_project");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&real, &alias).unwrap();
            assert!(
                same_project_dir(&real, &alias),
                "symlink alias must match the real project dir"
            );
        }
        #[cfg(not(unix))]
        {
            // No symlink fixture on this target; exact-path equality still holds.
            assert!(same_project_dir(&real, &real));
            let _ = alias;
        }
        assert!(same_project_dir(&real, &real));
        assert!(!same_project_dir(&real, &dir.join("other")));

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
