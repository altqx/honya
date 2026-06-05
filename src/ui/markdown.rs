//! Markdown → ratatui rendering: turn a Markdown string into styled, wrappable
//! [`Line`]s so the TUI shows *rendered* prose (bold is bold, `#` headings are
//! styled, `![alt](url)` becomes an image chip) instead of leaking raw syntax.
//!
//! Scope is the honya Markdown vocabulary (see [`crate::cleanse`]): emphasis
//! (`**`/`*`, `__`/`_`), inline code, fenced code, ATX headings, blockquotes,
//! unordered/ordered lists, thematic breaks, links and images. `<br>` now cleanses
//! to a `---` thematic break (rendered as a rule); the legacy `&nbsp;` sentinel is
//! still split into line breaks for chapters translated before that change. It is
//! line-oriented and returns one [`Line`] per logical row; the caller's `Paragraph::wrap` reflows
//! long rows. Every emitted text run passes through [`thai_display_safe`] so
//! Thai SARA AM never smears (the one width invariant the whole UI relies on).
//!
//! Deliberately NOT a CommonMark engine: no reference links, tables, nested-list
//! reindentation, or setext headings — those never appear in cleansed chapters.
//! Anything unrecognized falls through as literal text, so it can't panic or eat
//! content.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::Theme;
use crate::ui::text::thai_display_safe;

/// Fallback alt text for an image whose `![]()` carried no alt (Thai: "illustration").
const IMAGE_ALT_FALLBACK: &str = "ภาพประกอบ";

/// Render Markdown `md` into themed lines.
///
/// `base` is the default foreground for ordinary prose (e.g. `theme.ja_text` /
/// `theme.th_text`). `width` is the content width in columns, used only to size
/// thematic-break rules; pass `0` if unknown (a sensible default is used).
pub fn render(md: &str, base: Color, theme: &Theme, width: usize) -> Vec<Line<'static>> {
    let base_style = Style::default().fg(base);
    let mut out: Vec<Line<'static>> = Vec::new();
    // `Some((fence_char, len))` while inside a fenced code block.
    let mut fence: Option<(char, usize)> = None;

    // Split into block lines on REAL newlines. The legacy `&nbsp;` sentinel (from
    // chapters cleansed before `<br>`->`---`) is kept intact through inline parsing —
    // so an emphasis/link/code span can straddle it — and only broken into display
    // lines afterwards, in `emit_block`.
    for raw in md.split('\n') {
        let trimmed = raw.trim_start();

        // ---- inside a fenced code block: emit verbatim until the closer ----
        if let Some((fc, fn_len)) = fence {
            if is_fence_close(trimmed, fc, fn_len) {
                fence = None; // hide the closing fence
            } else {
                out.push(code_line(raw, theme));
            }
            continue;
        }

        // ---- opening fence: hide the marker line, enter code mode ----
        if let Some((fc, fn_len)) = fence_open(trimmed) {
            fence = Some((fc, fn_len));
            continue;
        }

        // ---- blank line → paragraph spacing ----
        if trimmed.is_empty() {
            out.push(Line::raw(""));
            continue;
        }

        // ---- ATX heading ----
        if let Some((level, text)) = heading(trimmed) {
            emit_block(heading_spans(level, text, theme), &mut out);
            continue;
        }

        // ---- thematic break (must precede the `-`/`*` list checks) ----
        if is_thematic_break(trimmed) {
            out.push(rule_line(theme, width));
            continue;
        }

        // ---- blockquote (possibly nested `>>`) ----
        if trimmed.starts_with('>') {
            emit_block(blockquote_spans(trimmed, theme), &mut out);
            continue;
        }

        // ---- list item (unordered `-`/`*`/`+` or ordered `1.`/`1)`) ----
        if let Some((marker, content)) = list_item(trimmed, theme) {
            let mut spans = vec![marker];
            inline_into(content, base_style, theme, &mut spans);
            emit_block(spans, &mut out);
            continue;
        }

        // ---- ordinary prose line: preserve leading indentation ----
        let mut spans = Vec::new();
        inline_into(raw, base_style, theme, &mut spans);
        emit_block(spans, &mut out);
    }

    out
}

