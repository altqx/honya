//! src/workspace/characters.rs — read/upsert/render CHARACTERS.md.
//!
//! The data block shape is `{"characters": [Character, ...]}`. The Markdown body
//! above it is a rendered table derived from that array. `upsert` merges
//! non-null fields and unions relationships so repeated tool calls accumulate
//! detail instead of clobbering it.

use serde::{Deserialize, Serialize};

use crate::model::{Character, Relationship};
use crate::workspace::Workspace;
use crate::workspace::data_block;

/// On-disk JSON payload wrapper for CHARACTERS.md.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CharactersBlock {
    #[serde(default)]
    characters: Vec<Character>,
}

/// Load all characters from CHARACTERS.md (empty vec when absent/empty).
pub fn load(ws: &Workspace) -> Vec<Character> {
    let block: CharactersBlock = data_block::read_data_block(&ws.characters_md());
    block.characters
}

/// Insert or merge a character into CHARACTERS.md and re-render the table.
///
/// Matching: by `c.id`; if empty, the id is derived from `slugify(jp_name)`
/// (falling back to `slugify(thai_name)`, then a stable hash, so an entry is
/// never lost to an empty id). On a match, non-null fields from `c` overwrite
/// the existing record and relationships are unioned by `(target_id, relation)`.
pub fn upsert(ws: &Workspace, mut c: Character) -> std::io::Result<()> {
    if c.id.trim().is_empty() {
        c.id = derive_id(&c);
    }

    let mut chars = load(ws);
    if let Some(existing) = chars.iter_mut().find(|e| e.id == c.id) {
        merge_into(existing, c);
    } else {
        chars.push(c);
    }

    // Stable display order: by id.
    chars.sort_by(|a, b| a.id.cmp(&b.id));

    let body = render_table(&chars);
    let block = CharactersBlock { characters: chars };
    data_block::write_with_data(&ws.characters_md(), &body, &block)
}

/// Remove the character with the given `id`. No-op if absent.
pub fn remove(ws: &Workspace, id: &str) -> std::io::Result<()> {
    let mut chars = load(ws);
    let before = chars.len();
    chars.retain(|c| c.id != id);
    if chars.len() == before {
        return Ok(());
    }
    let body = render_table(&chars);
    let block = CharactersBlock { characters: chars };
    data_block::write_with_data(&ws.characters_md(), &body, &block)
}

/// Query characters by free-text `query` (matches id/jp_name/thai_name/romaji,
/// case-insensitive substring) and/or exact `id`. With both `None`, returns all.
pub fn get(ws: &Workspace, query: Option<&str>, id: Option<&str>) -> Vec<Character> {
    let chars = load(ws);
    let q = query.map(|s| s.trim().to_lowercase());
    let q = q.filter(|s| !s.is_empty());
    chars
        .into_iter()
        .filter(|c| match id {
            Some(want) => c.id == want,
            None => true,
        })
        .filter(|c| match &q {
            Some(needle) => character_matches(c, needle),
            None => true,
        })
        .collect()
}

/// Render the human-readable Markdown body for CHARACTERS.md.
pub fn render_table(chars: &[Character]) -> String {
    let mut s = String::new();
    s.push_str("# ตัวละคร / Characters\n\n");

    if chars.is_empty() {
        s.push_str("_ยังไม่มีตัวละครที่บันทึกไว้ — เพิ่มได้ผ่านเครื่องมือของ Orchestrator หรือหน้าจอ Lexicon_\n");
        return s;
    }

    s.push_str("| ID | 日本語 | ไทย | Romaji | เพศ | คำลงท้าย | สรรพนาม/น้ำเสียง | ความสัมพันธ์ | บทแรก | หมายเหตุ |\n");
    s.push_str("|----|--------|-----|--------|-----|----------|------------------|---------------|-------|----------|\n");
    for c in chars {
        let rels = if c.relationships.is_empty() {
            "—".to_string()
        } else {
            c.relationships
                .iter()
                .map(|r| format!("{} → {}", r.relation, r.target_id))
                .collect::<Vec<_>>()
                .join("; ")
        };
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            cell(&c.id),
            cell(&c.jp_name),
            cell(&c.thai_name),
            opt(&c.romaji),
            opt(&c.gender),
            opt(&c.honorific),
            opt(&c.speech_style),
            cell(&rels),
            c.first_seen_chapter
                .map(|n| n.to_string())
                .unwrap_or_else(|| "—".to_string()),
            opt(&c.notes),
        ));
    }
    s
}

