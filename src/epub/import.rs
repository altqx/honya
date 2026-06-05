//! src/epub/import.rs — top-level EPUB import orchestration.
//!
//! Pipeline (`import_with_media`):
//!   1. open the archive,
//!   2. extract everything (zip-slip-safe) into `work_dir`,
//!   3. locate + parse the OPF (metadata / manifest / spine / cover),
//!   4. build the TOC preferring nav.xhtml > NCX(spine toc=) > NCX-by-media-type,
//!   5. COPY image resources into `images_dir`, returning the rewrite maps.
//!
//! `reading_order_paths` is spine-authoritative; `disk_path` maps an archive
//! path to its on-disk location under `work_dir`.

use std::path::{Path, PathBuf};

use super::extract::{
    extract_all, locate_opf_from_dir, open_archive, read_entry_to_string,
};
use super::media::{relocate_images, MediaRelocation};
use super::opf::{parse_opf, ParsedOpf};
use super::toc::{parse_ncx, parse_nav_xhtml};
use super::{EpubBook, Result, TocEntry};

/// Import an EPUB into `work_dir`, returning the parsed book (no media relocation).
pub fn import_epub(epub_path: &Path, work_dir: &Path) -> Result<EpubBook> {
    let mut archive = open_archive(epub_path)?;
    extract_all(&mut archive, work_dir)?;

    let opf_path = locate_opf_from_dir(work_dir)?;
    let opf_xml = read_extracted(work_dir, &opf_path)?;
    let parsed = parse_opf(&opf_xml, &opf_path)?;

    let toc = build_toc(&mut archive, work_dir, &opf_path, &parsed)?;

    Ok(assemble_book(work_dir, opf_path, parsed, toc))
}

/// Import an EPUB and relocate its images into `images_dir`. Returns the book
/// plus the media rewrite maps the cleanse step consumes.
pub fn import_with_media(
    epub_path: &Path,
    work_dir: &Path,
    images_dir: &Path,
    images_rel: &str,
) -> Result<(EpubBook, MediaRelocation)> {
    let mut archive = open_archive(epub_path)?;
    extract_all(&mut archive, work_dir)?;

    let opf_path = locate_opf_from_dir(work_dir)?;
    let opf_xml = read_extracted(work_dir, &opf_path)?;
    let parsed = parse_opf(&opf_xml, &opf_path)?;

    let toc = build_toc(&mut archive, work_dir, &opf_path, &parsed)?;

    let reloc = relocate_images(&parsed.manifest, work_dir, images_dir, images_rel)?;

    let book = assemble_book(work_dir, opf_path, parsed, toc);
    Ok((book, reloc))
}

/// Read an extracted archive-relative file from `work_dir` as a UTF-8 string.
fn read_extracted(work_dir: &Path, archive_path: &str) -> Result<String> {
    let p = disk_path_for(work_dir, archive_path);
    Ok(std::fs::read_to_string(&p)?)
}

/// Build the disk path for an archive-relative '/'-separated path under `base`.
fn disk_path_for(base: &Path, archive_path: &str) -> PathBuf {
    let mut p = base.to_path_buf();
    for seg in archive_path.split('/') {
        if !seg.is_empty() {
            p.push(seg);
        }
    }
    p
}

/// TOC preference: EPUB3 nav.xhtml (by manifest `nav` property) >
/// NCX referenced by the spine `toc=` / NCX media-type. Falls back to empty.
fn build_toc(
    archive: &mut zip::ZipArchive<std::fs::File>,
    work_dir: &Path,
    _opf_path: &str,
    parsed: &ParsedOpf,
) -> Result<Vec<TocEntry>> {
    // 1) Prefer the EPUB3 nav document.
    if let Some(nav_id) = &parsed.nav_id {
        if let Some(&idx) = parsed.manifest_by_id.get(nav_id) {
            let nav_path = parsed.manifest[idx].resolved_path.clone();
            if let Ok(xml) = read_or_archive(archive, work_dir, &nav_path) {
                if let Ok(entries) = parse_nav_xhtml(&xml, &nav_path) {
                    if !entries.is_empty() {
                        return Ok(entries);
                    }
                }
            }
        }
    }

    // 2) Fall back to the NCX (spine toc= or x-dtbncx+xml media-type).
    if let Some(ncx_id) = &parsed.ncx_id {
        if let Some(&idx) = parsed.manifest_by_id.get(ncx_id) {
            let ncx_path = parsed.manifest[idx].resolved_path.clone();
            if let Ok(xml) = read_or_archive(archive, work_dir, &ncx_path) {
                if let Ok(entries) = parse_ncx(&xml, &ncx_path) {
                    return Ok(entries);
                }
            }
        }
    }

    Ok(Vec::new())
}

/// Read an archive-relative file, preferring the extracted copy on disk and
/// falling back to the still-open archive.
fn read_or_archive(
    archive: &mut zip::ZipArchive<std::fs::File>,
    work_dir: &Path,
    archive_path: &str,
) -> Result<String> {
    let disk = disk_path_for(work_dir, archive_path);
    if let Ok(s) = std::fs::read_to_string(&disk) {
        return Ok(s);
    }
    read_entry_to_string(archive, archive_path)
}

/// Stitch parsed OPF + TOC into the public `EpubBook`.
fn assemble_book(
    work_dir: &Path,
    opf_path: String,
    parsed: ParsedOpf,
    toc: Vec<TocEntry>,
) -> EpubBook {
    EpubBook {
        work_dir: work_dir.to_path_buf(),
        opf_path,
        metadata: parsed.metadata,
        manifest: parsed.manifest,
        manifest_by_id: parsed.manifest_by_id,
        spine: parsed.spine,
        toc,
    }
}

impl EpubBook {
    /// Reading order = the spine, in order. Returns archive-relative resolved
    /// paths. Spine-authoritative (does not re-derive from the manifest or TOC).
    pub fn reading_order_paths(&self) -> Vec<&str> {
        self.spine.iter().map(|s| s.resolved_path.as_str()).collect()
    }

    /// Map an archive-relative '/'-separated path to its on-disk location under
    /// this book's extraction `work_dir`.
    pub fn disk_path(&self, archive_path: &str) -> PathBuf {
        disk_path_for(&self.work_dir, archive_path)
    }
}
