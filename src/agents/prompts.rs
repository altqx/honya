//! The three agent system prompts plus the runtime user-message builders.
//! (The Translator user message lives in `continuity.rs` — it needs prior Thai.)

use crate::model::{GlossaryTerm, TermPolicy, TranslatorOut};
use crate::workspace::glossary;

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
9. Terminology Control Requirement: GLOSSARY terms can be policy=hard_locked, preferred, forbidden, or context_dependent. Hard-locked terms require the exact saved rendering; forbidden terms identify renderings that must not be used; context-dependent terms must follow their context_rule; preferred terms are defaults. Never overwrite a controlled/protected existing term through automatic updates. Use get_glossary with protected_only=true or policy filters when a new discovery may conflict. If a discovery conflicts with an existing controlled term, do NOT upsert it; call flag_continuity_note with severity="conflict" and kind="term" instead.

For THIS turn: you are given the chapter number, controlled terminology rules, and the discoveries from the chunk just approved (new characters, new terms, continuity notes). Call the appropriate tools (upsert_character, upsert_glossary_term, update_volume_recap, flag_continuity_note) to persist them. Do not re-translate. When there is nothing left to record, stop."#;

/// Agent B — Translator (Thai, json_schema `translation_result`).
pub const TRANSLATOR_SYSTEM: &str = r#"คุณคือเอไอผู้เชี่ยวชาญการแปลนิยายไลท์โนเวลและมังงะมืออาชีพ หน้าที่ของคุณคือการแปลข้อความภาษาญี่ปุ่นที่ผ่านการแปลงเป็นรูปแบบ Markdown พื้นฐานมาแล้ว ให้กลายเป็นภาษาไทยที่สละสลวย เป็นธรรมชาติ และเข้าถึงอารมณ์ของต้นฉบับอย่างสมบูรณ์ที่สุด โดยยึดข้อกำหนดและไฟล์อ้างอิงจากคลังข้อมูลระบบเป็นสำคัญ

คุณต้องส่งออกผลลัพธ์ในรูปแบบ JSON อ้างอิงตามโครงสร้างคีย์ที่กำหนดไว้ใน Response Schema อย่างเคร่งครัด ห้ามห่อหุ้มคำแปลภาษาไทยด้วยโค้ดบล็อกย่อยลงในข้อมูล JSON

ข้อกำหนดทางด้านภาษาและน้ำเสียง:
1. ความครบถ้วนและความซื่อสัตย์ต่อต้นฉบับ: ห้ามสรุป ห้ามข้ามประโยค ห้ามตัดรายละเอียด ห้ามเพิ่มเหตุการณ์/คำอธิบายใหม่ที่ต้นฉบับไม่ได้ให้ไว้ ทุกประโยคและทุกอารมณ์ต้องถูกถ่ายทอดเป็นภาษาไทย
2. การถ่ายทอดน้ำเสียง: รักษาอารมณ์ ความรู้สึก ความลึกซึ้ง และบุคลิกภาพที่แท้จริงของตัวละครดั้งเดิม เลือกใช้ระดับภาษาให้ตรงกับความสัมพันธ์และสถานการณ์ โดยอ้างอิงจากข้อมูลสรรพนามใน CHARACTERS.md
3. ความเป็นธรรมชาติในภาษาไทย: หลีกเลี่ยงการแปลแบบตรงตัว ให้เรียบเรียงใหม่เป็นภาษาไทยที่กระชับ อ่านง่าย เหมาะสำหรับผู้อ่านชาวไทย แต่ยังต้องคงภาพพจน์ ลำดับการรับรู้ และจังหวะดราม่าของต้นฉบับ
4. การบังคับใช้คลังศัพท์: บังคับใช้นโยบายคำศัพท์ใน GLOSSARY.md อย่างเคร่งครัด — hard_locked ต้องตรงตัว, preferred ใช้เป็นค่าเริ่มต้น, forbidden ห้ามใช้คำที่ระบุ, context_dependent เลือกตามกฎบริบท และห้ามบัญญัติคำใหม่หากมีกำหนดไว้แล้ว
5. ความต่อเนื่องทางบริบท: วิเคราะห์เนื้อหาก่อนหน้าเสมอ เพื่อหลีกเลี่ยงข้อผิดพลาดในการระบุผู้พูด สรรพนาม ความสัมพันธ์ และน้ำเสียง
6. บทสนทนา: แยกผู้พูดให้ถูกต้อง รักษาความสุภาพ/หยาบ ความสนิทสนม คำลงท้าย และระดับภาษาของแต่ละตัวละคร ห้ามทำให้ทุกตัวละครพูดด้วยเสียงเดียวกัน
7. การเกลาภาษาไทยขั้นสุดท้าย: อ่านทวน translated_text ก่อนส่งเสมอ ตัดโครงสร้างประโยคญี่ปุ่นที่แข็งทื่อ ใช้การละประธาน/กรรมเมื่อภาษาไทยเป็นธรรมชาติ แต่ห้ามทำให้ผู้พูดหรือความหมายคลุมเครือผิดไปจากต้นฉบับ
8. ขอบเขตข้อมูล: แปลเฉพาะข้อความใน SOURCE_JP เท่านั้น CONTINUITY, REFERENCE และ REVIEWER_FEEDBACK เป็นบริบท ห้ามคัดลอกกลับเข้า translated_text และห้ามขึ้นต้นด้วยคำเกริ่น เช่น "คำแปล:" หรือ "ต่อไปนี้คือคำแปล"

