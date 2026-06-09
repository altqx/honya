//! Cursor-aware editing for the app's text fields.
//!
//! Every input keeps its value as a plain `String` (read elsewhere verbatim —
//! by config save, the pipeline, tests) plus a byte-offset `cursor` that always
//! sits on a grapheme boundary. These helpers mutate `(value, cursor)` together
//! so arrows / Home / End / word-jumps / forward-delete work the same in every
//! box, and [`caret_halves`] splits a value at the cursor for in-place rendering.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;

use super::text::{col_width, thai_display_safe, truncate_cols};

/// Options for [`handle`], per field kind.
#[derive(Debug, Clone, Copy, Default)]
pub struct EditOpts {
    /// Reject non-digit character input (the numeric settings fields).
    pub numeric_only: bool,
    /// Multi-line value: Home/End are line-relative rather than whole-field.
    pub multiline: bool,
}

/// Outcome of feeding a key to [`handle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edited {
    /// The value was mutated (insert / delete).
    Changed,
    /// Only the cursor moved.
    Moved,
    /// Not an editing/navigation key — the caller should handle it.
    Ignored,
}

fn word_mod(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::ALT)
}

/// Fold one keypress into `(value, cursor)`. Returns [`Edited::Ignored`] for keys
/// the field owner must handle itself (Enter, Tab, Esc, Up/Down, Ctrl-shortcuts).
pub fn handle(value: &mut String, cursor: &mut usize, key: KeyEvent, opts: EditOpts) -> Edited {
    match key.code {
        KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if delete_word_back(value, cursor) {
                Edited::Changed
            } else {
                Edited::Moved
            }
        }
        KeyCode::Char(c)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            if opts.numeric_only && !c.is_ascii_digit() {
                return Edited::Moved;
            }
            insert_char(value, cursor, c);
            Edited::Changed
        }
        KeyCode::Backspace if word_mod(&key) => {
            if delete_word_back(value, cursor) {
                Edited::Changed
            } else {
                Edited::Moved
            }
        }
        KeyCode::Backspace => {
            if backspace(value, cursor) {
                Edited::Changed
            } else {
                Edited::Moved
            }
        }
        KeyCode::Delete => {
            if delete(value, cursor) {
                Edited::Changed
            } else {
                Edited::Moved
            }
        }
        KeyCode::Left if word_mod(&key) => {
            move_word_left(value, cursor);
            Edited::Moved
        }
        KeyCode::Left => {
            move_left(value, cursor);
            Edited::Moved
        }
        KeyCode::Right if word_mod(&key) => {
            move_word_right(value, cursor);
            Edited::Moved
        }
        KeyCode::Right => {
            move_right(value, cursor);
            Edited::Moved
        }
        KeyCode::Home => {
            move_home(value, cursor, opts.multiline);
            Edited::Moved
        }
        KeyCode::End => {
            move_end(value, cursor, opts.multiline);
            Edited::Moved
        }
        _ => Edited::Ignored,
    }
}

/// Snap `cursor` to the nearest grapheme boundary not exceeding `value.len()`.
pub fn clamp_cursor(value: &str, cursor: usize) -> usize {
    if cursor >= value.len() {
        return value.len();
    }
    let mut last = 0;
    for (i, _) in value.grapheme_indices(true) {
        if i <= cursor {
            last = i;
        } else {
            break;
        }
    }
    last
}

/// Insert `c` at the cursor and advance past it.
pub fn insert_char(value: &mut String, cursor: &mut usize, c: char) {
    let pos = clamp_cursor(value, *cursor);
    value.insert(pos, c);
    *cursor = pos + c.len_utf8();
}

/// Delete the grapheme before the cursor (Backspace). Returns whether anything
/// was removed.
pub fn backspace(value: &mut String, cursor: &mut usize) -> bool {
    let c = clamp_cursor(value, *cursor);
    if c == 0 {
        *cursor = 0;
        return false;
    }
    let prev = prev_boundary(value, c);
    value.replace_range(prev..c, "");
    *cursor = prev;
    true
}

