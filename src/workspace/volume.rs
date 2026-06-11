//! Read/update VOLUME.md. The data block is the full `VolumeData` JSON; the
//! Markdown body above it is re-rendered from that payload on every write.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use crate::model::{
    ChapterRun, ContinuityNote, ReaderAnnotation, ReaderBookmark, RunHistoryEntry,
    RunHistoryStatus, StyleExample, UsageStats, VolumeData,
};
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

/// Replace source-file metadata discovered during import and persist.
pub fn set_source_metadata(
    ws: &Workspace,
    metadata: BTreeMap<String, String>,
) -> std::io::Result<()> {
    if metadata.is_empty() {
        return Ok(());
    }
    let mut data = load(ws);
    data.source_metadata = metadata;
    write(ws, &data)
}

/// Mark the pre-extraction pass as done (or not) for this volume and persist.
pub fn set_prepass_done(ws: &Workspace, done: bool) -> std::io::Result<()> {
    let mut data = load(ws);
    data.prepass_done = done;
    write(ws, &data)
}

/// Merge freshly-extracted style exemplars into the volume's set (de-duplicated on
/// the JP side, capped) and persist. Hand-edited entries are preserved.
pub fn add_style_examples(ws: &Workspace, examples: Vec<StyleExample>) -> std::io::Result<()> {
    const MAX_STYLE_EXAMPLES: usize = 6;
    let mut data = load(ws);
    for ex in examples {
        let jp = ex.jp.trim();
        let th = ex.th.trim();
        if jp.is_empty() || th.is_empty() {
            continue;
        }
        if data.style_examples.iter().any(|e| e.jp.trim() == jp) {
            continue;
        }
        data.style_examples.push(StyleExample {
            jp: jp.to_string(),
            th: th.to_string(),
            note: ex.note.filter(|n| !n.trim().is_empty()),
        });
    }
    data.style_examples.truncate(MAX_STYLE_EXAMPLES);
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

/// Toggle a navigation bookmark at `chapter`/`line`: remove an existing bookmark on
/// that exact line, otherwise add one. Returns `true` when a bookmark was added,
/// `false` when one was removed — the caller surfaces the right toast. The `line` is
/// normalized to 1-based to match annotation anchoring.
pub fn toggle_reader_bookmark(
    ws: &Workspace,
    chapter: u32,
    line: u32,
    label: &str,
) -> std::io::Result<bool> {
    let line = line.max(1);
    let mut data = load(ws);
    if let Some(pos) = data
        .bookmarks
        .iter()
        .position(|b| b.chapter == chapter && b.line == line)
    {
        data.bookmarks.remove(pos);
        write(ws, &data)?;
        return Ok(false);
    }
    data.bookmarks.push(ReaderBookmark {
        chapter,
        line,
        label: label.trim().to_string(),
        created_at: Some(Utc::now()),
    });
    write(ws, &data)?;
    Ok(true)
}

/// Return every bookmark in the volume, sorted by (chapter, line) for the picker.
pub fn reader_bookmarks(ws: &Workspace) -> Vec<ReaderBookmark> {
    let mut bookmarks = load(ws).bookmarks;
    bookmarks.sort_by_key(|b| (b.chapter, b.line, b.created_at));
    bookmarks
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

/// Per-chapter run records (for rerun comparison) retained per chapter. Older
/// versions' archived Thai is deleted when trimmed.
const MAX_RUNS_PER_CHAPTER: usize = 5;

/// Append a finished chapter's per-run record (cost / QA / glossary deltas) and
/// persist. Trims to the most recent [`MAX_RUNS_PER_CHAPTER`] per chapter, deleting
/// the archived Thai of any run that falls off the back.
pub fn record_chapter_run(ws: &Workspace, run: ChapterRun) -> std::io::Result<()> {
    let mut data = load(ws);
    data.chapter_runs.push(run);
    trim_chapter_runs(ws, &mut data);
    write(ws, &data)
}

/// This chapter's run records, oldest → newest. The newest record without an
/// `archived` path is the live version; the newest record *with* one points at the
/// most recently displaced version, which the Reader diffs against.
pub fn chapter_runs(ws: &Workspace, chapter: u32) -> Vec<ChapterRun> {
    let mut runs: Vec<ChapterRun> = load(ws)
        .chapter_runs
        .into_iter()
        .filter(|r| r.chapter == chapter)
        .collect();
    runs.sort_by_key(|r| r.finished_at);
    runs
}

/// Best-effort facts about the version being archived, used to synthesize a run
/// record for a translation that predates per-run recording.
pub struct PriorVersion {
    pub finished_at: DateTime<Utc>,
    pub review_needed: u32,
    pub failed: bool,
    pub total_chunks: u32,
    pub committed_chunks: u32,
}

/// Mark that `rel_path` (relative to the volume dir) now holds the archived Thai of
/// a chapter's current, about-to-be-overwritten version. Attaches the path to that
/// version's existing run record; if none exists (a translation made before this
/// feature), synthesizes a `"(prior)"` record so the text diff still works (its
/// per-run cost shows as n/a). Call this right before `reset_chapter`.
pub fn archive_prev_version(
    ws: &Workspace,
    chapter: u32,
    rel_path: &str,
    prior: PriorVersion,
) -> std::io::Result<()> {
    let mut data = load(ws);
    let latest = data
        .chapter_runs
        .iter_mut()
        .filter(|r| r.chapter == chapter && r.archived.is_none())
        .max_by_key(|r| r.finished_at);
    match latest {
        Some(run) => run.archived = Some(rel_path.to_string()),
        None => data.chapter_runs.push(ChapterRun {
            chapter,
            run_id: "(prior)".to_string(),
            finished_at: prior.finished_at,
            usage: UsageStats::default(),
            usage_unknown: true,
            review_needed: prior.review_needed,
            failed: prior.failed,
            total_chunks: prior.total_chunks,
            committed_chunks: prior.committed_chunks,
            glossary_added: Vec::new(),
            glossary_changed: Vec::new(),
            archived: Some(rel_path.to_string()),
        }),
    }
    trim_chapter_runs(ws, &mut data);
    write(ws, &data)
}

/// Keep only the newest [`MAX_RUNS_PER_CHAPTER`] records per chapter, deleting the
/// archived Thai files of the runs that fall off so `reruns/` cannot grow without
/// bound.
fn trim_chapter_runs(ws: &Workspace, data: &mut VolumeData) {
    let mut per_chapter: std::collections::BTreeMap<u32, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (i, r) in data.chapter_runs.iter().enumerate() {
        per_chapter.entry(r.chapter).or_default().push(i);
    }
    let mut remove: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for idxs in per_chapter.values() {
        if idxs.len() > MAX_RUNS_PER_CHAPTER {
            let mut by_age = idxs.clone();
            by_age.sort_by_key(|&i| data.chapter_runs[i].finished_at);
            for &i in by_age.iter().take(idxs.len() - MAX_RUNS_PER_CHAPTER) {
                remove.insert(i);
            }
        }
    }
    if remove.is_empty() {
        return;
    }
    for &i in &remove {
        if let Some(rel) = &data.chapter_runs[i].archived {
            let _ = std::fs::remove_file(ws.vol_rel(rel));
        }
    }
    let kept: Vec<ChapterRun> = std::mem::take(&mut data.chapter_runs)
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !remove.contains(i))
        .map(|(_, r)| r)
        .collect();
    data.chapter_runs = kept;
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

    if !data.style_examples.is_empty() {
        s.push_str("## ตัวอย่างสำนวน / Style Examples\n\n");
        s.push_str("| 日本語 | ไทย | หมายเหตุ |\n");
        s.push_str("|--------|-----|----------|\n");
        for ex in &data.style_examples {
            s.push_str(&format!(
                "| {} | {} | {} |\n",
                cell(&ex.jp),
                cell(&ex.th),
                ex.note
                    .as_deref()
                    .map(cell)
                    .unwrap_or_else(|| "—".to_string()),
            ));
        }
        s.push('\n');
    }

    if !data.source_metadata.is_empty() {
        s.push_str("## ข้อมูลต้นฉบับ / Source Metadata\n\n");
        s.push_str("| Field | Value |\n");
        s.push_str("|-------|-------|\n");
        for (key, value) in &data.source_metadata {
            s.push_str(&format!("| {} | {} |\n", cell(key), cell(value)));
        }
        s.push('\n');
    }

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
        annotations
            .sort_by_key(|annotation| (annotation.chapter, annotation.line, annotation.created_at));
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
    s.push('\n');

    s.push_str("## ที่คั่นหน้า (Bookmarks)\n\n");
    if data.bookmarks.is_empty() {
        s.push_str("_ยังไม่มีที่คั่นหน้า_\n");
    } else {
        let mut bookmarks: Vec<&ReaderBookmark> = data.bookmarks.iter().collect();
        bookmarks.sort_by_key(|b| (b.chapter, b.line, b.created_at));
        s.push_str("| บท | บรรทัด | ข้อความ |\n");
        s.push_str("|----|---------:|--------|\n");
        for bookmark in bookmarks {
            s.push_str(&format!(
                "| {} | {} | {} |\n",
                bookmark.chapter,
                bookmark.line,
                cell(&bookmark.label),
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
    use crate::model::{ReaderAnnotation, TokenUsage, UsageStats};

    fn temp_ws(tag: &str) -> (std::path::PathBuf, Workspace) {
        let base = std::env::temp_dir().join(format!("honya_volume_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let ws = Workspace::new(base.clone(), 1);
        std::fs::create_dir_all(&ws.vol_dir).unwrap();
        (base, ws)
    }

    #[test]
    fn synopsis_is_scoped_per_volume() {
        // Two volumes under one project root must keep independent synopses, so the
        // add-volume flow's per-volume synopsis never bleeds into another volume.
        let root = std::env::temp_dir().join(format!("honya_vol_syn_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let v1 = Workspace::new(root.clone(), 1);
        let v2 = Workspace::new(root.clone(), 2);
        std::fs::create_dir_all(&v1.vol_dir).unwrap();
        std::fs::create_dir_all(&v2.vol_dir).unwrap();

        set_synopsis(&v1, "一巻の原文", "เล่มหนึ่ง").unwrap();
        set_synopsis(&v2, "二巻の原文", "เล่มสอง").unwrap();

        assert_eq!(load(&v1).synopsis_raw, "一巻の原文");
        assert_eq!(load(&v1).synopsis_th, "เล่มหนึ่ง");
        assert_eq!(load(&v2).synopsis_raw, "二巻の原文");
        assert_eq!(load(&v2).synopsis_th, "เล่มสอง");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reader_annotations_round_trip_and_render() {
        let (base, ws) = temp_ws("annotations");
        add_reader_annotation(
            &ws,
            ReaderAnnotation {
                chapter: 2,
                line: 0,
                note: "  check honorific  ".to_string(),
                created_at: Some(Utc::now()),
            },
        )
        .unwrap();
        add_reader_annotation(
            &ws,
            ReaderAnnotation {
                chapter: 2,
                line: 8,
                note: "".to_string(),
                created_at: Some(Utc::now()),
            },
        )
        .unwrap();

        let annotations = reader_annotations(&ws, 2);
        assert_eq!(annotations.len(), 1, "empty notes are ignored");
        assert_eq!(annotations[0].line, 1, "line anchors are 1-based");
        assert_eq!(annotations[0].note, "check honorific");

        let body = std::fs::read_to_string(ws.volume_md()).unwrap();
        assert!(body.contains("Reader Annotations"));
        assert!(body.contains("check honorific"));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn reader_bookmarks_toggle_and_render() {
        let (base, ws) = temp_ws("bookmarks");

        // First toggle adds; the helper reports `true` (added).
        assert!(toggle_reader_bookmark(&ws, 3, 0, "  สวัสดี  ").unwrap());
        let bookmarks = reader_bookmarks(&ws);
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].line, 1, "line anchors are 1-based");
        assert_eq!(bookmarks[0].label, "สวัสดี", "label is trimmed");

        let body = std::fs::read_to_string(ws.volume_md()).unwrap();
        assert!(body.contains("Bookmarks"));
        assert!(body.contains("สวัสดี"));

        // A second toggle on the same (normalized) line removes it and reports `false`.
        assert!(!toggle_reader_bookmark(&ws, 3, 1, "ignored").unwrap());
        assert!(
            reader_bookmarks(&ws).is_empty(),
            "toggle removes the bookmark"
        );

        let _ = std::fs::remove_dir_all(&base);
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

    fn chapter_run(chapter: u32, id: &str, secs: i64, cost: f64) -> ChapterRun {
        ChapterRun {
            chapter,
            run_id: id.to_string(),
            finished_at: DateTime::<Utc>::from_timestamp(secs, 0).unwrap(),
            usage: UsageStats {
                cost_usd: cost,
                ..Default::default()
            },
            usage_unknown: false,
            review_needed: 0,
            failed: false,
            total_chunks: 3,
            committed_chunks: 3,
            glossary_added: Vec::new(),
            glossary_changed: Vec::new(),
            archived: None,
        }
    }

    #[test]
    fn chapter_runs_filter_sort_and_trim_deletes_archives() {
        let (base, ws) = temp_ws("chapter_runs");
        // Six runs for ch 3 (cap is 5) plus one unrelated ch 4 run.
        for i in 0..6 {
            let mut run = chapter_run(3, &format!("r{i}"), 1_000 + i, 0.01 * (i as f64 + 1.0));
            // Give the two oldest an archived file on disk to prove trim removes them.
            if i < 2 {
                let rel = format!("reruns/ch_003/r{i}.md");
                let abs = ws.vol_rel(&rel);
                std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
                std::fs::write(&abs, "old").unwrap();
                run.archived = Some(rel);
            }
            record_chapter_run(&ws, run).unwrap();
        }
        record_chapter_run(&ws, chapter_run(4, "other", 2_000, 0.5)).unwrap();

        let runs = chapter_runs(&ws, 3);
        assert_eq!(runs.len(), 5, "trimmed to MAX_RUNS_PER_CHAPTER");
        // Newest-by-finished_at retained, oldest dropped → r1..=r5 survive (r0 gone).
        assert_eq!(runs.first().unwrap().run_id, "r1");
        assert_eq!(runs.last().unwrap().run_id, "r5");
        // The dropped oldest run's archive file was deleted; the surviving one stays.
        assert!(!ws.vol_rel("reruns/ch_003/r0.md").exists());
        assert!(ws.vol_rel("reruns/ch_003/r1.md").exists());
        // Other chapters are untouched.
        assert_eq!(chapter_runs(&ws, 4).len(), 1);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn archive_prev_attaches_to_latest_then_synthesizes() {
        let (base, ws) = temp_ws("archive_prev");
        // A real recorded run exists → archiving attaches the path to it.
        record_chapter_run(&ws, chapter_run(1, "real", 10, 0.04)).unwrap();
        archive_prev_version(
            &ws,
            1,
            "reruns/ch_001/real.md",
            PriorVersion {
                finished_at: DateTime::<Utc>::from_timestamp(10, 0).unwrap(),
                review_needed: 0,
                failed: false,
                total_chunks: 3,
                committed_chunks: 3,
            },
        )
        .unwrap();
        let runs = chapter_runs(&ws, 1);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].archived.as_deref(), Some("reruns/ch_001/real.md"));
        assert!(!runs[0].usage_unknown);

        // A chapter with no recorded run (pre-feature) → a "(prior)" stub is made.
        archive_prev_version(
            &ws,
            2,
            "reruns/ch_002/prior.md",
            PriorVersion {
                finished_at: DateTime::<Utc>::from_timestamp(5, 0).unwrap(),
                review_needed: 2,
                failed: false,
                total_chunks: 4,
                committed_chunks: 4,
            },
        )
        .unwrap();
        let runs = chapter_runs(&ws, 2);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, "(prior)");
        assert!(runs[0].usage_unknown);
        assert_eq!(runs[0].review_needed, 2);
        assert_eq!(runs[0].archived.as_deref(), Some("reruns/ch_002/prior.md"));

        let _ = std::fs::remove_dir_all(&base);
    }
}
