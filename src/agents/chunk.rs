//! Paragraph-first, sentence-fallback chunking with protected atoms.
//!
//! `atomize` splits markdown into protected spans (never split internally:
//! fenced code, links, inline code, emphasis, ruby) and plain text;
//! `group_paragraphs` rejoins them; paragraphs are greedily packed up to
//! `target`, and any paragraph over `hard_cap` is split on sentence boundaries
//! that never fall inside a protected atom.

use once_cell::sync::Lazy;
use regex::Regex;

use crate::agents::tokenize::{estimate_tokens, is_cjk};

/// One unit of work handed to the Translator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub index: usize,
    pub text: String,
    pub est_tokens: usize,
}

/// An atom is either free-flowing plain text (splittable) or a protected span
/// that must travel as one indivisible unit.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Atom {
    Plain(String),
    Protected(String),
}

// Sentence terminator: JP 。！？．… or western full stop, plus trailing closing brackets/quotes.
static SENTENCE_END: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"[。！？．\.…]+[」』）】\)\]”’]*").expect("sentence-end regex is valid")
});

/// Split chapter markdown into chunks: `target` is the soft per-chunk budget, `hard_cap` the ceiling above which a lone paragraph is split into sentences.
pub fn chunk_chapter(md: &str, target: usize, hard_cap: usize) -> Vec<Chunk> {
    let atoms = atomize(md);
    let paragraphs = group_paragraphs(&atoms);

    // Pre-split over-cap paragraphs into pieces so the packer only sees pack-able units.
    let mut units: Vec<String> = Vec::new();
    for para in paragraphs {
        if estimate_tokens(&para) > hard_cap {
            for piece in split_sentences_capped(&para, target, hard_cap) {
                if piece.trim().is_empty() {
                    continue;
                }
                // No terminal punctuation to split on: force under the cap so a chunk can't balloon.
                if estimate_tokens(&piece) > hard_cap {
                    units.extend(hard_split_unit(&piece, hard_cap));
                } else {
                    units.push(piece);
                }
            }
        } else {
            units.push(para);
        }
    }

    // Greedy pack, never exceeding `target` unless a lone unit is itself larger.
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut current = String::new();
    let mut current_tokens = 0usize;

    for unit in units {
        let unit_tokens = estimate_tokens(&unit);

        if current.is_empty() {
            current = unit;
            current_tokens = unit_tokens;
            // A lone unit already over target stands as its own chunk.
            if current_tokens >= target {
                push_chunk(&mut chunks, std::mem::take(&mut current));
                current_tokens = 0;
            }
            continue;
        }

        if current_tokens + unit_tokens > target {
            push_chunk(&mut chunks, std::mem::take(&mut current));
            current = unit;
            current_tokens = unit_tokens;
            if current_tokens >= target {
                push_chunk(&mut chunks, std::mem::take(&mut current));
                current_tokens = 0;
            }
        } else {
            current.push_str("\n\n");
            current.push_str(&unit);
            current_tokens += unit_tokens;
        }
    }

    if !current.trim().is_empty() {
        push_chunk(&mut chunks, current);
    }

    chunks
}

fn push_chunk(chunks: &mut Vec<Chunk>, text: String) {
    let text = text.trim().to_string();
    if text.is_empty() {
        return;
    }
    let index = chunks.len();
    // Recompute tokens from the trimmed text so the figure matches the payload.
    let est_tokens = estimate_tokens(&text);
    chunks.push(Chunk {
        index,
        text,
        est_tokens,
    });
}

/// Split markdown into protected/plain atoms in one left-to-right pass; protected patterns are tried in priority order.
fn atomize(md: &str) -> Vec<Atom> {
    let bytes = md.as_bytes();
    let len = bytes.len();
    let mut atoms: Vec<Atom> = Vec::new();
    let mut plain = String::new();
    let mut i = 0usize;

    while i < len {
        if let Some(end) = match_protected(md, i) {
            if !plain.is_empty() {
                atoms.push(Atom::Plain(std::mem::take(&mut plain)));
            }
            atoms.push(Atom::Protected(md[i..end].to_string()));
            i = end;
            continue;
        }

        let ch = md[i..]
            .chars()
            .next()
            .expect("index points at a char boundary");
        plain.push(ch);
        i += ch.len_utf8();
    }

    if !plain.is_empty() {
        atoms.push(Atom::Plain(plain));
    }
    atoms
}

