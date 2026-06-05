//! src/workspace/volume.rs — read/update VOLUME.md.
//!
//! The data block is the full `VolumeData` JSON (`running_recap`, per-chapter
//! `chapters` map keyed by `chapter.to_string()`, and continuity `notes`). The
//! Markdown body above it is re-rendered from that payload on every write.

use crate::model::{ContinuityNote, VolumeData};
use crate::workspace::Workspace;
use crate::workspace::data_block;

/// Load the volume metadata (defaults when VOLUME.md is absent/empty).
pub fn load(ws: &Workspace) -> VolumeData {
    data_block::read_data_block(&ws.volume_md())
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

/// Render the human-readable Markdown body for VOLUME.md.
pub fn render_body(data: &VolumeData) -> String {
    let mut s = String::new();
    s.push_str("# บันทึกเล่ม / Volume Notes\n\n");

    // Running recap.
    s.push_str("## เนื้อเรื่องสะสม (Running Recap)\n\n");
    if data.running_recap.trim().is_empty() {
        s.push_str("_ยังไม่มีสรุปเนื้อเรื่อง_\n");
    } else {
        s.push_str(data.running_recap.trim());
        s.push('\n');
    }
    s.push('\n');

    // Per-chapter summaries (sorted numerically by chapter key).
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

    // Continuity notes.
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

// --- helpers ----------------------------------------------------------------

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
