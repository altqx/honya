//! src/app/lexicon.rs — the Lexicon (5 辞): editor for the tool-mutated context
//! files. Tab cycles Glossary ↔ Characters ↔ Style. Entries can be added / edited /
//! deleted inline, persisting via workspace::{glossary,characters}::upsert.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::model::{Character, GlossaryTerm};
use crate::theme::{self, Theme};
use crate::ui::text::{pad_to_cols, truncate_cols};
use crate::workspace::Workspace;

use super::Action;
use super::overlay::Overlay;

const SUB_GLOSSARY: u8 = 0;
const SUB_CHARACTERS: u8 = 1;
const SUB_STYLE: u8 = 2;

/// An inline edit form. The fields are generic key→value pairs so the same form
/// type backs both glossary terms and characters; `kind` says which it commits to.
#[derive(Debug, Clone)]
pub struct EditForm {
    kind: u8,
    /// Field labels + current values, in tab order.
    fields: Vec<(&'static str, String)>,
    field: usize,
    /// True for a brand-new entry (vs editing an existing one).
    is_new: bool,
}

impl EditForm {
    fn new_glossary(seed: Option<&GlossaryTerm>) -> Self {
        let g = seed.cloned().unwrap_or(GlossaryTerm {
            jp_term: String::new(),
            thai_term: String::new(),
            romaji: None,
            category: None,
            gloss: None,
            do_not_translate: None,
            first_seen_chapter: None,
        });
        Self {
            kind: SUB_GLOSSARY,
            fields: vec![
                ("JP term", g.jp_term),
                ("Thai term", g.thai_term),
                ("Category", g.category.unwrap_or_default()),
                ("Gloss", g.gloss.unwrap_or_default()),
            ],
            field: 0,
            is_new: seed.is_none(),
        }
    }

    fn new_character(seed: Option<&Character>) -> Self {
        let c = seed.cloned();
        Self {
            kind: SUB_CHARACTERS,
            fields: vec![
                (
                    "JP name",
                    c.as_ref().map(|x| x.jp_name.clone()).unwrap_or_default(),
                ),
                (
                    "Thai name",
                    c.as_ref().map(|x| x.thai_name.clone()).unwrap_or_default(),
                ),
                (
                    "Gender",
                    c.as_ref()
                        .and_then(|x| x.gender.clone())
                        .unwrap_or_default(),
                ),
                (
                    "Notes",
                    c.as_ref().and_then(|x| x.notes.clone()).unwrap_or_default(),
                ),
            ],
            field: 0,
            is_new: seed.is_none(),
        }
    }

    fn current_mut(&mut self) -> &mut String {
        &mut self.fields[self.field].1
    }

    fn next_field(&mut self) {
        self.field = (self.field + 1) % self.fields.len();
    }

    fn prev_field(&mut self) {
        self.field = (self.field + self.fields.len() - 1) % self.fields.len();
    }

    fn to_glossary(&self) -> GlossaryTerm {
        let get = |i: usize| self.fields.get(i).map(|f| f.1.clone()).unwrap_or_default();
        GlossaryTerm {
            jp_term: get(0),
            thai_term: get(1),
            romaji: None,
            category: opt(get(2)),
            gloss: opt(get(3)),
            do_not_translate: None,
            first_seen_chapter: None,
        }
    }

    fn to_character(&self) -> Character {
        let get = |i: usize| self.fields.get(i).map(|f| f.1.clone()).unwrap_or_default();
        let jp = get(0);
        Character {
            id: slug_id(&jp),
            jp_name: jp,
            thai_name: get(1),
            romaji: None,
            gender: opt(get(2)),
            honorific: None,
            speech_style: None,
            relationships: Vec::new(),
            notes: opt(get(3)),
            first_seen_chapter: None,
        }
    }
}

pub struct LexiconScreen {
    sub: u8,
    list: ListState,
    editing: Option<EditForm>,
    filter: String,
    /// True while the `/` search field is capturing input.
    searching: bool,
}

impl LexiconScreen {
    pub fn new() -> Self {
        let mut list = ListState::default();
        list.select(Some(0));
        Self {
            sub: SUB_GLOSSARY,
            list,
            editing: None,
            filter: String::new(),
            searching: false,
        }
    }

    pub fn reset(&mut self) {
        self.editing = None;
        self.searching = false;
        self.filter.clear();
        self.list.select(Some(0));
    }

