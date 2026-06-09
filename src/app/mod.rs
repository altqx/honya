//! App state, the global key router, and the top-level render dispatch.
//!
//! Layout invariant (see ui::layout::skeleton): header / tabs / rule / body /
//! toast / footer; the overlay is drawn LAST over a `Clear` so it always wins.

pub mod lexicon;
pub mod overlay;
pub mod project;
pub mod qa;
pub mod reader;
pub mod shelf;
pub mod translate;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ratatui::Frame;
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::llm::client::LlmClient;
use crate::model::{
    AppConfig, AppEvent, ChapterRun, ChapterStatus, EventTx, LogLevel, ModelSet, Project,
    ReaderAnnotation, RunHistoryEntry, RunHistoryStatus, ThemeId, UsageStats,
};
use crate::theme::Theme;
use crate::ui::chrome::{self, StatusTally};
use crate::ui::layout::{self, Skeleton};
use crate::ui::mouse::MouseInput;
use crate::ui::text::{thai_display_safe, truncate_cols};
use crate::workspace::Workspace;

use self::lexicon::LexiconScreen;
use self::overlay::{JumpKind, JumpTarget, Overlay};
use self::project::ProjectScreen;
use self::reader::ReaderScreen;
use self::shelf::ShelfScreen;
use self::translate::{QueueRow, TranslateScreen};

// ui::chrome imports `Screen`, so the variant names and ORDER are load-bearing.
/// The five primary tabs (1-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Shelf,
    Project,
    Translate,
    Reader,
    Lexicon,
}

impl Screen {
    /// Map a `1`..=`5` digit to its screen.
    pub fn from_digit(d: char) -> Option<Screen> {
        match d {
            '1' => Some(Screen::Shelf),
            '2' => Some(Screen::Project),
            '3' => Some(Screen::Translate),
            '4' => Some(Screen::Reader),
            '5' => Some(Screen::Lexicon),
            _ => None,
        }
    }
}

/// Everything a screen / overlay can ask the App to do; `apply` is the single mutation funnel.
#[derive(Debug, Clone)]
pub enum Action {
    None,
    Quit,
    Goto(Screen),
    ImportEpub {
        epub: PathBuf,
        title: String,
        vol: u32,
        synopsis_raw: String,
        synopsis_th: String,
    },
    /// Translate a volume synopsis with the Translator agent; `attempt` rises on
    /// each reroll to vary the sampling temperature. Result returns as an AppEvent.
    TranslateSynopsis {
        raw: String,
        attempt: u32,
    },
    /// Persist the active volume's synopsis (standalone editor accept).
    SaveSynopsis {
        raw: String,
        th: String,
    },
    /// Persist a human Reader annotation anchored to a translated line.
    SaveReaderNote {
        chapter: u32,
        line: u32,
        note: String,
    },
    OpenProject(String),
    /// Switch the open project's active volume (the one Reader / Translate / synopsis
    /// / QA resolve against). Emitted by the Project tree as the cursor moves between
    /// volumes ("auto-follow the cursor").
    SetActiveVolume {
        vol: u32,
    },
    /// Open the import wizard pre-targeted at the open project to add the next volume.
    AddVolume,
    /// Export the given volume of the open project to the chosen deliverable
    /// formats (merged Markdown / EPUB / DOCX), written under `<root>/exports/`.
    ExportVolume {
        vol: u32,
        formats: Vec<crate::export::ExportFormat>,
    },
    /// Permanently delete a project directory (raw + translations + metadata) from
    /// disk. Confirmed via a modal first; refuses if a run is in progress.
    DeleteProject {
        id: String,
    },
    OpenChapter {
        chapter: u32,
    },
    /// Open a chapter and scroll to a 1-based line (jump-to-section / bookmark).
    OpenChapterAt {
        chapter: u32,
        line: u32,
    },
    /// Run a Reader global search across both panes for `query`.
    ReaderSearch {
        query: String,
    },
    /// Toggle a navigation bookmark at a Reader line (add if absent, else remove).
    ToggleReaderBookmark {
        chapter: u32,
        line: u32,
        label: String,
    },
    /// User request to translate these chapters. The App may first ask whether
    /// to resume existing chunk markers or restart from scratch.
    StartTranslation {
        chapters: Vec<u32>,
    },
    /// Confirmed resume: keep existing translated chunks and continue at gaps.
    ContinueTranslation {
        chapters: Vec<u32>,
    },
    /// Confirmed restart: delete translated outputs before translating.
    RestartTranslation {
        chapters: Vec<u32>,
    },
    /// One-click auto-translate the whole project: every not-yet-done chapter
    /// across every volume, resume-aware. Raises a confirm first.
    StartProjectTranslation,
    /// Confirmed: begin the whole-project auto-translate run.
    BeginProjectTranslation,
    EnqueueChapters {
        vol: u32,
        chapters: Vec<u32>,
    },
    /// Reorder by `(vol, ch)` identity; the running head is not pending.
    QueueMoveUp {
        vol: u32,
        ch: u32,
    },
    QueueMoveDown {
        vol: u32,
        ch: u32,
    },
    SortQueue,
    DequeueChapter {
        vol: u32,
        ch: u32,
    },
    PauseRun,
    StopRun,
    /// Resume the interrupted run recorded in the recovery checkpoint (reopen the
    /// project and continue translating its chapter queue).
    ResumeSession,
    /// Forget the recovery checkpoint without resuming.
    DiscardSession,
    DeleteGlossary {
        jp_term: String,
    },
    DeleteCharacter {
        id: String,
    },
    SaveSettings {
        base_url: String,
        orchestrator: String,
        translator: String,
        reviewer: String,
        /// New config API key: `Some("")` clears it, `Some(k)` sets it, `None`
        /// leaves it untouched (the env var supplies the key, so config is moot).
        api_key: Option<String>,
        /// Startup update behavior (auto-install vs. notify only).
        update_mode: crate::model::UpdateMode,
        /// OpenRouter `service_tier` for every request (`None` = provider default).
        service_tier: Option<crate::model::ServiceTier>,
        /// Max Translator↔Reviewer retry attempts per chunk (already clamped 1..=20).
        max_attempts: u32,
        /// Loop-watchdog stall window in seconds (already clamped; 0 disables it).
        loop_stall_secs: u64,
        /// Whole-chapter re-translates before a looping chapter aborts the run.
        max_chapter_retranslates: u32,
    },
    /// Create the bundled sample project (if absent) and open it.
    CreateSample,
    /// Open the EPUB import wizard from the Welcome overlay (App supplies the list).
    OpenImport,
    /// Dismiss the Welcome overlay and persist that onboarding is complete.
    DismissWelcome,
    /// Live-preview a theme without persisting (theme picker navigation).
    PreviewTheme(ThemeId),
    /// Commit a theme: apply, persist to config, close the picker.
    SaveTheme(ThemeId),
    /// Abandon the picker: restore the previously-saved theme, close.
    CancelTheme,
    /// Boxed to break the `Action → Overlay → Dialog → Action` size cycle.
    ShowOverlay(Box<Overlay>),
    CloseOverlay,
}

impl Action {
    pub fn show_overlay(ov: Overlay) -> Self {
        Action::ShowOverlay(Box::new(ov))
    }
}

/// One toast line in the gap above the footer. Auto-dismissed on the next event.
#[derive(Debug, Clone)]
pub struct Toast {
    pub msg: String,
    pub level: LogLevel,
}

impl Toast {
    fn info(msg: impl Into<String>) -> Self {
        Self {
            msg: msg.into(),
            level: LogLevel::Info,
        }
    }
    fn warn(msg: impl Into<String>) -> Self {
        Self {
            msg: msg.into(),
            level: LogLevel::Warn,
        }
    }
    fn error(msg: impl Into<String>) -> Self {
        Self {
            msg: msg.into(),
            level: LogLevel::Error,
        }
    }
}

/// The currently-open project; `models` is the project override ∨ config default.
pub struct ActiveProject {
    pub project: Project,
    pub workspace: Workspace,
    /// The LLM client, built lazily: `None` when no API key is configured yet, so a
    /// project can still be opened and browsed offline (Reader / Lexicon). Built /
    /// cached on demand by `App::ensure_active_client` when a run needs it.
    pub client: Option<Arc<dyn LlmClient>>,
    pub models: ModelSet,
    /// The active volume number — the one `workspace` resolves and that runs
    /// translate against. Set when the project is opened (and honored on resume,
    /// so a checkpoint's volume is respected rather than silently defaulting to
    /// the first volume).
    pub vol: u32,
}

impl ActiveProject {
    /// The active volume number.
    fn active_vol(&self) -> u32 {
        self.vol
    }
}

pub struct App {
    pub running: bool,
    pub screen: Screen,
    pub overlay: Overlay,
    pub frame: u64,
    pub tx: EventTx,
    pub cfg: AppConfig,
    pub theme: Theme,
    pub projects: Vec<Project>,
    pub active: Option<ActiveProject>,
    pub shelf: ShelfScreen,
    pub project: ProjectScreen,
    pub translate: TranslateScreen,
    pub reader: ReaderScreen,
    pub lexicon: LexiconScreen,
    pub toast: Option<Toast>,
    /// True while a pipeline run is live (drives the tab-3 spinner badge).
    pub run_active: bool,
    /// True while a volume export is being written (guards against overlapping
    /// exports; independent of `run_active` so it never blocks a translation run).
    pub export_active: bool,
    /// Shared pause/stop control for the in-flight pipeline run (None when idle).
    pub run_ctl: Option<crate::agents::pipeline::RunControl>,
    pub run_queue: Option<crate::agents::pipeline::ChapterQueue>,
    /// Rolling activity log shown in the Log overlay.
    pub log: Vec<(LogLevel, String)>,
    /// Set when a newer release is detected at startup (drives a footer hint).
    pub update_available: Option<String>,
    /// Set when an auto-update installed a new release this session (drives a
    /// "restart to apply" footer badge; the running process is still the old one).
    pub update_installed: Option<String>,
    /// An interrupted run found at startup, awaiting the user's resume/discard
    /// choice in the recovery overlay (see `init_recovery_prompt`).
    pub pending_recovery: Option<crate::workspace::session::SessionCheckpoint>,
    /// Checkpoint for the currently running pipeline. Kept in memory so the final
    /// `PipelineFinished` event can close the matching VOLUME.md run-history row.
    pub active_run: Option<crate::workspace::session::SessionCheckpoint>,
    /// The volume the running pipeline is *currently* on, for an auto project run
    /// that walks volumes (`VolumeStarted`). Decoupled from `active_run.vol` (which
    /// stays at a stable volume so the run-history row keys consistently across
    /// resume) so per-chapter events — whose numbers repeat across volumes — scope
    /// to the live volume. `None` outside a multi-volume run.
    running_vol: Option<u32>,
    /// The per-chapter run record being assembled across this run's events (cost is
    /// folded in from `ChapterUsage`, QA/glossary deltas computed at the terminal
    /// state). Recorded to VOLUME.md so reruns can be compared. One at a time:
    /// chapters run sequentially.
    pending_chapter_run: Option<PendingChapterRun>,
    /// Mouse hit-testing state, refreshed every frame in `render`. The skeleton
    /// gives the header/tabs/body/footer regions; `tab_zones` maps each tab's
    /// rectangle to its screen; `last_area` is the full frame (for overlay modal
    /// geometry). `last_click` carries the previous left-press for double-click
    /// detection.
    last_area: Rect,
    last_skeleton: Option<Skeleton>,
    tab_zones: Vec<(Rect, Screen)>,
    last_click: Option<(std::time::Instant, u16, u16)>,
}

/// Accumulates one chapter's run facts between `ChapterStarted` and its terminal
/// state event, so the finished record can be written in one place.
struct PendingChapterRun {
    chapter: u32,
    run_id: String,
    /// Glossary jp_term → Thai snapshot taken at the chapter's start, diffed at the
    /// end to report what this run added / changed.
    glossary_before: HashMap<String, String>,
    /// This run's spend on the chapter, folded in from `ChapterUsage`.
    usage: UsageStats,
    /// Whether any usage was seen — image-only / instant-fail chapters get none and
    /// are not worth a comparison record.
    has_usage: bool,
}

impl App {
    pub fn new(tx: EventTx, cfg: AppConfig) -> Self {
        let projects = crate::workspace::scan::scan_projects(&working_root());
        let mut shelf = ShelfScreen::new();
        shelf.rescan(&working_root());
        if !projects.is_empty() {
            shelf.select_first();
        }
        let theme = cfg.theme.build();
        // Seed the Reader's chunk budgets from config so `s` (show source) re-derives
        // JA chunk boundaries the same way the pipeline did.
        let mut reader = ReaderScreen::new();
        reader.set_chunk_cfg(cfg.chunk_target_tokens, cfg.chunk_hard_cap_tokens);
        Self {
            running: true,
            screen: Screen::Shelf,
            overlay: Overlay::None,
            frame: 0,
            tx,
            cfg,
            theme,
            projects,
            active: None,
            shelf,
            project: ProjectScreen::new(),
            translate: TranslateScreen::new(),
            reader,
            lexicon: LexiconScreen::new(),
            toast: None,
            run_active: false,
            export_active: false,
            run_ctl: None,
            run_queue: None,
            log: Vec::new(),
            update_available: None,
            update_installed: None,
            pending_recovery: None,
            active_run: None,
            running_vol: None,
            pending_chapter_run: None,
            last_area: Rect::default(),
            last_skeleton: None,
            tab_zones: Vec::new(),
            last_click: None,
        }
    }

    /// Check for an interrupted run (crash / power loss / hard kill) and, if one
    /// is resumable, raise the recovery overlay. Called once from `main` after
    /// `App::new`; kept out of `App::new` itself so test apps never touch the real
    /// recovery file. A stale checkpoint (project gone, or already fully done) is
    /// cleared quietly rather than prompted.
    pub fn init_recovery_prompt(&mut self) {
        let Some(mut cp) = crate::workspace::session::load() else {
            return;
        };
        cp.ensure_run_id();
        if !cp.is_resumable() {
            crate::workspace::session::clear();
            return;
        }
        let (done, total) = self.recovery_progress(&cp);
        if total > 0 && done >= total {
            // The crash landed after the last chunk committed but before the run
            // was marked finished — nothing left to do, so don't nag. Close the
            // run-history row too so it never remains stuck at "running".
            self.finish_recovered_all_done(&cp, done as u32);
            crate::workspace::session::clear();
            return;
        }
        let body = recovery_body(&cp, done, total);
        self.overlay = Overlay::confirm_with_alternate(
            "Resume interrupted run?",
            body,
            "resume",
            Action::ResumeSession,
            'd',
            "discard",
            Action::DiscardSession,
        );
        self.pending_recovery = Some(cp);
    }

