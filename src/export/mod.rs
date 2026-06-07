//! Export a finished volume to shareable deliverables — merged Markdown, EPUB3, and
//! DOCX — so work that ends as Thai Markdown under `translated/` becomes something a
//! user can hand to an editor, sideload onto an e-reader, or open in Word.
//!
//! Flow: [`book::gather`] reads the volume off disk into an [`ExportBook`], then
//! [`export_volume`] renders each requested [`ExportFormat`] into `<root>/exports/`,
//! emitting [`AppEvent::ExportProgress`] per step. Everything is pure Rust over the
//! vendored `zip` writer and hand-built XML — no new dependencies (see CLAUDE.md).

pub mod blocks;
pub mod book;
pub mod docx;
pub mod epub;
pub mod html;
pub mod markdown;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::model::{AppEvent, EventTx};
use crate::workspace::Workspace;

pub use book::{ExportBook, gather};

/// A deliverable output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Markdown,
    Epub,
    Docx,
}

impl ExportFormat {
    /// All formats in display/selection order (drives the overlay checklist).
    pub const ALL: [ExportFormat; 3] = [
        ExportFormat::Markdown,
        ExportFormat::Epub,
        ExportFormat::Docx,
    ];

    /// Lower-case file extension.
    pub fn ext(self) -> &'static str {
        match self {
            ExportFormat::Markdown => "md",
            ExportFormat::Epub => "epub",
            ExportFormat::Docx => "docx",
        }
    }

    /// Short human label.
    pub fn label(self) -> &'static str {
        match self {
            ExportFormat::Markdown => "Markdown",
            ExportFormat::Epub => "EPUB",
            ExportFormat::Docx => "DOCX",
        }
    }
}

/// The `exports/` directory under a project root.
pub fn exports_dir(root: &Path) -> PathBuf {
    root.join("exports")
}

/// Render `book` to each requested `format` under `<root>/exports/`. Returns the
/// written file paths and the gathered, non-fatal warnings. Each renderer runs on a
/// blocking thread (zip + image I/O) so the async runtime never stalls.
pub async fn export_volume(
    ws: &Workspace,
    book: ExportBook,
    formats: &[ExportFormat],
    tx: &EventTx,
) -> anyhow::Result<(Vec<PathBuf>, Vec<String>)> {
    let dir = exports_dir(&ws.root);
    tokio::fs::create_dir_all(&dir).await?;

    let warnings = book.warnings.clone();
    let book = Arc::new(book);
    let base = format!("{}_Vol_{:02}", book.project_id, book.volume_number);
    let total = formats.len();
    let mut paths = Vec::with_capacity(total);

    for (i, &format) in formats.iter().enumerate() {
        tx.send(AppEvent::ExportProgress {
            done: i,
            total,
            label: format.label().to_string(),
        });

        let out = dir.join(format!("{base}.{}", format.ext()));
        let book = Arc::clone(&book);
        let out_for_task = out.clone();
        tokio::task::spawn_blocking(move || render(format, &book, &out_for_task))
            .await?
            .map_err(|e| anyhow::anyhow!("{}: {e}", format.label()))?;
        paths.push(out);

        tx.send(AppEvent::ExportProgress {
            done: i + 1,
            total,
            label: format.label().to_string(),
        });
        tokio::task::yield_now().await;
    }

    Ok((paths, warnings))
}

/// Dispatch one format to its renderer (sync; runs on a blocking thread).
fn render(format: ExportFormat, book: &ExportBook, out: &Path) -> std::io::Result<()> {
    match format {
        ExportFormat::Markdown => markdown::write(book, out),
        ExportFormat::Epub => epub::write(book, out),
        ExportFormat::Docx => docx::write(book, out),
    }
}

#[cfg(test)]
mod tests {
    use super::book::{ExportBook, ExportChapter};
    use super::*;
    use crate::model::EventTx;

    #[test]
    fn format_metadata() {
        assert_eq!(ExportFormat::Epub.ext(), "epub");
        assert_eq!(ExportFormat::Docx.label(), "DOCX");
        assert_eq!(ExportFormat::ALL.len(), 3);
    }

    fn fake_png() -> Vec<u8> {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(&[0, 0, 0, 13]);
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&8u32.to_be_bytes());
        png.extend_from_slice(&8u32.to_be_bytes());
        png
    }

    /// Export an EPUB, then re-import it with the production importer: the round trip
    /// proves the generated package is structurally valid (container → OPF → spine →
    /// manifest), reusing `epub::import` as an oracle.
    #[tokio::test]
    async fn epub_export_round_trips_through_importer() {
        let root = std::env::temp_dir().join(format!("honya_epub_rt_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let images = root.join("images");
        std::fs::create_dir_all(&images).unwrap();
        std::fs::write(images.join("p.png"), fake_png()).unwrap();

        let book = ExportBook {
            project_title: "Test Novel".to_string(),
            project_id: "test".to_string(),
            volume_number: 1,
            volume_label: None,
            synopsis_th: "เรื่องย่อ".to_string(),
            language: "th".to_string(),
            chapters: vec![
                ExportChapter {
                    number: 1,
                    title: "บทที่ ๑".to_string(),
                    markdown: "เนื้อหาบทแรก\n\n![ภาพ](../../images/p.png)".to_string(),
                },
                ExportChapter {
                    number: 2,
                    title: "บทที่ ๒".to_string(),
                    markdown: "เนื้อหาบทสอง".to_string(),
                },
            ],
            images_dir: images.clone(),
            images: vec!["p.png".to_string()],
            warnings: vec![],
        };

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let ws = crate::workspace::Workspace::new(root.clone(), 1);
        let (paths, warnings) = export_volume(&ws, book, &[ExportFormat::Epub], &EventTx(tx))
            .await
            .unwrap();
        assert_eq!(paths.len(), 1);
        assert!(warnings.is_empty());
        let epub = &paths[0];
        assert!(epub.exists());

        let work = root.join("reimport");
        let imported = crate::epub::import::import_epub(epub, &work).unwrap();
        // front matter + 2 chapters = 3 spine documents.
        assert_eq!(imported.spine.len(), 3);
        assert!(
            imported
                .metadata
                .title
                .as_deref()
                .unwrap_or_default()
                .contains("Test Novel")
        );
        assert_eq!(imported.metadata.language.as_deref(), Some("th"));
        // The image survived into the manifest.
        assert!(imported.manifest.iter().any(|m| m.href.contains("p.png")));

        let _ = std::fs::remove_dir_all(&root);
    }
}