    /// True while a text field owns the keyboard. The App router consults this to
    /// suppress single-letter globals (`q`, `l`, …) so the user can type freely.
    pub fn is_capturing(&self) -> bool {
        self.editing.is_some() || self.searching
    }

    fn glossary(&self, ws: &Workspace) -> Vec<GlossaryTerm> {
        let all = crate::workspace::glossary::load(ws);
        if self.filter.is_empty() {
            all
        } else {
            let q = self.filter.to_lowercase();
            all.into_iter()
                .filter(|t| {
                    t.jp_term.to_lowercase().contains(&q)
                        || t.thai_term.to_lowercase().contains(&q)
                        || t.category
                            .as_deref()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains(&q)
                })
                .collect()
        }
    }

    fn characters(&self, ws: &Workspace) -> Vec<Character> {
        let all = crate::workspace::characters::load(ws);
        if self.filter.is_empty() {
            all
        } else {
            let q = self.filter.to_lowercase();
            all.into_iter()
                .filter(|c| {
                    c.jp_name.to_lowercase().contains(&q) || c.thai_name.to_lowercase().contains(&q)
                })
                .collect()
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, ws: Option<&Workspace>) -> Action {
        // ---- edit mode owns all keys ----
        if self.editing.is_some() {
            return self.handle_edit_key(key, ws);
        }

        // ---- search field owns text input ----
        if self.searching {
            match key.code {
                KeyCode::Esc => {
                    self.searching = false;
                    self.filter.clear();
                }
                KeyCode::Enter => self.searching = false,
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(c) => self.filter.push(c),
                _ => {}
            }
            return Action::None;
        }

        match key.code {
            KeyCode::Tab => {
                self.sub = (self.sub + 1) % 3;
                self.list.select(Some(0));
                Action::None
            }
            KeyCode::BackTab => {
                self.sub = (self.sub + 2) % 3;
                self.list.select(Some(0));
                Action::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_sel(ws, -1);
                Action::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_sel(ws, 1);
                Action::None
            }
            KeyCode::Char('/') => {
                self.searching = true;
                self.filter.clear();
                Action::None
            }
            KeyCode::Char('n') => {
                if ws.is_some() {
                    self.editing = Some(match self.sub {
                        SUB_CHARACTERS => EditForm::new_character(None),
                        SUB_STYLE => EditForm {
                            kind: SUB_STYLE,
                            fields: vec![("Style note", String::new())],
                            field: 0,
                            is_new: true,
                        },
                        _ => EditForm::new_glossary(None),
                    });
                }
                Action::None
            }
            KeyCode::Enter | KeyCode::Char('e') => {
                self.begin_edit(ws);
                Action::None
            }
            KeyCode::Char('d') => self.begin_delete(ws),
            _ => Action::None,
        }
    }

    fn handle_edit_key(&mut self, key: KeyEvent, ws: Option<&Workspace>) -> Action {
        let Some(form) = self.editing.as_mut() else {
            return Action::None;
        };
        match key.code {
            KeyCode::Esc => {
                self.editing = None;
                Action::None
            }
            KeyCode::Tab | KeyCode::Down => {
                form.next_field();
                Action::None
            }
            KeyCode::BackTab | KeyCode::Up => {
                form.prev_field();
                Action::None
            }
            KeyCode::Backspace => {
                form.current_mut().pop();
                Action::None
            }
            KeyCode::Enter => {
                // Commit on Enter from the last field, else advance.
                if form.field + 1 < form.fields.len() {
                    form.next_field();
                    Action::None
                } else {
                    self.commit_edit(ws)
                }
            }
            KeyCode::Char(c) => {
                form.current_mut().push(c);
                Action::None
            }
            _ => Action::None,
        }
    }

    fn commit_edit(&mut self, ws: Option<&Workspace>) -> Action {
        let Some(form) = self.editing.take() else {
            return Action::None;
        };
        let Some(ws) = ws else { return Action::None };
        let result = match form.kind {
            SUB_CHARACTERS => crate::workspace::characters::upsert(ws, form.to_character()),
            SUB_STYLE => crate::workspace::style::append_note(
                ws,
                form.fields.first().map(|f| f.1.as_str()).unwrap_or(""),
            ),
            _ => crate::workspace::glossary::upsert(ws, form.to_glossary()),
        };
        // Surface success/failure via a confirm-less toast modal-free path: we use
        // a transient modal only on error; success is reflected by the refreshed list.
        match result {
            Ok(()) => Action::None,
            Err(e) => Action::show_overlay(Overlay::confirm(
                "Save failed",
                format!("Could not write the entry: {e}"),
                Action::CloseOverlay,
            )),
        }
    }

    fn begin_edit(&mut self, ws: Option<&Workspace>) {
        let Some(ws) = ws else { return };
        let idx = self.list.selected().unwrap_or(0);
        match self.sub {
            SUB_CHARACTERS => {
                let list = self.characters(ws);
                self.editing = Some(EditForm::new_character(list.get(idx)));
            }
            SUB_STYLE => {
                // Style is a free-form file; offer a single-field note editor.
                self.editing = Some(EditForm {
                    kind: SUB_STYLE,
                    fields: vec![("Style note", String::new())],
                    field: 0,
                    is_new: true,
                });
            }
            _ => {
                let list = self.glossary(ws);
                self.editing = Some(EditForm::new_glossary(list.get(idx)));
            }
        }
    }

    fn begin_delete(&mut self, ws: Option<&Workspace>) -> Action {
        let Some(ws) = ws else { return Action::None };
        let idx = self.list.selected().unwrap_or(0);
        // (display label, delete action) for the selected entry.
        let labelled: Option<(String, Action)> = match self.sub {
            SUB_CHARACTERS => self.characters(ws).get(idx).map(|c| {
                (
                    format!("{} → {}", c.jp_name, c.thai_name),
                    Action::DeleteCharacter { id: c.id.clone() },
                )
            }),
            // Style is append-only free-form prose; nothing structured to delete.
            SUB_STYLE => None,
            _ => self.glossary(ws).get(idx).map(|t| {
                (
                    format!("{} → {}", t.jp_term, t.thai_term),
                    Action::DeleteGlossary {
                        jp_term: t.jp_term.clone(),
                    },
                )
            }),
        };
        match labelled {
            Some((label, action)) => Action::show_overlay(Overlay::confirm(
                "Delete entry?",
                format!("Remove {label}."),
                action,
            )),
            None => Action::None,
        }
    }

    fn move_sel(&mut self, ws: Option<&Workspace>, delta: i32) {
        let len = match (ws, self.sub) {
            (Some(ws), SUB_CHARACTERS) => self.characters(ws).len(),
            (Some(ws), SUB_GLOSSARY) => self.glossary(ws).len(),
            _ => 0,
        };
        if len == 0 {
            self.list.select(Some(0));
            return;
        }
        let cur = self.list.selected().unwrap_or(0) as i32;
        let next = (cur + delta).rem_euclid(len as i32) as usize;
        self.list.select(Some(next));
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, ws: Option<&Workspace>, theme: &Theme) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(area);

        self.render_header(f, rows[0], ws, theme);
        self.render_table(f, rows[1], ws, theme);

        if self.editing.is_some() {
            self.render_edit(f, area, theme);
        }
    }

