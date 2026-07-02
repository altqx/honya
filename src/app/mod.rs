//! App state, the global key router, and the top-level render dispatch.
//!
//! Layout invariant (see ui::layout::skeleton): header / tabs / rule / body /
//! toast / footer; the overlay is drawn LAST over a `Clear` so it always wins.

pub mod lexicon;
pub mod overlay;
pub mod project;
pub mod qa;
pub mod reader;
pub mod refine;
pub mod shelf;
pub mod translate;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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
use self::refine::RefineScreen;
use self::shelf::ShelfScreen;
use self::translate::{QueueRow, TranslateScreen};

// ui::chrome imports `Screen`, so the variant names and order are load-bearing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Shelf,
    Project,
    Translate,
    Reader,
    Lexicon,
    Refine,
}

impl Screen {
    pub fn from_digit(d: char) -> Option<Screen> {
        match d {
            '1' => Some(Screen::Shelf),
            '2' => Some(Screen::Project),
            '3' => Some(Screen::Translate),
            '4' => Some(Screen::Reader),
            '5' => Some(Screen::Lexicon),
            '6' => Some(Screen::Refine),
            _ => None,
        }
    }
}

/// Screen and overlay requests handled by `apply`.
#[derive(Debug, Clone)]
pub enum Action {
    None,
    Quit,
    Goto(Screen),
    ImportFile {
        source: PathBuf,
        title: String,
        /// Thai title from the wizard's translate step (empty = none).
        title_th: String,
        vol: u32,
        synopsis_raw: String,
        synopsis_th: String,
        /// Append the source's chapters after the volume's last one instead of
        /// scaffolding a fresh project/volume (the "add chapters" flow).
        append: bool,
    },
    /// Translator round-trip for synopsis rerolls.
    TranslateSynopsis {
        raw: String,
        attempt: u32,
    },
    /// Persist the active volume's synopsis (standalone editor accept).
    SaveSynopsis {
        raw: String,
        th: String,
    },
    /// Translator round-trip for title rerolls.
    TranslateProjectTitle {
        raw: String,
        attempt: u32,
    },
    /// Persist PROJECT.md title fields; `id` is the slug.
    SaveProjectTitle {
        id: String,
        raw: String,
        th: String,
    },
    /// Persist a human Reader annotation anchored to a translated line.
    SaveReaderNote {
        chapter: u32,
        line: u32,
        note: String,
    },
    /// Open the chunk editor from raw on-disk Thai.
    OpenReaderEdit {
        chapter: u32,
        chunk: u32,
    },
    /// Save edited chunk prose and clear its review flag.
    SaveReaderEdit {
        chapter: u32,
        chunk: u32,
        text: String,
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
    /// Open the import wizard in append mode: its chapters are appended after
    /// `vol`'s last chapter (e.g. importing a bonus short-story EPUB into Vol.III).
    AddChapters {
        vol: u32,
    },
    RefreshVolumeImages {
        vol: u32,
    },
    RefreshVolumeImagesFromFile {
        vol: u32,
        source: PathBuf,
    },
    RescanImageSources {
        vol: u32,
    },
    /// Delete the given chapters' raw/translated/reruns files from `vol`. Raised
    /// behind a confirm dialog.
    DeleteChapters {
        vol: u32,
        chapters: Vec<u32>,
    },
    /// Re-discover importable source files while the import wizard is open (its
    /// pick step's `r`), so a freshly-dropped file shows up without reopening.
    RescanImports,
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
    OpenChapterAtChunk {
        chapter: u32,
        chunk: u32,
    },
    ReaderStepChapter {
        forward: bool,
    },
    ReaderCopy {
        text: String,
        lines: usize,
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
    /// Translate a whole volume: every not-yet-done chapter, checked against
    /// disk chunk markers (a partial file can scan as done). Raises a confirm.
    StartVolumeTranslation {
        vol: u32,
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
        /// Per-agent provider/model/effort selection (boxed: keeps `Action` small).
        models: Box<crate::model::ModelSet>,
        /// New OpenRouter key: `Some("")` clears it, `Some(k)` sets it, `None`
        /// leaves it untouched (an env var supplies the key, so config is moot).
        openrouter_key: Option<String>,
        /// New Tokenrouter key (same `Some`/`None` semantics as `openrouter_key`).
        tokenrouter_key: Option<String>,
        /// New Google key (same `Some`/`None` semantics as `openrouter_key`).
        google_key: Option<String>,
        /// New Cloudflare account id (same `Some`/`None` semantics as `openrouter_key`).
        cloudflare_account_id: Option<String>,
        /// New Cloudflare API token (same `Some`/`None` semantics as `openrouter_key`).
        cloudflare_api_token: Option<String>,
        /// Startup update behavior (auto-install vs. notify only).
        update_mode: crate::model::UpdateMode,
        /// Update channel: stable releases vs latest git built from source.
        release_channel: crate::model::ReleaseChannel,
        /// Provider request tier for every request (`None` = provider default).
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
    /// Open the import wizard from the Welcome overlay (App supplies the list).
    OpenImport,
    /// Dismiss the Welcome overlay and persist that onboarding is complete.
    DismissWelcome,
    /// Live-preview a theme without persisting (theme picker navigation).
    PreviewTheme(ThemeId),
    /// Commit a theme: apply, persist to config, close the picker.
    SaveTheme(ThemeId),
    /// Abandon the picker: restore the previously-saved theme, close.
    CancelTheme,
    /// Begin GitHub sign-in (Device Flow) to link this app to an account.
    StartRemoteLogin,
    /// Connect the web remote-control relay (requires a linked account).
    EnableRemote,
    /// Disconnect the relay and stop sharing this session.
    DisableRemote,
    /// Sign out: disconnect and forget the linked account.
    RemoteLogout,
    /// Toggle Codex ("Sign in with ChatGPT"): sign in if signed out, else sign out.
    ToggleCodexSignIn,
    /// Open the pending GitHub auth URL.
    OpenAuthUrl,
    /// Copy the pending GitHub auth code via OSC-52.
    CopyAuthCode,
    RefineSubmit {
        text: String,
    },
    RefineCancel,
    RefineClear,
    RefineOpenSessions,
    RefineNewSession,
    RefineSwitchSession {
        id: String,
    },
    RefineDeleteSession {
        id: String,
    },
    RefineRenameSession {
        title: String,
    },
    /// Empty `model` reports the current one.
    RefineSetModel {
        model: String,
    },
    RefineUndo,
    RefineOpenDiff,
    RefineCompact,
    RefineExport,
    /// Cycle the refine edit-approval mode (always-approve → ask → auto) — Ctrl+Tab.
    RefineCycleApprovalMode,
    /// The user's answer to a blocking refine prompt (approval / ask_user).
    RefineRespondInteraction {
        id: u64,
        answer: String,
    },
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
    /// Per-provider clients, built lazily so projects can still open offline.
    pub clients: Option<crate::llm::ClientSet>,
    pub models: ModelSet,
    /// Volume `workspace` resolves and translation runs target.
    pub vol: u32,
}

impl ActiveProject {
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
    pub refine: RefineScreen,
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
    quit_armed_at: Option<std::time::Instant>,

    /// Outbound feed to the relay task while remote is connected.
    remote_out: Option<tokio::sync::mpsc::UnboundedSender<crate::remote::protocol::RemoteOutbound>>,
    /// Kill-switch for the relay task when the channel is not enough.
    remote_kill: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    remote_state: crate::remote::protocol::RemoteState,
    remote_watchers: u32,
    remote_auth_code: Option<crate::model::AuthCodePrompt>,
    /// Live Codex model ids; empty means Settings uses its static fallback.
    codex_models: Vec<String>,
    /// Dashboard label for the live remote session.
    session_label: Option<String>,

    /// Long-lived Refine agent task, spawned lazily on first submit.
    refine_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::agents::refine::RefineControl>>,
    refine_cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    refine_steering: Option<Arc<Mutex<VecDeque<crate::agents::refine::UserTurn>>>>,
    /// Shared approval/ask_user channel for Refine.
    refine_interact: crate::agents::refine::RefineInteract,
    refine_sessions: Vec<crate::workspace::refine_session::SessionMeta>,
    refine_session_id: String,
    /// Last Refine-edited chapter `(vol, ch)`, for `/undo` and `/diff`.
    refine_last_edit: Option<(u32, u32)>,
    refine_dirty: bool,
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
        // Keep Reader source chunks aligned with the pipeline budget.
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
            refine: RefineScreen::new(),
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
            quit_armed_at: None,
            remote_out: None,
            remote_kill: None,
            remote_state: crate::remote::protocol::RemoteState::Disconnected,
            remote_watchers: 0,
            remote_auth_code: None,
            codex_models: Vec::new(),
            session_label: None,
            refine_tx: None,
            refine_cancel: None,
            refine_steering: None,
            refine_interact: crate::agents::refine::RefineInteract::default(),
            refine_sessions: Vec::new(),
            refine_session_id: String::new(),
            refine_last_edit: None,
            refine_dirty: false,
        }
    }

    /// Kept out of `App::new` so tests never spawn a network task.
    pub fn init_remote(&mut self, opt_in: bool) {
        if opt_in && self.cfg.account.is_some() {
            self.apply(Action::EnableRemote);
        }
    }

    /// Raise the recovery overlay for a resumable checkpoint. Kept out of
    /// `App::new` so tests never touch the real recovery file.
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
            // Crash after the last commit: clear the prompt and close the run row.
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

    /// Count checkpoint progress from the shelf scan or checkpoint path fallback.
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
        self.refine.on_app_event(&ev);

        match &ev {
            AppEvent::ChapterStarted { chapter } => {
                self.set_chapter_status(*chapter, ChapterStatus::Translating);
                self.run_active = true;
                self.begin_pending_chapter_run(*chapter);
                if let Some(title) = self.chapter_title(*chapter) {
                    self.translate.set_chapter_title(title);
                }
                // The pipeline already moved this chapter into the running slot.
                self.refresh_queue_panel();
            }
            AppEvent::ChapterQueued { chapter } => {
                self.set_chapter_status(*chapter, ChapterStatus::Pending);
            }
            AppEvent::QueueChanged => {
                self.refresh_queue_panel();
            }
            AppEvent::VolumeStarted { vol, label } => {
                // Scope following chapter events to the new running volume.
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
                salvaged,
            } => {
                if !*salvaged && let Some(ch) = self.chapter_in_event_vol_mut(*chapter) {
                    ch.skipped_chunks = ch.skipped_chunks.saturating_add(1);
                }
                let verb = if *salvaged {
                    "committed unreviewed"
                } else {
                    "skipped (no translation)"
                };
                self.toast = Some(Toast::warn(format!(
                    "ch {chapter} chunk {} {verb} · needs manual review",
                    chunk + 1
                )));
                self.push_log(
                    LogLevel::Warn,
                    format!(
                        "ch {chapter} chunk {} {verb} after {attempts} attempt(s): {reason}",
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
                if context == "import" || context == "image refresh" {
                    self.run_active = false;
                }
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
            AppEvent::VolumeImagesUpdated {
                project_id,
                vol,
                images,
                raw_files,
                translated_files,
            } => {
                self.run_active = false;
                self.refresh_projects();
                self.open_project(project_id.clone());
                self.focus_active_volume(*vol);
                let msg = format!(
                    "updated Vol.{vol:02} images · {images} image(s), {raw_files} raw file(s), {translated_files} translated file(s)"
                );
                self.toast = Some(Toast::info(msg.clone()));
                self.push_log(LogLevel::Info, msg);
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
            // The title editor reuses the synopsis result events.
            AppEvent::SynopsisTranslated { text } => {
                self.overlay.set_synopsis_result(Ok(text.clone()));
                self.toast = Some(Toast::info("translation ready"));
            }
            AppEvent::SynopsisFailed { msg } => {
                self.overlay.set_synopsis_result(Err(msg.clone()));
                self.push_log(LogLevel::Error, format!("translate: {msg}"));
                self.toast = Some(Toast::error(format!("translate: {msg}")));
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
            AppEvent::RemoteAuthCode {
                user_code,
                verification_uri,
                verification_uri_complete,
            } => {
                self.remote_auth_code = Some(crate::model::AuthCodePrompt {
                    code: user_code.clone(),
                    uri: verification_uri.clone(),
                    uri_complete: verification_uri_complete.clone(),
                });
                self.toast = Some(Toast::info(format!(
                    "GitHub: enter {user_code} at {verification_uri} · Ctrl-B open · Ctrl-K copy"
                )));
                self.push_log(
                    LogLevel::Info,
                    format!("GitHub sign-in: code {user_code} → {verification_uri}"),
                );
                self.sync_settings_remote();
            }
            AppEvent::RemoteAuthPending => {}
            AppEvent::RemotePaired {
                login,
                device_id,
                device_token,
            } => {
                self.cfg.account = Some(crate::model::RemoteAccount {
                    github_login: login.clone(),
                    device_id: device_id.clone(),
                    device_token: device_token.clone(),
                });
                self.remote_auth_code = None;
                self.remote_state = crate::remote::protocol::RemoteState::Disconnected;
                let _ = crate::config::save(&self.cfg);
                self.toast = Some(Toast::info(format!("signed in as @{login}")));
                self.push_log(LogLevel::Info, format!("GitHub account linked: @{login}"));
                self.apply(Action::EnableRemote);
            }
            AppEvent::RemoteAuthError { msg } => {
                self.remote_auth_code = None;
                self.remote_state = crate::remote::protocol::RemoteState::Disconnected;
                self.toast = Some(Toast::error(format!("GitHub sign-in: {msg}")));
                self.push_log(LogLevel::Warn, format!("GitHub sign-in failed: {msg}"));
                self.sync_settings_remote();
            }
            AppEvent::CodexAuthUrl { url } => {
                self.toast = Some(Toast::info(
                    "Codex: opening browser to sign in…".to_string(),
                ));
                self.push_log(LogLevel::Info, format!("Codex sign-in: {url}"));
            }
            AppEvent::CodexSignedIn { auth } => {
                self.cfg.codex_auth = Some((**auth).clone());
                let _ = crate::config::save(&self.cfg);
                if let Some(active) = self.active.as_mut() {
                    active.clients = crate::build_clients(&self.cfg).ok();
                }
                crate::codex::models::spawn_fetch_models(*auth.clone(), self.tx.clone());
                self.toast = Some(Toast::info("signed in to Codex".to_string()));
                self.push_log(LogLevel::Info, "Codex account linked".to_string());
            }
            AppEvent::CodexAuthError { msg } => {
                self.toast = Some(Toast::error(format!("Codex sign-in: {msg}")));
                self.push_log(LogLevel::Warn, format!("Codex sign-in failed: {msg}"));
            }
            AppEvent::CodexModels { models } => {
                self.codex_models = models.clone();
                self.sync_settings_remote();
            }
            AppEvent::RemoteStatus { state, watchers } => {
                let was_watchers = self.remote_watchers;
                self.remote_state = *state;
                self.remote_watchers = *watchers;
                // Re-seed the relay cache for a newly-opened dashboard.
                if matches!(state, crate::remote::protocol::RemoteState::Connected)
                    && *watchers >= was_watchers
                {
                    self.push_remote_snapshot();
                }
                self.sync_settings_remote();
            }
            AppEvent::RemoteCommand(cmd) => {
                use crate::remote::protocol::RemoteCommand as Rc;
                let needs_snapshot = matches!(cmd, Rc::OpenProject { .. } | Rc::SetVolume { .. });
                let action = Self::map_remote_command(cmd.clone());
                self.apply(action);
                if needs_snapshot {
                    self.push_remote_snapshot();
                }
            }
            AppEvent::RefineThreadUpdated { session, messages } => {
                self.persist_refine_thread(session.clone(), messages.clone());
            }
            AppEvent::RefineChapterEdited { vol, ch } => {
                self.refine_last_edit = Some((*vol, *ch));
                self.refine_dirty = true;
                self.refresh_active_project();
                if matches!(self.screen, Screen::Reader)
                    && self.active.as_ref().map(|a| a.vol) == Some(*vol)
                {
                    let ws = Workspace::new(self.refine_root().unwrap_or_default(), *vol);
                    self.reader.reload_if_showing(&ws, *ch);
                }
            }
            AppEvent::RefineMessageDone if self.refine_dirty => {
                self.refine_dirty = false;
                self.refresh_active_project();
            }
            AppEvent::RefineRequest(req) => {
                self.handle_refine_request(req.clone());
            }
            _ => {}
        }

        // Project after local state is folded so browsers mirror the TUI.
        if self.remote_out.is_some() {
            self.project_and_send_remote(&ev);
        }
    }

    /// Forward a submitted chat message, spawning the agent lazily.
    fn refine_submit(&mut self, text: String) {
        if self.active.is_none() {
            self.toast = Some(Toast::warn("open a project first to use Refine"));
            return;
        }
        // Refine and the pipeline write the same data blocks; concurrent turns lose updates.
        if self.run_active {
            self.toast = Some(Toast::warn(
                "a translation run is active — pause or stop it before using Refine",
            ));
            return;
        }
        if !self.ensure_refine_agent() {
            return;
        }
        if self.refine.is_in_flight() {
            if let Some(steering) = &self.refine_steering
                && let Ok(mut queue) = steering.lock()
            {
                queue.push_back(crate::agents::refine::UserTurn { text });
                self.toast = Some(Toast::info(
                    "steering queued for the running Refine turn".to_string(),
                ));
            }
            return;
        }
        if let Some(tx) = &self.refine_tx {
            let _ = tx.send(crate::agents::refine::RefineControl::Submit(
                crate::agents::refine::UserTurn { text },
            ));
            self.refine.begin_turn();
        }
    }

    fn ensure_refine_agent(&mut self) -> bool {
        if self.refine_tx.is_some() {
            return true;
        }
        let Some(clients) = self.ensure_active_clients() else {
            self.toast = Some(Toast::warn(
                "no API key — add one in Settings (Ctrl-,) to use Refine",
            ));
            return false;
        };
        let Some(active) = self.active.as_ref() else {
            return false;
        };
        let refine_model = active.models.refine.clone();
        let Some(client) = clients.for_agent(&refine_model) else {
            self.toast = Some(Toast::warn(format!(
                "no API key for {} — add one in Settings (Ctrl-,) to use Refine",
                refine_model.provider.label()
            )));
            return false;
        };
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let steering = Arc::new(Mutex::new(VecDeque::new()));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx_max = crate::agents::refine::model_max_context(&refine_model.model);
        let ctx = crate::agents::refine::RefineCtx {
            client,
            root: active.project.dir.clone(),
            default_vol: active.vol,
            model: refine_model,
            tx: self.tx.clone(),
            cancel: cancel.clone(),
            session_id: self.refine_session_id.clone(),
            interact: self.refine_interact.clone(),
            steering: steering.clone(),
        };
        tokio::spawn(async move {
            crate::agents::refine::run_refine_agent(ctx, rx).await;
        });
        self.refine.set_context_max(ctx_max);
        self.refine_tx = Some(tx);
        self.refine_cancel = Some(cancel);
        self.refine_steering = Some(steering);
        true
    }

    fn refine_cancel(&mut self) {
        if let Some(c) = &self.refine_cancel {
            c.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.refine_interact.cancel_all();
        self.clear_refine_steering();
        self.refine.cancel();
    }

    /// Reset the conversation and stop any in-flight turn at the next round.
    fn refine_clear(&mut self) {
        if let Some(c) = &self.refine_cancel {
            c.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.clear_refine_steering();
        self.refine.clear();
        if let Some(tx) = &self.refine_tx {
            let _ = tx.send(crate::agents::refine::RefineControl::Clear);
        }
    }

    /// Tear down the agent; late thread updates are session-guarded.
    fn teardown_refine(&mut self) {
        if let Some(c) = &self.refine_cancel {
            c.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(tx) = self.refine_tx.take() {
            let _ = tx.send(crate::agents::refine::RefineControl::Shutdown);
        }
        self.refine_cancel = None;
        self.clear_refine_steering();
        self.refine_steering = None;
        self.refine.clear();
        self.refine_sessions.clear();
        self.refine_session_id.clear();
    }

    fn clear_refine_steering(&mut self) {
        if let Some(steering) = &self.refine_steering
            && let Ok(mut queue) = steering.lock()
        {
            queue.clear();
        }
    }

    fn refine_root(&self) -> Option<PathBuf> {
        self.active.as_ref().map(|a| a.project.dir.clone())
    }

    /// Load the most recent session without spawning the agent.
    fn load_refine_sessions(&mut self) {
        let Some(root) = self.refine_root() else {
            return;
        };
        let sessions = crate::workspace::refine_session::list(&root);
        let id = sessions
            .first()
            .map(|s| s.id.clone())
            .unwrap_or_else(crate::workspace::refine_session::new_id);
        let loaded = crate::workspace::refine_session::load(&root, &id);
        let turns = loaded
            .as_ref()
            .map(|s| refine::display_turns(&s.messages))
            .unwrap_or_default();
        let plan = loaded.map(|s| s.plan).unwrap_or_default();
        self.refine.load_turns(turns, id.clone());
        self.refine.set_plan(plan);
        self.refine_session_id = id;
        self.refine_sessions = sessions;
    }

    fn refresh_refine_sessions(&mut self) {
        if let Some(root) = self.refine_root() {
            self.refine_sessions = crate::workspace::refine_session::list(&root);
        }
    }

    /// Drop stale updates so switched sessions cannot be overwritten.
    fn persist_refine_thread(&mut self, session: String, messages: Vec<crate::llm::Message>) {
        use crate::workspace::refine_session::{self as rs, RefineSession};
        if session.is_empty() || session != self.refine_session_id {
            return;
        }
        let Some(root) = self.refine_root() else {
            return;
        };
        let model = self
            .active
            .as_ref()
            .map(|a| a.models.refine.model.clone())
            .unwrap_or_default();
        let mut saved = rs::load(&root, &session)
            .unwrap_or_else(|| RefineSession::new(session.clone(), model.clone()));
        saved.messages = messages;
        saved.model = model;
        saved.plan = self.refine.plan().to_vec();
        saved.updated = chrono::Utc::now();
        if saved.title.trim().is_empty() {
            saved.title = refine_session_title(&saved.messages);
        }
        if let Err(e) = rs::save(&root, &saved) {
            self.push_log(
                LogLevel::Warn,
                format!("could not save refine session: {e}"),
            );
        }
        // Avoid a directory scan on every streamed turn; the picker refreshes lazily.
    }

    fn refine_new_session(&mut self) {
        let id = crate::workspace::refine_session::new_id();
        self.clear_refine_steering();
        self.refine.load_turns(Vec::new(), id.clone());
        self.refine_session_id = id.clone();
        if let Some(tx) = &self.refine_tx {
            let _ = tx.send(crate::agents::refine::RefineControl::SwitchSession(id));
        }
        self.refresh_refine_sessions();
    }

    fn refine_switch_session(&mut self, id: String) {
        let Some(root) = self.refine_root() else {
            return;
        };
        let session = crate::workspace::refine_session::load(&root, &id);
        let turns = session
            .as_ref()
            .map(|s| refine::display_turns(&s.messages))
            .unwrap_or_default();
        let plan = session.map(|s| s.plan).unwrap_or_default();
        self.clear_refine_steering();
        self.refine.load_turns(turns, id.clone());
        self.refine.set_plan(plan);
        self.refine_session_id = id.clone();
        if let Some(tx) = &self.refine_tx {
            let _ = tx.send(crate::agents::refine::RefineControl::SwitchSession(id));
        }
    }

    fn refine_delete_session(&mut self, id: String) {
        let Some(root) = self.refine_root() else {
            return;
        };
        if id.is_empty() {
            return;
        }
        let _ = crate::workspace::refine_session::delete(&root, &id);
        if id == self.refine_session_id {
            let next = crate::workspace::refine_session::list(&root)
                .into_iter()
                .find(|s| s.id != id)
                .map(|s| s.id);
            match next {
                Some(next_id) => self.refine_switch_session(next_id),
                None => self.refine_new_session(),
            }
        }
        self.refresh_refine_sessions();
        if self.refine.picker_open() {
            self.refine
                .open_picker(self.refine_sessions.clone(), self.refine_session_id.clone());
        }
    }

    fn refine_rename_session(&mut self, title: String) {
        use crate::workspace::refine_session::{self as rs, RefineSession};
        let Some(root) = self.refine_root() else {
            return;
        };
        let id = self.refine_session_id.clone();
        let model = self
            .active
            .as_ref()
            .map(|a| a.models.refine.model.clone())
            .unwrap_or_default();
        let mut session =
            rs::load(&root, &id).unwrap_or_else(|| RefineSession::new(id.clone(), model));
        session.title = title;
        session.updated = chrono::Utc::now();
        if let Err(e) = rs::save(&root, &session) {
            self.push_log(
                LogLevel::Warn,
                format!("could not rename refine session: {e}"),
            );
        }
        self.refresh_refine_sessions();
    }

    fn refine_set_model(&mut self, model: String) {
        let model = model.trim().to_string();
        if model.is_empty() {
            let cur = self
                .active
                .as_ref()
                .map(|a| a.models.refine.model.clone())
                .unwrap_or_else(|| self.cfg.models.refine.model.clone());
            self.toast = Some(Toast::info(format!("refine model: {cur}")));
            return;
        }
        if let Some(active) = self.active.as_mut() {
            active.models.refine.set_model(model.clone());
        }
        self.cfg.models.refine.set_model(model.clone());
        self.refine
            .set_context_max(crate::agents::refine::model_max_context(&model));
        if let Err(e) = crate::config::save(&self.cfg) {
            self.push_log(
                LogLevel::Warn,
                format!("could not persist refine model: {e}"),
            );
        }
        if let Some(tx) = &self.refine_tx {
            let _ = tx.send(crate::agents::refine::RefineControl::SetModel(
                model.clone(),
            ));
        }
        self.toast = Some(Toast::info(format!("refine model set to {model}")));
    }

    /// Restore the latest archived prior version from `reruns/`.
    /// Export the active refine conversation to a markdown file at the project root.
    fn refine_export(&mut self) {
        let Some(root) = self.refine_root() else {
            self.toast = Some(Toast::warn("no active refine session"));
            return;
        };
        let id = self.refine_session_id.clone();
        let Some(session) = crate::workspace::refine_session::load(&root, &id) else {
            self.toast = Some(Toast::warn("session not found"));
            return;
        };
        let md = refine_session_to_markdown(&session);
        let path = root.join(format!("refine-{id}.md"));
        match std::fs::write(&path, md) {
            Ok(()) => self.toast = Some(Toast::info(format!("exported to {}", path.display()))),
            Err(e) => self.toast = Some(Toast::error(format!("export failed: {e}"))),
        }
    }

    fn refine_undo(&mut self) {
        let Some((vol, ch)) = self.refine_last_edit else {
            self.toast = Some(Toast::warn("nothing to undo"));
            return;
        };
        let Some(root) = self.refine_root() else {
            return;
        };
        let ws = Workspace::new(root, vol);
        let newest = crate::workspace::volume::chapter_runs(&ws, ch)
            .into_iter()
            .filter_map(|r| r.archived)
            .max();
        let Some(rel) = newest else {
            self.toast = Some(Toast::warn("no archived version to restore"));
            return;
        };
        match std::fs::read_to_string(ws.vol_rel(&rel)) {
            Ok(content) => {
                if let Err(e) = std::fs::write(ws.translated(ch), content) {
                    self.toast = Some(Toast::error(format!("undo failed: {e}")));
                    return;
                }
                self.refine_last_edit = None;
                self.refresh_active_project();
                if matches!(self.screen, Screen::Reader) {
                    self.reader.load(&ws, ch);
                }
                self.toast = Some(Toast::info(format!(
                    "restored the previous version of vol {vol} ch {ch}"
                )));
            }
            Err(e) => self.toast = Some(Toast::error(format!("undo failed: {e}"))),
        }
    }

    fn refine_open_diff(&mut self) {
        let Some((vol, ch)) = self.refine_last_edit else {
            self.toast = Some(Toast::warn("no recent edit to diff"));
            return;
        };
        self.set_active_volume(vol);
        self.apply(Action::OpenChapter { chapter: ch });
        self.reader.enter_diff();
    }

    /// Keep heavier spawns in `App`; the agent only emits requests.
    fn handle_refine_request(&mut self, req: crate::model::RefineRequest) {
        use crate::model::RefineRequest as R;
        match req {
            R::Retranslate { vol, chapters } => {
                self.set_active_volume(vol);
                self.apply(Action::RestartTranslation { chapters });
            }
            R::RefineChapter { vol, ch, feedback } => {
                if let Some(active) = self.active.as_ref() {
                    let ws = Workspace::new(active.project.dir.clone(), vol);
                    if let Err(e) = crate::workspace::style::append_note(&ws, &feedback) {
                        self.push_log(
                            LogLevel::Warn,
                            format!("could not record refine feedback: {e}"),
                        );
                    }
                }
                self.set_active_volume(vol);
                self.apply(Action::RestartTranslation { chapters: vec![ch] });
            }
        }
    }

    fn push_remote_snapshot(&self) {
        if let Some(out) = &self.remote_out {
            let _ = out.send(crate::remote::protocol::RemoteOutbound::Snapshot(Box::new(
                self.remote_snapshot(),
            )));
        }
    }

    fn remote_snapshot(&self) -> crate::remote::protocol::RemoteSnapshot {
        use crate::remote::protocol::{LogLine, RemoteSnapshot};
        let (running, pending) = self.remote_queue();
        let paused = self
            .run_ctl
            .as_ref()
            .map(|c| c.is_paused())
            .unwrap_or(false);
        let (run, chapter) = self.translate.usage_snapshots();
        let log_tail = self
            .log
            .iter()
            .rev()
            .take(40)
            .rev()
            .map(|(level, msg)| LogLine {
                level: remote_log_level(*level).into(),
                msg: msg.clone(),
            })
            .collect();
        RemoteSnapshot {
            app_version: crate::update::version_string(),
            project: self.active.as_ref().map(|a| a.project.title.clone()),
            vol: self.active.as_ref().map(|a| a.vol),
            run_active: self.run_active,
            paused,
            running,
            queue: pending.clone(),
            tally: self.remote_tally(),
            usage_run: run,
            usage_chapter: chapter,
            log_tail,
            chapters: self.remote_chapter_roster(),
            projects: self.remote_projects(),
            volumes: self.remote_volumes(),
            lexicon: self.remote_lexicon(),
        }
    }

    fn remote_projects(&self) -> Vec<crate::remote::protocol::RemoteProject> {
        use crate::remote::protocol::RemoteProject;
        let active_id = self.active.as_ref().map(|a| a.project.id.as_str());
        self.projects
            .iter()
            .map(|p| {
                let mut chapters = 0u32;
                let mut done = 0u32;
                for v in &p.volumes {
                    for ch in &v.chapters {
                        chapters += 1;
                        if ch.status == ChapterStatus::Done {
                            done += 1;
                        }
                    }
                }
                RemoteProject {
                    id: p.id.clone(),
                    title: p.title.clone(),
                    title_th: p.title_th.clone(),
                    volumes: p.volumes.len() as u32,
                    chapters,
                    done,
                    active: active_id == Some(p.id.as_str()),
                }
            })
            .collect()
    }

    fn remote_volumes(&self) -> Vec<crate::remote::protocol::RemoteVolume> {
        use crate::remote::protocol::RemoteVolume;
        let Some(active) = self.active.as_ref() else {
            return Vec::new();
        };
        // The active volume's recap/synopsis is one disk read against the live
        // workspace; other volumes carry counts + label only.
        let vd = crate::workspace::volume::load(&active.workspace);
        active
            .project
            .volumes
            .iter()
            .map(|v| {
                let total = v.chapters.len() as u32;
                let done = v
                    .chapters
                    .iter()
                    .filter(|c| c.status == ChapterStatus::Done)
                    .count() as u32;
                let is_active = v.number == active.vol;
                RemoteVolume {
                    number: v.number,
                    label: v.label.clone(),
                    synopsis_th: if is_active {
                        vd.synopsis_th.clone()
                    } else {
                        String::new()
                    },
                    recap: if is_active {
                        vd.running_recap.clone()
                    } else {
                        String::new()
                    },
                    done,
                    total,
                    active: is_active,
                }
            })
            .collect()
    }

    fn remote_lexicon(&self) -> crate::remote::protocol::RemoteLexicon {
        use crate::remote::protocol::{RemoteCharacter, RemoteLexicon, RemoteTerm};
        let Some(active) = self.active.as_ref() else {
            return RemoteLexicon::default();
        };
        let ws = &active.workspace;
        let characters = crate::workspace::characters::load(ws)
            .into_iter()
            .map(|c| RemoteCharacter {
                jp_name: c.jp_name,
                thai_name: c.thai_name,
                romaji: c.romaji,
                gender: c.gender,
                honorific: c.honorific,
                notes: c.notes,
                first_seen_chapter: c.first_seen_chapter,
            })
            .collect();
        let glossary = crate::workspace::glossary::load(ws)
            .into_iter()
            .map(|t| {
                let policy = remote_term_policy(crate::workspace::glossary::effective_policy(&t));
                RemoteTerm {
                    jp_term: t.jp_term,
                    thai_term: t.thai_term,
                    romaji: t.romaji,
                    category: t.category,
                    gloss: t.gloss,
                    policy: policy.into(),
                    first_seen_chapter: t.first_seen_chapter,
                }
            })
            .collect();
        RemoteLexicon {
            characters,
            glossary,
        }
    }

    fn remote_chapter_roster(&self) -> Vec<crate::remote::protocol::RemoteChapter> {
        use crate::model::ChapterKind;
        use crate::remote::protocol::RemoteChapter;
        let Some(active) = self.active.as_ref() else {
            return Vec::new();
        };
        let mut roster = Vec::new();
        for vol in &active.project.volumes {
            for ch in &vol.chapters {
                let kind = match ch.kind {
                    ChapterKind::Prose => "prose",
                    ChapterKind::ImageOnly => "image",
                    ChapterKind::Empty => "empty",
                };
                roster.push(RemoteChapter {
                    vol: vol.number,
                    ch: ch.number,
                    title: ch.title.clone(),
                    kind: kind.into(),
                    status: remote_chapter_status(ch.status).into(),
                });
            }
        }
        roster
    }

    fn remote_queue(
        &self,
    ) -> (
        Option<crate::remote::protocol::ChapterId>,
        Vec<crate::remote::protocol::ChapterId>,
    ) {
        use crate::remote::protocol::ChapterId;
        match self.run_queue.as_ref() {
            Some(q) => {
                let (running, pending) = q.snapshot();
                let running = running.map(|(vol, ch)| ChapterId { vol, ch });
                let pending = pending
                    .into_iter()
                    .map(|(vol, ch)| ChapterId { vol, ch })
                    .collect();
                (running, pending)
            }
            None => (None, Vec::new()),
        }
    }

    fn remote_tally(&self) -> crate::remote::protocol::TallySnapshot {
        let t = self.tally();
        crate::remote::protocol::TallySnapshot {
            done: t.done,
            working: t.working,
            pending: t.pending,
            failed: t.failed,
            total: t.done + t.working + t.pending + t.failed,
        }
    }

    fn project_and_send_remote(&self, ev: &AppEvent) {
        use crate::remote::protocol::{LogLine, RemoteDelta as D, RemoteOutbound as O};
        let Some(out) = &self.remote_out else {
            return;
        };
        let send = |d: D| {
            let _ = out.send(O::Delta(d));
        };
        match ev {
            AppEvent::QueueChanged => {
                let (running, pending) = self.remote_queue();
                send(D::Queue { running, pending });
            }
            AppEvent::ChapterStarted { chapter } => {
                let (running, pending) = self.remote_queue();
                send(D::Queue { running, pending });
                send(D::Chapter {
                    chapter: *chapter,
                    status: "translating".into(),
                });
                send(D::Tally(self.remote_tally()));
            }
            AppEvent::ChapterStateChanged { chapter, state } => {
                send(D::Chapter {
                    chapter: *chapter,
                    status: remote_chapter_status(*state).into(),
                });
                send(D::Tally(self.remote_tally()));
            }
            AppEvent::ChapterCompleted { chapter } => {
                send(D::Chapter {
                    chapter: *chapter,
                    status: "done".into(),
                });
                send(D::Tally(self.remote_tally()));
            }
            AppEvent::ChapterFailed { chapter, .. } => {
                send(D::Chapter {
                    chapter: *chapter,
                    status: "failed".into(),
                });
                send(D::Tally(self.remote_tally()));
            }
            AppEvent::ChunkStarted {
                chapter,
                chunk,
                total,
                ..
            } => send(D::Chunk {
                chapter: *chapter,
                chunk: *chunk,
                total: *total,
                state: "translating".into(),
            }),
            AppEvent::ChunkStateChanged {
                chapter,
                chunk,
                state,
            } => send(D::Chunk {
                chapter: *chapter,
                chunk: *chunk,
                total: 0,
                state: remote_chunk_state(*state).into(),
            }),
            AppEvent::StreamDelta {
                chapter,
                chunk,
                role,
                delta,
            } => send(D::Stream {
                chapter: *chapter,
                chunk: *chunk,
                role: remote_agent_role(*role).into(),
                delta: delta.clone(),
            }),
            AppEvent::UsageUpdate { run, chapter } => send(D::Usage {
                run: run.into(),
                chapter: chapter.into(),
            }),
            AppEvent::Log { level, msg } => send(D::Log(LogLine {
                level: remote_log_level(*level).into(),
                msg: msg.clone(),
            })),
            AppEvent::Error { context, msg } => send(D::Log(LogLine {
                level: "error".into(),
                msg: format!("{context}: {msg}"),
            })),
            AppEvent::PipelineFinished {
                chapters_done,
                chapters_failed,
                chapters_need_review,
                stopped,
                ..
            } => {
                send(D::RunFinished {
                    done: *chapters_done,
                    failed: *chapters_failed,
                    need_review: *chapters_need_review,
                    stopped: *stopped,
                });
                send(D::Tally(self.remote_tally()));
            }
            // Cast/glossary changed — refresh the lexicon view wholesale. Fires
            // during the Orchestrator's metadata turn; cheap (small files).
            AppEvent::CharacterUpserted { .. } | AppEvent::GlossaryUpserted { .. } => {
                send(D::Lexicon(self.remote_lexicon()));
            }
            AppEvent::PipelinePaused
            | AppEvent::PipelineResumed
            | AppEvent::VolumeStarted { .. }
            | AppEvent::VolumeRecapUpdated { .. } => {
                let _ = out.send(O::Snapshot(Box::new(self.remote_snapshot())));
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
            // A run re-attempts every flagged chunk, re-emitting ChunkNeedsReview
            // for any still skipped — so rebuild the count from zero each run.
            ch.skipped_chunks = 0;
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

    /// Snapshot glossary state for the chapter's finished run record.
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
            Screen::Refine if self.active.is_none() => Action::None,
            Screen::Refine => self.refine.handle_mouse(m),
        }
    }

    /// True during the one-second Ctrl-C confirmation window.
    fn quit_armed(&self) -> bool {
        const QUIT_CONFIRM_WINDOW: std::time::Duration = std::time::Duration::from_secs(1);
        self.quit_armed_at
            .is_some_and(|t| t.elapsed() < QUIT_CONFIRM_WINDOW)
    }

    /// Decide what a key means given the current overlay / screen / focus state.
    fn route_key(&mut self, k: KeyEvent) -> Action {
        if k.modifiers.contains(KeyModifiers::CONTROL)
            && k.code == KeyCode::Char('c')
            && matches!(self.overlay, Overlay::None)
            && matches!(self.screen, Screen::Refine)
            && self.refine.is_in_flight()
        {
            return self.route_to_screen(k);
        }

        if k.modifiers.contains(KeyModifiers::CONTROL)
            && k.code == KeyCode::Char('r')
            && matches!(self.overlay, Overlay::None)
            && matches!(self.screen, Screen::Refine)
            && self.active.is_some()
        {
            return self.route_to_screen(k);
        }

        // First Ctrl-C arms; a second inside the window quits.
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            if self.quit_armed() {
                return Action::Quit;
            }
            self.quit_armed_at = Some(std::time::Instant::now());
            return Action::None;
        }

        if k.modifiers.contains(KeyModifiers::CONTROL)
            && k.code == KeyCode::Char('r')
            && let Some(action) = self.remote_shortcut_action()
        {
            return action;
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
            KeyCode::Char(d @ '1'..='6') => {
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
            KeyCode::Esc | KeyCode::Backspace if self.toast.is_some() => {
                self.toast = None;
                return Action::None;
            }
            _ => {}
        }

        // 4) Otherwise the active screen decides.
        self.route_to_screen(k)
    }

    fn remote_shortcut_action(&self) -> Option<Action> {
        self.cfg.account.as_ref()?;
        Some(if self.remote_out.is_some() {
            Action::DisableRemote
        } else {
            Action::EnableRemote
        })
    }

    /// True when a focused screen text field should swallow single-letter globals.
    fn screen_is_capturing(&self) -> bool {
        (matches!(self.screen, Screen::Lexicon) && self.lexicon.is_capturing())
            // Refine only captures (and only works) when a project is open.
            || (matches!(self.screen, Screen::Refine)
                && self.active.is_some()
                && self.refine.is_capturing())
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
            // Refine is per-project: ignore keys until a project is open.
            Screen::Refine if self.active.is_none() => Action::None,
            Screen::Refine => self
                .refine
                .handle_key(k, self.active.as_ref().map(|a| &a.project)),
        }
    }

    fn next_screen(&self) -> Screen {
        match self.screen {
            Screen::Shelf => Screen::Project,
            Screen::Project => Screen::Translate,
            Screen::Translate => Screen::Reader,
            Screen::Reader => Screen::Lexicon,
            Screen::Lexicon => Screen::Refine,
            Screen::Refine => Screen::Shelf,
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
                self.sync_settings_remote();
            }
            Action::CloseOverlay => {
                self.overlay = Overlay::None;
            }
            Action::RefineSubmit { text } => {
                self.refine_submit(text);
            }
            Action::RefineCancel => {
                self.refine_cancel();
            }
            Action::RefineClear => {
                self.refine_clear();
            }
            Action::RefineOpenSessions => {
                self.refresh_refine_sessions();
                self.refine
                    .open_picker(self.refine_sessions.clone(), self.refine_session_id.clone());
            }
            Action::RefineNewSession => {
                self.refine_new_session();
            }
            Action::RefineSwitchSession { id } => {
                self.refine_switch_session(id);
            }
            Action::RefineDeleteSession { id } => {
                self.refine_delete_session(id);
            }
            Action::RefineRenameSession { title } => {
                self.refine_rename_session(title);
            }
            Action::RefineSetModel { model } => {
                self.refine_set_model(model);
            }
            Action::RefineUndo => {
                self.refine_undo();
            }
            Action::RefineOpenDiff => {
                self.refine_open_diff();
            }
            Action::RefineCompact => {
                if let Some(tx) = &self.refine_tx {
                    let _ = tx.send(crate::agents::refine::RefineControl::Compact);
                    self.toast = Some(Toast::info("compacting refine conversation…".to_string()));
                } else {
                    self.toast = Some(Toast::warn("no active refine session".to_string()));
                }
            }
            Action::RefineExport => {
                self.refine_export();
            }
            Action::RefineCycleApprovalMode => {
                let mode = self.refine_interact.cycle_mode();
                self.refine.set_approval_mode(mode);
                let detail = match mode {
                    crate::agents::refine::ApprovalMode::Auto => {
                        "auto — the model asks before edits you might not want"
                    }
                    crate::agents::refine::ApprovalMode::Ask => {
                        "ask — every edit pauses for your accept/reject"
                    }
                    crate::agents::refine::ApprovalMode::Always => {
                        "always-approve — edits apply directly"
                    }
                };
                self.toast = Some(Toast::info(format!("approval mode: {detail}")));
            }
            Action::RefineRespondInteraction { id, answer } => {
                self.refine_interact.resolve(id, answer);
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
            Action::AddChapters { vol } => {
                self.open_add_chapters(vol);
            }
            Action::RefreshVolumeImages { vol } => {
                self.refresh_volume_images(vol);
            }
            Action::RefreshVolumeImagesFromFile { vol, source } => {
                self.start_volume_image_refresh(vol, source);
            }
            Action::RescanImageSources { vol } => {
                let files = epub_source_files();
                self.overlay.set_image_source_files(files.clone());
                if !matches!(self.overlay, Overlay::ImageSource(_)) {
                    self.overlay = Overlay::image_source(files.clone(), vol);
                }
                self.toast = Some(Toast::info(format!(
                    "{} source EPUB file(s) found",
                    files.len()
                )));
            }
            Action::DeleteChapters { vol, chapters } => {
                self.delete_chapters(vol, &chapters);
            }
            Action::RescanImports => {
                let files = crate::workspace::scan::find_importable_files(&working_root());
                self.shelf.rescan(&working_root());
                if let Overlay::Import(st) = &mut self.overlay {
                    st.set_files(files.clone());
                }
                self.toast = Some(Toast::info(format!(
                    "{} importable file(s) found",
                    files.len()
                )));
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
            Action::OpenChapterAtChunk { chapter, chunk } => {
                self.open_chapter(chapter);
                if matches!(self.screen, Screen::Reader) && self.reader.current_chapter() == chapter
                {
                    self.reader.scroll_to_chunk(chunk);
                }
            }
            Action::ReaderStepChapter { forward } => {
                self.step_reader_chapter(forward);
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
            Action::ImportFile {
                source,
                title,
                title_th,
                vol,
                synopsis_raw,
                synopsis_th,
                append,
            } => {
                self.start_import(
                    source,
                    title,
                    title_th,
                    vol,
                    synopsis_raw,
                    synopsis_th,
                    append,
                );
            }
            Action::TranslateSynopsis { raw, attempt } => {
                self.translate_synopsis(raw, attempt);
            }
            Action::SaveSynopsis { raw, th } => {
                self.save_synopsis(raw, th);
            }
            Action::TranslateProjectTitle { raw, attempt } => {
                self.translate_project_title(raw, attempt);
            }
            Action::SaveProjectTitle { id, raw, th } => {
                self.save_project_title(id, raw, th);
            }
            Action::SaveReaderNote {
                chapter,
                line,
                note,
            } => {
                self.save_reader_note(chapter, line, note);
            }
            Action::OpenReaderEdit { chapter, chunk } => {
                self.open_reader_edit(chapter, chunk);
            }
            Action::SaveReaderEdit {
                chapter,
                chunk,
                text,
            } => {
                self.save_reader_edit(chapter, chunk, text);
            }
            Action::StartTranslation { chapters } => {
                self.request_translation(chapters);
            }
            Action::EnqueueChapters { vol, chapters } => {
                if self.is_live_run() {
                    self.enqueue_live(vol, chapters);
                } else {
                    // Idle: a run starts on the active volume, so honor the
                    // requested volume first (no-op if it is already active or
                    // absent). Without this an enqueue for another volume — now
                    // reachable from the dashboard's chapter board — would
                    // silently translate the active volume instead.
                    self.set_active_volume(vol);
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
            Action::StartVolumeTranslation { vol } => {
                self.request_volume_translation(vol);
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
                models,
                openrouter_key,
                tokenrouter_key,
                google_key,
                cloudflare_account_id,
                cloudflare_api_token,
                update_mode,
                release_channel,
                service_tier,
                max_attempts,
                loop_stall_secs,
                max_chapter_retranslates,
            } => {
                self.save_settings(
                    *models,
                    openrouter_key,
                    tokenrouter_key,
                    google_key,
                    cloudflare_account_id,
                    cloudflare_api_token,
                    update_mode,
                    release_channel,
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
                let files = crate::workspace::scan::find_importable_files(&working_root());
                self.overlay = Overlay::import(files, &self.projects);
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
            Action::StartRemoteLogin => self.start_remote_login(),
            Action::EnableRemote => self.enable_remote(),
            Action::DisableRemote => self.disable_remote(),
            Action::RemoteLogout => self.remote_logout(),
            Action::ToggleCodexSignIn => {
                if self.cfg.codex_auth.is_some() {
                    self.cfg.codex_auth = None;
                    let _ = crate::config::save(&self.cfg);
                    if let Some(active) = self.active.as_mut() {
                        active.clients = crate::build_clients(&self.cfg).ok();
                    }
                    self.toast = Some(Toast::info("signed out of Codex".to_string()));
                } else {
                    crate::codex::auth::spawn_codex_login(self.tx.clone());
                }
            }
            Action::OpenAuthUrl => self.open_auth_url(),
            Action::CopyAuthCode => self.copy_auth_code(),
            Action::ReaderCopy { text, lines } => {
                // OSC-52 has no reliable success signal, so always toast.
                match crate::remote::copy_to_clipboard(&text) {
                    Ok(()) => {
                        self.toast = Some(Toast::info(format!("copied {lines} lines of Thai")))
                    }
                    Err(_) => {
                        self.toast = Some(Toast::warn("couldn't copy — terminal blocked clipboard"))
                    }
                }
            }
        }
    }

    fn open_auth_url(&mut self) {
        let Some(prompt) = self.remote_auth_code.as_ref() else {
            return;
        };
        let target = if prompt.uri_complete.is_empty() {
            prompt.uri.clone()
        } else {
            prompt.uri_complete.clone()
        };
        match crate::remote::open_url(&target) {
            Ok(()) => self.toast = Some(Toast::info("opened GitHub in your browser")),
            Err(_) => {
                self.toast = Some(Toast::warn(
                    "couldn't open a browser — copy the code instead",
                ))
            }
        }
    }

    fn copy_auth_code(&mut self) {
        let Some(prompt) = self.remote_auth_code.as_ref() else {
            return;
        };
        let code = prompt.code.clone();
        // OSC-52 cannot be confirmed, so the code stays visible.
        match crate::remote::copy_to_clipboard(&code) {
            Ok(()) => self.toast = Some(Toast::info(format!("copied {code} — paste at GitHub"))),
            Err(_) => self.toast = Some(Toast::warn("couldn't copy — type the code shown")),
        }
    }

    fn start_remote_login(&mut self) {
        if !crate::remote::github_login_configured() {
            self.toast = Some(Toast::warn("GitHub sign-in isn't configured in this build"));
            return;
        }
        if matches!(
            self.remote_state,
            crate::remote::protocol::RemoteState::Pairing
        ) {
            return;
        }
        self.remote_state = crate::remote::protocol::RemoteState::Pairing;
        self.remote_auth_code = None;
        crate::remote::auth::spawn_device_login(self.tx.clone());
        self.toast = Some(Toast::info("starting GitHub sign-in …"));
        self.sync_settings_remote();
    }

    fn enable_remote(&mut self) {
        let Some(account) = self.cfg.account.clone() else {
            self.toast = Some(Toast::warn("sign in with GitHub first"));
            return;
        };
        if self.remote_out.is_some() {
            return;
        }
        // Fresh per launch, so parallel local sessions stay distinct.
        let session_id = crate::remote::new_session_id();
        let label = self.remote_session_label();
        let (out_tx, out_rx) = tokio::sync::mpsc::unbounded_channel();
        let kill = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        self.remote_out = Some(out_tx);
        self.remote_kill = Some(kill.clone());
        self.session_label = Some(label.clone());
        crate::remote::relay::spawn_relay(
            self.tx.clone(),
            account.device_token,
            session_id,
            label,
            out_rx,
            kill,
        );
        self.push_log(LogLevel::Info, "remote control enabled".into());
        self.sync_settings_remote();
    }

    /// Dashboard label: project volume or shelf cwd, suffixed with the host.
    fn remote_session_label(&self) -> String {
        let host = crate::remote::auth::device_label();
        let base = match self.active.as_ref() {
            Some(active) => format!("{} · Vol.{:02}", active.project.title, active.vol),
            None => {
                let cwd = std::env::current_dir()
                    .ok()
                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .unwrap_or_else(|| "honya".into());
                format!("shelf {cwd}")
            }
        };
        format!("{base} · {host}")
    }

    fn disable_remote(&mut self) {
        if let Some(kill) = self.remote_kill.take() {
            kill.store(false, std::sync::atomic::Ordering::Relaxed);
        }
        self.remote_out = None;
        self.remote_watchers = 0;
        self.remote_state = crate::remote::protocol::RemoteState::Disconnected;
        self.session_label = None;
        self.sync_settings_remote();
    }

    fn remote_logout(&mut self) {
        self.disable_remote();
        self.cfg.account = None;
        self.remote_auth_code = None;
        let _ = crate::config::save(&self.cfg);
        self.toast = Some(Toast::info("signed out of GitHub"));
        self.sync_settings_remote();
    }

    fn sync_settings_remote(&mut self) {
        let login = self.cfg.account.as_ref().map(|a| a.github_login.clone());
        // Enabled means a live relay feed, not persisted config.
        let enabled = self.remote_out.is_some();
        let state = self.remote_state;
        let watchers = self.remote_watchers;
        let code = self.remote_auth_code.clone();
        let session_label = self.session_label.clone();
        let codex_models = self.codex_models.clone();
        if let self::overlay::Overlay::Settings(st) = &mut self.overlay {
            st.account_login = login;
            st.remote_enabled = enabled;
            st.remote_state = state;
            st.remote_watchers = watchers;
            st.remote_auth_code = code;
            st.session_label = session_label;
            if !codex_models.is_empty() {
                st.codex_models = codex_models;
            }
        }
    }

    /// Browser commands reuse the same mutation funnel as keystrokes.
    fn map_remote_command(cmd: crate::remote::protocol::RemoteCommand) -> Action {
        use crate::remote::protocol::RemoteCommand as Rc;
        match cmd {
            Rc::Pause => Action::PauseRun,
            Rc::Stop => Action::StopRun,
            Rc::StartProject => Action::BeginProjectTranslation,
            Rc::Enqueue { vol, chapters } => Action::EnqueueChapters { vol, chapters },
            Rc::QueueMoveUp { vol, ch } => Action::QueueMoveUp { vol, ch },
            Rc::QueueMoveDown { vol, ch } => Action::QueueMoveDown { vol, ch },
            Rc::Dequeue { vol, ch } => Action::DequeueChapter { vol, ch },
            Rc::OpenProject { id } => Action::OpenProject(id),
            Rc::SetVolume { vol } => Action::SetActiveVolume { vol },
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

    /// Switch active volume without resetting screens.
    fn set_active_volume(&mut self, vol: u32) {
        // Refine is cross-volume, so cursor-following must not wipe the chat.
        if let Some(active) = self.active.as_mut()
            && active.vol != vol
            && active.project.volumes.iter().any(|v| v.number == vol)
        {
            active.vol = vol;
            active.workspace = Workspace::new(active.project.dir.clone(), vol);
        }
    }

    /// Move the Project cursor onto `vol` and make it the active volume.
    fn focus_active_volume(&mut self, vol: u32) {
        if let Some(active) = self.active.as_ref() {
            self.project.focus_volume(active, vol);
        }
        self.set_active_volume(vol);
    }

    /// Open the import wizard for adding the open project's next volume.
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
        let files = crate::workspace::scan::find_importable_files(&working_root());
        self.overlay = Overlay::import_into(files, &self.projects, title, next);
    }

    /// Open the import wizard in append mode: chapters land after `vol`'s last one.
    fn open_add_chapters(&mut self, vol: u32) {
        if self.run_active {
            self.toast = Some(Toast::warn("a run is already in progress"));
            return;
        }
        let Some(active) = self.active.as_ref() else {
            self.toast = Some(Toast::warn("no project open"));
            return;
        };
        let title = active.project.title.clone();
        let files = crate::workspace::scan::find_importable_files(&working_root());
        self.overlay = Overlay::import_append(files, &self.projects, title, vol);
    }

    fn refresh_volume_images(&mut self, vol: u32) {
        if self.run_active {
            self.toast = Some(Toast::warn("a run is already in progress"));
            return;
        }
        let Some(active) = self.active.as_ref() else {
            self.toast = Some(Toast::warn("no project open"));
            return;
        };
        let ws = Workspace::new(active.project.dir.clone(), vol);
        let Some(source) = volume_source_file(&ws) else {
            self.open_image_source_picker(vol);
            return;
        };
        self.start_volume_image_refresh(vol, source);
    }

    fn open_image_source_picker(&mut self, vol: u32) {
        let files = epub_source_files();
        self.overlay = Overlay::image_source(files, vol);
        self.toast = Some(Toast::info(format!("choose source EPUB for Vol.{vol:02}")));
    }

    fn start_volume_image_refresh(&mut self, vol: u32, source: PathBuf) {
        if self.run_active {
            self.toast = Some(Toast::warn("a run is already in progress"));
            return;
        }
        let Some(active) = self.active.as_ref() else {
            self.toast = Some(Toast::warn("no project open"));
            return;
        };
        let project_id = active.project.id.clone();
        let project_dir = active.project.dir.clone();
        if !source.exists() {
            self.open_image_source_picker(vol);
            self.toast = Some(Toast::warn(format!(
                "source EPUB not found; choose it again: {}",
                source.display()
            )));
            return;
        }
        if !crate::document_import::is_epub_path(&source) {
            self.toast = Some(Toast::warn(
                "image refresh currently supports EPUB sources only",
            ));
            return;
        }

        let tx = self.tx.clone();
        self.overlay = Overlay::None;
        self.run_active = true;
        self.toast = Some(Toast::info(format!("updating Vol.{vol:02} images …")));
        tokio::spawn(async move {
            match run_volume_image_refresh(source, project_dir, vol, &tx).await {
                Ok(report) => tx.send(AppEvent::VolumeImagesUpdated {
                    project_id,
                    vol,
                    images: report.images,
                    raw_files: report.raw_files,
                    translated_files: report.translated_files,
                }),
                Err(e) => tx.send(AppEvent::Error {
                    context: "image refresh".to_string(),
                    msg: e.to_string(),
                }),
            }
        });
    }

    /// Remove chapters' raw/translated/reruns files from the active project's `vol`,
    /// then re-scan so the tree reflects the deletion. Scan tolerates the resulting
    /// gaps in chapter numbering, so no renumbering is needed.
    fn delete_chapters(&mut self, vol: u32, chapters: &[u32]) {
        if self.run_active {
            self.toast = Some(Toast::warn(
                "can't delete chapters while a run is in progress",
            ));
            return;
        }
        let Some(active) = self.active.as_ref() else {
            return;
        };
        let ws = Workspace::new(active.project.dir.clone(), vol);
        let mut removed = 0;
        for &ch in chapters {
            match crate::workspace::translation::delete_chapter(&ws, ch) {
                Ok(()) => removed += 1,
                Err(e) => self.push_log(LogLevel::Error, format!("delete ch {ch:03}: {e}")),
            }
        }
        self.rescan_active();
        self.resync_run_checkpoint();
        self.toast = Some(Toast::info(format!(
            "deleted {removed} chapter(s) from Vol.{vol:02}"
        )));
    }

    /// Re-scan the open project from disk and swap it into `self.active`, keeping
    /// the active volume and clients. Used after on-disk chapter edits.
    fn rescan_active(&mut self) {
        let Some(dir) = self.active.as_ref().map(|a| a.project.dir.clone()) else {
            return;
        };
        if let Some(fresh) = crate::workspace::scan::scan_one_project(&dir)
            && let Some(a) = self.active.as_mut()
        {
            a.project = fresh;
        }
        self.refresh_projects();
    }

    /// Delete a project directory from disk. The shelf is a live scan, so this is
    /// the only removal path; `remove_project_dir` guards the recursive delete.
    fn delete_project(&mut self, id: String) {
        if self.run_active {
            self.toast = Some(Toast::warn(
                "can't delete a project while a run is in progress",
            ));
            return;
        }
        // Resolve from the rendered shelf; stale entries still hit the disk guard.
        let Some(project) = self.projects.iter().find(|p| p.id == id).cloned() else {
            self.toast = Some(Toast::error(format!("project {id} not found")));
            return;
        };
        match remove_project_dir(&project.dir) {
            Ok(()) => {
                // Close the workspace if it pointed at the deleted directory.
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

    /// Open `project` on `vol`, reset per-project screens, and land on Project.
    /// Missing keys disable translation only; browsing still works.
    fn activate_project(&mut self, project: Project, vol: u32) -> bool {
        // The refine agent is bound to the previous project root + volume.
        self.teardown_refine();
        let models = project
            .models
            .clone()
            .unwrap_or_else(|| self.cfg.models.clone());
        let workspace = Workspace::new(project.dir.clone(), vol);
        let clients = crate::build_clients(&self.cfg).ok();
        // Only the no-key case gets the Settings hint toast.
        let no_key = !crate::config::any_provider_key(&self.cfg);
        let id = project.id.clone();
        self.active = Some(ActiveProject {
            project,
            workspace,
            clients,
            models,
            vol,
        });
        self.lexicon.reset();
        self.project = ProjectScreen::new();
        self.load_refine_sessions();
        self.screen = Screen::Project;
        // Clear stale "draft" status from projects finished in another session.
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
    fn ensure_active_clients(&mut self) -> Option<crate::llm::ClientSet> {
        let active = self.active.as_mut()?;
        if active.clients.is_none() {
            active.clients = crate::build_clients(&self.cfg).ok();
        }
        // Treat a key-less set (no provider configured) as "no client", matching
        // the old single-client gate so callers still surface the no-key toast.
        active.clients.clone().filter(|c| !c.is_empty())
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

    /// Step the Reader one chapter, crossing volume boundaries when needed.
    fn step_reader_chapter(&mut self, forward: bool) {
        let cur = self.reader.current_chapter();
        let Some(active) = self.active.as_ref() else {
            self.toast = Some(Toast::warn("no project open"));
            return;
        };
        let vol = active.active_vol();
        let vol_max = active
            .project
            .volumes
            .iter()
            .find(|v| v.number == vol)
            .map(|v| v.chapters.iter().map(|c| c.number).max().unwrap_or(0))
            .unwrap_or(0);

        let next = if forward {
            cur + 1
        } else {
            cur.saturating_sub(1)
        };
        if next >= 1 && next <= vol_max {
            self.open_chapter(next);
            return;
        }

        let mut vols: Vec<u32> = active.project.volumes.iter().map(|v| v.number).collect();
        vols.sort_unstable();
        let adj = vols.iter().position(|&n| n == vol).and_then(|i| {
            if forward {
                vols.get(i + 1).copied()
            } else {
                i.checked_sub(1).and_then(|j| vols.get(j).copied())
            }
        });
        let target = adj.and_then(|adj_vol| {
            let vol = active
                .project
                .volumes
                .iter()
                .find(|v| v.number == adj_vol)?;
            let nums = vol.chapters.iter().map(|c| c.number);
            let ch = if forward { nums.min() } else { nums.max() }?;
            Some((adj_vol, ch))
        });

        match target {
            Some((adj_vol, ch)) => {
                self.set_active_volume(adj_vol);
                self.open_chapter(ch);
                self.toast = Some(Toast::info(format!("Vol.{adj_vol:02} · chapter {ch}")));
            }
            None => self.toast = Some(Toast::info("no more chapters")),
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
                "a run is in progress — press t to add chapters to the queue",
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
        let Some(clients) = self.ensure_active_clients() else {
            // None means either no key (the common case) or a key that failed to
            // build a client — surface the real error in the latter case rather than
            // misleadingly telling the user to add a key they already have.
            self.toast = Some(if !crate::config::any_provider_key(&self.cfg) {
                Toast::warn("no API key — open Settings ( : → Settings ) to add one")
            } else {
                match crate::build_clients(&self.cfg) {
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
            clients,
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

    /// Whole-volume translate (Shift-T): queue the volume's not-yet-done prose
    /// chapters using the same disk-aware rule as the project plan, so a chapter
    /// whose translated file is partial (even one predating the chunks-total
    /// marker, which scans as Done) is re-queued and resumes at its first gap.
    fn request_volume_translation(&mut self, vol: u32) {
        if self.run_active {
            self.toast = Some(Toast::warn(
                "a run is in progress — press t to add chapters to the queue",
            ));
            return;
        }
        let Some(active) = self.active.as_ref() else {
            self.toast = Some(Toast::warn("no project open"));
            return;
        };
        let ws = Workspace::new(active.project.dir.clone(), vol);
        let chapters: Vec<u32> = active
            .project
            .volumes
            .iter()
            .find(|v| v.number == vol)
            .map(|v| {
                v.chapters
                    .iter()
                    .filter(|c| {
                        project::translatable(c)
                            || (matches!(c.kind, crate::model::ChapterKind::Prose)
                                && !chapter_complete_on_disk(&ws, c.number, &self.cfg))
                    })
                    .map(|c| c.number)
                    .collect()
            })
            .unwrap_or_default();
        if chapters.is_empty() {
            self.toast = Some(Toast::info(format!(
                "Vol.{vol:02} already fully translated"
            )));
            return;
        }
        self.overlay = Overlay::confirm(
            "Translate whole volume?",
            format!("Queue {} chapter(s) in Vol.{:02}.", chapters.len(), vol),
            Action::StartTranslation { chapters },
        );
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
        let Some(clients) = self.ensure_active_clients() else {
            self.toast = Some(if !crate::config::any_provider_key(&self.cfg) {
                Toast::warn("no API key — open Settings ( : → Settings ) to add one")
            } else {
                match crate::build_clients(&self.cfg) {
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
            clients,
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
                    ch.skipped_chunks = 0;
                    ch.last_run = None;
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn start_import(
        &mut self,
        source: PathBuf,
        title: String,
        title_th: String,
        vol: u32,
        synopsis_raw: String,
        synopsis_th: String,
        append: bool,
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
        let verb = if append {
            "adding chapters to"
        } else {
            "importing"
        };
        self.toast = Some(Toast::info(format!("{verb} {slug} …")));
        tokio::spawn(async move {
            match run_import(
                source,
                dest,
                title,
                title_th,
                vol,
                models,
                synopsis_raw,
                synopsis_th,
                append,
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
        // Exports should use the translated title when available.
        let title = if project.title_th.trim().is_empty() {
            project.title.clone()
        } else {
            project.title_th.clone()
        };
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

    /// Client/model pair for one-off editor translations; failures notify the editor.
    fn editor_translator(&mut self) -> Option<(Arc<dyn LlmClient>, crate::model::AgentModel)> {
        let model = self
            .active
            .as_ref()
            .map(|a| a.models.translator.clone())
            .unwrap_or_else(|| self.cfg.models.translator.clone());
        let clients = match self.ensure_active_clients() {
            Some(c) => c,
            None => match crate::build_clients(&self.cfg) {
                Ok(c) => c,
                Err(e) => {
                    self.tx
                        .send(AppEvent::SynopsisFailed { msg: e.to_string() });
                    return None;
                }
            },
        };
        let Some(client) = clients.for_agent(&model) else {
            self.tx.send(AppEvent::SynopsisFailed {
                msg: format!("no API key for {}", model.provider.label()),
            });
            return None;
        };
        Some((client, model))
    }

    fn translate_synopsis(&mut self, raw: String, attempt: u32) {
        let Some((client, model)) = self.editor_translator() else {
            return;
        };
        let temperature = crate::agents::synopsis::reroll_temperature(attempt);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            match crate::agents::synopsis::translate_synopsis(
                client.as_ref(),
                &model.model,
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

    fn translate_project_title(&mut self, raw: String, attempt: u32) {
        let Some((client, model)) = self.editor_translator() else {
            return;
        };
        let temperature = crate::agents::synopsis::reroll_temperature(attempt);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            match crate::agents::synopsis::translate_title(
                client.as_ref(),
                &model.model,
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

    /// Persist edited Settings and close. Key fields are `None` when the
    /// environment supplies that key, `Some("")` clears the saved key, and
    /// `Some(k)` sets it. After a key change, the active project's clients are
    /// rebuilt so translation works at once (or stops, if the key was cleared).
    // Args mirror the `Action::SaveSettings` payload one-to-one (config fields the
    // Settings overlay edits); bundling them into a struct would only add ceremony.
    #[allow(clippy::too_many_arguments)]
    fn save_settings(
        &mut self,
        models: crate::model::ModelSet,
        openrouter_key: Option<String>,
        tokenrouter_key: Option<String>,
        google_key: Option<String>,
        cloudflare_account_id: Option<String>,
        cloudflare_api_token: Option<String>,
        update_mode: crate::model::UpdateMode,
        release_channel: crate::model::ReleaseChannel,
        service_tier: Option<crate::model::ServiceTier>,
        max_attempts: u32,
        loop_stall_secs: u64,
        max_chapter_retranslates: u32,
    ) {
        let models_changed = self.cfg.models != models;
        self.cfg.models = models.clone();
        self.cfg.update_mode = update_mode;
        let channel_changed = self.cfg.release_channel != release_channel;
        self.cfg.release_channel = release_channel;
        let tier_changed = self.cfg.service_tier != service_tier;
        self.cfg.service_tier = service_tier;
        self.cfg.max_attempts = max_attempts;
        self.cfg.loop_stall_secs = loop_stall_secs;
        self.cfg.max_chapter_retranslates = max_chapter_retranslates;
        let mut keys_changed = false;
        if let Some(k) = openrouter_key {
            let k = k.trim();
            let next = (!k.is_empty()).then(|| k.to_string());
            keys_changed |= next != self.cfg.api_key;
            self.cfg.api_key = next;
        }
        if let Some(k) = tokenrouter_key {
            let k = k.trim();
            let next = (!k.is_empty()).then(|| k.to_string());
            keys_changed |= next != self.cfg.tokenrouter_api_key;
            self.cfg.tokenrouter_api_key = next;
        }
        if let Some(k) = google_key {
            let k = k.trim();
            let next = (!k.is_empty()).then(|| k.to_string());
            keys_changed |= next != self.cfg.google_api_key;
            self.cfg.google_api_key = next;
        }
        if let Some(id) = cloudflare_account_id {
            let id = id.trim();
            let next = (!id.is_empty()).then(|| id.to_string());
            keys_changed |= next != self.cfg.cloudflare_account_id;
            self.cfg.cloudflare_account_id = next;
        }
        if let Some(token) = cloudflare_api_token {
            let token = token.trim();
            let next = (!token.is_empty()).then(|| token.to_string());
            keys_changed |= next != self.cfg.cloudflare_api_token;
            self.cfg.cloudflare_api_token = next;
        }
        // Propagate the working model set to the active project so an in-flight
        // session's next chapter / refine turn uses the new selection.
        if let Some(active) = self.active.as_mut() {
            active.models = models;
        }
        // Rebuild the active clients so changed keys, providers, or service tier
        // (snapshotted into ClientConfig) take hold without reopening.
        if (keys_changed || tier_changed || models_changed)
            && let Some(active) = self.active.as_mut()
        {
            active.clients = crate::build_clients(&self.cfg).ok();
        }
        match crate::config::save(&self.cfg) {
            Ok(()) => self.toast = Some(Toast::info("settings saved")),
            Err(e) => self.toast = Some(Toast::error(format!("save failed: {e}"))),
        }
        if tier_changed {
            self.push_log(
                LogLevel::Info,
                format!(
                    "service tier → {} ({})",
                    crate::model::ServiceTier::label(self.cfg.service_tier),
                    crate::model::ServiceTier::desc(self.cfg.service_tier)
                ),
            );
        }
        // A channel switch should take effect now, not at the next launch: kick
        // off the same background update pass that runs at startup.
        if channel_changed {
            self.push_log(
                LogLevel::Info,
                format!(
                    "update channel → {}; checking for an update",
                    self.cfg.release_channel.label()
                ),
            );
            crate::update::spawn_background_update(
                self.tx.clone(),
                self.cfg.update_mode,
                self.cfg.release_channel,
            );
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
        let key_present = crate::config::any_provider_key(&self.cfg);
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

    /// Persist title edits without changing the project slug.
    fn save_project_title(&mut self, id: String, raw: String, th: String) {
        self.overlay = Overlay::None;
        let raw = raw.trim().to_string();
        if raw.is_empty() {
            self.toast = Some(Toast::warn("empty name ignored"));
            return;
        }
        let Some(dir) = self
            .projects
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.dir.clone())
        else {
            self.toast = Some(Toast::warn(format!("project “{id}” not found")));
            return;
        };
        let ws = Workspace::new(dir, 1);
        match crate::workspace::scaffold::set_title(&ws, &raw, &th) {
            Ok(()) => {
                if let Some(p) = self.projects.iter_mut().find(|p| p.id == id) {
                    p.title = raw.clone();
                    p.title_th = th.trim().to_string();
                }
                if let Some(active) = self.active.as_mut()
                    && active.project.id == id
                {
                    active.project.title = raw;
                    active.project.title_th = th.trim().to_string();
                }
                self.toast = Some(Toast::info("project name saved"));
            }
            Err(e) => self.toast = Some(Toast::error(format!("rename failed: {e}"))),
        }
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

    /// Seed the editor from raw Thai so display-decomposed SARA AM is never persisted.
    fn open_reader_edit(&mut self, chapter: u32, chunk: u32) {
        let Some(active) = self.active.as_ref() else {
            self.toast = Some(Toast::warn("no project open"));
            return;
        };
        let raw = std::fs::read_to_string(active.workspace.translated(chapter)).unwrap_or_default();
        match crate::workspace::translation::chunk_prose_in(&raw, chunk) {
            Some(prose) => self.overlay = Overlay::reader_edit(chapter, chunk, prose),
            None => self.toast = Some(Toast::warn("couldn't load that chunk to edit")),
        }
    }

    /// Save edited prose, clear any review flag, and refresh Reader/project status.
    fn save_reader_edit(&mut self, chapter: u32, chunk: u32, text: String) {
        self.overlay = Overlay::None;
        let Some(active) = self.active.as_ref() else {
            self.toast = Some(Toast::warn("no project open"));
            return;
        };
        let workspace = active.workspace.clone();
        let path = workspace.translated(chapter);
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let Some(updated) =
            crate::workspace::translation::replace_chunk_body(&existing, chunk, text.trim_end())
        else {
            self.toast = Some(Toast::warn("couldn't locate that chunk to save"));
            return;
        };
        match crate::workspace::data_block::atomic_write(&path, &updated) {
            Ok(()) => {
                self.reader.reload_if_showing(&workspace, chapter);
                self.refresh_active_project();
                self.toast = Some(Toast::info(format!(
                    "saved edit · ch {chapter:03} · chunk {}",
                    chunk + 1
                )));
            }
            Err(e) => self.toast = Some(Toast::error(format!("edit save failed: {e}"))),
        }
    }

    fn refresh_projects(&mut self) {
        self.projects = crate::workspace::scan::scan_projects(&working_root());
        self.shelf.rescan(&working_root());
    }

    /// Re-scan only the active project from disk (after a refine edit changed a
    /// chapter), updating its in-memory status without disturbing the volume/client.
    fn refresh_active_project(&mut self) {
        let Some(dir) = self.active.as_ref().map(|a| a.project.dir.clone()) else {
            return;
        };
        let Some(fresh) = crate::workspace::scan::scan_one_project(&dir) else {
            return;
        };
        if let Some(slot) = self.projects.iter_mut().find(|p| p.dir == dir) {
            *slot = fresh.clone();
        }
        if let Some(active) = self.active.as_mut() {
            active.project = fresh;
        }
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
        let show_toast = self.toast.is_some() || self.quit_armed();
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
        chrome::render_header(
            f,
            sk.header,
            &crumb,
            &tally,
            (self.remote_state, self.remote_watchers),
            &self.theme,
        );

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
            if self.quit_armed() {
                self.render_notice(
                    f,
                    sk.toast,
                    LogLevel::Warn,
                    "press Ctrl-C again to quit",
                    "",
                );
            } else if let Some(t) = self.toast.as_ref() {
                self.render_notice(f, sk.toast, t.level, &t.msg, "⌫ dismiss ");
            }
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
                .render(f, area, &self.theme, &self.cfg, &self.log, self.frame);
        }
    }

    fn render_body(&mut self, f: &mut Frame, body: Rect) {
        match self.screen {
            Screen::Shelf => self.shelf.render(f, body, &self.projects, &self.theme),
            Screen::Project => self
                .project
                .render(f, body, self.active.as_ref(), &self.theme),
            Screen::Translate => {
                self.translate
                    .render(f, body, self.frame, &self.theme, self.cfg.service_tier)
            }
            Screen::Reader => self.reader.render(f, body, &self.theme),
            Screen::Lexicon => self.lexicon.render(
                f,
                body,
                self.active.as_ref().map(|a| &a.workspace),
                &self.theme,
            ),
            Screen::Refine => {
                self.refine
                    .render(f, body, self.frame, self.active.is_some(), &self.theme)
            }
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

    /// Render a toast or quit prompt above the footer.
    fn render_notice(&self, f: &mut Frame, area: Rect, level: LogLevel, msg: &str, hint: &str) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let (glyph, color) = match level {
            LogLevel::Trace => ("·", self.theme.ink_faint),
            LogLevel::Info => ("✓", self.theme.status_done),
            LogLevel::Warn => ("!", self.theme.status_warn),
            LogLevel::Error => ("✗", self.theme.status_failed),
        };
        let body = truncate_cols(
            &thai_display_safe(msg),
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
        if hint.is_empty() {
            return;
        }
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
            Screen::Refine => self.refine.hints(),
        }
    }
}

/// Title from the first user message, with the scope hint stripped.
/// Render a refine session's transcript as readable markdown for `/export`.
fn refine_session_to_markdown(s: &crate::workspace::refine_session::RefineSession) -> String {
    use crate::llm::Role;
    let title = if s.title.trim().is_empty() {
        s.id.as_str()
    } else {
        s.title.as_str()
    };
    let mut out = format!(
        "# Refine — {title}\n\n*model: {} · {} · {} messages*\n\n",
        s.model,
        s.updated.format("%Y-%m-%d %H:%M"),
        s.messages.len()
    );
    for m in &s.messages {
        let body = m.content.as_deref().unwrap_or("").trim();
        match m.role {
            Role::User => {
                out.push_str("## 🧑 User\n\n");
                out.push_str(body);
                out.push_str("\n\n");
            }
            Role::Assistant => {
                out.push_str("## 🤖 Assistant\n\n");
                if !body.is_empty() {
                    out.push_str(body);
                    out.push_str("\n\n");
                }
                for tc in m.tool_calls.iter().flatten() {
                    out.push_str(&format!("- 🔧 `{}`\n", tc.function.name));
                }
                if m.tool_calls.is_some() {
                    out.push('\n');
                }
            }
            Role::Tool => {
                let clipped: String = body.chars().take(400).collect();
                out.push_str("> ");
                out.push_str(&clipped.replace('\n', "\n> "));
                out.push_str("\n\n");
            }
            Role::System => {}
        }
    }
    out
}

fn refine_session_title(messages: &[crate::llm::Message]) -> String {
    for m in messages {
        if matches!(m.role, crate::llm::Role::User)
            && let Some(content) = &m.content
        {
            let head = content
                .split_once("\n\n(In scope:")
                .map(|(h, _)| h)
                .unwrap_or(content);
            let line = head.lines().next().unwrap_or("").trim();
            if !line.is_empty() {
                return line.chars().take(48).collect();
            }
        }
    }
    "(untitled)".to_string()
}

// Wire strings are relay protocol values, not display copy.
fn remote_chapter_status(s: ChapterStatus) -> &'static str {
    match s {
        ChapterStatus::Pending => "pending",
        ChapterStatus::Chunking => "chunking",
        ChapterStatus::Translating => "translating",
        ChapterStatus::Reviewing => "reviewing",
        ChapterStatus::Appended => "appended",
        ChapterStatus::Done => "done",
        ChapterStatus::NeedsReview => "needs_review",
        ChapterStatus::Failed => "failed",
        ChapterStatus::Paused => "paused",
        ChapterStatus::Partial => "partial",
    }
}

fn remote_chunk_state(s: crate::model::ChunkState) -> &'static str {
    use crate::model::ChunkState as C;
    match s {
        C::Queued => "queued",
        C::Translating => "translating",
        C::Reviewing => "reviewing",
        C::Rejected => "rejected",
        C::Approved => "approved",
        C::Committed => "committed",
        C::NeedsReview => "needs_review",
    }
}

fn remote_agent_role(r: crate::model::AgentRole) -> &'static str {
    use crate::model::AgentRole as R;
    match r {
        R::Orchestrator => "orchestrator",
        R::Translator => "translator",
        R::Reviewer => "reviewer",
    }
}

fn remote_term_policy(p: crate::model::TermPolicy) -> &'static str {
    use crate::model::TermPolicy as P;
    match p {
        P::HardLocked => "hard_locked",
        P::Preferred => "preferred",
        P::Forbidden => "forbidden",
        P::ContextDependent => "context_dependent",
    }
}

fn remote_log_level(l: LogLevel) -> &'static str {
    match l {
        LogLevel::Trace => "trace",
        LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error => "error",
    }
}

/// Whole-project recovery progress. Completeness comes from chunk markers on disk,
/// not scanned status, because partial translated files can scan as done.
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

/// True when every source chunk has a translated marker on disk. This mirrors the
/// pipeline's resume accounting and avoids trusting partial translated files.
fn chapter_complete_on_disk(ws: &Workspace, chapter: u32, cfg: &AppConfig) -> bool {
    let raw = std::fs::read_to_string(ws.raw(chapter)).unwrap_or_default();
    if raw.trim().is_empty() {
        return true;
    }
    let translated = std::fs::read_to_string(ws.translated(chapter)).unwrap_or_default();
    // Image-only chapters are written directly to translated/ during import.
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

/// The working root we scan for projects / importable source files. Falls back to `.`.
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
        ChapterStatus::Partial => "partial",
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

/// Import driver: scaffold the tree, convert the selected source to Markdown,
/// write raw/ (+ translated/ for image-only), emit ImportProgress. Returns the slug.
#[allow(clippy::too_many_arguments)]
async fn run_import(
    source: PathBuf,
    dest: PathBuf,
    title: String,
    title_th: String,
    vol: u32,
    models: ModelSet,
    synopsis_raw: String,
    synopsis_th: String,
    append: bool,
    tx: &EventTx,
) -> anyhow::Result<String> {
    if !crate::document_import::is_supported_import_path(&source) {
        anyhow::bail!("unsupported import source: {}", source.display());
    }

    let slug = dest
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| slugify(&title));

    // Append mode targets an existing project/volume, so scaffolding and the
    // title/synopsis/source-metadata writes are skipped — only new chapters land.
    if !append {
        // Scaffold on a blocking thread so the fs work never stalls the runtime.
        let dest = dest.clone();
        let title = title.clone();
        let models = models.clone();
        tokio::task::spawn_blocking(move || {
            crate::workspace::scaffold::create_project(&dest, &title, &models, vol)?;
            let ws = Workspace::new(dest.clone(), vol);
            // Persist the wizard's Thai title / volume synopsis (if any); empty
            // means skipped, and never clobbers an existing merge target's title.
            if !title_th.trim().is_empty() {
                crate::workspace::scaffold::set_title(&ws, &title, &title_th)?;
            }
            if !synopsis_raw.trim().is_empty() || !synopsis_th.trim().is_empty() {
                crate::workspace::volume::set_synopsis(&ws, &synopsis_raw, &synopsis_th)?;
            }
            Ok::<(), std::io::Error>(())
        })
        .await??;
    }

    if crate::document_import::is_epub_path(&source) {
        run_epub_import(source, dest.clone(), vol, append, tx).await?;
    } else {
        run_markitdown_import(source, dest.clone(), title, vol, append, tx).await?;
    }

    Ok(slug)
}

async fn run_epub_import(
    epub: PathBuf,
    dest: PathBuf,
    vol: u32,
    append: bool,
    tx: &EventTx,
) -> anyhow::Result<()> {
    let prepared = prepare_epub_import(
        epub.clone(),
        dest.join(".epub_work"),
        dest.join("images"),
        vol,
        tx,
    )
    .await?;
    let ws = Workspace::new(dest.clone(), vol);
    let base = if append {
        crate::workspace::translation::max_chapter_number(&ws)
    } else {
        crate::workspace::volume::set_source_metadata(
            &ws,
            epub_source_metadata(&prepared.metadata, &epub),
        )?;
        0
    };

    for (idx, chapter) in prepared.chapters.iter().enumerate() {
        write_import_chapter(
            &ws,
            base + (idx + 1) as u32,
            &chapter.body,
            chapter.image_only,
        )?;
    }

    Ok(())
}

struct PreparedEpubImport {
    metadata: crate::epub::Metadata,
    image_count: usize,
    chapters: Vec<PreparedImportChapter>,
}

struct PreparedImportChapter {
    body: String,
    image_only: bool,
}

async fn prepare_epub_import(
    epub: PathBuf,
    work_dir: PathBuf,
    images_dir: PathBuf,
    vol: u32,
    tx: &EventTx,
) -> anyhow::Result<PreparedEpubImport> {
    use crate::epub::import::import_with_media_prefixed;
    use crate::epub::paths::{dir_of, resolve_href};

    tx.send(AppEvent::ImportProgress {
        done: 0,
        total: 0,
        label: "extracting epub".to_string(),
    });

    let image_prefix = volume_image_prefix(vol);
    let (book, media) = {
        let epub = epub.clone();
        let work_dir = work_dir.clone();
        let images_dir = images_dir.clone();
        tokio::task::spawn_blocking(move || {
            import_with_media_prefixed(&epub, &work_dir, &images_dir, "images", &image_prefix)
        })
        .await?
        .map_err(|e| anyhow::anyhow!("epub: {e}"))?
    };

    let metadata = book.metadata.clone();
    let image_count = media.written.len();
    let doc_paths: Vec<String> = book
        .reading_order_paths()
        .into_iter()
        .map(|s| s.to_string())
        .collect();
    let total = doc_paths.len();

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
            toc_title: toc_titles
                .get(archive_path)
                .cloned()
                .or_else(|| crate::cleanse::leading_image_title(&html)),
            internal_link_count: count_internal_xhtml_links(&html),
            body_class: body_class(&html),
        });

        tx.send(AppEvent::ImportProgress {
            done: i + 1,
            total,
            label: format!("cleansing {}/{}", i + 1, total),
        });
        tokio::task::yield_now().await;
    }

    let chapters = crate::epub::segment::segment(&docs);
    let chapters = chapters
        .iter()
        .map(|lc| match lc.kind {
            crate::epub::segment::LogicalKind::ImageOnly => PreparedImportChapter {
                body: lc.body.clone(),
                image_only: true,
            },
            crate::epub::segment::LogicalKind::Prose => PreparedImportChapter {
                body: match &lc.title {
                    Some(t) => format!("# {t}\n\n{}", lc.body),
                    None => lc.body.clone(),
                },
                image_only: false,
            },
        })
        .collect();

    Ok(PreparedEpubImport {
        metadata,
        image_count,
        chapters,
    })
}

#[derive(Default)]
struct RefreshImageReport {
    images: usize,
    raw_files: usize,
    translated_files: usize,
}

async fn run_volume_image_refresh(
    source: PathBuf,
    dest: PathBuf,
    vol: u32,
    tx: &EventTx,
) -> anyhow::Result<RefreshImageReport> {
    let work_dir = dest
        .join(".epub_image_refresh")
        .join(format!("Vol_{vol:02}"));
    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    let prepared =
        prepare_epub_import(source.clone(), work_dir, dest.join("images"), vol, tx).await?;
    let ws = Workspace::new(dest, vol);
    crate::workspace::volume::set_source_metadata(
        &ws,
        epub_source_metadata(&prepared.metadata, &source),
    )?;

    let mut report = RefreshImageReport {
        images: prepared.image_count,
        ..Default::default()
    };
    let total = prepared.chapters.len();
    for (idx, chapter) in prepared.chapters.iter().enumerate() {
        let ch = (idx + 1) as u32;
        let images = markdown_image_basenames(&chapter.body);
        if !images.is_empty() {
            if rewrite_markdown_image_file(&ws.raw(ch), &images)? {
                report.raw_files += 1;
            }
            if rewrite_markdown_image_file(&ws.translated(ch), &images)? {
                report.translated_files += 1;
            }
        }
        tx.send(AppEvent::ImportProgress {
            done: idx + 1,
            total,
            label: format!("rewriting image links {}/{}", idx + 1, total),
        });
        tokio::task::yield_now().await;
    }

    Ok(report)
}

async fn run_markitdown_import(
    source: PathBuf,
    dest: PathBuf,
    title: String,
    vol: u32,
    append: bool,
    tx: &EventTx,
) -> anyhow::Result<()> {
    let source_name = source
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("source")
        .to_string();
    tx.send(AppEvent::ImportProgress {
        done: 0,
        total: 0,
        label: format!("converting {source_name}"),
    });

    let progress_tx = tx.clone();
    let converted = tokio::task::spawn_blocking(move || {
        crate::document_import::convert_markitdown_path(&source, &title, move |progress| {
            progress_tx.send(AppEvent::ImportProgress {
                done: 0,
                total: 0,
                label: format!("markitdown · {progress}"),
            });
        })
    })
    .await??;

    if converted.degraded {
        tx.send(AppEvent::Log {
            level: LogLevel::Warn,
            msg: format!("{source_name}: MarkItDown reported a degraded conversion"),
        });
    }

    let ws = Workspace::new(dest, vol);
    let base = if append {
        crate::workspace::translation::max_chapter_number(&ws)
    } else {
        0
    };
    let total = converted.chapters.len();
    if total == 0 {
        anyhow::bail!("MarkItDown produced no chapters");
    }
    for (idx, chapter) in converted.chapters.iter().enumerate() {
        let ch_number = base + (idx + 1) as u32;
        write_import_chapter(&ws, ch_number, &chapter.body, chapter.image_only)?;
        tx.send(AppEvent::ImportProgress {
            done: idx + 1,
            total,
            label: format!("writing chapter {}/{}", idx + 1, total),
        });
        tokio::task::yield_now().await;
    }

    Ok(())
}

fn write_import_chapter(
    ws: &Workspace,
    chapter: u32,
    body: &str,
    image_only: bool,
) -> std::io::Result<()> {
    crate::workspace::translation::write_raw(ws, chapter, body)?;
    if image_only {
        crate::workspace::translation::write_image_only(ws, chapter, body)?;
    }
    Ok(())
}

const SOURCE_FILE_METADATA_KEY: &str = "Source file";

fn epub_source_metadata(
    metadata: &crate::epub::Metadata,
    source: &Path,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    out.insert(
        SOURCE_FILE_METADATA_KEY.to_string(),
        source_file_display(source),
    );
    insert_metadata(&mut out, "Title", metadata.title.as_deref());
    if metadata.authors.is_empty() {
        insert_metadata(&mut out, "Authors", metadata.creator.as_deref());
    } else {
        out.insert("Authors".to_string(), metadata.authors.join(", "));
    }
    insert_metadata(&mut out, "Language", metadata.language.as_deref());
    insert_metadata(&mut out, "Publisher", metadata.publisher.as_deref());
    insert_metadata(&mut out, "Date", metadata.date.as_deref());
    insert_metadata(&mut out, "Description", metadata.description.as_deref());
    insert_metadata(&mut out, "Identifier", metadata.identifier.as_deref());
    insert_metadata(
        &mut out,
        "Cover image",
        metadata.cover_image_path.as_deref(),
    );
    out
}

fn source_file_display(source: &Path) -> String {
    std::fs::canonicalize(source)
        .unwrap_or_else(|_| source.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn volume_source_file(ws: &Workspace) -> Option<PathBuf> {
    let data = crate::workspace::volume::load(ws);
    data.source_metadata
        .get(SOURCE_FILE_METADATA_KEY)
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                ws.root.join(path)
            }
        })
}

fn epub_source_files() -> Vec<(PathBuf, u64)> {
    crate::workspace::scan::find_unimported_epubs(&working_root())
}

fn volume_image_prefix(vol: u32) -> String {
    format!("vol{vol}_")
}

fn rewrite_markdown_image_file(path: &Path, image_names: &[String]) -> std::io::Result<bool> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let (updated, rewrites) = rewrite_markdown_image_links(&text, image_names);
    if rewrites == 0 || updated == text {
        return Ok(false);
    }
    std::fs::write(path, updated)?;
    Ok(true)
}

fn markdown_image_basenames(markdown: &str) -> Vec<String> {
    let mut names = Vec::new();
    for target in markdown_image_targets(markdown) {
        if let Some(name) = image_target_basename(target) {
            names.push(name.to_string());
        }
    }
    names
}

fn rewrite_markdown_image_links(markdown: &str, image_names: &[String]) -> (String, usize) {
    let mut out = String::with_capacity(markdown.len());
    let mut cursor = 0;
    let mut search = 0;
    let mut image_idx = 0;
    let mut rewrites = 0;

    while let Some(rel_start) = markdown[search..].find("![") {
        let start = search + rel_start;
        let Some(label_end_rel) = markdown[start + 2..].find("](") else {
            break;
        };
        let target_start = start + 2 + label_end_rel + 2;
        let Some(target_end_rel) = markdown[target_start..].find(')') else {
            break;
        };
        let target_end = target_start + target_end_rel;
        let target = &markdown[target_start..target_end];
        if image_target_basename(target).is_some() {
            if let Some(new_name) = image_names.get(image_idx) {
                out.push_str(&markdown[cursor..target_start]);
                out.push_str(&rewrite_image_target(target, new_name));
                cursor = target_end;
                rewrites += 1;
            }
            image_idx += 1;
        }
        search = target_end + 1;
    }

    out.push_str(&markdown[cursor..]);
    (out, rewrites)
}

fn markdown_image_targets(markdown: &str) -> Vec<&str> {
    let mut targets = Vec::new();
    let mut search = 0;
    while let Some(rel_start) = markdown[search..].find("![") {
        let start = search + rel_start;
        let Some(label_end_rel) = markdown[start + 2..].find("](") else {
            break;
        };
        let target_start = start + 2 + label_end_rel + 2;
        let Some(target_end_rel) = markdown[target_start..].find(')') else {
            break;
        };
        let target_end = target_start + target_end_rel;
        targets.push(&markdown[target_start..target_end]);
        search = target_end + 1;
    }
    targets
}

fn image_target_basename(target: &str) -> Option<&str> {
    let path = target_path_part(target);
    let lower = path.to_ascii_lowercase();
    if lower.contains("://") || (!lower.contains("/images/") && !lower.starts_with("images/")) {
        return None;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    (!name.is_empty()).then_some(name)
}

fn rewrite_image_target(target: &str, new_basename: &str) -> String {
    let path = target_path_part(target);
    let suffix = &target[path.len()..];
    let prefix = path
        .rfind('/')
        .map(|idx| &path[..=idx])
        .unwrap_or("../../images/");
    format!("{prefix}{new_basename}{suffix}")
}

fn target_path_part(target: &str) -> &str {
    let query = target.find('?');
    let fragment = target.find('#');
    let end = match (query, fragment) {
        (Some(q), Some(f)) => q.min(f),
        (Some(q), None) => q,
        (None, Some(f)) => f,
        (None, None) => target.len(),
    };
    &target[..end]
}

fn insert_metadata(out: &mut BTreeMap<String, String>, key: &str, value: Option<&str>) {
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        out.insert(key.to_string(), value.to_string());
    }
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
mod image_refresh_tests {
    use super::*;
    use crate::model::Volume;

    #[test]
    fn volume_image_prefix_matches_import_contract() {
        assert_eq!(volume_image_prefix(1), "vol1_");
        assert_eq!(volume_image_prefix(12), "vol12_");
    }

    #[test]
    fn markdown_image_links_rewrite_by_position() {
        let md = "ก่อน ![ภาพ](../../images/old.png) กลาง ![x](../../images/old_2.jpg#frag) หลัง";
        let names = vec!["vol2_a.png".to_string(), "vol2_b.jpg".to_string()];

        let (out, rewrites) = rewrite_markdown_image_links(md, &names);

        assert_eq!(rewrites, 2);
        assert_eq!(
            out,
            "ก่อน ![ภาพ](../../images/vol2_a.png) กลาง ![x](../../images/vol2_b.jpg#frag) หลัง"
        );
    }

    #[test]
    fn markdown_image_basenames_ignores_non_project_images() {
        let md =
            "![remote](https://example.com/images/image.png)\n![local](../../images/vol1_a.png)";

        assert_eq!(markdown_image_basenames(md), vec!["vol1_a.png"]);
    }

    #[test]
    fn missing_source_file_opens_image_source_picker() {
        let base =
            std::env::temp_dir().join(format!("honya_image_source_picker_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("Vol_01")).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(EventTx(tx), AppConfig::default());
        let project = Project {
            id: "old-project".to_string(),
            dir: base.clone(),
            title: "Old Project".to_string(),
            title_th: String::new(),
            created: None,
            touched: None,
            volumes: vec![Volume {
                number: 1,
                dir: base.join("Vol_01"),
                label: None,
                chapters: Vec::new(),
            }],
            models: None,
        };
        app.active = Some(ActiveProject {
            project,
            workspace: Workspace::new(base.clone(), 1),
            clients: None,
            models: ModelSet::default(),
            vol: 1,
        });

        app.refresh_volume_images(1);

        match app.overlay {
            Overlay::ImageSource(st) => assert_eq!(st.vol, 1),
            other => panic!("expected image source picker, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&base);
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

    /// Partial translated files can scan as `Done`; chunk markers decide completeness.
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

        assert!(!chapter_complete_on_disk(&ws, 1, &cfg));

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

#[cfg(test)]
mod remote_tests {
    use super::*;
    use crate::agents::pipeline::RunControl;
    use crate::model::Volume;
    use crate::remote::protocol::{RemoteCommand, RemoteDelta, RemoteOutbound};

    fn app() -> App {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(EventTx(tx), AppConfig::default())
    }

    fn linked_account() -> crate::model::RemoteAccount {
        crate::model::RemoteAccount {
            github_login: "altq".into(),
            device_id: "device".into(),
            device_token: "token".into(),
        }
    }

    fn ctrl_r() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)
    }

    #[test]
    fn ctrl_r_global_enables_remote_when_account_linked() {
        let mut app = app();
        app.cfg.account = Some(linked_account());
        app.overlay = Overlay::Help(0);

        assert!(matches!(app.route_key(ctrl_r()), Action::EnableRemote));
    }

    #[test]
    fn ctrl_r_global_disables_remote_when_already_connected() {
        let mut app = app();
        app.cfg.account = Some(linked_account());
        let (out_tx, _out_rx) = tokio::sync::mpsc::unbounded_channel();
        app.remote_out = Some(out_tx);
        app.overlay = Overlay::Log(0);

        assert!(matches!(app.route_key(ctrl_r()), Action::DisableRemote));
    }

    #[test]
    fn ctrl_r_without_account_keeps_settings_sign_in_shortcut() {
        let mut app = app();
        app.overlay = Overlay::settings_with_field(&app.cfg, 0);

        assert!(matches!(app.route_key(ctrl_r()), Action::StartRemoteLogin));
    }

    #[test]
    fn ctrl_r_expands_refine_instead_of_toggling_remote() {
        let base =
            std::env::temp_dir().join(format!("honya_refine_ctrl_r_route_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("Vol_01")).unwrap();
        let mut app = app();
        app.cfg.account = Some(linked_account());
        app.screen = Screen::Refine;
        app.active = Some(ActiveProject {
            project: Project {
                id: "novel".to_string(),
                dir: base.clone(),
                title: "Novel".to_string(),
                title_th: String::new(),
                created: None,
                touched: None,
                volumes: vec![Volume {
                    number: 1,
                    dir: base.join("Vol_01"),
                    label: None,
                    chapters: Vec::new(),
                }],
                models: None,
            },
            workspace: Workspace::new(base.clone(), 1),
            clients: None,
            models: ModelSet::default(),
            vol: 1,
        });

        assert!(matches!(app.route_key(ctrl_r()), Action::None));
        assert!(app.refine.expanded_for_test());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn remote_commands_map_to_existing_actions() {
        use RemoteCommand as C;
        assert!(matches!(
            App::map_remote_command(C::Pause),
            Action::PauseRun
        ));
        assert!(matches!(App::map_remote_command(C::Stop), Action::StopRun));
        assert!(matches!(
            App::map_remote_command(C::StartProject),
            Action::BeginProjectTranslation
        ));
        assert!(matches!(
            App::map_remote_command(C::Enqueue {
                vol: 2,
                chapters: vec![3]
            }),
            Action::EnqueueChapters { vol: 2, .. }
        ));
        assert!(matches!(
            App::map_remote_command(C::Dequeue { vol: 1, ch: 4 }),
            Action::DequeueChapter { vol: 1, ch: 4 }
        ));
        assert!(matches!(
            App::map_remote_command(C::SetVolume { vol: 3 }),
            Action::SetActiveVolume { vol: 3 }
        ));
        assert!(matches!(
            App::map_remote_command(C::OpenProject { id: "re-zero".into() }),
            Action::OpenProject(id) if id == "re-zero"
        ));
    }

    #[test]
    fn remote_pause_toggles_run_control_like_a_keystroke() {
        let mut app = app();
        let ctl = RunControl::new();
        app.run_ctl = Some(ctl.clone());
        app.run_active = true;
        assert!(!ctl.is_paused());
        app.on_app_event(AppEvent::RemoteCommand(RemoteCommand::Pause));
        assert!(ctl.is_paused(), "remote pause must toggle RunControl");
    }

    #[test]
    fn events_project_to_relay_deltas_when_connected() {
        let mut app = app();
        let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<RemoteOutbound>();
        app.remote_out = Some(out_tx);

        app.on_app_event(AppEvent::Log {
            level: LogLevel::Info,
            msg: "hello".into(),
        });

        let mut saw_log = false;
        while let Ok(msg) = out_rx.try_recv() {
            if let RemoteOutbound::Delta(RemoteDelta::Log(line)) = msg
                && line.msg == "hello"
                && line.level == "info"
            {
                saw_log = true;
            }
        }
        assert!(saw_log, "a Log event should project a Log delta");
    }

    #[test]
    fn no_projection_when_disconnected() {
        let mut app = app();
        assert!(app.remote_out.is_none());
        app.on_app_event(AppEvent::Log {
            level: LogLevel::Warn,
            msg: "x".into(),
        });
        assert!(app.log.iter().any(|(_, m)| m == "x"));
    }
}
