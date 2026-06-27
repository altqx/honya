//! Last-N Thai sentence extraction + Translator user-message assembly.
//!
//! `last_thai_sentences` returns the previous chunk's tail sentences so the next
//! chunk's prompt keeps tone/pronouns continuous without re-translating them.

use once_cell::sync::Lazy;
use regex::Regex;

use crate::workspace::Workspace;
use crate::workspace::translation::read_translated;

/// Matches a `<!-- honya:chunk N -->` marker (any whitespace, any integer).
static CHUNK_MARKER: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"<!--\s*honya:chunk\s+\d+\s*-->").expect("chunk-marker regex is valid")
});

/// Sentence terminators for continuity. Thai `ฯ`/`ๆ` are word-level, so excluded.
static TERMINATOR: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[.!?。！？…]+[”’」』）】\)\]]*").expect("terminator regex is valid"));

/// Bounds Thai tails when missing terminators make one "sentence" very long.
const MAX_CONTINUITY_CHARS: usize = 1200;

/// Return the last `n` non-empty Thai sentences of `chapter` (in order) to seed
/// continuity. Bounded twice: at most `n` sentences AND [`MAX_CONTINUITY_CHARS`] total.
pub async fn last_thai_sentences(ws: &Workspace, chapter: u32, n: usize) -> Vec<String> {
    if n == 0 {
        return Vec::new();
    }
    let raw = read_translated(ws, chapter).await;
    if raw.trim().is_empty() {
        return Vec::new();
    }

    // Strip chunk markers so they never bleed into the prompt.
    let cleaned = CHUNK_MARKER.replace_all(&raw, " ");

    let sentences = split_sentences(&cleaned);
    let len = sentences.len();
    let start = len.saturating_sub(n);
    let mut tail = sentences[start..].to_vec();

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
    if !trimmed.is_empty() {
        out.push(trimmed.to_string());
    }
    current.clear();
}