    /// Count how many of the checkpoint's chapters are already finished on disk,
    /// using the freshly-scanned project state when it is in the current shelf and
    /// falling back to scanning the checkpoint's absolute project path.
    fn recovery_progress(
        &self,
        cp: &crate::workspace::session::SessionCheckpoint,
    ) -> (usize, usize) {
        let project = self
            .projects
            .iter()
            .find(|p| p.dir == cp.project_dir)
            .cloned()
            .or_else(|| crate::workspace::scan::scan_one_project(&cp.project_dir));
        if cp.whole_project {
            // Whole-project run: count prose chapters across every volume.
            return project
                .map(|p| project_prose_progress(&p, &self.cfg))
                .unwrap_or((0, 0));
        }
        let total = cp.chapters.len();
        let done = project.map(|p| done_recovery_chapters(&p, cp)).unwrap_or(0);
        (done, total)
    }

    pub fn on_app_event(&mut self, ev: AppEvent) {
        // Translate screen observes everything so its live panel stays current off-tab.
        self.translate.on_app_event(&ev);

        match &ev {
            AppEvent::ChapterStarted { chapter } => {
                self.set_chapter_status(*chapter, ChapterStatus::Translating);
                self.run_active = true;
                self.begin_pending_chapter_run(*chapter);
                if let Some(title) = self.chapter_title(*chapter) {
                    self.translate.set_chapter_title(title);
                }
                // The pipeline popped this chapter to the queue's running head before
                // emitting the event, so refresh now drops it from the pending panel.
                self.refresh_queue_panel();
            }
            AppEvent::ChapterQueued { chapter } => {
                self.set_chapter_status(*chapter, ChapterStatus::Pending);
            }
            AppEvent::QueueChanged => {
                self.refresh_queue_panel();
            }
            AppEvent::VolumeStarted { vol, label } => {
                // The project run advanced to a new volume: re-point the live
                // volume so subsequent per-chapter events scope correctly, and
                // surface which volume is running.
                self.running_vol = Some(*vol);
                self.set_active_volume(*vol);
                let name = label
                    .as_deref()
                    .map(|l| format!("Vol.{vol:02} · {l}"))
                    .unwrap_or_else(|| format!("Vol.{vol:02}"));
                self.toast = Some(Toast::info(format!("translating {name}")));
                self.push_log(LogLevel::Info, format!("run advanced to {name}"));
            }
            AppEvent::ChapterLooping {
                chapter,
                reason,
                attempt,
                max,
            } => {
                self.toast = Some(Toast::warn(format!(
                    "ch {chapter} {reason} · re-translating whole chapter ({attempt}/{max})"
                )));
                self.push_log(
                    LogLevel::Warn,
                    format!(
                        "ch {chapter} {reason}; re-translating whole chapter ({attempt}/{max})"
                    ),
                );
            }
            AppEvent::ChapterStateChanged { chapter, state } => {
                self.set_chapter_status(*chapter, *state);
                if matches!(
                    state,
                    ChapterStatus::Done | ChapterStatus::NeedsReview | ChapterStatus::Failed
                ) {
                    self.finalize_pending_chapter_run(*chapter, *state);
                }
            }
            AppEvent::ChapterChunked {
                chapter,
                total_chunks,
                ..
            } => {
                self.set_chapter_chunks(*chapter, *total_chunks as u32, None);
            }
            AppEvent::ChunkCommitted {
                chapter,
                bytes_written,
                ..
            } => {
                self.bump_committed(*chapter, *bytes_written);
            }
            AppEvent::ChunkNeedsReview {
                chapter,
                chunk,
                attempts,
                reason,
            } => {
                self.toast = Some(Toast::warn(format!(
                    "ch {chapter} chunk {} committed unreviewed · needs manual review",
                    chunk + 1
                )));
                self.push_log(
                    LogLevel::Warn,
                    format!(
                        "ch {chapter} chunk {} committed unreviewed after {attempts} attempt(s): {reason}",
                        chunk + 1
                    ),
                );
            }
            AppEvent::ChapterCompleted { chapter } => {
                self.set_chapter_status(*chapter, ChapterStatus::Done);
                self.push_log(LogLevel::Info, format!("chapter {chapter} done"));
            }
            AppEvent::ChapterUsage { chapter, delta } => {
                self.add_chapter_usage(*chapter, delta);
                if let Some(p) = self.pending_chapter_run.as_mut()
                    && p.chapter == *chapter
                {
                    p.usage.add(delta);
                    p.has_usage = true;
                }
            }
            AppEvent::ChapterFailed { chapter, reason } => {
                self.set_chapter_status(*chapter, ChapterStatus::Failed);
                self.toast = Some(Toast::error(format!("ch {chapter} failed · {reason}")));
                self.push_log(LogLevel::Error, format!("ch {chapter} failed: {reason}"));
            }
            AppEvent::UpdateAvailable { version } => {
                self.update_available = Some(version.clone());
                self.toast = Some(Toast::info(format!(
                    "honya {version} available — run `honya update`"
                )));
                self.push_log(LogLevel::Info, format!("update available: honya {version}"));
            }
            AppEvent::UpdateInstalled { version } => {
                // The new binary is on disk; the running process is still the old
                // one, so the change applies on next launch.
                self.update_installed = Some(version.clone());
                self.update_available = None;
                self.toast = Some(Toast::info(format!(
                    "updated to honya {version} — restart to apply"
                )));
                self.push_log(
                    LogLevel::Info,
                    format!("auto-updated to honya {version}; restart to apply"),
                );
            }
            AppEvent::PipelinePaused => {
                // Stay run_active (held, not finished) so a second run can't start while paused.
                self.toast = Some(Toast::warn("run paused · p to resume"));
            }
            AppEvent::PipelineResumed => {
                self.run_active = true;
                self.toast = Some(Toast::info("run resumed"));
            }
            AppEvent::PipelineFinished {
                chapters_done,
                chapters_failed,
                chapters_need_review,
                stopped,
                run,
            } => {
                self.run_active = false;
                self.run_ctl = None;
                self.run_queue = None;
                self.running_vol = None;
                self.refresh_queue_panel();
                // A chapter interrupted by Stop never reaches a terminal state event,
                // so drop any half-built record rather than mis-recording it.
                self.pending_chapter_run = None;
                self.finish_active_run_history(
                    *chapters_done,
                    *chapters_failed,
                    *chapters_need_review,
                    *stopped,
                    *run,
                );
                // Mirror the now-current progress into STYLE.md / PROJECT.md so the
                // file views advance draft → in progress → done with the run.
                self.persist_project_status();
                // The run reached its end (finished, stopped, or all-failed): the
                // recovery checkpoint has served its purpose, so retire it.
                crate::workspace::session::clear();
                self.pending_recovery = None;
                let review = if *chapters_need_review > 0 {
                    format!(" · {chapters_need_review} need review")
                } else {
                    String::new()
                };
                let stopped_note = if *stopped { " · stopped" } else { "" };
                self.toast = Some(Toast::info(format!(
                    "run finished · {chapters_done} done · {chapters_failed} failed{review}{stopped_note}"
                )));
                self.push_log(
                    LogLevel::Info,
                    format!(
                        "pipeline finished: {chapters_done} done, {chapters_failed} failed, {chapters_need_review} need review{stopped_note}"
                    ),
                );
            }
            AppEvent::Error { context, msg } => {
                // An export failure clears its guard and dismisses the export overlay
                // so the error toast is visible.
                if self.export_active {
                    self.export_active = false;
                    if matches!(self.overlay, Overlay::Export(_)) {
                        self.overlay = Overlay::None;
                    }
                }
                self.toast = Some(Toast::error(format!("{context}: {msg}")));
                self.push_log(LogLevel::Error, format!("{context}: {msg}"));
            }
            AppEvent::Log { level, msg } => {
                self.push_log(*level, msg.clone());
            }
            AppEvent::ImportProgress { done, total, label } => {
                self.overlay.set_import_progress(*done, *total, label);
                self.toast = Some(Toast::info(format!("importing · {label}")));
            }
            AppEvent::ImportFinished { project_id, vol } => {
                self.run_active = false;
                self.overlay = Overlay::None;
                self.refresh_projects();
                self.toast = Some(Toast::info(format!("imported {project_id} · Vol.{vol:02}")));
                self.push_log(
                    LogLevel::Info,
                    format!("imported {project_id} Vol.{vol:02}"),
                );
                self.open_project(project_id.clone());
                // Land on the imported volume (Vol.01 for a fresh import, the new
                // volume for an add-volume), keeping the cursor and active volume in
                // sync so auto-follow doesn't flip it on the next keystroke.
                self.focus_active_volume(*vol);
            }
            AppEvent::ExportProgress { done, total, label } => {
                self.overlay.set_export_progress(*done, *total, label);
            }
            AppEvent::ExportFinished { paths, warnings } => {
                self.export_active = false;
                self.overlay
                    .set_export_done(paths.clone(), warnings.clone());
                let dir = paths
                    .first()
                    .and_then(|p| p.parent())
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                self.toast = Some(Toast::info(format!(
                    "exported {} file(s) → {dir}",
                    paths.len()
                )));
                self.push_log(
                    LogLevel::Info,
                    format!("exported {} file(s) to {dir}", paths.len()),
                );
                for w in warnings {
                    self.push_log(LogLevel::Warn, format!("export: {w}"));
                }
            }
            AppEvent::SynopsisTranslated { text } => {
                self.overlay.set_synopsis_result(Ok(text.clone()));
                self.toast = Some(Toast::info("synopsis translated"));
            }
            AppEvent::SynopsisFailed { msg } => {
                self.overlay.set_synopsis_result(Err(msg.clone()));
                self.push_log(LogLevel::Error, format!("synopsis: {msg}"));
                self.toast = Some(Toast::error(format!("synopsis: {msg}")));
            }
            AppEvent::CharacterUpserted { thai_name, .. } => {
                self.toast = Some(Toast::info(format!("character → {thai_name}")));
            }
            AppEvent::GlossaryUpserted { jp_term, thai_term } => {
                self.toast = Some(Toast::info(format!("glossary {jp_term} → {thai_term}")));
            }
            AppEvent::ContinuityFlag { severity, note, .. } => {
                self.push_log(LogLevel::Warn, format!("[{severity}] {note}"));
            }
            _ => {}
        }
    }

    /// The volume a pipeline event belongs to: the *running* volume during a run
    /// (recorded in the checkpoint), else the active volume. Chapter numbers are
    /// per-volume, so locating a chapter by number alone is ambiguous in a
    /// multi-volume project — every event-driven mutator scopes through here so a
    /// run on Vol.02 never touches Vol.01's same-numbered chapter.
    fn event_vol(&self) -> Option<u32> {
        self.running_vol
            .or_else(|| self.active_run.as_ref().map(|cp| cp.vol))
            .or_else(|| self.active.as_ref().map(|a| a.vol))
    }

    /// Mutable handle to `chapter` within the event's volume (see [`Self::event_vol`]).
    fn chapter_in_event_vol_mut(&mut self, chapter: u32) -> Option<&mut crate::model::Chapter> {
        let vol = self.event_vol()?;
        self.active
            .as_mut()?
            .project
            .volumes
            .iter_mut()
            .find(|v| v.number == vol)?
            .chapters
            .iter_mut()
            .find(|c| c.number == chapter)
    }

    fn set_chapter_status(&mut self, chapter: u32, status: ChapterStatus) {
        if let Some(ch) = self.chapter_in_event_vol_mut(chapter) {
            ch.status = status;
        }
    }

    /// Best-effort: mirror the active project's live translation status into its
    /// STYLE.md / PROJECT.md so the file views match the Context panel. Cheap and
    /// idempotent — [`scaffold::sync_status`] only writes on an actual change, so
    /// it's safe to call on every project open and run completion.
    fn persist_project_status(&mut self) {
        let Some(active) = self.active.as_ref() else {
            return;
        };
        let status = active.project.translation_progress().status;
        let ws = active.workspace.clone();
        if let Err(e) = crate::workspace::scaffold::sync_status(&ws, status) {
            self.push_log(
                LogLevel::Warn,
                format!("could not update project status: {e}"),
            );
        }
    }

    fn set_chapter_chunks(&mut self, chapter: u32, total: u32, committed: Option<u32>) {
        if let Some(ch) = self.chapter_in_event_vol_mut(chapter) {
            ch.total_chunks = total;
            if let Some(c) = committed {
                ch.committed_chunks = c;
            } else if total > 0 {
                ch.committed_chunks = ch.committed_chunks.min(total);
            }
        }
    }

    fn bump_committed(&mut self, chapter: u32, bytes_written: usize) {
        if bytes_written == 0 {
            return;
        }
        if let Some(ch) = self.chapter_in_event_vol_mut(chapter) {
            ch.committed_chunks = ch.committed_chunks.saturating_add(1);
            if ch.total_chunks > 0 {
                ch.committed_chunks = ch.committed_chunks.min(ch.total_chunks);
            }
        }
    }

    /// Start assembling a per-chapter run record (snapshot the glossary so we can
    /// report what this run adds / changes). Called on `ChapterStarted`.
    fn begin_pending_chapter_run(&mut self, chapter: u32) {
        // Bind the workspace to the running volume, not `active.workspace`, which can
        // drift if the user navigates to another volume mid-run (auto-follow).
        let Some(vol) = self.event_vol() else {
            return;
        };
        let Some(active) = self.active.as_ref() else {
            return;
        };
        let ws = Workspace::new(active.project.dir.clone(), vol);
        let run_id = self
            .active_run
            .as_ref()
            .map(|c| c.run_id.clone())
            .unwrap_or_default();
        let glossary_before = glossary_map(&ws);
        self.pending_chapter_run = Some(PendingChapterRun {
            chapter,
            run_id,
            glossary_before,
            usage: UsageStats::default(),
            has_usage: false,
        });
    }

    /// Finish and persist the per-chapter run record once the chapter reaches a
    /// terminal state, capturing the cost (folded earlier), the glossary delta vs
    /// the start-of-run snapshot, and the QA outcome (review-needed chunks / fail).
    fn finalize_pending_chapter_run(&mut self, chapter: u32, state: ChapterStatus) {
        let Some(pending) = self.pending_chapter_run.take() else {
            return;
        };
        // Only the chapter we were tracking, and only if it actually spent tokens
        // (image-only / instant-fail chapters have nothing worth comparing).
        if pending.chapter != chapter || !pending.has_usage {
            return;
        }
        // Bind to the running volume (see begin_pending_chapter_run) so the translated
        // file and VOLUME.md run record resolve to the volume actually translated.
        let Some(vol) = self.event_vol() else {
            return;
        };
        let Some(active) = self.active.as_ref() else {
            return;
        };
        let ws = Workspace::new(active.project.dir.clone(), vol);
        let total_chunks = self.find_chapter(chapter).map(|c| c.total_chunks);

        let after = glossary_map(&ws);
        let (glossary_added, glossary_changed) = glossary_delta(&pending.glossary_before, &after);

        let translated = std::fs::read_to_string(ws.translated(chapter)).unwrap_or_default();
        let review_needed =
            crate::workspace::translation::review_needed_chunk_indices_in(&translated).len() as u32;
        let committed_chunks =
            crate::workspace::translation::committed_chunk_indices_in(&translated).len() as u32;

        let run = ChapterRun {
            chapter,
            run_id: if pending.run_id.trim().is_empty() {
                "(run)".to_string()
            } else {
                pending.run_id
            },
            finished_at: chrono::Utc::now(),
            usage: pending.usage,
            usage_unknown: false,
            review_needed,
            failed: matches!(state, ChapterStatus::Failed),
            total_chunks: total_chunks.unwrap_or(committed_chunks),
            committed_chunks,
            glossary_added,
            glossary_changed,
            archived: None,
        };
        if let Err(e) = crate::workspace::volume::record_chapter_run(&ws, run) {
            self.push_log(
                LogLevel::Warn,
                format!("could not record chapter run for ch {chapter}: {e}"),
            );
        }
    }

