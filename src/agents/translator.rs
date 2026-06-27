//! Run the Translator agent (Thai, json_schema `translation_result`) for one chunk.

use crate::agents::continuity::build_translator_user_msg;
use crate::agents::prompts::TRANSLATOR_SYSTEM;
use crate::llm::client::{LlmClient, LlmError};
use crate::llm::structured::{chat_structured_stream_field, translator_schema};
use crate::llm::{ChatRequest, Message, Usage};
use crate::model::{AgentModel, TranslatorOut};

#[derive(Debug, thiserror::Error)]
#[error("{source}")]
pub struct TranslatorStreamError {
    #[source]
    source: LlmError,
    partial_translated_text: String,
}

impl TranslatorStreamError {
    pub fn partial_translated_text(&self) -> &str {
        &self.partial_translated_text
    }

    /// True when the underlying failure is a provider content-policy block (e.g.
    /// Gemini `PROHIBITED_CONTENT`), which a verbatim retry can't clear.
    pub fn is_content_policy_block(&self) -> bool {
        self.source.is_content_policy_block()
    }

    /// True for `finish_reason=length`, which needs a tighten-output retry.
    pub fn is_length_truncation(&self) -> bool {
        self.source.is_length_truncation()
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn translate_chunk_streaming<F>(
    client: &dyn LlmClient,
    model: &AgentModel,
    reference_ctx: &str,
    prev_thai: &[String],
    current_pov: Option<&str>,
    raw_chunk: &str,
    feedback: Option<&str>,
    attempt: u32,
    on_delta: F,
) -> std::result::Result<(TranslatorOut, Usage, bool), TranslatorStreamError>
where
    F: for<'a> FnMut(&'a str) + Send,
{
    let req = translator_request(
        model,
        reference_ctx,
        prev_thai,
        current_pov,
        raw_chunk,
        feedback,
        attempt,
    );
    let mut partial_translated_text = String::new();
    let mut on_delta = on_delta;
    let mut relay_delta = |delta: &str| {
        partial_translated_text.push_str(delta);
        on_delta(delta);
    };

    // The pipeline owns Translator retries so it can react to partial streamed
    // `translated_text` instead of silently replaying the same prompt here.
    chat_structured_stream_field::<TranslatorOut, _>(
        client,
        req,
        "translation_result",
        translator_schema(),
        0,
        "translated_text",
        &mut relay_delta,
    )
    .await
    .map_err(|source| TranslatorStreamError {
        source,
        partial_translated_text,
    })
}

fn translator_request(
    model: &AgentModel,
    reference_ctx: &str,
    prev_thai: &[String],
    current_pov: Option<&str>,
    raw_chunk: &str,
    feedback: Option<&str>,
    attempt: u32,
) -> ChatRequest {
    let mut user = String::new();

    let rctx = reference_ctx.trim();
    if !rctx.is_empty() {
        user.push_str(rctx);
        user.push_str("\n\n");
    }

    user.push_str(&build_translator_user_msg(
        prev_thai,
        current_pov,
        raw_chunk,
    ));

    let mut messages = vec![Message::system(TRANSLATOR_SYSTEM), Message::user(user)];
    if let Some(fb) = feedback.map(str::trim).filter(|fb| !fb.is_empty()) {
        messages.push(Message::user(format_retry_feedback(fb, attempt)));
    }

    ChatRequest {
        model: model.model.clone(),
        messages,
        temperature: Some(translator_temperature(feedback)),
        reasoning: model.reasoning_param(),
        ..ChatRequest::default()
    }
}

fn translator_temperature(feedback: Option<&str>) -> f32 {
    if feedback.is_some_and(|fb| !fb.trim().is_empty()) {
        0.15
    } else {
        0.3
    }
}

fn format_retry_feedback(feedback: &str, attempt: u32) -> String {
    format!(
        "<<REVIEWER_FEEDBACK: RETRY {attempt} — แก้ทุกข้อก่อนตอบ JSON>>\n\
         งานรอบนี้คือแก้คำแปลเดิมอย่างตรงจุด ไม่ใช่แปลใหม่แบบสุ่มจนเกิดข้อผิดพลาดใหม่\n\
         - อ่าน SOURCE_JP ด้านบนใหม่ แล้วแก้ทุกข้อใน feedback นี้ใน translated_text ฉบับเต็ม\n\
         - ถ้า feedback พูดถึงชื่อ สรรพนาม POV ผู้พูด หรือความหมายเฉพาะ ให้ตรวจทุกตำแหน่งที่เกี่ยวข้องใน SOURCE_JP ก่อนส่ง\n\
         - รักษาส่วนที่ผ่านแล้วไว้ อย่าเปลี่ยนน้ำเสียง ย่อหน้า Markdown ชื่อเฉพาะ หรือคำศัพท์ที่ไม่ได้ถูกตีกลับ\n\
         - ห้ามเขียนคำอธิบายการแก้ ให้ตอบเป็น JSON schema เท่านั้น\n\n\
         {feedback}\n\
         <<END_REVIEWER_FEEDBACK>>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AgentModel;

    #[test]
    fn retry_feedback_is_last_user_message() {
        let req = translator_request(
            &AgentModel::openrouter("test/model"),
            "",
            &[],
            None,
            "俺は笑った。",
            Some("POV pronoun wrong"),
            3,
        );

        assert_eq!(req.messages.len(), 3);
        assert!(
            req.messages[1]
                .content
                .as_deref()
                .is_some_and(|s| s.contains("<<SOURCE_JP>>"))
        );
        assert!(
            req.messages[2]
                .content
                .as_deref()
                .is_some_and(|s| s.contains("RETRY 3"))
        );
        assert!(
            req.messages[2]
                .content
                .as_deref()
                .is_some_and(|s| s.contains("POV pronoun wrong"))
        );
        assert_eq!(req.temperature, Some(0.15));
    }

    #[test]
    fn first_attempt_has_no_retry_feedback_message() {
        let req = translator_request(
            &AgentModel::openrouter("test/model"),
            "",
            &[],
            None,
            "彼女は笑った。",
            None,
            1,
        );

        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.temperature, Some(0.3));
    }
}
