//! Markdown → ratatui rendering backed by [`termimad`].
//!
//! Honya stores chapters as Markdown but renders them inside ratatui panes.  The
//! parser/formatter here delegates block and inline Markdown recognition to
//! termimad, then converts termimad's formatted lines into owned ratatui
//! [`Line`]s/ [`Span`]s using the active [`Theme`].  A tiny pre/post layer keeps
//! honya-specific behavior that termimad intentionally doesn't cover: EPUB image
//! chips, ordinary link labels, underscore emphasis from older cleanses,
//! ordered-list markers, preserved leading prose indentation, and the legacy
//! `&nbsp;` hard-break sentinel.
//!
//! The caller still owns wrapping through `Paragraph::wrap`; `width` is used only
//! for full-width thematic rules.  Every emitted text run passes through
//! [`thai_display_safe`] so Thai SARA AM never smears across cells.

use std::hash::{Hash, Hasher};

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use termimad::{
    Alignment, CompositeKind, FmtComposite, FmtLine, FmtText, MadSkin, RelativePosition,
};

use crate::theme::Theme;
use crate::ui::text::thai_display_safe;

/// Fallback alt text for an image whose `![]()` carried no alt (Thai: "illustration").
const IMAGE_ALT_FALLBACK: &str = "ภาพประกอบ";

/// Private-use markers inserted before termimad parsing, then stripped while
/// applying honya-only styles.  They must be single scalar values so they don't
/// disturb termimad's byte-slice based compounds.
const LINK_START: char = '\u{E000}';
const LINK_END: char = '\u{E001}';
const IMAGE_START: char = '\u{E002}';
const IMAGE_END: char = '\u{E003}';
const ORDER_START: char = '\u{E004}';
const ORDER_END: char = '\u{E005}';
const PROTECTED_SPACE: char = '\u{E006}';
const STAR_LITERAL: char = '\u{E007}';

const NBSP_SENTINEL: &str = "&nbsp;";

/// Render Markdown `md` into themed, owned ratatui lines.
///
/// `base` is the default foreground for ordinary prose (e.g. `theme.ja_text` /
/// `theme.th_text`). `width` sizes thematic-break rules; pass `0` if unknown.
/// Wrapping is intentionally left to the `Paragraph` that consumes these lines.
pub fn render(md: &str, base: Color, theme: &Theme, width: usize) -> Vec<Line<'static>> {
    if md.is_empty() {
        return vec![Line::raw("")];
    }

    let prepared = preprocess_markdown(md);
    let skin = MadSkin::no_style();
    let text = FmtText::from(&skin, &prepared, None);
    let base_style = Style::default().fg(base);
    let mut out = Vec::new();

    for line in &text.lines {
        push_fmt_line(line, base_style, theme, width, &mut out);
    }

    if out.is_empty() {
        out.push(Line::raw(""));
    }
    out
}

/// Memoizes the lines produced by [`render`] (plus any post-styling the caller adds
/// in `build`, e.g. [`highlight`]) so the Markdown parse — the expensive part, run
/// over the *whole* document — happens only when an input changes, not on every
/// 100 ms animation tick. The caller passes a `key` that must capture every input
/// affecting the output (content, width, foreground, theme, highlight/search state …);
/// an unchanged key returns the cached lines untouched. Scrolling is *not* an input —
/// it is applied by the consuming `Paragraph`, so it never invalidates the cache.
#[derive(Default)]
pub struct RenderCache {
    key: Option<u64>,
    lines: Vec<Line<'static>>,
}

impl RenderCache {
    /// The cached lines for `key`, rebuilding via `build` only when the key changed.
    pub fn lines(
        &mut self,
        key: u64,
        build: impl FnOnce() -> Vec<Line<'static>>,
    ) -> &[Line<'static>] {
        if self.key != Some(key) {
            self.lines = build();
            self.key = Some(key);
        }
        &self.lines
    }
}

