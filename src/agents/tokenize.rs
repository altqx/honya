//! Tokenizer-free token estimation by script class: a cheap stable estimate to
//! keep chunks near a target budget without invoking a real BPE tokenizer.

/// Estimate tokens of `s`: CJK chars weigh 1.05 tok/char, others 0.30, summed and ceil'd.
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

/// True when `c` is in a Japanese/CJK script block the estimator treats as dense (≈1 tok/char).
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
