//! src/workspace/style.rs — STYLE.md is free-form translation-memory prose (no
//! tool-owned JSON beyond its small metadata block). The Lexicon's Style tab
//! appends bullet notes here; they're inserted into the body ABOVE the
//! `honya:data` block so the block stays valid.

use crate::workspace::{data_block, Workspace};

/// Append a `- {note}` bullet to STYLE.md's body, preserving the data block.
pub fn append_note(ws: &Workspace, note: &str) -> std::io::Result<()> {
    let note = note.trim();
    if note.is_empty() {
        return Ok(());
    }
    let path = ws.style_md();
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let bullet = format!("- {note}");

    let new = match existing.find("<!-- honya:data") {
        Some(i) => {
            let body = existing[..i].trim_end();
            let block = &existing[i..];
            format!("{body}\n{bullet}\n\n{block}")
        }
        None => {
            let body = existing.trim_end();
            if body.is_empty() {
                format!("{bullet}\n")
            } else {
                format!("{body}\n{bullet}\n")
            }
        }
    };
    data_block::atomic_write(&path, &new)
}
