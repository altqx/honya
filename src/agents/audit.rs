//! Deterministic translation-quality checks that run between Translator and
//! Reviewer. These are intentionally narrow mechanical gates: the LLM Reviewer
//! still judges meaning, tone, glossary, and pronouns.

use once_cell::sync::Lazy;
use regex::Regex;

use crate::model::{GlossaryTerm, TermPolicy};
use crate::workspace::glossary;

static HTML_TAG: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?ix)
        </?
        (?: a|abbr|b|big|blockquote|br|center|code|del|div|em|font
          | h[1-6]|hr|i|img|image|ins|kbd|li|mark|ol|p|pre|q|rp|rt|ruby
          | s|samp|small|span|strike|strong|sub|sup|svg
          | table|tbody|td|tfoot|th|thead|tr|tt|u|ul|var|wbr )
        \b
        (?:\s+[^<>\n]{0,160})?
        \s*/?>",
    )
    .expect("html-tag regex is valid")
});

static MARKDOWN_IMAGE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"!\[[^\]\n]*\]\([^\)\n]+\)").expect("image regex is valid"));

static TRANSLATION_LABEL: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^\s*(?:translation|translated text|thai translation|คำแปล|คำแปลภาษาไทย|แปลไทย)\s*[:：\-]")
        .expect("translation-label regex is valid")
});

/// A blank-line run — two or more newlines with any surrounding spaces/tabs —
/// left behind after excising a copied span. Collapsed to a single blank line so
/// removing a copy from the middle of a chunk doesn't leave a gap.
static BLANK_RUN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[ \t]*\n[ \t\n]*\n[ \t]*").expect("blank-run regex is valid"));

/// Return concise, actionable findings with deterministic terminology checks for
/// scoped glossary terms that appear in this source chunk.
pub fn audit_translation_with_terms(
    source_jp: &str,
    thai: &str,
    prev_thai: &[String],
    terms: &[GlossaryTerm],
) -> Vec<String> {
    let source = source_jp.trim();
    let translated = thai.trim();
    let mut findings = Vec::new();

    if translated.is_empty() {
        findings.push(
            "translated_text is empty; translate the source chunk instead of returning blank output"
                .to_string(),
        );
        return findings;
    }

    if translated.contains("&nbsp;") {
        findings.push("remove `&nbsp;` tokens from translated_text".to_string());
    }

    if let Some(tag) = HTML_TAG.find(translated) {
        findings.push(format!(
            "remove HTML tag `{}` from translated_text; keep Markdown only",
            tag.as_str()
        ));
    }

    if translated.contains("<<SOURCE_JP>>")
        || translated.contains("<<END_SOURCE_JP>>")
        || translated.contains("<<TRANSLATION_TH>>")
        || translated.contains("<<REFERENCE")
        || translated.contains("<<CONTINUITY")
        || translated.contains("<<REVIEWER_FEEDBACK")
    {
        findings.push("remove prompt delimiter text from translated_text".to_string());
    }

    if TRANSLATION_LABEL.is_match(translated)
        || translated.starts_with("ต่อไปนี้คือคำแปล")
        || translated.starts_with("นี่คือคำแปล")
    {
        findings.push(
            "remove translation labels or assistant prefaces; translated_text must contain only the final Thai Markdown"
                .to_string(),
        );
    }

    if copied_continuity(prev_thai, translated) {
        findings.push(
            "translated_text appears to copy prior continuity context; remove already-translated Thai and translate only the current SOURCE_JP chunk"
                .to_string(),
        );
    }

    if let Some(gloss) = japanese_parenthetical_gloss(translated) {
        findings.push(format!(
            "remove Japanese parenthetical gloss `{gloss}` from translated_text; render ordinary names/terms in Thai only, and mention source Japanese only when it is plot-critical"
        ));
    }

    compare_count(
        &mut findings,
        "scene divider `---`",
        count_scene_dividers(source),
        count_scene_dividers(translated),
        "keep every source scene divider as its own `---` line",
    );

    compare_count(
        &mut findings,
        "Markdown image tag",
        MARKDOWN_IMAGE.find_iter(source).count(),
        MARKDOWN_IMAGE.find_iter(translated).count(),
        "preserve illustration links exactly once and in order",
    );

    compare_count(
        &mut findings,
        "bold marker `**`",
        source.matches("**").count(),
        translated.matches("**").count(),
        "preserve bold Markdown markers around the corresponding translated text",
    );

    compare_count(
        &mut findings,
        "fenced-code marker ```",
        source.matches("```").count(),
        translated.matches("```").count(),
        "preserve fenced code blocks without adding or dropping fences",
    );

    let source_jp_chars = japanese_char_count(source);
    let thai_chars = thai_char_count(translated);
    if source_jp_chars > 0 && thai_chars == 0 {
        findings.push(
            "translated_text contains no Thai characters; translate the Japanese prose into Thai"
                .to_string(),
        );
    }

    let translated_jp_chars = japanese_char_count(translated);
    let translated_chars = translated.chars().filter(|ch| !ch.is_whitespace()).count();
    // Residual JP above ~1/6 of output usually means a half-translated chunk.
    // The floor avoids tripping on one retained name or term.
    if translated_jp_chars >= 24 && translated_jp_chars * 6 >= translated_chars {
        findings.push(
            "translated_text still contains substantial untranslated Japanese; translate remaining Japanese prose while preserving locked names/terms"
                .to_string(),
        );
    }

    if let Some(garbage) = glyphs_absent_from_source(source, translated, is_non_cjk_alien) {
        findings.push(format!(
            "translated_text contains corrupted non-Thai glyphs ({garbage}) that are not in the source; these are decoding artifacts — re-translate the chunk into clean Thai without them"
        ));
    }

    audit_terminology(&mut findings, source, translated, terms);

    findings
}

