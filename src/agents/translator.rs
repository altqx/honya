//! Run the Translator agent (Thai, json_schema `translation_result`) for one chunk.

use crate::agents::continuity::build_translator_user_msg;
use crate::agents::prompts::TRANSLATOR_SYSTEM;
use crate::llm::client::{LlmClient, LlmError};
use crate::llm::structured::{chat_structured_stream_field, translator_schema};
use crate::llm::{ChatRequest, Message, Usage};
use crate::model::TranslatorOut;

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
}

pub async fn translate_chunk_streaming<F>(
    client: &dyn LlmClient,
    model: &str,
    reference_ctx: &str,
    prev_thai: &[String],
    raw_chunk: &str,
    feedback: Option<&str>,
    on_delta: F,
) -> std::result::Result<(TranslatorOut, Usage, bool), TranslatorStreamError>
where
    F: for<'a> FnMut(&'a str) + Send,
{
    let req = translator_request(model, reference_ctx, prev_thai, raw_chunk, feedback);
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
    model: &str,
    reference_ctx: &str,
    prev_thai: &[String],
    raw_chunk: &str,
    feedback: Option<&str>,
) -> ChatRequest {
    let mut user = String::new();

    let rctx = reference_ctx.trim();
    if !rctx.is_empty() {
        user.push_str(rctx);
        user.push_str("\n\n");
    }

    if let Some(fb) = feedback {
        let fb = fb.trim();
        if !fb.is_empty() {
            user.push_str("<<REVIEWER_FEEDBACK: แก้ไขตามรายการต่อไปนี้แล้วแปลใหม่>>\n");
            user.push_str(fb);
            user.push_str("\n<<END_REVIEWER_FEEDBACK>>\n\n");
        }
    }

    user.push_str(&build_translator_user_msg(prev_thai, raw_chunk));

    ChatRequest {
        model: model.to_string(),
        messages: vec![Message::system(TRANSLATOR_SYSTEM), Message::user(user)],
        temperature: Some(0.3),
        ..ChatRequest::default()
    }
}
