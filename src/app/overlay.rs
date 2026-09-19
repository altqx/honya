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
use crate::model::{
    AppConfig, LogLevel, ReleaseChannel, ServiceTier, TargetLanguage, ThemeId, UpdateMode,
};
use crate::theme::{self, ALL_THEMES, Theme};
use crate::ui::input::{self, EditOpts, Edited};
use crate::ui::layout::{centered_modal, centered_pct};
use crate::ui::mouse::{MouseGesture, MouseInput, hit};
use crate::ui::text::{col_width, pad_to_cols, thai_display_safe, truncate_cols};
use crate::ui::kit::{ZoneId, ZoneKind};
use crate::ui::widgets::render_gauge;

use super::qa;
use super::{Action, Screen, slugify};

/// Zone indices for the choices every confirm dialog offers. Distinct from a
/// modal's close button, which the kit owns.
pub const DIALOG_CANCEL: u32 = 1;
pub const DIALOG_CONFIRM: u32 = 2;
pub const DIALOG_ALTERNATE: u32 = 3;

/// One line of the keybinding reference.
enum HelpRow {
    Section(&'static str),
    Binding(&'static str, &'static str),
    Blank,
}

/// The keybinding reference, as data rather than as pre-formatted lines.
///
/// Structured so the list component can window it, and so the same table can
/// later feed the shortcuts bar and the command bar instead of all three
/// keeping their own copy.
fn help_rows() -> Vec<HelpRow> {
    use HelpRow::{Binding, Blank, Section};
    vec![
        Section("Global"),
        Binding("1–6 / Tab", "switch primary tab"),
        Binding(": / Ctrl-P / Ctrl-K", "command bar"),
        Binding("Ctrl-T", "theme picker"),
        Binding("` / l", "activity log (Project keeps l)"),
        Binding("?", "this help"),
        Binding("Esc / Backspace", "close overlay · dismiss toast"),
        Binding("Ctrl-C", "quit (twice)"),
        Blank,
        Section("Mouse"),
        Binding("click", "tabs, rows, buttons, breadcrumb, tally"),
        Binding("double-click", "open the row under the pointer"),
        Binding("wheel", "scroll the pane under the pointer"),
        Binding("right-click", "back · dismiss"),
        Blank,
        Section("Shelf 書架"),
        Binding("↵", "open project"),
        Binding("i", "import a source file"),
        Binding("d / R / r", "delete · rename · rescan"),
        Blank,
        Section("Project 棚"),
        Binding("↵", "read chapter"),
        Binding("Space", "mark chapter (cross-volume ok)"),
        Binding("t / a", "translate · queue marked or current"),
        Binding("T / A", "translate volume · whole project"),
        Binding("V / i", "add volume · add chapters"),
        Binding("h / l", "collapse · expand volume, focus panel"),
        Binding("z / Z", "collapse · expand all volumes"),
        Binding("x / Q", "export · QA report"),
        Binding("e / y", "edit title · synopsis"),
        Blank,
        Section("Translate 訳"),
        Binding("p / s", "pause · stop the run"),
        Binding("J / K", "move queued chapter down · up"),
        Blank,
        Section("Reader 読"),
        Binding("/ ", "search both panes"),
        Binding("g", "jump to chapter, section or bookmark"),
        Binding("w / y", "wrap · sync the panes"),
        Binding("b / n", "bookmark · note this line"),
        Blank,
        Section("Lexicon 辞"),
        Binding("↵ / d", "edit · delete entry"),
        Binding("/ ", "filter"),
        Blank,
        Section("Refine 推"),
        Binding("Ctrl-R", "new session"),
        Binding("Ctrl-C", "cancel the in-flight turn"),
    ]
}

/// Where a synopsis editor sits in its lifecycle.
#[derive(Debug, Clone, PartialEq)]
pub enum SynPhase {
    /// Typing the raw source text.
    Editing,
    /// Awaiting a Translator round-trip (reroll or first translation).
    Translating,
    /// A translation is in hand; can accept / reroll / edit.
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
    /// Latest target-language translation, directly hand-editable when focused.
    pub translated_text: String,
    /// Caret byte-offset into `translated_text`.
    pub translated_cursor: usize,
    /// Focus on the translated field: type to edit it by hand instead of only
    /// translating via the agent. Only used by the single-line title editors.
    pub edit_translation: bool,
    pub phase: SynPhase,
    /// Error text shown while `phase == Failed`.
    pub error: String,
    /// Reroll counter — drives rising translation temperature.
    pub attempt: u32,
    /// False when Enter should submit instead of inserting a newline.
    pub multiline: bool,
}

impl SynopsisState {
    pub fn new(raw: String, translated_text: String) -> Self {
        let phase = if translated_text.trim().is_empty() {
            SynPhase::Editing
        } else {
            SynPhase::Done
        };
        Self {
            cursor: raw.len(),
            translated_cursor: translated_text.len(),
            raw,
            translated_text,
            edit_translation: false,
            phase,
            error: String::new(),
            attempt: 0,
            multiline: true,
        }
    }

    pub fn new_single_line(raw: String, translated_text: String) -> Self {
        Self {
            multiline: false,
            ..Self::new(raw, translated_text)
        }
    }