/// A cheap fingerprint of the theme colors the Markdown renderer bakes into spans, so
/// a [`RenderCache`] key changes when the active theme does — without comparing whole
/// `Theme`s. Covers exactly the palette fields `render`'s span styling reads.
pub fn theme_fingerprint(theme: &Theme) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for c in [
        theme.accent,
        theme.accent_soft,
        theme.ink_soft,
        theme.bg_inset,
        theme.status_image,
        theme.rule,
    ] {
        c.hash(&mut h);
    }
    h.finish()
}

/// Tint every occurrence of any string in `needles` across already-rendered
/// `lines`, merging `over`'s set fields onto each match's existing span style.
///
/// Matching is ASCII-case-insensitive plain substring on the visible span text —
/// exact for CJK/Thai (where case folding is a no-op), forgiving for Latin. Needles
/// are tried longest-first so a longer term tints before one of its prefixes. Empty
/// needles are skipped. The Reader uses this to highlight glossary terms and live
/// search matches without re-parsing Markdown; matching is confined to a single span,
/// so a term split across styling boundaries (rare) is left untinted rather than
/// mis-sliced.
pub fn highlight(lines: &mut [Line<'static>], needles: &[String], over: Style) {
    let mut active: Vec<&str> = needles
        .iter()
        .map(String::as_str)
        .filter(|n| !n.is_empty())
        .collect();
    if active.is_empty() {
        return;
    }
    active.sort_by_key(|n| std::cmp::Reverse(n.len()));
    active.dedup();

    for line in lines.iter_mut() {
        let spans = std::mem::take(&mut line.spans);
        let mut rebuilt: Vec<Span<'static>> = Vec::with_capacity(spans.len());
        for span in spans {
            highlight_span(span, &active, over, &mut rebuilt);
        }
        line.spans = rebuilt;
    }
}

/// Re-emit `span`, splitting out every needle match into its own span tinted with
/// `over` merged over the span's base style. Non-matching runs keep the base style.
fn highlight_span(
    span: Span<'static>,
    needles: &[&str],
    over: Style,
    out: &mut Vec<Span<'static>>,
) {
    let base = span.style;
    let content = span.content.into_owned();
    if content.is_empty() {
        out.push(Span::styled(content, base));
        return;
    }
    let hit = merge_style(base, over);
    let mut cursor = 0usize;
    while cursor < content.len() {
        let mut best: Option<(usize, usize)> = None;
        for &needle in needles {
            if let Some(rel) = ascii_ci_find(&content[cursor..], needle) {
                let start = cursor + rel;
                if best.is_none_or(|(bs, _)| start < bs) {
                    best = Some((start, start + needle.len()));
                }
            }
        }
        match best {
            Some((start, end)) => {
                if start > cursor {
                    out.push(Span::styled(content[cursor..start].to_string(), base));
                }
                out.push(Span::styled(content[start..end].to_string(), hit));
                cursor = end;
            }
            None => {
                out.push(Span::styled(content[cursor..].to_string(), base));
                break;
            }
        }
    }
}

/// Overlay the set fields of `over` (fg / bg / added modifiers) onto `base`. Avoids
/// depending on `Style::patch`, and only the fields the Reader highlights need are
/// merged.
fn merge_style(base: Style, over: Style) -> Style {
    let mut merged = base;
    if let Some(fg) = over.fg {
        merged = merged.fg(fg);
    }
    if let Some(bg) = over.bg {
        merged = merged.bg(bg);
    }
    merged.add_modifier(over.add_modifier)
}

/// `true` when `needle` occurs in `hay` under ASCII-case-insensitive matching.
/// Exposed for the Reader's global search (exact for CJK/Thai, case-forgiving for
/// Latin); an empty needle never matches.
pub fn contains_ci(hay: &str, needle: &str) -> bool {
    !needle.is_empty() && ascii_ci_find(hay, needle).is_some()
}

/// Byte offset of the first ASCII-case-insensitive occurrence of `needle` in `hay`,
/// or `None`. The returned offset always lands on a UTF-8 boundary: ASCII folding
/// never changes byte length and bytes ≥ 0x80 must match exactly, so a match cannot
/// begin mid-codepoint.
fn ascii_ci_find(hay: &str, needle: &str) -> Option<usize> {
    let h = hay.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() || n.len() > h.len() {
        return None;
    }
    (0..=(h.len() - n.len())).find(|&i| {
        h[i..i + n.len()]
            .iter()
            .zip(n)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
    })
}

// ============================================================================
// termimad → ratatui conversion
// ============================================================================

fn push_fmt_line(
    line: &FmtLine<'_>,
    base: Style,
    theme: &Theme,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    match line {
        FmtLine::Normal(fc) => push_composite_line(fc, base, theme, out),
        FmtLine::HorizontalRule => out.push(rule_line(theme, width)),
        FmtLine::TableRow(row) => push_table_row(row, base, theme, out),
        FmtLine::TableRule(rule) => push_table_rule(rule, theme, out),
    }
}

fn push_composite_line(
    fc: &FmtComposite<'_>,
    base: Style,
    theme: &Theme,
    out: &mut Vec<Line<'static>>,
) {
    let mut current = prefix_spans(fc.kind, theme);
    let mut state = MarkerState::default();

    for compound in &fc.compounds {
        let style = compound_style(fc.kind, compound, base, theme);
        append_segments(
            compound.src,
            style,
            theme,
            &mut state,
            &mut current,
            out,
            true,
        );
    }

    out.push(Line::from(current));
}

fn push_table_row(
    row: &termimad::FmtTableRow<'_>,
    base: Style,
    theme: &Theme,
    out: &mut Vec<Line<'static>>,
) {
    let border_style = Style::default().fg(theme.rule);
    let mut spans = vec![Span::styled("│".to_string(), border_style)];

    for cell in &row.cells {
        let content_width = cell.visible_length;
        let (target_width, align) = cell
            .spacing
            .map(|spacing| (spacing.width, spacing.align))
            .unwrap_or((content_width, Alignment::Left));
        let (left_pad, right_pad) = cell_padding(content_width, target_width, align);

        if left_pad > 0 {
            spans.push(Span::styled(" ".repeat(left_pad), base));
        }
        append_composite_inline(cell, base, theme, &mut spans);
        if right_pad > 0 {
            spans.push(Span::styled(" ".repeat(right_pad), base));
        }
        spans.push(Span::styled("│".to_string(), border_style));
    }

    out.push(Line::from(spans));
}

fn push_table_rule(rule: &termimad::FmtTableRule, theme: &Theme, out: &mut Vec<Line<'static>>) {
    let (left, join, right) = match rule.position {
        RelativePosition::Top => ('┌', '┬', '┐'),
        RelativePosition::Bottom => ('└', '┴', '┘'),
        RelativePosition::Other => ('├', '┼', '┤'),
    };
    let mut s = String::new();
    s.push(left);
    for (idx, width) in rule.widths.iter().copied().enumerate() {
        if idx > 0 {
            s.push(join);
        }
        s.extend(std::iter::repeat_n('─', width.max(1)));
    }
    s.push(right);
    out.push(Line::from(Span::styled(s, Style::default().fg(theme.rule))));
}

fn append_composite_inline(
    fc: &FmtComposite<'_>,
    base: Style,
    theme: &Theme,
    spans: &mut Vec<Span<'static>>,
) {
    let mut state = MarkerState::default();
    let mut ignored_breaks = Vec::new();
    for compound in &fc.compounds {
        let style = compound_style(fc.kind, compound, base, theme);
        append_segments(
            compound.src,
            style,
            theme,
            &mut state,
            spans,
            &mut ignored_breaks,
            false,
        );
    }
}

fn prefix_spans(kind: CompositeKind, theme: &Theme) -> Vec<Span<'static>> {
    match kind {
        CompositeKind::Code => vec![Span::styled(
            "  ".to_string(),
            Style::default().fg(theme.ink_soft).bg(theme.bg_inset),
        )],
        CompositeKind::Quote => vec![Span::styled(
            "▏ ".to_string(),
            Style::default().fg(theme.accent_soft),
        )],
        CompositeKind::ListItem(depth) => vec![Span::styled(
            format!("{} • ", " ".repeat(depth as usize)),
            Style::default().fg(theme.accent),
        )],
        CompositeKind::ListItemFollowUp(depth) => {
            vec![Span::raw(format!("{}  ", " ".repeat(depth as usize)))]
        }
        _ => Vec::new(),
    }
}

