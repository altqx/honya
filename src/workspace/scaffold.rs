//! Create the project tree + metadata templates. On import we materialize the
//! canonical layout and write all five metadata files, each with a `honya:data`
//! block so the agents can read/mutate them through the workspace API.

use std::path::Path;

use chrono::Local;
use serde::{Deserialize, Serialize};

use crate::model::{ModelSet, ProjectStatus, TargetLanguage};
use crate::workspace::Workspace;
use crate::workspace::data_block;
use crate::workspace::{characters, glossary, volume};

/// PROJECT.md machine payload (kept minimal; expanded by the app over time).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ProjectMeta {
    title: String,
    /// Target-language title, empty until set.
    #[serde(default, alias = "title_th")]
    translated_title: String,
    created: String,
    models: ModelSet,
    /// One-line synopsis (free text, human-editable above the block too).
    #[serde(default)]
    synopsis: String,
    #[serde(default)]
    target_language: TargetLanguage,
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
const STATUS_LINE_PREFIX_EN: &str = "- **Status:**";

const TITLE_TH_LINE_PREFIX: &str = "- **ชื่อไทย / Thai title:**";
const TITLE_TARGET_LINE_PREFIX: &str = "- **Translated title:**";

/// Rewrite PROJECT.md title fields while preserving unrelated body/data.
pub fn set_title(ws: &Workspace, title: &str, translated_title: &str) -> std::io::Result<()> {
    let path = ws.project_md();
    let title = title.trim();
    let translated_title = translated_title.trim();

    let mut meta: ProjectMeta = data_block::read_data_block(&path);
    meta.title = title.to_string();
    meta.translated_title = translated_title.to_string();
    let title_prefix = match meta.target_language {
        TargetLanguage::Thai => TITLE_TH_LINE_PREFIX,
        TargetLanguage::English => TITLE_TARGET_LINE_PREFIX,
    };

    let body = data_block::read_body(&path);
    let mut heading_rewritten = false;
    let mut lines: Vec<String> = Vec::new();
    for line in body.lines() {
        if !heading_rewritten && line.trim_start().starts_with("# ") {
            heading_rewritten = true;
            lines.push(format!("# {title}"));
            if !translated_title.is_empty() {
                lines.push(format!("{title_prefix} {translated_title}"));
            }
            continue;
        }
        if line.trim_start().starts_with(TITLE_TH_LINE_PREFIX)
            || line.trim_start().starts_with(TITLE_TARGET_LINE_PREFIX)
        {
            continue;
        }
        lines.push(line.to_string());
    }
    if !heading_rewritten {
        // Preserve hand-edited files with no heading.
        let mut head = vec![format!("# {title}")];
        if !translated_title.is_empty() {
            head.push(format!("{title_prefix} {translated_title}"));
        }
        head.extend(lines);
        lines = head;
    }

    data_block::write_with_data(&path, &lines.join("\n"), &meta)
}

pub fn read_translated_title(project_dir: &Path) -> String {
    let meta: ProjectMeta = data_block::read_data_block(&project_dir.join("PROJECT.md"));
    meta.translated_title
}

pub fn read_target_language(project_dir: &Path) -> TargetLanguage {
    let meta: ProjectMeta = data_block::read_data_block(&project_dir.join("PROJECT.md"));
    meta.target_language
}