/// Emit one logical block as one-or-more display lines, breaking at any legacy
/// `&nbsp;` sentinel while carrying each span's style across the break (for chapters
/// translated before `<br>`->`---`). A span run that holds no sentinel becomes
/// exactly one line.
fn emit_block(spans: Vec<Span<'static>>, out: &mut Vec<Line<'static>>) {
    let mut current: Vec<Span<'static>> = Vec::new();
    for span in spans {
        let style = span.style;
        let content = span.content.into_owned();
        if !content.contains("&nbsp;") {
            current.push(Span::styled(content, style));
            continue;
        }
        let mut parts = content.split("&nbsp;");
        if let Some(first) = parts.next()
            && !first.is_empty()
        {
            current.push(Span::styled(first.to_string(), style));
        }
        for part in parts {
            out.push(Line::from(std::mem::take(&mut current)));
            if !part.is_empty() {
                current.push(Span::styled(part.to_string(), style));
            }
        }
    }
    out.push(Line::from(current));
}

// ============================================================================
// BLOCK CONSTRUCTS
// ============================================================================

/// An opening (or closing) code fence: a run of ≥3 backticks or tildes.
fn fence_open(s: &str) -> Option<(char, usize)> {
    let c = s.chars().next()?;
    if c != '`' && c != '~' {
        return None;
    }
    let len = s.chars().take_while(|&x| x == c).count();
    if len >= 3 { Some((c, len)) } else { None }
}

/// A closing fence: only the fence char, at least as long as the opener.
fn is_fence_close(s: &str, fc: char, open_len: usize) -> bool {
    let t = s.trim_end();
    if t.is_empty() {
        return false;
    }
    let len = t.chars().take_while(|&x| x == fc).count();
    len >= open_len && t.chars().all(|x| x == fc)
}

/// One verbatim code line, inset and washed with the inset background.
fn code_line(raw: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        format!("  {}", thai_display_safe(raw)),
        Style::default().fg(theme.ink_soft).bg(theme.bg_inset),
    ))
}

/// `# … ###### ` → (level, heading text). Requires a space after the hashes so
/// `#hashtag` stays literal.
fn heading(s: &str) -> Option<(u8, &str)> {
    let hashes = s.bytes().take_while(|&b| b == b'#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let after = &s[hashes..];
    if after.is_empty() {
        return Some((hashes as u8, ""));
    }
    let rest = after.strip_prefix(' ')?;
    // Drop an optional trailing run of `#` (closed ATX heading).
    Some((
        hashes as u8,
        rest.trim_end().trim_end_matches('#').trim_end(),
    ))
}

/// Style a heading: indigo, bold, with h1 underlined and h3+ softened.
fn heading_spans(level: u8, text: &str, theme: &Theme) -> Vec<Span<'static>> {
    let style = match level {
        1 => Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        2 => Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
        _ => Style::default()
            .fg(theme.accent_soft)
            .add_modifier(Modifier::BOLD),
    };
    let mut spans = Vec::new();
    inline_into(text, style, theme, &mut spans);
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), style));
    }
    spans
}

/// A thematic break: ≥3 of one of `-`/`*`/`_`, only those plus spaces.
fn is_thematic_break(s: &str) -> bool {
    let t = s.trim();
    let mut chars = t.chars();
    let Some(marker) = chars.next() else {
        return false;
    };
    if marker != '-' && marker != '*' && marker != '_' {
        return false;
    }
    let mut count = 0usize;
    for ch in t.chars() {
        if ch == marker {
            count += 1;
        } else if ch == ' ' || ch == '\t' {
            continue;
        } else {
            return false;
        }
    }
    count >= 3
}

