//! Drives the per-chapter / per-chunk pipeline and emits the UI `AppEvent` stream.
//! Image-only chapters skip agents; prose runs translate → audit → review. Approved
//! Thai is appended app-side, not via an LLM tool; exhausted retries are committed
//! with a review-needed marker so the chapter can finish.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::agents::audit::{
    advisory_findings_with_references, audit_character_pronoun_rules, audit_translation_with_terms,
    normalize_japanese_punctuation_residue, strip_copied_continuity,
};
use crate::agents::chunk::{Chunk, chunk_chapter};
use crate::agents::coherence;
use crate::agents::continuity;
use crate::agents::prepass;
use crate::agents::prompts::{ORCHESTRATOR_SYSTEM, build_orchestrator_metadata_msg};
use crate::agents::reviewer::review_chunk;
use crate::agents::tools::{WorkspaceTools, orchestrator_tools};
use crate::agents::translator::{TranslatorStreamError, translate_chunk_streaming};
use crate::cleanse;
use crate::llm::client::{ClientSet, LlmClient};
use crate::llm::tool_loop::run_tool_loop;
use crate::llm::{ChatRequest, Message, Tool, Usage};
use crate::model::{
    AgentModel, AgentRole, AppConfig, AppEvent, ChapterStatus, ChunkState, ContinuityNote, EventTx,
    GlossaryTerm, LogLevel, ModelSet, ReviewVerdict, ReviewerOut, ServiceTier, TokenUsage,
    TranslatorOut, UsageStats,
};
use crate::workspace::{Workspace, characters, data_block, glossary, translation, volume};

/// Shared, cheap-to-clone run control toggled by the UI (p pause / s stop) and
/// polled by the pipeline between chunks. 0 = running, 1 = paused, 2 = stopped.
#[derive(Clone)]
pub struct RunControl(Arc<AtomicU8>);

impl RunControl {
    pub fn new() -> Self {
        Self(Arc::new(AtomicU8::new(0)))
    }
    pub fn stop(&self) {
        self.0.store(2, Ordering::Relaxed);
    }
    /// Pause↔resume toggle (no effect once stopped).
    pub fn toggle_pause(&self) {
        let _ = self
            .0
            .compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed)
            .or_else(|_| {
                self.0
                    .compare_exchange(1, 0, Ordering::Relaxed, Ordering::Relaxed)
            });
    }
    pub fn is_paused(&self) -> bool {
        self.0.load(Ordering::Relaxed) == 1
    }
    pub fn is_stopped(&self) -> bool {
        self.0.load(Ordering::Relaxed) == 2
    }
}

impl Default for RunControl {
    fn default() -> Self {
        Self::new()
    }
}

/// Queue shared by the UI and pipeline. Items are `(vol, chapter)` because chapter
/// numbers repeat across volumes.
///
/// `running` is kept out of `pending`; UI mutations only touch `pending`, so the
/// active chapter cannot be reordered or removed.
#[derive(Clone, Default)]
pub struct ChapterQueue(Arc<Mutex<QueueInner>>);

#[derive(Default)]
struct QueueInner {
    pending: VecDeque<(u32, u32)>,
    running: Option<(u32, u32)>,
}

impl ChapterQueue {
    pub fn new(items: Vec<(u32, u32)>) -> Self {
        Self(Arc::new(Mutex::new(QueueInner {
            pending: items.into_iter().collect(),
            running: None,
        })))
    }

    pub fn seed(&self, items: Vec<(u32, u32)>) {
        let mut g = self.0.lock().unwrap();
        for it in items {
            if g.running == Some(it) || g.pending.contains(&it) {
                continue;
            }
            g.pending.push_back(it);
        }
    }

    pub fn next(&self) -> Option<(u32, u32)> {
        let mut g = self.0.lock().unwrap();
        g.running = g.pending.pop_front();
        g.running
    }

    /// Pop the next item for `vol`, leaving other volumes queued.
    pub fn next_for(&self, vol: u32) -> Option<(u32, u32)> {
        let mut g = self.0.lock().unwrap();
        match g.pending.iter().position(|(v, _)| *v == vol) {
            Some(pos) => {
                let it = g.pending.remove(pos);
                g.running = it;
                it
            }
            None => {
                g.running = None;
                None
            }
        }
    }

    pub fn push_back(&self, vol: u32, ch: u32) -> bool {
        let mut g = self.0.lock().unwrap();
        let it = (vol, ch);
        if g.running == Some(it) || g.pending.contains(&it) {
            return false;
        }
        g.pending.push_back(it);
        true
    }

    /// Move a pending item by identity, never by UI position.
    pub fn move_item_up(&self, vol: u32, ch: u32) {
        let mut g = self.0.lock().unwrap();
        if let Some(pos) = g.pending.iter().position(|&it| it == (vol, ch))
            && pos > 0
        {
            g.pending.swap(pos, pos - 1);
        }
    }

    pub fn move_item_down(&self, vol: u32, ch: u32) {
        let mut g = self.0.lock().unwrap();
        if let Some(pos) = g.pending.iter().position(|&it| it == (vol, ch))
            && pos + 1 < g.pending.len()
        {
            g.pending.swap(pos, pos + 1);
        }
    }

    pub fn sort_by_number(&self) {
        let mut g = self.0.lock().unwrap();
        g.pending.make_contiguous().sort_unstable();
    }

    pub fn remove_item(&self, vol: u32, ch: u32) -> bool {
        let mut g = self.0.lock().unwrap();
        if let Some(pos) = g.pending.iter().position(|&it| it == (vol, ch)) {
            g.pending.remove(pos);
            true
        } else {
            false
        }
    }

    pub fn snapshot(&self) -> QueueSnapshot {
        let g = self.0.lock().unwrap();
        (g.running, g.pending.iter().copied().collect())
    }
}

pub type QueueSnapshot = (Option<(u32, u32)>, Vec<(u32, u32)>);

/// Why the chapter-level watchdog tripped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LoopReason {
    /// The chapter made no pipeline progress for `loop_stall_secs`.
    Stall,
}

impl LoopReason {
    fn describe(self) -> &'static str {
        match self {
            LoopReason::Stall => "no progress for too long",
        }
    }
}

// Repetition-detector tuning. A degenerate loop repeats a short unit many times;
// these thresholds are high enough that ordinary prose (even with a refrain) does
// not trip, while a model spinning on the same phrase does.
const REP_WINDOW: usize = 400; // chars of streamed tail examined
const REP_MIN_UNIT: usize = 4; // shortest repeating unit considered
const REP_MIN_REPEATS: usize = 8; // consecutive copies of the unit to call it a loop
const REP_MIN_TOTAL: usize = 48; // don't judge until this much text has streamed
const REP_MIN_LINES: usize = 6; // identical consecutive lines to call it a loop
const REP_MIN_LINE_LEN: usize = 3; // ignore repeated blank/tiny lines
const REP_CHECK_EVERY: usize = 48; // re-run the (bounded) scan every N new chars
const STALL_EXTERNAL_WAIT_GRACE: u32 = 2; // active model calls get one extra window

/// True when the tail of streamed text looks like a degenerate repetition loop:
/// either a short character-level cycle repeated many times, or the same non-empty
/// line repeated several times in a row. Pure + bounded (examines only the last
/// [`REP_WINDOW`] chars) so it is cheap to call as text streams in.
fn looks_like_repetition_loop(text: &str) -> bool {
    let trimmed = text.trim_end();
    if trimmed.chars().count() < REP_MIN_TOTAL {
        return false;
    }

    // Character-cycle check over the last REP_WINDOW chars.
    let window: Vec<char> = {
        let mut rev: Vec<char> = trimmed.chars().rev().take(REP_WINDOW).collect();
        rev.reverse();
        rev
    };
    let n = window.len();
    let max_unit = n / REP_MIN_REPEATS;
    let mut p = REP_MIN_UNIT;
    while p <= max_unit {
        let needed = p * REP_MIN_REPEATS;
        let start = n - needed;
        let unit = &window[start..start + p];
        let is_cycle =
            (1..REP_MIN_REPEATS).all(|r| &window[start + r * p..start + (r + 1) * p] == unit);
        if is_cycle && repeating_unit_has_signal(unit) {
            return true;
        }
        p += 1;
    }

    // Line-repeat check: the last REP_MIN_LINES non-empty lines are all identical.
    let lines: Vec<&str> = trimmed
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.len() >= REP_MIN_LINES {
        let tail = &lines[lines.len() - REP_MIN_LINES..];
        let first = tail[0];
        if first.chars().count() >= REP_MIN_LINE_LEN && tail.iter().all(|l| *l == first) {
            return true;
        }
    }
    false
}

fn repeating_unit_has_signal(unit: &[char]) -> bool {
    let mut distinct = Vec::new();
    for &ch in unit {
        if ch.is_whitespace() || is_combining_mark(ch) {
            continue;
        }
        if !distinct.contains(&ch) {
            distinct.push(ch);
            if distinct.len() >= 2 {
                return true;
            }
        }
    }
    false
}

fn is_combining_mark(ch: char) -> bool {
    matches!(
        ch as u32,
        0x0300..=0x036F
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x20D0..=0x20FF
            | 0xFE20..=0xFE2F
            | 0x0E31
            | 0x0E34..=0x0E3A
            | 0x0E47..=0x0E4E
    )
}

/// Detects quiet pipeline stalls and repeated Translator streams. Active model-call
/// stalls and repeated streamed output are handled inside the active chunk first;
/// chapter-level recovery is reserved for stalls outside a tracked chunk attempt.
struct Watchdog {
    /// Master switch (`loop_stall_secs > 0`). When off, neither arm trips.
    enabled: bool,
    /// Stall window; `None` disables only the time arm (repetition still runs).
    stall: Option<Duration>,
    /// Last time the pipeline reported progress (a step or a streamed delta).
    last_progress: Mutex<Instant>,
    /// Active external model calls. These can be legitimately quiet for a while.
    external_waits: AtomicU32,
    /// Rolling tail of the current chunk's streamed Thai (bounded).
    repeat_buf: Mutex<String>,
    /// Chars accumulated since the last repetition scan (throttle).
    since_check: Mutex<usize>,
    /// Set once the repetition detector fires; cleared per chunk/chapter.
    repetition: AtomicBool,
}

impl Watchdog {
    fn new(cfg: &AppConfig) -> Self {
        let enabled = cfg.loop_stall_secs > 0;
        let stall = enabled.then(|| Duration::from_secs(cfg.loop_stall_secs));
        Self {
            enabled,
            stall,
            last_progress: Mutex::new(Instant::now()),
            external_waits: AtomicU32::new(0),
            repeat_buf: Mutex::new(String::new()),
            since_check: Mutex::new(0),
            repetition: AtomicBool::new(false),
        }
    }

    /// Construct with an explicit stall window, watchdog enabled (tests use a
    /// sub-second value so the stall arm trips without a real multi-second wait).
    #[cfg(test)]
    fn with_stall(stall: Option<Duration>) -> Self {
        Self {
            enabled: true,
            stall,
            last_progress: Mutex::new(Instant::now()),
            external_waits: AtomicU32::new(0),
            repeat_buf: Mutex::new(String::new()),
            since_check: Mutex::new(0),
            repetition: AtomicBool::new(false),
        }
    }

    /// Record pipeline progress (resets the stall timer).
    fn ping(&self) {
        *self.last_progress.lock().unwrap() = Instant::now();
    }

    fn external_wait(&self) -> WatchdogExternalWait<'_> {
        self.external_waits.fetch_add(1, Ordering::Relaxed);
        self.ping();
        WatchdogExternalWait { wd: self }
    }

    /// Feed a streamed Translator delta: counts as progress and feeds the
    /// repetition detector (a loop streams plenty, so it must not look like a stall).
    fn feed_stream(&self, delta: &str) {
        self.ping();
        if !self.enabled || self.repetition.load(Ordering::Relaxed) {
            return;
        }
        let mut buf = self.repeat_buf.lock().unwrap();
        buf.push_str(delta);
        // Keep only the tail we examine, on a char boundary.
        let cap = REP_WINDOW * 2;
        if buf.chars().count() > cap {
            let keep: String = buf
                .chars()
                .rev()
                .take(cap)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            *buf = keep;
        }
        let mut since = self.since_check.lock().unwrap();
        *since += delta.chars().count();
        if *since >= REP_CHECK_EVERY {
            *since = 0;
            if looks_like_repetition_loop(&buf) {
                self.repetition.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Reset the per-chunk repetition state (new chunk, same chapter).
    fn reset_chunk(&self) {
        self.repeat_buf.lock().unwrap().clear();
        *self.since_check.lock().unwrap() = 0;
        self.repetition.store(false, Ordering::Relaxed);
        self.ping();
    }

    /// Reset everything for a fresh chapter attempt.
    fn reset_chapter(&self) {
        self.reset_chunk();
    }

    fn repetition_triggered(&self) -> bool {
        self.repetition.load(Ordering::Relaxed)
    }

    /// Resolve as soon as the active chunk's stream looks like a repetition loop.
    async fn watch_repetition(&self, ctl: &RunControl) {
        if !self.enabled {
            std::future::pending::<()>().await;
        }
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if ctl.is_paused() || ctl.is_stopped() {
                continue;
            }
            if self.repetition_triggered() {
                return;
            }
        }
    }

    async fn watch_active_call_stall(&self, ctl: &RunControl) -> LoopReason {
        self.watch_stall(ctl, true).await
    }

    /// Resolve as soon as the chapter stalls outside an active model call. Polls a
    /// few times a second; treats a paused/stopped run as progress so it does not
    /// falsely read as a stall.
    async fn watch(&self, ctl: &RunControl) -> LoopReason {
        self.watch_stall(ctl, false).await
    }

    async fn watch_stall(&self, ctl: &RunControl, include_external_waits: bool) -> LoopReason {
        if !self.enabled {
            std::future::pending::<()>().await;
        }
        let poll = self
            .stall
            .map(|s| (s / 4).clamp(Duration::from_millis(100), Duration::from_millis(500)))
            .unwrap_or(Duration::from_millis(250));
        loop {
            tokio::time::sleep(poll).await;
            if ctl.is_paused() || ctl.is_stopped() {
                self.ping();
                continue;
            }
            if let Some(stall) = self.stall {
                let idle = self.last_progress.lock().unwrap().elapsed();
                let external_waiting = self.external_waits.load(Ordering::Relaxed) > 0;
                let deadline = if external_waiting && include_external_waits {
                    stall.saturating_mul(STALL_EXTERNAL_WAIT_GRACE)
                } else if external_waiting {
                    continue;
                } else {
                    stall
                };
                if idle >= deadline {
                    return LoopReason::Stall;
                }
            }
        }
    }
}

struct WatchdogExternalWait<'a> {
    wd: &'a Watchdog,
}

impl Drop for WatchdogExternalWait<'_> {
    fn drop(&mut self) {
        self.wd.external_waits.fetch_sub(1, Ordering::Relaxed);
        self.wd.ping();
    }
}

/// One volume's slice of an auto project-translate run: the volume number, its
/// optional label (for the `VolumeStarted` UI event), and the chapter queue.
#[derive(Clone, Debug)]
pub struct VolumePlan {
    pub vol: u32,
    pub label: Option<String>,
    pub chapters: Vec<u32>,
}

/// Running per-run chapter tallies, summed across every volume of a project run.
#[derive(Default)]
struct Totals {
    done: u32,
    failed: u32,
    need_review: u32,
}

/// How a volume's chapter loop ended.
enum Halt {
    /// All chapters processed; the run may continue to the next volume.
    Completed,
    /// The user stopped the run, or a chapter looped past its re-translate limit.
    /// Either way the whole run halts.
    Stopped,
}

/// How a chapter finished: ran to completion, completed with ≥1 chunk committed
/// unreviewed (flagged for a human), the user stopped the run, or it looped past
/// its re-translate limit (which aborts the whole run).
enum Outcome {
    Completed,
    NeedsReview,
    Stopped,
    Aborted { reason: String },
}

/// How a single chunk resolved: committed after approval, or committed unreviewed
/// after exhausting its review attempts (the resilient path).
enum ChunkOutcome {
    Committed,
    NeedsReview,
}

enum TranslatorAttemptRun {
    Finished(Box<Result<(TranslatorOut, Usage, bool), TranslatorStreamError>>),
    Repeated,
    Stalled(LoopReason),
}

enum ReviewerAttemptRun {
    Finished(crate::llm::client::Result<(ReviewerOut, Usage)>),
    Stalled(LoopReason),
}

