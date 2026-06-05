//! src/workspace/scaffold.rs — create the project tree + metadata templates.
//!
//! On import we materialize the canonical layout and write all five metadata
//! files, each carrying a `<!-- honya:data ... honya:data -->` block so the
//! agents can immediately read/mutate them through the workspace API.
//!
//! ```text
//! <root>/
//! ├── PROJECT.md          (synopsis, models, config — honya:data: ProjectMeta)
//! ├── CHARACTERS.md       (empty character table     — honya:data: {characters:[]})
//! ├── GLOSSARY.md         (empty glossary table      — honya:data: {terms:[]})
//! ├── STYLE.md            (style guide draft         — honya:data: StyleMeta)
//! ├── images/
//! └── Vol_NN/
//!     ├── VOLUME.md        (recap/summaries/notes     — honya:data: VolumeData)
//!     ├── raw/
//!     └── translated/
//! ```

use std::path::Path;

use chrono::Local;
use serde::{Deserialize, Serialize};

use crate::model::ModelSet;
use crate::workspace::Workspace;
use crate::workspace::data_block;
use crate::workspace::{characters, glossary, volume};

/// PROJECT.md machine payload (kept minimal; expanded by the app over time).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectMeta {
    title: String,
    created: String,
    models: ModelSet,
    /// One-line synopsis (free text, human-editable above the block too).
    #[serde(default)]
    synopsis: String,
}

/// STYLE.md machine payload — toggles the style-guide rendering reads.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StyleMeta {
    created: String,
    /// Draft vs finalized; surfaced as the STYLE.md status in the Context panel.
    status: String,
}

/// Create the full project tree and write all root metadata + the first volume.
///
/// Idempotent for directory creation (`create_dir_all`); metadata files are
/// (re)written from the templates, so call this once on import.
pub fn create_project(
    root: &Path,
    title: &str,
    models: &ModelSet,
    vol_number: u32,
) -> std::io::Result<()> {
    let ws = Workspace::new(root.to_path_buf(), vol_number);

    // Directory tree.
    std::fs::create_dir_all(root)?;
    std::fs::create_dir_all(ws.images_dir())?;
    std::fs::create_dir_all(ws.vol_dir.join("raw"))?;
    std::fs::create_dir_all(ws.vol_dir.join("translated"))?;

    let date = Local::now().format("%Y-%m-%d").to_string();

    // Root metadata files are written ONLY when absent, so re-importing another
    // volume into an existing project never clobbers a built-up CHARACTERS /
    // GLOSSARY / PROJECT / STYLE (directory creation above is already idempotent).

    // PROJECT.md
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

    // CHARACTERS.md (empty table + {characters:[]})
    if !ws.characters_md().exists() {
        data_block::write_with_data(
            &ws.characters_md(),
            &characters::render_table(&[]),
            &EmptyCharacters::default(),
        )?;
    }

    // GLOSSARY.md (empty table + {terms:[]})
    if !ws.glossary_md().exists() {
        data_block::write_with_data(
            &ws.glossary_md(),
            &glossary::render_table(&[]),
            &EmptyTerms::default(),
        )?;
    }

    // STYLE.md
    if !ws.style_md().exists() {
        let style_meta = StyleMeta {
            created: date.clone(),
            status: "draft".to_string(),
        };
        data_block::write_with_data(&ws.style_md(), &render_style_body(&date), &style_meta)?;
    }

    // VOLUME.md for the first volume.
    write_volume_md(&ws, None)?;

    Ok(())
}

/// Ensure `Vol_NN` exists with its raw/translated subdirs and a VOLUME.md.
///
/// Existing VOLUME.md content is preserved: the volume data block is loaded and
/// re-rendered (only refreshing the label when one is supplied).
pub fn ensure_volume(root: &Path, vol_number: u32, label: Option<&str>) -> std::io::Result<()> {
    let ws = Workspace::new(root.to_path_buf(), vol_number);
    std::fs::create_dir_all(ws.vol_dir.join("raw"))?;
    std::fs::create_dir_all(ws.vol_dir.join("translated"))?;
    // Preserves existing VOLUME.md data (loaded + re-rendered); seeds the label
    // into the recap only when the recap is still empty.
    write_volume_md(&ws, label)
}

// --- template bodies --------------------------------------------------------

fn render_project_body(title: &str, date: &str, models: &ModelSet) -> String {
    format!(
        "# {title}\n\
         \n\
         > โปรเจกต์แปลนิยายไลท์โนเวล ญี่ปุ่น → ไทย (honya 本屋)\n\
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
    // Load existing data so re-running ensure_volume never destroys content.
    let mut data = volume::load(ws);

    // Seed an empty recap for a brand-new volume with a known label.
    if data.running_recap.trim().is_empty() {
        if let Some(lbl) = label.filter(|l| !l.trim().is_empty()) {
            data.running_recap = format!("เล่ม: {}", lbl.trim());
        }
    }

    let body = volume::render_body(&data);
    data_block::write_with_data(&ws.volume_md(), &body, &data)
}

// --- empty-payload wrappers (match characters.rs / glossary.rs block shapes) -

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EmptyCharacters {
    characters: Vec<serde_json::Value>,
}
impl Default for EmptyCharacters {
    fn default() -> Self {
        Self { characters: Vec::new() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EmptyTerms {
    terms: Vec<serde_json::Value>,
}
impl Default for EmptyTerms {
    fn default() -> Self {
        Self { terms: Vec::new() }
    }
}