    /// Single-line editor focused on hand-editing the translated project title.
    pub fn new_title(raw: String, translated_text: String) -> Self {
        Self {
            edit_translation: true,
            ..Self::new_single_line(raw, translated_text)
        }
    }
}

/// What a synopsis keypress means to the embedding overlay.
enum SynKey {
    None,
    /// Start translating the current `raw` (phase already set to Translating).
    Translate,
    /// Accept the current source/translation pair.
    Accept,
    /// Proceed without a synopsis.
    Skip,
    /// Esc out of the editor (caller decides: prev step / close).
    Back,
}

/// Fold one keypress into a [`SynopsisState`], returning the embedder's next move.
fn handle_synopsis_keys(st: &mut SynopsisState, key: KeyEvent) -> SynKey {
    // A translation round-trip is in flight; ignore input but allow cancel.
    // Late results are dropped by set_synopsis_result once the phase changes.
    if st.phase == SynPhase::Translating {
        if key.code == KeyCode::Esc {
            st.phase = SynPhase::Editing;
        }
        return SynKey::None;
    }
    // Hand-editing the translated field directly: type to edit, Tab to (re)translate from
    // the source. Single-line (title): Enter accepts, Esc cancels. Multiline
    // (synopsis): Enter adds a newline, Esc returns to the settled view to save.
    if st.edit_translation {
        let opts = EditOpts {
            numeric_only: false,
            multiline: st.multiline,
        };
        if input::handle(
            &mut st.translated_text,
            &mut st.translated_cursor,
            key,
            opts,
        ) != Edited::Ignored
        {
            return SynKey::None;
        }
        return match key.code {
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => SynKey::Accept,
            KeyCode::Tab => {
                if st.raw.trim().is_empty() {
                    SynKey::None
                } else {
                    st.phase = SynPhase::Translating;
                    SynKey::Translate
                }
            }
            KeyCode::Enter if st.multiline => {
                input::insert_char(&mut st.translated_text, &mut st.translated_cursor, '\n');
                SynKey::None
            }
            KeyCode::Enter => SynKey::Accept,
            KeyCode::Esc if st.multiline => {
                st.edit_translation = false;
                SynKey::None
            }
            KeyCode::Esc => SynKey::Back,
            _ => SynKey::None,
        };
    }
    match st.phase {
        SynPhase::Translating => SynKey::None,
        SynPhase::Editing => {
            let opts = EditOpts {
                numeric_only: false,
                multiline: st.multiline,
            };
            if input::handle(&mut st.raw, &mut st.cursor, key, opts) != Edited::Ignored {
                return SynKey::None;
            }
            match key.code {
                KeyCode::Esc => SynKey::Back,
                // Continue without translating: keep the source as-is and move on.
                KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if st.raw.trim().is_empty() {
                        SynKey::Skip
                    } else {
                        SynKey::Accept
                    }
                }
                KeyCode::Tab => {
                    if st.raw.trim().is_empty() {
                        SynKey::Skip
                    } else {
                        st.phase = SynPhase::Translating;
                        SynKey::Translate
                    }
                }
                KeyCode::Enter if st.multiline => {
                    input::insert_char(&mut st.raw, &mut st.cursor, '\n');
                    SynKey::None
                }
                KeyCode::Enter => {
                    if st.raw.trim().is_empty() {
                        SynKey::None
                    } else {
                        st.phase = SynPhase::Translating;
                        SynKey::Translate
                    }
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
            // Edit the translation by hand.
            KeyCode::Char('e') | KeyCode::Char('E') => {
                st.edit_translation = true;
                st.translated_cursor = st.translated_text.len();
                SynKey::None
            }
            // Edit the original source again.
            KeyCode::Char('o') | KeyCode::Char('O') => {
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
    pub target_language: TargetLanguage,
    pub syn: SynopsisState,
}

/// Project title editor state; `id` is the stable project slug.
#[derive(Debug, Clone)]
pub struct TitleEditState {
    pub id: String,
    pub target_language: TargetLanguage,
    pub syn: SynopsisState,
}

/// A snapshot of an existing project, carried by the import wizard so it can give
/// live merge feedback (name collides → "adds into …", existing-volume warnings).
#[derive(Debug, Clone)]
pub struct ProjectRef {
    /// Stable slug = directory name.
    pub slug: String,
    pub title: String,
    pub target_language: TargetLanguage,
    /// (volume number, chapter count), ascending.
    pub volumes: Vec<(u32, usize)>,
}

impl ProjectRef {
    pub fn of(p: &crate::model::Project) -> Self {
        Self {
            slug: p.id.clone(),
            title: p.title.clone(),
            target_language: p.target_language,
            volumes: p
                .volumes
                .iter()
                .map(|v| (v.number, v.chapters.len()))
                .collect(),
        }
    }
}

/// The import wizard: pick source file → name → translated title → volume → synopsis →
/// importing. When `lock_name` is set (the "add volume to this project" flow),
/// the name and translated-title steps are skipped and the title is fixed to the open
/// project's.
#[derive(Debug, Clone)]
pub struct ImportState {
    /// 0 = pick, 1 = name, 2 = translated title, 3 = volume, 4 = synopsis, 5 = importing.
    pub step: u8,
    /// Importable source files (path, byte size) found in the working root.
    pub files: Vec<(PathBuf, u64)>,
    pub sel: usize,
    pub name: String,
    /// Caret byte-offset into `name` (the wizard's name step).
    pub name_cursor: usize,
    /// True once the user edited the name; stops re-seeding it from the file stem.
    pub name_touched: bool,
    pub vol: u32,
    /// True once the user adjusted the volume; stops the next-volume auto-suggest.
    pub vol_touched: bool,
    /// True for the "add volume" flow: the name is the open project's and locked,
    /// so the wizard skips the name step (pick → volume → synopsis).
    pub lock_name: bool,
    /// Language for a new project, seeded from the app preference.
    pub target_language: TargetLanguage,
    /// Existing projects, for merge detection and existing-volume feedback.
    pub projects: Vec<ProjectRef>,
    /// Transient validation note (name step); cleared on the next edit.
    pub note: Option<&'static str>,
    /// Translated-title reroll loop (wizard step 2); `raw` mirrors `name`.
    pub title_syn: SynopsisState,
    /// Synopsis input + translate/reroll loop (wizard step 4).
    pub syn: SynopsisState,
    /// Live preprocessing progress (done, total, label) once the import starts.
    pub progress: Option<(usize, usize, String)>,
    /// Append mode: when `Some(vol)`, the pick step launches the import straight
    /// away (no name/title/synopsis steps) and chapters land after `vol`'s last.
    pub append_to: Option<u32>,
}

impl ImportState {
    fn new(
        files: Vec<(PathBuf, u64)>,
        projects: Vec<ProjectRef>,
        preferred_language: TargetLanguage,
    ) -> Self {
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
            target_language: preferred_language,
            projects,
            note: None,
            title_syn: SynopsisState::new_title(String::new(), String::new()),
            syn: SynopsisState::new(String::new(), String::new()),
            progress: None,
            append_to: None,
        }
    }

    /// "Add chapters" wizard: pick a source file and append its chapters after the
    /// existing volume `vol`'s last chapter. The name is the open project's (so the
    /// import resolves to its directory); every wizard step but the pick is skipped.
    fn new_append(
        files: Vec<(PathBuf, u64)>,
        projects: Vec<ProjectRef>,
        title: String,
        vol: u32,
        target_language: TargetLanguage,
    ) -> Self {
        Self {
            append_to: Some(vol.max(1)),
            ..Self::new_into(files, projects, title, vol, target_language)
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
        target_language: TargetLanguage,
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
            target_language,
            projects,
            note: None,
            title_syn: SynopsisState::new_title(String::new(), String::new()),
            syn: SynopsisState::new(String::new(), String::new()),
            progress: None,
            append_to: None,
        }
    }

    pub fn selected_file(&self) -> Option<&PathBuf> {
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

    pub fn effective_target_language(&self) -> TargetLanguage {
        self.target_project()
            .map(|project| project.target_language)
            .unwrap_or(self.target_language)
    }

    /// When the name targets an existing project and the user hasn't picked a
    /// volume yet, default to one past its highest (instead of a colliding 1).
    pub fn suggest_volume(&mut self) {
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

#[derive(Debug, Clone)]
pub struct ImageSourceState {
    pub vol: u32,
    pub files: Vec<(PathBuf, u64)>,
    pub sel: usize,
}

impl ImageSourceState {
    fn new(files: Vec<(PathBuf, u64)>, vol: u32) -> Self {
        Self { vol, files, sel: 0 }
    }

    pub fn selected_file(&self) -> Option<&PathBuf> {
        self.files.get(self.sel).map(|(p, _)| p)
    }

    pub fn set_files(&mut self, files: Vec<(PathBuf, u64)>) {
        self.files = files;
        self.sel = self.sel.min(self.files.len().saturating_sub(1));
    }
}

/// One focusable Settings field. The order of [`SETTINGS_ORDER`] is the on-screen
/// order and the index space `SettingsState::field` walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SField {
    OrchProvider,
    OrchModel,
    OrchEffort,
    TransProvider,
    TransModel,
    TransEffort,
    ReviewProvider,
    ReviewModel,
    ReviewEffort,
    RefineProvider,
    RefineModel,
    RefineEffort,
    OpenRouterKey,
    TokenrouterKey,
    GoogleKey,
    CloudflareAccount,
    CloudflareToken,
    PreferredLanguageField,
    MaxAttempts,
    ContinuitySentences,
    LoopStall,
    Retranslates,
    ServiceTierField,
    ParallelLookahead,
    SystemOneEnabled,
    GateMode,
    GateProvider,
    GateModel,
    GateKey,
    GateConfidence,
    FeatAudit,
    FeatContinuity,
    FeatEntityAlignment,
    FeatSegmentation,
    FeatReferenceScope,
    UpdateModeField,
    ReleaseChannelField,
}

/// The per-feature toggle rows, in render order. `None` for the rows that are
/// not feature toggles keeps [`SETTINGS_ORDER`] the single source of ordering.
fn field_feature(field: SField) -> Option<crate::model::SystemOneFeature> {
    use crate::model::SystemOneFeature as F;
    Some(match field {
        SField::FeatAudit => F::Audit,
        SField::FeatContinuity => F::Continuity,
        SField::FeatEntityAlignment => F::EntityAlignment,
        SField::FeatSegmentation => F::Segmentation,
        SField::FeatReferenceScope => F::ReferenceScope,
        _ => return None,
    })
}

const SETTINGS_ORDER: [SField; 37] = [
    SField::OrchProvider,
    SField::OrchModel,
    SField::OrchEffort,
    SField::TransProvider,
    SField::TransModel,
    SField::TransEffort,
    SField::ReviewProvider,
    SField::ReviewModel,
    SField::ReviewEffort,
    SField::RefineProvider,
    SField::RefineModel,
    SField::RefineEffort,
    SField::OpenRouterKey,
    SField::TokenrouterKey,
    SField::GoogleKey,
    SField::CloudflareAccount,
    SField::CloudflareToken,
    SField::PreferredLanguageField,
    SField::MaxAttempts,
    SField::ContinuitySentences,
    SField::LoopStall,
    SField::Retranslates,
    SField::ServiceTierField,
    SField::ParallelLookahead,
    SField::SystemOneEnabled,
    SField::GateMode,
    SField::GateProvider,
    SField::GateModel,
    SField::GateKey,
    SField::GateConfidence,
    SField::FeatAudit,
    SField::FeatContinuity,
    SField::FeatEntityAlignment,
    SField::FeatSegmentation,
    SField::FeatReferenceScope,
    SField::UpdateModeField,
    SField::ReleaseChannelField,
];

/// Number of focusable Settings fields.
const SETTINGS_FIELDS: u8 = SETTINGS_ORDER.len() as u8;
/// Index of the first per-feature System One toggle, which the renderer walks
/// alongside `SystemOneFeature::ALL`. Asserted against `SETTINGS_ORDER` in tests.
const FIRST_FEATURE_FIELD: u8 = 30;
/// Fallback Codex model ids until the live list arrives.
const CODEX_MODELS: [&str; 3] = ["gpt-5.5", "gpt-5.4", "gpt-5.4-mini"];

fn default_codex_models() -> Vec<String> {
    CODEX_MODELS.iter().map(|s| s.to_string()).collect()
}

fn provider_model_fallback(
    current: crate::model::Provider,
    next: crate::model::Provider,
    current_model: &str,
    codex_models: &[String],
) -> String {
    if matches!(
        (current, next),
        (
            crate::model::Provider::OpenRouter,
            crate::model::Provider::Tokenrouter
        ) | (
            crate::model::Provider::Tokenrouter,
            crate::model::Provider::OpenRouter
        )
    ) && !current_model.trim().is_empty()
    {
        return current_model.to_string();
    }
    if next == crate::model::Provider::Codex
        && let Some(first) = codex_models.first()
    {
        return first.clone();
    }
    next.default_model().to_string()
}
/// Index of the OpenRouter API-key field (callers open Settings focused here).
pub const SETTINGS_KEY_FIELD: u8 = 12;

/// Settings tabs group contiguous field ranges; Account has only actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    Agents,
    Providers,
    Pipeline,
    Appearance,
    Account,
}

impl SettingsTab {
    pub const ALL: [SettingsTab; 5] = [
        SettingsTab::Agents,
        SettingsTab::Providers,
        SettingsTab::Pipeline,
        SettingsTab::Appearance,
        SettingsTab::Account,
    ];

    pub fn title(self) -> &'static str {
        match self {
            SettingsTab::Agents => "Agents",
            SettingsTab::Providers => "Providers",
            SettingsTab::Pipeline => "Pipeline",
            SettingsTab::Appearance => "Appearance",
            SettingsTab::Account => "Account",
        }
    }

    /// Focusable field range; Account has none.
    fn field_range(self) -> Option<(u8, u8)> {
        Some(match self {
            SettingsTab::Agents => (0, 12),
            SettingsTab::Providers => (12, 17),
            SettingsTab::Pipeline => (17, 35),
            SettingsTab::Appearance => (35, 37),
            SettingsTab::Account => return None,
        })
    }

    fn for_field(field: u8) -> SettingsTab {
        SettingsTab::ALL
            .into_iter()
            .find(|t| {
                t.field_range()
                    .is_some_and(|(s, e)| field >= s && field < e)
            })
            .unwrap_or(SettingsTab::Agents)
    }

    fn cycled(self, forward: bool) -> SettingsTab {
        let i = SettingsTab::ALL
            .iter()
            .position(|t| *t == self)
            .unwrap_or(0);
        SettingsTab::ALL[step(i, SettingsTab::ALL.len(), forward)]
    }
}

impl SField {
    /// A free-text editable field (vs. a Left/Right cycle field).
    fn is_text(self) -> bool {
        matches!(
            self,
            SField::OrchModel
                | SField::TransModel
                | SField::ReviewModel
                | SField::RefineModel
                | SField::OpenRouterKey
                | SField::TokenrouterKey
                | SField::GoogleKey
                | SField::CloudflareAccount
                | SField::CloudflareToken
                | SField::MaxAttempts
                | SField::ContinuitySentences
                | SField::LoopStall
                | SField::Retranslates
                | SField::GateModel
                | SField::GateKey
                | SField::GateConfidence
        )
    }

    /// A digits-only numeric field.
    fn is_numeric(self) -> bool {
        matches!(
            self,
            SField::MaxAttempts
                | SField::ContinuitySentences
                | SField::LoopStall
                | SField::Retranslates
                | SField::GateConfidence
        )
    }

    /// A masked secret (API key) field.
    fn is_secret(self) -> bool {
        matches!(
            self,
            SField::OpenRouterKey
                | SField::TokenrouterKey
                | SField::GoogleKey
                | SField::CloudflareToken
                | SField::GateKey
        )
    }
}

/// Step an index forward/backward through a wrapped cycle of `len` items.
fn step(i: usize, len: usize, forward: bool) -> usize {
    if forward {
        (i + 1) % len
    } else {
        (i + len - 1) % len
    }
}

/// Settings: per-agent provider/model/effort, provider keys, pipeline limits, and
/// the appearance + account controls.
#[derive(Debug, Clone)]
pub struct SettingsState {
    /// Working copy of the per-agent provider/model/effort selection.
    pub models: crate::model::ModelSet,
    /// The config-stored OpenRouter key, editable here (masked). Empty = none.
    pub openrouter_key: String,
    /// True when an env var (HONYA_API_KEY / OPENROUTER_API_KEY) supplies the key;
    /// it overrides config, so the field is shown read-only.
    pub api_key_env: bool,
    /// The config-stored Tokenrouter key, editable here (masked). Empty = none.
    pub tokenrouter_key: String,
    /// True when an env var supplies the Tokenrouter key (shown read-only).
    pub tokenrouter_key_env: bool,
    /// The config-stored Google key, editable here (masked). Empty = none.
    pub google_key: String,
    /// True when an env var supplies the Google key (shown read-only).
    pub google_key_env: bool,
    /// Cloudflare Workers AI account id, editable unless supplied by env.
    pub cloudflare_account_id: String,
    /// True when an env var supplies the Cloudflare account id (shown read-only).
    pub cloudflare_account_id_env: bool,
    /// Cloudflare Workers AI API token, editable unless supplied by env.
    pub cloudflare_api_token: String,
    /// True when an env var supplies the Cloudflare API token (shown read-only).
    pub cloudflare_api_token_env: bool,
    /// Startup update behavior (cycle field; also Ctrl-U).
    pub update_mode: UpdateMode,
    /// Update channel (cycle field; also Ctrl-G).
    pub release_channel: ReleaseChannel,
    /// Request tier (cycle field; also Ctrl-Y).
    pub service_tier: Option<ServiceTier>,
    /// Validate and reuse one speculative next-chunk Translator draft.
    pub parallel_lookahead: bool,
    /// Default language initially selected when creating a project.
    pub preferred_language: TargetLanguage,
    /// Max Translator↔Reviewer retry attempts per chunk, as typed (digits only).
    pub max_attempts: String,
    /// Prior translated sentences injected per chunk, as typed (digits only).
    pub continuity_sentences: String,
    /// Loop-watchdog stall window in seconds, as typed (digits only; 0 disables).
    pub loop_stall_secs: String,
    /// Whole-chapter re-translations allowed on a detected loop, as typed (digits).
    pub max_chapter_retranslates: String,
    /// Working System One copy: master switch, transport, model id (edited in
    /// place as a text field) and the per-feature toggles. Boxed because
    /// `SettingsState` is already the largest `Overlay` variant.
    pub system_one: Box<crate::model::SystemOne>,
    /// Minimum confidence, typed as a whole percent (digits only). Kept apart
    /// from `system_one.min_confidence` because the field is edited as text.
    pub system_one_confidence: String,
    /// TypeSafe key, editable here (masked). Only the TypeSafe transport needs it.
    pub typesafe_key: String,
    /// True when an env var supplies the TypeSafe key (shown read-only).
    pub typesafe_key_env: bool,
    pub tab: SettingsTab,
    /// Which field is focused (index into [`SETTINGS_ORDER`]).
    pub field: u8,
    /// Caret byte-offset into the focused text field. Secret fields edit at the end.
    pub cursor: usize,
    /// Codex model picker options.
    pub codex_models: Vec<String>,
    pub account_login: Option<String>,
    pub remote_enabled: bool,
    pub remote_state: crate::remote::protocol::RemoteState,
    pub remote_watchers: u32,
    pub remote_auth_code: Option<crate::model::AuthCodePrompt>,
    /// Dashboard label for the live remote session.
    pub session_label: Option<String>,
}

impl SettingsState {
    fn from_cfg_focus(cfg: &AppConfig, field: u8) -> Self {
        let mut st = Self {
            models: cfg.models.clone(),
            openrouter_key: cfg.api_key.clone().unwrap_or_default(),
            api_key_env: crate::config::api_key_from_env().is_some(),
            tokenrouter_key: cfg.tokenrouter_api_key.clone().unwrap_or_default(),
            tokenrouter_key_env: crate::config::tokenrouter_key_from_env().is_some(),
            google_key: cfg.google_api_key.clone().unwrap_or_default(),
            google_key_env: crate::config::google_key_from_env().is_some(),
            cloudflare_account_id: cfg.cloudflare_account_id.clone().unwrap_or_default(),
            cloudflare_account_id_env: crate::config::cloudflare_account_id_from_env().is_some(),
            cloudflare_api_token: cfg.cloudflare_api_token.clone().unwrap_or_default(),
            cloudflare_api_token_env: crate::config::cloudflare_api_token_from_env().is_some(),
            update_mode: cfg.update_mode,
            release_channel: cfg.release_channel,
            service_tier: cfg.service_tier,
            parallel_lookahead: cfg.parallel_lookahead,
            preferred_language: cfg.preferred_language,
            max_attempts: cfg.max_attempts.to_string(),
            continuity_sentences: cfg.continuity_sentences.to_string(),
            loop_stall_secs: cfg.loop_stall_secs.to_string(),
            max_chapter_retranslates: cfg.max_chapter_retranslates.to_string(),
            system_one: Box::new(cfg.system_one.clone()),
            system_one_confidence: ((cfg.system_one.confidence_threshold() * 100.0).round()
                as u32)
                .to_string(),
            typesafe_key: cfg.typesafe_api_key.clone().unwrap_or_default(),
            typesafe_key_env: crate::config::typesafe_key_from_env().is_some(),
            tab: SettingsTab::Agents,
            field: 0,
            cursor: 0,
            codex_models: default_codex_models(),
            account_login: cfg.account.as_ref().map(|a| a.github_login.clone()),
            // App syncs live remote values after opening Settings.
            remote_enabled: false,
            remote_state: crate::remote::protocol::RemoteState::Disconnected,
            remote_watchers: 0,
            remote_auth_code: None,
            session_label: None,
        };
        st.focus(field.min(SETTINGS_FIELDS - 1));
        st.tab = SettingsTab::for_field(st.field);
        st
    }

    /// A default-config Settings state focused on `field` (tests only).
    #[cfg(test)]
    pub fn for_test(field: u8) -> Self {
        Self::from_cfg_focus(&AppConfig::default(), field)
    }

    /// The currently focused field.
    fn current(&self) -> SField {
        SETTINGS_ORDER[self.field as usize]
    }

    /// Mutable handle to the focused text buffer (None for cycle fields).
    fn text_field_mut(&mut self) -> Option<&mut String> {
        Some(match self.current() {
            SField::OrchModel => &mut self.models.orchestrator.model,
            SField::TransModel => &mut self.models.translator.model,
            SField::ReviewModel => &mut self.models.reviewer.model,
            SField::RefineModel => &mut self.models.refine.model,
            SField::OpenRouterKey => &mut self.openrouter_key,
            SField::TokenrouterKey => &mut self.tokenrouter_key,
            SField::GoogleKey => &mut self.google_key,
            SField::CloudflareAccount => &mut self.cloudflare_account_id,
            SField::CloudflareToken => &mut self.cloudflare_api_token,
            SField::MaxAttempts => &mut self.max_attempts,
            SField::ContinuitySentences => &mut self.continuity_sentences,
            SField::LoopStall => &mut self.loop_stall_secs,
            SField::Retranslates => &mut self.max_chapter_retranslates,
            SField::GateModel => &mut self.system_one.model,
            SField::GateKey => &mut self.typesafe_key,
            SField::GateConfidence => &mut self.system_one_confidence,
            _ => return None,
        })
    }

    /// The working agent for an agent-row field, if this field belongs to one.
    fn agent_for(&mut self, field: SField) -> Option<&mut crate::model::AgentModel> {
        Some(match field {
            SField::OrchProvider | SField::OrchModel | SField::OrchEffort => {
                &mut self.models.orchestrator
            }
            SField::TransProvider | SField::TransModel | SField::TransEffort => {
                &mut self.models.translator
            }
            SField::ReviewProvider | SField::ReviewModel | SField::ReviewEffort => {
                &mut self.models.reviewer
            }
            SField::RefineProvider | SField::RefineModel | SField::RefineEffort => {
                &mut self.models.refine
            }
            _ => return None,
        })
    }

    /// The provider of the agent owning the focused field, if it's an agent field.
    fn agent_provider(&self) -> Option<crate::model::Provider> {
        Some(match self.current() {
            SField::OrchProvider | SField::OrchModel | SField::OrchEffort => {
                self.models.orchestrator.provider
            }
            SField::TransProvider | SField::TransModel | SField::TransEffort => {
                self.models.translator.provider
            }
            SField::ReviewProvider | SField::ReviewModel | SField::ReviewEffort => {
                self.models.reviewer.provider
            }
            SField::RefineProvider | SField::RefineModel | SField::RefineEffort => {
                self.models.refine.provider
            }
            _ => return None,
        })
    }

    /// A model field whose provider is Codex — picked from a list, not typed.
    fn is_codex_model(&self) -> bool {
        matches!(
            self.current(),
            SField::OrchModel | SField::TransModel | SField::ReviewModel | SField::RefineModel
        ) && self.agent_provider() == Some(crate::model::Provider::Codex)
    }

    /// Whether the focused field accepts typed text (vs. a Left/Right choice).
    fn current_is_editable_text(&self) -> bool {
        self.current().is_text() && !self.is_codex_model()
    }

    /// Cycle the focused non-text field. `forward` is Right/Space; `false` is Left.
    fn cycle(&mut self, forward: bool) {
        let cur = self.current();
        if self.is_codex_model() {
            let models = self.codex_models.clone();
            if let Some(a) = self.agent_for(cur)
                && !models.is_empty()
            {
                let next = match models.iter().position(|m| *m == a.model) {
                    Some(i) => step(i, models.len(), forward),
                    None => 0,
                };
                a.set_model(models[next].clone());
            }
            return;
        }
        let codex_models = self.codex_models.clone();
        match cur {
            SField::OrchProvider
            | SField::TransProvider
            | SField::ReviewProvider
            | SField::RefineProvider => {
                if let Some(a) = self.agent_for(cur) {
                    let current = a.provider;
                    let next = a.provider.cycled(forward);
                    let fallback = provider_model_fallback(current, next, &a.model, &codex_models);
                    a.switch_provider(next, Some(&fallback));
                }
            }
            SField::OrchEffort
            | SField::TransEffort
            | SField::ReviewEffort
            | SField::RefineEffort => {
                const E: [Option<crate::model::Effort>; 6] = [
                    None,
                    Some(crate::model::Effort::Minimal),
                    Some(crate::model::Effort::Low),
                    Some(crate::model::Effort::Medium),
                    Some(crate::model::Effort::High),
                    Some(crate::model::Effort::Xhigh),
                ];
                if let Some(a) = self.agent_for(cur) {
                    let i = E.iter().position(|e| *e == a.effort).unwrap_or(0);
                    a.effort = E[step(i, E.len(), forward)];
                }
            }
            SField::ServiceTierField => {
                const T: [Option<ServiceTier>; 3] =
                    [None, Some(ServiceTier::Flex), Some(ServiceTier::Priority)];
                let i = T.iter().position(|t| *t == self.service_tier).unwrap_or(0);
                self.service_tier = T[step(i, T.len(), forward)];
            }
            SField::PreferredLanguageField => {
                self.preferred_language = self.preferred_language.cycled();
            }
            SField::ParallelLookahead => self.parallel_lookahead = !self.parallel_lookahead,
            SField::SystemOneEnabled => self.system_one.enabled = !self.system_one.enabled,
            SField::GateMode => {
                self.system_one.review_gate = self.system_one.review_gate.cycled(forward)
            }
            SField::GateProvider => {
                let next = self.system_one.provider.cycled(forward);
                self.system_one.switch_provider(next);
            }
            f if field_feature(f).is_some() => {
                let feature = field_feature(f).expect("guarded by the match arm");
                let slot = self.system_one.feature_mut(feature);
                *slot = !*slot;
            }
            SField::UpdateModeField => self.update_mode = self.update_mode.toggled(),
            SField::ReleaseChannelField => self.release_channel = self.release_channel.toggled(),
            _ => {}
        }
    }

    /// Focus a field and drop the caret at its end.
    fn focus(&mut self, field: u8) {
        self.field = field % SETTINGS_FIELDS;
        self.cursor = self.text_field_mut().map(|s| s.len()).unwrap_or(0);
    }

    fn next_field(&mut self) {
        if let Some((start, end)) = self.tab.field_range() {
            let next = if self.field + 1 >= end {
                start
            } else {
                self.field + 1
            };
            self.focus(next);
        }
    }

    fn prev_field(&mut self) {
        if let Some((start, end)) = self.tab.field_range() {
            let prev = if self.field <= start {
                end - 1
            } else {
                self.field - 1
            };
            self.focus(prev);
        }
    }

    fn switch_tab(&mut self, forward: bool) {
        self.tab = self.tab.cycled(forward);
        if let Some((start, _)) = self.tab.field_range() {
            self.focus(start);
        }
    }

    fn tab_has_fields(&self) -> bool {
        self.tab.field_range().is_some()
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

    /// Prior translated sentences included with each chunk (0 disables it).
    /// Empty or invalid input falls back to the default; capped at 100.
    fn continuity_sentences_value(&self) -> usize {
        let raw = self.continuity_sentences.trim();
        if raw.is_empty() {
            return AppConfig::default().continuity_sentences;
        }
        raw.parse::<usize>()
            .unwrap_or_else(|_| AppConfig::default().continuity_sentences)
            .min(100)
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

    /// The `SaveSettings` action for the current working copy — shared by the
    /// TUI's Enter key and the GUI's Save button. Env-supplied keys stay `None`
    /// so they never overwrite saved config.
    pub fn save_action(&self) -> Action {
        let mut models = self.models.clone();
        models.remember_active_models();
        Action::SaveSettings {
            models: Box::new(models),
            openrouter_key: (!self.api_key_env).then(|| self.openrouter_key.clone()),
            tokenrouter_key: (!self.tokenrouter_key_env).then(|| self.tokenrouter_key.clone()),
            google_key: (!self.google_key_env).then(|| self.google_key.clone()),
            cloudflare_account_id: (!self.cloudflare_account_id_env)
                .then(|| self.cloudflare_account_id.clone()),
            cloudflare_api_token: (!self.cloudflare_api_token_env)
                .then(|| self.cloudflare_api_token.clone()),
            update_mode: self.update_mode,
            release_channel: self.release_channel,
            service_tier: self.service_tier,
            preferred_language: self.preferred_language,
            max_attempts: self.max_attempts_value(),
            continuity_sentences: self.continuity_sentences_value(),
            loop_stall_secs: self.loop_stall_secs_value(),
            max_chapter_retranslates: self.max_chapter_retranslates_value(),
            parallel_lookahead: self.parallel_lookahead,
            system_one: Box::new(self.system_one()),
            typesafe_key: (!self.typesafe_key_env).then(|| self.typesafe_key.clone()),
        }
    }

    /// The working System One config assembled from the edited fields.
    fn system_one(&self) -> crate::model::SystemOne {
        crate::model::SystemOne {
            min_confidence: f64::from(
                self.system_one_confidence
                    .trim()
                    .parse::<u32>()
                    .unwrap_or(80)
                    .min(100),
            ) / 100.0,
            ..(*self.system_one).clone()
        }
    }

    /// Switch one agent's provider (0 orchestrator · 1 translator · 2 reviewer ·
    /// 3 refine), carrying the same model fallbacks as the TUI's cycle key.
    pub fn switch_agent_provider(&mut self, agent: usize, next: crate::model::Provider) {
        let codex_models = self.codex_models.clone();
        let a = match agent {
            0 => &mut self.models.orchestrator,
            1 => &mut self.models.translator,
            2 => &mut self.models.reviewer,
            _ => &mut self.models.refine,
        };
        let fallback = provider_model_fallback(a.provider, next, &a.model, &codex_models);
        a.switch_provider(next, Some(&fallback));
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
                label: "Getting started",
                action: Action::show_overlay(Overlay::welcome_placeholder()),
            },
            PaletteItem {
                label: "Go: Shelf",
                action: Action::Goto(Screen::Shelf),
            },
            PaletteItem {
                label: "Go: Project",
                action: Action::Goto(Screen::Project),
            },
            PaletteItem {
                label: "Go: Translate",
                action: Action::Goto(Screen::Translate),
            },
            PaletteItem {
                label: "Translate whole project",
                action: Action::StartProjectTranslation,
            },
            PaletteItem {
                label: "Go: Reader",
                action: Action::Goto(Screen::Reader),
            },
            PaletteItem {
                label: "Go: Lexicon",
                action: Action::Goto(Screen::Lexicon),
            },
            PaletteItem {
                label: "Settings",
                action: Action::show_overlay(Overlay::settings_placeholder()),
            },
            PaletteItem {
                label: "Theme",
                action: Action::show_overlay(Overlay::theme_placeholder()),
            },
            PaletteItem {
                label: "Help",
                action: Action::show_overlay(Overlay::Help(0)),
            },
            PaletteItem {
                label: "About",
                action: Action::show_overlay(Overlay::About),
            },
            PaletteItem {
                label: "QA review",
                action: Action::show_overlay(Overlay::qa_placeholder()),
            },
            PaletteItem {
                label: "Activity log",
                action: Action::show_overlay(Overlay::Log(0)),
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
    pub fn matches(&self) -> Vec<usize> {
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

/// Chunk proofreading popover: source, translation, and active reviewer note.
#[derive(Debug, Clone)]
pub struct ReaderInspectState {
    pub chapter: u32,
    pub chunk: u32,
    pub source_jp: String,
    pub translated_text: String,
    pub review: Option<String>,
    pub scroll: u16,
}

/// Editor for one translated chunk; saving clears any review-needed flag.
#[derive(Debug, Clone)]
pub struct ReaderEditState {
    pub chapter: u32,
    pub chunk: u32,
    pub text: String,
    /// Caret byte-offset into `text`.
    pub cursor: usize,
}

/// Reader global-search input: a single text field. On commit the App hands the
/// query to the Reader, which finds matches across both source and translation panes.
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
    ImageSource(ImageSourceState),
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
    ProjectTitle(TitleEditState),
    /// Translation QA inbox — per-chapter issue counts + navigable findings, opened
    /// from the Project or Reader tab (Enter jumps to the chapter in the Reader).
    Qa(QaState),
    /// Reader proofreading note editor, anchored to a translated line.
    ReaderNote(ReaderNoteState),
    /// Read-only source‖translation‖reviewer-note popover for the current chunk.
    ReaderInspect(ReaderInspectState),
    /// In-place editor for the current chunk's translated prose.
    ReaderEdit(ReaderEditState),
    /// Reader global search across both panes.
    ReaderSearch(ReaderSearchState),
    /// Reader jump/outline picker (chapters · sections · bookmarks).
    ReaderJump(ReaderJumpState),
    /// Export the active volume to deliverable formats (Markdown · EPUB · DOCX).
    Export(ExportState),
}

impl Overlay {
    pub fn import(
        files: Vec<(PathBuf, u64)>,
        projects: &[crate::model::Project],
        preferred_language: TargetLanguage,
    ) -> Self {
        let refs = projects.iter().map(ProjectRef::of).collect();
        Overlay::Import(ImportState::new(files, refs, preferred_language))
    }

    /// "Add volume" wizard, pre-targeted at an open project: the name is locked to
    /// `title` and the volume defaults to `vol` (the project's next number).
    pub fn import_into(
        files: Vec<(PathBuf, u64)>,
        projects: &[crate::model::Project],
        title: String,
        vol: u32,
        target_language: TargetLanguage,
    ) -> Self {
        let refs = projects.iter().map(ProjectRef::of).collect();
        Overlay::Import(ImportState::new_into(
            files,
            refs,
            title,
            vol,
            target_language,
        ))
    }

    /// "Add chapters" wizard, pre-targeted at an open project's volume `vol`: the
    /// pick step launches the import immediately and chapters append after `vol`'s
    /// last chapter.
    pub fn import_append(
        files: Vec<(PathBuf, u64)>,
        projects: &[crate::model::Project],
        title: String,
        vol: u32,
        target_language: TargetLanguage,
    ) -> Self {
        let refs = projects.iter().map(ProjectRef::of).collect();
        Overlay::Import(ImportState::new_append(
            files,
            refs,
            title,
            vol,
            target_language,
        ))
    }

    pub fn image_source(files: Vec<(PathBuf, u64)>, vol: u32) -> Self {
        Overlay::ImageSource(ImageSourceState::new(files, vol))
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

    /// Standalone synopsis editor seeded from stored source/translation text; `vol`
    /// and `title` name the target in the modal title.
    pub fn synopsis_edit(
        raw: String,
        translated_text: String,
        vol: u32,
        title: String,
        target_language: TargetLanguage,
    ) -> Self {
        Overlay::Synopsis(SynopsisEditState {
            vol,
            title,
            target_language,
            syn: SynopsisState::new(raw, translated_text),
        })
    }

    pub fn project_title_edit(
        id: String,
        title: String,
        translated_title: String,
        target_language: TargetLanguage,
    ) -> Self {
        Overlay::ProjectTitle(TitleEditState {
            id,
            target_language,
            syn: SynopsisState::new_title(title, translated_title),
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

    pub fn reader_inspect(
        chapter: u32,
        chunk: u32,
        source_jp: String,
        translated_text: String,
        review: Option<String>,
    ) -> Self {
        Overlay::ReaderInspect(ReaderInspectState {
            chapter,
            chunk,
            source_jp,
            translated_text,
            review,
            scroll: 0,
        })
    }

    pub fn reader_edit(chapter: u32, chunk: u32, text: String) -> Self {
        let cursor = text.len();
        Overlay::ReaderEdit(ReaderEditState {
            chapter,
            chunk,
            text,
            cursor,
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
    pub fn settings_at(field: u8) -> Self {
        Overlay::Settings(SettingsState {
            models: crate::model::ModelSet::default(),
            openrouter_key: String::new(),
            api_key_env: false,
            tokenrouter_key: String::new(),
            tokenrouter_key_env: false,
            google_key: String::new(),
            google_key_env: false,
            cloudflare_account_id: String::new(),
            cloudflare_account_id_env: false,
            cloudflare_api_token: String::new(),
            cloudflare_api_token_env: false,
            typesafe_key: String::new(),
            typesafe_key_env: false,
            system_one: Box::new(crate::model::SystemOne::default()),
            system_one_confidence: "80".to_string(),
            update_mode: UpdateMode::default(),
            release_channel: ReleaseChannel::default(),
            service_tier: None,
            parallel_lookahead: true,
            preferred_language: TargetLanguage::default(),
            max_attempts: String::new(),
            continuity_sentences: String::new(),
            loop_stall_secs: String::new(),
            max_chapter_retranslates: String::new(),
            tab: SettingsTab::for_field(field.min(SETTINGS_FIELDS - 1)),
            field: field.min(SETTINGS_FIELDS - 1),
            cursor: 0,
            codex_models: default_codex_models(),
            account_login: None,
            remote_enabled: false,
            remote_state: crate::remote::protocol::RemoteState::Disconnected,
            remote_watchers: 0,
            remote_auth_code: None,
            session_label: None,
        })
    }

    fn settings_placeholder() -> Self {
        Overlay::settings_at(0)
    }

    /// Settings opened on the Account tab. That tab holds only actions, so no
    /// field index selects it and `settings_at` cannot reach it.
    pub fn settings_account() -> Self {
        let mut ov = Overlay::settings_at(0);
        if let Overlay::Settings(st) = &mut ov {
            st.tab = SettingsTab::Account;
        }
        ov
    }

    /// Export-volume overlay for `vol` (format checklist → gauge → results).
    pub fn export(vol: u32) -> Self {
        Overlay::Export(ExportState::new(vol))
    }

    pub fn set_import_progress(&mut self, done: usize, total: usize, label: &str) {
        if let Overlay::Import(st) = self {
            st.step = 5;
            st.progress = Some((done, total, label.to_string()));
        }
    }

    pub fn set_image_source_files(&mut self, files: Vec<(PathBuf, u64)>) {
        if let Overlay::ImageSource(st) = self {
            st.set_files(files);
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
            Overlay::Import(s) if s.step == 2 => &mut s.title_syn,
            Overlay::Import(s) if s.step == 4 => &mut s.syn,
            Overlay::Synopsis(s) => &mut s.syn,
            Overlay::ProjectTitle(s) => &mut s.syn,
            _ => return,
        };
        if st.phase != SynPhase::Translating {
            return;
        }
        match result {
            Ok(text) => {
                st.translated_text = text;
                st.translated_cursor = st.translated_text.len();
                st.error.clear();
                st.phase = SynPhase::Done;
                // Single-line title flow: drop the user back into editing the
                // result so they can tweak the agent's translation by hand.
                if !st.multiline {
                    st.edit_translation = true;
                }
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
                st.step == 1
                    || (st.step == 2 && st.title_syn.edit_translation)
                    || (st.step == 4
                        && (st.syn.phase == SynPhase::Editing || st.syn.edit_translation))
            }
            Overlay::Synopsis(st) => st.syn.phase == SynPhase::Editing || st.syn.edit_translation,
            Overlay::ProjectTitle(st) => {
                st.syn.edit_translation || st.syn.phase == SynPhase::Editing
            }
            Overlay::ReaderNote(_) => true,
            Overlay::ReaderEdit(_) => true,
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
            Overlay::ImageSource(_) => self.handle_image_source_key(key),
            Overlay::Settings(_) => self.handle_settings_key(key),
            Overlay::Theme(_) => self.handle_theme_key(key),
            Overlay::Palette(_) => self.handle_palette_key(key),
            Overlay::Modal(_) => self.handle_modal_key(key),
            Overlay::Synopsis(_) => self.handle_synopsis_overlay_key(key),
            Overlay::ProjectTitle(_) => self.handle_project_title_key(key),
            Overlay::Qa(_) => self.handle_qa_key(key),
            Overlay::ReaderNote(_) => self.handle_reader_note_key(key),
            Overlay::ReaderInspect(_) => self.handle_reader_inspect_key(key),
            Overlay::ReaderEdit(_) => self.handle_reader_edit_key(key),
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
        }
    }

    /// Mouse handling for a kit-rendered overlay: clicks resolve from the zone
    /// registry rather than from a restated copy of the layout.
    ///
    /// The wheel and right-click still go through the keyboard handlers, since
    /// scrolling and stepping back mean the same thing however they arrive.
    pub fn handle_mouse_zones(
        &mut self,
        m: MouseInput,
        zones: &crate::ui::kit::Zones,
    ) -> Action {
        match m.gesture {
            MouseGesture::ScrollUp => return self.handle_key(synth(KeyCode::Up)),
            MouseGesture::ScrollDown => return self.handle_key(synth(KeyCode::Down)),
            MouseGesture::RightClick => return self.handle_key(synth(KeyCode::Esc)),
            MouseGesture::Click { .. } => {}
        }
        let Some(id) = zones.at(m.col, m.row) else {
            return Action::None;
        };
        self.zone_action(id, m.is_double())
    }

    /// What a click on `id` means for this overlay.
    fn zone_action(&mut self, id: ZoneId, double: bool) -> Action {
        use crate::ui::kit::modal::CLOSE_BUTTON;

        // Shared frame behaviour first: these mean the same in every modal.
        match id.kind {
            // Clicking beside the modal steps back, exactly as Esc does.
            ZoneKind::Backdrop => return self.handle_key(synth(KeyCode::Esc)),
            ZoneKind::Button if id.index == CLOSE_BUTTON => {
                return self.handle_key(synth(KeyCode::Esc));
            }
            // Inside the modal but on nothing: inert, never a dismiss.
            ZoneKind::ModalFrame => return Action::None,
            _ => {}
        }

        match self {
            // Every button routes through its own keyboard accelerator rather
            // than re-deriving what the choice means. Confirm has to dismiss
            // the dialog before running the wrapped action, and stating that
            // twice is how the two paths drift apart.
            Overlay::Modal(dlg) => {
                let alt_key = dlg.alternate.as_ref().map(|a| a.key);
                match (id.kind, id.index) {
                    (ZoneKind::Button, DIALOG_CONFIRM) => {
                        self.handle_key(synth(KeyCode::Enter))
                    }
                    (ZoneKind::Button, DIALOG_CANCEL) => self.handle_key(synth(KeyCode::Esc)),
                    (ZoneKind::Button, DIALOG_ALTERNATE) => match alt_key {
                        Some(k) => self.handle_key(synth(KeyCode::Char(k))),
                        None => Action::None,
                    },
                    _ => Action::None,
                }
            }
            Overlay::Export(st) => match (id.kind, id.index) {
                (ZoneKind::Button, DIALOG_CANCEL) => Action::CloseOverlay,
                (ZoneKind::Button, DIALOG_CONFIRM) => self.handle_key(synth(KeyCode::Enter)),
                (ZoneKind::Row, i) if (i as usize) < st.formats.len() => {
                    st.sel = i as usize;
                    // A single click toggles, because a checklist row *is* the
                    // checkbox; a double click also runs the export.
                    st.formats[st.sel] = !st.formats[st.sel];
                    if double {
                        self.handle_key(synth(KeyCode::Enter))
                    } else {
                        Action::None
                    }
                }
                _ => Action::None,
            },
            Overlay::Theme(st) => match (id.kind, id.index) {
                (ZoneKind::Row, i) if (i as usize) < ALL_THEMES.len() => {
                    let already = st.sel == i as usize;
                    st.sel = i as usize;
                    if double || already {
                        self.handle_key(synth(KeyCode::Enter))
                    } else {
                        // A single click previews, so the list doubles as the
                        // preview surface.
                        Action::PreviewTheme(st.current())
                    }
                }
                _ => Action::None,
            },
            // Help, About and the Log are read-only: their rows do nothing, and
            // the frame behaviour above already covers closing them.
            _ => Action::None,
        }
    }

    /// The centered rectangle each overlay variant draws into (mirrors its render
    /// fn's `centered_modal` / `centered_pct` call), used for click hit-testing.
    fn modal_rect(&self, area: Rect) -> Rect {
        match self {
            // Kit-rendered overlays register the geometry they actually drew,
            // so nothing here may restate it — a second copy is what let
            // Settings draw at 76x24 while this claimed 72x26. These arms are
            // unreachable: `handle_mouse_zones` answers for them instead.
            Overlay::None
            | Overlay::Help(_)
            | Overlay::About
            | Overlay::Log(_)
            | Overlay::Modal(_)
            | Overlay::Export(_)
            | Overlay::Theme(_) => area,
            Overlay::Welcome(_) => centered_modal(76, 24, area),
            // One size for every wizard step (the modal must not jump around as
            // the user advances); mirrors render_import.
            Overlay::Import(_) => centered_modal(78, 24, area),
            Overlay::ImageSource(_) => centered_modal(78, 24, area),
            // Must mirror render_settings' centered_modal(72, 26, …) so clicks
            // near the modal's top/bottom hit-test inside it (not as a dismiss).
            Overlay::Settings(_) => centered_modal(72, 26, area),
            Overlay::Palette(_) => centered_modal(60, 20, area),
            Overlay::Synopsis(_) => centered_modal(76, 24, area),
            Overlay::ProjectTitle(_) => centered_modal(72, 16, area),
            Overlay::Qa(_) => centered_pct(80, 80, area),
            Overlay::ReaderNote(_) => centered_modal(72, 14, area),
            Overlay::ReaderInspect(_) => centered_pct(82, 80, area),
            Overlay::ReaderEdit(_) => centered_pct(82, 75, area),
            Overlay::ReaderSearch(_) => centered_modal(64, 7, area),
            Overlay::ReaderJump(_) => centered_modal(72, 24, area),
        }
    }

    /// Map a click inside the modal interior to an outcome. Row selection is set
    /// here (within the borrow); the actual activation key is synthesized by the
    /// caller after the borrow ends. `inner` is the bordered modal's content rect.
    fn resolve_click(&mut self, m: MouseInput, inner: Rect, double: bool) -> ClickOutcome {
        // Kit-rendered overlays are absent here on purpose: they answer clicks
        // from the zone registry, so re-deriving their row offsets would be a
        // second source of truth. The catch-all at the bottom covers them.
        match self {
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
            Overlay::ImageSource(st) => {
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
                KeyCode::Left | KeyCode::Char('h') if !st.lock_name => {
                    st.target_language = st.target_language.cycled();
                    Action::None
                }
                KeyCode::Right | KeyCode::Char('l') if !st.lock_name => {
                    st.target_language = st.target_language.cycled();
                    Action::None
                }
                KeyCode::Enter => {
                    if st.files.is_empty() {
                        Action::CloseOverlay
                    } else if let Some(vol) = st.append_to {
                        // Append mode skips every wizard step: import the picked
                        // file straight into the target volume.
                        let source = st.selected_file().cloned().unwrap_or_default();
                        let title = st.name.trim().to_string();
                        st.step = 5;
                        st.progress = Some((0, 0, "starting".to_string()));
                        Action::ImportFile {
                            source,
                            title,
                            translated_title: String::new(),
                            vol,
                            synopsis_raw: String::new(),
                            translated_synopsis: String::new(),
                            target_language: st.effective_target_language(),
                            append: true,
                        }
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
                            st.step = 3;
                        } else {
                            st.step = 1;
                        }
                        Action::None
                    }
                }
                _ => Action::None,
            },
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
                            st.note = Some("a project name is required");
                        } else {
                            st.note = None;
                            // A changed name invalidates any earlier translation.
                            let raw = st.name.trim().to_string();
                            if st.title_syn.raw != raw {
                                st.title_syn = SynopsisState::new_title(raw, String::new());
                            }
                            st.step = 2;
                        }
                        Action::None
                    }
                    _ => Action::None,
                }
            }
            2 => {
                // Type the translated title by hand; Tab translates via the agent;
                // Enter (with or without one) continues; Esc returns to the name step.
                match handle_synopsis_keys(&mut st.title_syn, key) {
                    SynKey::None => Action::None,
                    SynKey::Translate => Action::TranslateProjectTitle {
                        raw: st.title_syn.raw.clone(),
                        attempt: st.title_syn.attempt,
                        target_language: st.effective_target_language(),
                    },
                    SynKey::Accept | SynKey::Skip => {
                        st.step = 3;
                        st.suggest_volume();
                        Action::None
                    }
                    SynKey::Back => {
                        st.step = 1;
                        st.name_cursor = st.name.len();
                        Action::None
                    }
                }
            }
            3 => match key.code {
                KeyCode::Esc => {
                    st.step = if st.lock_name { 0 } else { 2 };
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
                    st.step = 4;
                    Action::None
                }
                _ => Action::None,
            },
            4 => {
                let intent = handle_synopsis_keys(&mut st.syn, key);
                match intent {
                    SynKey::None => Action::None,
                    SynKey::Translate => Action::TranslateSynopsis {
                        raw: st.syn.raw.clone(),
                        attempt: st.syn.attempt,
                        target_language: st.effective_target_language(),
                    },
                    SynKey::Back => {
                        st.step = 3;
                        Action::None
                    }
                    SynKey::Accept => {
                        let source = st.selected_file().cloned().unwrap_or_default();
                        let title = st.name.trim().to_string();
                        let translated_title = st.title_syn.translated_text.trim().to_string();
                        let vol = st.vol.max(1);
                        let synopsis_raw = st.syn.raw.trim().to_string();
                        let translated_synopsis = st.syn.translated_text.trim().to_string();
                        st.step = 5;
                        st.progress = Some((0, 0, "starting".to_string()));
                        Action::ImportFile {
                            source,
                            title,
                            translated_title,
                            vol,
                            synopsis_raw,
                            translated_synopsis,
                            target_language: st.effective_target_language(),
                            append: false,
                        }
                    }
                    SynKey::Skip => {
                        let source = st.selected_file().cloned().unwrap_or_default();
                        let title = st.name.trim().to_string();
                        let translated_title = st.title_syn.translated_text.trim().to_string();
                        let vol = st.vol.max(1);
                        st.step = 5;
                        st.progress = Some((0, 0, "starting".to_string()));
                        Action::ImportFile {
                            source,
                            title,
                            translated_title,
                            vol,
                            synopsis_raw: String::new(),
                            translated_synopsis: String::new(),
                            target_language: st.effective_target_language(),
                            append: false,
                        }
                    }
                }
            }
            _ => match key.code {
                KeyCode::Esc => Action::CloseOverlay,
                _ => Action::None,
            },
        }
    }

    fn handle_image_source_key(&mut self, key: KeyEvent) -> Action {
        let Overlay::ImageSource(st) = self else {
            return Action::None;
        };
        match key.code {
            KeyCode::Esc => Action::CloseOverlay,
            KeyCode::Char('r') | KeyCode::Char('R') => Action::RescanImageSources { vol: st.vol },
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
                    Action::RefreshVolumeImagesFromFile {
                        vol: st.vol,
                        source: st.selected_file().cloned().unwrap_or_default(),
                    }
                }
            }
            _ => Action::None,
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
            // Ctrl-G toggles the update channel (stable releases ↔ latest git).
            KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                st.release_channel = st.release_channel.toggled();
                Action::None
            }
            // Ctrl-Y cycles the OpenRouter service tier (Off → Flex → Priority).
            KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                st.service_tier = ServiceTier::cycled(st.service_tier);
                Action::None
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if st.account_login.is_some() {
                    Action::None
                } else {
                    Action::StartRemoteLogin
                }
            }
            KeyCode::Char('x') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Action::ToggleCodexSignIn
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if st.account_login.is_none() {
                    Action::StartRemoteLogin
                } else if st.remote_enabled {
                    Action::DisableRemote
                } else {
                    Action::EnableRemote
                }
            }
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if st.account_login.is_some() {
                    Action::RemoteLogout
                } else {
                    Action::None
                }
            }
            // Ctrl-modified so focused text fields do not swallow sign-in helpers.
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if st.remote_auth_code.is_some() {
                    Action::OpenAuthUrl
                } else {
                    Action::None
                }
            }
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if st.remote_auth_code.is_some() {
                    Action::CopyAuthCode
                } else {
                    Action::None
                }
            }
            KeyCode::Enter => st.save_action(),
            // Tab switches between Settings tabs; Up/Down move fields within a tab.
            KeyCode::Tab => {
                st.switch_tab(true);
                Action::None
            }
            KeyCode::BackTab => {
                st.switch_tab(false);
                Action::None
            }
            KeyCode::Down => {
                st.next_field();
                Action::None
            }
            KeyCode::Up => {
                st.prev_field();
                Action::None
            }
            // Text fields keep Left/Right for caret movement below.
            KeyCode::Left | KeyCode::Right
                if st.tab_has_fields() && !st.current_is_editable_text() =>
            {
                st.cycle(matches!(key.code, KeyCode::Right));
                Action::None
            }
            _ if !st.tab_has_fields() => Action::None,
            _ => {
                let cur = st.current();
                // Secret (key) fields are masked, so they edit at the end only.
                if cur.is_secret() {
                    let env = match cur {
                        SField::OpenRouterKey => st.api_key_env,
                        SField::TokenrouterKey => st.tokenrouter_key_env,
                        SField::GoogleKey => st.google_key_env,
                        SField::CloudflareToken => st.cloudflare_api_token_env,
                        _ => false,
                    };
                    if env {
                        return Action::None; // env key is read-only
                    }
                    match key.code {
                        KeyCode::Backspace => {
                            if let Some(buf) = st.text_field_mut() {
                                buf.pop();
                            }
                        }
                        KeyCode::Char(c)
                            if !key.modifiers.contains(KeyModifiers::CONTROL)
                                && !key.modifiers.contains(KeyModifiers::ALT) =>
                        {
                            if let Some(buf) = st.text_field_mut() {
                                buf.push(c);
                            }
                        }
                        _ => {}
                    }
                    return Action::None;
                }
                if !st.current_is_editable_text() {
                    return Action::None;
                }
                if cur == SField::CloudflareAccount && st.cloudflare_account_id_env {
                    return Action::None;
                }
                let opts = EditOpts {
                    numeric_only: cur.is_numeric(),
                    multiline: false,
                };
                let mut cursor = st.cursor;
                if let Some(buf) = st.text_field_mut() {
                    input::handle(buf, &mut cursor, key, opts);
                }
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
                let target = match self {
                    Overlay::Qa(st) => st
                        .report
                        .issues
                        .get(st.sel)
                        .and_then(|i| i.chapter.map(|ch| (ch, i.kind.clone()))),
                    _ => None,
                };
                match target {
                    // Flagged chunks jump to the review-needed passage.
                    Some((chapter, qa::QaKind::ReviewChunk { chunk })) => {
                        *self = Overlay::None;
                        Action::OpenChapterAtChunk { chapter, chunk }
                    }
                    Some((chapter, _)) => {
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
        if input::handle(&mut st.text, &mut st.cursor, key, EditOpts::default()) != Edited::Ignored
        {
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

    fn handle_reader_inspect_key(&mut self, key: KeyEvent) -> Action {
        let Overlay::ReaderInspect(st) = self else {
            return Action::None;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => Action::CloseOverlay,
            KeyCode::Char('j') | KeyCode::Down => {
                st.scroll = st.scroll.saturating_add(1);
                Action::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                st.scroll = st.scroll.saturating_sub(1);
                Action::None
            }
            // Jump into the editor for this chunk; the App re-seeds it from the raw
            // on-disk Thai (composed), not the display-decomposed popover text.
            KeyCode::Char('e') => {
                let (chapter, chunk) = (st.chapter, st.chunk);
                *self = Overlay::None;
                Action::OpenReaderEdit { chapter, chunk }
            }
            _ => Action::None,
        }
    }

    fn handle_reader_edit_key(&mut self, key: KeyEvent) -> Action {
        let Overlay::ReaderEdit(st) = self else {
            return Action::None;
        };
        let opts = EditOpts {
            numeric_only: false,
            multiline: true,
        };
        if input::handle(&mut st.text, &mut st.cursor, key, opts) != Edited::Ignored {
            return Action::None;
        }
        match key.code {
            KeyCode::Esc => Action::CloseOverlay,
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Action::SaveReaderEdit {
                    chapter: st.chapter,
                    chunk: st.chunk,
                    text: st.text.clone(),
                }
            }
            KeyCode::Enter => {
                input::insert_char(&mut st.text, &mut st.cursor, '\n');
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
                target_language: st.target_language,
            },
            SynKey::Accept => Action::SaveSynopsis {
                raw: st.syn.raw.clone(),
                translated_synopsis: st.syn.translated_text.clone(),
            },
            SynKey::Skip | SynKey::Back => Action::CloseOverlay,
        }
    }

    fn handle_project_title_key(&mut self, key: KeyEvent) -> Action {
        let Overlay::ProjectTitle(st) = self else {
            return Action::None;
        };
        match handle_synopsis_keys(&mut st.syn, key) {
            SynKey::None => Action::None,
            SynKey::Translate => Action::TranslateProjectTitle {
                raw: st.syn.raw.clone(),
                attempt: st.syn.attempt,
                target_language: st.target_language,
            },
            SynKey::Accept => Action::SaveProjectTitle {
                id: st.id.clone(),
                raw: st.syn.raw.clone(),
                translated_title: st.syn.translated_text.clone(),
            },
            SynKey::Skip | SynKey::Back => Action::CloseOverlay,
        }
    }

    pub fn hints(&self) -> &'static [(&'static str, &'static str)] {
        match self {
            Overlay::Welcome(_) => &[("↑↓", "move"), ("↵", "select"), ("Esc", "skip")],
            Overlay::Import(st) => match st.step {
                0 => &[
                    ("↑↓", "pick"),
                    ("←→", "language"),
                    ("↵", "next"),
                    ("r", "rescan"),
                    ("Esc", "cancel"),
                ],
                1 => &[("type", "name"), ("↵/Tab", "next"), ("Esc", "back")],
                2 => import_title_hints(&st.title_syn),
                3 => &[("↑↓/type", "volume"), ("↵", "next"), ("Esc", "back")],
                4 => synopsis_hints(&st.syn, true),
                _ => &[("Esc", "close")],
            },
            Overlay::ImageSource(_) => &[
                ("↑↓", "pick"),
                ("↵", "update"),
                ("r", "rescan"),
                ("Esc", "cancel"),
            ],
            Overlay::Synopsis(st) => synopsis_hints(&st.syn, false),
            Overlay::ProjectTitle(st) => title_hints(&st.syn),
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
            Overlay::ReaderInspect(_) => &[
                ("jk", "scroll"),
                ("e", "edit translation"),
                ("Esc/q", "close"),
            ],
            Overlay::ReaderEdit(_) => &[
                ("type", "edit"),
                ("↵", "newline"),
                ("^S", "save"),
                ("Esc", "cancel"),
            ],
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

    /// Whether this overlay draws through the component kit.
    ///
    /// Migration marker. A kit-rendered overlay registers its own zones, so its
    /// clicks resolve from the registry and it needs no entry in `modal_rect`
    /// or `resolve_click`. Both of those go away once this returns true for
    /// every variant.
    pub fn is_kit_rendered(&self) -> bool {
        matches!(
            self,
            Overlay::None
                | Overlay::Help(_)
                | Overlay::About
                | Overlay::Log(_)
                | Overlay::Modal(_)
                | Overlay::Export(_)
                | Overlay::Theme(_)
        )
    }

    pub fn render(
        &self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        cfg: &AppConfig,
        log: &[(LogLevel, String)],
    ) {
        // Overlays still on the old path draw straight to the frame. They are
        // converted a batch at a time; `is_kit_rendered` says which are done.
        match self {
            Overlay::None => {}
            Overlay::Help(off) => self.render_help_kit(ui, area, *off),
            Overlay::About => self.render_about_kit(ui, area),
            Overlay::Log(off) => self.render_log_kit(ui, area, log, *off),
            Overlay::Modal(dlg) => self.render_modal_kit(ui, area, dlg),
            Overlay::Export(st) => self.render_export_kit(ui, area, st),
            Overlay::Theme(st) => self.render_theme_kit(ui, area, st),

            Overlay::Welcome(st) => {
                let theme = ui.theme;
                self.render_welcome(ui.frame, area, theme, st)
            }
            Overlay::Import(st) => {
                let theme = ui.theme;
                self.render_import(ui.frame, area, theme, st)
            }
            Overlay::ImageSource(st) => {
                let theme = ui.theme;
                self.render_image_source(ui.frame, area, theme, st)
            }
            Overlay::Settings(st) => {
                let theme = ui.theme;
                self.render_settings(ui.frame, area, theme, cfg, st)
            }
            Overlay::Palette(st) => {
                let theme = ui.theme;
                self.render_palette(ui.frame, area, theme, st)
            }
            Overlay::Synopsis(st) => {
                let theme = ui.theme;
                self.render_synopsis(ui.frame, area, theme, st)
            }
            Overlay::ProjectTitle(st) => {
                let theme = ui.theme;
                self.render_project_title(ui.frame, area, theme, st)
            }
            Overlay::Qa(st) => {
                let theme = ui.theme;
                self.render_qa(ui.frame, area, theme, st)
            }
            Overlay::ReaderNote(st) => {
                let theme = ui.theme;
                self.render_reader_note(ui.frame, area, theme, st)
            }
            Overlay::ReaderInspect(st) => {
                let theme = ui.theme;
                self.render_reader_inspect(ui.frame, area, theme, st)
            }
            Overlay::ReaderEdit(st) => {
                let theme = ui.theme;
                self.render_reader_edit(ui.frame, area, theme, st)
            }
            Overlay::ReaderSearch(st) => {
                let theme = ui.theme;
                self.render_reader_search(ui.frame, area, theme, st)
            }
            Overlay::ReaderJump(st) => {
                let theme = ui.theme;
                self.render_reader_jump(ui.frame, area, theme, st)
            }
        }
    }

    // ---- kit-rendered overlays -------------------------------------------
    //
    // Each of these draws through `kit::Modal`, which owns the frame, the
    // backdrop, the close affordance and the focus trap. None of them restate
    // their geometry anywhere: the registry the modal writes to is what clicks
    // resolve against.

    /// Keybinding reference, grouped by where the bindings apply.
    fn render_help_kit(&self, ui: &mut crate::ui::kit::Ui, area: Rect, off: u16) {
        use crate::ui::kit::list::{self, ListState, Row};
        use crate::ui::kit::modal::{Modal, Sizing};

        let frame = Modal::new("Help — keybindings")
            .sizing(Sizing::large())
            .subtitle("jk scroll · Esc close")
            .render(ui, area);

        let rows = help_rows();
        let mut st = ListState::new();
        st.scroll_by(off as isize, frame.body.height, rows.len());
        let dim = Style::default().fg(ui.theme.ink_faint);
        let key = Style::default()
            .fg(ui.theme.ink_soft)
            .add_modifier(Modifier::BOLD);
        let head = Style::default()
            .fg(ui.theme.accent)
            .add_modifier(Modifier::BOLD);

        list::render(
            ui,
            frame.body,
            &mut st,
            rows.len(),
            list::Opts {
                rail: false,
                scrollbar: true,
                kind: ZoneKind::Row,
                id_base: 0,
            },
            |i| match &rows[i] {
                HelpRow::Section(title) => {
                    Row::header(Line::from(Span::styled(title.to_string(), head)))
                }
                HelpRow::Blank => Row::header(Line::raw("")),
                HelpRow::Binding(k, what) => Row::header(Line::from(vec![
                    Span::styled(format!("  {k:<18}"), key),
                    Span::styled(what.to_string(), dim),
                ])),
            },
        );
    }

    /// About card. Animated off the frame ticker: the waxing-moon status
    /// metaphor cycles, the three agents pulse left to right, and a line of
    /// Japanese is typed out in Thai one grapheme at a time — typing by
    /// grapheme rather than by char so a Thai cluster is never split mid-mark.
    fn render_about_kit(&self, ui: &mut crate::ui::kit::Ui, area: Rect) {
        use crate::ui::kit::ctx::row_at;
        use crate::ui::kit::modal::{Modal, Sizing};
        use unicode_segmentation::UnicodeSegmentation;

        let frame = ui.frame_count;
        let title = thai_display_safe("About · เกี่ยวกับ");
        let f = Modal::new(&title)
            .sizing(Sizing::small())
            .subtitle(env!("CARGO_PKG_VERSION"))
            .render(ui, area);

        let bg = ui.theme.bg_elevated;
        let dim = Style::default().fg(ui.theme.ink_faint).bg(bg);
        let soft = Style::default().fg(ui.theme.ink_soft).bg(bg);
        let accent = Style::default()
            .fg(ui.theme.accent)
            .bg(bg)
            .add_modifier(Modifier::BOLD);

        const PHASES: [crate::ui::glyphs::Glyph; 5] = [
            crate::ui::glyphs::MOON_NEW,
            crate::ui::glyphs::MOON_CRESCENT,
            crate::ui::glyphs::MOON_FIRST_QUARTER,
            crate::ui::glyphs::MOON_LAST_QUARTER,
            crate::ui::glyphs::MOON_FULL,
        ];
        let moon = PHASES[(frame / 4) as usize % PHASES.len()];

        let mut row = 0u16;
        let mut put = |ui: &mut crate::ui::kit::Ui, line: Line<'static>| {
            let r = row_at(f.body, row);
            if r.height > 0 {
                ui.line(r, line, Style::default().bg(bg));
            }
            row += 1;
        };

        put(
            ui,
            Line::from(vec![
                Span::styled(format!("{} ", moon.as_str()), accent),
                Span::styled("honya 本屋", accent),
            ]),
        );
        put(
            ui,
            Line::from(Span::styled(
                "Japanese → Thai / English light-novel translation.",
                soft,
            )),
        );
        put(ui, Line::raw(""));

        // The typing demo: hold the finished line a moment, then start over.
        let jp = "「月が綺麗ですね。」";
        let th_full = thai_display_safe("— พระจันทร์คืนนี้สวยเหลือเกินนะ");
        let graphemes: Vec<&str> = th_full.graphemes(true).collect();
        const HOLD: usize = 22;
        let pos = (frame as usize) % (graphemes.len() + HOLD);
        let shown = pos.min(graphemes.len());
        let typed: String = graphemes[..shown].concat();
        let caret_on = shown < graphemes.len() || frame % 10 < 5;

        put(ui, Line::from(Span::styled(jp, soft)));
        put(
            ui,
            Line::from(vec![
                Span::styled(typed, Style::default().fg(ui.theme.translated_text).bg(bg)),
                Span::styled(
                    if caret_on {
                        crate::ui::glyphs::ACCENT_RAIL.as_str()
                    } else {
                        " "
                    },
                    Style::default().fg(ui.theme.stream_cursor).bg(bg),
                ),
            ]),
        );
        put(ui, Line::raw(""));

        // The three agents, pulsing left to right.
        let active = ((frame / 6) % 3) as usize;
        let agents = [
            ("Orchestrator", ui.theme.accent),
            ("Translator", ui.theme.status_working),
            ("Reviewer", ui.theme.accent_soft),
        ];
        let spinner = crate::ui::glyphs::frame_of(&crate::ui::glyphs::SPINNER, frame);
        let mut pipeline: Vec<Span<'static>> = Vec::new();
        for (i, (name, color)) in agents.into_iter().enumerate() {
            if i > 0 {
                pipeline.push(Span::styled(
                    format!(" {} ", crate::ui::glyphs::RULE_H.as_str()),
                    dim,
                ));
            }
            let (mark, style) = if i == active {
                (
                    spinner.as_str().to_string(),
                    Style::default().fg(color).bg(bg).add_modifier(Modifier::BOLD),
                )
            } else {
                (crate::ui::glyphs::BADGE_ORCHESTRATOR.as_str().to_string(), dim)
            };
            pipeline.push(Span::styled(format!("{mark} {name}"), style));
        }
        put(ui, Line::from(pipeline));
        put(ui, Line::raw(""));

        let commit = option_env!("HONYA_BUILD_COMMIT").unwrap_or("dev");
        put(
            ui,
            Line::from(vec![
                Span::styled(format!("build {commit}   "), dim),
                Span::styled(env!("CARGO_PKG_HOMEPAGE"), dim),
            ]),
        );
    }

    /// The activity log, newest last, scrolled back by `off`.
    fn render_log_kit(
        &self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        log: &[(LogLevel, String)],
        off: u16,
    ) {
        use crate::ui::kit::list::{self, ListState, Row};
        use crate::ui::kit::modal::{Modal, Sizing};

        let frame = Modal::new("Activity log")
            .sizing(Sizing::large())
            .subtitle(format!("{} entries", log.len()))
            .render(ui, area);

        // `off` counts backwards from the newest entry, which is where the log
        // sits when opened.
        let mut st = ListState::following();
        if off > 0 {
            st.set_follow(false);
            st.scroll_by(
                -(off as isize),
                frame.body.height,
                log.len(),
            );
        }
        let colors = [
            ui.theme.ink_faint,
            ui.theme.ink_soft,
            ui.theme.status_warn,
            ui.theme.status_failed,
        ];
        list::render(
            ui,
            frame.body,
            &mut st,
            log.len(),
            list::Opts {
                rail: false,
                scrollbar: true,
                kind: ZoneKind::Row,
                id_base: 0,
            },
            |i| {
                let (level, msg) = &log[i];
                let (glyph, color) = match level {
                    LogLevel::Trace => (crate::ui::glyphs::DOT, colors[0]),
                    LogLevel::Info => (crate::ui::glyphs::CHECK, colors[1]),
                    LogLevel::Warn => (crate::ui::glyphs::FLAG, colors[2]),
                    LogLevel::Error => (crate::ui::glyphs::CROSS, colors[3]),
                };
                Row::header(Line::from(vec![
                    Span::styled(
                        format!("{} ", glyph.as_str()),
                        Style::default().fg(color),
                    ),
                    Span::styled(
                        thai_display_safe(msg),
                        Style::default().fg(colors[1]),
                    ),
                ]))
            },
        );
    }

    /// A confirm dialog, with its choices as real buttons.
    fn render_modal_kit(&self, ui: &mut crate::ui::kit::Ui, area: Rect, dlg: &Dialog) {
        use crate::ui::kit::button::{Button, ButtonRow};
        use crate::ui::kit::modal::{self, Modal, Sizing};

        let frame = Modal::new(&dlg.title)
            .sizing(Sizing::small())
            .footer(1)
            .closable(false)
            .render(ui, area);

        let wrapped = crate::ui::kit::editor::wrap(&dlg.body, frame.body.width);
        for (n, range) in wrapped.iter().enumerate() {
            if (n as u16) >= frame.body.height.saturating_sub(1) {
                break;
            }
            ui.text(
                crate::ui::kit::ctx::row_at(frame.body, n as u16),
                thai_display_safe(&dlg.body[range.clone()]),
                Style::default()
                    .fg(ui.theme.ink_soft)
                    .bg(ui.theme.bg_elevated),
            );
        }

        let mut buttons = vec![Button::new(ZoneId::button(DIALOG_CANCEL), "Cancel").accel("esc")];
        if let Some(alt) = &dlg.alternate {
            buttons.push(
                Button::new(ZoneId::button(DIALOG_ALTERNATE), alt.label.clone())
                    .accel(alt.key.to_string()),
            );
        }
        buttons.push(
            Button::new(ZoneId::button(DIALOG_CONFIRM), dlg.confirm_label.clone())
                .accel("↵")
                .primary(),
        );
        modal::render_footer(ui, frame.footer, ButtonRow::new(buttons));
    }

    /// Export picker: a format checklist, then progress, then results.
    fn render_export_kit(&self, ui: &mut crate::ui::kit::Ui, area: Rect, st: &ExportState) {
        use crate::ui::kit::button::{Button, ButtonRow};
        use crate::ui::kit::modal::{self, Modal, Sizing};
        use crate::ui::kit::{badge::Chip, progress};

        let frame = Modal::new("Export volume")
            .sizing(Sizing::medium())
            .subtitle(format!("Vol.{:02}", st.vol))
            .footer(1)
            .render(ui, area);

        if let Some((written, warnings)) = &st.done {
            let mut n = 0u16;
            ui.text(
                crate::ui::kit::ctx::row_at(frame.body, n),
                format!("Wrote {} file(s)", written.len()),
                Style::default()
                    .fg(ui.theme.status_done)
                    .bg(ui.theme.bg_elevated)
                    .add_modifier(Modifier::BOLD),
            );
            n += 1;
            for path in written.iter().take(frame.body.height.saturating_sub(2) as usize) {
                ui.text(
                    crate::ui::kit::ctx::row_at(frame.body, n),
                    truncate_cols(&path.display().to_string(), frame.body.width as usize),
                    Style::default()
                        .fg(ui.theme.ink_soft)
                        .bg(ui.theme.bg_elevated),
                );
                n += 1;
            }
            for w in warnings.iter().take(2) {
                ui.text(
                    crate::ui::kit::ctx::row_at(frame.body, n),
                    truncate_cols(w, frame.body.width as usize),
                    Style::default()
                        .fg(ui.theme.status_warn)
                        .bg(ui.theme.bg_elevated),
                );
                n += 1;
            }
            modal::render_footer(
                ui,
                frame.footer,
                ButtonRow::new(vec![
                    Button::new(ZoneId::button(DIALOG_CONFIRM), "Close")
                        .accel("↵")
                        .primary(),
                ]),
            );
            return;
        }

        if let Some((done, total, what)) = &st.progress {
            ui.text(
                crate::ui::kit::ctx::row_at(frame.body, 0),
                format!("Exporting {what}…"),
                Style::default()
                    .fg(ui.theme.ink_soft)
                    .bg(ui.theme.bg_elevated),
            );
            progress::bar_with_label(
                ui,
                crate::ui::kit::ctx::row_at(frame.body, 2),
                *done,
                *total,
            );
            modal::footer_hint(ui, frame.footer, "  export continues in the background");
            return;
        }

        for (i, fmt) in crate::export::ExportFormat::ALL.iter().enumerate() {
            let row = crate::ui::kit::ctx::row_at(frame.body, i as u16);
            if row.height == 0 {
                break;
            }
            Chip::new(ZoneId::row(i), fmt.label(), st.formats[i]).render(ui, row);
            let desc_x = row.x + 18;
            if desc_x < row.x + row.width {
                ui.text(
                    Rect {
                        x: desc_x,
                        width: row.width - 18,
                        ..row
                    },
                    export_desc(*fmt),
                    Style::default()
                        .fg(ui.theme.ink_faint)
                        .bg(ui.theme.bg_elevated),
                );
            }
        }

        let any = st.formats.iter().any(|b| *b);
        modal::render_footer(
            ui,
            frame.footer,
            ButtonRow::new(vec![
                Button::new(ZoneId::button(DIALOG_CANCEL), "Cancel").accel("esc"),
                Button::new(ZoneId::button(DIALOG_CONFIRM), "Export")
                    .accel("↵")
                    .primary()
                    .disabled(!any),
            ]),
        );
    }

    /// Theme picker. The selection previews live, so the list doubles as the
    /// preview surface and needs no separate swatch.
    fn render_theme_kit(&self, ui: &mut crate::ui::kit::Ui, area: Rect, st: &ThemePickerState) {
        use crate::ui::kit::list::{self, ListState, Row};
        use crate::ui::kit::modal::{self, Modal, Sizing};

        let frame = Modal::new("Theme")
            .sizing(Sizing::medium())
            .subtitle("↵ keep · Esc revert")
            .footer(1)
            .render(ui, area);

        let mut list_state = ListState::new();
        list_state.select(Some(st.sel));
        let dim = Style::default().fg(ui.theme.ink_faint);
        list::render(
            ui,
            frame.body,
            &mut list_state,
            ALL_THEMES.len(),
            list::Opts {
                rail: true,
                scrollbar: true,
                kind: ZoneKind::Row,
                id_base: 0,
            },
            |i| {
                let id = ALL_THEMES[i];
                Row::new(Line::from(vec![
                    Span::raw(format!("{:<22}", id.label())),
                    Span::styled(id.tone().to_string(), dim),
                ]))
            },
        );
        modal::footer_hint(
            ui,
            frame.footer,
            "  the selection previews live — Esc puts the old one back",
        );
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
                "  AI-assisted Japanese → Thai / English light-novel translation.",
                soft,
            )),
            Line::raw(""),
            Line::from(Span::styled("  The five screens (1–5 / Tab):", dim)),
        ];
        let screens = [
            ("1", "書架 Shelf", "import files · pick a project"),
            ("2", "棚 Project", "chapters · queue · run translation"),
            ("3", "訳 Translate", "watch the live 3-agent pipeline"),
            ("4", "読 Reader", "read source ↔ translation side by side"),
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
            2 => self.render_import_title(f, rows[3], theme, st),
            3 => self.render_import_volume(f, rows[3], theme, st),
            4 => render_synopsis_body(f, rows[3], theme, &st.syn, "start import"),
            _ => self.render_import_progress(f, rows[3], theme, st),
        }
    }

    fn render_image_source(&self, f: &mut Frame, area: Rect, theme: &Theme, st: &ImageSourceState) {
        let modal = centered_modal(78, 24, area);
        f.render_widget(Clear, modal);
        let title = format!("Update images — Vol.{:02}", st.vol);
        let block = self.modal_block(&title, theme);
        let inner = block.inner(modal);
        f.render_widget(block, modal);

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(inner);

        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    "  Source EPUB missing from VOLUME.md",
                    Style::default().fg(theme.status_warn),
                ),
                Span::styled(
                    "  choose the volume's original file",
                    Style::default().fg(theme.ink_faint),
                ),
            ]))
            .style(Style::default().bg(theme.bg_panel)),
            rows[0],
        );

