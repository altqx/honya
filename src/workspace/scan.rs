//! src/workspace/scan.rs — discover projects/volumes/chapters on disk.
//!
//! Drives the Shelf and Project screens. A directory is treated as a project
//! when it contains a `PROJECT.md`. Volumes are `Vol_NN` subdirectories;
//! chapters are `ch_NNN.md` files under `raw/` (with status derived from the
//! presence/shape of the matching `translated/ch_NNN.md`).
//!
//! All functions are sync (std::fs + walkdir); the pipeline owns the async file
//! IO. `derive_status` reads translated/raw files directly rather than via the
//! async `translation` helpers so it stays usable from synchronous render paths.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use walkdir::WalkDir;

use crate::cleanse;
use crate::model::{Chapter, ChapterKind, ChapterStatus, Project, Volume};

/// Discover all projects directly under `root` (one level deep), ascending by id.
///
/// A child directory is a project iff it contains a `PROJECT.md`. Each returned
/// `Project` is fully populated with its volumes and chapters.
pub fn scan_projects(root: &Path) -> Vec<Project> {
    let mut projects = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return projects,
    };

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        if !dir.join("PROJECT.md").is_file() {
            continue;
        }

        let id = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if id.is_empty() {
            continue;
        }

        let volumes = scan_volumes(&dir);
        let (created, touched) = dir_times(&dir);
        let title = read_project_title(&dir).unwrap_or_else(|| id.clone());

        projects.push(Project {
            id,
            dir,
            title,
            created,
            touched,
            volumes,
            models: None,
        });
    }

    projects.sort_by(|a, b| a.id.cmp(&b.id));
    projects
}

/// Discover `Vol_NN` volumes under a project directory, ascending by number.
pub fn scan_volumes(project_dir: &Path) -> Vec<Volume> {
    let mut volumes = Vec::new();
    let entries = match std::fs::read_dir(project_dir) {
        Ok(e) => e,
        Err(_) => return volumes,
    };

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let name = match dir.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        let number = match parse_vol_number(name) {
            Some(n) => n,
            None => continue,
        };

        let chapters = scan_chapters(&dir);
        let label = read_volume_label(&dir);
        volumes.push(Volume {
            number,
            dir,
            label,
            chapters,
        });
    }

    volumes.sort_by_key(|v| v.number);
    volumes
}

/// Discover chapters in a volume directory from `raw/ch_NNN.md`, ascending.
///
/// Title defaults to "Chapter NNN" (the EPUB TOC title is not persisted on
/// disk; the live `Project` is the authority for titles when available).
pub fn scan_chapters(vol_dir: &Path) -> Vec<Chapter> {
    let raw_dir = vol_dir.join("raw");
    let mut numbers: Vec<u32> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&raw_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if let Some(n) = parse_chapter_number(path.file_name().and_then(|n| n.to_str())) {
                numbers.push(n);
            }
        }
    }

    numbers.sort_unstable();
    numbers.dedup();

    numbers
        .into_iter()
        .map(|number| {
            let raw_md = std::fs::read_to_string(vol_dir.join("raw").join(format!("ch_{:03}.md", number)))
                .unwrap_or_default();
            let kind = if !raw_md.trim().is_empty() && cleanse::is_image_only(&raw_md) {
                ChapterKind::ImageOnly
            } else if raw_md.trim().is_empty() {
                ChapterKind::Empty
            } else {
                ChapterKind::Prose
            };
            let status = derive_status(vol_dir, number, kind);
            let last_run = file_modified(&vol_dir.join("translated").join(format!("ch_{:03}.md", number)))
                .or_else(|| file_modified(&vol_dir.join("raw").join(format!("ch_{:03}.md", number))));

            Chapter {
                number,
                // Import writes the EPUB TOC title as a leading `# ` heading on
                // prose chapters; fall back to the generic label when absent.
                title: first_md_heading(&raw_md)
                    .unwrap_or_else(|| format!("Chapter {:03}", number)),
                kind,
                status,
                source_segments: count_segments(&raw_md),
                total_chunks: 0,
                committed_chunks: count_committed_chunks(vol_dir, number),
                last_run,
            }
        })
        .collect()
}

/// Read the chapter title from a leading Markdown `# ` heading, if the raw file
/// starts with one. Stops at the first non-empty non-heading line so narrative
/// text is never mistaken for a title.
fn first_md_heading(md: &str) -> Option<String> {
    for line in md.lines() {
        let t = line.trim_start();
        if let Some(h) = t.strip_prefix("# ") {
            let h = h.trim();
            if !h.is_empty() {
                return Some(h.to_string());
            }
        }
        if !t.is_empty() && !t.starts_with('#') {
            break;
        }
    }
    None
}

