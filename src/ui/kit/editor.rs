//! An editable text surface you can point at.
//!
//! Every editor overlay in the app is click-inert today: you can open one, but
//! the only way to move the caret is arrow keys, even though byte-offset cursors
//! and a caret renderer already exist. The missing piece is the inverse mapping
//! — from a screen cell back to an offset in the text — which is what this
//! module provides.
//!
//! Getting that mapping right is entirely a width problem, and honya's text is
//! the hard case: a Japanese line is mostly two-column glyphs, and a Thai line
//! contains combining marks that occupy no columns at all. So the walk is by
//! grapheme and by display column, never by byte or `char`, and a click past the
//! end of a line lands at the line's end rather than wrapping onto the next.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

use super::ctx::Ui;
use super::zones::{ZoneId, ZoneKind};
use crate::ui::glyphs;
use crate::ui::text::{col_width, thai_display_safe};

/// A selected span of the text, as byte offsets. `anchor` is where the drag
/// began and `head` is where it is now, so `anchor > head` is a backwards
/// selection rather than an invalid one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

// Drag never reaches a screen — `MouseInput::from_event` drops it — so nothing
// constructs a Selection yet. The rendering below handles one correctly and is
// tested; deleting it would mean rewriting the span-splitting loop that four
// overlays draw through, to remove a branch an empty selection already skips.
#[allow(dead_code)]
impl Selection {
    pub fn new(at: usize) -> Self {
        Self {
            anchor: at,
            head: at,
        }
    }

    pub fn range(self) -> std::ops::Range<usize> {
        if self.anchor <= self.head {
            self.anchor..self.head
        } else {
            self.head..self.anchor
        }
    }

    pub fn is_empty(self) -> bool {
        self.anchor == self.head
    }
}

/// Soft-wrap `text` to `width` columns, returning each visual line's byte range.
///
/// Wrapping happens at the last space that fits, falling back to a hard break
/// mid-word when a single word is wider than the viewport — which for Japanese,
/// where a whole sentence can contain no spaces at all, is the normal case
/// rather than the exception.
pub fn wrap(text: &str, width: u16) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    if width == 0 {
        return out;
    }
    let width = width as usize;

    for line in split_lines(text) {
        let slice = &text[line.clone()];
        if slice.is_empty() {
            out.push(line.clone());
            continue;
        }
        let mut start = line.start;
        let mut cols = 0usize;
        let mut last_break: Option<usize> = None;

        for (offset, g) in slice.grapheme_indices(true) {
            let abs = line.start + offset;
            let w = col_width(g);
            if cols + w > width && abs > start {
                // Prefer the last space; otherwise break right here.
                let brk = last_break.filter(|&b| b > start).unwrap_or(abs);
                out.push(start..brk);
                start = brk;
                cols = text[start..abs].graphemes(true).map(col_width).sum();
                last_break = None;
            }
            cols += w;
            if g == " " {
                last_break = Some(abs + g.len());
            }
        }
        out.push(start..line.end);
    }
    if out.is_empty() {
        out.push(0..0);
    }
    out
}

/// Byte ranges of each hard line, excluding the newline itself.
fn split_lines(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            out.push(start..i);
            start = i + 1;
        }
    }
    out.push(start..text.len());
    out
}

/// The byte offset a click at (`col`, `row`) refers to.
///
/// `area` is where the text is drawn and `scroll` is the first visible visual
/// line. A click below the last line lands at the end of the text, and a click
/// past a line's end lands at that line's end — never on the next line, which
/// is what makes clicking into the right-hand whitespace of a short line feel
/// correct.
pub fn offset_at(
    text: &str,
    lines: &[std::ops::Range<usize>],
    area: Rect,
    scroll: usize,
    col: u16,
    row: u16,
) -> usize {
    if lines.is_empty() {
        return 0;
    }
    let rel_row = row.saturating_sub(area.y) as usize;
    let idx = scroll + rel_row;
    let Some(range) = lines.get(idx) else {
        return text.len();
    };
    let target = col.saturating_sub(area.x) as usize;

    let slice = &text[range.clone()];
    let mut cols = 0usize;
    for (offset, g) in slice.grapheme_indices(true) {
        let w = col_width(g);
        // Land on whichever side of the glyph the click was nearer, so clicking
        // the right half of a wide character puts the caret after it.
        if target < cols + w.max(1).div_ceil(2) {
            return range.start + offset;
        }
        if target < cols + w {
            return range.start + offset + g.len();
        }
        cols += w;
    }
    range.end
}

