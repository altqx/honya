//! CJK/Thai-safe column measurement + truncation.
//!
//! Terminal layout MUST be driven by display columns, never byte length or
//! scalar count: a CJK glyph is two columns, many Thai marks are zero, emoji
//! vary. Every UI width decision flows through these helpers. NEVER use
//! `str::len()` / `str::chars().count()` for layout — they lie about render width.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Display width of `s` in terminal columns (CJK = 2, combining marks = 0).
pub fn col_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Truncate `s` to at most `max_cols` columns, appending a single-column `…`
/// when it doesn't fit (max_cols 0 → empty, 1 → just the ellipsis).
pub fn truncate_cols(s: &str, max_cols: usize) -> String {
    if col_width(s) <= max_cols {
        return s.to_string();
    }
    if max_cols == 0 {
        return String::new();
    }
    // Reserve one column for the ellipsis.
    let budget = max_cols - 1;
    let mut out = String::new();
    let mut used = 0usize;
    for g in s.graphemes(true) {
        let w = col_width(g);
        if used + w > budget {
            break;
        }
        out.push_str(g);
        used += w;
    }
    out.push('…');
    out
}

/// Right-pad (or truncate) `s` so it renders exactly `cols` columns wide.
pub fn pad_to_cols(s: &str, cols: usize) -> String {
    let w = col_width(s);
    if w == cols {
        return s.to_string();
    }
    if w < cols {
        let mut out = String::with_capacity(s.len() + (cols - w));
        out.push_str(s);
        out.extend(std::iter::repeat_n(' ', cols - w));
        return out;
    }
    // Too wide — truncate, then pad any residual gap (a 2-col glyph boundary can
    // land one column short of `cols`).
    let trimmed = truncate_cols(s, cols);
    let tw = col_width(&trimmed);
    if tw < cols {
        let mut out = String::with_capacity(trimmed.len() + (cols - tw));
        out.push_str(&trimmed);
        out.extend(std::iter::repeat_n(' ', cols - tw));
        out
    } else {
        trimmed
    }
}

/// Decompose Thai SARA AM (U+0E33) into NIKHAHIT (U+0E4D) + SARA AA (U+0E32):
/// many terminals advance only one column for the composed form while ratatui
/// treats it as two, so stale glyphs smear; splitting it keeps every cell at one
/// column while preserving total width. No-op (fast path) when no SARA AM.
pub fn thai_display_safe(s: &str) -> String {
    if !s.contains('\u{0E33}') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    for ch in s.chars() {
        if ch == '\u{0E33}' {
            out.push('\u{0E4D}');
            out.push('\u{0E32}');
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sara_am_decomposes_and_keeps_width() {
        let composed = "ค\u{0E33}"; // คำ
        let safe = thai_display_safe(composed);
        assert!(!safe.contains('\u{0E33}'));
        assert!(safe.contains('\u{0E4D}') && safe.contains('\u{0E32}'));
        assert_eq!(col_width(composed), col_width(&safe));
    }

    #[test]
    fn non_thai_is_untouched() {
        let s = "日本語 ABC 123";
        assert_eq!(thai_display_safe(s), s);
    }
}
