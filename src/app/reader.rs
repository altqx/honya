//! src/app/reader.rs — the Reader / Diff view (4 読).
//!
//! Synced side-by-side JA source vs TH translation for proofreading. A single
//! shared scroll position drives both panes; `z` decouples; `[`/`]` move between
//! chapters; `o` cycles split / JA-only / TH-only; `w` toggles wrap.

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

use super::Action;
use super::overlay::Overlay;

/// Layout modes for `o`.
const MODE_SPLIT: u8 = 0;
const MODE_JA: u8 = 1;
const MODE_TH: u8 = 2;

/// Default soft / hard chunk budgets, mirroring `AppConfig`. Used to re-derive JA
/// chunk boundaries for `s` (show source) until the App seeds the live values.
const DEFAULT_CHUNK_TARGET: usize = 1000;
const DEFAULT_CHUNK_HARD_CAP: usize = 1200;

/// Which pane a search hit lives in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Ja,
    Th,
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
    /// Display-safe form of the query (matched against the decomposed TH pane).
    th_query: String,
    hits: Vec<SearchHit>,
    sel: usize,
}

pub struct ReaderScreen {
    scroll: u16,
    /// Second (Thai) scroll offset, used only when sync is off.
    th_scroll: u16,
    sync: bool,
    wrap: bool,
    layout_mode: u8,
    ja: String,
    th: String,
    annotations: Vec<ReaderAnnotation>,
    show_annotations: bool,
    chapter: u32,
    /// Rerun comparison for the current chapter, present only when an earlier
    /// version is archived (i.e. the chapter has been retranslated at least once).
    compare: Option<RerunCompare>,
    /// Diff mode active: side-by-side old vs new Thai. Only enterable when
    /// `compare` is `Some`; `d` toggles it.
    diff_mode: bool,
    /// Glossary/character JP forms present in this chapter, tinted in the JA pane.
    hl_ja: Vec<String>,
    /// Glossary/character Thai forms present in this chapter (display-safe), TH pane.
    hl_th: Vec<String>,
    /// Whether glossary-term highlighting is on (toggle with `G`).
    highlight: bool,
    /// Active search across both panes, or `None`.
    search: Option<ReaderSearch>,
    /// `[REVIEW NEEDED]` banner line anchors (0-based) in the TH pane, for `r`.
    review_lines: Vec<u16>,
    /// Bookmark line anchors (1-based) for the current chapter, for the status badge.
    bookmark_lines: Vec<u32>,
    /// Soft / hard chunk budgets used to align a TH chunk to its JA source (`s`).
    chunk_cfg: (usize, usize),
    /// Pane rectangles, refreshed every frame, so the wheel scrolls whichever pane
    /// the pointer is over when the two are decoupled. Empty when a pane is hidden.
    ja_area: Rect,
    th_area: Rect,
    /// Bumped whenever the rendered *content* of a pane changes (chapter load, note
    /// edits). Folds into the per-pane cache key so the expensive Markdown parse is
    /// skipped on the 100 ms ticker / pipeline events while reading a static chapter.
    content_rev: u64,
    /// Memoized rendered lines for the JA / TH panes, rebuilt only when their cache
    /// key (content_rev, width, theme, highlight/search state …) changes — never on a
    /// bare scroll or animation tick.
    ja_cache: RefCell<crate::ui::markdown::RenderCache>,
    th_cache: RefCell<crate::ui::markdown::RenderCache>,
}

impl ReaderScreen {
    pub fn new() -> Self {
        Self {
            scroll: 0,
            th_scroll: 0,
            sync: true,
            wrap: true,
            layout_mode: MODE_SPLIT,
            ja: String::new(),
            th: String::new(),
            annotations: Vec::new(),
            show_annotations: true,
            chapter: 0,
            compare: None,
            diff_mode: false,
            hl_ja: Vec::new(),
            hl_th: Vec::new(),
            highlight: true,
            search: None,
            review_lines: Vec::new(),
            bookmark_lines: Vec::new(),
            chunk_cfg: (DEFAULT_CHUNK_TARGET, DEFAULT_CHUNK_HARD_CAP),
            ja_area: Rect::default(),
            th_area: Rect::default(),
            content_rev: 0,
            ja_cache: RefCell::new(crate::ui::markdown::RenderCache::default()),
            th_cache: RefCell::new(crate::ui::markdown::RenderCache::default()),
        }
    }