        if st.files.is_empty() {
            let p = Paragraph::new(vec![
                Line::raw(""),
                Line::from(Span::styled(
                    "  No EPUB files found in this folder.",
                    Style::default().fg(theme.ink_soft),
                )),
                Line::raw(""),
                Line::from(Span::styled(
                    "  Drop the source EPUB into this folder, then press r to rescan.",
                    Style::default().fg(theme.ink_faint),
                )),
            ])
            .style(Style::default().bg(theme.bg_panel));
            f.render_widget(p, rows[3]);
            return;
        }

        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    "  Choose a source EPUB",
                    Style::default().fg(theme.ink_soft),
                ),
                Span::styled(
                    format!("  ({} found · r rescan)", st.files.len()),
                    Style::default().fg(theme.ink_faint),
                ),
            ]))
            .style(Style::default().bg(theme.bg_panel)),
            rows[2],
        );

        let cap = rows[3].height.max(1);
        let start = windowed_start(st.sel, cap);
        let end = (start + cap as usize).min(st.files.len());
        let size_w = 9usize;
        let name_w = (rows[3].width as usize).saturating_sub(6 + size_w);

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
            rows[3],
        );
    }

    /// Standalone synopsis editor modal (re-opened from the Project screen).
    fn render_synopsis(&self, f: &mut Frame, area: Rect, theme: &Theme, st: &SynopsisEditState) {
        let modal = centered_modal(76, 24, area);
        f.render_widget(Clear, modal);
        let title = thai_display_safe(&format!(
            "Synopsis — Vol.{:02} · {}",
            st.vol,
            truncate_cols(st.title.trim(), 40)
        ));
        let block = self.modal_block(&title, theme);
        let inner = block.inner(modal);
        f.render_widget(block, modal);
        render_synopsis_body(f, inner, theme, &st.syn, "save");
    }

    fn render_project_title(&self, f: &mut Frame, area: Rect, theme: &Theme, st: &TitleEditState) {
        let modal = centered_modal(72, 16, area);
        f.render_widget(Clear, modal);
        let title = thai_display_safe(&format!("Title — {}", truncate_cols(&st.id, 40)));
        let block = self.modal_block(&title, theme);
        let inner = block.inner(modal);
        f.render_widget(block, modal);
        render_editor_body(
            f,
            inner,
            theme,
            &st.syn,
            "save",
            &EditorLabels {
                label: "  Title · source  (translate with the Translator agent)",
                placeholder: "Type the source title…",
                input_rows: 3,
            },
        );
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
            let (before, after) = input::caret_halves(
                &st.text,
                st.cursor,
                rows[2].width.saturating_sub(6) as usize,
            );
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

    fn render_reader_inspect(
        &self,
        f: &mut Frame,
        area: Rect,
        theme: &Theme,
        st: &ReaderInspectState,
    ) {
        let modal = centered_pct(82, 80, area);
        f.render_widget(Clear, modal);
        let title = format!("Inspect · ch {:03} · chunk {}", st.chapter, st.chunk + 1);
        let block = self.modal_block(&title, theme);
        let inner = block.inner(modal);
        f.render_widget(block, modal);

        let head = |s: &str, c: ratatui::style::Color| {
            Line::from(Span::styled(
                s.to_string(),
                Style::default().fg(c).add_modifier(Modifier::BOLD),
            ))
        };
        let mut lines: Vec<Line> = Vec::new();
        lines.push(head("ญี่ปุ่น · source", theme.accent));
        for l in st.source_jp.lines() {
            lines.push(Line::from(Span::styled(
                l.to_string(),
                Style::default().fg(theme.ja_text),
            )));
        }
        lines.push(Line::raw(""));
        lines.push(head("Translation", theme.accent));
        for l in st.translated_text.lines() {
            lines.push(Line::from(Span::styled(
                l.to_string(),
                Style::default().fg(theme.translated_text),
            )));
        }
        if let Some(r) = &st.review {
            lines.push(Line::raw(""));
            lines.push(head("ผู้ตรวจ · reviewer", theme.status_warn));
            let note = if r.trim().is_empty() {
                "flagged for review (no reason recorded)"
            } else {
                r.as_str()
            };
            lines.push(Line::from(Span::styled(
                note.to_string(),
                Style::default().fg(theme.ink_soft),
            )));
        }

        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((st.scroll, 0))
                .style(Style::default().bg(theme.bg_panel)),
            inner,
        );
    }

    fn render_reader_edit(&self, f: &mut Frame, area: Rect, theme: &Theme, st: &ReaderEditState) {
        let modal = centered_pct(82, 75, area);
        f.render_widget(Clear, modal);
        let title = format!(
            "Edit translation · ch {:03} · chunk {}",
            st.chapter,
            st.chunk + 1
        );
        let block = self.modal_block(&title, theme);
        let inner = block.inner(modal);
        f.render_widget(block, modal);

        // Keep the buffer composed; decompose only the rendered preview.
        let (before, after) = st.text.split_at(st.cursor.min(st.text.len()));
        let mut body = String::with_capacity(st.text.len() + 1);
        body.push_str(before);
        body.push('▏');
        body.push_str(after);
        let body = crate::ui::text::thai_display_safe(&body);

        let mut lines: Vec<Line> = body
            .lines()
            .map(|l| {
                Line::from(Span::styled(
                    l.to_string(),
                    Style::default().fg(theme.translated_text),
                ))
            })
            .collect();
        if body.ends_with('\n') {
            lines.push(Line::raw(""));
        }
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "^S save · Enter newline · Esc cancel — saving clears this chunk's review flag",
            Style::default().fg(theme.ink_faint),
        )));

        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .style(Style::default().bg(theme.bg_inset)),
            inner,
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
        let block = self.modal_block("Search (source + translation)", theme);
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
                    "聖剣 · a character · a phrase to locate…",
                    Style::default().fg(theme.ink_faint),
                ),
                Span::styled("▏", Style::default().fg(theme.stream_cursor)),
            ])
        } else {
            let (before, after) = input::caret_halves(
                &st.query,
                st.cursor,
                rows[1].width.saturating_sub(6) as usize,
            );
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

        let (before, after) = input::caret_halves(
            &st.query,
            st.cursor,
            rows[0].width.saturating_sub(5) as usize,
        );
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
                Constraint::Length(1), // language selector
                Constraint::Length(1), // gap
                Constraint::Min(0),    // windowed file list
            ])
            .split(area);
        let header = match st.append_to {
            Some(vol) => format!("  Add chapters to Vol.{vol:02} — choose a source file"),
            None => "  Choose a source file".to_string(),
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(header, Style::default().fg(theme.ink_soft)),
                Span::styled(
                    format!("  ({} found · r rescan)", st.files.len()),
                    Style::default().fg(theme.ink_faint),
                ),
            ]))
            .style(Style::default().bg(theme.bg_panel)),
            rows[0],
        );

        let language_locked = st.lock_name;
        let language = if language_locked {
            st.effective_target_language()
        } else {
            st.target_language
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    "  Translation language  ",
                    Style::default().fg(theme.ink_faint),
                ),
                Span::styled(
                    if language_locked { "  " } else { "◂ " },
                    Style::default().fg(theme.accent_soft),
                ),
                Span::styled(
                    language.label(),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    if language_locked {
                        "  (fixed for this project)"
                    } else {
                        " ▸  (from Settings preference)"
                    },
                    Style::default().fg(theme.ink_faint),
                ),
            ]))
            .style(Style::default().bg(theme.bg_panel)),
            rows[1],
        );

        // Window the rows so the selection stays visible with long file lists.
        let cap = rows[3].height.max(1);
        let start = windowed_start(st.sel, cap);
        let end = (start + cap as usize).min(st.files.len());
        let size_w = 9usize;
        let name_w = (rows[3].width as usize).saturating_sub(6 + size_w);

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
            rows[3],
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
            feedback.push(Line::from(Span::styled(
                format!("    project language: {}", target.target_language.label()),
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

    fn render_import_title(&self, f: &mut Frame, area: Rect, theme: &Theme, st: &ImportState) {
        let syn = &st.title_syn;
        let faint = Style::default().fg(theme.ink_faint);
        let mut lines = vec![
            Line::from(vec![
                Span::styled(
                    format!(
                        "  Translated title ({})",
                        st.effective_target_language().label()
                    ),
                    Style::default().fg(theme.ink_soft),
                ),
                Span::styled("   ◦ optional", faint),
            ]),
            Line::from(Span::styled(
                "  Shown on the Shelf and in exports — or add it later from the Project screen.",
                faint,
            )),
            Line::raw(""),
            Line::from(vec![
                Span::styled("  Original  ", faint),
                Span::styled(
                    thai_display_safe(syn.raw.trim()),
                    Style::default().fg(theme.ink),
                ),
            ]),
            Line::raw(""),
            {
                let mut spans = vec![Span::styled("  Translation", faint)];
                if syn.edit_translation {
                    if syn.translated_text.is_empty() {
                        spans.push(Span::styled("Type translated title…", faint));
                        spans.push(Span::styled("▏", Style::default().fg(theme.stream_cursor)));
                    } else {
                        let (before, after) =
                            input::caret_halves(&syn.translated_text, syn.translated_cursor, 48);
                        spans.push(Span::styled(
                            thai_display_safe(&before),
                            Style::default()
                                .fg(theme.accent)
                                .add_modifier(Modifier::BOLD),
                        ));
                        spans.push(Span::styled("▏", Style::default().fg(theme.stream_cursor)));
                        spans.push(Span::styled(
                            thai_display_safe(&after),
                            Style::default()
                                .fg(theme.accent)
                                .add_modifier(Modifier::BOLD),
                        ));
                    }
                } else if syn.translated_text.trim().is_empty() {
                    spans.push(Span::styled(
                        thai_display_safe("(no translated title yet.)"),
                        faint,
                    ));
                } else {
                    spans.push(Span::styled(
                        thai_display_safe(syn.translated_text.trim()),
                        Style::default()
                            .fg(theme.accent)
                            .add_modifier(Modifier::BOLD),
                    ));
                }
                Line::from(spans)
            },
            Line::raw(""),
        ];
        match syn.phase {
            SynPhase::Translating => lines.push(Line::from(Span::styled(
                thai_display_safe("  ◐ Translator agent is working … (Esc to cancel)"),
                Style::default().fg(theme.status_working),
            ))),
            SynPhase::Failed => {
                lines.push(Line::from(Span::styled(
                    thai_display_safe(
                        "  ✗ failed — type a translated title, Tab to retry · Esc cancel",
                    ),
                    Style::default().fg(theme.status_failed),
                )));
                lines.push(Line::from(Span::styled(
                    thai_display_safe(&format!("    {}", syn.error)),
                    Style::default().fg(theme.status_failed),
                )));
            }
            // Translation editing is active for both Editing and Done in the title flow.
            _ => {
                let msg = if syn.translated_text.trim().is_empty() {
                    "  ↵ skip · type a translated title · Tab to translate it for you"
                } else {
                    "  ↵ next · type to edit · Tab to retranslate"
                };
                lines.push(Line::from(Span::styled(thai_display_safe(msg), faint)));
            }
        }
        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .style(Style::default().bg(theme.bg_panel)),
            area,
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

    fn render_settings(
        &self,
        f: &mut Frame,
        area: Rect,
        theme: &Theme,
        cfg: &AppConfig,
        st: &SettingsState,
    ) {
        let modal = centered_modal(76, 24, area);
        f.render_widget(Clear, modal);
        let block = self.modal_block("Settings", theme);
        let inner = block.inner(modal);
        f.render_widget(block, modal);

        let val_w = area.width.saturating_sub(26) as usize;
        // None renders the caret at the end for masked API-key fields.
        let field_line =
            |label: &str, value: String, focused: bool, caret: Option<usize>| -> Line<'static> {
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

        // Track the focused row for scroll positioning.
        let row = |idx: u8, label: &str, value: String, text: bool| -> Line<'static> {
            field_line(label, value, st.field == idx, text.then_some(st.cursor))
        };
        let mask = |val: &str, env: bool| -> String {
            if env {
                "● via environment (read-only)".to_string()
            } else if val.trim().is_empty() {
                "— not set —".to_string()
            } else {
                mask_secret(val)
            }
        };
        let plain_setting = |val: &str, env: bool| -> String {
            if env {
                "via environment (read-only)".to_string()
            } else if val.trim().is_empty() {
                "— not set —".to_string()
            } else {
                val.to_string()
            }
        };

        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut focus_line = 0usize;
        let push = |lines: &mut Vec<Line<'static>>,
                    focus_line: &mut usize,
                    line: Line<'static>,
                    focused: bool| {
            if focused {
                *focus_line = lines.len();
            }
            lines.push(line);
        };

        let mut tab_spans: Vec<Span<'static>> = vec![Span::raw("  ")];
        for (i, t) in SettingsTab::ALL.iter().enumerate() {
            if i > 0 {
                tab_spans.push(Span::styled(" · ", Style::default().fg(theme.ink_faint)));
            }
            let style = if *t == st.tab {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                Style::default().fg(theme.ink_soft)
            };
            tab_spans.push(Span::styled(t.title(), style));
        }
        lines.push(Line::from(tab_spans));
        lines.push(Line::from(Span::styled(
            "  Tab switch tab · ↑↓ field · ←→ change · type to edit · ↵ save",
            Style::default().fg(theme.ink_faint),
        )));
        lines.push(Line::raw(""));

        if st.tab == SettingsTab::Agents {
            for (name, base, agent) in [
                ("Orchestrator", 0u8, &st.models.orchestrator),
                ("Translator", 3, &st.models.translator),
                ("Reviewer", 6, &st.models.reviewer),
                ("Refine", 9, &st.models.refine),
            ] {
                push(
                    &mut lines,
                    &mut focus_line,
                    row(base, name, agent.provider.label().to_string(), false),
                    st.field == base,
                );
                push(
                    &mut lines,
                    &mut focus_line,
                    row(base + 1, "  model", agent.model.clone(), true),
                    st.field == base + 1,
                );
                push(
                    &mut lines,
                    &mut focus_line,
                    row(
                        base + 2,
                        "  effort",
                        crate::model::Effort::label(agent.effort).to_string(),
                        false,
                    ),
                    st.field == base + 2,
                );
            }
        }
        if st.tab == SettingsTab::Providers {
            push(
                &mut lines,
                &mut focus_line,
                row(
                    12,
                    "OpenRouter key",
                    mask(&st.openrouter_key, st.api_key_env),
                    false,
                ),
                st.field == 12,
            );
            push(
                &mut lines,
                &mut focus_line,
                row(
                    13,
                    "Tokenrouter key",
                    mask(&st.tokenrouter_key, st.tokenrouter_key_env),
                    false,
                ),
                st.field == 13,
            );
            push(
                &mut lines,
                &mut focus_line,
                row(
                    14,
                    "Google key",
                    mask(&st.google_key, st.google_key_env),
                    false,
                ),
                st.field == 14,
            );
            push(
                &mut lines,
                &mut focus_line,
                row(
                    15,
                    "Cloudflare account",
                    plain_setting(&st.cloudflare_account_id, st.cloudflare_account_id_env),
                    !st.cloudflare_account_id_env,
                ),
                st.field == 15,
            );
            push(
                &mut lines,
                &mut focus_line,
                row(
                    16,
                    "Cloudflare token",
                    mask(&st.cloudflare_api_token, st.cloudflare_api_token_env),
                    false,
                ),
                st.field == 16,
            );
            let (codex_status, codex_color, codex_hint) = match &cfg.codex_auth {
                Some(_) => ("signed in", theme.status_done, "Ctrl-X sign out"),
                None => ("not signed in", theme.ink_soft, "Ctrl-X sign in"),
            };
            lines.push(Line::from(vec![
                Span::styled(
                    "   Codex (ChatGPT)     ",
                    Style::default().fg(theme.ink_faint),
                ),
                Span::styled(codex_status, Style::default().fg(codex_color)),
                Span::styled(
                    format!("   {codex_hint}"),
                    Style::default().fg(theme.ink_faint),
                ),
            ]));
        }
        if st.tab == SettingsTab::Pipeline {
            push(
                &mut lines,
                &mut focus_line,
                row(
                    17,
                    "Preferred language",
                    st.preferred_language.label().to_string(),
                    false,
                ),
                st.field == 17,
            );
            lines.push(Line::from(Span::styled(
                "      ↳ Default for new projects; existing projects keep their language",
                Style::default().fg(theme.ink_faint),
            )));
            push(
                &mut lines,
                &mut focus_line,
                row(18, "Retry attempts", st.max_attempts.clone(), true),
                st.field == 18,
            );
            lines.push(Line::from(Span::styled(
                "      ↳ Translator↔Reviewer loop per chunk (1–20)",
                Style::default().fg(theme.ink_faint),
            )));
            push(
                &mut lines,
                &mut focus_line,
                row(
                    19,
                    "Continuity sentences",
                    st.continuity_sentences.clone(),
                    true,
                ),
                st.field == 19,
            );
            lines.push(Line::from(Span::styled(
                "      ↳ Prior translated sentences per chunk (0–100; 0 disables; 2,000-char cap)",
                Style::default().fg(theme.ink_faint),
            )));
            push(
                &mut lines,
                &mut focus_line,
                row(20, "Loop watchdog (s)", st.loop_stall_secs.clone(), true),
                st.field == 20,
            );
            lines.push(Line::from(Span::styled(
                "      ↳ quiet pipeline stalls after N s; active model calls retry chunk first",
                Style::default().fg(theme.ink_faint),
            )));
            push(
                &mut lines,
                &mut focus_line,
                row(
                    21,
                    "Loop re-translates",
                    st.max_chapter_retranslates.clone(),
                    true,
                ),
                st.field == 21,
            );
            lines.push(Line::from(Span::styled(
                "      ↳ stalled-chapter re-translates before the run aborts (0–10)",
                Style::default().fg(theme.ink_faint),
            )));
            push(
                &mut lines,
                &mut focus_line,
                row(
                    22,
                    "Service tier",
                    ServiceTier::label(st.service_tier).to_string(),
                    false,
                ),
                st.field == 22,
            );
            lines.push(Line::from(Span::styled(
                format!("      ↳ {}", ServiceTier::desc(st.service_tier)),
                Style::default().fg(theme.ink_faint),
            )));
            push(
                &mut lines,
                &mut focus_line,
                row(
                    23,
                    "Parallel lookahead",
                    if st.parallel_lookahead { "On" } else { "Off" }.to_string(),
                    false,
                ),
                st.field == 23,
            );
            lines.push(Line::from(Span::styled(
                "      ↳ Faster between chunks; invalidated drafts may increase API cost",
                Style::default().fg(theme.ink_faint),
            )));
            push(
                &mut lines,
                &mut focus_line,
                row(
                    24,
                    "System One (Jev)",
                    if st.system_one.enabled { "On" } else { "Off" }.to_string(),
                    false,
                ),
                st.field == 24,
            );
            lines.push(Line::from(Span::styled(
                "      ↳ Master switch. Off means no typed-judgement call is made,                  whatever the per-feature toggles below say",
                Style::default().fg(theme.ink_faint),
            )));
            push(
                &mut lines,
                &mut focus_line,
                row(
                    25,
                    "  review gate",
                    st.system_one.review_gate.label().to_string(),
                    false,
                ),
                st.field == 25,
            );
            lines.push(Line::from(Span::styled(
                "      ↳ gate = skip a clean chunk's Reviewer call; standalone = it                  reviews alone",
                Style::default().fg(theme.ink_faint),
            )));
            push(
                &mut lines,
                &mut focus_line,
                row(
                    26,
                    "  transport",
                    st.system_one.provider.label().to_string(),
                    false,
                ),
                st.field == 26,
            );
            push(
                &mut lines,
                &mut focus_line,
                row(27, "  model", st.system_one.model.clone(), true),
                st.field == 27,
            );
            push(
                &mut lines,
                &mut focus_line,
                row(
                    28,
                    "  TypeSafe key",
                    mask(&st.typesafe_key, st.typesafe_key_env),
                    false,
                ),
                st.field == 28,
            );
            lines.push(Line::from(Span::styled(
                "      ↳ Only for the TypeSafe transport; over OpenRouter System One                  reuses your OpenRouter key",
                Style::default().fg(theme.ink_faint),
            )));
            push(
                &mut lines,
                &mut focus_line,
                row(
                    29,
                    "  min confidence",
                    format!("{}%", st.system_one_confidence),
                    true,
                ),
                st.field == 29,
            );
            lines.push(Line::from(Span::styled(
                "      ↳ Below this a judgement is not trusted and the deterministic                  path runs instead",
                Style::default().fg(theme.ink_faint),
            )));
            for (offset, feature) in crate::model::SystemOneFeature::ALL.into_iter().enumerate() {
                let idx = FIRST_FEATURE_FIELD + offset as u8;
                push(
                    &mut lines,
                    &mut focus_line,
                    row(
                        idx,
                        &format!("  {}", feature.label()),
                        if st.system_one.armed(feature) { "On" } else { "Off" }.to_string(),
                        false,
                    ),
                    st.field == idx,
                );
                lines.push(Line::from(Span::styled(
                    format!("      ↳ {}", feature.desc()),
                    Style::default().fg(theme.ink_faint),
                )));
            }
        }
        if st.tab == SettingsTab::Appearance {
            push(
                &mut lines,
                &mut focus_line,
                row(35, "Auto-update", st.update_mode.label().to_string(), false),
                st.field == 35,
            );
            push(
                &mut lines,
                &mut focus_line,
                row(
                    36,
                    "Update channel",
                    st.release_channel.label().to_string(),
                    false,
                ),
                st.field == 36,
            );
            lines.push(Line::from(vec![
                Span::styled(
                    "   Theme               ",
                    Style::default().fg(theme.ink_faint),
                ),
                Span::styled(cfg.theme.label(), Style::default().fg(theme.accent)),
                Span::styled("   Ctrl-T to change", Style::default().fg(theme.ink_faint)),
            ]));
        }
        if st.tab == SettingsTab::Account {
            match (&st.account_login, &st.remote_auth_code) {
                (_, Some(prompt)) => {
                    lines.push(Line::from(vec![
                        Span::styled(
                            "   GitHub             ",
                            Style::default().fg(theme.ink_faint),
                        ),
                        Span::styled(
                            prompt.code.clone(),
                            Style::default()
                                .fg(theme.accent)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    lines.push(Line::from(Span::styled(
                        format!("      ↳ enter it at {}", prompt.uri),
                        Style::default().fg(theme.ink_faint),
                    )));
                    lines.push(Line::from(Span::styled(
                        "      ↳ Ctrl-B open in browser · Ctrl-K copy code".to_string(),
                        Style::default().fg(theme.ink_faint),
                    )));
                }
                (None, None) => {
                    lines.push(Line::from(vec![
                        Span::styled(
                            "   GitHub             ",
                            Style::default().fg(theme.ink_faint),
                        ),
                        Span::styled("not signed in", Style::default().fg(theme.ink_soft)),
                        Span::styled("   Ctrl-A to sign in", Style::default().fg(theme.ink_faint)),
                    ]));
                }
                (Some(login), _) => {
                    lines.push(Line::from(vec![
                        Span::styled(
                            "   GitHub             ",
                            Style::default().fg(theme.ink_faint),
                        ),
                        Span::styled(format!("@{login}"), Style::default().fg(theme.status_done)),
                        Span::styled("   Ctrl-O sign out", Style::default().fg(theme.ink_faint)),
                    ]));
                    let (state_label, state_color) = if st.remote_enabled {
                        (
                            st.remote_state.label(),
                            match st.remote_state {
                                crate::remote::protocol::RemoteState::Connected => {
                                    theme.status_done
                                }
                                crate::remote::protocol::RemoteState::Error => theme.status_failed,
                                _ => theme.status_working,
                            },
                        )
                    } else {
                        ("disabled", theme.ink_soft)
                    };
                    lines.push(Line::from(vec![
                        Span::styled(
                            "   Remote link        ",
                            Style::default().fg(theme.ink_faint),
                        ),
                        Span::styled(state_label.to_string(), Style::default().fg(state_color)),
                        Span::styled("   Ctrl-R to toggle", Style::default().fg(theme.ink_faint)),
                    ]));
                    if let Some(label) = st.session_label.as_ref().filter(|_| st.remote_enabled) {
                        lines.push(Line::from(Span::styled(
                            format!("      ↳ this session: {label}"),
                            Style::default().fg(theme.ink_faint),
                        )));
                    }
                    if st.remote_enabled
                        && matches!(
                            st.remote_state,
                            crate::remote::protocol::RemoteState::Connected
                        )
                    {
                        let watchers = st.remote_watchers;
                        let note = if watchers == 0 {
                            "      ↳ no dashboards watching · open honya.altqx.com/app".to_string()
                        } else {
                            format!("      ↳ {watchers} dashboard(s) watching this session")
                        };
                        lines.push(Line::from(Span::styled(
                            note,
                            Style::default().fg(theme.ink_faint),
                        )));
                    }
                }
            }
        }

        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "   Keys saved to config.json (0600) · env vars override · ↵ save · Esc close",
            Style::default().fg(theme.ink_faint),
        )));
        // Keep the focused row visible (most tabs fit; scroll is a no-op then).
        let content_h = inner.height.max(1) as usize;
        let max_scroll = lines.len().saturating_sub(content_h);
        let scroll_y = focus_line.saturating_sub(content_h / 2).min(max_scroll) as u16;
        f.render_widget(
            Paragraph::new(lines)
                .style(Style::default().bg(theme.bg_panel))
                .scroll((scroll_y, 0)),
            inner,
        );
    }

    fn render_palette(&self, f: &mut Frame, area: Rect, theme: &Theme, st: &PaletteState) {
        let modal = centered_modal(60, 20, area);
        f.render_widget(Clear, modal);
        let block = self.modal_block("Command palette", theme);
        let inner = block.inner(modal);
        f.render_widget(block, modal);

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(0)])
            .split(inner);

        let (before, after) = input::caret_halves(
            &st.query,
            st.cursor,
            rows[0].width.saturating_sub(5) as usize,
        );
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

        // Window the rows so the selection stays visible (theme/jump pattern).
        let cap = (rows[1].height as usize).max(1);
        let sel = st.sel.min(matches.len() - 1);
        let start = if sel >= cap { sel + 1 - cap } else { 0 };
        let end = (start + cap).min(matches.len());

        let mut lines = Vec::with_capacity(end - start);
        for (row, &idx) in matches.iter().enumerate().take(end).skip(start) {
            let selected = row == sel;
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
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme.bg_panel)),
            rows[1],
        );
        crate::ui::widgets::render_scrollbar(f, rows[1], matches.len(), start, theme);
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

}