fn line_style(kind: CompositeKind, base: Style, theme: &Theme) -> Style {
    match kind {
        CompositeKind::Header(1) => Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        CompositeKind::Header(2) => Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
        CompositeKind::Header(_) => Style::default()
            .fg(theme.accent_soft)
            .add_modifier(Modifier::BOLD),
        CompositeKind::Code => Style::default().fg(theme.ink_soft).bg(theme.bg_inset),
        CompositeKind::Quote => Style::default()
            .fg(theme.ink_soft)
            .add_modifier(Modifier::ITALIC),
        _ => base,
    }
}

fn compound_style(
    kind: CompositeKind,
    compound: &termimad::minimad::Compound<'_>,
    base: Style,
    theme: &Theme,
) -> Style {
    let mut style = if compound.code {
        Style::default().fg(theme.accent_soft).bg(theme.bg_inset)
    } else {
        line_style(kind, base, theme)
    };

    if compound.bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if compound.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if compound.strikeout {
        style = style.add_modifier(Modifier::CROSSED_OUT);
    }
    style
}

#[derive(Default)]
struct MarkerState {
    link: bool,
    image: bool,
    order: bool,
}

#[allow(clippy::too_many_arguments)]
fn append_segments(
    text: &str,
    style: Style,
    theme: &Theme,
    state: &mut MarkerState,
    current: &mut Vec<Span<'static>>,
    out: &mut Vec<Line<'static>>,
    break_on_nbsp: bool,
) {
    let mut buf = String::new();
    let mut i = 0usize;

    while i < text.len() {
        let rest = &text[i..];

        if rest.starts_with(NBSP_SENTINEL) {
            flush_segment(&mut buf, style, theme, state, current);
            if break_on_nbsp {
                out.push(Line::from(std::mem::take(current)));
            } else {
                buf.push(' ');
            }
            i += NBSP_SENTINEL.len();
            continue;
        }

        let ch = rest.chars().next().expect("i is on a char boundary");
        match ch {
            LINK_START => {
                flush_segment(&mut buf, style, theme, state, current);
                state.link = true;
            }
            LINK_END => {
                flush_segment(&mut buf, style, theme, state, current);
                state.link = false;
            }
            IMAGE_START => {
                flush_segment(&mut buf, style, theme, state, current);
                state.image = true;
            }
            IMAGE_END => {
                flush_segment(&mut buf, style, theme, state, current);
                state.image = false;
            }
            ORDER_START => {
                flush_segment(&mut buf, style, theme, state, current);
                state.order = true;
            }
            ORDER_END => {
                flush_segment(&mut buf, style, theme, state, current);
                state.order = false;
            }
            _ => buf.push(ch),
        }
        i += ch.len_utf8();
    }

    flush_segment(&mut buf, style, theme, state, current);
}

