//! Transient overlays drawn last over a `Clear`: import, settings, logs, help,
//! palette, confirm dialogs, and editors.
//!
//! `is_input_capturing()` suppresses single-letter globals while text input has focus.

use std::path::PathBuf;

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::export::ExportFormat;
use crate::model::{AppConfig, LogLevel, ServiceTier, ThemeId, UpdateMode};
use crate::theme::{self, ALL_THEMES, Theme};
use crate::ui::input::{self, EditOpts, Edited};
use crate::ui::layout::{centered_modal, centered_pct};
use crate::ui::mouse::{MouseGesture, MouseInput, hit};
use crate::ui::text::{col_width, pad_to_cols, thai_display_safe, truncate_cols};
use crate::ui::widgets::render_gauge;

use super::qa;
use super::{Action, Screen, slugify};

/// Where a synopsis editor sits in its lifecycle.
#[derive(Debug, Clone, PartialEq)]
pub enum SynPhase {
    /// Typing the raw source text.
    Editing,
    /// Awaiting a Translator round-trip (reroll or first translation).
    Translating,
    /// A Thai translation is in hand; can accept / reroll / edit.
    Done,
    /// The last translation attempt errored (message in `error`).
    Failed,
}

/// Shared state for the synopsis input + translate/reroll loop, embedded both in
/// the import wizard (step 3) and the standalone `Overlay::Synopsis` editor.
#[derive(Debug, Clone)]
pub struct SynopsisState {
    /// Raw, untranslated source synopsis (multi-line allowed).
    pub raw: String,
    /// Caret byte-offset into `raw`.
    pub cursor: usize,
    /// Latest Thai translation (empty until a roll lands).
    pub th: String,
    pub phase: SynPhase,
    /// Error text shown while `phase == Failed`.
    pub error: String,
    /// Reroll counter — drives rising translation temperature.
    pub attempt: u32,
}

impl SynopsisState {
    pub fn new(raw: String, th: String) -> Self {
        let phase = if th.trim().is_empty() {
            SynPhase::Editing
        } else {
            SynPhase::Done
        };
        Self {
            cursor: raw.len(),
            raw,
            th,
            phase,
            error: String::new(),
            attempt: 0,
        }
    }
}

/// What a synopsis keypress means to the embedding overlay.
enum SynKey {
    None,
    /// Start translating the current `raw` (phase already set to Translating).
    Translate,
    /// Accept the current (raw, th) pair.
    Accept,
    /// Proceed without a synopsis.
    Skip,
    /// Esc out of the editor (caller decides: prev step / close).
    Back,
}

/// Fold one keypress into a [`SynopsisState`], returning the embedder's next move.
fn handle_synopsis_keys(st: &mut SynopsisState, key: KeyEvent) -> SynKey {
    match st.phase {
        // Late results are ignored by set_synopsis_result once phase changes.
        SynPhase::Translating => match key.code {
            KeyCode::Esc => {
                st.phase = SynPhase::Editing;
                SynKey::None
            }
            _ => SynKey::None,
        },
        SynPhase::Editing => {
            let opts = EditOpts {
                numeric_only: false,
                multiline: true,
            };
            if input::handle(&mut st.raw, &mut st.cursor, key, opts) != Edited::Ignored {
                return SynKey::None;
            }
            match key.code {
                KeyCode::Esc => SynKey::Back,
                KeyCode::Tab => {
                    if st.raw.trim().is_empty() {
                        SynKey::Skip
                    } else {
                        st.phase = SynPhase::Translating;
                        SynKey::Translate
                    }
                }
                KeyCode::Enter => {
                    input::insert_char(&mut st.raw, &mut st.cursor, '\n');
                    SynKey::None
                }
                _ => SynKey::None,
            }
        }
        SynPhase::Done | SynPhase::Failed => match key.code {
            KeyCode::Char('r') | KeyCode::Char('R') => {
                st.attempt += 1;
                st.phase = SynPhase::Translating;
                SynKey::Translate
            }
            KeyCode::Char('e') | KeyCode::Char('E') => {
                st.phase = SynPhase::Editing;
                SynKey::None
            }
            KeyCode::Char('s') | KeyCode::Char('S') => SynKey::Skip,
            KeyCode::Enter if st.phase == SynPhase::Done => SynKey::Accept,
            KeyCode::Esc => {
                st.phase = SynPhase::Editing;
                SynKey::None
            }
            _ => SynKey::None,
        },
    }
}

/// Standalone synopsis editor (Project screen `y`): the shared editor state plus
/// the volume / project it targets, named in the modal title.
#[derive(Debug, Clone)]
pub struct SynopsisEditState {
    pub vol: u32,
    pub title: String,
    pub syn: SynopsisState,
}

/// A snapshot of an existing project, carried by the import wizard so it can give
/// live merge feedback (name collides → "adds into …", existing-volume warnings).
#[derive(Debug, Clone)]
pub struct ProjectRef {
    /// Stable slug = directory name.
    pub slug: String,
    pub title: String,
    /// (volume number, chapter count), ascending.
    pub volumes: Vec<(u32, usize)>,
}

impl ProjectRef {
    pub fn of(p: &crate::model::Project) -> Self {
        Self {
            slug: p.id.clone(),
            title: p.title.clone(),
            volumes: p
                .volumes
                .iter()
                .map(|v| (v.number, v.chapters.len()))
                .collect(),
        }
    }
}

/// The import wizard: pick source file → name → volume → synopsis → importing. When
/// `lock_name` is set (the "add volume to this project" flow), the name step is
/// skipped and the title is fixed to the open project's.
#[derive(Debug, Clone)]
pub struct ImportState {
    /// 0 = pick, 1 = name, 2 = volume, 3 = synopsis, 4 = importing (gauge).
    pub step: u8,
    /// Importable source files (path, byte size) found in the working root.
    pub files: Vec<(PathBuf, u64)>,
    pub sel: usize,
    pub name: String,
    /// Caret byte-offset into `name` (the wizard's name step).
    pub name_cursor: usize,
    /// True once the user edited the name; stops re-seeding it from the file stem.
    name_touched: bool,
    pub vol: u32,
    /// True once the user adjusted the volume; stops the next-volume auto-suggest.
    vol_touched: bool,
    /// True for the "add volume" flow: the name is the open project's and locked,
    /// so the wizard skips the name step (pick → volume → synopsis).
    pub lock_name: bool,
    /// Existing projects, for merge detection and existing-volume feedback.
    pub projects: Vec<ProjectRef>,
    /// Transient validation note (name step); cleared on the next edit.
    pub note: Option<&'static str>,
    /// Synopsis input + translate/reroll loop (wizard step 3).
    pub syn: SynopsisState,
    /// Live preprocessing progress (done, total, label) once the import starts.
    pub progress: Option<(usize, usize, String)>,
}

impl ImportState {
    fn new(files: Vec<(PathBuf, u64)>, projects: Vec<ProjectRef>) -> Self {
        // Seed the name field from the first source file's stem for a friendly default.
        let name = files
            .first()
            .and_then(|(p, _)| p.file_stem())
            .and_then(|s| s.to_str())
            .map(prettify_stem)
            .unwrap_or_default();
        Self {
            step: 0,
            files,
            sel: 0,
            name_cursor: name.len(),
            name,
            name_touched: false,
            vol: 1,
            vol_touched: false,
            lock_name: false,
            projects,
            note: None,
            syn: SynopsisState::new(String::new(), String::new()),
            progress: None,
        }
    }

    /// "Add volume" wizard: name fixed to `title`, volume pre-set to `vol`, and the
    /// name step skipped. The import merges into the existing project because its
    /// slug collides with the open project's.
    fn new_into(
        files: Vec<(PathBuf, u64)>,
        projects: Vec<ProjectRef>,
        title: String,
        vol: u32,
    ) -> Self {
        Self {
            step: 0,
            files,
            sel: 0,
            name_cursor: title.len(),
            name: title,
            name_touched: true,
            vol: vol.max(1),
            // The caller already computed the project's next volume.
            vol_touched: true,
            lock_name: true,
            projects,
            note: None,
            syn: SynopsisState::new(String::new(), String::new()),
            progress: None,
        }
    }

    fn selected_file(&self) -> Option<&PathBuf> {
        self.files.get(self.sel).map(|(p, _)| p)
    }

    /// The existing project this import would merge into (its slug matches the
    /// current name's), if any.
    pub fn target_project(&self) -> Option<&ProjectRef> {
        let slug = slugify(self.name.trim());
        if slug.is_empty() {
            return None;
        }
        self.projects.iter().find(|p| p.slug == slug)
    }

    /// When the name targets an existing project and the user hasn't picked a
    /// volume yet, default to one past its highest (instead of a colliding 1).
    fn suggest_volume(&mut self) {
        if self.vol_touched {
            return;
        }
        self.vol = match self.target_project() {
            Some(t) => (t.volumes.iter().map(|&(n, _)| n).max().unwrap_or(0) + 1).min(999),
            None => 1,
        };
    }

    /// Replace the file list after an in-wizard rescan, keeping the cursor sane.
    pub fn set_files(&mut self, files: Vec<(PathBuf, u64)>) {
        self.files = files;
        self.sel = self.sel.min(self.files.len().saturating_sub(1));
    }
}

/// Number of focusable Settings fields (base URL, 3 models, API key, retries).
const SETTINGS_FIELDS: u8 = 8;
/// Index of the API-key field within Settings.
const SETTINGS_KEY_FIELD: u8 = 4;
/// Index of the retry-attempts field within Settings (digits only).
const SETTINGS_RETRIES_FIELD: u8 = 5;
/// Index of the loop-watchdog stall field within Settings (digits only, seconds).
const SETTINGS_STALL_FIELD: u8 = 6;
/// Index of the per-chapter re-translate-limit field within Settings (digits only).
const SETTINGS_RETRANSLATE_FIELD: u8 = 7;

/// Whether a Settings field index is a digits-only numeric field.
fn settings_numeric_field(field: u8) -> bool {
    matches!(
        field,
        SETTINGS_RETRIES_FIELD | SETTINGS_STALL_FIELD | SETTINGS_RETRANSLATE_FIELD
    )
}

/// Settings: editable base URL + model ids + an editable, masked API key.
#[derive(Debug, Clone)]
pub struct SettingsState {
    pub base_url: String,
    pub orchestrator: String,
    pub translator: String,
    pub reviewer: String,
    /// The config-stored API key, editable here (masked on screen). Empty = none.
    pub api_key: String,
    /// True when an env var (HONYA_API_KEY / OPENROUTER_API_KEY) supplies the key;
    /// it overrides config, so the field is shown read-only.
    pub api_key_env: bool,
    /// Startup update behavior; toggled with Ctrl-U (not a text field).
    pub update_mode: UpdateMode,
    /// OpenRouter request tier; cycled with Ctrl-Y (not a text field).
    pub service_tier: Option<ServiceTier>,
    /// Max Translator↔Reviewer retry attempts per chunk, as typed (digits only).
    /// Parsed and clamped to 1..=20 on save via [`SettingsState::max_attempts_value`].
    pub max_attempts: String,
    /// Loop-watchdog stall window in seconds, as typed (digits only; 0 disables
    /// the time arm). Parsed via [`SettingsState::loop_stall_secs_value`].
    pub loop_stall_secs: String,
    /// Whole-chapter re-translations allowed on a detected loop before the run
    /// aborts, as typed (digits only). Parsed via
    /// [`SettingsState::max_chapter_retranslates_value`].
    pub max_chapter_retranslates: String,
    /// Which field is focused (0..=7).
    pub field: u8,
    /// Caret byte-offset into the focused (non-secret) field. The API-key field
    /// is masked, so it edits at the end and ignores this.
    pub cursor: usize,
}

impl SettingsState {
    fn from_cfg_focus(cfg: &AppConfig, field: u8) -> Self {
        let mut st = Self {
            base_url: cfg.base_url.clone(),
            orchestrator: cfg.models.orchestrator.clone(),
            translator: cfg.models.translator.clone(),
            reviewer: cfg.models.reviewer.clone(),
            api_key: cfg.api_key.clone().unwrap_or_default(),
            api_key_env: crate::config::api_key_from_env().is_some(),
            update_mode: cfg.update_mode,
            service_tier: cfg.service_tier,
            max_attempts: cfg.max_attempts.to_string(),
            loop_stall_secs: cfg.loop_stall_secs.to_string(),
            max_chapter_retranslates: cfg.max_chapter_retranslates.to_string(),
            field: 0,
            cursor: 0,
        };
        st.focus(field.min(SETTINGS_FIELDS - 1));
        st
    }

    /// Focus a field and drop the caret at its end.
    fn focus(&mut self, field: u8) {
        self.field = field % SETTINGS_FIELDS;
        self.cursor = self.field_mut().len();
    }

    fn next_field(&mut self) {
        self.focus(self.field + 1);
    }

    fn prev_field(&mut self) {
        self.focus(self.field + SETTINGS_FIELDS - 1);
    }

    fn field_mut(&mut self) -> &mut String {
        match self.field {
            0 => &mut self.base_url,
            1 => &mut self.orchestrator,
            2 => &mut self.translator,
            3 => &mut self.reviewer,
            4 => &mut self.api_key,
            5 => &mut self.max_attempts,
            6 => &mut self.loop_stall_secs,
            _ => &mut self.max_chapter_retranslates,
        }
    }

    /// The retries field parsed into a usable attempt count. Empty, non-numeric,
    /// or 0 falls back to 1; values are capped at 20 to keep the loop bounded.
    fn max_attempts_value(&self) -> u32 {
        self.max_attempts
            .trim()
            .parse::<u32>()
            .unwrap_or(0)
            .clamp(1, 20)
    }

    /// Loop-watchdog stall window in seconds (0 disables the time arm). Non-numeric
    /// falls back to the default; capped at 3600 s.
    fn loop_stall_secs_value(&self) -> u64 {
        let raw = self.loop_stall_secs.trim();
        if raw.is_empty() {
            return AppConfig::default().loop_stall_secs;
        }
        raw.parse::<u64>()
            .unwrap_or_else(|_| AppConfig::default().loop_stall_secs)
            .min(3600)
    }

    /// Whole-chapter re-translations allowed before a looping chapter aborts the
    /// run. Non-numeric falls back to the default; capped at 10.
    fn max_chapter_retranslates_value(&self) -> u32 {
        let raw = self.max_chapter_retranslates.trim();
        if raw.is_empty() {
            return AppConfig::default().max_chapter_retranslates;
        }
        raw.parse::<u32>()
            .unwrap_or_else(|_| AppConfig::default().max_chapter_retranslates)
            .min(10)
    }
}

/// Theme picker; navigating live-previews via `PreviewTheme`, so the whole UI
/// behind the modal recolors as the selection moves.
#[derive(Debug, Clone)]
pub struct ThemePickerState {
    /// Index into [`ALL_THEMES`].
    pub sel: usize,
}

impl ThemePickerState {
    fn new(current: ThemeId) -> Self {
        Self {
            sel: current.index(),
        }
    }

    fn current(&self) -> ThemeId {
        ALL_THEMES.get(self.sel).copied().unwrap_or_default()
    }
}

/// Command palette: a fuzzy-ish filtered list of navigation commands.
#[derive(Debug, Clone)]
pub struct PaletteState {
    pub query: String,
    /// Caret byte-offset into `query`.
    pub cursor: usize,
    pub items: Vec<PaletteItem>,
    pub sel: usize,
}

#[derive(Debug, Clone)]
pub struct PaletteItem {
    pub label: &'static str,
    pub action: Action,
}

impl PaletteState {
    fn new() -> Self {
        let items = vec![
            PaletteItem {
                label: "Getting started 入門",
                action: Action::show_overlay(Overlay::welcome_placeholder()),
            },
            PaletteItem {
                label: "Go: Shelf 書架",
                action: Action::Goto(Screen::Shelf),
            },
            PaletteItem {
                label: "Go: Project 棚",
                action: Action::Goto(Screen::Project),
            },
            PaletteItem {
                label: "Go: Translate 訳",
                action: Action::Goto(Screen::Translate),
            },
            PaletteItem {
                label: "Translate whole project 全",
                action: Action::StartProjectTranslation,
            },
            PaletteItem {
                label: "Go: Reader 読",
                action: Action::Goto(Screen::Reader),
            },
            PaletteItem {
                label: "Go: Lexicon 辞",
                action: Action::Goto(Screen::Lexicon),
            },
            PaletteItem {
                label: "Settings",
                action: Action::show_overlay(Overlay::settings_placeholder()),
            },
            PaletteItem {
                label: "Theme 配色",
                action: Action::show_overlay(Overlay::theme_placeholder()),
            },
            PaletteItem {
                label: "Help",
                action: Action::show_overlay(Overlay::Help(0)),
            },
            PaletteItem {
                label: "About 本屋",
                action: Action::show_overlay(Overlay::About),
            },
            PaletteItem {
                label: "QA review レビュー",
                action: Action::show_overlay(Overlay::qa_placeholder()),
            },
            PaletteItem {
                label: "Activity log",
                action: Action::show_overlay(Overlay::Log(0)),
            },
            PaletteItem {
                label: "Quit",
                action: Action::Quit,
            },
        ];
        Self {
            query: String::new(),
            cursor: 0,
            items,
            sel: 0,
        }
    }

