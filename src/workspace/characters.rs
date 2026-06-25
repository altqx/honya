//! Read/upsert/render CHARACTERS.md. Data block is `{"characters":[Character,...]}`.
//! `upsert` merges non-null fields and unions relationships so repeated tool calls
//! accumulate detail instead of clobbering it.

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

/// What `upsert` did, so callers (notably the Orchestrator tool) can react —
/// e.g. nudge the model to consolidate a likely duplicate via `merge`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CharacterUpsertOutcome {
    /// Folded into an existing entry (exact id, exact JP/alias, or an unambiguous
    /// surname+given-name pair). `into_id` is the surviving entry.
    Merged { into_id: String },
    /// Added as a new entry with no same-person signal.
    Inserted,
    /// Added as a new entry, but one or more existing entries look like they could
    /// be the same person (ambiguous name overlap or matching reading). The model
    /// should inspect and call `merge` if appropriate — we never auto-merge these.
    InsertedWithCandidates { id: String, candidates: Vec<String> },
}

/// Insert or merge a character. Matching, in order: exact `id`; exact JP surface
/// form against an existing `jp_name`/alias; an unambiguous JA surname+given pair
/// (one name is the other with a surname prepended, corroborated by a shared romaji
/// token). Weaker signals return merge candidates; see [`CharacterUpsertOutcome`].
pub fn upsert(ws: &Workspace, mut c: Character) -> std::io::Result<CharacterUpsertOutcome> {
    if c.id.trim().is_empty() {
        c.id = derive_id(&c);
    }

    let mut chars = load(ws);

    // 1a. Exact id match.
    if let Some(i) = chars.iter().position(|e| e.id == c.id) {
        let into_id = chars[i].id.clone();
        absorb(&mut chars[i], c);
        return finish(ws, chars, CharacterUpsertOutcome::Merged { into_id });
    }

    // 1b. Exact JP surface form (jp_name or a recorded alias) — same written name
    // can never be two different people, so this is always safe to auto-merge.
    if let Some(i) = find_exact(&chars, &c.jp_name) {
        let into_id = chars[i].id.clone();
        absorb(&mut chars[i], c);
        return finish(ws, chars, CharacterUpsertOutcome::Merged { into_id });
    }

    // 2. Conservative surname+given auto-merge: only when exactly one existing entry
    //    forms an unambiguous pair with the incoming name.
    let suffix = suffix_candidates(&chars, &c);
    if suffix.len() == 1 {
        let i = suffix[0];
        let into_id = chars[i].id.clone();
        absorb(&mut chars[i], c);
        return finish(ws, chars, CharacterUpsertOutcome::Merged { into_id });
    }

    // 3. Weaker signals → suggest-only. Insert, but report candidates.
    let mut cand_idx = suffix; // ambiguous (>=2) surname+given matches
    for i in reading_candidates(&chars, &c) {
        if !cand_idx.contains(&i) {
            cand_idx.push(i);
        }
    }
    let candidates: Vec<String> = cand_idx.iter().map(|&i| chars[i].id.clone()).collect();
    let id = c.id.clone();
    chars.push(c);
    let outcome = if candidates.is_empty() {
        CharacterUpsertOutcome::Inserted
    } else {
        CharacterUpsertOutcome::InsertedWithCandidates { id, candidates }
    };
    finish(ws, chars, outcome)
}

/// Like [`upsert`], but preserves established Thai names on auto-merge.
/// Agent writes use this; human/coherence edits still use plain [`upsert`].
pub fn upsert_keep_thai(
    ws: &Workspace,
    mut c: Character,
) -> std::io::Result<CharacterUpsertOutcome> {
    if !c.thai_name.trim().is_empty() {
        if c.id.trim().is_empty() {
            c.id = derive_id(&c);
        }
        let chars = load(ws);
        if let Some(i) = find_match(&chars, &c)
            && !chars[i].thai_name.trim().is_empty()
        {
            c.thai_name = String::new();
        }
    }
    upsert(ws, c)
}

/// Existing entry that would receive `c` under the auto-merge rules.
fn find_match(chars: &[Character], c: &Character) -> Option<usize> {
    if let Some(i) = chars.iter().position(|e| e.id == c.id) {
        return Some(i);
    }
    if let Some(i) = find_exact(chars, &c.jp_name) {
        return Some(i);
    }
    let suffix = suffix_candidates(chars, c);
    (suffix.len() == 1).then(|| suffix[0])
}

/// Sort by id, re-render the table, and write the data block atomically.
fn finish(
    ws: &Workspace,
    mut chars: Vec<Character>,
    outcome: CharacterUpsertOutcome,
) -> std::io::Result<CharacterUpsertOutcome> {
    chars.sort_by(|a, b| a.id.cmp(&b.id));
    let body = render_table(&chars);
    let block = CharactersBlock { characters: chars };
    data_block::write_with_data(&ws.characters_md(), &body, &block)?;
    Ok(outcome)
}