/// Everything one pipeline run needs: the per-provider clients, the project
/// workspace, the model set, the runtime config, the UI event channel, and the
/// shared pause/stop control. Each agent routes to the client for its configured
/// provider via [`ClientSet::for_agent`].
pub struct PipelineCtx {
    pub clients: ClientSet,
    pub ws: Workspace,
    pub models: ModelSet,
    pub cfg: AppConfig,
    pub tx: EventTx,
    pub ctl: RunControl,
    /// The live chapter queue this run drains. Shared with the UI so chapters can be
    /// enqueued / reordered while the run is in flight (see [`ChapterQueue`]).
    pub queue: ChapterQueue,
}

impl PipelineCtx {
    /// Resolve the live client for an agent's configured provider, or an error
    /// naming the missing provider (the run preflight normally catches this).
    fn client_for(&self, agent: &AgentModel) -> anyhow::Result<Arc<dyn LlmClient>> {
        self.clients.for_agent(agent).ok_or_else(|| {
            anyhow::anyhow!(
                "no API key configured for {} (model {})",
                agent.provider.label(),
                agent.model
            )
        })
    }

    /// Derive the 1-based volume number from the workspace's `Vol_NN` directory
    /// name so the Orchestrator tool executor can rebuild a fresh `Workspace`.
    fn vol_number(&self) -> u32 {
        self.ws
            .vol_dir
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(|name| name.strip_prefix("Vol_"))
            .and_then(|digits| digits.trim_start_matches('0').parse::<u32>().ok())
            .unwrap_or(1)
    }

    /// A clone of this context re-pointed at volume `vol` (same project root,
    /// shared client / config / event channel / run control). Used by the auto
    /// project-translate run to step through volumes with one set of controls.
    fn with_volume(&self, vol: u32) -> PipelineCtx {
        PipelineCtx {
            clients: self.clients.clone(),
            ws: Workspace::new(self.ws.root.clone(), vol),
            models: self.models.clone(),
            cfg: self.cfg.clone(),
            tx: self.tx.clone(),
            ctl: self.ctl.clone(),
            queue: self.queue.clone(),
        }
    }
}

/// Run the pipeline across `chapters` of one volume (in the given order),
/// emitting the full `AppEvent` sequence. A per-chapter failure is reported as
/// `ChapterFailed` but does NOT abort the run; `PipelineFinished` always fires at
/// the end.
pub async fn run_pipeline(ctx: PipelineCtx, chapters: Vec<u32>) -> anyhow::Result<()> {
    let wd = Watchdog::new(&ctx.cfg);
    let mut acc = Acc::default();
    let mut totals = Totals::default();
    let vol = ctx.vol_number();
    ctx.queue
        .seed(chapters.into_iter().map(|c| (vol, c)).collect());
    maybe_run_prepass(&ctx, &mut acc).await;
    let halt = run_volume_chapters(&ctx, None, &wd, &mut acc, &mut totals).await;
    ctx.tx.send(AppEvent::PipelineFinished {
        chapters_done: totals.done,
        chapters_failed: totals.failed,
        chapters_need_review: totals.need_review,
        stopped: matches!(halt, Halt::Stopped),
        run: acc.run,
    });
    Ok(())
}

/// Run the auto project-translate: every volume's queued chapters in `plan` order,
/// under one shared run control / watchdog / cost accumulator, emitting a single
/// `PipelineFinished` at the very end. A `VolumeStarted` precedes each volume so
/// the UI re-points its running volume (chapter numbers repeat across volumes).
/// Stop and a loop-abort halt the whole project, not just the current volume.
pub async fn run_project_pipeline(ctx: PipelineCtx, plan: Vec<VolumePlan>) -> anyhow::Result<()> {
    let wd = Watchdog::new(&ctx.cfg);
    let mut acc = Acc::default();
    let mut totals = Totals::default();
    let mut stopped = false;

    for vp in &plan {
        if ctx.ctl.is_stopped() {
            stopped = true;
            break;
        }
        let vctx = ctx.with_volume(vp.vol);
        vctx.queue
            .seed(vp.chapters.iter().map(|&c| (vp.vol, c)).collect());
        vctx.tx.send(AppEvent::VolumeStarted {
            vol: vp.vol,
            label: vp.label.clone(),
        });
        vctx.tx.send(AppEvent::Log {
            level: LogLevel::Info,
            msg: format!(
                "translating Vol.{:02} · {} chapter(s)",
                vp.vol,
                vp.chapters.len()
            ),
        });
        maybe_run_prepass(&vctx, &mut acc).await;
        let halt = run_volume_chapters(&vctx, Some(vp.vol), &wd, &mut acc, &mut totals).await;
        if matches!(halt, Halt::Stopped) {
            stopped = true;
            break;
        }
    }

    // Catch live-added chapters in volumes the plan already passed or never listed.
    while !stopped && !ctx.ctl.is_stopped() {
        let (_, pending) = ctx.queue.snapshot();
        let Some(&(vol, _)) = pending.first() else {
            break;
        };
        let vctx = ctx.with_volume(vol);
        vctx.tx.send(AppEvent::VolumeStarted { vol, label: None });
        maybe_run_prepass(&vctx, &mut acc).await;
        let halt = run_volume_chapters(&vctx, Some(vol), &wd, &mut acc, &mut totals).await;
        if matches!(halt, Halt::Stopped) {
            stopped = true;
            break;
        }
    }

    ctx.tx.send(AppEvent::PipelineFinished {
        chapters_done: totals.done,
        chapters_failed: totals.failed,
        chapters_need_review: totals.need_review,
        stopped,
        run: acc.run,
    });
    Ok(())
}

/// Run the per-volume pre-extraction pass once, seeding CHARACTERS.md / GLOSSARY.md
/// and a few style exemplars before chunk 1 so early chapters get the same context
/// depth as late ones. Idempotent via `VolumeData.prepass_done`; best-effort (a
/// failure logs and the run proceeds). Its spend folds into the run total only — it
/// is not a chapter's cost.
async fn maybe_run_prepass(ctx: &PipelineCtx, acc: &mut Acc) {
    if !ctx.cfg.prepass_extract || ctx.ctl.is_stopped() {
        return;
    }
    if volume::load(&ctx.ws).prepass_done {
        return;
    }
    let vol = ctx.vol_number();
    ctx.tx.send(AppEvent::PrepassStarted { vol });
    ctx.tx.send(AppEvent::Log {
        level: LogLevel::Info,
        msg: "pre-scan: extracting characters & terms before translating".to_string(),
    });
    let prepass_client = match ctx.client_for(&ctx.models.translator) {
        Ok(c) => c,
        Err(e) => {
            ctx.tx.send(AppEvent::Log {
                level: LogLevel::Warn,
                msg: format!("prepass skipped: {e}"),
            });
            return;
        }
    };
    match prepass::run_prepass(prepass_client.as_ref(), &ctx.models.translator, &ctx.ws).await {
        Ok(Some(seeded)) => {
            acc.run.add(&stats_from_usage(&seeded.usage));
            note_served_tier(ctx, acc, &ctx.models.translator, &seeded.usage);
            ctx.tx.send(AppEvent::UsageUpdate {
                run: acc.run,
                chapter: acc.chapter,
            });
            let _ = volume::set_prepass_done(&ctx.ws, true);
            ctx.tx.send(AppEvent::PrepassFinished {
                vol,
                characters: seeded.characters,
                terms: seeded.terms,
                examples: seeded.examples,
            });
            ctx.tx.send(AppEvent::Log {
                level: LogLevel::Info,
                msg: format!(
                    "pre-scan: seeded {} character(s), {} term(s), {} style example(s)",
                    seeded.characters, seeded.terms, seeded.examples
                ),
            });
        }
        // No raw to sample (empty volume): leave prepass_done false, nothing to do.
        Ok(None) => {
            ctx.tx.send(AppEvent::PrepassFinished {
                vol,
                characters: 0,
                terms: 0,
                examples: 0,
            });
        }
        Err(e) => {
            ctx.tx.send(AppEvent::PrepassFailed {
                vol,
                reason: e.to_string(),
            });
            ctx.tx.send(AppEvent::Log {
                level: LogLevel::Warn,
                msg: format!("pre-scan skipped: {e}"),
            });
        }
    }
}

/// Drive one volume's chapter queue, folding tallies into `totals` and cost into
/// `acc`. Returns [`Halt::Stopped`] if the user stopped the run or a chapter
/// looped past its re-translate limit (both halt the whole run); otherwise
/// [`Halt::Completed`]. Does NOT emit `PipelineFinished` — the caller owns that so
/// a project run emits exactly one across all volumes.
async fn run_volume_chapters(
    ctx: &PipelineCtx,
    vol_scope: Option<u32>,
    wd: &Watchdog,
    acc: &mut Acc,
    totals: &mut Totals,
) -> Halt {
    loop {
        if ctx.ctl.is_stopped() {
            ctx.tx.send(AppEvent::Log {
                level: LogLevel::Warn,
                msg: "run stopped before chapter".to_string(),
            });
            return Halt::Stopped;
        }
        let chapter = match vol_scope {
            Some(vol) => match ctx.queue.next_for(vol) {
                Some((_, c)) => c,
                None => return Halt::Completed,
            },
            None => match ctx.queue.next() {
                Some((_, c)) => c,
                None => return Halt::Completed,
            },
        };
        ctx.tx.send(AppEvent::ChapterStarted { chapter });
        ctx.tx.send(AppEvent::ChapterStateChanged {
            chapter,
            state: ChapterStatus::Chunking,
        });

        let outcome = process_chapter_watched(ctx, chapter, acc, wd).await;

        // Persist this chapter's spend to VOLUME.md (cumulative lifetime accounting)
        // however it ended, then reset the per-chapter sub-total for the next one.
        // A loop-retranslate's wasted spend stays folded in: it was really spent.
        if !acc.chapter.is_zero() {
            if let Err(e) = volume::add_chapter_usage(&ctx.ws, chapter, &acc.chapter) {
                ctx.tx.send(AppEvent::Log {
                    level: LogLevel::Warn,
                    msg: format!("could not persist usage for chapter {chapter}: {e}"),
                });
            }
            ctx.tx.send(AppEvent::ChapterUsage {
                chapter,
                delta: acc.chapter,
            });
        }
        acc.chapter = UsageStats::default();

        match outcome {
            Ok(Outcome::Completed) => {
                totals.done += 1;
                ctx.tx.send(AppEvent::ChapterStateChanged {
                    chapter,
                    state: ChapterStatus::Done,
                });
                ctx.tx.send(AppEvent::ChapterCompleted { chapter });
            }
            Ok(Outcome::NeedsReview) => {
                // The chapter is fully written, but ≥1 chunk was committed without
                // passing review. It "completed" (counts toward `done`) yet stays
                // flagged `NeedsReview` instead of `Done` so a human can fix it.
                totals.done += 1;
                totals.need_review += 1;
                ctx.tx.send(AppEvent::ChapterStateChanged {
                    chapter,
                    state: ChapterStatus::NeedsReview,
                });
                ctx.tx.send(AppEvent::Log {
                    level: LogLevel::Warn,
                    msg: format!("chapter {chapter} completed with chunk(s) needing manual review"),
                });
            }
            Ok(Outcome::Stopped) => {
                ctx.tx.send(AppEvent::Log {
                    level: LogLevel::Warn,
                    msg: format!("run stopped during chapter {chapter}"),
                });
                return Halt::Stopped;
            }
            Ok(Outcome::Aborted { reason }) => {
                // A chapter looped past its re-translate limit. Mark it Failed and
                // halt the entire run (the user chose abort-on-loop semantics).
                totals.failed += 1;
                ctx.tx.send(AppEvent::ChapterStateChanged {
                    chapter,
                    state: ChapterStatus::Failed,
                });
                ctx.tx.send(AppEvent::ChapterFailed {
                    chapter,
                    reason: reason.clone(),
                });
                ctx.tx.send(AppEvent::Error {
                    context: format!("chapter {chapter}"),
                    msg: reason,
                });
                // Stop so a project run does not advance to the next volume.
                ctx.ctl.stop();
                return Halt::Stopped;
            }
            Err(e) => {
                totals.failed += 1;
                let reason = e.to_string();
                ctx.tx.send(AppEvent::ChapterStateChanged {
                    chapter,
                    state: ChapterStatus::Failed,
                });
                ctx.tx.send(AppEvent::ChapterFailed {
                    chapter,
                    reason: reason.clone(),
                });
                ctx.tx.send(AppEvent::Error {
                    context: format!("chapter {chapter}"),
                    msg: reason,
                });
            }
        }
    }
}

/// Process a chapter under the outer stall watchdog. Active model-call stalls are
/// retried inside the current chunk; if the chapter stalls outside that tracked
/// chunk work, the in-flight chapter is cancelled and re-translated whole — up to
/// `cfg.max_chapter_retranslates` times, after which the run halts.
async fn process_chapter_watched(
    ctx: &PipelineCtx,
    chapter: u32,
    acc: &mut Acc,
    wd: &Watchdog,
) -> anyhow::Result<Outcome> {
    let max_retranslates = ctx.cfg.max_chapter_retranslates;
    let mut retranslates = 0u32;
    loop {
        wd.reset_chapter();
        let run = tokio::select! {
            biased;
            res = process_chapter(ctx, chapter, acc, wd) => ChapterRun::Finished(res),
            reason = wd.watch(&ctx.ctl) => ChapterRun::Looped(reason),
        };

        match run {
            ChapterRun::Finished(res) => return res,
            ChapterRun::Looped(reason) => {
                if retranslates >= max_retranslates {
                    let msg = format!(
                        "chapter {chapter} stalled ({}) after {retranslates} re-translation(s) — aborting run",
                        reason.describe()
                    );
                    ctx.tx.send(AppEvent::Log {
                        level: LogLevel::Error,
                        msg: msg.clone(),
                    });
                    return Ok(Outcome::Aborted { reason: msg });
                }
                retranslates += 1;
                ctx.tx.send(AppEvent::ChapterLooping {
                    chapter,
                    reason: reason.describe().to_string(),
                    attempt: retranslates,
                    max: max_retranslates,
                });
                ctx.tx.send(AppEvent::Log {
                    level: LogLevel::Warn,
                    msg: format!(
                        "chapter {chapter} {} — re-translating whole chapter ({retranslates}/{max_retranslates})",
                        reason.describe()
                    ),
                });
                // Wipe the chapter so the re-translate starts clean (a poisoned
                // continuity tail or half-looped chunk must not carry over).
                if let Err(e) = translation::reset_chapter(&ctx.ws, chapter) {
                    ctx.tx.send(AppEvent::Log {
                        level: LogLevel::Warn,
                        msg: format!("could not reset chapter {chapter} for re-translate: {e}"),
                    });
                }
                ctx.tx.send(AppEvent::ChapterStateChanged {
                    chapter,
                    state: ChapterStatus::Chunking,
                });
            }
        }
    }
}

/// Result of one watchdog-guarded chapter attempt.
enum ChapterRun {
    /// `process_chapter` ran to its own conclusion (the watchdog never tripped).
    Finished(anyhow::Result<Outcome>),
    /// The watchdog tripped; the attempt was cancelled.
    Looped(LoopReason),
}