/// Delete the grapheme at the cursor (Delete). Returns whether anything was removed.
pub fn delete(value: &mut String, cursor: &mut usize) -> bool {
    let c = clamp_cursor(value, *cursor);
    *cursor = c;
    if c >= value.len() {
        return false;
    }
    let next = next_boundary(value, c);
    value.replace_range(c..next, "");
    true
}

/// Delete from the start of the previous word to the cursor (Ctrl-W). Returns
/// whether anything was removed.
pub fn delete_word_back(value: &mut String, cursor: &mut usize) -> bool {
    let c = clamp_cursor(value, *cursor);
    let mut target = c;
    move_word_left(value, &mut target);
    if target >= c {
        *cursor = c;
        return false;
    }
    value.replace_range(target..c, "");
    *cursor = target;
    true
}

pub fn move_left(value: &str, cursor: &mut usize) {
    let c = clamp_cursor(value, *cursor);
    *cursor = prev_boundary(value, c);
}

pub fn move_right(value: &str, cursor: &mut usize) {
    let c = clamp_cursor(value, *cursor);
    *cursor = next_boundary(value, c);
}

pub fn move_home(value: &str, cursor: &mut usize, multiline: bool) {
    let c = clamp_cursor(value, *cursor);
    *cursor = if multiline { line_start(value, c) } else { 0 };
}

pub fn move_end(value: &str, cursor: &mut usize, multiline: bool) {
    let c = clamp_cursor(value, *cursor);
    *cursor = if multiline {
        line_end(value, c)
    } else {
        value.len()
    };
}

/// Move to the start of the current or previous word (skipping trailing space).
pub fn move_word_left(value: &str, cursor: &mut usize) {
    let c = clamp_cursor(value, *cursor);
    let chars: Vec<(usize, char)> = value[..c].char_indices().collect();
    let mut idx = chars.len();
    while idx > 0 && chars[idx - 1].1.is_whitespace() {
        idx -= 1;
    }
    while idx > 0 && !chars[idx - 1].1.is_whitespace() {
        idx -= 1;
    }
    *cursor = chars.get(idx).map(|&(b, _)| b).unwrap_or(0);
}

/// Move to the start of the next word (skipping the current word + space).
pub fn move_word_right(value: &str, cursor: &mut usize) {
    let c = clamp_cursor(value, *cursor);
    let chars: Vec<(usize, char)> = value[c..].char_indices().map(|(b, ch)| (b + c, ch)).collect();
    let mut idx = 0;
    while idx < chars.len() && !chars[idx].1.is_whitespace() {
        idx += 1;
    }
    while idx < chars.len() && chars[idx].1.is_whitespace() {
        idx += 1;
    }
    *cursor = chars.get(idx).map(|&(b, _)| b).unwrap_or(value.len());
}

fn prev_boundary(value: &str, c: usize) -> usize {
    value[..c]
        .grapheme_indices(true)
        .next_back()
        .map(|(i, _)| i)
        .unwrap_or(0)
}

fn next_boundary(value: &str, c: usize) -> usize {
    value[c..]
        .grapheme_indices(true)
        .nth(1)
        .map(|(i, _)| i + c)
        .unwrap_or(value.len())
}

fn line_start(value: &str, c: usize) -> usize {
    value[..c].rfind('\n').map(|i| i + 1).unwrap_or(0)
}

fn line_end(value: &str, c: usize) -> usize {
    value[c..].find('\n').map(|i| i + c).unwrap_or(value.len())
}

/// Display-safe (`before`, `after`) halves around the caret for a single-line
/// field of `width` columns, scrolled so the caret stays visible when the value
/// overflows. Render `before`, then the caret glyph, then `after`.
pub fn caret_halves(value: &str, cursor: usize, width: usize) -> (String, String) {
    let c = clamp_cursor(value, cursor);
    let left = thai_display_safe(&value[..c]);
    let right = thai_display_safe(&value[c..]);
    let lw = col_width(&left);
    if lw <= width {
        (left, truncate_cols(&right, width - lw))
    } else {
        (take_last_cols(&left, width), String::new())
    }
}

