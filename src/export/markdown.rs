//! Merged-Markdown export: one `.md` file for the whole volume — a metadata front
//! page, then every chapter under a `## ` heading, separated by `---`. Images are
//! copied into a sibling `images/` directory and links rewritten to `images/FILE`,
//! so the `.md` + `images/` pair is self-contained and portable.

use std::io;
use std::path::Path;

use super::blocks;
use super::book::ExportBook;

/// Write the merged Markdown to `out_path` and copy referenced images alongside it.
pub fn write(book: &ExportBook, out_path: &Path) -> io::Result<()> {
    let dir = out_path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;

    let mut doc = String::new();

    // ---- metadata front page ----
    doc.push_str(&format!("# {}\n\n", book.display_title()));
    if let Some(label) = book.volume_label.as_ref().filter(|l| !l.trim().is_empty()) {
        doc.push_str(&format!("**{}**\n\n", label.trim()));
    }
    let synopsis = book.synopsis_th.trim();
    if !synopsis.is_empty() {
        doc.push_str("## เรื่องย่อ\n\n");
        doc.push_str(synopsis);
        doc.push_str("\n\n");
    }

    // ---- chapters ----
    for ch in &book.chapters {
        doc.push_str("---\n\n");
        doc.push_str(&format!("## {}\n\n", ch.title.trim()));
        doc.push_str(blocks::rewrite_image_links(&ch.markdown, "images/").trim());
        doc.push_str("\n\n");
    }

    std::fs::write(out_path, doc.trim_start().as_bytes())?;

    // ---- copy images ----
    if !book.images.is_empty() {
        let img_out = dir.join("images");
        std::fs::create_dir_all(&img_out)?;
        for file in &book.images {
            let src = book.images_dir.join(file);
            let dst = img_out.join(file);
            // Best-effort: a missing/locked image must not abort the export.
            let _ = std::fs::copy(&src, &dst);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::book::{ExportBook, ExportChapter};
    use std::path::PathBuf;

    fn book() -> ExportBook {
        ExportBook {
            project_title: "นิยาย".to_string(),
            project_id: "novel".to_string(),
            volume_number: 1,
            volume_label: Some("黎明".to_string()),
            synopsis_th: "เรื่องย่อทดสอบ".to_string(),
            language: "th".to_string(),
            chapters: vec![
                ExportChapter {
                    number: 1,
                    title: "บทที่ ๑".to_string(),
                    markdown: "เนื้อหา ![ภาพ](../../images/a.png)".to_string(),
                },
                ExportChapter {
                    number: 2,
                    title: "บทที่ ๒".to_string(),
                    markdown: "อีกบท".to_string(),
                },
            ],
            images_dir: PathBuf::from("/nonexistent"),
            images: vec!["a.png".to_string()],
            warnings: vec![],
        }
    }

    #[test]
    fn merged_markdown_has_frontpage_separators_and_rewritten_links() {
        let dir = std::env::temp_dir().join(format!("honya_md_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join("out.md");
        write(&book(), &out).unwrap();
        let doc = std::fs::read_to_string(&out).unwrap();
        assert!(doc.contains("# นิยาย — เล่ม 01"));
        assert!(doc.contains("## เรื่องย่อ"));
        assert!(doc.contains("## บทที่ ๑"));
        assert!(doc.contains("## บทที่ ๒"));
        assert!(doc.matches("---").count() >= 2); // a separator before each chapter
        assert!(doc.contains("![ภาพ](images/a.png)")); // rewritten
        let _ = std::fs::remove_dir_all(&dir);
    }
}