/// Process one chapter end to end. Image-only chapters short-circuit (the image
/// markdown is copied straight to `translated/`); prose chapters are chunked and
/// each chunk is translated + reviewed + committed.
async fn process_chapter(
    ctx: &PipelineCtx,
    chapter: u32,
    acc: &mut Acc,
    wd: &Watchdog,
) -> anyhow::Result<Outcome> {
    let raw_path = ctx.ws.raw(chapter);
    let raw = tokio::fs::read_to_string(&raw_path)
        .await
        .map_err(|e| anyhow::anyhow!("read {}: {e}", raw_path.display()))?;
    if raw.trim().is_empty() {
        anyhow::bail!("chapter {chapter} has no raw source");
    }

    // Image-only chapters skip the agents entirely.
    if cleanse::is_image_only(&raw) {
        translation::write_image_only(&ctx.ws, chapter, &raw)?;
        ctx.tx.send(AppEvent::Log {
            level: LogLevel::Info,
            msg: format!("chapter {chapter}: image-only, copied straight to translated/"),
        });
        return Ok(Outcome::Completed);
    }

    let chunks = chunk_chapter(
        &raw,
        ctx.cfg.chunk_target_tokens,
        ctx.cfg.chunk_hard_cap_tokens,
    );
    if chunks.is_empty() {
        // No translatable prose after chunking — treat as image-only passthrough.
        translation::write_image_only(&ctx.ws, chapter, &raw)?;
        return Ok(Outcome::Completed);
    }

    let est_total: usize = chunks.iter().map(|c| c.est_tokens).sum();
    ctx.tx.send(AppEvent::ChapterChunked {
        chapter,
        total_chunks: chunks.len(),
        est_tokens_total: est_total,
    });

    let total = chunks.len();

    // Record the expected total up front so a stop/crash mid-chapter leaves a
    // file `scan::derive_status` can recognize as Partial instead of Done.
    if let Err(e) = translation::record_total_chunks(&ctx.ws, chapter, total as u32).await {
        ctx.tx.send(AppEvent::Log {
            level: LogLevel::Warn,
            msg: format!("chapter {chapter}: could not record chunk total: {e}"),
        });
    }

    // Resume support: translated files are append-only, chunk-marked logs. If a
    // previous run failed after committing chunk N, a re-run should start at the
    // next missing marker instead of re-spending tokens on chunks already on disk.
    let existing_translation = translation::read_translated(&ctx.ws, chapter).await;
    let committed = translation::committed_chunk_indices_in(&existing_translation);
    let needs_review = translation::review_needed_chunk_indices_in(&existing_translation);
    let clean_committed: std::collections::BTreeSet<u32> =
        committed.difference(&needs_review).copied().collect();
    let skipped = chunks
        .iter()
        .filter(|chunk| clean_committed.contains(&(chunk.index as u32)))
        .count();
    if skipped > 0 {
        ctx.tx.send(AppEvent::Log {
            level: LogLevel::Info,
            msg: format!(
                "chapter {chapter}: resuming from translated file · skipping {skipped}/{total} committed chunk(s)"
            ),
        });
    }

    if !needs_review.is_empty() {
        ctx.tx.send(AppEvent::Log {
            level: LogLevel::Info,
            msg: format!(
                "chapter {chapter}: rechecking {} review-needed chunk(s)",
                needs_review.len()
            ),
        });
    }

    // First-person narrator carried chunk-to-chunk within this chapter so a POV
    // section that spans a chunk boundary keeps the right "I". Reset per chapter.
    let mut pov_carry: Option<String> = None;
    // The previous chunk's source, in reading order, so a character referred to only
    // by pronoun in this chunk stays in the injected roster (see build_reference_ctx).
    let mut prev_chunk_text: Option<&str> = None;
    for chunk in &chunks {
        if clean_committed.contains(&(chunk.index as u32)) {
            prev_chunk_text = Some(chunk.text.as_str());
            continue;
        }

        // Honor pause/stop between chunks ("current chunk finishes, then halts").
        if !gate(ctx, chapter).await {
            // Leave the interrupted chapter showing its true resting state — a
            // stop mid-chapter must not linger as Translating/Paused (or read
            // back as Done on the next scan).
            let on_disk = translation::read_translated(&ctx.ws, chapter).await;
            let state = if translation::committed_chunk_indices_in(&on_disk).is_empty() {
                ChapterStatus::Pending
            } else {
                ChapterStatus::Partial
            };
            ctx.tx
                .send(AppEvent::ChapterStateChanged { chapter, state });
            return Ok(Outcome::Stopped);
        }
        ctx.tx.send(AppEvent::ChunkStarted {
            chapter,
            chunk: chunk.index,
            total,
            est_tokens: chunk.est_tokens,
        });
        // Fresh repetition state per chunk so one chunk's tail can't trip on the
        // next chunk's start.
        wd.reset_chunk();
        match process_chunk(
            ctx,
            chapter,
            chunk,
            acc,
            wd,
            &mut pov_carry,
            prev_chunk_text,
        )
        .await?
        {
            ChunkOutcome::Committed | ChunkOutcome::NeedsReview => {}
        }
        prev_chunk_text = Some(chunk.text.as_str());
    }

    // Whole-chapter coherence sweep: catch cross-chunk drift the per-chunk reviewer
    // can't see. Findings land as continuity notes (surfaced in the QA inbox), never
    // auto-applied. Best-effort — never fails the chapter.
    if ctx.cfg.coherence_check {
        run_coherence_sweep(ctx, chapter, &raw, acc, wd).await;
    }

    let any_needs_review = translation::read_translated(&ctx.ws, chapter)
        .await
        .contains(translation::REVIEW_NEEDED_MARKER);

    // All chunks are written either way; the run loop maps the outcome to the
    // chapter's final status (Done vs NeedsReview).
    ctx.tx.send(AppEvent::ChapterStateChanged {
        chapter,
        state: ChapterStatus::Appended,
    });
    if any_needs_review {
        Ok(Outcome::NeedsReview)
    } else {
        Ok(Outcome::Completed)
    }
}

/// Block while paused; return `false` if the run is (or becomes) stopped so the
/// caller aborts. Emits `PipelinePaused`/`PipelineResumed` and flips the active
/// chapter to `Paused` so the UI reflects the held state.
async fn gate(ctx: &PipelineCtx, chapter: u32) -> bool {
    if ctx.ctl.is_stopped() {
        return false;
    }
    if ctx.ctl.is_paused() {
        ctx.tx.send(AppEvent::PipelinePaused);
        ctx.tx.send(AppEvent::ChapterStateChanged {
            chapter,
            state: ChapterStatus::Paused,
        });
        while ctx.ctl.is_paused() {
            tokio::time::sleep(Duration::from_millis(120)).await;
        }
        if ctx.ctl.is_stopped() {
            return false;
        }
        ctx.tx.send(AppEvent::PipelineResumed);
    }
    true
}

const MAX_GLOSSARY_IN_CTX: usize = 80;
const MAX_CHARACTERS_IN_CTX: usize = 40;
const MAX_PROTECTED_TERMS_FOR_ORCH: usize = 40;
const ORCHESTRATOR_MAX_TOOL_ROUNDS: usize = 32;

fn glossary_terms_for_chunk(ws: &Workspace, chunk_text: &str, max: usize) -> Vec<GlossaryTerm> {
    let mut terms = glossary::load(ws);
    // Keep only terms the chunk actually uses, so the injected glossary tracks
    // the chunk rather than the whole, ever-growing volume.
    terms.retain(|t| {
        let jp = t.jp_term.trim();
        !jp.is_empty() && chunk_text.contains(jp)
    });
    terms.truncate(max);
    terms
}

fn characters_for_chunk(
    ws: &Workspace,
    chunk_text: &str,
    prev_chunk_text: Option<&str>,
    max: usize,
) -> Vec<crate::model::Character> {
    let mut chars = characters::load(ws);
    let mentions = |c: &crate::model::Character, text: &str| {
        let jp = c.jp_name.trim();
        (!jp.is_empty() && text.contains(jp))
            || c.aliases
                .iter()
                .any(|a| !a.trim().is_empty() && text.contains(a.trim()))
            || c.also_called
                .iter()
                .any(|a| !a.jp.trim().is_empty() && text.contains(a.jp.trim()))
    };
    chars.retain(|c| {
        mentions(c, chunk_text) || prev_chunk_text.is_some_and(|prev| mentions(c, prev))
    });
    chars.truncate(max);
    chars
}

/// Assemble the reference context bundled into every Translator/Reviewer call:
/// the scoped terminology policies, the character roster (pronouns/register), the
/// few-shot style exemplars, and the PROJECT/STYLE prose — each in its own clearly-
/// delimited section. Re-read per chunk so mid-chapter glossary/character additions
/// take effect immediately. `prev_chunk_text` (the previous chunk's source) keeps a
/// character in scope across a chunk boundary even when this chunk refers to them
/// only by pronoun — the case the POV/pronoun handling most needs.
fn build_reference_ctx(ws: &Workspace, chunk_text: &str, prev_chunk_text: Option<&str>) -> String {
    fn section(out: &mut String, open: &str, body: &str, close: &str) {
        let b = body.trim();
        if !b.is_empty() {
            out.push_str(open);
            out.push('\n');
            out.push_str(b);
            out.push('\n');
            out.push_str(close);
            out.push_str("\n\n");
        }
    }
    fn excerpt(s: String, max: usize) -> String {
        let t = s.trim();
        if t.chars().count() <= max {
            t.to_string()
        } else {
            t.chars().take(max).collect::<String>() + "…"
        }
    }

    let mut s = String::new();
    let terms = glossary_terms_for_chunk(ws, chunk_text, MAX_GLOSSARY_IN_CTX);
    section(
        &mut s,
        "<<GLOSSARY: นโยบายคำศัพท์ (hard lock / preferred / forbidden / context)>>",
        &glossary::render_context_blurb(&terms),
        "<<END_GLOSSARY>>",
    );
    let chars = characters_for_chunk(ws, chunk_text, prev_chunk_text, MAX_CHARACTERS_IN_CTX);
    section(
        &mut s,
        "<<CHARACTERS: สรรพนาม/น้ำเสียงที่กำหนด>>",
        &characters::render_context_blurb(&chars),
        "<<END_CHARACTERS>>",
    );
    section(
        &mut s,
        "<<STYLE_EXAMPLES: ตัวอย่างคู่ ญี่ปุ่น→ไทย ใช้เป็นแนวสำนวน/น้ำเสียงที่ต้องการ ห้ามคัดลอกลงคำแปล>>",
        &render_style_examples(&volume::load(ws).style_examples),
        "<<END_STYLE_EXAMPLES>>",
    );
    section(
        &mut s,
        "<<VOLUME_SYNOPSIS: เรื่องย่อของเล่มนี้ ใช้เป็นบริบทภาพรวม>>",
        &excerpt(volume::load(ws).synopsis_th, 1200),
        "<<END_VOLUME_SYNOPSIS>>",
    );
    section(
        &mut s,
        "<<PROJECT: บริบท/โครงเรื่องโดยรวม>>",
        &excerpt(data_block::read_body(&ws.project_md()), 1400),
        "<<END_PROJECT>>",
    );
    section(
        &mut s,
        "<<STYLE: แนวทางโทน/สำนวน>>",
        &excerpt(data_block::read_body(&ws.style_md()), 1400),
        "<<END_STYLE>>",
    );
    s
}

/// Render the few-shot style exemplars as `JP → TH` bullets for the prompt.
fn render_style_examples(examples: &[crate::model::StyleExample]) -> String {
    let mut s = String::new();
    for ex in examples {
        let jp = ex.jp.trim();
        let th = ex.th.trim();
        if jp.is_empty() || th.is_empty() {
            continue;
        }
        s.push_str("- JP: ");
        s.push_str(jp);
        s.push_str("\n  TH: ");
        s.push_str(th);
        if let Some(note) = ex.note.as_deref().filter(|n| !n.trim().is_empty()) {
            s.push_str(&format!("  ({})", note.trim()));
        }
        s.push('\n');
    }
    s
}

/// Convert API token `Usage` into the UI's `TokenUsage`. Falls back to
/// `prompt + completion` when a provider omits `total_tokens` (some BYOK
/// providers do) so the running total never silently stalls.
fn to_tokens(u: &Usage) -> TokenUsage {
    let total = if u.total_tokens != 0 {
        u.total_tokens
    } else {
        u.prompt_tokens.saturating_add(u.completion_tokens)
    };
    TokenUsage {
        prompt: u.prompt_tokens,
        completion: u.completion_tokens,
        total,
    }
}

fn effective_feedback_text(audit_findings: &[String], review: &ReviewerOut) -> String {
    let mut feedback = Vec::new();
    feedback.extend(
        audit_findings
            .iter()
            .map(|f| format!("Local audit: {}", f.trim()))
            .filter(|f| !f.trim().is_empty()),
    );
    let reviewer_feedback = review.feedback_text();
    if !reviewer_feedback.trim().is_empty() {
        feedback.push(reviewer_feedback);
    }
    feedback.join("; ")
}

const REFUSAL_RETRY_FEEDBACK: &str = "The previous output was a refusal or policy notice, not a translation. Treat this as neutral literary translation work: translate only SOURCE_JP into Thai, preserve Markdown, do not add explicit detail or commentary, and return the final Thai story text in translated_text.";
const PARTIAL_STREAM_RETRY_FEEDBACK: &str = "The previous stream stopped after an incomplete translated_text. Discard that partial output and translate the entire SOURCE_JP again from scratch. Keep neutral literary wording, preserve Markdown, do not add commentary, and return complete final Thai story text in valid translated_text JSON.";
const LENGTH_RETRY_FEEDBACK: &str = "The previous attempt ran out of output tokens before completing translated_text. Be far more concise: keep thought_process to a few short words or leave it empty, never draft the translation there, and spend the budget on translated_text only. Translate the ENTIRE SOURCE_JP without omitting anything and return the complete final Thai in valid JSON.";
const REPETITION_RETRY_FEEDBACK: &str = "The previous translated_text started repeating inside this chunk. Discard that output and redo only this SOURCE_JP chunk from the beginning. Do not copy any repeated tail, do not continue the loop, preserve Markdown, and return complete final Thai story text in valid JSON.";
const STALL_RETRY_FEEDBACK: &str = "The previous attempt made no progress for too long. Redo only this SOURCE_JP chunk from the beginning, keep the output concise, preserve Markdown, and return complete final Thai story text in valid JSON.";

/// User-facing NeedsReview reason for token-budget cutoffs.
fn length_reason(length_trunc: bool, base: &str) -> String {
    if length_trunc {
        "translator ran out of output tokens before finishing — lower chunk_target_tokens in Settings, then re-run this chapter".to_string()
    } else {
        base.to_string()
    }
}

fn refusal_retry_feedback(translated: &str) -> Option<&'static str> {
    looks_like_model_refusal(translated).then_some(REFUSAL_RETRY_FEEDBACK)
}

fn looks_like_model_refusal(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }

    let lower = trimmed.to_lowercase();
    let head: String = lower.chars().take(220).collect();
    let starts_with_refusal = [
        "i'm sorry",
        "i am sorry",
        "sorry, but",
        "i can't",
        "i cannot",
        "i can’t",
        "i’m unable",
        "i am unable",
        "ขออภัย",
        "ขอโทษ",
    ]
    .iter()
    .any(|prefix| head.starts_with(prefix));
    let policy_language = [
        "content policy",
        "safety policy",
        "policy",
        "cannot assist",
        "can't assist",
        "unable to assist",
        "unable to translate",
        "cannot translate",
        "ไม่สามารถช่วย",
        "ไม่สามารถแปล",
        "ไม่สามารถดำเนินการ",
        "ตามนโยบาย",
        "นโยบายความปลอดภัย",
        "คำขอนี้",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    let thai_refusal = [
        "ไม่สามารถช่วย",
        "ไม่สามารถแปล",
        "ไม่สามารถดำเนินการ",
        "ไม่อาจช่วย",
        "ไม่อาจแปล",
        "ไม่เหมาะสม",
    ]
    .iter()
    .any(|needle| head.contains(needle));

    policy_language || (starts_with_refusal && thai_refusal)
}

/// Build a `UsageStats` from one API call's token + BYOK-aware cost usage.
fn stats_from_usage(u: &Usage) -> UsageStats {
    UsageStats {
        tokens: to_tokens(u),
        cost_usd: u.cost_usd(),
        tool_calls: 0,
    }
}

/// The two running totals one pipeline run maintains in parallel: `run` spans the
/// whole run (drives the run meter), `chapter` resets at each chapter boundary
/// (drives the chapter meter and the persisted per-chapter total).
#[derive(Default)]
struct Acc {
    run: UsageStats,
    chapter: UsageStats,
    /// Models whose served service tier has already been reported this run.
    tier_noted: HashSet<String>,
}

impl Acc {
    /// Fold one API call's token + cost usage into both totals.
    fn fold(&mut self, u: &Usage) {
        let s = stats_from_usage(u);
        self.run.add(&s);
        self.chapter.add(&s);
    }

    /// Fold `n` Orchestrator tool calls into both totals.
    fn add_tool_calls(&mut self, n: u32) {
        self.run.tool_calls = self.run.tool_calls.saturating_add(n);
        self.chapter.tool_calls = self.chapter.tool_calls.saturating_add(n);
    }
}

/// Report, once per model per run, whether the configured `service_tier` was
/// actually honored. OpenRouter bills whichever tier really served the request,
/// and a flex/priority ask silently falls back to the standard tier on models
/// without tier support — without this echo check the user believes the flex
/// discount (or priority speed-up) is in effect when it isn't.
fn note_served_tier(ctx: &PipelineCtx, acc: &mut Acc, agent: &AgentModel, usage: &Usage) {
    let Some(requested) = ctx.cfg.service_tier else {
        return;
    };
    let model = &agent.model;
    if !acc.tier_noted.insert(model.clone()) {
        return;
    }
    let tier = match requested {
        ServiceTier::Flex => "flex",
        ServiceTier::Priority => "priority",
    };
    let (level, msg) = match usage.served_tier {
        Some(served) if served.satisfies(requested) => (
            LogLevel::Info,
            format!("service tier {tier} active on {model}"),
        ),
        _ => (
            LogLevel::Warn,
            format!("service tier {tier} not applied on {model} — billed at the standard rate"),
        ),
    };
    ctx.tx.send(AppEvent::Log { level, msg });
}

