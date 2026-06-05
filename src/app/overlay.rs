//! src/app/overlay.rs — transient overlays drawn LAST over a `Clear`:
//! the import wizard, settings, the activity log, the help key-table, the command
//! palette, and a generic confirm modal.
//!
//! `is_input_capturing()` is the load-bearing rule: when a text field is focused
//! the single-letter global keys (`q`, `l`, `?`, …) are suppressed so the user can
//! type freely. The App router consults this before falling through to globals.

use std::path::PathBuf;

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::model::{AppConfig, LogLevel, ThemeId};
use crate::theme::{self, ALL_THEMES, Theme};
use crate::ui::layout::{centered_modal, centered_pct};
use crate::ui::text::truncate_cols;
use crate::ui::widgets::render_gauge;

use super::{Action, Screen, slugify};

// ============================================================================
// STATE STRUCTS
// ============================================================================

/// The 3-step import wizard: pick epub → name + slug preview → volume.
#[derive(Debug, Clone)]
pub struct ImportState {
    /// 0 = pick, 1 = name, 2 = volume, 3 = importing (gauge).
    pub step: u8,
    pub epubs: Vec<PathBuf>,
    pub sel: usize,
    pub name: String,
    pub vol: u32,
    /// Live preprocessing progress (done, total, label) once the import starts.
    pub progress: Option<(usize, usize, String)>,
}

impl ImportState {
    fn new(epubs: Vec<PathBuf>) -> Self {
        // Seed the name field from the first epub's stem for a friendly default.
        let name = epubs
            .first()
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .map(prettify_stem)
            .unwrap_or_default();
        Self {
            step: 0,
            epubs,
            sel: 0,
            name,
            vol: 1,
            progress: None,
        }
    }

    fn selected_epub(&self) -> Option<&PathBuf> {
        self.epubs.get(self.sel)
    }
}

/// Settings: editable model ids + base URL + an api-key-present indicator.
#[derive(Debug, Clone)]
pub struct SettingsState {
    pub base_url: String,
    pub orchestrator: String,
    pub translator: String,
    pub reviewer: String,
    pub api_key_present: bool,
    /// Which field is focused (0..=3).
    pub field: u8,
}

impl SettingsState {
    fn from_cfg(cfg: &AppConfig) -> Self {
        Self {
            base_url: cfg.base_url.clone(),
            orchestrator: cfg.models.orchestrator.clone(),
            translator: cfg.models.translator.clone(),
            reviewer: cfg.models.reviewer.clone(),
            api_key_present: crate::config::resolve_api_key(cfg).is_some(),
            field: 0,
        }
    }

