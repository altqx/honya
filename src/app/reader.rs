//! src/app/reader.rs — the Reader / Diff view (4 読).
//!
//! Synced side-by-side Japanese source and target translation for proofreading. A single
//! shared scroll position drives both panes; `z` decouples; `[`/`]` move between
//! chapters; `o` cycles split / JA-only / translation-only; `w` toggles wrap.

use std::cell::RefCell;
use std::hash::{Hash, Hasher};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use chrono::{DateTime, Utc};

use crate::model::{ChapterRun, ReaderAnnotation};
use crate::theme::{self, Theme};
use crate::ui::mouse::{MouseGesture, MouseInput};
use crate::workspace::Workspace;

use super::action_table::{self, Act};
use super::Action;
use super::overlay::Overlay;

/// Action ids for this screen's table. Stable within the screen: they are also
/// the zone index every one of its controls registers under.
const A_SYNC: u16 = 0;
const A_WRAP: u16 = 1;
const A_MODE: u16 = 2;
const A_HILITE: u16 = 3;
const A_NOTES: u16 = 4;
const A_DIFF: u16 = 5;
const A_SEARCH: u16 = 6;
const A_JUMP: u16 = 7;
const A_INSPECT: u16 = 8;
const A_EDIT: u16 = 9;
const A_NOTE: u16 = 10;
const A_MARK: u16 = 11;
const A_REVIEW: u16 = 12;
const A_SOURCE: u16 = 13;
pub(crate) const A_COPY: u16 = 14;
const A_QA: u16 = 15;
const A_SEARCH_NEXT: u16 = 16;
const A_SEARCH_PREV: u16 = 17;
const A_PREV_CH: u16 = 18;
const A_NEXT_CH: u16 = 19;

/// Layout modes for `o`.
const MODE_SPLIT: u8 = 0;
const MODE_JA: u8 = 1;
const MODE_TRANSLATION: u8 = 2;

/// Default soft / hard chunk budgets, mirroring `AppConfig`. Used to re-derive JA
/// chunk boundaries for `s` (show source) until the App seeds the live values.
const DEFAULT_CHUNK_TARGET: usize = 1000;
const DEFAULT_CHUNK_HARD_CAP: usize = 1200;

/// Which pane a search hit lives in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Ja,
    Translation,
}

/// One global-search match: a pane and the (0-based) source line it sits on.
#[derive(Clone, Copy, Debug)]
struct SearchHit {
    side: Side,
    line: u16,
}

/// Active Reader search across both panes.
#[derive(Clone, Debug)]
struct ReaderSearch {
    /// Query as typed (matched against the JA pane).
    query: String,
    /// Display-safe form of the query (matched against the decomposed translation pane).
    translated_query: String,
    hits: Vec<SearchHit>,
    sel: usize,
}

pub struct ReaderScreen {
    scroll: u16,
    /// Translation-pane scroll offset, used only when sync is off.
    translation_scroll: u16,
    sync: bool,
    wrap: bool,
    layout_mode: u8,
    pub(crate) ja: String,
    pub(crate) translated_text: String,
    annotations: Vec<ReaderAnnotation>,
    show_annotations: bool,
    pub(crate) chapter: u32,
    /// Rerun comparison for the current chapter, present only when an earlier
    /// version is archived (i.e. the chapter has been retranslated at least once).
    compare: Option<RerunCompare>,
    /// Diff mode active: side-by-side old vs new translation. Only enterable when
    /// `compare` is `Some`; `d` toggles it.
    diff_mode: bool,
    /// Glossary/character JP forms present in this chapter, tinted in the JA pane.
    hl_ja: Vec<String>,
    /// Glossary/character target forms present in this chapter, display-safe.
    translated_highlights: Vec<String>,
    /// Whether glossary-term highlighting is on (toggle with `G`).
    highlight: bool,
    /// Active search across both panes, or `None`.
    search: Option<ReaderSearch>,
    /// `[REVIEW NEEDED]` banner line anchors (0-based) in the translation pane, for `r`.
    review_lines: Vec<u16>,
    /// Bookmark line anchors (1-based) for the current chapter, for the status badge.
    bookmark_lines: Vec<u32>,
    /// Soft / hard chunk budgets used to align a translation chunk to its JA source (`s`).
    chunk_cfg: (usize, usize),
    /// Pane rectangles, refreshed every frame, so the wheel scrolls whichever pane
    /// the pointer is over when the two are decoupled. Empty when a pane is hidden.
    ja_area: Rect,
    translation_area: Rect,
    /// Clickable cells of the status bar (sync/wrap/mode/… toggles), refreshed
    /// every frame like the pane rects.
    /// Bumped whenever the rendered *content* of a pane changes (chapter load, note
    /// edits). Folds into the per-pane cache key so the expensive Markdown parse is
    /// skipped on the 100 ms ticker / pipeline events while reading a static chapter.
    content_rev: u64,
    /// Memoized rendered lines for the JA / translation panes, rebuilt only when their cache
    /// key (content_rev, width, theme, highlight/search state …) changes — never on a
    /// bare scroll or animation tick.
    ja_cache: RefCell<crate::ui::markdown::RenderCache>,
    translation_cache: RefCell<crate::ui::markdown::RenderCache>,
}

impl ReaderScreen {
    #[cfg(test)]
    pub fn scroll_for_test(&self) -> u16 {
        self.scroll
    }

    #[cfg(test)]
    pub fn set_scroll_for_test(&mut self, scroll: u16) {
        self.scroll = scroll;
    }

    pub fn new() -> Self {
        Self {
            scroll: 0,
            translation_scroll: 0,
            sync: true,
            wrap: true,
            layout_mode: MODE_SPLIT,
            ja: String::new(),
            translated_text: String::new(),
            annotations: Vec::new(),
            show_annotations: true,
            chapter: 0,
            compare: None,
            diff_mode: false,
            hl_ja: Vec::new(),
            translated_highlights: Vec::new(),
            highlight: true,
            search: None,
            review_lines: Vec::new(),
            bookmark_lines: Vec::new(),
            chunk_cfg: (DEFAULT_CHUNK_TARGET, DEFAULT_CHUNK_HARD_CAP),
            ja_area: Rect::default(),
            translation_area: Rect::default(),
            content_rev: 0,
            ja_cache: RefCell::new(crate::ui::markdown::RenderCache::default()),
            translation_cache: RefCell::new(crate::ui::markdown::RenderCache::default()),
        }
    }

    /// Seed the soft / hard chunk budgets from the live `AppConfig` so `s` (show
    /// source) re-chunks the JA raw exactly as the pipeline did. Called once at
    /// startup; the defaults match `AppConfig` so it is safe before this runs.
    pub fn set_chunk_cfg(&mut self, target: usize, hard_cap: usize) {
        self.chunk_cfg = (target.max(1), hard_cap.max(target.max(1)));
    }

    /// Enter the rerun diff view (old vs new translation).
    pub fn enter_diff(&mut self) {
        self.diff_mode = true;
    }

    /// Reload only if the Reader is already showing `chapter`.
    pub fn reload_if_showing(&mut self, ws: &Workspace, chapter: u32) {
        if self.chapter == chapter {
            self.load(ws, chapter);
        }
    }

    /// Load raw/ (JA) + translated/ (translation) for a chapter.
    pub fn load(&mut self, ws: &Workspace, chapter: u32) {
        self.chapter = chapter;
        self.scroll = 0;
        self.translation_scroll = 0;
        self.diff_mode = false;
        self.ja = std::fs::read_to_string(ws.raw(chapter))
            .unwrap_or_else(|_| "(raw not found)".to_string());
        let translated = std::fs::read_to_string(ws.translated(chapter))
            .unwrap_or_else(|_| "(not translated yet)".to_string());
        // Decompose Thai SARA AM so it never lands as a width-2 single cell that
        // desyncs the terminal and smears ำ across the screen on the next redraw.
        self.translated_text = crate::ui::text::thai_display_safe(&translated);
        self.search = None;
        self.content_rev = self.content_rev.wrapping_add(1);
        self.reload_annotations(ws);
        self.reload_highlight_terms(ws);
        self.reload_bookmarks(ws);
        self.recompute_review_lines();
        self.load_compare(ws);
    }

