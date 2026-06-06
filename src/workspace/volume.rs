//! Read/update VOLUME.md. The data block is the full `VolumeData` JSON; the
//! Markdown body above it is re-rendered from that payload on every write.

use chrono::{DateTime, Utc};

use crate::model::{
    ContinuityNote, ReaderAnnotation, RunHistoryEntry, RunHistoryStatus, UsageStats, VolumeData,
};
use crate::workspace::data_block;
use crate::workspace::Workspace;

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

/// Append a human Reader annotation and persist. Empty notes are ignored so an
/// accidental Enter in the editor never dirties VOLUME.md.
pub fn add_reader_annotation(
    ws: &Workspace,
    mut annotation: ReaderAnnotation,
) -> std::io::Result<()> {
    let note = annotation.note.trim();
    if note.is_empty() {
        return Ok(());
    }
    annotation.note = note.to_string();
    annotation.line = annotation.line.max(1);

    let mut data = load(ws);
    data.annotations.push(annotation);
    write(ws, &data)
}

/// Return this chapter's Reader annotations in stable on-page order.
pub fn reader_annotations(ws: &Workspace, chapter: u32) -> Vec<ReaderAnnotation> {
    let mut annotations: Vec<ReaderAnnotation> = load(ws)
        .annotations
        .into_iter()
        .filter(|annotation| annotation.chapter == chapter)
        .collect();
    annotations.sort_by_key(|annotation| (annotation.line, annotation.created_at));
    annotations
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

/// Keep at most this many durable run rows. The newest rows are retained; older
/// entries are already reflected in the cumulative chapter usage totals.
const MAX_RUN_HISTORY: usize = 100;

/// Upsert a `running` history row when a translation run starts (or resumes from
/// an older checkpoint whose row is missing). The row shares its id with the
/// crash-recovery checkpoint.
pub fn record_run_started(ws: &Workspace, run: RunHistoryEntry) -> std::io::Result<()> {
    let mut data = load(ws);
    if let Some(existing) = data.run_history.iter_mut().find(|r| r.id == run.id) {
        existing.status = RunHistoryStatus::Running;
        existing.finished_at = None;
        existing.chapters = run.chapters;
        if existing.honya_version.trim().is_empty() {
            existing.honya_version = run.honya_version;
        }
    } else {
        data.run_history.push(run);
    }
    trim_run_history(&mut data.run_history);
    write(ws, &data)
}

/// Terminal summary used to close out a durable run-history row.
pub struct RunHistoryFinish {
    pub status: RunHistoryStatus,
    pub finished_at: DateTime<Utc>,
    /// Chapter queue for fallback rows (for example a crash between checkpoint
    /// write and run-history start write).
    pub chapters: Vec<u32>,
    pub chapters_done: u32,
    pub chapters_failed: u32,
    pub chapters_need_review: u32,
    pub usage: UsageStats,
}

/// Finalize a run-history row when the pipeline reports its terminal summary.
pub fn record_run_finished(
    ws: &Workspace,
    run_id: &str,
    finish: RunHistoryFinish,
) -> std::io::Result<()> {
    let mut data = load(ws);
    if let Some(existing) = data.run_history.iter_mut().find(|r| r.id == run_id) {
        existing.finished_at = Some(finish.finished_at);
        existing.status = finish.status;
        if existing.chapters.is_empty() && !finish.chapters.is_empty() {
            existing.chapters = finish.chapters;
        }
        existing.chapters_done = finish.chapters_done;
        existing.chapters_failed = finish.chapters_failed;
        existing.chapters_need_review = finish.chapters_need_review;
        // Recovery cleanup may not know the final run total; do not erase a
        // nonzero value if a future progress writer already populated it.
        if !finish.usage.is_zero() || existing.usage.is_zero() {
            existing.usage = finish.usage;
        }
    } else {
        data.run_history.push(RunHistoryEntry {
            id: run_id.to_string(),
            started_at: finish.finished_at,
            finished_at: Some(finish.finished_at),
            status: finish.status,
            chapters: finish.chapters,
            chapters_done: finish.chapters_done,
            chapters_failed: finish.chapters_failed,
            chapters_need_review: finish.chapters_need_review,
            usage: finish.usage,
            honya_version: crate::update::current_version().to_string(),
        });
    }
    trim_run_history(&mut data.run_history);
    write(ws, &data)
}

/// Mark an interrupted run as intentionally abandoned when the user chooses
/// "discard" in the recovery prompt.
pub fn record_run_discarded(ws: &Workspace, run_id: &str) -> std::io::Result<()> {
    let mut data = load(ws);
    if let Some(existing) = data.run_history.iter_mut().find(|r| r.id == run_id) {
        existing.finished_at = Some(Utc::now());
        existing.status = RunHistoryStatus::Discarded;
        write(ws, &data)
    } else {
        Ok(())
    }
}

fn trim_run_history(history: &mut Vec<RunHistoryEntry>) {
    if history.len() > MAX_RUN_HISTORY {
        let overflow = history.len() - MAX_RUN_HISTORY;
        history.drain(0..overflow);
    }
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

    s.push_str("## ประวัติรัน (Run History)\n\n");
    if data.run_history.is_empty() {
        s.push_str("_ยังไม่มีประวัติรัน_\n");
    } else {
        s.push_str("| เริ่ม | จบ | สถานะ | บท | ผลลัพธ์ | usage |\n");
        s.push_str("|------|----|--------|----|---------|-------|\n");
        for run in data.run_history.iter().rev().take(20) {
            s.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                cell(&short_time(run.started_at)),
                cell(
                    &run.finished_at
                        .map(short_time)
                        .unwrap_or_else(|| "—".to_string())
                ),
                cell(run_status_label(run.status)),
                cell(&chapters_cell(&run.chapters)),
                cell(&format!(
                    "{} done · {} failed · {} review",
                    run.chapters_done, run.chapters_failed, run.chapters_need_review
                )),
                cell(&format!(
                    "{} tok · {} tools · ${:.4}",
                    run.usage.tokens.total, run.usage.tool_calls, run.usage.cost_usd
                )),
            ));
        }
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
    s.push('\n');

    s.push_str("## บันทึกผู้อ่าน (Reader Annotations)\n\n");
    if data.annotations.is_empty() {
        s.push_str("_ยังไม่มีโน้ตพิสูจน์อักษร_\n");
    } else {
        let mut annotations: Vec<&ReaderAnnotation> = data.annotations.iter().collect();
        annotations.sort_by_key(|annotation| (annotation.chapter, annotation.line, annotation.created_at));
        s.push_str("| บท | บรรทัด | เวลา | บันทึก |\n");
        s.push_str("|----|---------:|------|--------|\n");
        for annotation in annotations {
            let created = annotation
                .created_at
                .map(short_time)
                .unwrap_or_else(|| "—".to_string());
            s.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                annotation.chapter,
                annotation.line,
                cell(&created),
                cell(&annotation.note),
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

fn short_time(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M").to_string()
}

fn run_status_label(status: RunHistoryStatus) -> &'static str {
    match status {
        RunHistoryStatus::Running => "running",
        RunHistoryStatus::Completed => "completed",
        RunHistoryStatus::NeedsReview => "needs review",
        RunHistoryStatus::Partial => "partial",
        RunHistoryStatus::Failed => "failed",
        RunHistoryStatus::Stopped => "stopped",
        RunHistoryStatus::Discarded => "discarded",
    }
}

fn chapters_cell(chapters: &[u32]) -> String {
    if chapters.is_empty() {
        return "—".to_string();
    }
    if chapters.len() <= 6 {
        return chapters
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ");
    }
    let head = chapters
        .iter()
        .take(5)
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    format!("{head}, … (+{})", chapters.len() - 5)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{TokenUsage, UsageStats};

    fn temp_ws(tag: &str) -> (std::path::PathBuf, Workspace) {
        let base = std::env::temp_dir().join(format!("honya_volume_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let ws = Workspace::new(base.clone(), 1);
        std::fs::create_dir_all(&ws.vol_dir).unwrap();
        (base, ws)
    }

    #[test]
    fn run_history_start_finish_and_discard_round_trip() {
        let (base, ws) = temp_ws("history");
        let started_at = Utc::now();
        let run = RunHistoryEntry::started(
            "run-test".to_string(),
            started_at,
            vec![1, 2, 5],
            "0.test".to_string(),
        );

        record_run_started(&ws, run).unwrap();
        let data = load(&ws);
        assert_eq!(data.run_history.len(), 1);
        assert_eq!(data.run_history[0].status, RunHistoryStatus::Running);
        assert!(render_body(&data).contains("Run History"));

        let usage = UsageStats {
            tokens: TokenUsage {
                prompt: 10,
                completion: 20,
                total: 30,
            },
            cost_usd: 0.0123,
            tool_calls: 2,
        };
        record_run_finished(
            &ws,
            "run-test",
            RunHistoryFinish {
                status: RunHistoryStatus::NeedsReview,
                finished_at: Utc::now(),
                chapters: vec![1, 2, 5],
                chapters_done: 2,
                chapters_failed: 1,
                chapters_need_review: 1,
                usage,
            },
        )
        .unwrap();
        let data = load(&ws);
        let row = &data.run_history[0];
        assert_eq!(row.status, RunHistoryStatus::NeedsReview);
        assert_eq!(row.chapters_done, 2);
        assert_eq!(row.chapters_failed, 1);
        assert_eq!(row.chapters_need_review, 1);
        assert_eq!(row.usage.tokens.total, 30);
        assert!(row.finished_at.is_some());

        record_run_discarded(&ws, "run-test").unwrap();
        let data = load(&ws);
        assert_eq!(data.run_history[0].status, RunHistoryStatus::Discarded);

        let _ = std::fs::remove_dir_all(&base);
    }
}
