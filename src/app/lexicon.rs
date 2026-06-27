//! Lexicon screen for editing glossary, character, and style context files.

use std::hash::{Hash, Hasher};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::model::{AltName, Character, GlossaryTerm, TermPolicy};
use crate::theme::{self, Theme};
use crate::ui::input::{self, EditOpts, Edited};
use crate::ui::mouse::{MouseGesture, MouseInput};
use crate::ui::text::{col_width, pad_to_cols, thai_display_safe, truncate_cols};
use crate::workspace::Workspace;

use super::Action;
use super::overlay::Overlay;

const SUB_GLOSSARY: u8 = 0;
const SUB_CHARACTERS: u8 = 1;
const SUB_STYLE: u8 = 2;

/// Generic inline edit form for glossary terms, characters, and style notes.
#[derive(Debug, Clone)]
pub struct EditForm {
    kind: u8,
    id: Option<String>,
    // Field labels + current values, in tab order.
    fields: Vec<(&'static str, String)>,
    field: usize,
    // Byte offset into the focused field.
    cursor: usize,
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
            policy: Some(TermPolicy::Preferred),
            forbidden_thai: Vec::new(),
            context_rule: None,
            protected: None,
            do_not_translate: None,
            first_seen_chapter: None,
        });
        let policy = policy_field(crate::workspace::glossary::effective_policy(&g));
        let fields = vec![
            ("JP term", g.jp_term),
            ("Thai term", g.thai_term),
            ("Category", g.category.unwrap_or_default()),
            ("Policy", policy),
            ("Do not trans", bool_field(g.do_not_translate)),
            ("Forbidden", g.forbidden_thai.join(", ")),
            ("Context rule", g.context_rule.unwrap_or_default()),
            ("Gloss", g.gloss.unwrap_or_default()),
        ];
        Self {
            kind: SUB_GLOSSARY,
            id: None,
            cursor: fields[0].1.len(),
            fields,
            field: 0,
            is_new: seed.is_none(),
        }
    }

    fn new_character(seed: Option<&Character>) -> Self {
        let c = seed.cloned();
        let fields = vec![
            (
                "JP name",
                c.as_ref().map(|x| x.jp_name.clone()).unwrap_or_default(),
            ),
            (
                "Thai name",
                c.as_ref().map(|x| x.thai_name.clone()).unwrap_or_default(),
            ),
            (
                "Aliases",
                c.as_ref().map(format_aliases).unwrap_or_default(),
            ),
            (
                "Also called",
                c.as_ref().map(format_also_called).unwrap_or_default(),
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
        ];
        Self {
            kind: SUB_CHARACTERS,
            id: c.as_ref().map(|x| x.id.clone()),
            cursor: fields[0].1.len(),
            fields,
            field: 0,
            is_new: seed.is_none(),
        }
    }

    fn current_mut(&mut self) -> &mut String {
        &mut self.fields[self.field].1
    }

    /// Focus a field and drop the caret at its end.
    fn focus_field(&mut self, field: usize) {
        self.field = field.min(self.fields.len().saturating_sub(1));
        self.cursor = self.fields[self.field].1.len();
    }

    fn next_field(&mut self) {
        self.focus_field((self.field + 1) % self.fields.len());
    }

    fn prev_field(&mut self) {
        self.focus_field((self.field + self.fields.len() - 1) % self.fields.len());
    }

    fn to_glossary(&self) -> GlossaryTerm {
        let get = |i: usize| self.fields.get(i).map(|f| f.1.clone()).unwrap_or_default();
        let policy = parse_policy(&get(3)).unwrap_or(TermPolicy::Preferred);
        GlossaryTerm {
            jp_term: get(0),
            thai_term: get(1),
            romaji: None,
            category: opt(get(2)),
            gloss: opt(get(7)),
            policy: Some(policy),
            forbidden_thai: split_list(&get(5)),
            context_rule: opt(get(6)),
            protected: matches!(
                policy,
                TermPolicy::HardLocked | TermPolicy::Forbidden | TermPolicy::ContextDependent
            )
            .then_some(true),
            do_not_translate: bool_opt(get(4)),
            first_seen_chapter: None,
        }
    }

    fn to_character(&self) -> Character {
        let get = |i: usize| self.fields.get(i).map(|f| f.1.clone()).unwrap_or_default();
        let jp = get(0);
        Character {
            id: self.id.clone().unwrap_or_else(|| slug_id(&jp)),
            jp_name: jp,
            thai_name: get(1),
            romaji: None,
            gender: opt(get(4)),
            honorific: None,
            speech_style: None,
            relationships: Vec::new(),
            aliases: split_list(&get(2)),
            also_called: parse_also_called(&get(3)),
            notes: opt(get(5)),
            first_seen_chapter: None,
        }
    }
}