    /// Load the project glossary + characters and keep only the source/target forms that
    /// actually appear in this chapter, so highlighting is bounded per chapter (the
    /// same present-only filter `build_reference_ctx` uses for injected context).
    fn reload_highlight_terms(&mut self, ws: &Workspace) {
        self.hl_ja.clear();
        self.translated_highlights.clear();
        if self.chapter == 0 {
            return;
        }
        let glossary = crate::workspace::glossary::load(ws);
        let characters = crate::workspace::characters::load(ws);

        // translation forms: decompose like the pane content, keep only those present.
        let mut translated_seen = std::collections::HashSet::new();
        let mut translated_highlights = Vec::new();
        let translated_forms = glossary
            .iter()
            .map(|t| t.translated_term.as_str())
            .chain(characters.iter().map(|c| c.translated_name.as_str()));
        for raw in translated_forms {
            let safe = crate::ui::text::thai_display_safe(raw.trim());
            // Match case-insensitively to stay consistent with the renderer's
            // `highlight` (matters only for ASCII terms like "HP"/"SSR").
            if !safe.is_empty()
                && crate::ui::markdown::contains_ci(&self.translated_text, &safe)
                && translated_seen.insert(safe.clone())
            {
                translated_highlights.push(safe);
            }
        }

        // JP forms: exact substring against the raw pane (same as build_reference_ctx).
        let mut ja_seen = std::collections::HashSet::new();
        let mut hl_ja = Vec::new();
        let ja_forms = glossary
            .iter()
            .map(|t| t.jp_term.as_str())
            .chain(characters.iter().map(|c| c.jp_name.as_str()));
        for raw in ja_forms {
            let jp = raw.trim();
            if !jp.is_empty()
                && crate::ui::markdown::contains_ci(&self.ja, jp)
                && ja_seen.insert(jp.to_string())
            {
                hl_ja.push(jp.to_string());
            }
        }

        self.translated_highlights = translated_highlights;
        self.hl_ja = hl_ja;
    }

    /// Refresh the current chapter's bookmark line anchors from VOLUME.md.
    pub fn reload_bookmarks(&mut self, ws: &Workspace) {
        self.bookmark_lines = if self.chapter == 0 {
            Vec::new()
        } else {
            crate::workspace::volume::reader_bookmarks(ws)
                .into_iter()
                .filter(|b| b.chapter == self.chapter)
                .map(|b| b.line)
                .collect()
        };
    }

    /// Cache the (0-based) source lines carrying a visible `[REVIEW NEEDED]` banner,
    /// in document order, so `r` can cycle through them and the status bar can count.
    fn recompute_review_lines(&mut self) {
        self.review_lines = self
            .translated_text
            .lines()
            .enumerate()
            .filter(|(_, l)| l.contains("[REVIEW NEEDED]"))
            .map(|(i, _)| i as u16)
            .collect();
    }

    /// Build the rerun comparison for the current chapter from its run records and
    /// the most recently archived prior version. Leaves `compare` `None` when the
    /// chapter has never been retranslated (nothing to compare).
    fn load_compare(&mut self, ws: &Workspace) {
        self.compare = None;
        if self.chapter == 0 || !ws.translated(self.chapter).is_file() {
            return;
        }
        let runs = crate::workspace::volume::chapter_runs(ws, self.chapter);
        let (prev, live) = select_compare_runs(&runs);
        let Some(prev) = prev else { return };
        let Some(rel) = prev.archived.as_deref() else {
            return;
        };
        let Ok(old_raw) = std::fs::read_to_string(ws.vol_rel(rel)) else {
            return;
        };

        let old_translation = crate::workspace::translation::prose_only(
            &crate::ui::text::thai_display_safe(&old_raw),
        );
        // `self.translated_text` is already display-safe; strip the chunk markers for a clean diff.
        let new_translation = crate::workspace::translation::prose_only(&self.translated_text);
        let line = crate::ui::diff::diff_lines(&old_translation, &new_translation);

        let old_cost = (!prev.usage_unknown).then_some(prev.usage.cost_usd);
        let new_cost = live.and_then(|r| (!r.usage_unknown).then_some(r.usage.cost_usd));
        let (new_review, new_failed) = match live {
            Some(r) => (r.review_needed, r.failed),
            // No recorded live run (e.g. a rerun crashed before finishing): read the
            // review-needed count straight off the live file.
            None => (
                crate::workspace::translation::review_needed_chunk_indices_in(&self.translated_text)
                    .len() as u32,
                false,
            ),
        };
        let qa = qa_trend(prev.failed, prev.review_needed, new_failed, new_review);
        let (terms_added, terms_changed) = live
            .map(|r| (r.glossary_added.len(), r.glossary_changed.len()))
            .unwrap_or((0, 0));

        self.compare = Some(RerunCompare {
            old_label: short_dt(prev.finished_at),
            new_label: live
                .map(|r| short_dt(r.finished_at))
                .unwrap_or_else(|| "live".to_string()),
            old_translation,
            new_translation,
            line,
            old_cost,
            new_cost,
            old_review: prev.review_needed,
            new_review,
            old_failed: prev.failed,
            new_failed,
            qa,
            terms_added,
            terms_changed,
        });
    }