/// Where the caret sits on screen, or `None` when it is scrolled out of view.
pub fn caret_cell(
    text: &str,
    lines: &[std::ops::Range<usize>],
    area: Rect,
    scroll: usize,
    cursor: usize,
) -> Option<(u16, u16)> {
    let cursor = cursor.min(text.len());
    let idx = lines
        .iter()
        .position(|r| cursor >= r.start && cursor <= r.end)?;
    if idx < scroll || idx >= scroll + area.height as usize {
        return None;
    }
    let range = &lines[idx];
    let cols: usize = text[range.start..cursor]
        .graphemes(true)
        .map(col_width)
        .sum();
    Some((
        area.x + cols.min(area.width.saturating_sub(1) as usize) as u16,
        area.y + (idx - scroll) as u16,
    ))
}

/// Everything needed to draw one editable surface: the text, its wrap, where
/// the view starts, and where the caret and selection are. Grouped because they
/// only make sense together — a wrap computed for a different width, or a scroll
/// from a different text, is a bug rather than a configuration.
pub struct View<'a> {
    pub text: &'a str,
    pub lines: &'a [std::ops::Range<usize>],
    pub scroll: usize,
    pub cursor: Option<usize>,
    pub selection: Option<Selection>,
}

impl<'a> View<'a> {
    pub fn new(text: &'a str, lines: &'a [std::ops::Range<usize>]) -> Self {
        Self {
            text,
            lines,
            scroll: 0,
            cursor: None,
            selection: None,
        }
    }

    pub fn scroll(mut self, scroll: usize) -> Self {
        self.scroll = scroll;
        self
    }

    pub fn cursor(mut self, cursor: usize) -> Self {
        self.cursor = Some(cursor);
        self
    }

    #[allow(dead_code)]
    pub fn selection(mut self, selection: Option<Selection>) -> Self {
        self.selection = selection;
        self
    }
}

