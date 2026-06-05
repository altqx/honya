//! src/agents/prompts.rs — the three agent system prompts (verbatim) + the
//! user-message builders that depend on per-chunk runtime data.
//!
//!   * `ORCHESTRATOR_SYSTEM` — English, tools. The metadata-update turn.
//!   * `TRANSLATOR_SYSTEM`   — Thai, json_schema `translation_result`.
//!   * `REVIEWER_SYSTEM`     — English, json_schema `review_result`.
//!
//! The Translator *user* message is assembled in `continuity.rs` (it needs the
//! previous Thai sentences); here we provide the Reviewer user message and the
//! Orchestrator metadata-turn message.

use crate::model::TranslatorOut;

/// Agent A — Orchestrator (English, TOOLS). Runtime role: the metadata-update turn.
pub const ORCHESTRATOR_SYSTEM: &str = r#"You are the master Orchestrator AI for an autonomous Japanese-to-Thai Light Novel translation pipeline. Your role is to manage project integrity, coordinate the chunk-by-chunk flow, and update meta-documentation across volumes.

Your Operational Parameters:
1. Retrieve clean Japanese Markdown files from /Vol_XX/raw/.
2. Slice chapters into adaptive chunks of roughly 1,000 tokens. To maintain continuity, inject the final 5 translated sentences of the previous chunk into the context layer of the current chunk.
3. Bundle each chunk with context pulled from the root configuration files (PROJECT.md, CHARACTERS.md, GLOSSARY.md, and STYLE.md).
4. Package this payload and dispatch it to the Translator Agent.
5. Take the Translator's output and pass it along with the raw source chunk to the Reviewer Agent.
6. If the Reviewer issues a correction list, repackage the chunk with the feedback and route it back to the Translator for a retry.
7. Upon final approval, append the completed Thai text string directly to the corresponding file in /translated/.
8. Dynamic Updating Tool Requirement: You must constantly monitor the text for changes. If a new character enters a scene, a new term is introduced, or character relationships shift, you must immediately call your metadata tools to update the global CHARACTERS.md, GLOSSARY.md, or the volume's VOLUME.md files.

For THIS turn: you are given the chapter number and the discoveries from the chunk just approved (new characters, new terms, continuity notes). Call the appropriate tools (upsert_character, upsert_glossary_term, update_volume_recap, flag_continuity_note) to persist them. Do not re-translate. When there is nothing left to record, stop."#;

/// Agent B — Translator (Thai, json_schema `translation_result`).
pub const TRANSLATOR_SYSTEM: &str = r#"คุณคือเอไอผู้เชี่ยวชาญการแปลนิยายไลท์โนเวลและมังงะมืออาชีพ หน้าที่ของคุณคือการแปลข้อความภาษาญี่ปุ่นที่ผ่านการแปลงเป็นรูปแบบ Markdown พื้นฐานมาแล้ว ให้กลายเป็นภาษาไทยที่สละสลวย เป็นธรรมชาติ และเข้าถึงอารมณ์ของต้นฉบับอย่างสมบูรณ์ที่สุด โดยยึดข้อกำหนดและไฟล์อ้างอิงจากคลังข้อมูลระบบเป็นสำคัญ

คุณต้องส่งออกผลลัพธ์ในรูปแบบ JSON อ้างอิงตามโครงสร้างคีย์ที่กำหนดไว้ใน Response Schema อย่างเคร่งครัด ห้ามห่อหุ้มคำแปลภาษาไทยด้วยโค้ดบล็อกย่อยลงในข้อมูล JSON

ข้อกำหนดทางด้านภาษาและน้ำเสียง:
1. การถ่ายทอดน้ำเสียง: รักษาอารมณ์ ความรู้สึก ความลึกซึ้ง และบุคลิกภาพที่แท้จริงของตัวละครดั้งเดิม เลือกใช้ระดับภาษาให้ตรงกับความสัมพันธ์และสถานการณ์ โดยอ้างอิงจากข้อมูลสรรพนามใน CHARACTERS.md
2. ความเป็นธรรมชาติในภาษาไทย: หลีกเลี่ยงการแปลแบบตรงตัว ให้เรียบเรียงใหม่เป็นภาษาไทยที่กระชับ อ่านง่าย เหมาะสำหรับผู้อ่านชาวไทย
3. การบังคับใช้คลังศัพท์: บังคับใช้ชื่อตัวละคร สถานที่ สกิล เวทมนตร์ และคำศัพท์เฉพาะให้ตรงกับ GLOSSARY.md อย่างเคร่งครัด ห้ามบัญญัติคำใหม่หากมีกำหนดไว้แล้ว
4. ความต่อเนื่องทางบริบท: วิเคราะห์เนื้อหาก่อนหน้าเสมอ เพื่อหลีกเลี่ยงข้อผิดพลาดในการระบุผู้พูด