/// If a protected span starts at byte offset `start`, return the offset just past its end. Patterns tried in priority order.
fn match_protected(md: &str, start: usize) -> Option<usize> {
    let rest = &md[start..];
    let bytes = rest.as_bytes();

    // Fenced code block: ``` ... ``` (or longer fences), spanning newlines.
    if rest.starts_with("```")
        && let Some(end) = match_fenced_code(rest)
    {
        return Some(start + end);
    }

    // Image link: ![alt](url)
    if rest.starts_with("![")
        && let Some(end) = match_link(rest, 2)
    {
        return Some(start + end);
    }
    // Inline link: [text](url) (image case handled above)
    if !bytes.is_empty()
        && bytes[0] == b'['
        && let Some(end) = match_link(rest, 1)
    {
        return Some(start + end);
    }

    // Inline code: `code` (N backticks, same count to close).
    if !bytes.is_empty()
        && bytes[0] == b'`'
        && let Some(end) = match_inline_code(rest)
    {
        return Some(start + end);
    }

    // Emphasis: **bold** / __bold__ / *italic* / _italic_ (longest first).
    for delim in ["**", "__", "*", "_"] {
        if rest.starts_with(delim)
            && let Some(end) = match_emphasis(rest, delim)
        {
            return Some(start + end);
        }
    }

    // Ruby with base marker: ｜漢字《かんじ》 (fullwidth U+FF5C or ASCII '|').
    if (rest.starts_with('｜') || rest.starts_with('|'))
        && let Some(end) = match_ruby_with_base(rest)
    {
        return Some(start + end);
    }
    // Bare ruby reading: 《..》
    if rest.starts_with('《')
        && let Some(end) = match_bracketed(rest, '《', '》')
    {
        return Some(start + end);
    }

    None
}

/// Match a fenced code block; returns its byte length. An unterminated fence consumes to end-of-string.
fn match_fenced_code(rest: &str) -> Option<usize> {
    let fence_len = rest.bytes().take_while(|&b| b == b'`').count();
    if fence_len < 3 {
        return None;
    }
    let fence: String = "`".repeat(fence_len);

    let after_open_line = match rest.find('\n') {
        Some(nl) => nl + 1,
        None => return Some(rest.len()), // opening fence with no newline: whole thing
    };

    let mut offset = after_open_line;
    let tail = &rest[after_open_line..];
    for line in tail.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with(&fence) {
            return Some(offset + line.len());
        }
        offset += line.len();
    }
    Some(rest.len()) // unterminated
}

/// Match a markdown link/image of shape PREFIX `[`text`]``(`url`)`. `lead` is the
/// prefix byte count (`1` for `[`, `2` for `![`); `[...]` nesting is tracked, the URL stops at the first unescaped `)`.
fn match_link(rest: &str, lead: usize) -> Option<usize> {
    let b = rest.as_bytes();
    if lead == 0 || lead > b.len() || b[lead - 1] != b'[' {
        return None;
    }
    let mut i = lead;
    let mut depth = 1i32;
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
                    break;
                }
            }
            b'\n' => return None, // links don't span blank structure
            _ => i += 1,
        }
    }
    if depth != 0 {
        return None;
    }
    if i >= b.len() || b[i] != b'(' {
        return None;
    }
    i += 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b')' => {
                i += 1;
                return Some(i);
            }
            b'\n' => return None,
            _ => i += 1,
        }
    }
    None
}

/// Match inline code: N backticks, content, then a run of exactly N backticks.
fn match_inline_code(rest: &str) -> Option<usize> {
    let b = rest.as_bytes();
    let ticks = b.iter().take_while(|&&c| c == b'`').count();
    if ticks == 0 {
        return None;
    }
    let close: String = "`".repeat(ticks);
    let after = ticks;
    if let Some(rel) = rest[after..].find(&close) {
        return Some(after + rel + ticks);
    }
    None
}

/// Match an emphasis span delimited by `delim` (`**`/`__`/`*`/`_`); needs a non-empty body and a close on the same run.
fn match_emphasis(rest: &str, delim: &str) -> Option<usize> {
    let dl = delim.len();
    if rest.len() <= dl {
        return None;
    }
    let body = &rest[dl..];
    // Empty body (e.g. `****`) isn't emphasis — leave it to plain.
    if body.starts_with(delim) {
        return None;
    }
    // Don't let an emphasis span swallow a paragraph break.
    let search_limit = body.find("\n\n").unwrap_or(body.len());
    let window = &body[..search_limit];
    let close_rel = window.find(delim)?;
    if close_rel == 0 {
        return None;
    }
    Some(dl + close_rel + dl)
}