/// Soft Reviewer signals, not hard gates; false positives must never force reject.
/// Catches dropped multi-digit numbers and severe length shortfalls.
pub fn advisory_findings(source_jp: &str, thai: &str) -> Vec<String> {
    let source = source_jp.trim();
    let translated = thai.trim();
    let mut findings = Vec::new();
    if translated.is_empty() {
        return findings;
    }

    // Multi-digit source numbers should survive; single digits are often spelled out.
    let translated_numbers = digit_runs(translated);
    let mut reported: Vec<String> = Vec::new();
    for num in digit_runs(source) {
        if num.chars().count() < 2 {
            continue;
        }
        if !translated_numbers.contains(&num) && !reported.contains(&num) {
            findings.push(format!(
                "source number `{num}` does not appear in the translation; confirm it was not dropped or altered (it may be spelled out in Thai)"
            ));
            reported.push(num);
        }
    }

    let jp = japanese_char_count(source);
    let th = thai_char_count(translated);
    // Thai under ~3/4 of the source length is an omission smell on substantial chunks.
    if jp >= 80 && th * 4 < jp * 3 {
        findings.push(format!(
            "translation looks much shorter than the source ({th} Thai chars vs {jp} Japanese chars); verify no sentences or details were omitted"
        ));
    }

    // Advisory only: the Reviewer decides whether stray Han is a name or corruption.
    if let Some(glyphs) = glyphs_absent_from_source(source, translated, is_cjk_ideograph) {
        findings.push(format!(
            "translated_text contains Han/CJK characters ({glyphs}) not present in this source chunk; if they are a deliberately retained name/term keep them, otherwise they are stray corruption — verify against the source and re-render in Thai"
        ));
    }

    findings
}

/// Distinct numeric tokens written with ASCII or fullwidth digits, normalized to
/// ASCII so `２０２４` and `2024` compare equal.
fn digit_runs(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let flush = |current: &mut String, out: &mut Vec<String>| {
        if !current.is_empty() {
            if !out.contains(current) {
                out.push(current.clone());
            }
            current.clear();
        }
    };
    for ch in text.chars() {
        let ascii = match ch {
            '0'..='9' => Some(ch),
            '０'..='９' => char::from_u32(ch as u32 - '０' as u32 + '0' as u32),
            _ => None,
        };
        match ascii {
            Some(d) => current.push(d),
            None => flush(&mut current, &mut out),
        }
    }
    flush(&mut current, &mut out);
    out
}

fn audit_terminology(
    findings: &mut Vec<String>,
    source: &str,
    translated: &str,
    terms: &[GlossaryTerm],
) {
    for term in terms {
        let jp = term.jp_term.trim();
        if jp.is_empty() || !source.contains(jp) {
            continue;
        }

        match glossary::effective_policy(term) {
            TermPolicy::HardLocked => {
                let expected = expected_hard_locked_rendering(term);
                if !expected.is_empty() && !translated.contains(expected) {
                    findings.push(format!(
                        "hard-locked glossary term `{jp}` must use exact rendering `{expected}` in translated_text"
                    ));
                }
            }
            TermPolicy::Forbidden => {}
            TermPolicy::Preferred | TermPolicy::ContextDependent => {}
        }

        for forbidden in glossary::forbidden_renderings(term) {
            if !forbidden.is_empty() && translated.contains(&forbidden) {
                findings.push(format!(
                    "forbidden glossary rendering `{forbidden}` was used for `{jp}`; choose an allowed/contextual rendering instead"
                ));
            }
        }
    }
}

