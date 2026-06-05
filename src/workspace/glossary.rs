//! Read/upsert/render GLOSSARY.md. Data block is `{"terms": [GlossaryTerm,...]}`,
//! keyed on the normalized (trimmed) `jp_term`. `render_context_blurb` builds the
//! locked-terms list injected into the Translator prompt to force canonical Thai.

use serde::{Deserialize, Serialize};

use crate::model::GlossaryTerm;
use crate::workspace::Workspace;
use crate::workspace::data_block;

/// On-disk JSON payload wrapper for GLOSSARY.md.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct GlossaryBlock {
    #[serde(default)]
    terms: Vec<GlossaryTerm>,
}

/// Load all glossary terms (empty vec when absent/empty).
pub fn load(ws: &Workspace) -> Vec<GlossaryTerm> {
    let block: GlossaryBlock = data_block::read_data_block(&ws.glossary_md());
    block.terms
}

/// Insert or merge a term (matched on normalized `jp_term`) and re-render; on a
/// match, non-empty incoming fields overwrite, missing fields are preserved.
pub fn upsert(ws: &Workspace, t: GlossaryTerm) -> std::io::Result<()> {
    let key = normalize(&t.jp_term);
    let mut terms = load(ws);

    if let Some(existing) = terms.iter_mut().find(|e| normalize(&e.jp_term) == key) {
        merge_into(existing, t);
    } else {
        terms.push(t);
    }

    // Stable order: category then jp_term.
    terms.sort_by(|a, b| {
        let ca = a.category.as_deref().unwrap_or("");
        let cb = b.category.as_deref().unwrap_or("");
        ca.cmp(cb).then_with(|| a.jp_term.cmp(&b.jp_term))
    });

    let body = render_table(&terms);
    let block = GlossaryBlock { terms };
    data_block::write_with_data(&ws.glossary_md(), &body, &block)
}

/// Remove the term whose normalized `jp_term` matches. No-op if absent.
pub fn remove(ws: &Workspace, jp_term: &str) -> std::io::Result<()> {
    let key = normalize(jp_term);
    let mut terms = load(ws);
    let before = terms.len();
    terms.retain(|t| normalize(&t.jp_term) != key);
    if terms.len() == before {
        return Ok(());
    }
    let body = render_table(&terms);
    let block = GlossaryBlock { terms };
    data_block::write_with_data(&ws.glossary_md(), &body, &block)
}

/// Query terms by case-insensitive substring `query` (jp_term/thai_term/romaji/
/// gloss) and optional exact `category`, capped at `limit` (0 means no cap).
pub fn get(
    ws: &Workspace,
    query: Option<&str>,
    category: Option<&str>,
    limit: usize,
) -> Vec<GlossaryTerm> {
    let terms = load(ws);
    let q = query
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty());
    let cat = category
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty());

    let mut out: Vec<GlossaryTerm> = terms
        .into_iter()
        .filter(|t| match &cat {
            Some(want) => t
                .category
                .as_deref()
                .map(|c| c.trim().to_lowercase())
                .map(|c| c == *want)
                .unwrap_or(false),
            None => true,
        })
        .filter(|t| match &q {
            Some(needle) => term_matches(t, needle),
            None => true,
        })
        .collect();

    if limit > 0 && out.len() > limit {
        out.truncate(limit);
    }
    out
}

/// Render the human-readable Markdown body for GLOSSARY.md.
pub fn render_table(terms: &[GlossaryTerm]) -> String {
    let mut s = String::new();
    s.push_str("# คลังคำศัพท์ / Glossary\n\n");

    if terms.is_empty() {
        s.push_str("_ยังไม่มีคำศัพท์ที่บันทึกไว้ — เพิ่มได้ผ่านเครื่องมือของ Orchestrator หรือหน้าจอ Lexicon_\n");
        return s;
    }

    s.push_str("| 日本語 | ไทย | Romaji | หมวด | ห้ามแปล | บทแรก | คำอธิบาย |\n");
    s.push_str("|--------|-----|--------|------|---------|-------|----------|\n");
    for t in terms {
        let dnt = match t.do_not_translate {
            Some(true) => "✓",
            _ => "—",
        };
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            cell(&t.jp_term),
            cell(&t.thai_term),
            opt(&t.romaji),
            opt(&t.category),
            dnt,
            t.first_seen_chapter
                .map(|n| n.to_string())
                .unwrap_or_else(|| "—".to_string()),
            opt(&t.gloss),
        ));
    }
    s
}

/// Render the locked-terms blurb for the Translator prompt: one `日本語 → ไทย`
/// bullet per term (romaji hint, `[หมวด]` tag, `[ห้ามแปล]` flag); empty list → "".
pub fn render_context_blurb(terms: &[GlossaryTerm]) -> String {
    if terms.is_empty() {
        return String::new();
    }
    let mut s = String::new();
    s.push_str("คำศัพท์ที่กำหนดไว้ (บังคับใช้ให้ตรงกัน):\n");
    for t in terms {
        let jp = t.jp_term.trim();
        let th = t.thai_term.trim();
        if jp.is_empty() {
            continue;
        }
        s.push_str("- ");
        s.push_str(jp);
        if let Some(romaji) = t.romaji.as_deref().filter(|r| !r.trim().is_empty()) {
            s.push_str(&format!(" ({})", romaji.trim()));
        }
        s.push_str(" → ");
        s.push_str(if th.is_empty() { "—" } else { th });
        if let Some(cat) = t.category.as_deref().filter(|c| !c.trim().is_empty()) {
            s.push_str(&format!(" [{}]", cat.trim()));
        }
        if matches!(t.do_not_translate, Some(true)) {
            s.push_str(" [ห้ามแปล/คงคำเดิม]");
        }
        s.push('\n');
    }
    s
}

fn merge_into(target: &mut GlossaryTerm, incoming: GlossaryTerm) {
    if !incoming.jp_term.trim().is_empty() {
        target.jp_term = incoming.jp_term;
    }
    if !incoming.thai_term.trim().is_empty() {
        target.thai_term = incoming.thai_term;
    }
    merge_opt(&mut target.romaji, incoming.romaji);
    merge_opt(&mut target.category, incoming.category);
    merge_opt(&mut target.gloss, incoming.gloss);
    if incoming.do_not_translate.is_some() {
        target.do_not_translate = incoming.do_not_translate;
    }
    if incoming.first_seen_chapter.is_some() {
        target.first_seen_chapter = match (target.first_seen_chapter, incoming.first_seen_chapter) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (None, b) => b,
            (a, None) => a,
        };
    }
}

fn merge_opt(slot: &mut Option<String>, incoming: Option<String>) {
    if let Some(v) = incoming
        && !v.trim().is_empty() {
            *slot = Some(v);
        }
}

/// Normalize a jp_term for keying: trim whitespace.
fn normalize(s: &str) -> String {
    s.trim().to_string()
}

fn term_matches(t: &GlossaryTerm, needle: &str) -> bool {
    let hay = [
        t.jp_term.to_lowercase(),
        t.thai_term.to_lowercase(),
        t.romaji.as_deref().unwrap_or("").to_lowercase(),
        t.gloss.as_deref().unwrap_or("").to_lowercase(),
    ];
    hay.iter().any(|h| h.contains(needle))
}

fn cell(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return "—".to_string();
    }
    trimmed.replace('|', "\\|").replace('\n', " ")
}

fn opt(s: &Option<String>) -> String {
    match s {
        Some(v) if !v.trim().is_empty() => cell(v),
        _ => "—".to_string(),
    }
}
