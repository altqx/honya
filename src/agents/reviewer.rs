//! Run the Reviewer agent (English, json_schema `review_result`) for one chunk:
//! diff Japanese source against Thai output for a verdict plus itemized feedback.

use crate::agents::prompts::{REVIEWER_SYSTEM, build_reviewer_user};
use crate::llm::client::{LlmClient, Result};
use crate::llm::structured::{chat_structured, reviewer_schema};
use crate::llm::{ChatRequest, Message, Usage};
use crate::model::ReviewerOut;

/// Review one translated chunk against its source. `reference_ctx` matches the
/// Translator context so glossary/pronoun/style checks are enforceable.
#[allow(clippy::too_many_arguments)]
pub async fn review_chunk(
    client: &dyn LlmClient,
    model: &str,
    source_jp: &str,
    thai: &str,
    reference_ctx: &str,
    audit_findings: &[String],
    advisory_findings: &[String],
    prev_thai: &[String],
    attempt: u32,
) -> Result<(ReviewerOut, Usage)> {
    let user = build_reviewer_user(
        source_jp,
        thai,
        reference_ctx,
        audit_findings,
        advisory_findings,
        prev_thai,
    );

    let mut messages = vec![Message::system(REVIEWER_SYSTEM), Message::user(user)];
    if attempt > 1 {
        messages.push(Message::user(
            "Your previous reply was not a valid review_result (it was missing the required \
             \"status\" field). Respond with ONLY a JSON object of the exact form \
             {\"status\":\"approve\"|\"reject\",\"feedback\":[...]} and nothing else. \
             feedback MUST be empty when status is \"approve\".",
        ));
    }

    let req = ChatRequest {
        model: model.to_string(),
        temperature: Some(if attempt > 1 { 0.4 } else { 0.0 }),
        messages,
        ..ChatRequest::default()
    };

    chat_structured::<ReviewerOut>(client, req, "review_result", reviewer_schema(), 0).await
}