fn expected_hard_locked_rendering(term: &GlossaryTerm) -> &str {
    let thai = term.thai_term.trim();
    if !thai.is_empty() {
        thai
    } else if matches!(term.do_not_translate, Some(true)) {
        term.jp_term.trim()
    } else {
        ""
    }
}

fn japanese_parenthetical_gloss(translated: &str) -> Option<String> {
    let mut iter = translated.char_indices().peekable();
    while let Some((open_idx, open)) = iter.next() {
        if !matches!(open, '(' | '（') {
            continue;
        }
        let close = if open == '(' { ')' } else { '）' };
        let content_start = open_idx + open.len_utf8();
        let mut content_end = None;
        while let Some(&(idx, ch)) = iter.peek() {
            iter.next();
            if ch == close {
                content_end = Some(idx);
                break;
            }
            if ch == '\n' || idx.saturating_sub(content_start) > 96 {
                break;
            }
        }
        let Some(end) = content_end else {
            continue;
        };
        let content = translated[content_start..end].trim();
        if is_japanese_only_parenthetical(content) {
            return Some(format!("{open}{content}{close}"));
        }
    }
    None
}

fn is_japanese_only_parenthetical(content: &str) -> bool {
    let mut has_japanese = false;
    let mut count = 0usize;
    for ch in content.chars() {
        if ch.is_whitespace() {
            continue;
        }
        count += 1;
        if is_japanese_text_char(ch) {
            has_japanese = true;
            continue;
        }
        if is_japanese_parenthetical_punct(ch) || ch.is_ascii_digit() || matches!(ch, '０'..='９')
        {
            continue;
        }
        return false;
    }
    has_japanese && count <= 40
}

fn is_japanese_text_char(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3040..=0x309F // Hiragana
            | 0x30A0..=0x30FF // Katakana
            | 0x3400..=0x4DBF // CJK Extension A
            | 0x4E00..=0x9FFF // CJK Unified Ideographs
            | 0xF900..=0xFAFF // CJK Compatibility Ideographs
    )
}

fn is_japanese_parenthetical_punct(ch: char) -> bool {
    matches!(
        ch,
        '々' | '〆'
            | 'ヶ'
            | 'ー'
            | '・'
            | '＝'
            | '、'
            | '。'
            | '「'
            | '」'
            | '『'
            | '』'
            | '〜'
            | '～'
            | '-'
            | '/'
    )
}

