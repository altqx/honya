//! Bundled sample project: an original mini light novel with two translated
//! chapters and one pending chapter for trying the pipeline.

use std::path::Path;

use chrono::Utc;

use crate::model::{Character, GlossaryTerm, ModelSet, ReaderAnnotation, Relationship, TermPolicy};
use crate::workspace::{Workspace, characters, glossary, scaffold, translation, volume};

/// Bundled sample project slug.
pub const SAMPLE_SLUG: &str = "honya-sample";

const SAMPLE_TITLE: &str = "星詠みの図書館 ・ honya sample";

/// True if the bundled sample has already been created.
pub fn sample_exists(root: &Path) -> bool {
    root.join(SAMPLE_SLUG).join("PROJECT.md").is_file()
}

/// Create the bundled sample project without clobbering an existing copy.
pub fn create_sample_project(root: &Path, models: &ModelSet) -> std::io::Result<String> {
    let dir = root.join(SAMPLE_SLUG);
    if dir.join("PROJECT.md").is_file() {
        return Ok(SAMPLE_SLUG.to_string());
    }

    scaffold::create_project(&dir, SAMPLE_TITLE, models, 1)?;
    let ws = Workspace::new(dir.clone(), 1);

    write_glossary(&ws)?;
    write_characters(&ws)?;
    write_volume_meta(&ws)?;
    write_chapters(&ws)?;
    write_reader_marks(&ws)?;

    Ok(SAMPLE_SLUG.to_string())
}

/// Seed terms that exercise hard/preferred/forbidden policies.
fn write_glossary(ws: &Workspace) -> std::io::Result<()> {
    let terms = [
        GlossaryTerm {
            jp_term: "星詠み".into(),
            thai_term: "ผู้ขับขานดารา".into(),
            romaji: Some("hoshiyomi".into()),
            category: Some("ตำแหน่ง/พลัง".into()),
            gloss: Some("ผู้ถอดอ่านชะตากรรมจากการโคจรของดวงดาว".into()),
            policy: Some(TermPolicy::HardLocked),
            forbidden_thai: Vec::new(),
            context_rule: None,
            protected: Some(true),
            do_not_translate: None,
            first_seen_chapter: Some(1),
        },
        GlossaryTerm {
            jp_term: "魔導書".into(),
            thai_term: "ตำราเวทมนตร์".into(),
            romaji: Some("madōsho".into()),
            category: Some("ไอเทม".into()),
            gloss: Some("หนังสือบรรจุเวทมนตร์โบราณ".into()),
            policy: Some(TermPolicy::Preferred),
            forbidden_thai: Vec::new(),
            context_rule: None,
            protected: None,
            do_not_translate: None,
            first_seen_chapter: Some(1),
        },
        GlossaryTerm {
            jp_term: "魔法".into(),
            thai_term: "เวทมนตร์".into(),
            romaji: Some("mahō".into()),
            category: Some("ทั่วไป".into()),
            gloss: Some("ใช้ “เวทมนตร์” เสมอ ห้ามแปลเป็น “มายากล”".into()),
            policy: Some(TermPolicy::Forbidden),
            forbidden_thai: vec!["มายากล".into()],
            context_rule: None,
            protected: None,
            do_not_translate: None,
            first_seen_chapter: Some(2),
        },
    ];
    for t in terms {
        glossary::upsert(ws, t)?;
    }
    Ok(())
}

/// Seed two characters, one of which carries a relationship + speech-style notes so
/// the Characters panel is non-trivial.
fn write_characters(ws: &Workspace) -> std::io::Result<()> {
    characters::upsert(
        ws,
        Character {
            id: "rei".into(),
            jp_name: "レイ".into(),
            thai_name: "เรย์".into(),
            romaji: Some("Rei".into()),
            gender: Some("ชาย".into()),
            honorific: None,
            speech_style: Some("สุภาพ พูดน้อย".into()),
            relationships: Vec::new(),
            aliases: Vec::new(),
            also_called: Vec::new(),
            notes: Some("ตัวเอก ผู้มาเยือนหอสมุดยามค่ำคืน".into()),
            first_seen_chapter: Some(1),
        },
    )?;
    characters::upsert(
        ws,
        Character {
            id: "sena".into(),
            jp_name: "セナ".into(),
            thai_name: "เซนะ".into(),
            romaji: Some("Sena".into()),
            gender: Some("หญิง".into()),
            honorific: None,
            speech_style: Some("อ่อนโยน ลึกลับ".into()),
            relationships: vec![Relationship {
                target_id: "rei".into(),
                relation: "ผู้ชี้นำ".into(),
            }],
            aliases: Vec::new(),
            also_called: Vec::new(),
            notes: Some("ผู้ขับขานดาราผมสีเงิน ผู้ดูแลหอสมุด".into()),
            first_seen_chapter: Some(1),
        },
    )?;
    Ok(())
}