    fn field_mut(&mut self) -> &mut String {
        match self.field {
            0 => &mut self.base_url,
            1 => &mut self.orchestrator,
            2 => &mut self.translator,
            _ => &mut self.reviewer,
        }
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

/// A generic confirm modal wrapping the action to run on confirm.
#[derive(Debug, Clone)]
pub struct Dialog {
    pub title: String,
    pub body: String,
    pub confirm: Action,
}

// ============================================================================
// OVERLAY
// ============================================================================

#[derive(Debug, Clone)]
pub enum Overlay {
    None,
    Import(ImportState),
    Settings(SettingsState),
    /// Live-preview color theme picker.
    Theme(ThemePickerState),
    /// Activity log; the `u16` is the scroll-back offset (0 = newest tail).
    Log(u16),
    /// Keybinding reference; the `u16` is the vertical scroll offset.
    Help(u16),
    Palette(PaletteState),
    Modal(Dialog),
}

impl Overlay {
    // ---- constructors -----------------------------------------------------

    pub fn import(epubs: Vec<PathBuf>) -> Self {
        Overlay::Import(ImportState::new(epubs))
    }

    pub fn settings(cfg: &AppConfig) -> Self {
        Overlay::Settings(SettingsState::from_cfg(cfg))
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

    pub fn confirm(title: impl Into<String>, body: impl Into<String>, confirm: Action) -> Self {
        Overlay::Modal(Dialog {
            title: title.into(),
            body: body.into(),
            confirm,
        })
    }

    /// A settings overlay built from defaults — used by the palette which has no
    /// `&AppConfig` handle; the App swaps in the real config field values on show.
    fn settings_placeholder() -> Self {
        Overlay::Settings(SettingsState {
            base_url: String::new(),
            orchestrator: String::new(),
            translator: String::new(),
            reviewer: String::new(),
            api_key_present: false,
            field: 0,
        })
    }

    // ---- import progress passthrough (called from App::on_app_event) ------

    pub fn set_import_progress(&mut self, done: usize, total: usize, label: &str) {
        if let Overlay::Import(st) = self {
            st.step = 3;
            st.progress = Some((done, total, label.to_string()));
        }
    }

    // ---- input capture rule -----------------------------------------------

    /// True when a text field is focused → suppress single-letter globals.
    #[allow(dead_code)]
    pub fn is_input_capturing(&self) -> bool {
        match self {
            Overlay::Import(st) => st.step == 1, // name field
            Overlay::Settings(_) => true,        // always editing a field
            Overlay::Palette(_) => true,         // query field
            _ => false,
        }
    }

    // ---- key handling ------------------------------------------------------

    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        match self {
            Overlay::None => Action::None,
            Overlay::Import(_) => self.handle_import_key(key),
            Overlay::Settings(_) => self.handle_settings_key(key),
            Overlay::Theme(_) => self.handle_theme_key(key),
            Overlay::Palette(_) => self.handle_palette_key(key),
            Overlay::Modal(_) => self.handle_modal_key(key),
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

    fn handle_import_key(&mut self, key: KeyEvent) -> Action {
        let Overlay::Import(st) = self else {
            return Action::None;
        };
        match st.step {
            // ---- step 0: pick epub ----
            0 => match key.code {
                KeyCode::Esc => Action::CloseOverlay,
                KeyCode::Up | KeyCode::Char('k') => {
                    if st.sel > 0 {
                        st.sel -= 1;
                    }
                    Action::None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if st.sel + 1 < st.epubs.len() {
                        st.sel += 1;
                    }
                    Action::None
                }
                KeyCode::Enter => {
                    if st.epubs.is_empty() {
                        Action::CloseOverlay
                    } else {
                        // Refresh the name default from the chosen file.
                        if st.name.trim().is_empty()
                            && let Some(stem) = st
                                .selected_epub()
                                .and_then(|p| p.file_stem())
                                .and_then(|s| s.to_str())
                        {
                            st.name = prettify_stem(stem);
                        }
                        st.step = 1;
                        Action::None
                    }
                }
                _ => Action::None,
            },
            // ---- step 1: name (text field) ----
            1 => match key.code {
                KeyCode::Esc => {
                    st.step = 0;
                    Action::None
                }
                KeyCode::Enter | KeyCode::Tab => {
                    if st.name.trim().is_empty() {
                        Action::None
                    } else {
                        st.step = 2;
                        Action::None
                    }
                }
                KeyCode::Backspace => {
                    st.name.pop();
                    Action::None
                }
                KeyCode::Char(c) => {
                    st.name.push(c);
                    Action::None
                }
                _ => Action::None,
            },
            // ---- step 2: volume ----
            2 => match key.code {
                KeyCode::Esc => {
                    st.step = 1;
                    Action::None
                }
                KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('+') | KeyCode::Right => {
                    st.vol = st.vol.saturating_add(1).min(999);
                    Action::None
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('-') | KeyCode::Left => {
                    st.vol = st.vol.saturating_sub(1).max(1);
                    Action::None
                }
                KeyCode::Char(d @ '0'..='9') => {
                    // Type a number directly.
                    let digit = d as u32 - '0' as u32;
                    st.vol = (st.vol.saturating_mul(10).saturating_add(digit)).clamp(1, 999);
                    Action::None
                }
                KeyCode::Backspace => {
                    st.vol = (st.vol / 10).max(1);
                    Action::None
                }
                KeyCode::Enter => {
                    let epub = st.selected_epub().cloned().unwrap_or_default();
                    let title = st.name.trim().to_string();
                    let vol = st.vol.max(1);
                    st.step = 3;
                    st.progress = Some((0, 0, "starting".to_string()));
                    Action::ImportEpub { epub, title, vol }
                }
                _ => Action::None,
            },
            // ---- step 3: importing (gauge) — Esc cancels by closing ----
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
            // Honor the advertised "Ctrl-T to change" here, since an open overlay
            // swallows keys before the global Ctrl-T router is reached.
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Action::show_overlay(Overlay::theme_placeholder())
            }
            KeyCode::Enter => Action::SaveSettings {
                base_url: st.base_url.clone(),
                orchestrator: st.orchestrator.clone(),
                translator: st.translator.clone(),
                reviewer: st.reviewer.clone(),
            },
            KeyCode::Tab | KeyCode::Down => {
                st.field = (st.field + 1) % 4;
                Action::None
            }
            KeyCode::Up | KeyCode::BackTab => {
                st.field = (st.field + 3) % 4;
                Action::None
            }
            KeyCode::Backspace => {
                st.field_mut().pop();
                Action::None
            }
            KeyCode::Char(c) => {
                st.field_mut().push(c);
                Action::None
            }
            _ => Action::None,
        }
    }

    fn handle_theme_key(&mut self, key: KeyEvent) -> Action {
        let Overlay::Theme(st) = self else {
            return Action::None;
        };
        match key.code {
            // Move + live-preview (App applies PreviewTheme to `app.theme`).
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
            // Commit: persist + close.
            KeyCode::Enter => Action::SaveTheme(st.current()),
            // Abandon: restore the previously-saved theme + close.
            KeyCode::Esc | KeyCode::Char('q') => Action::CancelTheme,
            _ => Action::None,
        }
    }

    fn handle_palette_key(&mut self, key: KeyEvent) -> Action {
        let Overlay::Palette(st) = self else {
            return Action::None;
        };
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
            KeyCode::Backspace => {
                st.query.pop();
                st.sel = 0;
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
            KeyCode::Char(c) => {
                st.query.push(c);
                st.sel = 0;
                Action::None
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
                // Always dismiss on confirm; the wrapped action then runs (some
                // actions — StopRun, the shelf placeholders — don't self-close).
                *self = Overlay::None;
                action
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => Action::CloseOverlay,
            _ => Action::None,
        }
    }

    // ---- hints -------------------------------------------------------------

    pub fn hints(&self) -> &'static [(&'static str, &'static str)] {
        match self {
            Overlay::Import(st) => match st.step {
                0 => &[("↑↓", "pick"), ("↵", "next"), ("Esc", "cancel")],
                1 => &[("type", "name"), ("↵/Tab", "next"), ("Esc", "back")],
                2 => &[("↑↓", "volume"), ("↵", "import"), ("Esc", "back")],
                _ => &[("Esc", "close")],
            },
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
            Overlay::Modal(_) => &[("y", "confirm"), ("n/Esc", "cancel")],
            Overlay::None => &[],
        }
    }

    // ---- render ------------------------------------------------------------

    pub fn render(
        &self,
        f: &mut Frame,
        area: Rect,
        theme: &Theme,
        cfg: &AppConfig,
        log: &[(LogLevel, String)],
    ) {
        match self {
            Overlay::None => {}
            Overlay::Import(st) => self.render_import(f, area, theme, st),
            Overlay::Settings(st) => self.render_settings(f, area, theme, cfg, st),
            Overlay::Theme(st) => self.render_theme(f, area, theme, st),
            Overlay::Palette(st) => self.render_palette(f, area, theme, st),
            Overlay::Log(off) => self.render_log(f, area, theme, log, *off),
            Overlay::Help(off) => self.render_help(f, area, theme, *off),
            Overlay::Modal(dlg) => self.render_modal(f, area, theme, dlg),
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

    fn render_import(&self, f: &mut Frame, area: Rect, theme: &Theme, st: &ImportState) {
        let modal = centered_modal(74, 18, area);
        f.render_widget(Clear, modal);
        let step_no = st.step.min(2) + 1;
        let title = format!("Import EPUB — step {step_no} / 3");
        let block = self.modal_block(&title, theme);
        let inner = block.inner(modal);
        f.render_widget(block, modal);

        match st.step {
            0 => self.render_import_pick(f, inner, theme, st),
            1 => self.render_import_name(f, inner, theme, st),
            2 => self.render_import_volume(f, inner, theme, st),
            _ => self.render_import_progress(f, inner, theme, st),
        }
    }

    fn render_import_pick(&self, f: &mut Frame, area: Rect, theme: &Theme, st: &ImportState) {
        if st.epubs.is_empty() {
            let p = Paragraph::new(vec![
                Line::raw(""),
                Line::from(Span::styled(
                    "  No .epub files found in this folder.",
                    Style::default().fg(theme.ink_soft),
                )),
                Line::raw(""),
                Line::from(Span::styled(
                    "  Drop an .epub here and press r on the Shelf to rescan.",
                    Style::default().fg(theme.ink_faint),
                )),
            ])
            .style(Style::default().bg(theme.bg_panel));
            f.render_widget(p, area);
            return;
        }
        let mut lines = vec![
            Line::from(Span::styled(
                "  Choose a source EPUB:",
                Style::default().fg(theme.ink_soft),
            )),
            Line::raw(""),
        ];
        for (i, p) in st.epubs.iter().enumerate() {
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
                Span::styled(
                    truncate_cols(name, area.width.saturating_sub(6) as usize),
                    style,
                ),
            ]));
        }
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme.bg_panel)),
            area,
        );
    }