    /// Indices of items matching the current (case-insensitive substring) query.
    fn matches(&self) -> Vec<usize> {
        if self.query.is_empty() {
            return (0..self.items.len()).collect();
        }
        let q = self.query.to_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter(|(_, it)| it.label.to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect()
    }
}

/// A generic confirm modal wrapping the action to run on confirm, optionally
/// with one alternate key/action (used for Continue vs Restart decisions).
#[derive(Debug, Clone)]
pub struct Dialog {
    pub title: String,
    pub body: String,
    pub confirm_label: String,
    pub confirm: Action,
    pub alternate: Option<DialogAlternate>,
}

#[derive(Debug, Clone)]
pub struct DialogAlternate {
    pub key: char,
    pub label: String,
    pub action: Action,
}

/// Snapshot of the active volume's QA report, navigated in the QA overlay. Built
/// once when the overlay opens (like the palette / settings snapshots).
#[derive(Debug, Clone)]
pub struct QaState {
    /// Header label (`project · Vol.NN`), or a "no project" note.
    pub title: String,
    /// Issues + chapter-level counts, gathered at open time.
    pub report: qa::QaReport,
    /// Selected finding index into `report.issues`.
    pub sel: usize,
}

/// Single-line proofreading note captured from the Reader and persisted to
/// VOLUME.md as a line-anchored annotation.
#[derive(Debug, Clone)]
pub struct ReaderNoteState {
    pub chapter: u32,
    pub line: u32,
    pub text: String,
    /// Caret byte-offset into `text`.
    pub cursor: usize,
}

/// Reader global-search input: a single text field. On commit the App hands the
/// query to the Reader, which finds matches across both the JA and TH panes.
#[derive(Debug, Clone)]
pub struct ReaderSearchState {
    pub query: String,
    /// Caret byte-offset into `query`.
    pub cursor: usize,
}

/// What a [`JumpTarget`] points at — decides its glyph in the picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JumpKind {
    Chapter,
    Section,
    Bookmark,
}

/// One destination in the Reader jump/outline picker: a chapter, a section heading
/// within the current chapter, or a saved bookmark.
#[derive(Debug, Clone)]
pub struct JumpTarget {
    pub chapter: u32,
    /// 1-based line to scroll to within the chapter (1 = top).
    pub line: u32,
    /// Display label, also matched by the filter query.
    pub label: String,
    pub kind: JumpKind,
}

/// Reader jump/outline picker: a filterable list of chapters, the current chapter's
/// section headings, and the volume's bookmarks. Built App-side from live state
/// (like the QA overlay), so the placeholder constructor carries no items.
#[derive(Debug, Clone)]
pub struct ReaderJumpState {
    pub title: String,
    pub query: String,
    /// Caret byte-offset into `query`.
    pub cursor: usize,
    pub items: Vec<JumpTarget>,
    pub sel: usize,
}

impl ReaderJumpState {
    /// Indices of items whose label contains the (case-insensitive) query.
    pub fn matches(&self) -> Vec<usize> {
        if self.query.trim().is_empty() {
            return (0..self.items.len()).collect();
        }
        let q = self.query.to_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter(|(_, it)| it.label.to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect()
    }
}

/// First-run welcome / getting-started overlay: a short explainer of the five-screen
/// workflow plus an action menu. Built App-side with live key/sample status so the
/// menu labels read correctly.
#[derive(Debug, Clone)]
pub struct WelcomeState {
    /// Selected menu row (0..WELCOME_ITEMS).
    pub sel: usize,
    /// Whether an API key is already configured (env or saved).
    pub api_key_present: bool,
    /// Whether the bundled sample project already exists on disk.
    pub sample_exists: bool,
}

/// Number of selectable rows in the Welcome action menu.
const WELCOME_ITEMS: usize = 4;

/// Export-volume overlay: a format checklist, then a live gauge, then a results
/// panel. `formats` mirrors [`ExportFormat::ALL`] order (Markdown · EPUB · DOCX).
#[derive(Debug, Clone)]
pub struct ExportState {
    pub vol: u32,
    /// Per-format on/off, indexed like [`ExportFormat::ALL`].
    pub formats: [bool; 3],
    /// Cursor row over the format list.
    pub sel: usize,
    /// Live progress (done, total, current-format label) once export starts.
    pub progress: Option<(usize, usize, String)>,
    /// Results once finished: (written file paths, warnings).
    pub done: Option<(Vec<PathBuf>, Vec<String>)>,
}

impl ExportState {
    fn new(vol: u32) -> Self {
        // Default: all three formats selected (user opts out of what they don't want).
        Self {
            vol,
            formats: [true, true, true],
            sel: 0,
            progress: None,
            done: None,
        }
    }

    /// Selected formats in display order.
    fn selected_formats(&self) -> Vec<ExportFormat> {
        ExportFormat::ALL
            .iter()
            .zip(self.formats)
            .filter_map(|(f, on)| on.then_some(*f))
            .collect()
    }
}

#[derive(Debug, Clone)]
pub enum Overlay {
    None,
    /// First-run getting-started overlay (sample / import / key / dismiss).
    Welcome(WelcomeState),
    Import(ImportState),
    Settings(SettingsState),
    /// Live-preview color theme picker.
    Theme(ThemePickerState),
    /// Activity log; the `u16` is the scroll-back offset (0 = newest tail).
    Log(u16),
    /// Keybinding reference; the `u16` is the vertical scroll offset.
    Help(u16),
    /// About card — version, credits, and a frame-driven moon/translation motif.
    About,
    Palette(PaletteState),
    Modal(Dialog),
    /// Standalone volume-synopsis editor (re-opened from the Project screen).
    Synopsis(SynopsisEditState),
    /// Translation QA inbox — per-chapter issue counts + navigable findings, opened
    /// from the Project or Reader tab (Enter jumps to the chapter in the Reader).
    Qa(QaState),
    /// Reader proofreading note editor, anchored to a translated line.
    ReaderNote(ReaderNoteState),
    /// Reader global search across both panes (JA + TH).
    ReaderSearch(ReaderSearchState),
    /// Reader jump/outline picker (chapters · sections · bookmarks).
    ReaderJump(ReaderJumpState),
    /// Export the active volume to deliverable formats (Markdown · EPUB · DOCX).
    Export(ExportState),
}

impl Overlay {
    pub fn import(files: Vec<(PathBuf, u64)>, projects: &[crate::model::Project]) -> Self {
        let refs = projects.iter().map(ProjectRef::of).collect();
        Overlay::Import(ImportState::new(files, refs))
    }

    /// "Add volume" wizard, pre-targeted at an open project: the name is locked to
    /// `title` and the volume defaults to `vol` (the project's next number).
    pub fn import_into(
        files: Vec<(PathBuf, u64)>,
        projects: &[crate::model::Project],
        title: String,
        vol: u32,
    ) -> Self {
        let refs = projects.iter().map(ProjectRef::of).collect();
        Overlay::Import(ImportState::new_into(files, refs, title, vol))
    }

    /// Welcome overlay seeded with live key / sample status.
    pub fn welcome(api_key_present: bool, sample_exists: bool) -> Self {
        Overlay::Welcome(WelcomeState {
            sel: 0,
            api_key_present,
            sample_exists,
        })
    }

    /// Placeholder for the palette (no live status handle); the App swaps in the
    /// real key/sample status on show, mirroring the Settings/QA placeholder pattern.
    fn welcome_placeholder() -> Self {
        Overlay::welcome(false, false)
    }

    /// Settings built from live config with a specific field pre-focused (0 = top;
    /// the Welcome overlay's "Set API key" shortcut focuses the key field).
    pub fn settings_with_field(cfg: &AppConfig, field: u8) -> Self {
        Overlay::Settings(SettingsState::from_cfg_focus(cfg, field))
    }

    pub fn theme(current: ThemeId) -> Self {
        Overlay::Theme(ThemePickerState::new(current))
    }

    /// Placeholder picker for the palette (no `ThemeId` handle); the App swaps in
    /// the live `cfg.theme` on show.
    fn theme_placeholder() -> Self {
        Overlay::Theme(ThemePickerState { sel: 0 })
    }

    pub fn palette() -> Self {
        Overlay::Palette(PaletteState::new())
    }

    /// Standalone synopsis editor seeded from a volume's stored raw/Thai; `vol`
    /// and `title` name the target in the modal title.
    pub fn synopsis_edit(raw: String, th: String, vol: u32, title: String) -> Self {
        Overlay::Synopsis(SynopsisEditState {
            vol,
            title,
            syn: SynopsisState::new(raw, th),
        })
    }

    pub fn reader_note(chapter: u32, line: u32) -> Self {
        Overlay::ReaderNote(ReaderNoteState {
            chapter,
            line: line.max(1),
            text: String::new(),
            cursor: 0,
        })
    }

    pub fn reader_search() -> Self {
        Overlay::ReaderSearch(ReaderSearchState {
            query: String::new(),
            cursor: 0,
        })
    }

    /// Jump/outline picker seeded with live targets.
    pub fn reader_jump(title: String, items: Vec<JumpTarget>) -> Self {
        Overlay::ReaderJump(ReaderJumpState {
            title,
            query: String::new(),
            cursor: 0,
            items,
            sel: 0,
        })
    }

    /// Placeholder jump picker for the Reader `g` key; the App swaps in the live
    /// chapter/section/bookmark list on show, mirroring the QA placeholder pattern.
    pub fn reader_jump_placeholder() -> Self {
        Overlay::ReaderJump(ReaderJumpState {
            title: String::new(),
            query: String::new(),
            cursor: 0,
            items: Vec::new(),
            sel: 0,
        })
    }

    /// QA overlay seeded with a freshly-gathered report.
    pub fn qa(title: String, report: qa::QaReport) -> Self {
        Overlay::Qa(QaState {
            title,
            report,
            sel: 0,
        })
    }

    /// Placeholder QA overlay for the palette / screen `Q`; the App swaps in the
    /// live report (gathered from the active project) on show, mirroring the
    /// Settings/Theme placeholder pattern.
    pub fn qa_placeholder() -> Self {
        Overlay::Qa(QaState {
            title: String::new(),
            report: qa::QaReport::default(),
            sel: 0,
        })
    }

    pub fn confirm(title: impl Into<String>, body: impl Into<String>, confirm: Action) -> Self {
        Overlay::Modal(Dialog {
            title: title.into(),
            body: body.into(),
            confirm_label: "confirm".to_string(),
            confirm,
            alternate: None,
        })
    }

    pub fn confirm_with_alternate(
        title: impl Into<String>,
        body: impl Into<String>,
        confirm_label: impl Into<String>,
        confirm: Action,
        alternate_key: char,
        alternate_label: impl Into<String>,
        alternate_action: Action,
    ) -> Self {
        Overlay::Modal(Dialog {
            title: title.into(),
            body: body.into(),
            confirm_label: confirm_label.into(),
            confirm,
            alternate: Some(DialogAlternate {
                key: alternate_key,
                label: alternate_label.into(),
                action: alternate_action,
            }),
        })
    }

    /// A settings overlay placeholder — used by callers without an `&AppConfig`
    /// handle (palette, Welcome); the App swaps in the real config field values on
    /// show, preserving the requested focused `field`.
    fn settings_at(field: u8) -> Self {
        Overlay::Settings(SettingsState {
            base_url: String::new(),
            orchestrator: String::new(),
            translator: String::new(),
            reviewer: String::new(),
            api_key: String::new(),
            api_key_env: false,
            update_mode: UpdateMode::default(),
            service_tier: None,
            max_attempts: String::new(),
            loop_stall_secs: String::new(),
            max_chapter_retranslates: String::new(),
            field: field.min(SETTINGS_FIELDS - 1),
            cursor: 0,
        })
    }

    fn settings_placeholder() -> Self {
        Overlay::settings_at(0)
    }

    /// Export-volume overlay for `vol` (format checklist → gauge → results).
    pub fn export(vol: u32) -> Self {
        Overlay::Export(ExportState::new(vol))
    }

    pub fn set_import_progress(&mut self, done: usize, total: usize, label: &str) {
        if let Overlay::Import(st) = self {
            st.step = 4;
            st.progress = Some((done, total, label.to_string()));
        }
    }

    pub fn set_export_progress(&mut self, done: usize, total: usize, label: &str) {
        if let Overlay::Export(st) = self {
            st.progress = Some((done, total, label.to_string()));
        }
    }

    pub fn set_export_done(&mut self, paths: Vec<PathBuf>, warnings: Vec<String>) {
        if let Overlay::Export(st) = self {
            st.done = Some((paths, warnings));
        }
    }

    /// Fold a finished translation (or its error) into whichever synopsis editor
    /// is still awaiting it.
    pub fn set_synopsis_result(&mut self, result: std::result::Result<String, String>) {
        let st = match self {
            Overlay::Import(s) if s.step == 3 => &mut s.syn,
            Overlay::Synopsis(s) => &mut s.syn,
            _ => return,
        };
        if st.phase != SynPhase::Translating {
            return;
        }
        match result {
            Ok(text) => {
                st.th = text;
                st.error.clear();
                st.phase = SynPhase::Done;
            }
            Err(msg) => {
                st.error = msg;
                st.phase = SynPhase::Failed;
            }
        }
    }

    /// True when a text field is focused → suppress single-letter globals.
    #[allow(dead_code)]
    pub fn is_input_capturing(&self) -> bool {
        match self {
            Overlay::Import(st) => {
                st.step == 1 || (st.step == 3 && st.syn.phase == SynPhase::Editing)
            }
            Overlay::Synopsis(st) => st.syn.phase == SynPhase::Editing,
            Overlay::ReaderNote(_) => true,
            Overlay::ReaderSearch(_) => true, // query field
            Overlay::ReaderJump(_) => true,   // filter field
            Overlay::Settings(_) => true,     // always editing a field
            Overlay::Palette(_) => true,      // query field
            _ => false,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        match self {
            Overlay::None => Action::None,
            Overlay::Welcome(_) => self.handle_welcome_key(key),
            Overlay::Import(_) => self.handle_import_key(key),
            Overlay::Settings(_) => self.handle_settings_key(key),
            Overlay::Theme(_) => self.handle_theme_key(key),
            Overlay::Palette(_) => self.handle_palette_key(key),
            Overlay::Modal(_) => self.handle_modal_key(key),
            Overlay::Synopsis(_) => self.handle_synopsis_overlay_key(key),
            Overlay::Qa(_) => self.handle_qa_key(key),
            Overlay::ReaderNote(_) => self.handle_reader_note_key(key),
            Overlay::ReaderSearch(_) => self.handle_reader_search_key(key),
            Overlay::ReaderJump(_) => self.handle_reader_jump_key(key),
            Overlay::Export(_) => self.handle_export_key(key),
            Overlay::Log(off) => match key.code {
                KeyCode::Esc | KeyCode::Char('l') | KeyCode::Char('q') => Action::CloseOverlay,
                KeyCode::Char('k') | KeyCode::Up => {
                    *off = off.saturating_add(1);
                    Action::None
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    *off = off.saturating_sub(1);
                    Action::None
                }
                _ => Action::None,
            },
            // The About card is read-only: any dismiss-ish key closes it.
            Overlay::About => match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Backspace => {
                    Action::CloseOverlay
                }
                _ => Action::None,
            },
            Overlay::Help(off) => match key.code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => Action::CloseOverlay,
                KeyCode::Char('j') | KeyCode::Down => {
                    *off = off.saturating_add(1);
                    Action::None
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    *off = off.saturating_sub(1);
                    Action::None
                }
                _ => Action::None,
            },
        }
    }