/// Translate and review one chunk. Approved output is appended deterministically;
/// exhausted attempts commit the best/empty NeedsReview block. Only a Translator
/// that never yields anything can fail the chapter.
#[allow(clippy::too_many_arguments)]
async fn process_chunk(
    ctx: &PipelineCtx,
    chapter: u32,
    chunk: &Chunk,
    acc: &mut Acc,
    wd: &Watchdog,
    pov: &mut Option<String>,
    prev_chunk_text: Option<&str>,
) -> anyhow::Result<ChunkOutcome> {
    // Each step below counts as progress for the stall arm of the watchdog.
    wd.ping();
    ctx.tx.send(AppEvent::ChunkStateChanged {
        chapter,
        chunk: chunk.index,
        state: ChunkState::Queued,
    });

    // Context and continuity are stable across this chunk's attempts.
    let reference_ctx = build_reference_ctx(&ctx.ws, &chunk.text, prev_chunk_text);
    let audit_characters =
        characters_for_chunk(&ctx.ws, &chunk.text, prev_chunk_text, MAX_CHARACTERS_IN_CTX);
    let mut prev_thai =
        continuity::last_thai_sentences(&ctx.ws, chapter, ctx.cfg.continuity_sentences).await;
    // Seed chunk 0 from the previous chapter when this one has no Thai yet.
    if prev_thai.is_empty() && chunk.index == 0 && chapter > 1 {
        prev_thai =
            continuity::last_thai_sentences(&ctx.ws, chapter - 1, ctx.cfg.continuity_sentences)
                .await;
    }

    let max = ctx.cfg.max_attempts.max(1);
    let mut feedback: Option<String> = None;
    // Keep the best non-refusal Thai so later hard errors can still yield NeedsReview.
    let mut candidate: Option<String> = None;
    // Every reviewer rejection so far, so retry 2+ sees the whole critique history
    // and stops repeating mistakes it was already told to fix.
    let mut past_reviews: Vec<String> = Vec::new();

    'attempts: for attempt in 1..=max {
        ctx.tx.send(AppEvent::ChunkStateChanged {
            chapter,
            chunk: chunk.index,
            state: ChunkState::Translating,
        });
        ctx.tx.send(AppEvent::ChapterStateChanged {
            chapter,
            state: ChapterStatus::Translating,
        });
        ctx.tx.send(AppEvent::TranslatorRequested {
            chapter,
            chunk: chunk.index,
            attempt,
        });

        wd.reset_chunk();
        let tx = ctx.tx.clone();
        let translator_client = ctx.client_for(&ctx.models.translator)?;
        let translate_res = {
            let _wait = wd.external_wait();
            let translate = translate_chunk_streaming(
                translator_client.as_ref(),
                &ctx.models.translator,
                &reference_ctx,
                &prev_thai,
                pov.as_deref(),
                &chunk.text,
                feedback.as_deref(),
                attempt,
                move |delta| {
                    // Feed the watchdog: streaming is progress (resets the stall
                    // timer) and the repetition detector sees the live tail.
                    wd.feed_stream(delta);
                    tx.send(AppEvent::StreamDelta {
                        chapter,
                        chunk: chunk.index,
                        role: AgentRole::Translator,
                        delta: delta.to_string(),
                    });
                },
            );
            tokio::pin!(translate);
            let repeated = wd.watch_repetition(&ctx.ctl);
            tokio::pin!(repeated);
            let stalled = wd.watch_active_call_stall(&ctx.ctl);
            tokio::pin!(stalled);
            tokio::select! {
                biased;
                _ = &mut repeated => TranslatorAttemptRun::Repeated,
                reason = &mut stalled => TranslatorAttemptRun::Stalled(reason),
                res = &mut translate => TranslatorAttemptRun::Finished(Box::new(res)),
            }
        };

        let (out, t_usage, streamed_preview): (TranslatorOut, Usage, bool) = match translate_res {
            TranslatorAttemptRun::Finished(res) => match *res {
                Ok(o) if !wd.repetition_triggered() => o,
                Ok(_) => {
                    ctx.tx.send(AppEvent::Log {
                        level: LogLevel::Warn,
                        msg: format!(
                            "chapter {chapter} chunk {} output repeated — retrying chunk only ({attempt}/{max})",
                            chunk.index + 1
                        ),
                    });
                    if attempt < max {
                        emit_attempt_failed_retry(
                            ctx,
                            chapter,
                            chunk,
                            attempt,
                            max,
                            REPETITION_RETRY_FEEDBACK,
                        );
                        feedback = Some(REPETITION_RETRY_FEEDBACK.to_string());
                        continue;
                    }

                    match candidate {
                        Some(thai) => {
                            let reason = format!(
                                "translator output kept repeating on chunk {} after {max} attempts; committed the best available result for review",
                                chunk.index + 1
                            );
                            return commit_chunk_needs_review(
                                ctx, chapter, chunk, &thai, attempt, reason,
                            )
                            .await;
                        }
                        None => {
                            let reason = format!(
                                "translator output kept repeating on chunk {} after {max} attempts; no usable translation was produced",
                                chunk.index + 1
                            );
                            return commit_chunk_needs_review(
                                ctx, chapter, chunk, "", attempt, reason,
                            )
                            .await;
                        }
                    }
                }
                Err(e) => {
                    let partial = e.partial_translated_text().trim().to_string();
                    // Token-budget cutoffs need a tighter retry, not a verbatim replay.
                    let length_trunc = e.is_length_truncation();
                    // Retry while attempts remain; otherwise keep any usable Thai.
                    ctx.tx.send(AppEvent::Error {
                        context: format!("translator ch{chapter} chunk{}", chunk.index),
                        msg: e.to_string(),
                    });
                    if !partial.is_empty() {
                        if attempt < max {
                            let fb = if length_trunc {
                                LENGTH_RETRY_FEEDBACK
                            } else {
                                PARTIAL_STREAM_RETRY_FEEDBACK
                            };
                            emit_attempt_failed_retry(ctx, chapter, chunk, attempt, max, fb);
                            feedback = Some(fb.to_string());
                            continue;
                        }
                        // Prefer an earlier complete translation; otherwise salvage
                        // the partial stream and flag the chunk for human review.
                        let (thai, reason) = match candidate {
                            Some(thai) => (
                                thai,
                                length_reason(
                                    length_trunc,
                                    "translator stream stopped after partial output on the final attempt",
                                ),
                            ),
                            None => {
                                let salvaged = strip_copied_continuity(&prev_thai, &partial);
                                let thai = if salvaged.trim().is_empty() {
                                    partial.clone()
                                } else {
                                    salvaged
                                };
                                (
                                    thai,
                                    length_reason(
                                        length_trunc,
                                        "translator stream cut off mid-output; salvaged the partial translation for review",
                                    ),
                                )
                            }
                        };
                        return commit_chunk_needs_review(
                            ctx, chapter, chunk, &thai, attempt, reason,
                        )
                        .await;
                    }
                    // Content-policy blocks need the de-escalation prompt, not replay.
                    let policy_block = e.is_content_policy_block();
                    if attempt < max {
                        if policy_block {
                            emit_attempt_failed_retry(
                                ctx,
                                chapter,
                                chunk,
                                attempt,
                                max,
                                REFUSAL_RETRY_FEEDBACK,
                            );
                            feedback = Some(REFUSAL_RETRY_FEEDBACK.to_string());
                        } else if length_trunc {
                            emit_attempt_failed_retry(
                                ctx,
                                chapter,
                                chunk,
                                attempt,
                                max,
                                LENGTH_RETRY_FEEDBACK,
                            );
                            feedback = Some(LENGTH_RETRY_FEEDBACK.to_string());
                        } else {
                            emit_attempt_failed_retry(
                                ctx,
                                chapter,
                                chunk,
                                attempt,
                                max,
                                &format!("translator error, retrying: {e}"),
                            );
                        }
                        continue;
                    }
                    match candidate {
                        Some(thai) => {
                            return commit_chunk_needs_review(
                                ctx,
                                chapter,
                                chunk,
                                &thai,
                                attempt,
                                format!("translator failed on the final attempt: {e}"),
                            )
                            .await;
                        }
                        // Nothing to keep; an empty NeedsReview chunk is retryable later.
                        None => {
                            let reason = if policy_block {
                                format!(
                                    "translator blocked by the model's content policy after {max} attempts ({e}) — try a different translator model for this chunk"
                                )
                            } else if length_trunc {
                                format!(
                                    "translator ran out of output tokens after {max} attempts — lower chunk_target_tokens in Settings, then re-run this chapter"
                                )
                            } else {
                                format!("translator produced no output after {max} attempts: {e}")
                            };
                            return commit_chunk_needs_review(
                                ctx, chapter, chunk, "", attempt, reason,
                            )
                            .await;
                        }
                    }
                }
            },
            TranslatorAttemptRun::Repeated => {
                ctx.tx.send(AppEvent::Log {
                    level: LogLevel::Warn,
                    msg: format!(
                        "chapter {chapter} chunk {} output repeated — retrying chunk only ({attempt}/{max})",
                        chunk.index + 1
                    ),
                });
                if attempt < max {
                    emit_attempt_failed_retry(
                        ctx,
                        chapter,
                        chunk,
                        attempt,
                        max,
                        REPETITION_RETRY_FEEDBACK,
                    );
                    feedback = Some(REPETITION_RETRY_FEEDBACK.to_string());
                    continue;
                }

                match candidate {
                    Some(thai) => {
                        let reason = format!(
                            "translator output kept repeating on chunk {} after {max} attempts; committed the best available result for review",
                            chunk.index + 1
                        );
                        return commit_chunk_needs_review(
                            ctx, chapter, chunk, &thai, attempt, reason,
                        )
                        .await;
                    }
                    None => {
                        let reason = format!(
                            "translator output kept repeating on chunk {} after {max} attempts; no usable translation was produced",
                            chunk.index + 1
                        );
                        return commit_chunk_needs_review(ctx, chapter, chunk, "", attempt, reason)
                            .await;
                    }
                }
            }
            TranslatorAttemptRun::Stalled(reason) => {
                ctx.tx.send(AppEvent::Log {
                    level: LogLevel::Warn,
                    msg: format!(
                        "chapter {chapter} chunk {} translator stalled ({}) — retrying chunk only ({attempt}/{max})",
                        chunk.index + 1,
                        reason.describe()
                    ),
                });
                if attempt < max {
                    emit_attempt_failed_retry(
                        ctx,
                        chapter,
                        chunk,
                        attempt,
                        max,
                        STALL_RETRY_FEEDBACK,
                    );
                    feedback = Some(STALL_RETRY_FEEDBACK.to_string());
                    continue;
                }

                match candidate {
                    Some(thai) => {
                        let reason = format!(
                            "translator stalled on chunk {} after {max} attempts; committed the best available result for review",
                            chunk.index + 1
                        );
                        return commit_chunk_needs_review(
                            ctx, chapter, chunk, &thai, attempt, reason,
                        )
                        .await;
                    }
                    None => {
                        let reason = format!(
                            "translator stalled on chunk {} after {max} attempts; no usable translation was produced",
                            chunk.index + 1
                        );
                        return commit_chunk_needs_review(ctx, chapter, chunk, "", attempt, reason)
                            .await;
                    }
                }
            }
        };

        // Drop echoed continuity and normalize mechanical punctuation residue
        // before audit/review/append; neither needs another model turn.
        let thai = strip_copied_continuity(&prev_thai, &out.translated_text);
        let thai = normalize_japanese_punctuation_residue(&thai);
        let refusal_feedback = refusal_retry_feedback(&thai);
        if refusal_feedback.is_none() && !thai.trim().is_empty() {
            candidate = Some(thai.clone());
        }
        // Carry the ending narrator forward even when this chunk becomes NeedsReview.
        // Otherwise the next chunk can inherit a stale pre-switch POV.
        if !out.pov.trim().is_empty() {
            *pov = Some(out.pov.trim().to_string());
        }
        let tok = to_tokens(&t_usage);
        acc.fold(&t_usage);
        note_served_tier(ctx, acc, &ctx.models.translator, &t_usage);
        ctx.tx.send(AppEvent::TranslatorReturned {
            chapter,
            chunk: chunk.index,
            attempt,
            // If the streaming path emitted translated_text deltas, avoid
            // appending the same chunk again when the final schema lands.
            thai_preview: if streamed_preview {
                String::new()
            } else {
                thai.clone()
            },
            tokens: tok,
        });
        ctx.tx.send(AppEvent::UsageUpdate {
            run: acc.run,
            chapter: acc.chapter,
        });

        if let Some(fb) = refusal_feedback {
            if attempt < max {
                emit_attempt_failed_retry(ctx, chapter, chunk, attempt, max, fb);
                feedback = Some(fb.to_string());
                continue;
            }

            match candidate {
                Some(best) => {
                    return commit_chunk_needs_review(
                        ctx,
                        chapter,
                        chunk,
                        &best,
                        attempt,
                        "translator returned a refusal or policy notice on the final attempt"
                            .to_string(),
                    )
                    .await;
                }
                // Keep refusal text out of the next chunk's continuity tail.
                None => {
                    return commit_chunk_needs_review(
                        ctx,
                        chapter,
                        chunk,
                        "",
                        attempt,
                        format!("translator returned only refusals after {max} attempts"),
                    )
                    .await;
                }
            }
        }

        let audit_terms = glossary_terms_for_chunk(&ctx.ws, &chunk.text, MAX_GLOSSARY_IN_CTX);
        let mut audit_findings =
            audit_translation_with_terms(&chunk.text, &thai, &prev_thai, &audit_terms);
        audit_findings.extend(audit_character_pronoun_rules(
            &chunk.text,
            &thai,
            pov.as_deref(),
            &audit_characters,
        ));
        // Non-gating signals for the Reviewer to verify.
        let advisory = advisory_findings_with_references(&chunk.text, &thai, &audit_characters);
        ctx.tx.send(AppEvent::ChunkStateChanged {
            chapter,
            chunk: chunk.index,
            state: ChunkState::Reviewing,
        });
        ctx.tx.send(AppEvent::ChapterStateChanged {
            chapter,
            state: ChapterStatus::Reviewing,
        });
        // Missing Reviewer verdicts retry in place; the Thai already passed audit.
        let (review, r_usage) = {
            let mut review_attempt = 1u32;
            loop {
                wd.ping();
                ctx.tx.send(AppEvent::ReviewerRequested {
                    chapter,
                    chunk: chunk.index,
                    attempt,
                });
                let reviewer_client = ctx.client_for(&ctx.models.reviewer)?;
                let result = {
                    let _wait = wd.external_wait();
                    let review = review_chunk(
                        reviewer_client.as_ref(),
                        &ctx.models.reviewer,
                        &chunk.text,
                        &thai,
                        &reference_ctx,
                        &audit_findings,
                        &advisory,
                        &prev_thai,
                        review_attempt,
                    );
                    tokio::pin!(review);
                    let stalled = wd.watch_active_call_stall(&ctx.ctl);
                    tokio::pin!(stalled);
                    tokio::select! {
                        biased;
                        reason = &mut stalled => ReviewerAttemptRun::Stalled(reason),
                        r = &mut review => ReviewerAttemptRun::Finished(r),
                    }
                };
                match result {
                    ReviewerAttemptRun::Finished(Ok(r)) => break r,
                    ReviewerAttemptRun::Stalled(reason) => {
                        ctx.tx.send(AppEvent::Log {
                            level: LogLevel::Warn,
                            msg: format!(
                                "chapter {chapter} chunk {} reviewer stalled ({}) — retrying chunk only ({attempt}/{max})",
                                chunk.index + 1,
                                reason.describe()
                            ),
                        });
                        if attempt < max {
                            emit_attempt_failed_retry(
                                ctx,
                                chapter,
                                chunk,
                                attempt,
                                max,
                                STALL_RETRY_FEEDBACK,
                            );
                            feedback = Some(STALL_RETRY_FEEDBACK.to_string());
                            continue 'attempts;
                        }
                        return commit_chunk_needs_review(
                            ctx,
                            chapter,
                            chunk,
                            &thai,
                            attempt,
                            format!(
                                "reviewer stalled on chunk {} after {max} attempts; committed without review",
                                chunk.index + 1
                            ),
                        )
                        .await;
                    }
                    ReviewerAttemptRun::Finished(Err(e)) => {
                        ctx.tx.send(AppEvent::Error {
                            context: format!("reviewer ch{chapter} chunk{}", chunk.index),
                            msg: e.to_string(),
                        });
                        if review_attempt < max {
                            ctx.tx.send(AppEvent::ChunkRetry {
                                chapter,
                                chunk: chunk.index,
                                attempt: review_attempt,
                                max,
                                feedback: format!(
                                    "reviewer returned no verdict, retrying reviewer: {e}"
                                ),
                            });
                            review_attempt += 1;
                            continue;
                        }
                        return commit_chunk_needs_review(
                            ctx,
                            chapter,
                            chunk,
                            &thai,
                            attempt,
                            format!(
                                "reviewer returned no verdict after {review_attempt} attempts; committed without review: {e}"
                            ),
                        )
                        .await;
                    }
                }
            }
        };
        wd.ping();
        acc.fold(&r_usage);
        note_served_tier(ctx, acc, &ctx.models.reviewer, &r_usage);
        ctx.tx.send(AppEvent::UsageUpdate {
            run: acc.run,
            chapter: acc.chapter,
        });

        let approved = review.approved() && audit_findings.is_empty();
        let fb_text = effective_feedback_text(&audit_findings, &review);
        ctx.tx.send(AppEvent::ReviewerReturned {
            chapter,
            chunk: chunk.index,
            attempt,
            verdict: if approved {
                ReviewVerdict::Approve
            } else {
                ReviewVerdict::Reject
            },
            feedback: if fb_text.is_empty() {
                None
            } else {
                Some(fb_text.clone())
            },
        });

        if approved {
            ctx.tx.send(AppEvent::ChunkStateChanged {
                chapter,
                chunk: chunk.index,
                state: ChunkState::Approved,
            });

            let bytes = translation::append_chunk(&ctx.ws, chapter, chunk.index as u32, &thai)
                .await
                .map_err(|e| anyhow::anyhow!("append chunk {} failed: {e}", chunk.index))?;

            ctx.tx.send(AppEvent::ChunkCommitted {
                chapter,
                chunk: chunk.index,
                bytes_written: bytes,
            });
            ctx.tx.send(AppEvent::ChunkStateChanged {
                chapter,
                chunk: chunk.index,
                state: ChunkState::Committed,
            });

            // Metadata turns can exceed the stall window without streaming; ping
            // while they run so the watchdog only trips on a wedged turn.
            let orch = {
                let turn = run_orchestrator_metadata_turn(ctx, chapter, &out);
                tokio::pin!(turn);
                loop {
                    tokio::select! {
                        biased;
                        r = &mut turn => break r,
                        _ = tokio::time::sleep(Duration::from_millis(500)) => wd.ping(),
                    }
                }
            };
            match orch {
                Ok((o_usage, n_tool_calls)) => {
                    acc.fold(&o_usage);
                    note_served_tier(ctx, acc, &ctx.models.orchestrator, &o_usage);
                    acc.add_tool_calls(n_tool_calls as u32);
                    ctx.tx.send(AppEvent::UsageUpdate {
                        run: acc.run,
                        chapter: acc.chapter,
                    });
                }
                // Metadata persistence is best-effort; never fail the chunk on it.
                Err(e) => {
                    ctx.tx.send(AppEvent::Error {
                        context: format!("orchestrator ch{chapter} chunk{}", chunk.index),
                        msg: e.to_string(),
                    });
                }
            }

            return Ok(ChunkOutcome::Committed);
        }

        if attempt < max {
            emit_attempt_failed_retry(ctx, chapter, chunk, attempt, max, &fb_text);
            feedback = Some(combine_review_feedback(&past_reviews, &fb_text));
            past_reviews.push(fb_text);
        } else {
            let reason = if fb_text.is_empty() {
                "reviewer rejected after max attempts".to_string()
            } else {
                fb_text
            };
            return commit_chunk_needs_review(ctx, chapter, chunk, &thai, max, reason).await;
        }
    }

    // Unreachable: the loop returns on approve, on the final rejection, and on a
    // terminal Translator/Reviewer error.
    anyhow::bail!(
        "chunk {} exhausted attempts without resolution",
        chunk.index
    )
}