    /// Archive a chapter's current translation before a restart deletes it, so the
    /// upcoming rerun can be diffed against it. Attaches the archive to that
    /// version's run record (or synthesizes a `"(prior)"` one for a pre-feature
    /// translation). Best-effort: failures only warn.
    fn archive_for_rerun(&mut self, ws: &Workspace, chapter: u32) {
        let path = ws.translated(chapter);
        let content = match std::fs::read_to_string(&path) {
            Ok(c) if !c.trim().is_empty() => c,
            _ => return, // nothing translated yet → nothing to archive
        };
        let dir = ws.reruns_dir(chapter);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.push_log(
                LogLevel::Warn,
                format!("could not create rerun archive dir for ch {chapter}: {e}"),
            );
            return;
        }
        let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
        let fname = format!("{stamp}.md");
        if let Err(e) = std::fs::write(dir.join(&fname), &content) {
            self.push_log(
                LogLevel::Warn,
                format!("could not archive prior translation for ch {chapter}: {e}"),
            );
            return;
        }
        let rel = format!("reruns/ch_{chapter:03}/{fname}");

        let review_needed =
            crate::workspace::translation::review_needed_chunk_indices_in(&content).len() as u32;
        let committed_chunks =
            crate::workspace::translation::committed_chunk_indices_in(&content).len() as u32;
        let total_chunks = self
            .find_chapter(chapter)
            .map(|c| c.total_chunks)
            .filter(|t| *t > 0)
            .unwrap_or(committed_chunks);
        let failed = matches!(
            self.find_chapter(chapter).map(|c| c.status),
            Some(ChapterStatus::Failed)
        );
        let finished_at = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .map(chrono::DateTime::<chrono::Utc>::from)
            .unwrap_or_else(|_| chrono::Utc::now());