/// A full-width hairline rule.
fn rule_line(theme: &Theme, width: usize) -> Line<'static> {
    let n = if width == 0 { 24 } else { width.clamp(4, 200) };
    Line::from(Span::styled("─".repeat(n), Style::default().fg(theme.rule)))
}

/// Render a blockquote: a left bar per nesting level, then italic soft ink.
fn blockquote_spans(s: &str, theme: &Theme) -> Vec<Span<'static>> {
    let mut rest = s;
    let mut depth = 0usize;
    while let Some(stripped) = rest.strip_prefix('>') {
        depth += 1;
        rest = stripped.strip_prefix(' ').unwrap_or(stripped);
    }
    let bar = "▏".repeat(depth.max(1));
    let mut spans = vec![Span::styled(
        format!("{bar} "),
        Style::default().fg(theme.accent_soft),
    )];
    let style = Style::default()
        .fg(theme.ink_soft)
        .add_modifier(Modifier::ITALIC);
    inline_into(rest, style, theme, &mut spans);
    spans
}

/// Detect a list item; returns the styled marker span and the remaining content.
fn list_item<'a>(s: &'a str, theme: &Theme) -> Option<(Span<'static>, &'a str)> {
    // Unordered: `- ` / `* ` / `+ ` (the trailing space distinguishes `*foo*`).
    for lead in ['-', '*', '+'] {
        let mut it = s.chars();
        if it.next() == Some(lead) && it.next() == Some(' ') {
            let content = &s[2..];
            let marker = Span::styled(" • ".to_string(), Style::default().fg(theme.accent));
            return Some((marker, content));
        }
    }
    // Ordered: one or more digits then `.`/`)` then a space.
    let digits = s.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits > 0 {
        let after = &s[digits..];
        if let Some(rest) = after
            .strip_prefix(". ")
            .or_else(|| after.strip_prefix(") "))
        {
            let marker = Span::styled(
                format!(" {}. ", &s[..digits]),
                Style::default().fg(theme.accent_soft),
            );
            return Some((marker, rest));
        }
    }
    None
}

// ============================================================================
// INLINE PARSING
// ============================================================================

/// Parse inline Markdown in `text`, appending styled spans to `out`. `base` is
/// the style every plain run inherits; emphasis layers modifiers onto it.
fn inline_into(text: &str, base: Style, theme: &Theme, out: &mut Vec<Span<'static>>) {
    let mut buf = String::new();
    let mut i = 0usize;
    // The char immediately before `i` in `text` — used for `_` intraword guards.
    let mut prev: Option<char> = None;

    while i < text.len() {
        let rest = &text[i..];
        if let Some((mut produced, consumed)) = construct(rest, prev, base, theme)
            && consumed > 0
        {
            if !buf.is_empty() {
                out.push(plain_span(std::mem::take(&mut buf), base));
            }
            out.append(&mut produced);
            prev = text[..i + consumed].chars().next_back();
            i += consumed;
            continue;
        }
        let ch = rest.chars().next().expect("i is on a char boundary");
        buf.push(ch);
        prev = Some(ch);
        i += ch.len_utf8();
    }

    if !buf.is_empty() {
        out.push(plain_span(buf, base));
    }
}