const REVIEW_FEEDBACK_HISTORY_LIMIT: usize = 3;

/// Package reviewer rejections as a retry contract. The latest verdict comes
/// first; recent history is capped so long retry loops do not drown the fix.
fn combine_review_feedback(past: &[String], latest: &str) -> String {
    let retry_no = past.len() + 2;
    let mut s = format!(
        "รอบถัดไปคือ retry #{retry_no}: คำแปลก่อนหน้าถูก Reviewer ตีกลับ ต้องแก้ทุกข้อด้านล่างก่อนส่ง JSON ใหม่\n\
         ห้ามแก้แบบเดาสุ่มหรือ rewrite จนเกิดข้อผิดพลาดใหม่ ให้แก้จุดที่ถูกตีกลับและรักษาส่วนที่ถูกต้องไว้\n\n\
         [ข้อที่ต้องแก้ล่าสุด]\n{}\n",
        latest.trim()
    );

    if retry_no >= 4 {
        s.push_str(
            "\nคำเตือน: ชังก์นี้ถูกตีกลับหลายรอบแล้ว ก่อนตอบให้ทำ self-check กับ SOURCE_JP ทีละบรรทัด โดยเฉพาะชื่อ สรรพนาม POV ผู้พูด คำศัพท์ และประโยคท้ายชังก์\n",
        );
    }

    if !past.is_empty() {
        s.push_str("\n[ประวัติ feedback ล่าสุด ห้ามทำผิดซ้ำ]\n");
        let start = past.len().saturating_sub(REVIEW_FEEDBACK_HISTORY_LIMIT);
        for (idx, fb) in past.iter().enumerate().skip(start) {
            s.push_str(&format!("[รอบที่ {}]\n{}\n\n", idx + 1, fb.trim()));
        }
    }
    s
}

/// Emit the per-attempt "rejected, will retry" event pair the UI renders when an
/// attempt fails — either a reviewer rejection or a transient hard error — and at
/// least one more attempt remains.
fn emit_attempt_failed_retry(
    ctx: &PipelineCtx,
    chapter: u32,
    chunk: &Chunk,
    attempt: u32,
    max: u32,
    feedback: &str,
) {
    ctx.tx.send(AppEvent::ChunkStateChanged {
        chapter,
        chunk: chunk.index,
        state: ChunkState::Rejected,
    });
    ctx.tx.send(AppEvent::ChunkRetry {
        chapter,
        chunk: chunk.index,
        attempt,
        max,
        feedback: feedback.to_string(),
    });
}

/// Commit a chunk's best-available translation flagged for manual review, emitting
/// the same event sequence whether we got here by exhausting review rejections or
/// by hitting a transient Translator/Reviewer error we couldn't recover from. The
/// `[REVIEW NEEDED]` banner lets a human find and fix this one spot later; the
/// Orchestrator metadata turn is deliberately SKIPPED so an unreviewed translation
/// can't pollute the glossary/character roster.
async fn commit_chunk_needs_review(
    ctx: &PipelineCtx,
    chapter: u32,
    chunk: &Chunk,
    thai: &str,
    attempts: u32,
    reason: String,
) -> anyhow::Result<ChunkOutcome> {
    let bytes = translation::append_chunk_needs_review(
        &ctx.ws,
        chapter,
        chunk.index as u32,
        thai,
        attempts,
        &reason,
    )
    .await
    .map_err(|e| anyhow::anyhow!("append needs-review chunk {} failed: {e}", chunk.index))?;

    ctx.tx.send(AppEvent::ChunkStateChanged {
        chapter,
        chunk: chunk.index,
        state: ChunkState::NeedsReview,
    });
    // Count it as committed (it IS on disk) so the chapter's chunk progress reads
    // as fully written.
    ctx.tx.send(AppEvent::ChunkCommitted {
        chapter,
        chunk: chunk.index,
        bytes_written: bytes,
    });
    ctx.tx.send(AppEvent::ChunkNeedsReview {
        chapter,
        chunk: chunk.index,
        attempts,
        reason,
        salvaged: !thai.trim().is_empty(),
    });

    Ok(ChunkOutcome::NeedsReview)
}

fn controlled_terms_for_orchestrator(ws: &Workspace, out: &TranslatorOut) -> Vec<GlossaryTerm> {
    if out.new_terms.is_empty() {
        return Vec::new();
    }

    let mut terms: Vec<GlossaryTerm> = glossary::load(ws)
        .into_iter()
        .filter(glossary::blocks_automatic_update)
        .collect();

    // Prioritize controlled terms that resemble this chunk's reported discoveries,
    // then include a bounded fallback list so the Orchestrator can still reason
    // about nearby terminology without ballooning the prompt.
    terms.sort_by_key(|t| !controlled_term_matches_discovery(t, out));
    terms.truncate(MAX_PROTECTED_TERMS_FOR_ORCH);
    terms
}

fn controlled_term_matches_discovery(term: &GlossaryTerm, out: &TranslatorOut) -> bool {
    let jp = term.jp_term.trim();
    let th = term.thai_term.trim();
    out.new_terms.iter().any(|new| {
        let new_jp = new.jp_term.trim();
        let new_th = new.thai_term.trim();
        (!jp.is_empty() && !new_jp.is_empty() && (jp.contains(new_jp) || new_jp.contains(jp)))
            || (!th.is_empty()
                && !new_th.is_empty()
                && (th.contains(new_th) || new_th.contains(th)))
    })
}

/// Run the Orchestrator metadata turn for a just-approved chunk: a single tool
/// loop that lets the Orchestrator persist new characters / terms / continuity
/// notes and advance the volume recap through the backend tools.
async fn run_orchestrator_metadata_turn(
    ctx: &PipelineCtx,
    chapter: u32,
    out: &TranslatorOut,
) -> anyhow::Result<(Usage, usize)> {
    let controlled_terms = controlled_terms_for_orchestrator(&ctx.ws, out);
    let user = build_orchestrator_metadata_msg(chapter, out, &controlled_terms);

    let tools: Vec<Tool> = serde_json::from_value(orchestrator_tools())
        .map_err(|e| anyhow::anyhow!("failed to build orchestrator tools: {e}"))?;

    // tools present + tool_choice unset => OpenRouter defaults to "auto".
    // Leaving tool_choice at its Default avoids coupling to its exact field type.
    let req = ChatRequest {
        model: ctx.models.orchestrator.model.clone(),
        messages: vec![Message::system(ORCHESTRATOR_SYSTEM), Message::user(user)],
        temperature: Some(0.2),
        tools: Some(tools),
        reasoning: ctx.models.orchestrator.reasoning_param(),
        ..ChatRequest::default()
    };

    let executor = WorkspaceTools::new(
        ctx.ws.root.clone(),
        ctx.vol_number(),
        ctx.tx.clone(),
        chapter,
    );

    let orch_client = ctx.client_for(&ctx.models.orchestrator)?;
    let outcome = run_tool_loop(
        orch_client.as_ref(),
        req,
        &executor,
        ORCHESTRATOR_MAX_TOOL_ROUNDS,
    )
    .await
    .map_err(|e| anyhow::anyhow!("orchestrator tool loop failed: {e}"))?;

    Ok((outcome.usage, outcome.tool_calls))
}

/// Run the whole-chapter coherence sweep over the assembled Thai and persist any
/// warning/conflict findings as continuity notes (surfaced in the QA inbox).
/// Best-effort: a failed sweep logs and returns without affecting the outcome. A
/// keep-alive pings the watchdog so this non-streaming call can't read as a stall.
async fn run_coherence_sweep(
    ctx: &PipelineCtx,
    chapter: u32,
    raw: &str,
    acc: &mut Acc,
    wd: &Watchdog,
) {
    let assembled = translation::read_translated(&ctx.ws, chapter).await;
    let thai = strip_translation_markers(&assembled);
    if thai.trim().is_empty() {
        return;
    }
    // Scope the reference bundle to the whole chapter source so every character and
    // term the chapter uses is available to the auditor.
    let reference_ctx = build_reference_ctx(&ctx.ws, raw, None);

    let coherence_client = match ctx.client_for(&ctx.models.reviewer) {
        Ok(c) => c,
        Err(e) => {
            ctx.tx.send(AppEvent::Log {
                level: LogLevel::Warn,
                msg: format!("coherence sweep skipped: {e}"),
            });
            return;
        }
    };
    wd.ping();
    let result = {
        let fut = coherence::coherence_sweep(
            coherence_client.as_ref(),
            &ctx.models.reviewer,
            &thai,
            &reference_ctx,
        );
        tokio::pin!(fut);
        loop {
            tokio::select! {
                biased;
                r = &mut fut => break r,
                _ = tokio::time::sleep(Duration::from_millis(500)) => wd.ping(),
            }
        }
    };
    wd.ping();

    let (out, usage, truncated) = match result {
        Ok(v) => v,
        Err(e) => {
            ctx.tx.send(AppEvent::Log {
                level: LogLevel::Warn,
                msg: format!("coherence sweep failed for chapter {chapter}: {e}"),
            });
            return;
        }
    };
    acc.fold(&usage);
    note_served_tier(ctx, acc, &ctx.models.reviewer, &usage);
    ctx.tx.send(AppEvent::UsageUpdate {
        run: acc.run,
        chapter: acc.chapter,
    });
    if truncated {
        ctx.tx.send(AppEvent::Log {
            level: LogLevel::Info,
            msg: format!(
                "coherence sweep for chapter {chapter} examined the first {} chars (chapter exceeds the cap)",
                coherence::MAX_CHAPTER_CHARS
            ),
        });
    }

    let mut recorded = 0usize;
    for issue in &out.issues {
        // info-level notes are intentionally not persisted (the QA inbox skips them);
        // surface only actionable drift.
        let severity = issue.severity.trim().to_lowercase();
        if severity != "warning" && severity != "conflict" {
            continue;
        }
        let note_text = issue.note.trim();
        if note_text.is_empty() {
            continue;
        }
        let _ = volume::add_note(
            &ctx.ws,
            ContinuityNote {
                chapter: Some(chapter),
                severity: severity.clone(),
                kind: Some("coherence".to_string()),
                note: note_text.to_string(),
            },
        );
        ctx.tx.send(AppEvent::ContinuityFlag {
            chapter,
            severity: severity.clone(),
            kind: "coherence".to_string(),
            note: note_text.to_string(),
        });
        recorded += 1;

        // Pin a named drift only when one correct form is clear.
        if let Some(msg) = reconcile_coherence_issue(&ctx.ws, issue) {
            ctx.tx.send(AppEvent::Log {
                level: LogLevel::Info,
                msg: format!("coherence sweep: chapter {chapter} {msg}"),
            });
        }
    }
    if recorded > 0 {
        ctx.tx.send(AppEvent::Log {
            level: LogLevel::Warn,
            msg: format!(
                "coherence sweep: chapter {chapter} flagged {recorded} cross-chunk issue(s) for review"
            ),
        });
    }
}

/// Pin a named drift without creating characters or overwriting protected terms.
/// Returns a short log message when a roster entry changed.
fn reconcile_coherence_issue(ws: &Workspace, issue: &coherence::CoherenceIssue) -> Option<String> {
    let sev = issue.severity.trim().to_lowercase();
    if sev != "warning" && sev != "conflict" {
        return None;
    }
    let jp = issue.resolve_jp.trim();
    let th = issue.resolve_canonical_th.trim();
    if jp.is_empty() || th.is_empty() {
        return None;
    }
    match issue.resolve_kind.trim() {
        "term" => {
            let term = GlossaryTerm {
                jp_term: jp.to_string(),
                thai_term: th.to_string(),
                romaji: None,
                category: None,
                gloss: Some("canonical rendering pinned by the coherence sweep".to_string()),
                policy: Some(crate::model::TermPolicy::Preferred),
                forbidden_thai: Vec::new(),
                context_rule: None,
                protected: None,
                do_not_translate: None,
                first_seen_chapter: None,
            };
            match glossary::upsert_from_orchestrator(ws, term) {
                Ok(glossary::GlossaryUpsertOutcome::Protected { .. }) | Err(_) => None,
                Ok(_) => Some(format!("pinned term {jp} → {th} (preferred)")),
            }
        }
        "character" => {
            let existing = characters::get(ws, Some(jp), None)
                .into_iter()
                .find(|c| character_matches_surface(c, jp))?;
            if existing.thai_name.trim() == th {
                return None;
            }
            Some(format!(
                "kept character {jp} → {} unchanged; suggested {th} left as a note",
                existing.thai_name.trim()
            ))
        }
        _ => None,
    }
}

