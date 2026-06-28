//! Relocate image resources into the project's `images/` dir, dedup filename
//! collisions, and build lookup maps for rewriting `<img src>` hrefs.
//!
//! Default is COPY so the extracted work dir stays reprocessable. The cleanse
//! step emits a fixed `../../images/FILE` prefix, so maps store only the basename.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use super::{ManifestItem, Result};

/// Maps that let the importer rewrite image references.
#[derive(Debug, Clone, Default)]
pub struct MediaRelocation {
    /// Resolved archive path -> relocated basename.
    pub by_resolved_path: HashMap<String, String>,
    /// Raw manifest href -> relocated basename.
    pub by_href: HashMap<String, String>,
    /// Absolute paths actually written into the images dir.
    pub written: Vec<PathBuf>,
}

/// Copy every image manifest item from `work_dir` into `images_dir`, dedup
/// filenames, and build the lookup maps.
pub fn relocate_images(
    manifest: &[ManifestItem],
    work_dir: &Path,
    images_dir: &Path,
    images_rel: &str,
) -> Result<MediaRelocation> {
    relocate_inner(manifest, work_dir, images_dir, images_rel, "", false)
}

pub fn relocate_images_with_prefix(
    manifest: &[ManifestItem],
    work_dir: &Path,
    images_dir: &Path,
    images_rel: &str,
    basename_prefix: &str,
) -> Result<MediaRelocation> {
    relocate_inner(
        manifest,
        work_dir,
        images_dir,
        images_rel,
        basename_prefix,
        false,
    )
}

/// Like `relocate_images` but MOVES each source file (rename, falling back to copy+remove).
#[allow(dead_code)]
pub fn relocate_images_move(
    manifest: &[ManifestItem],
    work_dir: &Path,
    images_dir: &Path,
    images_rel: &str,
) -> Result<MediaRelocation> {
    relocate_inner(manifest, work_dir, images_dir, images_rel, "", true)
}

fn relocate_inner(
    manifest: &[ManifestItem],
    work_dir: &Path,
    images_dir: &Path,
    _images_rel: &str,
    basename_prefix: &str,
    move_files: bool,
) -> Result<MediaRelocation> {
    let mut reloc = MediaRelocation::default();

    // Only create the images dir if there's at least one image to place.
    let has_images = manifest.iter().any(|m| m.is_image());
    if has_images {
        fs::create_dir_all(images_dir)?;
    }

    let mut used = if has_images {
        existing_basenames(images_dir)?
    } else {
        HashSet::new()
    };

    for item in manifest.iter().filter(|m| m.is_image()) {
        let src_path = join_archive_path(work_dir, &item.resolved_path);
        if !src_path.exists() {
            // Tolerate a manifest entry whose file is absent.
            continue;
        }

        let raw_basename = prefixed_basename(basename_of(&item.resolved_path), basename_prefix);
        let unique = dedup_name(&raw_basename, &mut used);
        let dest_path = images_dir.join(&unique);

        if move_files {
            if fs::rename(&src_path, &dest_path).is_err() {
                // Cross-device rename can fail; fall back to copy + remove.
                fs::copy(&src_path, &dest_path)?;
                let _ = fs::remove_file(&src_path);
            }
        } else {
            fs::copy(&src_path, &dest_path)?;
        }

        reloc
            .by_resolved_path
            .insert(item.resolved_path.clone(), unique.clone());
        reloc.by_href.insert(item.href.clone(), unique.clone());
        reloc.written.push(dest_path);
    }

    Ok(reloc)
}

/// Join a '/'-separated archive path onto a filesystem base, segment by segment
/// (so the result uses OS-correct separators).
fn join_archive_path(base: &Path, archive_path: &str) -> PathBuf {
    let mut p = base.to_path_buf();
    for seg in archive_path.split('/') {
        if !seg.is_empty() {
            p.push(seg);
        }
    }
    p
}

fn basename_of(archive_path: &str) -> &str {
    archive_path.rsplit('/').next().unwrap_or(archive_path)
}

fn prefixed_basename(basename: &str, prefix: &str) -> String {
    if prefix.is_empty() || basename.starts_with(prefix) {
        basename.to_string()
    } else {
        format!("{prefix}{basename}")
    }
}

fn existing_basenames(dir: &Path) -> Result<HashSet<String>> {
    let mut names = HashSet::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        names.insert(entry.file_name().to_string_lossy().into_owned());
    }
    Ok(names)
}

