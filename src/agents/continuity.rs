//! Last-N Thai sentence extraction + Translator user-message assembly.
//!
//! `last_translated_sentences` returns the previous chunk's tail sentences so the next
//! chunk's prompt keeps tone/pronouns continuous without re-translating them.

use once_cell::sync::Lazy;
use regex::Regex;

use crate::model::TargetLanguage;
use crate::workspace::Workspace;
use crate::workspace::translation::read_translated;

/// Matches machine-only honya markers embedded in translated Markdown.
static HONYA_MARKER: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"<!--\s*honya:[^>]*-->").expect("honya-marker regex is valid"));

/// Sentence terminators for continuity. Thai `ฯ`/`ๆ` are word-level, so excluded.
static TERMINATOR: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[.!?。！？…]+[”’」』）】\)\]]*").expect("terminator regex is valid"));

/// Bounds Thai tails when missing terminators make one "sentence" very long.
const MAX_CONTINUITY_CHARS: usize = 2000;

/// Return the last `n` non-empty translated sentences ending at `chapter`, walking
/// backward through earlier chapters as needed. Bounded twice: at most `n`
/// sentences AND [`MAX_CONTINUITY_CHARS`] total.
pub async fn last_translated_sentences(ws: &Workspace, chapter: u32, n: usize) -> Vec<String> {
    if n == 0 || chapter == 0 {
        return Vec::new();
    }

    let mut tail = Vec::with_capacity(n);
    for candidate in (1..=chapter).rev() {
        let raw = read_translated(ws, candidate).await;
        if raw.trim().is_empty() {
            continue;
        }

        let cleaned = HONYA_MARKER.replace_all(&raw, " ");
        let sentences = split_sentences(&cleaned);
        let missing = n - tail.len();
        let start = sentences.len().saturating_sub(missing);
        let mut older = sentences[start..].to_vec();
        older.append(&mut tail);
        tail = older;
        if tail.len() == n {
            break;
        }
    }

    // Drop old sentences first; clamp one over-long survivor to its newest chars.
    while tail.len() > 1 && joined_chars(&tail) > MAX_CONTINUITY_CHARS {
        tail.remove(0);
    }
    if let Some(last) = tail.last_mut() {
        let count = last.chars().count();
        if count > MAX_CONTINUITY_CHARS {
            *last = last.chars().skip(count - MAX_CONTINUITY_CHARS).collect();
        }
    }
    tail
}

/// Char count of the tail, counting one separator per sentence (the joining newline).
fn joined_chars(tail: &[String]) -> usize {
    tail.iter().map(|s| s.chars().count() + 1).sum()
}

/// Split text into trimmed non-empty sentences on terminal punctuation and line breaks.
fn split_sentences(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();

    for line in text.split('\n') {
        let line = line.trim();
        if line.is_empty() {
            // A blank line is a soft boundary — flush whatever is pending.
            push_trimmed(&mut current, &mut out);
            continue;
        }

        let mut last = 0usize;
        for m in TERMINATOR.find_iter(line) {
            current.push_str(&line[last..m.end()]);
            push_trimmed(&mut current, &mut out);
            last = m.end();
        }
        current.push_str(&line[last..]);
        // Each source line ends a logical unit for our coarse splitter.
        current.push(' ');
        push_trimmed(&mut current, &mut out);
    }

    push_trimmed(&mut current, &mut out);
    out
}

fn push_trimmed(current: &mut String, out: &mut Vec<String>) {
    let trimmed = current.trim();
    if !trimmed.is_empty() && trimmed != "---" {
        out.push(trimmed.to_string());
    }
    current.clear();
}

/// Assemble the Translator user message: optional continuity block, a scoped
/// task reminder, then the source delimited by `<<SOURCE_JP>> … <<END_SOURCE_JP>>`.
#[cfg(test)]
pub fn build_translator_user_msg(
    previous_translation: &[String],
    current_pov: Option<&str>,
    raw_chunk: &str,
    retry_feedback: Option<&str>,
    attempt: u32,
) -> String {
    build_translator_user_msg_for_language(
        TargetLanguage::Thai,
        previous_translation,
        current_pov,
        raw_chunk,
        retry_feedback,
        attempt,
    )
}