/// What a resolved overlay click should do — a synthesized key (reusing the
/// keyboard handlers), a direct action, or nothing.
enum ClickOutcome {
    Nothing,
    Key(KeyCode),
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

/// Rows of wizard chrome (step rail · context line · gap) above each step body;
/// resolve_click must mirror render_import's layout.
const IMPORT_HEADER_ROWS: u16 = 3;
/// Rows the pick step draws above its file list (header · gap).
const IMPORT_PICK_LIST_OFFSET: u16 = 3;

/// The wizard's step rail: done steps get a check, the current step is
/// highlighted, future steps are dimmed. The add-volume flow hides "Name".
fn step_rail(st: &ImportState, theme: &Theme) -> Line<'static> {
    // (step id, label, optional). Required steps are numbered; optional steps
    // Optional translated title and synopsis steps get a `◦` marker.
    let steps: &[(u8, &str, bool)] = if st.lock_name {
        &[
            (0, "File", false),
            (3, "Volume", false),
            (4, "Synopsis", true),
        ]
    } else {
        &[
            (0, "File", false),
            (1, "Name", false),
            (2, "Translated title", true),
            (3, "Volume", false),
            (4, "Synopsis", true),
        ]
    };
    let mut spans = vec![Span::raw(" ")];
    let mut num = 0u8;
    for (i, &(id, label, optional)) in steps.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ›  ", Style::default().fg(theme.rule)));
        }
        let marker = if optional {
            "◦".to_string()
        } else {
            num += 1;
            num.to_string()
        };
        if st.step > id {
            spans.push(Span::styled(
                format!("✓ {label}"),
                Style::default().fg(theme.status_done),
            ));
        } else if st.step == id {
            spans.push(Span::styled(
                format!("{marker} {label}"),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                format!("{marker} {label}"),
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
    spans.push(sep.clone());
    spans.push(Span::styled(
        st.effective_target_language().label().to_string(),
        Style::default().fg(theme.accent_soft),
    ));
    if st.lock_name || st.step > 1 {
        spans.push(sep.clone());
        spans.push(Span::styled(
            truncate_cols(&thai_display_safe(st.name.trim()), 30),
            confirmed,
        ));
    }
    if st.lock_name || st.step > 3 {
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
fn synopsis_hints(st: &SynopsisState, wizard: bool) -> &'static [(&'static str, &'static str)] {
    if st.edit_translation {
        return &[
            ("type", "translation"),
            ("Tab", "retranslate"),
            ("^S", "save"),
            ("Esc", "done"),
        ];
    }
    match st.phase {
        SynPhase::Editing => {
            if st.raw.trim().is_empty() {
                &[("type", "raw"), ("Tab", "skip"), ("Esc", "back")]
            } else {
                &[
                    ("type", "raw"),
                    ("Tab", "translate"),
                    ("^S", "continue"),
                    ("Esc", "back"),
                ]
            }
        }
        SynPhase::Translating => &[("Esc", "cancel"), ("…", "translating")],
        SynPhase::Done if wizard => &[
            ("↵", "start import"),
            ("e", "edit translation"),
            ("r", "reroll"),
            ("s", "skip"),
        ],
        SynPhase::Done => &[
            ("↵", "save"),
            ("e", "edit translation"),
            ("r", "reroll"),
            ("o", "src"),
        ],
        SynPhase::Failed => &[("e", "edit translation"), ("r", "retry"), ("o", "src")],
    }
}

/// Footer hints for the translated-title step: the user types the title
/// directly, with Tab to translate via the agent.
fn import_title_hints(st: &SynopsisState) -> &'static [(&'static str, &'static str)] {
    if st.phase == SynPhase::Translating {
        return &[("Esc", "cancel"), ("…", "translating")];
    }
    if st.translated_text.trim().is_empty() {
        return &[
            ("↵", "skip"),
            ("type", "translation"),
            ("Tab", "translate"),
            ("Esc", "back"),
        ];
    }
    &[
        ("type", "translation"),
        ("Tab", "translate"),
        ("↵", "next"),
        ("Esc", "back"),
    ]
}

fn title_hints(st: &SynopsisState) -> &'static [(&'static str, &'static str)] {
    if st.phase == SynPhase::Translating {
        return &[("Esc", "cancel"), ("…", "translating")];
    }
    &[
        ("type", "translation"),
        ("Tab", "translate"),
        ("↵", "save"),
        ("Esc", "cancel"),
    ]
}

struct EditorLabels {
    label: &'static str,
    placeholder: &'static str,
    input_rows: u16,
}

/// Render the synopsis editor body used by import and standalone edit.
fn render_synopsis_body(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    st: &SynopsisState,
    accept_label: &str,
) {
    render_editor_body(
        f,
        area,
        theme,
        st,
        accept_label,
        &EditorLabels {
            label: "  Synopsis · source  (optional — used as translation context)",
            placeholder: "Type or paste the source-language synopsis…  (leave empty, Tab to skip)",
            input_rows: 9,
        },
    );
}

/// Render `text` as caret-bearing lines (multi-line aware) for an editable field.
fn caret_text_lines<'a>(text: &'a str, cursor: usize, theme: &Theme) -> Vec<Line<'a>> {
    let cursor = input::clamp_cursor(text, cursor);
    let mut lines = Vec::new();
    let mut line_start = 0usize;
    for part in text.split('\n') {
        let line_end = line_start + part.len();
        let on_line = cursor >= line_start && cursor <= line_end;
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
        lines.push(Line::from(spans));
        line_start = line_end + 1;
    }
    lines
}