/// Consolidate two existing entries that are the same person: fold `from` into
/// `into`, keeping `into` as canonical. `into`'s populated fields win; gaps are
/// filled from `from`; the earliest `first_seen_chapter` is kept; relationships are
/// unioned; `from`'s name forms become aliases of `into`. Every relationship across
/// the roster that pointed at `from_id` is repointed to `into_id` so no reference
/// dangles, then `from` is removed. Returns `Ok(false)` (no write) when the ids are
/// equal or either is absent.
pub fn merge(ws: &Workspace, from_id: &str, into_id: &str) -> std::io::Result<bool> {
    if from_id == into_id {
        return Ok(false);
    }
    let mut chars = load(ws);
    let Some(from_pos) = chars.iter().position(|c| c.id == from_id) else {
        return Ok(false);
    };
    if !chars.iter().any(|c| c.id == into_id) {
        return Ok(false);
    }
    let from = chars.remove(from_pos);

    let into_idx = chars
        .iter()
        .position(|c| c.id == into_id)
        .expect("into_id present and distinct from from_id");
    {
        let into = &mut chars[into_idx];
        fill_from(into, &from);
        record_alias(into, &from.jp_name);
        for a in &from.aliases {
            record_alias(into, a);
        }
        union_relationships(&mut into.relationships, from.relationships.clone());
    }

    // Repoint inbound references, then drop self-loops and duplicates everywhere.
    for c in chars.iter_mut() {
        for r in c.relationships.iter_mut() {
            if r.target_id == from_id {
                r.target_id = into_id.to_string();
            }
        }
        dedup_relationships(c);
    }

    chars.sort_by(|a, b| a.id.cmp(&b.id));
    let body = render_table(&chars);
    let block = CharactersBlock { characters: chars };
    data_block::write_with_data(&ws.characters_md(), &body, &block)?;
    Ok(true)
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

/// Query by case/whitespace-insensitive substring `query` (id/jp_name/thai_name/
/// romaji/aliases) and/or `id`; both absent (or blank — LLM callers routinely send
/// `""` for params they don't use) returns all. An `id` with no exact hit degrades
/// to a name needle, since models often put the surface name in `id`.
pub fn get(ws: &Workspace, query: Option<&str>, id: Option<&str>) -> Vec<Character> {
    let chars = load(ws);
    let id = id.map(str::trim).filter(|s| !s.is_empty());
    let q = query.map(str::trim).filter(|s| !s.is_empty());

    if let Some(want) = id {
        let exact: Vec<Character> = chars.iter().filter(|c| c.id == want).cloned().collect();
        if !exact.is_empty() {
            return exact;
        }
    }

    let needles: Vec<&str> = id.into_iter().chain(q).collect();
    if needles.is_empty() {
        return chars;
    }
    chars
        .into_iter()
        .filter(|c| needles.iter().any(|n| character_matches(c, n)))
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
        let jp = if c.aliases.is_empty() {
            cell(&c.jp_name)
        } else {
            cell(&format!("{} ({})", c.jp_name, c.aliases.join(", ")))
        };
        let thai = if c.also_called.is_empty() {
            cell(&c.thai_name)
        } else {
            let calls = c
                .also_called
                .iter()
                .filter(|a| !a.jp.trim().is_empty())
                .map(|a| {
                    let th = if a.thai.trim().is_empty() {
                        c.thai_name.trim()
                    } else {
                        a.thai.trim()
                    };
                    format!("{}→{}", a.jp.trim(), th)
                })
                .collect::<Vec<_>>()
                .join(", ");
            cell(&format!("{} [{}]", c.thai_name, calls))
        };
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            cell(&c.id),
            jp,
            thai,
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

/// Render the character blurb for the Translator/Reviewer prompt: one `日本語 → ไทย`
/// bullet plus honorific/speech-style (pronoun/register the spec needs for
/// continuity); empty roster → "".
pub fn render_context_blurb(chars: &[Character]) -> String {
    if chars.is_empty() {
        return String::new();
    }
    let mut s = String::new();
    s.push_str("ตัวละคร — ชื่อไทยหลัง → คือการสะกดที่ตายตัว (canonical) ต้องใช้ให้ตรงตัวทุกครั้งที่เอ่ยถึงตัวละครนั้น ห้ามสะกดต่าง ห้ามบัญญัติชื่อไทยใหม่ และต้องใช้สรรพนาม/น้ำเสียงที่กำหนดให้สอดคล้อง:\n");
    for c in chars {
        let jp = c.jp_name.trim();
        if jp.is_empty() {
            continue;
        }
        s.push_str("- ");
        s.push_str(jp);
        // Alias hits keep bare-name mentions tied to this character.
        let aliases: Vec<&str> = c
            .aliases
            .iter()
            .map(|a| a.trim())
            .filter(|a| !a.is_empty())
            .collect();
        if !aliases.is_empty() {
            s.push_str(&format!(" (={})", aliases.join(", ")));
        }
        s.push_str(" → ");
        s.push_str(if c.thai_name.trim().is_empty() {
            "—"
        } else {
            c.thai_name.trim()
        });
        if let Some(g) = c.gender.as_deref().filter(|x| !x.trim().is_empty()) {
            let th = match g.trim() {
                "male" => "ชาย",
                "female" => "หญิง",
                other => other,
            };
            s.push_str(&format!(" [เพศ: {th}]"));
        }
        if let Some(h) = c.honorific.as_deref().filter(|x| !x.trim().is_empty()) {
            s.push_str(&format!(" [คำลงท้าย: {}]", h.trim()));
        }
        if let Some(sp) = c.speech_style.as_deref().filter(|x| !x.trim().is_empty()) {
            s.push_str(&format!(" [น้ำเสียง: {}]", sp.trim()));
        }
        let calls: Vec<String> = c
            .also_called
            .iter()
            .filter(|a| !a.jp.trim().is_empty())
            .map(|a| {
                let th = if a.thai.trim().is_empty() {
                    c.thai_name.trim()
                } else {
                    a.thai.trim()
                };
                match a.by.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
                    Some(by) => format!("{}→{} (โดย {by})", a.jp.trim(), th),
                    None => format!("{}→{}", a.jp.trim(), th),
                }
            })
            .collect();
        if !calls.is_empty() {
            s.push_str(&format!(" [เรียกอีกชื่อ: {}]", calls.join(", ")));
        }
        s.push('\n');
    }
    s
}

/// Merge `incoming` into `target`: empty/None incoming fields keep existing;
/// relationships are unioned.
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
    union_also_called(&mut target.also_called, incoming.also_called);
}

