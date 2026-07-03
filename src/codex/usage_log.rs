//! Append Codex-compatible session JSONL so tools like ccusage can aggregate usage.
//!
//! ccusage reads `token_count` events (with cumulative `total_token_usage`) and
//! the preceding `turn_context` model from `${CODEX_HOME}/sessions/**/*.jsonl`.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use chrono::{Datelike, Local, SecondsFormat, Utc};
use serde::Serialize;

use super::{codex_home, uuid_v4};

#[cfg_attr(test, allow(dead_code))]
static SHARED: OnceLock<Arc<UsageLog>> = OnceLock::new();

/// Per-turn token breakdown from a Codex Responses API completion.
#[derive(Debug, Clone, Copy, Default)]
pub struct TurnUsage {
    pub input_tokens: u32,
    pub cached_input_tokens: u32,
    pub output_tokens: u32,
    pub reasoning_output_tokens: u32,
    pub total_tokens: u32,
}

impl TurnUsage {
    pub fn is_empty(self) -> bool {
        self.input_tokens == 0
            && self.cached_input_tokens == 0
            && self.output_tokens == 0
            && self.reasoning_output_tokens == 0
    }
}

/// One process-wide session log under [`codex_home()`].
#[cfg_attr(test, allow(dead_code))]
pub struct UsageLog {
    file: Mutex<Option<fs::File>>,
    totals: Mutex<TokenTotals>,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
struct TokenTotals {
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
    total_tokens: u64,
}

#[derive(Serialize)]
struct LogLine<'a, P> {
    timestamp: &'a str,
    #[serde(rename = "type")]
    line_type: &'a str,
    payload: P,
}

#[derive(Serialize)]
struct SessionMeta<'a> {
    session_id: &'a str,
    originator: &'a str,
    #[serde(rename = "cli_version")]
    cli_version: &'a str,
    source: &'a str,
}

#[derive(Serialize)]
struct TurnContext<'a> {
    model: &'a str,
    cwd: &'a str,
}

#[derive(Serialize)]
struct EventMsg<'a, P> {
    #[serde(rename = "type")]
    payload_type: &'a str,
    info: P,
}

#[derive(Serialize)]
struct TokenCountInfo<'a> {
    model: &'a str,
    last_token_usage: TokenTotals,
    total_token_usage: TokenTotals,
}

impl UsageLog {
    #[cfg_attr(test, allow(dead_code))]
    pub fn shared() -> Arc<Self> {
        SHARED
            .get_or_init(|| Arc::new(Self::open().unwrap_or_else(|_| Self::disabled())))
            .clone()
    }

    #[cfg_attr(test, allow(dead_code))]
    fn disabled() -> Self {
        Self {
            file: Mutex::new(None),
            totals: Mutex::new(TokenTotals::default()),
        }
    }