/// Shared edit/translate/accept body for synopsis and title editors.
fn render_editor_body(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    st: &SynopsisState,
    accept_label: &str,
    labels: &EditorLabels,
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(labels.input_rows),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);

    f.render_widget(
        Paragraph::new(Span::styled(
            thai_display_safe(labels.label),
            Style::default().fg(theme.ink_soft),
        ))
        .style(Style::default().bg(theme.bg_panel)),
        rows[0],
    );

    // While hand-editing the translation, the source box is a read-only reference.
    let editing = st.phase == SynPhase::Editing && !st.edit_translation;
    let border_color = if st.edit_translation || !editing {
        theme.rule
    } else {
        theme.accent_soft
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
                thai_display_safe(labels.placeholder),
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

    f.render_widget(
        Paragraph::new(editor_status(st, accept_label, theme, rows[2].width))
            .style(Style::default().bg(theme.bg_panel)),
        rows[2],
    );

    f.render_widget(
        Paragraph::new(Span::styled(
            "  Translation",
            Style::default().fg(theme.ink_soft),
        ))
        .style(Style::default().bg(theme.bg_panel)),
        rows[3],
    );

    // The translated output gets the same boxed treatment as the source, so the pair
    // reads as siblings; its border accents while it is the field being edited.
    let translation_block = Block::default()
        .borders(Borders::ALL)
        .border_set(theme::hairline_set())
        .border_style(Style::default().fg(if st.edit_translation {
            theme.accent_soft
        } else {
            theme.rule
        }))
        .style(Style::default().bg(theme.bg_inset));

    if st.edit_translation {
        let lines = if st.translated_text.is_empty() {
            vec![Line::from(vec![
                Span::styled(
                    "Type the translation, or press Tab to translate",
                    Style::default().fg(theme.ink_faint),
                ),
                Span::styled("▏", Style::default().fg(theme.stream_cursor)),
            ])]
        } else {
            caret_text_lines(&st.translated_text, st.translated_cursor, theme)
        };
        f.render_widget(
            Paragraph::new(Text::from(lines))
                .wrap(Wrap { trim: false })
                .block(translation_block),
            indent(rows[4], 2),
        );
        return;
    }

    let (body, color) = match st.phase {
        SynPhase::Failed => (st.error.clone(), theme.status_failed),
        _ if st.translated_text.trim().is_empty() => (
            "No translation yet — press Tab to translate".to_string(),
            theme.ink_faint,
        ),
        _ => (st.translated_text.clone(), theme.ink),
    };
    f.render_widget(
        Paragraph::new(crate::ui::text::thai_display_safe(&body))
            .wrap(Wrap { trim: false })
            .block(translation_block)
            .style(Style::default().fg(color).bg(theme.bg_inset)),
        indent(rows[4], 2),
    );
}