        if let Err(e) = crate::workspace::volume::archive_prev_version(
            ws,
            chapter,
            &rel,
            crate::workspace::volume::PriorVersion {
                finished_at,
                review_needed,
                failed,
                total_chunks,
                committed_chunks,
            },
        ) {
            self.push_log(
                LogLevel::Warn,
                format!("could not record rerun archive for ch {chapter}: {e}"),
            );
        }
    }

    /// The in-memory chapter record (across the active project's volumes).
    fn find_chapter(&self, chapter: u32) -> Option<&crate::model::Chapter> {
        let vol = self.event_vol()?;
        self.active
            .as_ref()?
            .project
            .volumes
            .iter()
            .find(|v| v.number == vol)?
            .chapters
            .iter()
            .find(|c| c.number == chapter)
    }

    /// Fold a finished chapter's run usage into its in-memory lifetime total,
    /// mirroring the VOLUME.md persistence so the Project screen stays live.
    fn add_chapter_usage(&mut self, chapter: u32, delta: &UsageStats) {
        if let Some(ch) = self.chapter_in_event_vol_mut(chapter) {
            ch.usage.add(delta);
        }
    }

    fn finish_recovered_all_done(
        &mut self,
        cp: &crate::workspace::session::SessionCheckpoint,
        chapters_done: u32,
    ) {
        if cp.run_id.trim().is_empty() {
            return;
        }
        let chapters_need_review = self.recovery_need_review(cp) as u32;
        let status = if chapters_need_review > 0 {
            RunHistoryStatus::NeedsReview
        } else {
            RunHistoryStatus::Completed
        };
        let ws = Workspace::new(cp.project_dir.clone(), cp.vol);
        if let Err(e) = crate::workspace::volume::record_run_finished(
            &ws,
            &cp.run_id,
            crate::workspace::volume::RunHistoryFinish {
                status,
                finished_at: chrono::Utc::now(),
                chapters: cp.chapters.clone(),
                chapters_done,
                chapters_failed: 0,
                chapters_need_review,
                usage: UsageStats::default(),
            },
        ) {
            self.push_log(
                LogLevel::Warn,
                format!(
                    "could not close recovered run history for {}: {e}",
                    cp.project_id
                ),
            );
        }
    }

    fn recovery_need_review(&self, cp: &crate::workspace::session::SessionCheckpoint) -> usize {
        self.projects
            .iter()
            .find(|p| p.dir == cp.project_dir)
            .map(|p| need_review_recovery_chapters(p, cp))
            .or_else(|| {
                crate::workspace::scan::scan_one_project(&cp.project_dir)
                    .map(|p| need_review_recovery_chapters(&p, cp))
            })
            .unwrap_or(0)
    }

    fn finish_active_run_history(
        &mut self,
        chapters_done: u32,
        chapters_failed: u32,
        chapters_need_review: u32,
        stopped: bool,
        run: UsageStats,
    ) {
        let Some(mut cp) = self.active_run.take() else {
            return;
        };
        cp.ensure_run_id();
        let status = run_history_status(
            chapters_done,
            chapters_failed,
            chapters_need_review,
            stopped,
        );
        let ws = Workspace::new(cp.project_dir.clone(), cp.vol);
        if let Err(e) = crate::workspace::volume::record_run_finished(
            &ws,
            &cp.run_id,
            crate::workspace::volume::RunHistoryFinish {
                status,
                finished_at: chrono::Utc::now(),
                chapters: cp.chapters.clone(),
                chapters_done,
                chapters_failed,
                chapters_need_review,
                usage: run,
            },
        ) {
            self.push_log(
                LogLevel::Warn,
                format!("could not persist run history for {}: {e}", cp.project_id),
            );
        }
    }

    fn mark_recovery_discarded(&mut self, cp: &crate::workspace::session::SessionCheckpoint) {
        if cp.run_id.trim().is_empty() {
            return;
        }
        let ws = Workspace::new(cp.project_dir.clone(), cp.vol);
        if let Err(e) = crate::workspace::volume::record_run_discarded(&ws, &cp.run_id) {
            self.push_log(
                LogLevel::Warn,
                format!("could not mark run {} discarded: {e}", cp.run_id),
            );
        }
    }

    fn push_log(&mut self, level: LogLevel, msg: String) {
        self.log.push((level, msg));
        if self.log.len() > 500 {
            let overflow = self.log.len() - 500;
            self.log.drain(0..overflow);
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        let action = self.route_key(key);
        self.apply(action);
    }

    /// Fold one terminal mouse event into state. Motion / drag / button-up are
    /// dropped in [`MouseInput::from_event`]; the rest become an `Action` through
    /// the same `apply` funnel keys use. Left-press double-click is detected here
    /// (the App owns the clock) before the gesture is normalized.
    pub fn on_mouse(&mut self, me: MouseEvent) {
        let double = if matches!(me.kind, MouseEventKind::Down(MouseButton::Left)) {
            let now = std::time::Instant::now();
            let is_double = self.last_click.is_some_and(|(t, c, r)| {
                c == me.column
                    && r == me.row
                    && now.duration_since(t) < std::time::Duration::from_millis(400)
            });
            // Consume the pair so a third press doesn't read as another double.
            self.last_click = if is_double {
                None
            } else {
                Some((now, me.column, me.row))
            };
            is_double
        } else {
            false
        };
        let Some(input) = MouseInput::from_event(&me, double) else {
            return;
        };
        let action = self.route_mouse(input);
        self.apply(action);
    }

    /// Decide what a mouse gesture means given the current overlay / chrome /
    /// screen regions (mirrors `route_key`'s precedence: overlay first, then the
    /// global chrome, then the active screen).
    fn route_mouse(&mut self, m: MouseInput) -> Action {
        // 1) An open overlay gets first refusal, just like keys.
        if !matches!(self.overlay, Overlay::None) {
            return self.overlay.handle_mouse(m, self.last_area);
        }
        let Some(sk) = self.last_skeleton else {
            return Action::None;
        };

        // 2) The wheel always scrolls the active screen, wherever the pointer is.
        if m.is_scroll() {
            return self.route_mouse_to_screen(m);
        }

        // 3) Clicking the toast row dismisses it (matches Esc/Backspace).
        if self.toast.is_some() && m.in_rect(sk.toast) {
            self.toast = None;
            return Action::None;
        }

        // 4) Tab bar click → switch to that screen.
        if m.in_rect(sk.tabs) {
            if let Some((_, screen)) = self.tab_zones.iter().copied().find(|(r, _)| m.in_rect(*r)) {
                return Action::Goto(screen);
            }
            return Action::None;
        }

        // 5) Breadcrumb / header click → home to the Shelf.
        if m.is_click() && m.in_rect(sk.header) {
            return Action::Goto(Screen::Shelf);
        }

        // 6) Body → the active screen decides (select / activate / focus).
        if m.in_rect(sk.body) {
            return self.route_mouse_to_screen(m);
        }
        Action::None
    }

    fn route_mouse_to_screen(&mut self, m: MouseInput) -> Action {
        match self.screen {
            Screen::Shelf => self.shelf.handle_mouse(m, &self.projects),
            Screen::Project => self.project.handle_mouse(m, self.active.as_ref()),
            Screen::Translate => self.translate.handle_mouse(m),
            Screen::Reader => self.reader.handle_mouse(m),
            Screen::Lexicon => self
                .lexicon
                .handle_mouse(m, self.active.as_ref().map(|a| &a.workspace)),
        }
    }

    /// Decide what a key means given the current overlay / screen / focus state.
    fn route_key(&mut self, k: KeyEvent) -> Action {
        // Ctrl-C is a hard quit regardless of context.
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            return Action::Quit;
        }

        // 1) An open overlay gets first refusal (swallows single-letter globals when capturing).
        if !matches!(self.overlay, Overlay::None) {
            return self.overlay.handle_key(k);
        }

        // 2) Ctrl-P opens the palette even outside an overlay.
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('p') {
            return Action::show_overlay(Overlay::palette());
        }

        // 2a) Ctrl-T opens the theme picker from anywhere.
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('t') {
            return Action::show_overlay(Overlay::theme(self.cfg.theme));
        }

        // 2b) A focused screen text field (Lexicon search/editor) captures all keys.
        if self.screen_is_capturing() {
            return self.route_to_screen(k);
        }

        // 3) Global keys (only when nothing is capturing input).
        match k.code {
            KeyCode::Char(d @ '1'..='5') => {
                if let Some(s) = Screen::from_digit(d) {
                    return Action::Goto(s);
                }
            }
            KeyCode::Tab => {
                // Lexicon owns Tab to cycle its sub-sections; every other screen advances tabs.
                if matches!(self.screen, Screen::Lexicon) {
                    return self.route_to_screen(k);
                }
                return Action::Goto(self.next_screen());
            }
            KeyCode::Char('?') => return Action::show_overlay(Overlay::Help(0)),
            KeyCode::Char(':') => return Action::show_overlay(Overlay::palette()),
            KeyCode::Char('l')
                if matches!(self.screen, Screen::Project) && self.active.is_some() =>
            {
                return self.route_to_screen(k);
            }
            KeyCode::Char('l') | KeyCode::Char('`') => {
                return Action::show_overlay(Overlay::Log(0));
            }
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Esc | KeyCode::Backspace if self.toast.is_some() => {
                self.toast = None;
                return Action::None;
            }
            _ => {}
        }

        // 4) Otherwise the active screen decides.
        self.route_to_screen(k)
    }

    /// True when a focused screen text field should swallow single-letter globals.
    fn screen_is_capturing(&self) -> bool {
        matches!(self.screen, Screen::Lexicon) && self.lexicon.is_capturing()
    }

    fn route_to_screen(&mut self, k: KeyEvent) -> Action {
        match self.screen {
            Screen::Shelf => self.shelf.handle_key(k, &self.projects),
            Screen::Project => self.project.handle_key(k, self.active.as_ref()),
            Screen::Translate => self.translate.handle_key(k),
            Screen::Reader => self.reader.handle_key(k),
            Screen::Lexicon => self
                .lexicon
                .handle_key(k, self.active.as_ref().map(|a| &a.workspace)),
        }
    }

    fn next_screen(&self) -> Screen {
        match self.screen {
            Screen::Shelf => Screen::Project,
            Screen::Project => Screen::Translate,
            Screen::Translate => Screen::Reader,
            Screen::Reader => Screen::Lexicon,
            Screen::Lexicon => Screen::Shelf,
        }
    }

    fn apply(&mut self, a: Action) {
        match a {
            Action::None => {}
            Action::Quit => {
                self.running = false;
            }
            Action::Goto(s) => {
                self.screen = s;
                self.toast = None;
            }
            Action::ShowOverlay(ov) => {
                // Palette placeholders carry no config; rebuild from live config.
                self.overlay = match *ov {
                    // Welcome placeholders carry no status; rebuild from live state.
                    Overlay::Welcome(_) => {
                        let key = crate::config::resolve_api_key(&self.cfg).is_some();
                        let sample = crate::workspace::sample::sample_exists(&working_root());
                        Overlay::welcome(key, sample)
                    }
                    Overlay::Settings(ph) => Overlay::settings_with_field(&self.cfg, ph.field),
                    Overlay::Theme(_) => Overlay::theme(self.cfg.theme),
                    // QA placeholders (palette / screen `Q`) carry no data; rebuild
                    // the report from the live active project on show.
                    Overlay::Qa(_) => self.build_qa_overlay(),
                    // Reader jump placeholders carry no targets; rebuild the
                    // chapter/section/bookmark list from live state on show.
                    Overlay::ReaderJump(_) => self.build_jump_overlay(),
                    other => other,
                };
            }
            Action::CloseOverlay => {
                self.overlay = Overlay::None;
            }
            Action::OpenProject(id) => {
                self.open_project(id);
            }
            Action::SetActiveVolume { vol } => {
                self.set_active_volume(vol);
            }
            Action::AddVolume => {
                self.open_add_volume();
            }
            Action::ExportVolume { vol, formats } => {
                self.start_export(vol, formats);
            }
            Action::DeleteProject { id } => {
                self.delete_project(id);
            }
            Action::OpenChapter { chapter } => {
                self.open_chapter(chapter);
            }
            Action::OpenChapterAt { chapter, line } => {
                self.open_chapter(chapter);
                // open_chapter only navigates on success; scroll only when it landed.
                if matches!(self.screen, Screen::Reader) && self.reader.current_chapter() == chapter
                {
                    self.reader.scroll_to_line(line);
                }
            }
            Action::ReaderSearch { query } => {
                let count = self.reader.run_search(&query);
                self.toast = Some(if count == 0 {
                    Toast::info(format!("no matches for “{}”", query.trim()))
                } else {
                    Toast::info(format!("{count} match(es) · > next · < prev"))
                });
            }
            Action::ToggleReaderBookmark {
                chapter,
                line,
                label,
            } => {
                self.toggle_reader_bookmark(chapter, line, label);
            }
            Action::ImportEpub {
                epub,
                title,
                vol,
                synopsis_raw,
                synopsis_th,
            } => {
                self.start_import(epub, title, vol, synopsis_raw, synopsis_th);
            }
            Action::TranslateSynopsis { raw, attempt } => {
                self.translate_synopsis(raw, attempt);
            }
            Action::SaveSynopsis { raw, th } => {
                self.save_synopsis(raw, th);
            }
            Action::SaveReaderNote {
                chapter,
                line,
                note,
            } => {
                self.save_reader_note(chapter, line, note);
            }
            Action::StartTranslation { chapters } => {
                self.request_translation(chapters);
            }
            Action::EnqueueChapters { vol, chapters } => {
                if self.is_live_run() {
                    self.enqueue_live(vol, chapters);
                } else {
                    // Idle: start a fresh run (the continue/restart prompt applies).
                    self.request_translation(chapters);
                }
            }
            Action::QueueMoveUp { vol, ch } => {
                if let Some(q) = self.run_queue.as_ref() {
                    q.move_item_up(vol, ch);
                    self.refresh_queue_panel();
                    self.tx.send(AppEvent::QueueChanged);
                }
            }
            Action::QueueMoveDown { vol, ch } => {
                if let Some(q) = self.run_queue.as_ref() {
                    q.move_item_down(vol, ch);
                    self.refresh_queue_panel();
                    self.tx.send(AppEvent::QueueChanged);
                }
            }
            Action::SortQueue => {
                if let Some(q) = self.run_queue.as_ref() {
                    q.sort_by_number();
                    self.refresh_queue_panel();
                    self.tx.send(AppEvent::QueueChanged);
                }
            }
            Action::DequeueChapter { vol, ch } => {
                if let Some(q) = self.run_queue.as_ref() {
                    if q.remove_item(vol, ch) {
                        self.toast = Some(Toast::info(format!("removed ch {ch} from the queue")));
                    }
                    self.resync_run_checkpoint();
                    self.refresh_queue_panel();
                    self.tx.send(AppEvent::QueueChanged);
                }
            }
            Action::ContinueTranslation { chapters } => {
                self.begin_translation(chapters, false);
            }
            Action::RestartTranslation { chapters } => {
                self.begin_translation(chapters, true);
            }
            Action::StartProjectTranslation => {
                self.request_project_translation();
            }
            Action::BeginProjectTranslation => {
                self.begin_project_translation(None);
            }
            Action::PauseRun => match &self.run_ctl {
                Some(ctl) => {
                    ctl.toggle_pause();
                    self.toast = Some(Toast::info("pause toggled (resumes on p)"));
                }
                None => self.toast = Some(Toast::warn("no run in progress")),
            },
            Action::StopRun => match &self.run_ctl {
                Some(ctl) => {
                    ctl.stop();
                    self.toast = Some(Toast::warn("stopping after current chunk …"));
                }
                None => self.toast = Some(Toast::warn("no run in progress")),
            },
            Action::ResumeSession => {
                self.resume_session();
            }
            Action::DiscardSession => {
                if let Some(cp) = self.pending_recovery.take() {
                    self.mark_recovery_discarded(&cp);
                }
                crate::workspace::session::clear();
                self.overlay = Overlay::None;
                self.toast = Some(Toast::info("interrupted run discarded"));
            }
            Action::DeleteGlossary { jp_term } => {
                if let Some(active) = self.active.as_ref() {
                    match crate::workspace::glossary::remove(&active.workspace, &jp_term) {
                        Ok(()) => self.toast = Some(Toast::info(format!("removed {jp_term}"))),
                        Err(e) => self.toast = Some(Toast::error(format!("delete failed: {e}"))),
                    }
                }
                self.overlay = Overlay::None;
            }
            Action::DeleteCharacter { id } => {
                if let Some(active) = self.active.as_ref() {
                    match crate::workspace::characters::remove(&active.workspace, &id) {
                        Ok(()) => self.toast = Some(Toast::info("character removed")),
                        Err(e) => self.toast = Some(Toast::error(format!("delete failed: {e}"))),
                    }
                }
                self.overlay = Overlay::None;
            }
            Action::SaveSettings {
                base_url,
                orchestrator,
                translator,
                reviewer,
                api_key,
                update_mode,
                service_tier,
                max_attempts,
                loop_stall_secs,
                max_chapter_retranslates,
            } => {
                self.save_settings(
                    base_url,
                    orchestrator,
                    translator,
                    reviewer,
                    api_key,
                    update_mode,
                    service_tier,
                    max_attempts,
                    loop_stall_secs,
                    max_chapter_retranslates,
                );
            }
            Action::CreateSample => {
                self.create_sample_project();
            }
            Action::OpenImport => {
                let epubs = crate::workspace::scan::find_unimported_epubs(&working_root())
                    .into_iter()
                    .map(|(p, _)| p)
                    .collect();
                self.overlay = Overlay::import(epubs);
            }
            Action::DismissWelcome => {
                self.mark_onboarded();
                self.overlay = Overlay::None;
            }
            Action::PreviewTheme(id) => {
                // Live recolor only; picker stays open, config untouched.
                self.theme = id.build();
            }
            Action::SaveTheme(id) => {
                self.cfg.theme = id;
                self.theme = id.build();
                match crate::config::save(&self.cfg) {
                    Ok(()) => self.toast = Some(Toast::info(format!("theme → {}", id.label()))),
                    Err(e) => self.toast = Some(Toast::error(format!("save failed: {e}"))),
                }
                self.overlay = Overlay::None;
            }
            Action::CancelTheme => {
                // Revert any preview to the saved theme and close.
                self.theme = self.cfg.theme.build();
                self.overlay = Overlay::None;
            }
        }
    }

    fn open_project(&mut self, id: String) {
        // Re-scan from disk first: `self.projects` is otherwise only populated at launch,
        // so without this, re-open would clone a stale snapshot and revert done chapters.
        self.refresh_projects();
        let Some(project) = self.projects.iter().find(|p| p.id == id).cloned() else {
            self.toast = Some(Toast::error(format!("project {id} not found")));
            return;
        };
        let vol = project.volumes.first().map(|v| v.number).unwrap_or(1);
        self.activate_project(project, vol);
    }

    /// Switch the active volume in place (no screen reset): re-point the workspace at
    /// `vol` so Reader / Translate / synopsis / QA resolve against it. No-op if `vol`
    /// is already active or absent. Lightweight — called as the Project tree cursor
    /// crosses volume boundaries, so it must stay cheap and side-effect-free.
    fn set_active_volume(&mut self, vol: u32) {
        if let Some(active) = self.active.as_mut()
            && active.vol != vol
            && active.project.volumes.iter().any(|v| v.number == vol)
        {
            active.vol = vol;
            active.workspace = Workspace::new(active.project.dir.clone(), vol);
        }
    }

    /// Move the Project tree cursor onto `vol` and make it the active volume, so the
    /// user lands on a freshly-imported volume (cursor and active volume stay in
    /// sync). No-op if no project is open or `vol` is absent.
    fn focus_active_volume(&mut self, vol: u32) {
        if let Some(active) = self.active.as_ref() {
            self.project.focus_volume(active, vol);
        }
        self.set_active_volume(vol);
    }

    /// Open the import wizard pre-targeted at the open project to add its next
    /// volume: the name is locked to the project's and the volume defaults to one
    /// past its highest. The import merges in because the slug collides.
    fn open_add_volume(&mut self) {
        if self.run_active {
            self.toast = Some(Toast::warn("a run is already in progress"));
            return;
        }
        let Some(active) = self.active.as_ref() else {
            self.toast = Some(Toast::warn("no project open"));
            return;
        };
        let title = active.project.title.clone();
        let next = active
            .project
            .volumes
            .iter()
            .map(|v| v.number)
            .max()
            .unwrap_or(0)
            + 1;
        let epubs: Vec<PathBuf> = crate::workspace::scan::find_unimported_epubs(&working_root())
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        self.overlay = Overlay::import_into(epubs, title, next);
    }

    /// Permanently delete a project directory from disk (the only way to remove it
    /// from the shelf, which is a live scan of the working root — there is no
    /// separate list to drop it from). Guarded: refuses during an active run, and
    /// `remove_project_dir` refuses any directory that is not actually a project.
    fn delete_project(&mut self, id: String) {
        if self.run_active {
            self.toast = Some(Toast::warn(
                "can't delete a project while a run is in progress",
            ));
            return;
        }
        // The id comes from the rendered shelf (`self.projects`), so find it there;
        // a stale entry just fails the disk guard below rather than deleting wrongly.
        let Some(project) = self.projects.iter().find(|p| p.id == id).cloned() else {
            self.toast = Some(Toast::error(format!("project {id} not found")));
            return;
        };
        match remove_project_dir(&project.dir) {
            Ok(()) => {
                // Close it if it was the open project, so we don't keep a dangling
                // workspace pointed at a now-deleted directory.
                if self.active.as_ref().is_some_and(|a| a.project.id == id) {
                    self.active = None;
                    self.screen = Screen::Shelf;
                }
                let root = working_root();
                self.refresh_projects();
                self.shelf.rescan(&root);
                self.shelf.select_first();
                self.push_log(LogLevel::Info, format!("deleted project {id}"));
                self.toast = Some(Toast::info(format!("deleted {}", project.title)));
            }
            Err(e) => {
                self.toast = Some(Toast::error(format!("delete failed: {e}")));
            }
        }
    }

    /// Make `project` the active project on volume `vol`: build its LLM client (if a
    /// key is configured), reset the per-project screens, and land on the Project
    /// tab. Always succeeds — a missing key only disables translation, not browsing,
    /// so the client is left `None` and built lazily once a key is set. Returns
    /// `true` for callers that branch on activation.
    fn activate_project(&mut self, project: Project, vol: u32) -> bool {
        let models = project
            .models
            .clone()
            .unwrap_or_else(|| self.cfg.models.clone());
        let workspace = Workspace::new(project.dir.clone(), vol);
        let client = crate::build_client(&self.cfg).ok();
        // Distinguish "no key configured" (the expected offline case) from a client
        // that failed to build despite a key — only the former drives the hint toast.
        let no_key = crate::config::resolve_api_key(&self.cfg).is_none();
        let id = project.id.clone();
        self.active = Some(ActiveProject {
            project,
            workspace,
            client,
            models,
            vol,
        });
        self.lexicon.reset();
        self.project = ProjectScreen::new();
        self.screen = Screen::Project;
        // Reconcile STYLE.md / PROJECT.md with the freshly-scanned progress, so a
        // project finished in a previous session no longer shows a stale "draft".
        self.persist_project_status();
        self.toast = Some(if no_key {
            Toast::info(format!(
                "opened {id} · add an API key in Settings to translate"
            ))
        } else {
            Toast::info(format!("opened {id}"))
        });
        true
    }

    /// Resolve the active project's LLM client, building and caching it from config
    /// if it was opened without a key (the user may have since added one in
    /// Settings). `None` means no key is configured anywhere.
    fn ensure_active_client(&mut self) -> Option<Arc<dyn LlmClient>> {
        let active = self.active.as_mut()?;
        if active.client.is_none() {
            active.client = crate::build_client(&self.cfg).ok();
        }
        active.client.clone()
    }

    /// Resume the interrupted run from the recovery checkpoint: reopen its project
    /// (by absolute path, so a different launch directory still works) and continue
    /// translating its chapter queue. The pipeline's own chunk-level resume then
    /// skips everything already committed to disk.
    fn resume_session(&mut self) {
        let Some(mut cp) = self.pending_recovery.take() else {
            self.toast = Some(Toast::warn("no run to resume"));
            return;
        };
        cp.ensure_run_id();
        self.overlay = Overlay::None;
        // Match strictly on the absolute project directory — never on the slug
        // alone, or launching from elsewhere could resume into a *different*
        // same-named project. The scan fallback covers an out-of-shelf project.
        self.refresh_projects();
        let project = self
            .projects
            .iter()
            .find(|p| p.dir == cp.project_dir)
            .cloned()
            .or_else(|| crate::workspace::scan::scan_one_project(&cp.project_dir));
        let Some(project) = project else {
            crate::workspace::session::clear();
            self.toast = Some(Toast::error(format!(
                "could not reopen {} to resume",
                cp.project_id
            )));
            return;
        };
        if !self.activate_project(project, cp.vol) {
            return; // activate_project already toasted the reason
        }
        // Continue (not restart): keep committed chunks, fill the gaps, and keep
        // writing to the same durable run-history row.
        if cp.whole_project {
            // Whole-project run: the queue is recomputed from the freshly-scanned
            // project across all volumes (the checkpoint only recorded the segment
            // in flight at crash time), so resume is inherently gap-filling.
            self.begin_project_translation(Some(cp));
        } else {
            self.begin_translation_with_checkpoint(cp.chapters.clone(), false, Some(cp));
        }
    }

    fn open_chapter(&mut self, chapter: u32) {
        if let Some(active) = self.active.as_ref() {
            let vol = active.active_vol();
            let max = active
                .project
                .volumes
                .iter()
                .find(|v| v.number == vol)
                .map(|v| v.chapters.iter().map(|c| c.number).max().unwrap_or(0))
                .unwrap_or(0);
            if chapter < 1 || chapter > max {
                self.toast = Some(Toast::info("no more chapters"));
                return;
            }
            self.reader.load(&active.workspace, chapter);
            self.screen = Screen::Reader;
        } else {
            self.toast = Some(Toast::warn("no project open"));
        }
    }

    /// A chapter's display title from the running/active volume (live Translate header).
    fn chapter_title(&self, chapter: u32) -> Option<String> {
        self.find_chapter(chapter).map(|c| c.title.clone())
    }

    /// A run is live and accepting enqueues (not idle and not already stopping).
    fn is_live_run(&self) -> bool {
        self.run_active
            && self.run_queue.is_some()
            && !self
                .run_ctl
                .as_ref()
                .map(|c| c.is_stopped())
                .unwrap_or(false)
    }

    fn enqueue_live(&mut self, vol: u32, chapters: Vec<u32>) {
        let Some(queue) = self.run_queue.clone() else {
            return;
        };
        if chapters.is_empty() {
            self.toast = Some(Toast::warn("nothing selected"));
            return;
        }
        let whole_project = self
            .active_run
            .as_ref()
            .map(|cp| cp.whole_project)
            .unwrap_or(false);
        if !whole_project {
            // Single-volume runs resolve raw files through one workspace.
            let run_vol = self.active_run.as_ref().map(|cp| cp.vol);
            if Some(vol) != run_vol {
                self.toast = Some(Toast::warn(format!(
                    "this run only translates Vol.{:02} — can't queue another volume",
                    run_vol.unwrap_or(vol)
                )));
                return;
            }
        }
        let mut added = 0u32;
        let mut skipped = 0u32;
        for ch in chapters {
            let eligible = self
                .chapter_in_vol(vol, ch)
                .map(project::translatable)
                .unwrap_or(false);
            if eligible && queue.push_back(vol, ch) {
                added += 1;
            } else {
                skipped += 1;
            }
        }
        self.toast = Some(Toast::info(if skipped > 0 {
            format!("queued {added} · skipped {skipped} (done / running / not prose)")
        } else {
            format!("queued {added} chapter(s)")
        }));
        if added > 0 {
            self.push_log(LogLevel::Info, format!("enqueued {added} chapter(s)"));
            self.resync_run_checkpoint();
        }
        self.refresh_queue_panel();
        self.tx.send(AppEvent::QueueChanged);
    }

    fn chapter_in_vol(&self, vol: u32, chapter: u32) -> Option<&crate::model::Chapter> {
        self.active
            .as_ref()?
            .project
            .volumes
            .iter()
            .find(|v| v.number == vol)?
            .chapters
            .iter()
            .find(|c| c.number == chapter)
    }

    fn refresh_queue_panel(&mut self) {
        let Some(queue) = self.run_queue.clone() else {
            self.translate.set_queue(Vec::new());
            return;
        };
        let (running, pending) = queue.snapshot();
        let mut rows = Vec::with_capacity(pending.len() + 1);
        let mut pos = 1usize;
        if let Some((vol, ch)) = running {
            rows.push(self.queue_row(vol, ch, true, pos));
            pos += 1;
        }
        for (vol, ch) in pending {
            rows.push(self.queue_row(vol, ch, false, pos));
            pos += 1;
        }
        self.translate.set_queue(rows);
    }

    fn queue_row(&self, vol: u32, chapter: u32, running: bool, est_pos: usize) -> QueueRow {
        let ch = self.chapter_in_vol(vol, chapter);
        QueueRow {
            vol,
            number: chapter,
            title: ch.map(|c| c.title.clone()).unwrap_or_default(),
            kind: ch
                .map(|c| c.kind)
                .unwrap_or(crate::model::ChapterKind::Prose),
            status: ch.map(|c| c.status).unwrap_or(ChapterStatus::Pending),
            source_segments: ch.map(|c| c.source_segments).unwrap_or(0),
            running,
            est_pos,
        }
    }

    fn resync_run_checkpoint(&mut self) {
        let Some(queue) = self.run_queue.as_ref() else {
            return;
        };
        let (running, pending) = queue.snapshot();
        let chapters: Vec<u32> = running.into_iter().chain(pending).map(|(_, c)| c).collect();
        if let Some(cp) = self.active_run.as_mut() {
            cp.chapters = chapters;
            if let Err(e) = crate::workspace::session::save(cp) {
                self.push_log(
                    LogLevel::Warn,
                    format!("could not update recovery checkpoint: {e}"),
                );
            }
        }
    }

    fn request_translation(&mut self, chapters: Vec<u32>) {
        if self.run_active {
            self.toast = Some(Toast::warn(
                "a run is in progress — press i to add chapters to the queue",
            ));
            return;
        }
        if self.active.is_none() {
            self.toast = Some(Toast::warn("no project open"));
            return;
        }
        if chapters.is_empty() {
            self.toast = Some(Toast::warn("nothing selected"));
            return;
        }

        let resumable = self.chapters_with_translation_progress(&chapters);
        if resumable.is_empty() {
            self.begin_translation(chapters, false);
            return;
        }

        let list = chapter_list_preview(&resumable);
        let body = format!(
            "{} chapter(s) already have translated chunks or a failed/paused state ({list}). Continue skips committed chunks and resumes at the next gap. Restart deletes translated output for the requested chapter(s) and starts over.",
            resumable.len()
        );
        self.overlay = Overlay::confirm_with_alternate(
            "Continue previous translation?",
            body,
            "continue",
            Action::ContinueTranslation {
                chapters: chapters.clone(),
            },
            'r',
            "restart",
            Action::RestartTranslation { chapters },
        );
    }

    fn begin_translation(&mut self, chapters: Vec<u32>, restart: bool) {
        self.begin_translation_with_checkpoint(chapters, restart, None);
    }

    fn begin_translation_with_checkpoint(
        &mut self,
        chapters: Vec<u32>,
        restart: bool,
        checkpoint: Option<crate::workspace::session::SessionCheckpoint>,
    ) {
        if self.run_active {
            self.toast = Some(Toast::warn("a run is already in progress"));
            return;
        }
        let Some((vol, project_dir, project_id, project_title, models)) =
            self.active.as_ref().map(|active| {
                (
                    active.active_vol(),
                    active.project.dir.clone(),
                    active.project.id.clone(),
                    active.project.title.clone(),
                    active.models.clone(),
                )
            })
        else {
            self.toast = Some(Toast::warn("no project open"));
            return;
        };
        if chapters.is_empty() {
            self.toast = Some(Toast::warn("nothing selected"));
            return;
        }
        let Some(client) = self.ensure_active_client() else {
            // None means either no key (the common case) or a key that failed to
            // build a client — surface the real error in the latter case rather than
            // misleadingly telling the user to add a key they already have.
            self.toast = Some(if crate::config::resolve_api_key(&self.cfg).is_none() {
                Toast::warn("no API key — open Settings ( : → Settings ) to add one")
            } else {
                match crate::build_client(&self.cfg) {
                    Err(e) => Toast::error(format!("LLM client unavailable: {e}")),
                    Ok(_) => Toast::warn("could not start translation"),
                }
            });
            return;
        };

        let ws = Workspace::new(project_dir.clone(), vol);
        if restart {
            for chapter in &chapters {
                // Snapshot the existing translation for rerun comparison before the
                // restart deletes it.
                self.archive_for_rerun(&ws, *chapter);
                if let Err(e) = crate::workspace::translation::reset_chapter(&ws, *chapter) {
                    self.toast = Some(Toast::error(format!(
                        "restart failed for ch {chapter}: {e}"
                    )));
                    return;
                }
            }
            self.reset_in_memory_translation_progress(&chapters);
        }

        // Record a recovery checkpoint *before* the run starts: from here until the
        // pipeline finishes, a crash/power-loss can be resumed on next launch.
        let mut checkpoint = checkpoint.unwrap_or_else(|| {
            crate::workspace::session::SessionCheckpoint::new(
                project_dir.clone(),
                project_id,
                project_title,
                vol,
                chapters.clone(),
            )
        });
        checkpoint.ensure_run_id();
        if let Err(e) = crate::workspace::session::save(&checkpoint) {
            self.push_log(
                LogLevel::Warn,
                format!("could not write recovery checkpoint: {e}"),
            );
        }
        let history_version = if checkpoint.honya_version.trim().is_empty() {
            crate::update::current_version().to_string()
        } else {
            checkpoint.honya_version.clone()
        };
        let history = RunHistoryEntry::started(
            checkpoint.run_id.clone(),
            checkpoint.started_at,
            checkpoint.chapters.clone(),
            history_version,
        );
        if let Err(e) = crate::workspace::volume::record_run_started(&ws, history) {
            self.push_log(LogLevel::Warn, format!("could not write run history: {e}"));
        }

        let ctl = crate::agents::pipeline::RunControl::new();
        let queue = crate::agents::pipeline::ChapterQueue::new(
            checkpoint.chapters.iter().map(|&c| (vol, c)).collect(),
        );
        let ctx = crate::agents::pipeline::PipelineCtx {
            client,
            ws,
            models,
            cfg: self.cfg.clone(),
            tx: self.tx.clone(),
            ctl: ctl.clone(),
            queue: queue.clone(),
        };
        self.run_ctl = Some(ctl);
        self.run_queue = Some(queue);
        self.active_run = Some(checkpoint.clone());
        // Single-volume run: per-chapter events scope through active_run.vol.
        self.running_vol = None;
        self.refresh_queue_panel();
        let chapters_for_task = checkpoint.chapters.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::agents::run_pipeline(ctx, chapters_for_task).await {
                tx.send(AppEvent::Error {
                    context: "pipeline".to_string(),
                    msg: e.to_string(),
                });
            }
        });
        self.run_active = true;
        self.screen = Screen::Translate;
        self.toast = Some(Toast::info(format!(
            "{} {} chapter(s)",
            if restart { "restarting" } else { "translating" },
            chapters.len()
        )));
    }

    /// The auto project-translate queue: every volume's not-yet-done prose chapters
    /// (resume-aware), mirroring the per-chapter `translatable` rule the Project
    /// screen uses — Done / NeedsReview are skipped, Failed / Pending / interrupted
    /// are (re)queued. Volumes with nothing to do are dropped, so each `VolumePlan`
    /// has at least one chapter.
    fn project_translation_plan(&self) -> Vec<crate::agents::VolumePlan> {
        let Some(active) = self.active.as_ref() else {
            return Vec::new();
        };
        active
            .project
            .volumes
            .iter()
            .filter_map(|v| {
                let ws = Workspace::new(active.project.dir.clone(), v.number);
                let chapters: Vec<u32> = v
                    .chapters
                    .iter()
                    .filter(|c| {
                        if !matches!(c.kind, crate::model::ChapterKind::Prose) {
                            return false;
                        }
                        // A non-empty translated file can still be missing chunks.
                        !c.status.is_terminal()
                            || c.status == ChapterStatus::Failed
                            || !chapter_complete_on_disk(&ws, c.number, &self.cfg)
                    })
                    .map(|c| c.number)
                    .collect();
                (!chapters.is_empty()).then(|| crate::agents::VolumePlan {
                    vol: v.number,
                    label: v.label.clone(),
                    chapters,
                })
            })
            .collect()
    }

    /// Entry point for the one-click whole-project translate: validate, then raise
    /// a confirm summarizing the queue before [`Self::begin_project_translation`].
    fn request_project_translation(&mut self) {
        if self.run_active {
            self.toast = Some(Toast::warn("a run is already in progress"));
            return;
        }
        if self.active.is_none() {
            self.toast = Some(Toast::warn("no project open"));
            return;
        }
        let plan = self.project_translation_plan();
        if plan.is_empty() {
            self.toast = Some(Toast::info("project already fully translated"));
            return;
        }
        let vols = plan.len();
        let chapters: usize = plan.iter().map(|p| p.chapters.len()).sum();
        let body = format!(
            "Queue {chapters} not-yet-done chapter(s) across {vols} volume(s) and translate them one after another. Finished chapters are skipped (no tokens re-spent). If a chapter gets stuck in a loop it is re-translated whole, up to {} time(s); a chapter still looping past that aborts the run.",
            self.cfg.max_chapter_retranslates
        );
        self.overlay = Overlay::confirm(
            "Auto-translate the whole project?",
            body,
            Action::BeginProjectTranslation,
        );
    }

    /// Begin (or resume) the whole-project auto-translate: spawn
    /// [`crate::agents::run_project_pipeline`] over every volume's queue, under one
    /// recovery checkpoint flagged `whole_project`. The plan is recomputed from the
    /// freshly-scanned project each time, so it is inherently resume-aware.
    fn begin_project_translation(
        &mut self,
        checkpoint: Option<crate::workspace::session::SessionCheckpoint>,
    ) {
        if self.run_active {
            self.toast = Some(Toast::warn("a run is already in progress"));
            return;
        }
        let plan = self.project_translation_plan();
        if plan.is_empty() {
            self.toast = Some(Toast::info("project already fully translated"));
            return;
        }
        let Some((project_dir, project_id, project_title, models, history_vol)) =
            self.active.as_ref().map(|active| {
                // Key the run-history row to the project's LOWEST volume number,
                // which is stable across resumes (earlier volumes finishing never
                // removes them). Keying to the first *plan* volume would strand a
                // "Running" row when a resume's recomputed plan starts on a later
                // volume.
                let history_vol = active
                    .project
                    .volumes
                    .iter()
                    .map(|v| v.number)
                    .min()
                    .unwrap_or(active.vol);
                (
                    active.project.dir.clone(),
                    active.project.id.clone(),
                    active.project.title.clone(),
                    active.models.clone(),
                    history_vol,
                )
            })
        else {
            self.toast = Some(Toast::warn("no project open"));
            return;
        };
        let Some(client) = self.ensure_active_client() else {
            self.toast = Some(if crate::config::resolve_api_key(&self.cfg).is_none() {
                Toast::warn("no API key — open Settings ( : → Settings ) to add one")
            } else {
                match crate::build_client(&self.cfg) {
                    Err(e) => Toast::error(format!("LLM client unavailable: {e}")),
                    Ok(_) => Toast::warn("could not start translation"),
                }
            });
            return;
        };

        // Store project-run history on a stable volume even when the live volume moves.
        let ws = Workspace::new(project_dir.clone(), history_vol);
        let all_chapters: Vec<u32> = plan.iter().flat_map(|p| p.chapters.clone()).collect();

        let mut checkpoint = checkpoint.unwrap_or_else(|| {
            crate::workspace::session::SessionCheckpoint::new(
                project_dir.clone(),
                project_id,
                project_title,
                history_vol,
                all_chapters.clone(),
            )
        });
        checkpoint.whole_project = true;
        checkpoint.vol = history_vol;
        checkpoint.chapters = all_chapters;
        checkpoint.ensure_run_id();
        if let Err(e) = crate::workspace::session::save(&checkpoint) {
            self.push_log(
                LogLevel::Warn,
                format!("could not write recovery checkpoint: {e}"),
            );
        }
        let history_version = if checkpoint.honya_version.trim().is_empty() {
            crate::update::current_version().to_string()
        } else {
            checkpoint.honya_version.clone()
        };
        let history = RunHistoryEntry::started(
            checkpoint.run_id.clone(),
            checkpoint.started_at,
            checkpoint.chapters.clone(),
            history_version,
        );
        if let Err(e) = crate::workspace::volume::record_run_started(&ws, history) {
            self.push_log(LogLevel::Warn, format!("could not write run history: {e}"));
        }

        let ctl = crate::agents::pipeline::RunControl::new();
        let queue_seed: Vec<(u32, u32)> = plan
            .iter()
            .flat_map(|p| p.chapters.iter().map(|&c| (p.vol, c)))
            .collect();
        let queue = crate::agents::pipeline::ChapterQueue::new(queue_seed);
        let ctx = crate::agents::pipeline::PipelineCtx {
            client,
            ws,
            models,
            cfg: self.cfg.clone(),
            tx: self.tx.clone(),
            ctl: ctl.clone(),
            queue: queue.clone(),
        };
        self.run_ctl = Some(ctl);
        self.run_queue = Some(queue);
        self.active_run = Some(checkpoint);
        self.running_vol = None;
        self.refresh_queue_panel();
        let vols = plan.len();
        let total: usize = plan.iter().map(|p| p.chapters.len()).sum();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::agents::run_project_pipeline(ctx, plan).await {
                tx.send(AppEvent::Error {
                    context: "pipeline".to_string(),
                    msg: e.to_string(),
                });
            }
        });
        self.run_active = true;
        self.screen = Screen::Translate;
        self.toast = Some(Toast::info(format!(
            "auto-translating {total} chapter(s) across {vols} volume(s)"
        )));
    }

    fn chapters_with_translation_progress(&self, chapters: &[u32]) -> Vec<u32> {
        // Scope to the target volume: at request time there's no run yet, so this
        // resolves to the active (cursor-followed) volume, which is where the marked
        // chapters live. Without scoping, a same-numbered chapter in another volume
        // would mis-trigger the continue/restart prompt.
        let Some(vol) = self.event_vol() else {
            return Vec::new();
        };
        let Some(volume) = self
            .active
            .as_ref()
            .and_then(|a| a.project.volumes.iter().find(|v| v.number == vol))
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for chapter in chapters {
            if let Some(ch) = volume.chapters.iter().find(|ch| ch.number == *chapter)
                && (ch.committed_chunks > 0
                    || matches!(ch.status, ChapterStatus::Failed | ChapterStatus::Paused))
            {
                out.push(*chapter);
            }
        }
        out
    }

    fn reset_in_memory_translation_progress(&mut self, chapters: &[u32]) {
        if let Some(active) = self.active.as_mut() {
            for ch in active
                .project
                .volumes
                .iter_mut()
                .flat_map(|v| v.chapters.iter_mut())
            {
                if chapters.contains(&ch.number) {
                    ch.status = ChapterStatus::Pending;
                    ch.total_chunks = 0;
                    ch.committed_chunks = 0;
                    ch.last_run = None;
                }
            }
        }
    }

    fn start_import(
        &mut self,
        epub: PathBuf,
        title: String,
        vol: u32,
        synopsis_raw: String,
        synopsis_th: String,
    ) {
        if self.run_active {
            self.toast = Some(Toast::warn("a run is already in progress"));
            return;
        }
        let slug = slugify(&title);
        let dest = working_root().join(&slug);
        let models = self.cfg.models.clone();
        let tx = self.tx.clone();
        self.run_active = true;
        self.toast = Some(Toast::info(format!("importing {slug} …")));
        tokio::spawn(async move {
            match run_import(
                epub,
                dest,
                title,
                vol,
                models,
                synopsis_raw,
                synopsis_th,
                &tx,
            )
            .await
            {
                Ok(project_id) => tx.send(AppEvent::ImportFinished { project_id, vol }),
                Err(e) => tx.send(AppEvent::Error {
                    context: "import".to_string(),
                    msg: e.to_string(),
                }),
            }
        });
    }

    /// Gather the active project's `vol` and write it to the chosen deliverable
    /// formats on a background task. Mirrors `start_import`: clone the owned inputs
    /// out of `self.active`, spawn, and report back only via `AppEvent`s. Runs
    /// independently of `run_active` (export is read-only on project data).
    fn start_export(&mut self, vol: u32, formats: Vec<crate::export::ExportFormat>) {
        if formats.is_empty() {
            self.toast = Some(Toast::warn("no export formats selected"));
            return;
        }
        if self.export_active {
            self.toast = Some(Toast::warn("an export is already running"));
            return;
        }
        let Some(active) = self.active.as_ref() else {
            self.toast = Some(Toast::warn("open a project first"));
            return;
        };
        let project = &active.project;
        let Some(volume) = project.volumes.iter().find(|v| v.number == vol) else {
            self.toast = Some(Toast::warn(format!("Vol.{vol:02} not found")));
            return;
        };
        if volume.chapters.is_empty() {
            self.toast = Some(Toast::warn(format!("Vol.{vol:02} has no chapters")));
            return;
        }

        let root = project.dir.clone();
        let project_id = project.id.clone();
        let title = project.title.clone();
        let vol_label = volume.label.clone();
        let chapters = volume.chapters.clone();
        let tx = self.tx.clone();

        self.export_active = true;
        // Seed the gauge immediately so the overlay shows progress before the first
        // file lands (gather reads every chapter off disk first).
        self.overlay
            .set_export_progress(0, formats.len(), "gathering");
        self.toast = Some(Toast::info(format!("exporting Vol.{vol:02} …")));

        tokio::spawn(async move {
            let ws = Workspace::new(root, vol);
            let book =
                crate::export::gather(&ws, &title, &project_id, vol, vol_label, &chapters).await;
            match crate::export::export_volume(&ws, book, &formats, &tx).await {
                Ok((paths, warnings)) => tx.send(AppEvent::ExportFinished { paths, warnings }),
                Err(e) => tx.send(AppEvent::Error {
                    context: "export".to_string(),
                    msg: e.to_string(),
                }),
            }
        });
    }

    /// Spawn a background Translator round-trip for a volume synopsis. Uses the
    /// active project's client when one is open, else builds from config (the
    /// import wizard runs from the Shelf with no project open yet).
    fn translate_synopsis(&mut self, raw: String, attempt: u32) {
        // Prefer the active project's translator model; fall back to config (the
        // import wizard runs from the Shelf with no project open yet).
        let model = self
            .active
            .as_ref()
            .map(|a| a.models.translator.clone())
            .unwrap_or_else(|| self.cfg.models.translator.clone());
        let client = match self.ensure_active_client() {
            Some(c) => c,
            None => match crate::build_client(&self.cfg) {
                Ok(c) => c,
                Err(e) => {
                    // No client → report failure so the editor leaves Translating.
                    self.tx
                        .send(AppEvent::SynopsisFailed { msg: e.to_string() });
                    return;
                }
            },
        };
        let temperature = crate::agents::synopsis::reroll_temperature(attempt);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            match crate::agents::synopsis::translate_synopsis(
                client.as_ref(),
                &model,
                &raw,
                temperature,
            )
            .await
            {
                Ok((text, _usage)) => tx.send(AppEvent::SynopsisTranslated { text }),
                Err(e) => tx.send(AppEvent::SynopsisFailed { msg: e.to_string() }),
            }
        });
    }

    /// Persist edited Settings (base URL, model ids, and the config API key) and
    /// close. `api_key` is `None` when the environment supplies the key (config is
    /// left untouched), `Some("")` clears the saved key, `Some(k)` sets it. After a
    /// key change, the active project's client is rebuilt so translation works at
    /// once (or stops, if the key was cleared).
    // Args mirror the `Action::SaveSettings` payload one-to-one (config fields the
    // Settings overlay edits); bundling them into a struct would only add ceremony.
    #[allow(clippy::too_many_arguments)]
    fn save_settings(
        &mut self,
        base_url: String,
        orchestrator: String,
        translator: String,
        reviewer: String,
        api_key: Option<String>,
        update_mode: crate::model::UpdateMode,
        service_tier: Option<crate::model::ServiceTier>,
        max_attempts: u32,
        loop_stall_secs: u64,
        max_chapter_retranslates: u32,
    ) {
        self.cfg.base_url = base_url;
        self.cfg.models.orchestrator = orchestrator;
        self.cfg.models.translator = translator;
        self.cfg.models.reviewer = reviewer;
        self.cfg.update_mode = update_mode;
        let tier_changed = self.cfg.service_tier != service_tier;
        self.cfg.service_tier = service_tier;
        self.cfg.max_attempts = max_attempts;
        self.cfg.loop_stall_secs = loop_stall_secs;
        self.cfg.max_chapter_retranslates = max_chapter_retranslates;
        let key_changed = if let Some(k) = api_key {
            let k = k.trim();
            let next = (!k.is_empty()).then(|| k.to_string());
            let changed = next != self.cfg.api_key;
            self.cfg.api_key = next;
            changed
        } else {
            false
        };
        // Rebuild the active client so a newly-added/changed/cleared key — or a new
        // service tier (snapshotted into ClientConfig) — takes hold without reopening.
        if (key_changed || tier_changed) && let Some(active) = self.active.as_mut() {
            active.client = crate::build_client(&self.cfg).ok();
        }
        match crate::config::save(&self.cfg) {
            Ok(()) => self.toast = Some(Toast::info("settings saved")),
            Err(e) => self.toast = Some(Toast::error(format!("save failed: {e}"))),
        }
        self.overlay = Overlay::None;
    }

    /// Create the bundled sample project (if absent) and open it — the offline,
    /// no-key path for learning the five-screen workflow. Marks onboarding complete.
    fn create_sample_project(&mut self) {
        match crate::workspace::sample::create_sample_project(&working_root(), &self.cfg.models) {
            Ok(id) => {
                self.mark_onboarded();
                self.refresh_projects();
                self.shelf.rescan(&working_root());
                self.push_log(LogLevel::Info, format!("created sample project {id}"));
                // Close the Welcome overlay first: `open_project` lands on the Project
                // screen, but it does not clear overlays, so without this the modal
                // would stay rendered over (and keep swallowing input for) the project
                // the user just opened.
                self.overlay = Overlay::None;
                self.open_project(id);
            }
            Err(e) => {
                self.toast = Some(Toast::error(format!("could not create sample: {e}")));
            }
        }
    }

    /// Record that the user has completed (or dismissed) onboarding and persist it,
    /// so the Welcome overlay does not auto-open on subsequent launches.
    fn mark_onboarded(&mut self) {
        if !self.cfg.onboarded {
            self.cfg.onboarded = true;
            let _ = crate::config::save(&self.cfg);
        }
    }

    /// On first launch (no projects yet and not previously onboarded), raise the
    /// in-app Welcome overlay. Called from `main` after `init_recovery_prompt`, so a
    /// pending crash-recovery prompt keeps priority. Existing users (who already
    /// have projects) are quietly marked onboarded and never see it.
    pub fn init_onboarding(&mut self) {
        if self.cfg.onboarded {
            return;
        }
        if !self.projects.is_empty() {
            // A returning user from before this flag existed — don't nag them.
            self.mark_onboarded();
            return;
        }
        // Recovery prompt (or any overlay already up) wins this launch.
        if !matches!(self.overlay, Overlay::None) {
            return;
        }
        let key_present = crate::config::resolve_api_key(&self.cfg).is_some();
        let sample_exists = crate::workspace::sample::sample_exists(&working_root());
        // Shown once automatically; mark it so we don't nag on every launch. It stays
        // reachable from the palette / Help for anyone who wants it again.
        self.mark_onboarded();
        self.overlay = Overlay::welcome(key_present, sample_exists);
    }

    /// Persist the active volume's synopsis (from the standalone editor) and close.
    fn save_synopsis(&mut self, raw: String, th: String) {
        if let Some(active) = self.active.as_ref() {
            match crate::workspace::volume::set_synopsis(&active.workspace, &raw, &th) {
                Ok(()) => self.toast = Some(Toast::info("synopsis saved")),
                Err(e) => self.toast = Some(Toast::error(format!("save failed: {e}"))),
            }
        }
        self.overlay = Overlay::None;
    }

    /// Persist a Reader proofreading annotation and refresh the inline note cache.
    fn save_reader_note(&mut self, chapter: u32, line: u32, note: String) {
        self.overlay = Overlay::None;
        let note = note.trim();
        if note.is_empty() {
            self.toast = Some(Toast::info("empty note ignored"));
            return;
        }
        let Some(active) = self.active.as_ref() else {
            self.toast = Some(Toast::warn("no project open"));
            return;
        };
        let workspace = active.workspace.clone();
        let line = line.max(1);
        let annotation = ReaderAnnotation {
            chapter,
            line,
            note: note.to_string(),
            created_at: Some(chrono::Utc::now()),
        };
        match crate::workspace::volume::add_reader_annotation(&workspace, annotation) {
            Ok(()) => {
                self.reader.reload_annotations(&workspace);
                self.toast = Some(Toast::info(format!("note saved · ch {chapter:03} L{line}")));
            }
            Err(e) => self.toast = Some(Toast::error(format!("note save failed: {e}"))),
        }
    }

    fn refresh_projects(&mut self) {
        self.projects = crate::workspace::scan::scan_projects(&working_root());
        self.shelf.rescan(&working_root());
    }

    /// Gather the active volume's QA report and wrap it in the QA overlay. With no
    /// project open (palette path) the overlay still shows, with an empty report and
    /// a "no project" header, so the user gets feedback rather than a silent no-op.
    fn build_qa_overlay(&self) -> Overlay {
        match self.active.as_ref() {
            Some(active) => {
                let report = qa::collect(active);
                let label = active
                    .project
                    .volumes
                    .iter()
                    .find(|v| v.number == active.vol)
                    .and_then(|v| v.label.as_deref());
                let title = match label {
                    Some(label) => {
                        format!("{} · Vol.{:02} {label}", active.project.title, active.vol)
                    }
                    None => format!("{} · Vol.{:02}", active.project.title, active.vol),
                };
                Overlay::qa(title, report)
            }
            None => Overlay::qa("(no project open)".to_string(), qa::QaReport::default()),
        }
    }

    /// Toggle a navigation bookmark at a Reader line, persist it, and refresh the
    /// Reader's badge. Mirrors `save_reader_note`'s validation/feedback shape.
    fn toggle_reader_bookmark(&mut self, chapter: u32, line: u32, label: String) {
        let Some(active) = self.active.as_ref() else {
            self.toast = Some(Toast::warn("no project open"));
            return;
        };
        let workspace = active.workspace.clone();
        let line = line.max(1);
        match crate::workspace::volume::toggle_reader_bookmark(&workspace, chapter, line, &label) {
            Ok(added) => {
                self.reader.reload_bookmarks(&workspace);
                self.toast = Some(Toast::info(if added {
                    format!("bookmarked · ch {chapter:03} L{line}")
                } else {
                    format!("bookmark removed · ch {chapter:03} L{line}")
                }));
            }
            Err(e) => self.toast = Some(Toast::error(format!("bookmark failed: {e}"))),
        }
    }

    /// Build the Reader jump/outline picker: every chapter in the active volume, the
    /// current chapter's section headings nested beneath it, and the volume's
    /// bookmarks. With no project open the picker still shows (empty) for feedback.
    fn build_jump_overlay(&self) -> Overlay {
        let Some(active) = self.active.as_ref() else {
            return Overlay::reader_jump("(no project open)".to_string(), Vec::new());
        };
        let title = format!("{} · Vol.{:02}", active.project.title, active.vol);
        let current = self.reader.current_chapter();
        let mut items: Vec<JumpTarget> = Vec::new();

        if let Some(volume) = active
            .project
            .volumes
            .iter()
            .find(|v| v.number == active.vol)
        {
            for ch in &volume.chapters {
                items.push(JumpTarget {
                    chapter: ch.number,
                    line: 1,
                    label: format!(
                        "ch {:03}  {}  [{}]",
                        ch.number,
                        ch.title,
                        status_tag(ch.status)
                    ),
                    kind: JumpKind::Chapter,
                });
                if ch.number == current {
                    for (line, level, text) in self.reader.outline() {
                        let indent = "  ".repeat(level.min(4).saturating_sub(1) as usize);
                        items.push(JumpTarget {
                            chapter: current,
                            line,
                            label: format!("    {indent}{text}"),
                            kind: JumpKind::Section,
                        });
                    }
                }
            }
        }

        for b in crate::workspace::volume::reader_bookmarks(&active.workspace) {
            let label = if b.label.trim().is_empty() {
                format!("ch {:03} L{}", b.chapter, b.line)
            } else {
                format!("ch {:03} L{} · {}", b.chapter, b.line, b.label)
            };
            items.push(JumpTarget {
                chapter: b.chapter,
                line: b.line,
                label,
                kind: JumpKind::Bookmark,
            });
        }

        Overlay::reader_jump(title, items)
    }

    pub fn render(&mut self, f: &mut Frame) {
        let area = f.area();
        let show_toast = self.toast.is_some();
        let sk: Skeleton = layout::skeleton(area, show_toast);
        // Stash this frame's geometry so the next mouse event can hit-test it.
        self.last_area = area;
        self.last_skeleton = Some(sk);

        f.render_widget(
            Paragraph::new("").style(Style::default().bg(self.theme.bg)),
            area,
        );

        let crumb = self.crumb();
        let tally = self.tally();
        chrome::render_header(f, sk.header, &crumb, &tally, &self.theme);

        self.tab_zones = chrome::render_tabbar(
            f,
            sk.tabs,
            self.screen,
            self.run_active,
            self.translate.active_agent_role(),
            self.frame,
            &self.theme,
        );

        self.render_rule(f, sk.rule);

        self.render_body(f, sk.body);

        if show_toast {
            self.render_toast(f, sk.toast);
        }

        let hints = self.hints();
        chrome::render_footer(
            f,
            sk.footer,
            hints,
            self.update_available.as_deref(),
            self.update_installed.as_deref(),
            &self.theme,
        );

        // Overlay last, over a Clear, so it always wins.
        if !matches!(self.overlay, Overlay::None) {
            self.overlay
                .render(f, area, &self.theme, &self.cfg, &self.log);
        }
    }

    fn render_body(&mut self, f: &mut Frame, body: Rect) {
        match self.screen {
            Screen::Shelf => self.shelf.render(f, body, &self.projects, &self.theme),
            Screen::Project => self
                .project
                .render(f, body, self.active.as_ref(), &self.theme),
            Screen::Translate => self.translate.render(f, body, self.frame, &self.theme),
            Screen::Reader => self.reader.render(f, body, &self.theme),
            Screen::Lexicon => self.lexicon.render(
                f,
                body,
                self.active.as_ref().map(|a| &a.workspace),
                &self.theme,
            ),
        }
    }

    fn render_rule(&self, f: &mut Frame, area: Rect) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let rule = "─".repeat(area.width as usize);
        f.render_widget(
            Paragraph::new(rule).style(Style::default().fg(self.theme.rule).bg(self.theme.bg)),
            area,
        );
    }

    fn render_toast(&self, f: &mut Frame, area: Rect) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let Some(toast) = self.toast.as_ref() else {
            return;
        };
        let (glyph, color) = match toast.level {
            LogLevel::Trace => ("·", self.theme.ink_faint),
            LogLevel::Info => ("✓", self.theme.status_done),
            LogLevel::Warn => ("!", self.theme.status_warn),
            LogLevel::Error => ("✗", self.theme.status_failed),
        };
        let body = truncate_cols(
            &thai_display_safe(&toast.msg),
            area.width.saturating_sub(14) as usize,
        );
        let left = Line::from(vec![
            Span::raw(" "),
            Span::styled(glyph, Style::default().fg(color)),
            Span::raw(" "),
            Span::styled(body, Style::default().fg(self.theme.ink_soft)),
        ]);
        f.render_widget(
            Paragraph::new(left).style(Style::default().bg(self.theme.bg)),
            area,
        );
        let hint = "⌫ dismiss ";
        let hint_w = crate::ui::text::col_width(hint) as u16;
        if area.width > hint_w {
            let hint_area = Rect {
                x: area.x + area.width - hint_w,
                y: area.y,
                width: hint_w,
                height: 1,
            };
            f.render_widget(
                Paragraph::new(Span::styled(
                    hint,
                    Style::default().fg(self.theme.ink_faint),
                ))
                .style(Style::default().bg(self.theme.bg)),
                hint_area,
            );
        }
    }

    fn crumb(&self) -> String {
        match (&self.active, self.screen) {
            (Some(active), Screen::Shelf) => format!("honya 本屋   {}", active.project.title),
            (Some(active), _) => {
                let vol = active.active_vol();
                if let Some(label) = active
                    .project
                    .volumes
                    .iter()
                    .find(|v| v.number == vol)
                    .and_then(|v| v.label.as_deref())
                {
                    format!("honya 本屋   {} · Vol.{vol} {label}", active.project.title)
                } else {
                    format!("honya 本屋   {} · Vol.{vol}", active.project.title)
                }
            }
            (None, _) => "honya 本屋".to_string(),
        }
    }

    fn tally(&self) -> StatusTally {
        let mut t = StatusTally {
            done: 0,
            working: 0,
            pending: 0,
            failed: 0,
        };
        if let Some(active) = self.active.as_ref() {
            for vol in &active.project.volumes {
                for ch in &vol.chapters {
                    match ch.status {
                        ChapterStatus::Done
                        | ChapterStatus::Appended
                        | ChapterStatus::NeedsReview => t.done += 1,
                        ChapterStatus::Failed => t.failed += 1,
                        s if s.is_active() => t.working += 1,
                        ChapterStatus::Paused => t.working += 1,
                        _ => t.pending += 1,
                    }
                }
            }
        } else {
            // On the shelf with no project open: aggregate across the whole shelf.
            for p in &self.projects {
                for vol in &p.volumes {
                    for ch in &vol.chapters {
                        match ch.status {
                            ChapterStatus::Done
                            | ChapterStatus::Appended
                            | ChapterStatus::NeedsReview => t.done += 1,
                            ChapterStatus::Failed => t.failed += 1,
                            s if s.is_active() => t.working += 1,
                            _ => t.pending += 1,
                        }
                    }
                }
            }
        }
        t
    }

    fn hints(&self) -> &'static [(&'static str, &'static str)] {
        if !matches!(self.overlay, Overlay::None) {
            return self.overlay.hints();
        }
        match self.screen {
            Screen::Shelf => self.shelf.hints(),
            Screen::Project => self.project.hints(),
            Screen::Translate => self.translate.hints(),
            Screen::Reader => self.reader.hints(),
            Screen::Lexicon => self.lexicon.hints(),
        }
    }
}