/// Union address forms by normalized JP; later non-empty Thai/`by` values win.
fn union_also_called(
    target: &mut Vec<crate::model::AltName>,
    incoming: Vec<crate::model::AltName>,
) {
    for inc in incoming {
        if inc.jp.trim().is_empty() {
            continue;
        }
        let key = norm_name(&inc.jp);
        if let Some(existing) = target.iter_mut().find(|e| norm_name(&e.jp) == key) {
            if !inc.thai.trim().is_empty() {
                existing.thai = inc.thai;
            }
            if inc.by.as_deref().is_some_and(|b| !b.trim().is_empty()) {
                existing.by = inc.by;
            }
        } else {
            target.push(inc);
        }
    }
}

/// Overwrite `slot` only when `incoming` is a non-empty `Some`.
fn merge_opt(slot: &mut Option<String>, incoming: Option<String>) {
    if let Some(v) = incoming
        && !v.trim().is_empty()
    {
        *slot = Some(v);
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
    // Last resort: deterministic hash so two distinct unnamed entries don't collide.
    format!(
        "char-{:08x}",
        fnv1a(&format!("{}|{}", c.jp_name, c.thai_name))
    )
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
    // Strip spaces/separators on both sides so "桐島 朱夏" finds "桐島朱夏".
    let norm_needle = norm_name(needle).to_lowercase();
    if norm_needle.is_empty() {
        return false;
    }
    let mut hay = vec![
        norm_name(&c.id).to_lowercase(),
        norm_name(&c.jp_name).to_lowercase(),
        norm_name(&c.thai_name).to_lowercase(),
        norm_name(c.romaji.as_deref().unwrap_or("")).to_lowercase(),
    ];
    hay.extend(c.aliases.iter().map(|a| norm_name(a).to_lowercase()));
    hay.extend(c.also_called.iter().flat_map(|a| {
        [
            norm_name(&a.jp).to_lowercase(),
            norm_name(&a.thai).to_lowercase(),
        ]
    }));
    if hay.iter().any(|h| contains_either(h, &norm_needle)) {
        return true;
    }

    // Reading channel: the Orchestrator searches "by reading", which arrives as
    // kana (ノノカ) and can never substring-match kanji — bridge it through the
    // romaji field (and kana names/aliases) instead.
    let needle_reading = crate::workspace::kana::kana_to_romaji(needle)
        .map(|r| norm_romaji(&r))
        .unwrap_or_else(|| norm_romaji(needle));
    if needle_reading.is_empty() {
        return false;
    }
    let mut readings: Vec<String> = vec![norm_romaji(c.romaji.as_deref().unwrap_or(""))];
    readings.extend(
        std::iter::once(c.jp_name.as_str())
            .chain(c.aliases.iter().map(String::as_str))
            .filter_map(crate::workspace::kana::kana_to_romaji)
            .map(|r| norm_romaji(&r)),
    );
    readings.iter().any(|h| contains_either(h, &needle_reading))
}

/// Substring match in either direction, so the query 「朱夏ちゃん」 still finds the
/// entry stored as 朱夏. The reverse direction requires the stored form to be more
/// than one ASCII char, or single-letter ids would match almost any query.
pub(crate) fn contains_either(hay: &str, needle: &str) -> bool {
    if hay.is_empty() || needle.is_empty() {
        return false;
    }
    if hay.contains(needle) {
        return true;
    }
    (hay.chars().count() >= 2 || !hay.is_ascii()) && needle.contains(hay)
}

/// Auto-merge an incoming entry into `target`: keep the longer JP surface form as
/// the canonical `jp_name`, record the other as an alias, then fold the remaining
/// fields and relationships. Alias bookkeeping is kept out of `merge_into` so a
/// Lexicon edit (which passes empty aliases) never clobbers existing aliases.
fn absorb(target: &mut Character, incoming: Character) {
    let inc_jp = incoming.jp_name.trim().to_string();
    let inc_len = norm_name(&inc_jp).chars().count();
    let cur_len = norm_name(&target.jp_name).chars().count();
    let differ = !inc_jp.is_empty() && norm_name(&inc_jp) != norm_name(&target.jp_name);
    let inc_shorter = differ && inc_len < cur_len;
    if differ {
        if inc_len > cur_len {
            // Incoming is the fuller name — adopt it as canonical, then demote the
            // old form to an alias (set jp_name first, or record_alias rejects it
            // as equal to the canonical name).
            let old = target.jp_name.clone();
            target.jp_name = inc_jp;
            record_alias(target, &old);
        } else {
            record_alias(target, &inc_jp);
        }
    }
    for a in &incoming.aliases {
        record_alias(target, a);
    }
    // Delegate the non-name fields; blank the name forms so merge_into leaves the
    // canonical jp_name we just chose untouched.
    let mut rest = incoming;
    rest.jp_name = String::new();
    rest.aliases = Vec::new();
    if inc_shorter {
        // A shorter variant's Thai must not overwrite the canonical entry's Thai.
        rest.thai_name = String::new();
    }
    merge_into(target, rest);
    // Canonical name must never also sit in aliases.
    let canon = norm_name(&target.jp_name);
    target.aliases.retain(|a| norm_name(a) != canon);
}

/// Fill only the empty/None fields of `target` from `src` (survivor-wins), keeping
/// the earliest `first_seen_chapter`. Used by [`merge`] where `target` is canonical.
fn fill_from(target: &mut Character, src: &Character) {
    if target.thai_name.trim().is_empty() && !src.thai_name.trim().is_empty() {
        target.thai_name = src.thai_name.clone();
    }
    fill_opt(&mut target.romaji, &src.romaji);
    fill_opt(&mut target.gender, &src.gender);
    fill_opt(&mut target.honorific, &src.honorific);
    fill_opt(&mut target.speech_style, &src.speech_style);
    fill_opt(&mut target.notes, &src.notes);
    target.first_seen_chapter = match (target.first_seen_chapter, src.first_seen_chapter) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (None, b) => b,
        (a, None) => a,
    };
}