/// JP name or alias match.
fn character_matches_surface(c: &crate::model::Character, jp: &str) -> bool {
    let jp = jp.trim();
    c.jp_name.trim() == jp || c.aliases.iter().any(|a| a.trim() == jp)
}

/// Strip honya bookkeeping (chunk-index comments, the review-needed marker/banner)
/// from an assembled translated file so the coherence auditor sees only prose.
fn strip_translation_markers(text: &str) -> String {
    translation::export_prose(text)
}

#[cfg(test)]
mod queue_tests {
    use super::ChapterQueue;

    #[test]
    fn drains_in_order_then_empties() {
        let q = ChapterQueue::new(vec![(1, 1), (1, 2), (1, 3)]);
        assert_eq!(q.next(), Some((1, 1)));
        assert_eq!(q.next(), Some((1, 2)));
        assert_eq!(q.next(), Some((1, 3)));
        assert_eq!(q.next(), None);
        let (running, pending) = q.snapshot();
        assert_eq!(running, None);
        assert!(pending.is_empty());
    }

    #[test]
    fn enqueue_while_running_is_picked_up_at_next_pop() {
        let q = ChapterQueue::new(vec![(1, 1)]);
        assert_eq!(q.next(), Some((1, 1)));
        assert!(q.push_back(1, 5));
        assert_eq!(q.next(), Some((1, 5)));
        assert_eq!(q.next(), None);
    }

    #[test]
    fn push_back_dedupes_against_running_and_pending() {
        let q = ChapterQueue::new(vec![(1, 2)]);
        assert_eq!(q.next(), Some((1, 2)));
        assert!(
            !q.push_back(1, 2),
            "re-adding the running chapter is a no-op"
        );
        assert!(q.push_back(1, 4));
        assert!(!q.push_back(1, 4), "re-adding a pending chapter is a no-op");
        let (_, pending) = q.snapshot();
        assert_eq!(pending, vec![(1, 4)]);
    }

    #[test]
    fn reorder_and_sort_touch_only_pending() {
        let q = ChapterQueue::new(vec![(1, 5), (1, 3), (1, 4)]);
        assert_eq!(q.next(), Some((1, 5)));
        q.move_item_down(1, 3);
        assert_eq!(q.snapshot().1, vec![(1, 4), (1, 3)]);
        q.move_item_up(1, 3);
        assert_eq!(q.snapshot().1, vec![(1, 3), (1, 4)]);
        q.push_back(1, 1);
        q.sort_by_number();
        let (running, pending) = q.snapshot();
        assert_eq!(running, Some((1, 5)), "the head is never reordered/sorted");
        assert_eq!(pending, vec![(1, 1), (1, 3), (1, 4)]);
    }

    #[test]
    fn move_and_remove_are_identity_addressed_and_safe() {
        let q = ChapterQueue::new(vec![(1, 1), (1, 2)]);
        q.move_item_up(1, 1);
        q.move_item_down(1, 2);
        q.move_item_up(9, 9);
        assert_eq!(q.snapshot().1, vec![(1, 1), (1, 2)]);
        assert!(q.remove_item(1, 1));
        assert!(!q.remove_item(1, 9), "removing an absent item is a no-op");
        assert_eq!(q.snapshot().1, vec![(1, 2)]);
    }

    #[test]
    fn reorder_targets_by_identity_across_a_concurrent_pop() {
        let q = ChapterQueue::new(vec![(1, 2), (1, 3), (1, 4), (1, 5)]);
        assert_eq!(q.next(), Some((1, 2)));
        assert_eq!(q.next(), Some((1, 3)));
        q.move_item_up(1, 5);
        assert_eq!(q.snapshot().1, vec![(1, 5), (1, 4)]);
    }

    #[test]
    fn next_for_scopes_to_a_volume_and_advances_when_drained() {
        let q = ChapterQueue::new(vec![(1, 1), (2, 7), (1, 2)]);
        assert_eq!(q.next_for(1), Some((1, 1)));
        assert_eq!(q.next_for(1), Some((1, 2)));
        assert_eq!(q.next_for(1), None);
        assert_eq!(q.next_for(2), Some((2, 7)));
        assert_eq!(q.next_for(2), None);
    }

