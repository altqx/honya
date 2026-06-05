//! src/workspace/mod.rs — Workspace path resolver for one project + active volume.
//!
//! A `Workspace` binds a project root directory together with one active volume
//! directory (`Vol_NN`). All on-disk metadata (CHARACTERS.md, GLOSSARY.md,
//! STYLE.md, PROJECT.md) lives at the root; per-volume state (VOLUME.md,
//! raw/, translated/) lives under the volume directory.
//!
//! Layout (mirrors model::Project / model::Volume / model::Chapter):
//! ```text
//! <root>/
//! ├── PROJECT.md
//! ├── CHARACTERS.md
//! ├── GLOSSARY.md
//! ├── STYLE.md
//! ├── images/
//! └── Vol_NN/
//!     ├── VOLUME.md
//!     ├── raw/ch_NNN.md
//!     └── translated/ch_NNN.md
//! ```

use std::path::PathBuf;

pub mod characters;
pub mod data_block;
pub mod glossary;
pub mod scaffold;
pub mod scan;
pub mod style;
pub mod translation;
pub mod volume;

/// Resolves every path honya touches for a single project + active volume.
#[derive(Debug, Clone)]
pub struct Workspace {
    /// Absolute (or CWD-relative) path to the project directory.
    pub root: PathBuf,
    /// Absolute path to the active volume directory (`<root>/Vol_NN`).
    pub vol_dir: PathBuf,
}

impl Workspace {
    /// Build a workspace for `root` with `vol_number` as the active volume.
    /// `vol_dir = root / "Vol_{vol_number:02}"`.
    pub fn new(root: PathBuf, vol_number: u32) -> Self {
        let vol_dir = root.join(format!("Vol_{:02}", vol_number));
        Self { root, vol_dir }
    }

    /// `<root>/CHARACTERS.md`
    pub fn characters_md(&self) -> PathBuf {
        self.root.join("CHARACTERS.md")
    }

    /// `<root>/GLOSSARY.md`
    pub fn glossary_md(&self) -> PathBuf {
        self.root.join("GLOSSARY.md")
    }

    /// `<root>/STYLE.md`
    pub fn style_md(&self) -> PathBuf {
        self.root.join("STYLE.md")
    }

    /// `<root>/PROJECT.md`
    pub fn project_md(&self) -> PathBuf {
        self.root.join("PROJECT.md")
    }

    /// `<vol_dir>/VOLUME.md`
    pub fn volume_md(&self) -> PathBuf {
        self.vol_dir.join("VOLUME.md")
    }

    /// `<root>/images`
    pub fn images_dir(&self) -> PathBuf {
        self.root.join("images")
    }

    /// `<vol_dir>/raw/ch_{ch:03}.md`
    pub fn raw(&self, ch: u32) -> PathBuf {
        self.vol_dir.join("raw").join(format!("ch_{:03}.md", ch))
    }

    /// `<vol_dir>/translated/ch_{ch:03}.md`
    pub fn translated(&self, ch: u32) -> PathBuf {
        self.vol_dir
            .join("translated")
            .join(format!("ch_{:03}.md", ch))
    }
}

/// Slugify an arbitrary string into a stable, filesystem-safe ascii slug.
///
/// - Lowercases ascii letters.
/// - Maps every non-`[a-z0-9]` character to `-`.
/// - Collapses runs of `-` into a single `-`.
/// - Trims leading/trailing `-`.
///
/// Non-ascii characters (e.g. Japanese) become separators, so a pure-Japanese
/// title slugifies to the empty string; callers should fall back to another id
/// source when this returns `""`.
///
/// Used by the import wizard, ingest, and `characters::upsert` (id derivation).
pub fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for ch in s.chars() {
        let mapped: Option<char> = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else {
            None
        };
        match mapped {
            Some(c) => {
                out.push(c);
                prev_dash = false;
            }
            None => {
                // Any non-alphanumeric (including unicode, whitespace, punctuation)
                // becomes a single collapsed separator.
                if !prev_dash && !out.is_empty() {
                    out.push('-');
                    prev_dash = true;
                }
            }
        }
    }
    // Trim a trailing dash produced by separators at the end of the input.
    while out.ends_with('-') {
        out.pop();
    }
    out
}