/// The Translator receives the previous chunk's last Thai sentences as
/// `<<CONTINUITY>>` context and is told to use them only for flow, never to
/// repeat them. When the surrounding context is sparse (early chapters, before
/// the glossary/character/recap files fill in) it disobeys most often, echoing
/// that tail back into `translated_text`. Deterministically strip those copies so
/// only the current chunk's translation is committed — sparing a retry the
/// Reviewer/audit would otherwise spend rejecting it.
///
/// Removes copies **anywhere** in the output — leading, trailing, merged onto one
/// line, reflowed across lines, or preceded by a preserved scene divider / image
/// / lead-in sentence (the chapter-1 case that otherwise re-flags the audit every
/// attempt). This mirrors [`copied_continuity`]'s "appears anywhere" detection, so
/// once stripped the audit can no longer flag the same copy. Matching is on the
/// whitespace-stripped character stream and consumes only continuity lines the
/// audit itself would flag (see [`copied_continuity`]'s thresholds). If stripping
/// would leave nothing (the whole output was the copy) the original is returned
/// untouched so the audit/Reviewer can still flag it rather than committing empty.
pub fn strip_copied_continuity(prev_thai: &[String], translated: &str) -> String {
    if prev_thai.is_empty() {
        return translated.to_string();
    }

    // The continuity lines the audit recognizes as a "substantial copy" if
    // echoed back, as char vectors for exact matching.
    let mut continuity: Vec<Vec<char>> = prev_thai
        .iter()
        .filter(|line| thai_char_count(line) >= 24)
        .map(|line| {
            normalize_for_duplicate_check(line)
                .chars()
                .collect::<Vec<char>>()
        })
        .filter(|norm| norm.len() >= 32)
        .collect();
    if continuity.is_empty() {
        return translated.to_string();
    }
    // Longest first, so a copied run is consumed maximally per match.
    continuity.sort_by_key(|c| std::cmp::Reverse(c.len()));

    // Non-whitespace chars of the output tagged with their byte span, so a
    // normalized match maps cleanly back to a byte range in the original.
    let norm: Vec<(usize, usize, char)> = translated
        .char_indices()
        .filter(|(_, c)| !c.is_whitespace())
        .map(|(i, c)| (i, i + c.len_utf8(), c))
        .collect();

    // Greedily find every non-overlapping continuity copy, recording the original
    // byte range each one spans (its internal whitespace included).
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    let mut pos = 0usize;
    while pos < norm.len() {
        let remaining = norm.len() - pos;
        let matched = continuity.iter().find(|c| {
            c.len() <= remaining && c.iter().enumerate().all(|(k, ch)| norm[pos + k].2 == *ch)
        });
        match matched {
            Some(c) => {
                cuts.push((norm[pos].0, norm[pos + c.len() - 1].1));
                pos += c.len();
            }
            None => pos += 1,
        }
    }
    if cuts.is_empty() {
        return translated.to_string();
    }

    // Rebuild the output minus the copied spans.
    let mut kept = String::with_capacity(translated.len());
    let mut last = 0usize;
    for (start, end) in cuts {
        kept.push_str(&translated[last..start]);
        last = end;
    }
    kept.push_str(&translated[last..]);

    // Excising a span can leave a blank-line run (or stray spaces) where the copy
    // used to be; collapse them so the committed chunk reads cleanly.
    let cleaned = BLANK_RUN.replace_all(&kept, "\n\n");
    let cleaned = cleaned.trim();

    // The whole output was the copy: leave the original for the audit/Reviewer to
    // flag rather than committing an empty chunk.
    if cleaned.is_empty() {
        translated.to_string()
    } else {
        cleaned.to_string()
    }
}

fn copied_continuity(prev_thai: &[String], translated: &str) -> bool {
    let translated_norm = normalize_for_duplicate_check(translated);
    if translated_norm.is_empty() {
        return false;
    }

    prev_thai.iter().any(|line| {
        let line_norm = normalize_for_duplicate_check(line);
        let thai_chars = thai_char_count(line);
        // Short dialogue beats repeat naturally, so only flag substantial exact
        // copies of the injected continuity tail.
        thai_chars >= 24 && line_norm.chars().count() >= 32 && translated_norm.contains(&line_norm)
    })
}

fn normalize_for_duplicate_check(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>()
}

fn compare_count(
    findings: &mut Vec<String>,
    label: &str,
    source_count: usize,
    translated_count: usize,
    instruction: &str,
) {
    if source_count != translated_count {
        findings.push(format!(
            "{label} count changed: source has {source_count}, translation has {translated_count}; {instruction}"
        ));
    }
}

fn count_scene_dividers(text: &str) -> usize {
    text.lines().filter(|line| line.trim() == "---").count()
}

fn thai_char_count(text: &str) -> usize {
    text.chars()
        .filter(|ch| matches!(*ch as u32, 0x0E00..=0x0E7F))
        .count()
}

/// Non-CJK scripts that never legitimately appear in Thai prose translated from
/// Japanese: Hangul, Cyrillic, and Vietnamese-marked Latin. If the Japanese
/// source genuinely contained any of these, the characters would be present in
/// the source itself (caught by the absent-from-source filter), so seeing one in
/// the output that is *not* in the source is a decoding artifact with near
/// certainty — safe to gate on. CJK is deliberately excluded here: kanji is
/// Japanese, so a stray ideograph might be a retained name (handled as an
/// advisory instead).
///
/// Vietnamese is restricted to its high-signal markers — the precomposed vowels
/// in Latin Extended Additional and the đ/ơ/ư letters — and deliberately omits
/// the Latin-1 accents (à é ê ô …) shared with romanized names and European
/// loanwords, which would false-positive.
fn is_non_cjk_alien(ch: char) -> bool {
    matches!(
        ch as u32,
        0x0400..=0x052F          // Cyrillic + Cyrillic Supplement
            | 0x0110..=0x0111    // Đ đ (Vietnamese)
            | 0x01A0..=0x01B0    // Ơ ơ Ư ư (Vietnamese)
            | 0x1100..=0x11FF    // Hangul Jamo
            | 0x1EA0..=0x1EFF    // Latin Extended Additional (Vietnamese vowels)
            | 0x3130..=0x318F    // Hangul Compatibility Jamo
            | 0xA960..=0xA97F    // Hangul Jamo Extended-A
            | 0xAC00..=0xD7A3    // Hangul Syllables
            | 0xD7B0..=0xD7FF    // Hangul Jamo Extended-B
    )
}

