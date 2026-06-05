//! The `<!-- honya:data ... honya:data -->` block: each metadata file carries a
//! machine-owned JSON payload in a trailing HTML comment that is the source of
//! truth; the Markdown body above it is a derived view re-rendered on each write.
//! Reads are intentionally tolerant — missing file/block or bad JSON yield
//! `T::default()` so a partial or hand-edited file never crashes the pipeline.

use std::io::Write;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

const BLOCK_OPEN: &str = "<!-- honya:data";
const BLOCK_CLOSE: &str = "honya:data -->";

/// Deserialize the data block's JSON, or `T::default()` on missing file/block/bad JSON.
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

/// Read the trimmed Markdown body (everything before the `honya:data` block);
/// missing file yields `""`, no-block file returns its whole trimmed contents.
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

/// Slice out the raw JSON between the block delimiters, if present.
fn extract_json(text: &str) -> Option<&str> {
    let open = text.find(BLOCK_OPEN)?;
    let after_open = open + BLOCK_OPEN.len();
    let rest = &text[after_open..];
    let close_rel = rest.find(BLOCK_CLOSE)?;
    let json = rest[..close_rel].trim();
    if json.is_empty() { None } else { Some(json) }
}

/// Atomically write `rendered_body` followed by the comment-wrapped pretty JSON
/// `data` block (creating the parent dir if needed).
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

/// Atomic write via temp sibling + `fs::rename` so a reader never sees a
/// half-written file (rename is atomic within a filesystem). Creates parent dir.
pub fn atomic_write(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }

    let tmp = temp_sibling(path);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.flush()?;
        f.sync_all()?;
    }

    // Best-effort cleanup of the temp file if the rename fails.
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Temp sibling next to `path` (same dir so `rename` stays on one filesystem);
/// name uses pid + file name to avoid clobbering.
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
