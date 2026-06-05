//! Write/append/read translated chapters. Approved Thai is appended chunk-by-chunk
//! behind a `<!-- honya:chunk N -->` marker; a repeated `N` is a no-op, so retries
//! and replays stay idempotent (risks.txt #11). Append/read are async (pipeline
//! task); write_raw/write_image_only are sync (import path).

use std::path::Path;

use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;

use crate::workspace::Workspace;

fn chunk_marker(n: u32) -> String {
    format!("<!-- honya:chunk {} -->", n)
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

    // Idempotency: skip if the chunk's marker already exists.
    if let Ok(existing) = tokio::fs::read_to_string(&path).await
        && existing.contains(marker)
    {
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
        assert!(body.contains(REVIEW_NEEDED_MARKER), "machine marker present");
        assert!(body.contains("<!-- honya:chunk 8 -->"), "chunk marker present");
        assert!(body.contains("[REVIEW NEEDED]"), "visible banner present");
        assert!(body.contains("chunk 9"), "banner shows the 1-based chunk number");
        assert!(
            body.contains("คำแปลที่ยังไม่ผ่านการตรวจ"),
            "the last attempt's Thai is committed, not dropped"
        );
        assert!(body.contains("meaning drift"), "reviewer's reason surfaced");

        // Idempotent across both append paths: the chunk marker guards re-writes,
        // so a replay never duplicates the block.
        let again = append_chunk_needs_review(&ws, 5, 8, "x", 3, "y").await.unwrap();
        assert_eq!(again, 0, "re-flagging the same chunk is a no-op");
        let plain = append_chunk(&ws, 5, 8, "x").await.unwrap();
        assert_eq!(plain, 0, "plain append of the same index is a no-op too");

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