    /// Seed the soft / hard chunk budgets from the live `AppConfig` so `s` (show
    /// source) re-chunks the JA raw exactly as the pipeline did. Called once at
    /// startup; the defaults match `AppConfig` so it is safe before this runs.
    pub fn set_chunk_cfg(&mut self, target: usize, hard_cap: usize) {
        self.chunk_cfg = (target.max(1), hard_cap.max(target.max(1)));
    }

    /// Enter the rerun diff view (old vs new Thai).
    pub fn enter_diff(&mut self) {
        self.diff_mode = true;
    }

    /// Reload only if the Reader is already showing `chapter`.
    pub fn reload_if_showing(&mut self, ws: &Workspace, chapter: u32) {
        if self.chapter == chapter {
            self.load(ws, chapter);
        }
    }

    /// Load raw/ (JA) + translated/ (TH) for a chapter.
    pub fn load(&mut self, ws: &Workspace, chapter: u32) {
        self.chapter = chapter;
        self.scroll = 0;
        self.th_scroll = 0;
        self.diff_mode = false;
        self.ja = std::fs::read_to_string(ws.raw(chapter))
            .unwrap_or_else(|_| "(raw not found)".to_string());
        let th = std::fs::read_to_string(ws.translated(chapter))
            .unwrap_or_else(|_| "(not translated yet)".to_string());
        // Decompose Thai SARA AM so it never lands as a width-2 single cell that
        // desyncs the terminal and smears ำ across the screen on the next redraw.
        self.th = crate::ui::text::thai_display_safe(&th);
        self.search = None;
        self.content_rev = self.content_rev.wrapping_add(1);
        self.reload_annotations(ws);
        self.reload_highlight_terms(ws);
        self.reload_bookmarks(ws);
        self.recompute_review_lines();
        self.load_compare(ws);
    }

