//! Deterministic translation-quality checks that run between Translator and
//! Reviewer. These are intentionally narrow mechanical gates: the LLM Reviewer
//! still judges meaning, tone, glossary, and pronouns.

use once_cell::sync::Lazy;
use regex::Regex;

use crate::model::{GlossaryTerm, TermPolicy};
use crate::workspace::glossary;

static HTML_TAG: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"</?[A-Za-z][A-Za-z0-9:-]*(?:\s+[^<>\n]{0,160})?>")
        .expect("html-tag regex is valid")
});

static MARKDOWN_IMAGE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"!\[[^\]\n]*\]\([^\)\n]+\)").expect("image regex is valid"));

static TRANSLATION_LABEL: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^\s*(?:translation|translated text|thai translation|คำแปล|คำแปลภาษาไทย|แปลไทย)\s*[:：\-]")
        .expect("translation-label regex is valid")
});

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
    if translated_jp_chars >= 30 && translated_jp_chars * 5 >= translated_chars {
        findings.push(
            "translated_text still contains substantial untranslated Japanese; translate remaining Japanese prose while preserving locked names/terms"
                .to_string(),
        );
    }

    audit_terminology(&mut findings, source, translated, terms);

    findings
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
    fn audit_flags_translation_label() {
        let findings = audit_translation_with_terms("彼女は笑った。", "คำแปล: เธอหัวเราะ", &[], &[]);

        assert!(findings.iter().any(|f| f.contains("translation labels")));
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
