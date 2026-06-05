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

/// Append chunk `chunk_index` of `chapter`, guarded by its marker. Returns bytes
/// written (0 when the chunk was already present, i.e. a no-op).
pub async fn append_chunk(
    ws: &Workspace,
    chapter: u32,
    chunk_index: u32,
    thai_text: &str,
) -> std::io::Result<usize> {
    let path = ws.translated(chapter);
    let marker = chunk_marker(chunk_index);

    // Idempotency: skip if the marker already exists.
    if let Ok(existing) = tokio::fs::read_to_string(&path).await
        && existing.contains(&marker)
    {
        return Ok(0);
    }

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Trailing blank line keeps successive chunks separated.
    let mut block = String::with_capacity(thai_text.len() + marker.len() + 4);
    block.push_str(&marker);
    block.push('\n');
    block.push_str(thai_text.trim_end_matches('\n'));
    block.push_str("\n\n");

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