/// Draw a [`View`] into `area`.
///
/// Registers the whole area as a text surface, so clicks and drags over it
/// resolve here rather than to whatever pane is behind.
pub fn render(ui: &mut Ui, area: Rect, view: &View<'_>, id_index: u32) {
    let View {
        text,
        lines,
        scroll,
        cursor,
        selection,
    } = *view;
    if area.width == 0 || area.height == 0 {
        return;
    }
    ui.hit_only(area, ZoneId::new(ZoneKind::TextSurface, id_index));
    ui.fill(area, Style::default().bg(ui.surface()));

    let plain = Style::default().fg(ui.theme.ink).bg(ui.surface());
    let picked = Style::default()
        .fg(ui.theme.ink)
        .bg(ui.theme.accent_bg)
        .add_modifier(if ui.theme.paints_fills() {
            Modifier::empty()
        } else {
            // With no fill available, reverse video is the only way a selection
            // can show at all.
            Modifier::REVERSED
        });
    let sel = selection.map(Selection::range).unwrap_or(0..0);

    for n in 0..area.height as usize {
        let Some(range) = lines.get(scroll + n) else {
            break;
        };
        let row = Rect {
            x: area.x,
            y: area.y + n as u16,
            width: area.width,
            height: 1,
        };
        // Split the line into runs by whether each grapheme is selected, so a
        // selection spanning several lines needs no special casing.
        let slice = &text[range.clone()];
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut run = String::new();
        let mut run_selected = false;
        for (offset, g) in slice.grapheme_indices(true) {
            let abs = range.start + offset;
            let is_sel = !sel.is_empty() && sel.contains(&abs);
            if is_sel != run_selected && !run.is_empty() {
                spans.push(Span::styled(
                    thai_display_safe(&run),
                    if run_selected { picked } else { plain },
                ));
                run.clear();
            }
            run_selected = is_sel;
            run.push_str(g);
        }
        if !run.is_empty() {
            spans.push(Span::styled(
                thai_display_safe(&run),
                if run_selected { picked } else { plain },
            ));
        }
        ui.line(row, Line::from(spans), plain);
    }

    if let Some(c) = cursor
        && let Some((cx, cy)) = caret_cell(text, lines, area, scroll, c)
    {
        ui.line(
            Rect {
                x: cx,
                y: cy,
                width: 1,
                height: 1,
            },
            Line::from(Span::styled(
                glyphs::ACCENT_RAIL.as_str().to_string(),
                Style::default().fg(ui.theme.stream_cursor).bg(ui.surface()),
            )),
            plain,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(w: u16, h: u16) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        }
    }

    #[test]
    fn wrapping_covers_the_whole_text_with_no_gaps_or_overlaps() {
        for text in [
            "hello world this is a fairly long line that wraps",
            "短い行\nもう一つの行",
            "",
            "no-spaces-at-all-in-this-very-long-single-word-here",
        ] {
            for w in [4u16, 10, 20, 80] {
                let lines = wrap(text, w);
                assert!(!lines.is_empty(), "{text:?} at {w} produced nothing");
                assert_eq!(lines[0].start, 0, "wrapping must start at 0");
                for pair in lines.windows(2) {
                    assert!(
                        pair[0].end <= pair[1].start,
                        "{text:?} at {w}: {:?} overlaps {:?}",
                        pair[0],
                        pair[1]
                    );
                }
                assert_eq!(
                    lines.last().unwrap().end,
                    text.len(),
                    "{text:?} at {w} lost the tail"
                );
            }
        }
    }

    #[test]
    fn no_wrapped_line_is_wider_than_the_viewport() {
        let text = "the quick brown fox jumps over the lazy dog";
        for w in [6u16, 12, 25] {
            for r in wrap(text, w) {
                let cols: usize = text[r.clone()].graphemes(true).map(col_width).sum();
                assert!(cols <= w as usize, "line {r:?} is {cols} cols, max {w}");
            }
        }
    }

    #[test]
    fn japanese_wraps_mid_sentence_because_it_has_no_spaces_to_break_at() {
        // A whole Japanese sentence is one "word"; refusing to break it would
        // overflow the viewport rather than wrap.
        let jp = "吾輩は猫である。名前はまだ無い。";
        let lines = wrap(jp, 10);
        assert!(lines.len() > 1, "should have wrapped: {lines:?}");
        for r in &lines {
            let cols: usize = jp[r.clone()].graphemes(true).map(col_width).sum();
            assert!(cols <= 10, "wide glyphs overflowed: {cols}");
        }
    }

    #[test]
    fn hard_newlines_always_start_a_new_line() {
        let text = "a\nb\nc";
        let lines = wrap(text, 80);
        assert_eq!(lines.len(), 3);
        assert_eq!(&text[lines[0].clone()], "a");
        assert_eq!(&text[lines[1].clone()], "b");
        assert_eq!(&text[lines[2].clone()], "c");
    }

    #[test]
    fn an_empty_line_between_paragraphs_is_preserved() {
        let text = "one\n\ntwo";
        let lines = wrap(text, 20);
        assert_eq!(lines.len(), 3);
        assert_eq!(&text[lines[1].clone()], "", "the blank line must survive");
    }

    #[test]
    fn clicking_a_character_puts_the_caret_beside_it() {
        let text = "hello";
        let lines = wrap(text, 20);
        let a = area(20, 3);
        assert_eq!(offset_at(text, &lines, a, 0, 0, 0), 0, "before 'h'");
        assert_eq!(offset_at(text, &lines, a, 0, 1, 0), 1, "between h and e");
        assert_eq!(offset_at(text, &lines, a, 0, 5, 0), 5, "after the last char");
    }

    #[test]
    fn clicking_past_the_end_of_a_line_lands_at_its_end_not_the_next_line() {
        let text = "ab\ncd";
        let lines = wrap(text, 20);
        let a = area(20, 3);
        // Far to the right of a two-character first line.
        assert_eq!(offset_at(text, &lines, a, 0, 18, 0), 2, "end of line one");
        // And the second line behaves the same.
        assert_eq!(offset_at(text, &lines, a, 0, 18, 1), 5, "end of line two");
    }

    #[test]
    fn clicking_below_the_last_line_lands_at_the_end_of_the_text() {
        let text = "only one line";
        let lines = wrap(text, 40);
        let a = area(40, 10);
        assert_eq!(offset_at(text, &lines, a, 0, 0, 7), text.len());
    }

    #[test]
    fn a_wide_glyph_splits_at_its_middle() {
        // Each of these is two columns, so the caret should land before the
        // character on its left half and after it on its right.
        let jp = "日本語";
        let lines = wrap(jp, 20);
        let a = area(20, 2);
        assert_eq!(offset_at(jp, &lines, a, 0, 0, 0), 0, "left half of 日");
        assert_eq!(offset_at(jp, &lines, a, 0, 1, 0), 3, "right half of 日");
        assert_eq!(offset_at(jp, &lines, a, 0, 2, 0), 3, "left half of 本");
        // And every returned offset is a real character boundary.
        for col in 0..12u16 {
            let off = offset_at(jp, &lines, a, 0, col, 0);
            assert!(jp.is_char_boundary(off), "offset {off} splits a character");
        }
    }

    #[test]
    fn thai_combining_marks_never_produce_a_broken_offset() {
        // SARA AM and friends occupy zero columns; a column-walking mapper has
        // to keep returning grapheme boundaries regardless.
        let th = "กำลังแปล";
        let lines = wrap(th, 20);
        let a = area(20, 2);
        for col in 0..20u16 {
            let off = offset_at(th, &lines, a, 0, col, 0);
            assert!(th.is_char_boundary(off), "offset {off} splits a character");
            assert!(off <= th.len());
        }
    }

    #[test]
    fn clicking_a_scrolled_view_accounts_for_the_offset() {
        let text = "l0\nl1\nl2\nl3\nl4";
        let lines = wrap(text, 20);
        let a = area(20, 2);
        // With two lines scrolled away, the top visible row is line 2.
        let off = offset_at(text, &lines, a, 2, 0, 0);
        assert_eq!(off, lines[2].start);
    }

    #[test]
    fn the_caret_round_trips_through_a_click() {
        let text = "the quick brown fox";
        let lines = wrap(text, 12);
        let a = area(12, 4);
        for cursor in [0usize, 4, 9, 13, text.len()] {
            let Some((cx, cy)) = caret_cell(text, &lines, a, 0, cursor) else {
                continue;
            };
            let back = offset_at(text, &lines, a, 0, cx, cy);
            assert_eq!(
                back, cursor,
                "caret at {cursor} drew at ({cx},{cy}) but clicking there gave {back}"
            );
        }
    }

    #[test]
    fn a_caret_scrolled_out_of_view_reports_no_cell() {
        let text = "a\nb\nc\nd\ne\nf";
        let lines = wrap(text, 20);
        let a = area(20, 2);
        assert!(caret_cell(text, &lines, a, 0, 0).is_some());
        assert!(
            caret_cell(text, &lines, a, 4, 0).is_none(),
            "line 0 is scrolled away"
        );
    }

    #[test]
    fn a_selection_normalises_whichever_way_it_was_dragged() {
        let forward = Selection {
            anchor: 2,
            head: 8,
        };
        let backward = Selection {
            anchor: 8,
            head: 2,
        };
        assert_eq!(forward.range(), 2..8);
        assert_eq!(
            backward.range(),
            2..8,
            "dragging right to left is a selection too"
        );
        assert!(Selection::new(4).is_empty());
    }
}
