//! STYLE.md is free-form translation-memory prose. Appended bullet notes go into
//! the body ABOVE the `honya:data` block so the block stays valid.

use crate::workspace::{Workspace, data_block};

/// Append a `- {note}` bullet to STYLE.md's body, preserving the data block.
pub fn append_note(ws: &Workspace, note: &str) -> std::io::Result<()> {
    let note = note.trim();
    if note.is_empty() {
        return Ok(());
    }
    let path = ws.style_md();
    let body = data_block::read_body(&path);
    let bullet = format!("- {note}");
    let body = if body.is_empty() {
        bullet
    } else {
        format!("{body}\n{bullet}")
    };
    data_block::write_body(&path, &body)
}