/// Find `*.epub` files directly under `root` that are not already a project
/// directory, returned with their byte size for the Shelf import list.
///
/// Walks one level deep (depth 1) so we list importable epubs sitting beside
/// existing project directories without descending into them.
pub fn find_unimported_epubs(root: &Path) -> Vec<(PathBuf, u64)> {
    let mut out = Vec::new();

    for entry in WalkDir::new(root)
        .min_depth(1)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let is_epub = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("epub"))
            .unwrap_or(false);
        if !is_epub {
            continue;
        }

        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        out.push((path.to_path_buf(), size));
    }

    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Derive a chapter's lifecycle status from disk state.
///
/// Disk alone cannot distinguish "mid-run" from "finished" reliably, so this is
/// the resting status; the live pipeline overrides it via `AppEvent` while a run
/// is in flight. The rules:
/// - `ImageOnly` → [`ChapterStatus::Done`] once written to translated/ (image
///   chapters are copied straight there at import), else `Pending`.
/// - non-empty translated/ → [`ChapterStatus::Done`].
/// - only raw/ (or nothing) → [`ChapterStatus::Pending`].
pub fn derive_status(vol_dir: &Path, chapter: u32, kind: ChapterKind) -> ChapterStatus {
    let translated = vol_dir
        .join("translated")
        .join(format!("ch_{:03}.md", chapter));

    if matches!(kind, ChapterKind::ImageOnly) {
        // Image-only content is finished as soon as it's written to translated/.
        return if translated.is_file() {
            ChapterStatus::Done
        } else {
            ChapterStatus::Pending
        };
    }

    let translated_non_empty = std::fs::read_to_string(&translated)
        .map(|t| !t.trim().is_empty())
        .unwrap_or(false);

    if translated_non_empty {
        ChapterStatus::Done
    } else {
        ChapterStatus::Pending
    }
}

// --- helpers ----------------------------------------------------------------

/// Count `<!-- honya:chunk N -->` markers in translated text.
fn count_chunk_markers(text: &str) -> usize {
    text.matches("<!-- honya:chunk ").count()
}

/// Count committed chunks for a chapter by reading its translated file.
fn count_committed_chunks(vol_dir: &Path, chapter: u32) -> u32 {
    let path = vol_dir
        .join("translated")
        .join(format!("ch_{:03}.md", chapter));
    match std::fs::read_to_string(path) {
        Ok(text) => count_chunk_markers(&text) as u32,
        Err(_) => 0,
    }
}

/// Best-effort source-segment count: non-empty lines of the raw markdown.
fn count_segments(raw_md: &str) -> u32 {
    raw_md.lines().filter(|l| !l.trim().is_empty()).count() as u32
}

/// Parse `Vol_07` → `7`.
fn parse_vol_number(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("Vol_")?;
    rest.parse::<u32>().ok()
}

/// Parse `ch_003.md` → `3`.
fn parse_chapter_number(name: Option<&str>) -> Option<u32> {
    let name = name?;
    let stem = name.strip_suffix(".md")?;
    let num = stem.strip_prefix("ch_")?;
    num.parse::<u32>().ok()
}

/// Read the project title from PROJECT.md's first `# ` heading.
fn read_project_title(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("PROJECT.md")).ok()?;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(title) = trimmed.strip_prefix("# ") {
            let title = title.trim();
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }
    }
    None
}

/// Read a volume label from VOLUME.md's running recap header (`เล่ม: ...`).
fn read_volume_label(vol_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(vol_dir.join("VOLUME.md")).ok()?;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(label) = trimmed.strip_prefix("เล่ม:") {
            let label = label.trim();
            if !label.is_empty() {
                return Some(label.to_string());
            }
        }
    }
    None
}

/// Filesystem created/modified times for a directory, as UTC datetimes.
fn dir_times(dir: &Path) -> (Option<DateTime<Utc>>, Option<DateTime<Utc>>) {
    let meta = match std::fs::metadata(dir) {
        Ok(m) => m,
        Err(_) => return (None, None),
    };
    let created = meta.created().ok().map(DateTime::<Utc>::from);
    let touched = meta.modified().ok().map(DateTime::<Utc>::from);
    (created, touched)
}

/// Modified time of a file as a UTC datetime, if it exists.
fn file_modified(path: &Path) -> Option<DateTime<Utc>> {
    let meta = std::fs::metadata(path).ok()?;
    meta.modified().ok().map(DateTime::<Utc>::from)
}
