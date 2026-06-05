//! Top-level EPUB import orchestration: extract (zip-slip-safe), parse the OPF,
//! build the TOC (nav.xhtml > NCX), and copy image resources, returning rewrite maps.

use std::path::{Path, PathBuf};

use super::extract::{extract_all, locate_opf_from_dir, open_archive, read_entry_to_string};
use super::media::{MediaRelocation, relocate_images};
use super::opf::{ParsedOpf, parse_opf};
use super::toc::{parse_nav_xhtml, parse_ncx};
use super::{EpubBook, Result, TocEntry};

/// Import an EPUB into `work_dir`, returning the parsed book (no media relocation).
#[allow(dead_code)]
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

fn read_extracted(work_dir: &Path, archive_path: &str) -> Result<String> {
    let p = disk_path_for(work_dir, archive_path);
    Ok(std::fs::read_to_string(&p)?)
}

fn disk_path_for(base: &Path, archive_path: &str) -> PathBuf {
    let mut p = base.to_path_buf();
    for seg in archive_path.split('/') {
        if !seg.is_empty() {
            p.push(seg);
        }
    }
    p
}

/// TOC preference: EPUB3 nav.xhtml (manifest `nav` property) > NCX (spine `toc=`
/// or x-dtbncx+xml media-type) > empty.
fn build_toc(
    archive: &mut zip::ZipArchive<std::fs::File>,
    work_dir: &Path,
    _opf_path: &str,
    parsed: &ParsedOpf,
) -> Result<Vec<TocEntry>> {
    if let Some(nav_id) = &parsed.nav_id
        && let Some(&idx) = parsed.manifest_by_id.get(nav_id)
    {
        let nav_path = parsed.manifest[idx].resolved_path.clone();
        if let Ok(xml) = read_or_archive(archive, work_dir, &nav_path)
            && let Ok(entries) = parse_nav_xhtml(&xml, &nav_path)
            && !entries.is_empty()
        {
            return Ok(entries);
        }
    }

    if let Some(ncx_id) = &parsed.ncx_id
        && let Some(&idx) = parsed.manifest_by_id.get(ncx_id)
    {
        let ncx_path = parsed.manifest[idx].resolved_path.clone();
        if let Ok(xml) = read_or_archive(archive, work_dir, &ncx_path)
            && let Ok(entries) = parse_ncx(&xml, &ncx_path)
        {
            return Ok(entries);
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
    /// Spine-authoritative reading order as archive-relative resolved paths
    /// (not re-derived from the manifest or TOC).
    pub fn reading_order_paths(&self) -> Vec<&str> {
        self.spine
            .iter()
            .map(|s| s.resolved_path.as_str())
            .collect()
    }

    /// Map an archive-relative '/'-path to its on-disk location under `work_dir`.
    pub fn disk_path(&self, archive_path: &str) -> PathBuf {
        disk_path_for(&self.work_dir, archive_path)
    }
}