/// Set `slot` from `src` only when `slot` is empty/None and `src` is non-empty.
fn fill_opt(slot: &mut Option<String>, src: &Option<String>) {
    let empty = slot.as_deref().map(|s| s.trim().is_empty()).unwrap_or(true);
    if empty
        && let Some(v) = src
        && !v.trim().is_empty()
    {
        *slot = Some(v.clone());
    }
}

/// Record `name` as an alias of `c` if it is non-empty, not the canonical jp_name,
/// and not already present (all compared with spacing normalized away).
fn record_alias(c: &mut Character, name: &str) {
    let n = name.trim();
    if n.is_empty() {
        return;
    }
    let key = norm_name(n);
    if key.is_empty() || key == norm_name(&c.jp_name) {
        return;
    }
    if c.aliases.iter().any(|a| norm_name(a) == key) {
        return;
    }
    c.aliases.push(n.to_string());
}

/// Drop self-loops and `(target_id, relation)` duplicates after a repoint.
fn dedup_relationships(c: &mut Character) {
    let id = c.id.clone();
    let mut seen: Vec<(String, String)> = Vec::new();
    c.relationships.retain(|r| {
        if r.target_id == id {
            return false;
        }
        let key = (r.target_id.clone(), r.relation.clone());
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });
}

/// Layer 1b: index of an existing entry whose `jp_name` or an alias is the same
/// written form as `incoming_jp` (spacing normalized). Identical surface forms can
/// never be two different characters, so the caller auto-merges on a hit.
fn find_exact(chars: &[Character], incoming_jp: &str) -> Option<usize> {
    let target = norm_name(incoming_jp);
    if target.is_empty() {
        return None;
    }
    chars.iter().position(|e| {
        norm_name(&e.jp_name) == target || e.aliases.iter().any(|a| norm_name(a) == target)
    })
}