    pub fn reload_annotations(&mut self, ws: &Workspace) {
        self.annotations = if self.chapter == 0 {
            Vec::new()
        } else {
            crate::workspace::volume::reader_annotations(ws, self.chapter)
        };
        // Notes are interleaved into the translation pane, so a note edit changes its render.
        self.content_rev = self.content_rev.wrapping_add(1);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        // Commands come from the table, so the key printed on a chip or listed
        // in the menu is the one that runs it. Only navigation is left here.
        let acts = self.actions();
        match action_table::hit(&acts, &key) {
            action_table::KeyHit::Run(id) => return self.run(id).unwrap_or(Action::None),
            action_table::KeyHit::Blocked => return Action::None,
            action_table::KeyHit::Miss => {}
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.scroll_by(1);
                Action::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.scroll_by(-1);
                Action::None
            }
            KeyCode::Char(' ') => {
                self.scroll_by(10);
                Action::None
            }
            KeyCode::Char('b') | KeyCode::PageUp => {
                self.scroll_by(-10);
                Action::None
            }
            KeyCode::Esc if self.search.is_some() => {
                self.search = None;
                Action::None
            }
            _ => Action::None,
        }
    }

    /// Mouse: the wheel scrolls. When the panes are synced (or in diff mode) both
    /// move together; when decoupled, only the pane under the pointer scrolls — so
    /// you can read JA and translation at independent positions with the wheel. A click on
    /// a status-bar cell fires its key binding (sync/wrap/mode/…).
    pub fn handle_mouse(
        &mut self,
        m: MouseInput,
        zone: Option<crate::ui::kit::ZoneId>,
    ) -> Action {
        match m.gesture {
            MouseGesture::ScrollUp => self.scroll_targeted(m.col, -3),
            MouseGesture::ScrollDown => self.scroll_targeted(m.col, 3),
            // A toolbar control is registered under its action id and is
            // answered by the router, which runs the same `run` arm the key
            // does — so there is nothing for the screen to mirror here.
            MouseGesture::Click { .. } | MouseGesture::RightClick => {}
        }
        let _ = zone;
        Action::None
    }

    fn scroll_targeted(&mut self, col: u16, delta: i32) {
        // Diff and synced reading both move a single shared offset.
        if self.diff_mode || self.sync {
            self.scroll_by(delta);
            return;
        }
        if col_in(self.translation_area, col) {
            self.translation_scroll = step(self.translation_scroll, delta);
        } else if col_in(self.ja_area, col) {
            self.scroll = step(self.scroll, delta);
        } else {
            self.scroll_by(delta);
        }
    }

    fn scroll_by(&mut self, delta: i32) {
        self.scroll = step(self.scroll, delta);
        if self.sync {
            self.translation_scroll = self.scroll;
        } else {
            self.translation_scroll = step(self.translation_scroll, delta);
        }
    }

    pub fn render(&mut self, ui: &mut crate::ui::kit::Ui, area: Rect) {
        if self.diff_mode {
            if self.compare.is_some() {
                self.render_diff(ui, area);
                return;
            }
            self.diff_mode = false; // compare went away (e.g. chapter reloaded)
        }

        let theme: &Theme = ui.theme;
        let f: &mut Frame = ui.frame;

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(4), Constraint::Length(1)])
            .split(area);
        let body = rows[0];

        match self.layout_mode {
            MODE_JA => {
                self.ja_area = body;
                self.translation_area = Rect::default();
                self.render_pane(
                    f,
                    body,
                    theme,
                    "Japanese (raw)",
                    &self.ja,
                    theme.ja_text,
                    self.scroll,
                    false,
                    None,
                );
            }
            MODE_TRANSLATION => {
                self.ja_area = Rect::default();
                self.translation_area = body;
                self.render_pane(
                    f,
                    body,
                    theme,
                    "Translation",
                    &self.translated_text,
                    theme.translated_text,
                    self.effective_translation_scroll(),
                    true,
                    Some(&self.annotations),
                );
            }
            _ => {
                let cols = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(48), Constraint::Percentage(52)])
                    .split(body);
                self.ja_area = cols[0];
                self.translation_area = cols[1];
                self.render_pane(
                    f,
                    cols[0],
                    theme,
                    "Japanese (raw)",
                    &self.ja,
                    theme.ja_text,
                    self.scroll,
                    false,
                    None,
                );
                self.render_pane(
                    f,
                    cols[1],
                    theme,
                    "Translation",
                    &self.translated_text,
                    theme.translated_text,
                    self.effective_translation_scroll(),
                    true,
                    Some(&self.annotations),
                );
            }
        }

        self.render_status(ui, rows[1]);
    }

    fn effective_translation_scroll(&self) -> u16 {
        if self.sync {
            self.scroll
        } else {
            self.translation_scroll
        }
    }

    fn current_annotation_line(&self) -> u32 {
        let line_count = self.translated_text.lines().count().max(1) as u32;
        (u32::from(self.effective_translation_scroll()) + 1).clamp(1, line_count)
    }

    /// The chapter currently loaded (0 = none), for the App's jump-overlay builder.
    pub fn current_chapter(&self) -> u32 {
        self.chapter
    }

    /// A short preview of the current translation line, used as a bookmark label.
    pub fn current_line_preview(&self) -> String {
        // Clamp to the last line so a scroll-past-EOF cursor still yields real text.
        let idx = (self.effective_translation_scroll() as usize)
            .min(self.translated_text.lines().count().saturating_sub(1));
        let raw = self.translated_text.lines().nth(idx).unwrap_or("");
        let cleaned = crate::workspace::translation::prose_only(raw);
        let text = if cleaned.trim().is_empty() {
            raw.trim()
        } else {
            cleaned.trim()
        };
        crate::ui::text::truncate_cols(text, 60)
    }

    /// Section outline (heading line / level / text) of the loaded chapter, parsed
    /// from the translation pane (falling back to JA when the chapter is untranslated). The
    /// line is the 1-based source line, the same basis the scroll offset tracks.
    pub fn outline(&self) -> Vec<(u32, u8, String)> {
        let from_translation = parse_headings(&self.translated_text);
        if from_translation.is_empty() {
            parse_headings(&self.ja)
        } else {
            from_translation
        }
    }

    /// Run a global search across both panes, scrolling to the first hit. An empty
    /// or match-less query clears any active search.
    pub fn run_search(&mut self, query: &str) -> usize {
        let query = query.to_string();
        if query.trim().is_empty() {
            self.search = None;
            return 0;
        }
        let translated_query = crate::ui::text::thai_display_safe(&query);
        let mut hits = Vec::new();
        for (i, line) in self.ja.lines().enumerate() {
            if crate::ui::markdown::contains_ci(line, &query) {
                hits.push(SearchHit {
                    side: Side::Ja,
                    line: i.min(u16::MAX as usize) as u16,
                });
            }
        }
        for (i, line) in self.translated_text.lines().enumerate() {
            if crate::ui::markdown::contains_ci(line, &translated_query) {
                hits.push(SearchHit {
                    side: Side::Translation,
                    line: i.min(u16::MAX as usize) as u16,
                });
            }
        }
        // Document order, JA then translation on the same line, so `>` walks top-to-bottom.
        hits.sort_by_key(|h| (h.line, h.side == Side::Translation));
        let count = hits.len();
        if count == 0 {
            self.search = None;
            return 0;
        }
        self.search = Some(ReaderSearch {
            query,
            translated_query,
            hits,
            sel: 0,
        });
        self.focus_hit();
        count
    }

    /// Move to the next (`forward`) or previous search hit, wrapping. No-op when no
    /// search is active.
    fn search_step(&mut self, forward: bool) {
        let Some(search) = self.search.as_mut() else {
            return;
        };
        if search.hits.is_empty() {
            return;
        }
        let n = search.hits.len();
        search.sel = if forward {
            (search.sel + 1) % n
        } else {
            (search.sel + n - 1) % n
        };
        self.focus_hit();
    }

    /// Scroll the relevant pane(s) so the selected search hit is near the top.
    fn focus_hit(&mut self) {
        let Some(search) = self.search.as_ref() else {
            return;
        };
        let Some(hit) = search.hits.get(search.sel) else {
            return;
        };
        let target = hit.line;
        if self.sync {
            self.scroll = target;
            self.translation_scroll = target;
        } else {
            match hit.side {
                Side::Ja => self.scroll = target,
                Side::Translation => self.translation_scroll = target,
            }
        }
    }

    /// Scroll to the next `[REVIEW NEEDED]` banner in the translation pane, wrapping. No-op
    /// when the chapter carries no review markers.
    fn jump_next_review(&mut self) {
        if self.review_lines.is_empty() {
            return;
        }
        let cur = self.effective_translation_scroll();
        let target = self
            .review_lines
            .iter()
            .copied()
            .find(|&l| l > cur)
            .unwrap_or(self.review_lines[0]);
        if self.sync {
            self.scroll = target;
            self.translation_scroll = target;
        } else {
            self.translation_scroll = target;
        }
    }

    /// Align the JA pane to the source chunk that produced the translation paragraph under
    /// the cursor: find the translation chunk via its marker, re-chunk the JA raw the same
    /// way the pipeline did, and scroll the JA pane to that chunk's first line.
    /// Decouples sync and forces split layout so the source is actually visible.
    fn show_source(&mut self) {
        let cur = self.effective_translation_scroll() as usize;
        let Some(chunk_idx) = translation_chunk_at_line(&self.translated_text, cur) else {
            return;
        };
        let chunks =
            crate::agents::chunk::chunk_chapter(&self.ja, self.chunk_cfg.0, self.chunk_cfg.1);
        let Some(chunk) = chunks.get(chunk_idx as usize) else {
            return;
        };
        let Some(needle) = first_nonempty_line(&chunk.text) else {
            return;
        };
        if let Some(line) = find_source_line(&self.ja, needle) {
            self.layout_mode = MODE_SPLIT;
            self.sync = false;
            self.scroll = line.min(u16::MAX as usize) as u16;
        }
    }

    /// Chunk under the cursor, from translation chunk markers.
    fn current_chunk(&self) -> Option<u32> {
        translation_chunk_at_line(
            &self.translated_text,
            self.effective_translation_scroll() as usize,
        )
    }

    /// Source chunk text, re-chunked with the pipeline settings.
    fn source_for_chunk(&self, chunk: u32) -> Option<String> {
        let chunks =
            crate::agents::chunk::chunk_chapter(&self.ja, self.chunk_cfg.0, self.chunk_cfg.1);
        chunks.get(chunk as usize).map(|c| c.text.clone())
    }

    /// Show source, translation, and active review note for the cursor chunk.
    fn inspect_overlay(&self) -> Action {
        if self.chapter == 0 {
            return Action::None;
        }
        let Some(chunk) = self.current_chunk() else {
            return Action::None;
        };
        let source_jp = self.source_for_chunk(chunk).unwrap_or_default();
        let translated =
            crate::workspace::translation::chunk_prose_in(&self.translated_text, chunk)
                .unwrap_or_default();
        let review =
            crate::workspace::translation::chunk_review_reason_in(&self.translated_text, chunk);
        Action::show_overlay(Overlay::reader_inspect(
            self.chapter,
            chunk,
            source_jp,
            translated,
            review,
        ))
    }

    /// Request an editor seeded App-side from composed on-disk translated text.
    /// Avoids editing `self.translated_text`, which is display-decomposed for the terminal.
    fn edit_overlay(&self) -> Action {
        if self.chapter == 0 {
            return Action::None;
        }
        match self.current_chunk() {
            Some(chunk) => Action::OpenReaderEdit {
                chapter: self.chapter,
                chunk,
            },
            None => Action::None,
        }
    }

    /// Scroll both panes to a 1-based `line` (used by jump-to-chapter/section). The
    /// chapter is already loaded by the caller.
    pub fn scroll_to_line(&mut self, line: u32) {
        let target = line.saturating_sub(1).min(u16::MAX as u32) as u16;
        self.scroll = target;
        self.translation_scroll = target;
    }

    /// Scroll to a QA-flagged chunk when its marker exists.
    pub fn scroll_to_chunk(&mut self, chunk: u32) {
        if let Some(line) =
            crate::workspace::translation::chunk_marker_line_in(&self.translated_text, chunk)
        {
            self.scroll_to_line(line);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_pane(
        &self,
        f: &mut Frame,
        area: Rect,
        theme: &Theme,
        title: &str,
        content: &str,
        fg: ratatui::style::Color,
        scroll: u16,
        is_translation: bool,
        annotations: Option<&[ReaderAnnotation]>,
    ) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(theme.rule))
            .title(Span::styled(
                format!(" {title} "),
                Style::default().fg(theme.ink_soft),
            ))
            .style(Style::default().bg(theme.bg_panel));
        let inner = block.inner(area);
        f.render_widget(block, area);

        // Re-parsing the whole chapter's Markdown every frame is what makes a large
        // chapter lag (the loop redraws on each 100 ms tick and pipeline event). The
        // parse output depends only on the inputs folded into `key` below — never on
        // the scroll offset — so memoize it and let the `Paragraph` re-scroll cheaply.
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.content_rev.hash(&mut h);
        is_translation.hash(&mut h);
        inner.width.hash(&mut h);
        self.highlight.hash(&mut h);
        self.show_annotations.hash(&mut h);
        fg.hash(&mut h);
        crate::ui::markdown::theme_fingerprint(theme).hash(&mut h);
        self.search.as_ref().map(|s| &s.query).hash(&mut h);
        let key = h.finish();

        let cache = if is_translation {
            &self.translation_cache
        } else {
            &self.ja_cache
        };
        let mut cache = cache.borrow_mut();
        cache.lines(key, || {
            // Hide the machine-only chunk / review markers from the translation pane (they
            // would otherwise show as literal `<!-- honya:chunk N -->` lines) while
            // preserving the line count, so note/bookmark/review anchors keep their
            // file-line basis.
            let cleaned;
            let base: &str = if is_translation {
                cleaned = hide_markers(content);
                cleaned.as_str()
            } else {
                content
            };

            // Render the chapter Markdown as styled prose (bold/italic, headings,
            // image chips, …) rather than leaking raw `**`/`![]()` syntax.
            let annotated;
            let render_content = if is_translation && self.show_annotations {
                if let Some(annotations) = annotations.filter(|notes| !notes.is_empty()) {
                    annotated = annotate_markdown(base, annotations);
                    annotated.as_str()
                } else {
                    base
                }
            } else {
                base
            };
            let mut lines =
                crate::ui::markdown::render(render_content, fg, theme, inner.width as usize);

            // Glossary terms first (subtle tint), then search matches on top
            // (standout), so an active query always wins the cell where they overlap.
            if self.highlight {
                let needles = if is_translation {
                    &self.translated_highlights
                } else {
                    &self.hl_ja
                };
                crate::ui::markdown::highlight(&mut lines, needles, glossary_style(theme));
            }
            if let Some(search) = self.search.as_ref() {
                let needle = if is_translation {
                    &search.translated_query
                } else {
                    &search.query
                };
                crate::ui::markdown::highlight(
                    &mut lines,
                    std::slice::from_ref(needle),
                    search_style(theme),
                );
            }
            lines
        });
        let total_rows = cache.display_rows(self.wrap, inner.width as usize);
        let lines = cache.cached().to_vec();

        let mut para = Paragraph::new(lines)
            .scroll((scroll, 0))
            .style(Style::default().bg(theme.bg_panel));
        if self.wrap {
            // Always trim:false — Thai has no inter-word spaces so trim:true would
            // produce long unbroken runs (risks.txt); JA leading spaces in dialogue
            // blocks are likewise intentional. `is_translation` is kept for callers' intent.
            let _ = is_translation;
            para = para.wrap(Wrap { trim: false });
        }
        f.render_widget(para, inner);
        crate::ui::widgets::render_panel_scrollbar(f, area, total_rows, scroll as usize, theme);
    }

    /// Rerun diff: archived old translation vs live new translation.
    fn render_diff(&mut self, ui: &mut crate::ui::kit::Ui, area: Rect) {
        let theme: &Theme = ui.theme;
        let f: &mut Frame = ui.frame;
        let Some(cmp) = self.compare.as_ref() else {
            return;
        };
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(4), Constraint::Length(1)])
            .split(area);
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(rows[0]);
        self.render_diff_pane(
            f,
            cols[0],
            theme,
            &format!("old · {}", cmp.old_label),
            &cmp.old_translation,
            &cmp.line.old_changed,
            false,
        );
        self.render_diff_pane(
            f,
            cols[1],
            theme,
            &format!("new · {}", cmp.new_label),
            &cmp.new_translation,
            &cmp.line.new_changed,
            true,
        );
        let exit_zone = self.render_compare_summary(f, rows[1], theme, cmp);
        // The one control the diff view offers, registered like any other.
        ui.zones.push(exit_zone, crate::ui::kit::ZoneId::action(A_DIFF));
    }

    /// One pane of the diff: plain prose lines (no Markdown styling, so changed
    /// lines colour cleanly) with a `-`/`+` gutter; removed lines tint red on the
    /// old side, added lines tint green on the new side.
    #[allow(clippy::too_many_arguments)]
    fn render_diff_pane(
        &self,
        f: &mut Frame,
        area: Rect,
        theme: &Theme,
        title: &str,
        content: &str,
        changed: &[bool],
        is_new: bool,
    ) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme::hairline_set())
            .border_style(Style::default().fg(theme.rule))
            .title(Span::styled(
                format!(" {title} "),
                Style::default().fg(theme.ink_soft),
            ))
            .style(Style::default().bg(theme.bg_panel));
        let inner = block.inner(area);
        f.render_widget(block, area);

        let changed_style = Style::default().fg(if is_new {
            theme.status_done
        } else {
            theme.status_failed
        });
        let normal_style = Style::default().fg(theme.translated_text);
        let gutter_changed = if is_new { "+ " } else { "- " };
        let gutter_style = Style::default().fg(theme.ink_faint);

        let lines: Vec<Line> = content
            .lines()
            .enumerate()
            .map(|(i, text)| {
                let is_changed = changed.get(i).copied().unwrap_or(false);
                let (gutter, style) = if is_changed {
                    (gutter_changed, changed_style)
                } else {
                    ("  ", normal_style)
                };
                Line::from(vec![
                    Span::styled(gutter, gutter_style),
                    Span::styled(text.to_string(), style),
                ])
            })
            .collect();

        let total_rows = if self.wrap {
            crate::ui::markdown::wrapped_rows(&lines, inner.width as usize)
        } else {
            lines.len()
        };
        let mut para = Paragraph::new(lines)
            .scroll((self.scroll, 0))
            .style(Style::default().bg(theme.bg_panel));
        if self.wrap {
            para = para.wrap(Wrap { trim: false });
        }
        f.render_widget(para, inner);
        crate::ui::widgets::render_panel_scrollbar(
            f,
            area,
            total_rows,
            self.scroll as usize,
            theme,
        );
    }

    /// Returns the clickable "[d] exit diff" cell so `render_diff` can register it.
    fn render_compare_summary(
        &self,
        f: &mut Frame,
        area: Rect,
        theme: &Theme,
        cmp: &RerunCompare,
    ) -> Rect {
        let faint = Style::default().fg(theme.ink_faint);
        let sep = || Span::styled(" · ", faint);

        let (cost_text, cost_style) = cmp.cost_summary(theme);
        let mut spans = vec![Span::raw(" "), Span::styled(cost_text, cost_style), sep()];

        spans.push(Span::styled(
            format!("QA {}→{} review", cmp.old_review, cmp.new_review),
            Style::default().fg(theme.ink_soft),
        ));
        let (trend_text, trend_style) = cmp.qa.label(theme);
        spans.push(Span::raw(" "));
        spans.push(Span::styled(trend_text, trend_style));
        if cmp.has_failure() {
            spans.push(Span::styled(
                format!(
                    " · fail {}→{}",
                    yesno(cmp.old_failed),
                    yesno(cmp.new_failed)
                ),
                Style::default().fg(theme.status_failed),
            ));
        }
        spans.push(sep());

        spans.push(Span::styled(
            format!("terms +{} new/~{} chg", cmp.terms_added, cmp.terms_changed),
            Style::default().fg(theme.accent_soft),
        ));
        spans.push(sep());
        let before: usize = spans
            .iter()
            .map(|s| crate::ui::text::col_width(s.content.as_ref()))
            .sum();
        let exit = "[d] exit diff";
        spans.push(Span::styled(exit, faint));
        let exit_zone = Rect {
            x: area.x.saturating_add(before as u16),
            y: area.y,
            width: crate::ui::text::col_width(exit) as u16,
            height: 1,
        };

        f.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(theme.bg)),
            area,
        );
        exit_zone
    }

    /// The Reader toolbar: toggles as chips, counters as badges.
    ///
    /// These were labelled text with hand-tracked column rectangles behind
    /// them. As chips they look like the controls they always were, and each
    /// registers itself, so the bar no longer keeps its own parallel list of
    /// where everything landed.
    fn render_status(&mut self, ui: &mut crate::ui::kit::Ui, area: Rect) {
        use crate::ui::kit::badge::Chip;
        use crate::ui::kit::toolbar::Toolbar;

        ui.fill(area, Style::default().bg(ui.theme.bg));
        let faint = Style::default().fg(ui.theme.ink_faint).bg(ui.theme.bg);
        let mut x = area.x + 1;
        let right = area.x + area.width;

        // The position readout is measured first: it is right-aligned so it
        // does not move as controls come and go, which means the toolbar's
        // budget is whatever it leaves behind.
        let pos = format!(
            "line {} · ch {:03}",
            self.current_annotation_line(),
            self.chapter
        );
        let pw = crate::ui::text::col_width(&pos) as u16;

        // An active search leads the bar: it is the most relevant state when
        // set, and the readout doubles as the control for the next match.
        if let Some(search) = self.search.as_ref() {
            let label = format!(
                "“{}” {}/{}",
                crate::ui::text::truncate_cols(
                    &crate::ui::text::thai_display_safe(&search.query),
                    16
                ),
                if search.hits.is_empty() { 0 } else { search.sel + 1 },
                search.hits.len()
            );
            let chip = Chip::new(crate::ui::kit::ZoneId::action(A_SEARCH_NEXT), label, true).plain();
            let w = chip.width().min(right.saturating_sub(x));
            chip.render(ui, Rect { x, width: w, height: 1, ..area });
            x += w + 1;
        }

        // Bookmarks are a readout — there is nowhere to go from a count of them.
        if !self.bookmark_lines.is_empty() && x < right {
            let text = format!(" ★ {} ", self.bookmark_lines.len());
            let w = (crate::ui::text::col_width(&text) as u16).min(right - x);
            ui.text(
                Rect { x, width: w, height: 1, ..area },
                text,
                Style::default().fg(ui.theme.status_warn).bg(ui.theme.bg),
            );
            x += w + 1;
        }

        let acts = self.actions();
        let budget = right.saturating_sub(x).saturating_sub(pw + 2);
        Toolbar::new(&acts).has_menu(true).render(
            ui,
            Rect {
                x,
                width: budget,
                height: 1,
                ..area
            },
        );

        if right.saturating_sub(x) > pw + 1 {
            ui.text(
                Rect {
                    x: right - pw - 1,
                    width: pw,
                    height: 1,
                    ..area
                },
                pos,
                faint,
            );
        }
    }

    /// This screen's commands, availability resolved for this frame. Drawn as
    /// the status row's chips.
    ///
    /// `[` and `]` are declared here so they step chapters in the Reader and
    /// cycle screens everywhere else. With nothing open they are unavailable
    /// rather than absent, and an unavailable key falls through to the global
    /// binding — so an empty Reader can still be left with `]`.
    pub fn actions(&self) -> Vec<Act> {
        use action_table::Accel;

        let has_ch = self.chapter != 0;
        let searching = self.search.is_some();
        let mode = match self.layout_mode {
            MODE_JA => "JA",
            MODE_TRANSLATION => "TR",
            _ => "split",
        };

        vec![
            Act::toolbar(A_SYNC, "sync", Accel::key('z')).toggle(self.sync),
            Act::toolbar(A_WRAP, "wrap", Accel::key('w')).toggle(self.wrap),
            Act::toolbar(A_MODE, "mode", Accel::key('o')).cycle().value(mode),
            Act::toolbar(A_HILITE, "hl", Accel::key('G')).toggle(self.highlight),
            Act::toolbar(A_NOTES, "notes", Accel::key('N'))
                .toggle(self.show_annotations)
                .count(self.annotations.len() as u32),
            Act::toolbar(A_DIFF, "diff", Accel::key('d'))
                .toggle(self.diff_mode)
                .when(self.compare.is_some()),
            Act::toolbar(A_SEARCH, "search", Accel::key('/')).when(has_ch),
            Act::toolbar(A_JUMP, "jump", Accel::key('g')).when(has_ch),
            Act::menu(A_INSPECT, "inspect chunk", Accel::key('i')).when(has_ch),
            Act::menu(A_EDIT, "edit translation", Accel::key('e')).when(has_ch),
            Act::menu(A_NOTE, "add note", Accel::key('n')).when(has_ch),
            Act::menu(A_MARK, "bookmark line", Accel::key('m')).when(has_ch),
            Act::menu(A_REVIEW, "next flag", Accel::key('r'))
                .count(self.review_lines.len() as u32)
                .when(!self.review_lines.is_empty()),
            Act::menu(A_SOURCE, "show source", Accel::key('s')).when(has_ch),
            Act::menu(A_COPY, "copy translation", Accel::key('y')).when(has_ch),
            Act::menu(A_QA, "QA report", Accel::key('Q')),
            Act::menu(
                A_SEARCH_NEXT,
                "next match",
                Accel::key('>').or(KeyCode::Char('.')),
            )
            .when(searching),
            Act::menu(
                A_SEARCH_PREV,
                "previous match",
                Accel::key('<').or(KeyCode::Char(',')),
            )
            .when(searching),
            Act::menu(A_PREV_CH, "previous chapter", Accel::key('[')).when(has_ch),
            Act::menu(A_NEXT_CH, "next chapter", Accel::key(']')).when(has_ch),
        ]
    }

    /// Run the action `id` stands for, however it was reached. `None` means
    /// no such action here.
    pub fn run(&mut self, id: u16) -> Option<Action> {
        Some(match id {
            A_SYNC => {
                self.sync = !self.sync;
                if self.sync {
                    self.translation_scroll = self.scroll;
                }
                Action::None
            }
            A_WRAP => {
                self.wrap = !self.wrap;
                Action::None
            }
            A_MODE => {
                self.layout_mode = (self.layout_mode + 1) % 3;
                Action::None
            }
            A_HILITE => {
                self.highlight = !self.highlight;
                Action::None
            }
            A_NOTES => {
                self.show_annotations = !self.show_annotations;
                Action::None
            }
            A_DIFF => {
                // A no-op when nothing was retranslated; the control is
                // unavailable then, so this only guards the key.
                if self.compare.is_some() {
                    self.diff_mode = !self.diff_mode;
                }
                Action::None
            }
            A_SEARCH => Action::show_overlay(Overlay::reader_search()),
            A_JUMP => Action::show_overlay(Overlay::reader_jump_placeholder()),
            A_INSPECT => self.inspect_overlay(),
            A_EDIT => self.edit_overlay(),
            A_NOTE => {
                let line = self.current_annotation_line();
                Action::show_overlay(Overlay::reader_note(self.chapter, line))
            }
            A_MARK => Action::ToggleReaderBookmark {
                chapter: self.chapter,
                line: self.current_annotation_line(),
                label: self.current_line_preview(),
            },
            A_REVIEW => {
                self.jump_next_review();
                Action::None
            }
            A_SOURCE => {
                self.show_source();
                Action::None
            }
            A_COPY => {
                let text = crate::workspace::translation::prose_only(&self.translated_text);
                if text.trim().is_empty() {
                    Action::None
                } else {
                    let lines = text.lines().filter(|l| !l.trim().is_empty()).count();
                    Action::ReaderCopy { text, lines }
                }
            }
            // The App rebuilds the report from the live project on show.
            A_QA => Action::show_overlay(Overlay::qa_placeholder()),
            A_SEARCH_NEXT => {
                self.search_step(true);
                Action::None
            }
            A_SEARCH_PREV => {
                self.search_step(false);
                Action::None
            }
            A_PREV_CH => Action::ReaderStepChapter { forward: false },
            A_NEXT_CH => Action::ReaderStepChapter { forward: true },
            _ => return None,
        })
    }

    /// Navigation only: the toggles are chips on the status row and the rest
    /// is a right-click away, so the footer no longer lists fourteen keys under
    /// a row that already shows most of them.
    pub fn hints(&self) -> &'static [(&'static str, &'static str)] {
        &[("↑↓", "scroll"), ("Space/b", "page"), ("[ ]", "chapter")]
    }
}

