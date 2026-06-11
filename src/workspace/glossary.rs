//! Read/upsert/render GLOSSARY.md. Data block is `{"terms": [GlossaryTerm,...]}`,
//! keyed on the normalized (trimmed) `jp_term`. `render_context_blurb` builds the
//! terminology-control list injected into the Translator prompt.

use serde::{Deserialize, Serialize};

use crate::model::{GlossaryTerm, TermPolicy};
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

/// Result of a glossary upsert that may be blocked by a controlled terminology rule.
#[derive(Debug, Clone)]
pub enum GlossaryUpsertOutcome {
    Inserted,
    Updated,
    /// The matching existing term is controlled/protected, so an automatic
    /// Orchestrator write was skipped. `conflict` means the attempted rendering
    /// differed from, or would violate, the saved terminology policy.
    Protected {
        existing: GlossaryTerm,
        conflict: bool,
    },
}

/// Insert or merge a term (matched on normalized `jp_term`) and re-render; on a
/// match, non-empty incoming fields overwrite, missing fields are preserved.
/// This is the trusted/manual path used by the Lexicon and tests; it may edit or
/// clear a protected flag.
pub fn upsert(ws: &Workspace, t: GlossaryTerm) -> std::io::Result<()> {
    upsert_inner(ws, t, false).map(|_| ())
}

/// Orchestrator-safe upsert: protected existing terms are terminology locks and
/// are never overwritten by automatic metadata turns.
pub fn upsert_from_orchestrator(
    ws: &Workspace,
    t: GlossaryTerm,
) -> std::io::Result<GlossaryUpsertOutcome> {
    upsert_inner(ws, t, true)
}

fn upsert_inner(
    ws: &Workspace,
    mut t: GlossaryTerm,
    respect_protection: bool,
) -> std::io::Result<GlossaryUpsertOutcome> {
    normalize_term_controls(&mut t);
    let key = normalize(&t.jp_term);
    let mut terms = load(ws);
    let outcome;

    if let Some(existing) = terms.iter_mut().find(|e| normalize(&e.jp_term) == key) {
        if respect_protection && blocks_automatic_update(existing) {
            let conflict = controlled_conflict(existing, &t);
            return Ok(GlossaryUpsertOutcome::Protected {
                existing: existing.clone(),
                conflict,
            });
        }
        merge_into(existing, t);
        outcome = GlossaryUpsertOutcome::Updated;
    } else {
        terms.push(t);
        outcome = GlossaryUpsertOutcome::Inserted;
    }

    // Stable order: category then jp_term.
    terms.sort_by(|a, b| {
        let ca = a.category.as_deref().unwrap_or("");
        let cb = b.category.as_deref().unwrap_or("");
        ca.cmp(cb).then_with(|| a.jp_term.cmp(&b.jp_term))
    });

    let body = render_table(&terms);
    let block = GlossaryBlock { terms };
    data_block::write_with_data(&ws.glossary_md(), &body, &block)?;
    Ok(outcome)
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
/// gloss/policy) and optional exact `category` / `policy` / protected-only flag,
/// capped at `limit` (0 means no cap).
pub fn get(
    ws: &Workspace,
    query: Option<&str>,
    category: Option<&str>,
    policy: Option<TermPolicy>,
    protected_only: bool,
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
        .filter(|t| !protected_only || blocks_automatic_update(t))
        .filter(|t| policy.is_none_or(|want| effective_policy(t) == want))
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

    s.push_str(
        "| 日本語 | ไทย | Romaji | หมวด | นโยบาย | ห้ามแปล | ห้ามใช้ | บทแรก | บริบท/คำอธิบาย |\n",
    );
    s.push_str(
        "|--------|-----|--------|------|--------|---------|---------|-------|----------------|\n",
    );
    for t in terms {
        let dnt = match t.do_not_translate {
            Some(true) => "✓",
            _ => "—",
        };
        let forbidden = forbidden_renderings(t).join(", ");
        let notes = [t.context_rule.as_deref(), t.gloss.as_deref()]
            .into_iter()
            .flatten()
            .filter(|v| !v.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            cell(&t.jp_term),
            cell(&t.thai_term),
            opt(&t.romaji),
            opt(&t.category),
            cell(policy_label(effective_policy(t))),
            dnt,
            cell(&forbidden),
            t.first_seen_chapter
                .map(|n| n.to_string())
                .unwrap_or_else(|| "—".to_string()),
            cell(&notes),
        ));
    }
    s
}

