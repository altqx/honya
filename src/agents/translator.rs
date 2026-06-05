//! Run the Translator agent (Thai, json_schema `translation_result`) for one chunk.

use crate::agents::continuity::build_translator_user_msg;
use crate::agents::prompts::TRANSLATOR_SYSTEM;
use crate::llm::client::{LlmClient, Result};
use crate::llm::structured::{chat_structured, translator_schema};
use crate::llm::{ChatRequest, Message, Usage};
use crate::model::TranslatorOut;

/// Translate one source chunk into Thai. `prev_thai` is continuity (do not re-translate); `feedback` is `Some` on a retry. Returns output + token `Usage`.
pub async fn translate_chunk(
    client: &dyn LlmClient,
    model: &str,
    reference_ctx: &str,
    prev_thai: &[String],
    raw_chunk: &str,
    feedback: Option<&str>,
) -> Result<(TranslatorOut, Usage)> {
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

    let req = ChatRequest {
        model: model.to_string(),
        messages: vec![Message::system(TRANSLATOR_SYSTEM), Message::user(user)],
        temperature: Some(0.3),
        ..ChatRequest::default()
    };

    chat_structured::<TranslatorOut>(client, req, "translation_result", translator_schema(), 2)
        .await
}
