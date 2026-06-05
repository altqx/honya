//! src/agents/chunk.rs — paragraph-first, sentence-fallback chunking with
//! protected atoms.
//!
//! Pipeline (verbatim from the design):
//!   1. `atomize(md)` splits the markdown into atoms = protected span | plain text.
//!      Protected spans are NEVER split internally (fenced code, image/inline
//!      links, inline `code`, **/__ bold, * / _ italics, ｜漢字《かんじ》 ruby and
//!      bare 《..》).
//!   2. `group_paragraphs` re-joins atoms into paragraph strings, splitting plain
//!      text on blank lines while keeping protected atoms attached to their
//!      paragraph.
//!   3. Greedily pack paragraphs into chunks until adding the next would exceed
//!      `target`; a blank line is always a boundary.
//!   4. A single paragraph above `hard_cap` is split with `split_sentences_capped`
//!      on terminal punctuation (never inside a protected atom).
//!   5. An oversized lone protected atom becomes its own chunk.

use once_cell::sync::Lazy;
use regex::Regex;

use crate::agents::tokenize::estimate_tokens;

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

// --- terminal-punctuation matcher for the sentence fallback ----------------
// Japanese 。！？．… plus the western full stop, each optionally trailed by
// Japanese closing brackets 」』）】 and any run of closing quotes/parens. The
// match ends a sentence; everything up to and including it is one sentence.
static SENTENCE_END: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"[。！？．\.…]+[」』）】\)\]”’]*").expect("sentence-end regex is valid")
});

