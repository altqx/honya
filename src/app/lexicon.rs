//! Lexicon screen for editing glossary, character, and style context files.

use std::hash::{Hash, Hasher};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::model::{AltName, Character, GlossaryTerm, LogLevel, TermPolicy};
use crate::theme::{self, Theme};
use crate::ui::input::{self, EditOpts, Edited};
use crate::ui::kit::list::ListState;
use crate::ui::kit::table::{self, Column, Width};
use crate::ui::mouse::{MouseGesture, MouseInput};
use crate::ui::text::{col_width, thai_display_safe, truncate_cols};
use crate::workspace::Workspace;

use super::action_table::{self, Act};
use super::lexicon_defs::{
    self, Def, DraftEntry, EntryKind, FieldValue, Kind, LexField,
};
use super::Action;
use super::overlay::Overlay;

const SUB_GLOSSARY: u8 = 0;
const SUB_CHARACTERS: u8 = 1;
const SUB_STYLE: u8 = 2;

/// The entry being edited, in place.
///
/// The draft *is* the entry, cloned from what was on disk, so committing is a
/// write rather than a reassembly — which is why a field with no row can no
/// longer be dropped on the way out.
#[derive(Debug, Clone)]
pub struct EditForm {
    draft: DraftEntry,
    /// What it looked like when it opened, for the dirty check.
    seed: DraftEntry,
    /// An index into [`lexicon_defs::ORDER`].
    field: u8,
    /// Byte offset into whatever the focused row is editing.
    cursor: usize,
    /// The entry being typed on a list row, before it becomes a chip.
    buffer: String,
    /// Windowing only; the selection is `field`, which `sync` mirrors into it.
    list: ListState,
    is_new: bool,
}

impl EditForm {
    fn new(draft: DraftEntry, is_new: bool) -> Self {
        let field = draft.kind().first_field().unwrap_or(0);
        let mut form = Self {
            seed: draft.clone(),
            draft,
            field,
            cursor: 0,
            buffer: String::new(),
            list: ListState::new(),
            is_new,
        };
        form.focus_field(field);
        form
    }

    /// Where the focused row sits among the rows this kind actually shows —
    /// which is what the form renders and what its zone ids count from.
    fn local(&self) -> usize {
        self.rows()
            .iter()
            .position(|f| *f == self.field)
            .unwrap_or(0)
    }

    fn kind(&self) -> EntryKind {
        self.draft.kind()
    }

    fn rows(&self) -> Vec<u8> {
        self.kind().fields()
    }

