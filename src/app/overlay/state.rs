//! The state each overlay carries, and the helpers that shape it.
//!
//! Split out because these types are shared rather than owned by one
//! overlay: `SynopsisState` backs three of them, `ProjectRef` two, and the
//! settings working copy is read by the GUI as well. Filing them under
//! whichever overlay happens to render them would have put the shared ones
//! somewhere arbitrary.

use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::export::ExportFormat;
use crate::model::{
    AppConfig, ReleaseChannel, ServiceTier, TargetLanguage, ThemeId, UpdateMode,
};
use crate::theme::ALL_THEMES;
use crate::ui::input::{self, EditOpts, Edited};
use crate::ui::text::thai_display_safe;
use crate::ui::kit::picker::{self, PickerState};

use crate::app::qa;
use crate::app::settings_defs::{self, Group, SField};
use crate::app::{Action, Screen, slugify};
use super::{Overlay, prettify_stem};

/// Zone indices for the choices every confirm dialog offers. Distinct from a
/// modal's close button, which the kit owns.
pub const DIALOG_CANCEL: u32 = 1;
pub const DIALOG_CONFIRM: u32 = 2;
pub const DIALOG_ALTERNATE: u32 = 3;

/// Where the import wizard is.
///
/// Was a bare `u8` compared against literals in twenty-odd places, where
/// nothing said which number meant which step and an off-by-one would simply
/// land somewhere else. `Ord` is derived because the step rail genuinely asks
/// "is this step behind the one we are on".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ImportStep {
    /// Choose a source file.
    Pick,
    /// Name the project.
    Name,
    /// Optional: translate the title.
    Title,
    /// Choose the volume number.
    Volume,
    /// Optional: write and translate a synopsis.
    Synopsis,
    /// Running; progress only.
    Importing,
}

impl ImportStep {
    /// The steps in order, for the rail.
    pub const ALL: [ImportStep; 6] = [
        ImportStep::Pick,
        ImportStep::Name,
        ImportStep::Title,
        ImportStep::Volume,
        ImportStep::Synopsis,
        ImportStep::Importing,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ImportStep::Pick => "File",
            ImportStep::Name => "Name",
            ImportStep::Title => "Title",
            ImportStep::Volume => "Volume",
            ImportStep::Synopsis => "Synopsis",
            ImportStep::Importing => "Import",
        }
    }

    /// Steps the wizard will happily skip past.
    pub fn is_optional(self) -> bool {
        matches!(self, ImportStep::Title | ImportStep::Synopsis)
    }
}

/// The four ways into the app from the first-run menu, with a note on each.
///
/// Built from live state, so the sample row says whether one already exists and
/// the key row says whether translation is actually available yet.
pub(super) fn welcome_items(st: &WelcomeState) -> [(&'static str, &'static str); WELCOME_ITEMS] {
    [
        (
            if st.sample_exists {
                "Open the sample project"
            } else {
                "Create a sample project"
            },
            "explore offline, no API key needed",
        ),
        ("Import a file", "EPUB · PDF · HTML · Markdown"),
        (
            "Set OpenRouter API key",
            if st.api_key_present {
                "already configured"
            } else {
                "needed to translate"
            },
        ),
        ("Skip", "explore on my own"),
    ]
}

