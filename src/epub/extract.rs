//! src/epub/extract.rs — unzip (zip-slip-safe) + locate the OPF via container.xml.
//!
//! zip 8.6 read API used: `ZipArchive::new(File)`, `.len()`, `.by_index(i)`,
//! `.by_name(name)`, `ZipFile::{name(), enclosed_name() -> Option<PathBuf>,
//! is_dir(), size(), Read}`. `enclosed_name()` returning `None` is the
//! zip-slip / traversal guard.

use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use zip::ZipArchive;

use super::paths::percent_decode;
use super::{EpubError, Result, ns};

/// Open an EPUB file as a zip archive.
pub fn open_archive(epub_path: &Path) -> Result<ZipArchive<File>> {
    let file = File::open(epub_path)?;
    let archive = ZipArchive::new(file)?;
    Ok(archive)
}

/// Extract every entry of `archive` into `dest_dir`, zip-slip-safe. Returns the
/// list of '/'-separated archive entry keys (directories excluded) actually
/// written, in archive order.
pub fn extract_all(archive: &mut ZipArchive<File>, dest_dir: &Path) -> Result<Vec<String>> {
    fs::create_dir_all(dest_dir)?;
    let mut written: Vec<String> = Vec::new();

    for i in 0..archive.len() {
        let entry = archive.by_index(i)?;

        // The raw archive name (for diagnostics + our '/'-keyed records).
        let raw_name = entry.name().to_string();

        // zip-slip guard: enclosed_name() validates the path stays inside the
        // destination (no absolute paths, no `..` escaping). None => reject.
        let enclosed = match entry.enclosed_name() {
            Some(p) => p,
            None => return Err(EpubError::UnsafeEntryName(raw_name)),
        };

        let out_path = dest_dir.join(&enclosed);

        if entry.is_dir() {
            fs::create_dir_all(&out_path)?;
            continue;
        }

        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let size = entry.size();
        let buf = read_entry_capped(entry, size, &raw_name)?;
        fs::write(&out_path, &buf)?;

        // Record the '/'-separated key (archive names already use '/').
        written.push(raw_name.replace('\\', "/"));
    }

    Ok(written)
}

/// Read a single archive entry (by '/'-separated name) into a UTF-8 string
/// (lossy on invalid bytes).
pub fn read_entry_to_string(archive: &mut ZipArchive<File>, name: &str) -> Result<String> {
    let entry = archive.by_name(name)?;
    let size = entry.size();
    let buf = read_entry_capped(entry, size, name)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Per-entry decompression budget. A single EPUB resource above this is treated
/// as a decompression bomb and rejected (defends both the `with_capacity` hint
/// and the actual read from an attacker-inflated uncompressed size).
const MAX_ENTRY_BYTES: u64 = 64 * 1024 * 1024;

/// Read an archive entry with a capacity hint capped to the budget AND the read
/// itself bounded by `take`, so a lie in the zip header can neither pre-allocate
/// nor stream past the budget.
fn read_entry_capped<R: Read>(entry: R, size_hint: u64, name: &str) -> Result<Vec<u8>> {
    let cap = size_hint.min(MAX_ENTRY_BYTES) as usize;
    let mut buf = Vec::with_capacity(cap);
    entry.take(MAX_ENTRY_BYTES + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > MAX_ENTRY_BYTES {
        return Err(EpubError::EntryTooLarge(name.to_string()));
    }
    Ok(buf)
}

/// Locate the OPF package document by reading META-INF/container.xml from the
/// still-zipped archive.
#[allow(dead_code)]
pub fn locate_opf(archive: &mut ZipArchive<File>) -> Result<String> {
    let xml = read_entry_to_string(archive, "META-INF/container.xml")?;
    locate_opf_from_str(&xml)
}

/// Locate the OPF package document by reading an already-extracted work dir's
/// META-INF/container.xml.
pub fn locate_opf_from_dir(work_dir: &Path) -> Result<String> {
    let container_path = work_dir.join("META-INF").join("container.xml");
    let xml = fs::read_to_string(&container_path).map_err(|_| EpubError::MissingOpf)?;
    locate_opf_from_str(&xml)
}

/// Parse a container.xml string and return the chosen rootfile `full-path`,
/// percent-decoded and '/'-separated. Prefers a rootfile whose `media-type` is
/// `application/oebps-package+xml`; falls back to the first rootfile found.
pub fn locate_opf_from_str(xml: &str) -> Result<String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| EpubError::Xml {
        context: "META-INF/container.xml".to_string(),
        source: e,
    })?;

    let mut fallback: Option<String> = None;

    for node in doc.descendants() {
        if !node.is_element() {
            continue;
        }
        // Accept the element whether it carries the container namespace or no
        // namespace at all (some packagers omit it).
        let name = node.tag_name();
        let ns_ok = match name.namespace() {
            Some(uri) => uri == ns::CONTAINER,
            None => true,
        };
        let is_rootfile = name.name() == "rootfile" && ns_ok;
        if !is_rootfile {
            continue;
        }

        let full_path = node
            .attribute("full-path")
            .or_else(|| node.attribute((ns::CONTAINER, "full-path")));
        let Some(full_path) = full_path else { continue };

        let decoded = percent_decode(full_path).replace('\\', "/");
        let media = node.attribute("media-type").unwrap_or("");

        if media == "application/oebps-package+xml" {
            return Ok(decoded);
        }
        if fallback.is_none() {
            fallback = Some(decoded);
        }
    }

    fallback.ok_or(EpubError::MissingOpf)
}