    /// Fold one mouse gesture into the open overlay. Scroll and right-click reuse
    /// the keyboard handlers (navigation / scroll / dismiss logic stays in one
    /// place); a left click is resolved against the overlay's modal geometry —
    /// outside the modal dismisses, inside selects or activates a row / button.
    pub fn handle_mouse(&mut self, m: MouseInput, area: Rect) -> Action {
        match m.gesture {
            MouseGesture::ScrollUp => return self.handle_key(synth(KeyCode::Up)),
            MouseGesture::ScrollDown => return self.handle_key(synth(KeyCode::Down)),
            MouseGesture::RightClick => return self.handle_key(synth(KeyCode::Esc)),
            MouseGesture::Click { .. } => {}
        }
        let double = m.is_double();
        let modal = self.modal_rect(area);
        if !hit(modal, m.col, m.row) {
            // A click off the modal dismisses / steps back (each overlay's Esc).
            return self.handle_key(synth(KeyCode::Esc));
        }
        // Resolve the click against the modal's interior, then act on the outcome
        // outside the borrow so the keyboard handlers can be reused.
        let inner = inset(modal);
        match self.resolve_click(m, inner, double) {
            ClickOutcome::Nothing => Action::None,
            ClickOutcome::Key(code) => self.handle_key(synth(code)),
            ClickOutcome::Act(action) => action,
        }
    }

    /// The centered rectangle each overlay variant draws into (mirrors its render
    /// fn's `centered_modal` / `centered_pct` call), used for click hit-testing.
    fn modal_rect(&self, area: Rect) -> Rect {
        match self {
            Overlay::None => area,
            Overlay::Welcome(_) => centered_modal(76, 24, area),
            // One size for every wizard step (the modal must not jump around as
            // the user advances); mirrors render_import.
            Overlay::Import(_) => centered_modal(78, 24, area),
            // Must mirror render_settings' centered_modal(72, 26, …) so clicks
            // near the modal's top/bottom hit-test inside it (not as a dismiss).
            Overlay::Settings(_) => centered_modal(72, 26, area),
            Overlay::Theme(_) => centered_modal(60, 20, area),
            Overlay::Palette(_) => centered_modal(60, 16, area),
            Overlay::Log(_) => centered_pct(80, 80, area),
            Overlay::Help(_) => centered_modal(72, 24, area),
            Overlay::About => centered_modal(64, 20, area),
            Overlay::Modal(dlg) => {
                centered_modal(64, if dlg.alternate.is_some() { 11 } else { 9 }, area)
            }
            Overlay::Synopsis(_) => centered_modal(76, 24, area),
            Overlay::Qa(_) => centered_pct(80, 80, area),
            Overlay::ReaderNote(_) => centered_modal(72, 14, area),
            Overlay::ReaderSearch(_) => centered_modal(64, 7, area),
            Overlay::ReaderJump(_) => centered_modal(72, 24, area),
            Overlay::Export(_) => centered_modal(66, 15, area),
        }
    }

    /// Map a click inside the modal interior to an outcome. Row selection is set
    /// here (within the borrow); the actual activation key is synthesized by the
    /// caller after the borrow ends. `inner` is the bordered modal's content rect.
    fn resolve_click(&mut self, m: MouseInput, inner: Rect, double: bool) -> ClickOutcome {
        match self {
            Overlay::Modal(dlg) => {
                // The button row is the last interior line.
                let row = inner.y + inner.height.saturating_sub(1);
                if m.row != row {
                    return ClickOutcome::Nothing;
                }
                match modal_button_at(dlg, inner, m.col) {
                    Some(ModalButton::Confirm) => ClickOutcome::Key(KeyCode::Enter),
                    Some(ModalButton::Cancel) => ClickOutcome::Key(KeyCode::Esc),
                    Some(ModalButton::Alternate(c)) => ClickOutcome::Key(KeyCode::Char(c)),
                    None => ClickOutcome::Nothing,
                }
            }
            // Welcome menu: 4 items at a fixed offset below the preamble (see
            // `render_welcome` — 10 preamble lines precede the first item).
            Overlay::Welcome(st) => {
                let base = inner.y + 10;
                if m.row >= base && (m.row - base) < WELCOME_ITEMS as u16 {
                    let idx = (m.row - base) as usize;
                    let already = st.sel == idx;
                    st.sel = idx;
                    if double || already {
                        return ClickOutcome::Key(KeyCode::Enter);
                    }
                }
                ClickOutcome::Nothing
            }
            // Theme list fills the interior above a 2-line swatch; windowed so the
            // selection stays visible. Single click previews; double commits.
            Overlay::Theme(st) => {
                let list_h = inner.height.saturating_sub(2);
                if m.row >= inner.y && (m.row - inner.y) < list_h {
                    let start = windowed_start(st.sel, list_h);
                    let idx = start + (m.row - inner.y) as usize;
                    if idx < ALL_THEMES.len() {
                        let already = st.sel == idx;
                        st.sel = idx;
                        if double || already {
                            return ClickOutcome::Key(KeyCode::Enter);
                        }
                        return ClickOutcome::Act(Action::PreviewTheme(st.current()));
                    }
                }
                ClickOutcome::Nothing
            }
            // Palette list starts 2 lines below the query and isn't windowed.
            Overlay::Palette(st) => {
                let top = inner.y + 2;
                let len = st.matches().len();
                if m.row >= top {
                    let idx = (m.row - top) as usize;
                    if idx < len {
                        let already = st.sel == idx;
                        st.sel = idx;
                        if double || already {
                            return ClickOutcome::Key(KeyCode::Enter);
                        }
                    }
                }
                ClickOutcome::Nothing
            }
            // Jump list starts 2 lines below the query and is windowed.
            Overlay::ReaderJump(st) => {
                let top = inner.y + 2;
                let list_h = inner.height.saturating_sub(2);
                let len = st.matches().len();
                if m.row >= top && (m.row - top) < list_h {
                    let start = windowed_start(st.sel, list_h);
                    let idx = start + (m.row - top) as usize;
                    if idx < len {
                        let already = st.sel == idx;
                        st.sel = idx;
                        if double || already {
                            return ClickOutcome::Key(KeyCode::Enter);
                        }
                    }
                }
                ClickOutcome::Nothing
            }
            // Export format checklist: rows start 2 lines down. A single click
            // toggles the format under it; a double click exports.
            Overlay::Export(st) => {
                if st.done.is_some() || st.progress.is_some() {
                    return if double {
                        ClickOutcome::Key(KeyCode::Enter)
                    } else {
                        ClickOutcome::Nothing
                    };
                }
                let base = inner.y + 2;
                if m.row >= base && ((m.row - base) as usize) < st.formats.len() {
                    st.sel = (m.row - base) as usize;
                    return if double {
                        ClickOutcome::Key(KeyCode::Enter)
                    } else {
                        ClickOutcome::Key(KeyCode::Char(' '))
                    };
                }
                ClickOutcome::Nothing
            }
            // Import wizard: in the file-pick step a click selects the row under
            // it; a double click (or a click on the current pick) advances.
            Overlay::Import(st) if st.step == 0 => {
                let top = inner.y + IMPORT_HEADER_ROWS + IMPORT_PICK_LIST_OFFSET;
                let list_h = inner
                    .height
                    .saturating_sub(IMPORT_HEADER_ROWS + IMPORT_PICK_LIST_OFFSET);
                if m.row >= top && (m.row - top) < list_h {
                    let start = windowed_start(st.sel, list_h);
                    let idx = start + (m.row - top) as usize;
                    if idx < st.files.len() {
                        let already = st.sel == idx;
                        st.sel = idx;
                        if double || already {
                            return ClickOutcome::Key(KeyCode::Enter);
                        }
                    }
                }
                ClickOutcome::Nothing
            }
            // QA findings interleave non-selectable chapter headers, so a click
            // just activates the current pick (the wheel moves it).
            Overlay::Qa(_) => {
                if double {
                    ClickOutcome::Key(KeyCode::Enter)
                } else {
                    ClickOutcome::Nothing
                }
            }
            // Text editors / progress views: inside-clicks do nothing (scroll and
            // click-outside still work).
            _ => ClickOutcome::Nothing,
        }
    }

    fn handle_welcome_key(&mut self, key: KeyEvent) -> Action {
        let Overlay::Welcome(st) = self else {
            return Action::None;
        };
        match key.code {
            // Esc / q dismiss and mark onboarding complete (App persists the flag).
            KeyCode::Esc | KeyCode::Char('q') => Action::DismissWelcome,
            KeyCode::Up | KeyCode::Char('k') => {
                st.sel = (st.sel + WELCOME_ITEMS - 1) % WELCOME_ITEMS;
                Action::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                st.sel = (st.sel + 1) % WELCOME_ITEMS;
                Action::None
            }
            KeyCode::Enter => match st.sel {
                0 => Action::CreateSample,
                1 => Action::OpenImport,
                2 => Action::show_overlay(Overlay::settings_at(SETTINGS_KEY_FIELD)),
                _ => Action::DismissWelcome,
            },
            _ => Action::None,
        }
    }

