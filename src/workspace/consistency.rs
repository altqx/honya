//! Read-only project-wide roster consistency checks for QA.

use std::collections::BTreeMap;

use crate::model::{Character, GlossaryTerm};

/// Advisory project-wide finding surfaced in the QA inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsistencyIssue {
    pub detail: String,
}

fn norm(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Distinct, non-empty, trimmed Thai forms in first-seen order.
fn distinct(forms: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in forms {
        let f = f.trim().to_string();
        if !f.is_empty() && !out.contains(&f) {
            out.push(f);
        }
    }
    out
}

/// Audit the shared roster for cross-volume Thai rendering drift.
pub fn roster_consistency(chars: &[Character], terms: &[GlossaryTerm]) -> Vec<ConsistencyIssue> {
    let mut issues = Vec::new();

    let mut by_reading: BTreeMap<String, Vec<&Character>> = BTreeMap::new();
    for c in chars {
        if let Some(r) = c.romaji.as_deref().map(norm).filter(|r| !r.is_empty()) {
            by_reading.entry(r).or_default().push(c);
        }
    }
    for (reading, group) in &by_reading {
        let forms = distinct(group.iter().map(|c| c.thai_name.clone()));
        if forms.len() > 1 {
            let names: Vec<&str> = group.iter().map(|c| c.jp_name.trim()).collect();
            issues.push(ConsistencyIssue {
                detail: format!(
                    "characters sharing reading \"{reading}\" ({}) render as different Thai names: {} — merge them or disambiguate",
                    names.join(", "),
                    forms.join(" / "),
                ),
            });
        }
    }

    let mut terms_by_reading: BTreeMap<String, Vec<&GlossaryTerm>> = BTreeMap::new();
    for t in terms {
        if let Some(r) = t.romaji.as_deref().map(norm).filter(|r| !r.is_empty()) {
            terms_by_reading.entry(r).or_default().push(t);
        }
    }
    for (reading, group) in &terms_by_reading {
        let forms = distinct(group.iter().map(|t| t.thai_term.clone()));
        if forms.len() > 1 {
            let jps: Vec<&str> = group.iter().map(|t| t.jp_term.trim()).collect();
            issues.push(ConsistencyIssue {
                detail: format!(
                    "glossary terms sharing reading \"{reading}\" ({}) map to different Thai: {} — pin one rendering",
                    jps.join(", "),
                    forms.join(" / "),
                ),
            });
        }
    }

    let term_by_jp: BTreeMap<String, &GlossaryTerm> =
        terms.iter().map(|t| (norm(&t.jp_term), t)).collect();
    for c in chars {
        let key = norm(&c.jp_name);
        if key.is_empty() || c.thai_name.trim().is_empty() {
            continue;
        }
        if let Some(t) = term_by_jp.get(&key)
            && !t.thai_term.trim().is_empty()
            && t.thai_term.trim() != c.thai_name.trim()
        {
            issues.push(ConsistencyIssue {
                detail: format!(
                    "\"{}\" is a character (→ {}) and a glossary term (→ {}) with different Thai — align them",
                    c.jp_name.trim(),
                    c.thai_name.trim(),
                    t.thai_term.trim(),
                ),
            });
        }
    }

    issues
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(jp: &str, th: &str, romaji: &str) -> Character {
        Character {
            id: jp.to_string(),
            jp_name: jp.to_string(),
            thai_name: th.to_string(),
            romaji: Some(romaji.to_string()),
            gender: None,
            honorific: None,
            speech_style: None,
            relationships: Vec::new(),
            aliases: Vec::new(),
            also_called: Vec::new(),
            notes: None,
            first_seen_chapter: None,
        }
    }

    fn term(jp: &str, th: &str, romaji: &str) -> GlossaryTerm {
        GlossaryTerm {
            jp_term: jp.to_string(),
            thai_term: th.to_string(),
            romaji: Some(romaji.to_string()),
            category: None,
            gloss: None,
            policy: None,
            forbidden_thai: Vec::new(),
            context_rule: None,
            protected: None,
            do_not_translate: None,
            first_seen_chapter: None,
        }
    }

    #[test]
    fn flags_characters_with_same_reading_different_thai() {
        let chars = vec![
            ch("朱夏", "ชูกะ", "Shuka"),
            ch("シュカ", "ชูคา", "shuka"),
        ];
        let issues = roster_consistency(&chars, &[]);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].detail.contains("ชูกะ"));
        assert!(issues[0].detail.contains("ชูคา"));
    }

    #[test]
    fn consistent_roster_has_no_issues() {
        let chars = vec![ch("朱夏", "ชูกะ", "Shuka"), ch("シュカ", "ชูกะ", "shuka")];
        assert!(roster_consistency(&chars, &[]).is_empty());
    }

    #[test]
    fn flags_term_reading_divergence() {
        let terms = vec![term("聖剣", "ดาบเทพ", "seiken"), term("正剣", "ดาบแท้", "seiken")];
        let issues = roster_consistency(&[], &terms);
        assert!(issues.iter().any(|i| i.detail.contains("map to different Thai")));
    }

    #[test]
    fn flags_cross_roster_name_term_mismatch() {
        let chars = vec![ch("聖剣", "ดาบศักดิ์สิทธิ์", "Seiken")];
        let terms = vec![term("聖剣", "ดาบเทพ", "seiken")];
        let issues = roster_consistency(&chars, &terms);
        assert!(issues.iter().any(|i| i.detail.contains("character")));
    }
}
