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