/// The editor's status/actions line — phase-aware, English chrome. `accept_label`
/// is the verb shown for the commit key (e.g. "save" / "start import").
fn editor_status(
    st: &SynopsisState,
    accept_label: &str,
    theme: &Theme,
    max_cols: u16,
) -> Span<'static> {
    let faint = Style::default().fg(theme.ink_faint);
    let text = editor_status_text(st, accept_label, max_cols as usize);
    if st.edit_translation {
        return Span::styled(text, faint);
    }
    match st.phase {
        SynPhase::Editing => Span::styled(text, faint),
        SynPhase::Translating => Span::styled(text, Style::default().fg(theme.status_working)),
        SynPhase::Done => Span::styled(text, Style::default().fg(theme.status_done)),
        SynPhase::Failed => Span::styled(text, Style::default().fg(theme.status_failed)),
    }
}

fn editor_status_text(st: &SynopsisState, accept_label: &str, max_cols: usize) -> String {
    if st.edit_translation {
        let msg = if st.multiline {
            "  Editing translation · Tab retranslate · Enter newline · Esc done".to_string()
        } else {
            format!("  Editing translation · Tab retranslate · Enter {accept_label} · Esc done")
        };
        return fit_status_text(std::iter::once(msg), max_cols);
    }

    match st.phase {
        SynPhase::Editing => {
            let msg = if st.raw.trim().is_empty() {
                "  Empty — Tab to skip · Esc back".to_string()
            } else if st.multiline {
                format!(
                    "  {} chars · Tab translate · Ctrl+S continue · Esc back",
                    st.raw.chars().count()
                )
            } else {
                format!(
                    "  {} chars · Tab/Enter translate · Esc back",
                    st.raw.chars().count()
                )
            };
            fit_status_text(std::iter::once(msg), max_cols)
        }
        SynPhase::Translating => fit_status_text(
            std::iter::once("  ◐ Translating with the agent… (Esc to cancel)".to_string()),
            max_cols,
        ),
        SynPhase::Done => {
            let attempt = st.attempt + 1;
            let accept = accept_label.trim();
            let short_accept = if accept == "start import" {
                "start"
            } else {
                accept
            };
            fit_status_text(
                [
                    format!(
                        "  ✓ Translated (try {attempt}) · Enter {accept} · e edit · r reroll · o source · s skip"
                    ),
                    format!(
                        "  ✓ Translated (try {attempt}) · Enter {short_accept} · e edit · r reroll · o source · s skip"
                    ),
                    format!(
                        "  ✓ Translated (try {attempt}) · Enter {short_accept} · e edit · r reroll · s skip"
                    ),
                    format!(
                        "  ✓ Translated (try {attempt}) · Enter {short_accept} · e edit · r reroll"
                    ),
                    format!("  ✓ Translated (try {attempt}) · Enter {short_accept}"),
                    format!("  ✓ Translated (try {attempt})"),
                ],
                max_cols,
            )
        }
        SynPhase::Failed => fit_status_text(
            [
                "  ✗ Translation failed · e write it · r retry · o source · s skip".to_string(),
                "  ✗ Translation failed · e write · r retry · s skip".to_string(),
                "  ✗ Translation failed · r retry · s skip".to_string(),
                "  ✗ Translation failed".to_string(),
            ],
            max_cols,
        ),
    }
}