/// (done, total) prose chapters across every volume — the recovery progress for a
/// whole-project run, whose queue spans all volumes. Completeness is re-derived
/// from disk (every chunk committed), NOT from the scanned status: a chapter
/// interrupted mid-way scans as `Done`/`NeedsReview` (any non-empty translated
/// file) yet is missing chunks, and must NOT count as done or the recovery prompt
/// would silently clear a run with real work left.
fn project_prose_progress(project: &Project, cfg: &AppConfig) -> (usize, usize) {
    let mut done = 0;
    let mut total = 0;
    for v in &project.volumes {
        let ws = Workspace::new(project.dir.clone(), v.number);
        for c in &v.chapters {
            if c.kind != crate::model::ChapterKind::Prose {
                continue;
            }
            total += 1;
            if chapter_complete_on_disk(&ws, c.number, cfg) {
                done += 1;
            }
        }
    }
    (done, total)
}

/// Whether a chapter's translated output is fully present on disk: every chunk the
/// raw produces has a committed marker (clean or flagged-needs-review both count as
/// "written"). This is the authoritative completeness signal — the scanned
/// `ChapterStatus` cannot distinguish a finished chapter from one interrupted with
/// only some chunks committed (the translated file is non-empty either way), so any
/// "skip if done" decision for the whole-project run must consult this instead.
/// Mirrors `pipeline::process_chapter`'s own resume/skip accounting.
fn chapter_complete_on_disk(ws: &Workspace, chapter: u32, cfg: &AppConfig) -> bool {
    let raw = std::fs::read_to_string(ws.raw(chapter)).unwrap_or_default();
    if raw.trim().is_empty() {
        // No source to translate → nothing outstanding.
        return true;
    }
    let translated = std::fs::read_to_string(ws.translated(chapter)).unwrap_or_default();
    // Image-only / no-prose chapters are written straight to translated/ at import;
    // their completeness is just "the file exists".
    if crate::cleanse::is_image_only(&raw) {
        return !translated.trim().is_empty();
    }
    let chunks = crate::agents::chunk::chunk_chapter(
        &raw,
        cfg.chunk_target_tokens,
        cfg.chunk_hard_cap_tokens,
    );
    if chunks.is_empty() {
        return !translated.trim().is_empty();
    }
    let committed = crate::workspace::translation::committed_chunk_indices_in(&translated);
    chunks.iter().all(|c| committed.contains(&(c.index as u32)))
}

