//! src/agents/reviewer.rs — run the Reviewer agent (English, json_schema
//! `review_result`) for one chunk.
//!
//! Compares the raw Japanese source against the Translator's Thai output and
//! returns a binary verdict plus an itemized feedback list. Uses
//! `chat_structured::<ReviewerOut>` with the strict reviewer schema at
//! temperature 0.0 with 2 parse retries.

use crate::agents::prompts::{build_reviewer_user, REVIEWER_SYSTEM};
use crate::llm::client::{LlmClient, Result};
use crate::llm::structured::{chat_structured, reviewer_schema};
use crate::llm::{ChatRequest, Message, Usage};
use crate::model::ReviewerOut;

/// Review one translated chunk against its Japanese source.
///
/// `reference_ctx` carries the same locked glossary / character-pronoun / style
/// context the Translator saw, so the Reviewer can actually enforce glossary and
/// pronoun matching (checklist items 3 & 4). Returns the verdict plus token `Usage`.
pub async fn review_chunk(
    client: &dyn LlmClient,
    model: &str,
    source_jp: &str,
    thai: &str,
    reference_ctx: &str,
) -> Result<(ReviewerOut, Usage)> {
    let user = build_reviewer_user(source_jp, thai, reference_ctx);

    let req = ChatRequest {
        model: model.to_string(),
        messages: vec![Message::system(REVIEWER_SYSTEM), Message::user(user)],
        temperature: Some(0.0),
        ..ChatRequest::default()
    };

    chat_structured::<ReviewerOut>(client, req, "review_result", reviewer_schema(), 2).await
}