/// Layer 2: indices of existing entries that form a JA `surname + given` pair with
/// `inc` — the shorter name is a suffix of the longer (a surname was prepended) AND
/// the shorter's romaji is a token of the longer's. The caller auto-merges only when
/// exactly one entry qualifies; ≥2 is ambiguous and downgraded to suggest-only.
fn suffix_candidates(chars: &[Character], inc: &Character) -> Vec<usize> {
    let mut out = Vec::new();
    for (i, e) in chars.iter().enumerate() {
        let inc_len = norm_name(&inc.jp_name).chars().count();
        let e_len = norm_name(&e.jp_name).chars().count();
        let (short, long, short_r, long_r) = if inc_len < e_len {
            (&inc.jp_name, &e.jp_name, &inc.romaji, &e.romaji)
        } else {
            (&e.jp_name, &inc.jp_name, &e.romaji, &inc.romaji)
        };
        if is_given_suffix(short, long)
            && let (Some(sr), Some(lr)) = (short_r.as_deref(), long_r.as_deref())
            && romaji_corroborates(sr, lr)
        {
            out.push(i);
        }
    }
    out
}

/// Layer 3: indices of existing entries that share BOTH reading channels (normalized
/// romaji and Thai) with `inc` but have no surface overlap — likely the same person
/// under different kanji (みや vs 未夜), but possibly a homophone, so never auto-merged.
fn reading_candidates(chars: &[Character], inc: &Character) -> Vec<usize> {
    let inc_r = inc
        .romaji
        .as_deref()
        .map(norm_romaji)
        .filter(|s| !s.is_empty());
    let inc_t = norm_name(&inc.thai_name);
    let (Some(inc_r), false) = (inc_r, inc_t.is_empty()) else {
        return Vec::new();
    };
    chars
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            e.romaji.as_deref().map(norm_romaji).as_deref() == Some(inc_r.as_str())
                && norm_name(&e.thai_name) == inc_t
                && norm_name(&e.jp_name) != norm_name(&inc.jp_name)
        })
        .map(|(i, _)| i)
        .collect()
}

/// True when `short` is `long` with a surname prepended: a strict, non-empty suffix
/// after collapsing spacing (有月勇 ⊃ 勇, 源道寺朝華 ⊃ 朝華).
fn is_given_suffix(short: &str, long: &str) -> bool {
    let s = norm_name(short);
    let l = norm_name(long);
    !s.is_empty() && s.len() < l.len() && l.ends_with(&s)
}

/// True when the shorter name's romaji appears as a whole token of the longer's,
/// e.g. "Yuu" within "Aritsuki Yuu". Requiring a token (not a substring) avoids
/// fusing distinct readings like Yuu vs Daiyuu.
fn romaji_corroborates(short_romaji: &str, long_romaji: &str) -> bool {
    let s = norm_romaji(short_romaji);
    !s.is_empty() && romaji_tokens(long_romaji).contains(&s)
}

/// Split romaji into normalized tokens on whitespace/hyphen/period.
fn romaji_tokens(s: &str) -> Vec<String> {
    s.split(|c: char| c.is_whitespace() || c == '-' || c == '.')
        .map(norm_romaji)
        .filter(|t| !t.is_empty())
        .collect()
}

/// Normalize romaji for loose comparison: lowercase, drop spacing/punctuation, fold
/// long-vowel macrons to doubled vowels (ū→uu, ō→ou).
pub(crate) fn norm_romaji(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.trim().chars() {
        match ch {
            ' ' | '\t' | '-' | '.' | '\'' | '_' | '・' => {}
            'ā' | 'Ā' => out.push_str("aa"),
            'ī' | 'Ī' => out.push_str("ii"),
            'ū' | 'Ū' => out.push_str("uu"),
            'ē' | 'Ē' => out.push_str("ee"),
            'ō' | 'Ō' => out.push_str("ou"),
            c => out.extend(c.to_lowercase()),
        }
    }
    out
}