/// Write the volume synopsis (JA + TH), a running recap (whose `เล่ม:` line gives the
/// volume its label), and one-line summaries for the two finished chapters.
fn write_volume_meta(ws: &Workspace) -> std::io::Result<()> {
    volume::set_synopsis(
        ws,
        "星を読み、運命を綴る図書館。少年レイは、銀髪の星詠みセナと出会い、\
         自らの星が指し示す物語を辿りはじめる。",
        "หอสมุดที่อ่านดวงดาวและร้อยเรียงชะตากรรม เด็กหนุ่มเรย์ได้พบกับเซนะ \
         ผู้ขับขานดาราผมสีเงิน และเริ่มต้นติดตามเรื่องราวที่ดวงดาวของเขาชี้นำ",
    )?;
    volume::set_recap(
        ws,
        "เล่ม: 星の章\nเรย์มาถึงหอสมุดยามราตรีและได้พบเซนะ ผู้ขับขานดารา จากนั้นได้รับตำราเวทมนตร์ลึกลับ",
    )?;
    volume::set_chapter_summary(ws, 1, "เรย์มาถึงหอสมุดยามราตรีและได้พบเซนะ ผู้ขับขานดารา")?;
    volume::set_chapter_summary(ws, 2, "เซนะอธิบายพลังการอ่านดวงดาว เรย์เปิดตำราเวทมนตร์เป็นครั้งแรก")?;
    Ok(())
}

/// Write two translated chapters and one pending chapter.
fn write_chapters(ws: &Workspace) -> std::io::Result<()> {
    translation::write_raw(ws, 1, CH1_JA)?;
    write_translated(ws, 1, CH1_TH)?;

    translation::write_raw(ws, 2, CH2_JA)?;
    write_translated(ws, 2, CH2_TH)?;

    translation::write_raw(ws, 3, CH3_JA)?;
    Ok(())
}

/// Write a finished translated chapter in the pipeline's chunk format.
fn write_translated(ws: &Workspace, chapter: u32, thai: &str) -> std::io::Result<()> {
    let body = format!("<!-- honya:chunk 0 -->\n{}\n", thai.trim_end_matches('\n'));
    let path = ws.translated(chapter);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, body)
}

/// Seed Reader marks for first-open discovery.
fn write_reader_marks(ws: &Workspace) -> std::io::Result<()> {
    volume::toggle_reader_bookmark(ws, 1, 2, "เปิดเรื่อง — หอสมุดยามราตรี")?;
    volume::add_reader_annotation(
        ws,
        ReaderAnnotation {
            chapter: 1,
            line: 8,
            note: "ตรวจน้ำเสียงของเซนะให้ลึกลับสม่ำเสมอ".into(),
            created_at: Some(Utc::now()),
        },
    )?;
    Ok(())
}

// Original sample story text.

const CH1_JA: &str = "\
# 第一章 星の図書館

夜が深くなるほど、その図書館は本当の姿を現す。

レイは古い扉を押し開けた。埃の匂いと、かすかな星の光が彼を迎えた。

「ようこそ、星詠みの図書館へ」

声の主は、銀色の髪をした少女——セナだった。彼女は微笑みながら、一冊の魔導書を差し出した。
";

const CH1_TH: &str = "\
# บทที่หนึ่ง หอสมุดแห่งดารา

ยิ่งราตรีดิ่งลึกลงเท่าใด หอสมุดแห่งนั้นก็ยิ่งเผยโฉมที่แท้จริงออกมา