/// Keep the trailing graphemes of `s` that fit within `max` display columns.
fn take_last_cols(s: &str, max: usize) -> String {
    let mut acc = 0usize;
    let mut start = s.len();
    for g in s.graphemes(true).rev() {
        let w = col_width(g);
        if acc + w > max {
            break;
        }
        acc += w;
        start -= g.len();
    }
    s[start..].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }
    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn left_right_then_insert_lands_mid_string() {
        let mut v = "abcd".to_string();
        let mut c = v.len();
        move_left(&v, &mut c);
        move_left(&v, &mut c);
        insert_char(&mut v, &mut c, 'X');
        assert_eq!(v, "abXcd");
        assert_eq!(c, 3);
    }

    #[test]
    fn backspace_deletes_before_cursor_not_end() {
        let mut v = "abcd".to_string();
        let mut c = 2; // between b and c
        assert!(backspace(&mut v, &mut c));
        assert_eq!(v, "acd");
        assert_eq!(c, 1);
    }

    #[test]
    fn delete_removes_char_at_cursor() {
        let mut v = "abcd".to_string();
        let mut c = 1;
        assert!(delete(&mut v, &mut c));
        assert_eq!(v, "acd");
        assert_eq!(c, 1);
        // Delete at end is a no-op.
        let mut end = v.len();
        assert!(!delete(&mut v, &mut end));
    }

    #[test]
    fn home_and_end_jump_to_bounds() {
        let v = "hello".to_string();
        let mut c = 2;
        move_home(&v, &mut c, false);
        assert_eq!(c, 0);
        move_end(&v, &mut c, false);
        assert_eq!(c, v.len());
    }

    #[test]
    fn multiline_home_end_are_line_relative() {
        let v = "ab\ncde".to_string();
        let mut c = 5; // within "cde"
        move_home(&v, &mut c, true);
        assert_eq!(c, 3); // start of second line
        move_end(&v, &mut c, true);
        assert_eq!(c, v.len());
    }

    #[test]
    fn word_moves_skip_whitespace() {
        let v = "foo bar baz".to_string();
        let mut c = v.len();
        move_word_left(&v, &mut c);
        assert_eq!(&v[c..], "baz");
        move_word_left(&v, &mut c);
        assert_eq!(&v[c..], "bar baz");
        move_word_right(&v, &mut c);
        assert_eq!(&v[c..], "baz");
    }

    #[test]
    fn ctrl_w_deletes_previous_word() {
        let mut v = "foo bar".to_string();
        let mut c = v.len();
        assert_eq!(handle(&mut v, &mut c, ctrl(KeyCode::Char('w')), EditOpts::default()), Edited::Changed);
        assert_eq!(v, "foo ");
        assert_eq!(c, 4);
    }

    #[test]
    fn cursor_survives_multibyte_graphemes() {
        let mut v = "あい".to_string(); // two 3-byte chars
        let mut c = v.len();
        move_left(&v, &mut c);
        assert_eq!(c, 3);
        insert_char(&mut v, &mut c, 'X');
        assert_eq!(v, "あXい");
    }

    #[test]
    fn numeric_field_rejects_letters_but_allows_digits() {
        let mut v = String::new();
        let mut c = 0;
        let opts = EditOpts { numeric_only: true, multiline: false };
        assert_eq!(handle(&mut v, &mut c, key(KeyCode::Char('a')), opts), Edited::Moved);
        assert_eq!(v, "");
        assert_eq!(handle(&mut v, &mut c, key(KeyCode::Char('7')), opts), Edited::Changed);
        assert_eq!(v, "7");
    }

    #[test]
    fn caret_halves_splits_at_cursor() {
        let (before, after) = caret_halves("hello", 2, 40);
        assert_eq!(before, "he");
        assert_eq!(after, "llo");
    }

    #[test]
    fn caret_halves_scrolls_to_keep_caret_visible() {
        let v = "0123456789abcdef";
        let (before, after) = caret_halves(v, v.len(), 5);
        assert_eq!(after, "");
        assert_eq!(col_width(&before), 5);
        assert_eq!(before, "bcdef");
    }
}