    fn handle_export_key(&mut self, key: KeyEvent) -> Action {
        let Overlay::Export(st) = self else {
            return Action::None;
        };
        // Done: any acknowledge key closes.
        if st.done.is_some() {
            return match key.code {
                KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') => Action::CloseOverlay,
                _ => Action::None,
            };
        }
        // Running: export continues in the background.
        if st.progress.is_some() {
            return match key.code {
                KeyCode::Esc | KeyCode::Char('q') => Action::CloseOverlay,
                _ => Action::None,
            };
        }
        let n = st.formats.len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => Action::CloseOverlay,
            KeyCode::Up | KeyCode::Char('k') => {
                st.sel = (st.sel + n - 1) % n;
                Action::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                st.sel = (st.sel + 1) % n;
                Action::None
            }
            KeyCode::Char(' ') => {
                st.formats[st.sel] = !st.formats[st.sel];
                Action::None
            }
            KeyCode::Enter => {
                let formats = st.selected_formats();
                if formats.is_empty() {
                    Action::None
                } else {
                    Action::ExportVolume {
                        vol: st.vol,
                        formats,
                    }
                }
            }
            _ => Action::None,
        }
    }

    fn handle_import_key(&mut self, key: KeyEvent) -> Action {
        let Overlay::Import(st) = self else {
            return Action::None;
        };
        match st.step {
            // Step 0: pick source file.
            0 => match key.code {
                KeyCode::Esc => Action::CloseOverlay,
                KeyCode::Char('r') | KeyCode::Char('R') => Action::RescanImports,
                KeyCode::Up | KeyCode::Char('k') => {
                    if st.sel > 0 {
                        st.sel -= 1;
                    }
                    Action::None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if st.sel + 1 < st.files.len() {
                        st.sel += 1;
                    }
                    Action::None
                }
                KeyCode::Home | KeyCode::Char('g') => {
                    st.sel = 0;
                    Action::None
                }
                KeyCode::End | KeyCode::Char('G') => {
                    st.sel = st.files.len().saturating_sub(1);
                    Action::None
                }
                KeyCode::Enter => {
                    if st.files.is_empty() {
                        Action::CloseOverlay
                    } else {
                        // Until the user types their own name, follow the selected
                        // file's stem so the default tracks the actual pick.
                        if !st.lock_name
                            && !st.name_touched
                            && let Some(stem) = st
                                .selected_file()
                                .and_then(|p| p.file_stem())
                                .and_then(|s| s.to_str())
                        {
                            st.name = prettify_stem(stem);
                        }
                        st.name_cursor = st.name.len();
                        if st.lock_name {
                            st.step = 2;
                        } else {
                            st.step = 1;
                        }
                        Action::None
                    }
                }
                _ => Action::None,
            },
            // Step 1: name.
            1 => {
                if input::handle(&mut st.name, &mut st.name_cursor, key, EditOpts::default())
                    != Edited::Ignored
                {
                    st.name_touched = true;
                    st.note = None;
                    return Action::None;
                }
                match key.code {
                    KeyCode::Esc => {
                        st.step = 0;
                        st.note = None;
                        Action::None
                    }
                    KeyCode::Enter | KeyCode::Tab => {
                        if st.name.trim().is_empty() {
                            st.note = Some("ใส่ชื่อโปรเจกต์ก่อน · a project name is required");
                        } else {
                            st.note = None;
                            st.step = 2;
                            st.suggest_volume();
                        }
                        Action::None
                    }
                    _ => Action::None,
                }
            }
            // Step 2: volume.
            2 => match key.code {
                KeyCode::Esc => {
                    st.step = if st.lock_name { 0 } else { 1 };
                    Action::None
                }
                KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('+') | KeyCode::Right => {
                    st.vol = st.vol.saturating_add(1).min(999);
                    st.vol_touched = true;
                    Action::None
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('-') | KeyCode::Left => {
                    st.vol = st.vol.saturating_sub(1).max(1);
                    st.vol_touched = true;
                    Action::None
                }
                KeyCode::Char(d @ '0'..='9') => {
                    let digit = d as u32 - '0' as u32;
                    st.vol = (st.vol.saturating_mul(10).saturating_add(digit)).clamp(1, 999);
                    st.vol_touched = true;
                    Action::None
                }
                KeyCode::Backspace => {
                    st.vol = (st.vol / 10).max(1);
                    st.vol_touched = true;
                    Action::None
                }
                KeyCode::Enter => {
                    st.step = 3;
                    Action::None
                }
                _ => Action::None,
            },
            // Step 3: synopsis.
            3 => {
                let intent = handle_synopsis_keys(&mut st.syn, key);
                match intent {
                    SynKey::None => Action::None,
                    SynKey::Translate => Action::TranslateSynopsis {
                        raw: st.syn.raw.clone(),
                        attempt: st.syn.attempt,
                    },
                    SynKey::Back => {
                        st.step = 2;
                        Action::None
                    }
                    SynKey::Accept => {
                        let source = st.selected_file().cloned().unwrap_or_default();
                        let title = st.name.trim().to_string();
                        let vol = st.vol.max(1);
                        let synopsis_raw = st.syn.raw.trim().to_string();
                        let synopsis_th = st.syn.th.trim().to_string();
                        st.step = 4;
                        st.progress = Some((0, 0, "starting".to_string()));
                        Action::ImportFile {
                            source,
                            title,
                            vol,
                            synopsis_raw,
                            synopsis_th,
                        }
                    }
                    SynKey::Skip => {
                        let source = st.selected_file().cloned().unwrap_or_default();
                        let title = st.name.trim().to_string();
                        let vol = st.vol.max(1);
                        st.step = 4;
                        st.progress = Some((0, 0, "starting".to_string()));
                        Action::ImportFile {
                            source,
                            title,
                            vol,
                            synopsis_raw: String::new(),
                            synopsis_th: String::new(),
                        }
                    }
                }
            }
            // Step 4: importing.
            _ => match key.code {
                KeyCode::Esc => Action::CloseOverlay,
                _ => Action::None,
            },
        }
    }

    fn handle_settings_key(&mut self, key: KeyEvent) -> Action {
        let Overlay::Settings(st) = self else {
            return Action::None;
        };
        match key.code {
            KeyCode::Esc => Action::CloseOverlay,
            // Overlay captures keys before the global Ctrl-T router.
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Action::show_overlay(Overlay::theme_placeholder())
            }
            // Ctrl-U mirrors Ctrl-T inside Settings.
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                st.update_mode = st.update_mode.toggled();
                Action::None
            }
            // Ctrl-Y cycles the OpenRouter service tier (Off → Flex → Priority).
            KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                st.service_tier = ServiceTier::cycled(st.service_tier);
                Action::None
            }
            KeyCode::Enter => Action::SaveSettings {
                base_url: st.base_url.clone(),
                orchestrator: st.orchestrator.clone(),
                translator: st.translator.clone(),
                reviewer: st.reviewer.clone(),
                // Env keys must not overwrite saved config.
                api_key: if st.api_key_env {
                    None
                } else {
                    Some(st.api_key.clone())
                },
                update_mode: st.update_mode,
                service_tier: st.service_tier,
                max_attempts: st.max_attempts_value(),
                loop_stall_secs: st.loop_stall_secs_value(),
                max_chapter_retranslates: st.max_chapter_retranslates_value(),
            },
            KeyCode::Tab | KeyCode::Down => {
                st.next_field();
                Action::None
            }
            KeyCode::Up | KeyCode::BackTab => {
                st.prev_field();
                Action::None
            }
            _ => {
                // The API-key field is masked, so it edits at the end only
                // (a positioned caret would be meaningless under masking).
                if st.field == SETTINGS_KEY_FIELD {
                    if st.api_key_env {
                        return Action::None; // env key is read-only
                    }
                    match key.code {
                        KeyCode::Backspace => {
                            st.api_key.pop();
                        }
                        KeyCode::Char(c)
                            if !key.modifiers.contains(KeyModifiers::CONTROL)
                                && !key.modifiers.contains(KeyModifiers::ALT) =>
                        {
                            st.api_key.push(c);
                        }
                        _ => {}
                    }
                    return Action::None;
                }
                let opts = EditOpts {
                    numeric_only: settings_numeric_field(st.field),
                    multiline: false,
                };
                let mut cursor = st.cursor;
                input::handle(st.field_mut(), &mut cursor, key, opts);
                st.cursor = cursor;
                Action::None
            }
        }
    }

    fn handle_theme_key(&mut self, key: KeyEvent) -> Action {
        let Overlay::Theme(st) = self else {
            return Action::None;
        };
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                st.sel = st.sel.saturating_sub(1);
                Action::PreviewTheme(st.current())
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if st.sel + 1 < ALL_THEMES.len() {
                    st.sel += 1;
                }
                Action::PreviewTheme(st.current())
            }
            KeyCode::Enter => Action::SaveTheme(st.current()),
            KeyCode::Esc | KeyCode::Char('q') => Action::CancelTheme,
            _ => Action::None,
        }
    }

    fn handle_palette_key(&mut self, key: KeyEvent) -> Action {
        let Overlay::Palette(st) = self else {
            return Action::None;
        };
        match input::handle(&mut st.query, &mut st.cursor, key, EditOpts::default()) {
            Edited::Changed => {
                st.sel = 0;
                return Action::None;
            }
            Edited::Moved => return Action::None,
            Edited::Ignored => {}
        }
        match key.code {
            KeyCode::Esc => Action::CloseOverlay,
            KeyCode::Up => {
                if st.sel > 0 {
                    st.sel -= 1;
                }
                Action::None
            }
            KeyCode::Down => {
                let n = st.matches().len();
                if n > 0 && st.sel + 1 < n {
                    st.sel += 1;
                }
                Action::None
            }
            KeyCode::Enter => {
                let matches = st.matches();
                if let Some(&idx) = matches.get(st.sel) {
                    st.items[idx].action.clone()
                } else {
                    Action::CloseOverlay
                }
            }
            _ => Action::None,
        }
    }

    fn handle_modal_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                let action = match self {
                    Overlay::Modal(dlg) => dlg.confirm.clone(),
                    _ => return Action::None,
                };
                // Confirm always dismisses before running wrapped actions.
                *self = Overlay::None;
                action
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => Action::CloseOverlay,
            KeyCode::Char(c) => {
                let action = match self {
                    Overlay::Modal(dlg)
                        if dlg
                            .alternate
                            .as_ref()
                            .is_some_and(|alt| alt.key.eq_ignore_ascii_case(&c)) =>
                    {
                        dlg.alternate.as_ref().map(|alt| alt.action.clone())
                    }
                    _ => None,
                };
                if let Some(action) = action {
                    *self = Overlay::None;
                    action
                } else {
                    Action::None
                }
            }
            _ => Action::None,
        }
    }

    fn handle_qa_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => Action::CloseOverlay,
            KeyCode::Down | KeyCode::Char('j') => {
                if let Overlay::Qa(st) = self {
                    let n = st.report.issues.len();
                    if n > 0 {
                        st.sel = (st.sel + 1).min(n - 1);
                    }
                }
                Action::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Overlay::Qa(st) = self {
                    st.sel = st.sel.saturating_sub(1);
                }
                Action::None
            }
            KeyCode::Enter => {
                // Unanchored notes have no chapter target.
                let target = match self {
                    Overlay::Qa(st) => st.report.issues.get(st.sel).and_then(|i| i.chapter),
                    _ => None,
                };
                match target {
                    Some(chapter) => {
                        *self = Overlay::None;
                        Action::OpenChapter { chapter }
                    }
                    None => Action::None,
                }
            }
            _ => Action::None,
        }
    }

    fn handle_reader_note_key(&mut self, key: KeyEvent) -> Action {
        let Overlay::ReaderNote(st) = self else {
            return Action::None;
        };
        if input::handle(&mut st.text, &mut st.cursor, key, EditOpts::default()) != Edited::Ignored {
            return Action::None;
        }
        match key.code {
            KeyCode::Esc => Action::CloseOverlay,
            KeyCode::Enter => Action::SaveReaderNote {
                chapter: st.chapter,
                line: st.line,
                note: st.text.clone(),
            },
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                st.text.clear();
                st.cursor = 0;
                Action::None
            }
            _ => Action::None,
        }
    }

    fn handle_reader_search_key(&mut self, key: KeyEvent) -> Action {
        let Overlay::ReaderSearch(st) = self else {
            return Action::None;
        };
        if input::handle(&mut st.query, &mut st.cursor, key, EditOpts::default()) != Edited::Ignored
        {
            return Action::None;
        }
        match key.code {
            KeyCode::Esc => Action::CloseOverlay,
            KeyCode::Enter => {
                if st.query.trim().is_empty() {
                    Action::CloseOverlay
                } else {
                    Action::ReaderSearch {
                        query: st.query.clone(),
                    }
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                st.query.clear();
                st.cursor = 0;
                Action::None
            }
            _ => Action::None,
        }
    }

    fn handle_reader_jump_key(&mut self, key: KeyEvent) -> Action {
        let Overlay::ReaderJump(st) = self else {
            return Action::None;
        };
        match input::handle(&mut st.query, &mut st.cursor, key, EditOpts::default()) {
            Edited::Changed => {
                st.sel = 0;
                return Action::None;
            }
            Edited::Moved => return Action::None,
            Edited::Ignored => {}
        }
        match key.code {
            KeyCode::Esc => Action::CloseOverlay,
            KeyCode::Up => {
                st.sel = st.sel.saturating_sub(1);
                Action::None
            }
            KeyCode::Down => {
                let n = st.matches().len();
                if n > 0 {
                    st.sel = (st.sel + 1).min(n - 1);
                }
                Action::None
            }
            KeyCode::Enter => {
                let target = st
                    .matches()
                    .get(st.sel)
                    .and_then(|&i| st.items.get(i))
                    .map(|t| (t.chapter, t.line));
                match target {
                    Some((chapter, line)) => {
                        *self = Overlay::None;
                        Action::OpenChapterAt { chapter, line }
                    }
                    None => Action::CloseOverlay,
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                st.query.clear();
                st.cursor = 0;
                st.sel = 0;
                Action::None
            }
            _ => Action::None,
        }
    }

    fn handle_synopsis_overlay_key(&mut self, key: KeyEvent) -> Action {
        let Overlay::Synopsis(st) = self else {
            return Action::None;
        };
        match handle_synopsis_keys(&mut st.syn, key) {
            SynKey::None => Action::None,
            SynKey::Translate => Action::TranslateSynopsis {
                raw: st.syn.raw.clone(),
                attempt: st.syn.attempt,
            },
            SynKey::Accept => Action::SaveSynopsis {
                raw: st.syn.raw.clone(),
                th: st.syn.th.clone(),
            },
            // Skip/back leave the stored synopsis untouched.
            SynKey::Skip | SynKey::Back => Action::CloseOverlay,
        }
    }

    pub fn hints(&self) -> &'static [(&'static str, &'static str)] {
        match self {
            Overlay::Welcome(_) => &[("↑↓", "move"), ("↵", "select"), ("Esc", "skip")],
            Overlay::Import(st) => match st.step {
                0 => &[
                    ("↑↓", "pick"),
                    ("↵", "next"),
                    ("r", "rescan"),
                    ("Esc", "cancel"),
                ],
                1 => &[("type", "name"), ("↵/Tab", "next"), ("Esc", "back")],
                2 => &[("↑↓/type", "volume"), ("↵", "next"), ("Esc", "back")],
                3 => synopsis_hints(&st.syn, true),
                _ => &[("Esc", "close")],
            },
            Overlay::Synopsis(st) => synopsis_hints(&st.syn, false),
            Overlay::Settings(_) => &[("Tab", "field"), ("type", "edit"), ("Esc/↵", "close")],
            Overlay::Theme(_) => &[("jk/↑↓", "preview"), ("↵", "apply"), ("Esc", "revert")],
            Overlay::Palette(_) => &[
                ("type", "filter"),
                ("↑↓", "move"),
                ("↵", "run"),
                ("Esc", "close"),
            ],
            Overlay::Log(_) => &[("jk", "scroll"), ("Esc/l", "close")],
            Overlay::Help(_) => &[("jk", "scroll"), ("Esc/?", "close")],
            Overlay::About => &[("Esc/↵", "close")],
            Overlay::Qa(_) => &[("jk", "move"), ("↵", "jump to chapter"), ("Esc", "close")],
            Overlay::ReaderNote(_) => &[("type", "note"), ("↵", "save"), ("Esc", "cancel")],
            Overlay::ReaderSearch(_) => &[("type", "query"), ("↵", "search"), ("Esc", "cancel")],
            Overlay::ReaderJump(_) => &[
                ("type", "filter"),
                ("↑↓", "move"),
                ("↵", "jump"),
                ("Esc", "close"),
            ],
            Overlay::Export(st) => {
                if st.done.is_some() {
                    &[("↵/Esc", "close")]
                } else if st.progress.is_some() {
                    &[("Esc", "close")]
                } else {
                    &[
                        ("↑↓", "move"),
                        ("Space", "toggle"),
                        ("↵", "export"),
                        ("Esc", "cancel"),
                    ]
                }
            }
            Overlay::Modal(dlg) if dlg.alternate.is_some() => {
                // Alternate-key modals need distinct footer labels.
                match dlg.alternate.as_ref().map(|alt| alt.key) {
                    // Recovery Esc/n defers; it does not discard the checkpoint.
                    Some('d') => &[("y/↵", "resume"), ("d", "discard"), ("n/Esc", "later")],
                    _ => &[("y/↵", "continue"), ("r", "restart"), ("n/Esc", "cancel")],
                }
            }
            Overlay::Modal(_) => &[("y", "confirm"), ("n/Esc", "cancel")],
            Overlay::None => &[],
        }
    }

    pub fn render(
        &self,
        f: &mut Frame,
        area: Rect,
        theme: &Theme,
        cfg: &AppConfig,
        log: &[(LogLevel, String)],
        frame: u64,
    ) {
        match self {
            Overlay::None => {}
            Overlay::Welcome(st) => self.render_welcome(f, area, theme, st),
            Overlay::Import(st) => self.render_import(f, area, theme, st),
            Overlay::Settings(st) => self.render_settings(f, area, theme, cfg, st),
            Overlay::Theme(st) => self.render_theme(f, area, theme, st),
            Overlay::Palette(st) => self.render_palette(f, area, theme, st),
            Overlay::Log(off) => self.render_log(f, area, theme, log, *off),
            Overlay::Help(off) => self.render_help(f, area, theme, *off),
            Overlay::About => self.render_about(f, area, theme, frame),
            Overlay::Modal(dlg) => self.render_modal(f, area, theme, dlg),
            Overlay::Synopsis(st) => self.render_synopsis(f, area, theme, st),
            Overlay::Qa(st) => self.render_qa(f, area, theme, st),
            Overlay::ReaderNote(st) => self.render_reader_note(f, area, theme, st),
            Overlay::ReaderSearch(st) => self.render_reader_search(f, area, theme, st),
            Overlay::ReaderJump(st) => self.render_reader_jump(f, area, theme, st),
            Overlay::Export(st) => self.render_export(f, area, theme, st),
        }
    }

    fn modal_block<'a>(&self, title: &'a str, theme: &Theme) -> Block<'a> {
        Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(theme.accent))
            .title(Span::styled(
                format!(" {title} "),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.bg_panel))
    }

    fn render_welcome(&self, f: &mut Frame, area: Rect, theme: &Theme, st: &WelcomeState) {
        let modal = centered_modal(76, 24, area);
        f.render_widget(Clear, modal);
        let block = self.modal_block("ようこそ · Welcome to honya 本屋", theme);
        let inner = block.inner(modal);
        f.render_widget(block, modal);

        let dim = Style::default().fg(theme.ink_faint);
        let soft = Style::default().fg(theme.ink_soft);
        let accent = Style::default().fg(theme.accent);

        let mut lines: Vec<Line> = vec![
            Line::raw(""),
            Line::from(Span::styled(
                "  AI-assisted Japanese → Thai light-novel translation.",
                soft,
            )),
            Line::raw(""),
            Line::from(Span::styled("  The five screens (1–5 / Tab):", dim)),
        ];
        let screens = [
            ("1", "書架 Shelf", "import files · pick a project"),
            ("2", "棚 Project", "chapters · queue · run translation"),
            ("3", "訳 Translate", "watch the live 3-agent pipeline"),
            ("4", "読 Reader", "read JA ↔ TH side by side"),
            ("5", "辞 Lexicon", "glossary · characters · style"),
        ];
        for (num, name, desc) in screens {
            lines.push(Line::from(vec![
                Span::styled(format!("   {num} "), accent),
                Span::styled(format!("{name:<14}"), Style::default().fg(theme.ink)),
                Span::styled(desc, soft),
            ]));
        }
        lines.push(Line::raw(""));

        let sample_label = if st.sample_exists {
            "Open the sample project".to_string()
        } else {
            "Create a sample project".to_string()
        };
        let key_status = if st.api_key_present {
            ("✓ key configured", theme.status_done)
        } else {
            ("needed to translate", theme.status_warn)
        };
        let items: [(String, Vec<Span>); WELCOME_ITEMS] = [
            (
                sample_label,
                vec![Span::styled(" — explore offline, no API key needed", dim)],
            ),
            ("Import a file".to_string(), vec![]),
            (
                "Set OpenRouter API key".to_string(),
                vec![
                    Span::styled("  ", dim),
                    Span::styled(key_status.0, Style::default().fg(key_status.1)),
                ],
            ),
            ("Skip — I'll explore on my own".to_string(), vec![]),
        ];
        for (i, (label, suffix)) in items.into_iter().enumerate() {
            let selected = i == st.sel;
            let bar = if selected { theme::SELECT_BAR } else { ' ' };
            let label_style = if selected {
                Style::default()
                    .fg(theme.ink)
                    .bg(theme.accent_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.ink)
            };
            let mut spans = vec![
                Span::styled(format!("  {bar} "), accent),
                Span::styled(label, label_style),
            ];
            spans.extend(suffix);
            lines.push(Line::from(spans));
        }

        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .style(Style::default().bg(theme.bg_panel)),
            inner,
        );
    }

    fn render_import(&self, f: &mut Frame, area: Rect, theme: &Theme, st: &ImportState) {
        // One fixed size for every step; mirrored by modal_rect for hit-testing.
        let modal = centered_modal(78, 24, area);
        f.render_widget(Clear, modal);
        let title = thai_display_safe(if st.lock_name {
            "Add volume · เพิ่มเล่ม"
        } else {
            "New project · นำเข้าไฟล์"
        });
        let block = self.modal_block(&title, theme);
        let inner = block.inner(modal);
        f.render_widget(block, modal);

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // step rail
                Constraint::Length(1), // accumulated choices
                Constraint::Length(1), // gap
                Constraint::Min(0),    // step body
            ])
            .split(inner);
        f.render_widget(
            Paragraph::new(step_rail(st, theme)).style(Style::default().bg(theme.bg_panel)),
            rows[0],
        );
        f.render_widget(
            Paragraph::new(import_context_line(st, theme))
                .style(Style::default().bg(theme.bg_panel)),
            rows[1],
        );

        match st.step {
            0 => self.render_import_pick(f, rows[3], theme, st),
            1 => self.render_import_name(f, rows[3], theme, st),
            2 => self.render_import_volume(f, rows[3], theme, st),
            3 => render_synopsis_body(f, rows[3], theme, &st.syn, "เริ่มนำเข้า"),
            _ => self.render_import_progress(f, rows[3], theme, st),
        }
    }

    /// Standalone synopsis editor modal (re-opened from the Project screen).
    fn render_synopsis(&self, f: &mut Frame, area: Rect, theme: &Theme, st: &SynopsisEditState) {
        let modal = centered_modal(76, 24, area);
        f.render_widget(Clear, modal);
        let title = thai_display_safe(&format!(
            "เรื่องย่อเล่ม — Vol.{:02} · {}",
            st.vol,
            truncate_cols(st.title.trim(), 40)
        ));
        let block = self.modal_block(&title, theme);
        let inner = block.inner(modal);
        f.render_widget(block, modal);
        render_synopsis_body(f, inner, theme, &st.syn, "บันทึก");
    }

    fn render_reader_note(&self, f: &mut Frame, area: Rect, theme: &Theme, st: &ReaderNoteState) {
        let modal = centered_modal(72, 14, area);
        f.render_widget(Clear, modal);
        let block = self.modal_block("Reader note · proofreading", theme);
        let inner = block.inner(modal);
        f.render_widget(block, modal);

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // anchor
                Constraint::Length(1), // label
                Constraint::Length(3), // input box
                Constraint::Length(1), // examples label
                Constraint::Min(0),    // examples
            ])
            .split(inner);

        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  Anchor  ", Style::default().fg(theme.ink_faint)),
                Span::styled(
                    format!("ch {:03} · translated line {}", st.chapter, st.line),
                    Style::default().fg(theme.accent_soft),
                ),
            ]))
            .style(Style::default().bg(theme.bg_panel)),
            rows[0],
        );

        f.render_widget(
            Paragraph::new(Span::styled("  Note", Style::default().fg(theme.ink_soft)))
                .style(Style::default().bg(theme.bg_panel)),
            rows[1],
        );

        let input_block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(theme.accent_soft))
            .style(Style::default().bg(theme.bg_inset));
        let input = if st.text.is_empty() {
            Line::from(vec![
                Span::styled(
                    "awkward phrasing / check honorific / rename skill term / review tone",
                    Style::default().fg(theme.ink_faint),
                ),
                Span::styled("▏", Style::default().fg(theme.stream_cursor)),
            ])
        } else {
            let (before, after) =
                input::caret_halves(&st.text, st.cursor, rows[2].width.saturating_sub(6) as usize);
            Line::from(vec![
                Span::styled(before, Style::default().fg(theme.ink)),
                Span::styled("▏", Style::default().fg(theme.stream_cursor)),
                Span::styled(after, Style::default().fg(theme.ink)),
            ])
        };
        f.render_widget(Paragraph::new(input).block(input_block), indent(rows[2], 2));

        f.render_widget(
            Paragraph::new(Span::styled(
                "  Examples",
                Style::default().fg(theme.ink_faint),
            ))
            .style(Style::default().bg(theme.bg_panel)),
            rows[3],
        );
        let examples = vec![
            Line::from(Span::styled(
                "  • awkward phrasing      • check honorific",
                Style::default().fg(theme.ink_soft),
            )),
            Line::from(Span::styled(
                "  • rename skill term     • review tone",
                Style::default().fg(theme.ink_soft),
            )),
            Line::from(Span::styled(
                "  Enter saves inline; Esc cancels.",
                Style::default().fg(theme.ink_faint),
            )),
        ];
        f.render_widget(
            Paragraph::new(examples).style(Style::default().bg(theme.bg_panel)),
            rows[4],
        );
    }

    fn render_reader_search(
        &self,
        f: &mut Frame,
        area: Rect,
        theme: &Theme,
        st: &ReaderSearchState,
    ) {
        let modal = centered_modal(64, 7, area);
        f.render_widget(Clear, modal);
        let block = self.modal_block("Search · ค้นหา (JA + TH)", theme);
        let inner = block.inner(modal);
        f.render_widget(block, modal);

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // label
                Constraint::Length(3), // input box
                Constraint::Min(0),    // hint
            ])
            .split(inner);

        f.render_widget(
            Paragraph::new(Span::styled(
                "  Find across both panes",
                Style::default().fg(theme.ink_soft),
            ))
            .style(Style::default().bg(theme.bg_panel)),
            rows[0],
        );

        let input_block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(theme.accent_soft))
            .style(Style::default().bg(theme.bg_inset));
        let input = if st.query.is_empty() {
            Line::from(vec![
                Span::styled(
                    "聖剣 · ตัวละคร · a phrase to locate…",
                    Style::default().fg(theme.ink_faint),
                ),
                Span::styled("▏", Style::default().fg(theme.stream_cursor)),
            ])
        } else {
            let (before, after) =
                input::caret_halves(&st.query, st.cursor, rows[1].width.saturating_sub(6) as usize);
            Line::from(vec![
                Span::styled(before, Style::default().fg(theme.ink)),
                Span::styled("▏", Style::default().fg(theme.stream_cursor)),
                Span::styled(after, Style::default().fg(theme.ink)),
            ])
        };
        f.render_widget(Paragraph::new(input).block(input_block), indent(rows[1], 2));

        f.render_widget(
            Paragraph::new(Span::styled(
                "  Enter searches; then  >  next  ·  <  prev  ·  Esc clears.",
                Style::default().fg(theme.ink_faint),
            ))
            .style(Style::default().bg(theme.bg_panel)),
            rows[2],
        );
    }

    fn render_reader_jump(&self, f: &mut Frame, area: Rect, theme: &Theme, st: &ReaderJumpState) {
        let modal = centered_modal(72, 24, area);
        f.render_widget(Clear, modal);
        let title = if st.title.is_empty() {
            "Jump · ไปยัง".to_string()
        } else {
            format!("Jump · {}", st.title)
        };
        let block = self.modal_block(&title, theme);
        let inner = block.inner(modal);
        f.render_widget(block, modal);

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(0)])
            .split(inner);

        let (before, after) =
            input::caret_halves(&st.query, st.cursor, rows[0].width.saturating_sub(5) as usize);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  / ", Style::default().fg(theme.accent)),
                Span::styled(before, Style::default().fg(theme.ink)),
                Span::styled("▏", Style::default().fg(theme.stream_cursor)),
                Span::styled(after, Style::default().fg(theme.ink)),
            ]))
            .style(Style::default().bg(theme.bg_panel)),
            rows[0],
        );

        let matches = st.matches();
        if matches.is_empty() {
            f.render_widget(
                Paragraph::new(Span::styled(
                    "   no matches",
                    Style::default().fg(theme.ink_faint),
                ))
                .style(Style::default().bg(theme.bg_panel)),
                rows[1],
            );
            return;
        }

        // Window the rows so the selection stays visible (theme/QA pattern).
        let cap = (rows[1].height as usize).max(1);
        let sel = st.sel.min(matches.len() - 1);
        let start = if sel >= cap { sel + 1 - cap } else { 0 };
        let end = (start + cap).min(matches.len());
        let width = rows[1].width.saturating_sub(6) as usize;

        let mut lines = Vec::with_capacity(end - start);
        for (row, &idx) in matches.iter().enumerate().take(end).skip(start) {
            let item = &st.items[idx];
            let selected = row == sel;
            let bar = if selected {
                theme::SELECT_BAR.to_string()
            } else {
                " ".to_string()
            };
            let (glyph, glyph_color) = match item.kind {
                JumpKind::Chapter => ("▣", theme.accent_soft),
                JumpKind::Section => ("§", theme.ink_soft),
                JumpKind::Bookmark => ("★", theme.status_warn),
            };
            let label_style = if selected {
                Style::default().fg(theme.ink).bg(theme.accent_bg)
            } else {
                Style::default().fg(theme.ink_soft)
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {bar} "), Style::default().fg(theme.accent)),
                Span::styled(format!("{glyph} "), Style::default().fg(glyph_color)),
                Span::styled(truncate_cols(&item.label, width), label_style),
            ]));
        }
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme.bg_panel)),
            rows[1],
        );
    }

    fn render_import_pick(&self, f: &mut Frame, area: Rect, theme: &Theme, st: &ImportState) {
        if st.files.is_empty() {
            let p = Paragraph::new(vec![
                Line::raw(""),
                Line::from(Span::styled(
                    "  No importable files found in this folder.",
                    Style::default().fg(theme.ink_soft),
                )),
                Line::raw(""),
                Line::from(Span::styled(
                    "  Drop a supported file into this folder, then press r to rescan.",
                    Style::default().fg(theme.ink_faint),
                )),
                Line::raw(""),
                Line::from(Span::styled(
                    format!(
                        "  Supported: {}",
                        crate::document_import::supported_import_summary()
                    ),
                    Style::default().fg(theme.ink_faint),
                )),
            ])
            .style(Style::default().bg(theme.bg_panel));
            f.render_widget(p, area);
            return;
        }

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // header
                Constraint::Length(1), // gap
                Constraint::Min(0),    // windowed file list
            ])
            .split(area);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    "  Choose a source file",
                    Style::default().fg(theme.ink_soft),
                ),
                Span::styled(
                    format!("  ({} found · r rescan)", st.files.len()),
                    Style::default().fg(theme.ink_faint),
                ),
            ]))
            .style(Style::default().bg(theme.bg_panel)),
            rows[0],
        );

        // Window the rows so the selection stays visible with long file lists.
        let cap = rows[2].height.max(1);
        let start = windowed_start(st.sel, cap);
        let end = (start + cap as usize).min(st.files.len());
        let size_w = 9usize;
        let name_w = (rows[2].width as usize).saturating_sub(6 + size_w);

        let mut lines = Vec::with_capacity(end - start);
        for (i, (p, size)) in st.files.iter().enumerate().take(end).skip(start) {
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("?");
            let selected = i == st.sel;
            let bar = if selected {
                theme::SELECT_BAR.to_string()
            } else {
                " ".to_string()
            };
            let style = if selected {
                Style::default().fg(theme.ink).bg(theme.accent_bg)
            } else {
                Style::default().fg(theme.ink_soft)
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {bar} "), Style::default().fg(theme.accent)),
                Span::styled(pad_to_cols(&thai_display_safe(name), name_w), style),
                Span::styled(
                    format!("{:>size_w$}", super::shelf::human_size(*size)),
                    Style::default().fg(theme.ink_faint),
                ),
            ]));
        }
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme.bg_panel)),
            rows[2],
        );
    }

    fn render_import_name(&self, f: &mut Frame, area: Rect, theme: &Theme, st: &ImportState) {
        let slug = slugify(st.name.trim());
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // label
                Constraint::Length(3), // boxed input
                Constraint::Length(1), // folder preview
                Constraint::Length(1), // gap
                Constraint::Min(0),    // validation / merge feedback
            ])
            .split(area);

        f.render_widget(
            Paragraph::new(Span::styled(
                "  Project name",
                Style::default().fg(theme.ink_soft),
            ))
            .style(Style::default().bg(theme.bg_panel)),
            rows[0],
        );

        let input_block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(theme.accent_soft))
            .style(Style::default().bg(theme.bg_inset));
        let field_w = rows[1].width.saturating_sub(6) as usize;
        let (before, after) = input::caret_halves(&st.name, st.name_cursor, field_w);
        let caret_line = Line::from(vec![
            Span::styled(thai_display_safe(&before), Style::default().fg(theme.ink)),
            Span::styled("▏", Style::default().fg(theme.stream_cursor)),
            Span::styled(thai_display_safe(&after), Style::default().fg(theme.ink)),
        ]);
        f.render_widget(
            Paragraph::new(caret_line).block(input_block),
            indent(rows[1], 2),
        );

        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  Folder   ", Style::default().fg(theme.ink_faint)),
                Span::styled(
                    if slug.is_empty() {
                        "—".to_string()
                    } else {
                        thai_display_safe(&format!("./{slug}/"))
                    },
                    Style::default().fg(theme.accent_soft),
                ),
            ]))
            .style(Style::default().bg(theme.bg_panel)),
            rows[2],
        );

        // Live feedback: required-name nudge, or what this name will do (create
        // a fresh project vs merge into the existing one with the same slug).
        let mut feedback: Vec<Line> = Vec::new();
        if let Some(note) = st.note {
            feedback.push(Line::from(Span::styled(
                thai_display_safe(&format!("  ⚠ {note}")),
                Style::default().fg(theme.status_warn),
            )));
        } else if st.name.trim().is_empty() {
            feedback.push(Line::from(Span::styled(
                "  type a project name to continue",
                Style::default().fg(theme.ink_faint),
            )));
        } else if let Some(target) = st.target_project() {
            feedback.push(Line::from(Span::styled(
                thai_display_safe(&format!(
                    "  ⊕ adds into the existing project “{}”",
                    truncate_cols(target.title.trim(), 40)
                )),
                Style::default().fg(theme.status_warn),
            )));
            feedback.push(Line::from(Span::styled(
                format!("    already has {}", volume_chips(&target.volumes)),
                Style::default().fg(theme.ink_faint),
            )));
        } else {
            feedback.push(Line::from(Span::styled(
                "  ✓ creates a new project",
                Style::default().fg(theme.status_done),
            )));
        }
        f.render_widget(
            Paragraph::new(feedback)
                .wrap(Wrap { trim: false })
                .style(Style::default().bg(theme.bg_panel)),
            rows[4],
        );
    }

    fn render_import_volume(&self, f: &mut Frame, area: Rect, theme: &Theme, st: &ImportState) {
        let target = st.target_project();
        let existing = target.map(|t| t.volumes.as_slice()).unwrap_or(&[]);
        let collides = existing.iter().any(|&(n, _)| n == st.vol);

        let mut lines = vec![
            Line::from(vec![
                Span::styled("  Project   ", Style::default().fg(theme.ink_faint)),
                Span::styled(
                    thai_display_safe(st.name.trim()),
                    Style::default().fg(theme.ink_soft),
                ),
                Span::styled(
                    if target.is_some() {
                        "  (existing)"
                    } else {
                        "  (new)"
                    },
                    Style::default().fg(theme.ink_faint),
                ),
            ]),
            Line::raw(""),
            Line::from(vec![
                Span::styled("  Volume    ", Style::default().fg(theme.ink_faint)),
                Span::styled("◂  ", Style::default().fg(theme.accent_soft)),
                Span::styled(
                    format!("Vol.{:02}", st.vol),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("  ▸", Style::default().fg(theme.accent_soft)),
            ]),
            Line::raw(""),
            Line::from(vec![
                Span::styled("  Existing  ", Style::default().fg(theme.ink_faint)),
                Span::styled(
                    if existing.is_empty() {
                        "none — this is the project's first volume".to_string()
                    } else {
                        volume_chips(existing)
                    },
                    Style::default().fg(theme.ink_soft),
                ),
            ]),
            Line::raw(""),
        ];
        if collides {
            lines.push(Line::from(Span::styled(
                format!(
                    "  ⚠ Vol.{:02} already exists — imported chapters are added into it",
                    st.vol
                ),
                Style::default().fg(theme.status_warn),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                format!("  ✓ creates a new volume (Vol_{:02})", st.vol),
                Style::default().fg(theme.status_done),
            )));
        }
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "  ↑↓ / + - adjust · type a number · Enter to continue",
            Style::default().fg(theme.ink_faint),
        )));
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme.bg_panel)),
            area,
        );
    }

    fn render_import_progress(&self, f: &mut Frame, area: Rect, theme: &Theme, st: &ImportState) {
        let (done, total, label) = st
            .progress
            .clone()
            .unwrap_or((0, 0, "preparing".to_string()));
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Length(1), // label
                Constraint::Length(1), // gauge
                Constraint::Length(2), // gap
                Constraint::Min(0),    // background note
            ])
            .split(area);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  Preprocessing  ", Style::default().fg(theme.ink_soft)),
                Span::styled(
                    thai_display_safe(&label),
                    Style::default().fg(theme.accent_soft),
                ),
            ]))
            .style(Style::default().bg(theme.bg_panel)),
            rows[1],
        );
        render_gauge(f, indent(rows[2], 2), done, total.max(1), theme);
        f.render_widget(
            Paragraph::new(Span::styled(
                "  Esc closes this dialog — the import keeps running in the background.",
                Style::default().fg(theme.ink_faint),
            ))
            .style(Style::default().bg(theme.bg_panel)),
            rows[4],
        );
    }

    fn render_export(&self, f: &mut Frame, area: Rect, theme: &Theme, st: &ExportState) {
        let modal = centered_modal(66, 15, area);
        f.render_widget(Clear, modal);
        let title = format!("Export volume — Vol.{:02}", st.vol);
        let block = self.modal_block(&title, theme);
        let inner = block.inner(modal);
        f.render_widget(block, modal);

        if let Some((paths, warnings)) = st.done.as_ref() {
            self.render_export_done(f, inner, theme, paths, warnings);
        } else if let Some((done, total, label)) = st.progress.as_ref() {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(2),
                    Constraint::Length(1), // label
                    Constraint::Length(1), // gauge
                    Constraint::Min(0),
                ])
                .split(inner);
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("  Writing  ", Style::default().fg(theme.ink_soft)),
                    Span::styled(label.clone(), Style::default().fg(theme.accent_soft)),
                ]))
                .style(Style::default().bg(theme.bg_panel)),
                rows[1],
            );
            render_gauge(f, indent(rows[2], 2), *done, (*total).max(1), theme);
        } else {
            self.render_export_pick(f, inner, theme, st);
        }
    }

    fn render_export_pick(&self, f: &mut Frame, area: Rect, theme: &Theme, st: &ExportState) {
        let mut lines = vec![
            Line::from(Span::styled(
                "  Choose formats, then ↵ to export:",
                Style::default().fg(theme.ink_soft),
            )),
            Line::from(""),
        ];
        for (i, fmt) in ExportFormat::ALL.iter().enumerate() {
            let on = st.formats[i];
            let selected = i == st.sel;
            let checkbox = if on { "[x]" } else { "[ ]" };
            let name_style = if selected {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else if on {
                Style::default().fg(theme.ink)
            } else {
                Style::default().fg(theme.ink_faint)
            };
            lines.push(Line::from(vec![
                Span::styled(
                    if selected { "  › " } else { "    " },
                    Style::default().fg(theme.accent),
                ),
                Span::styled(
                    format!("{checkbox} "),
                    Style::default().fg(if on {
                        theme.status_done
                    } else {
                        theme.ink_faint
                    }),
                ),
                Span::styled(format!("{:<9}", fmt.label()), name_style),
                Span::styled(export_desc(*fmt), Style::default().fg(theme.ink_faint)),
            ]));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  written to the project's exports/ folder",
            Style::default().fg(theme.ink_faint),
        )));
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme.bg_panel)),
            area,
        );
    }

    fn render_export_done(
        &self,
        f: &mut Frame,
        area: Rect,
        theme: &Theme,
        paths: &[PathBuf],
        warnings: &[String],
    ) {
        let mut lines = vec![Line::from(Span::styled(
            format!("  ✓ wrote {} file(s):", paths.len()),
            Style::default().fg(theme.status_done),
        ))];
        for p in paths {
            let name = p
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            lines.push(Line::from(Span::styled(
                format!("    {name}"),
                Style::default().fg(theme.ink),
            )));
        }
        lines.push(Line::from(""));
        if warnings.is_empty() {
            lines.push(Line::from(Span::styled(
                "  no warnings",
                Style::default().fg(theme.ink_faint),
            )));
        } else {
            let shown = warnings.len().min(4);
            lines.push(Line::from(Span::styled(
                format!("  ! {} warning(s):", warnings.len()),
                Style::default().fg(theme.status_warn),
            )));
            for w in warnings.iter().take(shown) {
                lines.push(Line::from(Span::styled(
                    format!("    {}", thai_display_safe(w)),
                    Style::default().fg(theme.ink_soft),
                )));
            }
            if warnings.len() > shown {
                lines.push(Line::from(Span::styled(
                    format!("    … +{} more (see activity log)", warnings.len() - shown),
                    Style::default().fg(theme.ink_faint),
                )));
            }
        }
        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .style(Style::default().bg(theme.bg_panel)),
            area,
        );
    }

    fn render_settings(
        &self,
        f: &mut Frame,
        area: Rect,
        theme: &Theme,
        cfg: &AppConfig,
        st: &SettingsState,
    ) {
        let modal = centered_modal(72, 26, area);
        f.render_widget(Clear, modal);
        let block = self.modal_block("Settings", theme);
        let inner = block.inner(modal);
        f.render_widget(block, modal);

        let val_w = area.width.saturating_sub(26) as usize;
        // `caret` positions the bar inside the value (None → caret at the end,
        // used for the masked API-key field where mid-string position is moot).
        let field_line = |label: &str, value: String, focused: bool, caret: Option<usize>| -> Line<'static> {
            let marker = if focused { theme::SELECT_BAR } else { ' ' };
            let value_style = if focused {
                Style::default().fg(theme.ink).bg(theme.accent_bg)
            } else {
                Style::default().fg(theme.ink_soft)
            };
            let mut spans = vec![
                Span::styled(format!(" {marker} "), Style::default().fg(theme.accent)),
                Span::styled(format!("{label:<20}"), Style::default().fg(theme.ink_faint)),
            ];
            match (focused, caret) {
                (true, Some(cursor)) => {
                    let (before, after) = input::caret_halves(&value, cursor, val_w);
                    spans.push(Span::styled(before, value_style));
                    spans.push(Span::styled("▏", Style::default().fg(theme.stream_cursor)));
                    spans.push(Span::styled(after, value_style));
                }
                (true, None) => {
                    spans.push(Span::styled(truncate_cols(&value, val_w), value_style));
                    spans.push(Span::styled("▏", Style::default().fg(theme.stream_cursor)));
                }
                (false, _) => {
                    spans.push(Span::styled(truncate_cols(&value, val_w), value_style));
                }
            }
            Line::from(spans)
        };

        let fields = [
            ("Base URL", st.base_url.as_str(), 0u8),
            ("Orchestrator model", st.orchestrator.as_str(), 1),
            ("Translator model", st.translator.as_str(), 2),
            ("Reviewer model", st.reviewer.as_str(), 3),
        ];
        let mut lines = vec![Line::raw("")];
        for (label, value, idx) in fields {
            lines.push(field_line(label, value.to_string(), st.field == idx, Some(st.cursor)));
        }

        let focused_key = st.field == SETTINGS_KEY_FIELD;
        if st.api_key_env {
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", if focused_key { theme::SELECT_BAR } else { ' ' }),
                    Style::default().fg(theme.accent),
                ),
                Span::styled("API key             ", Style::default().fg(theme.ink_faint)),
                Span::styled("● via environment", Style::default().fg(theme.status_done)),
                Span::styled(" (read-only)", Style::default().fg(theme.ink_faint)),
            ]));
        } else {
            let shown = if st.api_key.trim().is_empty() {
                "— not set —".to_string()
            } else {
                mask_secret(&st.api_key)
            };
            lines.push(field_line("API key", shown, focused_key, None));
        }

        lines.push(field_line(
            "Retry attempts",
            st.max_attempts.clone(),
            st.field == SETTINGS_RETRIES_FIELD,
            Some(st.cursor),
        ));
        lines.push(Line::from(Span::styled(
            "      ↳ Translator↔Reviewer loop per chunk (1–20)",
            Style::default().fg(theme.ink_faint),
        )));

        lines.push(field_line(
            "Loop watchdog (s)",
            st.loop_stall_secs.clone(),
            st.field == SETTINGS_STALL_FIELD,
            Some(st.cursor),
        ));
        lines.push(Line::from(Span::styled(
            "      ↳ stuck/looping chapter re-translated after N s (0 = off)",
            Style::default().fg(theme.ink_faint),
        )));
        lines.push(field_line(
            "Loop re-translates",
            st.max_chapter_retranslates.clone(),
            st.field == SETTINGS_RETRANSLATE_FIELD,
            Some(st.cursor),
        ));
        lines.push(Line::from(Span::styled(
            "      ↳ whole-chapter re-translates before the run aborts (0–10)",
            Style::default().fg(theme.ink_faint),
        )));

        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled(
                "   Theme               ",
                Style::default().fg(theme.ink_faint),
            ),
            Span::styled(cfg.theme.label(), Style::default().fg(theme.accent)),
            Span::styled("   Ctrl-T to change", Style::default().fg(theme.ink_faint)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                "   Auto-update         ",
                Style::default().fg(theme.ink_faint),
            ),
            Span::styled(st.update_mode.label(), Style::default().fg(theme.accent)),
            Span::styled("   Ctrl-U to toggle", Style::default().fg(theme.ink_faint)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                "   Service tier         ",
                Style::default().fg(theme.ink_faint),
            ),
            Span::styled(
                ServiceTier::label(st.service_tier),
                Style::default().fg(theme.accent),
            ),
            Span::styled("   Ctrl-Y to cycle", Style::default().fg(theme.ink_faint)),
        ]));
        lines.push(Line::raw(""));
        let footer = if st.api_key_env {
            "   Key from HONYA_API_KEY / OPENROUTER_API_KEY · ↵ save · Esc close"
        } else {
            "   Paste an OpenRouter key (sk-or-…); saved to config.json · ↵ save"
        };
        lines.push(Line::from(Span::styled(
            footer,
            Style::default().fg(theme.ink_faint),
        )));
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme.bg_panel)),
            inner,
        );
    }

    /// Render the theme picker: a name list plus a swatch row of the focused
    /// theme's key colors. Drawn with the live `theme`, so the modal recolors too.
    fn render_theme(&self, f: &mut Frame, area: Rect, theme: &Theme, st: &ThemePickerState) {
        let modal = centered_modal(60, 20, area);
        f.render_widget(Clear, modal);
        let block = self.modal_block("Theme 配色", theme);
        let inner = block.inner(modal);
        f.render_widget(block, modal);

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(2)])
            .split(inner);

        // Windowed so the selected row stays visible when the modal is clamped short.
        let cap = (rows[0].height as usize).max(1);
        let start = if st.sel >= cap { st.sel + 1 - cap } else { 0 };
        let end = (start + cap).min(ALL_THEMES.len());
        let mut lines = Vec::with_capacity(end - start);
        for (i, id) in ALL_THEMES.iter().enumerate().take(end).skip(start) {
            let selected = i == st.sel;
            let bar = if selected {
                theme::SELECT_BAR.to_string()
            } else {
                " ".to_string()
            };
            let name_style = if selected {
                Style::default().fg(theme.ink).bg(theme.accent_bg)
            } else {
                Style::default().fg(theme.ink_soft)
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {bar} "), Style::default().fg(theme.accent)),
                Span::styled(format!("{:<22}", id.label()), name_style),
                Span::styled(
                    format!(" {}", id.tone()),
                    Style::default().fg(theme.ink_faint),
                ),
            ]));
        }
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme.bg_panel)),
            rows[0],
        );

        let swatch = |glyph: &str, color, label: &str| -> Vec<Span<'static>> {
            vec![
                Span::styled(format!(" {glyph} "), Style::default().fg(color)),
                Span::styled(format!("{label}  "), Style::default().fg(theme.ink_faint)),
            ]
        };
        let mut chips = Vec::new();
        chips.extend(swatch("●", theme.accent, "accent"));
        chips.extend(swatch("◐", theme.status_working, "live"));
        chips.extend(swatch("●", theme.status_done, "done"));
        chips.extend(swatch("✗", theme.status_failed, "fail"));
        chips.extend(swatch("‖", theme.status_warn, "warn"));
        chips.extend(swatch("▣", theme.status_image, "img"));
        f.render_widget(
            Paragraph::new(vec![Line::raw(""), Line::from(chips)])
                .style(Style::default().bg(theme.bg_panel)),
            rows[1],
        );
    }

    fn render_palette(&self, f: &mut Frame, area: Rect, theme: &Theme, st: &PaletteState) {
        let modal = centered_modal(60, 16, area);
        f.render_widget(Clear, modal);
        let block = self.modal_block("Command palette", theme);
        let inner = block.inner(modal);
        f.render_widget(block, modal);

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(0)])
            .split(inner);

        let (before, after) =
            input::caret_halves(&st.query, st.cursor, rows[0].width.saturating_sub(5) as usize);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  : ", Style::default().fg(theme.accent)),
                Span::styled(before, Style::default().fg(theme.ink)),
                Span::styled("▏", Style::default().fg(theme.stream_cursor)),
                Span::styled(after, Style::default().fg(theme.ink)),
            ]))
            .style(Style::default().bg(theme.bg_panel)),
            rows[0],
        );

        let matches = st.matches();
        let mut lines = Vec::new();
        for (row, &idx) in matches.iter().enumerate() {
            let selected = row == st.sel;
            let bar = if selected {
                theme::SELECT_BAR.to_string()
            } else {
                " ".to_string()
            };
            let style = if selected {
                Style::default().fg(theme.ink).bg(theme.accent_bg)
            } else {
                Style::default().fg(theme.ink_soft)
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {bar} "), Style::default().fg(theme.accent)),
                Span::styled(st.items[idx].label, style),
            ]));
        }
        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                "   no matches",
                Style::default().fg(theme.ink_faint),
            )));
        }
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme.bg_panel)),
            rows[1],
        );
    }

    fn render_log(
        &self,
        f: &mut Frame,
        area: Rect,
        theme: &Theme,
        log: &[(LogLevel, String)],
        off: u16,
    ) {
        let modal = centered_pct(80, 80, area);
        f.render_widget(Clear, modal);
        let block = self.modal_block("Activity log", theme);
        let inner = block.inner(modal);
        f.render_widget(block, modal);

        // Window of `cap` entries ending `off` rows back from the newest (off=0
        // shows the tail; k scrolls back into history).
        let cap = inner.height as usize;
        let max_off = log.len().saturating_sub(cap);
        let off = (off as usize).min(max_off);
        let end = log.len().saturating_sub(off);
        let start = end.saturating_sub(cap);
        let mut lines = Vec::new();
        for (level, msg) in &log[start..end] {
            let (glyph, color) = match level {
                LogLevel::Trace => ("·", theme.ink_faint),
                LogLevel::Info => ("✓", theme.status_done),
                LogLevel::Warn => ("!", theme.status_warn),
                LogLevel::Error => ("✗", theme.status_failed),
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {glyph} "), Style::default().fg(color)),
                Span::styled(
                    truncate_cols(
                        &crate::ui::text::thai_display_safe(msg),
                        inner.width.saturating_sub(4) as usize,
                    ),
                    Style::default().fg(theme.ink_soft),
                ),
            ]));
        }
        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                "   (no activity yet)",
                Style::default().fg(theme.ink_faint),
            )));
        }
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme.bg_panel)),
            inner,
        );
    }

    fn render_help(&self, f: &mut Frame, area: Rect, theme: &Theme, off: u16) {
        let modal = centered_modal(72, 24, area);
        f.render_widget(Clear, modal);
        let block = self.modal_block("Help — keybindings", theme);
        let inner = block.inner(modal);
        f.render_widget(block, modal);

        // 2-column key table, grouped by section.
        let groups: &[(&str, &[(&str, &str)])] = &[
            (
                "Global",
                &[
                    ("1–5 / Tab", "switch primary tab"),
                    ("?", "toggle this help"),
                    (": / Ctrl-P", "command palette (→ Getting started)"),
                    ("Ctrl-T", "theme picker"),
                    ("` / l", "activity log (Project keeps l)"),
                    ("Esc / Backspace", "close overlay / dismiss toast"),
                    ("Mouse", "click tabs/rows · wheel scrolls · dbl-click opens"),
                    ("q", "quit        Ctrl-C hard quit"),
                ],
            ),
            (
                "Shelf 書架",
                &[
                    ("↵", "open project"),
                    ("i", "import file"),
                    ("d / R / r", "delete · rename · rescan"),
                ],
            ),
            (
                "Project 棚",
                &[
                    ("↵", "read chapter"),
                    ("t / a", "translate marked/current"),
                    ("T", "whole volume"),
                    ("Space", "mark chapter"),
                    ("h / l", "collapse · expand / focus"),
                    ("e", "edit context (Lexicon)"),
                    ("y", "volume synopsis (translate/reroll)"),
                    ("V", "add volume (import wizard)"),
                    ("Q", "QA review (flagged issues)"),
                ],
            ),
            (
                "Translate 訳",
                &[
                    ("p / s", "pause · stop"),
                    ("f", "toggle follow-streaming"),
                    ("c", "cycle focused agent"),
                    ("↵", "open result in Reader"),
                ],
            ),
            (
                "Reader 読",
                &[
                    ("jk / ↑↓", "scroll (synced)"),
                    ("[ ]", "prev · next chapter"),
                    ("z / w / o", "sync · wrap · layout"),
                    ("/  > <", "search JA+TH · next · prev match"),
                    ("g", "jump (chapters · sections · marks)"),
                    ("G", "toggle glossary highlight"),
                    ("r", "next [REVIEW NEEDED] in chapter"),
                    ("s", "show source for this TH chunk"),
                    ("m", "toggle bookmark at this line"),
                    ("n / N", "add note · show/hide notes"),
                    ("d / y", "rerun diff · copy visible Thai"),
                    ("Q", "QA review (flagged issues)"),
                ],
            ),
            (
                "Lexicon 辞",
                &[
                    ("Tab", "Glossary↔Characters↔Style"),
                    ("↵ / e / n", "edit · edit · new"),
                    ("d / /", "delete · search"),
                ],
            ),
        ];

        let mut lines = Vec::new();
        for (section, keys) in groups {
            lines.push(Line::from(Span::styled(
                format!(" {section}"),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            )));
            for (k, desc) in *keys {
                lines.push(Line::from(vec![
                    Span::styled(format!("   {k:<14}"), Style::default().fg(theme.ink)),
                    Span::styled(*desc, Style::default().fg(theme.ink_soft)),
                ]));
            }
            lines.push(Line::raw(""));
        }
        // Scroll instead of truncating so the lower sections stay reachable.
        let cap = inner.height as usize;
        let max_off = lines.len().saturating_sub(cap) as u16;
        let off = off.min(max_off);
        f.render_widget(
            Paragraph::new(Text::from(lines))
                .wrap(Wrap { trim: false })
                .scroll((off, 0))
                .style(Style::default().bg(theme.bg_panel)),
            inner,
        );
    }

    /// The About card, animated off the app's 100ms ticker: a moon that waxes
    /// through the chapter-status phases, a looping JA→TH typing demo, the
    /// three-agent pipeline pulsing, and a phase-shifted moon wave divider.
    fn render_about(&self, f: &mut Frame, area: Rect, theme: &Theme, frame: u64) {
        use ratatui::layout::Alignment;
        use unicode_segmentation::UnicodeSegmentation;

        let modal = centered_modal(64, 20, area);
        f.render_widget(Clear, modal);
        let title = thai_display_safe("About · เกี่ยวกับ");
        let block = self.modal_block(&title, theme);
        let inner = block.inner(modal);
        f.render_widget(block, modal);

        // The waxing-moon chapter-status metaphor, here as a slow cycle.
        const PHASES: [char; 5] = ['○', '◔', '◐', '◑', '●'];
        let moon = PHASES[(frame / 4) as usize % PHASES.len()];

        // Looping typing demo: type one grapheme per tick, hold the finished
        // line, then start over. Graphemes keep Thai clusters intact mid-type.
        let jp = "「月が綺麗ですね。」";
        let th_full = thai_display_safe("— พระจันทร์คืนนี้สวยเหลือเกินนะ");
        let graphemes: Vec<&str> = th_full.graphemes(true).collect();
        const HOLD: usize = 22;
        let pos = (frame as usize) % (graphemes.len() + HOLD);
        let shown = pos.min(graphemes.len());
        let typed: String = graphemes[..shown].concat();
        let caret_on = shown < graphemes.len() || frame % 10 < 5;

        // The three-agent pipeline, pulsing left to right.
        let active = ((frame / 6) % 3) as usize;
        let mut pipeline: Vec<Span> = Vec::new();
        for (i, (name, color)) in [
            ("Orchestrator", theme.accent),
            ("Translator", theme.status_working),
            ("Reviewer", theme.accent_soft),
        ]
        .into_iter()
        .enumerate()
        {
            if i > 0 {
                pipeline.push(Span::styled(" ─── ", Style::default().fg(theme.rule)));
            }
            if i == active {
                pipeline.push(Span::styled(
                    format!("{} {name}", theme::spinner_frame(frame)),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ));
            } else {
                pipeline.push(Span::styled(
                    format!("◇ {name}"),
                    Style::default().fg(theme.ink_faint),
                ));
            }
        }

        // A moon wave: each column sits a phase behind its neighbour, so the
        // waxing/waning cycle ripples across the divider.
        let wave_cols = (inner.width.saturating_sub(8) / 2).min(22) as usize;
        let mut wave: Vec<Span> = Vec::new();
        for i in 0..wave_cols {
            // Ping-pong through the phases: ○◔◐◑●◑◐◔ …
            let idx = ((frame / 2) as usize + i) % (PHASES.len() * 2 - 2);
            let phase = if idx < PHASES.len() {
                idx
            } else {
                PHASES.len() * 2 - 2 - idx
            };
            let color = if phase == PHASES.len() - 1 {
                theme.accent_soft
            } else {
                theme.ink_faint
            };
            wave.push(Span::styled(
                format!("{} ", PHASES[phase]),
                Style::default().fg(color),
            ));
        }

        let lines = vec![
            Line::raw(""),
            Line::from(vec![
                Span::styled(format!("{moon} "), Style::default().fg(theme.accent)),
                Span::styled(
                    "本屋 honya",
                    Style::default().fg(theme.ink).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(Span::styled(
                concat!("v", env!("CARGO_PKG_VERSION"), " · Rust + Ratatui · Apache-2.0"),
                Style::default().fg(theme.ink_faint),
            )),
            Line::raw(""),
            Line::from(Span::styled(
                "AI-assisted Japanese → Thai light-novel translation TUI",
                Style::default().fg(theme.ink_soft),
            )),
            Line::from(Span::styled(
                thai_display_safe("แอปเทอร์มินัลช่วยแปลไลต์โนเวลญี่ปุ่นเป็นไทยด้วย AI"),
                Style::default().fg(theme.ink_soft),
            )),
            Line::raw(""),
            Line::from(Span::styled(jp, Style::default().fg(theme.ink_faint))),
            Line::from(vec![
                Span::styled(typed, Style::default().fg(theme.ink)),
                if caret_on {
                    Span::styled("▏", Style::default().fg(theme.stream_cursor))
                } else {
                    Span::raw(" ")
                },
            ]),
            Line::raw(""),
            Line::from(pipeline),
            Line::raw(""),
            Line::from(wave),
            Line::raw(""),
            Line::from(Span::styled(
                "github.com/altqx/honya",
                Style::default().fg(theme.accent_soft),
            )),
            Line::from(Span::styled(
                "honya.altqx.com",
                Style::default().fg(theme.accent_soft),
            )),
        ];
        f.render_widget(
            Paragraph::new(lines)
                .alignment(Alignment::Center)
                .style(Style::default().bg(theme.bg_panel)),
            inner,
        );
    }

    /// Render the QA inbox: a chapter-level summary header over a navigable list of
    /// findings grouped by chapter (each group headed by its issue count). The list
    /// windows so the selected finding stays visible; one line per finding keeps the
    /// selection index aligned with the rendered rows.
    fn render_qa(&self, f: &mut Frame, area: Rect, theme: &Theme, st: &QaState) {
        let modal = centered_pct(80, 80, area);
        f.render_widget(Clear, modal);
        let block = self.modal_block("Translation QA · レビュー", theme);
        let inner = block.inner(modal);
        f.render_widget(block, modal);

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // title + summary counts
                Constraint::Length(1), // divider
                Constraint::Min(0),    // grouped findings
            ])
            .split(inner);

        let report = &st.report;

        let title = if st.title.is_empty() {
            "Translation QA".to_string()
        } else {
            st.title.clone()
        };
        let mut counts = vec![
            Span::styled(
                format!(" ✓ {} done", report.done),
                Style::default().fg(theme.status_done),
            ),
            Span::styled("    ", Style::default().fg(theme.ink_faint)),
            Span::styled(
                format!("⚠ {} review", report.review),
                Style::default().fg(if report.review > 0 {
                    theme.status_warn
                } else {
                    theme.ink_faint
                }),
            ),
            Span::styled("    ", Style::default().fg(theme.ink_faint)),
            Span::styled(
                format!("✗ {} failed", report.failed),
                Style::default().fg(if report.failed > 0 {
                    theme.status_failed
                } else {
                    theme.ink_faint
                }),
            ),
        ];
        if let Some(pct) = report.clean_pct() {
            counts.push(Span::styled("     ", Style::default().fg(theme.ink_faint)));
            counts.push(Span::styled(
                format!("{pct}% clean"),
                Style::default()
                    .fg(theme.ink_soft)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        let header = vec![
            Line::from(Span::styled(
                format!(
                    " {}",
                    truncate_cols(
                        &thai_display_safe(&title),
                        rows[0].width.saturating_sub(2) as usize,
                    )
                ),
                Style::default()
                    .fg(theme.ink_soft)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(counts),
        ];
        f.render_widget(
            Paragraph::new(header).style(Style::default().bg(theme.bg_panel)),
            rows[0],
        );

        if rows[1].width > 0 {
            f.render_widget(
                Paragraph::new("─".repeat(rows[1].width as usize))
                    .style(Style::default().fg(theme.rule).bg(theme.bg_panel)),
                rows[1],
            );
        }

        let list_area = rows[2];
        let n = report.issues.len();

        if n == 0 {
            let (msg, color) = if report.done + report.review + report.failed == 0 {
                (
                    "   ยังไม่มีบทที่แปล — nothing translated yet for this volume.",
                    theme.ink_faint,
                )
            } else {
                (
                    "   ✓ All clear — no QA issues for this volume.",
                    theme.status_done,
                )
            };
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(msg, Style::default().fg(color))))
                    .style(Style::default().bg(theme.bg_panel)),
                list_area,
            );
            return;
        }

        let sel = st.sel.min(n - 1);
        // Fixed prefix is 7+TAG_W cols; TAG_W fits "chunk 999" and "conflict".
        const TAG_W: usize = 9;
        let detail_w = (list_area.width as usize).saturating_sub(7 + TAG_W);

        let mut lines: Vec<Line> = Vec::new();
        let mut sel_line = 0usize;
        let mut prev: Option<Option<u32>> = None;
        for (i, issue) in report.issues.iter().enumerate() {
            if prev != Some(issue.chapter) {
                prev = Some(issue.chapter);
                let count = report.count_for(issue.chapter);
                let ch_label = match issue.chapter {
                    Some(c) => format!(" ch {c:03}"),
                    None => " ch —".to_string(),
                };
                let mut head = vec![Span::styled(
                    ch_label,
                    Style::default()
                        .fg(theme.ink_soft)
                        .add_modifier(Modifier::BOLD),
                )];
                if !issue.title.is_empty() {
                    // Leave room for the count badge after title truncation.
                    head.push(Span::styled(
                        format!(
                            "  {}",
                            truncate_cols(&thai_display_safe(&issue.title), detail_w)
                        ),
                        Style::default().fg(theme.ink_faint),
                    ));
                }
                head.push(Span::styled(
                    format!("  ({count})"),
                    Style::default().fg(theme.accent_soft),
                ));
                lines.push(Line::from(head));
            }

            let selected = i == sel;
            if selected {
                sel_line = lines.len();
            }
            let row_bg = if selected {
                theme.accent_bg
            } else {
                theme.bg_panel
            };
            let (glyph, color, tag) = qa_visual(issue, theme);
            let bar = if selected { theme::SELECT_BAR } else { ' ' };
            let detail_src = if issue.detail.trim().is_empty() {
                qa_default_detail(issue).to_string()
            } else {
                thai_display_safe(&issue.detail)
            };
            let detail = truncate_cols(&detail_src, detail_w);
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {bar} "),
                    Style::default().fg(theme.accent).bg(row_bg),
                ),
                Span::styled(format!("{glyph} "), Style::default().fg(color).bg(row_bg)),
                Span::styled(
                    // Keep long tags from shifting detail.
                    format!("{} ", pad_to_cols(&tag, TAG_W)),
                    Style::default().fg(theme.ink_soft).bg(row_bg),
                ),
                Span::styled(detail, Style::default().fg(theme.ink).bg(row_bg)),
            ]));
        }

        let cap = (list_area.height as usize).max(1);
        let start = if sel_line >= cap {
            sel_line + 1 - cap
        } else {
            0
        };
        let end = (start + cap).min(lines.len());
        let visible: Vec<Line> = lines[start..end].to_vec();
        f.render_widget(
            Paragraph::new(visible).style(Style::default().bg(theme.bg_panel)),
            list_area,
        );
    }

    fn render_modal(&self, f: &mut Frame, area: Rect, theme: &Theme, dlg: &Dialog) {
        let modal = centered_modal(64, if dlg.alternate.is_some() { 11 } else { 9 }, area);
        f.render_widget(Clear, modal);
        let block = self.modal_block(&dlg.title, theme);
        let inner = block.inner(modal);
        f.render_widget(block, modal);

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(inner);
        f.render_widget(
            Paragraph::new(Span::styled(
                format!("  {}", thai_display_safe(&dlg.body)),
                Style::default().fg(theme.ink_soft),
            ))
            .wrap(Wrap { trim: false })
            .style(Style::default().bg(theme.bg_panel)),
            rows[0],
        );
        let mut controls = vec![Span::styled(
            format!("  [ y/↵ ] {}", dlg.confirm_label),
            Style::default().fg(theme.accent),
        )];
        if let Some(alt) = &dlg.alternate {
            controls.push(Span::raw("     "));
            controls.push(Span::styled(
                format!("[ {} ] {}", alt.key, alt.label),
                Style::default().fg(theme.status_warn),
            ));
        }
        controls.push(Span::raw("     "));
        controls.push(Span::styled(
            "[ n / Esc ] cancel",
            Style::default().fg(theme.ink_faint),
        ));
        f.render_widget(
            Paragraph::new(Line::from(controls)).style(Style::default().bg(theme.bg_panel)),
            rows[1],
        );
    }
}

