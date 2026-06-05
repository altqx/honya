//! src/workspace/data_block.rs — the `<!-- honya:data ... honya:data -->` block.
//!
//! Every tool-mutated metadata file (CHARACTERS.md / GLOSSARY.md / VOLUME.md)
//! carries a single machine-owned JSON payload appended at the end of the file
//! inside an HTML comment. The human-readable Markdown body above it is
//! re-rendered from that payload on every write, so the JSON block is the
//! source of truth and the table above it is a derived view.
//!
//! Block shape (literal delimiters):
//! ```text
//! ...markdown body...
//!
//! <!-- honya:data
//! { ...json... }
//! honya:data -->
//! ```
//!
//! `read_data_block` is intentionally tolerant: a missing file, a missing block,
//! or unparseable JSON all yield `T::default()` so a partially-written or
//! hand-edited file never crashes the pipeline.

use std::io::Write;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Opening delimiter of the data block (start of the HTML comment).
const BLOCK_OPEN: &str = "<!-- honya:data";
/// Closing delimiter of the data block (end of the HTML comment).
const BLOCK_CLOSE: &str = "honya:data -->";

/// Read and deserialize the JSON payload from a metadata file's data block.
///
/// Returns `T::default()` when the file is missing, has no data block, or the
/// JSON inside the block fails to parse.
pub fn read_data_block<T: DeserializeOwned + Default>(path: &Path) -> T {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return T::default(),
    };
    match extract_json(&text) {
        Some(json) => serde_json::from_str::<T>(json).unwrap_or_default(),
        None => T::default(),
    }
}

/// Read the human-readable Markdown body — everything before the `honya:data`
/// block — trimmed. Used to bundle PROJECT.md / STYLE.md prose into the agent
/// context. A missing file yields an empty string; a file with no data block
/// returns its whole (trimmed) contents.
pub fn read_body(path: &Path) -> String {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return String::new(),
    };
    match text.find(BLOCK_OPEN) {
        Some(i) => text[..i].trim().to_string(),
        None => text.trim().to_string(),
    }
}

/// Slice out the raw JSON text sitting between the block delimiters, if present.
fn extract_json(text: &str) -> Option<&str> {
    let open = text.find(BLOCK_OPEN)?;
    // JSON starts immediately after the opening delimiter.
    let after_open = open + BLOCK_OPEN.len();
    let rest = &text[after_open..];
    let close_rel = rest.find(BLOCK_CLOSE)?;
    let json = rest[..close_rel].trim();
    if json.is_empty() { None } else { Some(json) }
}

/// Render a metadata file: the human-readable Markdown `rendered_body`, then the
/// comment-wrapped pretty JSON `data` block. Written atomically.
///
/// The parent directory is created if it does not already exist.
pub fn write_with_data<T: Serialize>(
    path: &Path,
    rendered_body: &str,
    data: &T,
) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(data)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let mut out = String::with_capacity(rendered_body.len() + json.len() + 64);
    out.push_str(rendered_body.trim_end());
    out.push_str("\n\n");
    out.push_str(BLOCK_OPEN);
    out.push('\n');
    out.push_str(&json);
    out.push('\n');
    out.push_str(BLOCK_CLOSE);
    out.push('\n');

    atomic_write(path, &out)
}

/// Write `contents` to `path` atomically: write a temp sibling then `fs::rename`.
///
/// The rename is atomic within a filesystem, so a reader never observes a
/// half-written metadata file. The parent directory is created if absent.
pub fn atomic_write(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }

    let tmp = temp_sibling(path);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.flush()?;
        f.sync_all()?;
    }

    // Rename into place. On failure, best-effort cleanup of the temp file.
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Build a temp sibling path next to `path` (same dir, so `rename` stays on one
/// filesystem). Uses the process id + file name to avoid clobbering.
fn temp_sibling(path: &Path) -> std::path::PathBuf {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "honya-tmp".to_string());
    let tmp_name = format!(".{}.{}.tmp", file_name, std::process::id());
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(tmp_name),
        _ => std::path::PathBuf::from(tmp_name),
    }
}