/// Assemble the Translator user message: optional continuity block, a scoped
/// task reminder, then the source delimited by `<<SOURCE_JP>> … <<END_SOURCE_JP>>`.
pub fn build_translator_user_msg(
    prev_thai: &[String],
    current_pov: Option<&str>,
    raw_chunk: &str,
) -> String {
    let mut s = String::new();

    if let Some(pov) = current_pov.map(str::trim).filter(|p| !p.is_empty()) {
        s.push_str(
            "<<CURRENT_POV: ผู้เล่า (มุมมองบุรุษที่ 1) ที่ไหลมาจากชังก์ก่อนหน้า ใช้เป็นจุดตั้งต้น แต่ถ้าในชังก์นี้มีตัวแบ่งฉากที่สลับผู้เล่า ให้ยึดตามเนื้อความ>>\n",
        );
        s.push_str(pov);
        s.push_str("\n<<END_CURRENT_POV>>\n\n");
    }

    if !prev_thai.is_empty() {
        s.push_str(&format!(
            "<<CONTINUITY: ประโยคแปลล่าสุด {} ประโยคก่อนหน้า (ห้ามแปลซ้ำ ใช้เพื่อความต่อเนื่องเท่านั้น)>>\n",
            prev_thai.len()
        ));
        for line in prev_thai {
            s.push_str(line.trim());
            s.push('\n');
        }
        s.push_str("<<END_CONTINUITY>>\n\n");
    }

    s.push_str(
        "<<TASK: แปลเฉพาะข้อความใน SOURCE_JP เป็นภาษาไทยเท่านั้น>>\n\
         - ใช้ CONTINUITY/REFERENCE/REVIEWER_FEEDBACK เป็นบริบท ห้ามคัดลอกลง translated_text\n\
         - translated_text ต้องเป็น Markdown ภาษาไทยฉบับสุดท้าย ไม่มีหัวข้อ \"คำแปล:\" ไม่มีคำเกริ่น และไม่มีคำอธิบายงาน\n\
         - อย่าใส่วงเล็บคำอ่าน/คำเดิมหลังคำไทยสำหรับชื่อหรือคำธรรมดา เช่น \"สุดาตะ (さかた)\", \"ชมรม (同好会)\", \"รับทราบ (โอส)!\", \"รักแรกพบ (ฮิโตเมะโบเระ)\" หรือ \"เพอร์เฟกต์ (Perfect)\"; อ้างรูปเดิมเฉพาะเมื่อเป็นข้อมูลพล็อตที่จำเป็นจริง ๆ\n\
         - เลี่ยง \"วะ\" และ \"ว่ะ\" เป็นคำลงท้าย/คำอุทานทั่วไป ให้ใช้ \"ฟะ\" แทน; \"เว้ย\" ใช้ได้แบบหายากเมื่อเป็นคำอุทานแรง ๆ ที่จำเป็น เช่น \"โธ่เว้ย\" ถ้าเป็นเสียงโวยวายทั่วไปให้ใช้ \"เฟ้ย\"\n\
         - ใช้ชื่อ/คำเรียกจาก REFERENCE ให้ตรงตัว โดยเฉพาะรายการ \"เรียกอีกชื่อ\" เช่น JP surface ใดมีรูปไทยกำกับ ต้องใช้รูปนั้น ห้ามสลับกับชื่อหลักหรือสะกดเองใหม่; ถ้าเป็นชื่อคันจิ + さん เช่น `亜玖璃さん` ให้รักษาเป็น \"คุณ...\" เว้นแต่มีรายการ exact surface กำหนดต่างออกไป\n\
         - ถ้า SOURCE_JP มี `俺` ในข้อความเล่าเรื่องหรือบทพูด ให้ถือเป็นสัญญาณเสียงชาย/กันเองก่อนเสมอ ห้ามใช้ \"ฉัน\" เว้นแต่ REFERENCE/CHARACTERS หรือบริบทระบุชัดว่าตัวละครชายคนนั้นใช้ \"ฉัน\"; ถ้า REFERENCE/CHARACTERS ระบุว่าไม่ใช้ \"ฉัน\" ให้ยึดกฎนั้นเหนือ CONTINUITY/feedback เก่า; อย่าลากสรรพนามจาก CONTINUITY ข้าม POV\n\
         - ถ้าบทพูดญี่ปุ่นไม่มีสรรพนามตัวเอง (私/自分/俺 ฯลฯ) อย่าเติม \"ฉันคิดว่า\" หรือสรรพนามไทยโดยไม่จำเป็น ให้ละประธานได้เมื่อผู้พูดชัดอยู่แล้ว\n\
         - ระวังวลีขยายคำนามแบบญี่ปุ่น: ถ้ามีกริยาต่อเนื่องก่อนคำนาม เช่น `...睨みつけてくる女二人に、俺は...` กริยาเหล่านั้นเป็นของคำนาม `女二人` ไม่ใช่ของ `俺は` ที่ตามมา\n\
         - ห้ามใช้ \"ฟะ/เฟ้ย\" กับบทพูดสุภาพ です/ます/ません หรือผู้พูดหญิงสุภาพ เว้นแต่ SOURCE_JP/REFERENCE ระบุเสียงหยาบชัดเจน; ประโยคพร้อมกันแบบ `訊けたら苦労しません！` ควรคงความสุภาพ/ตะโกน ไม่ทำให้เป็นเสียงชายห้วน\n\
         - ก่อนส่งให้เทียบ SOURCE_JP กับ translated_text ครบทุกบรรทัด: ห้ามตกประโยคหลังตัวแบ่งฉาก หัวเรื่อง เครดิต บทพูดในวงเล็บ หรือประโยคท้ายชังก์; ต้นฉบับที่จบด้วย `。` ไม่ต้องเติมจุด `.` ในภาษาไทยโดยอัตโนมัติ แต่ประโยคไทยต้องอ่านจบสมบูรณ์ และให้รักษาน้ำเสียงคำถาม/อุทาน/ทอดเสียงจาก `？` `！` `…` ด้วยสำนวนหรือเครื่องหมายที่เหมาะสม\n\
         - ถ้าเจอ 互いに/お互い/互いへの/向け合う/向き合う กับ 絆/関係/思い/気持/感情 ให้แปลเป็นความสัมพันธ์/ความรู้สึกที่มีต่อกันหรือผูกพันกัน ไม่ใช่ \"มอบให้\" แบบการให้ฝ่ายเดียว และอย่าให้ `互いへの感情` กลายเป็นความรู้สึกกว้าง ๆ ของใครก็ได้\n\
         - แปลสำนวนบรรยากาศให้เป็นธรรมชาติ เช่น `場の空気が冷えた` คือบรรยากาศกร่อย/เย็นชา/เงียบลง ไม่ใช่ \"เย็นลงอย่างสมบูรณ์\" แบบอุณหภูมิ\n\
         - ห้ามทิ้งเครื่องหมายญี่ปุ่น `。` `、` `「」` `『』` `（）` ไว้ใน translated_text เว้นแต่เป็นข้อความญี่ปุ่นที่เรื่องต้องการให้ผู้อ่านเห็นจริง ๆ\n\
         - รักษาย่อหน้าและจังหวะของต้นฉบับเท่าที่ทำได้ โดยเรียบเรียงให้เป็นภาษาไทยธรรมชาติ\n\
         - ระบุผู้เล่า (POV) ของแต่ละช่วง เลือกสรรพนามบุรุษที่ 1 ให้ตรงผู้เล่า และสลับเมื่อข้ามตัวแบ่งฉากที่เปลี่ยนมุมมอง แล้วบันทึกผู้เล่าท้ายชังก์ลงฟิลด์ pov\n\
         <<END_TASK>>\n\n",
    );

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
        let msg = build_translator_user_msg(&[], None, "雨野君は笑った。");

        assert!(msg.contains("เรียกอีกชื่อ"));
        assert!(msg.contains("ครบทุกบรรทัด"));
        assert!(msg.contains("เครื่องหมายญี่ปุ่น"));
        assert!(msg.contains("睨みつけてくる女二人"));
        assert!(msg.contains("訊けたら苦労しません"));
        assert!(msg.contains("亜玖璃さん"));
        assert!(msg.contains("ฉันคิดว่า"));
        assert!(msg.contains("互いへの"));
        assert!(msg.contains("場の空気が冷えた"));
        assert!(msg.contains("<<SOURCE_JP>>"));
    }
}
