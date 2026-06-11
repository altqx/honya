//! Write/append/read translated chapters. Approved Thai is appended chunk-by-chunk
//! behind a `<!-- honya:chunk N -->` marker; a repeated `N` is a no-op, so retries
//! and replays stay idempotent (risks.txt #11). Append/read are async (pipeline
//! task); write_raw/write_image_only are sync (import path).

use std::collections::BTreeSet;
use std::path::Path;

use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;

use crate::workspace::Workspace;

const CHUNK_MARKER_PREFIX: &str = "<!-- honya:chunk ";
const CHUNK_MARKER_SUFFIX: &str = " -->";

const TOTAL_MARKER_PREFIX: &str = "<!-- honya:chunks-total ";
const TOTAL_MARKER_SUFFIX: &str = " -->";

fn chunk_marker(n: u32) -> String {
    format!("{CHUNK_MARKER_PREFIX}{n}{CHUNK_MARKER_SUFFIX}")
}

fn total_marker(n: u32) -> String {
    format!("{TOTAL_MARKER_PREFIX}{n}{TOTAL_MARKER_SUFFIX}")
}

/// Parse a `<!-- honya:chunks-total N -->` line.
pub fn parse_total_marker(line: &str) -> Option<u32> {
    line.trim()
        .strip_prefix(TOTAL_MARKER_PREFIX)?
        .strip_suffix(TOTAL_MARKER_SUFFIX)?
        .trim()
        .parse()
        .ok()
}

/// The expected chunk count recorded by the pipeline at chunking time, if any.
/// Lets `scan::derive_status` tell a finished chapter from one whose run stopped
/// or failed partway (the last marker wins if the file somehow has several).
pub fn total_chunks_in(text: &str) -> Option<u32> {
    text.lines().filter_map(parse_total_marker).next_back()
}

/// Record the chapter's expected chunk count in its translated file so that,
/// after a stop or crash, the resting status can be derived as partial rather
/// than done. Idempotent; if a previous run recorded a different total (chunk
/// config changed), the old marker is replaced.
pub async fn record_total_chunks(ws: &Workspace, chapter: u32, total: u32) -> std::io::Result<()> {
    let path = ws.translated(chapter);
    let existing = tokio::fs::read_to_string(&path).await.unwrap_or_default();
    match total_chunks_in(&existing) {
        Some(n) if n == total => Ok(()),
        Some(_) => {
            let marker = total_marker(total);
            let mut out = String::with_capacity(existing.len());
            for (i, line) in existing.lines().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                if parse_total_marker(line).is_some() {
                    out.push_str(&marker);
                } else {
                    out.push_str(line);
                }
            }
            if existing.ends_with('\n') {
                out.push('\n');
            }
            tokio::fs::write(&path, out).await
        }
        None => {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await?;
            let mut block = total_marker(total);
            block.push_str("\n\n");
            file.write_all(block.as_bytes()).await?;
            file.flush().await
        }
    }
}

/// Parse a `<!-- honya:chunk N -->` line, returning its 0-based chunk index. Public
/// so the Reader can map a TH line back to the chunk it belongs to (source align).
pub fn parse_chunk_marker(line: &str) -> Option<u32> {
    let trimmed = line.trim();
    let n = trimmed
        .strip_prefix(CHUNK_MARKER_PREFIX)?
        .strip_suffix(CHUNK_MARKER_SUFFIX)?
        .trim();
    n.parse().ok()
}

/// Return every committed chunk index found in translated chapter text.
///
/// The pipeline uses this to resume a failed run without spending tokens on
/// chunks that already landed on disk. A set (rather than a count) matters: old
/// or hand-edited files can have gaps, and resume must skip only the chunks whose
/// exact marker is present.
pub fn committed_chunk_indices_in(text: &str) -> BTreeSet<u32> {
    text.lines().filter_map(parse_chunk_marker).collect()
}

/// Return chunk indices whose committed block still carries the review-needed
/// marker. These chunks are *not* clean for resume purposes: a later run should
/// retranslate them and replace the flagged block.
pub fn review_needed_chunk_indices_in(text: &str) -> BTreeSet<u32> {
    let mut out = BTreeSet::new();
    for (idx, start, end) in chunk_block_ranges(text) {
        if text[start..end].contains(REVIEW_NEEDED_MARKER) {
            out.insert(idx);
        }
    }
    out
}

/// For each chunk still flagged review-needed, return `(chunk_index, reason)` where
/// `reason` is the reviewer's final objection lifted from the in-file banner (empty
/// when the banner carries no reason line). Drives the QA panel; the chunk order is
/// document order, matching `review_needed_chunk_indices_in`.
pub fn review_needed_details_in(text: &str) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    for (idx, start, end) in chunk_block_ranges(text) {
        let block = &text[start..end];
        if block.contains(REVIEW_NEEDED_MARKER) {
            out.push((idx, extract_review_reason(block)));
        }
    }
    out
}