/// Match `｜base《reading》` (or `|base《reading》`): base run up to `《`, then the bracketed reading.
fn match_ruby_with_base(rest: &str) -> Option<usize> {
    let mut chars = rest.char_indices();
    let (_, bar) = chars.next()?;
    if bar != '｜' && bar != '|' {
        return None;
    }
    // Require at least one base char between the bar and 《.
    let mut saw_base = false;
    for (idx, c) in chars {
        if c == '《' {
            if !saw_base {
                return None;
            }
            let bracket = match_bracketed(&rest[idx..], '《', '》')?;
            return Some(idx + bracket);
        }
        if c == '\n' {
            return None;
        }
        saw_base = true;
    }
    None
}

/// Match an `open … close` bracketed span (no nesting; stops at a newline).
fn match_bracketed(rest: &str, open: char, close: char) -> Option<usize> {
    let mut chars = rest.char_indices();
    let (_, first) = chars.next()?;
    if first != open {
        return None;
    }
    for (idx, c) in chars {
        if c == '\n' {
            return None;
        }
        if c == close {
            return Some(idx + c.len_utf8());
        }
    }
    None
}

/// Re-assemble atoms into paragraphs: plain atoms split on blank lines; protected atoms never introduce a boundary.
fn group_paragraphs(atoms: &[Atom]) -> Vec<String> {
    let mut paragraphs: Vec<String> = Vec::new();
    let mut current = String::new();

    let flush = |current: &mut String, paragraphs: &mut Vec<String>| {
        let trimmed = current.trim();
        if !trimmed.is_empty() {
            paragraphs.push(trimmed.to_string());
        }
        current.clear();
    };

    for atom in atoms {
        match atom {
            Atom::Protected(s) => current.push_str(s),
            Atom::Plain(s) => {
                // Split on blank lines, keeping attached text glued to the preceding protected atom.
                let mut segment = s.as_str();
                while let Some(pos) = find_blank_line(segment) {
                    let (head, tail) = segment.split_at(pos.0);
                    current.push_str(head);
                    flush(&mut current, &mut paragraphs);
                    segment = &tail[pos.1..];
                }
                current.push_str(segment);
            }
        }
    }
    flush(&mut current, &mut paragraphs);
    paragraphs
}

/// Find the first blank-line boundary (>=2 newlines), returning `(start, len)`: byte offset of the run's first `\n` and bytes to skip.
fn find_blank_line(s: &str) -> Option<(usize, usize)> {
    let b = s.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        if b[i] == b'\n' {
            // \r counts as skippable padding within the run.
            let run_start = i;
            let mut j = i;
            let mut newlines = 0usize;
            while j < b.len() && (b[j] == b'\n' || b[j] == b'\r') {
                if b[j] == b'\n' {
                    newlines += 1;
                }
                j += 1;
            }
            if newlines >= 2 {
                return Some((run_start, j - run_start));
            }
            i = j;
        } else {
            i += 1;
        }
    }
    None
}