fn flush_segment(
    buf: &mut String,
    style: Style,
    theme: &Theme,
    state: &MarkerState,
    current: &mut Vec<Span<'static>>,
) {
    if buf.is_empty() {
        return;
    }
    let visible = restore_protected_spaces(buf);
    let visible = thai_display_safe(&visible);
    if visible.is_empty() {
        buf.clear();
        return;
    }
    current.push(Span::styled(visible, effective_style(style, theme, state)));
    buf.clear();
}

fn effective_style(style: Style, theme: &Theme, state: &MarkerState) -> Style {
    if state.image {
        Style::default().fg(theme.status_image)
    } else if state.link {
        style.fg(theme.accent).add_modifier(Modifier::UNDERLINED)
    } else if state.order {
        Style::default().fg(theme.accent_soft)
    } else {
        style
    }
}

fn restore_protected_spaces(s: &str) -> String {
    s.chars()
        .map(|ch| match ch {
            PROTECTED_SPACE => ' ',
            STAR_LITERAL => '*',
            _ => ch,
        })
        .collect()
}

fn cell_padding(content_width: usize, target_width: usize, align: Alignment) -> (usize, usize) {
    if target_width <= content_width {
        return (0, 0);
    }
    let pad = target_width - content_width;
    match align {
        Alignment::Right => (pad, 0),
        Alignment::Center => (pad / 2, pad - pad / 2),
        _ => (0, pad),
    }
}

