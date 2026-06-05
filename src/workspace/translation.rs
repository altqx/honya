//! src/workspace/translation.rs — write/append/read translated chapters.
//!
//! Approved Thai is appended to `translated/ch_NNN.md` chunk-by-chunk. Each
//! append is preceded by a `<!-- honya:chunk N -->` marker; if a marker for the
//! same `N` is already present the append is a no-op, making retries and
//! replays idempotent (see risks.txt #11 — both the deterministic app-side path
//! and the LLM `append_translation` tool funnel through `append_chunk`).
//!
//! `append_chunk` / `read_translated` are async (tokio::fs) because they run on
//! the pipeline task; `write_raw` / `write_image_only` are sync std::fs because
//! they run during import.

use std::path::Path;

use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;

use crate::workspace::Workspace;

/// Build the idempotency marker for chunk `n`.
fn chunk_marker(n: u32) -> String {
    format!("<!-- honya:chunk {} -->", n)
}

/// Append the approved Thai for chunk `chunk_index` of `chapter` to
/// `translated/ch_NNN.md`, guarded by a `<!-- honya:chunk N -->` marker.
///
/// Returns the number of bytes written (0 when the chunk was already present
/// and the call was a no-op).
pub async fn append_chunk(
    ws: &Workspace,
    chapter: u32,
    chunk_index: u32,
    thai_text: &str,
) -> std::io::Result<usize> {
    let path = ws.translated(chapter);
    let marker = chunk_marker(chunk_index);

    // Idempotency: if the marker already exists, do nothing.
    if let Ok(existing) = tokio::fs::read_to_string(&path).await {
        if existing.contains(&marker) {
            return Ok(0);
        }
    }

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Assemble the block: marker line, the Thai body, then a trailing blank
    // line so successive chunks stay separated.
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
    match tokio::fs::read_to_string(ws.translated(chapter)).await {
        Ok(text) => text,
        Err(_) => String::new(),
    }
}

/// Write (overwrite) the cleansed source Markdown for `chapter` to
/// `raw/ch_NNN.md`. Creates the `raw/` directory if needed.
pub fn write_raw(ws: &Workspace, chapter: u32, markdown: &str) -> std::io::Result<()> {
    let path = ws.raw(chapter);
    ensure_parent(&path)?;
    std::fs::write(path, markdown)
}

/// Write image-only chapter content straight to `translated/ch_NNN.md`.
///
/// Image-only chapters skip the agents entirely: the cleansed image-link
/// Markdown is the finished translation, copied verbatim to translated/.
/// Creates the `translated/` directory if needed.
pub fn write_image_only(ws: &Workspace, chapter: u32, markdown: &str) -> std::io::Result<()> {
    let path = ws.translated(chapter);
    ensure_parent(&path)?;
    std::fs::write(path, markdown)
}

fn ensure_parent(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}