fn annotate_markdown(content: &str, annotations: &[ReaderAnnotation]) -> String {
    let mut by_line: std::collections::BTreeMap<u32, Vec<&ReaderAnnotation>> =
        std::collections::BTreeMap::new();
    for annotation in annotations {
        by_line
            .entry(annotation.line.max(1))
            .or_default()
            .push(annotation);
    }

    let mut out = String::new();
    let mut wrote_line = false;
    let mut line_no = 1u32;
    if content.is_empty() {
        push_annotations_for_line(&mut out, 1, &mut by_line);
    } else {
        for line in content.split('\n') {
            if wrote_line {
                out.push('\n');
            }
            out.push_str(line);
            wrote_line = true;
            push_annotations_for_line(&mut out, line_no, &mut by_line);
            line_no = line_no.saturating_add(1);
        }
    }

    // Notes anchored past EOF (for example after a hand edit shrank the file) stay
    // visible at the tail with their original line number.
    for (line, notes) in by_line {
        for note in notes {
            if wrote_line || !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("> 📝 L{line}: {}", inline_note_text(&note.note)));
            wrote_line = true;
        }
    }

    out
}

fn push_annotations_for_line(
    out: &mut String,
    line: u32,
    by_line: &mut std::collections::BTreeMap<u32, Vec<&ReaderAnnotation>>,
) {
    let Some(notes) = by_line.remove(&line) else {
        return;
    };
    for note in notes {
        out.push('\n');
        out.push_str("> 📝 ");
        out.push_str(&inline_note_text(&note.note));
    }
}

