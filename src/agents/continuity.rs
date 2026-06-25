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