/// What a resolved overlay click should do — a synthesized key (reusing the
/// keyboard handlers), a direct action, or nothing.
enum ClickOutcome {
    Nothing,
    Key(KeyCode),
    Act(Action),
}

/// A confirm-dialog button identified by a click on the control row.
enum ModalButton {
    Confirm,
    Cancel,
    Alternate(char),
}

/// A key event with no modifiers — used to replay a gesture through the keyboard
/// handlers so navigation / dismiss logic lives in exactly one place.
fn synth(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::empty())
}

/// The content rectangle inside a single-cell border (mirrors `Block::inner` for
/// `Borders::ALL`), used to hit-test modal interiors.
fn inset(r: Rect) -> Rect {
    Rect {
        x: r.x.saturating_add(1),
        y: r.y.saturating_add(1),
        width: r.width.saturating_sub(2),
        height: r.height.saturating_sub(2),
    }
}

/// First visible index of a windowed list that keeps `sel` on screen given a
/// visible height of `cap` rows — the shared rule the list overlays render with.
fn windowed_start(sel: usize, cap: u16) -> usize {
    let cap = (cap as usize).max(1);
    if sel >= cap { sel + 1 - cap } else { 0 }
}

/// Which confirm-dialog button (if any) sits at column `col` on the control row.
/// The label spans mirror `render_modal`: `  [ y/↵ ] confirm`, an optional
/// `[ key ] alt`, then `[ n / Esc ] cancel`, each separated by five spaces.
fn modal_button_at(dlg: &Dialog, inner: Rect, col: u16) -> Option<ModalButton> {
    let in_range = |start: u16, width: u16| col >= start && col < start.saturating_add(width);

    let confirm = format!("  [ y/↵ ] {}", dlg.confirm_label);
    let confirm_w = col_width(&confirm) as u16;
    let mut x = inner.x;
    if in_range(x, confirm_w) {
        return Some(ModalButton::Confirm);
    }
    x = x.saturating_add(confirm_w).saturating_add(5);

    if let Some(alt) = &dlg.alternate {
        let s = format!("[ {} ] {}", alt.key, alt.label);
        let w = col_width(&s) as u16;
        if in_range(x, w) {
            return Some(ModalButton::Alternate(alt.key));
        }
        x = x.saturating_add(w).saturating_add(5);
    }

    let cancel_w = col_width("[ n / Esc ] cancel") as u16;
    if in_range(x, cancel_w) {
        return Some(ModalButton::Cancel);
    }
    None
}