/// Render the terminology-control blurb for the Translator prompt: one bullet per
/// term with explicit policy semantics (hard lock / preferred / forbidden /
/// context-dependent); empty list → "".
pub fn render_context_blurb(terms: &[GlossaryTerm]) -> String {
    if terms.is_empty() {
        return String::new();
    }
    let mut s = String::new();
    s.push_str("คำศัพท์ที่กำหนดไว้ (อ่านนโยบายทุกข้อก่อนแปล):\n");
    s.push_str("- HARD_LOCKED: ต้องใช้คำที่กำหนดแบบตรงตัว ห้ามเปลี่ยนเอง\n");
    s.push_str("- PREFERRED: ควรใช้คำที่กำหนดเป็นค่าเริ่มต้น แต่ปรับได้เมื่อไวยากรณ์/บริบทจำเป็น\n");
    s.push_str("- FORBIDDEN: ห้ามใช้คำแปลที่ระบุไว้กับศัพท์ญี่ปุ่นนี้\n");
    s.push_str("- CONTEXT_DEPENDENT: เลือกคำตามกฎบริบทที่ระบุ\n");
    for t in terms {
        let jp = t.jp_term.trim();
        let th = t.thai_term.trim();
        if jp.is_empty() {
            continue;
        }
        let policy = effective_policy(t);
        s.push_str("- ");
        s.push_str(policy_prompt_label(policy));
        s.push_str(" | ");
        s.push_str(jp);
        if let Some(romaji) = t.romaji.as_deref().filter(|r| !r.trim().is_empty()) {
            s.push_str(&format!(" ({})", romaji.trim()));
        }
        match policy {
            TermPolicy::Forbidden => {
                let forbidden = forbidden_renderings(t);
                if forbidden.is_empty() {
                    s.push_str(" | ห้ามใช้คำแปลที่ระบุในหมายเหตุ");
                } else {
                    s.push_str(" | ห้ามใช้: ");
                    s.push_str(&forbidden.join(", "));
                }
            }
            _ => {
                s.push_str(" → ");
                s.push_str(if th.is_empty() { "—" } else { th });
            }
        }
        if let Some(cat) = t.category.as_deref().filter(|c| !c.trim().is_empty()) {
            s.push_str(&format!(" [{}]", cat.trim()));
        }
        if matches!(t.do_not_translate, Some(true)) {
            s.push_str(" [ห้ามแปล/คงคำเดิม]");
        }
        if let Some(rule) = t.context_rule.as_deref().filter(|r| !r.trim().is_empty()) {
            s.push_str(&format!(" [กฎบริบท: {}]", rule.trim()));
        }
        if let Some(gloss) = t.gloss.as_deref().filter(|g| !g.trim().is_empty()) {
            s.push_str(&format!(" [หมายเหตุ: {}]", gloss.trim()));
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
    if incoming.policy.is_some() {
        target.policy = incoming.policy;
    }
    if !incoming.forbidden_thai.is_empty() {
        target.forbidden_thai = normalized_list(incoming.forbidden_thai);
    }
    merge_opt(&mut target.context_rule, incoming.context_rule);
    if incoming.protected.is_some() {
        target.protected = incoming.protected;
    }
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

fn normalize_term_controls(t: &mut GlossaryTerm) {
    t.forbidden_thai = normalized_list(std::mem::take(&mut t.forbidden_thai));
    if let Some(policy) = t.policy
        && matches!(
            policy,
            TermPolicy::HardLocked | TermPolicy::Forbidden | TermPolicy::ContextDependent
        )
        && t.protected.is_none()
    {
        t.protected = Some(true);
    }
}

fn normalized_list(values: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        let v = value.trim();
        if !v.is_empty() && !out.iter().any(|existing| existing == v) {
            out.push(v.to_string());
        }
    }
    out
}

/// Effective policy for an entry. Legacy rows without `policy` but with
/// `protected=true` are treated as hard locks for backwards compatibility.
pub fn effective_policy(t: &GlossaryTerm) -> TermPolicy {
    t.policy.unwrap_or({
        if matches!(t.protected, Some(true)) {
            TermPolicy::HardLocked
        } else {
            TermPolicy::Preferred
        }
    })
}

/// True when an automatic Orchestrator upsert must not rewrite the term. Hard
/// locks, forbidden/context-dependent rules, and explicit legacy `protected=true`
/// rows are all human controls.
pub fn blocks_automatic_update(t: &GlossaryTerm) -> bool {
    matches!(t.protected, Some(true))
        || matches!(
            effective_policy(t),
            TermPolicy::HardLocked | TermPolicy::Forbidden | TermPolicy::ContextDependent
        )
}

/// Forbidden Thai renderings for this term. For a `forbidden` policy, `thai_term`
/// itself is also interpreted as a forbidden rendering.
pub fn forbidden_renderings(t: &GlossaryTerm) -> Vec<String> {
    let mut out = Vec::new();
    if matches!(effective_policy(t), TermPolicy::Forbidden) && !t.thai_term.trim().is_empty() {
        out.push(t.thai_term.trim().to_string());
    }
    for value in &t.forbidden_thai {
        let v = value.trim();
        if !v.is_empty() && !out.iter().any(|existing| existing == v) {
            out.push(v.to_string());
        }
    }
    out
}

pub fn policy_label(policy: TermPolicy) -> &'static str {
    match policy {
        TermPolicy::HardLocked => "hard locked",
        TermPolicy::Preferred => "preferred",
        TermPolicy::Forbidden => "forbidden",
        TermPolicy::ContextDependent => "context dependent",
    }
}

fn policy_prompt_label(policy: TermPolicy) -> &'static str {
    match policy {
        TermPolicy::HardLocked => "HARD_LOCKED",
        TermPolicy::Preferred => "PREFERRED",
        TermPolicy::Forbidden => "FORBIDDEN",
        TermPolicy::ContextDependent => "CONTEXT_DEPENDENT",
    }
}

