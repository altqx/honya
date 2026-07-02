//! Run the Translator agent (Thai, json_schema `translation_result`) for one chunk.

use crate::agents::continuity::build_translator_user_msg;
use crate::agents::prompts::TRANSLATOR_SYSTEM;
use crate::llm::client::{LlmClient, LlmError};
use crate::llm::structured::{chat_structured_stream_fields, translator_schema};
use crate::llm::{ChatRequest, Message, Usage};
use crate::model::{AgentModel, ThoughtProcessField, TranslatorOut};

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
    on_thought_delta: impl for<'a> FnMut(ThoughtProcessField, &'a str) + Send,
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
    let mut on_thought_delta = on_thought_delta;
    let mut streamed_translated_text = false;
    let mut relay_delta = |field: &'static str, delta: &str| match field {
        "scene_analysis" => on_thought_delta(ThoughtProcessField::SceneAnalysis, delta),
        "glossary_check" => on_thought_delta(ThoughtProcessField::GlossaryCheck, delta),
        "translated_text" => {
            streamed_translated_text = true;
            partial_translated_text.push_str(delta);
            on_delta(delta);
        }
        _ => {}
    };

    // The pipeline owns Translator retries so it can react to partial streamed
    // `translated_text` instead of silently replaying the same prompt here.
    let res = chat_structured_stream_fields::<TranslatorOut, _>(
        client,
        req,
        "translation_result",
        translator_schema(),
        0,
        &["scene_analysis", "glossary_check", "translated_text"],
        &mut relay_delta,
    )
    .await;

    match res {
        Ok((out, usage, _)) => Ok((out, usage, streamed_translated_text)),
        Err(source) => Err(TranslatorStreamError {
            source,
            partial_translated_text,
        }),
    }
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
    let retry_feedback = feedback.map(str::trim).filter(|fb| !fb.is_empty());

    let rctx = reference_ctx.trim();
    if !rctx.is_empty() {
        user.push_str(rctx);
        user.push_str("\n\n");
    }

    user.push_str(&build_translator_user_msg(
        prev_thai,
        current_pov,
        raw_chunk,
        retry_feedback,
        attempt,
    ));

    let mut messages = vec![Message::system(TRANSLATOR_SYSTEM), Message::user(user)];
    if retry_feedback.is_some() {
        messages.push(Message::user(format_retry_feedback_reminder(attempt)));
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

fn format_retry_feedback_reminder(attempt: u32) -> String {
    format!(
        "<<RETRY_CONFIRMATION: RETRY {attempt}>>\n\
         ก่อนตอบ JSON ให้ตรวจ translated_text กับ REVIEWER_FEEDBACK ในข้อความก่อนหน้าอีกครั้ง: \
         ทุกข้อที่ถูกตีกลับต้องถูกแก้จริง ห้ามปล่อยความผิดเดิมค้าง และห้ามเขียนคำอธิบายการแก้\n\
         <<END_RETRY_CONFIRMATION>>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AgentModel;

    #[test]
    fn retry_feedback_is_in_primary_payload_and_reminder_is_last() {
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
        let payload = req.messages[1].content.as_deref().expect("user payload");
        assert!(payload.contains("<<SOURCE_JP>>"));
        assert!(payload.contains("<<REVIEWER_FEEDBACK: RETRY 3"));
        assert!(payload.contains("POV pronoun wrong"));
        assert!(
            payload
                .find("<<REVIEWER_FEEDBACK")
                .expect("feedback marker")
                < payload.find("<<SOURCE_JP>>").expect("source marker")
        );

        let reminder = req.messages[2].content.as_deref().expect("retry reminder");
        assert!(reminder.contains("RETRY_CONFIRMATION"));
        assert!(reminder.contains("RETRY 3"));
        assert!(!reminder.contains("POV pronoun wrong"));
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