/// A full-width hairline rule.
fn rule_line(theme: &Theme, width: usize) -> Line<'static> {
    let n = if width == 0 { 24 } else { width.clamp(4, 200) };
    Line::from(Span::styled("─".repeat(n), Style::default().fg(theme.rule)))
}

// ============================================================================
// honya compatibility preprocessing before termimad parsing
// ============================================================================

fn preprocess_markdown(md: &str) -> String {
    let mut lines = Vec::new();
    let mut fence: Option<(char, usize)> = None;

    for raw in md.split('\n') {
        let trimmed = raw.trim_start();

        if let Some((fc, len)) = fence {
            lines.push(raw.to_string());
            if is_fence_close(trimmed, fc, len) {
                fence = None;
            }
            continue;
        }

        if let Some((fc, len)) = fence_open(trimmed) {
            fence = Some((fc, len));
            lines.push(raw.to_string());
            continue;
        }

        lines.push(preprocess_non_code_line(raw));
    }

    lines.join("\n")
}

fn preprocess_non_code_line(raw: &str) -> String {
    let trimmed = raw.trim_start();
    if trimmed.is_empty() {
        return String::new();
    }

    if let Some((level, text)) = heading(trimmed) {
        let hashes = "#".repeat(level as usize);
        let text = preprocess_inline(text);
        return if text.is_empty() {
            format!("{hashes} ")
        } else {
            format!("{hashes} {text}")
        };
    }

    if is_thematic_break(trimmed) {
        return trimmed.to_string();
    }

    if let Some(content) = unordered_list(trimmed) {
        return format!("* {}", preprocess_inline(content));
    }

    if let Some((number, content)) = ordered_list(trimmed) {
        return format!(
            "{ORDER_START} {number}. {ORDER_END}{}",
            preprocess_inline(content)
        );
    }

    // Match the old renderer's behavior: screen prose may intentionally start
    // with spaces (dialogue indentation), and that must not become an indented
    // code block just because termimad supports that Markdown form.
    let raw = if trimmed.starts_with('>') {
        trimmed.to_string()
    } else {
        protect_leading_spaces(raw)
    };
    preprocess_inline(&raw)
}

