//! Transient overlays drawn last over a `Clear`: import, settings, logs, help,
//! palette, confirm dialogs, and editors.
//!
//! `is_input_capturing()` suppresses single-letter globals while text input has focus.

use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::export::ExportFormat;
use crate::model::{
    AppConfig, LogLevel, ReleaseChannel, ServiceTier, TargetLanguage, ThemeId, UpdateMode,
};
use crate::theme::{ALL_THEMES, Theme};
use crate::ui::input::{self, EditOpts, Edited};
use crate::ui::mouse::{MouseGesture, MouseInput};
use crate::ui::kit::{ZoneId, ZoneKind};
use crate::ui::text::{col_width, pad_to_cols, thai_display_safe, truncate_cols};

use super::qa;
use super::settings_defs::{self, Group, SField};
use super::Action;

mod state;

pub use state::*;

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
            chunk_target_tokens: String::new(),
            chunk_hard_cap_tokens: String::new(),
            prepass_extract: true,
            coherence_check: true,
            preferred_language: TargetLanguage::default(),
            max_attempts: String::new(),
            continuity_sentences: String::new(),
            loop_stall_secs: String::new(),
            max_chapter_retranslates: String::new(),
            tab: SettingsTab::for_field(field.min(settings_fields() - 1)),
            field: field.min(settings_fields() - 1),
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
            st.step = ImportStep::Importing;
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
            Overlay::Import(s) if s.step == ImportStep::Title => &mut s.title_syn,
            Overlay::Import(s) if s.step == ImportStep::Synopsis => &mut s.syn,
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
                st.step == ImportStep::Name
                    || (st.step == ImportStep::Title && st.title_syn.edit_translation)
                    || (st.step == ImportStep::Synopsis
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

    /// Whether this overlay binds Tab itself.
    ///
    /// Tab is focus traversal everywhere else, but the translate-and-accept
    /// editors and the import wizard have bound it to "translate this" for
    /// long enough that taking it away would be the more surprising change.
    /// Their footers now carry a Translate button too, so the binding is a
    /// shortcut rather than the only way through.
    pub fn uses_tab(&self) -> bool {
        matches!(
            self,
            Overlay::Synopsis(_) | Overlay::ProjectTitle(_) | Overlay::Import(_)
        )
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
            // A picker row: move the selection there, then activate through the
            // same Enter the keyboard uses. Clicking an already-selected row
            // activates immediately, since pointing at it twice is a clear
            // enough statement of intent.
            Overlay::Palette(st) => match id.kind {
                ZoneKind::Row if (id.index as usize) < st.matches().len() => {
                    let already = st.sel == id.index as usize;
                    st.sel = id.index as usize;
                    if double || already {
                        self.handle_key(synth(KeyCode::Enter))
                    } else {
                        Action::None
                    }
                }
                _ => Action::None,
            },
            Overlay::ReaderJump(st) => match id.kind {
                ZoneKind::Row if (id.index as usize) < st.matches().len() => {
                    let already = st.sel == id.index as usize;
                    st.sel = id.index as usize;
                    if double || already {
                        self.handle_key(synth(KeyCode::Enter))
                    } else {
                        Action::None
                    }
                }
                _ => Action::None,
            },
            Overlay::Welcome(st) => match id.kind {
                ZoneKind::Row if (id.index as usize) < WELCOME_ITEMS => {
                    let already = st.sel == id.index as usize;
                    st.sel = id.index as usize;
                    if double || already {
                        self.handle_key(synth(KeyCode::Enter))
                    } else {
                        Action::None
                    }
                }
                _ => Action::None,
            },
            Overlay::ImageSource(st) => match id.kind {
                ZoneKind::Row if (id.index as usize) < st.files.len() => {
                    let already = st.sel == id.index as usize;
                    st.sel = id.index as usize;
                    if double || already {
                        self.handle_key(synth(KeyCode::Enter))
                    } else {
                        Action::None
                    }
                }
                _ => Action::None,
            },
            // A clicked QA row is a row index, and rows include the chapter
            // headings; `qa_rows` is what makes the mapping back to a finding
            // the same one the renderer used.
            Overlay::Qa(st) => match id.kind {
                ZoneKind::Row => {
                    let rows = qa_rows(&st.report);
                    match rows.get(id.index as usize) {
                        Some(QaRow::Issue(n)) => {
                            let already = st.sel == *n;
                            st.sel = *n;
                            if double || already {
                                self.handle_key(synth(KeyCode::Enter))
                            } else {
                                Action::None
                            }
                        }
                        _ => Action::None,
                    }
                }
                _ => Action::None,
            },
            // Both translate-and-accept editors share one button row.
            Overlay::Synopsis(_) | Overlay::ProjectTitle(_) => match (id.kind, id.index) {
                (ZoneKind::Button, DIALOG_CONFIRM) => self.handle_key(synth(KeyCode::Enter)),
                (ZoneKind::Button, DIALOG_CANCEL) => self.handle_key(synth(KeyCode::Esc)),
                (ZoneKind::Button, DIALOG_ALTERNATE) => self.handle_key(synth(KeyCode::Tab)),
                _ => Action::None,
            },
            Overlay::ReaderEdit(_) => match (id.kind, id.index) {
                (ZoneKind::Button, DIALOG_CONFIRM) => {
                    self.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
                }
                (ZoneKind::Button, DIALOG_CANCEL) => self.handle_key(synth(KeyCode::Esc)),
                _ => Action::None,
            },
                        Overlay::Import(st) => match id.kind {
                // Only completed steps register, so this can only ever go back.
                ZoneKind::Step => {
                    let visible = st.visible_steps();
                    if let Some(&target) = visible.get(id.index as usize)
                        && target < st.step
                    {
                        st.step = target;
                    }
                    Action::None
                }
                ZoneKind::Row if (id.index as usize) < st.files.len() => {
                    let already = st.sel == id.index as usize;
                    st.sel = id.index as usize;
                    if double || already {
                        self.handle_key(synth(KeyCode::Enter))
                    } else {
                        Action::None
                    }
                }
                ZoneKind::Button => match id.index {
                    DIALOG_CONFIRM => self.handle_key(synth(KeyCode::Enter)),
                    DIALOG_CANCEL => self.handle_key(synth(KeyCode::Esc)),
                    DIALOG_ALTERNATE => self.handle_key(synth(KeyCode::Tab)),
                    _ => Action::None,
                },
                _ => Action::None,
            },
                        Overlay::ReaderNote(_) => match (id.kind, id.index) {
                (ZoneKind::Button, DIALOG_CONFIRM) => self.handle_key(synth(KeyCode::Enter)),
                (ZoneKind::Button, DIALOG_CANCEL) => self.handle_key(synth(KeyCode::Esc)),
                _ => Action::None,
            },
            // Help, About, the Log and Reader search are read-only or
            // single-field: the frame behaviour above already covers them.
            _ => Action::None,
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
                2 => Action::show_overlay(Overlay::settings_at(settings_defs::key_field())),
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
            ImportStep::Pick => match key.code {
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
                        st.step = ImportStep::Importing;
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
                            st.step = ImportStep::Volume;
                        } else {
                            st.step = ImportStep::Name;
                        }
                        Action::None
                    }
                }
                _ => Action::None,
            },
            ImportStep::Name => {
                if input::handle(&mut st.name, &mut st.name_cursor, key, EditOpts::default())
                    != Edited::Ignored
                {
                    st.name_touched = true;
                    st.note = None;
                    return Action::None;
                }
                match key.code {
                    KeyCode::Esc => {
                        st.step = ImportStep::Pick;
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
                            st.step = ImportStep::Title;
                        }
                        Action::None
                    }
                    _ => Action::None,
                }
            }
            ImportStep::Title => {
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
                        st.step = ImportStep::Volume;
                        st.suggest_volume();
                        Action::None
                    }
                    SynKey::Back => {
                        st.step = ImportStep::Name;
                        st.name_cursor = st.name.len();
                        Action::None
                    }
                }
            }
            ImportStep::Volume => match key.code {
                KeyCode::Esc => {
                    st.step = if st.lock_name { ImportStep::Pick } else { ImportStep::Title };
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
                    st.step = ImportStep::Synopsis;
                    Action::None
                }
                _ => Action::None,
            },
            ImportStep::Synopsis => {
                let intent = handle_synopsis_keys(&mut st.syn, key);
                match intent {
                    SynKey::None => Action::None,
                    SynKey::Translate => Action::TranslateSynopsis {
                        raw: st.syn.raw.clone(),
                        attempt: st.syn.attempt,
                        target_language: st.effective_target_language(),
                    },
                    SynKey::Back => {
                        st.step = ImportStep::Volume;
                        Action::None
                    }
                    SynKey::Accept => {
                        let source = st.selected_file().cloned().unwrap_or_default();
                        let title = st.name.trim().to_string();
                        let translated_title = st.title_syn.translated_text.trim().to_string();
                        let vol = st.vol.max(1);
                        let synopsis_raw = st.syn.raw.trim().to_string();
                        let translated_synopsis = st.syn.translated_text.trim().to_string();
                        st.step = ImportStep::Importing;
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
                        st.step = ImportStep::Importing;
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
                ImportStep::Pick => &[
                    ("↑↓", "pick"),
                    ("←→", "language"),
                    ("↵", "next"),
                    ("r", "rescan"),
                    ("Esc", "cancel"),
                ],
                ImportStep::Name => &[("type", "name"), ("↵/Tab", "next"), ("Esc", "back")],
                ImportStep::Title => import_title_hints(&st.title_syn),
                ImportStep::Volume => &[("↑↓/type", "volume"), ("↵", "next"), ("Esc", "back")],
                ImportStep::Synopsis => synopsis_hints(&st.syn, true),
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

            Overlay::Welcome(st) => self.render_welcome_kit(ui, area, st),
            Overlay::Import(st) => self.render_import_kit(ui, area, st),
            Overlay::ImageSource(st) => self.render_image_source_kit(ui, area, st),
            Overlay::Settings(st) => self.render_settings_kit(ui, area, cfg, st),
            Overlay::Palette(st) => self.render_palette_kit(ui, area, st),
            Overlay::Synopsis(st) => self.render_synopsis_kit(ui, area, st),
            Overlay::ProjectTitle(st) => self.render_project_title_kit(ui, area, st),
            Overlay::Qa(st) => self.render_qa_kit(ui, area, st),
            Overlay::ReaderNote(st) => self.render_reader_note_kit(ui, area, st),
            Overlay::ReaderInspect(st) => self.render_reader_inspect_kit(ui, area, st),
            Overlay::ReaderEdit(st) => self.render_reader_edit_kit(ui, area, st),
            Overlay::ReaderSearch(st) => self.render_reader_search_kit(ui, area, st),
            Overlay::ReaderJump(st) => self.render_reader_jump_kit(ui, area, st),
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
        // The key column is as wide as the widest key plus a gutter, rather
        // than a guess: ": / Ctrl-P / Ctrl-K" overran a hardcoded 18 and ran
        // into its own description.
        let key_cols = rows
            .iter()
            .filter_map(|r| match r {
                HelpRow::Binding(k, _) => Some(col_width(k)),
                _ => None,
            })
            .max()
            .unwrap_or(16)
            + 4;
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
                    Span::styled(pad_to_cols(k, key_cols), key),
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

    /// A query line over a ranked list — the shape the command bar and the
    /// Reader's jump list both are.
    ///
    /// `rows` is already filtered and ranked; this only draws it. Matched
    /// characters are lifted so it is visible *why* a row ranked where it did.
    fn render_query_list(
        &self,
        ui: &mut crate::ui::kit::Ui,
        body: Rect,
        query: &str,
        cursor: usize,
        sel: usize,
        rows: &[(String, Option<String>)],
    ) {
        use crate::ui::kit::list::{self, ListState, Row};

        let bg = ui.theme.bg_elevated;
        let prompt = Style::default()
            .fg(ui.theme.accent)
            .bg(bg)
            .add_modifier(Modifier::BOLD);
        let typed = Style::default().fg(ui.theme.ink).bg(bg);
        let caret = Style::default().fg(ui.theme.stream_cursor).bg(bg);

        // Query line.
        let (before, after) = input::caret_halves(
            query,
            cursor,
            body.width.saturating_sub(4) as usize,
        );
        ui.line(
            crate::ui::kit::ctx::row_at(body, 0),
            Line::from(vec![
                Span::styled(
                    format!("{} ", crate::ui::glyphs::CHEVRON_RIGHT.as_str()),
                    prompt,
                ),
                Span::styled(before, typed),
                Span::styled(crate::ui::glyphs::ACCENT_RAIL.as_str().to_string(), caret),
                Span::styled(after, typed),
            ]),
            Style::default().bg(bg),
        );

        let list_area = Rect {
            y: body.y + 1,
            height: body.height.saturating_sub(1),
            ..body
        };
        if rows.is_empty() {
            ui.text(
                crate::ui::kit::ctx::row_at(list_area, 0),
                "no matches",
                Style::default().fg(ui.theme.ink_faint).bg(bg),
            );
            return;
        }

        let hit = Style::default()
            .fg(ui.theme.accent)
            .add_modifier(Modifier::BOLD);
        let plain = Style::default().fg(ui.theme.ink);
        let dim = Style::default().fg(ui.theme.ink_faint);
        let owned_query = query.to_string();

        let mut st = ListState::new();
        st.select(Some(sel.min(rows.len().saturating_sub(1))));
        list::render(
            ui,
            list_area,
            &mut st,
            rows.len(),
            list::Opts {
                rail: true,
                scrollbar: true,
                kind: ZoneKind::Row,
                id_base: 0,
            },
            |i| {
                let (label, detail) = &rows[i];
                let positions = crate::ui::kit::picker::score(&owned_query, label)
                    .map(|(_, p)| p)
                    .unwrap_or_default();
                let mut spans: Vec<Span<'static>> = label
                    .chars()
                    .enumerate()
                    .map(|(n, ch)| {
                        let style = if positions.contains(&n) { hit } else { plain };
                        Span::styled(ch.to_string(), style)
                    })
                    .collect();
                if let Some(d) = detail {
                    spans.push(Span::styled(format!("   {d}"), dim));
                }
                Row::new(Line::from(spans))
            },
        );
    }

    /// The command bar: every action that has a name, one search away.
    fn render_palette_kit(&self, ui: &mut crate::ui::kit::Ui, area: Rect, st: &PaletteState) {
        use crate::ui::kit::modal::{Modal, Sizing};

        let matches = st.matches();
        let frame = Modal::new("Command bar")
            .sizing(Sizing::medium())
            .subtitle(format!("{} of {}", matches.len(), st.items.len()))
            .render(ui, area);

        let rows: Vec<(String, Option<String>)> = matches
            .iter()
            .map(|&i| (st.items[i].label.to_string(), None))
            .collect();
        self.render_query_list(ui, frame.body, &st.query, st.cursor, st.sel, &rows);
    }

    /// The Reader's jump list: chapters, sections and bookmarks together.
    fn render_reader_jump_kit(
        &self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        st: &ReaderJumpState,
    ) {
        use crate::ui::kit::modal::{Modal, Sizing};

        let matches = st.matches();
        let frame = Modal::new("Jump to")
            .sizing(Sizing::medium())
            .subtitle(truncate_cols(&thai_display_safe(&st.title), 40))
            .render(ui, area);

        let rows: Vec<(String, Option<String>)> = matches
            .iter()
            .map(|&i| {
                let it = &st.items[i];
                let kind = match it.kind {
                    JumpKind::Chapter => "chapter",
                    JumpKind::Section => "section",
                    JumpKind::Bookmark => "bookmark",
                };
                (
                    thai_display_safe(&it.label),
                    Some(format!("{kind} · ch.{:03}", it.chapter)),
                )
            })
            .collect();
        self.render_query_list(ui, frame.body, &st.query, st.cursor, st.sel, &rows);
    }

    /// Reader search: one query line, applied to both panes.
    fn render_reader_search_kit(
        &self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        st: &ReaderSearchState,
    ) {
        use crate::ui::kit::modal::{self, Modal, Sizing};

        let frame = Modal::new("Search")
            .sizing(Sizing::small())
            .footer(1)
            .render(ui, area);
        self.render_query_list(ui, frame.body, &st.query, st.cursor, 0, &[]);
        modal::footer_hint(ui, frame.footer, "  ↵ search both panes · Esc cancel");
    }

    /// A proofreading note, anchored to one translated line.
    fn render_reader_note_kit(
        &self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        st: &ReaderNoteState,
    ) {
        use crate::ui::kit::button::{Button, ButtonRow};
        use crate::ui::kit::editor;
        use crate::ui::kit::modal::{self, Modal, Sizing};

        let frame = Modal::new("Note")
            .sizing(Sizing::small())
            .subtitle(format!("ch.{:03} · line {}", st.chapter, st.line))
            .footer(1)
            .render(ui, area);

        let lines = editor::wrap(&st.text, frame.body.width);
        editor::render(
            ui,
            frame.body,
            &editor::View::new(&st.text, &lines).cursor(st.cursor),
            0,
        );
        modal::render_footer(
            ui,
            frame.footer,
            ButtonRow::new(vec![
                Button::new(ZoneId::button(DIALOG_CANCEL), "Cancel").accel("esc"),
                Button::new(ZoneId::button(DIALOG_CONFIRM), "Save")
                    .accel("↵")
                    .primary()
                    .disabled(st.text.trim().is_empty()),
            ]),
        );
    }

    /// First-run menu: four ways in, each a real row.
    fn render_welcome_kit(&self, ui: &mut crate::ui::kit::Ui, area: Rect, st: &WelcomeState) {
        use crate::ui::kit::ctx::row_at;
        use crate::ui::kit::list::{self, ListState, Row};
        use crate::ui::kit::modal::{Modal, Sizing};

        let frame = Modal::new("ようこそ · Welcome to honya 本屋")
            .sizing(Sizing::medium())
            .render(ui, area);

        let bg = ui.theme.bg_elevated;
        let soft = Style::default().fg(ui.theme.ink_soft).bg(bg);
        let dim = Style::default().fg(ui.theme.ink_faint).bg(bg);
        for (n, line) in [
            "AI-assisted Japanese → Thai / English light-novel translation.",
            "",
            "Import an EPUB, then a three-agent pipeline works through it:",
            "Orchestrator ◆ plans · Translator ▲ drafts · Reviewer ■ checks.",
            "",
        ]
        .into_iter()
        .enumerate()
        {
            ui.text(
                row_at(frame.body, n as u16),
                line,
                if line.starts_with("AI-") { soft } else { dim },
            );
        }

        let menu = welcome_items(st);
        let list_area = Rect {
            y: frame.body.y + 5,
            height: frame.body.height.saturating_sub(5),
            ..frame.body
        };
        let mut ls = ListState::new();
        ls.select(Some(st.sel));
        list::render(
            ui,
            list_area,
            &mut ls,
            menu.len(),
            list::Opts {
                rail: true,
                scrollbar: false,
                kind: ZoneKind::Row,
                id_base: 0,
            },
            |i| {
                let (label, note) = menu[i];
                Row::new(Line::from(vec![
                    Span::raw(label.to_string()),
                    Span::styled(format!("   {note}"), dim),
                ]))
            },
        );
    }

    /// Pick a source file to refresh a volume's images from.
    fn render_image_source_kit(
        &self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        st: &ImageSourceState,
    ) {
        use crate::ui::kit::list::{self, ListState, Row};
        use crate::ui::kit::modal::{self, Modal, Sizing};

        let frame = Modal::new("Update volume images")
            .sizing(Sizing::medium())
            .subtitle(format!("Vol.{:02}", st.vol))
            .footer(1)
            .render(ui, area);

        let dim = Style::default().fg(ui.theme.ink_faint);
        let mut ls = ListState::new();
        ls.select(Some(st.sel));
        list::render(
            ui,
            frame.body,
            &mut ls,
            st.files.len(),
            list::Opts {
                rail: true,
                scrollbar: true,
                kind: ZoneKind::Row,
                id_base: 0,
            },
            |i| {
                let (path, size) = &st.files[i];
                let name = thai_display_safe(
                    &path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                );
                Row::new(Line::from(vec![
                    Span::raw(name),
                    Span::styled(format!("   {}", human_size(*size)), dim),
                ]))
            },
        );
        modal::footer_hint(ui, frame.footer, "  ↵ use this file · Esc cancel");
    }

    /// The QA inbox: a run summary, then every finding grouped under the
    /// chapter it belongs to.
    fn render_qa_kit(&self, ui: &mut crate::ui::kit::Ui, area: Rect, st: &QaState) {
        use crate::ui::kit::ctx::row_at;
        use crate::ui::kit::list::{self, ListState, Row};
        use crate::ui::kit::modal::{self, Modal, Sizing};

        let frame = Modal::new("QA review")
            .sizing(Sizing::large())
            .subtitle(truncate_cols(&thai_display_safe(&st.title), 48))
            .footer(1)
            .render(ui, area);

        let bg = ui.theme.bg_elevated;
        let dim = Style::default().fg(ui.theme.ink_faint).bg(bg);

        // Summary band: how the run went, before the list of what went wrong.
        let mut summary: Vec<Span<'static>> = Vec::new();
        for (glyph, count, color) in [
            (crate::ui::glyphs::MOON_FULL, st.report.done, ui.theme.status_done),
            (crate::ui::glyphs::FLAG, st.report.review, ui.theme.status_warn),
            (crate::ui::glyphs::CROSS, st.report.failed, ui.theme.status_failed),
        ] {
            summary.push(Span::styled(
                format!("{}{count}", glyph.as_str()),
                Style::default()
                    .fg(if count == 0 { ui.theme.ink_faint } else { color })
                    .bg(bg),
            ));
            summary.push(Span::styled("    ", dim));
        }
        if let Some(pct) = st.report.clean_pct() {
            summary.push(Span::styled(
                format!("{pct}% clean"),
                Style::default()
                    .fg(ui.theme.ink_soft)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        ui.line(row_at(frame.body, 0), Line::from(summary), Style::default().bg(bg));

        let list_area = Rect {
            y: frame.body.y + 2,
            height: frame.body.height.saturating_sub(2),
            ..frame.body
        };

        if st.report.issues.is_empty() {
            ui.text(
                row_at(list_area, 0),
                "Nothing flagged — every finished chapter passed review.",
                Style::default().fg(ui.theme.status_done).bg(bg),
            );
            modal::footer_hint(ui, frame.footer, "  Esc close");
            return;
        }

        let rows = qa_rows(&st.report);
        let selected_row = rows
            .iter()
            .position(|r| matches!(r, QaRow::Issue(i) if *i == st.sel));

        let head = Style::default()
            .fg(ui.theme.accent)
            .add_modifier(Modifier::BOLD);
        let plain = Style::default().fg(ui.theme.ink);
        // Per-chapter counts, keyed the same way the headings are grouped.
        let counts: Vec<usize> = rows
            .iter()
            .map(|r| match r {
                QaRow::Heading(_) => 0,
                QaRow::Issue(i) => st.report.count_for(st.report.issues[*i].chapter),
            })
            .collect();
        let visuals: Vec<(String, ratatui::style::Color, String)> = st
            .report
            .issues
            .iter()
            .map(|iss| {
                let (g, c, tag) = qa_visual(iss, ui.theme);
                (g.to_string(), c, tag)
            })
            .collect();
        // An empty reason still has to say something, or the row reads as a
        // finding with no content.
        let details: Vec<String> = st
            .report
            .issues
            .iter()
            .map(|iss| {
                if iss.detail.trim().is_empty() {
                    qa_default_detail(iss).to_string()
                } else {
                    thai_display_safe(&iss.detail)
                }
            })
            .collect();

        let mut ls = ListState::new();
        ls.select(selected_row);
        list::render(
            ui,
            list_area,
            &mut ls,
            rows.len(),
            list::Opts {
                rail: true,
                scrollbar: true,
                kind: ZoneKind::Row,
                id_base: 0,
            },
            |i| match &rows[i] {
                QaRow::Heading(t) => {
                    // The count belongs to the heading, so take it from the
                    // finding immediately below.
                    let n = counts.get(i + 1).copied().unwrap_or(0);
                    Row::header(Line::from(vec![
                        Span::styled(t.clone(), head),
                        Span::styled(
                            if n > 1 { format!("   {n} findings") } else { String::new() },
                            Style::default().fg(ui.theme.ink_faint),
                        ),
                    ]))
                }
                QaRow::Issue(n) => {
                    let (glyph, color, tag) = &visuals[*n];
                    Row::new(Line::from(vec![
                        Span::styled(format!("  {glyph} "), Style::default().fg(*color)),
                        Span::styled(
                            format!("{tag:<12}"),
                            Style::default().fg(ui.theme.ink_faint),
                        ),
                        Span::styled(details[*n].clone(), plain),
                    ]))
                }
            },
        );
        modal::footer_hint(
            ui,
            frame.footer,
            "  ↵ open in the Reader · jk move · Esc close",
        );
    }

    /// Read-only source ‖ translation ‖ reviewer note for one chunk.
    fn render_reader_inspect_kit(
        &self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        st: &ReaderInspectState,
    ) {
        use crate::ui::kit::card::Card;
        use crate::ui::kit::editor;
        use crate::ui::kit::modal::{self, Modal, Sizing};

        let frame = Modal::new("Inspect chunk")
            .sizing(Sizing::large())
            .subtitle(format!("ch.{:03} · chunk {}", st.chapter, st.chunk + 1))
            .footer(1)
            .render(ui, area);

        // Source and translation share the width; the reviewer note, when there
        // is one, takes a band underneath both.
        let note_rows = if st.review.is_some() {
            (frame.body.height / 4).clamp(3, 8)
        } else {
            0
        };
        let panes_h = frame.body.height.saturating_sub(note_rows);
        let half = frame.body.width / 2;

        for (n, (title, text)) in [
            ("Source 日本語", &st.source_jp),
            ("Translation", &st.translated_text),
        ]
        .into_iter()
        .enumerate()
        {
            let pane = Rect {
                x: frame.body.x + n as u16 * half,
                y: frame.body.y,
                width: half,
                height: panes_h,
            };
            let body = Card::new(title).render(ui, pane);
            let wrapped = editor::wrap(text, body.width);
            editor::render(
                ui,
                body,
                &editor::View::new(text, &wrapped).scroll(st.scroll as usize),
                n as u32,
            );
        }

        if let Some(review) = &st.review {
            let band = Rect {
                y: frame.body.y + panes_h,
                height: note_rows,
                ..frame.body
            };
            let body = Card::new("Reviewer note")
                .accent(ui.theme.status_warn)
                .render(ui, band);
            let wrapped = editor::wrap(review, body.width);
            editor::render(ui, body, &editor::View::new(review, &wrapped), 2);
        }
        modal::footer_hint(ui, frame.footer, "  jk scroll · Esc close");
    }

    /// In-place editor for one chunk's translated prose.
    fn render_reader_edit_kit(
        &self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        st: &ReaderEditState,
    ) {
        use crate::ui::kit::button::{Button, ButtonRow};
        use crate::ui::kit::editor;
        use crate::ui::kit::modal::{self, Modal, Sizing};

        let frame = Modal::new("Edit translation")
            .sizing(Sizing::large())
            .subtitle(format!("ch.{:03} · chunk {}", st.chapter, st.chunk + 1))
            .footer(1)
            .render(ui, area);

        let wrapped = editor::wrap(&st.text, frame.body.width);
        editor::render(
            ui,
            frame.body,
            &editor::View::new(&st.text, &wrapped).cursor(st.cursor),
            0,
        );
        modal::render_footer(
            ui,
            frame.footer,
            ButtonRow::new(vec![
                Button::new(ZoneId::button(DIALOG_CANCEL), "Discard").accel("esc"),
                Button::new(ZoneId::button(DIALOG_CONFIRM), "Save")
                    .accel("^s")
                    .primary(),
            ]),
        );
    }

    /// The shared source/translation editor behind the synopsis and title
    /// overlays: raw text above, its translation below, with the agent's phase
    /// reported between them.
    fn render_syn_body(
        &self,
        ui: &mut crate::ui::kit::Ui,
        body: Rect,
        syn: &SynopsisState,
        raw_title: &str,
    ) {
        use crate::ui::kit::card::Card;
        use crate::ui::kit::editor;

        let half = body.height / 2;
        let raw_area = Rect {
            height: half,
            ..body
        };
        let out_area = Rect {
            y: body.y + half,
            height: body.height.saturating_sub(half),
            ..body
        };

        let raw_body = Card::new(raw_title).render(ui, raw_area);
        let raw_wrapped = editor::wrap(&syn.raw, raw_body.width);
        editor::render(
            ui,
            raw_body,
            &editor::View::new(&syn.raw, &raw_wrapped)
                .cursor(if syn.edit_translation { usize::MAX } else { syn.cursor }),
            0,
        );

        let (label, accent) = match syn.phase {
            SynPhase::Editing => ("Translation", ui.theme.rule),
            SynPhase::Translating => ("Translation · working…", ui.theme.status_working),
            SynPhase::Done => ("Translation", ui.theme.status_done),
            SynPhase::Failed => ("Translation · failed", ui.theme.status_failed),
        };
        let out_body = Card::new(label).accent(accent).render(ui, out_area);
        let shown = if matches!(syn.phase, SynPhase::Failed) && !syn.error.is_empty() {
            &syn.error
        } else {
            &syn.translated_text
        };
        let out_wrapped = editor::wrap(shown, out_body.width);
        editor::render(
            ui,
            out_body,
            &editor::View::new(shown, &out_wrapped).cursor(if syn.edit_translation {
                syn.translated_cursor
            } else {
                usize::MAX
            }),
            1,
        );
    }

    /// Volume synopsis editor.
    fn render_synopsis_kit(
        &self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        st: &SynopsisEditState,
    ) {
        use crate::ui::kit::modal::{Modal, Sizing};

        let frame = Modal::new("Synopsis")
            .sizing(Sizing::large())
            .subtitle(format!(
                "Vol.{:02} · {}",
                st.vol,
                truncate_cols(&thai_display_safe(&st.title), 30)
            ))
            .footer(1)
            .render(ui, area);
        self.render_syn_body(ui, frame.body, &st.syn, "Source 日本語");
        self.render_syn_footer(ui, frame.footer, &st.syn);
    }

    /// Project title editor — the same shape, one line each.
    fn render_project_title_kit(
        &self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        st: &TitleEditState,
    ) {
        use crate::ui::kit::modal::{Modal, Sizing};

        let frame = Modal::new("Project title")
            .sizing(Sizing::medium())
            .footer(1)
            .render(ui, area);
        self.render_syn_body(ui, frame.body, &st.syn, "Title 日本語");
        self.render_syn_footer(ui, frame.footer, &st.syn);
    }

    /// Buttons shared by both translate-and-accept editors.
    fn render_syn_footer(&self, ui: &mut crate::ui::kit::Ui, footer: Rect, syn: &SynopsisState) {
        use crate::ui::kit::button::{Button, ButtonRow};
        use crate::ui::kit::modal;

        let working = matches!(syn.phase, SynPhase::Translating);
        let label = if syn.attempt > 0 { "Reroll" } else { "Translate" };
        modal::render_footer(
            ui,
            footer,
            ButtonRow::new(vec![
                Button::new(ZoneId::button(DIALOG_CANCEL), "Cancel").accel("esc"),
                Button::new(ZoneId::button(DIALOG_ALTERNATE), label)
                    .accel("tab")
                    .disabled(working || syn.raw.trim().is_empty()),
                Button::new(ZoneId::button(DIALOG_CONFIRM), "Save")
                    .accel("↵")
                    .primary()
                    .disabled(working),
            ]),
        );
    }

    /// Settings: a category rail beside a form generated from the registry.
    ///
    /// Nothing here knows what any individual setting *is* — the rows come from
    /// [`settings_defs::ORDER`] and their live values from `field_kind`, which
    /// is the whole point of declaring them once.
    fn render_settings_kit(
        &self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        cfg: &AppConfig,
        st: &SettingsState,
    ) {
        use crate::ui::kit::form;
        use crate::ui::kit::list::ListState;
        use crate::ui::kit::modal::{self, Modal, Sizing};

        let frame = Modal::new("Settings")
            .sizing(Sizing::large())
            .footer(2)
            .render(ui, area);

        // Rail on the left, form on the right. At narrow widths the rail
        // collapses to a single strip of initials rather than stealing the
        // columns the values need.
        let rail_w = if ui.metrics.is_narrow() { 3 } else { 16 };
        let rail = Rect {
            width: rail_w.min(frame.body.width / 3),
            ..frame.body
        };
        let form_area = Rect {
            x: frame.body.x + rail.width + 1,
            width: frame.body.width.saturating_sub(rail.width + 1),
            ..frame.body
        };
        self.render_settings_rail(ui, rail, st);

        if st.tab == SettingsTab::Account {
            self.render_settings_account(ui, form_area, st);
            modal::footer_hint(ui, frame.footer, "  tab switch section · Esc close");
            return;
        }

        let fields: Vec<form::Field> = st
            .tab
            .group()
            .fields()
            .into_iter()
            .filter_map(|i| settings_defs::at(i).map(|d| self.settings_field(cfg, st, d)))
            .collect();
        let group_start = st.tab.group().first_field().unwrap_or(0);
        let mut ls = ListState::new();
        ls.select(Some(st.field.saturating_sub(group_start) as usize));

        form::render(
            ui,
            form_area,
            &mut ls,
            &fields,
            form::Opts {
                label_cols: 26,
                id_base: group_start as u32,
            },
        );

        // The focused row's help, rather than a help line on every row.
        let focused = (st.field.saturating_sub(group_start)) as usize;
        form::render_help(
            ui,
            Rect {
                height: 1,
                ..frame.footer
            },
            fields.get(focused),
        );
        modal::footer_hint(
            ui,
            Rect {
                y: frame.footer.y + 1,
                height: 1,
                ..frame.footer
            },
            "  ↵ save · tab section · ←→ change · Esc close",
        );
    }

    /// The category rail. Each entry is a zone, so a section is one click away.
    fn render_settings_rail(
        &self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        st: &SettingsState,
    ) {
        use crate::ui::kit::style;

        let narrow = area.width < 6;
        for (n, group) in Group::ALL.into_iter().enumerate() {
            let row = crate::ui::kit::ctx::row_at(area, n as u16);
            if row.height == 0 {
                break;
            }
            let active = st.tab.group() == group;
            let state = ui.interactive(row, ZoneId::segment(n), active);
            let base = style::row(state, ui.theme);
            ui.fill(row, base);
            let (glyph, rail_style) = match style::rail(state, ui.theme) {
                Some((g, s)) => (g.as_str().to_string(), s),
                None => (" ".to_string(), base),
            };
            let label = if narrow {
                group.title().chars().next().unwrap_or(' ').to_string()
            } else {
                group.title().to_string()
            };
            ui.line(
                row,
                Line::from(vec![
                    Span::styled(glyph, rail_style),
                    Span::styled(
                        format!(" {label}"),
                        if active {
                            base.add_modifier(Modifier::BOLD)
                        } else {
                            base
                        },
                    ),
                ]),
                base,
            );
        }
    }

    /// The Account section: sign-in state and the remote link, which are
    /// actions rather than settings and so have no registry rows.
    fn render_settings_account(
        &self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        st: &SettingsState,
    ) {
        use crate::remote::protocol::RemoteState;
        use crate::ui::kit::ctx::row_at;

        let bg = ui.theme.bg_elevated;
        let dim = Style::default().fg(ui.theme.ink_faint).bg(bg);
        let mut row = 0u16;
        let mut put = |ui: &mut crate::ui::kit::Ui, spans: Vec<Span<'static>>| {
            let r = row_at(area, row);
            if r.height > 0 {
                ui.line(r, Line::from(spans), Style::default().bg(bg));
            }
            row += 1;
        };

        let (account, color) = match &st.account_login {
            Some(login) => (format!("signed in as {login}"), ui.theme.status_done),
            None => ("not signed in".to_string(), ui.theme.ink_soft),
        };
        put(
            ui,
            vec![
                Span::styled("GitHub        ", dim),
                Span::styled(account, Style::default().fg(color).bg(bg)),
            ],
        );
        put(ui, vec![Span::styled("  Ctrl-A sign in · Ctrl-O sign out", dim)]);
        put(ui, vec![]);

        let (link, link_color) = match st.remote_state {
            RemoteState::Connected => (
                format!("connected · {} watching", st.remote_watchers),
                ui.theme.status_done,
            ),
            RemoteState::Connecting => ("connecting…".into(), ui.theme.status_working),
            RemoteState::Pairing => ("pairing…".into(), ui.theme.status_working),
            RemoteState::Error => ("error".into(), ui.theme.status_failed),
            RemoteState::Disconnected => (
                if st.remote_enabled { "enabled".into() } else { "off".to_string() },
                ui.theme.ink_soft,
            ),
        };
        put(
            ui,
            vec![
                Span::styled("Remote        ", dim),
                Span::styled(link, Style::default().fg(link_color).bg(bg)),
            ],
        );
        put(ui, vec![Span::styled("  Ctrl-R toggle the relay link", dim)]);

        if let Some(prompt) = &st.remote_auth_code {
            put(ui, vec![]);
            put(
                ui,
                vec![
                    Span::styled("Code          ", dim),
                    Span::styled(
                        prompt.code.clone(),
                        Style::default()
                            .fg(ui.theme.accent)
                            .bg(bg)
                            .add_modifier(Modifier::BOLD),
                    ),
                ],
            );
            put(
                ui,
                vec![Span::styled(
                    format!("  {}   Ctrl-B open · Ctrl-K copy", prompt.uri),
                    dim,
                )],
            );
        }
    }

    /// One registry row as a live form field.
    fn settings_field(
        &self,
        cfg: &AppConfig,
        st: &SettingsState,
        d: &settings_defs::Def,
    ) -> crate::ui::kit::form::Field {
        use crate::ui::kit::form::{Field, Kind};
        use settings_defs::Kind as DKind;

        let cursor = if settings_defs::index_of(d.field) == st.field {
            st.cursor
        } else {
            0
        };
        let kind = match d.kind {
            DKind::Toggle => Kind::Toggle {
                on: self.settings_toggle(st, d.field),
            },
            DKind::Select => Kind::Select {
                // The current value only: the list is cycled through the
                // arrows, which is how these have always been edited.
                options: vec![self.settings_select_label(cfg, st, d.field)],
                index: 0,
            },
            DKind::Secret => {
                let (value, from_env) = self.settings_secret(st, d.field);
                Kind::Secret {
                    value,
                    cursor,
                    from_env,
                }
            }
            DKind::Number { min, max } => Kind::Number {
                value: self
                    .settings_text(st, d.field)
                    .trim()
                    .parse::<i64>()
                    .unwrap_or(min),
                min,
                max,
            },
            DKind::Text => Kind::Text {
                value: self.settings_text(st, d.field),
                cursor,
                placeholder: "unset".into(),
            },
        };
        Field::new(d.label, kind)
            .help(d.help)
            .disabled(self.settings_disabled(st, d.field))
    }

    /// The live value of a toggle row.
    fn settings_toggle(&self, st: &SettingsState, f: SField) -> bool {
        match f {
            SField::ParallelLookahead => st.parallel_lookahead,
            SField::PrepassExtract => st.prepass_extract,
            SField::CoherenceCheck => st.coherence_check,
            SField::SystemOneEnabled => st.system_one.enabled,
            other => settings_defs::feature_of(other)
                .map(|feat| st.system_one.armed(feat))
                .unwrap_or(false),
        }
    }

    /// The label a cycle row currently shows.
    fn settings_select_label(
        &self,
        _cfg: &AppConfig,
        st: &SettingsState,
        f: SField,
    ) -> String {
        match f {
            SField::OrchProvider => st.models.orchestrator.provider.label().to_string(),
            SField::TransProvider => st.models.translator.provider.label().to_string(),
            SField::ReviewProvider => st.models.reviewer.provider.label().to_string(),
            SField::RefineProvider => st.models.refine.provider.label().to_string(),
            SField::OrchEffort => crate::model::Effort::label(st.models.orchestrator.effort).to_string(),
            SField::TransEffort => crate::model::Effort::label(st.models.translator.effort).to_string(),
            SField::ReviewEffort => crate::model::Effort::label(st.models.reviewer.effort).to_string(),
            SField::RefineEffort => crate::model::Effort::label(st.models.refine.effort).to_string(),
            SField::PreferredLanguageField => st.preferred_language.label().to_string(),
            SField::ServiceTierField => {
                crate::model::ServiceTier::label(st.service_tier).to_string()
            }
            SField::GateMode => st.system_one.review_gate.label().to_string(),
            SField::GateProvider => st.system_one.provider.label().to_string(),
            SField::UpdateModeField => st.update_mode.label().to_string(),
            SField::ReleaseChannelField => st.release_channel.label().to_string(),
            _ => String::new(),
        }
    }

    /// A secret row's value, and whether the environment already supplies it.
    fn settings_secret(&self, st: &SettingsState, f: SField) -> (String, bool) {
        match f {
            SField::OpenRouterKey => (st.openrouter_key.clone(), st.api_key_env),
            SField::TokenrouterKey => (st.tokenrouter_key.clone(), st.tokenrouter_key_env),
            SField::GoogleKey => (st.google_key.clone(), st.google_key_env),
            SField::CloudflareToken => (
                st.cloudflare_api_token.clone(),
                st.cloudflare_api_token_env,
            ),
            SField::GateKey => (st.typesafe_key.clone(), st.typesafe_key_env),
            _ => (String::new(), false),
        }
    }

    /// A text or numeric row's current contents.
    fn settings_text(&self, st: &SettingsState, f: SField) -> String {
        match f {
            SField::OrchModel => st.models.orchestrator.model.clone(),
            SField::TransModel => st.models.translator.model.clone(),
            SField::ReviewModel => st.models.reviewer.model.clone(),
            SField::RefineModel => st.models.refine.model.clone(),
            SField::CloudflareAccount => st.cloudflare_account_id.clone(),
            SField::MaxAttempts => st.max_attempts.clone(),
            SField::ContinuitySentences => st.continuity_sentences.clone(),
            SField::LoopStall => st.loop_stall_secs.clone(),
            SField::Retranslates => st.max_chapter_retranslates.clone(),
            SField::ChunkTargetTokens => st.chunk_target_tokens.clone(),
            SField::ChunkHardCapTokens => st.chunk_hard_cap_tokens.clone(),
            SField::GateModel => st.system_one.model.clone(),
            SField::GateConfidence => st.system_one_confidence.clone(),
            _ => String::new(),
        }
    }

    /// Whether a row is currently inert.
    ///
    /// The System One block is the only case: with the master switch off, every
    /// judgement row below it does nothing, and saying so is better than
    /// letting someone set five toggles that have no effect.
    fn settings_disabled(&self, st: &SettingsState, f: SField) -> bool {
        if matches!(f, SField::SystemOneEnabled) {
            return false;
        }
        let in_block = settings_defs::feature_of(f).is_some()
            || matches!(
                f,
                SField::GateMode
                    | SField::GateProvider
                    | SField::GateModel
                    | SField::GateKey
                    | SField::GateConfidence
            );
        in_block && !st.system_one.enabled
    }

    /// The import wizard.
    ///
    /// The frame is a fixed height across every step on purpose: a modal that
    /// resized as you advanced would move the controls out from under the
    /// pointer. Completed steps on the rail are clickable, so going back is one
    /// click rather than the right number of Escapes.
    fn render_import_kit(&self, ui: &mut crate::ui::kit::Ui, area: Rect, st: &ImportState) {
        use crate::ui::kit::ctx::row_at;
        use crate::ui::kit::modal::{Modal, Sizing};
        use crate::ui::kit::progress::{self, Step, StepState};

        let title = thai_display_safe(if st.lock_name {
            "Add volume · เพิ่มเล่ม"
        } else {
            "New project · นำเข้าไฟล์"
        });
        let frame = Modal::new(&title)
            .sizing(Sizing::medium())
            .fixed_height(24)
            .footer(1)
            .render(ui, area);

        // The rail shows only the steps this flow actually visits.
        let visible = st.visible_steps();
        let steps: Vec<Step> = visible
            .iter()
            .map(|&s| {
                let state = if s < st.step {
                    StepState::Done
                } else if s == st.step {
                    StepState::Current
                } else if s.is_optional() {
                    StepState::Optional
                } else {
                    StepState::Ahead
                };
                Step::new(s.label(), state)
            })
            .collect();
        progress::stepper(ui, row_at(frame.body, 0), &steps);

        ui.line(
            row_at(frame.body, 1),
            import_context_line(st, ui.theme),
            Style::default().bg(ui.theme.bg_elevated),
        );

        let body = Rect {
            y: frame.body.y + 3,
            height: frame.body.height.saturating_sub(3),
            ..frame.body
        };
        self.render_import_body(ui, body, st);
        self.render_import_footer(ui, frame.footer, st);
    }

    fn render_import_body(
        &self,
        ui: &mut crate::ui::kit::Ui,
        body: Rect,
        st: &ImportState,
    ) {
        use crate::ui::kit::ctx::row_at;
        use crate::ui::kit::list::{self, ListState, Row};
        use crate::ui::kit::progress;

        let bg = ui.theme.bg_elevated;
        let dim = Style::default().fg(ui.theme.ink_faint).bg(bg);
        let ink = Style::default().fg(ui.theme.ink).bg(bg);

        match st.step {
            ImportStep::Pick => {
                if st.files.is_empty() {
                    ui.text(
                        row_at(body, 0),
                        "No importable files in this folder — press r to rescan.",
                        dim,
                    );
                    ui.text(
                        row_at(body, 2),
                        format!(
                            "Accepted: {}",
                            crate::document_import::supported_import_summary()
                        ),
                        dim,
                    );
                    return;
                }
                let mut ls = ListState::new();
                ls.select(Some(st.sel));
                list::render(
                    ui,
                    body,
                    &mut ls,
                    st.files.len(),
                    list::Opts {
                        rail: true,
                        scrollbar: true,
                        kind: ZoneKind::Row,
                        id_base: 0,
                    },
                    |i| {
                        let (path, size) = &st.files[i];
                        // A filename can be Thai, and every string reaching
                        // the screen has to be decomposed first or the cells
                        // drift.
                        let name = thai_display_safe(
                            &path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default(),
                        );
                        Row::new(Line::from(vec![
                            Span::raw(name),
                            Span::styled(format!("   {}", human_size(*size)), dim),
                        ]))
                    },
                );
            }
            ImportStep::Name => {
                ui.text(row_at(body, 0), "What is this project called?", dim);
                let (before, after) = input::caret_halves(
                    &st.name,
                    st.name_cursor,
                    body.width.saturating_sub(2) as usize,
                );
                ui.line(
                    row_at(body, 2),
                    Line::from(vec![
                        Span::styled(before, ink),
                        Span::styled(
                            crate::ui::glyphs::ACCENT_RAIL.as_str().to_string(),
                            Style::default().fg(ui.theme.stream_cursor).bg(bg),
                        ),
                        Span::styled(after, ink),
                    ]),
                    Style::default().bg(bg),
                );
                if let Some(note) = st.note {
                    ui.text(
                        row_at(body, 4),
                        note,
                        Style::default().fg(ui.theme.status_warn).bg(bg),
                    );
                }
            }
            ImportStep::Title => {
                self.render_syn_body(ui, body, &st.title_syn, "Title 日本語");
            }
            ImportStep::Volume => {
                ui.text(row_at(body, 0), "Which volume is this?", dim);
                ui.line(
                    row_at(body, 2),
                    Line::from(vec![
                        Span::styled("  ", ink),
                        Span::styled(
                            format!("Vol.{:02}", st.vol),
                            Style::default()
                                .fg(ui.theme.accent)
                                .bg(bg)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    Style::default().bg(bg),
                );
                ui.text(row_at(body, 4), "↑↓ or type a number", dim);
                if let Some(target) = st.target_project()
                    && !target.volumes.is_empty()
                {
                    ui.text(
                        row_at(body, 6),
                        format!("Already has  {}", volume_chips(&target.volumes)),
                        dim,
                    );
                }
            }
            ImportStep::Synopsis => {
                self.render_syn_body(ui, body, &st.syn, "Synopsis 日本語");
            }
            ImportStep::Importing => {
                let (done, total, what) =
                    st.progress.clone().unwrap_or((0, 0, "starting".into()));
                ui.text(row_at(body, 0), format!("Importing · {what}"), ink);
                progress::bar_with_label(ui, row_at(body, 2), done, total);
                ui.text(
                    row_at(body, 4),
                    "Chapters are read in spine order and illustrations relocated.",
                    dim,
                );
            }
        }
    }

    /// Back / Skip / Next, as real buttons.
    fn render_import_footer(
        &self,
        ui: &mut crate::ui::kit::Ui,
        footer: Rect,
        st: &ImportState,
    ) {
        use crate::ui::kit::button::{Button, ButtonRow};
        use crate::ui::kit::modal;

        if st.step == ImportStep::Importing {
            modal::footer_hint(ui, footer, "  import continues in the background");
            return;
        }
        let mut buttons = vec![Button::new(ZoneId::button(DIALOG_CANCEL), "Back").accel("esc")];
        if matches!(st.step, ImportStep::Title | ImportStep::Synopsis) {
            buttons.push(
                Button::new(ZoneId::button(DIALOG_ALTERNATE), "Translate").accel("tab"),
            );
        }
        // One forward button, labelled for what it will actually do. A separate
        // Skip would be a second button doing the same thing as Next, since an
        // optional step advances either way.
        let empty_optional = match st.step {
            ImportStep::Title => st.title_syn.translated_text.trim().is_empty(),
            ImportStep::Synopsis => st.syn.raw.trim().is_empty(),
            _ => false,
        };
        let next_label = match st.step {
            ImportStep::Synopsis if !empty_optional => "Import",
            ImportStep::Synopsis => "Skip · import",
            s if s.is_optional() && empty_optional => "Skip",
            _ => "Next",
        };
        buttons.push(
            Button::new(ZoneId::button(DIALOG_CONFIRM), next_label)
                .accel("↵")
                .primary()
                .disabled(st.step == ImportStep::Pick && st.files.is_empty()),
        );
        modal::render_footer(ui, footer, ButtonRow::new(buttons));
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

}

/// A key event with no modifiers — used to replay a gesture through the keyboard
/// handlers so navigation / dismiss logic lives in exactly one place.
fn synth(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::empty())
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
        if st.step == ImportStep::Pick { pending } else { confirmed },
    ));
    spans.push(sep.clone());
    spans.push(Span::styled(
        st.effective_target_language().label().to_string(),
        Style::default().fg(theme.accent_soft),
    ));
    if st.lock_name || st.step > ImportStep::Name {
        spans.push(sep.clone());
        spans.push(Span::styled(
            truncate_cols(&thai_display_safe(st.name.trim()), 30),
            confirmed,
        ));
    }
    if st.lock_name || st.step > ImportStep::Volume {
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

    /// Every glyph the terminal would draw for the synopsis editor,
    /// concatenated. Goes through the real overlay so the test exercises the
    /// path the app actually renders.
    fn rendered_glyphs(st: &SynopsisState) -> String {
        let ov = Overlay::Synopsis(SynopsisEditState {
            vol: 1,
            title: "t".into(),
            target_language: TargetLanguage::Thai,
            syn: st.clone(),
        });
        render_overlay(&ov, 80, 20).0.concat()
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
            Overlay::palette(),
            Overlay::reader_search(),
            Overlay::reader_note(3, 12),
            Overlay::welcome(false, false),
            Overlay::image_source(
                vec![(std::path::PathBuf::from("book.epub"), 4_200_000)],
                1,
            ),
            Overlay::qa_placeholder(),
            Overlay::synopsis_edit(
                "夏の物語".into(),
                "เรื่องราวฤดูร้อน".into(),
                2,
                "ある夏".into(),
                TargetLanguage::Thai,
            ),
            Overlay::project_title_edit(
                "natsu".into(),
                "ある夏の物語".into(),
                "เรื่องราวฤดูร้อนหนึ่ง".into(),
                TargetLanguage::Thai,
            ),
            Overlay::reader_inspect(
                3,
                1,
                "「月が綺麗ですね。」".into(),
                "พระจันทร์คืนนี้สวยนะ".into(),
                Some("tone drifts formal here".into()),
            ),
            Overlay::reader_edit(3, 1, "พระจันทร์คืนนี้สวยนะ".into()),
            Overlay::reader_jump(
                "ある夏の物語".into(),
                vec![
                    JumpTarget {
                        chapter: 1,
                        line: 1,
                        label: "第一章 はじまり".into(),
                        kind: JumpKind::Chapter,
                    },
                    JumpTarget {
                        chapter: 2,
                        line: 40,
                        label: "Re-read this bit".into(),
                        kind: JumpKind::Bookmark,
                    },
                ],
            ),
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
    fn the_command_bar_can_be_aimed_not_just_filtered() {
        // Subsequence matching is the point: a substring filter cannot get from
        // an abbreviation to the command it stands for.
        let mut ov = Overlay::palette();
        let Overlay::Palette(st) = &mut ov else {
            unreachable!()
        };
        // An abbreviation no substring search could ever resolve.
        st.query = "twp".into();
        let hits = st.matches();
        assert!(!hits.is_empty(), "\"twp\" should reach the whole-project run");
        assert_eq!(st.items[hits[0]].label, "Translate whole project");

        // Every screen is reachable by name, including the sixth.
        st.query = "refine".into();
        let hits = st.matches();
        assert!(
            hits.iter().any(|&i| st.items[i].label == "Go: Refine"),
            "the command bar should be able to reach every tab"
        );

        // And an empty query keeps the caller's deliberate ordering.
        st.query.clear();
        assert_eq!(st.matches(), (0..st.items.len()).collect::<Vec<_>>());
    }

    #[test]
    fn clicking_a_command_bar_row_runs_that_command() {
        let mut ov = Overlay::palette();
        let (_, zones) = render_overlay(&ov, 100, 30);
        let target = match &ov {
            Overlay::Palette(st) => st.matches()[2],
            _ => unreachable!(),
        };
        let expected = match &ov {
            Overlay::Palette(st) => format!("{:?}", st.items[target].action),
            _ => unreachable!(),
        };
        let rect = zones.rect_of(ZoneId::row(2)).expect("row 2");
        // First click selects, second activates — the same two-step the
        // keyboard takes with arrows then Enter.
        let m = |double| MouseInput {
            gesture: MouseGesture::Click { double },
            col: rect.x + 2,
            row: rect.y,
        };
        assert!(matches!(ov.handle_mouse_zones(m(false), &zones), Action::None));
        let got = ov.handle_mouse_zones(m(false), &zones);
        assert_eq!(format!("{got:?}"), expected);
    }

    #[test]
    fn a_qa_row_click_maps_back_to_the_finding_it_shows() {
        // Rows interleave chapter headings with findings, so a clicked row
        // index is not a finding index. Both sides go through `qa_rows`.
        let mut ov = Overlay::Qa(QaState {
            title: "proj".into(),
            report: qa::QaReport {
                issues: vec![
                    qa::QaIssue {
                        chapter: Some(1),
                        title: "one".into(),
                        kind: qa::QaKind::ChapterFailed,
                        detail: String::new(),
                    },
                    qa::QaIssue {
                        chapter: Some(2),
                        title: "two".into(),
                        kind: qa::QaKind::ChapterFailed,
                        detail: "boom".into(),
                    },
                ],
                done: 3,
                review: 1,
                failed: 1,
            },
            sel: 0,
        });
        let (lines, zones) = render_overlay(&ov, 100, 30);
        let joined = lines.join("\n");
        assert!(joined.contains("clean"), "summary missing:\n{joined}");

        let rows = match &ov {
            Overlay::Qa(st) => qa_rows(&st.report),
            _ => unreachable!(),
        };
        // Row 3 is the second finding (heading, finding, heading, finding).
        assert!(matches!(rows[3], QaRow::Issue(1)));
        let rect = zones.rect_of(ZoneId::row(3)).expect("row 3 registered");
        ov.handle_mouse_zones(
            MouseInput {
                gesture: MouseGesture::Click { double: false },
                col: rect.x + 4,
                row: rect.y,
            },
            &zones,
        );
        match &ov {
            Overlay::Qa(st) => assert_eq!(st.sel, 1, "clicked row selected the wrong finding"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn a_finding_with_no_reason_still_says_something() {
        let ov = Overlay::Qa(QaState {
            title: "proj".into(),
            report: qa::QaReport {
                issues: vec![qa::QaIssue {
                    chapter: Some(1),
                    title: "one".into(),
                    kind: qa::QaKind::ChapterFailed,
                    detail: String::new(),
                }],
                done: 0,
                review: 0,
                failed: 1,
            },
            sel: 0,
        });
        let (lines, _) = render_overlay(&ov, 100, 20);
        let joined = lines.join("\n");
        let expected = qa_default_detail(&qa::QaIssue {
            chapter: Some(1),
            title: "one".into(),
            kind: qa::QaKind::ChapterFailed,
            detail: String::new(),
        });
        assert!(
            joined.contains(expected),
            "an empty reason should fall back to {expected:?}:\n{joined}"
        );
    }

    #[test]
    fn the_key_field_constant_tracks_the_registry() {
        // The GUI needs a constant, so this pairing cannot be derived away —
        // but it can be pinned, so inserting a row above the key row fails
        // here rather than silently opening Settings on the wrong field.
        assert_eq!(SETTINGS_KEY_FIELD, settings_defs::key_field());
        assert_eq!(
            settings_defs::ORDER[SETTINGS_KEY_FIELD as usize].field,
            SField::OpenRouterKey
        );
    }

    #[test]
    fn the_four_newly_reachable_settings_round_trip_through_save() {
        // They exist in AppConfig but had no row, so until now they could only
        // be changed by editing config.json by hand.
        let cfg = AppConfig {
            chunk_target_tokens: 900,
            chunk_hard_cap_tokens: 1500,
            prepass_extract: false,
            coherence_check: false,
            ..AppConfig::default()
        };

        let Overlay::Settings(st) = Overlay::settings_with_field(&cfg, 0) else {
            unreachable!()
        };
        assert_eq!(st.chunk_target_tokens, "900");
        assert_eq!(st.chunk_hard_cap_tokens, "1500");
        assert!(!st.prepass_extract);
        assert!(!st.coherence_check);

        let Action::SaveSettings {
            chunk_target_tokens,
            chunk_hard_cap_tokens,
            prepass_extract,
            coherence_check,
            ..
        } = st.save_action()
        else {
            panic!("expected a save action")
        };
        assert_eq!(chunk_target_tokens, 900);
        assert_eq!(chunk_hard_cap_tokens, 1500);
        assert!(!prepass_extract);
        assert!(!coherence_check);
    }

    #[test]
    fn a_hard_cap_below_the_target_is_lifted_rather_than_accepted() {
        // A cap under the target would make the target unreachable, which is
        // not a configuration anyone means to express.
        let cfg = AppConfig::default();
        let Overlay::Settings(mut st) = Overlay::settings_with_field(&cfg, 0) else {
            unreachable!()
        };
        st.chunk_target_tokens = "2000".into();
        st.chunk_hard_cap_tokens = "500".into();
        let Action::SaveSettings {
            chunk_target_tokens,
            chunk_hard_cap_tokens,
            ..
        } = st.save_action()
        else {
            panic!("expected a save action")
        };
        assert_eq!(chunk_target_tokens, 2000);
        assert_eq!(chunk_hard_cap_tokens, 2000, "the cap is raised to the target");
    }

    #[test]
    fn the_new_toggles_respond_to_cycling() {
        let cfg = AppConfig::default();
        let Overlay::Settings(mut st) = Overlay::settings_with_field(&cfg, 0) else {
            unreachable!()
        };
        for (field, read) in [
            (SField::PrepassExtract, 0usize),
            (SField::CoherenceCheck, 1),
        ] {
            st.focus(settings_defs::index_of(field));
            let before = if read == 0 { st.prepass_extract } else { st.coherence_check };
            st.cycle(true);
            let after = if read == 0 { st.prepass_extract } else { st.coherence_check };
            assert_ne!(before, after, "{field:?} did not toggle");
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
            st.step = ImportStep::Synopsis;
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
            slug: crate::app::slugify("Cursed Blade"),
            title: "Cursed Blade".to_string(),
            target_language: TargetLanguage::Thai,
            volumes: vec![(1, 12)],
        };
        let mut st = ImportState::new(
            vec![(PathBuf::from("cursed_blade_v03.epub"), 2_345_678)],
            vec![existing],
            TargetLanguage::English,
        );
        st.step = ImportStep::Synopsis;
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
            st.step = ImportStep::Name;
            st.name.clear();
            st.name_cursor = 0;
        }
        ov.handle_key(key(KeyCode::Enter));
        let Overlay::Import(st) = &ov else {
            panic!("overlay changed variant")
        };
        assert_eq!(st.step, ImportStep::Name, "must not advance without a name");
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
            slug: crate::app::slugify("Cursed Blade"),
            title: "Cursed Blade".to_string(),
            volumes: vec![(1, 12), (2, 9)],
            target_language: TargetLanguage::Thai,
        };
        let mut ov = wizard(vec![existing]);
        if let Overlay::Import(st) = &mut ov {
            st.step = ImportStep::Name;
            st.name = "Cursed Blade".to_string();
            st.name_cursor = st.name.len();
        }
        ov.handle_key(key(KeyCode::Enter)); // → translated-title step
        ov.handle_key(key(KeyCode::Enter)); // empty translated title → volume step
        let Overlay::Import(st) = &ov else {
            panic!("overlay changed variant")
        };
        assert!(st.target_project().is_some());
        assert_eq!(st.step, ImportStep::Volume);
        assert_eq!(st.vol, 3, "should pre-pick one past the highest volume");
    }

    /// A manually adjusted volume must survive going back and forward again.
    #[test]
    fn user_chosen_volume_is_not_overridden_by_the_suggestion() {
        let existing = ProjectRef {
            slug: crate::app::slugify("Cursed Blade"),
            title: "Cursed Blade".to_string(),
            volumes: vec![(1, 12)],
            target_language: TargetLanguage::Thai,
        };
        let mut ov = wizard(vec![existing]);
        if let Overlay::Import(st) = &mut ov {
            st.step = ImportStep::Name;
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
            st.step = ImportStep::Name;
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
            st.step = ImportStep::Name;
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
        let existing = ProjectRef {
            slug: crate::app::slugify("Cursed Blade"),
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
        for step in ImportStep::ALL {
            st.step = step;
            let ov = Overlay::Import(st.clone());
            let glyphs: String = render_overlay(&ov, 80, 26).0.concat();
            assert!(
                !glyphs.contains('\u{0E33}'),
                "raw SARA AM leaked into wizard step {step:?}"
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

        let st = SettingsState::for_test(settings_defs::index_of(SField::PreferredLanguageField));
        assert_eq!(st.tab, SettingsTab::Pipeline);
        let mut st = st;
        // The System One block is appended to this tab, so walking it reaches
        // every Pipeline row and then wraps rather than spilling into the next.
        let pipeline = Group::Pipeline.fields();
        for expected in pipeline.iter().skip(1) {
            st.next_field();
            assert_eq!(st.field, *expected);
        }
        st.next_field();
        assert_eq!(
            st.field, pipeline[0],
            "pipeline field nav wraps within the tab"
        );

        let appearance = Group::Appearance.fields();
        let mut st = SettingsState::for_test(appearance[0]);
        assert_eq!(st.tab, SettingsTab::Appearance);
        st.next_field();
        assert_eq!(st.field, appearance[1]);
        st.next_field();
        assert_eq!(
            st.field, appearance[0],
            "appearance field nav wraps within the tab"
        );

        let mut st = SettingsState::for_test(0);
        st.tab = SettingsTab::Account;
        assert!(!st.tab_has_fields());
    }

    #[test]
    fn review_gate_mode_cycles_and_reaches_save_action() {
        use crate::model::{DecisionsProvider, ReviewGateMode};

        let mut st = SettingsState::for_test(settings_defs::index_of(SField::GateMode));
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

        let mut st = SettingsState::for_test(settings_defs::index_of(SField::GateProvider));
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
            let field = settings_defs::ORDER[settings_defs::index_of(SField::FeatAudit) as usize + offset].field;
            assert_eq!(
                settings_defs::feature_of(field),
                Some(feature),
                "settings row {} must be the {feature:?} toggle",
                settings_defs::index_of(SField::FeatAudit) as usize + offset
            );
        }
        assert_eq!(
            settings_defs::ORDER[settings_defs::index_of(SField::FeatAudit) as usize + SystemOneFeature::ALL.len()].field,
            SField::UpdateModeField,
            "the feature block must end where the Appearance tab begins"
        );
    }

    #[test]
    fn master_switch_and_each_feature_toggle_independently() {
        use crate::model::SystemOneFeature;

        let mut st = SettingsState::for_test(settings_defs::index_of(SField::SystemOneEnabled));
        assert!(!st.system_one.enabled);
        st.cycle(true);
        assert!(st.system_one.enabled);

        // Every feature starts armed, so the suite follows the master switch.
        assert!(SystemOneFeature::ALL.iter().all(|f| st.system_one.feature(*f)));

        st.focus(settings_defs::index_of(SField::FeatAudit)); // Semantic audit
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
        let mut st = SettingsState::for_test(settings_defs::index_of(SField::OrchProvider));
        st.cycle(true); // OpenRouter → Tokenrouter
        st.cycle(true); // Tokenrouter → Google
        st.cycle(true); // Google → Cloudflare
        st.cycle(true); // Cloudflare → Codex
        assert_eq!(
            st.models.orchestrator.provider,
            crate::model::Provider::Codex
        );
        assert!(CODEX_MODELS.contains(&st.models.orchestrator.model.as_str()));

        st.focus(settings_defs::index_of(SField::OrchModel));
        assert!(st.is_codex_model());
        assert!(!st.current_is_editable_text(), "Codex model is a picker");
        let before = st.models.orchestrator.model.clone();
        st.cycle(true);
        assert_ne!(st.models.orchestrator.model, before);
        assert!(CODEX_MODELS.contains(&st.models.orchestrator.model.as_str()));
    }

    #[test]
    fn provider_cycle_restores_remembered_model_for_each_provider() {
        let mut st = SettingsState::for_test(settings_defs::index_of(SField::OrchProvider));
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
        for field in 0..settings_fields() {
            let ov = Overlay::Settings(SettingsState::for_test(field));
            let (lines, _) = render_overlay(&ov, 80, 24);
            let glyphs: String = lines.concat();
            if field == settings_defs::index_of(SField::ContinuitySentences) {
                assert!(glyphs.contains("Continuity sentences"));
            }
            if field == settings_defs::index_of(SField::ParallelLookahead) {
                assert!(glyphs.contains("Parallel lookahead"));
                // The focused row's help is shown in the footer, and it comes
                // from the registry rather than from the renderer. Only the
                // opening is asserted: a full sentence does not fit at 80
                // columns and is trimmed, which is correct.
                let help = settings_defs::at(field).unwrap().help;
                let opening: String = help.chars().take(24).collect();
                assert!(
                    glyphs.contains(&opening),
                    "the focused row's help should be on screen: {opening:?}"
                );
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

    /// The wizard's forward action survives at every width it can be shown at.
    ///
    /// This used to be a hand-fitted status line that chose between phrasings
    /// to avoid truncating. The footer is a real button row now, which drops
    /// buttons from the left and keeps the primary one, so the property is the
    /// same but the mechanism no longer needs to know about phrasings.
    #[test]
    fn the_wizards_forward_action_survives_every_width() {
        let mut st = ImportState::new(
            vec![(PathBuf::from("book.epub"), 1_000)],
            vec![],
            TargetLanguage::Thai,
        );
        st.name = "ดาบคำสาป".to_string();
        for step in [
            ImportStep::Pick,
            ImportStep::Name,
            ImportStep::Volume,
            ImportStep::Synopsis,
        ] {
            st.step = step;
            let ov = Overlay::Import(st.clone());
            for w in [50u16, 66, 80, 120] {
                let (_, zones) = render_overlay(&ov, w, 26);
                assert!(
                    zones.contains(ZoneId::button(DIALOG_CONFIRM)),
                    "{step:?} at {w}: the forward action was dropped"
                );
                // Zone bounds rather than a character count: a wide glyph
                // occupies two cells but yields one symbol, so counting
                // characters under-reads any row carrying CJK or Thai.
                for (rect, id) in zones.all() {
                    assert!(
                        rect.x + rect.width <= w && rect.y + rect.height <= 26,
                        "{step:?} at {w}: {id:?} at {rect:?} escaped"
                    );
                }
            }
        }
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