    fn render_header(&self, f: &mut Frame, area: Rect, ws: Option<&Workspace>, theme: &Theme) {
        let tabs = [
            ("Glossary", SUB_GLOSSARY),
            ("Characters", SUB_CHARACTERS),
            ("Style", SUB_STYLE),
        ];
        let mut spans = vec![Span::raw("  ")];
        for (label, id) in tabs {
            if id == self.sub {
                spans.push(Span::styled(
                    format!("〔 {label} 〕"),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::styled(
                    format!("  {label}  "),
                    Style::default().fg(theme.ink_faint),
                ));
            }
            spans.push(Span::raw(" "));
        }
        // Right side: filter + count.
        let count = match (ws, self.sub) {
            (Some(ws), SUB_GLOSSARY) => format!("{} terms", self.glossary(ws).len()),
            (Some(ws), SUB_CHARACTERS) => format!("{} characters", self.characters(ws).len()),
            _ => "—".to_string(),
        };
        let filter_str = if self.searching || !self.filter.is_empty() {
            format!(
                "/ filter: {}{}   ",
                self.filter,
                if self.searching { "▏" } else { "" }
            )
        } else {
            String::new()
        };
        let left = Line::from(spans);
        f.render_widget(
            Paragraph::new(left).style(Style::default().bg(theme.bg)),
            area,
        );
        let right = format!("{filter_str}({count})");
        let rw = crate::ui::text::col_width(&right) as u16;
        if area.width > rw + 2 {
            f.render_widget(
                Paragraph::new(Span::styled(right, Style::default().fg(theme.ink_faint)))
                    .style(Style::default().bg(theme.bg)),
                Rect {
                    x: area.x + area.width - rw - 1,
                    y: area.y,
                    width: rw,
                    height: 1,
                },
            );
        }
    }

    fn render_table(&mut self, f: &mut Frame, area: Rect, ws: Option<&Workspace>, theme: &Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(theme.rule))
            .style(Style::default().bg(theme.bg_panel));
        let inner = block.inner(area);
        f.render_widget(block, area);

        let Some(ws) = ws else {
            f.render_widget(
                Paragraph::new(Span::styled(
                    "  Open a project (Shelf → ↵) to edit its lexicon.",
                    Style::default().fg(theme.ink_faint),
                ))
                .style(Style::default().bg(theme.bg_panel)),
                inner,
            );
            return;
        };

        match self.sub {
            SUB_GLOSSARY => self.render_glossary_table(f, inner, ws, theme),
            SUB_CHARACTERS => self.render_characters_table(f, inner, ws, theme),
            _ => self.render_style(f, inner, ws, theme),
        }
    }