/// Rows of wizard chrome (step rail · context line · gap) above each step body;
/// resolve_click must mirror render_import's layout.
const IMPORT_HEADER_ROWS: u16 = 3;
/// Rows the pick step draws above its file list (header · gap).
const IMPORT_PICK_LIST_OFFSET: u16 = 2;

/// The wizard's step rail: done steps get a check, the current step is
/// highlighted, future steps are dimmed. The add-volume flow hides "Name".
fn step_rail(st: &ImportState, theme: &Theme) -> Line<'static> {
    let steps: &[(u8, &str)] = if st.lock_name {
        &[(0, "File"), (2, "Volume"), (3, "Synopsis")]
    } else {
        &[(0, "File"), (1, "Name"), (2, "Volume"), (3, "Synopsis")]
    };
    let mut spans = vec![Span::raw(" ")];
    for (i, &(id, label)) in steps.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ›  ", Style::default().fg(theme.rule)));
        }
        if st.step > id {
            spans.push(Span::styled(
                format!("✓ {label}"),
                Style::default().fg(theme.status_done),
            ));
        } else if st.step == id {
            spans.push(Span::styled(
                format!("{} {label}", i + 1),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                format!("{} {label}", i + 1),
                Style::default().fg(theme.ink_faint),
            ));
        }
    }
    Line::from(spans)
}