    fn render_import_name(&self, f: &mut Frame, area: Rect, theme: &Theme, st: &ImportState) {
        let src = st
            .selected_epub()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("?");
        let slug = slugify(&st.name);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // source
                Constraint::Length(1), // gap
                Constraint::Length(1), // label
                Constraint::Length(3), // boxed input
                Constraint::Length(1), // slug
                Constraint::Min(0),
            ])
            .split(area);

        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  Source:  ", Style::default().fg(theme.ink_faint)),
                Span::styled(src, Style::default().fg(theme.ink_soft)),
            ]))
            .style(Style::default().bg(theme.bg_panel)),
            rows[0],
        );
        f.render_widget(
            Paragraph::new(Span::styled(
                "  Project name",
                Style::default().fg(theme.ink_soft),
            ))
            .style(Style::default().bg(theme.bg_panel)),
            rows[2],
        );

        let input_block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(theme.accent_soft))
            .style(Style::default().bg(theme.bg_inset));
        let caret_line = Line::from(vec![
            Span::styled(st.name.clone(), Style::default().fg(theme.ink)),
            Span::styled("▏", Style::default().fg(theme.stream_cursor)),
        ]);
        f.render_widget(
            Paragraph::new(caret_line).block(input_block),
            indent(rows[3], 2),
        );
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  Slug:  ", Style::default().fg(theme.ink_faint)),
                Span::styled(slug, Style::default().fg(theme.accent_soft)),
            ]))
            .style(Style::default().bg(theme.bg_panel)),
            rows[4],
        );
    }

    fn render_import_volume(&self, f: &mut Frame, area: Rect, theme: &Theme, st: &ImportState) {
        let lines = vec![
            Line::raw(""),
            Line::from(Span::styled(
                format!("  Project:  {}", st.name.trim()),
                Style::default().fg(theme.ink_soft),
            )),
            Line::raw(""),
            Line::from(vec![
                Span::styled("  Volume   ", Style::default().fg(theme.ink_soft)),
                Span::styled(
                    format!("Vol.{:02}", st.vol),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::raw(""),
            Line::from(Span::styled(
                "  ↑↓ adjust · type a number · Enter to import",
                Style::default().fg(theme.ink_faint),
            )),
        ];
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
                Constraint::Min(0),
            ])
            .split(area);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  Preprocessing  ", Style::default().fg(theme.ink_soft)),
                Span::styled(label, Style::default().fg(theme.accent_soft)),
            ]))
            .style(Style::default().bg(theme.bg_panel)),
            rows[1],
        );
        render_gauge(f, indent(rows[2], 2), done, total.max(1), theme);
    }

    fn render_settings(
        &self,
        f: &mut Frame,
        area: Rect,
        theme: &Theme,
        cfg: &AppConfig,
        st: &SettingsState,
    ) {
        let modal = centered_modal(72, 16, area);
        f.render_widget(Clear, modal);
        let block = self.modal_block("Settings", theme);
        let inner = block.inner(modal);
        f.render_widget(block, modal);

        let fields = [
            ("Base URL", st.base_url.as_str(), 0u8),
            ("Orchestrator model", st.orchestrator.as_str(), 1),
            ("Translator model", st.translator.as_str(), 2),
            ("Reviewer model", st.reviewer.as_str(), 3),
        ];
        let mut lines = vec![Line::raw("")];
        for (label, value, idx) in fields {
            let focused = st.field == idx;
            let marker = if focused { theme::SELECT_BAR } else { ' ' };
            let value_style = if focused {
                Style::default().fg(theme.ink).bg(theme.accent_bg)
            } else {
                Style::default().fg(theme.ink_soft)
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {marker} "), Style::default().fg(theme.accent)),
                Span::styled(format!("{label:<20}"), Style::default().fg(theme.ink_faint)),
                Span::styled(
                    truncate_cols(value, area.width.saturating_sub(26) as usize),
                    value_style,
                ),
                if focused {
                    Span::styled("▏", Style::default().fg(theme.stream_cursor))
                } else {
                    Span::raw("")
                },
            ]));
        }
        lines.push(Line::raw(""));
        let key_state = if st.api_key_present {
            Span::styled("● present", Style::default().fg(theme.status_done))
        } else {
            Span::styled(
                "○ missing — set HONYA_API_KEY",
                Style::default().fg(theme.status_warn),
            )
        };
        lines.push(Line::from(vec![
            Span::styled(
                "   API key             ",
                Style::default().fg(theme.ink_faint),
            ),
            key_state,
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                "   Theme               ",
                Style::default().fg(theme.ink_faint),
            ),
            Span::styled(cfg.theme.label(), Style::default().fg(theme.accent)),
            Span::styled(
                "   Ctrl-T to change",
                Style::default().fg(theme.ink_faint),
            ),
        ]));
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "   Set HONYA_API_KEY or OPENROUTER_API_KEY in your environment.",
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
        let start = if st.sel >= cap {
            st.sel + 1 - cap
        } else {
            0
        };
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

        // Swatch row: ● accent ◐ working ● done ✗ failed ‖ warn ▣ image.
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

        // Query line.
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  : ", Style::default().fg(theme.accent)),
                Span::styled(st.query.clone(), Style::default().fg(theme.ink)),
                Span::styled("▏", Style::default().fg(theme.stream_cursor)),
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
                    (": / Ctrl-P", "command palette"),
                    ("Ctrl-T", "theme picker"),
                    ("l / `", "activity log"),
                    ("Esc", "back / close overlay"),
                    ("q", "quit        Ctrl-C hard quit"),
                ],
            ),
            (
                "Shelf 書架",
                &[
                    ("↵", "open project"),
                    ("i", "import epub"),
                    ("d / R / r", "delete · rename · rescan"),
                ],
            ),
            (
                "Project 棚",
                &[
                    ("↵", "read chapter"),
                    ("t / T", "translate chapter · whole volume"),
                    ("Space / a", "select · translate selected"),
                    ("h / l", "collapse · expand / focus"),
                    ("e", "edit context (Lexicon)"),
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
                    ("y", "copy visible Thai"),
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

    fn render_modal(&self, f: &mut Frame, area: Rect, theme: &Theme, dlg: &Dialog) {
        let modal = centered_modal(56, 9, area);
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
                format!("  {}", dlg.body),
                Style::default().fg(theme.ink_soft),
            ))
            .wrap(Wrap { trim: false })
            .style(Style::default().bg(theme.bg_panel)),
            rows[0],
        );
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  [ y ] confirm", Style::default().fg(theme.accent)),
                Span::raw("     "),
                Span::styled("[ n / Esc ] cancel", Style::default().fg(theme.ink_faint)),
            ]))
            .style(Style::default().bg(theme.bg_panel)),
            rows[1],
        );
    }
}

// ============================================================================
// HELPERS
// ============================================================================

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
