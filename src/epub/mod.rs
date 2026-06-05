//! src/epub/mod.rs — EPUB domain types, error enum, namespace constants.
//!
//! This module (and the whole `epub::*` tree) is self-contained: it depends
//! ONLY on its own submodules and external crates. In particular `EpubError`
//! must NOT depend on `crate::model` — `error.rs` does `#[from] crate::epub::EpubError`
//! later, so a back-dependency here would create a cycle.

pub mod extract;
pub mod import;
pub mod media;
pub mod opf;
pub mod paths;
pub mod toc;

use std::collections::HashMap;
use std::path::PathBuf;

/// XML / OPF namespace URIs used throughout EPUB parsing.
pub mod ns {
    /// OPF (Open Packaging Format) package namespace.
    pub const OPF: &str = "http://www.idpf.org/2007/opf";
    /// Dublin Core metadata namespace.
    pub const DC: &str = "http://purl.org/dc/elements/1.1/";
    /// OCF container.xml namespace.
    pub const CONTAINER: &str = "urn:oasis:names:tc:opendocument:xmlns:container";
    /// NCX (EPUB2 navigation) namespace.
    pub const NCX: &str = "http://www.daisy.org/z3986/2005/ncx/";
    /// XHTML namespace (nav documents, content docs).
    pub const XHTML: &str = "http://www.w3.org/1999/xhtml";
    /// EPUB structural semantics (epub:type) namespace.
    pub const OPS: &str = "http://www.idpf.org/2007/ops";
}

/// Errors surfaced while extracting/parsing an EPUB. Deliberately free of any
/// `crate::model` dependency so the crate-wide error enum can `#[from]` it.
#[derive(thiserror::Error, Debug)]
pub enum EpubError {
    /// A zip entry resolved outside the destination directory (zip-slip).
    #[error("unsafe archive entry name: {0}")]
    UnsafeEntryName(String),

    /// Underlying filesystem I/O failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Underlying zip-archive failure.
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),

    /// Malformed XML (container.xml / OPF / NCX / nav).
    #[error("xml parse error in {context}: {source}")]
    Xml {
        context: String,
        #[source]
        source: roxmltree::Error,
    },

    /// container.xml had no usable rootfile pointing at the OPF.
    #[error("no OPF package found (META-INF/container.xml missing or invalid)")]
    MissingOpf,

    /// A spine `itemref` referenced a manifest id that does not exist.
    #[error("dangling spine reference: idref '{0}' not in manifest")]
    DanglingSpineRef(String),

    /// A zip entry's decompressed size exceeded the safety budget (zip bomb guard).
    #[error("archive entry too large (decompression-bomb guard): {0}")]
    EntryTooLarge(String),
}

/// Crate-local result alias. NOTE: this shadows `std::result::Result`; any
/// `serde_json` / other code in this module that needs the std two-arg form
/// must use the fully-qualified `std::result::Result`.
pub type Result<T> = std::result::Result<T, EpubError>;

/// A single `<item>` from the OPF `<manifest>`.
#[derive(Debug, Clone)]
#[allow(dead_code)] // full OPF record; callers use resolved_path / media_type helpers
pub struct ManifestItem {
    /// `id` attribute (unique within the manifest).
    pub id: String,
    /// Raw `href` attribute (OPF-relative, possibly percent-encoded, may carry a fragment).
    pub href: String,
    /// Archive-relative resolved path ('/'-separated, fragment stripped, percent-decoded).
    pub resolved_path: String,
    /// `media-type` attribute.
    pub media_type: String,
    /// EPUB3 `properties` tokens (e.g. "nav", "cover-image"), whitespace-split.
    pub properties: Vec<String>,
}

impl ManifestItem {
    /// True if `prop` appears among this item's `properties` tokens.
    pub fn has_property(&self, prop: &str) -> bool {
        self.properties.iter().any(|p| p == prop)
    }

    /// True if the media-type is an image type.
    pub fn is_image(&self) -> bool {
        self.media_type.starts_with("image/")
    }
}

/// A single `<itemref>` from the OPF `<spine>`, resolved against the manifest.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SpineEntry {
    /// `idref` attribute (points at a `ManifestItem::id`).
    pub idref: String,
    /// `false` only when `linear="no"` (front/back matter that readers may skip).
    pub linear: bool,
    /// Archive-relative resolved path of the referenced manifest item.
    pub resolved_path: String,
    /// Media-type of the referenced manifest item.
    pub media_type: String,
}

/// Dublin Core metadata distilled from the OPF.
#[derive(Debug, Clone, Default)]
pub struct Metadata {
    pub title: Option<String>,
    pub creator: Option<String>,
    pub language: Option<String>,
    /// Archive-relative path of the cover image, if one was identified.
    pub cover_image_path: Option<String>,
}

/// One flattened TOC entry (from nav.xhtml or NCX), depth preserved.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TocEntry {
    /// Display label.
    pub title: String,
    /// Archive-relative path of the target content document (fragment stripped).
    pub content_path: String,
    /// In-document fragment (`#id`) if the link had one.
    pub fragment: Option<String>,
    /// Nesting depth (0 = top level).
    pub depth: usize,
}

/// Fully parsed EPUB: where it was extracted, the OPF essentials, and the TOC.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct EpubBook {
    /// Directory the archive was extracted into (absolute or caller-relative).
    pub work_dir: PathBuf,
    /// Archive-relative path of the OPF package document.
    pub opf_path: String,
    /// Distilled metadata.
    pub metadata: Metadata,
    /// All manifest items in document order.
    pub manifest: Vec<ManifestItem>,
    /// `id` -> index into `manifest`.
    pub manifest_by_id: HashMap<String, usize>,
    /// Spine entries in reading order.
    pub spine: Vec<SpineEntry>,
    /// Flattened table of contents (nav.xhtml preferred over NCX).
    pub toc: Vec<TocEntry>,
}
