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

/// Replace the Markdown body, leaving the data block exactly as it was.
///
/// For the files whose body is the point rather than a rendered view of the
/// JSON — STYLE.md is free-form prose the agents append to. Without this a
/// caller has to find the delimiter itself, which is how `style.rs` came to
/// carry its own copy of `"<!-- honya:data"` and splice around it by hand.
pub fn write_body(path: &Path, body: &str) -> std::io::Result<()> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let out = match existing.find(BLOCK_OPEN) {
        Some(i) => format!("{}\n\n{}", body.trim_end(), existing[i..].trim_start()),
        None => format!("{}\n", body.trim_end()),
    };
    atomic_write(path, &out)
}

/// Atomic write via temp sibling + replace. Windows falls back to remove-then-rename
/// because `fs::rename` cannot overwrite an existing destination there.
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

    // Best-effort cleanup if the replace fails.
    match replace_through(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Move `tmp` onto `dst`, overwriting when the platform permits it.
#[cfg(not(windows))]
fn replace_through(tmp: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::rename(tmp, dst)
}

#[cfg(windows)]
fn replace_through(tmp: &Path, dst: &Path) -> std::io::Result<()> {
    match std::fs::rename(tmp, dst) {
        Ok(()) => Ok(()),
        Err(_) => {
            let _ = std::fs::remove_file(dst);
            std::fs::rename(tmp, dst)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    struct Block {
        items: Vec<String>,
    }

    fn temp_file(tag: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("honya_db_{tag}_{}_{stamp}", std::process::id()))
    }

    #[test]
    fn body_and_data_round_trip() {
        let path = temp_file("roundtrip").join("nested").join("FILE.md");
        let data = Block {
            items: vec!["猫".to_string(), "แมว".to_string()],
        };
        write_with_data(&path, "# Title\n\n| a |\n", &data).unwrap();

        assert_eq!(read_data_block::<Block>(&path), data);
        assert_eq!(read_body(&path), "# Title\n\n| a |");
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    /// A partial or hand-edited file must never take the pipeline down, so
    /// every unreadable shape yields the default rather than an error.
    #[test]
    fn unreadable_shapes_all_read_as_default() {
        let missing = temp_file("missing");
        assert_eq!(read_data_block::<Block>(&missing), Block::default());
        assert_eq!(read_body(&missing), "");

        let no_block = temp_file("noblock");
        std::fs::write(&no_block, "just prose\n").unwrap();
        assert_eq!(read_data_block::<Block>(&no_block), Block::default());
        assert_eq!(
            read_body(&no_block),
            "just prose",
            "a file with no block is all body"
        );

        let bad = temp_file("badjson");
        std::fs::write(&bad, "body\n\n<!-- honya:data\n{not json\nhonya:data -->\n").unwrap();
        assert_eq!(read_data_block::<Block>(&bad), Block::default());
        assert_eq!(read_body(&bad), "body");

        let empty_block = temp_file("emptyblock");
        std::fs::write(&empty_block, "body\n\n<!-- honya:data\n\nhonya:data -->\n").unwrap();
        assert_eq!(read_data_block::<Block>(&empty_block), Block::default());

        for p in [&missing, &no_block, &bad, &empty_block] {
            let _ = std::fs::remove_file(p);
        }
    }

    /// STYLE.md's case: the body is the point and the block is none of the
    /// caller's business, so rewriting one must leave the other byte-identical.
    #[test]
    fn write_body_leaves_the_data_block_alone() {
        let path = temp_file("writebody");
        let data = Block {
            items: vec!["kept".to_string()],
        };
        write_with_data(&path, "- first", &data).unwrap();

        write_body(&path, "- first\n- second").unwrap();
        assert_eq!(read_body(&path), "- first\n- second");
        assert_eq!(
            read_data_block::<Block>(&path),
            data,
            "the block survived a body rewrite"
        );

        let fresh = temp_file("writebody_fresh");
        write_body(&fresh, "- only").unwrap();
        assert_eq!(read_body(&fresh), "- only");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&fresh);
    }
}
