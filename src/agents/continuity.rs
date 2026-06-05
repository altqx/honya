//! Last-N Thai sentence extraction + Translator user-message assembly.
//!
//! `last_thai_sentences` returns the previous chunk's tail sentences so the next
//! chunk's prompt keeps tone/pronouns continuous without re-translating them.

use once_cell::sync::Lazy;
use regex::Regex;

use crate::workspace::Workspace;
use crate::workspace::translation::read_translated;

/// Matches a `<!-- honya:chunk N -->` marker (any whitespace, any integer).
static CHUNK_MARKER: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"<!--\s*honya:chunk\s+\d+\s*-->").expect("chunk-marker regex is valid")
});

/// Sentence terminators for continuity: western `.!?…` and east-asian `。！？`. Thai `ฯ`/`ๆ` are word-level, deliberately excluded.
static TERMINATOR: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[.!?。！？…]+[”’」』）】\)\]]*").expect("terminator regex is valid"));

/// Char ceiling on the continuity tail: Thai often lacks terminators, so a lone "sentence" can be a whole long line; bound by length so the prompt can't balloon.
const MAX_CONTINUITY_CHARS: usize = 1200;

/// Return the last `n` non-empty Thai sentences of `chapter` (in order) to seed
/// continuity. Bounded twice: at most `n` sentences AND [`MAX_CONTINUITY_CHARS`] total.
pub async fn last_thai_sentences(ws: &Workspace, chapter: u32, n: usize) -> Vec<String> {
    if n == 0 {
        return Vec::new();
    }
    let raw = read_translated(ws, chapter).await;
    if raw.trim().is_empty() {
        return Vec::new();
    }

    // Strip chunk markers so they never bleed into the prompt.
    let cleaned = CHUNK_MARKER.replace_all(&raw, " ");

    let sentences = split_sentences(&cleaned);
    let len = sentences.len();
    let start = len.saturating_sub(n);
    let mut tail = sentences[start..].to_vec();

    // Drop oldest sentences until the tail fits, then clamp a lone over-long one to its most-recent chars.
    while tail.len() > 1 && joined_chars(&tail) > MAX_CONTINUITY_CHARS {
        tail.remove(0);
    }
    if let Some(last) = tail.last_mut() {
        let count = last.chars().count();
        if count > MAX_CONTINUITY_CHARS {
            *last = last.chars().skip(count - MAX_CONTINUITY_CHARS).collect();
        }
    }
    tail
}

/// Char count of the tail, counting one separator per sentence (the joining newline).
fn joined_chars(tail: &[String]) -> usize {
    tail.iter().map(|s| s.chars().count() + 1).sum()
}

/// Split text into trimmed non-empty sentences on terminal punctuation and line breaks.
fn split_sentences(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();

    for line in text.split('\n') {
        let line = line.trim();
        if line.is_empty() {
            // A blank line is a soft boundary — flush whatever is pending.
            push_trimmed(&mut current, &mut out);
            continue;
        }

        let mut last = 0usize;
        for m in TERMINATOR.find_iter(line) {
            current.push_str(&line[last..m.end()]);
            push_trimmed(&mut current, &mut out);
            last = m.end();
        }
        current.push_str(&line[last..]);
        // Each source line ends a logical unit for our coarse splitter.
        current.push(' ');
        push_trimmed(&mut current, &mut out);
    }

    push_trimmed(&mut current, &mut out);
    out
}

fn push_trimmed(current: &mut String, out: &mut Vec<String>) {
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        out.push(trimmed.to_string());
    }
    current.clear();
}

/// Assemble the Translator user message: optional continuity block then the source delimited by `<<SOURCE_JP>> … <<END_SOURCE_JP>>`.
pub fn build_translator_user_msg(prev_thai: &[String], raw_chunk: &str) -> String {
    let mut s = String::new();

    if !prev_thai.is_empty() {
        s.push_str(&format!(
            "<<CONTINUITY: ประโยคแปลล่าสุด {} ประโยคก่อนหน้า (ห้ามแปลซ้ำ ใช้เพื่อความต่อเนื่องเท่านั้น)>>\n",
            prev_thai.len()
        ));
        for line in prev_thai {
            s.push_str(line.trim());
            s.push('\n');
        }
        s.push_str("<<END_CONTINUITY>>\n\n");
    }

    s.push_str("<<SOURCE_JP>>\n");
    s.push_str(raw_chunk);
    if !raw_chunk.ends_with('\n') {
        s.push('\n');
    }
    s.push_str("<<END_SOURCE_JP>>");
    s
}