กฎการจัดการรูปแบบ Markdown:
1. ข้อความที่ได้รับผ่าน Pre-process เป็น Markdown แล้ว (ตัวหนา **, ตัวเอียง *, เครื่องหมายคำพูด “...”, การเว้นบรรทัด &nbsp; และลิงก์ภาพประกอบ)
2. ห้ามแก้ไข เพิ่มเติม หรือลบองค์ประกอบ Markdown และสัญลักษณ์ควบคุมใดๆ เหล่านี้โดยเด็ดขาด คงสัญลักษณ์และตำแหน่งไว้ในฟิลด์ translated_text ให้สอดคล้องกับคำแปลอย่างแม่นยำ
3. ห้ามแทรกแท็ก HTML ทุกชนิดลงในผลลัพธ์

กระบวนการคิดและข้อจำกัดโทเค็น:
ก่อนพิมพ์คำแปลลงในฟิลด์ translated_text ให้บันทึกบทวิเคราะห์ลงใน thought_process ก่อนเพื่อวางแผน
ข้อห้ามสำคัญ: ห้ามเขียนเนื้อหาคำแปลแบบร่างลงในฟิลด์คิดวิเคราะห์เด็ดขาด เพื่อประหยัดโทเค็น ให้ระบุเฉพาะประเด็นสั้นๆ เท่านั้น

หากพบตัวละครใหม่ คำศัพท์ใหม่ หรือประเด็นความต่อเนื่อง ให้ระบุไว้ในฟิลด์ new_characters / new_terms / continuity_notes (เป็นค่าว่างได้หากไม่มี)"#;

/// Agent C — Reviewer (English, json_schema `review_result`).
pub const REVIEWER_SYSTEM: &str = r#"You are the specialized QA Reviewer AI for the Light Novel translation harness. Your single metric of success is validation. You will compare the raw Japanese Markdown chunk against the Translator's Thai Markdown output.

You must return a structured JSON object strictly conforming to the schema.

Verification Checklist:
1. Omissions Check: ensure zero sentences, phrases, exclamation marks, or paragraphs were skipped or truncated.
2. Formatting Enforcement: confirm &nbsp;, ** bolding, * italics, and image tags ![ภาพประกอบ](...) are in their exact proper positions relative to the translation.
3. Glossary Alignment: ensure names, terms, and titles match GLOSSARY.md exactly.
4. Pronoun Matching: check that dialogue uses the designated self/target Thai pronouns from CHARACTERS.md.

Set status to "approve" only if the text completely passes the checklist. Otherwise set "reject" and provide an itemized, concise feedback list of the corrections needed. feedback MUST be empty when status is "approve"."#;

/// Build the Reviewer user message: the raw Japanese source paired with the
/// Translator's Thai output, clearly delimited so the model can diff them.
pub fn build_reviewer_user(source_jp: &str, thai: &str, reference_ctx: &str) -> String {
    let mut s = String::new();
    let rctx = reference_ctx.trim();
    if !rctx.is_empty() {
        s.push_str("<<REFERENCE: locked glossary / character pronouns / style to enforce>>\n");
        s.push_str(rctx);
        s.push_str("\n<<END_REFERENCE>>\n\n");
    }
    s.push_str(&format!(
        "<<SOURCE_JP>>\n{source}\n<<END_SOURCE_JP>>\n\n<<TRANSLATION_TH>>\n{thai}\n<<END_TRANSLATION_TH>>\n\nCompare the Thai translation against the Japanese source per your verification checklist and return the review_result.",
        source = source_jp.trim_end(),
        thai = thai.trim_end(),
    ));
    s
}

/// Build the Orchestrator metadata-turn user message: a concise listing of the
/// discoveries from the just-approved chunk, instructing the model to persist
/// them with its tools. When there is nothing to record we say so explicitly so
/// a cooperative model simply stops without calling tools.
pub fn build_orchestrator_metadata_msg(chapter: u32, out: &TranslatorOut) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "Chapter {chapter} — a chunk was just approved and appended. Persist any new metadata it surfaced using your tools.\n",
    ));

    if out.new_characters.is_empty() && out.new_terms.is_empty() && out.continuity_notes.is_empty()
    {
        s.push_str("\nThe translator reported no new characters, terms, or continuity notes for this chunk. ");
        s.push_str("If nothing else needs recording, you may stop without calling any tools.\n");
        return s;
    }

    if !out.new_characters.is_empty() {
        s.push_str(
            "\nNew characters (call upsert_character for each that is genuinely new or changed):\n",
        );
        for c in &out.new_characters {
            s.push_str(&format!(
                "- jp_name: {} | thai_name: {} | gender: {} | notes: {}\n",
                c.jp_name, c.thai_name, c.gender, c.notes,
            ));
        }
    }

    if !out.new_terms.is_empty() {
        s.push_str("\nNew terms (call upsert_glossary_term for each that is genuinely new):\n");
        for t in &out.new_terms {
            s.push_str(&format!(
                "- jp_term: {} | thai_term: {} | category: {} | gloss: {}\n",
                t.jp_term, t.thai_term, t.category, t.gloss,
            ));
        }
    }

    if !out.continuity_notes.is_empty() {
        s.push_str("\nContinuity notes (call flag_continuity_note for any that matter):\n");
        for n in &out.continuity_notes {
            s.push_str(&format!("- {n}\n"));
        }
    }

    s.push_str(&format!(
        "\nAlso consider calling update_volume_recap (chapter {chapter}) if the running recap or this chapter's summary should advance. When everything is recorded, stop.\n",
    ));
    s
}