/// Strip the machine-only markers (`<!-- honya:chunk N -->` and the review-needed
/// marker) from a translated chapter, leaving just the readable prose. Used by the
/// Reader diff view so a rerun comparison shows text changes, not marker churn. The
/// visible `[REVIEW NEEDED]` banner is deliberately kept — its appearing/vanishing
/// across runs is meaningful diff signal. Collapses the blank-line runs the dropped
/// markers leave behind so the two sides line up.
pub fn prose_only(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_run = 0u32;
    for line in text.lines() {
        if parse_chunk_marker(line).is_some()
            || parse_total_marker(line).is_some()
            || line.trim() == REVIEW_NEEDED_MARKER
        {
            continue;
        }
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue; // collapse 2+ blank lines into one
            }
        } else {
            blank_run = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_matches('\n').to_string()
}

/// Like [`prose_only`], but also strips the visible `[REVIEW NEEDED]` banner so an
/// exported deliverable carries clean Thai with no internal QA scaffolding. The
/// banner is the contiguous blockquote introduced by the `**[REVIEW NEEDED]**` line
/// (its spacer/reason `>` lines follow); everything else — including any legitimate
/// blockquote the translator produced — is preserved. Used only by the export
/// module; the QA panel and Reader diff keep the banner via [`prose_only`].
pub fn export_prose(text: &str) -> String {
    let stripped = prose_only(text);
    let mut out = String::with_capacity(stripped.len());
    let mut in_banner = false;
    let mut blank_run = 0u32;
    for line in stripped.lines() {
        let t = line.trim_start();
        if t.starts_with('>') && t.contains("[REVIEW NEEDED]") {
            in_banner = true; // drop the banner's lead line …
            continue;
        }
        if in_banner {
            if t.starts_with('>') {
                continue; // … and its trailing spacer / reason blockquote lines
            }
            in_banner = false;
        }
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue; // collapse the blank run the dropped banner leaves behind
            }
        } else {
            blank_run = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_matches('\n').to_string()
}

/// Pull the reviewer's objection out of a review-needed banner block: the text after
/// the `เหตุผลจากผู้ตรวจ:` blockquote line, trimmed; empty when that line is absent.
/// Kept in lock-step with the banner `append_chunk_needs_review` writes.
fn extract_review_reason(block: &str) -> String {
    const KEY: &str = "เหตุผลจากผู้ตรวจ:";
    for line in block.lines() {
        if let Some(pos) = line.find(KEY) {
            return line[pos + KEY.len()..].trim().to_string();
        }
    }
    String::new()
}

fn chunk_block_ranges(text: &str) -> Vec<(u32, usize, usize)> {
    let mut starts = Vec::new();
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        if let Some(idx) = parse_chunk_marker(line) {
            starts.push((idx, offset));
        }
        offset += line.len();
    }
    let mut ranges = Vec::with_capacity(starts.len());
    for i in 0..starts.len() {
        let (idx, start) = starts[i];
        let end = starts.get(i + 1).map(|(_, s)| *s).unwrap_or(text.len());
        ranges.push((idx, start, end));
    }
    ranges
}

fn replace_review_needed_block(existing: &str, marker: &str, block: &str) -> Option<String> {
    for (_, start, end) in chunk_block_ranges(existing) {
        let existing_block = &existing[start..end];
        if existing_block.starts_with(marker) && existing_block.contains(REVIEW_NEEDED_MARKER) {
            let mut replaced = String::with_capacity(existing.len() - (end - start) + block.len());
            replaced.push_str(&existing[..start]);
            replaced.push_str(block);
            replaced.push_str(&existing[end..]);
            return Some(replaced);
        }
    }
    None
}

/// Machine-detectable marker embedded for chunks committed without passing
/// review. `scan::derive_status` looks for it to re-derive `NeedsReview` on
/// reopen, and a human can grep for it to find every spot that needs fixing.
pub const REVIEW_NEEDED_MARKER: &str = "<!-- honya:review-needed -->";