/// Collapse spacing so "有月 勇" / "有月-勇" / "有月勇" compare equal.
fn norm_name(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != '-' && *c != '・')
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Relationship;

    fn temp_ws(tag: &str) -> (std::path::PathBuf, Workspace) {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "honya_chars_{tag}_{}_{}",
            std::process::id(),
            stamp
        ));
        std::fs::create_dir_all(&base).unwrap();
        let ws = Workspace::new(base.clone(), 1);
        (base, ws)
    }

    fn ch(id: &str, jp: &str, thai: &str, romaji: Option<&str>) -> Character {
        Character {
            id: id.into(),
            jp_name: jp.into(),
            thai_name: thai.into(),
            romaji: romaji.map(|s| s.to_string()),
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

    fn find<'a>(chars: &'a [Character], id: &str) -> Option<&'a Character> {
        chars.iter().find(|c| c.id == id)
    }

    /// Address forms union, search, and render with per-form Thai.
    #[test]
    fn also_called_per_form_thai() {
        use crate::model::AltName;
        let (base, ws) = temp_ws("also_called");
        let mut yuu = ch("yuu", "結城勇", "ยูกิ ยู", Some("Yuuki Yuu"));
        yuu.also_called = vec![AltName {
            jp: "ユウ".into(),
            thai: "ยู".into(),
            by: Some("เพื่อนสนิท".into()),
        }];
        upsert(&ws, yuu).unwrap();

        let mut again = ch("yuu", "結城勇", "ยูกิ ยู", None);
        again.also_called = vec![AltName {
            jp: "お兄ちゃん".into(),
            thai: "พี่".into(),
            by: None,
        }];
        upsert(&ws, again).unwrap();

        let chars = load(&ws);
        assert_eq!(chars.len(), 1, "alt-names must not spawn duplicate entries");
        let c = &chars[0];
        assert_eq!(c.also_called.len(), 2, "alt-names union, not clobber");

        assert!(get(&ws, Some("ユウ"), None).iter().any(|c| c.id == "yuu"));

        let blurb = render_context_blurb(&chars);
        assert!(blurb.contains("ユウ→ยู"), "alt Thai shown:\n{blurb}");
        assert!(
            blurb.contains("お兄ちゃん→พี่"),
            "sister's name shown:\n{blurb}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Same written JP name under two different ids → one entry (always safe).
    #[test]
    fn upsert_exact_jp_dedup() {
        let (base, ws) = temp_ws("exact_jp");
        upsert(&ws, ch("yuu1", "勇", "ยู", Some("Yuu"))).unwrap();
        let out = upsert(&ws, ch("yuu2", "勇", "ยู", Some("Yuu"))).unwrap();

        let chars = load(&ws);
        assert_eq!(chars.len(), 1, "identical JP form must not duplicate");
        assert!(matches!(out, CharacterUpsertOutcome::Merged { .. }));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Agent upserts preserve established Thai names but set first sightings.
    #[test]
    fn keep_thai_preserves_established_name() {
        let (base, ws) = temp_ws("keep_thai");
        upsert_keep_thai(&ws, ch("yuu", "勇", "ยู", Some("Yuu"))).unwrap();
        upsert_keep_thai(&ws, ch("yuu", "勇", "ยูว์", Some("Yuu"))).unwrap();

        let chars = load(&ws);
        assert_eq!(chars.len(), 1);
        assert_eq!(chars[0].thai_name, "ยู", "established Thai name must stick");

        upsert_keep_thai(&ws, ch("miya", "未夜", "มิยะ", Some("Miya"))).unwrap();
        assert_eq!(find(&load(&ws), "miya").unwrap().thai_name, "มิยะ");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A bare form already recorded as an alias routes future upserts to the canonical entry.
    #[test]
    fn upsert_alias_dedup() {
        let (base, ws) = temp_ws("alias");
        let mut full = ch("aritsuki-yuu", "有月勇", "อาริทสึกิ ยู", Some("Aritsuki Yuu"));
        full.aliases = vec!["勇".into()];
        upsert(&ws, full).unwrap();

        let out = upsert(&ws, ch("bare", "勇", "ยู", Some("Yuu"))).unwrap();
        let chars = load(&ws);
        assert_eq!(chars.len(), 1, "alias hit must merge, not insert");
        assert!(matches!(out, CharacterUpsertOutcome::Merged { .. }));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 勇 → 有月勇: one unambiguous surname+given pair auto-merges, bare form becomes an alias.
    #[test]
    fn auto_merge_suffix_unambiguous() {
        let (base, ws) = temp_ws("suffix");
        upsert(&ws, ch("af", "有月勇", "อาริทสึกิ ยู", Some("Aritsuki Yuu"))).unwrap();
        let out = upsert(&ws, ch("bare", "勇", "ยู", Some("Yuu"))).unwrap();

        let chars = load(&ws);
        assert_eq!(chars.len(), 1);
        assert!(matches!(out, CharacterUpsertOutcome::Merged { .. }));
        let yuu = &chars[0];
        assert_eq!(yuu.jp_name, "有月勇", "fuller name stays canonical");
        assert_eq!(
            yuu.thai_name, "อาริทสึกิ ยู",
            "shorter variant's Thai must not win"
        );
        assert!(yuu.aliases.iter().any(|a| a == "勇"));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Bare name first; later full name becomes canonical.
    #[test]
    fn auto_merge_adopts_fuller_name() {
        let (base, ws) = temp_ws("fuller");
        upsert(&ws, ch("yuu", "勇", "ยู", Some("Yuu"))).unwrap();
        upsert(&ws, ch("af", "有月勇", "อาริทสึกิ ยู", Some("Aritsuki Yuu"))).unwrap();

        let chars = load(&ws);
        assert_eq!(chars.len(), 1);
        let yuu = &chars[0];
        assert_eq!(yuu.id, "yuu", "surviving id is stable");
        assert_eq!(yuu.jp_name, "有月勇");
        assert!(yuu.aliases.iter().any(|a| a == "勇"));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Shared given name across two people is ambiguous, so suggest only.
    #[test]
    fn ambiguous_suffix_is_suggest_only() {
        let (base, ws) = temp_ws("ambiguous");
        upsert(
            &ws,
            ch("a", "源道寺朝華", "เกนโดจิ อาซากะ", Some("Gendouji Asaka")),
        )
        .unwrap();
        upsert(
            &ws,
            ch("b", "香坂朝華", "โคซากะ อาซากะ", Some("Kousaka Asaka")),
        )
        .unwrap();
        let out = upsert(&ws, ch("c", "朝華", "อาซากะ", Some("Asaka"))).unwrap();

        let chars = load(&ws);
        assert_eq!(chars.len(), 3, "ambiguous match must not auto-merge");
        match out {
            CharacterUpsertOutcome::InsertedWithCandidates { candidates, .. } => {
                assert_eq!(candidates.len(), 2);
            }
            other => panic!("expected suggestions, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Same reading but different kanji suggests only.
    #[test]
    fn reading_match_is_suggest_only() {
        let (base, ws) = temp_ws("reading");
        upsert(&ws, ch("miya", "みや", "มิยะ", Some("Miya"))).unwrap();
        let out = upsert(&ws, ch("miya2", "未夜", "มิยะ", Some("Miya"))).unwrap();

        let chars = load(&ws);
        assert_eq!(chars.len(), 2, "homophone/alt-kanji must not auto-merge");
        match out {
            CharacterUpsertOutcome::InsertedWithCandidates { candidates, .. } => {
                assert_eq!(candidates, vec!["miya".to_string()]);
            }
            other => panic!("expected suggestions, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Homophones with no surface overlap are reported, not fused.
    #[test]
    fn homophone_not_auto_merged() {
        let (base, ws) = temp_ws("homophone");
        upsert(&ws, ch("a", "雪", "ยูกิ", Some("Yuki"))).unwrap();
        let out = upsert(&ws, ch("b", "幸", "ยูกิ", Some("Yuki"))).unwrap();

        assert_eq!(load(&ws).len(), 2);
        assert!(matches!(
            out,
            CharacterUpsertOutcome::InsertedWithCandidates { .. }
        ));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// merge() folds, aliases the old forms, and repoints inbound relationships.
    #[test]
    fn merge_repoints_relationships() {
        let (base, ws) = temp_ws("merge_rel");
        upsert(&ws, ch("a", "天", "เท็น", None)).unwrap();
        upsert(&ws, ch("b-old", "B", "บี", None)).unwrap();
        upsert(&ws, ch("b", "本田B", "ฮอนดะ บี", Some("Honda B"))).unwrap();
        let mut c = ch("c", "C", "ซี", None);
        c.relationships = vec![Relationship {
            target_id: "b-old".into(),
            relation: "friend".into(),
        }];
        upsert(&ws, c).unwrap();

        // 本田B vs B form a surname+given pair, but no romaji on B-old, so they did NOT
        // auto-merge; consolidate explicitly.
        assert!(merge(&ws, "b-old", "b").unwrap());

        let chars = load(&ws);
        assert!(find(&chars, "b-old").is_none(), "from entry removed");
        let b = find(&chars, "b").expect("survivor present");
        assert!(b.aliases.iter().any(|a| a == "B"), "old form aliased");
        let cc = find(&chars, "c").unwrap();
        assert_eq!(cc.relationships[0].target_id, "b", "inbound ref repointed");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// merge() is a no-op on equal ids or a missing endpoint.
    #[test]
    fn merge_noop() {
        let (base, ws) = temp_ws("merge_noop");
        upsert(&ws, ch("a", "甲", "เอ", None)).unwrap();
        assert!(!merge(&ws, "a", "a").unwrap());
        assert!(!merge(&ws, "missing", "a").unwrap());
        assert_eq!(load(&ws).len(), 1);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// get() matches alias forms, not just jp_name/thai_name.
    #[test]
    fn get_finds_by_alias() {
        let (base, ws) = temp_ws("get_alias");
        let mut full = ch("yuu", "有月勇", "อาริทสึกิ ยู", Some("Aritsuki Yuu"));
        full.aliases = vec!["勇".into()];
        upsert(&ws, full).unwrap();

        let hits = get(&ws, Some("勇"), None);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "yuu");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// An upsert that carries no aliases (e.g. a Lexicon edit) must not wipe existing ones.
    #[test]
    fn upsert_keeps_existing_aliases() {
        let (base, ws) = temp_ws("keep_alias");
        let mut full = ch("yuu", "有月勇", "อาริทสึกิ ยู", Some("Aritsuki Yuu"));
        full.aliases = vec!["勇".into()];
        upsert(&ws, full).unwrap();

        // Same canonical name, empty aliases, new note.
        let mut edit = ch("yuu", "有月勇", "อาริทสึกิ ยู", Some("Aritsuki Yuu"));
        edit.notes = Some("28 ปี".into());
        upsert(&ws, edit).unwrap();

        let chars = load(&ws);
        let yuu = find(&chars, "yuu").unwrap();
        assert_eq!(yuu.aliases, vec!["勇".to_string()], "aliases preserved");
        assert_eq!(yuu.notes.as_deref(), Some("28 ปี"));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// LLM callers send `""` for params they don't use — a blank id/query must not
    /// filter everything out (the "read 0 character(s)" bug).
    #[test]
    fn get_ignores_blank_id_and_query() {
        let (base, ws) = temp_ws("get_blank");
        upsert(&ws, ch("yuu", "有月勇", "อาริทสึกิ ยู", Some("Aritsuki Yuu"))).unwrap();
        upsert(&ws, ch("miya", "みや", "มิยะ", Some("Miya"))).unwrap();

        assert_eq!(get(&ws, Some(""), Some("")).len(), 2);
        assert_eq!(get(&ws, Some("勇"), Some("  ")).len(), 1);
        assert_eq!(get(&ws, None, None).len(), 2);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A name passed as `id` with no exact id hit degrades to a substring match.
    #[test]
    fn get_id_falls_back_to_name_match() {
        let (base, ws) = temp_ws("get_id_fallback");
        upsert(&ws, ch("yuu", "有月勇", "อาริทสึกิ ยู", Some("Aritsuki Yuu"))).unwrap();

        let hits = get(&ws, None, Some("有月勇"));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "yuu");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Queries with spaces/separators match stored names without them (桐島 朱夏 → 桐島朱夏).
    #[test]
    fn get_query_is_whitespace_insensitive() {
        let (base, ws) = temp_ws("get_spaced");
        upsert(
            &ws,
            ch("shuka", "桐島朱夏", "คิริชิมะ ชูคะ", Some("Kirishima Shuka")),
        )
        .unwrap();

        assert_eq!(get(&ws, Some("桐島 朱夏"), None).len(), 1);
        assert_eq!(get(&ws, Some("kirishima shuka"), None).len(), 1);
        assert_eq!(get(&ws, Some("ชูคะ"), None).len(), 1);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The Orchestrator searches "by reading": kana queries must reach the stored
    /// romaji, and romaji queries must reach kana-only names.
    #[test]
    fn get_matches_kana_reading_against_romaji() {
        let (base, ws) = temp_ws("get_reading");
        upsert(&ws, ch("nonoka", "乃々香", "โนโนกะ", Some("Nonoka"))).unwrap();
        let mut sumomo = ch("sumomo", "日比野すもも", "ฮิบิโนะ สึโมโมะ", None);
        sumomo.aliases = vec!["すもも".into()];
        upsert(&ws, sumomo).unwrap();

        assert_eq!(get(&ws, Some("ののか"), None).len(), 1, "hiragana reading");
        assert_eq!(get(&ws, Some("ノノカ"), None).len(), 1, "katakana reading");
        assert_eq!(
            get(&ws, Some("sumomo"), None).len(),
            1,
            "romaji vs kana name"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A surface form with an honorific still finds the bare stored name.
    #[test]
    fn get_matches_query_with_honorific() {
        let (base, ws) = temp_ws("get_honorific");
        let mut shuka = ch("shuka", "桐島朱夏", "คิริชิมะ ชูคะ", Some("Kirishima Shuka"));
        shuka.aliases = vec!["朱夏".into()];
        upsert(&ws, shuka).unwrap();
        upsert(&ws, ch("naruse", "成瀬", "นารุเสะ", Some("Naruse"))).unwrap();

        assert_eq!(get(&ws, Some("朱夏ちゃん"), None).len(), 1);
        assert_eq!(get(&ws, Some("桐島朱夏さん"), None).len(), 1);
        assert_eq!(get(&ws, Some("成瀬先生"), None).len(), 1);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Old CHARACTERS.md without an `aliases` field loads, and empty aliases never serialize.
    #[test]
    fn serde_back_compat_for_aliases() {
        let json = r#"{"characters":[{"id":"a","jp_name":"A","thai_name":"เอ"}]}"#;
        let block: CharactersBlock = serde_json::from_str(json).unwrap();
        assert_eq!(block.characters.len(), 1);
        assert!(block.characters[0].aliases.is_empty());

        let round = serde_json::to_string(&block).unwrap();
        assert!(
            !round.contains("aliases"),
            "empty aliases must be skipped on serialize: {round}"
        );
    }
}