fn done_recovery_chapters(
    project: &Project,
    cp: &crate::workspace::session::SessionCheckpoint,
) -> usize {
    project
        .volumes
        .iter()
        .find(|v| v.number == cp.vol)
        .map(|v| {
            cp.chapters
                .iter()
                .filter(|&&num| {
                    v.chapters.iter().any(|ch| {
                        ch.number == num
                            && matches!(ch.status, ChapterStatus::Done | ChapterStatus::NeedsReview)
                    })
                })
                .count()
        })
        .unwrap_or(0)
}

fn need_review_recovery_chapters(
    project: &Project,
    cp: &crate::workspace::session::SessionCheckpoint,
) -> usize {
    project
        .volumes
        .iter()
        .find(|v| v.number == cp.vol)
        .map(|v| {
            cp.chapters
                .iter()
                .filter(|&&num| {
                    v.chapters.iter().any(|ch| {
                        ch.number == num && matches!(ch.status, ChapterStatus::NeedsReview)
                    })
                })
                .count()
        })
        .unwrap_or(0)
}

/// One-paragraph body for the startup recovery prompt. Flowing prose (no hard
/// newlines) so the modal wraps it cleanly.
fn recovery_body(
    cp: &crate::workspace::session::SessionCheckpoint,
    done: usize,
    total: usize,
) -> String {
    let progress = if done > 0 {
        format!("{done}/{total} chapter(s) already finished")
    } else {
        format!("{total} chapter(s) queued")
    };
    let scope = if cp.whole_project {
        "whole project".to_string()
    } else {
        format!("Vol.{:02}", cp.vol)
    };
    format!(
        "honya didn't shut down cleanly during a translation run — «{}» {scope} · {progress}. Resume picks up from the last committed chunk (finished chunks are skipped, so no tokens are re-spent). Discard forgets this run; Esc keeps it for next launch.",
        cp.project_title
    )
}

