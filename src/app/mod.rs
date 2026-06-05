//! App state, the global key router, and the top-level render dispatch.
//!
//! Layout invariant (see ui::layout::skeleton): header / tabs / rule / body /
//! toast / footer; the overlay is drawn LAST over a `Clear` so it always wins.

pub mod lexicon;
pub mod overlay;
pub mod project;
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
    },
    OpenProject(String),
    OpenChapter {
        chapter: u32,
    },
    StartTranslation {
        chapters: Vec<u32>,
    },
    PauseRun,
    StopRun,
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
}

impl ActiveProject {
    /// First/active volume number (defaults to 1 if the project has none).
    fn active_vol(&self) -> u32 {
        self.project.volumes.first().map(|v| v.number).unwrap_or(1)
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
        }
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
            AppEvent::ChunkCommitted { chapter, .. } => {
                self.bump_committed(*chapter);
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
            } => {
                self.run_active = false;
                self.run_ctl = None;
                self.toast = Some(Toast::info(format!(
                    "run finished · {chapters_done} done · {chapters_failed} failed"
                )));
                self.push_log(
                    LogLevel::Info,
                    format!("pipeline finished: {chapters_done} done, {chapters_failed} failed"),
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
                        }
                        return;
                    }
                }
            }
        }
    }

    fn bump_committed(&mut self, chapter: u32) {
        if let Some(active) = self.active.as_mut() {
            for vol in active.project.volumes.iter_mut() {
                for ch in vol.chapters.iter_mut() {
                    if ch.number == chapter {
                        ch.committed_chunks = ch.committed_chunks.saturating_add(1);
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
            KeyCode::Char('l') | KeyCode::Char('`') => {
                return Action::show_overlay(Overlay::Log(0));
            }
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Esc if self.toast.is_some() => {
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
            Action::ImportEpub { epub, title, vol } => {
                self.start_import(epub, title, vol);
            }
            Action::StartTranslation { chapters } => {
                self.start_translation(chapters);
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
        let models = project
            .models
            .clone()
            .unwrap_or_else(|| self.cfg.models.clone());
        let workspace = Workspace::new(project.dir.clone(), vol);
        let client = match crate::build_client(&self.cfg) {
            Ok(client) => client,
            Err(e) => {
                self.toast = Some(Toast::error(format!("LLM client unavailable: {e}")));
                return;
            }
        };
        self.active = Some(ActiveProject {
            project,
            workspace,
            client,
            models,
        });
        self.lexicon.reset();
        self.project = ProjectScreen::new();
        self.screen = Screen::Project;
        self.toast = Some(Toast::info(format!("opened {id}")));
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

    fn start_translation(&mut self, chapters: Vec<u32>) {
        if self.run_active {
            self.toast = Some(Toast::warn("a run is already in progress"));
            return;
        }
        let Some(active) = self.active.as_ref() else {
            self.toast = Some(Toast::warn("no project open"));
            return;
        };
        if chapters.is_empty() {
            self.toast = Some(Toast::warn("nothing selected"));
            return;
        }
        let vol = active.active_vol();
        let ctl = crate::agents::pipeline::RunControl::new();
        let ctx = crate::agents::pipeline::PipelineCtx {
            client: Arc::clone(&active.client),
            ws: Workspace::new(active.project.dir.clone(), vol),
            models: active.models.clone(),
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
            "translating {} chapter(s)",
            chapters.len()
        )));
    }

    fn start_import(&mut self, epub: PathBuf, title: String, vol: u32) {
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
            match run_import(epub, dest, title, vol, models, &tx).await {
                Ok(project_id) => tx.send(AppEvent::ImportFinished { project_id }),
                Err(e) => tx.send(AppEvent::Error {
                    context: "import".to_string(),
                    msg: e.to_string(),
                }),
            }
        });
    }

    fn refresh_projects(&mut self) {
        self.projects = crate::workspace::scan::scan_projects(&working_root());
        self.shelf.rescan(&working_root());
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
                        ChapterStatus::Done | ChapterStatus::Appended => t.done += 1,
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
                            ChapterStatus::Done | ChapterStatus::Appended => t.done += 1,
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
async fn run_import(
    epub: PathBuf,
    dest: PathBuf,
    title: String,
    vol: u32,
    models: ModelSet,
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
            crate::workspace::scaffold::create_project(&dest, &title, &models, vol)
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
