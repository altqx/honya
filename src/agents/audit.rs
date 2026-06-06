//! Deterministic translation-quality checks that run between Translator and
//! Reviewer. These are intentionally narrow mechanical gates: the LLM Reviewer
//! still judges meaning, tone, glossary, and pronouns.

use once_cell::sync::Lazy;
use regex::Regex;

static HTML_TAG: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"</?[A-Za-z][A-Za-z0-9:-]*(?:\s+[^<>\n]{0,160})?>")
        .expect("html-tag regex is valid")
});

static MARKDOWN_IMAGE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"!\[[^\]\n]*\]\([^\)\n]+\)").expect("image regex is valid"));

/// Return concise, actionable findings. An empty list means the deterministic
/// gate did not find a mechanical problem.
pub fn audit_translation(source_jp: &str, thai: &str) -> Vec<String> {
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
    {
        findings.push("remove prompt delimiter text from translated_text".to_string());
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

    findings
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

    #[test]
    fn audit_accepts_clean_thai_markdown() {
        let source =
            "彼女は笑った。\n\n---\n\n![ภาพประกอบ](../images/a.webp)\n\n**強い光**が差した。";
        let thai = "เธอหัวเราะ\n\n---\n\n![ภาพประกอบ](../images/a.webp)\n\n**แสงแรงกล้า**สาดเข้ามา";

        assert_eq!(audit_translation(source, thai), Vec::<String>::new());
    }

    #[test]
    fn audit_flags_structural_and_language_failures() {
        let source = "一文目。\n\n---\n\n![ภาพประกอบ](../images/a.webp)\n\n**二文目。**";
        let thai = "<div>一文目。</div>\n\n**二文目。** &nbsp;";

        let findings = audit_translation(source, thai);
        assert!(findings.iter().any(|f| f.contains("HTML tag")));
        assert!(findings.iter().any(|f| f.contains("&nbsp;")));
        assert!(findings.iter().any(|f| f.contains("scene divider")));
        assert!(findings.iter().any(|f| f.contains("Markdown image")));
        assert!(findings.iter().any(|f| f.contains("no Thai characters")));
    }
}