/// Try to match an inline construct at the start of `rest`. Returns the produced
/// spans and the number of bytes consumed (always > 0 on success).
fn construct(
    rest: &str,
    prev: Option<char>,
    base: Style,
    theme: &Theme,
) -> Option<(Vec<Span<'static>>, usize)> {
    let bytes = rest.as_bytes();
    let b0 = *bytes.first()?;

    // ---- image: ![alt](url) → an image chip (terminals can't draw the file) ----
    if rest.starts_with("![")
        && let Some((label, _url, consumed)) = parse_link(rest, 2)
    {
        let alt = label.trim();
        let alt = if alt.is_empty() {
            IMAGE_ALT_FALLBACK
        } else {
            alt
        };
        // Keep the chip on one line: collapse any `<br>` sentinel in the alt.
        let alt = thai_display_safe(alt).replace("&nbsp;", " ");
        let span = Span::styled(format!("▣ {alt}"), Style::default().fg(theme.status_image));
        return Some((vec![span], consumed));
    }

    // ---- link: [text](url) → underlined accent text, URL dropped ----
    if b0 == b'['
        && let Some((label, _url, consumed)) = parse_link(rest, 1)
    {
        let link_style = base.fg(theme.accent).add_modifier(Modifier::UNDERLINED);
        let mut spans = Vec::new();
        inline_into(label, link_style, theme, &mut spans);
        if spans.is_empty() {
            spans.push(Span::styled(String::new(), link_style));
        }
        return Some((spans, consumed));
    }

    // ---- inline code: `code` (N backticks) ----
    if b0 == b'`'
        && let Some((code, consumed)) = parse_code(rest)
    {
        let span = Span::styled(
            thai_display_safe(code),
            Style::default().fg(theme.accent_soft).bg(theme.bg_inset),
        );
        return Some((vec![span], consumed));
    }

    // ---- emphasis: ***/**/* and ___/__/_ (longest delimiter first) ----
    if b0 == b'*' || b0 == b'_' {
        let c = b0 as char;
        let triple: String = std::iter::repeat_n(c, 3).collect();
        let double: String = std::iter::repeat_n(c, 2).collect();
        let single: String = std::iter::once(c).collect();
        let candidates = [
            (triple.as_str(), Modifier::BOLD | Modifier::ITALIC),
            (double.as_str(), Modifier::BOLD),
            (single.as_str(), Modifier::ITALIC),
        ];
        for (delim, modifier) in candidates {
            if !rest.starts_with(delim) {
                continue;
            }
            // `_` only opens at a word boundary (so `snake_case` stays literal).
            if c == '_' && prev.is_some_and(is_word_char) {
                continue;
            }
            if let Some((inner, consumed)) = match_emphasis(rest, delim) {
                // `_` only closes at a word boundary.
                if c == '_' && rest[consumed..].chars().next().is_some_and(is_word_char) {
                    continue;
                }
                let mut spans = Vec::new();
                inline_into(inner, base.add_modifier(modifier), theme, &mut spans);
                if spans.is_empty() {
                    continue;
                }
                return Some((spans, consumed));
            }
        }
    }

    None
}

/// A plain (Thai-safe) text span.
fn plain_span(text: String, style: Style) -> Span<'static> {
    Span::styled(thai_display_safe(&text), style)
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Match `[label](url)` (or `![label](url)` when `lead == 2`) starting at `rest`.
/// Returns `(label, url, bytes_consumed)`. Tracks `[]` nesting in the label and
/// stops the URL at the first unescaped `)`; never spans a newline.
fn parse_link(rest: &str, lead: usize) -> Option<(&str, &str, usize)> {
    let b = rest.as_bytes();
    if lead == 0 || lead > b.len() || b[lead - 1] != b'[' {
        return None;
    }
    let label_start = lead;
    let mut i = lead;
    let mut depth = 1i32;
    let mut label_end = None;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'[' => {
                depth += 1;
                i += 1;
            }
            b']' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    label_end = Some(i - 1);
                    break;
                }
            }
            b'\n' => return None,
            _ => i += 1,
        }
    }
    let label_end = label_end?;
    if i >= b.len() || b[i] != b'(' {
        return None;
    }
    i += 1;
    let url_start = i;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b')' => {
                let url_end = i;
                i += 1;
                // Defensive: only return on real char boundaries (escapes can't
                // land mid-codepoint here since all delimiters are ASCII).
                return Some((&rest[label_start..label_end], &rest[url_start..url_end], i));
            }
            b'\n' => return None,
            _ => i += 1,
        }
    }
    None
}