/// One line of accumulated choices under the rail, so every step shows what is
/// already decided: file · project name · volume. The file is dimmed while it is
/// still being picked; the add-volume flow shows its fixed target up front.
fn import_context_line(st: &ImportState, theme: &Theme) -> Line<'static> {
    let confirmed = Style::default().fg(theme.ink_soft);
    let pending = Style::default().fg(theme.ink_faint);
    let sep = Span::styled("  ·  ", Style::default().fg(theme.rule));
    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    let file = st
        .selected_file()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("—");
    spans.push(Span::styled(
        truncate_cols(&thai_display_safe(file), 30),
        if st.step == 0 { pending } else { confirmed },
    ));
    if st.lock_name || st.step > 1 {
        spans.push(sep.clone());
        spans.push(Span::styled(
            truncate_cols(&thai_display_safe(st.name.trim()), 30),
            confirmed,
        ));
    }
    if st.lock_name || st.step > 2 {
        spans.push(sep);
        spans.push(Span::styled(
            format!("Vol.{:02}", st.vol),
            Style::default().fg(theme.accent_soft),
        ));
    }
    Line::from(spans)
}

/// `Vol.01 (12 ch) · Vol.02 (9 ch)` chips for a project's existing volumes.
fn volume_chips(volumes: &[(u32, usize)]) -> String {
    volumes
        .iter()
        .map(|&(n, ch)| format!("Vol.{n:02} ({ch} ch)"))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Phase-dependent footer hints for the synopsis editor (shared by the wizard
/// step and the standalone overlay); `wizard` switches the accept label, since
/// accepting in the wizard starts the import while standalone accept saves.
fn synopsis_hints(
    st: &SynopsisState,
    wizard: bool,
) -> &'static [(&'static str, &'static str)] {
    match st.phase {
        SynPhase::Editing => {
            if st.raw.trim().is_empty() {
                &[("type", "raw"), ("Tab", "skip"), ("Esc", "back")]
            } else {
                &[("type", "raw"), ("Tab", "translate"), ("Esc", "back")]
            }
        }
        SynPhase::Translating => &[("Esc", "cancel"), ("…", "translating")],
        SynPhase::Done if wizard => &[
            ("↵", "start import"),
            ("r", "reroll"),
            ("e", "edit"),
            ("s", "skip"),
        ],
        SynPhase::Done => &[
            ("↵", "save"),
            ("r", "reroll"),
            ("e", "edit"),
            ("s", "skip"),
        ],
        SynPhase::Failed => &[("r", "retry"), ("e", "edit"), ("s", "skip")],
    }
}

/// Render the synopsis editor body (raw input box, status line, translation) into
/// `area`. Shared verbatim by the import wizard's step 3 and `render_synopsis`.
fn render_synopsis_body(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    st: &SynopsisState,
    accept_label: &str,
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // label
            Constraint::Length(9), // raw input box (incl. border)
            Constraint::Length(1), // status line
            Constraint::Length(1), // divider
            Constraint::Min(0),    // translation / error
        ])
        .split(area);

    f.render_widget(
        Paragraph::new(Span::styled(
            thai_display_safe("  เรื่องย่อเล่ม (ไม่บังคับ) — AI ใช้เป็นบริบทตอนแปล"),
            Style::default().fg(theme.ink_soft),
        ))
        .style(Style::default().bg(theme.bg_panel)),
        rows[0],
    );

    let editing = st.phase == SynPhase::Editing;
    let border_color = if editing {
        theme.accent_soft
    } else {
        theme.rule
    };
    let input_block = Block::default()
        .borders(Borders::ALL)
        .border_set(theme::hairline_set())
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(theme.bg_inset));
    let mut text_lines: Vec<Line> = Vec::new();
    if st.raw.is_empty() {
        text_lines.push(Line::from(vec![
            Span::styled(
                thai_display_safe("พิมพ์หรือวางเรื่องย่อภาษาต้นฉบับ… (เว้นว่างแล้วกด Tab เพื่อข้าม)"),
                Style::default().fg(theme.ink_faint),
            ),
            if editing {
                Span::styled("▏", Style::default().fg(theme.stream_cursor))
            } else {
                Span::raw("")
            },
        ]));
    } else {
        let cursor = input::clamp_cursor(&st.raw, st.cursor);
        let mut line_start = 0usize;
        for part in st.raw.split('\n') {
            let line_end = line_start + part.len();
            // The caret sits on this line when the (clamped) cursor falls within
            // it; at a '\n' boundary it belongs to the earlier line's tail.
            let on_line = editing && cursor >= line_start && cursor <= line_end;
            let mut spans: Vec<Span> = Vec::new();
            if on_line {
                let off = cursor - line_start;
                spans.push(Span::styled(
                    thai_display_safe(&part[..off]),
                    Style::default().fg(theme.ink),
                ));
                spans.push(Span::styled("▏", Style::default().fg(theme.stream_cursor)));
                spans.push(Span::styled(
                    thai_display_safe(&part[off..]),
                    Style::default().fg(theme.ink),
                ));
            } else {
                spans.push(Span::styled(
                    thai_display_safe(part),
                    Style::default().fg(theme.ink),
                ));
            }
            text_lines.push(Line::from(spans));
            line_start = line_end + 1; // skip the '\n'
        }
    }
    f.render_widget(
        Paragraph::new(Text::from(text_lines))
            .wrap(Wrap { trim: false })
            .block(input_block),
        indent(rows[1], 2),
    );

    let status = match st.phase {
        SynPhase::Editing => Span::styled(
            thai_display_safe(&if st.raw.trim().is_empty() {
                "  ยังไม่มีข้อความ — Tab ข้ามขั้นตอนนี้ · Esc กลับ".to_string()
            } else {
                format!(
                    "  {} ตัวอักษร · Tab แปล · Enter ขึ้นบรรทัดใหม่ · Esc กลับ",
                    st.raw.chars().count()
                )
            }),
            Style::default().fg(theme.ink_faint),
        ),
        SynPhase::Translating => Span::styled(
            thai_display_safe("  ◐ กำลังแปลด้วย Translator agent … (Esc ยกเลิก)"),
            Style::default().fg(theme.status_working),
        ),
        SynPhase::Done => Span::styled(
            thai_display_safe(&format!(
                "  ✓ แปลแล้ว (รอบ {}) — Enter {accept_label} · r แปลใหม่ · e แก้ต้นฉบับ · s ข้าม",
                st.attempt + 1
            )),
            Style::default().fg(theme.status_done),
        ),
        SynPhase::Failed => Span::styled(
            thai_display_safe("  ✗ แปลไม่สำเร็จ — r ลองใหม่ · e แก้ต้นฉบับ · s ข้าม"),
            Style::default().fg(theme.status_failed),
        ),
    };
    f.render_widget(
        Paragraph::new(status).style(Style::default().bg(theme.bg_panel)),
        rows[2],
    );

    f.render_widget(
        Paragraph::new(Span::styled(
            thai_display_safe("  ── คำแปลภาษาไทย / Thai ──"),
            Style::default().fg(theme.ink_faint),
        ))
        .style(Style::default().bg(theme.bg_panel)),
        rows[3],
    );

    let (body, color) = match st.phase {
        SynPhase::Failed => (st.error.clone(), theme.status_failed),
        _ if st.th.trim().is_empty() => {
            ("(ยังไม่มีคำแปล — กด Tab เพื่อแปล)".to_string(), theme.ink_faint)
        }
        _ => (st.th.clone(), theme.ink),
    };
    f.render_widget(
        Paragraph::new(crate::ui::text::thai_display_safe(&body))
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(color).bg(theme.bg_panel)),
        indent(rows[4], 2),
    );
}

