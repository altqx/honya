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
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::llm::client::LlmClient;
use crate::model::{
    AppConfig, AppEvent, ChapterStatus, EventTx, LogLevel, ModelSet, Project, ThemeId, UsageStats,
};
use crate::theme::Theme;
use crate::ui::chrome::{self, StatusTally};
use crate::ui::layout::{self, Skeleton};
use crate::ui::text::{thai_display_safe, truncate_cols};
use crate::workspace::Workspace;

use self::lexicon::LexiconScreen;
use self::overlay::Overlay;
use self::project::ProjectScreen;
use self::reader::ReaderScreen;
use self::shelf::ShelfScreen;
use self::translate::TranslateScreen;

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
    /// 0-based tab index, used by the tab bar and `1`-`5` digit routing.
    pub fn index(self) -> usize {
        match self {
            Screen::Shelf => 0,
            Screen::Project => 1,
            Screen::Translate => 2,
            Screen::Reader => 3,
            Screen::Lexicon => 4,
        }
    }

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
    OpenProject(String),
    OpenChapter {
        chapter: u32,
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
    },
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
    pub client: Arc<dyn LlmClient>,
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
    /// Shared pause/stop control for the in-flight pipeline run (None when idle).
    pub run_ctl: Option<crate::agents::pipeline::RunControl>,
    /// Rolling activity log shown in the Log overlay.
    pub log: Vec<(LogLevel, String)>,
    /// Set when a newer release is detected at startup (drives a footer hint).
    pub update_available: Option<String>,
    /// An interrupted run found at startup, awaiting the user's resume/discard
    /// choice in the recovery overlay (see `init_recovery_prompt`).
    pub pending_recovery: Option<crate::workspace::session::SessionCheckpoint>,
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
            reader: ReaderScreen::new(),
            lexicon: LexiconScreen::new(),
            toast: None,
            run_active: false,
            run_ctl: None,
            log: Vec::new(),
            update_available: None,
            pending_recovery: None,
        }
    }

    /// Check for an interrupted run (crash / power loss / hard kill) and, if one
    /// is resumable, raise the recovery overlay. Called once from `main` after
    /// `App::new`; kept out of `App::new` itself so test apps never touch the real
    /// recovery file. A stale checkpoint (project gone, or already fully done) is
    /// cleared quietly rather than prompted.
    pub fn init_recovery_prompt(&mut self) {
        let Some(cp) = crate::workspace::session::load() else {
            return;
        };
        if !cp.is_resumable() {
            crate::workspace::session::clear();
            return;
        }
        let (done, total) = self.recovery_progress(&cp);
        if total > 0 && done >= total {
            // The crash landed after the last chunk committed but before the run
            // was marked finished — nothing left to do, so don't nag.
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
    /// using the freshly-scanned project state when it is in the current shelf
    /// (falls back to "unknown" → 0 done when launched from another directory).
    fn recovery_progress(
        &self,
        cp: &crate::workspace::session::SessionCheckpoint,
    ) -> (usize, usize) {
        let total = cp.chapters.len();
        let done = self
            .projects
            .iter()
            .find(|p| p.dir == cp.project_dir)
            .and_then(|p| p.volumes.iter().find(|v| v.number == cp.vol))
            .map(|v| {
                cp.chapters
                    .iter()
                    .filter(|&&num| {
                        v.chapters.iter().any(|ch| {
                            ch.number == num
                                && matches!(
                                    ch.status,
                                    ChapterStatus::Done | ChapterStatus::NeedsReview
                                )
                        })
                    })
                    .count()
            })
            .unwrap_or(0);
        (done, total)
    }

    pub fn on_app_event(&mut self, ev: AppEvent) {
        // Translate screen observes everything so its live panel stays current off-tab.
        self.translate.on_app_event(&ev);

        match &ev {
            AppEvent::ChapterStarted { chapter } => {
                self.set_chapter_status(*chapter, ChapterStatus::Translating);
                self.run_active = true;
                if let Some(title) = self.chapter_title(*chapter) {
                    self.translate.set_chapter_title(title);
                }
            }
            AppEvent::ChapterQueued { chapter } => {
                self.set_chapter_status(*chapter, ChapterStatus::Pending);
            }
            AppEvent::ChapterStateChanged { chapter, state } => {
                self.set_chapter_status(*chapter, *state);
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
            } => {
                self.run_active = false;
                self.run_ctl = None;
                // The run reached its end (finished, stopped, or all-failed): the
                // recovery checkpoint has served its purpose, so retire it.
                crate::workspace::session::clear();
                self.pending_recovery = None;
                let review = if *chapters_need_review > 0 {
                    format!(" · {chapters_need_review} need review")
                } else {
                    String::new()
                };
                self.toast = Some(Toast::info(format!(
                    "run finished · {chapters_done} done · {chapters_failed} failed{review}"
                )));
                self.push_log(
                    LogLevel::Info,
                    format!(
                        "pipeline finished: {chapters_done} done, {chapters_failed} failed, {chapters_need_review} need review"
                    ),
                );
            }
            AppEvent::Error { context, msg } => {
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
            AppEvent::ImportFinished { project_id } => {
                self.run_active = false;
                self.overlay = Overlay::None;
                self.refresh_projects();
                self.toast = Some(Toast::info(format!("imported {project_id}")));
                self.push_log(LogLevel::Info, format!("imported {project_id}"));
                self.open_project(project_id.clone());
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

    fn set_chapter_status(&mut self, chapter: u32, status: ChapterStatus) {
        if let Some(active) = self.active.as_mut() {
            for vol in active.project.volumes.iter_mut() {
                for ch in vol.chapters.iter_mut() {
                    if ch.number == chapter {
                        ch.status = status;
                        return;
                    }
                }
            }
        }
    }

    fn set_chapter_chunks(&mut self, chapter: u32, total: u32, committed: Option<u32>) {
        if let Some(active) = self.active.as_mut() {
            for vol in active.project.volumes.iter_mut() {
                for ch in vol.chapters.iter_mut() {
                    if ch.number == chapter {
                        ch.total_chunks = total;
                        if let Some(c) = committed {
                            ch.committed_chunks = c;
                        } else if total > 0 {
                            ch.committed_chunks = ch.committed_chunks.min(total);
                        }
                        return;
                    }
                }
            }
        }
    }

    fn bump_committed(&mut self, chapter: u32, bytes_written: usize) {
        if bytes_written == 0 {
            return;
        }
        if let Some(active) = self.active.as_mut() {
            for vol in active.project.volumes.iter_mut() {
                for ch in vol.chapters.iter_mut() {
                    if ch.number == chapter {
                        ch.committed_chunks = ch.committed_chunks.saturating_add(1);
                        if ch.total_chunks > 0 {
                            ch.committed_chunks = ch.committed_chunks.min(ch.total_chunks);
                        }
                        return;
                    }
                }
            }
        }
    }

    /// Fold a finished chapter's run usage into its in-memory lifetime total,
    /// mirroring the VOLUME.md persistence so the Project screen stays live.
    fn add_chapter_usage(&mut self, chapter: u32, delta: &UsageStats) {
        if let Some(active) = self.active.as_mut() {
            for vol in active.project.volumes.iter_mut() {
                for ch in vol.chapters.iter_mut() {
                    if ch.number == chapter {
                        ch.usage.add(delta);
                        return;
                    }
                }
            }
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
                    Overlay::Settings(_) => Overlay::settings(&self.cfg),
                    Overlay::Theme(_) => Overlay::theme(self.cfg.theme),
                    // QA placeholders (palette / screen `Q`) carry no data; rebuild
                    // the report from the live active project on show.
                    Overlay::Qa(_) => self.build_qa_overlay(),
                    other => other,
                };
            }
            Action::CloseOverlay => {
                self.overlay = Overlay::None;
            }
            Action::OpenProject(id) => {
                self.open_project(id);
            }
            Action::OpenChapter { chapter } => {
                self.open_chapter(chapter);
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
            Action::StartTranslation { chapters } => {
                self.request_translation(chapters);
            }
            Action::ContinueTranslation { chapters } => {
                self.begin_translation(chapters, false);
            }
            Action::RestartTranslation { chapters } => {
                self.begin_translation(chapters, true);
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
                crate::workspace::session::clear();
                self.pending_recovery = None;
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
            } => {
                self.cfg.base_url = base_url;
                self.cfg.models.orchestrator = orchestrator;
                self.cfg.models.translator = translator;
                self.cfg.models.reviewer = reviewer;
                match crate::config::save(&self.cfg) {
                    Ok(()) => self.toast = Some(Toast::info("settings saved")),
                    Err(e) => self.toast = Some(Toast::error(format!("save failed: {e}"))),
                }
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

    /// Make `project` the active project on volume `vol`: build its LLM client,
    /// reset the per-project screens, and land on the Project tab. Returns `false`
    /// (after toasting) when the client can't be built, so callers can bail.
    fn activate_project(&mut self, project: Project, vol: u32) -> bool {
        let models = project
            .models
            .clone()
            .unwrap_or_else(|| self.cfg.models.clone());
        let workspace = Workspace::new(project.dir.clone(), vol);
        let client = match crate::build_client(&self.cfg) {
            Ok(client) => client,
            Err(e) => {
                self.toast = Some(Toast::error(format!("LLM client unavailable: {e}")));
                return false;
            }
        };
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
        self.toast = Some(Toast::info(format!("opened {id}")));
        true
    }

    /// Resume the interrupted run from the recovery checkpoint: reopen its project
    /// (by absolute path, so a different launch directory still works) and continue
    /// translating its chapter queue. The pipeline's own chunk-level resume then
    /// skips everything already committed to disk.
    fn resume_session(&mut self) {
        let Some(cp) = self.pending_recovery.take() else {
            self.toast = Some(Toast::warn("no run to resume"));
            return;
        };
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
        // Continue (not restart): keep committed chunks, fill the gaps.
        self.begin_translation(cp.chapters, false);
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

    /// A chapter's display title from the active project (for the live Translate header).
    fn chapter_title(&self, chapter: u32) -> Option<String> {
        let active = self.active.as_ref()?;
        active
            .project
            .volumes
            .iter()
            .flat_map(|v| v.chapters.iter())
            .find(|c| c.number == chapter)
            .map(|c| c.title.clone())
    }

    fn request_translation(&mut self, chapters: Vec<u32>) {
        if self.run_active {
            self.toast = Some(Toast::warn("a run is already in progress"));
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
        if self.run_active {
            self.toast = Some(Toast::warn("a run is already in progress"));
            return;
        }
        let Some((vol, project_dir, project_id, project_title, client, models)) =
            self.active.as_ref().map(|active| {
                (
                    active.active_vol(),
                    active.project.dir.clone(),
                    active.project.id.clone(),
                    active.project.title.clone(),
                    Arc::clone(&active.client),
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

        let ws = Workspace::new(project_dir.clone(), vol);
        if restart {
            for chapter in &chapters {
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
        let checkpoint = crate::workspace::session::SessionCheckpoint::new(
            project_dir,
            project_id,
            project_title,
            vol,
            chapters.clone(),
        );
        if let Err(e) = crate::workspace::session::save(&checkpoint) {
            self.push_log(
                LogLevel::Warn,
                format!("could not write recovery checkpoint: {e}"),
            );
        }

        let ctl = crate::agents::pipeline::RunControl::new();
        let ctx = crate::agents::pipeline::PipelineCtx {
            client,
            ws,
            models,
            cfg: self.cfg.clone(),
            tx: self.tx.clone(),
            ctl: ctl.clone(),
        };
        self.run_ctl = Some(ctl);
        let chapters_for_task = chapters.clone();
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

    fn chapters_with_translation_progress(&self, chapters: &[u32]) -> Vec<u32> {
        let Some(active) = self.active.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for chapter in chapters {
            if let Some(ch) = active
                .project
                .volumes
                .iter()
                .flat_map(|v| v.chapters.iter())
                .find(|ch| ch.number == *chapter)
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
                Ok(project_id) => tx.send(AppEvent::ImportFinished { project_id }),
                Err(e) => tx.send(AppEvent::Error {
                    context: "import".to_string(),
                    msg: e.to_string(),
                }),
            }
        });
    }

    /// Spawn a background Translator round-trip for a volume synopsis. Uses the
    /// active project's client when one is open, else builds from config (the
    /// import wizard runs from the Shelf with no project open yet).
    fn translate_synopsis(&mut self, raw: String, attempt: u32) {
        let (client, model) = match self.active.as_ref() {
            Some(a) => (Arc::clone(&a.client), a.models.translator.clone()),
            None => match crate::build_client(&self.cfg) {
                Ok(c) => (c, self.cfg.models.translator.clone()),
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

    pub fn render(&mut self, f: &mut Frame) {
        let area = f.area();
        let show_toast = self.toast.is_some();
        let sk: Skeleton = layout::skeleton(area, show_toast);

        f.render_widget(
            Paragraph::new("").style(Style::default().bg(self.theme.bg)),
            area,
        );

        let crumb = self.crumb();
        let tally = self.tally();
        chrome::render_header(f, sk.header, &crumb, &tally, &self.theme);

        chrome::render_tabbar(
            f,
            sk.tabs,
            self.screen,
            self.run_active,
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
                    .first()
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
    format!(
        "honya didn't shut down cleanly during a translation run — «{}» Vol.{:02} · {progress}. Resume picks up from the last committed chunk (finished chunks are skipped, so no tokens are re-spent). Discard forgets this run; Esc keeps it for next launch.",
        cp.project_title, cp.vol
    )
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

/// Filesystem-safe slug: ASCII lowered, punctuation runs → single `-`; non-ASCII (CJK/Thai) preserved verbatim.
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
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "project".to_string()
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

/// Cheap, dependency-free scan for raw `<img>`/SVG `<image>` src strings (heavy parsing is cleanse's job).
fn collect_img_srcs(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = html.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Handle both <img src> and SVG <image xlink:href|href> (cover encoding); check "<image" first.
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