pub fn build_translator_user_msg_for_language(
    target: TargetLanguage,
    previous: &[String],
    current_pov: Option<&str>,
    raw_chunk: &str,
    retry_feedback: Option<&str>,
    attempt: u32,
) -> String {
    match target {
        TargetLanguage::Thai => build_translator_user_msg_thai(
            previous,
            current_pov,
            raw_chunk,
            retry_feedback,
            attempt,
        ),
        TargetLanguage::English => build_translator_user_msg_english(
            previous,
            current_pov,
            raw_chunk,
            retry_feedback,
            attempt,
        ),
    }
}

fn build_translator_user_msg_thai(
    previous_translation: &[String],
    current_pov: Option<&str>,
    raw_chunk: &str,
    retry_feedback: Option<&str>,
    attempt: u32,
) -> String {
    let mut s = String::new();

    if let Some(pov) = current_pov.map(str::trim).filter(|p| !p.is_empty()) {
        s.push_str(
            "<<CURRENT_POV: ผู้เล่า (มุมมองบุรุษที่ 1) ที่ไหลมาจากชังก์ก่อนหน้า ใช้เป็นจุดตั้งต้น แต่ถ้าในชังก์นี้มีตัวแบ่งฉากที่สลับผู้เล่า ให้ยึดตามเนื้อความ>>\n",
        );
        s.push_str(pov);
        s.push_str("\n<<END_CURRENT_POV>>\n\n");
    }

    if !previous_translation.is_empty() {
        s.push_str(&format!(
            "<<CONTINUITY: ประโยคแปลล่าสุด {} ประโยคก่อนหน้า (ห้ามแปลซ้ำ ใช้เพื่อความต่อเนื่องเท่านั้น)>>\n",
            previous_translation.len()
        ));
        for line in previous_translation {
            s.push_str(line.trim());
            s.push('\n');
        }
        s.push_str("<<END_CONTINUITY>>\n\n");
    }

    s.push_str(
        "<<TASK: แปลเฉพาะ SOURCE_JP เป็น Markdown ไทยฉบับสุดท้ายตาม system prompt>>\n\
         - CONTINUITY / REFERENCE / REVIEWER_FEEDBACK เป็นบริบท — ห้ามคัดลอกลง translated_text\n\
         - ถ้ามี REVIEWER_FEEDBACK ให้ถือเป็นเงื่อนไขผ่าน/ตกของรอบนี้: แก้ทุกข้อที่ถูกตีกลับจริง ห้ามย้อนรูปที่เพิ่งถูกบอกว่าผิด\n\
         - ก่อนตอบ เทียบ SOURCE_JP กับ translated_text ครบทุกบรรทัด; บันทึกผู้เล่าท้ายชังก์ใน `pov`\n\
         <<END_TASK>>\n\n",
    );

    if let Some(feedback) = retry_feedback.map(str::trim).filter(|fb| !fb.is_empty()) {
        s.push_str(&format!(
            "<<REVIEWER_FEEDBACK: RETRY {attempt} — ต้องแก้ให้ครบก่อนตอบ JSON>>\n\
             ข้อความนี้เป็นเงื่อนไขบังคับของ translated_text รอบนี้ ไม่ใช่คำแนะนำเสริม\n\
             - ทำ checklist จากทุกบรรทัดของ feedback แล้วแก้ในคำแปลฉบับเต็ม\n\
             - อ่าน SOURCE_JP ที่เกี่ยวข้องใหม่ ห้ามแก้จากความจำหรือแค่แทนคำแบบเดาสุ่ม\n\
             - Feedback ล่าสุดมีน้ำหนักเหนือ CONTINUITY/คำแปลรอบก่อนในจุดที่ถูกตีกลับ แต่ห้ามละเมิด REFERENCE/CHARACTERS/GLOSSARY\n\
             - ถ้า feedback เดิมอยู่ในประวัติ ให้ตรวจซ้ำว่าไม่มีความผิดเดิมหลงเหลือ\n\n\
             {feedback}\n\
             <<END_REVIEWER_FEEDBACK>>\n\n"
        ));
    }

    s.push_str("<<SOURCE_JP>>\n");
    s.push_str(raw_chunk);
    if !raw_chunk.ends_with('\n') {
        s.push('\n');
    }
    s.push_str("<<END_SOURCE_JP>>");
    s
}