/// Split an oversized paragraph on terminal punctuation, packing sentences up to `target` and staying under `hard_cap` unless a lone sentence exceeds it.
fn split_sentences_capped(para: &str, target: usize, hard_cap: usize) -> Vec<String> {
    let sentences = split_into_sentences(para);

    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_tokens = 0usize;

    for sentence in sentences {
        let s_tokens = estimate_tokens(&sentence);

        if current.is_empty() {
            current = sentence;
            current_tokens = s_tokens;
            if current_tokens >= hard_cap {
                out.push(std::mem::take(&mut current));
                current_tokens = 0;
            }
            continue;
        }

        if current_tokens + s_tokens > target {
            out.push(std::mem::take(&mut current));
            current = sentence;
            current_tokens = s_tokens;
            if current_tokens >= hard_cap {
                out.push(std::mem::take(&mut current));
                current_tokens = 0;
            }
        } else {
            current.push_str(&sentence);
            current_tokens += s_tokens;
        }
    }

    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Last-resort splitter for a unit still over `hard_cap` (no punctuation to break on): keep protected atoms whole, force-break plain runs by token budget; every piece fits the cap except a lone over-cap protected atom.
fn hard_split_unit(text: &str, hard_cap: usize) -> Vec<String> {
    let cap = hard_cap.max(1);
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_tokens = 0usize;

    let flush = |current: &mut String, current_tokens: &mut usize, out: &mut Vec<String>| {
        let trimmed = current.trim();
        if !trimmed.is_empty() {
            out.push(trimmed.to_string());
        }
        current.clear();
        *current_tokens = 0;
    };

    for atom in atomize(text) {
        let fragments = match atom {
            Atom::Protected(s) => vec![s],
            Atom::Plain(s) if estimate_tokens(&s) <= cap => vec![s],
            Atom::Plain(s) => break_plain_capped(&s, cap),
        };
        for frag in fragments {
            let t = estimate_tokens(&frag);
            if !current.is_empty() && current_tokens + t > cap {
                flush(&mut current, &mut current_tokens, &mut out);
            }
            current.push_str(&frag);
            current_tokens += t;
            if current_tokens >= cap {
                flush(&mut current, &mut current_tokens, &mut out);
            }
        }
    }
    flush(&mut current, &mut current_tokens, &mut out);
    out
}

/// Break a plain string into <=`cap`-token runs, cutting at the last whitespace before the budget (keeps words intact), else at a char boundary (CJK has no spaces).
fn break_plain_capped(s: &str, cap: usize) -> Vec<String> {
    let cap = cap.max(1) as f64;
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut weight = 0.0_f64;
    // Byte offset in `current` just past the most recent whitespace char.
    let mut last_ws: Option<usize> = None;

    let char_weight = |c: char| if is_cjk(c) { 1.05 } else { 0.30 };

    for ch in s.chars() {
        let w = char_weight(ch);
        if weight + w > cap && !current.is_empty() {
            match last_ws.filter(|&i| i > 0 && i < current.len()) {
                Some(idx) => {
                    // Carry the tail after the last whitespace into the next run.
                    let tail = current.split_off(idx);
                    out.push(std::mem::take(&mut current));
                    current = tail;
                }
                None => out.push(std::mem::take(&mut current)),
            }
            weight = current.chars().map(char_weight).sum();
            last_ws = None;
        }
        current.push(ch);
        weight += w;
        if ch.is_whitespace() {
            last_ws = Some(current.len());
        }
    }
    if !current.trim().is_empty() {
        out.push(current);
    }
    out
}

/// Split into sentences on terminal punctuation, never cutting inside a protected atom (only plain atoms are scanned for terminators).
fn split_into_sentences(para: &str) -> Vec<String> {
    let atoms = atomize(para);
    let mut sentences: Vec<String> = Vec::new();
    let mut current = String::new();

    for atom in &atoms {
        match atom {
            Atom::Protected(s) => current.push_str(s),
            Atom::Plain(s) => {
                let mut last = 0usize;
                for m in SENTENCE_END.find_iter(s) {
                    current.push_str(&s[last..m.end()]);
                    sentences.push(std::mem::take(&mut current));
                    last = m.end();
                }
                current.push_str(&s[last..]);
            }
        }
    }
    if !current.trim().is_empty() {
        sentences.push(current);
    }
    sentences
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A paragraph with no terminal punctuation must still be broken so no chunk exceeds the hard cap.
    #[test]
    fn wall_of_text_without_terminators_is_capped() {
        // ~600 CJK chars (≈630 tokens) with zero 。！？ — one giant "sentence".
        let para = "あ".repeat(600);
        let target = 100;
        let hard_cap = 120;
        let chunks = chunk_chapter(&para, target, hard_cap);

        assert!(chunks.len() > 1, "must split into multiple chunks");
        for c in &chunks {
            assert!(
                c.est_tokens <= hard_cap,
                "chunk {} over cap: {} > {}",
                c.index,
                c.est_tokens,
                hard_cap
            );
        }
        let rejoined: String = chunks.iter().map(|c| c.text.replace('\n', "")).collect();
        assert_eq!(rejoined, para);
    }

    /// Normal punctuated prose chunks on sentence boundaries and stays within the cap.
    #[test]
    fn punctuated_prose_stays_within_cap() {
        let para = "これは文です。".repeat(80);
        let chunks = chunk_chapter(&para, 100, 120);
        assert!(!chunks.is_empty());
        for c in &chunks {
            assert!(c.est_tokens <= 120, "chunk over cap: {}", c.est_tokens);
        }
    }
}