    fn def(&self) -> Option<&'static Def> {
        lexicon_defs::at(self.field)
    }

    fn value(&self, field: LexField) -> FieldValue {
        lexicon_defs::get(&self.draft, field)
    }

    fn set_value(&mut self, field: LexField, v: &FieldValue) {
        lexicon_defs::set(&mut self.draft, field, v);
    }

    /// The text the focused row is editing: the value itself, or the pending
    /// chip on a list row.
    fn current_text(&self) -> String {
        match self.def() {
            Some(d) if matches!(d.kind, Kind::Chips | Kind::Alts) => self.buffer.clone(),
            Some(d) => self.value(d.field).as_text().to_string(),
            None => String::new(),
        }
    }

    /// Focus a row and drop the caret at the end of what it is editing.
    fn focus_field(&mut self, field: u8) {
        let rows = self.rows();
        if rows.contains(&field) {
            self.field = field;
        } else if let Some(first) = rows.first() {
            self.field = *first;
        }
        self.buffer.clear();
        self.cursor = self.current_text().len();
        let local = self.local();
        self.list.select(Some(local));
    }

    /// Focus by position among the rendered rows, which is what a sub-control
    /// id carries.
    fn focus_local(&mut self, n: usize) {
        if let Some(f) = self.rows().get(n).copied() {
            self.focus_field(f);
        }
    }

    fn is_last_row(&self) -> bool {
        self.rows().last() == Some(&self.field)
    }

    fn chip_count(&self) -> usize {
        match self.def().map(|d| (d.kind, d.field)) {
            Some((Kind::Chips, f)) => match self.value(f) {
                FieldValue::List(items) => items.len(),
                _ => 0,
            },
            Some((Kind::Alts, f)) => match self.value(f) {
                FieldValue::Alts(alts) => alts.len(),
                _ => 0,
            },
            _ => 0,
        }
    }

    /// Write back whatever the focused row is editing.
    fn set_current_text(&mut self, text: String) {
        let Some(d) = self.def() else { return };
        match d.kind {
            Kind::Chips | Kind::Alts => self.buffer = text,
            // A chapter beyond `u32` is not a chapter; refusing it keeps the
            // stored value and the caret from disagreeing about what is there.
            Kind::Numeric if !text.trim().is_empty() && text.trim().parse::<u32>().is_err() => {}
            _ => self.set_value(d.field, &FieldValue::Text(text)),
        }
    }

    fn step_field(&mut self, forward: bool) {
        let rows = self.rows();
        if rows.is_empty() {
            return;
        }
        let at = rows.iter().position(|f| *f == self.field).unwrap_or(0);
        let n = rows.len();
        let next = if forward { (at + 1) % n } else { (at + n - 1) % n };
        self.focus_field(rows[next]);
    }

    /// Whether anything the form shows has moved since it opened.
    fn is_dirty(&self) -> bool {
        if let (DraftEntry::StyleNote(now), DraftEntry::StyleNote(was)) = (&self.draft, &self.seed) {
            return now != was;
        }
        self.rows().iter().any(|f| {
            lexicon_defs::at(*f).is_some_and(|d| {
                lexicon_defs::get(&self.draft, d.field) != lexicon_defs::get(&self.seed, d.field)
            })
        })
    }

    /// Commit the pending chip on a list row, if there is one.
    fn commit_chip(&mut self) {
        let Some(d) = self.def() else { return };
        let text = self.buffer.trim().to_string();
        if text.is_empty() {
            return;
        }
        match d.kind {
            Kind::Chips => {
                let FieldValue::List(mut items) = self.value(d.field) else {
                    return;
                };
                items.push(text);
                self.set_value(d.field, &FieldValue::List(items));
            }
            Kind::Alts => {
                let Some(alt) = parse_alt_name(&text) else { return };
                let FieldValue::Alts(mut alts) = self.value(d.field) else {
                    return;
                };
                alts.push(alt);
                self.set_value(d.field, &FieldValue::Alts(alts));
            }
            _ => return,
        }
        self.buffer.clear();
        self.cursor = 0;
    }

    /// Lift a chip back into the buffer, so one gesture serves edit and delete.
    fn lift_chip(&mut self, n: usize) {
        let Some(d) = self.def() else { return };
        match d.kind {
            Kind::Chips => {
                let FieldValue::List(mut items) = self.value(d.field) else {
                    return;
                };
                if n >= items.len() {
                    return;
                }
                self.buffer = items.remove(n);
                self.set_value(d.field, &FieldValue::List(items));
            }
            Kind::Alts => {
                let FieldValue::Alts(mut alts) = self.value(d.field) else {
                    return;
                };
                if n >= alts.len() {
                    return;
                }
                self.buffer = format_alt_name(&alts.remove(n));
                self.set_value(d.field, &FieldValue::Alts(alts));
            }
            _ => return,
        }
        self.cursor = self.buffer.len();
    }

    /// Cycle a Select or Combo, or flip a Toggle.
    fn cycle(&mut self, forward: bool, known: &[String]) {
        let Some(d) = self.def() else { return };
        match d.kind {
            Kind::Select => {
                let FieldValue::Choice(i) = self.value(d.field) else {
                    return;
                };
                let n = lexicon_defs::POLICIES.len();
                let next = if forward { (i + 1) % n } else { (i + n - 1) % n };
                self.set_value(d.field, &FieldValue::Choice(next));
            }
            Kind::Toggle => {
                let FieldValue::Flag(on) = self.value(d.field) else {
                    return;
                };
                self.set_value(d.field, &FieldValue::Flag(!on));
            }
            Kind::Combo => {
                if known.is_empty() {
                    return;
                }
                let current = self.value(d.field).as_text().to_string();
                let at = known.iter().position(|k| *k == current);
                let n = known.len();
                let next = match (at, forward) {
                    (Some(i), true) => (i + 1) % n,
                    (Some(i), false) => (i + n - 1) % n,
                    (None, _) => 0,
                };
                self.set_value(d.field, &FieldValue::Text(known[next].clone()));
                self.cursor = self.current_text().len();
            }
            _ => {}
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
    table_area: Rect,
    screen_area: Rect,
    /// Memoized Markdown render of STYLE.md, so the Style tab is not re-parsed on
    /// every 100 ms tick. Keyed on the file body + width + theme.
    style_cache: crate::ui::markdown::RenderCache,
    /// Vertical scroll offset of the Style tab (clamped to content in render).
    style_scroll: u16,
    /// Sort order per tabular section, indexed by `self.sub`. Style has no
    /// table, so its slot is never read.
    sort: [table::Sort; 2],
}

/// Action ids for this screen's table. Stable within the screen: they are also
/// the zone index every one of its controls registers under.
/// The edit form's footer buttons. Both indices sit below `form::DEC_BASE`, so
/// neither `form::field_of` nor `form::chip_of` can mistake one for a row's
/// sub-control.
const FORM_SAVE: u32 = 1;
const FORM_CANCEL: u32 = 2;

const L_NEW: u16 = 0;
const L_SEARCH: u16 = 1;
const L_EDIT: u16 = 2;
const L_DELETE: u16 = 3;

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
            table_area: Rect::default(),
            screen_area: Rect::default(),
            style_cache: crate::ui::markdown::RenderCache::default(),
            style_scroll: 0,
            sort: [table::Sort::default(); 2],
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
        let mut rows = if self.filter.is_empty() {
            all
        } else {
            let q = self.filter.to_lowercase();
            all.into_iter()
                .filter(|t| {
                    t.jp_term.to_lowercase().contains(&q)
                        || t.translated_term.to_lowercase().contains(&q)
                        || t.category
                            .as_deref()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains(&q)
                        || lexicon_defs::policy_label(
                            crate::workspace::glossary::effective_policy(t),
                        )
                        .contains(&q)
                        || t.forbidden_translations
                            .iter()
                            .any(|v| v.to_lowercase().contains(&q))
                        || t.context_rule
                            .as_deref()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains(&q)
                })
                .collect()
        };
        sort_rows(&mut rows, self.sort[SUB_GLOSSARY as usize], glossary_cells);
        rows
    }

    fn characters(&self, ws: &Workspace) -> Vec<Character> {
        let all = crate::workspace::characters::load(ws);
        let mut rows = if self.filter.is_empty() {
            all
        } else {
            let q = self.filter.to_lowercase();
            all.into_iter()
                .filter(|c| character_matches_filter(c, &q))
                .collect()
        };
        sort_rows(&mut rows, self.sort[SUB_CHARACTERS as usize], character_cells);
        rows
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

        // Commands come from the table; only navigation is left below. Tab is
        // the documented exception — it is a reserved global everywhere else,
        // and the Lexicon keeps it for its sections.
        let acts = self.actions(ws);
        match action_table::hit(&acts, &key) {
            action_table::KeyHit::Run(id) => return self.run(id, ws).unwrap_or(Action::None),
            action_table::KeyHit::Blocked => return Action::None,
            action_table::KeyHit::Miss => {}
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
            _ => Action::None,
        }
    }

    /// Mouse handling for tabs, table selection/editing, and edit-field focus.
    pub fn handle_mouse(
        &mut self,
        m: MouseInput,
        zone: Option<crate::ui::kit::ZoneId>,
        ws: Option<&Workspace>,
    ) -> Action {
        if self.editing.is_some() {
            return self.handle_edit_mouse(m, zone, ws);
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
                // A section tab takes priority over the table below it. The
                // strip registers its own segments, so nothing here keeps a
                // second copy of where they landed.
                if let Some(id) = zone
                    && id.kind == crate::ui::kit::ZoneKind::Segment
                {
                    let next = id.index as u8;
                    if next != self.sub {
                        self.sub = next;
                        self.list.select(Some(0));
                        self.style_scroll = 0;
                    }
                    return Action::None;
                }
                // A header sorts by its column; clicking the column already
                // sorted flips the direction.
                if let Some(col) = zone.and_then(table::header_column) {
                    let slot = self.sub as usize;
                    if let Some(sort) = self.sort.get_mut(slot) {
                        *sort = sort.toggled(col);
                        self.list.select(Some(0));
                    }
                    return Action::None;
                }
                // The clicked row comes from the registry the table wrote while
                // drawing, so there is no column arithmetic here to fall out of
                // step with it. Style has no selectable rows and registers none.
                let Some(idx) = zone.and_then(|z| z.row_index()) else {
                    return Action::None;
                };
                let already = self.list.selected() == Some(idx);
                self.list.select(Some(idx));
                if double || already {
                    self.begin_edit(ws);
                }
                Action::None
            }
            // The router opens this row's menu straight after, so the selection
            // has to be on the row the menu is about.
            MouseGesture::RightClick => {
                if let Some(idx) = zone.and_then(|z| z.row_index()) {
                    self.list.select(Some(idx));
                }
                Action::None
            }
        }
    }

    fn handle_edit_mouse(
        &mut self,
        m: MouseInput,
        zone: Option<crate::ui::kit::ZoneId>,
        ws: Option<&Workspace>,
    ) -> Action {
        match m.gesture {
            MouseGesture::ScrollUp | MouseGesture::ScrollDown => {
                if let Some(form) = self.editing.as_mut() {
                    form.step_field(matches!(m.gesture, MouseGesture::ScrollDown));
                }
                Action::None
            }
            MouseGesture::RightClick => self.cancel_edit(),
            MouseGesture::Click { .. } => match zone {
                Some(id) => self.click_edit_zone(id, ws),
                None => Action::None,
            },
        }
    }

    /// Route a click from the zone the form registered while drawing, so
    /// nothing here keeps a second copy of where a row landed.
    fn click_edit_zone(&mut self, id: crate::ui::kit::ZoneId, ws: Option<&Workspace>) -> Action {
        use crate::ui::kit::{ZoneKind, form as kit, modal};

        match id.kind {
            ZoneKind::Backdrop => return self.cancel_edit(),
            ZoneKind::Button => match id.index {
                modal::CLOSE_BUTTON | FORM_CANCEL => return self.cancel_edit(),
                FORM_SAVE => return self.commit_edit(ws),
                _ => {}
            },
            ZoneKind::Field | ZoneKind::TextSurface => {}
            _ => return Action::None,
        }

        let Some(form) = self.editing.as_mut() else {
            return Action::None;
        };
        // A form row's own id counts from `ORDER`, because that is the `id_base`
        // `render_edit` hands the form; its sub-controls count from the row's
        // place among the rows this kind shows.
        if id.kind == ZoneKind::Field {
            form.focus_field(id.index as u8);
            return Action::None;
        }
        if let Some((local, chip)) = kit::chip_of(id) {
            form.focus_local(local);
            form.lift_chip(chip);
            return Action::None;
        }
        if let Some((local, step)) = kit::field_of(id) {
            form.focus_local(local);
            let field = form.def().map(|d| d.field);
            let known = known_values(ws, field);
            form.cycle(step == kit::Step::Up, &known);
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
        use ratatui::crossterm::event::KeyModifiers;

        // Ctrl-S saves from any row, so a long form never has to be paged to
        // the end to be committed.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
            return self.commit_edit(ws);
        }
        let Some(form) = self.editing.as_mut() else {
            return Action::None;
        };
        let kind = form.def().map(|d| d.kind);
        let listy = matches!(kind, Some(Kind::Chips | Kind::Alts));

        if listy {
            match key.code {
                KeyCode::Char(',') => {
                    form.commit_chip();
                    return Action::None;
                }
                // Nothing left to erase in the buffer, so take the last chip
                // back into it — one gesture for "edit this" and "drop this",
                // the same one the click gives.
                KeyCode::Backspace if form.buffer.is_empty() => {
                    let n = form.chip_count();
                    if n > 0 {
                        form.lift_chip(n - 1);
                    }
                    return Action::None;
                }
                _ => {}
            }
        }

        // Typing only reaches rows that hold text. It used to reach every row,
        // which is why a keystroke on Policy mangled the value.
        let types = matches!(
            kind,
            Some(Kind::Text | Kind::Numeric | Kind::Combo | Kind::Chips | Kind::Alts)
        );
        let cycles = matches!(kind, Some(Kind::Select | Kind::Toggle | Kind::Combo));
        let reserved = matches!(
            key.code,
            KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::BackTab | KeyCode::Enter | KeyCode::Esc
        ) || (cycles && matches!(key.code, KeyCode::Left | KeyCode::Right));

        if types && !reserved {
            let mut text = form.current_text();
            let mut cursor = form.cursor.min(text.len());
            let opts = EditOpts {
                numeric_only: kind == Some(Kind::Numeric),
                multiline: false,
            };
            if input::handle(&mut text, &mut cursor, key, opts) != Edited::Ignored {
                form.set_current_text(text);
                form.cursor = cursor;
                return Action::None;
            }
        }

        match key.code {
            KeyCode::Esc => self.cancel_edit(),
            KeyCode::Tab | KeyCode::Down => {
                form.step_field(true);
                Action::None
            }
            KeyCode::BackTab | KeyCode::Up => {
                form.step_field(false);
                Action::None
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if cycles => {
                let field = form.def().map(|d| d.field);
                let known = known_values(ws, field);
                form.cycle(key.code != KeyCode::Left, &known);
                Action::None
            }
            KeyCode::Enter => {
                if listy && !form.buffer.trim().is_empty() {
                    form.commit_chip();
                    return Action::None;
                }
                if form.is_last_row() {
                    self.commit_edit(ws)
                } else {
                    form.step_field(true);
                    Action::None
                }
            }
            _ => Action::None,
        }
    }

    /// Back out, asking first when there is something to lose.
    fn cancel_edit(&mut self) -> Action {
        if self.editing.as_ref().is_some_and(EditForm::is_dirty) {
            return Action::show_overlay(Overlay::confirm(
                "Discard changes?",
                "This entry has edits that have not been saved.".to_string(),
                Action::CancelLexiconEdit,
            ));
        }
        self.editing = None;
        Action::None
    }

    /// Drop the form without asking. Only the discard confirmation calls this.
    pub fn discard_edit(&mut self) {
        self.editing = None;
    }

    fn commit_edit(&mut self, ws: Option<&Workspace>) -> Action {
        let Some(mut form) = self.editing.take() else {
            return Action::None;
        };
        let Some(ws) = ws else { return Action::None };
        // Whatever was half-typed on a list row is part of the entry the user
        // is looking at, so saving has to take it.
        form.commit_chip();

        let (label, result) = match form.draft {
            DraftEntry::Character(c) => {
                if c.jp_name.trim().is_empty() {
                    self.editing = Some(EditForm {
                        draft: DraftEntry::Character(c),
                        ..form
                    });
                    return Action::Notify {
                        level: LogLevel::Warn,
                        msg: "a character needs a JP name".to_string(),
                    };
                }
                (
                    format!("{} → {}", c.jp_name, c.translated_name),
                    crate::workspace::characters::replace(ws, *c),
                )
            }
            DraftEntry::Glossary(t) => {
                if t.jp_term.trim().is_empty() {
                    self.editing = Some(EditForm {
                        draft: DraftEntry::Glossary(t),
                        ..form
                    });
                    return Action::Notify {
                        level: LogLevel::Warn,
                        msg: "a glossary entry needs a JP term".to_string(),
                    };
                }
                (
                    format!("{} → {}", t.jp_term, t.translated_term),
                    crate::workspace::glossary::replace(ws, *t),
                )
            }
            DraftEntry::StyleNote(text) => (
                "style note".to_string(),
                crate::workspace::style::append_note(ws, &text),
            ),
        };
        match result {
            Ok(()) => Action::Notify {
                level: LogLevel::Info,
                msg: format!("saved · {}", truncate_cols(&label, 48)),
            },
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
        let draft = match self.sub {
            SUB_CHARACTERS => self
                .characters(ws)
                .get(idx)
                .map(|c| DraftEntry::Character(Box::new(c.clone()))),
            SUB_STYLE => None,
            _ => self
                .glossary(ws)
                .get(idx)
                .map(|t| DraftEntry::Glossary(Box::new(t.clone()))),
        };
        match draft {
            Some(draft) => self.editing = Some(EditForm::new(draft, false)),
            None => self.begin_new(),
        }
    }

    fn begin_new(&mut self) {
        let draft = match self.sub {
            SUB_CHARACTERS => DraftEntry::Character(Box::default()),
            SUB_STYLE => DraftEntry::StyleNote(String::new()),
            _ => DraftEntry::Glossary(Box::default()),
        };
        self.editing = Some(EditForm::new(draft, true));
    }

    fn begin_delete(&mut self, ws: Option<&Workspace>) -> Action {
        let Some(ws) = ws else { return Action::None };
        let idx = self.list.selected().unwrap_or(0);
        let labelled: Option<(String, Action)> = match self.sub {
            SUB_CHARACTERS => self.characters(ws).get(idx).map(|c| {
                (
                    format!("{} → {}", c.jp_name, c.translated_name),
                    Action::DeleteCharacter { id: c.id.clone() },
                )
            }),
            SUB_STYLE => None,
            _ => self.glossary(ws).get(idx).map(|t| {
                (
                    format!("{} → {}", t.jp_term, t.translated_term),
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

    pub fn render(
        &mut self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        ws: Option<&Workspace>,
    ) {
        self.screen_area = area;
        let header = Rect {
            height: 1,
            ..area
        };
        let body = Rect {
            y: area.y + 1,
            height: area.height.saturating_sub(1),
            ..area
        };
        let acts = self.actions(ws);
        self.render_header(ui, header, ws, &acts);
        self.render_table(ui, body, ws);

        // The selected row's own verbs, over the right end of the row the table
        // registered while drawing it.
        if let Some(sel) = self.list.selected()
            && let Some(rect) = ui.zones.rect_of(crate::ui::kit::ZoneId::row(sel))
        {
            crate::ui::kit::toolbar::RowActions::new(&acts).render(ui, rect);
        }

        if self.editing.is_some() {
            self.render_edit(ui, area, ws);
        }
    }

    /// Section strip on the left, filter and count on the right.
    fn render_header(
        &mut self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        ws: Option<&Workspace>,
        acts: &[Act],
    ) {
        use crate::ui::kit::ZoneKind;
        use crate::ui::kit::tabs::{Segment, SegmentedControl};
        use crate::ui::kit::toolbar::Toolbar;

        ui.fill(area, Style::default().bg(ui.theme.bg));

        // The count belongs on the section it counts, so each tab carries its
        // own rather than one number floating at the far end describing
        // whichever section happens to be open.
        let (terms, chars) = match ws {
            Some(ws) => (self.glossary(ws).len(), self.characters(ws).len()),
            None => (0, 0),
        };
        let segments = [
            Segment::new("Glossary").badge(terms.to_string()),
            Segment::new("Characters").badge(chars.to_string()),
            Segment::new("Style"),
        ];
        let strip = Rect {
            width: area.width / 2,
            height: 1,
            ..area
        };
        SegmentedControl::new(&segments, self.sub as usize)
            .ids(ZoneKind::Segment, 0)
            .render(ui, strip);

        // Filter, right-aligned, only when there is one or it is being typed.
        // Measured before the toolbar so the two share the free half rather
        // than drawing over each other.
        let faint = Style::default().fg(ui.theme.ink_faint).bg(ui.theme.bg);
        let showing_filter = self.searching || !self.filter.is_empty();
        let filter_cols = if showing_filter {
            col_width(&thai_display_safe(&self.filter)) as u16 + 3
        } else {
            0
        };
        let toolbar_x = area.x + strip.width + 1;
        let toolbar_w = (area.x + area.width)
            .saturating_sub(toolbar_x)
            .saturating_sub(filter_cols + 1);
        Toolbar::new(acts).render(
            ui,
            Rect {
                x: toolbar_x,
                y: area.y,
                width: toolbar_w,
                height: 1,
            },
        );

        if !showing_filter {
            return;
        }
        let mut spans = vec![Span::styled("/ ", faint)];
        if self.searching {
            let (before, after) =
                input::caret_halves(&self.filter, self.filter_cursor, usize::MAX);
            spans.push(Span::styled(before, faint));
            spans.push(Span::styled(
                crate::ui::glyphs::ACCENT_RAIL.as_str().to_string(),
                Style::default().fg(ui.theme.stream_cursor).bg(ui.theme.bg),
            ));
            spans.push(Span::styled(after, faint));
        } else {
            spans.push(Span::styled(thai_display_safe(&self.filter), faint));
        }
        let rw: u16 = spans
            .iter()
            .map(|s| col_width(s.content.as_ref()))
            .sum::<usize>() as u16;
        if area.width > rw + 2 {
            ui.line(
                Rect {
                    x: area.x + area.width - rw - 1,
                    y: area.y,
                    width: rw,
                    height: 1,
                },
                Line::from(spans),
                Style::default().bg(ui.theme.bg),
            );
        }
    }

    fn render_table(&mut self, ui: &mut crate::ui::kit::Ui, area: Rect, ws: Option<&Workspace>) {
        let theme: &Theme = ui.theme;
        let panel = theme.bg_panel;
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(theme.rule))
            .style(Style::default().bg(panel));
        let inner = block.inner(area);
        ui.frame.render_widget(block, area);
        self.table_area = inner;

        let Some(ws) = ws else {
            let faint = Style::default().fg(ui.theme.ink_faint).bg(panel);
            ui.text(
                crate::ui::kit::ctx::row_at(inner, 0),
                "  Open a project (Shelf → ↵) to edit its lexicon.",
                faint,
            );
            return;
        };

        // Everything below sits on the panel, not the screen behind it.
        ui.on_surface(panel, |ui| match self.sub {
            SUB_GLOSSARY => self.render_glossary_table(ui, inner, ws),
            SUB_CHARACTERS => self.render_characters_table(ui, inner, ws),
            _ => {
                let theme: &Theme = ui.theme;
                let f: &mut Frame = ui.frame;
                self.render_style(f, inner, ws, theme);
            }
        });
    }

    /// Split a table area into its header row and its body.
    fn table_rows(area: Rect) -> (Rect, Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(area);
        (rows[0], rows[1])
    }

    /// Draw one of the two tables: header, then body, then the empty-state line
    /// when there is nothing to show.
    fn render_rows<T>(
        &mut self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        columns: &[Column],
        rows: &[T],
        cells: fn(&T) -> Vec<String>,
        empty: &str,
    ) {
        let sort = self.sort[self.sub as usize];
        let (head, body) = Self::table_rows(area);
        table::render_header(ui, head, columns, sort);

        if rows.is_empty() {
            let faint = Style::default().fg(ui.theme.ink_faint).bg(ui.surface());
            ui.text(crate::ui::kit::ctx::row_at(body, 0), empty, faint);
            return;
        }
        table::render_body(ui, body, &mut self.list, columns, rows.len(), |i| {
            cells(&rows[i])
        });
    }

    fn render_glossary_table(&mut self, ui: &mut crate::ui::kit::Ui, area: Rect, ws: &Workspace) {
        let terms = self.glossary(ws);
        if self.list.selected().is_none_or(|s| s >= terms.len()) {
            self.list.select(Some(terms.len().saturating_sub(1)));
        }
        let columns = glossary_columns(ui.theme);
        self.render_rows(
            ui,
            area,
            &columns,
            &terms,
            glossary_cells,
            "  (no terms yet — n to add)",
        );
    }

    fn render_characters_table(
        &mut self,
        ui: &mut crate::ui::kit::Ui,
        area: Rect,
        ws: &Workspace,
    ) {
        let chars = self.characters(ws);
        if self.list.selected().is_none_or(|s| s >= chars.len()) {
            self.list.select(Some(chars.len().saturating_sub(1)));
        }
        let columns = character_table_columns(ui.theme);
        self.render_rows(
            ui,
            area,
            &columns,
            &chars,
            character_cells,
            "  (no characters yet — n to add)",
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
        let total = lines.len();
        let max_scroll = (total as u16).saturating_sub(area.height);
        self.style_scroll = self.style_scroll.min(max_scroll);
        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((self.style_scroll, 0))
                .style(Style::default().bg(theme.bg_panel)),
            area,
        );
        Self::scrollbar(f, area, total, self.style_scroll as usize, theme);
    }

    /// Scrollbar for a table/style region: the given `area` sits inside the
    /// bordered table block, so widen the strip one column to land the bar on
    /// the block's right border.
    fn scrollbar(f: &mut Frame, area: Rect, total: usize, offset: usize, theme: &Theme) {
        crate::ui::widgets::render_scrollbar(
            f,
            Rect {
                x: area.x,
                y: area.y,
                width: area.width.saturating_add(1),
                height: area.height,
            },
            total,
            offset,
            theme,
        );
    }

    /// The focused row's `Combo` vocabulary: what this project already uses.
    ///
    /// Read from the whole roster rather than `self.characters`/`self.glossary`,
    /// which apply the `/` filter — a filtered view would silently shrink the
    /// vocabulary to whatever happened to be on screen.
    fn edit_fields(&self, ws: Option<&Workspace>) -> Vec<crate::ui::kit::form::Field> {
        use crate::ui::kit::form as kit;

        let Some(form) = self.editing.as_ref() else {
            return Vec::new();
        };
        form.rows()
            .iter()
            .filter_map(|f| lexicon_defs::at(*f).map(|d| (*f, d)))
            .map(|(index, d)| {
                let focused = index == form.field;
                let cursor = if focused { form.cursor } else { 0 };
                let kind = match d.kind {
                    Kind::Text | Kind::Numeric => kit::Kind::Text {
                        value: form.value(d.field).as_text().to_string(),
                        cursor,
                        placeholder: String::new(),
                    },
                    Kind::Combo => kit::Kind::Combo {
                        value: form.value(d.field).as_text().to_string(),
                        cursor,
                        known: known_values(ws, Some(d.field)),
                    },
                    Kind::Select => kit::Kind::Select {
                        options: lexicon_defs::POLICIES
                            .iter()
                            .map(|p| lexicon_defs::policy_label(*p).to_string())
                            .collect(),
                        index: match form.value(d.field) {
                            FieldValue::Choice(i) => i,
                            _ => 0,
                        },
                    },
                    Kind::Toggle => kit::Kind::Toggle {
                        on: matches!(form.value(d.field), FieldValue::Flag(true)),
                    },
                    Kind::Chips => kit::Kind::Chips {
                        items: match form.value(d.field) {
                            FieldValue::List(items) => items,
                            _ => Vec::new(),
                        },
                        buffer: if focused {
                            form.buffer.clone()
                        } else {
                            String::new()
                        },
                        cursor,
                    },
                    Kind::Alts => kit::Kind::Chips {
                        items: match form.value(d.field) {
                            FieldValue::Alts(alts) => alts.iter().map(format_alt_name).collect(),
                            _ => Vec::new(),
                        },
                        buffer: if focused {
                            form.buffer.clone()
                        } else {
                            String::new()
                        },
                        cursor,
                    },
                };
                kit::Field::new(d.label, kind).help(d.help)
            })
            .collect()
    }

    fn render_edit(&mut self, ui: &mut crate::ui::kit::Ui, area: Rect, ws: Option<&Workspace>) {
        use crate::ui::kit::button::{Button, ButtonRow};
        use crate::ui::kit::form as kit;
        use crate::ui::kit::modal::{self, Modal, Sizing};
        use crate::ui::kit::zones::ZoneId;

        let Some(entry) = self.editing.as_ref().map(|f| f.kind()) else {
            return;
        };
        if entry == EntryKind::StyleNote {
            self.render_edit_note(ui, area);
            return;
        }

        let fields = self.edit_fields(ws);
        let Some(form) = self.editing.as_mut() else {
            return;
        };
        let title = if form.is_new {
            format!("New {}", entry.title())
        } else {
            format!("Edit {}", entry.title())
        };
        let frame = Modal::new(&title)
            .sizing(
                Sizing::medium()
                    .fit_width(72)
                    // Border, padding, title and the two footer rows.
                    .fit_height(fields.len() as u16 + 7),
            )
            .footer(2)
            .render(ui, area);

        let base = entry.first_field().unwrap_or(0) as u32;
        let local = form.local();
        form.list.select(Some(local));
        kit::render(
            ui,
            frame.body,
            &mut form.list,
            &fields,
            kit::Opts {
                label_cols: 16,
                id_base: base,
            },
        );
        kit::render_help(
            ui,
            Rect {
                height: 1,
                ..frame.footer
            },
            fields.get(local),
        );
        modal::render_footer(
            ui,
            frame.footer,
            ButtonRow::new(vec![
                Button::new(ZoneId::button(FORM_CANCEL), "Cancel").accel("esc"),
                Button::new(ZoneId::button(FORM_SAVE), "Save")
                    .accel("^s")
                    .primary(),
            ]),
        );
    }

    /// A style note is prose, not a set of fields, so it gets an editor.
    fn render_edit_note(&mut self, ui: &mut crate::ui::kit::Ui, area: Rect) {
        use crate::ui::kit::button::{Button, ButtonRow};
        use crate::ui::kit::editor;
        use crate::ui::kit::modal::{self, Modal, Sizing};
        use crate::ui::kit::zones::ZoneId;

        let Some(form) = self.editing.as_mut() else {
            return;
        };
        let DraftEntry::StyleNote(text) = &form.draft else {
            return;
        };
        let frame = Modal::new("New style note").sizing(Sizing::small()).footer(1).render(ui, area);
        let lines = editor::wrap(text, frame.body.width);
        editor::render(
            ui,
            frame.body,
            &editor::View::new(text, &lines).cursor(form.cursor),
            0,
        );
        modal::render_footer(
            ui,
            frame.footer,
            ButtonRow::new(vec![
                Button::new(ZoneId::button(FORM_CANCEL), "Cancel").accel("esc"),
                Button::new(ZoneId::button(FORM_SAVE), "Save")
                    .accel("^s")
                    .primary()
                    .disabled(text.trim().is_empty()),
            ]),
        );
    }

    /// This screen's commands, availability resolved for this frame.
    ///
    /// Everything greys out while a form or the filter holds the keyboard —
    /// those keys belong to the field being typed into — rather than the
    /// toolbar vanishing and the header changing shape mid-word.
    pub fn actions(&self, ws: Option<&Workspace>) -> Vec<Act> {
        use action_table::Accel;

        let live = ws.is_some() && !self.is_capturing();
        let has_ws = live;
        // Style is prose, not a list: it has no row to edit or delete.
        let on_row = live && self.sub != SUB_STYLE && self.current_len(ws) > 0;
        vec![
            Act::toolbar(L_NEW, "new", Accel::key('n')).when(has_ws),
            Act::toolbar(L_SEARCH, "search", Accel::key('/')).when(!self.is_capturing()),
            Act::row(
                L_EDIT,
                "edit",
                Accel::code(KeyCode::Enter).or(KeyCode::Char('e')),
            )
            .when(on_row),
            Act::row(L_DELETE, "delete", Accel::key('d')).when(on_row),
        ]
    }

    /// Run the action `id` stands for, however it was reached. `None` means
    /// no such action here.
    pub fn run(&mut self, id: u16, ws: Option<&Workspace>) -> Option<Action> {
        Some(match id {
            L_NEW => {
                if ws.is_some() {
                    self.begin_new();
                }
                Action::None
            }
            L_SEARCH => {
                self.searching = true;
                self.filter.clear();
                self.filter_cursor = 0;
                Action::None
            }
            L_EDIT => {
                self.begin_edit(ws);
                Action::None
            }
            L_DELETE => self.begin_delete(ws),
            _ => return None,
        })
    }

    /// Navigation only, except inside a form — where the keys genuinely have
    /// no control, because the form is the control.
    pub fn hints(&self) -> &'static [(&'static str, &'static str)] {
        if self.editing.is_some() {
            return &[
                ("Tab", "field"),
                ("↵", "next"),
                ("^s", "save"),
                ("Esc", "cancel"),
            ];
        }
        &[("Tab", "section"), ("↑↓", "move")]
    }
}

/// The glossary's columns. Width, priority and ink are declared once here
/// rather than re-derived by the renderer, so what happens at 60 columns is a
/// property of this table and not of whoever wrote that particular loop.
fn glossary_columns(theme: &Theme) -> Vec<Column> {
    vec![
        Column::new("JP term", Width::Flex { min: 8, weight: 2 })
            .priority(200)
            .tint(theme.ink),
        Column::new("Target term", Width::Flex { min: 10, weight: 2 })
            .priority(190)
            .tint(theme.translated_text),
        Column::new("Cat", Width::Fixed(8)).priority(60),
        Column::new("Policy", Width::Fixed(10)).priority(50),
        Column::new("DNT", Width::Fixed(3)).priority(40),
        Column::new("Notes", Width::Flex { min: 8, weight: 3 }).priority(30),
    ]
}

fn glossary_cells(t: &GlossaryTerm) -> Vec<String> {
    vec![
        t.jp_term.clone(),
        thai_display_safe(&t.translated_term),
        thai_display_safe(t.category.as_deref().unwrap_or("—")),
        policy_short(crate::workspace::glossary::effective_policy(t)).to_string(),
        if t.do_not_translate.unwrap_or(false) {
            "✓".into()
        } else {
            "·".into()
        },
        thai_display_safe(&term_note(t)),
    ]
}

fn character_table_columns(theme: &Theme) -> Vec<Column> {
    vec![
        Column::new("JP name", Width::Flex { min: 8, weight: 2 })
            .priority(200)
            .tint(theme.ink),
        Column::new("Target name", Width::Flex { min: 10, weight: 2 })
            .priority(190)
            .tint(theme.translated_text),
        Column::new("Gender", Width::Fixed(8)).priority(60),
        Column::new("Names / Notes", Width::Flex { min: 10, weight: 3 }).priority(30),
    ]
}

fn character_cells(c: &Character) -> Vec<String> {
    vec![
        c.jp_name.clone(),
        thai_display_safe(&c.translated_name),
        thai_display_safe(c.gender.as_deref().unwrap_or("—")),
        thai_display_safe(&character_extra(c)),
    ]
}

/// Order `rows` by the same text the table shows, so what a header click sorts
/// by is what the column under it displays.
fn sort_rows<T>(rows: &mut [T], sort: table::Sort, cells: fn(&T) -> Vec<String>) {
    rows.sort_by(|a, b| {
        let key = |v: &T| {
            cells(v)
                .get(sort.column)
                .cloned()
                .unwrap_or_default()
                .to_lowercase()
        };
        let ord = key(a).cmp(&key(b));
        if sort.descending { ord.reverse() } else { ord }
    });
}


fn character_matches_filter(c: &Character, q: &str) -> bool {
    let fields = [
        c.id.as_str(),
        c.jp_name.as_str(),
        c.translated_name.as_str(),
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
                || a.translated_name.to_lowercase().contains(q)
                || a.by.as_deref().unwrap_or("").to_lowercase().contains(q)
        })
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
    let mut out = if a.translated_name.trim().is_empty() {
        a.jp.trim().to_string()
    } else {
        format!("{}→{}", a.jp.trim(), a.translated_name.trim())
    };
    if let Some(by) = a.by.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        out.push_str(" @ ");
        out.push_str(by);
    }
    out
}

fn parse_alt_name(piece: &str) -> Option<AltName> {
    let (body, by) = piece
        .rsplit_once('@')
        .map(|(body, by)| (body.trim(), opt(by.trim().to_string())))
        .unwrap_or((piece.trim(), None));
    let (jp, translated) = body
        .split_once('→')
        .or_else(|| body.split_once("=>"))
        .or_else(|| body.split_once('='))
        .map(|(jp, translated)| (jp.trim(), translated.trim()))
        .unwrap_or((body, ""));
    if jp.is_empty() {
        return None;
    }
    Some(AltName {
        jp: jp.to_string(),
        translated_name: translated.to_string(),
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

/// What a `Combo` row offers: the values this project already uses.
///
/// Read from the whole roster rather than through the `/` filter, which would
/// silently shrink the vocabulary to whatever happened to be on screen.
fn known_values(ws: Option<&Workspace>, field: Option<LexField>) -> Vec<String> {
    let (Some(ws), Some(field)) = (ws, field) else {
        return Vec::new();
    };
    let mut seen: Vec<String> = match field {
        LexField::CGender => crate::workspace::characters::load(ws)
            .iter()
            .filter_map(|c| c.gender.clone())
            .collect(),
        LexField::GCategory => crate::workspace::glossary::load(ws)
            .iter()
            .filter_map(|t| t.category.clone())
            .collect(),
        _ => Vec::new(),
    };
    seen.retain(|v| !v.trim().is_empty());
    seen.sort();
    seen.dedup();
    seen
}

fn opt(s: String) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
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

/// A stable id derived from a JP name (mirrors the workspace's slugify-jp rule:
/// non-ASCII preserved, ASCII lowered, separators collapsed).
#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{Workspace, characters};

    /// The section strip answers to the pointer, not only to Tab.
    #[test]
    fn clicking_a_section_switches_to_it() {
        let mut s = LexiconScreen::new();
        assert_eq!(s.sub, SUB_GLOSSARY);

        let (_, zones) =
            crate::ui::kit::ctx::draw_test(100, 20, |ui, area| s.render(ui, area, None));
        let rect = zones
            .rect_of(crate::ui::kit::ZoneId::segment(SUB_CHARACTERS as usize))
            .expect("the Characters section should register a zone");
        let (col, row) = (rect.x + 1, rect.y);

        s.handle_mouse(
            MouseInput {
                gesture: MouseGesture::Click { double: false },
                col,
                row,
            },
            zones.at(col, row),
            None,
        );
        assert_eq!(s.sub, SUB_CHARACTERS, "clicking a section must switch to it");
    }

    fn temp_ws(tag: &str) -> (std::path::PathBuf, Workspace) {
        let base = std::env::temp_dir().join(format!("honya_lexicon_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let ws = Workspace::new(base.clone(), 1);
        (base, ws)
    }

    fn character_named(jp: &str, translated: &str) -> Character {
        Character {
            jp_name: jp.into(),
            translated_name: translated.into(),
            ..character()
        }
    }

    fn character() -> Character {
        Character {
            id: "char-3199b4b0".into(),
            jp_name: "清水圭".into(),
            translated_name: "ชิมิซุ เค".into(),
            romaji: Some("Shimizu Kei".into()),
            gender: Some("female".into()),
            honorific: Some("คุณ".into()),
            speech_style: None,
            relationships: Vec::new(),
            aliases: vec!["圭".into(), "シミズ".into()],
            also_called: vec![AltName {
                jp: "ケ様".into(),
                translated_name: "ท่านเค".into(),
                by: Some("清水愛".into()),
            }],
            notes: Some("นางเอก".into()),
            first_seen_chapter: None,
        }
    }

    /// The property the bespoke width function used to guard, now a property of
    /// the column declaration: whatever the pane width, the columns fill it
    /// exactly, and the flexible Names/Notes column is the one that grows.
    #[test]
    fn character_columns_fill_width_no_dead_space() {
        let theme = crate::model::ThemeId::default().build();
        let columns = character_table_columns(&theme);
        for w in [10u16, 30, 47, 67, 80, 200, 1900] {
            let cols = table::layout(&columns, w);
            let used: u16 = cols.iter().map(|&(_, cw)| cw).sum::<u16>()
                + cols.len().saturating_sub(1) as u16;
            assert_eq!(used, w, "columns should fill width {w} exactly");
        }
        // Wide panes spend their slack on Names/Notes, not on the name columns.
        let wide = table::layout(&columns, 200);
        let width_of = |i: usize| wide.iter().find(|&&(c, _)| c == i).map(|&(_, w)| w);
        assert!(
            width_of(3) > width_of(1),
            "Names/Notes should be the flexible column: {wide:?}"
        );
    }

    /// Sorting is by the same text the column shows, so a header click orders
    /// the rows the way the thing under it reads.
    #[test]
    fn a_header_sorts_by_what_its_column_displays() {
        let mut rows = vec![
            character_named("清水圭", "ชิมิซุ"),
            character_named("安藤", "อันโด"),
            character_named("村上", "มุราคามิ"),
        ];
        let names = |rows: &[Character]| -> Vec<String> {
            rows.iter().map(|c| c.jp_name.clone()).collect()
        };

        sort_rows(&mut rows, table::Sort::default(), character_cells);
        let mut sorted = names(&rows);
        sorted.sort();
        assert_eq!(names(&rows), sorted, "ascending by the JP name column");

        sort_rows(
            &mut rows,
            table::Sort {
                column: 0,
                descending: true,
            },
            character_cells,
        );
        sorted.reverse();
        assert_eq!(names(&rows), sorted, "clicking again flips the direction");
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

    /// The bug this redesign exists to fix. The form presented as an editor and
    /// behaved as a filler-in: `upsert` merges, so a blanked field kept its old
    /// value and a removed alias came straight back on the next load.
    #[test]
    fn clearing_a_field_actually_clears_it() {
        let (base, ws) = temp_ws("clear_field");
        characters::upsert(&ws, character()).unwrap();
        let mut screen = LexiconScreen::new();
        screen.sub = SUB_CHARACTERS;
        screen.list.select(Some(0));
        screen.begin_edit(Some(&ws));

        {
            let form = screen.editing.as_mut().expect("the row should open a form");
            for field in [LexField::CNotes, LexField::CHonorific] {
                form.focus_field(lexicon_defs::index_of(field));
                form.set_current_text(String::new());
            }
            form.focus_field(lexicon_defs::index_of(LexField::CAliases));
            form.lift_chip(1);
            form.buffer.clear();
        }
        assert!(matches!(
            screen.commit_edit(Some(&ws)),
            Action::Notify {
                level: LogLevel::Info,
                ..
            }
        ));

        let stored = characters::load(&ws);
        let stored = stored.first().expect("the character should still be there");
        assert_eq!(stored.notes, None, "a blanked note has to actually go");
        assert_eq!(stored.honorific, None, "a blanked honorific has to actually go");
        assert_eq!(
            stored.aliases,
            vec!["圭".to_string()],
            "a removed alias must not come back"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The invariant that already held, pinned so `replace` cannot break it: a
    /// field the form does not show is carried through an edit untouched.
    #[test]
    fn editing_an_entry_preserves_every_field_it_does_not_show() {
        let (base, ws) = temp_ws("preserve_fields");
        let mut seed = character();
        seed.relationships = vec![crate::model::Relationship {
            target_id: "char-ai".into(),
            relation: "sister".into(),
        }];
        characters::upsert(&ws, seed.clone()).unwrap();

        let mut screen = LexiconScreen::new();
        screen.sub = SUB_CHARACTERS;
        screen.list.select(Some(0));
        screen.begin_edit(Some(&ws));
        {
            let form = screen.editing.as_mut().expect("the row should open a form");
            form.focus_field(lexicon_defs::index_of(LexField::CNotes));
            form.set_current_text("พระเอก".to_string());
        }
        screen.commit_edit(Some(&ws));

        let stored = characters::load(&ws);
        let stored = stored.first().expect("the character should still be there");
        assert_eq!(stored.id, seed.id, "the id is the key and has no row");
        assert_eq!(stored.relationships, seed.relationships);
        assert_eq!(stored.romaji, seed.romaji);
        assert_eq!(stored.notes.as_deref(), Some("พระเอก"));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Every row the form draws is clickable, and the click lands on the row
    /// under the pointer — the drift the zone registry exists to remove.
    #[test]
    fn every_edit_row_registers_a_zone_a_click_focuses() {
        let mut screen = LexiconScreen::new();
        screen.sub = SUB_CHARACTERS;
        screen.begin_new();
        let rows = screen.editing.as_ref().unwrap().rows();

        let (_, zones) =
            crate::ui::kit::ctx::draw_test(100, 40, |ui, area| screen.render(ui, area, None));
        for field in &rows {
            let id = crate::ui::kit::ZoneId::new(crate::ui::kit::ZoneKind::Field, *field as u32);
            let rect = zones
                .rect_of(id)
                .unwrap_or_else(|| panic!("row {field} registered no zone"));
            let (col, row) = (rect.x + 1, rect.y);
            screen.handle_mouse(
                MouseInput {
                    gesture: MouseGesture::Click { double: false },
                    col,
                    row,
                },
                zones.at(col, row),
                None,
            );
            assert_eq!(
                screen.editing.as_ref().unwrap().field,
                *field,
                "clicking row {field} focused something else"
            );
        }
    }

    /// The old form was a hardcoded `fields * 2 + 6` rows tall, so a character
    /// simply ran off a short terminal. The kit's form windows instead.
    #[test]
    fn a_long_form_scrolls_instead_of_overflowing() {
        let mut screen = LexiconScreen::new();
        screen.sub = SUB_CHARACTERS;
        screen.begin_new();
        let rows = screen.editing.as_ref().unwrap().rows();
        let last = *rows.last().unwrap();
        screen.editing.as_mut().unwrap().focus_field(last);

        let (_, zones) =
            crate::ui::kit::ctx::draw_test(80, 10, |ui, area| screen.render(ui, area, None));
        let id = crate::ui::kit::ZoneId::new(crate::ui::kit::ZoneKind::Field, last as u32);
        let rect = zones
            .rect_of(id)
            .expect("the focused row must be scrolled into view");
        assert!(rect.y < 10, "the focused row must be on screen");
        assert!(
            zones
                .rect_of(crate::ui::kit::ZoneId::new(
                    crate::ui::kit::ZoneKind::Field,
                    rows[0] as u32
                ))
                .is_none(),
            "a form taller than the terminal must window, not overflow"
        );
    }

    /// Typing used to run over every row, including the ones that hold an enum,
    /// which is how a keystroke on Policy corrupted the value.
    #[test]
    fn typing_on_a_select_row_cycles_it_instead_of_mangling_it() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent};

        let mut screen = LexiconScreen::new();
        screen.sub = SUB_GLOSSARY;
        screen.begin_new();
        let policy = lexicon_defs::index_of(LexField::GPolicy);
        screen.editing.as_mut().unwrap().focus_field(policy);

        let before = screen.editing.as_ref().unwrap().value(LexField::GPolicy);
        screen.handle_edit_key(KeyEvent::from(KeyCode::Char('x')), None);
        assert_eq!(
            screen.editing.as_ref().unwrap().value(LexField::GPolicy),
            before,
            "a letter must not reach a row that holds an enum"
        );

        screen.handle_edit_key(KeyEvent::from(KeyCode::Right), None);
        assert_ne!(
            screen.editing.as_ref().unwrap().value(LexField::GPolicy),
            before,
            "→ should step the enum"
        );
    }

    /// A `Combo` offers what the project already says, in the project's own
    /// language — a fixed list would be wrong in every project but one.
    #[test]
    fn a_combo_learns_its_options_from_the_project() {
        let (base, ws) = temp_ws("combo_known");
        characters::upsert(&ws, character()).unwrap();
        let mut other = character_named("村上", "มุราคามิ");
        other.id = "char-murakami".into();
        other.gender = Some("ชาย".into());
        characters::upsert(&ws, other).unwrap();

        let known = known_values(Some(&ws), Some(LexField::CGender));
        assert!(known.contains(&"female".to_string()));
        assert!(known.contains(&"ชาย".to_string()));
        assert_eq!(known.len(), 2, "duplicates should collapse: {known:?}");
        let _ = std::fs::remove_dir_all(&base);
    }
}