fn build_translator_user_msg_english(
    previous: &[String],
    current_pov: Option<&str>,
    raw_chunk: &str,
    retry_feedback: Option<&str>,
    attempt: u32,
) -> String {
    let mut s = String::new();
    if let Some(pov) = current_pov.map(str::trim).filter(|p| !p.is_empty()) {
        s.push_str("<<CURRENT_POV: narrator carried from the previous chunk; use as the opening anchor, but follow any clear POV switch in this chunk>>\n");
        s.push_str(pov);
        s.push_str("\n<<END_CURRENT_POV>>\n\n");
    }
    if !previous.is_empty() {
        s.push_str(&format!(
            "<<CONTINUITY_EN: previous {} translated sentence(s), for voice and flow only; DO NOT repeat them>>\n",
            previous.len()
        ));
        for line in previous {
            s.push_str(line.trim());
            s.push('\n');
        }
        s.push_str("<<END_CONTINUITY_EN>>\n\n");
    }
    s.push_str(
        "<<TASK: translate only SOURCE_JP into final English Markdown per the system prompt>>\n\
         - Use REFERENCE, CONTINUITY, CURRENT_POV, and REVIEWER_FEEDBACK as constraints, never as text to copy.\n\
         - Before responding, compare SOURCE_JP and translated_text line by line; save the narrator at chunk end in `pov`.\n\
         <<END_TASK>>\n\n",
    );
    if let Some(feedback) = retry_feedback.map(str::trim).filter(|fb| !fb.is_empty()) {
        s.push_str(&format!(
            "<<REVIEWER_FEEDBACK: RETRY {attempt} — every item is mandatory>>\n\
             Re-read the relevant SOURCE_JP, revise the complete translation, fix every rejected point, and do not reintroduce an earlier error. REFERENCE remains authoritative.\n\n\
             {feedback}\n\
             <<END_REVIEWER_FEEDBACK>>\n\n"
        ));
    }
    s.push_str("<<SOURCE_JP>>\n");
    s.push_str(raw_chunk);
    if !raw_chunk.ends_with('\n') {
        s.push('\n');
    }
    s.push_str("<<END_SOURCE_JP>>");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translator_user_msg_carries_quality_reminders() {
        let msg = build_translator_user_msg(&[], None, "雨野君は笑った。", None, 1);

        assert!(msg.contains("<<TASK:"));
        assert!(msg.contains("เงื่อนไขผ่าน/ตก"));
        assert!(msg.contains("ครบทุกบรรทัด"));
        assert!(msg.contains("`pov`"));
        assert!(msg.contains("<<SOURCE_JP>>"));
        assert!(!msg.contains("睨みつけてくる女二人"));
    }

    #[test]
    fn translator_user_msg_embeds_retry_feedback_before_source() {
        let msg =
            build_translator_user_msg(&[], None, "亜玖璃さんは笑った。", Some("use คุณอากุริ"), 4);

        assert!(msg.contains("RETRY 4"));
        assert!(msg.contains("use คุณอากุริ"));
        assert!(
            msg.find("<<REVIEWER_FEEDBACK").expect("feedback marker")
                < msg.find("<<SOURCE_JP>>").expect("source marker")
        );
        assert!(msg.contains("เงื่อนไขผ่าน/ตก"));
    }

    #[test]
    fn english_user_msg_requests_polished_english_without_thai_rules() {
        let msg = build_translator_user_msg_for_language(
            TargetLanguage::English,
            &["She glanced away.".to_string()],
            Some("Keita / first-person"),
            "俺は笑った。",
            Some("Make the dialogue less literal."),
            2,
        );

        assert!(msg.contains("<<CONTINUITY_EN"));
        assert!(msg.contains("per the system prompt"));
        assert!(msg.contains("Make the dialogue less literal."));
        assert!(!msg.contains("ห้ามใช้ \"กู\""));
        assert!(!msg.contains("Do not Westernize"));
    }

    #[tokio::test]
    async fn sparse_continuity_backfills_previous_chapters_and_ignores_dividers() {
        let base =
            std::env::temp_dir().join(format!("honya_continuity_backfill_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let ws = Workspace::new(base.clone(), 1);

        for (chapter, text) in [
            (1, "บรรทัดเก่าสุด"),
            (2, "บรรทัดก่อนหน้า"),
            (3, "---\n\nบรรทัดล่าสุด"),
        ] {
            let path = ws.translated(chapter);
            std::fs::create_dir_all(path.parent().expect("translated parent")).unwrap();
            std::fs::write(
                path,
                format!("<!-- honya:chunks-total 1 -->\n\n<!-- honya:chunk 0 -->\n{text}\n"),
            )
            .unwrap();
        }

        let tail = last_translated_sentences(&ws, 3, 3).await;

        assert_eq!(tail, ["บรรทัดเก่าสุด", "บรรทัดก่อนหน้า", "บรรทัดล่าสุด"]);
        let _ = std::fs::remove_dir_all(base);
    }
}
