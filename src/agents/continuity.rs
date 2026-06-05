//! src/agents/continuity.rs — last-N Thai sentence extraction + Translator
//! user-message assembly.
//!
//! `last_thai_sentences` reads `translated/ch_NNN.md`, strips the
//! `<!-- honya:chunk N -->` markers, splits on terminal punctuation (Western +
//! Thai) and newlines, and returns the last `n` non-empty sentences. These are
//! injected into the next chunk's prompt so the Translator keeps tone and
//! pronouns continuous WITHOUT re-translating them.

use once_cell::sync::Lazy;
use regex::Regex;

use crate::workspace::translation::read_translated;
use crate::workspace::Workspace;

/// Matches a `<!-- honya:chunk N -->` marker (any whitespace, any integer).
static CHUNK_MARKER: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"<!--\s*honya:chunk\s+\d+\s*-->").expect("chunk-marker regex is valid")
});

/// Terminal punctuation that ends a sentence for continuity extraction:
/// Western `.!?…` and Thai/East-Asian `。！？` plus the Thai paragraph marks
/// `ฯ` (paiyannoi) and `ๆ` are intentionally NOT treated as terminators (they
/// are word-level), but the eastern/western full stops and bangs are.
static TERMINATOR: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"[.!?。！？…]+[”’」』）】\)\]]*").expect("terminator regex is valid")
});

/// Read the accumulated Thai for `chapter`, strip chunk markers, and return the
/// last `n` non-empty sentences (in original order). Used to seed the next
/// chunk's continuity context. Returns an empty vec when nothing is translated
/// yet or `n == 0`.
pub async fn last_thai_sentences(ws: &Workspace, chapter: u32, n: usize) -> Vec<String> {
    if n == 0 {
        return Vec::new();
    }
    let raw = read_translated(ws, chapter).await;
    if raw.trim().is_empty() {
        return Vec::new();
    }

    // Drop the chunk markers so they never bleed into the prompt.
    let cleaned = CHUNK_MARKER.replace_all(&raw, " ");

    let sentences = split_sentences(&cleaned);
    let len = sentences.len();
    let start = len.saturating_sub(n);
    sentences[start..].to_vec()
}

/// Split arbitrary Thai/Markdown text into trimmed, non-empty sentences using
/// terminal punctuation and hard line breaks as boundaries.
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

/// Assemble the Translator user message: an optional continuity block (the
/// previous Thai sentences, marked do-not-retranslate) followed by the raw
/// Japanese source delimited by `<<SOURCE_JP>> … <<END_SOURCE_JP>>`.
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