/// A byte count at human scale, to one decimal above a kilobyte.
pub(super) fn human_size(bytes: u64) -> String {
    pub(super) const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// A row of the QA list: either a chapter heading or one finding.
pub(super) enum QaRow {
    Heading(String),
    Issue(usize),
}

/// Lay findings out with a heading wherever the chapter changes.
///
/// Shared by the renderer and the click handler on purpose: the mapping from a
/// clicked row back to the finding it stands for is only correct while both
/// agree about where the headings went.
pub(super) fn qa_rows(report: &qa::QaReport) -> Vec<QaRow> {
    let mut rows = Vec::with_capacity(report.issues.len() + 4);
    let mut last: Option<u32> = None;
    for (i, issue) in report.issues.iter().enumerate() {
        if issue.chapter != last || i == 0 {
            last = issue.chapter;
            rows.push(QaRow::Heading(match issue.chapter {
                Some(c) => format!("ch.{c:03}  {}", thai_display_safe(&issue.title)),
                None => "unanchored".to_string(),
            }));
        }
        rows.push(QaRow::Issue(i));
    }
    rows
}

/// One line of the keybinding reference.
pub enum HelpRow {
    Section(&'static str),
    Binding(String, String),
    Blank,
}

/// The keybinding reference, built from the binding table.
///
/// Help used to carry its own copy of every binding. It reads the one table
/// now, so a key documented in the command bar and a key documented here
/// cannot disagree.
pub fn help_rows() -> Vec<HelpRow> {
    let mut rows = Vec::new();
    for (n, scope) in crate::app::bindings::SECTIONS.iter().enumerate() {
        if n > 0 {
            rows.push(HelpRow::Blank);
        }
        rows.push(HelpRow::Section(scope.title()));
        for b in crate::app::bindings::in_scope(*scope) {
            rows.push(HelpRow::Binding(b.keys.clone(), b.what.clone()));
        }
    }
    rows
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
pub(super) enum SynKey {
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
pub(super) fn handle_synopsis_keys(st: &mut SynopsisState, key: KeyEvent) -> SynKey {
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
    pub step: ImportStep,
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
    /// The steps this flow actually visits.
    ///
    /// The three entry points take different routes: a plain import walks
    /// everything, "add volume" already knows the project so it skips naming,
    /// and "add chapters" only needs the file. Showing steps the wizard will
    /// never reach would misreport how far along you are.
    pub fn visible_steps(&self) -> Vec<ImportStep> {
        if self.append_to.is_some() {
            return vec![ImportStep::Pick, ImportStep::Importing];
        }
        if self.lock_name {
            return vec![
                ImportStep::Pick,
                ImportStep::Volume,
                ImportStep::Synopsis,
                ImportStep::Importing,
            ];
        }
        ImportStep::ALL.to_vec()
    }

    pub(super) fn new(
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
            step: ImportStep::Pick,
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
    pub(super) fn new_append(
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
    pub(super) fn new_into(
        files: Vec<(PathBuf, u64)>,
        projects: Vec<ProjectRef>,
        title: String,
        vol: u32,
        target_language: TargetLanguage,
    ) -> Self {
        Self {
            step: ImportStep::Pick,
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
    pub(super) fn new(files: Vec<(PathBuf, u64)>, vol: u32) -> Self {
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

/// Fallback Codex model ids until the live list arrives.
pub(super) const CODEX_MODELS: [&str; 3] = ["gpt-5.5", "gpt-5.4", "gpt-5.4-mini"];

pub(super) fn default_codex_models() -> Vec<String> {
    CODEX_MODELS.iter().map(|s| s.to_string()).collect()
}

pub(super) fn provider_model_fallback(
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
///
/// A constant because the GUI needs one; a test pins it to
/// [`settings_defs::key_field`] so inserting a row above it cannot quietly
/// point this somewhere else.
pub const SETTINGS_KEY_FIELD: u8 = 12;

/// Number of focusable settings rows.
pub(super) fn settings_fields() -> u8 {
    settings_defs::count()
}

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

    /// The registry group this tab shows.
    pub(super) fn group(self) -> Group {
        match self {
            SettingsTab::Agents => Group::Agents,
            SettingsTab::Providers => Group::Providers,
            SettingsTab::Pipeline => Group::Pipeline,
            SettingsTab::Appearance => Group::Appearance,
            SettingsTab::Account => Group::Account,
        }
    }

    pub(super) fn of_group(group: Group) -> SettingsTab {
        match group {
            Group::Agents => SettingsTab::Agents,
            Group::Providers => SettingsTab::Providers,
            Group::Pipeline => SettingsTab::Pipeline,
            Group::Appearance => SettingsTab::Appearance,
            Group::Account => SettingsTab::Account,
        }
    }

    /// Focusable field range; Account has none.
    ///
    /// Derived from the registry rather than written down. The ranges used to
    /// be literals here, which made them a second copy of the declaration
    /// order — inserting a setting shifted every row past it and left this
    /// pointing at the wrong tab.
    pub(super) fn field_range(self) -> Option<(u8, u8)> {
        let fields = self.group().fields();
        let first = *fields.first()?;
        let last = *fields.last()?;
        Some((first, last + 1))
    }

    pub(super) fn for_field(field: u8) -> SettingsTab {
        SettingsTab::of_group(Group::of_field(field))
    }

    pub(super) fn cycled(self, forward: bool) -> SettingsTab {
        SettingsTab::of_group(self.group().cycled(forward))
    }
}


/// Step an index forward/backward through a wrapped cycle of `len` items.
pub(super) fn step(i: usize, len: usize, forward: bool) -> usize {
    if forward {
        (i + 1) % len
    } else {
        (i + len - 1) % len
    }
}

/// The Account section's controls. They ride [`crate::ui::kit::ZoneKind::Action`]
/// because the form has already claimed `Button` for its stepper arrows.
pub(super) const ACCOUNT_GITHUB: u16 = 0;
pub(super) const ACCOUNT_CODEX: u16 = 1;
pub(super) const ACCOUNT_REMOTE: u16 = 2;

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
    /// Theme being previewed. Live from the moment it is cycled; only written
    /// to config on save, and put back by `CloseOverlay` otherwise.
    pub theme: ThemeId,
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
    /// HTTP send attempts per call, as typed (digits only).
    pub retry_attempts: String,
    /// Cap on one backoff in seconds, as typed (digits only).
    pub retry_cooldown_secs: String,
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
    /// Chunk sizing, edited as text like the other numeric rows.
    pub chunk_target_tokens: String,
    pub chunk_hard_cap_tokens: String,
    /// Seed characters and terms from the raw text before translating.
    pub prepass_extract: bool,
    /// Re-read each finished chapter end to end and flag drift.
    pub coherence_check: bool,
    pub tab: SettingsTab,
    /// Which field is focused (index into [`settings_defs::ORDER`]).
    pub field: u8,
    /// Caret byte-offset into the focused text field. Secret fields edit at the end.
    pub cursor: usize,
    /// Codex model picker options.
    pub codex_models: Vec<String>,
    pub account_login: Option<String>,
    /// ChatGPT account id when signed in to Codex, so the Account section can
    /// say so. `Some("")` still means signed in — an older token carries no id.
    pub codex_account: Option<String>,
    pub remote_enabled: bool,
    pub remote_state: crate::remote::protocol::RemoteState,
    pub remote_watchers: u32,
    pub remote_auth_code: Option<crate::model::AuthCodePrompt>,
    /// Dashboard label for the live remote session.
    pub session_label: Option<String>,
}

impl SettingsState {
    pub(super) fn from_cfg_focus(cfg: &AppConfig, field: u8) -> Self {
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
            chunk_target_tokens: cfg.chunk_target_tokens.to_string(),
            chunk_hard_cap_tokens: cfg.chunk_hard_cap_tokens.to_string(),
            prepass_extract: cfg.prepass_extract,
            coherence_check: cfg.coherence_check,
            preferred_language: cfg.preferred_language,
            max_attempts: cfg.max_attempts.to_string(),
            retry_attempts: cfg.retry_attempts.to_string(),
            retry_cooldown_secs: cfg.retry_cooldown_secs.to_string(),
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
            theme: cfg.theme,
            account_login: cfg.account.as_ref().map(|a| a.github_login.clone()),
            codex_account: cfg.codex_auth.as_ref().map(|a| a.account_id.clone()),
            // App syncs live remote values after opening Settings.
            remote_enabled: false,
            remote_state: crate::remote::protocol::RemoteState::Disconnected,
            remote_watchers: 0,
            remote_auth_code: None,
            session_label: None,
        };
        st.focus(field.min(settings_fields() - 1));
        st.tab = SettingsTab::for_field(st.field);
        st
    }

    /// A default-config Settings state focused on `field` (tests only).
    #[cfg(test)]
    pub fn for_test(field: u8) -> Self {
        Self::from_cfg_focus(&AppConfig::default(), field)
    }

    /// The currently focused field.
    pub(super) fn current(&self) -> SField {
        settings_defs::ORDER[self.field as usize].field
    }

    /// Mutable handle to the focused text buffer (None for cycle fields).
    pub(super) fn text_field_mut(&mut self) -> Option<&mut String> {
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
            SField::RetryAttempts => &mut self.retry_attempts,
            SField::RetryCooldown => &mut self.retry_cooldown_secs,
            SField::ContinuitySentences => &mut self.continuity_sentences,
            SField::LoopStall => &mut self.loop_stall_secs,
            SField::Retranslates => &mut self.max_chapter_retranslates,
            SField::ChunkTargetTokens => &mut self.chunk_target_tokens,
            SField::ChunkHardCapTokens => &mut self.chunk_hard_cap_tokens,
            SField::GateModel => &mut self.system_one.model,
            SField::GateKey => &mut self.typesafe_key,
            SField::GateConfidence => &mut self.system_one_confidence,
            _ => return None,
        })
    }

    /// The working agent for an agent-row field, if this field belongs to one.
    pub(super) fn agent_for(&mut self, field: SField) -> Option<&mut crate::model::AgentModel> {
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
    pub(super) fn agent_provider(&self) -> Option<crate::model::Provider> {
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
    pub(super) fn is_codex_model(&self) -> bool {
        matches!(
            self.current(),
            SField::OrchModel | SField::TransModel | SField::ReviewModel | SField::RefineModel
        ) && self.agent_provider() == Some(crate::model::Provider::Codex)
    }

    /// Whether the focused field accepts typed text (vs. a Left/Right choice).
    pub(super) fn current_is_editable_text(&self) -> bool {
        self.current().is_text() && !self.is_codex_model()
    }

    /// Cycle the focused non-text field. `forward` is Right/Space; `false` is Left.
    pub(super) fn cycle(&mut self, forward: bool) {
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
            SField::PrepassExtract => self.prepass_extract = !self.prepass_extract,
            SField::CoherenceCheck => self.coherence_check = !self.coherence_check,
            SField::SystemOneEnabled => self.system_one.enabled = !self.system_one.enabled,
            SField::GateMode => {
                self.system_one.review_gate = self.system_one.review_gate.cycled(forward)
            }
            SField::GateProvider => {
                let next = self.system_one.provider.cycled(forward);
                self.system_one.switch_provider(next);
            }
            f if settings_defs::feature_of(f).is_some() => {
                let feature = settings_defs::feature_of(f).expect("guarded by the match arm");
                let slot = self.system_one.feature_mut(feature);
                *slot = !*slot;
            }
            SField::Theme => {
                let i = self.theme.index();
                let n = ALL_THEMES.len();
                self.theme = ALL_THEMES[step(i, n, forward)];
            }
            SField::UpdateModeField => self.update_mode = self.update_mode.toggled(),
            SField::ReleaseChannelField => self.release_channel = self.release_channel.toggled(),
            _ => {}
        }
    }

    /// Focus a field and drop the caret at its end.
    pub(super) fn focus(&mut self, field: u8) {
        self.field = field % settings_fields();
        self.cursor = self.text_field_mut().map(|s| s.len()).unwrap_or(0);
    }

    pub(super) fn next_field(&mut self) {
        if let Some((start, end)) = self.tab.field_range() {
            let next = if self.field + 1 >= end {
                start
            } else {
                self.field + 1
            };
            self.focus(next);
        }
    }

    pub(super) fn prev_field(&mut self) {
        if let Some((start, end)) = self.tab.field_range() {
            let prev = if self.field <= start {
                end - 1
            } else {
                self.field - 1
            };
            self.focus(prev);
        }
    }

    pub(super) fn switch_tab(&mut self, forward: bool) {
        self.tab = self.tab.cycled(forward);
        if let Some((start, _)) = self.tab.field_range() {
            self.focus(start);
        }
    }

    /// Step the focused numeric row by one, clamped to the range the registry
    /// declares.
    ///
    /// Numeric rows are typed into, so `cycle` leaves them alone — but the form
    /// draws stepper arrows beside them, and an arrow that does nothing is
    /// worse than no arrow. Returns false when the focused row is not numeric.
    pub(super) fn step_number(&mut self, up: bool) -> bool {
        let Some(def) = settings_defs::at(self.field) else {
            return false;
        };
        let settings_defs::Kind::Number { min, max } = def.kind else {
            return false;
        };
        let Some(slot) = self.text_field_mut() else {
            return false;
        };
        let current = slot.trim().parse::<i64>().unwrap_or(min);
        let next = if up { current + 1 } else { current - 1 }.clamp(min, max);
        *slot = next.to_string();
        true
    }

    /// Go straight to `tab`, as clicking its rail entry does. Focus lands on
    /// that section's first row, which is where Tab would leave it too.
    pub(super) fn select_tab(&mut self, tab: SettingsTab) {
        self.tab = tab;
        if let Some((start, _)) = self.tab.field_range() {
            self.focus(start);
        }
    }

    pub(super) fn tab_has_fields(&self) -> bool {
        self.tab.field_range().is_some()
    }

    /// The retries field parsed into a usable attempt count. Empty, non-numeric,
    /// or 0 falls back to 1; values are capped at 20 to keep the loop bounded.
    pub(super) fn max_attempts_value(&self) -> u32 {
        self.max_attempts
            .trim()
            .parse::<u32>()
            .unwrap_or(0)
            .clamp(1, 20)
    }

    /// Clamped to the range the registry advertises, so a typo cannot disable
    /// retrying by accident or let one call wait for minutes.
    pub(super) fn retry_attempts_value(&self) -> u32 {
        self.retry_attempts
            .trim()
            .parse::<u32>()
            .unwrap_or(3)
            .clamp(1, 10)
    }

    pub(super) fn retry_cooldown_value(&self) -> u64 {
        self.retry_cooldown_secs
            .trim()
            .parse::<u64>()
            .unwrap_or(20)
            .clamp(1, 120)
    }

    /// Target chunk size. Clamped to the range the registry advertises, so a
    /// typo cannot produce a chunk no provider will accept.
    pub(super) fn chunk_target_tokens_value(&self) -> usize {
        self.chunk_target_tokens
            .trim()
            .parse::<usize>()
            .unwrap_or(1000)
            .clamp(200, 8000)
    }

    /// Hard cap. Never below the target, whatever was typed — a cap under the
    /// target would silently make the target unreachable.
    pub(super) fn chunk_hard_cap_tokens_value(&self) -> usize {
        self.chunk_hard_cap_tokens
            .trim()
            .parse::<usize>()
            .unwrap_or(1200)
            .clamp(200, 16000)
            .max(self.chunk_target_tokens_value())
    }

    /// Prior translated sentences included with each chunk (0 disables it).
    /// Empty or invalid input falls back to the default; capped at 100.
    pub(super) fn continuity_sentences_value(&self) -> usize {
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
    pub(super) fn loop_stall_secs_value(&self) -> u64 {
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
    pub(super) fn max_chapter_retranslates_value(&self) -> u32 {
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
            theme: self.theme,
            update_mode: self.update_mode,
            release_channel: self.release_channel,
            service_tier: self.service_tier,
            preferred_language: self.preferred_language,
            max_attempts: self.max_attempts_value(),
            retry_attempts: self.retry_attempts_value(),
            retry_cooldown_secs: self.retry_cooldown_value(),
            continuity_sentences: self.continuity_sentences_value(),
            loop_stall_secs: self.loop_stall_secs_value(),
            max_chapter_retranslates: self.max_chapter_retranslates_value(),
            parallel_lookahead: self.parallel_lookahead,
            chunk_target_tokens: self.chunk_target_tokens_value(),
            chunk_hard_cap_tokens: self.chunk_hard_cap_tokens_value(),
            prepass_extract: self.prepass_extract,
            coherence_check: self.coherence_check,
            system_one: Box::new(self.system_one()),
            typesafe_key: (!self.typesafe_key_env).then(|| self.typesafe_key.clone()),
        }
    }

    /// The working System One config assembled from the edited fields.
    pub(super) fn system_one(&self) -> crate::model::SystemOne {
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
    pub(super) fn new(current: ThemeId) -> Self {
        Self {
            sel: current.index(),
        }
    }

    pub(super) fn current(&self) -> ThemeId {
        ALL_THEMES.get(self.sel).copied().unwrap_or_default()
    }
}

/// Command palette: a fuzzy-ish filtered list of navigation commands.
#[derive(Debug, Clone)]
pub struct PaletteState {
    pub items: Vec<PaletteItem>,
    /// Query, caret and selection all live in the picker, which is also what
    /// filters and draws them.
    pub picker: PickerState,
}

#[derive(Debug, Clone)]
pub struct PaletteItem {
    /// Owned, because most of what belongs here is named by the project — a
    /// chapter, a conversation — not by a literal in this file.
    pub label: String,
    pub action: Action,
}

impl PaletteItem {
    pub fn new(label: impl Into<String>, action: Action) -> Self {
        Self {
            label: label.into(),
            action,
        }
    }
}

impl PaletteState {
    pub(super) fn new() -> Self {
        let items = vec![
            PaletteItem {
                label: "Getting started".to_string(),
                action: Action::show_overlay(Overlay::welcome_placeholder()),
            },
            PaletteItem {
                label: "Go: Shelf".to_string(),
                action: Action::Goto(Screen::Shelf),
            },
            PaletteItem {
                label: "Go: Project".to_string(),
                action: Action::Goto(Screen::Project),
            },
            PaletteItem {
                label: "Go: Translate".to_string(),
                action: Action::Goto(Screen::Translate),
            },
            PaletteItem {
                label: "Translate whole project".to_string(),
                action: Action::StartProjectTranslation,
            },
            PaletteItem {
                label: "Go: Reader".to_string(),
                action: Action::Goto(Screen::Reader),
            },
            PaletteItem {
                label: "Go: Lexicon".to_string(),
                action: Action::Goto(Screen::Lexicon),
            },
            PaletteItem {
                label: "Go: Refine".to_string(),
                action: Action::Goto(Screen::Refine),
            },
            PaletteItem {
                label: "Settings".to_string(),
                action: Action::show_overlay(Overlay::settings_placeholder()),
            },
            PaletteItem {
                label: "Theme".to_string(),
                action: Action::show_overlay(Overlay::theme_placeholder()),
            },
            PaletteItem {
                label: "Help".to_string(),
                action: Action::show_overlay(Overlay::Help(0)),
            },
            PaletteItem {
                label: "About".to_string(),
                action: Action::show_overlay(Overlay::About),
            },
            PaletteItem {
                label: "QA review".to_string(),
                action: Action::show_overlay(Overlay::qa_placeholder()),
            },
            PaletteItem {
                label: "Activity log".to_string(),
                action: Action::show_overlay(Overlay::Log(0)),
            },
        ];
        Self {
            items,
            picker: PickerState::new(),
        }
    }

    /// Indices of items matching the current query, best match first.
    ///
    /// Subsequence-matched rather than substring, so "gorf" reaches
    /// "Go: Refine" — which is the difference between the command bar being a
    /// filter and being something you can actually aim.
    pub fn matches(&self) -> Vec<usize> {
        picker::filter(&self.picker.query, &self.picker_items())
    }

    /// What the picker matches against.
    pub fn picker_items(&self) -> Vec<picker::Item> {
        self.items
            .iter()
            .map(|i| picker::Item::new(i.label.clone()))
            .collect()
    }

    /// Add what the project itself offers, after the fixed commands.
    pub fn extend(&mut self, items: impl IntoIterator<Item = PaletteItem>) {
        self.items.extend(items);
    }

    /// Which match the cursor is on.
    pub fn sel(&self) -> usize {
        self.picker.list.selected().unwrap_or(0)
    }

    pub fn select(&mut self, index: usize) {
        self.picker.list.select(Some(index));
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
    pub items: Vec<JumpTarget>,
    pub picker: PickerState,
}

impl ReaderJumpState {
    /// What the picker matches against: the label, plus the kind and chapter as
    /// a searchable detail, so "bookmark" or "ch.012" narrows the list.
    pub fn picker_items(&self) -> Vec<picker::Item> {
        self.items
            .iter()
            .map(|t| {
                let kind = match t.kind {
                    JumpKind::Chapter => "chapter",
                    JumpKind::Section => "section",
                    JumpKind::Bookmark => "bookmark",
                };
                picker::Item::new(thai_display_safe(&t.label))
                    .detail(format!("{kind} · ch.{:03}", t.chapter))
            })
            .collect()
    }

    /// Indices of items matching the current query, best match first.
    pub fn matches(&self) -> Vec<usize> {
        picker::filter(&self.picker.query, &self.picker_items())
    }

    /// Which match the cursor is on.
    pub fn sel(&self) -> usize {
        self.picker.list.selected().unwrap_or(0)
    }

    pub fn select(&mut self, index: usize) {
        self.picker.list.select(Some(index));
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
pub(super) const WELCOME_ITEMS: usize = 4;

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
    pub(super) fn new(vol: u32) -> Self {
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
    pub(super) fn selected_formats(&self) -> Vec<ExportFormat> {
        ExportFormat::ALL
            .iter()
            .zip(self.formats)
            .filter_map(|(f, on)| on.then_some(*f))
            .collect()
    }
}