    /// Load the project glossary + characters and keep only the JP / Thai forms that
    /// actually appear in this chapter, so highlighting is bounded per chapter (the
    /// same present-only filter `build_reference_ctx` uses for injected context).
    fn reload_highlight_terms(&mut self, ws: &Workspace) {
        self.hl_ja.clear();
        self.hl_th.clear();
        if self.chapter == 0 {
            return;
        }
        let glossary = crate::workspace::glossary::load(ws);
        let characters = crate::workspace::characters::load(ws);

        // TH forms: decompose like the pane content, keep only those present.
        let mut th_seen = std::collections::HashSet::new();
        let mut hl_th = Vec::new();
        let th_forms = glossary
            .iter()
            .map(|t| t.thai_term.as_str())
            .chain(characters.iter().map(|c| c.thai_name.as_str()));
        for raw in th_forms {
            let safe = crate::ui::text::thai_display_safe(raw.trim());
            // Match case-insensitively to stay consistent with the renderer's
            // `highlight` (matters only for ASCII terms like "HP"/"SSR").
            if !safe.is_empty()
                && crate::ui::markdown::contains_ci(&self.th, &safe)
                && th_seen.insert(safe.clone())
            {
                hl_th.push(safe);
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

        self.hl_th = hl_th;
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
            .th
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

        let old_th = crate::workspace::translation::prose_only(
            &crate::ui::text::thai_display_safe(&old_raw),
        );
        // `self.th` is already display-safe; strip the chunk markers for a clean diff.
        let new_th = crate::workspace::translation::prose_only(&self.th);
        let line = crate::ui::diff::diff_lines(&old_th, &new_th);

        let old_cost = (!prev.usage_unknown).then_some(prev.usage.cost_usd);
        let new_cost = live.and_then(|r| (!r.usage_unknown).then_some(r.usage.cost_usd));
        let (new_review, new_failed) = match live {
            Some(r) => (r.review_needed, r.failed),
            // No recorded live run (e.g. a rerun crashed before finishing): read the
            // review-needed count straight off the live file.
            None => (
                crate::workspace::translation::review_needed_chunk_indices_in(&self.th).len()
                    as u32,
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
            old_th,
            new_th,
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
        // Notes are interleaved into the TH pane, so a note edit changes its render.
        self.content_rev = self.content_rev.wrapping_add(1);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
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
            KeyCode::Char('[') => {
                if self.chapter == 0 {
                    Action::None
                } else {
                    Action::ReaderStepChapter { forward: false }
                }
            }
            KeyCode::Char(']') => {
                if self.chapter == 0 {
                    Action::None
                } else {
                    Action::ReaderStepChapter { forward: true }
                }
            }
            KeyCode::Char('z') => {
                self.sync = !self.sync;
                if self.sync {
                    self.th_scroll = self.scroll;
                }
                Action::None
            }
            KeyCode::Char('w') => {
                self.wrap = !self.wrap;
                Action::None
            }
            KeyCode::Char('o') => {
                self.layout_mode = (self.layout_mode + 1) % 3;
                Action::None
            }
            KeyCode::Char('d') => {
                // Toggle the rerun diff view; a no-op when nothing was retranslated.
                if self.compare.is_some() {
                    self.diff_mode = !self.diff_mode;
                }
                Action::None
            }
            KeyCode::Char('n') => {
                if self.chapter == 0 {
                    Action::None
                } else {
                    let line = self.current_annotation_line();
                    Action::show_overlay(Overlay::reader_note(self.chapter, line))
                }
            }
            KeyCode::Char('N') => {
                self.show_annotations = !self.show_annotations;
                Action::None
            }
            KeyCode::Char('y') => {
                if self.chapter == 0 {
                    Action::None
                } else {
                    let text = crate::workspace::translation::prose_only(&self.th);
                    if text.trim().is_empty() {
                        Action::None
                    } else {
                        let lines = text.lines().filter(|l| !l.trim().is_empty()).count();
                        Action::ReaderCopy { text, lines }
                    }
                }
            }
            KeyCode::Char('/') => {
                if self.chapter == 0 {
                    Action::None
                } else {
                    Action::show_overlay(Overlay::reader_search())
                }
            }
            KeyCode::Char('>') | KeyCode::Char('.') if self.search.is_some() => {
                self.search_step(true);
                Action::None
            }
            KeyCode::Char('<') | KeyCode::Char(',') if self.search.is_some() => {
                self.search_step(false);
                Action::None
            }
            KeyCode::Esc if self.search.is_some() => {
                self.search = None;
                Action::None
            }
            KeyCode::Char('g') => {
                if self.chapter == 0 {
                    Action::None
                } else {
                    Action::show_overlay(Overlay::reader_jump_placeholder())
                }
            }
            KeyCode::Char('G') => {
                self.highlight = !self.highlight;
                Action::None
            }
            KeyCode::Char('r') => {
                self.jump_next_review();
                Action::None
            }
            KeyCode::Char('s') => {
                self.show_source();
                Action::None
            }
            KeyCode::Char('m') => {
                if self.chapter == 0 {
                    Action::None
                } else {
                    Action::ToggleReaderBookmark {
                        chapter: self.chapter,
                        line: self.current_annotation_line(),
                        label: self.current_line_preview(),
                    }
                }
            }
            // Translation QA inbox (App rebuilds the report from the live project).
            KeyCode::Char('Q') => Action::show_overlay(Overlay::qa_placeholder()),
            _ => Action::None,
        }
    }

    /// Mouse: the wheel scrolls. When the panes are synced (or in diff mode) both
    /// move together; when decoupled, only the pane under the pointer scrolls — so
    /// you can read JA and TH at independent positions with the wheel.
    pub fn handle_mouse(&mut self, m: MouseInput) -> Action {
        match m.gesture {
            MouseGesture::ScrollUp => self.scroll_targeted(m.col, -3),
            MouseGesture::ScrollDown => self.scroll_targeted(m.col, 3),
            MouseGesture::Click { .. } | MouseGesture::RightClick => {}
        }
        Action::None
    }

    fn scroll_targeted(&mut self, col: u16, delta: i32) {
        // Diff and synced reading both move a single shared offset.
        if self.diff_mode || self.sync {
            self.scroll_by(delta);
            return;
        }
        if col_in(self.th_area, col) {
            self.th_scroll = step(self.th_scroll, delta);
        } else if col_in(self.ja_area, col) {
            self.scroll = step(self.scroll, delta);
        } else {
            self.scroll_by(delta);
        }
    }

    fn scroll_by(&mut self, delta: i32) {
        self.scroll = step(self.scroll, delta);
        if self.sync {
            self.th_scroll = self.scroll;
        } else {
            self.th_scroll = step(self.th_scroll, delta);
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, theme: &Theme) {
        if self.diff_mode {
            if self.compare.is_some() {
                self.render_diff(f, area, theme);
                return;
            }
            self.diff_mode = false; // compare went away (e.g. chapter reloaded)
        }

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(4), Constraint::Length(1)])
            .split(area);
        let body = rows[0];

        match self.layout_mode {
            MODE_JA => {
                self.ja_area = body;
                self.th_area = Rect::default();
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
            MODE_TH => {
                self.ja_area = Rect::default();
                self.th_area = body;
                self.render_pane(
                    f,
                    body,
                    theme,
                    "Thai (translated)",
                    &self.th,
                    theme.th_text,
                    self.effective_th_scroll(),
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
                self.th_area = cols[1];
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
                    "Thai (translated)",
                    &self.th,
                    theme.th_text,
                    self.effective_th_scroll(),
                    true,
                    Some(&self.annotations),
                );
            }
        }

        self.render_status(f, rows[1], theme);
    }

    fn effective_th_scroll(&self) -> u16 {
        if self.sync {
            self.scroll
        } else {
            self.th_scroll
        }
    }

    fn current_annotation_line(&self) -> u32 {
        let line_count = self.th.lines().count().max(1) as u32;
        (u32::from(self.effective_th_scroll()) + 1).clamp(1, line_count)
    }

    /// The chapter currently loaded (0 = none), for the App's jump-overlay builder.
    pub fn current_chapter(&self) -> u32 {
        self.chapter
    }

    /// A short preview of the current TH line, used as a bookmark label.
    pub fn current_line_preview(&self) -> String {
        // Clamp to the last line so a scroll-past-EOF cursor still yields real text.
        let idx =
            (self.effective_th_scroll() as usize).min(self.th.lines().count().saturating_sub(1));
        let raw = self.th.lines().nth(idx).unwrap_or("");
        let cleaned = crate::workspace::translation::prose_only(raw);
        let text = if cleaned.trim().is_empty() {
            raw.trim()
        } else {
            cleaned.trim()
        };
        crate::ui::text::truncate_cols(text, 60)
    }

    /// Section outline (heading line / level / text) of the loaded chapter, parsed
    /// from the TH pane (falling back to JA when the chapter is untranslated). The
    /// line is the 1-based source line, the same basis the scroll offset tracks.
    pub fn outline(&self) -> Vec<(u32, u8, String)> {
        let from_th = parse_headings(&self.th);
        if from_th.is_empty() {
            parse_headings(&self.ja)
        } else {
            from_th
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
        let th_query = crate::ui::text::thai_display_safe(&query);
        let mut hits = Vec::new();
        for (i, line) in self.ja.lines().enumerate() {
            if crate::ui::markdown::contains_ci(line, &query) {
                hits.push(SearchHit {
                    side: Side::Ja,
                    line: i.min(u16::MAX as usize) as u16,
                });
            }
        }
        for (i, line) in self.th.lines().enumerate() {
            if crate::ui::markdown::contains_ci(line, &th_query) {
                hits.push(SearchHit {
                    side: Side::Th,
                    line: i.min(u16::MAX as usize) as u16,
                });
            }
        }
        // Document order, JA then TH on the same line, so `>` walks top-to-bottom.
        hits.sort_by_key(|h| (h.line, h.side == Side::Th));
        let count = hits.len();
        if count == 0 {
            self.search = None;
            return 0;
        }
        self.search = Some(ReaderSearch {
            query,
            th_query,
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
            self.th_scroll = target;
        } else {
            match hit.side {
                Side::Ja => self.scroll = target,
                Side::Th => self.th_scroll = target,
            }
        }
    }

    /// Scroll to the next `[REVIEW NEEDED]` banner in the TH pane, wrapping. No-op
    /// when the chapter carries no review markers.
    fn jump_next_review(&mut self) {
        if self.review_lines.is_empty() {
            return;
        }
        let cur = self.effective_th_scroll();
        let target = self
            .review_lines
            .iter()
            .copied()
            .find(|&l| l > cur)
            .unwrap_or(self.review_lines[0]);
        if self.sync {
            self.scroll = target;
            self.th_scroll = target;
        } else {
            self.th_scroll = target;
        }
    }

    /// Align the JA pane to the source chunk that produced the TH paragraph under
    /// the cursor: find the TH chunk via its marker, re-chunk the JA raw the same
    /// way the pipeline did, and scroll the JA pane to that chunk's first line.
    /// Decouples sync and forces split layout so the source is actually visible.
    fn show_source(&mut self) {
        let cur = self.effective_th_scroll() as usize;
        let Some(chunk_idx) = th_chunk_at_line(&self.th, cur) else {
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

    /// Scroll both panes to a 1-based `line` (used by jump-to-chapter/section). The
    /// chapter is already loaded by the caller.
    pub fn scroll_to_line(&mut self, line: u32) {
        let target = line.saturating_sub(1).min(u16::MAX as u32) as u16;
        self.scroll = target;
        self.th_scroll = target;
    }

    /// Scroll to a QA-flagged chunk when its marker exists.
    pub fn scroll_to_chunk(&mut self, chunk: u32) {
        if let Some(line) = crate::workspace::translation::chunk_marker_line_in(&self.th, chunk) {
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
        is_thai: bool,
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
        is_thai.hash(&mut h);
        inner.width.hash(&mut h);
        self.highlight.hash(&mut h);
        self.show_annotations.hash(&mut h);
        fg.hash(&mut h);
        crate::ui::markdown::theme_fingerprint(theme).hash(&mut h);
        self.search.as_ref().map(|s| &s.query).hash(&mut h);
        let key = h.finish();

        let cache = if is_thai {
            &self.th_cache
        } else {
            &self.ja_cache
        };
        let mut cache = cache.borrow_mut();
        let lines = cache.lines(key, || {
            // Hide the machine-only chunk / review markers from the TH pane (they
            // would otherwise show as literal `<!-- honya:chunk N -->` lines) while
            // preserving the line count, so note/bookmark/review anchors keep their
            // file-line basis.
            let cleaned;
            let base: &str = if is_thai {
                cleaned = hide_markers(content);
                cleaned.as_str()
            } else {
                content
            };

            // Render the chapter Markdown as styled prose (bold/italic, headings,
            // image chips, …) rather than leaking raw `**`/`![]()` syntax.
            let annotated;
            let render_content = if is_thai && self.show_annotations {
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
                let needles = if is_thai { &self.hl_th } else { &self.hl_ja };
                crate::ui::markdown::highlight(&mut lines, needles, glossary_style(theme));
            }
            if let Some(search) = self.search.as_ref() {
                let needle = if is_thai {
                    &search.th_query
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

        let mut para = Paragraph::new(lines.to_vec())
            .scroll((scroll, 0))
            .style(Style::default().bg(theme.bg_panel));
        if self.wrap {
            // Always trim:false — Thai has no inter-word spaces so trim:true would
            // produce long unbroken runs (risks.txt); JA leading spaces in dialogue
            // blocks are likewise intentional. `is_thai` is kept for callers' intent.
            let _ = is_thai;
            para = para.wrap(Wrap { trim: false });
        }
        f.render_widget(para, inner);
    }

    /// Rerun diff: archived old Thai vs live new Thai.
    fn render_diff(&self, f: &mut Frame, area: Rect, theme: &Theme) {
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
            &cmp.old_th,
            &cmp.line.old_changed,
            false,
        );
        self.render_diff_pane(
            f,
            cols[1],
            theme,
            &format!("new · {}", cmp.new_label),
            &cmp.new_th,
            &cmp.line.new_changed,
            true,
        );
        self.render_compare_summary(f, rows[1], theme, cmp);
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
        let normal_style = Style::default().fg(theme.th_text);
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

        let mut para = Paragraph::new(lines)
            .scroll((self.scroll, 0))
            .style(Style::default().bg(theme.bg_panel));
        if self.wrap {
            para = para.wrap(Wrap { trim: false });
        }
        f.render_widget(para, inner);
    }

    fn render_compare_summary(&self, f: &mut Frame, area: Rect, theme: &Theme, cmp: &RerunCompare) {
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
        spans.push(Span::styled("[d] exit diff", faint));

        f.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(theme.bg)),
            area,
        );
    }

    fn render_status(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        let faint = Style::default().fg(theme.ink_faint);
        let on = theme.status_done;
        let off = theme.ink_faint;
        let toggle = |state: bool| Style::default().fg(if state { on } else { off });
        let glyph = |state: bool| if state { "●" } else { "○" };
        let mode = match self.layout_mode {
            MODE_JA => "JA",
            MODE_TH => "TH",
            _ => "split",
        };

        let mut spans: Vec<Span> = Vec::new();

        // An active search leads the line — it's the most relevant state when set.
        if let Some(search) = self.search.as_ref() {
            let pos = if search.hits.is_empty() {
                0
            } else {
                search.sel + 1
            };
            spans.push(Span::styled("  search ", faint));
            spans.push(Span::styled(
                format!(
                    "“{}” {}/{}",
                    crate::ui::text::truncate_cols(
                        &crate::ui::text::thai_display_safe(&search.query),
                        16
                    ),
                    pos,
                    search.hits.len()
                ),
                Style::default().fg(theme.accent),
            ));
            spans.push(Span::styled(" · ", faint));
        } else {
            spans.push(Span::raw("  "));
        }

        spans.push(Span::styled("sync ", faint));
        spans.push(Span::styled(glyph(self.sync), toggle(self.sync)));
        spans.push(Span::styled(" · wrap ", faint));
        spans.push(Span::styled(glyph(self.wrap), toggle(self.wrap)));
        spans.push(Span::styled(" · ", faint));
        spans.push(Span::styled(mode, Style::default().fg(theme.accent_soft)));
        spans.push(Span::styled(" · hl ", faint));
        spans.push(Span::styled(glyph(self.highlight), toggle(self.highlight)));
        spans.push(Span::styled(" · notes ", faint));
        spans.push(Span::styled(
            format!(
                "{} {}",
                glyph(self.show_annotations),
                self.annotations.len()
            ),
            toggle(self.show_annotations),
        ));
        if !self.bookmark_lines.is_empty() {
            spans.push(Span::styled(" · ", faint));
            spans.push(Span::styled(
                format!("★ {}", self.bookmark_lines.len()),
                Style::default().fg(theme.status_warn),
            ));
        }
        if !self.review_lines.is_empty() {
            spans.push(Span::styled(" · ", faint));
            spans.push(Span::styled(
                format!("⚑ {}", self.review_lines.len()),
                Style::default().fg(theme.status_failed),
            ));
        }
        if self.compare.is_some() {
            spans.push(Span::styled(" · cmp ", faint));
            spans.push(Span::styled("● d", Style::default().fg(theme.accent_soft)));
        }
        spans.push(Span::styled(" · line ", faint));
        spans.push(Span::styled(
            self.current_annotation_line().to_string(),
            Style::default().fg(theme.accent_soft),
        ));
        spans.push(Span::styled(" · ch ", faint));
        spans.push(Span::styled(
            format!("{:03}", self.chapter),
            Style::default()
                .fg(theme.ink_soft)
                .add_modifier(Modifier::BOLD),
        ));

        f.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(theme.bg)),
            area,
        );
    }

    pub fn hints(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("↑↓", "scroll"),
            ("[ ]", "chapter"),
            ("/", "search"),
            ("g", "jump"),
            ("r", "review"),
            ("s", "source"),
            ("m", "mark"),
            ("n", "note"),
            ("G", "hilite"),
            ("z", "sync"),
            ("d", "diff"),
            ("Q", "QA"),
        ]
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
/// review-needed comment) for display, WITHOUT changing the line count — so the TH
/// pane reads cleanly while note/bookmark/review anchors keep their translated-file
/// line basis. The visible `[REVIEW NEEDED]` banner is deliberately kept.
fn hide_markers(th: &str) -> String {
    th.split('\n')
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

/// The 0-based TH chunk index covering source `line`: the largest chunk marker at or
/// above it. `None` before the first marker (e.g. an untranslated chapter).
fn th_chunk_at_line(th: &str, line: usize) -> Option<u32> {
    let mut current = None;
    for (i, l) in th.lines().enumerate() {
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

/// Side-by-side rerun comparison: archived previous Thai vs the live new Thai, plus
/// the cost / QA / glossary deltas between the two runs that produced them.
struct RerunCompare {
    old_label: String,
    new_label: String,
    /// Prose-only (markers stripped), display-safe Thai for each side.
    old_th: String,
    new_th: String,
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

    fn screen_with(ja: &str, th: &str) -> ReaderScreen {
        let mut r = ReaderScreen::new();
        r.chapter = 1;
        r.ja = ja.to_string();
        r.th = crate::ui::text::thai_display_safe(th);
        r.recompute_review_lines();
        r
    }

    #[test]
    fn search_finds_hits_across_both_panes_and_cycles() {
        // "skill" appears once in JA (line 2) and once in TH (line 0).
        let ja = "intro line\nsecond line\nthe skill awakens\n";
        let th = "the skill blooms\nบรรทัดสอง\nบรรทัดสาม\n";
        let mut r = screen_with(ja, th);

        let count = r.run_search("skill");
        assert_eq!(count, 2, "one JA hit + one TH hit");
        // Hits are document-ordered; the first is on line 0 (TH), so we land there.
        assert_eq!(r.effective_th_scroll(), 0);

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
        let th = "<!-- honya:chunk 0 -->\nclean prose\n\n<!-- honya:chunk 1 -->\n> ⚠️ **[REVIEW NEEDED]** chunk 2\nflagged\n\nmore\n> a second [REVIEW NEEDED] here\n";
        let mut r = screen_with("raw", th);
        assert_eq!(r.review_lines.len(), 2, "two banner lines detected");

        r.th_scroll = 0;
        r.sync = false;
        r.jump_next_review();
        let first = r.th_scroll;
        assert_eq!(first, r.review_lines[0]);
        r.jump_next_review();
        assert_eq!(r.th_scroll, r.review_lines[1]);
        r.jump_next_review();
        assert_eq!(r.th_scroll, r.review_lines[0], "wraps to the first banner");
    }

    #[test]
    fn show_source_aligns_ja_to_the_th_chunk() {
        // Two CJK paragraphs split into two chunks at this budget (probed).
        let ja = "あいうえおかきくけこさしすせそ\n\nたちつてとなにぬねのはひふへほ\n";
        let th = "<!-- honya:chunk 0 -->\nคำแปลหนึ่ง\n\n<!-- honya:chunk 1 -->\nคำแปลสอง\n";
        let mut r = screen_with(ja, th);
        r.set_chunk_cfg(8, 80);
        // Start synced with the cursor inside chunk 1 (line 4 of the TH file).
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
        let mut r = screen_with("raw", "first th line\nsecond th line\n");
        r.sync = false;
        r.th_scroll = 999; // scrolled far past EOF
        assert_eq!(r.current_line_preview(), "second th line");
    }

    #[test]
    fn hide_markers_blanks_machine_lines_keeping_line_count() {
        let th = "<!-- honya:chunk 0 -->\nสวัสดี\n\n<!-- honya:review-needed -->\n> ⚠️ [REVIEW NEEDED] chunk 1\nบรรทัด\n";
        let hidden = hide_markers(th);
        // Line count is preserved exactly (anchors keep their basis)…
        assert_eq!(th.split('\n').count(), hidden.split('\n').count());
        // …machine markers are gone…
        assert!(!hidden.contains("honya:chunk"));
        assert!(!hidden.contains("honya:review-needed"));
        // …but the human-facing banner stays.
        assert!(hidden.contains("[REVIEW NEEDED]"));
        assert!(hidden.contains("สวัสดี"));
    }

    #[test]
    fn outline_parses_headings_from_th_else_ja() {
        let th = "# บทที่หนึ่ง\nเนื้อหา\n## ฉากเปิด\nมากกว่า\n";
        let r = screen_with("raw", th);
        let outline = r.outline();
        assert_eq!(outline.len(), 2);
        assert_eq!(outline[0], (1, 1, "บทที่หนึ่ง".to_string()));
        assert_eq!(outline[1], (3, 2, "ฉากเปิด".to_string()));

        // Untranslated TH (no headings) falls back to the JA pane.
        let r2 = screen_with("# 第一章\nbody\n", "ยังไม่มีคำแปล\n");
        let outline2 = r2.outline();
        assert_eq!(outline2.len(), 1);
        assert_eq!(outline2[0].2, "第一章");
    }
}