/// Append a pre-built `block` to `chapter`'s translated file, guarded by
/// `marker` for idempotency. Returns bytes written (0 when `marker` is already
/// present, i.e. a replay no-op).
async fn append_guarded(
    ws: &Workspace,
    chapter: u32,
    marker: &str,
    block: &str,
) -> std::io::Result<usize> {
    let path = ws.translated(chapter);

    // Idempotency: skip if the chunk's marker already exists, except when the
    // existing block is a prior review-needed commit. In that case a successful
    // retranslation must replace the flagged block so the chapter can become
    // clean/done again instead of staying NeedsReview forever.
    if let Ok(existing) = tokio::fs::read_to_string(&path).await
        && existing.contains(marker)
    {
        if let Some(replaced) = replace_review_needed_block(&existing, marker, block) {
            tokio::fs::write(&path, replaced).await?;
        }
        return Ok(0);
    }

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await?;
    let bytes = block.as_bytes();
    file.write_all(bytes).await?;
    file.flush().await?;
    Ok(bytes.len())
}

/// Append chunk `chunk_index` of `chapter`, guarded by its marker. Returns bytes
/// written (0 when the chunk was already present, i.e. a no-op).
pub async fn append_chunk(
    ws: &Workspace,
    chapter: u32,
    chunk_index: u32,
    thai_text: &str,
) -> std::io::Result<usize> {
    let marker = chunk_marker(chunk_index);

    // Trailing blank line keeps successive chunks separated.
    let mut block = String::with_capacity(thai_text.len() + marker.len() + 4);
    block.push_str(&marker);
    block.push('\n');
    block.push_str(thai_text.trim_end_matches('\n'));
    block.push_str("\n\n");

    append_guarded(ws, chapter, &marker, &block).await
}

/// Append the last (unapproved) attempt for a chunk that exhausted its review
/// retries, flagged so the chapter still completes and a human can find and fix
/// it later. Writes the chunk marker, the machine-readable `REVIEW_NEEDED_MARKER`,
/// and a visible Markdown banner (with the reviewer's final objection) above the
/// Thai. Idempotent on replay via the chunk marker, like `append_chunk`.
pub async fn append_chunk_needs_review(
    ws: &Workspace,
    chapter: u32,
    chunk_index: u32,
    thai_text: &str,
    attempts: u32,
    reason: &str,
) -> std::io::Result<usize> {
    let marker = chunk_marker(chunk_index);

    let mut block = String::new();
    block.push_str(&marker);
    block.push('\n');
    block.push_str(REVIEW_NEEDED_MARKER);
    block.push('\n');
    block.push_str(&format!(
        "> ⚠️ **[REVIEW NEEDED]** chunk {n} — แปลอัตโนมัติไม่ผ่านการตรวจหลัง {attempts} ครั้ง (ต้องตรวจแก้ด้วยตนเอง)\n",
        n = chunk_index + 1,
    ));
    let reason = sanitize_reason(reason);
    if !reason.is_empty() {
        block.push_str(&format!(">\n> เหตุผลจากผู้ตรวจ: {reason}\n"));
    }
    block.push('\n');
    block.push_str(thai_text.trim_end_matches('\n'));
    block.push_str("\n\n");

    append_guarded(ws, chapter, &marker, &block).await
}

/// Collapse a reviewer feedback string into a single safe Markdown blockquote
/// line: newlines → spaces, runs of whitespace squeezed, truncated so the banner
/// stays compact.
fn sanitize_reason(reason: &str) -> String {
    let one_line: String = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX: usize = 240;
    if one_line.chars().count() <= MAX {
        one_line
    } else {
        one_line.chars().take(MAX).collect::<String>() + "…"
    }
}