    fn render_glossary_table(&mut self, f: &mut Frame, area: Rect, ws: &Workspace, theme: &Theme) {
        let terms = self.glossary(ws);
        if self.list.selected().is_none_or(|s| s >= terms.len()) {
            self.list.select(Some(terms.len().saturating_sub(1)));
        }
        let sel = self.list.selected().unwrap_or(0);

        // Header row.
        let head = Line::from(Span::styled(
            format!(
                "   {} {} {} {}  Gloss",
                pad_to_cols("JP term", 12),
                pad_to_cols("Thai term", 16),
                pad_to_cols("Cat", 8),
                "DNT"
            ),
            Style::default().fg(theme.ink_faint),
        ));

        let mut items = vec![ListItem::new(head)];
        let gloss_w = area.width.saturating_sub(48).max(8) as usize;
        for (i, t) in terms.iter().enumerate() {
            let selected = i == sel;
            let bar = if selected { theme::SELECT_BAR } else { ' ' };
            let bg = if selected {
                theme.accent_bg
            } else {
                theme.bg_panel
            };
            let dnt = if t.do_not_translate.unwrap_or(false) {
                "✓"
            } else {
                "·"
            };
            items.push(ListItem::new(Line::from(vec![
                Span::styled(format!(" {bar} "), Style::default().fg(theme.accent).bg(bg)),
                Span::styled(
                    pad_to_cols(&t.jp_term, 12),
                    Style::default().fg(theme.ink).bg(bg),
                ),
                Span::styled(" ", Style::default().bg(bg)),
                Span::styled(
                    pad_to_cols(&t.thai_term, 16),
                    Style::default().fg(theme.th_text).bg(bg),
                ),
                Span::styled(" ", Style::default().bg(bg)),
                Span::styled(
                    pad_to_cols(t.category.as_deref().unwrap_or("—"), 8),
                    Style::default().fg(theme.ink_soft).bg(bg),
                ),
                Span::styled(
                    format!(" {dnt}   "),
                    Style::default().fg(theme.ink_faint).bg(bg),
                ),
                Span::styled(
                    truncate_cols(t.gloss.as_deref().unwrap_or(""), gloss_w),
                    Style::default().fg(theme.ink_soft).bg(bg),
                ),
            ])));
        }
        if terms.is_empty() {
            items.push(ListItem::new(Line::from(Span::styled(
                "   (no terms yet — n to add)",
                Style::default().fg(theme.ink_faint),
            ))));
        }
        f.render_widget(
            List::new(items).style(Style::default().bg(theme.bg_panel)),
            area,
        );
    }