    #[test]
    fn seed_appends_without_duplicating() {
        let q = ChapterQueue::new(vec![(1, 1)]);
        q.seed(vec![(1, 1), (1, 2), (1, 3)]);
        assert_eq!(q.snapshot().1, vec![(1, 1), (1, 2), (1, 3)]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Character, GlossaryTerm};

    #[test]
    fn strip_translation_markers_removes_wrapped_review_banner() {
        let text = "<!-- honya:chunk 0 -->\n\
            <!-- honya:review-needed -->\n\
            > ⚠️ **[REVIEW NEEDED]** chunk 1 — แปลไม่ผ่าน\n\
            >\n\
            > เหตุผลจากผู้ตรวจ: ประโยคแรกยังผิด\n\
            > บริบทต่อท้ายที่ห้ามส่งกลับเข้าโมเดล\n\
            \n\
            เนื้อหาไทย\n\
            \n\
            <!-- honya:chunk 1 -->\n\
            คำแปลสอง\n";

        let stripped = strip_translation_markers(text);

        assert!(!stripped.contains("[REVIEW NEEDED]"));
        assert!(!stripped.contains("เหตุผลจากผู้ตรวจ"));
        assert!(!stripped.contains("บริบทต่อท้าย"));
        assert!(stripped.contains("เนื้อหาไทย"));
        assert!(stripped.contains("คำแปลสอง"));
    }

    #[test]
    fn combine_review_feedback_wraps_first_rejection() {
        let out = combine_review_feedback(&[], "fix tone");
        assert!(out.contains("retry #2"));
        assert!(out.contains("[ข้อที่ต้องแก้ล่าสุด]"));
        assert!(out.contains("fix tone"));
    }

    #[test]
    fn combine_review_feedback_prioritizes_latest_and_caps_history() {
        let past = vec![
            "round 1".into(),
            "round 2".into(),
            "round 3".into(),
            "round 4".into(),
        ];
        let out = combine_review_feedback(&past, "still off");
        assert!(out.contains("retry #6"));
        assert!(out.contains("still off"));
        assert!(out.contains("round 2"));
        assert!(out.contains("round 4"));
        assert!(!out.contains("round 1"));
        assert!(out.find("still off") < out.find("round 2"));
        assert!(out.contains("ถูกตีกลับหลายรอบ"));
    }

    fn temp_ws(tag: &str) -> (std::path::PathBuf, Workspace) {
        let base = std::env::temp_dir().join(format!("honya_ctx_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let ws = Workspace::new(base.clone(), 1);
        (base, ws)
    }

    fn term(jp: &str, th: &str) -> GlossaryTerm {
        GlossaryTerm {
            jp_term: jp.into(),
            thai_term: th.into(),
            romaji: None,
            category: None,
            gloss: None,
            policy: None,
            forbidden_thai: Vec::new(),
            context_rule: None,
            protected: None,
            do_not_translate: None,
            first_seen_chapter: None,
        }
    }

    fn character(id: &str, jp: &str, th: &str) -> Character {
        Character {
            id: id.into(),
            jp_name: jp.into(),
            thai_name: th.into(),
            romaji: None,
            gender: None,
            honorific: None,
            speech_style: None,
            relationships: Vec::new(),
            aliases: Vec::new(),
            also_called: Vec::new(),
            notes: None,
            first_seen_chapter: None,
        }
    }

    #[test]
    fn coherence_character_resolution_keeps_canonical_thai_name() {
        let (base, ws) = temp_ws("coherence_character_name");
        characters::upsert(&ws, character("ai", "清水愛", "ชิมิซุ ไอ")).unwrap();

        let issue = coherence::CoherenceIssue {
            severity: "warning".into(),
            note: "context form drift".into(),
            resolve_kind: "character".into(),
            resolve_jp: "清水愛".into(),
            resolve_canonical_th: "คุณไอ".into(),
        };
        let msg = reconcile_coherence_issue(&ws, &issue).unwrap();

        let chars = characters::load(&ws);
        assert_eq!(chars.len(), 1);
        assert_eq!(chars[0].thai_name, "ชิมิซุ ไอ");
        assert!(
            msg.contains("unchanged"),
            "coherence should report without mutating: {msg}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[derive(Default)]
    struct CountingClient {
        schemas: std::sync::Mutex<Vec<Option<String>>>,
    }

    impl CountingClient {
        fn schema_calls(&self, name: &str) -> usize {
            self.schemas
                .lock()
                .unwrap()
                .iter()
                .filter(|schema| schema.as_deref() == Some(name))
                .count()
        }
    }

    struct AuditRetryClient {
        schemas: std::sync::Mutex<Vec<Option<String>>>,
        translations: std::sync::Mutex<Vec<String>>,
    }

    impl AuditRetryClient {
        fn new(translations: Vec<&str>) -> Self {
            Self {
                schemas: std::sync::Mutex::new(Vec::new()),
                translations: std::sync::Mutex::new(
                    translations.into_iter().map(str::to_string).collect(),
                ),
            }
        }

        fn schema_calls(&self, name: &str) -> usize {
            self.schemas
                .lock()
                .unwrap()
                .iter()
                .filter(|schema| schema.as_deref() == Some(name))
                .count()
        }
    }

    #[async_trait::async_trait]
    impl crate::llm::client::LlmClient for AuditRetryClient {
        async fn chat(
            &self,
            req: &crate::llm::ChatRequest,
        ) -> crate::llm::client::Result<crate::llm::ChatResponse> {
            let schema_name = match &req.response_format {
                Some(crate::llm::ResponseFormat::JsonSchema { json_schema }) => {
                    Some(json_schema.name.clone())
                }
                _ => None,
            };
            self.schemas.lock().unwrap().push(schema_name.clone());

            let content = match schema_name.as_deref() {
                Some("translation_result") => {
                    let next = self.translations.lock().unwrap().remove(0);
                    serde_json::json!({
                        "thought_process": {
                            "scene_analysis": "(test)",
                            "glossary_check": "(test)"
                        },
                        "translated_text": next,
                        "new_characters": [],
                        "new_terms": [],
                        "continuity_notes": []
                    })
                    .to_string()
                }
                Some("review_result") => serde_json::json!({
                    "status": "approve",
                    "feedback": []
                })
                .to_string(),
                _ => "(test orchestrator: no tools)".to_string(),
            };

            Ok(crate::llm::ChatResponse {
                id: Some("audit-retry-client".to_string()),
                model: Some("honya/test".to_string()),
                choices: vec![crate::llm::Choice {
                    index: 0,
                    message: crate::llm::ResponseMessage {
                        role: Some("assistant".to_string()),
                        content: Some(content),
                        tool_calls: None,
                    },
                    finish_reason: Some("stop".to_string()),
                }],
                usage: Some(crate::llm::Usage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                    cost: 0.0,
                    cost_details: None,
                    ..Default::default()
                }),
                service_tier: None,
            })
        }
    }

    #[async_trait::async_trait]
    impl crate::llm::client::LlmClient for CountingClient {
        async fn chat(
            &self,
            req: &crate::llm::ChatRequest,
        ) -> crate::llm::client::Result<crate::llm::ChatResponse> {
            let schema_name = match &req.response_format {
                Some(crate::llm::ResponseFormat::JsonSchema { json_schema }) => {
                    Some(json_schema.name.clone())
                }
                _ => None,
            };
            self.schemas.lock().unwrap().push(schema_name.clone());

            let content = match schema_name.as_deref() {
                Some("translation_result") => serde_json::json!({
                    "thought_process": {
                        "scene_analysis": "(test)",
                        "glossary_check": "(test)"
                    },
                    "translated_text": "ข้อความแปลต่อ",
                    "new_characters": [],
                    "new_terms": [],
                    "continuity_notes": []
                })
                .to_string(),
                Some("review_result") => serde_json::json!({
                    "status": "approve",
                    "feedback": []
                })
                .to_string(),
                _ => "(test orchestrator: no tools)".to_string(),
            };

            Ok(crate::llm::ChatResponse {
                id: Some("counting-client".to_string()),
                model: Some("honya/test".to_string()),
                choices: vec![crate::llm::Choice {
                    index: 0,
                    message: crate::llm::ResponseMessage {
                        role: Some("assistant".to_string()),
                        content: Some(content),
                        tool_calls: None,
                    },
                    finish_reason: Some("stop".to_string()),
                }],
                usage: Some(crate::llm::Usage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                    cost: 0.0,
                    cost_details: None,
                    ..Default::default()
                }),
                service_tier: None,
            })
        }
    }

    #[tokio::test]
    async fn local_audit_forces_retry_even_if_reviewer_approves() {
        let (base, ws) = temp_ws("audit_retry");
        let raw = "一文目。\n\n---\n\n二文目。";
        translation::write_raw(&ws, 1, raw).unwrap();

        let client = std::sync::Arc::new(AuditRetryClient::new(vec![
            "<div>一文目。</div>\n\n二文目。",
            "ประโยคแรก\n\n---\n\nประโยคที่สอง",
        ]));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = PipelineCtx {
            clients: ClientSet::single(
                client.clone() as std::sync::Arc<dyn crate::llm::client::LlmClient>
            ),
            ws: ws.clone(),
            models: crate::model::ModelSet::default(),
            cfg: crate::model::AppConfig {
                max_attempts: 2,
                ..crate::model::AppConfig::default()
            },
            tx: crate::model::EventTx(tx),
            ctl: RunControl::new(),
            queue: ChapterQueue::new(vec![]),
        };
        let mut acc = Acc::default();
        let chunk = Chunk {
            index: 0,
            text: raw.to_string(),
            est_tokens: 1,
        };

        let wd = Watchdog::new(&ctx.cfg);
        match process_chunk(&ctx, 1, &chunk, &mut acc, &wd, &mut None, None)
            .await
            .expect("process_chunk")
        {
            ChunkOutcome::Committed => {}
            ChunkOutcome::NeedsReview => panic!("clean retry should be approved"),
        }

        assert_eq!(
            client.schema_calls("translation_result"),
            2,
            "audit findings should route back to the Translator"
        );
        assert_eq!(
            client.schema_calls("review_result"),
            2,
            "both attempts still pass through the Reviewer"
        );

        let translated = translation::read_translated(&ws, 1).await;
        assert!(translated.contains("ประโยคแรก"));
        assert!(!translated.contains("<div>"));

        let mut saw_audit_feedback = false;
        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::ChunkRetry { feedback, .. } = ev
                && feedback.contains("Local audit")
            {
                saw_audit_feedback = true;
            }
        }
        assert!(
            saw_audit_feedback,
            "retry feedback should include local audit findings"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn punctuation_residue_is_normalized_before_audit_retry() {
        let (base, ws) = temp_ws("punctuation_residue");
        let raw = "彼女は小さく（本当に小さく）頷いた。";
        translation::write_raw(&ws, 1, raw).unwrap();

        let client =
            std::sync::Arc::new(AuditRetryClient::new(vec!["เธอพยักหน้า（เบาจริง ๆ）อย่างลังเล"]));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = PipelineCtx {
            clients: ClientSet::single(
                client.clone() as std::sync::Arc<dyn crate::llm::client::LlmClient>
            ),
            ws: ws.clone(),
            models: crate::model::ModelSet::default(),
            cfg: crate::model::AppConfig {
                max_attempts: 1,
                ..crate::model::AppConfig::default()
            },
            tx: crate::model::EventTx(tx),
            ctl: RunControl::new(),
            queue: ChapterQueue::new(vec![]),
        };
        let mut acc = Acc::default();
        let chunk = Chunk {
            index: 0,
            text: raw.to_string(),
            est_tokens: 1,
        };

        let wd = Watchdog::new(&ctx.cfg);
        match process_chunk(&ctx, 1, &chunk, &mut acc, &wd, &mut None, None)
            .await
            .expect("process_chunk")
        {
            ChunkOutcome::Committed => {}
            ChunkOutcome::NeedsReview => panic!("normalized punctuation should be approved"),
        }

        assert_eq!(client.schema_calls("translation_result"), 1);
        assert_eq!(client.schema_calls("review_result"), 1);

        let translated = translation::read_translated(&ws, 1).await;
        assert!(translated.contains("(เบาจริง ๆ)"));
        assert!(!translated.contains('（'));
        assert!(!translated.contains('）'));

        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::ChunkRetry { feedback, .. } = ev {
                assert!(
                    !feedback.contains("Japanese punctuation"),
                    "normalization should not spend a retry on punctuation residue: {feedback}"
                );
            }
        }

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn repeated_stream_retries_only_the_active_chunk() {
        let (base, ws) = temp_ws("chunk_repeat_retry");
        let looped = "ก็ได้ครับ".repeat(20);
        let client = std::sync::Arc::new(AuditRetryClient::new(vec![&looped, "ข้อความแปลที่สะอาด"]));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = PipelineCtx {
            clients: ClientSet::single(
                client.clone() as std::sync::Arc<dyn crate::llm::client::LlmClient>
            ),
            ws: ws.clone(),
            models: crate::model::ModelSet::default(),
            cfg: crate::model::AppConfig {
                max_attempts: 2,
                ..crate::model::AppConfig::default()
            },
            tx: crate::model::EventTx(tx),
            ctl: RunControl::new(),
            queue: ChapterQueue::new(vec![]),
        };
        let chunk = Chunk {
            index: 0,
            text: "これは短い文です。".to_string(),
            est_tokens: 1,
        };
        let wd = Watchdog::new(&ctx.cfg);
        let mut acc = Acc::default();

        match process_chunk(&ctx, 1, &chunk, &mut acc, &wd, &mut None, None)
            .await
            .expect("process_chunk")
        {
            ChunkOutcome::Committed => {}
            ChunkOutcome::NeedsReview => panic!("clean retry should be approved"),
        }

        assert_eq!(
            client.schema_calls("translation_result"),
            2,
            "the repeated stream should spend one chunk retry"
        );
        assert_eq!(
            client.schema_calls("review_result"),
            1,
            "the repeated attempt should not reach the Reviewer"
        );

        let translated = translation::read_translated(&ws, 1).await;
        assert!(translated.contains("ข้อความแปลที่สะอาด"));
        assert!(!translated.contains(&looped));

        let mut saw_chunk_retry = false;
        let mut saw_chapter_loop = false;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AppEvent::ChunkRetry { feedback, .. }
                    if feedback.contains("started repeating inside this chunk") =>
                {
                    saw_chunk_retry = true;
                }
                AppEvent::ChapterLooping { .. } => saw_chapter_loop = true,
                _ => {}
            }
        }
        assert!(
            saw_chunk_retry,
            "repetition should surface as a chunk retry"
        );
        assert!(
            !saw_chapter_loop,
            "repetition must not wipe and retranslate the whole chapter"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn pipeline_resumes_from_committed_chunk_markers() {
        let (base, ws) = temp_ws("resume");
        let raw =
            "# 第一章\n\n一文目。\n\n二文目。\n\n三文目。\n\n四文目。\n\n五文目。\n\n六文目。";
        let cfg = crate::model::AppConfig {
            chunk_target_tokens: 4,
            chunk_hard_cap_tokens: 8,
            ..crate::model::AppConfig::default()
        };
        let chunks = chunk_chapter(raw, cfg.chunk_target_tokens, cfg.chunk_hard_cap_tokens);
        assert!(
            chunks.len() >= 3,
            "test raw should create multiple chunks: {chunks:?}"
        );

        translation::write_raw(&ws, 1, raw).unwrap();
        translation::append_chunk(&ws, 1, 0, "ข้อความเดิม")
            .await
            .unwrap();
        translation::append_chunk_needs_review(&ws, 1, 1, "คำแปลที่ต้องตรวจ", 3, "still rough")
            .await
            .unwrap();

        let client = std::sync::Arc::new(CountingClient::default());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = PipelineCtx {
            clients: ClientSet::single(
                client.clone() as std::sync::Arc<dyn crate::llm::client::LlmClient>
            ),
            ws: ws.clone(),
            models: crate::model::ModelSet::default(),
            cfg,
            tx: crate::model::EventTx(tx),
            ctl: RunControl::new(),
            queue: ChapterQueue::new(vec![]),
        };

        run_pipeline(ctx, vec![1]).await.expect("run_pipeline");

        assert_eq!(
            client.schema_calls("translation_result"),
            chunks.len() - 1,
            "only the clean existing marker should be skipped; review-needed chunks rerun"
        );
        let translated = translation::read_translated(&ws, 1).await;
        assert!(translated.contains("ข้อความเดิม"));
        assert!(
            !translated.contains(translation::REVIEW_NEEDED_MARKER),
            "approved retranslation should remove stale review-needed markers"
        );
        assert!(translation::review_needed_chunk_indices_in(&translated).is_empty());
        let committed = translation::committed_chunk_indices_in(&translated);
        assert_eq!(
            committed.len(),
            chunks.len(),
            "all chunks should be present after resume"
        );

        let mut saw_resume_log = false;
        let mut saw_recheck_log = false;
        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::Log { msg, .. } = ev {
                if msg.contains("resuming from translated file") {
                    saw_resume_log = true;
                }
                if msg.contains("rechecking") {
                    saw_recheck_log = true;
                }
            }
        }
        assert!(saw_resume_log, "resume should be visible in the run log");
        assert!(
            saw_recheck_log,
            "review-needed chunks should be visibly rerun"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Delegates to `CountingClient` but stamps a served tier onto the usage,
    /// mimicking the OpenRouter `service_tier` response echo.
    struct TierEchoClient {
        inner: CountingClient,
        tier: Option<crate::llm::ServedTier>,
    }

    #[async_trait::async_trait]
    impl crate::llm::client::LlmClient for TierEchoClient {
        async fn chat(
            &self,
            req: &crate::llm::ChatRequest,
        ) -> crate::llm::client::Result<crate::llm::ChatResponse> {
            let mut resp = self.inner.chat(req).await?;
            if let Some(u) = resp.usage.as_mut() {
                u.served_tier = self.tier;
            }
            Ok(resp)
        }
    }

    async fn run_with_tier_echo(
        tag: &str,
        served: Option<crate::llm::ServedTier>,
    ) -> Vec<(LogLevel, String)> {
        let (base, ws) = temp_ws(tag);
        let raw = "一文目。\n\n二文目。\n\n三文目。\n\n四文目。";
        translation::write_raw(&ws, 1, raw).unwrap();

        let cfg = crate::model::AppConfig {
            chunk_target_tokens: 4,
            chunk_hard_cap_tokens: 8,
            service_tier: Some(ServiceTier::Flex),
            ..crate::model::AppConfig::default()
        };
        let chunks = chunk_chapter(raw, cfg.chunk_target_tokens, cfg.chunk_hard_cap_tokens);
        assert!(
            chunks.len() >= 2,
            "need several chunks to prove per-model dedup"
        );

        let client = std::sync::Arc::new(TierEchoClient {
            inner: CountingClient::default(),
            tier: served,
        });
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = PipelineCtx {
            clients: ClientSet::single(client as std::sync::Arc<dyn crate::llm::client::LlmClient>),
            ws,
            models: crate::model::ModelSet::default(),
            cfg,
            tx: crate::model::EventTx(tx),
            ctl: RunControl::new(),
            queue: ChapterQueue::new(vec![]),
        };
        run_pipeline(ctx, vec![1]).await.expect("run_pipeline");

        let mut tier_logs = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::Log { level, msg } = ev
                && msg.starts_with("service tier")
            {
                tier_logs.push((level, msg));
            }
        }
        let _ = std::fs::remove_dir_all(&base);
        tier_logs
    }

    #[tokio::test]
    async fn service_tier_fallback_warns_once_per_model() {
        let logs = run_with_tier_echo("tier_fallback", None).await;
        // One notice per model (translator/reviewer/orchestrator), not per chunk.
        assert_eq!(logs.len(), 3, "{logs:?}");
        assert!(
            logs.iter()
                .all(|(level, msg)| matches!(level, LogLevel::Warn)
                    && msg.contains("flex not applied")
                    && msg.contains("standard rate")),
            "{logs:?}"
        );
    }

    #[tokio::test]
    async fn service_tier_match_confirms_once_per_model() {
        let logs = run_with_tier_echo("tier_match", Some(crate::llm::ServedTier::Flex)).await;
        assert_eq!(logs.len(), 3, "{logs:?}");
        assert!(
            logs.iter()
                .all(|(level, msg)| matches!(level, LogLevel::Info) && msg.contains("flex active")),
            "{logs:?}"
        );
    }

    /// The reference context injected per chunk must scope to terms/characters the
    /// chunk actually uses — otherwise it balloons with the whole accumulated
    /// roster as a volume progresses.
    #[test]
    fn reference_ctx_scopes_to_chunk() {
        let (base, ws) = temp_ws("ref");
        glossary::upsert(&ws, term("聖剣", "ดาบศักดิ์สิทธิ์")).unwrap();
        glossary::upsert(&ws, term("王都", "ราชธานี")).unwrap();
        characters::upsert(
            &ws,
            Character {
                id: "subaru".into(),
                jp_name: "スバル".into(),
                thai_name: "สบารุ".into(),
                romaji: None,
                gender: None,
                honorific: None,
                speech_style: None,
                relationships: Vec::new(),
                aliases: Vec::new(),
                also_called: Vec::new(),
                notes: None,
                first_seen_chapter: None,
            },
        )
        .unwrap();

        // The chunk references 聖剣 and スバル, but never 王都.
        let ctx = build_reference_ctx(&ws, "スバルは聖剣を抜いた。", None);
        assert!(
            ctx.contains("聖剣"),
            "in-chunk term must be injected:\n{ctx}"
        );
        assert!(
            ctx.contains("スバル"),
            "in-chunk character must be injected"
        );
        assert!(
            !ctx.contains("王都") && !ctx.contains("ราชธานี"),
            "absent term must NOT balloon the context:\n{ctx}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A chunk that uses only a character's alias (bare given name) must still pull
    /// in the one canonical entry, so the agents don't see them as a new person.
    #[test]
    fn reference_ctx_matches_alias() {
        let (base, ws) = temp_ws("ref_alias");
        let yuu = Character {
            id: "yuu".into(),
            jp_name: "有月勇".into(),
            thai_name: "อาริทสึกิ ยู".into(),
            romaji: Some("Aritsuki Yuu".into()),
            gender: None,
            honorific: None,
            speech_style: None,
            relationships: Vec::new(),
            aliases: vec!["勇".into()],
            also_called: Vec::new(),
            notes: None,
            first_seen_chapter: None,
        };
        // Persist the canonical entry with its alias.
        characters::upsert(&ws, yuu).unwrap();

        // The chunk only ever says 勇, never the full 有月勇.
        let ctx = build_reference_ctx(&ws, "勇は立ち上がった。", None);
        assert!(
            ctx.contains("อาริทสึกิ ยู"),
            "alias match must inject the canonical character:\n{ctx}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Carry-forward: a character present only in the PREVIOUS chunk (referred to in
    /// this chunk by pronoun only) must stay in the injected roster, so POV/pronoun
    /// guidance doesn't silently drop across a chunk boundary.
    #[test]
    fn reference_ctx_carries_previous_chunk_character() {
        let (base, ws) = temp_ws("ref_carry");
        characters::upsert(
            &ws,
            Character {
                id: "hikari".into(),
                jp_name: "ひかり".into(),
                thai_name: "ฮิคาริ".into(),
                romaji: None,
                gender: None,
                honorific: None,
                speech_style: Some("สรรพนามตัวเอง: ฉัน".into()),
                relationships: Vec::new(),
                aliases: Vec::new(),
                also_called: Vec::new(),
                notes: None,
                first_seen_chapter: None,
            },
        )
        .unwrap();

        // The current chunk never names ひかり; only the previous chunk did.
        let without = build_reference_ctx(&ws, "そして彼女は歩き出した。", None);
        assert!(
            !without.contains("ฮิคาริ"),
            "no carry → not injected:\n{without}"
        );

        let with = build_reference_ctx(
            &ws,
            "そして彼女は歩き出した。",
            Some("ひかりは振り返った。"),
        );
        assert!(
            with.contains("ฮิคาริ"),
            "previous-chunk character carried into scope:\n{with}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Volume style exemplars are injected as a few-shot block into every chunk's
    /// reference context.
    #[test]
    fn reference_ctx_injects_style_examples() {
        use crate::model::StyleExample;
        let (base, ws) = temp_ws("ref_style");
        std::fs::create_dir_all(&ws.vol_dir).unwrap();
        volume::add_style_examples(
            &ws,
            vec![StyleExample {
                jp: "彼は笑った。".into(),
                th: "เขาหัวเราะออกมา".into(),
                note: Some("น้ำเสียงสบาย ๆ".into()),
            }],
        )
        .unwrap();

        let ctx = build_reference_ctx(&ws, "無関係なテキスト", None);
        assert!(
            ctx.contains("STYLE_EXAMPLES"),
            "exemplar section present:\n{ctx}"
        );
        assert!(
            ctx.contains("เขาหัวเราะออกมา"),
            "exemplar Thai injected:\n{ctx}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A client whose translator always succeeds but whose reviewer always errors
    /// (a transient hard failure), to exercise the resilience path.
    struct ReviewerErrorClient;

    #[async_trait::async_trait]
    impl crate::llm::client::LlmClient for ReviewerErrorClient {
        async fn chat(
            &self,
            req: &crate::llm::ChatRequest,
        ) -> crate::llm::client::Result<crate::llm::ChatResponse> {
            let schema_name = match &req.response_format {
                Some(crate::llm::ResponseFormat::JsonSchema { json_schema }) => {
                    Some(json_schema.name.as_str())
                }
                _ => None,
            };
            // The reviewer hard-errors every time (e.g. transport / empty choices).
            if schema_name == Some("review_result") {
                return Err(crate::llm::client::LlmError::EmptyChoices);
            }
            let content = match schema_name {
                Some("translation_result") => serde_json::json!({
                    "thought_process": {"scene_analysis": "(t)", "glossary_check": "(t)"},
                    "translated_text": "ข้อความแปลภาษาไทย",
                    "new_characters": [],
                    "new_terms": [],
                    "continuity_notes": []
                })
                .to_string(),
                _ => "(orchestrator: no tools)".to_string(),
            };
            Ok(crate::llm::ChatResponse {
                id: Some("reviewer-error-client".to_string()),
                model: Some("honya/test".to_string()),
                choices: vec![crate::llm::Choice {
                    index: 0,
                    message: crate::llm::ResponseMessage {
                        role: Some("assistant".to_string()),
                        content: Some(content),
                        tool_calls: None,
                    },
                    finish_reason: Some("stop".to_string()),
                }],
                usage: Some(crate::llm::Usage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                    cost: 0.0,
                    cost_details: None,
                    ..Default::default()
                }),
                service_tier: None,
            })
        }
    }

    /// Regression: a single-chunk chapter whose only chunk hits a transient hard
    /// Reviewer error must NOT fail the whole chapter. The translation we already
    /// produced is committed flagged `NeedsReview` so the chapter completes (and
    /// the Thai is on disk) instead of showing ✗ Failed.
    #[tokio::test]
    async fn reviewer_hard_error_degrades_to_needs_review_not_failed() {
        let (base, ws) = temp_ws("reviewer_err");
        let raw = "# 第一章\n\nこれは短い章です。";
        translation::write_raw(&ws, 1, raw).unwrap();
        // Sanity: this raw really is a single chunk.
        let cfg = crate::model::AppConfig::default();
        assert_eq!(
            chunk_chapter(raw, cfg.chunk_target_tokens, cfg.chunk_hard_cap_tokens).len(),
            1,
            "test fixture must produce exactly one chunk"
        );

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = PipelineCtx {
            clients: ClientSet::single(std::sync::Arc::new(ReviewerErrorClient)
                as std::sync::Arc<dyn crate::llm::client::LlmClient>),
            ws: ws.clone(),
            models: crate::model::ModelSet::default(),
            cfg,
            tx: crate::model::EventTx(tx),
            ctl: RunControl::new(),
            queue: ChapterQueue::new(vec![]),
        };
        run_pipeline(ctx, vec![1]).await.expect("run_pipeline");

        let mut saw_failed = false;
        let mut final_state = None;
        let mut finished = None;
        let mut retries = 0u32;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AppEvent::ChapterFailed { .. } => saw_failed = true,
                AppEvent::ChapterStateChanged { state, .. } => final_state = Some(state),
                AppEvent::ChunkRetry { .. } => retries += 1,
                AppEvent::PipelineFinished {
                    chapters_done,
                    chapters_failed,
                    chapters_need_review,
                    ..
                } => finished = Some((chapters_done, chapters_failed, chapters_need_review)),
                _ => {}
            }
        }

        assert!(
            !saw_failed,
            "a transient reviewer error must not fail the chapter"
        );
        assert_eq!(
            final_state,
            Some(ChapterStatus::NeedsReview),
            "chapter should complete flagged NeedsReview"
        );
        assert_eq!(
            finished,
            Some((1, 0, 1)),
            "1 done (completed), 0 failed, 1 needs review"
        );
        assert!(
            retries >= 2,
            "the reviewer error should be retried before degrading (got {retries})"
        );

        // The translation we produced is on disk, flagged for manual review.
        let translated = translation::read_translated(&ws, 1).await;
        assert!(
            translated.contains("ข้อความแปลภาษาไทย"),
            "the produced translation must be committed, not discarded"
        );
        assert!(
            translated.contains(translation::REVIEW_NEEDED_MARKER),
            "the committed chunk must carry the review-needed marker"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn synopsis_injected_into_context_and_round_trips() {
        let (base, ws) = temp_ws("syn");
        volume::set_synopsis(&ws, "原文のあらすじ", "เรื่องย่อสำหรับบริบท").unwrap();

        // Round-trips both fields on disk.
        let loaded = volume::load(&ws);
        assert_eq!(loaded.synopsis_raw, "原文のあらすじ");
        assert_eq!(loaded.synopsis_th, "เรื่องย่อสำหรับบริบท");

        // The Thai synopsis is injected into every chunk's reference context.
        let ctx = build_reference_ctx(&ws, "無関係なテキスト", None);
        assert!(
            ctx.contains("VOLUME_SYNOPSIS") && ctx.contains("เรื่องย่อสำหรับบริบท"),
            "synopsis must be injected as context:\n{ctx}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn repetition_detector_flags_char_cycle() {
        let looped = "ก็ได้ครับ".repeat(20);
        assert!(
            looks_like_repetition_loop(&looped),
            "a phrase repeated 20× must read as a loop"
        );
    }

    #[test]
    fn repetition_detector_flags_repeated_lines() {
        let looped = "เขาเดินเข้ามาในห้อง\n".repeat(8);
        assert!(
            looks_like_repetition_loop(&looped),
            "the same line repeated 8× must read as a loop"
        );
    }

    #[test]
    fn repetition_detector_passes_normal_prose() {
        let prose = "เช้าวันนั้นแสงแดดสาดส่องเข้ามาทางหน้าต่าง เธอลุกขึ้นจากเตียงอย่างเชื่องช้า \
            แล้วเดินไปชงกาแฟสักแก้ว กลิ่นหอมอบอวลไปทั่วทั้งห้องครัวเล็ก ๆ ของเธอ \
            เสียงนกร้องดังมาจากต้นไม้ใหญ่หน้าบ้าน วันใหม่ได้เริ่มต้นขึ้นอีกครั้ง";
        assert!(
            !looks_like_repetition_loop(prose),
            "ordinary varied prose must not read as a loop"
        );
    }

    #[test]
    fn repetition_detector_allows_elongated_thai_shout() {
        let shout = format!("โว้ย{}\nเธอสูดหายใจแล้วพูดต่อ", "ย".repeat(80));
        assert!(
            !looks_like_repetition_loop(&shout),
            "an elongated single Thai character followed by another line is not a loop"
        );
    }

    #[test]
    fn repetition_detector_allows_streaming_elongated_tail() {
        let shout = format!("โว้ย{}", "ย".repeat(80));
        assert!(
            !looks_like_repetition_loop(&shout),
            "a stretched single-character tail should not trip before the next line arrives"
        );
    }

    #[test]
    fn repetition_detector_ignores_short_text() {
        assert!(
            !looks_like_repetition_loop("สั้น"),
            "too little text to judge must not trip"
        );
    }

    #[tokio::test]
    async fn disabled_watchdog_never_trips() {
        // loop_stall_secs = 0 is the global off-switch: neither arm may fire.
        let cfg = crate::model::AppConfig {
            loop_stall_secs: 0,
            ..crate::model::AppConfig::default()
        };
        let wd = Watchdog::new(&cfg);
        for _ in 0..40 {
            wd.feed_stream("ก็ได้ครับ");
        }
        assert!(
            !wd.repetition.load(Ordering::Relaxed),
            "repetition arm must stay off when the watchdog is disabled"
        );
        let ctl = RunControl::new();
        assert!(
            tokio::time::timeout(Duration::from_millis(300), wd.watch(&ctl))
                .await
                .is_err(),
            "a disabled watchdog must never trip"
        );
    }

    #[tokio::test]
    async fn stall_watchdog_trips_plain_idle_work_at_configured_window() {
        let wd = Watchdog::with_stall(Some(Duration::from_millis(120)));
        let ctl = RunControl::new();

        let reason = tokio::time::timeout(Duration::from_millis(260), wd.watch(&ctl))
            .await
            .expect("plain idle work should trip");

        assert_eq!(reason, LoopReason::Stall);
    }

    #[tokio::test]
    async fn stall_watchdog_graces_active_external_call_once() {
        let wd = Watchdog::with_stall(Some(Duration::from_millis(120)));
        let ctl = RunControl::new();
        let _wait = wd.external_wait();

        assert!(
            tokio::time::timeout(Duration::from_millis(170), wd.watch_active_call_stall(&ctl))
                .await
                .is_err(),
            "an active model call should not trip during the first quiet window"
        );

        let reason =
            tokio::time::timeout(Duration::from_millis(180), wd.watch_active_call_stall(&ctl))
                .await
                .expect("a still-silent model call should trip after the grace window");
        assert_eq!(reason, LoopReason::Stall);
    }

    #[tokio::test]
    async fn chapter_watchdog_ignores_active_external_call() {
        let wd = Watchdog::with_stall(Some(Duration::from_millis(80)));
        let ctl = RunControl::new();
        let _wait = wd.external_wait();

        assert!(
            tokio::time::timeout(Duration::from_millis(260), wd.watch(&ctl))
                .await
                .is_err(),
            "chapter-level recovery must not race ahead of chunk-level call recovery"
        );
    }

    /// A Translator whose every call hangs far longer than the watchdog's stall
    /// window — used to exercise the active-call stall arm.
    struct HangingClient;

    #[async_trait::async_trait]
    impl crate::llm::client::LlmClient for HangingClient {
        async fn chat(
            &self,
            _req: &crate::llm::ChatRequest,
        ) -> crate::llm::client::Result<crate::llm::ChatResponse> {
            // Far longer than the test's stall window; the watchdog cancels it.
            tokio::time::sleep(Duration::from_secs(30)).await;
            Err(crate::llm::client::LlmError::EmptyChoices)
        }
    }

    #[tokio::test]
    async fn watchdog_retries_stuck_chunk_before_chapter_retranslate() {
        let (base, ws) = temp_ws("watchdog_chunk_stall");
        translation::write_raw(&ws, 1, "# 第一章\n\nこれは短い章です。").unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = PipelineCtx {
            clients: ClientSet::single(std::sync::Arc::new(HangingClient)
                as std::sync::Arc<dyn crate::llm::client::LlmClient>),
            ws: ws.clone(),
            models: crate::model::ModelSet::default(),
            cfg: crate::model::AppConfig {
                max_attempts: 2,
                max_chapter_retranslates: 1,
                coherence_check: false,
                ..crate::model::AppConfig::default()
            },
            tx: crate::model::EventTx(tx),
            ctl: RunControl::new(),
            queue: ChapterQueue::new(vec![(1, 1)]),
        };
        // A sub-second stall so the stall arm fires without a real multi-second wait.
        let wd = Watchdog::with_stall(Some(Duration::from_millis(100)));
        let mut acc = Acc::default();
        let mut totals = Totals::default();

        let halt = run_volume_chapters(&ctx, None, &wd, &mut acc, &mut totals).await;

        assert!(
            matches!(halt, Halt::Completed),
            "a stuck model call should resolve at chunk scope, not halt the run"
        );
        assert_eq!(
            totals.failed, 0,
            "chunk-level stalls must not fail the chapter"
        );
        assert_eq!(
            totals.need_review, 1,
            "the unresolved chunk should be committed for review"
        );
        assert!(
            !ctx.ctl.is_stopped(),
            "chunk-level stall recovery must not stop the run control"
        );

        let mut chunk_retries = 0u32;
        let mut saw_needs_review = false;
        let mut saw_chapter_loop = false;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AppEvent::ChunkRetry {
                    attempt,
                    max,
                    feedback,
                    ..
                } if feedback.contains("made no progress") => {
                    chunk_retries += 1;
                    assert_eq!((attempt, max), (1, 2));
                }
                AppEvent::ChunkNeedsReview { reason, .. }
                    if reason.contains("translator stalled") =>
                {
                    saw_needs_review = true;
                }
                AppEvent::ChapterLooping { .. } => saw_chapter_loop = true,
                _ => {}
            }
        }
        assert_eq!(chunk_retries, 1, "first stall should retry the chunk once");
        assert!(
            saw_needs_review,
            "final stalled chunk should be visible as NeedsReview"
        );
        assert!(
            !saw_chapter_loop,
            "active-call stalls must not retranslate the whole chapter first"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn project_pipeline_runs_every_volume() {
        let base = std::env::temp_dir().join(format!("honya_proj_run_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        // Two volumes, one prose chapter each.
        let ws1 = Workspace::new(base.clone(), 1);
        let ws2 = Workspace::new(base.clone(), 2);
        translation::write_raw(&ws1, 1, "# 第一章\n\n短い章です。").unwrap();
        translation::write_raw(&ws2, 1, "# 第一章\n\n別の短い章です。").unwrap();

        let client = std::sync::Arc::new(CountingClient::default());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = PipelineCtx {
            clients: ClientSet::single(
                client.clone() as std::sync::Arc<dyn crate::llm::client::LlmClient>
            ),
            ws: ws1.clone(),
            models: crate::model::ModelSet::default(),
            cfg: crate::model::AppConfig::default(),
            tx: crate::model::EventTx(tx),
            ctl: RunControl::new(),
            queue: ChapterQueue::new(vec![]),
        };
        let plan = vec![
            VolumePlan {
                vol: 1,
                label: Some("一巻".to_string()),
                chapters: vec![1],
            },
            VolumePlan {
                vol: 2,
                label: None,
                chapters: vec![1],
            },
        ];

        run_project_pipeline(ctx, plan).await.expect("project run");

        let mut vols_started = Vec::new();
        let mut finished = None;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AppEvent::VolumeStarted { vol, .. } => vols_started.push(vol),
                AppEvent::PipelineFinished {
                    chapters_done,
                    stopped,
                    ..
                } => finished = Some((chapters_done, stopped)),
                _ => {}
            }
        }
        assert_eq!(
            vols_started,
            vec![1, 2],
            "each volume must announce its start"
        );
        assert_eq!(
            finished,
            Some((2, false)),
            "both volumes' chapters complete under one PipelineFinished"
        );
        // Both volumes were actually written.
        assert!(
            translation::read_translated(&ws1, 1)
                .await
                .contains("ข้อความแปลต่อ")
        );
        assert!(
            translation::read_translated(&ws2, 1)
                .await
                .contains("ข้อความแปลต่อ")
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn live_enqueue_is_drained_in_the_same_run() {
        let base = std::env::temp_dir().join(format!("honya_liveadd_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let ws = Workspace::new(base.clone(), 1);
        translation::write_raw(&ws, 1, "# 第一章\n\n短い章です。").unwrap();
        translation::write_raw(&ws, 2, "# 第二章\n\n別の短い章です。").unwrap();

        let client = std::sync::Arc::new(CountingClient::default());
        let queue = ChapterQueue::new(vec![]);
        assert!(queue.push_back(1, 2));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = PipelineCtx {
            clients: ClientSet::single(
                client.clone() as std::sync::Arc<dyn crate::llm::client::LlmClient>
            ),
            ws: ws.clone(),
            models: crate::model::ModelSet::default(),
            cfg: crate::model::AppConfig::default(),
            tx: crate::model::EventTx(tx),
            ctl: RunControl::new(),
            queue: queue.clone(),
        };

        run_pipeline(ctx, vec![1]).await.expect("run_pipeline");

        let mut completed = Vec::new();
        let mut finished = 0u32;
        let mut done_count = None;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AppEvent::ChapterCompleted { chapter } => completed.push(chapter),
                AppEvent::PipelineFinished { chapters_done, .. } => {
                    finished += 1;
                    done_count = Some(chapters_done);
                }
                _ => {}
            }
        }
        completed.sort_unstable();
        assert_eq!(completed, vec![1, 2], "the live-added chapter must run too");
        assert_eq!(
            finished, 1,
            "exactly one PipelineFinished for the whole run"
        );
        assert_eq!(done_count, Some(2));
        assert!(
            queue.snapshot().1.is_empty(),
            "the queue drains fully by the end of the run"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn cross_volume_enqueue_runs_in_its_own_volume() {
        let base = std::env::temp_dir().join(format!("honya_xvol_add_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let ws1 = Workspace::new(base.clone(), 1);
        let ws2 = Workspace::new(base.clone(), 2);
        translation::write_raw(&ws1, 1, "# 第一章\n\n短い章です。").unwrap();
        translation::write_raw(&ws2, 1, "# 第一章\n\n別の短い章です。").unwrap();
        translation::write_raw(&ws2, 2, "# 第二章\n\nさらに別の章です。").unwrap();

        let client = std::sync::Arc::new(CountingClient::default());
        let queue = ChapterQueue::new(vec![]);
        assert!(queue.push_back(2, 2));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = PipelineCtx {
            clients: ClientSet::single(
                client.clone() as std::sync::Arc<dyn crate::llm::client::LlmClient>
            ),
            ws: ws1.clone(),
            models: crate::model::ModelSet::default(),
            cfg: crate::model::AppConfig::default(),
            tx: crate::model::EventTx(tx),
            ctl: RunControl::new(),
            queue: queue.clone(),
        };
        let plan = vec![
            VolumePlan {
                vol: 1,
                label: None,
                chapters: vec![1],
            },
            VolumePlan {
                vol: 2,
                label: None,
                chapters: vec![1],
            },
        ];

        run_project_pipeline(ctx, plan).await.expect("project run");

        let mut finished = None;
        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::PipelineFinished { chapters_done, .. } = ev {
                finished = Some(chapters_done);
            }
        }
        assert_eq!(
            finished,
            Some(3),
            "Vol.1 ch1 + Vol.2 ch1 + the live-added Vol.2 ch2 all complete in one run"
        );
        assert!(
            translation::read_translated(&ws2, 2)
                .await
                .contains("ข้อความแปลต่อ"),
            "the cross-volume add was translated under its own volume"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn enqueue_to_a_volume_absent_from_the_plan_is_swept() {
        let base = std::env::temp_dir().join(format!("honya_sweep_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let ws1 = Workspace::new(base.clone(), 1);
        let ws3 = Workspace::new(base.clone(), 3);
        translation::write_raw(&ws1, 1, "# 第一章\n\n短い章です。").unwrap();
        translation::write_raw(&ws3, 1, "# 第一章\n\n三巻の章です。").unwrap();

        let client = std::sync::Arc::new(CountingClient::default());
        let queue = ChapterQueue::new(vec![]);
        assert!(queue.push_back(3, 1));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = PipelineCtx {
            clients: ClientSet::single(
                client.clone() as std::sync::Arc<dyn crate::llm::client::LlmClient>
            ),
            ws: ws1.clone(),
            models: crate::model::ModelSet::default(),
            cfg: crate::model::AppConfig::default(),
            tx: crate::model::EventTx(tx),
            ctl: RunControl::new(),
            queue: queue.clone(),
        };
        let plan = vec![VolumePlan {
            vol: 1,
            label: None,
            chapters: vec![1],
        }];

        run_project_pipeline(ctx, plan).await.expect("project run");

        let mut vols_started = Vec::new();
        let mut finished = None;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AppEvent::VolumeStarted { vol, .. } => vols_started.push(vol),
                AppEvent::PipelineFinished { chapters_done, .. } => finished = Some(chapters_done),
                _ => {}
            }
        }
        assert_eq!(
            finished,
            Some(2),
            "both Vol.1 ch1 and the swept Vol.3 ch1 ran"
        );
        assert_eq!(
            vols_started,
            vec![1, 3],
            "the sweep announces Vol.3 after the plan's Vol.1"
        );
        assert!(
            translation::read_translated(&ws3, 1)
                .await
                .contains("ข้อความแปลต่อ"),
            "the orphan was translated under Vol.3's workspace, not lost"
        );
        assert!(
            queue.snapshot().1.is_empty(),
            "nothing is left stranded in the queue"
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
