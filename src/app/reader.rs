//! src/app/reader.rs — the Reader / Diff view (4 読).
//!
//! Synced side-by-side JA source vs TH translation for proofreading. A single
//! shared scroll position drives both panes; `z` decouples; `[`/`]` move between
//! chapters; `o` cycles split / JA-only / TH-only; `w` toggles wrap.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use chrono::{DateTime, Utc};

use crate::model::{ChapterRun, ReaderAnnotation};
use crate::theme::{self, Theme};
use crate::workspace::Workspace;

use super::Action;
use super::overlay::Overlay;

/// Layout modes for `o`.
const MODE_SPLIT: u8 = 0;
const MODE_JA: u8 = 1;
const MODE_TH: u8 = 2;

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
        }
    }

    /// Load raw/ (JA) + translated/ (TH) for a chapter. Kept synchronous to match
    /// the locked signature; uses std::fs directly (the async helpers wrap the
    /// same files but `load` is called from the synchronous render/apply path).
    pub fn load(&mut self, ws: &Workspace, chapter: u32) {
        self.chapter = chapter;
        self.scroll = 0;
        self.th_scroll = 0;
        self.diff_mode = false;
        self.ja = std::fs::read_to_string(ws.raw(chapter))
            .unwrap_or_else(|_| "（原文がまだありません — raw not found）".to_string());
        let th = std::fs::read_to_string(ws.translated(chapter))
            .unwrap_or_else(|_| "（ยังไม่มีคำแปล — not translated yet）".to_string());
        // Decompose Thai SARA AM so it never lands as a width-2 single cell that
        // desyncs the terminal and smears ำ across the screen on the next redraw.
        self.th = crate::ui::text::thai_display_safe(&th);
        self.reload_annotations(ws);
        self.load_compare(ws);
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
                // Previous chapter: the App owns the workspace, so signal via
                // OpenChapter with the decremented number (App reloads us).
                if self.chapter > 1 {
                    Action::OpenChapter {
                        chapter: self.chapter - 1,
                    }
                } else {
                    Action::None
                }
            }
            KeyCode::Char(']') => Action::OpenChapter {
                chapter: self.chapter + 1,
            },
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
                // Copy is environment-dependent; we acknowledge via a no-op Action
                // and let the App surface a toast through its normal channels. We
                // keep it a no-op here so the build never depends on a clipboard dep.
                Action::None
            }
            // Translation QA inbox (App rebuilds the report from the live project).
            KeyCode::Char('Q') => Action::show_overlay(Overlay::qa_placeholder()),
            _ => Action::None,
        }
    }

    fn scroll_by(&mut self, delta: i32) {
        let apply = |v: u16| -> u16 {
            if delta >= 0 {
                v.saturating_add(delta as u16)
            } else {
                v.saturating_sub((-delta) as u16)
            }
        };
        self.scroll = apply(self.scroll);
        if self.sync {
            self.th_scroll = self.scroll;
        } else {
            self.th_scroll = apply(self.th_scroll);
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
                self.render_pane(
                    f,
                    body,
                    theme,
                    "日本語 (raw)",
                    &self.ja,
                    theme.ja_text,
                    self.scroll,
                    false,
                    None,
                );
            }
            MODE_TH => {
                self.render_pane(
                    f,
                    body,
                    theme,
                    "ไทย (translated)",
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
                self.render_pane(
                    f,
                    cols[0],
                    theme,
                    "日本語 (raw)",
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
                    "ไทย (translated)",
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

        // Render the chapter Markdown as styled prose (bold/italic, headings,
        // image chips, …) rather than leaking raw `**`/`![]()` syntax.
        let annotated;
        let render_content = if is_thai && self.show_annotations {
            if let Some(annotations) = annotations.filter(|notes| !notes.is_empty()) {
                annotated = annotate_markdown(content, annotations);
                annotated.as_str()
            } else {
                content
            }
        } else {
            content
        };
        let lines = crate::ui::markdown::render(render_content, fg, theme, inner.width as usize);

        let mut para = Paragraph::new(lines)
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

    /// Side-by-side rerun diff: archived old Thai (left) vs the live new Thai
    /// (right), changed lines tinted, with a one-line cost / QA / glossary summary.
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
            &format!("เก่า/old · {}", cmp.old_label),
            &cmp.old_th,
            &cmp.line.old_changed,
            false,
        );
        self.render_diff_pane(
            f,
            cols[1],
            theme,
            &format!("ใหม่/new · {}", cmp.new_label),
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
        let sync_glyph = if self.sync { "●" } else { "○" };
        let wrap_glyph = if self.wrap { "●" } else { "○" };
        let note_glyph = if self.show_annotations { "●" } else { "○" };
        let mode = match self.layout_mode {
            MODE_JA => "JA",
            MODE_TH => "TH",
            _ => "split",
        };
        let line = Line::from(vec![
            Span::styled("  sync ", Style::default().fg(theme.ink_faint)),
            Span::styled(
                sync_glyph,
                Style::default().fg(if self.sync {
                    theme.status_done
                } else {
                    theme.ink_faint
                }),
            ),
            Span::styled(" · wrap ", Style::default().fg(theme.ink_faint)),
            Span::styled(
                wrap_glyph,
                Style::default().fg(if self.wrap {
                    theme.status_done
                } else {
                    theme.ink_faint
                }),
            ),
            Span::styled(" · layout ", Style::default().fg(theme.ink_faint)),
            Span::styled(mode, Style::default().fg(theme.accent_soft)),
            Span::styled(" · notes ", Style::default().fg(theme.ink_faint)),
            Span::styled(
                format!("{} {}", note_glyph, self.annotations.len()),
                Style::default().fg(if self.show_annotations {
                    theme.status_done
                } else {
                    theme.ink_faint
                }),
            ),
            Span::styled(" · cmp ", Style::default().fg(theme.ink_faint)),
            Span::styled(
                if self.compare.is_some() {
                    "● d"
                } else {
                    "○"
                },
                Style::default().fg(if self.compare.is_some() {
                    theme.accent_soft
                } else {
                    theme.ink_faint
                }),
            ),
            Span::styled(" · line ", Style::default().fg(theme.ink_faint)),
            Span::styled(
                self.current_annotation_line().to_string(),
                Style::default().fg(theme.accent_soft),
            ),
            Span::styled(" · ch ", Style::default().fg(theme.ink_faint)),
            Span::styled(
                format!("{:03}", self.chapter),
                Style::default()
                    .fg(theme.ink_soft)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        f.render_widget(
            Paragraph::new(line).style(Style::default().bg(theme.bg)),
            area,
        );
    }

    pub fn hints(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("↑↓", "scroll"),
            ("[ ]", "chapter"),
            ("z", "sync"),
            ("o", "layout"),
            ("d", "diff"),
            ("w", "wrap"),
            ("n", "note"),
            ("N", "notes"),
            ("y", "copy"),
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
}
