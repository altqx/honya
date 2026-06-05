//! Read/update VOLUME.md. The data block is the full `VolumeData` JSON; the
//! Markdown body above it is re-rendered from that payload on every write.

use crate::model::{ContinuityNote, UsageStats, VolumeData};
use crate::workspace::Workspace;
use crate::workspace::data_block;

/// Load the volume metadata (defaults when VOLUME.md is absent/empty).
pub fn load(ws: &Workspace) -> VolumeData {
    data_block::read_data_block(&ws.volume_md())
}

/// Set the volume synopsis (raw source + Thai translation) and persist.
pub fn set_synopsis(ws: &Workspace, raw: &str, thai: &str) -> std::io::Result<()> {
    let mut data = load(ws);
    data.synopsis_raw = raw.trim().to_string();
    data.synopsis_th = thai.trim().to_string();
    write(ws, &data)
}

/// Replace the running recap and persist.
pub fn set_recap(ws: &Workspace, recap: &str) -> std::io::Result<()> {
    let mut data = load(ws);
    data.running_recap = recap.trim().to_string();
    write(ws, &data)
}

/// Set (or overwrite) a single chapter's one-line summary and persist.
pub fn set_chapter_summary(ws: &Workspace, chapter: u32, summary: &str) -> std::io::Result<()> {
    let mut data = load(ws);
    data.chapters
        .insert(chapter.to_string(), summary.trim().to_string());
    write(ws, &data)
}

/// Append a continuity note and persist.
pub fn add_note(ws: &Workspace, note: ContinuityNote) -> std::io::Result<()> {
    let mut data = load(ws);
    data.notes.push(note);
    write(ws, &data)
}

/// Add one run's usage `delta` to a chapter's cumulative lifetime total and
/// persist. The volume total is the sum over all chapters; the project total is
/// the sum over all volumes — neither is stored separately, so they never drift.
pub fn add_chapter_usage(ws: &Workspace, chapter: u32, delta: &UsageStats) -> std::io::Result<()> {
    let mut data = load(ws);
    data.chapter_usage
        .entry(chapter.to_string())
        .or_default()
        .add(delta);
    write(ws, &data)
}

/// Render the human-readable Markdown body for VOLUME.md.
pub fn render_body(data: &VolumeData) -> String {
    let mut s = String::new();
    s.push_str("# บันทึกเล่ม / Volume Notes\n\n");

    // Volume synopsis (เรื่องย่อ): Thai used as context; raw source kept for reference/reroll.
    s.push_str("## เรื่องย่อเล่ม / Volume Synopsis\n\n");
    if data.synopsis_th.trim().is_empty() && data.synopsis_raw.trim().is_empty() {
        s.push_str("_ยังไม่มีเรื่องย่อ_\n");
    } else {
        if !data.synopsis_th.trim().is_empty() {
            s.push_str(data.synopsis_th.trim());
            s.push('\n');
        }
        if !data.synopsis_raw.trim().is_empty() {
            s.push_str("\n<details><summary>ต้นฉบับ / Source</summary>\n\n");
            s.push_str(data.synopsis_raw.trim());
            s.push_str("\n</details>\n");
        }
    }
    s.push('\n');

    s.push_str("## เนื้อเรื่องสะสม (Running Recap)\n\n");
    if data.running_recap.trim().is_empty() {
        s.push_str("_ยังไม่มีสรุปเนื้อเรื่อง_\n");
    } else {
        s.push_str(data.running_recap.trim());
        s.push('\n');
    }
    s.push('\n');

    // Sorted numerically by chapter key.
    s.push_str("## สรุปรายบท (Chapter Summaries)\n\n");
    if data.chapters.is_empty() {
        s.push_str("_ยังไม่มีสรุปรายบท_\n");
    } else {
        let mut entries: Vec<(&String, &String)> = data.chapters.iter().collect();
        entries.sort_by_key(|(k, _)| k.parse::<u64>().unwrap_or(u64::MAX));
        s.push_str("| บท | สรุป |\n");
        s.push_str("|----|------|\n");
        for (chapter, summary) in entries {
            s.push_str(&format!("| {} | {} |\n", cell(chapter), cell(summary)));
        }
    }
    s.push('\n');

    // Usage & cost, per chapter, with a volume total row.
    s.push_str("## การใช้งาน/ค่าใช้จ่าย (Usage & Cost)\n\n");
    if data.chapter_usage.values().all(UsageStats::is_zero) {
        s.push_str("_ยังไม่มีการใช้งาน_\n");
    } else {
        let mut entries: Vec<(&String, &UsageStats)> = data.chapter_usage.iter().collect();
        entries.sort_by_key(|(k, _)| k.parse::<u64>().unwrap_or(u64::MAX));
        s.push_str("| บท | tokens (in/out/total) | tool calls | ค่าใช้จ่าย (USD) |\n");
        s.push_str("|----|-----------------------|-----------:|----------------:|\n");
        let mut total = UsageStats::default();
        for (chapter, u) in entries {
            total.add(u);
            s.push_str(&format!(
                "| {} | {}/{}/{} | {} | ${:.4} |\n",
                cell(chapter),
                u.tokens.prompt,
                u.tokens.completion,
                u.tokens.total,
                u.tool_calls,
                u.cost_usd,
            ));
        }
        s.push_str(&format!(
            "| **รวม/Total** | {}/{}/{} | {} | ${:.4} |\n",
            total.tokens.prompt,
            total.tokens.completion,
            total.tokens.total,
            total.tool_calls,
            total.cost_usd,
        ));
    }
    s.push('\n');

    s.push_str("## บันทึกความต่อเนื่อง (Continuity Notes)\n\n");
    if data.notes.is_empty() {
        s.push_str("_ไม่มีประเด็นความต่อเนื่อง_\n");
    } else {
        s.push_str("| บท | ระดับ | ประเภท | บันทึก |\n");
        s.push_str("|----|-------|--------|--------|\n");
        for note in &data.notes {
            s.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                note.chapter
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "—".to_string()),
                cell(&note.severity),
                note.kind
                    .as_deref()
                    .map(cell)
                    .unwrap_or_else(|| "—".to_string()),
                cell(&note.note),
            ));
        }
    }

    s
}

fn write(ws: &Workspace, data: &VolumeData) -> std::io::Result<()> {
    let body = render_body(data);
    data_block::write_with_data(&ws.volume_md(), &body, data)
}

fn cell(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return "—".to_string();
    }
    trimmed.replace('|', "\\|").replace('\n', " ")
}