fn preprocess_inline(text: &str) -> String {
    let mut out = String::new();
    let mut i = 0usize;
    let mut prev: Option<char> = None;

    while i < text.len() {
        let rest = &text[i..];

        if rest.starts_with("![")
            && let Some((label, _url, consumed)) = parse_link(rest, 2)
        {
            let alt = label.trim();
            let alt = if alt.is_empty() {
                IMAGE_ALT_FALLBACK
            } else {
                alt
            };
            out.push(IMAGE_START);
            out.push_str("▣ ");
            out.push_str(&alt.replace(NBSP_SENTINEL, " "));
            out.push(IMAGE_END);
            prev = out.chars().next_back();
            i += consumed;
            continue;
        }

        if rest.starts_with('[')
            && let Some((label, _url, consumed)) = parse_link(rest, 1)
        {
            out.push(LINK_START);
            out.push_str(&preprocess_inline(label));
            out.push(LINK_END);
            prev = out.chars().next_back();
            i += consumed;
            continue;
        }

        if rest.starts_with('`')
            && let Some(consumed) = code_span_len(rest)
        {
            out.push_str(&rest[..consumed]);
            prev = rest[..consumed].chars().next_back();
            i += consumed;
            continue;
        }

        let ch = rest.chars().next().expect("i is on a char boundary");
        if ch == '*' {
            if let Some((delim, inner, consumed)) = valid_star_emphasis(rest) {
                out.push_str(delim);
                out.push_str(&preprocess_inline(inner));
                out.push_str(delim);
                prev = out.chars().next_back();
                i += consumed;
                continue;
            }

            let run_len = rest.bytes().take_while(|&b| b == b'*').count();
            out.extend(std::iter::repeat_n(STAR_LITERAL, run_len));
            prev = Some('*');
            i += run_len;
            continue;
        }

        if ch == '_' {
            let run_len = rest.bytes().take_while(|&b| b == b'_').count();
            let next = rest[run_len..].chars().next();
            if should_convert_underscore(prev, next) {
                out.extend(std::iter::repeat_n('*', run_len));
            } else {
                out.push_str(&rest[..run_len]);
            }
            prev = Some('_');
            i += run_len;
            continue;
        }

        out.push(ch);
        prev = Some(ch);
        i += ch.len_utf8();
    }

    out
}

fn valid_star_emphasis(rest: &str) -> Option<(&'static str, &str, usize)> {
    for delim in ["***", "**", "*"] {
        if rest.starts_with(delim)
            && let Some((inner, consumed)) = match_emphasis(rest, delim)
        {
            return Some((delim, inner, consumed));
        }
    }
    None
}

fn protect_leading_spaces(raw: &str) -> String {
    let spaces = raw.chars().take_while(|&ch| ch == ' ').count();
    if spaces == 0 {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    out.extend(std::iter::repeat_n(PROTECTED_SPACE, spaces));
    out.push_str(&raw[spaces..]);
    out
}

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

/// `# … ###### ` → (level, heading text). Requires a space after the hashes so
/// `#hashtag` stays literal. Drops an optional closing `###` run.
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
    Some((
        hashes as u8,
        rest.trim_end().trim_end_matches('#').trim_end(),
    ))
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

fn unordered_list(s: &str) -> Option<&str> {
    for lead in ['-', '*', '+'] {
        let mut it = s.chars();
        if it.next() == Some(lead) && it.next() == Some(' ') {
            return Some(&s[2..]);
        }
    }
    None
}

fn ordered_list(s: &str) -> Option<(&str, &str)> {
    let digits = s.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let after = &s[digits..];
    let rest = after
        .strip_prefix(". ")
        .or_else(|| after.strip_prefix(") "))?;
    Some((&s[..digits], rest))
}

fn should_convert_underscore(prev: Option<char>, next: Option<char>) -> bool {
    // Termimad/minimad intentionally support star emphasis only.  Convert the
    // underscore flavor used by older cleansed chapters while keeping intraword
    // underscores (`do_a_thing`) literal.
    let prev_word = prev.is_some_and(is_word_char);
    let next_word = next.is_some_and(is_word_char);
    if prev_word && next_word {
        return false;
    }
    let prev_ws = prev.is_none_or(is_delim_space);
    let next_ws = next.is_none_or(is_delim_space);
    !next_ws || !prev_ws
}

fn is_delim_space(ch: char) -> bool {
    ch.is_whitespace() || ch == PROTECTED_SPACE
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
                return Some((&rest[label_start..label_end], &rest[url_start..url_end], i));
            }
            b'\n' => return None,
            _ => i += 1,
        }
    }
    None
}

fn code_span_len(rest: &str) -> Option<usize> {
    let ticks = rest.bytes().take_while(|&b| b == b'`').count();
    if ticks == 0 {
        return None;
    }
    let close = "`".repeat(ticks);
    let rel = rest[ticks..].find(&close)?;
    Some(ticks + rel + ticks)
}

