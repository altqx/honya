//! Deterministic translation-quality checks that run between Translator and
//! Reviewer. These are intentionally narrow mechanical gates: the LLM Reviewer
//! still judges meaning, tone, glossary, and pronouns.

use once_cell::sync::Lazy;
use regex::Regex;

use crate::model::{Character, GlossaryTerm, TermPolicy};
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

const DISCOURAGED_CASUAL_PARTICLES: [(&str, &str); 2] = [("ว่ะ", "ฟะ"), ("วะ", "ฟะ")];

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

    if let Some(gloss) = translated_parenthetical_gloss(translated) {
        findings.push(format!(
            "remove reading/original gloss `{gloss}` from translated_text; choose the natural Thai wording only unless the original sound/spelling is plot-critical"
        ));
    }

    if let Some(residue) = hard_japanese_punctuation_residue(translated) {
        findings.push(format!(
            "replace Japanese punctuation/brackets `{residue}` in translated_text with natural Thai punctuation, unless the source explicitly requires visible Japanese story text"
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

/// Deterministic character-voice checks backed by explicit roster rules.
/// Unlike name-surface hints, these only fire when CHARACTERS.md clearly says a
/// scoped male `俺` speaker must use a specific Thai self-pronoun and not `ฉัน`.
pub fn audit_character_pronoun_rules(
    source_jp: &str,
    thai: &str,
    current_pov: Option<&str>,
    characters: &[Character],
) -> Vec<String> {
    let source = source_jp.trim();
    let translated = thai.trim();
    if translated.is_empty()
        || !source.contains('俺')
        || !chan_conflicts_with_ore_voice(source, translated)
    {
        return Vec::new();
    }

    let candidates = characters
        .iter()
        .filter_map(|character| {
            explicit_male_ore_pronoun_rule(character).map(|expected| (character, expected))
        })
        .collect::<Vec<_>>();
    let require_anchor = candidates.len() > 1;
    let mut rules = Vec::new();
    for (character, expected) in candidates {
        if require_anchor && !character_voice_anchor(character, source, current_pov) {
            continue;
        }
        let name = if character.thai_name.trim().is_empty() {
            character.jp_name.trim()
        } else {
            character.thai_name.trim()
        };
        if name.is_empty() {
            continue;
        }
        rules.push((name.to_string(), expected));
    }
    if rules.is_empty() {
        return Vec::new();
    }

    let expected = rules.first().map(|(_, expected)| *expected).unwrap_or("ผม");
    let names = rules
        .iter()
        .take(3)
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(" / ");

    vec![format!(
        "POV/pronoun roster conflict: SOURCE_JP uses `俺` and translated_text uses `ฉัน`, but scoped CHARACTERS.md says `{names}` uses `{expected}` and does not use `ฉัน`; replace that narrator/speaker's self-reference and register with `{expected}` unless the line belongs to a different explicitly allowed speaker"
    )]
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

    if let Some((particle, preferred)) = discouraged_casual_particle(translated) {
        findings.push(format!(
            "discouraged casual Thai particle `{particle}` appears; prefer `{preferred}` unless this rare roughness is important to SOURCE_JP or an established character voice"
        ));
    }

    if let Some(residue) = japanese_punctuation_residue(translated) {
        findings.push(format!(
            "translated_text contains Japanese punctuation/brackets `{residue}`; verify it is intentional story text, otherwise replace it with natural Thai punctuation or remove the leftover source markers"
        ));
    }

    if ore_source_with_chan_rendering(source, translated) {
        findings.push(
            "SOURCE_JP uses masculine/casual first-person `俺`, while translated_text uses `ฉัน`; verify the narrator/speaker's CHARACTERS.md voice explicitly allows `ฉัน`, otherwise use the correct male self-pronoun/register and do not carry a female POV pronoun into this chunk"
                .to_string(),
        );
    }

    if reciprocal_bond_rendered_as_gift(source, translated) {
        findings.push(
            "SOURCE_JP uses reciprocal bond wording such as `互い`/`向け合う`/`絆`, but translated_text uses `มอบให้`; verify this does not turn a mutual/shared bond into a one-way gift-like action, and prefer wording like `มีต่อกัน`, `ผูกพันกัน`, or `มีให้กัน` when faithful"
                .to_string(),
        );
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

/// Advisory signals with scoped character surfaces from the roster injected into
/// this chunk. Character metadata is useful but not deterministic enough to force
/// local retries: aliases and address forms can be contextual, and the Reviewer
/// should make the final call.
pub fn advisory_findings_with_references(
    source_jp: &str,
    thai: &str,
    characters: &[Character],
) -> Vec<String> {
    let mut findings = advisory_findings(source_jp, thai);
    if thai.trim().is_empty() {
        return findings;
    }
    audit_character_names(&mut findings, source_jp.trim(), thai.trim(), characters);
    findings
}

fn discouraged_casual_particle(text: &str) -> Option<(&'static str, &'static str)> {
    DISCOURAGED_CASUAL_PARTICLES
        .into_iter()
        .find(|(particle, _)| contains_discouraged_particle(text, particle))
}

fn japanese_punctuation_residue(text: &str) -> Option<String> {
    let mut found = Vec::new();
    for ch in text.chars().filter(|ch| is_japanese_residue_punct(*ch)) {
        if !found.contains(&ch) {
            found.push(ch);
        }
        if found.len() >= 8 {
            break;
        }
    }
    (!found.is_empty()).then(|| found.into_iter().collect())
}

fn hard_japanese_punctuation_residue(text: &str) -> Option<String> {
    let mut found = Vec::new();
    for ch in text
        .chars()
        .filter(|ch| matches!(ch, '。' | '、' | '「' | '」' | '『' | '』' | '（' | '）'))
    {
        if !found.contains(&ch) {
            found.push(ch);
        }
        if found.len() >= 8 {
            break;
        }
    }
    (!found.is_empty()).then(|| found.into_iter().collect())
}

pub fn normalize_japanese_punctuation_residue(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '。' => '.',
            '、' => ',',
            '？' => '?',
            '！' => '!',
            '（' => '(',
            '）' => ')',
            '「' | '」' => '"',
            '『' | '』' => '\'',
            _ => ch,
        })
        .collect()
}

fn is_japanese_residue_punct(ch: char) -> bool {
    matches!(
        ch,
        '。' | '、' | '？' | '！' | '「' | '」' | '『' | '』' | '（' | '）'
    )
}

fn ore_source_with_chan_rendering(source: &str, translated: &str) -> bool {
    source.contains('俺') && chan_conflicts_with_ore_voice(source, translated)
}

fn chan_conflicts_with_ore_voice(source: &str, translated: &str) -> bool {
    translated.lines().any(line_has_unquoted_chan)
        || paired_quote_has_ore_and_chan(source, translated)
}

fn paired_quote_has_ore_and_chan(source: &str, translated: &str) -> bool {
    let source_quotes = quote_lines(source);
    let translated_quotes = quote_lines(translated);
    source_quotes
        .iter()
        .zip(translated_quotes.iter())
        .any(|(source_line, translated_line)| {
            source_line.contains('俺') && translated_line.contains("ฉัน")
        })
}

fn quote_lines(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|line| line.starts_with(['“', '‘', '"', '「', '『']))
        .collect()
}

fn line_has_unquoted_chan(line: &str) -> bool {
    let mut in_quote = false;
    for (idx, ch) in line.char_indices() {
        if ch == '"' {
            in_quote = !in_quote;
            continue;
        }
        if matches!(ch, '“' | '‘' | '「' | '『') {
            in_quote = true;
            continue;
        }
        if matches!(ch, '”' | '’' | '」' | '』') {
            in_quote = false;
            continue;
        }
        if !in_quote && line[idx..].starts_with("ฉัน") {
            return true;
        }
    }
    false
}

fn reciprocal_bond_rendered_as_gift(source: &str, translated: &str) -> bool {
    has_any(source, &["互い", "お互い", "向け合", "向き合"])
        && has_any(source, &["絆", "関係", "思い", "気持"])
        && has_any(translated, &["มอบให้", "มอบแก่", "มอบความ"])
}

fn has_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn explicit_male_ore_pronoun_rule(character: &Character) -> Option<&'static str> {
    if !is_male_character(character) {
        return None;
    }

    let mut rule_text = String::new();
    if let Some(style) = character.speech_style.as_deref() {
        rule_text.push_str(style);
    }
    if let Some(notes) = character.notes.as_deref() {
        rule_text.push('\n');
        rule_text.push_str(notes);
    }

    let normalized = normalize_pronoun_rule_text(&rule_text);
    if normalized.contains('俺')
        && normalized.contains("ผม")
        && (normalized.contains("ไม่ใช้ฉัน")
            || normalized.contains("ห้ามใช้ฉัน")
            || normalized.contains("notuseฉัน")
            || normalized.contains("doesnotuseฉัน"))
    {
        Some("ผม")
    } else {
        None
    }
}

fn character_voice_anchor(character: &Character, source: &str, current_pov: Option<&str>) -> bool {
    character_jp_surface_in(character, source)
        || current_pov.is_some_and(|pov| {
            character_jp_surface_in(character, pov)
                || contains_nonempty(pov, character.thai_name.trim())
                || character
                    .romaji
                    .as_deref()
                    .is_some_and(|romaji| contains_nonempty(pov, romaji.trim()))
                || contains_nonempty(pov, character.id.trim())
        })
}

fn character_jp_surface_in(character: &Character, text: &str) -> bool {
    contains_nonempty(text, character.jp_name.trim())
        || character
            .aliases
            .iter()
            .any(|alias| contains_nonempty(text, alias.trim()))
        || character
            .also_called
            .iter()
            .any(|alt| contains_nonempty(text, alt.jp.trim()))
}

fn contains_nonempty(text: &str, needle: &str) -> bool {
    !needle.is_empty() && text.contains(needle)
}

fn is_male_character(character: &Character) -> bool {
    let Some(gender) = character.gender.as_deref() else {
        return false;
    };
    let gender = gender.trim().to_lowercase();
    !gender.contains("female")
        && !gender.contains("หญิง")
        && (gender.contains("male") || gender.contains("ชาย"))
}

fn normalize_pronoun_rule_text(text: &str) -> String {
    text.chars()
        .filter(|ch| {
            !ch.is_whitespace()
                && !matches!(
                    *ch,
                    '\'' | '"' | '`' | '“' | '”' | '‘' | '’' | '「' | '」' | '『' | '』'
                )
        })
        .collect::<String>()
        .to_lowercase()
}

fn audit_character_names(
    findings: &mut Vec<String>,
    source: &str,
    translated: &str,
    characters: &[Character],
) {
    let mut reported: Vec<(String, String)> = Vec::new();
    for c in characters {
        let canonical = c.thai_name.trim();
        audit_character_surface(
            findings,
            &mut reported,
            source,
            translated,
            c.jp_name.trim(),
            canonical,
        );
        for alt in &c.also_called {
            let expected = if alt.thai.trim().is_empty() {
                canonical
            } else {
                alt.thai.trim()
            };
            audit_character_surface(
                findings,
                &mut reported,
                source,
                translated,
                alt.jp.trim(),
                expected,
            );
        }
    }
}

fn audit_character_surface(
    findings: &mut Vec<String>,
    reported: &mut Vec<(String, String)>,
    source: &str,
    translated: &str,
    surface: &str,
    expected: &str,
) {
    if surface.is_empty()
        || expected.is_empty()
        || !contains_japanese_text(surface)
        || !source.contains(surface)
        || translated.contains(expected)
    {
        return;
    }
    let key = (surface.to_string(), expected.to_string());
    if reported.contains(&key) {
        return;
    }
    reported.push(key);
    findings.push(format!(
        "character/name surface `{surface}` appears in SOURCE_JP but translated_text does not contain required rendering `{expected}` from CHARACTERS.md"
    ));
}

fn contains_discouraged_particle(text: &str, particle: &str) -> bool {
    let mut start = 0usize;
    while let Some(rel) = text[start..].find(particle) {
        let idx = start + rel;
        let end = idx + particle.len();
        let embedded_name_syllable =
            matches!(particle, "วะ" | "ว่ะ") && previous_char(text, idx).is_some_and(is_thai_mark);
        if !embedded_name_syllable && has_particle_boundary_after(text, end) {
            return true;
        }
        start = end;
    }
    false
}

fn has_particle_boundary_after(text: &str, end: usize) -> bool {
    let rest = &text[end..];
    rest.is_empty()
        || rest.starts_with("เนี่ย")
        || rest.starts_with("นี่")
        || rest.starts_with("นะ")
        || rest.chars().next().is_some_and(is_particle_boundary_char)
}

fn previous_char(text: &str, idx: usize) -> Option<char> {
    text[..idx].chars().next_back()
}

fn is_thai_mark(ch: char) -> bool {
    matches!(ch as u32, 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E)
}

fn is_particle_boundary_char(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '.' | ','
                | '!'
                | '?'
                | ':'
                | ';'
                | '"'
                | '\''
                | ')'
                | ']'
                | '}'
                | '>'
                | '…'
                | '。'
                | '、'
                | '！'
                | '？'
                | '”'
                | '’'
                | '」'
                | '』'
                | '）'
                | '】'
                | 'ฯ'
                | 'ๆ'
        )
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

fn contains_japanese_text(text: &str) -> bool {
    text.chars().any(is_japanese_text_char)
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

fn translated_parenthetical_gloss(translated: &str) -> Option<String> {
    let mut iter = translated.char_indices().peekable();
    while let Some((open_idx, open)) = iter.next() {
        if !matches!(open, '(' | '（') || !has_preceding_thai(translated, open_idx) {
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
            if ch == '\n' || idx.saturating_sub(content_start) > 48 {
                break;
            }
        }
        let Some(end) = content_end else {
            continue;
        };
        let content = translated[content_start..end].trim();
        if is_latin_original_parenthetical(content) || is_thai_phonetic_parenthetical(content) {
            return Some(format!("{open}{content}{close}"));
        }
    }
    None
}

fn has_preceding_thai(text: &str, open_idx: usize) -> bool {
    text[..open_idx]
        .chars()
        .rev()
        .find(|ch| !ch.is_whitespace())
        .is_some_and(is_thai_char)
}

fn is_latin_original_parenthetical(content: &str) -> bool {
    let mut has_ascii_alpha = false;
    for ch in content.chars() {
        if ch.is_ascii_alphabetic() {
            has_ascii_alpha = true;
            continue;
        }
        if ch.is_ascii_digit() || matches!(ch, ' ' | '-' | '\'' | '&' | '.' | '/') {
            continue;
        }
        return false;
    }
    has_ascii_alpha && content.chars().count() <= 64
}

fn is_thai_phonetic_parenthetical(content: &str) -> bool {
    let mut thai = 0usize;
    for ch in content.chars() {
        if ch.is_whitespace() {
            return false;
        }
        if is_thai_char(ch) || matches!(ch, 'ๆ' | 'ฯ' | '์') {
            thai += 1;
            continue;
        }
        return false;
    }
    (2..=24).contains(&thai)
        && !looks_like_thai_meaning_explanation(content)
        && looks_like_transliterated_japanese(content)
}

fn looks_like_thai_meaning_explanation(content: &str) -> bool {
    [
        "รัก",
        "หลง",
        "ชอบ",
        "น้อง",
        "พี่",
        "สาว",
        "ชาย",
        "เด็ก",
        "คน",
        "ผู้",
        "ความ",
        "การ",
        "แบบ",
        "อย่าง",
        "ของ",
        "ที่",
        "ไม่",
    ]
    .iter()
    .any(|word| content.contains(word))
}

fn looks_like_transliterated_japanese(content: &str) -> bool {
    [
        "ฮิ",
        "ฮิโ",
        "โต",
        "เมะ",
        "โบ",
        "เระ",
        "โอส",
        "เซ",
        "โซ",
        "คาวา",
        "อิ",
        "ซึ",
        "เดเระ",
        "โมริ",
        "มาบุ",
        "ดาจิ",
    ]
    .iter()
    .any(|part| content.contains(part))
}

fn is_thai_char(ch: char) -> bool {
    matches!(ch as u32, 0x0E00..=0x0E7F)
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

    fn character_with_voice(id: &str, style: &str) -> Character {
        character_with_named_voice(id, "上原祐", "อุเอฮาระ ทาสุคุ", style)
    }

    fn character_with_named_voice(
        id: &str,
        jp_name: &str,
        thai_name: &str,
        style: &str,
    ) -> Character {
        Character {
            id: id.to_string(),
            jp_name: jp_name.to_string(),
            thai_name: thai_name.to_string(),
            romaji: None,
            gender: Some("male".to_string()),
            honorific: None,
            speech_style: Some(style.to_string()),
            relationships: Vec::new(),
            aliases: if jp_name == "上原祐" {
                vec!["上原".to_string()]
            } else {
                Vec::new()
            },
            also_called: Vec::new(),
            notes: None,
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
    fn audit_flags_thai_pronunciation_parenthetical_glosses() {
        let source = "「押忍！」";
        let thai = "“รับทราบ (โอส)!”";

        let findings = audit_translation_with_terms(source, thai, &[], &[]);

        assert!(
            findings
                .iter()
                .any(|f| f.contains("reading/original gloss")),
            "Thai pronunciation gloss flagged: {findings:?}"
        );
    }

    #[test]
    fn audit_flags_long_thai_transliteration_parenthetical_glosses() {
        let source = "一目惚れ？";
        let thai = "หรือว่าจะเป็นรักแรกพบ (ฮิโตเมะโบเระ)?";

        let findings = audit_translation_with_terms(source, thai, &[], &[]);

        assert!(
            findings
                .iter()
                .any(|f| f.contains("reading/original gloss")),
            "long Thai transliteration gloss flagged: {findings:?}"
        );
    }

    #[test]
    fn audit_flags_latin_original_parenthetical_glosses() {
        let source = "Perfectだと思った。";
        let thai = "พี่นึกว่าเป็นคำตอบที่เพอร์เฟกต์ (Perfect) แล้ว";

        let findings = audit_translation_with_terms(source, thai, &[], &[]);

        assert!(
            findings
                .iter()
                .any(|f| f.contains("reading/original gloss")),
            "Latin original gloss flagged: {findings:?}"
        );
    }

    #[test]
    fn audit_allows_longer_thai_parenthetical_as_story_text() {
        let source = "彼女は小さく（本当に小さく）頷いた。";
        let thai = "เธอพยักหน้าเบา ๆ (เบาจริง ๆ) อย่างลังเล";

        let findings = audit_translation_with_terms(source, thai, &[], &[]);

        assert!(
            !findings
                .iter()
                .any(|f| f.contains("reading/original gloss")),
            "longer Thai parenthetical should not be treated as a pronunciation gloss: {findings:?}"
        );
    }

    #[test]
    fn audit_allows_thai_meaning_parenthetical_for_loanword() {
        let source = "シスコンなの？";
        let thai = "นายเข้าข่ายพวกซิสคอน (รักน้องสาวหลงน้องสาว) หรือเปล่า?";

        let findings = audit_translation_with_terms(source, thai, &[], &[]);

        assert!(
            !findings
                .iter()
                .any(|f| f.contains("reading/original gloss")),
            "Thai meaning explanation should be allowed: {findings:?}"
        );
    }

    #[test]
    fn normalize_rewrites_japanese_punctuation_residue() {
        let raw = "เธอพึมพำว่า「ใช่（เบามาก）！」";

        let normalized = normalize_japanese_punctuation_residue(raw);

        assert_eq!(normalized, "เธอพึมพำว่า\"ใช่(เบามาก)!\"");
        assert!(
            audit_translation_with_terms("彼女は小さく頷いた。", &normalized, &[], &[])
                .iter()
                .all(|f| !f.contains("Japanese punctuation")),
            "normalization should remove mechanical Japanese punctuation residue"
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
    fn advisory_flags_discouraged_casual_particles() {
        let findings = advisory_findings("何だよ。", "นี่มันอะไรวะ!");
        assert!(
            findings
                .iter()
                .any(|f| f.contains("casual Thai particle") && f.contains("`วะ`")),
            "sentence-final วะ flagged: {findings:?}"
        );

        let findings = advisory_findings("何だって。", "นี่มันอะไรว่ะ!");
        assert!(
            findings
                .iter()
                .any(|f| f.contains("casual Thai particle") && f.contains("`ว่ะ`")),
            "sentence-final ว่ะ flagged: {findings:?}"
        );
    }

    #[test]
    fn advisory_allows_preferred_particles_fuwa_name_and_tho_woei() {
        let source = "不破さんは叫んだ。";
        let thai = "คุณฟูวะตะโกนว่า “นี่มันอะไรกันฟะ เฟ้ย! โธ่เว้ย!”";
        let findings = advisory_findings(source, thai);
        assert!(
            !findings.iter().any(|f| f.contains("casual Thai particle")),
            "preferred particles, Fuwa name, and โธ่เว้ย are not flagged: {findings:?}"
        );
    }

    #[test]
    fn advisory_flags_ore_narration_rendered_with_chan() {
        let source = "俺は、あの二人を見てそう思わずにはいられなかった。";
        let thai = "ฉันกลับอดคิดแบบนี้ไม่ได้เมื่อมองสองคนนั้น";
        let findings = advisory_findings(source, thai);
        assert!(
            findings
                .iter()
                .any(|f| f.contains("`俺`") && f.contains("`ฉัน`")),
            "`俺` rendered with ฉัน should be surfaced for POV review: {findings:?}"
        );
    }

    #[test]
    fn advisory_ignores_chan_inside_non_ore_dialogue() {
        let source = "“……えとえと、それに、あの様子では、たとえ本人達に直接訊ねたところで、普通に否定するだけだったと思います……”\n\nだが、しかし。今の俺達三人が知りたいのは……そういうことでは、ないわけで。\n\n“ところで、上原君と亜玖璃さんがお付き合いなさっているというのは私も把握しておりますので”";
        let thai = "“……เอ่อ คือว่า แล้วก็ ดูจากท่าทางแบบนั้นแล้ว ต่อให้ไปถามเจ้าตัวโดยตรง ฉันคิดว่าพวกเขาก็คงแค่ปฏิเสธตามปกติค่ะ……”\n\nแต่ว่า สิ่งที่พวกผมทั้งสามคนในตอนนี้อยากรู้……มันไม่ใช่เรื่องแบบนั้นนี่นา\n\n“จะว่าไป เรื่องที่อุเอฮาระคุงกับอากุริคบกันอยู่ฉันเองก็รับทราบแล้ว”";

        let findings = advisory_findings(source, thai);

        assert!(
            !findings
                .iter()
                .any(|f| f.contains("`俺`") && f.contains("`ฉัน`")),
            "female/polite dialogue `ฉัน` should not be treated as the `俺` narrator: {findings:?}"
        );
    }

    #[test]
    fn audit_character_pronoun_rule_rejects_chan_against_explicit_roster_voice() {
        let character = character_with_voice(
            "uehara",
            "ผู้บรรยาย/ตัวละครชายวัยรุ่น ใช้สรรพนาม 俺 (Ore); ในไทยให้ใช้ 'ผม' เป็นสรรพนามบุรุษที่ 1 ทั้งในบทบรรยาย/พูดกับตัวเอง น้ำเสียงจริงจังและเป็นผู้ชาย ไม่ใช้ 'ฉัน'",
        );
        let source = "俺は大きくため息をついた。";
        let thai = "ฉันถอนหายใจเฮือกใหญ่";

        let findings = audit_character_pronoun_rules(source, thai, None, &[character]);

        assert!(
            findings
                .iter()
                .any(|f| f.contains("อุเอฮาระ ทาสุคุ") && f.contains("ผม") && f.contains("ฉัน")),
            "explicit roster voice should make the pronoun mismatch a hard audit finding: {findings:?}"
        );
    }

    #[test]
    fn audit_character_pronoun_rule_ignores_chan_inside_non_ore_dialogue() {
        let character = character_with_voice(
            "uehara",
            "ผู้บรรยายชาย ใช้สรรพนาม 俺 (Ore); ในไทยให้ใช้ 'ผม' ไม่ใช้ 'ฉัน'",
        );
        let source = "“……えとえと、それに、あの様子では、たとえ本人達に直接訊ねたところで、普通に否定するだけだったと思います……”\n\nだが、しかし。今の俺達三人が知りたいのは……そういうことでは、ないわけで。\n\n“ところで、上原君と亜玖璃さんがお付き合いなさっているというのは私も把握しておりますので”";
        let thai = "“……เอ่อ คือว่า แล้วก็ ดูจากท่าทางแบบนั้นแล้ว ต่อให้ไปถามเจ้าตัวโดยตรง ฉันคิดว่าพวกเขาก็คงแค่ปฏิเสธตามปกติค่ะ……”\n\nแต่ว่า สิ่งที่พวกผมทั้งสามคนในตอนนี้อยากรู้……มันไม่ใช่เรื่องแบบนั้นนี่นา\n\n“จะว่าไป เรื่องที่อุเอฮาระคุงกับอากุริคบกันอยู่ฉันเองก็รับทราบแล้ว”";

        let findings = audit_character_pronoun_rules(source, thai, None, &[character]);

        assert!(
            findings.is_empty(),
            "hard audit must not attach female/polite dialogue `ฉัน` to Uehara's narration: {findings:?}"
        );
    }

    #[test]
    fn audit_character_pronoun_rule_rejects_chan_in_ore_dialogue() {
        let character = character_with_voice(
            "uehara",
            "ผู้บรรยายชาย ใช้สรรพนาม 俺 (Ore); ในไทยให้ใช้ 'ผม' ไม่ใช้ 'ฉัน'",
        );

        let findings = audit_character_pronoun_rules(
            "“んなの、俺じゃなく当人達に──”",
            "“เรื่องแบบนั้น แทนที่จะมาถามฉัน ไปถามเจ้าตัวเขาเอง—”",
            None,
            &[character],
        );

        assert!(
            findings.iter().any(|f| f.contains("ผม")),
            "Uehara's own `俺` dialogue rendered as `ฉัน` should stay a hard audit finding: {findings:?}"
        );
    }

    #[test]
    fn audit_character_pronoun_rule_allows_explicit_chan_voice() {
        let character = character_with_voice(
            "male_chan",
            "ตัวละครชาย ใช้สรรพนาม 俺 ในญี่ปุ่น แต่ฉบับไทยให้ใช้ 'ฉัน' ตามเสียงประจำตัว",
        );

        let findings =
            audit_character_pronoun_rules("俺は首をかしげた。", "ฉันเอียงคอ", None, &[character]);

        assert!(
            findings.is_empty(),
            "hard audit requires an explicit not-ฉัน roster rule: {findings:?}"
        );
    }

    #[test]
    fn audit_character_pronoun_rule_ignores_non_ore_source() {
        let character = character_with_voice(
            "uehara",
            "ผู้บรรยายชาย ใช้สรรพนาม 俺 (Ore); ในไทยให้ใช้ 'ผม' ไม่ใช้ 'ฉัน'",
        );

        let findings =
            audit_character_pronoun_rules("彼は首をかしげた。", "ฉันเอียงคอ", None, &[character]);

        assert!(
            findings.is_empty(),
            "without source `俺`, this roster rule should not infer a first-person mismatch: {findings:?}"
        );
    }

    #[test]
    fn audit_character_pronoun_rule_uses_anchor_when_multiple_roster_rules_match() {
        let uehara = character_with_voice(
            "uehara",
            "ผู้บรรยายชาย ใช้สรรพนาม 俺 (Ore); ในไทยให้ใช้ 'ผม' ไม่ใช้ 'ฉัน'",
        );
        let other = character_with_named_voice(
            "other",
            "佐藤一郎",
            "ซาโต้ อิจิโร่",
            "ตัวละครชาย ใช้สรรพนาม 俺; ในไทยให้ใช้ 'ผม' ไม่ใช้ 'ฉัน'",
        );

        let findings = audit_character_pronoun_rules(
            "佐藤一郎は言った。\n“俺は帰る。”",
            "ซาโต้ อิจิโร่พูดขึ้น\n“ฉันจะกลับ”",
            None,
            &[uehara, other],
        );

        assert!(
            findings
                .iter()
                .any(|f| f.contains("ซาโต้ อิจิโร่") && !f.contains("อุเอฮาระ ทาสุคุ")),
            "multi-character hard audit should anchor to the mentioned/current POV character: {findings:?}"
        );
    }

    #[test]
    fn audit_character_pronoun_rule_skips_unanchored_multiple_matches() {
        let uehara = character_with_voice(
            "uehara",
            "ผู้บรรยายชาย ใช้สรรพนาม 俺 (Ore); ในไทยให้ใช้ 'ผม' ไม่ใช้ 'ฉัน'",
        );
        let other = character_with_named_voice(
            "other",
            "佐藤一郎",
            "ซาโต้ อิจิโร่",
            "ตัวละครชาย ใช้สรรพนาม 俺; ในไทยให้ใช้ 'ผม' ไม่ใช้ 'ฉัน'",
        );

        let findings = audit_character_pronoun_rules(
            "俺は帰る。",
            "ฉันจะกลับ",
            Some("POV: บุคคลไม่ชัดเจน"),
            &[uehara, other],
        );

        assert!(
            findings.is_empty(),
            "multiple possible roster rules without an anchor should stay out of hard audit: {findings:?}"
        );
    }

    #[test]
    fn audit_character_pronoun_rule_uses_current_pov_anchor() {
        let uehara = character_with_voice(
            "uehara",
            "ผู้บรรยายชาย ใช้สรรพนาม 俺 (Ore); ในไทยให้ใช้ 'ผม' ไม่ใช้ 'ฉัน'",
        );
        let other = character_with_named_voice(
            "other",
            "佐藤一郎",
            "ซาโต้ อิจิโร่",
            "ตัวละครชาย ใช้สรรพนาม 俺; ในไทยให้ใช้ 'ผม' ไม่ใช้ 'ฉัน'",
        );

        let findings = audit_character_pronoun_rules(
            "俺は大きくため息をついた。",
            "ฉันถอนหายใจเฮือกใหญ่",
            Some("อุเอฮาระ ทาสุคุ / self-pronoun: ผม"),
            &[uehara, other],
        );

        assert!(
            findings
                .iter()
                .any(|f| f.contains("อุเอฮาระ ทาสุคุ") && !f.contains("ซาโต้ อิจิโร่")),
            "current POV should anchor an otherwise surface-free `俺` narration: {findings:?}"
        );
    }

    #[test]
    fn advisory_flags_reciprocal_bond_rendered_as_gift() {
        let source = "あの二人が互いに向け合う以上の絆を、俺はそこに感じた。";
        let thai = "ผมรู้สึกถึงสายสัมพันธ์ที่สองคนนั้นมอบให้กันอยู่ตรงนั้น";
        let findings = advisory_findings(source, thai);
        assert!(
            findings
                .iter()
                .any(|f| f.contains("reciprocal bond") && f.contains("มอบให้")),
            "reciprocal bond wording should not silently become a gift-like action: {findings:?}"
        );
    }

    #[test]
    fn advisory_flags_japanese_punctuation_residue() {
        let source = "彼女は笑った。";
        let thai = "เธอยิ้ม。";
        let hard = audit_translation_with_terms(source, thai, &[], &[]);
        assert!(
            hard.iter().any(|f| f.contains("Japanese punctuation")),
            "Japanese punctuation residue is a hard mechanical gate: {hard:?}"
        );

        let findings = advisory_findings(source, thai);
        assert!(
            findings
                .iter()
                .any(|f| f.contains("Japanese punctuation") && f.contains("。")),
            "Japanese punctuation residue is surfaced to the reviewer: {findings:?}"
        );
    }

    #[test]
    fn advisory_flags_mapped_character_surface_drift() {
        let character = Character {
            id: "amano".to_string(),
            jp_name: "雨野景太".to_string(),
            thai_name: "อามาโนะ เคย์ตะ".to_string(),
            romaji: Some("Amano Keita".to_string()),
            gender: Some("male".to_string()),
            honorific: None,
            speech_style: None,
            relationships: Vec::new(),
            aliases: vec!["ケータ".to_string()],
            also_called: vec![crate::model::AltName {
                jp: "雨野君".to_string(),
                thai: "อามาโนะคุง".to_string(),
                by: None,
            }],
            notes: None,
            first_seen_chapter: None,
        };

        let hard = audit_translation_with_terms("雨野君は慌てた。", "คุณอามาโนะลนลาน", &[], &[]);
        assert!(
            !hard.iter().any(|f| f.contains("character/name surface")),
            "character names must not be hard local-audit gates: {hard:?}"
        );

        let findings =
            advisory_findings_with_references("雨野君は慌てた。", "คุณอามาโนะลนลาน", &[character]);

        assert!(
            findings
                .iter()
                .any(|f| f.contains("雨野君") && f.contains("อามาโนะคุง")),
            "mapped alt-name rendering drift should be flagged: {findings:?}"
        );
    }

    #[test]
    fn audit_does_not_retry_on_alias_inside_honorific() {
        let character = Character {
            id: "amano".to_string(),
            jp_name: "雨野景太".to_string(),
            thai_name: "อามาโนะ เคย์ตะ".to_string(),
            romaji: Some("Amano Keita".to_string()),
            gender: Some("male".to_string()),
            honorific: None,
            speech_style: None,
            relationships: Vec::new(),
            aliases: vec!["雨野".to_string()],
            also_called: Vec::new(),
            notes: None,
            first_seen_chapter: None,
        };

        let source = "失礼します。雨野君はいらっしゃいますか？";
        let thai = "ขออนุญาตค่ะ อามาโนะคุงอยู่ไหมคะ?";

        let hard = audit_translation_with_terms(source, thai, &[], &[]);
        assert!(
            !hard.iter().any(|f| f.contains("character/name surface")),
            "short aliases are context hints, not exact hard-gate surfaces: {hard:?}"
        );

        let advisory = advisory_findings_with_references(source, thai, &[character]);
        assert!(
            !advisory.iter().any(|f| f.contains("雨野")),
            "alias inside an honorific should not demand the canonical full name: {advisory:?}"
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