fn is_cjk_ideograph(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3400..=0x4DBF          // CJK Extension A
            | 0x4E00..=0x9FFF    // CJK Unified Ideographs
            | 0xF900..=0xFAFF    // CJK Compatibility Ideographs
    )
}

/// Collect distinct glyphs matching `pred` that appear in the Thai output but are
/// absent from the source. The absent-from-source filter is what keeps this from
/// flagging deliberately-retained Japanese — a name kept verbatim from the source
/// appears in the source, so it passes; a hallucinated token never does. Returns
/// a short, quoted sample of the offending glyphs, or `None` if there are none.
fn glyphs_absent_from_source(
    source: &str,
    translated: &str,
    pred: fn(char) -> bool,
) -> Option<String> {
    use std::collections::HashSet;
    let source_chars: HashSet<char> = source.chars().collect();
    let mut seen: HashSet<char> = HashSet::new();
    let mut sample: Vec<char> = Vec::new();
    for ch in translated.chars() {
        if pred(ch) && !source_chars.contains(&ch) && seen.insert(ch) {
            sample.push(ch);
        }
    }
    if sample.is_empty() {
        return None;
    }
    let shown: String = sample
        .iter()
        .take(8)
        .map(|c| format!("`{c}`"))
        .collect::<Vec<_>>()
        .join(" ");
    let suffix = if sample.len() > 8 { ", …" } else { "" };
    Some(format!("{shown}{suffix}"))
}