fn run_history_status(
    chapters_done: u32,
    chapters_failed: u32,
    chapters_need_review: u32,
    stopped: bool,
) -> RunHistoryStatus {
    if stopped {
        RunHistoryStatus::Stopped
    } else if chapters_failed > 0 && chapters_done == 0 {
        RunHistoryStatus::Failed
    } else if chapters_failed > 0 {
        RunHistoryStatus::Partial
    } else if chapters_need_review > 0 {
        RunHistoryStatus::NeedsReview
    } else {
        RunHistoryStatus::Completed
    }
}

/// Load the glossary as a `jp_term → Thai` map (keys trimmed), for diffing what a
/// run added / changed.
fn glossary_map(ws: &Workspace) -> HashMap<String, String> {
    crate::workspace::glossary::load(ws)
        .into_iter()
        .map(|t| (t.jp_term.trim().to_string(), t.thai_term))
        .collect()
}

/// Diff two glossary snapshots by jp_term: `(added, changed)` jp_terms (sorted,
/// capped). "Added" = a term absent before; "changed" = a term whose Thai differs.
fn glossary_delta(
    before: &HashMap<String, String>,
    after: &HashMap<String, String>,
) -> (Vec<String>, Vec<String>) {
    const CAP: usize = 100;
    let mut added = Vec::new();
    let mut changed = Vec::new();
    for (jp, thai) in after {
        match before.get(jp) {
            None => added.push(jp.clone()),
            Some(prev) if prev != thai => changed.push(jp.clone()),
            _ => {}
        }
    }
    added.sort();
    changed.sort();
    added.truncate(CAP);
    changed.truncate(CAP);
    (added, changed)
}

fn chapter_list_preview(chapters: &[u32]) -> String {
    let mut chapters = chapters.to_vec();
    chapters.sort_unstable();
    chapters.dedup();
    let shown: Vec<String> = chapters
        .iter()
        .take(5)
        .map(|chapter| format!("ch {chapter:03}"))
        .collect();
    if chapters.len() > shown.len() {
        format!(
            "{} +{} more",
            shown.join(", "),
            chapters.len() - shown.len()
        )
    } else {
        shown.join(", ")
    }
}

/// The working root we scan for projects / unimported epubs. Falls back to `.`.
fn working_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Recursively delete a project directory, refusing anything that is not actually a
/// honya project (must contain `PROJECT.md`). This guard keeps a stale/mis-set path
/// from turning a delete into an arbitrary `rm -rf`.
pub(crate) fn remove_project_dir(dir: &std::path::Path) -> std::io::Result<()> {
    if !dir.join("PROJECT.md").is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a honya project directory",
        ));
    }
    std::fs::remove_dir_all(dir)
}

/// Compact chapter-status tag for the Reader jump picker labels.
fn status_tag(status: ChapterStatus) -> &'static str {
    match status {
        ChapterStatus::Pending => "·",
        ChapterStatus::Chunking | ChapterStatus::Translating | ChapterStatus::Reviewing => "…",
        ChapterStatus::Appended | ChapterStatus::Done => "done",
        ChapterStatus::NeedsReview => "review",
        ChapterStatus::Failed => "failed",
        ChapterStatus::Paused => "paused",
    }
}