/// Render the compact character blurb injected into the Translator/Reviewer
/// prompt: one bullet per character emitting `日本語 → ไทย` plus the honorific and
/// speech-style (the pronoun/register fields the spec requires for continuity).
/// Empty roster yields an empty string so the caller can omit the section.
pub fn render_context_blurb(chars: &[Character]) -> String {
    if chars.is_empty() {
        return String::new();
    }
    let mut s = String::new();
    s.push_str("ตัวละคร (สรรพนาม/น้ำเสียงที่กำหนด ต้องใช้ให้สอดคล้อง):\n");
    for c in chars {
        let jp = c.jp_name.trim();
        if jp.is_empty() {
            continue;
        }
        s.push_str("- ");
        s.push_str(jp);
        s.push_str(" → ");
        s.push_str(if c.thai_name.trim().is_empty() {
            "—"
        } else {
            c.thai_name.trim()
        });
        if let Some(h) = c.honorific.as_deref().filter(|x| !x.trim().is_empty()) {
            s.push_str(&format!(" [คำลงท้าย: {}]", h.trim()));
        }
        if let Some(sp) = c.speech_style.as_deref().filter(|x| !x.trim().is_empty()) {
            s.push_str(&format!(" [น้ำเสียง: {}]", sp.trim()));
        }
        s.push('\n');
    }
    s
}

// --- helpers ----------------------------------------------------------------

/// Merge `incoming` into `target`, keeping existing values when the incoming
/// field is empty/None and unioning relationships.
fn merge_into(target: &mut Character, incoming: Character) {
    if !incoming.jp_name.trim().is_empty() {
        target.jp_name = incoming.jp_name;
    }
    if !incoming.thai_name.trim().is_empty() {
        target.thai_name = incoming.thai_name;
    }
    merge_opt(&mut target.romaji, incoming.romaji);
    merge_opt(&mut target.gender, incoming.gender);
    merge_opt(&mut target.honorific, incoming.honorific);
    merge_opt(&mut target.speech_style, incoming.speech_style);
    merge_opt(&mut target.notes, incoming.notes);
    if incoming.first_seen_chapter.is_some() {
        // Keep the earliest known appearance.
        target.first_seen_chapter = match (target.first_seen_chapter, incoming.first_seen_chapter) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (None, b) => b,
            (a, None) => a,
        };
    }
    union_relationships(&mut target.relationships, incoming.relationships);
}

/// Overwrite `slot` only when `incoming` is a non-empty `Some`.
fn merge_opt(slot: &mut Option<String>, incoming: Option<String>) {
    if let Some(v) = incoming {
        if !v.trim().is_empty() {
            *slot = Some(v);
        }
    }
}

/// Union relationships by `(target_id, relation)`; later wins on collision.
fn union_relationships(target: &mut Vec<Relationship>, incoming: Vec<Relationship>) {
    for rel in incoming {
        if let Some(existing) = target
            .iter_mut()
            .find(|e| e.target_id == rel.target_id && e.relation == rel.relation)
        {
            *existing = rel;
        } else {
            target.push(rel);
        }
    }
}

/// Derive a stable id from a character with an empty id.
fn derive_id(c: &Character) -> String {
    let from_jp = crate::workspace::slugify(&c.jp_name);
    if !from_jp.is_empty() {
        return from_jp;
    }
    let from_thai = crate::workspace::slugify(&c.thai_name);
    if !from_thai.is_empty() {
        return from_thai;
    }
    // Last resort: a deterministic short hash of the names so two distinct
    // unnamed entries don't collide.
    format!("char-{:08x}", fnv1a(&format!("{}|{}", c.jp_name, c.thai_name)))
}

/// Tiny FNV-1a hash for fallback id derivation (no external dep).
fn fnv1a(s: &str) -> u32 {
    let mut hash: u32 = 0x811c9dc5;
    for b in s.bytes() {
        hash ^= b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

fn character_matches(c: &Character, needle: &str) -> bool {
    let hay = [
        c.id.to_lowercase(),
        c.jp_name.to_lowercase(),
        c.thai_name.to_lowercase(),
        c.romaji.as_deref().unwrap_or("").to_lowercase(),
    ];
    hay.iter().any(|h| h.contains(needle))
}

/// Escape pipe/newline so a value never breaks the Markdown table.
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