fn inline_note_text(note: &str) -> String {
    note.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Blank the machine-only marker lines (`<!-- honya:chunk N -->` and the
/// review-needed comment) for display, WITHOUT changing the line count — so the translation
/// pane reads cleanly while note/bookmark/review anchors keep their translated-file
/// line basis. The visible `[REVIEW NEEDED]` banner is deliberately kept.
fn hide_markers(translated: &str) -> String {
    translated
        .split('\n')
        .map(|line| {
            if crate::workspace::translation::parse_chunk_marker(line).is_some()
                || crate::workspace::translation::parse_total_marker(line).is_some()
                || line.trim() == crate::workspace::translation::REVIEW_NEEDED_MARKER
            {
                ""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Subtle tint for glossary/character terms — a color shift, no background, so prose
/// stays readable when highlighting is on.
fn glossary_style(theme: &Theme) -> Style {
    Style::default().fg(theme.accent_soft)
}

/// Standout style for live search matches — reverse-ish (text on accent) plus bold,
/// so a match is unmistakable against either pane's prose.
fn search_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.bg)
        .bg(theme.accent)
        .add_modifier(Modifier::BOLD)
}

/// Parse Markdown ATX headings (`#`..`######` followed by a space) out of `text`,
/// returning `(1-based line, level, heading text)`. Lines inside fenced code blocks
/// are skipped so a `#` comment in prose-embedded code never reads as a section.
fn parse_headings(text: &str) -> Vec<(u32, u8, String)> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for (i, raw) in text.lines().enumerate() {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some((level, heading)) = parse_heading(trimmed) {
            out.push((i as u32 + 1, level, heading));
        }
    }
    out
}

/// One ATX heading line → `(level, text)`. Requires the space after the hashes so a
/// bare `#tag` stays literal, mirroring the Markdown renderer's rule.
fn parse_heading(line: &str) -> Option<(u8, String)> {
    let hashes = line.chars().take_while(|&c| c == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = line[hashes..].strip_prefix(' ')?;
    let text = rest.trim().trim_end_matches('#').trim();
    if text.is_empty() {
        None
    } else {
        Some((hashes as u8, text.to_string()))
    }
}

/// The 0-based translation chunk index covering source `line`: the largest chunk marker at or
/// above it. `None` before the first marker (e.g. an untranslated chapter).
fn translation_chunk_at_line(translated: &str, line: usize) -> Option<u32> {
    let mut current = None;
    for (i, l) in translated.lines().enumerate() {
        if i > line {
            break;
        }
        if let Some(n) = crate::workspace::translation::parse_chunk_marker(l) {
            current = Some(n);
        }
    }
    current
}

/// First non-blank, non-marker line of a chunk's source text, used to locate it back
/// in the JA pane.
fn first_nonempty_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|l| {
        !l.is_empty()
            && crate::workspace::translation::parse_chunk_marker(l).is_none()
            && crate::workspace::translation::parse_total_marker(l).is_none()
    })
}

/// 0-based line in `hay` whose trimmed content equals (else contains) `needle`. Used
/// to scroll the JA pane to a re-derived chunk boundary; best-effort by design.
fn find_source_line(hay: &str, needle: &str) -> Option<usize> {
    let needle = needle.trim();
    if needle.is_empty() {
        return None;
    }
    hay.lines()
        .position(|l| l.trim() == needle)
        .or_else(|| hay.lines().position(|l| l.contains(needle)))
}

/// QA movement between the previous and the new run of a chapter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum QaTrend {
    Better,
    Same,
    Worse,
}

impl QaTrend {
    fn label(self, theme: &Theme) -> (&'static str, Style) {
        match self {
            QaTrend::Better => ("✓ better", Style::default().fg(theme.status_done)),
            QaTrend::Worse => ("✗ worse", Style::default().fg(theme.status_failed)),
            QaTrend::Same => ("= same", Style::default().fg(theme.ink_faint)),
        }
    }
}

/// Side-by-side rerun comparison: archived previous translation vs the live new one, plus
/// the cost / QA / glossary deltas between the two runs that produced them.
struct RerunCompare {
    old_label: String,
    new_label: String,
    /// Prose-only (markers stripped), display-safe text for each side.
    old_translation: String,
    new_translation: String,
    line: crate::ui::diff::LineDiff,
    /// Per-run cost (USD); `None` when that run's spend was never recorded.
    old_cost: Option<f64>,
    new_cost: Option<f64>,
    old_review: u32,
    new_review: u32,
    old_failed: bool,
    new_failed: bool,
    qa: QaTrend,
    terms_added: usize,
    terms_changed: usize,
}

impl RerunCompare {
    /// The cost cell of the summary line: the new run's cost with a coloured delta
    /// vs the previous run, or an n/a note when either side was never recorded.
    fn cost_summary(&self, theme: &Theme) -> (String, Style) {
        match (self.old_cost, self.new_cost) {
            (Some(o), Some(n)) => {
                let d = n - o;
                if d.abs() < 0.00005 {
                    (
                        format!("cost ${n:.4} (=)"),
                        Style::default().fg(theme.ink_soft),
                    )
                } else if d > 0.0 {
                    (
                        format!("cost ${n:.4} ▲+${d:.4}"),
                        Style::default().fg(theme.status_warn),
                    )
                } else {
                    (
                        format!("cost ${n:.4} ▼-${:.4}", d.abs()),
                        Style::default().fg(theme.status_done),
                    )
                }
            }
            (None, Some(n)) => (
                format!("cost ${n:.4} (prev n/a)"),
                Style::default().fg(theme.ink_soft),
            ),
            (Some(o), None) => (
                format!("cost prev ${o:.4} · new n/a"),
                Style::default().fg(theme.ink_soft),
            ),
            (None, None) => ("cost n/a".to_string(), Style::default().fg(theme.ink_faint)),
        }
    }

    /// `true` when either run ended `Failed` — surfaced as a badge in the summary.
    fn has_failure(&self) -> bool {
        self.old_failed || self.new_failed
    }
}

/// From a chapter's run records (any order), pick the previous version to diff
/// against (most recently archived) and the live version (most recent un-archived).
fn select_compare_runs(runs: &[ChapterRun]) -> (Option<&ChapterRun>, Option<&ChapterRun>) {
    let prev = runs
        .iter()
        .filter(|r| r.archived.is_some())
        .max_by_key(|r| r.finished_at);
    let live = runs
        .iter()
        .filter(|r| r.archived.is_none())
        .max_by_key(|r| r.finished_at);
    (prev, live)
}

/// Classify QA movement: a chapter going from failed→ok (or fewer review-needed
/// chunks) is better; newly failed (or more review-needed) is worse.
fn qa_trend(old_failed: bool, old_review: u32, new_failed: bool, new_review: u32) -> QaTrend {
    if new_failed != old_failed {
        return if new_failed {
            QaTrend::Worse
        } else {
            QaTrend::Better
        };
    }
    match new_review.cmp(&old_review) {
        std::cmp::Ordering::Less => QaTrend::Better,
        std::cmp::Ordering::Greater => QaTrend::Worse,
        std::cmp::Ordering::Equal => QaTrend::Same,
    }
}

/// Apply a signed scroll `delta` to an offset, saturating at the u16 bounds.
fn step(v: u16, delta: i32) -> u16 {
    if delta >= 0 {
        v.saturating_add(delta as u16)
    } else {
        v.saturating_sub((-delta) as u16)
    }
}

/// True when terminal column `col` falls within `area`'s horizontal span.
fn col_in(area: Rect, col: u16) -> bool {
    area.width > 0 && col >= area.x && col < area.x.saturating_add(area.width)
}

fn short_dt(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%d %H:%M").to_string()
}

fn yesno(b: bool) -> &'static str {
    if b { "y" } else { "n" }
}