เรย์ผลักประตูบานเก่าให้เปิดออก กลิ่นฝุ่นและแสงดาวรางเลือนต้อนรับเขา

“ยินดีต้อนรับ สู่หอสมุดของผู้ขับขานดารา”

เจ้าของเสียงคือเด็กสาวผมสีเงิน—เซนะ เธอยิ้มพลางยื่นตำราเวทมนตร์เล่มหนึ่งให้
";

const CH2_JA: &str = "\
# 第二章 夜想曲

セナは星詠みだった。星の運行から、人々の運命を読み解く者。

「あなたの星は、まだ物語の途中なのよ」とセナは言った。

レイは魔導書のページをめくった。そこには、見たこともない文字が並んでいた。
";

const CH2_TH: &str = "\
# บทที่สอง บทเพลงรัตติกาล

เซนะคือผู้ขับขานดารา ผู้ถอดอ่านชะตากรรมของผู้คนจากการโคจรของดวงดาว

“ดวงดาวของเธอน่ะ ยังอยู่กลางเรื่องราวอยู่เลยล่ะ” เซนะเอ่ย

เรย์พลิกหน้าของตำราเวทมนตร์ บนนั้นเรียงรายด้วยอักขระที่เขาไม่เคยเห็นมาก่อน
";

const CH3_JA: &str = "\
# 第三章 来訪者

朝が来ても、セナは図書館から出ようとしなかった。

「外の世界は、もう私を覚えていないの」

その時、扉を叩く音が響いた。誰かが——あるいは何かが——訪れたのだ。
";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ChapterKind, ChapterStatus};

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!("honya_sample_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    /// The generated sample scans as a usable offline demo.
    #[test]
    fn sample_project_scans_as_a_usable_demo() {
        let root = temp_root("scan");
        let slug = create_sample_project(&root, &ModelSet::default()).unwrap();
        assert_eq!(slug, SAMPLE_SLUG);
        assert!(sample_exists(&root));

        let dir = root.join(SAMPLE_SLUG);
        let project = crate::workspace::scan::scan_one_project(&dir).expect("scans as a project");
        let vol = &project.volumes[0];
        assert_eq!(vol.chapters.len(), 3, "three chapters");
        assert_eq!(vol.chapters[0].status, ChapterStatus::Done);
        assert_eq!(vol.chapters[1].status, ChapterStatus::Done);
        assert_eq!(vol.chapters[2].status, ChapterStatus::Pending);
        assert!(vol.chapters.iter().all(|c| c.kind == ChapterKind::Prose));
        assert!(vol.chapters[0].title.contains("星の図書館"));
        assert_eq!(vol.label.as_deref(), Some("星の章"));

        let ws = Workspace::new(dir.clone(), 1);
        assert_eq!(glossary::load(&ws).len(), 3, "three glossary terms");
        assert_eq!(characters::load(&ws).len(), 2, "two characters");
        let data = volume::load(&ws);
        assert!(!data.synopsis_th.trim().is_empty(), "Thai synopsis present");
        assert!(
            !data.synopsis_raw.trim().is_empty(),
            "source synopsis present"
        );
        assert_eq!(data.bookmarks.len(), 1, "one seeded bookmark");
        assert_eq!(data.annotations.len(), 1, "one seeded proofreading note");
        assert!(data.chapters.contains_key("1"), "ch1 summary present");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Re-creating the sample is a no-op.
    #[test]
    fn sample_creation_is_idempotent() {
        let root = temp_root("idem");
        let models = ModelSet::default();
        let first = create_sample_project(&root, &models).unwrap();

        let ws = Workspace::new(root.join(SAMPLE_SLUG), 1);
        glossary::upsert(
            &ws,
            GlossaryTerm {
                jp_term: "扉".into(),
                thai_term: "ประตู".into(),
                romaji: None,
                category: None,
                gloss: None,
                policy: None,
                forbidden_thai: Vec::new(),
                context_rule: None,
                protected: None,
                do_not_translate: None,
                first_seen_chapter: None,
            },
        )
        .unwrap();

        let second = create_sample_project(&root, &models).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            glossary::load(&ws).len(),
            4,
            "re-create preserved the user-added term"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
