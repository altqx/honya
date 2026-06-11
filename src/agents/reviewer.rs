//! Run the Reviewer agent (English, json_schema `review_result`) for one chunk:
//! diff Japanese source against Thai output for a verdict plus itemized feedback.

use crate::agents::prompts::{REVIEWER_SYSTEM, build_reviewer_user};
use crate::llm::client::{LlmClient, Result};
use crate::llm::structured::{chat_structured, reviewer_schema};
use crate::llm::{ChatRequest, Message, Usage};
use crate::model::ReviewerOut;

/// Review one translated chunk against its source. `reference_ctx` matches the
/// Translator context so glossary/pronoun/style checks are enforceable.
pub async fn review_chunk(
    client: &dyn LlmClient,
    model: &str,
    source_jp: &str,
    thai: &str,
    reference_ctx: &str,
    audit_findings: &[String],
    prev_thai: &[String],
) -> Result<(ReviewerOut, Usage)> {
    let user = build_reviewer_user(source_jp, thai, reference_ctx, audit_findings, prev_thai);

    let req = ChatRequest {
        model: model.to_string(),
        messages: vec![Message::system(REVIEWER_SYSTEM), Message::user(user)],
        temperature: Some(0.0),
        ..ChatRequest::default()
    };

    // retries=0: the pipeline owns the Reviewer retry surface (like translator.rs);
    // re-sending an identical temp=0 request would just re-hit the same result.
    chat_structured::<ReviewerOut>(client, req, "review_result", reviewer_schema(), 0).await
}
