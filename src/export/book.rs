//! Gather one volume's translated chapters and metadata for export renderers.
//! `translation::export_prose` strips chunk markers/review banners; leading Thai
//! headings become chapter titles, and non-fatal gaps become warnings.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::model::{Chapter, ChapterKind, ChapterStatus};
use crate::workspace::{Workspace, translation, volume};

use super::blocks;

/// One volume gathered for export.
#[derive(Debug, Clone)]
pub struct ExportBook {
    pub project_title: String,
    pub project_id: String,
    pub volume_number: u32,
    pub volume_label: Option<String>,
    pub synopsis_th: String,
    /// BCP-47 language of the output (always Thai here).
    pub language: String,
    pub chapters: Vec<ExportChapter>,
    /// Absolute path to the project's shared `images/` directory (image source).
    pub images_dir: PathBuf,
    /// Every image basename actually present on disk and referenced by a chapter,
    /// sorted; the set every renderer embeds / manifests.
    pub images: Vec<String>,
    /// Human-readable, non-fatal notes (skipped/NeedsReview chapters, missing images).
    pub warnings: Vec<String>,
}

/// One chapter's cleaned body for export.
#[derive(Debug, Clone)]
pub struct ExportChapter {
    pub number: u32,
    /// Display title — the translated heading when present, else the scan title.
    pub title: String,
    /// Cleaned Markdown body (markers + QA banner stripped, leading title removed).
    pub markdown: String,
}

/// `# {project} — เล่ม {NN}` style book title.
impl ExportBook {
    pub fn display_title(&self) -> String {
        format!("{} — เล่ม {:02}", self.project_title, self.volume_number)
    }
}

/// Read the volume's chapters off disk and assemble the export book.
pub async fn gather(
    ws: &Workspace,
    project_title: &str,
    project_id: &str,
    vol: u32,
    vol_label: Option<String>,
    chapters: &[Chapter],
) -> ExportBook {
    let vol_data = volume::load(ws);
    let images_dir = ws.images_dir();

    let mut out_chapters = Vec::new();
    let mut warnings = Vec::new();
    let mut all_images: BTreeSet<String> = BTreeSet::new();

    for ch in chapters {
        let raw = translation::read_translated(ws, ch.number).await;
        let cleaned = translation::export_prose(&raw);
        let label = format!("ch_{:03} “{}”", ch.number, ch.title);

        if cleaned.trim().is_empty() {
            // Empty/front-matter chapters are allowed to stay blank.
            if ch.kind != ChapterKind::Empty {
                warnings.push(format!("{label} — no translation (skipped)"));
            }
            continue;
        }

        match ch.status {
            ChapterStatus::NeedsReview => warnings.push(format!("{label} — still NeedsReview")),
            ChapterStatus::Failed => warnings.push(format!("{label} — failed run (partial)")),
            ChapterStatus::Pending => warnings.push(format!("{label} — still Pending (partial)")),
            ChapterStatus::Partial => {
                warnings.push(format!("{label} — translation incomplete (partial)"))
            }
            _ => {}
        }

        let (heading_title, body) = blocks::split_leading_title(&cleaned);
        let title = heading_title
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| ch.title.clone());

        // Only embed files that still exist on disk.
        for file in blocks::referenced_images(&body) {
            if images_dir.join(&file).is_file() {
                all_images.insert(file);
            } else {
                warnings.push(format!("{label} — image not found: {file}"));
            }
        }

        out_chapters.push(ExportChapter {
            number: ch.number,
            title,
            markdown: body,
        });
    }

    ExportBook {
        project_title: project_title.to_string(),
        project_id: project_id.to_string(),
        volume_number: vol,
        volume_label: vol_label,
        synopsis_th: vol_data.synopsis_th,
        language: "th".to_string(),
        chapters: out_chapters,
        images_dir,
        images: all_images.into_iter().collect(),
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::UsageStats;

    fn chapter(number: u32, title: &str, kind: ChapterKind, status: ChapterStatus) -> Chapter {
        Chapter {
            number,
            title: title.to_string(),
            kind,
            status,
            source_segments: 0,
            total_chunks: 0,
            committed_chunks: 0,
            skipped_chunks: 0,
            last_run: None,
            usage: UsageStats::default(),
        }
    }

    #[tokio::test]
    async fn gather_strips_banner_and_warns_on_incomplete() {
        let root = std::env::temp_dir().join(format!("honya_gather_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ws = Workspace::new(root.clone(), 1);
        std::fs::create_dir_all(ws.vol_dir.join("translated")).unwrap();

        std::fs::write(
            ws.translated(1),
            "<!-- honya:chunk 0 -->\n\
             <!-- honya:review-needed -->\n\
             > ⚠️ **[REVIEW NEEDED]** chunk 1 — แปลไม่ผ่าน\n\
             >\n\
             > เหตุผลจากผู้ตรวจ: เพราะเหตุผล\n\
             เนื้อหาไทยที่ดี\n",
        )
        .unwrap();
        std::fs::write(ws.translated(3), "<!-- honya:chunk 0 -->\nบทที่สาม\n").unwrap();

        let chapters = vec![
            chapter(1, "หนึ่ง", ChapterKind::Prose, ChapterStatus::NeedsReview),
            chapter(2, "สอง", ChapterKind::Prose, ChapterStatus::Pending),
            chapter(3, "สาม", ChapterKind::Prose, ChapterStatus::Done),
        ];
        let book = gather(&ws, "โปรเจกต์", "proj", 1, None, &chapters).await;

        assert_eq!(book.chapters.len(), 2);
        let ch1 = &book.chapters[0];
        assert_eq!(ch1.number, 1);
        assert!(!ch1.markdown.contains("REVIEW NEEDED"));
        assert!(!ch1.markdown.contains("honya:chunk"));
        assert!(ch1.markdown.contains("เนื้อหาไทยที่ดี"));
        assert!(book.warnings.iter().any(|w| w.contains("NeedsReview")));
        assert!(book.warnings.iter().any(|w| w.contains("no translation")));

        let _ = std::fs::remove_dir_all(&root);
    }
}
