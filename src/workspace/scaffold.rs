//! Create the project tree + metadata templates. On import we materialize the
//! canonical layout and write all five metadata files, each with a `honya:data`
//! block so the agents can read/mutate them through the workspace API.

use std::path::Path;

use chrono::Local;
use serde::{Deserialize, Serialize};

use crate::model::{ModelSet, ProjectStatus};
use crate::workspace::Workspace;
use crate::workspace::data_block;
use crate::workspace::{characters, glossary, volume};

/// PROJECT.md machine payload (kept minimal; expanded by the app over time).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ProjectMeta {
    title: String,
    created: String,
    models: ModelSet,
    /// One-line synopsis (free text, human-editable above the block too).
    #[serde(default)]
    synopsis: String,
}

/// STYLE.md machine payload — toggles the style-guide rendering reads.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StyleMeta {
    created: String,
    /// Translation progress (`draft` / `in_progress` / `done`); surfaced as the
    /// STYLE.md status line and the Context panel. Kept in sync by [`sync_status`].
    status: String,
}

/// Markdown bullet whose value [`sync_status`] rewrites in STYLE.md / PROJECT.md.
const STATUS_LINE_PREFIX: &str = "- **สถานะ / Status:**";

/// Replace the value of the "สถานะ / Status:" bullet, preserving every other line
/// (appended style notes, the synopsis, etc.). Returns the body unchanged when no
/// such line is present.
fn rewrite_status_line(body: &str, label: &str) -> String {
    let mut replaced = false;
    let lines: Vec<String> = body
        .lines()
        .map(|line| {
            if !replaced && line.trim_start().starts_with(STATUS_LINE_PREFIX) {
                replaced = true;
                format!("{STATUS_LINE_PREFIX} {label}")
            } else {
                line.to_string()
            }
        })
        .collect();
    lines.join("\n")
}

/// Persist the project's live translation `status` into STYLE.md (body line + data
/// block) and PROJECT.md (body line), surgically — appended style notes, the
/// synopsis, and the rest of each data block are preserved. A no-op for files that
/// don't exist yet, and skips the write when nothing would change.
pub fn sync_status(ws: &Workspace, status: ProjectStatus) -> std::io::Result<()> {
    // STYLE.md: update both the human-readable line and the machine status field.
    let style_path = ws.style_md();
    if style_path.exists() {
        let body = data_block::read_body(&style_path);
        let mut meta: StyleMeta = data_block::read_data_block(&style_path);
        let new_body = rewrite_status_line(&body, status.label_th());
        if meta.status != status.slug() || new_body != body {
            meta.status = status.slug().to_string();
            data_block::write_with_data(&style_path, &new_body, &meta)?;
        }
    }

    // PROJECT.md: only the body line — its data block (title/synopsis/models) is
    // re-read and written back verbatim so the synopsis is never clobbered.
    let project_path = ws.project_md();
    if project_path.exists() {
        let body = data_block::read_body(&project_path);
        let new_body = rewrite_status_line(&body, status.label_th());
        if new_body != body {
            let meta: ProjectMeta = data_block::read_data_block(&project_path);
            data_block::write_with_data(&project_path, &new_body, &meta)?;
        }
    }

    Ok(())
}

/// Create the project tree and write all root metadata + the first volume.
/// Dir creation is idempotent; root metadata is written only when absent.
pub fn create_project(
    root: &Path,
    title: &str,
    models: &ModelSet,
    vol_number: u32,
) -> std::io::Result<()> {
    let ws = Workspace::new(root.to_path_buf(), vol_number);

    std::fs::create_dir_all(root)?;
    std::fs::create_dir_all(ws.images_dir())?;
    std::fs::create_dir_all(ws.vol_dir.join("raw"))?;
    std::fs::create_dir_all(ws.vol_dir.join("translated"))?;

    let date = Local::now().format("%Y-%m-%d").to_string();

    // Each root metadata file is written only when absent, so re-importing a
    // volume never clobbers a built-up CHARACTERS / GLOSSARY / PROJECT / STYLE.
    if !ws.project_md().exists() {
        let project_meta = ProjectMeta {
            title: title.to_string(),
            created: date.clone(),
            models: models.clone(),
            synopsis: String::new(),
        };
        data_block::write_with_data(
            &ws.project_md(),
            &render_project_body(title, &date, models),
            &project_meta,
        )?;
    }

    if !ws.characters_md().exists() {
        data_block::write_with_data(
            &ws.characters_md(),
            &characters::render_table(&[]),
            &EmptyCharacters::default(),
        )?;
    }

    if !ws.glossary_md().exists() {
        data_block::write_with_data(
            &ws.glossary_md(),
            &glossary::render_table(&[]),
            &EmptyTerms::default(),
        )?;
    }

    if !ws.style_md().exists() {
        let style_meta = StyleMeta {
            created: date.clone(),
            status: "draft".to_string(),
        };
        data_block::write_with_data(&ws.style_md(), &render_style_body(&date), &style_meta)?;
    }

    write_volume_md(&ws, None)?;

    Ok(())
}

/// Ensure `Vol_NN` exists with raw/translated subdirs and a VOLUME.md; existing
/// VOLUME.md content is preserved (loaded + re-rendered).
#[allow(dead_code)]
pub fn ensure_volume(root: &Path, vol_number: u32, label: Option<&str>) -> std::io::Result<()> {
    let ws = Workspace::new(root.to_path_buf(), vol_number);
    std::fs::create_dir_all(ws.vol_dir.join("raw"))?;
    std::fs::create_dir_all(ws.vol_dir.join("translated"))?;
    write_volume_md(&ws, label)
}

