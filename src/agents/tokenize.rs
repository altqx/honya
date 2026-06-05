//! src/agents/tokenize.rs — tokenizer-free token estimation by script class.
//!
//! We never call a real BPE tokenizer here; the chunker only needs a stable,
//! cheap upper-ish estimate to keep chunks near a target token budget.
//!
//! Rule (verbatim from the pipeline design):
//!   * CJK characters  → 1.05 tokens/char
//!   * everything else → 0.30 tokens/char
//!   * sum, then ceil to a whole token.

/// Estimate the token count of `s` by classifying each `char` as CJK or other.
///
/// CJK chars weigh 1.05 tok/char, all other chars 0.30 tok/char; the weighted
/// sum is rounded up (`ceil`) to a whole number of tokens. The empty string is
/// zero tokens.
pub fn estimate_tokens(s: &str) -> usize {
    let mut weighted = 0.0_f64;
    for c in s.chars() {
        if is_cjk(c) {
            weighted += 1.05;
        } else {
            weighted += 0.30;
        }
    }
    weighted.ceil() as usize
}

/// True when `c` belongs to a Japanese/CJK script block that the estimator
/// treats as dense (≈1 token per character).
///
/// Covers: hiragana, katakana (incl. phonetic extensions + halfwidth),
/// CJK Unified Ideographs (BMP + Extension A), CJK Compatibility Ideographs,
/// fullwidth/halfwidth forms, the CJK Symbols & Punctuation block, and the
/// katakana middle-dot / iteration marks that sit just outside the kana block.
pub fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        // CJK Symbols and Punctuation (。「」、々〆〇 …)
        0x3000..=0x303F
        // Hiragana
        | 0x3040..=0x309F
        // Katakana (incl. ・ ー and the iteration marks)
        | 0x30A0..=0x30FF
        // Katakana Phonetic Extensions
        | 0x31F0..=0x31FF
        // CJK Unified Ideographs, Extension A
        | 0x3400..=0x4DBF
        // CJK Unified Ideographs (the main block)
        | 0x4E00..=0x9FFF
        // CJK Compatibility Ideographs
        | 0xF900..=0xFAFF
        // Halfwidth and Fullwidth Forms (fullwidth ASCII, halfwidth kana, …)
        | 0xFF00..=0xFFEF
    )
}