fn controlled_conflict(existing: &GlossaryTerm, incoming: &GlossaryTerm) -> bool {
    let incoming_thai = incoming.thai_term.trim();
    if let Some(incoming_policy) = incoming.policy
        && incoming_policy != effective_policy(existing)
    {
        return true;
    }

    match effective_policy(existing) {
        TermPolicy::HardLocked => {
            if !incoming_thai.is_empty() && incoming_thai != existing.thai_term.trim() {
                return true;
            }
            if let Some(incoming_dnt) = incoming.do_not_translate
                && incoming_dnt != existing.do_not_translate.unwrap_or(false)
            {
                return true;
            }
            false
        }
        TermPolicy::Forbidden => {
            !incoming_thai.is_empty()
                && forbidden_renderings(existing)
                    .iter()
                    .any(|forbidden| forbidden == incoming_thai)
        }
        TermPolicy::ContextDependent => {
            !incoming_thai.is_empty()
                && !existing.thai_term.trim().is_empty()
                && incoming_thai != existing.thai_term.trim()
        }
        TermPolicy::Preferred => {
            matches!(existing.protected, Some(true))
                && !incoming_thai.is_empty()
                && incoming_thai != existing.thai_term.trim()
        }
    }
}

fn merge_opt(slot: &mut Option<String>, incoming: Option<String>) {
    if let Some(v) = incoming
        && !v.trim().is_empty()
    {
        *slot = Some(v);
    }
}

/// Normalize a jp_term for keying: trim whitespace.
fn normalize(s: &str) -> String {
    s.trim().to_string()
}

fn term_matches(t: &GlossaryTerm, needle: &str) -> bool {
    // Term-ish fields match in both directions (query 「聖剣エクスカリバー」 finds
    // the stored 聖剣); prose fields only forward, since a long query trivially
    // "contains" short prose fragments.
    let names = [
        t.jp_term.to_lowercase(),
        t.thai_term.to_lowercase(),
        t.romaji.as_deref().unwrap_or("").to_lowercase(),
    ];
    if names
        .iter()
        .any(|h| crate::workspace::characters::contains_either(h, needle))
    {
        return true;
    }

    let mut prose = vec![
        t.gloss.as_deref().unwrap_or("").to_lowercase(),
        t.context_rule.as_deref().unwrap_or("").to_lowercase(),
        policy_label(effective_policy(t)).to_lowercase(),
    ];
    prose.extend(t.forbidden_thai.iter().map(|v| v.to_lowercase()));
    if prose.iter().any(|h| h.contains(needle)) {
        return true;
    }

    // Reading channel: a kana query (ブレイブ) reaches the romaji field.
    let Some(reading) = crate::workspace::kana::kana_to_romaji(needle) else {
        return false;
    };
    let stored = t.romaji.as_deref().unwrap_or("").to_lowercase();
    let jp_reading = crate::workspace::kana::kana_to_romaji(&t.jp_term).unwrap_or_default();
    crate::workspace::characters::contains_either(&stored, &reading)
        || crate::workspace::characters::contains_either(&jp_reading.to_lowercase(), &reading)
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
