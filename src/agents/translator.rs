//! src/agents/translator.rs — run the Translator agent (Thai, json_schema
//! `translation_result`) for one chunk.
//!
//! Builds the user message via `continuity::build_translator_user_msg`,
//! prepends the locked-glossary context and any Reviewer feedback (on retries),
//! then calls `chat_structured::<TranslatorOut>` with the strict translator
//! schema at temperature 0.3 with 2 parse retries.

use crate::agents::continuity::build_translator_user_msg;
use crate::agents::prompts::TRANSLATOR_SYSTEM;
use crate::llm::client::{LlmClient, Result};
use crate::llm::structured::{chat_structured, translator_schema};
use crate::llm::{ChatRequest, Message, Usage};
use crate::model::TranslatorOut;

/// Translate one source chunk into Thai.
///
/// * `reference_ctx` — the pre-delimited reference block (locked glossary,
///                     character roster/pronouns, project synopsis, style guide)
///                     the pipeline assembles from the workspace.
/// * `prev_thai`     — the previous chunk's tail sentences (continuity, do not
///                     re-translate).
/// * `raw_chunk`     — the Japanese markdown to translate.
/// * `feedback`      — `Some` on a retry, carrying the Reviewer's correction list.
///
/// Returns the structured output together with the API token `Usage` so the
/// pipeline can surface a live token meter.
pub async fn translate_chunk(
    client: &dyn LlmClient,
    model: &str,
    reference_ctx: &str,
    prev_thai: &[String],
    raw_chunk: &str,
    feedback: Option<&str>,
) -> Result<(TranslatorOut, Usage)> {
    // Assemble the user content: reference context, optional reviewer feedback,
    // then the continuity + source block.
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