fn render_project_body(title: &str, date: &str, models: &ModelSet) -> String {
    format!(
        "# {title}\n\
         \n\
         - **สร้างเมื่อ / Created:** {date}\n\
         - **สถานะ / Status:** กำลังนำเข้า (importing)\n\
         \n\
         ## เรื่องย่อ / Synopsis\n\
         \n\
         _ยังไม่มีเรื่องย่อ — แก้ไขได้ที่นี่_\n\
         \n\
         ## โมเดลที่ใช้ / Models\n\
         \n\
         | บทบาท / Role | Model |\n\
         |--------------|-------|\n\
         | Orchestrator | `{orch}` |\n\
         | Translator   | `{trans}` |\n\
         | Reviewer     | `{rev}` |\n\
         \n\
         ## ไฟล์อ้างอิง / Reference files\n\
         \n\
         - `CHARACTERS.md` — ตัวละคร สรรพนาม น้ำเสียง\n\
         - `GLOSSARY.md` — คำศัพท์เฉพาะ ชื่อสถานที่ สกิล\n\
         - `STYLE.md` — แนวทางการเรียบเรียงและน้ำเสียงรวม\n",
        title = title,
        date = date,
        orch = models.orchestrator,
        trans = models.translator,
        rev = models.reviewer,
    )
}

fn render_style_body(date: &str) -> String {
    format!(
        "# แนวทางการแปล / Style Guide\n\
         \n\
         - **สร้างเมื่อ / Created:** {date}\n\
         - **สถานะ / Status:** ฉบับร่าง (draft)\n\
         \n\
         ## น้ำเสียงรวม / Overall Tone\n\
         \n\
         _กำหนดน้ำเสียงโดยรวมของงานแปล เช่น ทางการ/กันเอง ระดับความลื่นไหล_\n\
         \n\
         ## หลักการเรียบเรียง / Rendering Principles\n\
         \n\
         1. รักษาอารมณ์ ความรู้สึก และบุคลิกของตัวละครต้นฉบับ\n\
         2. หลีกเลี่ยงการแปลตรงตัว เรียบเรียงให้เป็นภาษาไทยที่เป็นธรรมชาติ\n\
         3. คงองค์ประกอบ Markdown (**ตัวหนา**, *ตัวเอียง*, “คำพูด”, ลิงก์ภาพ) ให้ครบถ้วน\n\
         4. บังคับใช้คำศัพท์และสรรพนามตาม `GLOSSARY.md` และ `CHARACTERS.md`\n\
         \n\
         ## ข้อตกลงเฉพาะเรื่อง / Series-specific Conventions\n\
         \n\
         _บันทึกข้อตกลงเฉพาะของซีรีส์นี้ เช่น การทับศัพท์ คำลงท้าย ระบบเรียกขาน_\n",
        date = date,
    )
}

fn write_volume_md(ws: &Workspace, label: Option<&str>) -> std::io::Result<()> {
    // Load existing data so re-running never destroys content.
    let mut data = volume::load(ws);

    // Seed the recap only for a brand-new volume with a known label.
    if data.running_recap.trim().is_empty()
        && let Some(lbl) = label.filter(|l| !l.trim().is_empty())
    {
        data.running_recap = format!("เล่ม: {}", lbl.trim());
    }

    let body = volume::render_body(&data);
    data_block::write_with_data(&ws.volume_md(), &body, &data)
}

// Empty-payload wrappers matching the characters.rs / glossary.rs block shapes.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct EmptyCharacters {
    characters: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct EmptyTerms {
    terms: Vec<serde_json::Value>,
}

#[cfg(test)]
mod status_tests {
    use super::*;
    use crate::workspace::style;

    fn temp_root(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("honya_status_{}_{}", tag, std::process::id()))
    }

    #[test]
    fn sync_status_updates_style_and_project_surgically() {
        let root = temp_root("sync");
        let _ = std::fs::remove_dir_all(&root);
        let ws = Workspace::new(root.clone(), 1);
        create_project(&root, "Test", &ModelSet::default(), 1).unwrap();

        // A style note the Orchestrator might append must survive a status sync.
        style::append_note(&ws, "ห้ามแปลชื่อสกิล").unwrap();

        sync_status(&ws, ProjectStatus::Done).unwrap();

        let style = std::fs::read_to_string(ws.style_md()).unwrap();
        assert!(style.contains("สถานะ / Status:** เสร็จสมบูรณ์ (done)"));
        // Machine field flipped too, and the appended note is preserved.
        let meta: StyleMeta = data_block::read_data_block(&ws.style_md());
        assert_eq!(meta.status, "done");
        assert!(style.contains("ห้ามแปลชื่อสกิล"));

        // PROJECT.md's "importing" line advances; its data block is untouched.
        let project = std::fs::read_to_string(ws.project_md()).unwrap();
        assert!(project.contains("สถานะ / Status:** เสร็จสมบูรณ์ (done)"));
        assert!(!project.contains("กำลังนำเข้า (importing)"));
        let pmeta: ProjectMeta = data_block::read_data_block(&ws.project_md());
        assert_eq!(pmeta.title, "Test");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sync_status_noop_when_unchanged() {
        let root = temp_root("noop");
        let _ = std::fs::remove_dir_all(&root);
        let ws = Workspace::new(root.clone(), 1);
        create_project(&root, "Test", &ModelSet::default(), 1).unwrap();

        // Fresh project is already "draft"; re-syncing draft must not rewrite the file.
        sync_status(&ws, ProjectStatus::Draft).unwrap();
        let before = std::fs::read_to_string(ws.style_md()).unwrap();
        sync_status(&ws, ProjectStatus::Draft).unwrap();
        let after = std::fs::read_to_string(ws.style_md()).unwrap();
        assert_eq!(before, after);

        let _ = std::fs::remove_dir_all(&root);
    }
}