/// Delete the translated output for `chapter`, if any, so a user-requested
/// restart begins from chunk 0 instead of resuming from existing markers.
pub fn reset_chapter(ws: &Workspace, chapter: u32) -> std::io::Result<()> {
    match std::fs::remove_file(ws.translated(chapter)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Read the full accumulated Thai for `chapter` (empty string when absent).
pub async fn read_translated(ws: &Workspace, chapter: u32) -> String {
    tokio::fs::read_to_string(ws.translated(chapter))
        .await
        .unwrap_or_default()
}

/// Overwrite the cleansed source Markdown for `chapter` to `raw/ch_NNN.md`.
pub fn write_raw(ws: &Workspace, chapter: u32, markdown: &str) -> std::io::Result<()> {
    let path = ws.raw(chapter);
    ensure_parent(&path)?;
    std::fs::write(path, markdown)
}

/// Write image-only content straight to `translated/`: these chapters skip the
/// agents, so the cleansed image-link Markdown is the finished translation.
pub fn write_image_only(ws: &Workspace, chapter: u32, markdown: &str) -> std::io::Result<()> {
    let path = ws.translated(chapter);
    ensure_parent(&path)?;
    std::fs::write(path, markdown)
}

fn ensure_parent(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_ws(tag: &str) -> (std::path::PathBuf, Workspace) {
        let base = std::env::temp_dir().join(format!("honya_tr_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let ws = Workspace::new(base.clone(), 1);
        (base, ws)
    }

    /// A chunk that exhausts its review attempts is committed (so the chapter can
    /// complete) but flagged in-file: machine marker for `scan`, a visible banner
    /// with the reviewer's reason, and the last attempt's Thai. Replays are no-ops.
    #[tokio::test]
    async fn needs_review_chunk_is_flagged_and_idempotent() {
        let (base, ws) = temp_ws("needs_review");

        let n = append_chunk_needs_review(
            &ws,
            5,
            8, // 0-based index → "chunk 9" in the banner
            "คำแปลที่ยังไม่ผ่านการตรวจ",
            3,
            "meaning drift on the final sentence",
        )
        .await
        .unwrap();
        assert!(n > 0, "first write returns bytes written");

        let body = read_translated(&ws, 5).await;
        assert!(
            body.contains(REVIEW_NEEDED_MARKER),
            "machine marker present"
        );
        assert!(
            body.contains("<!-- honya:chunk 8 -->"),
            "chunk marker present"
        );
        assert!(body.contains("[REVIEW NEEDED]"), "visible banner present");
        assert!(
            body.contains("chunk 9"),
            "banner shows the 1-based chunk number"
        );
        assert!(
            body.contains("คำแปลที่ยังไม่ผ่านการตรวจ"),
            "the last attempt's Thai is committed, not dropped"
        );
        assert!(body.contains("meaning drift"), "reviewer's reason surfaced");

        assert_eq!(
            review_needed_chunk_indices_in(&body),
            BTreeSet::from([8]),
            "review-needed chunk index is detectable"
        );

        // The QA panel reads each flagged chunk's index plus the reviewer's reason.
        let details = review_needed_details_in(&body);
        assert_eq!(details.len(), 1, "one flagged chunk: {details:?}");
        assert_eq!(details[0].0, 8, "chunk index surfaced");
        assert_eq!(
            details[0].1, "meaning drift on the final sentence",
            "reviewer reason lifted from the banner"
        );

        // Re-flagging the same chunk is idempotent in marker count.
        let again = append_chunk_needs_review(&ws, 5, 8, "x", 3, "y")
            .await
            .unwrap();
        assert_eq!(again, 0, "re-flagging the same chunk adds no new marker");

        // A later approved retranslation replaces the flagged block in-place,
        // removes the review marker, and still keeps exactly one chunk marker.
        let plain = append_chunk(&ws, 5, 8, "คำแปลที่ผ่านแล้ว").await.unwrap();
        assert_eq!(plain, 0, "clean replacement adds no new marker");
        let cleaned = read_translated(&ws, 5).await;
        assert!(!cleaned.contains(REVIEW_NEEDED_MARKER));
        assert!(cleaned.contains("คำแปลที่ผ่านแล้ว"));
        assert_eq!(committed_chunk_indices_in(&cleaned), BTreeSet::from([8]));
        assert!(review_needed_chunk_indices_in(&cleaned).is_empty());

        let _ = std::fs::remove_dir_all(&base);
    }

    /// The chunks-total marker is recorded once, updated in place when the
    /// chunking changed, never displayed, and read back for status derivation.
    #[tokio::test]
    async fn total_chunks_marker_roundtrip() {
        let (base, ws) = temp_ws("total_marker");

        record_total_chunks(&ws, 1, 5).await.unwrap();
        let body = read_translated(&ws, 1).await;
        assert_eq!(total_chunks_in(&body), Some(5));

        // Same total again is a no-op (no duplicate markers).
        record_total_chunks(&ws, 1, 5).await.unwrap();
        let body = read_translated(&ws, 1).await;
        assert_eq!(
            body.matches("honya:chunks-total").count(),
            1,
            "marker must not duplicate"
        );

        // Chunks append after the marker; a changed total replaces it in place.
        append_chunk(&ws, 1, 0, "หนึ่ง").await.unwrap();
        record_total_chunks(&ws, 1, 7).await.unwrap();
        let body = read_translated(&ws, 1).await;
        assert_eq!(total_chunks_in(&body), Some(7));
        assert_eq!(body.matches("honya:chunks-total").count(), 1);
        assert!(body.contains("หนึ่ง"), "existing chunks survive the update");

        // The marker is machine-only: prose_only must strip it.
        assert!(!prose_only(&body).contains("honya:chunks-total"));

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Reviewer feedback is squeezed to one safe blockquote line and truncated.
    #[test]
    fn sanitize_reason_is_one_line_and_bounded() {
        assert_eq!(sanitize_reason("  a\nb   c  "), "a b c");
        let long = "ก".repeat(500);
        let out = sanitize_reason(&long);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), 241); // 240 + ellipsis
    }
}