/// Match inline code: N backticks, content, then a closing run of exactly N.
fn parse_code(rest: &str) -> Option<(&str, usize)> {
    let ticks = rest.bytes().take_while(|&b| b == b'`').count();
    if ticks == 0 {
        return None;
    }
    let close = "`".repeat(ticks);
    let after = ticks;
    let rel = rest[after..].find(&close)?;
    Some((&rest[after..after + rel], after + rel + ticks))
}

/// Match an emphasis span delimited by `delim`. Requires a non-empty body whose
/// first and last chars aren't whitespace (CommonMark-ish flanking), and a close
/// on the same line. Returns `(inner, bytes_consumed)`.
fn match_emphasis<'a>(rest: &'a str, delim: &str) -> Option<(&'a str, usize)> {
    let dl = delim.len();
    if rest.len() <= dl {
        return None;
    }
    let body = &rest[dl..];
    // `****` etc. isn't emphasis; and the open must not be followed by space.
    if body.starts_with(delim) || body.starts_with(char::is_whitespace) {
        return None;
    }
    let mut search = 0usize;
    while let Some(rel) = body[search..].find(delim) {
        let close_at = search + rel;
        if close_at == 0 {
            return None;
        }
        let inner = &body[..close_at];
        if inner.ends_with(char::is_whitespace) {
            // A space right before the closer disqualifies it; keep looking.
            search = close_at + 1;
            if search >= body.len() {
                return None;
            }
            continue;
        }
        return Some((inner, dl + close_at + dl));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        Theme::washi()
    }

    /// Flatten a line's spans into its visible text.
    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn render_one(md: &str) -> Vec<Line<'static>> {
        render(md, Color::Reset, &theme(), 40)
    }

    fn first(md: &str) -> Line<'static> {
        render_one(md).into_iter().next().unwrap()
    }

    #[test]
    fn bold_strips_markers_and_sets_bold() {
        let line = first("a **bold** b");
        assert_eq!(line_text(&line), "a bold b");
        let bold = line
            .spans
            .iter()
            .find(|s| s.content == "bold")
            .expect("bold span");
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
        // The surrounding text is not bold.
        let plain = line.spans.iter().find(|s| s.content == "a ").unwrap();
        assert!(!plain.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn italic_strips_markers_and_sets_italic() {
        let line = first("an *italic* word");
        assert_eq!(line_text(&line), "an italic word");
        let it = line.spans.iter().find(|s| s.content == "italic").unwrap();
        assert!(it.style.add_modifier.contains(Modifier::ITALIC));
        assert!(!it.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn bold_italic_triple() {
        let line = first("***wow***");
        assert_eq!(line_text(&line), "wow");
        let s = line.spans.iter().find(|s| s.content == "wow").unwrap();
        assert!(s.style.add_modifier.contains(Modifier::BOLD));
        assert!(s.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn nested_emphasis_composes() {
        // bold outer, italic inner.
        let line = first("**a *b* c**");
        assert_eq!(line_text(&line), "a b c");
        let inner = line.spans.iter().find(|s| s.content == "b").unwrap();
        assert!(inner.style.add_modifier.contains(Modifier::BOLD));
        assert!(inner.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn unmatched_asterisk_is_literal() {
        let line = first("2 * 3 = 6");
        assert_eq!(line_text(&line), "2 * 3 = 6");
        assert!(
            !line
                .spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::ITALIC))
        );
    }

    #[test]
    fn underscores_in_words_are_literal() {
        let line = first("call do_a_thing now");
        assert_eq!(line_text(&line), "call do_a_thing now");
        assert!(
            !line
                .spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::ITALIC))
        );
    }

    #[test]
    fn underscore_emphasis_at_word_boundary() {
        let line = first("a _word_ b");
        assert_eq!(line_text(&line), "a word b");
        let w = line.spans.iter().find(|s| s.content == "word").unwrap();
        assert!(w.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn link_shows_label_not_url_and_underlines() {
        let line = first("see [the docs](https://x.test/a_b) now");
        assert_eq!(line_text(&line), "see the docs now");
        let l = line.spans.iter().find(|s| s.content == "the docs").unwrap();
        assert!(l.style.add_modifier.contains(Modifier::UNDERLINED));
        assert!(!line_text(&line).contains("http"));
    }

    #[test]
    fn image_becomes_chip_not_raw_markdown() {
        let line = first("![ภาพประกอบ](../../images/x.png)");
        let text = line_text(&line);
        assert!(text.starts_with("▣ "), "got {text:?}");
        assert!(!text.contains("]("));
        assert!(!text.contains("png"));
    }

    #[test]
    fn image_without_alt_uses_fallback() {
        let line = first("![](../../images/x.png)");
        assert_eq!(line_text(&line), format!("▣ {IMAGE_ALT_FALLBACK}"));
    }

    #[test]
    fn inline_code_strips_backticks() {
        let line = first("run `cargo test` ok");
        assert_eq!(line_text(&line), "run cargo test ok");
        let code = line
            .spans
            .iter()
            .find(|s| s.content == "cargo test")
            .unwrap();
        assert_eq!(code.style.bg, Some(theme().bg_inset));
    }

    #[test]
    fn heading_strips_hashes_and_styles() {
        let line = first("## Chapter One");
        assert_eq!(line_text(&line), "Chapter One");
        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(line.spans[0].style.fg, Some(theme().accent));
    }

    #[test]
    fn hashtag_without_space_is_not_heading() {
        let line = first("#notaheading");
        assert_eq!(line_text(&line), "#notaheading");
    }

    #[test]
    fn closed_atx_heading_trims_trailing_hashes() {
        let line = first("# Title ##");
        assert_eq!(line_text(&line), "Title");
    }

    #[test]
    fn thematic_break_renders_rule() {
        let lines = render_one("---");
        assert_eq!(lines.len(), 1);
        let t = line_text(&lines[0]);
        assert!(t.chars().all(|c| c == '─'));
        assert!(t.chars().count() >= 4);
    }

    #[test]
    fn bold_line_is_not_thematic_break() {
        let line = first("**bold**");
        assert_eq!(line_text(&line), "bold");
    }

    #[test]
    fn unordered_list_gets_bullet() {
        let line = first("- first item");
        let t = line_text(&line);
        assert!(t.contains('•'));
        assert!(t.contains("first item"));
        assert!(!t.starts_with("- "));
    }

    #[test]
    fn star_list_marker_vs_italic() {
        // `* x` is a list; `*x*` is italic.
        assert!(line_text(&first("* x")).contains('•'));
        assert_eq!(line_text(&first("*x*")), "x");
    }

    #[test]
    fn ordered_list_keeps_number() {
        let line = first("3. third");
        let t = line_text(&line);
        assert!(t.contains("3."));
        assert!(t.contains("third"));
    }

    #[test]
    fn blockquote_gets_bar_and_italic() {
        let line = first("> a quiet line");
        let t = line_text(&line);
        assert!(t.contains('▏'));
        assert!(t.contains("a quiet line"));
        let body = line
            .spans
            .iter()
            .find(|s| s.content.contains("quiet"))
            .unwrap();
        assert!(body.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn fenced_code_hides_fences_and_keeps_body() {
        let md = "before\n```rust\nlet x = 1;\n```\nafter";
        let lines = render_one(md);
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(texts.iter().any(|t| t == "before"));
        assert!(texts.iter().any(|t| t.contains("let x = 1;")));
        assert!(texts.iter().any(|t| t == "after"));
        // No line is a bare ``` fence.
        assert!(
            !texts
                .iter()
                .any(|t| t.trim() == "```" || t.trim() == "```rust")
        );
    }

    #[test]
    fn nbsp_sentinel_becomes_line_break() {
        // honya's <br> → literal &nbsp;; render as two display lines.
        let lines = render_one("first&nbsp;second");
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert_eq!(texts, vec!["first".to_string(), "second".to_string()]);
    }

    #[test]
    fn bold_spanning_nbsp_does_not_leak() {
        // <b>text<br/>bold</b> cleanses to **text&nbsp;bold**: it must break into
        // two lines that are BOTH bold, with no leaked `**` markers.
        let lines = render_one("**text&nbsp;bold**");
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert_eq!(texts, vec!["text".to_string(), "bold".to_string()]);
        for line in &lines {
            assert!(
                !line_text(line).contains('*'),
                "leaked marker: {:?}",
                line_text(line)
            );
            for s in &line.spans {
                assert!(
                    s.style.add_modifier.contains(Modifier::BOLD),
                    "span {:?} lost bold across the break",
                    s.content
                );
            }
        }
    }

    #[test]
    fn italic_spanning_nbsp_does_not_leak() {
        let lines = render_one("*soft&nbsp;words*");
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert_eq!(texts, vec!["soft".to_string(), "words".to_string()]);
        assert!(!texts.join("").contains('*'));
    }

    #[test]
    fn link_spanning_nbsp_does_not_leak() {
        let lines = render_one("[the&nbsp;docs](http://x.test)");
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert_eq!(texts, vec!["the".to_string(), "docs".to_string()]);
        assert!(!texts.join("").contains('['));
        assert!(!texts.join("").contains("http"));
        for line in &lines {
            for s in &line.spans {
                assert!(s.style.add_modifier.contains(Modifier::UNDERLINED));
            }
        }
    }

    #[test]
    fn code_spanning_nbsp_does_not_leak() {
        let lines = render_one("`a&nbsp;b`");
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert_eq!(texts, vec!["a".to_string(), "b".to_string()]);
        assert!(!texts.join("").contains('`'));
    }

    #[test]
    fn image_alt_with_nbsp_stays_one_line() {
        let lines = render_one("![ภาพ&nbsp;ประกอบ](../../images/x.png)");
        assert_eq!(lines.len(), 1);
        assert!(line_text(&lines[0]).starts_with("▣ "));
    }

    #[test]
    fn thai_sara_am_is_decomposed() {
        let line = first("คำ");
        let text = line_text(&line);
        assert!(!text.contains('\u{0E33}'));
        assert!(text.contains('\u{0E4D}') && text.contains('\u{0E32}'));
    }

    #[test]
    fn plain_prose_passes_through() {
        let line = first("ただのテキストです。");
        assert_eq!(line_text(&line), "ただのテキストです。");
    }

    #[test]
    fn leading_indent_is_preserved_on_prose() {
        let line = first("    indented dialogue");
        assert_eq!(line_text(&line), "    indented dialogue");
    }

    #[test]
    fn blank_lines_become_empty_lines() {
        let lines = render_one("a\n\nb");
        assert_eq!(lines.len(), 3);
        assert_eq!(line_text(&lines[1]), "");
    }

    #[test]
    fn unterminated_emphasis_does_not_eat_rest() {
        let line = first("a *b c d");
        assert_eq!(line_text(&line), "a *b c d");
    }

    #[test]
    fn unterminated_link_is_literal() {
        let line = first("[oops](no-close");
        assert_eq!(line_text(&line), "[oops](no-close");
    }

    #[test]
    fn curly_quotes_and_punctuation_survive() {
        let line = first("\u{201C}Hello,\u{201D} she said.");
        assert_eq!(line_text(&line), "\u{201C}Hello,\u{201D} she said.");
    }

    #[test]
    fn empty_input_yields_one_empty_line() {
        let lines = render_one("");
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "");
    }
}