/// The glyph, color, and short tag for a QA finding row.
fn qa_visual(issue: &qa::QaIssue, theme: &Theme) -> (&'static str, ratatui::style::Color, String) {
    use qa::{QaKind, Severity};
    match &issue.kind {
        QaKind::ReviewChunk { chunk } => ("⚠", theme.status_warn, format!("chunk {}", chunk + 1)),
        QaKind::ChapterFailed => ("✗", theme.status_failed, "failed".to_string()),
        QaKind::Continuity {
            severity: Severity::Conflict,
        } => ("‖", theme.status_failed, "conflict".to_string()),
        QaKind::Continuity {
            severity: Severity::Warning,
        } => ("‖", theme.status_warn, "warning".to_string()),
    }
}

/// Fallback detail text when a finding carries no reviewer reason / note.
fn qa_default_detail(issue: &qa::QaIssue) -> &'static str {
    match issue.kind {
        qa::QaKind::ChapterFailed => "translation failed — see activity log",
        qa::QaKind::ReviewChunk { .. } => "committed without passing review",
        qa::QaKind::Continuity { .. } => "continuity note",
    }
}

/// Short one-line description of an export format for the picker.
fn export_desc(fmt: ExportFormat) -> &'static str {
    match fmt {
        ExportFormat::Markdown => "merged .md + images",
        ExportFormat::Epub => "e-reader (EPUB3)",
        ExportFormat::Docx => "Word, for editors",
    }
}

/// Indent a Rect from the left/right by `pad` columns (keeps modals breathing).
fn indent(area: Rect, pad: u16) -> Rect {
    let pad = pad.min(area.width / 2);
    Rect {
        x: area.x + pad,
        y: area.y,
        width: area.width.saturating_sub(pad * 2),
        height: area.height,
    }
}

/// Mask a secret for display: a run of bullets with the last 4 characters revealed
/// (e.g. `sk-or-v1-…1a2b` → `••••••••1a2b`), so the user can confirm which key is
/// saved without exposing it. Short keys are fully bulleted.
fn mask_secret(s: &str) -> String {
    let chars: Vec<char> = s.trim().chars().collect();
    if chars.len() <= 4 {
        return "•".repeat(chars.len());
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    let dots = chars.len().saturating_sub(4).min(12);
    format!("{}{}", "•".repeat(dots), tail)
}

/// Turn an epub file stem into a readable default title: `_`/`-` → spaces,
/// trailing `_vNN` volume tags dropped, then word-cased lightly.
fn prettify_stem(stem: &str) -> String {
    let replaced: String = stem
        .chars()
        .map(|c| if c == '_' || c == '-' { ' ' } else { c })
        .collect();
    // Drop a trailing " v08" / " vol 8" style volume marker if present.
    let words: Vec<&str> = replaced.split_whitespace().collect();
    let trimmed: Vec<&str> = words
        .iter()
        .copied()
        .filter(|w| {
            let lw = w.to_ascii_lowercase();
            !(lw.starts_with('v') && lw[1..].chars().all(|c| c.is_ascii_digit()) && lw.len() > 1)
                && lw != "vol"
        })
        .collect();
    let base = if trimmed.is_empty() { words } else { trimmed };
    base.join(" ").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Every glyph the terminal would draw for the synopsis editor, concatenated.
    fn rendered_glyphs(st: &SynopsisState) -> String {
        let theme = Theme::washi();
        let mut term = Terminal::new(TestBackend::new(80, 16)).unwrap();
        term.draw(|f| render_synopsis_body(f, f.area(), &theme, st, "บันทึก"))
            .unwrap();
        term.backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn wizard(projects: Vec<ProjectRef>) -> Overlay {
        Overlay::Import(ImportState::new(
            vec![(PathBuf::from("cursed_blade_v03.epub"), 2_345_678)],
            projects,
        ))
    }

    /// An empty name no longer fails silently: Enter stays on the step and shows
    /// a visible nudge, and the nudge clears on the next keystroke.
    #[test]
    fn name_step_requires_a_name_and_says_so() {
        let mut ov = wizard(vec![]);
        if let Overlay::Import(st) = &mut ov {
            st.step = 1;
            st.name.clear();
            st.name_cursor = 0;
        }
        ov.handle_key(key(KeyCode::Enter));
        let Overlay::Import(st) = &ov else {
            panic!("overlay changed variant")
        };
        assert_eq!(st.step, 1, "must not advance without a name");
        assert!(st.note.is_some(), "must surface why Enter did nothing");

        ov.handle_key(key(KeyCode::Char('x')));
        let Overlay::Import(st) = &ov else {
            panic!("overlay changed variant")
        };
        assert!(st.note.is_none(), "typing must clear the nudge");
    }

    /// A name whose slug matches an existing project is detected (merge notice)
    /// and the volume step defaults to that project's next volume, not 1.
    #[test]
    fn name_matching_existing_project_suggests_next_volume() {
        let existing = ProjectRef {
            slug: slugify("Cursed Blade"),
            title: "Cursed Blade".to_string(),
            volumes: vec![(1, 12), (2, 9)],
        };
        let mut ov = wizard(vec![existing]);
        if let Overlay::Import(st) = &mut ov {
            st.step = 1;
            st.name = "Cursed Blade".to_string();
            st.name_cursor = st.name.len();
        }
        ov.handle_key(key(KeyCode::Enter));
        let Overlay::Import(st) = &ov else {
            panic!("overlay changed variant")
        };
        assert!(st.target_project().is_some());
        assert_eq!(st.step, 2);
        assert_eq!(st.vol, 3, "should pre-pick one past the highest volume");
    }

    /// A manually adjusted volume must survive going back and forward again.
    #[test]
    fn user_chosen_volume_is_not_overridden_by_the_suggestion() {
        let existing = ProjectRef {
            slug: slugify("Cursed Blade"),
            title: "Cursed Blade".to_string(),
            volumes: vec![(1, 12)],
        };
        let mut ov = wizard(vec![existing]);
        if let Overlay::Import(st) = &mut ov {
            st.step = 1;
            st.name = "Cursed Blade".to_string();
            st.name_cursor = st.name.len();
        }
        ov.handle_key(key(KeyCode::Enter)); // → volume step, suggested 2
        ov.handle_key(key(KeyCode::Up)); // user picks 3
        ov.handle_key(key(KeyCode::Esc)); // back to name
        ov.handle_key(key(KeyCode::Enter)); // forward again
        let Overlay::Import(st) = &ov else {
            panic!("overlay changed variant")
        };
        assert_eq!(st.vol, 3, "manual pick must not be re-suggested away");
    }

    /// Every wizard step must render without leaking raw SARA AM, including the
    /// new chrome (step rail, context line, feedback, volume chips).
    #[test]
    fn import_wizard_steps_render_without_raw_sara_am() {
        let theme = Theme::washi();
        let existing = ProjectRef {
            slug: slugify("Cursed Blade"),
            title: "ดาบคำสาป".to_string(),
            volumes: vec![(1, 3)],
        };
        let mut st = ImportState::new(
            vec![(PathBuf::from("ดาบคำสาป_v01.epub"), 2_345_678)],
            vec![existing],
        );
        st.name = "ดาบคำสาป".to_string();
        st.name_cursor = st.name.len();
        st.syn.raw = "คำสาปแห่งดาบ".to_string();
        for step in 0..=4u8 {
            st.step = step;
            let mut term = Terminal::new(TestBackend::new(80, 26)).unwrap();
            term.draw(|f| Overlay::None.render_import(f, f.area(), &theme, &st))
                .unwrap();
            let glyphs: String = term
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(
                !glyphs.contains('\u{0E33}'),
                "raw SARA AM leaked into wizard step {step}"
            );
        }
    }

    /// The About card animates off the frame ticker: every animation phase must
    /// render without panicking and without leaking raw SARA AM, and the typing
    /// loop must reach the full Thai line.
    #[test]
    fn about_card_renders_across_animation_frames() {
        let theme = Theme::washi();
        let mut saw_full_line = false;
        for frame in 0..80u64 {
            let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
            term.draw(|f| Overlay::About.render_about(f, f.area(), &theme, frame))
                .unwrap();
            let glyphs: String = term
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(
                !glyphs.contains('\u{0E33}'),
                "raw SARA AM leaked into the About card at frame {frame}"
            );
            assert!(glyphs.contains("honya"), "brand line missing at frame {frame}");
            if glyphs.contains("เหลือเกินนะ") {
                saw_full_line = true;
            }
        }
        assert!(saw_full_line, "typing demo never completed its line");
    }

    /// SARA AM (`ำ`, U+0E33) must never reach the terminal: every Thai label is
    /// decomposed to NIKHAHIT + SARA AA first, otherwise it smears across cells
    /// on the next redraw. The status labels here were a missed render site.
    #[test]
    fn synopsis_labels_never_emit_raw_sara_am() {
        for phase in [
            SynPhase::Editing,
            SynPhase::Translating,
            SynPhase::Done,
            SynPhase::Failed,
        ] {
            let mut st =
                SynopsisState::new("源のあらすじ".to_string(), "เรื่องย่อภาษาไทย".to_string());
            st.phase = phase.clone();
            st.error = "แปลไม่สำเร็จ".to_string();
            let glyphs = rendered_glyphs(&st);
            assert!(
                !glyphs.contains('\u{0E33}'),
                "raw SARA AM leaked into the {phase:?} synopsis render"
            );
        }
    }
}