fn fit_status_text<I>(candidates: I, max_cols: usize) -> String
where
    I: IntoIterator<Item = String>,
{
    let mut fallback = String::new();
    for msg in candidates {
        let safe = thai_display_safe(&msg);
        if col_width(&safe) <= max_cols {
            return safe;
        }
        fallback = safe;
    }
    truncate_cols(&fallback, max_cols)
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
        QaKind::Consistency => ("≠", theme.status_warn, "consistency".to_string()),
    }
}

/// Fallback detail text when a finding carries no reviewer reason / note.
fn qa_default_detail(issue: &qa::QaIssue) -> &'static str {
    match issue.kind {
        qa::QaKind::ChapterFailed => "translation failed — see activity log",
        qa::QaKind::ReviewChunk { .. } => "committed without passing review",
        qa::QaKind::Continuity { .. } => "continuity note",
        qa::QaKind::Consistency => "roster rendering differs across volumes",
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
pub fn prettify_stem(stem: &str) -> String {
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

    /// Render `ov` at `w`x`h` and hand back the painted lines plus the zones it
    /// registered. Kit-rendered overlays declare their own geometry, so this is
    /// how a test asks what is on screen and what is clickable at once.
    fn render_overlay(
        ov: &Overlay,
        w: u16,
        h: u16,
    ) -> (Vec<String>, crate::ui::kit::Zones) {
        render_overlay_at(ov, w, h, 0)
    }

    /// As [`render_overlay`], at a chosen animation frame.
    fn render_overlay_at(
        ov: &Overlay,
        w: u16,
        h: u16,
        frame: u64,
    ) -> (Vec<String>, crate::ui::kit::Zones) {
        use crate::ui::kit::focus::{Focus, Hover};
        use crate::ui::kit::tokens::Metrics;
        use crate::ui::kit::{Ui, Zones};

        let theme = Theme::washi();
        let cfg = AppConfig::default();
        let mut zones = Zones::new();
        let focus = Focus::new();
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let area = Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        term.draw(|f| {
            let metrics = Metrics::new(area, false);
            let mut ui = Ui::new(f, &mut zones, &theme, metrics, &focus, Hover::default(), frame);
            ov.render(&mut ui, area, &cfg, &[]);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let lines = (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
            .collect();
        (lines, zones)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// The kit-rendered overlays, with something in each worth drawing.
    fn kit_overlays() -> Vec<Overlay> {
        vec![
            Overlay::Help(0),
            Overlay::About,
            Overlay::Log(0),
            Overlay::confirm("Delete project?", "This cannot be undone.", Action::Quit),
            Overlay::export(2),
            Overlay::theme(ThemeId::default()),
        ]
    }

    #[test]
    fn every_kit_overlay_registers_only_what_it_drew() {
        // The invariant the whole phase is for: no zone outside the frame, and
        // no point inside a registered rect that hit-tests to nothing.
        for ov in kit_overlays() {
            for (w, h) in [(60u16, 20u16), (80, 24), (120, 40)] {
                let (_, zones) = render_overlay(&ov, w, h);
                assert!(
                    !zones.is_empty(),
                    "{ov:?} at {w}x{h} registered nothing at all"
                );
                for (rect, id) in zones.all() {
                    assert!(
                        rect.x + rect.width <= w && rect.y + rect.height <= h,
                        "{ov:?} at {w}x{h}: {id:?} at {rect:?} escaped the frame"
                    );
                    let (cx, cy) = (rect.x + rect.width / 2, rect.y + rect.height / 2);
                    assert!(
                        zones.at(cx, cy).is_some(),
                        "{ov:?} at {w}x{h}: nothing hit-tests inside {id:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_kit_overlay_shields_the_screen_behind_it() {
        // A click beside a modal must reach the backdrop, never the screen.
        for ov in kit_overlays() {
            let (_, zones) = render_overlay(&ov, 100, 30);
            assert_eq!(
                zones.at(0, 0),
                Some(ZoneId::bare(ZoneKind::Backdrop)),
                "{ov:?} left its corner unshielded"
            );
        }
    }

    #[test]
    fn every_kit_overlay_traps_the_focus_ring() {
        for ov in kit_overlays() {
            let (_, zones) = render_overlay(&ov, 100, 30);
            assert!(
                zones.is_trapped(),
                "{ov:?} did not trap focus, so Tab could wander behind it"
            );
        }
    }

    #[test]
    fn a_dialogs_buttons_do_exactly_what_their_keys_do() {
        // Keyboard and mouse parity, asserted rather than assumed: clicking a
        // button must leave the overlay in the same state its accelerator does.
        let mk = || {
            Overlay::confirm_with_alternate(
                "Resume run?",
                "An interrupted run was found.",
                "Resume",
                Action::Quit,
                'd',
                "Discard",
                Action::CloseOverlay,
            )
        };
        let (_, zones) = render_overlay(&mk(), 100, 30);

        for (button, code) in [
            (DIALOG_CONFIRM, KeyCode::Enter),
            (DIALOG_CANCEL, KeyCode::Esc),
            (DIALOG_ALTERNATE, KeyCode::Char('d')),
        ] {
            let rect = zones
                .rect_of(ZoneId::button(button))
                .unwrap_or_else(|| panic!("button {button} not registered"));

            let mut by_click = mk();
            let clicked = by_click.handle_mouse_zones(
                MouseInput {
                    gesture: MouseGesture::Click { double: false },
                    col: rect.x + rect.width / 2,
                    row: rect.y,
                },
                &zones,
            );
            let mut by_key = mk();
            let pressed = by_key.handle_key(key(code));

            assert_eq!(
                format!("{clicked:?}"),
                format!("{pressed:?}"),
                "button {button} and {code:?} produced different actions"
            );
            assert_eq!(
                matches!(by_click, Overlay::None),
                matches!(by_key, Overlay::None),
                "button {button} and {code:?} left different overlay state"
            );
        }
    }

    #[test]
    fn clicking_an_export_format_toggles_just_that_one() {
        let mut ov = Overlay::export(1);
        let (_, zones) = render_overlay(&ov, 100, 30);
        let before = match &ov {
            Overlay::Export(st) => st.formats,
            _ => unreachable!(),
        };
        let rect = zones.rect_of(ZoneId::row(0)).expect("format row 0");
        ov.handle_mouse_zones(
            MouseInput {
                gesture: MouseGesture::Click { double: false },
                col: rect.x + 1,
                row: rect.y,
            },
            &zones,
        );
        let after = match &ov {
            Overlay::Export(st) => st.formats,
            _ => unreachable!(),
        };
        assert_ne!(before[0], after[0], "the clicked format should have flipped");
        assert_eq!(&before[1..], &after[1..], "no other format may change");
    }

    #[test]
    fn clicking_a_theme_previews_and_double_clicking_keeps_it() {
        let mut ov = Overlay::theme(ThemeId::default());
        let (_, zones) = render_overlay(&ov, 100, 30);
        let rect = zones.rect_of(ZoneId::row(3)).expect("theme row 3");

        let preview = ov.handle_mouse_zones(
            MouseInput {
                gesture: MouseGesture::Click { double: false },
                col: rect.x + 2,
                row: rect.y,
            },
            &zones,
        );
        assert!(
            matches!(preview, Action::PreviewTheme(_)),
            "a single click should preview, got {preview:?}"
        );
        match &ov {
            Overlay::Theme(st) => assert_eq!(st.sel, 3),
            _ => unreachable!(),
        }

        let commit = ov.handle_mouse_zones(
            MouseInput {
                gesture: MouseGesture::Click { double: true },
                col: rect.x + 2,
                row: rect.y,
            },
            &zones,
        );
        assert!(
            matches!(commit, Action::SaveTheme(_)),
            "a double click should keep it, got {commit:?}"
        );
    }

    /// The close glyph and the backdrop both mean Esc, for every kit overlay.
    ///
    /// Asserted on the returned `Action` rather than on the overlay: most
    /// overlays hand `CloseOverlay` back for `App::apply` to act on, while a
    /// confirm dialog closes itself first so it can dismiss before running the
    /// action it wraps. Both are Esc; only one of them is visible from here.
    #[test]
    fn the_close_glyph_and_the_backdrop_both_mean_esc() {
        for ov in kit_overlays() {
            let (_, zones) = render_overlay(&ov, 100, 30);
            let close = zones
                .rect_of(ZoneId::button(crate::ui::kit::modal::CLOSE_BUTTON))
                // A dialog is answered, not dismissed, so it offers no close.
                .map(|r| (r.x + 1, r.y));
            let targets = [close, Some((0u16, 0u16))];

            for target in targets.into_iter().flatten() {
                let mut by_click = ov.clone();
                let clicked = by_click.handle_mouse_zones(
                    MouseInput {
                        gesture: MouseGesture::Click { double: false },
                        col: target.0,
                        row: target.1,
                    },
                    &zones,
                );
                let mut by_key = ov.clone();
                let pressed = by_key.handle_key(key(KeyCode::Esc));
                assert_eq!(
                    format!("{clicked:?}"),
                    format!("{pressed:?}"),
                    "{ov:?}: clicking {target:?} disagreed with Esc"
                );
            }
        }
    }

    #[test]
    fn the_help_table_covers_every_screen() {
        let rows = help_rows();
        let sections: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                HelpRow::Section(s) => Some(*s),
                _ => None,
            })
            .collect();
        for needle in ["Global", "Mouse", "Shelf", "Project", "Translate", "Reader", "Lexicon", "Refine"] {
            assert!(
                sections.iter().any(|s| s.contains(needle)),
                "help has no section for {needle}: {sections:?}"
            );
        }
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn wizard(projects: Vec<ProjectRef>) -> Overlay {
        wizard_with_language(projects, TargetLanguage::Thai)
    }

    fn wizard_with_language(
        projects: Vec<ProjectRef>,
        preferred_language: TargetLanguage,
    ) -> Overlay {
        Overlay::Import(ImportState::new(
            vec![(PathBuf::from("cursed_blade_v03.epub"), 2_345_678)],
            projects,
            preferred_language,
        ))
    }

    #[test]
    fn preferred_language_only_seeds_the_new_project_wizard() {
        let mut seeded = wizard_with_language(vec![], TargetLanguage::English);
        let Overlay::Import(st) = &seeded else {
            panic!("overlay changed variant")
        };
        assert_eq!(st.target_language, TargetLanguage::English);
        assert_eq!(st.effective_target_language(), TargetLanguage::English);
        if let Overlay::Import(st) = &mut seeded {
            st.step = 4;
        }
        match seeded.handle_key(key(KeyCode::Tab)) {
            Action::ImportFile {
                target_language, ..
            } => assert_eq!(target_language, TargetLanguage::English),
            other => panic!("expected ImportFile, got {other:?}"),
        }

        let mut ov = wizard_with_language(vec![], TargetLanguage::English);
        ov.handle_key(key(KeyCode::Right));
        let Overlay::Import(st) = &ov else {
            panic!("overlay changed variant")
        };
        assert_eq!(st.target_language, TargetLanguage::Thai);
    }

    #[test]
    fn existing_project_language_overrides_the_wizard_preference() {
        let existing = ProjectRef {
            slug: slugify("Cursed Blade"),
            title: "Cursed Blade".to_string(),
            target_language: TargetLanguage::Thai,
            volumes: vec![(1, 12)],
        };
        let mut st = ImportState::new(
            vec![(PathBuf::from("cursed_blade_v03.epub"), 2_345_678)],
            vec![existing],
            TargetLanguage::English,
        );
        st.step = 4;
        let mut ov = Overlay::Import(st);

        match ov.handle_key(key(KeyCode::Tab)) {
            Action::ImportFile {
                target_language, ..
            } => assert_eq!(target_language, TargetLanguage::Thai),
            other => panic!("expected ImportFile, got {other:?}"),
        }
    }

    /// Append mode skips every wizard step: picking a file (Enter on step 0)
    /// launches the import straight into the target volume.
    #[test]
    fn append_wizard_imports_into_target_volume_on_pick() {
        let mut ov = Overlay::Import(ImportState::new_append(
            vec![(PathBuf::from("bonus_short_story.epub"), 1_600_000)],
            vec![],
            "Party Got Sick III".into(),
            3,
            TargetLanguage::Thai,
        ));
        match ov.handle_key(key(KeyCode::Enter)) {
            Action::ImportFile {
                vol,
                append,
                source,
                ..
            } => {
                assert_eq!(vol, 3, "lands on the targeted volume");
                assert!(append, "append flag set");
                assert_eq!(source, PathBuf::from("bonus_short_story.epub"));
            }
            other => panic!("expected ImportFile, got {other:?}"),
        }
    }

    #[test]
    fn project_title_editor_can_hand_edit_translation_and_save() {
        let mut ov = Overlay::project_title_edit(
            "novel".into(),
            "夜の影".into(),
            String::new(),
            TargetLanguage::Thai,
        );

        // The editor starts focused on the translated field — type the name by hand.
        for c in "เงา".chars() {
            ov.handle_key(key(KeyCode::Char(c)));
        }
        if let Overlay::ProjectTitle(st) = &ov {
            assert!(
                st.syn.edit_translation,
                "starts editing the translated field"
            );
            assert_eq!(st.syn.translated_text, "เงา");
        } else {
            panic!("overlay changed variant");
        }
        match ov.handle_key(key(KeyCode::Enter)) {
            Action::SaveProjectTitle {
                id,
                raw,
                translated_title,
            } => {
                assert_eq!(id, "novel");
                assert_eq!(raw, "夜の影");
                assert_eq!(translated_title, "เงา", "hand-typed Thai is saved as-is");
            }
            other => panic!("expected SaveProjectTitle, got {other:?}"),
        }
    }

    #[test]
    fn synopsis_editor_can_continue_without_translating() {
        // Typed a source synopsis but don't want to translate: Ctrl+S continues.
        let mut ov = Overlay::synopsis_edit(
            String::new(),
            String::new(),
            1,
            "Novel".into(),
            TargetLanguage::Thai,
        );
        for c in "あらすじ".chars() {
            ov.handle_key(key(KeyCode::Char(c)));
        }
        match ov.handle_key(ctrl(KeyCode::Char('s'))) {
            Action::SaveSynopsis {
                raw,
                translated_synopsis,
            } => {
                assert_eq!(raw, "あらすじ");
                assert_eq!(translated_synopsis, "", "no translation was forced");
            }
            other => panic!("expected SaveSynopsis, got {other:?}"),
        }
    }

    #[test]
    fn synopsis_editor_can_hand_edit_translation() {
        let mut ov = Overlay::synopsis_edit(
            "源".into(),
            "เก่า".into(),
            1,
            "Novel".into(),
            TargetLanguage::Thai,
        );
        // 'e' focuses the translated field for hand-editing.
        ov.handle_key(key(KeyCode::Char('e')));
        if let Overlay::Synopsis(st) = &ov {
            assert!(st.syn.edit_translation, "'e' edits the translation by hand");
        } else {
            panic!("overlay changed variant");
        }
        for c in "ใหม่".chars() {
            ov.handle_key(key(KeyCode::Char(c)));
        }
        ov.handle_key(key(KeyCode::Esc)); // settle the translated field (multiline)
        match ov.handle_key(key(KeyCode::Enter)) {
            Action::SaveSynopsis {
                raw,
                translated_synopsis,
            } => {
                assert_eq!(raw, "源");
                assert_eq!(
                    translated_synopsis, "เก่าใหม่",
                    "hand-edited translation is saved"
                );
            }
            other => panic!("expected SaveSynopsis, got {other:?}"),
        }
    }

    #[test]
    fn project_title_editor_translates_then_saves() {
        let mut ov = Overlay::project_title_edit(
            "novel".into(),
            "夜の影".into(),
            String::new(),
            TargetLanguage::Thai,
        );

        // Tab hands the source off to the translator agent.
        match ov.handle_key(key(KeyCode::Tab)) {
            Action::TranslateProjectTitle {
                ref raw,
                attempt,
                target_language,
            } => {
                assert_eq!(raw, "夜の影");
                assert_eq!(attempt, 0);
                assert_eq!(target_language, TargetLanguage::Thai);
            }
            other => panic!("expected TranslateProjectTitle, got {other:?}"),
        }
        if let Overlay::ProjectTitle(st) = &ov {
            assert_eq!(st.syn.phase, SynPhase::Translating);
        } else {
            panic!("overlay changed variant");
        }

        ov.set_synopsis_result(Ok("เงาแห่งราตรี".into()));
        // The result lands back in the editable translated field; Enter saves it.
        match ov.handle_key(key(KeyCode::Enter)) {
            Action::SaveProjectTitle {
                id,
                raw,
                translated_title,
            } => {
                assert_eq!(id, "novel");
                assert_eq!(raw, "夜の影");
                assert_eq!(translated_title, "เงาแห่งราตรี");
            }
            other => panic!("expected SaveProjectTitle, got {other:?}"),
        }
    }

    #[test]
    fn project_title_editor_seeds_done_from_stored_translation() {
        let ov = Overlay::project_title_edit(
            "novel".into(),
            "夜の影".into(),
            "เงาแห่งราตรี".into(),
            TargetLanguage::Thai,
        );
        let Overlay::ProjectTitle(st) = &ov else {
            panic!("wrong variant");
        };
        assert_eq!(st.syn.phase, SynPhase::Done);
        assert!(!st.syn.multiline);
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
            target_language: TargetLanguage::Thai,
        };
        let mut ov = wizard(vec![existing]);
        if let Overlay::Import(st) = &mut ov {
            st.step = 1;
            st.name = "Cursed Blade".to_string();
            st.name_cursor = st.name.len();
        }
        ov.handle_key(key(KeyCode::Enter)); // → translated-title step
        ov.handle_key(key(KeyCode::Enter)); // empty translated title → volume step
        let Overlay::Import(st) = &ov else {
            panic!("overlay changed variant")
        };
        assert!(st.target_project().is_some());
        assert_eq!(st.step, 3);
        assert_eq!(st.vol, 3, "should pre-pick one past the highest volume");
    }

    /// A manually adjusted volume must survive going back and forward again.
    #[test]
    fn user_chosen_volume_is_not_overridden_by_the_suggestion() {
        let existing = ProjectRef {
            slug: slugify("Cursed Blade"),
            title: "Cursed Blade".to_string(),
            volumes: vec![(1, 12)],
            target_language: TargetLanguage::Thai,
        };
        let mut ov = wizard(vec![existing]);
        if let Overlay::Import(st) = &mut ov {
            st.step = 1;
            st.name = "Cursed Blade".to_string();
            st.name_cursor = st.name.len();
        }
        ov.handle_key(key(KeyCode::Enter)); // → translated-title step
        ov.handle_key(key(KeyCode::Enter)); // empty translated title → volume step, suggested 2
        ov.handle_key(key(KeyCode::Up)); // user picks 3
        ov.handle_key(key(KeyCode::Esc)); // back to translated title
        ov.handle_key(key(KeyCode::Esc)); // back to name
        ov.handle_key(key(KeyCode::Enter)); // forward again
        ov.handle_key(key(KeyCode::Enter)); // empty translated title → volume step
        let Overlay::Import(st) = &ov else {
            panic!("overlay changed variant")
        };
        assert_eq!(st.vol, 3, "manual pick must not be re-suggested away");
    }

    /// The translated-title step uses the title agent and threads the accepted
    /// result into the import action; skipping it
    /// leaves the title empty.
    #[test]
    fn wizard_title_step_threads_translated_title_into_import() {
        let mut ov = wizard(vec![]);
        if let Overlay::Import(st) = &mut ov {
            st.step = 1;
            st.name = "夜の影".to_string();
            st.name_cursor = st.name.len();
        }
        ov.handle_key(key(KeyCode::Enter)); // name → translated title
        match ov.handle_key(key(KeyCode::Tab)) {
            Action::TranslateProjectTitle {
                raw,
                attempt,
                target_language,
            } => {
                assert_eq!(raw, "夜の影");
                assert_eq!(attempt, 0);
                assert_eq!(target_language, TargetLanguage::Thai);
            }
            other => panic!("expected TranslateProjectTitle, got {other:?}"),
        }
        ov.set_synopsis_result(Ok("เงาแห่งราตรี".into()));
        ov.handle_key(key(KeyCode::Enter)); // accept → volume
        ov.handle_key(key(KeyCode::Enter)); // volume → synopsis
        match ov.handle_key(key(KeyCode::Tab)) {
            // Empty synopsis + Tab skips it and starts the import.
            Action::ImportFile {
                title,
                translated_title,
                ..
            } => {
                assert_eq!(title, "夜の影");
                assert_eq!(translated_title, "เงาแห่งราตรี");
            }
            other => panic!("expected ImportFile, got {other:?}"),
        }
    }

    /// Going back and re-entering the title step with the same name keeps the
    /// translation; changing the name resets it.
    #[test]
    fn wizard_title_translation_survives_back_unless_name_changes() {
        let mut ov = wizard(vec![]);
        if let Overlay::Import(st) = &mut ov {
            st.step = 1;
            st.name = "夜の影".to_string();
            st.name_cursor = st.name.len();
        }
        ov.handle_key(key(KeyCode::Enter));
        ov.handle_key(key(KeyCode::Tab));
        ov.set_synopsis_result(Ok("เงาแห่งราตรี".into()));
        ov.handle_key(key(KeyCode::Esc)); // back to name (same name)
        ov.handle_key(key(KeyCode::Enter)); // re-enter title step
        if let Overlay::Import(st) = &ov {
            assert_eq!(
                st.title_syn.translated_text, "เงาแห่งราตรี",
                "same name keeps the roll"
            );
            assert_eq!(st.title_syn.phase, SynPhase::Done);
        }
        ov.handle_key(key(KeyCode::Esc));
        ov.handle_key(key(KeyCode::Char('x'))); // edit the name
        ov.handle_key(key(KeyCode::Enter));
        let Overlay::Import(st) = &ov else {
            panic!("overlay changed variant")
        };
        assert!(
            st.title_syn.translated_text.is_empty(),
            "a changed name must reset the stale translation"
        );
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
            target_language: TargetLanguage::Thai,
        };
        let mut st = ImportState::new(
            vec![(PathBuf::from("ดาบคำสาป_v01.epub"), 2_345_678)],
            vec![existing],
            TargetLanguage::Thai,
        );
        st.name = "ดาบคำสาป".to_string();
        st.name_cursor = st.name.len();
        st.title_syn =
            SynopsisState::new_single_line("ดาบคำสาป".to_string(), "คำสาปดาบ".to_string());
        st.syn.raw = "คำสาปแห่งดาบ".to_string();
        for step in 0..=5u8 {
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

    #[test]
    fn settings_tabs_switch_and_scope_fields() {
        let mut st = SettingsState::for_test(0);
        assert_eq!(st.tab, SettingsTab::Agents);

        st.switch_tab(true);
        assert_eq!(st.tab, SettingsTab::Providers);
        assert_eq!(st.field, 12, "focus jumps to the tab's first field");
        st.next_field();
        assert_eq!(st.field, 13);
        st.next_field();
        assert_eq!(st.field, 14);
        st.next_field();
        assert_eq!(st.field, 15);
        st.next_field();
        assert_eq!(st.field, 16);
        st.next_field();
        assert_eq!(st.field, 12, "field nav wraps within the tab");

        let st = SettingsState::for_test(17); // Translation language
        assert_eq!(st.tab, SettingsTab::Pipeline);
        let mut st = st;
        // 24..=34 are the System One block appended to this tab.
        for expected in 18..=34 {
            st.next_field();
            assert_eq!(st.field, expected);
        }
        st.next_field();
        assert_eq!(st.field, 17, "pipeline field nav wraps within the tab");

        let mut st = SettingsState::for_test(35);
        assert_eq!(st.tab, SettingsTab::Appearance);
        st.next_field();
        assert_eq!(st.field, 36);
        st.next_field();
        assert_eq!(st.field, 35, "appearance field nav wraps within the tab");

        let mut st = SettingsState::for_test(0);
        st.tab = SettingsTab::Account;
        assert!(!st.tab_has_fields());
    }

    #[test]
    fn review_gate_mode_cycles_and_reaches_save_action() {
        use crate::model::{DecisionsProvider, ReviewGateMode};

        let mut st = SettingsState::for_test(25); // System One · review gate
        assert_eq!(st.system_one.review_gate, ReviewGateMode::Off);
        st.cycle(true);
        assert_eq!(st.system_one.review_gate, ReviewGateMode::Gate);
        st.cycle(true);
        assert_eq!(st.system_one.review_gate, ReviewGateMode::Standalone);
        st.cycle(true);
        assert_eq!(st.system_one.review_gate, ReviewGateMode::Off, "mode wraps");
        st.cycle(false);
        assert_eq!(st.system_one.review_gate, ReviewGateMode::Standalone);

        st.system_one_confidence = "65".to_string();
        let Action::SaveSettings { system_one, .. } = st.save_action() else {
            panic!("expected SaveSettings");
        };
        assert_eq!(system_one.review_gate, ReviewGateMode::Standalone);
        assert_eq!(system_one.provider, DecisionsProvider::OpenRouter);
        assert!((system_one.min_confidence - 0.65).abs() < 1e-9);
    }

    #[test]
    fn gate_transport_cycle_swaps_the_default_model_id() {
        use crate::model::DecisionsProvider;

        let mut st = SettingsState::for_test(26); // System One · transport
        assert_eq!(st.system_one.provider, DecisionsProvider::OpenRouter);
        assert_eq!(st.system_one.model, "typesafe/jev-1.13");

        st.cycle(true);
        assert_eq!(st.system_one.provider, DecisionsProvider::TypeSafe);
        assert_eq!(
            st.system_one.model, "jev-latest",
            "a model left at the old transport's default follows the transport"
        );

        // A hand-typed model id must survive a transport switch.
        st.system_one.model = "jev-pinned".to_string();
        st.cycle(true);
        assert_eq!(st.system_one.provider, DecisionsProvider::OpenRouter);
        assert_eq!(st.system_one.model, "jev-pinned");
    }

    /// The renderer walks `SystemOneFeature::ALL` from a fixed field index, so
    /// that index must line up with `SETTINGS_ORDER`.
    #[test]
    fn feature_rows_line_up_with_the_field_order() {
        use crate::model::SystemOneFeature;

        for (offset, feature) in SystemOneFeature::ALL.into_iter().enumerate() {
            let field = SETTINGS_ORDER[FIRST_FEATURE_FIELD as usize + offset];
            assert_eq!(
                field_feature(field),
                Some(feature),
                "settings row {} must be the {feature:?} toggle",
                FIRST_FEATURE_FIELD as usize + offset
            );
        }
        assert_eq!(
            SETTINGS_ORDER[FIRST_FEATURE_FIELD as usize + SystemOneFeature::ALL.len()],
            SField::UpdateModeField,
            "the feature block must end where the Appearance tab begins"
        );
    }

    #[test]
    fn master_switch_and_each_feature_toggle_independently() {
        use crate::model::SystemOneFeature;

        let mut st = SettingsState::for_test(24); // System One · master switch
        assert!(!st.system_one.enabled);
        st.cycle(true);
        assert!(st.system_one.enabled);

        // Every feature starts armed, so the suite follows the master switch.
        assert!(SystemOneFeature::ALL.iter().all(|f| st.system_one.feature(*f)));

        st.focus(FIRST_FEATURE_FIELD); // Semantic audit
        st.cycle(true);
        assert!(!st.system_one.audit, "the row toggles only its own feature");
        assert!(st.system_one.continuity);

        let Action::SaveSettings { system_one, .. } = st.save_action() else {
            panic!("expected SaveSettings");
        };
        assert!(system_one.enabled);
        assert!(!system_one.feature(SystemOneFeature::Audit));
        assert!(system_one.feature(SystemOneFeature::Continuity));
    }

    /// Codex model fields are pickers, not free text.
    #[test]
    fn codex_provider_snaps_and_cycles_model() {
        let mut st = SettingsState::for_test(0); // Orchestrator · provider
        st.cycle(true); // OpenRouter → Tokenrouter
        st.cycle(true); // Tokenrouter → Google
        st.cycle(true); // Google → Cloudflare
        st.cycle(true); // Cloudflare → Codex
        assert_eq!(
            st.models.orchestrator.provider,
            crate::model::Provider::Codex
        );
        assert!(CODEX_MODELS.contains(&st.models.orchestrator.model.as_str()));

        st.focus(1); // Orchestrator · model
        assert!(st.is_codex_model());
        assert!(!st.current_is_editable_text(), "Codex model is a picker");
        let before = st.models.orchestrator.model.clone();
        st.cycle(true);
        assert_ne!(st.models.orchestrator.model, before);
        assert!(CODEX_MODELS.contains(&st.models.orchestrator.model.as_str()));
    }

    #[test]
    fn provider_cycle_restores_remembered_model_for_each_provider() {
        let mut st = SettingsState::for_test(0); // Orchestrator · provider
        st.models.orchestrator.set_model("openrouter/custom");

        st.cycle(true); // OpenRouter → Tokenrouter
        st.models.orchestrator.set_model("tokenrouter/custom");

        st.cycle(true); // Tokenrouter → Google
        st.models.orchestrator.set_model("google/custom");

        st.cycle(true); // Google → Cloudflare
        st.models.orchestrator.set_model("@cf/custom/model");

        st.cycle(true); // Cloudflare → Codex
        st.cycle(true); // Codex → OpenRouter
        assert_eq!(st.models.orchestrator.model, "openrouter/custom");

        st.cycle(true); // OpenRouter → Tokenrouter
        assert_eq!(st.models.orchestrator.model, "tokenrouter/custom");

        st.cycle(true); // Tokenrouter → Google
        assert_eq!(st.models.orchestrator.model, "google/custom");

        st.cycle(true); // Google → Cloudflare
        assert_eq!(st.models.orchestrator.model, "@cf/custom/model");

        st.cycle(false); // Cloudflare → Google
        assert_eq!(st.models.orchestrator.model, "google/custom");
    }

    /// The redesigned Settings overlay must render at every focus position
    /// (exercising the focus-following scroll) without panicking, across a small
    /// modal where the field list is taller than the visible area.
    #[test]
    fn settings_overlay_renders_at_every_focus() {
        let theme = Theme::washi();
        let cfg = AppConfig::default();
        for field in 0..SETTINGS_FIELDS {
            let ov = Overlay::Settings(SettingsState::for_test(field));
            let Overlay::Settings(st) = &ov else {
                unreachable!()
            };
            let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
            term.draw(|f| ov.render_settings(f, f.area(), &theme, &cfg, st))
                .unwrap();
            if field == 19 {
                let glyphs: String = term
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                assert!(glyphs.contains("Continuity sentences"));
            }
            if field == 23 {
                let glyphs: String = term
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                assert!(glyphs.contains("Parallel lookahead"));
                assert!(glyphs.contains("increase API cost"));
            }
        }
    }

    /// The About card animates off the frame ticker: every animation phase must
    /// render without panicking and without leaking raw SARA AM, and the typing
    /// loop must reach the full Thai line.
    #[test]
    fn about_card_renders_across_animation_frames() {
        let mut saw_full_line = false;
        for frame in 0..80u64 {
            let (lines, _) = render_overlay_at(&Overlay::About, 80, 24, frame);
            let glyphs: String = lines.concat();
            assert!(
                !glyphs.contains('\u{0E33}'),
                "raw SARA AM leaked into the About card at frame {frame}"
            );
            assert!(
                glyphs.contains("honya"),
                "brand line missing at frame {frame}"
            );
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

    #[test]
    fn import_done_status_fits_modal_body() {
        let mut st = SynopsisState::new("源のあらすじ".to_string(), "เรื่องย่อภาษาไทย".to_string());
        st.phase = SynPhase::Done;

        let status = editor_status_text(&st, "start import", 76);

        assert!(
            col_width(&status) <= 76,
            "status exceeds import modal body: {status}"
        );
        assert!(
            !status.ends_with('…'),
            "status should choose a fitting variant, not truncate: {status}"
        );
        assert!(
            status.contains("s skip"),
            "status lost the final action hint: {status}"
        );
    }

    #[test]
    fn palette_keeps_selection_visible_when_list_overflows() {
        let mut ov = Overlay::palette();
        let Overlay::Palette(st) = &mut ov else {
            unreachable!()
        };
        // Force a selection past the first page of a short modal.
        st.sel = st.items.len().saturating_sub(1);
        let last_label = st.items[st.sel].label;

        let (lines, _) = render_overlay(&ov, 80, 16);
        let glyphs: String = lines.concat();
        assert!(
            glyphs.contains(last_label),
            "selected palette row should stay in view: missing {last_label:?} in {glyphs:?}"
        );
    }
}