fn japanese_char_count(text: &str) -> usize {
    text.chars()
        .filter(|ch| {
            matches!(
                *ch as u32,
                0x3040..=0x309F // Hiragana
                    | 0x30A0..=0x30FF // Katakana
                    | 0x3400..=0x4DBF // CJK Extension A
                    | 0x4E00..=0x9FFF // CJK Unified Ideographs
                    | 0xF900..=0xFAFF // CJK Compatibility Ideographs
            )
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GlossaryTerm, TermPolicy};

    fn term(jp: &str, thai: &str, policy: TermPolicy) -> GlossaryTerm {
        GlossaryTerm {
            jp_term: jp.to_string(),
            thai_term: thai.to_string(),
            romaji: None,
            category: None,
            gloss: None,
            policy: Some(policy),
            forbidden_thai: Vec::new(),
            context_rule: None,
            protected: matches!(
                policy,
                TermPolicy::HardLocked | TermPolicy::Forbidden | TermPolicy::ContextDependent
            )
            .then_some(true),
            do_not_translate: None,
            first_seen_chapter: None,
        }
    }

    #[test]
    fn audit_accepts_clean_thai_markdown() {
        let source =
            "彼女は笑った。\n\n---\n\n![ภาพประกอบ](../images/a.webp)\n\n**強い光**が差した。";
        let thai = "เธอหัวเราะ\n\n---\n\n![ภาพประกอบ](../images/a.webp)\n\n**แสงแรงกล้า**สาดเข้ามา";

        assert_eq!(
            audit_translation_with_terms(source, thai, &[], &[]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn audit_flags_structural_and_language_failures() {
        let source = "一文目。\n\n---\n\n![ภาพประกอบ](../images/a.webp)\n\n**二文目。**";
        let thai = "<div>一文目。</div>\n\n**二文目。** &nbsp;";

        let findings = audit_translation_with_terms(source, thai, &[], &[]);
        assert!(findings.iter().any(|f| f.contains("HTML tag")));
        assert!(findings.iter().any(|f| f.contains("&nbsp;")));
        assert!(findings.iter().any(|f| f.contains("scene divider")));
        assert!(findings.iter().any(|f| f.contains("Markdown image")));
        assert!(findings.iter().any(|f| f.contains("no Thai characters")));
    }

    #[test]
    fn audit_flags_copied_continuity() {
        let source = "彼女は振り返った。";
        let prev = vec!["เธอกำมือแน่นพลางฝืนยิ้มทั้งที่เสียงยังสั่นอยู่เล็กน้อย".to_string()];
        let thai = "เธอกำมือแน่นพลางฝืนยิ้มทั้งที่เสียงยังสั่นอยู่เล็กน้อย\n\nเธอหันกลับไป";

        let findings = audit_translation_with_terms(source, thai, &prev, &[]);

        assert!(findings.iter().any(|f| f.contains("continuity context")));
    }

    #[test]
    fn strip_removes_leading_copied_continuity_and_clears_audit() {
        let prev = vec!["เธอกำมือแน่นพลางฝืนยิ้มทั้งที่เสียงยังสั่นอยู่เล็กน้อย".to_string()];
        let source = "彼女は振り返った。";
        let raw = "เธอกำมือแน่นพลางฝืนยิ้มทั้งที่เสียงยังสั่นอยู่เล็กน้อย\n\nเธอหันกลับไป";

        let cleaned = strip_copied_continuity(&prev, raw);
        assert_eq!(cleaned, "เธอหันกลับไป");

        // After stripping, the deterministic audit no longer flags continuity.
        let findings = audit_translation_with_terms(source, &cleaned, &prev, &[]);
        assert!(!findings.iter().any(|f| f.contains("continuity context")));
    }

    #[test]
    fn strip_removes_non_leading_copied_continuity_and_clears_audit() {
        // The chapter-1 case that used to re-flag the audit every attempt: the
        // model wrote a genuine lead-in sentence, THEN echoed the continuity tail
        // before continuing. The copy isn't leading, so the old leading-only strip
        // left it and the audit flagged it. It must now be removed regardless of
        // position, and the deterministic audit must come back clean.
        let prev = vec!["เธอกำมือแน่นพลางฝืนยิ้มทั้งที่เสียงยังสั่นอยู่เล็กน้อย".to_string()];
        let source = "彼女は振り返った。";
        let raw = format!("เขาเดินเข้ามาในห้องอย่างเงียบงัน\n\n{}\n\nเธอหันกลับไป", prev[0]);

        let cleaned = strip_copied_continuity(&prev, &raw);
        assert!(!cleaned.contains(prev[0].as_str()));
        assert!(cleaned.contains("เขาเดินเข้ามาในห้องอย่างเงียบงัน"));
        assert!(cleaned.contains("เธอหันกลับไป"));

        let findings = audit_translation_with_terms(source, &cleaned, &prev, &[]);
        assert!(!findings.iter().any(|f| f.contains("continuity context")));
    }

    #[test]
    fn strip_collapses_merged_continuity_block() {
        // The model dumped the whole tail concatenated onto one line, then the
        // real translation on the next.
        let prev = vec![
            "เธอกำมือแน่นพลางฝืนยิ้มทั้งที่เสียงยังสั่นอยู่เล็กน้อย".to_string(),
            "ก่อนจะก้าวเดินออกไปจากห้องนั้นอย่างเงียบงัน".to_string(),
        ];
        let raw = format!("{} {}\n\nเขาเปิดประตู", prev[0], prev[1]);

        assert_eq!(strip_copied_continuity(&prev, &raw), "เขาเปิดประตู");
    }

    #[test]
    fn strip_handles_copy_sharing_a_line_with_real_text() {
        // The tail runs straight into the first real sentence on one line.
        let prev = vec!["เธอกำมือแน่นพลางฝืนยิ้มทั้งที่เสียงยังสั่นอยู่เล็กน้อย".to_string()];
        let raw = format!("{} เขาเปิดประตูแล้วเดินจากไป", prev[0]);

        assert_eq!(
            strip_copied_continuity(&prev, &raw),
            "เขาเปิดประตูแล้วเดินจากไป"
        );
    }

    #[test]
    fn strip_handles_continuity_reflowed_across_lines() {
        // The model re-broke one continuity sentence over two lines before the
        // real text — whitespace-insensitive matching still removes it.
        let line = "เธอกำมือแน่นพลางฝืนยิ้มทั้งที่เสียงยังสั่นอยู่เล็กน้อย";
        let prev = vec![line.to_string()];
        let half = line.chars().count() / 2;
        let (a, b): (String, String) = (
            line.chars().take(half).collect(),
            line.chars().skip(half).collect(),
        );
        let raw = format!("{a}\n{b}\n\nเขาเปิดประตู");

        assert_eq!(strip_copied_continuity(&prev, &raw), "เขาเปิดประตู");
    }

    #[test]
    fn strip_preserves_clean_translation() {
        let prev = vec!["เธอกำมือแน่นพลางฝืนยิ้มทั้งที่เสียงยังสั่นอยู่เล็กน้อย".to_string()];
        let clean = "เธอหันกลับไปแล้วเดินจากไป\n\nเขายืนนิ่งอยู่ตรงนั้น";

        assert_eq!(strip_copied_continuity(&prev, clean), clean);
    }

    #[test]
    fn strip_keeps_original_when_only_the_copy_is_present() {
        // Nothing new to keep — leave it for the audit/Reviewer to flag rather
        // than committing an empty translation.
        let prev = vec!["เธอกำมือแน่นพลางฝืนยิ้มทั้งที่เสียงยังสั่นอยู่เล็กน้อย".to_string()];
        let raw = "เธอกำมือแน่นพลางฝืนยิ้มทั้งที่เสียงยังสั่นอยู่เล็กน้อย";

        assert_eq!(strip_copied_continuity(&prev, raw), raw);
    }

    #[test]
    fn audit_flags_translation_label() {
        let findings = audit_translation_with_terms("彼女は笑った。", "คำแปล: เธอหัวเราะ", &[], &[]);

        assert!(findings.iter().any(|f| f.contains("translation labels")));
    }

    #[test]
    fn audit_flags_japanese_parenthetical_glosses() {
        let source = "坂田 (さかた)は幼馴染 (おさななじみ)で、同好会と部に入った。";
        let thai = "ซึ่ง ซากาตะ (さかた) เป็นเพื่อนสมัยเด็ก (おさななじみ) ที่ตั้งชมรม (同好会) กับแผนก (部)";

        let findings = audit_translation_with_terms(source, thai, &[], &[]);

        assert!(
            findings
                .iter()
                .any(|f| f.contains("Japanese parenthetical gloss")),
            "Japanese gloss parenthetical flagged: {findings:?}"
        );
    }

    #[test]
    fn audit_allows_plot_critical_quoted_japanese() {
        let source = "看板には「部」と書いてあった。";
        let thai = "บนป้ายเขียนตัวอักษร \"部\" เอาไว้";

        let findings = audit_translation_with_terms(source, thai, &[], &[]);

        assert!(
            !findings
                .iter()
                .any(|f| f.contains("Japanese parenthetical gloss")),
            "quoted plot-critical Japanese is not a parenthetical gloss: {findings:?}"
        );
    }

    #[test]
    fn advisory_flags_dropped_multidigit_number_but_not_single_digit() {
        // 2024 vanishes → flagged; the single digit 3 spelled out as สาม → not flagged.
        let source = "2024年、3人の少女がいた。";
        let thai = "ในปีนั้น มีเด็กสาวสามคน";
        let findings = advisory_findings(source, thai);
        assert!(
            findings.iter().any(|f| f.contains("2024")),
            "multi-digit number drop flagged: {findings:?}"
        );
        assert!(
            !findings
                .iter()
                .any(|f| f.contains('3') && f.contains("`3`")),
            "single digit must not be flagged: {findings:?}"
        );
    }

    #[test]
    fn advisory_accepts_preserved_fullwidth_number() {
        // Fullwidth ２０２４ in source, ASCII 2024 in translation — normalized equal.
        let findings = advisory_findings("２０２４年。", "ปี 2024");
        assert!(
            findings.is_empty(),
            "normalized number match should not flag: {findings:?}"
        );
    }

    #[test]
    fn advisory_flags_severe_length_shortfall() {
        let source = "あ".repeat(100);
        let thai = "สั้น";
        assert!(
            advisory_findings(&source, thai)
                .iter()
                .any(|f| f.contains("shorter")),
            "a translation far shorter than the source is flagged"
        );
    }

    #[test]
    fn advisory_does_not_flag_normal_length() {
        let source = "彼女は静かに笑って、窓の外を見つめていた。".to_string();
        let thai = "เธอยิ้มอย่างเงียบ ๆ แล้วมองออกไปนอกหน้าต่างอย่างเหม่อลอย".to_string();
        let findings = advisory_findings(&source, &thai);
        assert!(
            findings.is_empty(),
            "ordinary translation should produce no advisory findings: {findings:?}"
        );
    }

    #[test]
    fn audit_flags_corrupted_korean_and_cyrillic_glyphs() {
        // A handful of stray Hangul/Cyrillic glyphs bleeding into otherwise-clean
        // Thai — below the mass-untranslated-Japanese threshold, so only the
        // corruption gate catches them.
        let source = "彼女は黙って立ち上がった。";
        let thai = "เธอเงียบงันแล้วลุกขึ้นยืน 그리고 อย่างช้า ๆ และเดินจากไป";
        let findings = audit_translation_with_terms(source, thai, &[], &[]);
        assert!(
            findings
                .iter()
                .any(|f| f.contains("corrupted non-Thai glyphs")),
            "Korean corruption flagged: {findings:?}"
        );

        let thai_cyr = "เธอเงียบงันแล้วลุกขึ้นยืนและเดินจากไปเดอระวกอย่างเงียบงัน";
        let thai_cyr = format!("{thai_cyr} привет");
        let findings = audit_translation_with_terms(source, &thai_cyr, &[], &[]);
        assert!(
            findings
                .iter()
                .any(|f| f.contains("corrupted non-Thai glyphs")),
            "Cyrillic corruption flagged: {findings:?}"
        );
    }

    #[test]
    fn stray_hanzi_is_advisory_not_a_hard_gate() {
        // A CJK glyph absent from the source is surfaced to the Reviewer as an
        // advisory (kanji may be a retained name) — never as a hard gate that
        // force-rejects, since that risks killing a legitimate translation.
        let source = "彼女は笑った。";
        let thai = "เธอหัวเราะ东";

        let hard = audit_translation_with_terms(source, thai, &[], &[]);
        assert!(
            !hard.iter().any(|f| f.contains("corrupted non-Thai glyphs")),
            "stray CJK must not be a hard gate: {hard:?}"
        );

        let advisory = advisory_findings(source, thai);
        assert!(
            advisory.iter().any(|f| f.contains("Han/CJK characters")),
            "stray CJK surfaced as advisory: {advisory:?}"
        );
    }

    #[test]
    fn audit_flags_stray_vietnamese_but_not_plain_latin() {
        // Vietnamese-accented vowels bleeding into Thai are flagged...
        let source = "彼女は静かに頷いた。";
        let thai = "เธอพยักหน้าอย่างเงียบ ๆ rồi bước đi";
        let findings = audit_translation_with_terms(source, thai, &[], &[]);
        assert!(
            findings
                .iter()
                .any(|f| f.contains("corrupted non-Thai glyphs")),
            "Vietnamese corruption flagged: {findings:?}"
        );

        // ...but a plain-ASCII romanized name is not.
        let thai_ok = "Akira หันกลับมามองเธอ";
        let findings = audit_translation_with_terms(source, thai_ok, &[], &[]);
        assert!(
            !findings
                .iter()
                .any(|f| f.contains("corrupted non-Thai glyphs")),
            "plain Latin name must not be flagged: {findings:?}"
        );
    }

    #[test]
    fn audit_allows_retained_kanji_present_in_source() {
        // A do-not-translate name kept verbatim from the source must trip neither
        // the hard gate nor the advisory — the same kanji appear in the source.
        let source = "田中は剣を抜いた。";
        let thai = "田中ชักดาบออกมา";
        let hard = audit_translation_with_terms(source, thai, &[], &[]);
        assert!(
            !hard.iter().any(|f| f.contains("corrupted non-Thai glyphs")),
            "retained source kanji must not be a hard gate: {hard:?}"
        );
        let advisory = advisory_findings(source, thai);
        assert!(
            !advisory.iter().any(|f| f.contains("Han/CJK characters")),
            "retained source kanji must not be an advisory: {advisory:?}"
        );
    }

    #[test]
    fn audit_enforces_hard_locked_and_forbidden_terms() {
        let mut forbidden = term("黒炎", "ไฟดำ", TermPolicy::Forbidden);
        forbidden.forbidden_thai.push("เปลวไฟทมิฬ".to_string());
        let terms = vec![term("聖剣", "ดาบศักดิ์สิทธิ์", TermPolicy::HardLocked), forbidden];

        let findings =
            audit_translation_with_terms("聖剣が黒炎を切った。", "ดาบเทพตัดเปลวไฟทมิฬ", &[], &terms);

        assert!(findings.iter().any(|f| f.contains("hard-locked")));
        assert!(findings.iter().any(|f| f.contains("forbidden glossary")));
    }
}