/// Replace the value of the "สถานะ / Status:" bullet, preserving every other line
/// (appended style notes, the synopsis, etc.). Returns the body unchanged when no
/// such line is present.
fn rewrite_status_line(body: &str, label: &str) -> String {
    let mut replaced = false;
    let lines: Vec<String> = body
        .lines()
        .map(|line| {
            let prefix = if line.trim_start().starts_with(STATUS_LINE_PREFIX) {
                Some(STATUS_LINE_PREFIX)
            } else if line.trim_start().starts_with(STATUS_LINE_PREFIX_EN) {
                Some(STATUS_LINE_PREFIX_EN)
            } else {
                None
            };
            if !replaced && let Some(prefix) = prefix {
                replaced = true;
                format!("{prefix} {label}")
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
    let project_meta: ProjectMeta = data_block::read_data_block(&ws.project_md());
    let label = match project_meta.target_language {
        TargetLanguage::Thai => status.label_th(),
        TargetLanguage::English => status.label_en(),
    };
    // STYLE.md: update both the human-readable line and the machine status field.
    let style_path = ws.style_md();
    if style_path.exists() {
        let body = data_block::read_body(&style_path);
        let mut meta: StyleMeta = data_block::read_data_block(&style_path);
        let new_body = rewrite_status_line(&body, label);
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
        let new_body = rewrite_status_line(&body, label);
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
    create_project_for_language(root, title, models, vol_number, TargetLanguage::Thai)
}

pub fn create_project_for_language(
    root: &Path,
    title: &str,
    models: &ModelSet,
    vol_number: u32,
    target_language: TargetLanguage,
) -> std::io::Result<()> {
    let ws = Workspace::new(root.to_path_buf(), vol_number);

    std::fs::create_dir_all(root)?;
    std::fs::create_dir_all(ws.images_dir())?;
    std::fs::create_dir_all(ws.vol_dir.join("raw"))?;
    std::fs::create_dir_all(ws.vol_dir.join("translated"))?;

    let date = Local::now().format("%Y-%m-%d").to_string();

    // Each root metadata file is written only when absent, so re-importing a
    // volume never clobbers a built-up CHARACTERS / GLOSSARY / PROJECT / STYLE.
    let project_language = if ws.project_md().exists() {
        read_target_language(root)
    } else {
        target_language
    };
    if !ws.project_md().exists() {
        let project_meta = ProjectMeta {
            title: title.to_string(),
            translated_title: String::new(),
            created: date.clone(),
            models: models.clone(),
            synopsis: String::new(),
            target_language: project_language,
        };
        data_block::write_with_data(
            &ws.project_md(),
            &render_project_body_for_language(title, &date, models, project_language),
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
        data_block::write_with_data(
            &ws.style_md(),
            &render_style_body_for_language(&date, project_language),
            &style_meta,
        )?;
    }

    write_volume_md(&ws, None, project_language)?;

    Ok(())
}

/// Ensure `Vol_NN` exists with raw/translated subdirs and a VOLUME.md; existing
/// VOLUME.md content is preserved (loaded + re-rendered).
#[allow(dead_code)]
pub fn ensure_volume(root: &Path, vol_number: u32, label: Option<&str>) -> std::io::Result<()> {
    let ws = Workspace::new(root.to_path_buf(), vol_number);
    std::fs::create_dir_all(ws.vol_dir.join("raw"))?;
    std::fs::create_dir_all(ws.vol_dir.join("translated"))?;
    let meta: ProjectMeta = data_block::read_data_block(&ws.project_md());
    write_volume_md(&ws, label, meta.target_language)
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
        orch = models.orchestrator.model,
        trans = models.translator.model,
        rev = models.reviewer.model,
    )
}

fn render_project_body_for_language(
    title: &str,
    date: &str,
    models: &ModelSet,
    target_language: TargetLanguage,
) -> String {
    if target_language == TargetLanguage::Thai {
        return render_project_body(title, date, models);
    }
    format!(
        "# {title}\n\
         \n\
         - **Created:** {date}\n\
         - **Status:** importing\n\
         - **Translation language:** English\n\
         \n\
         ## Synopsis\n\
         \n\
         _No synopsis yet — edit this section at any time._\n\
         \n\
         ## Models\n\
         \n\
         | Role | Model |\n\
         |------|-------|\n\
         | Orchestrator | `{orch}` |\n\
         | Translator   | `{trans}` |\n\
         | Reviewer     | `{rev}` |\n\
         \n\
         ## Reference files\n\
         \n\
         - `CHARACTERS.md` — cast, names, forms of address, and voice\n\
         - `GLOSSARY.md` — terminology, places, organizations, and abilities\n\
         - `STYLE.md` — target prose and localization conventions\n",
        orch = models.orchestrator.model,
        trans = models.translator.model,
        rev = models.reviewer.model,
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

fn render_style_body_for_language(date: &str, target_language: TargetLanguage) -> String {
    if target_language == TargetLanguage::Thai {
        return render_style_body(date);
    }
    format!(
        "# Translation Style Guide\n\
         \n\
         - **Created:** {date}\n\
         - **Status:** draft\n\
         - **Target language:** English\n\
         \n\
         ## Overall Tone\n\
         \n\
         _Define the book's narrative voice, dialogue register, and degree of localization here._\n\
         \n\
         ## Rendering Principles\n\
         \n\
         1. Preserve the source's emotion, characterization, POV, subtext, and information.\n\
         2. Write fluent, publication-ready English for native light-novel readers; avoid literal Japanese syntax and stock translationese.\n\
         3. Preserve Japanese culture without gratuitous Westernization or intrusive translator notes.\n\
         4. Keep Markdown, scene breaks, emphasis, dialogue rhythm, and image links intact.\n\
         5. Follow `GLOSSARY.md` and `CHARACTERS.md` for terminology, names, address forms, and voice.\n\
         \n\
         ## Series-specific Conventions\n\
         \n\
         _Record romanization, honorific, naming-order, SFX, and dialogue conventions for this series._\n"
    )
}

fn write_volume_md(
    ws: &Workspace,
    label: Option<&str>,
    target_language: TargetLanguage,
) -> std::io::Result<()> {
    // Load existing data so re-running never destroys content.
    let mut data = volume::load(ws);

    // Seed the recap only for a brand-new volume with a known label.
    if data.running_recap.trim().is_empty()
        && let Some(lbl) = label.filter(|l| !l.trim().is_empty())
    {
        data.running_recap = match target_language {
            TargetLanguage::Thai => format!("เล่ม: {}", lbl.trim()),
            TargetLanguage::English => format!("Volume: {}", lbl.trim()),
        };
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
    fn set_title_rewrites_heading_and_thai_bullet() {
        let root = temp_root("title");
        let _ = std::fs::remove_dir_all(&root);
        let ws = Workspace::new(root.clone(), 1);
        create_project(&root, "陰の実力者", &ModelSet::default(), 1).unwrap();

        set_title(&ws, "陰の実力者になりたくて", "อยากเป็นผู้อยู่เบื้องหลังตัวจริง").unwrap();

        let text = std::fs::read_to_string(ws.project_md()).unwrap();
        assert!(text.contains("# 陰の実力者になりたくて"));
        assert!(text.contains("ชื่อไทย / Thai title:** อยากเป็นผู้อยู่เบื้องหลังตัวจริง"));
        assert!(text.contains("สถานะ / Status:**"));
        let meta: ProjectMeta = data_block::read_data_block(&ws.project_md());
        assert_eq!(meta.title, "陰の実力者になりたくて");
        assert_eq!(meta.translated_title, "อยากเป็นผู้อยู่เบื้องหลังตัวจริง");
        assert_eq!(read_translated_title(&root), "อยากเป็นผู้อยู่เบื้องหลังตัวจริง");

        set_title(&ws, "新タイトル", "").unwrap();
        let text = std::fs::read_to_string(ws.project_md()).unwrap();
        assert!(text.contains("# 新タイトル"));
        assert!(!text.contains("ชื่อไทย / Thai title:"));
        assert_eq!(read_translated_title(&root), "");
    }

    #[test]
    fn project_meta_accepts_legacy_title_key_and_writes_neutral_key() {
        let mut value = serde_json::to_value(ProjectMeta::default()).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("translated_title");
        object.insert("title_th".into(), serde_json::json!("ชื่อเก่า"));

        let meta: ProjectMeta = serde_json::from_value(value).unwrap();
        assert_eq!(meta.translated_title, "ชื่อเก่า");
        let rewritten = serde_json::to_value(meta).unwrap();
        assert_eq!(rewritten["translated_title"], "ชื่อเก่า");
        assert!(rewritten.get("title_th").is_none());
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

    #[test]
    fn english_project_gets_english_style_title_and_status() {
        let root = temp_root("english");
        let _ = std::fs::remove_dir_all(&root);
        let ws = Workspace::new(root.clone(), 1);
        create_project_for_language(
            &root,
            "夜の影",
            &ModelSet::default(),
            1,
            TargetLanguage::English,
        )
        .unwrap();

        let style = std::fs::read_to_string(ws.style_md()).unwrap();
        assert!(style.contains("publication-ready English"));
        assert!(style.contains("avoid literal Japanese syntax"));
        assert!(!style.contains("เรียบเรียงให้เป็นภาษาไทย"));

        set_title(&ws, "夜の影", "Shadow of the Night").unwrap();
        sync_status(&ws, ProjectStatus::InProgress).unwrap();
        let project = std::fs::read_to_string(ws.project_md()).unwrap();
        assert!(project.contains("Translated title:** Shadow of the Night"));
        assert!(project.contains("Status:** in progress"));
        let meta: ProjectMeta = data_block::read_data_block(&ws.project_md());
        assert_eq!(meta.target_language, TargetLanguage::English);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn adding_a_volume_keeps_the_existing_projects_language() {
        let root = temp_root("existing_language");
        let _ = std::fs::remove_dir_all(&root);
        create_project_for_language(
            &root,
            "夜の影",
            &ModelSet::default(),
            1,
            TargetLanguage::English,
        )
        .unwrap();

        create_project_for_language(
            &root,
            "夜の影",
            &ModelSet::default(),
            2,
            TargetLanguage::Thai,
        )
        .unwrap();

        assert_eq!(read_target_language(&root), TargetLanguage::English);
        let style = std::fs::read_to_string(root.join("STYLE.md")).unwrap();
        assert!(style.contains("publication-ready English"));

        let _ = std::fs::remove_dir_all(&root);
    }
}