    fn render_characters_table(
        &mut self,
        f: &mut Frame,
        area: Rect,
        ws: &Workspace,
        theme: &Theme,
    ) {
        let chars = self.characters(ws);
        if self.list.selected().is_none_or(|s| s >= chars.len()) {
            self.list.select(Some(chars.len().saturating_sub(1)));
        }
        let sel = self.list.selected().unwrap_or(0);

        let head = Line::from(Span::styled(
            format!(
                "   {} {} {}  Notes",
                pad_to_cols("JP name", 14),
                pad_to_cols("Thai name", 16),
                pad_to_cols("Gender", 8)
            ),
            Style::default().fg(theme.ink_faint),
        ));
        let mut items = vec![ListItem::new(head)];
        let notes_w = area.width.saturating_sub(46).max(8) as usize;
        for (i, c) in chars.iter().enumerate() {
            let selected = i == sel;
            let bar = if selected { theme::SELECT_BAR } else { ' ' };
            let bg = if selected {
                theme.accent_bg
            } else {
                theme.bg_panel
            };
            items.push(ListItem::new(Line::from(vec![
                Span::styled(format!(" {bar} "), Style::default().fg(theme.accent).bg(bg)),
                Span::styled(
                    pad_to_cols(&c.jp_name, 14),
                    Style::default().fg(theme.ink).bg(bg),
                ),
                Span::styled(" ", Style::default().bg(bg)),
                Span::styled(
                    pad_to_cols(&c.thai_name, 16),
                    Style::default().fg(theme.th_text).bg(bg),
                ),
                Span::styled(" ", Style::default().bg(bg)),
                Span::styled(
                    pad_to_cols(c.gender.as_deref().unwrap_or("—"), 8),
                    Style::default().fg(theme.ink_soft).bg(bg),
                ),
                Span::styled("  ", Style::default().bg(bg)),
                Span::styled(
                    truncate_cols(c.notes.as_deref().unwrap_or(""), notes_w),
                    Style::default().fg(theme.ink_soft).bg(bg),
                ),
            ])));
        }
        if chars.is_empty() {
            items.push(ListItem::new(Line::from(Span::styled(
                "   (no characters yet — n to add)",
                Style::default().fg(theme.ink_faint),
            ))));
        }
        f.render_widget(
            List::new(items).style(Style::default().bg(theme.bg_panel)),
            area,
        );
    }

    fn render_style(&self, f: &mut Frame, area: Rect, ws: &Workspace, theme: &Theme) {
        let body = std::fs::read_to_string(ws.style_md())
            .unwrap_or_else(|_| "STYLE.md not found.".to_string());
        let lines: Vec<Line> = body
            .split('\n')
            .take(area.height as usize)
            .map(|l| {
                Line::from(Span::styled(
                    truncate_cols(l, area.width.saturating_sub(2) as usize),
                    Style::default().fg(theme.ink_soft),
                ))
            })
            .collect();
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme.bg_panel)),
            area,
        );
    }

    fn render_edit(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        let Some(form) = self.editing.as_ref() else {
            return;
        };
        let modal = crate::ui::layout::centered_modal(60, (form.fields.len() as u16) * 2 + 6, area);
        f.render_widget(ratatui::widgets::Clear, modal);
        let title = if form.is_new {
            "New entry"
        } else {
            "Edit entry"
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(theme.accent))
            .title(Span::styled(
                format!(" {title} "),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.bg_panel));
        let inner = block.inner(modal);
        f.render_widget(block, modal);

        let mut lines = vec![Line::raw("")];
        for (i, (label, value)) in form.fields.iter().enumerate() {
            let focused = i == form.field;
            let marker = if focused { theme::SELECT_BAR } else { ' ' };
            let val_style = if focused {
                Style::default().fg(theme.ink).bg(theme.accent_bg)
            } else {
                Style::default().fg(theme.ink_soft)
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {marker} "), Style::default().fg(theme.accent)),
                Span::styled(pad_to_cols(label, 12), Style::default().fg(theme.ink_faint)),
                Span::styled(
                    truncate_cols(value, inner.width.saturating_sub(18) as usize),
                    val_style,
                ),
                if focused {
                    Span::styled("▏", Style::default().fg(theme.stream_cursor))
                } else {
                    Span::raw("")
                },
            ]));
            lines.push(Line::raw(""));
        }
        lines.push(Line::from(Span::styled(
            "   ↵ save · Tab next field · Esc cancel",
            Style::default().fg(theme.ink_faint),
        )));
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme.bg_panel)),
            inner,
        );
    }

    pub fn hints(&self) -> &'static [(&'static str, &'static str)] {
        if self.editing.is_some() {
            return &[("↵", "save"), ("Tab", "field"), ("Esc", "cancel")];
        }
        &[
            ("↵", "edit"),
            ("n", "new"),
            ("d", "del"),
            ("/", "search"),
            ("Tab", "section"),
        ]
    }
}

impl Default for LexiconScreen {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// HELPERS
// ============================================================================

fn opt(s: String) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// A stable id derived from a JP name (mirrors the workspace's slugify-jp rule:
/// non-ASCII preserved, ASCII lowered, separators collapsed).
fn slug_id(jp: &str) -> String {
    super::slugify(jp)
}