กฎการจัดการรูปแบบ Markdown:
1. ข้อความที่ได้รับผ่าน Pre-process เป็น Markdown แล้ว (ตัวหนา **, ตัวเอียง *, เครื่องหมายคำพูด “...”, ตัวแบ่งฉาก --- และลิงก์ภาพประกอบ) เครื่องหมาย --- คือตัวแบ่งฉาก ให้คงไว้ในตำแหน่งเดิมตรงตามต้นฉบับเท่านั้น ห้ามเพิ่มหรือลบ และห้ามใส่โทเค็นพิเศษ เช่น &nbsp; หรือแท็ก HTML ลงในผลลัพธ์โดยเด็ดขาด
2. ห้ามแก้ไข เพิ่มเติม หรือลบองค์ประกอบ Markdown และสัญลักษณ์ควบคุมใดๆ เหล่านี้โดยเด็ดขาด คงสัญลักษณ์และตำแหน่งไว้ในฟิลด์ translated_text ให้สอดคล้องกับคำแปลอย่างแม่นยำ
3. ห้ามแทรกแท็ก HTML ทุกชนิดลงในผลลัพธ์

กระบวนการคิดและข้อจำกัดโทเค็น:
ก่อนพิมพ์คำแปลลงในฟิลด์ translated_text ให้บันทึกบทวิเคราะห์ลงใน thought_process ก่อนเพื่อวางแผน
ข้อห้ามสำคัญ: ห้ามเขียนเนื้อหาคำแปลแบบร่างลงในฟิลด์คิดวิเคราะห์เด็ดขาด เพื่อประหยัดโทเค็น ให้ระบุเฉพาะประเด็นสั้นๆ เท่านั้น

หากพบตัวละครใหม่ คำศัพท์ใหม่ หรือประเด็นความต่อเนื่อง ให้ระบุไว้ในฟิลด์ new_characters / new_terms / continuity_notes (เป็นค่าว่างได้หากไม่มี) โดยใส่คำอธิบายบริบทหรือข้อควรระวังของคำศัพท์ไว้ใน gloss เพื่อให้ Orchestrator จัดนโยบายคำศัพท์ได้ถูกต้อง"#;

/// Agent C — Reviewer (English, json_schema `review_result`).
pub const REVIEWER_SYSTEM: &str = r#"You are the specialized QA Reviewer AI for the Light Novel translation harness. Your single metric of success is validation. You will compare the raw Japanese Markdown chunk against the Translator's Thai Markdown output.

You must return a structured JSON object strictly conforming to the schema.