pub struct LexiconScreen {
    sub: u8,
    list: ListState,
    editing: Option<EditForm>,
    filter: String,
    /// Caret byte-offset into `filter` (the `/` search field).
    filter_cursor: usize,
    /// True while the `/` search field is capturing input.
    searching: bool,
    /// Mouse hit-test rects, refreshed every frame: the section tabs, the table
    /// body, and the whole screen area (for locating the inline edit modal).
    tab_rects: Vec<(Rect, u8)>,
    table_area: Rect,
    screen_area: Rect,
    /// Memoized Markdown render of STYLE.md, so the Style tab is not re-parsed on
    /// every 100 ms tick. Keyed on the file body + width + theme.
    style_cache: crate::ui::markdown::RenderCache,
    /// Vertical scroll offset of the Style tab (clamped to content in render).
    style_scroll: u16,
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
            filter_cursor: 0,
            searching: false,
            tab_rects: Vec::new(),
            table_area: Rect::default(),
            screen_area: Rect::default(),
            style_cache: crate::ui::markdown::RenderCache::default(),
            style_scroll: 0,
        }
    }

    pub fn reset(&mut self) {
        self.editing = None;
        self.searching = false;
        self.filter.clear();
        self.filter_cursor = 0;
        self.list.select(Some(0));
        self.style_scroll = 0;
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
                        || policy_field(crate::workspace::glossary::effective_policy(t))
                            .contains(&q)
                        || t.forbidden_thai
                            .iter()
                            .any(|v| v.to_lowercase().contains(&q))
                        || t.context_rule
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
                .filter(|c| character_matches_filter(c, &q))
                .collect()
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, ws: Option<&Workspace>) -> Action {
        if self.editing.is_some() {
            return self.handle_edit_key(key, ws);
        }

        if self.searching {
            if input::handle(
                &mut self.filter,
                &mut self.filter_cursor,
                key,
                EditOpts::default(),
            ) != Edited::Ignored
            {
                return Action::None;
            }
            match key.code {
                KeyCode::Esc => {
                    self.searching = false;
                    self.filter.clear();
                    self.filter_cursor = 0;
                }
                KeyCode::Enter => self.searching = false,
                _ => {}
            }
            return Action::None;
        }

        match key.code {
            KeyCode::Tab => {
                self.sub = (self.sub + 1) % 3;
                self.list.select(Some(0));
                self.style_scroll = 0;
                Action::None
            }
            KeyCode::BackTab => {
                self.sub = (self.sub + 2) % 3;
                self.list.select(Some(0));
                self.style_scroll = 0;
                Action::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.sub == SUB_STYLE {
                    self.style_scroll = self.style_scroll.saturating_sub(1);
                } else {
                    self.move_sel(ws, -1);
                }
                Action::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.sub == SUB_STYLE {
                    self.style_scroll = self.style_scroll.saturating_add(1);
                } else {
                    self.move_sel(ws, 1);
                }
                Action::None
            }
            KeyCode::PageUp if self.sub == SUB_STYLE => {
                self.style_scroll = self.style_scroll.saturating_sub(10);
                Action::None
            }
            KeyCode::PageDown if self.sub == SUB_STYLE => {
                self.style_scroll = self.style_scroll.saturating_add(10);
                Action::None
            }
            KeyCode::Home if self.sub == SUB_STYLE => {
                self.style_scroll = 0;
                Action::None
            }
            KeyCode::End if self.sub == SUB_STYLE => {
                self.style_scroll = u16::MAX; // clamped to content in render_style
                Action::None
            }
            KeyCode::Char('/') => {
                self.searching = true;
                self.filter.clear();
                self.filter_cursor = 0;
                Action::None
            }
            KeyCode::Char('n') => {
                if ws.is_some() {
                    self.editing = Some(match self.sub {
                        SUB_CHARACTERS => EditForm::new_character(None),
                        SUB_STYLE => EditForm {
                            kind: SUB_STYLE,
                            id: None,
                            fields: vec![("Style note", String::new())],
                            field: 0,
                            cursor: 0,
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

    /// Mouse handling for tabs, table selection/editing, and edit-field focus.
    pub fn handle_mouse(&mut self, m: MouseInput, ws: Option<&Workspace>) -> Action {
        if self.editing.is_some() {
            return self.handle_edit_mouse(m);
        }
        match m.gesture {
            MouseGesture::ScrollUp => {
                if self.sub == SUB_STYLE {
                    self.style_scroll = self.style_scroll.saturating_sub(3);
                } else {
                    self.move_sel(ws, -1);
                }
                Action::None
            }
            MouseGesture::ScrollDown => {
                if self.sub == SUB_STYLE {
                    self.style_scroll = self.style_scroll.saturating_add(3);
                } else {
                    self.move_sel(ws, 1);
                }
                Action::None
            }
            MouseGesture::Click { double } => {
                // A section tab takes priority over the table below it.
                if let Some((_, id)) = self.tab_rects.iter().copied().find(|(r, _)| m.in_rect(*r)) {
                    if id != self.sub {
                        self.sub = id;
                        self.list.select(Some(0));
                        self.style_scroll = 0;
                    }
                    return Action::None;
                }
                // Style has no selectable rows; only the tabs are interactive.
                if self.sub == SUB_STYLE || !m.in_rect(self.table_area) {
                    return Action::None;
                }
                let len = self.current_len(ws);
                // Row 0 of the table is the column header; data starts one below.
                if m.row <= self.table_area.y {
                    return Action::None;
                }
                let idx = (m.row - self.table_area.y - 1) as usize + self.list.offset();
                if idx >= len {
                    return Action::None;
                }
                let already = self.list.selected() == Some(idx);
                self.list.select(Some(idx));
                if double || already {
                    self.begin_edit(ws);
                }
                Action::None
            }
            MouseGesture::RightClick => Action::None,
        }
    }

    fn handle_edit_mouse(&mut self, m: MouseInput) -> Action {
        let Some(form) = self.editing.as_mut() else {
            return Action::None;
        };
        match m.gesture {
            MouseGesture::ScrollUp => form.prev_field(),
            MouseGesture::ScrollDown => form.next_field(),
            MouseGesture::Click { .. } => {
                // Mirror `render_edit`'s field rows for hit testing.
                let modal = crate::ui::layout::centered_modal(
                    60,
                    (form.fields.len() as u16) * 2 + 6,
                    self.screen_area,
                );
                let inner_y = modal.y + 1;
                for i in 0..form.fields.len() {
                    if m.row == inner_y + 1 + (i as u16) * 2 {
                        form.focus_field(i);
                        break;
                    }
                }
            }
            MouseGesture::RightClick => {}
        }
        Action::None
    }

    /// Row count of the active section (0 for Style, which isn't a list).
    fn current_len(&self, ws: Option<&Workspace>) -> usize {
        match (ws, self.sub) {
            (Some(ws), SUB_GLOSSARY) => self.glossary(ws).len(),
            (Some(ws), SUB_CHARACTERS) => self.characters(ws).len(),
            _ => 0,
        }
    }

    fn handle_edit_key(&mut self, key: KeyEvent, ws: Option<&Workspace>) -> Action {
        let Some(form) = self.editing.as_mut() else {
            return Action::None;
        };
        // Up/Down/Tab move between fields, so the editor is single-line per field.
        let is_nav = matches!(
            key.code,
            KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::BackTab | KeyCode::Enter
        );
        if !is_nav {
            let mut cursor = form.cursor;
            let consumed = input::handle(form.current_mut(), &mut cursor, key, EditOpts::default())
                != Edited::Ignored;
            form.cursor = cursor;
            if consumed {
                return Action::None;
            }
        }
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
            KeyCode::Enter => {
                // Commit on Enter from the last field, else advance.
                if form.field + 1 < form.fields.len() {
                    form.next_field();
                    Action::None
                } else {
                    self.commit_edit(ws)
                }
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
            SUB_CHARACTERS => {
                crate::workspace::characters::upsert(ws, form.to_character()).map(|_| ())
            }
            SUB_STYLE => crate::workspace::style::append_note(
                ws,
                form.fields.first().map(|f| f.1.as_str()).unwrap_or(""),
            ),
            _ => crate::workspace::glossary::upsert(ws, form.to_glossary()),
        };
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
                self.editing = Some(EditForm {
                    kind: SUB_STYLE,
                    id: None,
                    fields: vec![("Style note", String::new())],
                    field: 0,
                    cursor: 0,
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
        let labelled: Option<(String, Action)> = match self.sub {
            SUB_CHARACTERS => self.characters(ws).get(idx).map(|c| {
                (
                    format!("{} → {}", c.jp_name, c.thai_name),
                    Action::DeleteCharacter { id: c.id.clone() },
                )
            }),
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
        self.screen_area = area;
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

    fn render_header(&mut self, f: &mut Frame, area: Rect, ws: Option<&Workspace>, theme: &Theme) {
        let tabs = [
            ("Glossary", SUB_GLOSSARY),
            ("Characters", SUB_CHARACTERS),
            ("Style", SUB_STYLE),
        ];
        let mut spans = vec![Span::raw("  ")];
        let mut x = area.x.saturating_add(2);
        self.tab_rects.clear();
        for (label, id) in tabs {
            let text = if id == self.sub {
                format!("〔 {label} 〕")
            } else {
                format!("  {label}  ")
            };
            let w = col_width(&text) as u16;
            self.tab_rects.push((
                Rect {
                    x,
                    y: area.y,
                    width: w,
                    height: 1,
                },
                id,
            ));
            x = x.saturating_add(w).saturating_add(1); // + trailing space
            if id == self.sub {
                spans.push(Span::styled(
                    text,
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::styled(text, Style::default().fg(theme.ink_faint)));
            }
            spans.push(Span::raw(" "));
        }
        let count = match (ws, self.sub) {
            (Some(ws), SUB_GLOSSARY) => format!("{} terms", self.glossary(ws).len()),
            (Some(ws), SUB_CHARACTERS) => format!("{} characters", self.characters(ws).len()),
            _ => "—".to_string(),
        };
        let faint = Style::default().fg(theme.ink_faint);
        let mut right_spans: Vec<Span> = Vec::new();
        if self.searching || !self.filter.is_empty() {
            right_spans.push(Span::styled("/ filter: ", faint));
            if self.searching {
                let (before, after) =
                    input::caret_halves(&self.filter, self.filter_cursor, usize::MAX);
                right_spans.push(Span::styled(before, faint));
                right_spans.push(Span::styled("▏", Style::default().fg(theme.stream_cursor)));
                right_spans.push(Span::styled(after, faint));
            } else {
                right_spans.push(Span::styled(thai_display_safe(&self.filter), faint));
            }
            right_spans.push(Span::styled("   ", faint));
        }
        right_spans.push(Span::styled(format!("({count})"), faint));

        let left = Line::from(spans);
        f.render_widget(
            Paragraph::new(left).style(Style::default().bg(theme.bg)),
            area,
        );
        let rw: u16 = right_spans
            .iter()
            .map(|s| col_width(s.content.as_ref()))
            .sum::<usize>() as u16;
        if area.width > rw + 2 {
            f.render_widget(
                Paragraph::new(Line::from(right_spans)).style(Style::default().bg(theme.bg)),
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
        self.table_area = inner;

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

        let head = Line::from(Span::styled(
            format!(
                "   {} {} {} {} {}  Notes",
                pad_to_cols("JP term", 12),
                pad_to_cols("Thai term", 16),
                pad_to_cols("Cat", 8),
                pad_to_cols("Policy", 10),
                "DNT"
            ),
            Style::default().fg(theme.ink_faint),
        ));

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(area);
        f.render_widget(
            Paragraph::new(head).style(Style::default().bg(theme.bg_panel)),
            rows[0],
        );

        let mut items: Vec<ListItem> = Vec::new();
        let gloss_w = area.width.saturating_sub(63).max(8) as usize;
        for (i, t) in terms.iter().enumerate() {
            let selected = i == sel;
            let bar = if selected { theme::SELECT_BAR } else { ' ' };
            let bg = if selected {
                theme.accent_bg
            } else {
                theme.bg_panel
            };
            let policy = policy_short(crate::workspace::glossary::effective_policy(t));
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
                    pad_to_cols(&thai_display_safe(&t.thai_term), 16),
                    Style::default().fg(theme.th_text).bg(bg),
                ),
                Span::styled(" ", Style::default().bg(bg)),
                Span::styled(
                    pad_to_cols(&thai_display_safe(t.category.as_deref().unwrap_or("—")), 8),
                    Style::default().fg(theme.ink_soft).bg(bg),
                ),
                Span::styled(
                    pad_to_cols(policy, 10),
                    Style::default().fg(theme.ink_faint).bg(bg),
                ),
                Span::styled(" ", Style::default().bg(bg)),
                Span::styled(
                    format!(" {dnt}   "),
                    Style::default().fg(theme.ink_faint).bg(bg),
                ),
                Span::styled(
                    truncate_cols(&thai_display_safe(&term_note(t)), gloss_w),
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
        f.render_stateful_widget(
            List::new(items).style(Style::default().bg(theme.bg_panel)),
            rows[1],
            &mut self.list,
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
        let cols = character_columns(area.width);

        let mut head = format!(
            "   {} {}",
            pad_to_cols("JP name", cols.jp),
            pad_to_cols("Thai name", cols.thai)
        );
        if cols.gender > 0 {
            head.push(' ');
            head.push_str(&pad_to_cols("Gender", cols.gender));
        }
        if cols.extra > 0 {
            head.push_str("  ");
            head.push_str(&pad_to_cols("Names / Notes", cols.extra));
        }
        let head = Line::from(Span::styled(head, Style::default().fg(theme.ink_faint)));
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(area);
        f.render_widget(
            Paragraph::new(head).style(Style::default().bg(theme.bg_panel)),
            rows[0],
        );
        let mut items: Vec<ListItem> = Vec::new();
        for (i, c) in chars.iter().enumerate() {
            let selected = i == sel;
            let bar = if selected { theme::SELECT_BAR } else { ' ' };
            let bg = if selected {
                theme.accent_bg
            } else {
                theme.bg_panel
            };
            let mut spans = vec![
                Span::styled(format!(" {bar} "), Style::default().fg(theme.accent).bg(bg)),
                Span::styled(
                    pad_to_cols(&c.jp_name, cols.jp),
                    Style::default().fg(theme.ink).bg(bg),
                ),
                Span::styled(" ", Style::default().bg(bg)),
                Span::styled(
                    pad_to_cols(&thai_display_safe(&c.thai_name), cols.thai),
                    Style::default().fg(theme.th_text).bg(bg),
                ),
            ];
            if cols.gender > 0 {
                spans.push(Span::styled(" ", Style::default().bg(bg)));
                spans.push(Span::styled(
                    pad_to_cols(
                        &thai_display_safe(c.gender.as_deref().unwrap_or("—")),
                        cols.gender,
                    ),
                    Style::default().fg(theme.ink_soft).bg(bg),
                ));
            }
            if cols.extra > 0 {
                spans.push(Span::styled("  ", Style::default().bg(bg)));
                spans.push(Span::styled(
                    truncate_cols(&thai_display_safe(&character_extra(c)), cols.extra),
                    Style::default().fg(theme.ink_soft).bg(bg),
                ));
            }
            items.push(ListItem::new(Line::from(spans)));
        }
        if chars.is_empty() {
            items.push(ListItem::new(Line::from(Span::styled(
                "   (no characters yet — n to add)",
                Style::default().fg(theme.ink_faint),
            ))));
        }
        f.render_stateful_widget(
            List::new(items).style(Style::default().bg(theme.bg_panel)),
            rows[1],
            &mut self.list,
        );
    }

    fn render_style(&mut self, f: &mut Frame, area: Rect, ws: &Workspace, theme: &Theme) {
        let body = std::fs::read_to_string(ws.style_md())
            .unwrap_or_else(|_| "STYLE.md not found.".to_string());
        // Cache the Markdown render by body, width, and theme.
        let width = area.width.saturating_sub(2) as usize;
        let fg = theme.ink_soft;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        body.hash(&mut h);
        width.hash(&mut h);
        fg.hash(&mut h);
        crate::ui::markdown::theme_fingerprint(theme).hash(&mut h);
        let key = h.finish();
        let lines = self
            .style_cache
            .lines(key, || crate::ui::markdown::render(&body, fg, theme, width))
            .to_vec();
        let max_scroll = (lines.len() as u16).saturating_sub(area.height);
        self.style_scroll = self.style_scroll.min(max_scroll);
        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((self.style_scroll, 0))
                .style(Style::default().bg(theme.bg_panel)),
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
            let val_w = inner.width.saturating_sub(18) as usize;
            let mut spans = vec![
                Span::styled(format!(" {marker} "), Style::default().fg(theme.accent)),
                Span::styled(pad_to_cols(label, 12), Style::default().fg(theme.ink_faint)),
            ];
            if focused {
                let (before, after) = input::caret_halves(value, form.cursor, val_w);
                spans.push(Span::styled(before, val_style));
                spans.push(Span::styled("▏", Style::default().fg(theme.stream_cursor)));
                spans.push(Span::styled(after, val_style));
            } else {
                spans.push(Span::styled(
                    truncate_cols(&thai_display_safe(value), val_w),
                    val_style,
                ));
            }
            lines.push(Line::from(spans));
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

#[derive(Debug, Clone, Copy)]
struct CharacterColumns {
    jp: usize,
    thai: usize,
    gender: usize,
    extra: usize,
}

fn character_columns(width: u16) -> CharacterColumns {
    let total = width as usize;
    let mut jp = if total >= 78 {
        20
    } else if total >= 48 {
        14
    } else {
        10.min(total.saturating_sub(5).max(4))
    };
    let mut gender = if total >= 68 { 8 } else { 0 };
    // Width of the leading bar + JP name + Gender — everything before Thai name.
    let base = |jp: usize, gender: usize| 3 + jp + 1 + if gender > 0 { 1 + gender } else { 0 };
    while total < base(jp, gender) + 9 {
        if gender > 0 {
            gender = 0;
        } else if jp > 4 {
            jp -= 1;
        } else {
            break;
        }
    }
    // Wide pane: "Names / Notes" soaks up the leftover width and Thai stays a
    // readable fixed width. Narrow pane (no room for notes): Thai absorbs the
    // slack so the table still fills the pane with no dead space.
    let thai_fixed = if total >= 48 { 20 } else { 12 };
    let leftover = total.saturating_sub(base(jp, gender) + thai_fixed + 2);
    let (thai, extra) = if leftover >= 18 {
        (thai_fixed, leftover)
    } else {
        (total.saturating_sub(base(jp, gender)).max(1), 0)
    };
    CharacterColumns {
        jp,
        thai,
        gender,
        extra,
    }
}

fn character_matches_filter(c: &Character, q: &str) -> bool {
    let fields = [
        c.id.as_str(),
        c.jp_name.as_str(),
        c.thai_name.as_str(),
        c.romaji.as_deref().unwrap_or(""),
        c.gender.as_deref().unwrap_or(""),
        c.honorific.as_deref().unwrap_or(""),
        c.speech_style.as_deref().unwrap_or(""),
        c.notes.as_deref().unwrap_or(""),
    ];
    fields.iter().any(|v| v.to_lowercase().contains(q))
        || c.aliases.iter().any(|v| v.to_lowercase().contains(q))
        || c.also_called.iter().any(|a| {
            a.jp.to_lowercase().contains(q)
                || a.thai.to_lowercase().contains(q)
                || a.by.as_deref().unwrap_or("").to_lowercase().contains(q)
        })
}

fn format_aliases(c: &Character) -> String {
    c.aliases.join(", ")
}

fn format_also_called(c: &Character) -> String {
    c.also_called
        .iter()
        .filter(|a| !a.jp.trim().is_empty())
        .map(format_alt_name)
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_alt_name(a: &AltName) -> String {
    let mut out = if a.thai.trim().is_empty() {
        a.jp.trim().to_string()
    } else {
        format!("{}→{}", a.jp.trim(), a.thai.trim())
    };
    if let Some(by) = a.by.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        out.push_str(" @ ");
        out.push_str(by);
    }
    out
}

fn parse_also_called(s: &str) -> Vec<AltName> {
    split_list(s)
        .into_iter()
        .filter_map(|piece| parse_alt_name(&piece))
        .collect()
}

fn parse_alt_name(piece: &str) -> Option<AltName> {
    let (body, by) = piece
        .rsplit_once('@')
        .map(|(body, by)| (body.trim(), opt(by.trim().to_string())))
        .unwrap_or((piece.trim(), None));
    let (jp, thai) = body
        .split_once('→')
        .or_else(|| body.split_once("=>"))
        .or_else(|| body.split_once('='))
        .map(|(jp, thai)| (jp.trim(), thai.trim()))
        .unwrap_or((body, ""));
    if jp.is_empty() {
        return None;
    }
    Some(AltName {
        jp: jp.to_string(),
        thai: thai.to_string(),
        by,
    })
}

fn character_extra(c: &Character) -> String {
    let mut parts = Vec::new();
    if !c.aliases.is_empty() {
        parts.push(format!("alias: {}", c.aliases.join(", ")));
    }
    let called = format_also_called(c);
    if !called.is_empty() {
        parts.push(format!("called: {called}"));
    }
    if let Some(notes) = c.notes.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        parts.push(notes.to_string());
    }
    parts.join(" · ")
}

fn opt(s: String) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn bool_field(v: Option<bool>) -> String {
    match v {
        Some(true) => "yes".to_string(),
        Some(false) => "no".to_string(),
        None => String::new(),
    }
}

fn bool_opt(s: String) -> Option<bool> {
    match s.trim().to_lowercase().as_str() {
        "" => None,
        "1" | "true" | "t" | "yes" | "y" | "on" | "lock" | "locked" | "protect" | "protected"
        | "✓" => Some(true),
        "0" | "false" | "f" | "no" | "n" | "off" | "unlock" | "unlocked" | "-" | "—" | "·" => {
            Some(false)
        }
        _ => None,
    }
}

fn policy_field(policy: TermPolicy) -> String {
    match policy {
        TermPolicy::HardLocked => "hard_locked".to_string(),
        TermPolicy::Preferred => "preferred".to_string(),
        TermPolicy::Forbidden => "forbidden".to_string(),
        TermPolicy::ContextDependent => "context_dependent".to_string(),
    }
}

fn parse_policy(s: &str) -> Option<TermPolicy> {
    match s.trim().to_lowercase().replace('-', "_").as_str() {
        "hard_locked" | "hard" | "lock" | "locked" | "protected" => Some(TermPolicy::HardLocked),
        "preferred" | "prefer" | "default" | "soft" => Some(TermPolicy::Preferred),
        "forbidden" | "forbid" | "ban" | "banned" => Some(TermPolicy::Forbidden),
        "context_dependent" | "context" | "conditional" | "depends" => {
            Some(TermPolicy::ContextDependent)
        }
        "" => None,
        _ => None,
    }
}

fn policy_short(policy: TermPolicy) -> &'static str {
    match policy {
        TermPolicy::HardLocked => "lock",
        TermPolicy::Preferred => "prefer",
        TermPolicy::Forbidden => "forbid",
        TermPolicy::ContextDependent => "context",
    }
}

fn term_note(t: &GlossaryTerm) -> String {
    let mut parts = Vec::new();
    if let Some(rule) = t.context_rule.as_deref().filter(|v| !v.trim().is_empty()) {
        parts.push(format!("ctx: {}", rule.trim()));
    }
    let forbidden = crate::workspace::glossary::forbidden_renderings(t);
    if !forbidden.is_empty() {
        parts.push(format!("avoid: {}", forbidden.join(", ")));
    }
    if let Some(gloss) = t.gloss.as_deref().filter(|v| !v.trim().is_empty()) {
        parts.push(gloss.trim().to_string());
    }
    parts.join(" · ")
}

fn split_list(s: &str) -> Vec<String> {
    s.split([',', ';', '\n'])
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect()
}

/// A stable id derived from a JP name (mirrors the workspace's slugify-jp rule:
/// non-ASCII preserved, ASCII lowered, separators collapsed).
fn slug_id(jp: &str) -> String {
    super::slugify(jp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{Workspace, characters};

    fn temp_ws(tag: &str) -> (std::path::PathBuf, Workspace) {
        let base = std::env::temp_dir().join(format!("honya_lexicon_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let ws = Workspace::new(base.clone(), 1);
        (base, ws)
    }

    fn character() -> Character {
        Character {
            id: "char-3199b4b0".into(),
            jp_name: "清水圭".into(),
            thai_name: "ชิมิซุ เค".into(),
            romaji: Some("Shimizu Kei".into()),
            gender: Some("female".into()),
            honorific: Some("คุณ".into()),
            speech_style: None,
            relationships: Vec::new(),
            aliases: vec!["圭".into(), "シミズ".into()],
            also_called: vec![AltName {
                jp: "ケ様".into(),
                thai: "ท่านเค".into(),
                by: Some("清水愛".into()),
            }],
            notes: Some("นางเอก".into()),
            first_seen_chapter: None,
        }
    }

    #[test]
    fn character_columns_fill_width_no_dead_space() {
        let used = |c: &CharacterColumns| {
            3 + c.jp
                + 1
                + c.thai
                + if c.gender > 0 { 1 + c.gender } else { 0 }
                + if c.extra > 0 { 2 + c.extra } else { 0 }
        };
        // Wide panes fill the full width via the flexible Names/Notes column.
        for w in [200u16, 1900] {
            let c = character_columns(w);
            assert_eq!(used(&c), w as usize, "columns should fill width {w}");
            assert!(
                c.extra > c.thai,
                "Names/Notes should be the flexible column"
            );
        }
        // Every width fills the pane without overflowing (Thai absorbs slack
        // when there is no room for a notes column).
        for w in [10u16, 30, 47, 67, 80] {
            let c = character_columns(w);
            assert_eq!(used(&c), w as usize, "no dead space at width {w}");
        }
    }

    #[test]
    fn character_filter_matches_alias_and_alt_name() {
        let (base, ws) = temp_ws("filter_names");
        characters::upsert(&ws, character()).unwrap();
        let mut screen = LexiconScreen::new();
        screen.sub = SUB_CHARACTERS;

        screen.filter = "シミズ".into();
        assert_eq!(screen.characters(&ws).len(), 1);

        screen.filter = "ท่านเค".into();
        assert_eq!(screen.characters(&ws).len(), 1);

        screen.filter = "นางเอก".into();
        assert_eq!(screen.characters(&ws).len(), 1);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn character_edit_form_round_trips_id_aliases_and_alt_names() {
        let c = character();
        let form = EditForm::new_character(Some(&c));
        let edited = form.to_character();

        assert_eq!(edited.id, c.id);
        assert_eq!(edited.jp_name, "清水圭");
        assert_eq!(edited.thai_name, "ชิมิซุ เค");
        assert_eq!(edited.aliases, vec!["圭".to_string(), "シミズ".to_string()]);
        assert_eq!(edited.also_called.len(), 1);
        assert_eq!(edited.also_called[0].jp, "ケ様");
        assert_eq!(edited.also_called[0].thai, "ท่านเค");
        assert_eq!(edited.also_called[0].by.as_deref(), Some("清水愛"));
        assert_eq!(edited.gender.as_deref(), Some("female"));
        assert_eq!(edited.notes.as_deref(), Some("นางเอก"));
    }
}