    fn open() -> std::io::Result<Self> {
        let home = codex_home().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "no codex home")
        })?;
        let now_local = Local::now();
        let dir = home.join("sessions").join(format!(
            "{:04}/{:02}/{:02}",
            now_local.year(),
            now_local.month(),
            now_local.day()
        ));
        fs::create_dir_all(&dir)?;
        let session_id = uuid_v4();
        let rollout_ts = now_local.format("%Y-%m-%dT%H-%M-%S");
        let path = dir.join(format!("rollout-{rollout_ts}-{session_id}.jsonl"));
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        write_line(
            &mut file,
            "session_meta",
            SessionMeta {
                session_id: &session_id,
                originator: "honya",
                cli_version: env!("CARGO_PKG_VERSION"),
                source: "honya",
            },
        )?;
        Ok(Self {
            file: Mutex::new(Some(file)),
            totals: Mutex::new(TokenTotals::default()),
        })
    }

    /// Append a `turn_context` + `token_count` pair for one Codex API call.
    pub fn record_turn(&self, model: &str, turn: TurnUsage) {
        if turn.is_empty() {
            return;
        }
        let Ok(mut guard) = self.file.lock() else {
            return;
        };
        let Some(file) = guard.as_mut() else {
            return;
        };
        let cwd = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .display()
            .to_string();
        if write_line(
            file,
            "turn_context",
            TurnContext { model, cwd: &cwd },
        )
        .is_err()
        {
            *guard = None;
            return;
        }
        let last = TokenTotals {
            input_tokens: turn.input_tokens as u64,
            cached_input_tokens: turn.cached_input_tokens as u64,
            output_tokens: turn.output_tokens as u64,
            reasoning_output_tokens: turn.reasoning_output_tokens as u64,
            total_tokens: turn.total_tokens as u64,
        };
        let mut totals = self.totals.lock().unwrap_or_else(|e| e.into_inner());
        totals.input_tokens = totals.input_tokens.saturating_add(last.input_tokens);
        totals.cached_input_tokens = totals
            .cached_input_tokens
            .saturating_add(last.cached_input_tokens);
        totals.output_tokens = totals.output_tokens.saturating_add(last.output_tokens);
        totals.reasoning_output_tokens = totals
            .reasoning_output_tokens
            .saturating_add(last.reasoning_output_tokens);
        totals.total_tokens = totals.total_tokens.saturating_add(last.total_tokens);
        let info = TokenCountInfo {
            model,
            last_token_usage: last,
            total_token_usage: *totals,
        };
        if write_line(
            file,
            "event_msg",
            EventMsg {
                payload_type: "token_count",
                info,
            },
        )
        .is_err()
        {
            *guard = None;
            return;
        }
        let _ = file.flush();
    }
}

fn write_line<P: Serialize>(
    file: &mut fs::File,
    line_type: &str,
    payload: P,
) -> std::io::Result<()> {
    let ts = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let line = LogLine {
        timestamp: &ts,
        line_type,
        payload,
    };
    serde_json::to_writer(&mut *file, &line)?;
    file.write_all(b"\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as TestMutex;

    static TEST_LOCK: TestMutex<()> = TestMutex::new(());

    #[test]
    fn writes_ccusage_compatible_token_count_lines() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("honya_codex_usage_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let prev = std::env::var_os("CODEX_HOME");
        // SAFETY: test mutex serializes env mutation.
        unsafe { std::env::set_var("CODEX_HOME", &dir) };
        let log = UsageLog::open().unwrap();
        log.record_turn(
            "gpt-5.5-codex",
            TurnUsage {
                input_tokens: 1000,
                cached_input_tokens: 250,
                output_tokens: 125,
                reasoning_output_tokens: 75,
                total_tokens: 1200,
            },
        );
        log.record_turn(
            "gpt-5.5-codex",
            TurnUsage {
                input_tokens: 1500,
                cached_input_tokens: 250,
                output_tokens: 300,
                reasoning_output_tokens: 200,
                total_tokens: 2000,
            },
        );
        let files: Vec<_> = walkdir::WalkDir::new(&dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
            .map(|e| e.path().to_path_buf())
            .collect();
        assert_eq!(files.len(), 1);
        let raw = std::fs::read_to_string(&files[0]).unwrap();
        let lines: Vec<serde_json::Value> = raw
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines[0]["type"], "session_meta");
        assert_eq!(lines[1]["type"], "turn_context");
        assert_eq!(lines[1]["payload"]["model"], "gpt-5.5-codex");
        assert_eq!(lines[2]["type"], "event_msg");
        assert_eq!(lines[2]["payload"]["type"], "token_count");
        assert_eq!(lines[2]["payload"]["info"]["last_token_usage"]["input_tokens"], 1000);
        assert_eq!(lines[2]["payload"]["info"]["total_token_usage"]["input_tokens"], 1000);
        assert_eq!(lines[4]["payload"]["info"]["total_token_usage"]["input_tokens"], 2500);
        match prev {
            Some(v) => unsafe { std::env::set_var("CODEX_HOME", v) },
            None => unsafe { std::env::remove_var("CODEX_HOME") },
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