/// Match an emphasis span delimited by `delim`. This mirrors the old renderer's
/// CommonMark-ish flanking rules so termimad doesn't eat unmatched `*` runs.
fn match_emphasis<'a>(rest: &'a str, delim: &str) -> Option<(&'a str, usize)> {
    let dl = delim.len();
    if rest.len() <= dl {
        return None;
    }
    let body = &rest[dl..];
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
    fn ascii_ci_find_is_boundary_safe_and_case_insensitive() {
        assert_eq!(ascii_ci_find("Hello World", "world"), Some(6));
        assert_eq!(ascii_ci_find("ABC", "abc"), Some(0));
        assert_eq!(ascii_ci_find("ไม่เจอ", "xyz"), None);
        // A CJK needle inside a longer CJK haystack returns a byte (not char) offset
        // that still lands on a UTF-8 boundary.
        let off = ascii_ci_find("彼は聖剣を抜いた", "聖剣").unwrap();
        assert!("彼は聖剣を抜いた".is_char_boundary(off));
        assert_eq!(&"彼は聖剣を抜いた"[off..off + "聖剣".len()], "聖剣");
    }

    #[test]
    fn highlight_tints_matches_and_preserves_other_text() {
        let mut lines = render_one("the 聖剣 glows");
        highlight(
            &mut lines,
            &["聖剣".to_string()],
            Style::default().fg(Color::Red),
        );
        // The visible text is unchanged — only spans were split.
        assert_eq!(line_text(&lines[0]), "the 聖剣 glows");
        let tinted = lines[0]
            .spans
            .iter()
            .find(|s| s.content == "聖剣")
            .expect("term split into its own span");
        assert_eq!(tinted.style.fg, Some(Color::Red));
        // Surrounding prose keeps the base color.
        let plain = lines[0].spans.iter().find(|s| s.content == "the ").unwrap();
        assert_ne!(plain.style.fg, Some(Color::Red));
    }

    #[test]
    fn highlight_longest_needle_wins() {
        let mut lines = render_one("聖剣騎士");
        highlight(
            &mut lines,
            &["聖剣".to_string(), "聖剣騎士".to_string()],
            Style::default().fg(Color::Green),
        );
        // The longer term tints as one span rather than leaving a "騎士" tail untinted.
        let tinted = lines[0]
            .spans
            .iter()
            .find(|s| s.style.fg == Some(Color::Green))
            .expect("a tinted span");
        assert_eq!(tinted.content, "聖剣騎士");
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
    fn thai_sara_am_after_tone_mark_is_reordered() {
        let line = first("น้ำ");
        assert_eq!(line_text(&line), "น\u{0E4D}\u{0E49}\u{0E32}");
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

    #[test]
    fn render_cache_rebuilds_only_on_key_change() {
        let mut cache = RenderCache::default();
        let builds = std::cell::Cell::new(0u32);
        let mut build = |key: u64, text: &'static str| {
            cache
                .lines(key, || {
                    builds.set(builds.get() + 1);
                    vec![Line::raw(text)]
                })
                .to_vec()
        };

        assert_eq!(line_text(&build(1, "alpha")[0]), "alpha");
        assert_eq!(builds.get(), 1);

        // Same key: the closure must NOT run again (this is the per-frame fast path).
        assert_eq!(
            line_text(&build(1, "ignored")[0]),
            "alpha",
            "an unchanged key reuses the cached lines"
        );
        assert_eq!(builds.get(), 1);

        // New key: rebuild.
        assert_eq!(line_text(&build(2, "beta")[0]), "beta");
        assert_eq!(builds.get(), 2);
    }

    #[test]
    fn theme_fingerprint_differs_across_themes_and_is_stable() {
        let washi = theme_fingerprint(&Theme::washi());
        assert_eq!(washi, theme_fingerprint(&Theme::washi()), "stable per theme");
        assert_ne!(
            washi,
            theme_fingerprint(&Theme::sumi()),
            "a different palette must change the key"
        );
    }
}