/// Return a basename not in `used`, appending `_N` before the extension on
/// collision; records the chosen name in `used`.
fn dedup_name(basename: &str, used: &mut HashSet<String>) -> String {
    if used.insert(basename.to_string()) {
        return basename.to_string();
    }
    let (stem, ext) = split_ext(basename);
    let mut n = 1;
    loop {
        let candidate = if ext.is_empty() {
            format!("{stem}_{n}")
        } else {
            format!("{stem}_{n}.{ext}")
        };
        if used.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

/// Split a basename into (stem, ext_without_dot); a leading-dot file is all-stem.
fn split_ext(basename: &str) -> (&str, &str) {
    match basename.rfind('.') {
        Some(idx) if idx > 0 => (&basename[..idx], &basename[idx + 1..]),
        _ => (basename, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(id: &str, href: &str, resolved: &str) -> ManifestItem {
        ManifestItem {
            id: id.into(),
            href: href.into(),
            resolved_path: resolved.into(),
            media_type: "image/png".into(),
            properties: vec![],
        }
    }

    #[test]
    fn basename_and_ext() {
        assert_eq!(basename_of("OEBPS/Images/a.png"), "a.png");
        assert_eq!(split_ext("a.png"), ("a", "png"));
        assert_eq!(split_ext("noext"), ("noext", ""));
    }

    #[test]
    fn dedup_appends_index() {
        let mut used = HashSet::new();
        assert_eq!(dedup_name("x.png", &mut used), "x.png");
        assert_eq!(dedup_name("x.png", &mut used), "x_1.png");
        assert_eq!(dedup_name("x.png", &mut used), "x_2.png");
        assert_eq!(dedup_name("y", &mut used), "y");
        assert_eq!(dedup_name("y", &mut used), "y_1");
    }

    #[test]
    fn prefix_is_applied_before_dedup() {
        let mut used = HashSet::new();
        assert_eq!(prefixed_basename("x.png", "vol1_"), "vol1_x.png");
        assert_eq!(prefixed_basename("vol1_x.png", "vol1_"), "vol1_x.png");
        assert_eq!(dedup_name("vol1_x.png", &mut used), "vol1_x.png");
        assert_eq!(dedup_name("vol1_x.png", &mut used), "vol1_x_1.png");
    }

    #[test]
    fn relocate_copies_and_dedups() {
        let tmp = std::env::temp_dir().join(format!("honya_media_test_{}", std::process::id()));
        let work = tmp.join("work");
        let images = tmp.join("images");
        let _ = fs::remove_dir_all(&tmp);
        // Two source images with the SAME basename in different dirs.
        fs::create_dir_all(work.join("OEBPS/Images")).unwrap();
        fs::create_dir_all(work.join("OEBPS/Extra")).unwrap();
        fs::write(work.join("OEBPS/Images/a.png"), b"first").unwrap();
        fs::write(work.join("OEBPS/Extra/a.png"), b"second").unwrap();

        let manifest = vec![
            img("i1", "Images/a.png", "OEBPS/Images/a.png"),
            img("i2", "Extra/a.png", "OEBPS/Extra/a.png"),
        ];

        let reloc = relocate_images(&manifest, &work, &images, "images").unwrap();
        assert_eq!(
            reloc
                .by_resolved_path
                .get("OEBPS/Images/a.png")
                .map(|s| s.as_str()),
            Some("a.png")
        );
        assert_eq!(
            reloc
                .by_resolved_path
                .get("OEBPS/Extra/a.png")
                .map(|s| s.as_str()),
            Some("a_1.png")
        );
        assert!(images.join("a.png").exists());
        assert!(images.join("a_1.png").exists());
        assert_eq!(reloc.written.len(), 2);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn relocate_prefixes_written_images() {
        let tmp =
            std::env::temp_dir().join(format!("honya_media_prefix_test_{}", std::process::id()));
        let work = tmp.join("work");
        let images = tmp.join("images");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(work.join("OEBPS/Images")).unwrap();
        fs::write(work.join("OEBPS/Images/a.png"), b"image").unwrap();

        let manifest = vec![img("i1", "Images/a.png", "OEBPS/Images/a.png")];

        let reloc = relocate_images_with_prefix(&manifest, &work, &images, "images", "vol1_")
            .expect("relocate with prefix");
        assert_eq!(
            reloc
                .by_resolved_path
                .get("OEBPS/Images/a.png")
                .map(|s| s.as_str()),
            Some("vol1_a.png")
        );
        assert!(images.join("vol1_a.png").exists());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn relocate_dedups_against_existing_images_dir() {
        let tmp =
            std::env::temp_dir().join(format!("honya_media_existing_test_{}", std::process::id()));
        let work = tmp.join("work");
        let images = tmp.join("images");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(work.join("OEBPS/Images")).unwrap();
        fs::create_dir_all(&images).unwrap();
        fs::write(images.join("a.png"), b"previous-volume").unwrap();
        fs::write(work.join("OEBPS/Images/a.png"), b"next-volume").unwrap();

        let manifest = vec![img("i1", "Images/a.png", "OEBPS/Images/a.png")];

        let reloc = relocate_images(&manifest, &work, &images, "images").unwrap();
        assert_eq!(
            reloc
                .by_resolved_path
                .get("OEBPS/Images/a.png")
                .map(|s| s.as_str()),
            Some("a_1.png")
        );
        assert_eq!(fs::read(images.join("a.png")).unwrap(), b"previous-volume");
        assert_eq!(fs::read(images.join("a_1.png")).unwrap(), b"next-volume");
        assert_eq!(reloc.written, vec![images.join("a_1.png")]);

        let _ = fs::remove_dir_all(&tmp);
    }
}
