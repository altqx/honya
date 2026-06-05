//! src/ui/text.rs — CJK/Thai-safe column measurement + truncation.
//!
//! Terminal layout for honya MUST be driven by display columns, never byte
//! length or scalar count: a single CJK glyph occupies two columns, many Thai
//! marks occupy zero, and emoji can vary. Every width decision in the UI flows
//! through these three helpers so side-by-side panes and list boxes stay aligned.
//!
//! NEVER use `str::len()` / `str::chars().count()` for layout — they lie about
//! how wide text renders.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Display width of `s` in terminal columns (CJK = 2, combining marks = 0).
///
/// Thin wrapper over `unicode_width` so call sites read intentfully and we have
/// a single choke-point if the width policy ever needs tweaking.
pub fn col_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Truncate `s` so it renders in at most `max_cols` columns.
///
/// If the string already fits, it is returned unchanged. Otherwise graphemes
/// are accumulated until adding the next one would exceed `max_cols - 1`, and a
/// single-column ellipsis `…` is appended (reserving that one column). When
/// `max_cols` is 0 the result is empty; when it is 1 only the ellipsis is kept.
pub fn truncate_cols(s: &str, max_cols: usize) -> String {
    // Fast path: already fits.
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

/// Right-pad `s` with spaces so it renders exactly `cols` columns wide.
///
/// If `s` is already wider than `cols` it is truncated (with an ellipsis) to fit,
/// then padded to the exact width so the returned string always measures `cols`.
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
    // Too wide — truncate to fit, then pad any residual gap (e.g. a 2-col glyph
    // boundary that lands one column short of `cols`).
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

/// Make Thai text safe to render in a terminal cell grid by decomposing SARA AM
/// (ำ, U+0E33) into NIKHAHIT (◌ํ, U+0E4D) + SARA AA (า, U+0E32).
///
/// SARA AM is the one Thai vowel that `unicode-width` reports as a *single
/// grapheme two columns wide*: it packs a base consonant and a spacing vowel into
/// one buffer cell (e.g. `คำ` → one cell, width 2). Many terminals advance only a
/// single column for U+0E33, so their cursor drifts out of step with ratatui's
/// two-column cell and stale `ำ` glyphs smear across the screen the moment the
/// view changes (e.g. tabbing away from the Reader). Splitting it into a true
/// zero-width combining mark (which folds onto the consonant's cell) plus a
/// width-1 vowel makes every cell advance exactly one column — something every
/// terminal agrees on — while preserving the original total column width.
///
/// The fast path returns the input untouched when there is no SARA AM, so this is
/// safe (and free) to call on non-Thai text such as the Japanese source pane.
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
        // The composed SARA AM is gone, replaced by nikhahit + sara aa.
        assert!(!safe.contains('\u{0E33}'));
        assert!(safe.contains('\u{0E4D}') && safe.contains('\u{0E32}'));
        // Total display width is unchanged.
        assert_eq!(col_width(composed), col_width(&safe));
    }

    #[test]
    fn non_thai_is_untouched() {
        let s = "日本語 ABC 123";
        assert_eq!(thai_display_safe(s), s);
    }
}