impl Default for ReaderScreen {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: &str, secs: i64, archived: Option<&str>) -> ChapterRun {
        ChapterRun {
            chapter: 1,
            run_id: id.into(),
            finished_at: DateTime::<Utc>::from_timestamp(secs, 0).unwrap(),
            usage: Default::default(),
            usage_unknown: false,
            review_needed: 0,
            failed: false,
            total_chunks: 0,
            committed_chunks: 0,
            glossary_added: vec![],
            glossary_changed: vec![],
            archived: archived.map(|s| s.to_string()),
        }
    }

    #[test]
    fn select_compare_picks_latest_archived_and_latest_live() {
        let runs = vec![
            run("r1", 10, Some("reruns/ch_001/r1.md")),
            run("r2", 20, Some("reruns/ch_001/r2.md")),
            run("r3", 30, None),
        ];
        let (prev, live) = select_compare_runs(&runs);
        assert_eq!(
            prev.unwrap().run_id,
            "r2",
            "newest archived is the previous"
        );
        assert_eq!(live.unwrap().run_id, "r3", "newest un-archived is live");
    }

    #[test]
    fn select_compare_none_when_never_retranslated() {
        let runs = vec![run("r1", 10, None)];
        let (prev, live) = select_compare_runs(&runs);
        assert!(prev.is_none());
        assert_eq!(live.unwrap().run_id, "r1");
    }

    #[test]
    fn qa_trend_classification() {
        assert_eq!(qa_trend(false, 2, false, 0), QaTrend::Better);
        assert_eq!(qa_trend(false, 0, false, 3), QaTrend::Worse);
        assert_eq!(qa_trend(false, 1, false, 1), QaTrend::Same);
        assert_eq!(qa_trend(true, 0, false, 0), QaTrend::Better); // failed → fixed
        assert_eq!(qa_trend(false, 0, true, 0), QaTrend::Worse); // newly failed
    }

    fn screen_with(ja: &str, translated: &str) -> ReaderScreen {
        let mut r = ReaderScreen::new();
        r.chapter = 1;
        r.ja = ja.to_string();
        r.translated_text = crate::ui::text::thai_display_safe(translated);
        r.recompute_review_lines();
        r
    }

    #[test]
    fn search_finds_hits_across_both_panes_and_cycles() {
        // "skill" appears once in JA (line 2) and once in translation (line 0).
        let ja = "intro line\nsecond line\nthe skill awakens\n";
        let translated = "the skill blooms\nบรรทัดสอง\nบรรทัดสาม\n";
        let mut r = screen_with(ja, translated);

        let count = r.run_search("skill");
        assert_eq!(count, 2, "one JA hit + one translation hit");
        // Hits are document-ordered; the first is on line 0 (translation), so we land there.
        assert_eq!(r.effective_translation_scroll(), 0);

        // `>` advances to the JA hit on line 2; `<` wraps back.
        r.search_step(true);
        assert_eq!(r.scroll, 2, "second hit is the JA occurrence on line 2");
        r.search_step(true);
        assert_eq!(r.scroll, 0, "wraps back to the first hit");
    }

    #[test]
    fn search_with_no_match_clears() {
        let mut r = screen_with("alpha\nbeta\n", "หนึ่ง\nสอง\n");
        assert_eq!(r.run_search("zzz"), 0);
        assert!(r.search.is_none());
    }

    #[test]
    fn jump_next_review_walks_banners_and_wraps() {
        let translated = "<!-- honya:chunk 0 -->\nclean prose\n\n<!-- honya:chunk 1 -->\n> ⚠️ **[REVIEW NEEDED]** chunk 2\nflagged\n\nmore\n> a second [REVIEW NEEDED] here\n";
        let mut r = screen_with("raw", translated);
        assert_eq!(r.review_lines.len(), 2, "two banner lines detected");

        r.translation_scroll = 0;
        r.sync = false;
        r.jump_next_review();
        let first = r.translation_scroll;
        assert_eq!(first, r.review_lines[0]);
        r.jump_next_review();
        assert_eq!(r.translation_scroll, r.review_lines[1]);
        r.jump_next_review();
        assert_eq!(
            r.translation_scroll, r.review_lines[0],
            "wraps to the first banner"
        );
    }

    #[test]
    fn show_source_aligns_ja_to_the_th_chunk() {
        // Two CJK paragraphs split into two chunks at this budget (probed).
        let ja = "あいうえおかきくけこさしすせそ\n\nたちつてとなにぬねのはひふへほ\n";
        let translated = "<!-- honya:chunk 0 -->\nคำแปลหนึ่ง\n\n<!-- honya:chunk 1 -->\nคำแปลสอง\n";
        let mut r = screen_with(ja, translated);
        r.set_chunk_cfg(8, 80);
        // Start synced with the cursor inside chunk 1 (line 4 of the translation file).
        r.sync = true;
        r.scroll = 4;

        r.show_source();
        assert!(!r.sync, "show source decouples the panes");
        assert_eq!(r.layout_mode, MODE_SPLIT);
        // chunk 1's source is the second paragraph on JA line index 2.
        assert_eq!(r.scroll, 2);
    }

    #[test]
    fn current_line_preview_clamps_past_eof() {
        let mut r = screen_with("raw", "first translated line\nsecond translated line\n");
        r.sync = false;
        r.translation_scroll = 999; // scrolled far past EOF
        assert_eq!(r.current_line_preview(), "second translated line");
    }

    #[test]
    fn hide_markers_blanks_machine_lines_keeping_line_count() {
        let translated = "<!-- honya:chunk 0 -->\nสวัสดี\n\n<!-- honya:review-needed -->\n> ⚠️ [REVIEW NEEDED] chunk 1\nบรรทัด\n";
        let hidden = hide_markers(translated);
        // Line count is preserved exactly (anchors keep their basis)…
        assert_eq!(translated.split('\n').count(), hidden.split('\n').count());
        // …machine markers are gone…
        assert!(!hidden.contains("honya:chunk"));
        assert!(!hidden.contains("honya:review-needed"));
        // …but the human-facing banner stays.
        assert!(hidden.contains("[REVIEW NEEDED]"));
        assert!(hidden.contains("สวัสดี"));
    }

    /// Every toolbar control draws where the registry says it is, and running
    /// it by id does what its key does. The two used to be separate matches
    /// that had to be kept in step by hand.
    #[test]
    fn status_controls_and_their_keys_agree() {
        use ratatui::crossterm::event::KeyModifiers;

        let mut r = screen_with("raw ja", "translated text");
        let (_, zones) = crate::ui::kit::ctx::draw_test(140, 24, |ui, area| r.render(ui, area));

        for act in r.actions() {
            if act.placement != action_table::Placement::Toolbar {
                continue;
            }
            assert!(
                zones.contains(act.zone()),
                "{} is declared for the toolbar but nothing drew it",
                act.label
            );
        }

        // A click resolves to the action id the router then runs, so running
        // the id and pressing the key must leave the same state.
        let by_key = |r: &mut ReaderScreen, c: char| {
            r.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        };

        assert!(r.sync);
        r.run(A_SYNC);
        assert!(!r.sync, "the control toggles it off");
        by_key(&mut r, 'z');
        assert!(r.sync, "and the key toggles it back");

        assert!(r.wrap);
        r.run(A_WRAP);
        assert!(!r.wrap);
        by_key(&mut r, 'w');
        assert!(r.wrap);

        assert_eq!(r.layout_mode, MODE_SPLIT);
        r.run(A_MODE);
        assert_eq!(r.layout_mode, MODE_JA, "the mode chip cycles the layout");
        by_key(&mut r, 'o');
        assert_eq!(r.layout_mode, MODE_TRANSLATION);
    }

    /// The regression the Phase 6 remap introduced: `]` and `[` were claimed by
    /// the chrome for screen cycling before the Reader was consulted, leaving
    /// `ReaderStepChapter` with no caller at all.
    #[test]
    fn bracket_keys_step_chapters_while_a_chapter_is_open() {
        let mut r = screen_with("raw ja", "translated text");
        r.chapter = 4;
        assert!(matches!(
            r.handle_key(KeyEvent::new(KeyCode::Char(']'), ratatui::crossterm::event::KeyModifiers::NONE)),
            Action::ReaderStepChapter { forward: true }
        ));
        assert!(matches!(
            r.handle_key(KeyEvent::new(KeyCode::Char('['), ratatui::crossterm::event::KeyModifiers::NONE)),
            Action::ReaderStepChapter { forward: false }
        ));

        // With nothing open they are still declared — help documents them —
        // but unavailable, which the router lets fall through to the global
        // screen-cycling binding rather than swallowing.
        r.chapter = 0;
        let acts = r.actions();
        assert_eq!(
            action_table::hit(
                &acts,
                &KeyEvent::new(
                    KeyCode::Char(']'),
                    ratatui::crossterm::event::KeyModifiers::NONE
                )
            ),
            action_table::KeyHit::Blocked,
            "an empty Reader must still be leavable with ]"
        );
    }

    /// The table is the only place an action's effect is written, so every id
    /// it declares must have a `run` arm.
    #[test]
    fn every_declared_action_has_a_handler() {
        let mut r = screen_with("raw ja", "translated text");
        r.chapter = 2;
        for act in r.actions() {
            let mut probe = screen_with("raw ja", "translated text");
            probe.chapter = 2;
            assert!(
                probe.run(act.id).is_some(),
                "{} ({}) is advertised with no handler",
                act.label,
                act.accel.shown()
            );
        }
    }

    #[test]
    fn outline_parses_headings_from_translation_else_ja() {
        let translated = "# บทที่หนึ่ง\nเนื้อหา\n## ฉากเปิด\nมากกว่า\n";
        let r = screen_with("raw", translated);
        let outline = r.outline();
        assert_eq!(outline.len(), 2);
        assert_eq!(outline[0], (1, 1, "บทที่หนึ่ง".to_string()));
        assert_eq!(outline[1], (3, 2, "ฉากเปิด".to_string()));

        // Untranslated translation (no headings) falls back to the JA pane.
        let r2 = screen_with("# 第一章\nbody\n", "ยังไม่มีคำแปล\n");
        let outline2 = r2.outline();
        assert_eq!(outline2.len(), 1);
        assert_eq!(outline2[0].2, "第一章");
    }
}
