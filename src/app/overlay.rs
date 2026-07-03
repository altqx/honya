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
use crate::model::{AppConfig, LogLevel, ReleaseChannel, ServiceTier, ThemeId, UpdateMode};
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
    /// Latest Thai translation — directly hand-editable when `edit_th` is set.
    pub th: String,
    /// Caret byte-offset into `th` (used while `edit_th`).
    pub th_cursor: usize,
    /// Focus on the Thai field: type to edit it by hand instead of only
    /// translating via the agent. Only used by the single-line title editors.
    pub edit_th: bool,
    pub phase: SynPhase,
    /// Error text shown while `phase == Failed`.
    pub error: String,
    /// Reroll counter — drives rising translation temperature.
    pub attempt: u32,
    /// False when Enter should submit instead of inserting a newline.
    pub multiline: bool,
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
            th_cursor: th.len(),
            raw,
            th,
            edit_th: false,
            phase,
            error: String::new(),
            attempt: 0,
            multiline: true,
        }
    }

    pub fn new_single_line(raw: String, th: String) -> Self {
        Self {
            multiline: false,
            ..Self::new(raw, th)
        }
    }

    /// Single-line editor focused on hand-editing the Thai field (project title):
    /// the user types the Thai name directly, with Tab to translate via the agent.
    pub fn new_title(raw: String, th: String) -> Self {
        Self {
            edit_th: true,
            ..Self::new_single_line(raw, th)
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
    // A translation round-trip is in flight; ignore input but allow cancel.
    // Late results are dropped by set_synopsis_result once the phase changes.
    if st.phase == SynPhase::Translating {
        if key.code == KeyCode::Esc {
            st.phase = SynPhase::Editing;
        }
        return SynKey::None;
    }
    // Hand-editing the Thai field directly: type to edit, Tab to (re)translate from
    // the source. Single-line (title): Enter accepts, Esc cancels. Multiline
    // (synopsis): Enter adds a newline, Esc returns to the settled view to save.
    if st.edit_th {
        let opts = EditOpts {
            numeric_only: false,
            multiline: st.multiline,
        };
        if input::handle(&mut st.th, &mut st.th_cursor, key, opts) != Edited::Ignored {
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
                input::insert_char(&mut st.th, &mut st.th_cursor, '\n');
                SynKey::None
            }
            KeyCode::Enter => SynKey::Accept,
            KeyCode::Esc if st.multiline => {
                st.edit_th = false;
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
            // Edit the Thai translation by hand.
            KeyCode::Char('e') | KeyCode::Char('E') => {
                st.edit_th = true;
                st.th_cursor = st.th.len();
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
    pub syn: SynopsisState,
}

/// Project title editor state; `id` is the stable project slug.
#[derive(Debug, Clone)]
pub struct TitleEditState {
    pub id: String,
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

/// The import wizard: pick source file → name → Thai title → volume → synopsis →
/// importing. When `lock_name` is set (the "add volume to this project" flow),
/// the name and Thai-title steps are skipped and the title is fixed to the open
/// project's.
#[derive(Debug, Clone)]
pub struct ImportState {
    /// 0 = pick, 1 = name, 2 = Thai title, 3 = volume, 4 = synopsis, 5 = importing.
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
    /// Thai-title translate/reroll loop (wizard step 2); `raw` mirrors `name`.
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
    ) -> Self {
        Self {
            append_to: Some(vol.max(1)),
            ..Self::new_into(files, projects, title, vol)
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
            title_syn: SynopsisState::new_title(String::new(), String::new()),
            syn: SynopsisState::new(String::new(), String::new()),
            progress: None,
            append_to: None,
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

    fn selected_file(&self) -> Option<&PathBuf> {
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
    MaxAttempts,
    LoopStall,
    Retranslates,
    ServiceTierField,
    UpdateModeField,
    ReleaseChannelField,
}

const SETTINGS_ORDER: [SField; 23] = [
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
    SField::MaxAttempts,
    SField::LoopStall,
    SField::Retranslates,
    SField::ServiceTierField,
    SField::UpdateModeField,
    SField::ReleaseChannelField,
];

/// Number of focusable Settings fields.
const SETTINGS_FIELDS: u8 = SETTINGS_ORDER.len() as u8;
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
const SETTINGS_KEY_FIELD: u8 = 12;

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
    const ALL: [SettingsTab; 5] = [
        SettingsTab::Agents,
        SettingsTab::Providers,
        SettingsTab::Pipeline,
        SettingsTab::Appearance,
        SettingsTab::Account,
    ];

    fn title(self) -> &'static str {
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
            SettingsTab::Pipeline => (17, 21),
            SettingsTab::Appearance => (21, 23),
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
                | SField::LoopStall
                | SField::Retranslates
        )
    }

    /// A digits-only numeric field.
    fn is_numeric(self) -> bool {
        matches!(
            self,
            SField::MaxAttempts | SField::LoopStall | SField::Retranslates
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
    /// Max Translator↔Reviewer retry attempts per chunk, as typed (digits only).
    pub max_attempts: String,
    /// Loop-watchdog stall window in seconds, as typed (digits only; 0 disables).
    pub loop_stall_secs: String,
    /// Whole-chapter re-translations allowed on a detected loop, as typed (digits).
    pub max_chapter_retranslates: String,
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
            max_attempts: cfg.max_attempts.to_string(),
            loop_stall_secs: cfg.loop_stall_secs.to_string(),
            max_chapter_retranslates: cfg.max_chapter_retranslates.to_string(),
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
            SField::LoopStall => &mut self.loop_stall_secs,
            SField::Retranslates => &mut self.max_chapter_retranslates,
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

/// Chunk proofreading popover: source, Thai, and active reviewer note.
#[derive(Debug, Clone)]
pub struct ReaderInspectState {
    pub chapter: u32,
    pub chunk: u32,
    pub source_jp: String,
    pub thai: String,
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
    /// Read-only source‖Thai‖reviewer-note popover for the current chunk.
    ReaderInspect(ReaderInspectState),
    /// In-place editor for the current chunk's Thai prose.
    ReaderEdit(ReaderEditState),
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

    /// "Add chapters" wizard, pre-targeted at an open project's volume `vol`: the
    /// pick step launches the import immediately and chapters append after `vol`'s
    /// last chapter.
    pub fn import_append(
        files: Vec<(PathBuf, u64)>,
        projects: &[crate::model::Project],
        title: String,
        vol: u32,
    ) -> Self {
        let refs = projects.iter().map(ProjectRef::of).collect();
        Overlay::Import(ImportState::new_append(files, refs, title, vol))
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

    /// Standalone synopsis editor seeded from a volume's stored raw/Thai; `vol`
    /// and `title` name the target in the modal title.
    pub fn synopsis_edit(raw: String, th: String, vol: u32, title: String) -> Self {
        Overlay::Synopsis(SynopsisEditState {
            vol,
            title,
            syn: SynopsisState::new(raw, th),
        })
    }

    pub fn project_title_edit(id: String, title: String, title_th: String) -> Self {
        Overlay::ProjectTitle(TitleEditState {
            id,
            syn: SynopsisState::new_title(title, title_th),
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
        thai: String,
        review: Option<String>,
    ) -> Self {
        Overlay::ReaderInspect(ReaderInspectState {
            chapter,
            chunk,
            source_jp,
            thai,
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
    fn settings_at(field: u8) -> Self {
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
            update_mode: UpdateMode::default(),
            release_channel: ReleaseChannel::default(),
            service_tier: None,
            max_attempts: String::new(),
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
                st.th = text;
                st.th_cursor = st.th.len();
                st.error.clear();
                st.phase = SynPhase::Done;
                // Single-line title flow: drop the user back into editing the
                // result so they can tweak the agent's translation by hand.
                if !st.multiline {
                    st.edit_th = true;
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
                    || (st.step == 2 && st.title_syn.edit_th)
                    || (st.step == 4 && (st.syn.phase == SynPhase::Editing || st.syn.edit_th))
            }
            Overlay::Synopsis(st) => st.syn.phase == SynPhase::Editing || st.syn.edit_th,
            Overlay::ProjectTitle(st) => st.syn.edit_th || st.syn.phase == SynPhase::Editing,
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
            Overlay::ImageSource(_) => centered_modal(78, 24, area),
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
            Overlay::ProjectTitle(_) => centered_modal(72, 16, area),
            Overlay::Qa(_) => centered_pct(80, 80, area),
            Overlay::ReaderNote(_) => centered_modal(72, 14, area),
            Overlay::ReaderInspect(_) => centered_pct(82, 80, area),
            Overlay::ReaderEdit(_) => centered_pct(82, 75, area),
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
                            title_th: String::new(),
                            vol,
                            synopsis_raw: String::new(),
                            synopsis_th: String::new(),
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
                // Type the Thai title by hand; Tab translates via the agent;
                // Enter (with or without a Thai title) continues; Esc → name step.
                match handle_synopsis_keys(&mut st.title_syn, key) {
                    SynKey::None => Action::None,
                    SynKey::Translate => Action::TranslateProjectTitle {
                        raw: st.title_syn.raw.clone(),
                        attempt: st.title_syn.attempt,
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
                    },
                    SynKey::Back => {
                        st.step = 3;
                        Action::None
                    }
                    SynKey::Accept => {
                        let source = st.selected_file().cloned().unwrap_or_default();
                        let title = st.name.trim().to_string();
                        let title_th = st.title_syn.th.trim().to_string();
                        let vol = st.vol.max(1);
                        let synopsis_raw = st.syn.raw.trim().to_string();
                        let synopsis_th = st.syn.th.trim().to_string();
                        st.step = 5;
                        st.progress = Some((0, 0, "starting".to_string()));
                        Action::ImportFile {
                            source,
                            title,
                            title_th,
                            vol,
                            synopsis_raw,
                            synopsis_th,
                            append: false,
                        }
                    }
                    SynKey::Skip => {
                        let source = st.selected_file().cloned().unwrap_or_default();
                        let title = st.name.trim().to_string();
                        let title_th = st.title_syn.th.trim().to_string();
                        let vol = st.vol.max(1);
                        st.step = 5;
                        st.progress = Some((0, 0, "starting".to_string()));
                        Action::ImportFile {
                            source,
                            title,
                            title_th,
                            vol,
                            synopsis_raw: String::new(),
                            synopsis_th: String::new(),
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
            KeyCode::Enter => {
                let mut models = st.models.clone();
                models.remember_active_models();
                Action::SaveSettings {
                    models: Box::new(models),
                    // Env keys must not overwrite saved config.
                    openrouter_key: if st.api_key_env {
                        None
                    } else {
                        Some(st.openrouter_key.clone())
                    },
                    tokenrouter_key: if st.tokenrouter_key_env {
                        None
                    } else {
                        Some(st.tokenrouter_key.clone())
                    },
                    google_key: if st.google_key_env {
                        None
                    } else {
                        Some(st.google_key.clone())
                    },
                    cloudflare_account_id: if st.cloudflare_account_id_env {
                        None
                    } else {
                        Some(st.cloudflare_account_id.clone())
                    },
                    cloudflare_api_token: if st.cloudflare_api_token_env {
                        None
                    } else {
                        Some(st.cloudflare_api_token.clone())
                    },
                    update_mode: st.update_mode,
                    release_channel: st.release_channel,
                    service_tier: st.service_tier,
                    max_attempts: st.max_attempts_value(),
                    loop_stall_secs: st.loop_stall_secs_value(),
                    max_chapter_retranslates: st.max_chapter_retranslates_value(),
                }
            }
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
            },
            SynKey::Accept => Action::SaveSynopsis {
                raw: st.syn.raw.clone(),
                th: st.syn.th.clone(),
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
            },
            SynKey::Accept => Action::SaveProjectTitle {
                id: st.id.clone(),
                raw: st.syn.raw.clone(),
                th: st.syn.th.clone(),
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
            Overlay::ReaderInspect(_) => {
                &[("jk", "scroll"), ("e", "edit Thai"), ("Esc/q", "close")]
            }
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
            Overlay::ImageSource(st) => self.render_image_source(f, area, theme, st),
            Overlay::Settings(st) => self.render_settings(f, area, theme, cfg, st),
            Overlay::Theme(st) => self.render_theme(f, area, theme, st),
            Overlay::Palette(st) => self.render_palette(f, area, theme, st),
            Overlay::Log(off) => self.render_log(f, area, theme, log, *off),
            Overlay::Help(off) => self.render_help(f, area, theme, *off),
            Overlay::About => self.render_about(f, area, theme, frame),
            Overlay::Modal(dlg) => self.render_modal(f, area, theme, dlg),
            Overlay::Synopsis(st) => self.render_synopsis(f, area, theme, st),
            Overlay::ProjectTitle(st) => self.render_project_title(f, area, theme, st),
            Overlay::Qa(st) => self.render_qa(f, area, theme, st),
            Overlay::ReaderNote(st) => self.render_reader_note(f, area, theme, st),
            Overlay::ReaderInspect(st) => self.render_reader_inspect(f, area, theme, st),
            Overlay::ReaderEdit(st) => self.render_reader_edit(f, area, theme, st),
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
                label: "  Title · source  (translate to Thai with the Translator agent)",
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
        lines.push(head("ไทย · translation", theme.accent));
        for l in st.thai.lines() {
            lines.push(Line::from(Span::styled(
                l.to_string(),
                Style::default().fg(theme.th_text),
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
        let title = format!("Edit Thai · ch {:03} · chunk {}", st.chapter, st.chunk + 1);
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
                    Style::default().fg(theme.th_text),
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

    fn render_import_title(&self, f: &mut Frame, area: Rect, theme: &Theme, st: &ImportState) {
        let syn = &st.title_syn;
        let faint = Style::default().fg(theme.ink_faint);
        let mut lines = vec![
            Line::from(vec![
                Span::styled("  Thai title", Style::default().fg(theme.ink_soft)),
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
                let mut spans = vec![Span::styled("  Thai      ", faint)];
                if syn.edit_th {
                    if syn.th.is_empty() {
                        spans.push(Span::styled(thai_display_safe("พิมพ์ชื่อไทย…"), faint));
                        spans.push(Span::styled("▏", Style::default().fg(theme.stream_cursor)));
                    } else {
                        let (before, after) = input::caret_halves(&syn.th, syn.th_cursor, 48);
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
                } else if syn.th.trim().is_empty() {
                    spans.push(Span::styled(
                        thai_display_safe("(no Thai title yet.)"),
                        faint,
                    ));
                } else {
                    spans.push(Span::styled(
                        thai_display_safe(syn.th.trim()),
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
                    thai_display_safe("  ✗ failed — type a Thai title, Tab to retry · Esc cancel"),
                    Style::default().fg(theme.status_failed),
                )));
                lines.push(Line::from(Span::styled(
                    thai_display_safe(&format!("    {}", syn.error)),
                    Style::default().fg(theme.status_failed),
                )));
            }
            // Edit_th is on for both Editing and Done in the title flow.
            _ => {
                let msg = if syn.th.trim().is_empty() {
                    "  ↵ skip · type a Thai title · Tab to translate it for you"
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
                row(17, "Retry attempts", st.max_attempts.clone(), true),
                st.field == 17,
            );
            lines.push(Line::from(Span::styled(
                "      ↳ Translator↔Reviewer loop per chunk (1–20)",
                Style::default().fg(theme.ink_faint),
            )));
            push(
                &mut lines,
                &mut focus_line,
                row(18, "Loop watchdog (s)", st.loop_stall_secs.clone(), true),
                st.field == 18,
            );
            lines.push(Line::from(Span::styled(
                "      ↳ quiet pipeline stalls after N s; active model calls retry chunk first",
                Style::default().fg(theme.ink_faint),
            )));
            push(
                &mut lines,
                &mut focus_line,
                row(
                    19,
                    "Loop re-translates",
                    st.max_chapter_retranslates.clone(),
                    true,
                ),
                st.field == 19,
            );
            lines.push(Line::from(Span::styled(
                "      ↳ stalled-chapter re-translates before the run aborts (0–10)",
                Style::default().fg(theme.ink_faint),
            )));
            push(
                &mut lines,
                &mut focus_line,
                row(
                    20,
                    "Service tier",
                    ServiceTier::label(st.service_tier).to_string(),
                    false,
                ),
                st.field == 20,
            );
            lines.push(Line::from(Span::styled(
                format!("      ↳ {}", ServiceTier::desc(st.service_tier)),
                Style::default().fg(theme.ink_faint),
            )));
        }
        if st.tab == SettingsTab::Appearance {
            push(
                &mut lines,
                &mut focus_line,
                row(21, "Auto-update", st.update_mode.label().to_string(), false),
                st.field == 21,
            );
            push(
                &mut lines,
                &mut focus_line,
                row(
                    22,
                    "Update channel",
                    st.release_channel.label().to_string(),
                    false,
                ),
                st.field == 22,
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
        crate::ui::widgets::render_panel_scrollbar(f, modal, log.len(), start, theme);
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
                    ("Ctrl-C", "quit"),
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
                    ("Space", "mark chapter (cross-volume ok)"),
                    ("t / a", "translate / queue marked or current"),
                    ("T", "whole volume"),
                    ("A", "whole project"),
                    ("h / l", "collapse · expand volume / focus panel"),
                    ("z / Z", "collapse all · expand all volumes"),
                    ("V", "add volume (import wizard)"),
                    ("i", "add chapters to volume (append import)"),
                    ("M", "update volume images from source EPUB"),
                    ("x", "export volume EPUB"),
                    ("e", "edit project Thai name"),
                    ("y", "volume synopsis (translate/reroll)"),
                    ("d", "delete marked/current chapter(s)"),
                    ("Q", "QA review (flagged issues)"),
                ],
            ),
            (
                "Translate 訳",
                &[
                    ("p / s", "pause · stop"),
                    ("f", "toggle follow-streaming"),
                    ("c", "cycle focused agent"),
                    ("g", "focus the run queue"),
                    ("J / K", "move queued chapter down · up"),
                    ("S", "sort the queue"),
                    ("x", "remove queued chapter"),
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
                    ("i / e", "inspect chunk (JP‖TH‖review) · edit Thai"),
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
            (
                "Refine 精",
                &[
                    ("↵", "send message · run slash command"),
                    ("@", "mention a chapter / character / term"),
                    ("⌃R", "expand / collapse reasoning + tool details"),
                    ("/new /sessions", "new conversation · switch"),
                    ("/undo /diff", "restore · diff last chapter edit"),
                    ("/model", "set the refine model"),
                    ("/cancel", "stop the in-flight reply"),
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
        let total = lines.len();
        let max_off = total.saturating_sub(cap) as u16;
        let off = off.min(max_off);
        f.render_widget(
            Paragraph::new(Text::from(lines))
                .wrap(Wrap { trim: false })
                .scroll((off, 0))
                .style(Style::default().bg(theme.bg_panel)),
            inner,
        );
        crate::ui::widgets::render_panel_scrollbar(f, modal, total, off as usize, theme);
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
                format!("v{}", crate::update::version_string()),
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
    // (step id, label, optional). Required steps are numbered; optional steps
    // (Thai title, synopsis) get a `◦` marker so the skippable ones read apart.
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
            (2, "Thai title", true),
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
    if st.edit_th {
        return &[
            ("type", "thai"),
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
            ("e", "edit th"),
            ("r", "reroll"),
            ("s", "skip"),
        ],
        SynPhase::Done => &[
            ("↵", "save"),
            ("e", "edit th"),
            ("r", "reroll"),
            ("o", "src"),
        ],
        SynPhase::Failed => &[("e", "edit th"), ("r", "retry"), ("o", "src")],
    }
}

/// Footer hints for the wizard's Thai-title step: the user types the Thai title
/// directly, with Tab to translate via the agent.
fn import_title_hints(st: &SynopsisState) -> &'static [(&'static str, &'static str)] {
    if st.phase == SynPhase::Translating {
        return &[("Esc", "cancel"), ("…", "translating")];
    }
    if st.th.trim().is_empty() {
        return &[
            ("↵", "skip"),
            ("type", "thai"),
            ("Tab", "translate"),
            ("Esc", "back"),
        ];
    }
    &[
        ("type", "thai"),
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
        ("type", "thai"),
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

    // While hand-editing the Thai field, the source box is a read-only reference.
    let editing = st.phase == SynPhase::Editing && !st.edit_th;
    let border_color = if st.edit_th || !editing {
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
            "  Thai translation",
            Style::default().fg(theme.ink_soft),
        ))
        .style(Style::default().bg(theme.bg_panel)),
        rows[3],
    );

    // The Thai output gets the same boxed treatment as the source, so the pair
    // reads as siblings; its border accents while it is the field being edited.
    let th_block = Block::default()
        .borders(Borders::ALL)
        .border_set(theme::hairline_set())
        .border_style(Style::default().fg(if st.edit_th {
            theme.accent_soft
        } else {
            theme.rule
        }))
        .style(Style::default().bg(theme.bg_inset));

    if st.edit_th {
        let lines = if st.th.is_empty() {
            vec![Line::from(vec![
                Span::styled(
                    "Type the Thai, or press Tab to translate",
                    Style::default().fg(theme.ink_faint),
                ),
                Span::styled("▏", Style::default().fg(theme.stream_cursor)),
            ])]
        } else {
            caret_text_lines(&st.th, st.th_cursor, theme)
        };
        f.render_widget(
            Paragraph::new(Text::from(lines))
                .wrap(Wrap { trim: false })
                .block(th_block),
            indent(rows[4], 2),
        );
        return;
    }

    let (body, color) = match st.phase {
        SynPhase::Failed => (st.error.clone(), theme.status_failed),
        _ if st.th.trim().is_empty() => (
            "No translation yet — press Tab to translate".to_string(),
            theme.ink_faint,
        ),
        _ => (st.th.clone(), theme.ink),
    };
    f.render_widget(
        Paragraph::new(crate::ui::text::thai_display_safe(&body))
            .wrap(Wrap { trim: false })
            .block(th_block)
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
    if st.edit_th {
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
    if st.edit_th {
        let msg = if st.multiline {
            "  Editing Thai · Tab retranslate · Enter newline · Esc done".to_string()
        } else {
            format!("  Editing Thai · Tab retranslate · Enter {accept_label} · Esc done")
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

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn wizard(projects: Vec<ProjectRef>) -> Overlay {
        Overlay::Import(ImportState::new(
            vec![(PathBuf::from("cursed_blade_v03.epub"), 2_345_678)],
            projects,
        ))
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
    fn project_title_editor_can_hand_edit_then_save() {
        let mut ov = Overlay::project_title_edit("novel".into(), "夜の影".into(), String::new());

        // The editor starts focused on the Thai field — type the name by hand.
        for c in "เงา".chars() {
            ov.handle_key(key(KeyCode::Char(c)));
        }
        if let Overlay::ProjectTitle(st) = &ov {
            assert!(st.syn.edit_th, "starts editing the Thai field");
            assert_eq!(st.syn.th, "เงา");
        } else {
            panic!("overlay changed variant");
        }
        match ov.handle_key(key(KeyCode::Enter)) {
            Action::SaveProjectTitle { id, raw, th } => {
                assert_eq!(id, "novel");
                assert_eq!(raw, "夜の影");
                assert_eq!(th, "เงา", "hand-typed Thai is saved as-is");
            }
            other => panic!("expected SaveProjectTitle, got {other:?}"),
        }
    }

    #[test]
    fn synopsis_editor_can_continue_without_translating() {
        // Typed a source synopsis but don't want to translate: Ctrl+S continues.
        let mut ov = Overlay::synopsis_edit(String::new(), String::new(), 1, "Novel".into());
        for c in "あらすじ".chars() {
            ov.handle_key(key(KeyCode::Char(c)));
        }
        match ov.handle_key(ctrl(KeyCode::Char('s'))) {
            Action::SaveSynopsis { raw, th } => {
                assert_eq!(raw, "あらすじ");
                assert_eq!(th, "", "no translation was forced");
            }
            other => panic!("expected SaveSynopsis, got {other:?}"),
        }
    }

    #[test]
    fn synopsis_editor_can_hand_edit_translation() {
        let mut ov = Overlay::synopsis_edit("源".into(), "เก่า".into(), 1, "Novel".into());
        // 'e' focuses the Thai field for hand-editing (was: edit the source).
        ov.handle_key(key(KeyCode::Char('e')));
        if let Overlay::Synopsis(st) = &ov {
            assert!(st.syn.edit_th, "'e' edits the translation by hand");
        } else {
            panic!("overlay changed variant");
        }
        for c in "ใหม่".chars() {
            ov.handle_key(key(KeyCode::Char(c)));
        }
        ov.handle_key(key(KeyCode::Esc)); // settle the Thai field (multiline)
        match ov.handle_key(key(KeyCode::Enter)) {
            Action::SaveSynopsis { raw, th } => {
                assert_eq!(raw, "源");
                assert_eq!(th, "เก่าใหม่", "hand-edited translation is saved");
            }
            other => panic!("expected SaveSynopsis, got {other:?}"),
        }
    }

    #[test]
    fn project_title_editor_translates_then_saves() {
        let mut ov = Overlay::project_title_edit("novel".into(), "夜の影".into(), String::new());

        // Tab hands the source off to the translator agent.
        match ov.handle_key(key(KeyCode::Tab)) {
            Action::TranslateProjectTitle { ref raw, attempt } => {
                assert_eq!(raw, "夜の影");
                assert_eq!(attempt, 0);
            }
            other => panic!("expected TranslateProjectTitle, got {other:?}"),
        }
        if let Overlay::ProjectTitle(st) = &ov {
            assert_eq!(st.syn.phase, SynPhase::Translating);
        } else {
            panic!("overlay changed variant");
        }

        ov.set_synopsis_result(Ok("เงาแห่งราตรี".into()));
        // The result lands back in the editable Thai field; Enter saves it.
        match ov.handle_key(key(KeyCode::Enter)) {
            Action::SaveProjectTitle { id, raw, th } => {
                assert_eq!(id, "novel");
                assert_eq!(raw, "夜の影");
                assert_eq!(th, "เงาแห่งราตรี");
            }
            other => panic!("expected SaveProjectTitle, got {other:?}"),
        }
    }

    #[test]
    fn project_title_editor_seeds_done_from_stored_translation() {
        let ov = Overlay::project_title_edit("novel".into(), "夜の影".into(), "เงาแห่งราตรี".into());
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
        };
        let mut ov = wizard(vec![existing]);
        if let Overlay::Import(st) = &mut ov {
            st.step = 1;
            st.name = "Cursed Blade".to_string();
            st.name_cursor = st.name.len();
        }
        ov.handle_key(key(KeyCode::Enter)); // → Thai-title step
        ov.handle_key(key(KeyCode::Enter)); // empty Thai title → volume step
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
        };
        let mut ov = wizard(vec![existing]);
        if let Overlay::Import(st) = &mut ov {
            st.step = 1;
            st.name = "Cursed Blade".to_string();
            st.name_cursor = st.name.len();
        }
        ov.handle_key(key(KeyCode::Enter)); // → Thai-title step
        ov.handle_key(key(KeyCode::Enter)); // empty Thai title → volume step, suggested 2
        ov.handle_key(key(KeyCode::Up)); // user picks 3
        ov.handle_key(key(KeyCode::Esc)); // back to Thai title
        ov.handle_key(key(KeyCode::Esc)); // back to name
        ov.handle_key(key(KeyCode::Enter)); // forward again
        ov.handle_key(key(KeyCode::Enter)); // empty Thai title → volume step
        let Overlay::Import(st) = &ov else {
            panic!("overlay changed variant")
        };
        assert_eq!(st.vol, 3, "manual pick must not be re-suggested away");
    }

    /// The Thai-title step translates the typed project name via the title agent
    /// and threads the accepted Thai title into the import action; skipping it
    /// leaves the title empty.
    #[test]
    fn wizard_title_step_threads_thai_title_into_import() {
        let mut ov = wizard(vec![]);
        if let Overlay::Import(st) = &mut ov {
            st.step = 1;
            st.name = "夜の影".to_string();
            st.name_cursor = st.name.len();
        }
        ov.handle_key(key(KeyCode::Enter)); // name → Thai title
        match ov.handle_key(key(KeyCode::Tab)) {
            Action::TranslateProjectTitle { raw, attempt } => {
                assert_eq!(raw, "夜の影");
                assert_eq!(attempt, 0);
            }
            other => panic!("expected TranslateProjectTitle, got {other:?}"),
        }
        ov.set_synopsis_result(Ok("เงาแห่งราตรี".into()));
        ov.handle_key(key(KeyCode::Enter)); // accept → volume
        ov.handle_key(key(KeyCode::Enter)); // volume → synopsis
        match ov.handle_key(key(KeyCode::Tab)) {
            // Empty synopsis + Tab skips it and starts the import.
            Action::ImportFile {
                title, title_th, ..
            } => {
                assert_eq!(title, "夜の影");
                assert_eq!(title_th, "เงาแห่งราตรี");
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
            assert_eq!(st.title_syn.th, "เงาแห่งราตรี", "same name keeps the roll");
            assert_eq!(st.title_syn.phase, SynPhase::Done);
        }
        ov.handle_key(key(KeyCode::Esc));
        ov.handle_key(key(KeyCode::Char('x'))); // edit the name
        ov.handle_key(key(KeyCode::Enter));
        let Overlay::Import(st) = &ov else {
            panic!("overlay changed variant")
        };
        assert!(
            st.title_syn.th.is_empty(),
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
        };
        let mut st = ImportState::new(
            vec![(PathBuf::from("ดาบคำสาป_v01.epub"), 2_345_678)],
            vec![existing],
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

        let st = SettingsState::for_test(17); // Retry attempts
        assert_eq!(st.tab, SettingsTab::Pipeline);

        let mut st = SettingsState::for_test(0);
        st.tab = SettingsTab::Account;
        assert!(!st.tab_has_fields());
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
}