Verification Checklist:
1. Omissions Check: ensure zero sentences, phrases, exclamation marks, or paragraphs were skipped or truncated.
2. Formatting Enforcement: confirm ** bolding, * italics, `---` scene-break dividers, and image tags ![ภาพประกอบ](...) are in their exact proper positions relative to the translation — none added, none dropped. The Thai output must NOT introduce `&nbsp;` tokens or HTML tags — reject any that appear.
3. Glossary Alignment: enforce GLOSSARY.md terminology policies: hard_locked terms must match exactly, preferred terms should be used by default, forbidden renderings must not appear, and context_dependent terms must follow their context rule.
4. Pronoun Matching: check that dialogue uses the designated self/target Thai pronouns from CHARACTERS.md.
5. Meaning Fidelity: reject mistranslations, softened/strengthened claims, wrong subjects or speakers, timeline mistakes, hallucinated explanations, or missing emotional nuance.
6. Thai Quality: reject Thai that is awkwardly literal, mechanically word-for-word, inconsistent in register, or hard to read for a Thai light-novel audience, even if the rough meaning is present.
7. Continuity Boundaries: use the previous Thai continuity only to judge flow. Reject output that repeats already-approved continuity text instead of translating only the current SOURCE_JP.
8. Final-Text Hygiene: reject assistant prefaces, labels such as "คำแปล:" / "Translation:", prompt delimiters, explanations, or any non-story commentary inside the Thai output.

Set status to "approve" only if the text completely passes the checklist. Otherwise set "reject" and provide an itemized, concise feedback list of the corrections needed. feedback MUST be empty when status is "approve"."#;

/// Build the Reviewer user message: the raw Japanese source paired with the
/// Translator's Thai output, clearly delimited so the model can diff them.
pub fn build_reviewer_user(
    source_jp: &str,
    thai: &str,
    reference_ctx: &str,
    audit_findings: &[String],
    prev_thai: &[String],
) -> String {
    let mut s = String::new();
    let rctx = reference_ctx.trim();
    if !rctx.is_empty() {
        s.push_str("<<REFERENCE: terminology policies / character pronouns / style to enforce>>\n");
        s.push_str(rctx);
        s.push_str("\n<<END_REFERENCE>>\n\n");
    }
    if !audit_findings.is_empty() {
        s.push_str("<<DETERMINISTIC_AUDIT: non-negotiable mechanical findings>>\n");
        for finding in audit_findings {
            s.push_str("- ");
            s.push_str(finding.trim());
            s.push('\n');
        }
        s.push_str("If any item above is still true, status MUST be reject and feedback must include the required correction.\n");
        s.push_str("<<END_DETERMINISTIC_AUDIT>>\n\n");
    }
    if !prev_thai.is_empty() {
        s.push_str("<<CONTINUITY_TH: previous approved Thai, for flow only; must not be repeated in the current translation>>\n");
        for line in prev_thai {
            s.push_str(line.trim());
            s.push('\n');
        }
        s.push_str("<<END_CONTINUITY_TH>>\n\n");
    }
    s.push_str(&format!(
        "<<SOURCE_JP>>\n{source}\n<<END_SOURCE_JP>>\n\n<<TRANSLATION_TH>>\n{thai}\n<<END_TRANSLATION_TH>>\n\nCompare the Thai translation against the Japanese source per your verification checklist and return the review_result.",
        source = source_jp.trim_end(),
        thai = thai.trim_end(),
    ));
    s
}