/// Split a cleansed chapter markdown into translator-sized chunks.
///
/// `target` is the soft token budget per chunk (paragraphs are packed up to it);
/// `hard_cap` is the ceiling above which a lone paragraph is broken into
/// sentences. Indices are assigned in order starting at 0.
pub fn chunk_chapter(md: &str, target: usize, hard_cap: usize) -> Vec<Chunk> {
    let atoms = atomize(md);
    let paragraphs = group_paragraphs(&atoms);

    // Expand any over-cap paragraph into sentence pieces up front so the packer
    // only ever sees pack-able units. Protected-only over-cap paragraphs survive
    // as a single (large) unit and will land in a chunk of their own.
    let mut units: Vec<String> = Vec::new();
    for para in paragraphs {
        if estimate_tokens(&para) > hard_cap {
            for piece in split_sentences_capped(&para, target, hard_cap) {
                if !piece.trim().is_empty() {
                    units.push(piece);
                }
            }
        } else {
            units.push(para);
        }
    }

    // Greedy pack units into chunks, never exceeding `target` unless a single
    // unit is itself larger than `target` (then it stands alone).
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
            // Flush the current chunk; start a new one with this unit.
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

/// Split markdown into protected/plain atoms in a single left-to-right pass.
/// Protected patterns are tried in priority order at each position; the first
/// that matches consumes its span as one `Atom::Protected`.
fn atomize(md: &str) -> Vec<Atom> {
    let bytes = md.as_bytes();
    let len = bytes.len();
    let mut atoms: Vec<Atom> = Vec::new();
    let mut plain = String::new();
    let mut i = 0usize;

    while i < len {
        if let Some(end) = match_protected(md, i) {
            // Flush any pending plain text first.
            if !plain.is_empty() {
                atoms.push(Atom::Plain(std::mem::take(&mut plain)));
            }
            atoms.push(Atom::Protected(md[i..end].to_string()));
            i = end;
            continue;
        }

        // Not a protected start: consume one full char into the plain buffer.
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

/// If a protected span starts exactly at byte offset `start`, return the byte
/// offset just past its end. Patterns are tried in priority order.
fn match_protected(md: &str, start: usize) -> Option<usize> {
    let rest = &md[start..];
    let bytes = rest.as_bytes();

    // 1. Fenced code block: ``` ... ``` (or longer fences), spanning newlines.
    if rest.starts_with("```")
        && let Some(end) = match_fenced_code(rest) {
            return Some(start + end);
        }

    // 2a. Image link: ![alt](url)
    if rest.starts_with("![")
        && let Some(end) = match_link(rest, 2) {
            return Some(start + end);
        }
    // 2b. Inline link: [text](url)  — but not the start of an image (handled above)
    if !bytes.is_empty() && bytes[0] == b'['
        && let Some(end) = match_link(rest, 1) {
            return Some(start + end);
        }

    // 3. Inline code: `code` (single or multiple backticks, same count to close).
    if !bytes.is_empty() && bytes[0] == b'`'
        && let Some(end) = match_inline_code(rest) {
            return Some(start + end);
        }

    // 4. Emphasis: **bold** / __bold__ / *italic* / _italic_ (longest first).
    for delim in ["**", "__", "*", "_"] {
        if rest.starts_with(delim)
            && let Some(end) = match_emphasis(rest, delim) {
                return Some(start + end);
            }
    }

    // 5a. Ruby with base marker: ｜漢字《かんじ》  (fullwidth vertical bar U+FF5C
    //     or ASCII '|' immediately followed by base text then 《reading》).
    if (rest.starts_with('｜') || rest.starts_with('|'))
        && let Some(end) = match_ruby_with_base(rest) {
            return Some(start + end);
        }
    // 5b. Bare ruby reading: 《..》
    if rest.starts_with('《')
        && let Some(end) = match_bracketed(rest, '《', '》') {
            return Some(start + end);
        }

    None
}

/// Match a fenced code block starting at the beginning of `rest`. Returns the
/// byte length of the whole fence including the closing line. An unterminated
/// fence consumes to end-of-string (still one protected atom).
fn match_fenced_code(rest: &str) -> Option<usize> {
    // Count the opening run of backticks (>= 3).
    let fence_len = rest.bytes().take_while(|&b| b == b'`').count();
    if fence_len < 3 {
        return None;
    }
    let fence: String = "`".repeat(fence_len);

    // Find the start of the line after the opening fence line.
    let after_open_line = match rest.find('\n') {
        Some(nl) => nl + 1,
        None => return Some(rest.len()), // opening fence with no newline: whole thing
    };

    // Search subsequent lines for a closing fence of at least `fence_len` ticks.
    let mut offset = after_open_line;
    let tail = &rest[after_open_line..];
    for line in tail.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with(&fence) {
            // Closing fence: consume through the end of this line.
            return Some(offset + line.len());
        }
        offset += line.len();
    }
    // Unterminated: take the rest as one atom.
    Some(rest.len())
}

/// Match a markdown link/image at the start of `rest`. `lead` is the number of
/// leading bytes already known to be the prefix (`1` for `[`, `2` for `![`).
/// Shape: PREFIX `[` text `]` `(` url `)`. Bracket nesting is tracked for the
/// `[...]` part; the URL part stops at the first unescaped `)`.
fn match_link(rest: &str, lead: usize) -> Option<usize> {
    let b = rest.as_bytes();
    // The '[' must be at index lead-1.
    if lead == 0 || lead > b.len() || b[lead - 1] != b'[' {
        return None;
    }
    let mut i = lead; // first char inside the bracket text
    let mut depth = 1i32;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2, // skip escaped char
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
            b'\n' => return None, // links don't span blank structure; bail
            _ => i += 1,
        }
    }
    if depth != 0 {
        return None;
    }
    // Immediately after ']' must be '('.
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

/// Match inline code: a run of N backticks, then content, then the next run of
/// exactly N backticks. Returns the full byte length.
fn match_inline_code(rest: &str) -> Option<usize> {
    let b = rest.as_bytes();
    let ticks = b.iter().take_while(|&&c| c == b'`').count();
    if ticks == 0 {
        return None;
    }
    let close: String = "`".repeat(ticks);
    let after = ticks;
    // Search for the closing run.
    if let Some(rel) = rest[after..].find(&close) {
        return Some(after + rel + ticks);
    }
    None
}

/// Match an emphasis span delimited by `delim` (`**`, `__`, `*`, `_`).
/// Requires a non-empty body and a matching closing delimiter on the same
/// logical run (no intervening blank line). Returns the full byte length.
fn match_emphasis(rest: &str, delim: &str) -> Option<usize> {
    let dl = delim.len();
    if rest.len() <= dl {
        return None;
    }
    let body = &rest[dl..];
    // Closing delimiter must exist; body must be non-empty and not start the
    // close immediately (would be an empty/`****` run — leave those to plain).
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

/// Match `｜base《reading》` (or `|base《reading》`). The base run is everything
/// up to the opening `《`; then the bracketed reading is consumed.
fn match_ruby_with_base(rest: &str) -> Option<usize> {
    let mut chars = rest.char_indices();
    // Consume the leading bar.
    let (_, bar) = chars.next()?;
    if bar != '｜' && bar != '|' {
        return None;
    }
    // Find the opening 《 with at least one base char between bar and it.
    let mut saw_base = false;
    for (idx, c) in chars {
        if c == '《' {
            if !saw_base {
                return None;
            }
            // Consume the bracketed reading from here.
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

/// Match a `open … close` bracketed span at the start of `rest`. Returns the
/// full byte length including both brackets. No nesting; stops at a newline.
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

/// Re-assemble atoms into paragraph strings. Plain atoms are split on blank
/// lines (a run of `\n\n` or more); protected atoms always attach to the
/// paragraph currently being built (they never introduce a boundary).
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
                // Split this plain run on blank lines while keeping attached
                // text glued to the preceding protected atom.
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

/// Find the first blank-line boundary in `s`. Returns `(start, len)` where
/// `start` is the byte offset of the first `\n` of the run and `len` is the
/// number of bytes to skip (the whole run of `\n` / `\r\n` constituting the
/// blank-line separator, which is two-or-more consecutive newlines).
fn find_blank_line(s: &str) -> Option<(usize, usize)> {
    let b = s.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        if b[i] == b'\n' {
            // Count the run of newlines (treating \r as skippable padding).
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

/// Split a single oversized paragraph into sub-strings by terminal punctuation,
/// packing sentences up to `target` and never producing a piece above
/// `hard_cap` unless a single sentence itself exceeds it. Sentence boundaries
/// inside a protected atom are ignored (we re-atomize to find safe cut points).
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

/// Split a paragraph into sentences on terminal punctuation, never cutting
/// inside a protected atom. We atomize the paragraph and only scan plain atoms
/// for terminators; protected atoms are appended whole to the pending sentence.
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
                    // Append up to and including the terminator to the sentence.
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