/// Filesystem-safe slug: ASCII lowered, punctuation collapsed, CJK/Thai preserved.
pub fn slugify(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut prev_dash = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if ch.is_ascii() {
            if !prev_dash && !out.is_empty() {
                out.push('-');
                prev_dash = true;
            }
        } else {
            out.push(ch);
            prev_dash = false;
        }
    }
    // Windows path components cannot end with '.' or ' '.
    let trimmed = out
        .trim_matches('-')
        .trim_end_matches(['.', ' '])
        .to_string();
    if trimmed.is_empty() {
        return "project".to_string();
    }
    // Avoid Windows device basenames, case-insensitive and before any extension.
    const RESERVED: &[&str] = &[
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    let stem = trimmed
        .split('.')
        .next()
        .unwrap_or(&trimmed)
        .to_ascii_lowercase();
    if RESERVED.contains(&stem.as_str()) {
        format!("_{trimmed}")
    } else {
        trimmed
    }
}

/// Import driver: scaffold the tree, extract+relocate media, cleanse each spine doc to
/// markdown, write raw/ (+ translated/ for image-only), emit ImportProgress. Returns the slug.
#[allow(clippy::too_many_arguments)]
async fn run_import(
    epub: PathBuf,
    dest: PathBuf,
    title: String,
    vol: u32,
    models: ModelSet,
    synopsis_raw: String,
    synopsis_th: String,
    tx: &EventTx,
) -> anyhow::Result<String> {
    use crate::epub::import::import_with_media;
    use crate::epub::paths::{dir_of, resolve_href};

    let slug = dest
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| slugify(&title));

    // Scaffold on a blocking thread so the fs work never stalls the runtime.
    {
        let dest = dest.clone();
        let title = title.clone();
        let models = models.clone();
        tokio::task::spawn_blocking(move || {
            crate::workspace::scaffold::create_project(&dest, &title, &models, vol)?;
            // Persist the volume synopsis (if any) onto the freshly-scaffolded volume.
            if !synopsis_raw.trim().is_empty() || !synopsis_th.trim().is_empty() {
                let ws = Workspace::new(dest.clone(), vol);
                crate::workspace::volume::set_synopsis(&ws, &synopsis_raw, &synopsis_th)?;
            }
            Ok::<(), std::io::Error>(())
        })
        .await??;
    }

    tx.send(AppEvent::ImportProgress {
        done: 0,
        total: 0,
        label: "extracting epub".to_string(),
    });

    // work_dir lives under the project so the archive stays reprocessable.
    let work_dir = dest.join(".epub_work");
    let images_dir = dest.join("images");
    let (book, media) = {
        let epub = epub.clone();
        let work_dir = work_dir.clone();
        let images_dir = images_dir.clone();
        tokio::task::spawn_blocking(move || {
            import_with_media(&epub, &work_dir, &images_dir, "images")
        })
        .await?
        .map_err(|e| anyhow::anyhow!("epub: {e}"))?
    };

    let doc_paths: Vec<String> = book
        .reading_order_paths()
        .into_iter()
        .map(|s| s.to_string())
        .collect();
    let total = doc_paths.len();
    let ws = Workspace::new(dest.clone(), vol);

    // TOC title per content doc (first entry wins); prepended as a `# ` heading on prose
    // so real chapter names survive instead of the generic "Chapter NNN".
    let mut toc_titles: HashMap<String, String> = HashMap::new();
    for t in &book.toc {
        let title = t.title.trim();
        if !title.is_empty() {
            toc_titles
                .entry(t.content_path.clone())
                .or_insert_with(|| title.to_string());
        }
    }

    // Cleanse every spine doc, segment into logical chapters, then write them.
    let mut docs: Vec<crate::epub::segment::DocInput> = Vec::with_capacity(total);
    for (i, archive_path) in doc_paths.iter().enumerate() {
        let disk = book.disk_path(archive_path);
        let html = match tokio::fs::read_to_string(&disk).await {
            Ok(h) => h,
            Err(_) => {
                // Non-text content docs (rare) are skipped rather than fatal.
                tx.send(AppEvent::ImportProgress {
                    done: i + 1,
                    total,
                    label: format!("skip {}", base_name(archive_path)),
                });
                continue;
            }
        };

        // Per-doc image map: raw <img src> -> relocated basename (empty map still
        // yields correct links via cleanse's raw-basename fallback).
        let base_dir = dir_of(archive_path);
        let image_map: HashMap<String, String> = collect_img_srcs(&html)
            .into_iter()
            .map(|src| {
                // Resolve archive-relative, look up the dedup-safe basename; fall back to raw.
                let resolved = resolve_href(&base_dir, &src);
                let file = media
                    .by_resolved_path
                    .get(&resolved)
                    .cloned()
                    .unwrap_or_else(|| base_name(&resolved));
                (src, file)
            })
            .collect();

        let md = crate::cleanse::xhtml_to_markdown(&html, &image_map);
        docs.push(crate::epub::segment::DocInput {
            archive_path: archive_path.clone(),
            markdown: md,
            toc_title: toc_titles.get(archive_path).cloned(),
            internal_link_count: count_internal_xhtml_links(&html),
            body_class: body_class(&html),
        });

        tx.send(AppEvent::ImportProgress {
            done: i + 1,
            total,
            label: format!("cleansing {}/{}", i + 1, total),
        });
        // Yield so the UI ticks the gauge between docs.
        tokio::task::yield_now().await;
    }

    let chapters = crate::epub::segment::segment(&docs);

    let mut ch_number: u32 = 0;
    for lc in &chapters {
        ch_number += 1;
        match lc.kind {
            crate::epub::segment::LogicalKind::ImageOnly => {
                // Seed raw/ for discovery AND translated/ so it reads as Done.
                let _ = crate::workspace::translation::write_raw(&ws, ch_number, &lc.body);
                let _ = crate::workspace::translation::write_image_only(&ws, ch_number, &lc.body);
            }
            crate::epub::segment::LogicalKind::Prose => {
                // Title heading goes ABOVE the leading m### image so scan.rs's
                // first_md_heading recovers it.
                let titled = match &lc.title {
                    Some(t) => format!("# {t}\n\n{}", lc.body),
                    None => lc.body.clone(),
                };
                let _ = crate::workspace::translation::write_raw(&ws, ch_number, &titled);
            }
        }
    }

    Ok(slug)
}

/// Count `<a href="…​.xhtml…">` links in raw HTML — the in-spine TOC/nav signal.
fn count_internal_xhtml_links(html: &str) -> usize {
    let lower = html.to_ascii_lowercase();
    let mut count = 0;
    let mut rest = lower.as_str();
    while let Some(p) = rest.find("<a") {
        let after = &rest[p + 2..];
        // Confirm a real <a tag start (next char whitespace or '>').
        let is_tag = after
            .chars()
            .next()
            .map(|c| c.is_whitespace() || c == '>')
            .unwrap_or(false);
        let end = after.find('>').unwrap_or(after.len());
        let tag = &after[..end];
        if is_tag && tag.contains("href") && tag.contains(".xhtml") {
            count += 1;
        }
        rest = &after[end..];
    }
    count
}

/// First `<body class="…">` value, if any (segmenter's nav/TOC + page-kind hint).
fn body_class(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let pos = lower.find("<body")?;
    let rest = &html[pos..];
    let end = rest.find('>').map(|e| e + 1).unwrap_or(rest.len());
    extract_attr(&rest[..end], "class")
}

/// Extract the trailing path component of a '/'-separated archive path.
fn base_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// Cheap scan for raw `<img>` / SVG `<image>` source strings.
fn collect_img_srcs(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = html.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Check `<image` first so SVG tags do not get caught as `<img`.
        let rest = &bytes[i..];
        let (matched, prefer_src) = if rest.starts_with(b"<image") {
            (true, false)
        } else if rest.starts_with(b"<img") {
            (true, true)
        } else {
            (false, false)
        };
        if matched {
            let tag_end = html[i..].find('>').map(|e| i + e).unwrap_or(bytes.len());
            let tag = &html[i..tag_end];
            let src = if prefer_src {
                extract_attr(tag, "src")
                    .or_else(|| extract_attr(tag, "xlink:href"))
                    .or_else(|| extract_attr(tag, "href"))
            } else {
                extract_attr(tag, "xlink:href")
                    .or_else(|| extract_attr(tag, "href"))
                    .or_else(|| extract_attr(tag, "src"))
            };
            if let Some(src) = src {
                out.push(src);
            }
            i = tag_end + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// Pull `attr="value"` (or `attr='value'`) out of a tag fragment.
fn extract_attr(tag: &str, attr: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let key = format!("{attr}=");
    let pos = lower.find(&key)?;
    let after = &tag[pos + key.len()..];
    let bytes = after.as_bytes();
    let first = *bytes.first()?;
    if first == b'"' || first == b'\'' {
        let quote = first as char;
        let rest = &after[1..];
        let end = rest.find(quote)?;
        Some(rest[..end].to_string())
    } else {
        // Unquoted: read until whitespace or tag end.
        let end = after
            .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .unwrap_or(after.len());
        Some(after[..end].to_string())
    }
}

#[cfg(test)]
mod mouse_tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

    use crate::app::overlay::Overlay;

    fn app() -> App {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(EventTx(tx), AppConfig::default())
    }

    fn render(app: &mut App, w: u16, h: u16) {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
    }

    fn ev(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::empty(),
        }
    }

    fn click(app: &mut App, col: u16, row: u16) {
        app.on_mouse(ev(MouseEventKind::Down(MouseButton::Left), col, row));
    }

    /// Clicking a tab in the bar switches to that screen; the zones the bar
    /// reports really land on their labels.
    #[test]
    fn clicking_tabs_switches_screens() {
        let mut app = app();
        for target in [
            Screen::Lexicon,
            Screen::Project,
            Screen::Reader,
            Screen::Shelf,
        ] {
            render(&mut app, 120, 40);
            let (rect, _) = app
                .tab_zones
                .iter()
                .copied()
                .find(|(_, s)| *s == target)
                .unwrap_or_else(|| panic!("no zone for {target:?}"));
            click(&mut app, rect.x + rect.width / 2, rect.y);
            assert_eq!(app.screen, target);
        }
    }

    /// Clicking the breadcrumb / header goes home to the Shelf.
    #[test]
    fn header_click_goes_home() {
        let mut app = app();
        app.screen = Screen::Reader;
        render(&mut app, 80, 24);
        click(&mut app, 2, 0); // header is row 0
        assert_eq!(app.screen, Screen::Shelf);
    }

    /// A click off an open overlay dismisses it (the overlay's Esc).
    #[test]
    fn click_outside_overlay_dismisses() {
        let mut app = app();
        app.overlay = Overlay::Help(0);
        render(&mut app, 120, 40);
        click(&mut app, 0, 0); // top-left corner is outside the centered modal
        assert!(matches!(app.overlay, Overlay::None));
    }

    /// Clicking a confirm dialog's confirm button runs its wrapped action.
    #[test]
    fn clicking_confirm_button_runs_action() {
        let mut app = app();
        app.overlay = Overlay::confirm("Title", "Body", Action::Goto(Screen::Lexicon));
        render(&mut app, 80, 24);
        // Resolve the modal the same way the render/hit-test path does, then click
        // the start of the confirm label on the button (last interior) row.
        let modal = crate::ui::layout::centered_modal(64, 9, app.last_area);
        let button_row = modal.y + modal.height - 2; // inner bottom line
        click(&mut app, modal.x + 4, button_row);
        assert!(matches!(app.overlay, Overlay::None));
        assert_eq!(app.screen, Screen::Lexicon);
    }
}

#[cfg(test)]
mod img_src_tests {
    use super::collect_img_srcs;

    #[test]
    fn collects_html_img_and_svg_image() {
        let html = r#"<p><img src="a.png"/></p>
            <svg xmlns:xlink="http://www.w3.org/1999/xlink"><image xlink:href="cover.png"/></svg>
            <svg><image href="bare.png"/></svg>"#;
        let srcs = collect_img_srcs(html);
        assert!(srcs.contains(&"a.png".to_string()), "html img: {srcs:?}");
        assert!(
            srcs.contains(&"cover.png".to_string()),
            "svg xlink:href: {srcs:?}"
        );
        assert!(
            srcs.contains(&"bare.png".to_string()),
            "svg bare href: {srcs:?}"
        );
    }
}

#[cfg(test)]
mod completeness_tests {
    use super::chapter_complete_on_disk;
    use crate::model::AppConfig;
    use crate::workspace::{Workspace, translation};

    fn temp_ws(tag: &str) -> (std::path::PathBuf, Workspace) {
        let base =
            std::env::temp_dir().join(format!("honya_complete_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let ws = Workspace::new(base.clone(), 1);
        (base, ws)
    }

    /// The crux of the whole-project resume fix: a chapter with only some chunks
    /// committed must read as INCOMPLETE even though `derive_status` would scan its
    /// non-empty translated file as `Done`. Otherwise the project run silently skips
    /// it and its remaining chunks are never translated.
    #[tokio::test]
    async fn partial_chapter_is_incomplete_until_every_chunk_is_committed() {
        let (base, ws) = temp_ws("partial");
        let cfg = AppConfig {
            chunk_target_tokens: 4,
            chunk_hard_cap_tokens: 8,
            ..AppConfig::default()
        };
        let raw =
            "# 第一章\n\n一文目。\n\n二文目。\n\n三文目。\n\n四文目。\n\n五文目。\n\n六文目。";
        translation::write_raw(&ws, 1, raw).unwrap();
        let chunks = crate::agents::chunk::chunk_chapter(
            raw,
            cfg.chunk_target_tokens,
            cfg.chunk_hard_cap_tokens,
        );
        assert!(chunks.len() >= 3, "fixture must produce several chunks");

        // No translation yet → incomplete.
        assert!(!chapter_complete_on_disk(&ws, 1, &cfg));

        // Commit only the first chunk → still incomplete (the bug: scans as Done).
        translation::append_chunk(&ws, 1, 0, "ประโยคแรก")
            .await
            .unwrap();
        assert!(
            !chapter_complete_on_disk(&ws, 1, &cfg),
            "a chapter missing chunks must not read as complete"
        );

        for c in &chunks[1..] {
            translation::append_chunk(&ws, 1, c.index as u32, "ประโยค")
                .await
                .unwrap();
        }
        assert!(
            chapter_complete_on_disk(&ws, 1, &cfg),
            "all chunks committed → complete"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// An empty / no-prose chapter has nothing outstanding (avoids queueing it forever).
    #[test]
    fn empty_raw_is_trivially_complete() {
        let (base, ws) = temp_ws("empty");
        translation::write_raw(&ws, 1, "   ").unwrap();
        assert!(chapter_complete_on_disk(&ws, 1, &AppConfig::default()));
        let _ = std::fs::remove_dir_all(&base);
    }
}