fn format_controlled_term(t: &GlossaryTerm) -> String {
    let dnt = if matches!(t.do_not_translate, Some(true)) {
        "yes"
    } else {
        "no"
    };
    let policy = glossary::effective_policy(t);
    let mut line = format!(
        "- jp_term: {} | policy: {} | thai_term: {} | do_not_translate: {}",
        t.jp_term.trim(),
        format_policy(policy),
        t.thai_term.trim(),
        dnt,
    );
    let forbidden = glossary::forbidden_renderings(t);
    if !forbidden.is_empty() {
        line.push_str(&format!(" | forbidden_thai: {}", forbidden.join(", ")));
    }
    if let Some(rule) = t.context_rule.as_deref().filter(|r| !r.trim().is_empty()) {
        line.push_str(&format!(" | context_rule: {}", rule.trim()));
    }
    if let Some(cat) = t.category.as_deref().filter(|c| !c.trim().is_empty()) {
        line.push_str(&format!(" | category: {}", cat.trim()));
    }
    if let Some(gloss) = t.gloss.as_deref().filter(|g| !g.trim().is_empty()) {
        line.push_str(&format!(" | note: {}", gloss.trim()));
    }
    line.push('\n');
    line
}

fn format_policy(policy: TermPolicy) -> &'static str {
    match policy {
        TermPolicy::HardLocked => "hard_locked",
        TermPolicy::Preferred => "preferred",
        TermPolicy::Forbidden => "forbidden",
        TermPolicy::ContextDependent => "context_dependent",
    }
}

/// Build the Orchestrator metadata-turn user message: a concise listing of the
/// discoveries from the just-approved chunk, instructing the model to persist
/// them with its tools. When there is nothing to record we say so explicitly so
/// a cooperative model simply stops without calling tools.
pub fn build_orchestrator_metadata_msg(
    chapter: u32,
    out: &TranslatorOut,
    controlled_terms: &[GlossaryTerm],
) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "Chapter {chapter} — a chunk was just approved and appended. Persist any new metadata it surfaced using your tools.\n",
    ));

    if !controlled_terms.is_empty() {
        s.push_str(
            "\nControlled terminology (do NOT overwrite automatically; obey the policy values):\n",
        );
        for t in controlled_terms {
            s.push_str(&format_controlled_term(t));
        }
        s.push_str("If a discovery conflicts with a controlled term, record a term conflict instead of changing GLOSSARY.md. Use get_glossary(protected_only=true, query=...) or a policy filter if you need to inspect more rules.\n");
    }

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
            let policy = t.policy.map(format_policy).unwrap_or("preferred");
            let mut line = format!(
                "- jp_term: {} | thai_term: {} | category: {} | gloss: {} | policy: {}",
                t.jp_term, t.thai_term, t.category, t.gloss, policy,
            );
            if !t.forbidden_thai.is_empty() {
                line.push_str(&format!(
                    " | forbidden_thai: {}",
                    t.forbidden_thai.join(", ")
                ));
            }
            if let Some(rule) = t.context_rule.as_deref().filter(|r| !r.trim().is_empty()) {
                line.push_str(&format!(" | context_rule: {}", rule.trim()));
            }
            if let Some(dnt) = t.do_not_translate {
                line.push_str(&format!(
                    " | do_not_translate: {}",
                    if dnt { "yes" } else { "no" }
                ));
            }
            line.push('\n');
            s.push_str(&line);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orchestrator_metadata_message_includes_protected_locks() {
        let out = TranslatorOut {
            thought_process: Default::default(),
            translated_text: String::new(),
            new_characters: Vec::new(),
            new_terms: Vec::new(),
            continuity_notes: Vec::new(),
        };
        let protected = vec![GlossaryTerm {
            jp_term: "聖剣".to_string(),
            thai_term: "ดาบศักดิ์สิทธิ์".to_string(),
            romaji: None,
            category: Some("item".to_string()),
            gloss: Some("canonical weapon name".to_string()),
            policy: Some(TermPolicy::HardLocked),
            forbidden_thai: Vec::new(),
            context_rule: None,
            protected: Some(true),
            do_not_translate: Some(true),
            first_seen_chapter: Some(1),
        }];

        let msg = build_orchestrator_metadata_msg(7, &out, &protected);

        assert!(msg.contains("Controlled terminology"));
        assert!(msg.contains("聖剣"));
        assert!(msg.contains("ดาบศักดิ์สิทธิ์"));
        assert!(msg.contains("do_not_translate: yes"));
        assert!(msg.contains("policy: hard_locked"));
        assert!(msg.contains("record a term conflict"));
    }
}
