//! Run the Reviewer agent (English, json_schema `review_result`) for one chunk:
//! diff Japanese source against target-language output for a verdict and feedback.

use crate::agents::prompts::{build_reviewer_user_for_language, reviewer_system};
use crate::llm::client::{LlmClient, Result};
use crate::llm::structured::{chat_structured, reviewer_schema};
use crate::llm::{ChatRequest, Message, Usage};
use crate::model::{AgentModel, ReviewerOut, TargetLanguage};

/// Review one translated chunk against its source. `reference_ctx` matches the
/// Translator context so glossary/pronoun/style checks are enforceable.
#[allow(clippy::too_many_arguments)]
pub async fn review_chunk(
    client: &dyn LlmClient,
    model: &AgentModel,
    target_language: TargetLanguage,
    source_jp: &str,
    translated: &str,
    reference_ctx: &str,
    audit_findings: &[String],
    advisory_findings: &[String],
    previous_translation: &[String],
    attempt: u32,
) -> Result<(ReviewerOut, Usage)> {
    let user = build_reviewer_user_for_language(
        target_language,
        source_jp,
        translated,
        reference_ctx,
        audit_findings,
        advisory_findings,
        previous_translation,
    );

    let mut messages = vec![
        Message::system(reviewer_system(target_language)),
        Message::user(user),
    ];
    if attempt > 1 {
        messages.push(Message::user(
            "Your previous reply was not an actionable review_result: it either missed required \
             fields or set status=\"reject\" without any correction in feedback. Respond with \
             ONLY a JSON object of the exact form \
             {\"status\":\"approve\"|\"reject\",\"feedback\":[...]} and nothing else. \
             feedback MUST contain at least one concise correction when status is \"reject\", \
             and MUST be empty when status is \"approve\".",
        ));
    }

    let req = ChatRequest {
        model: model.model.clone(),
        temperature: Some(if attempt > 1 { 0.4 } else { 0.0 }),
        messages,
        reasoning: model.reasoning_param(),
        ..ChatRequest::default()
    };

    chat_structured::<ReviewerOut>(client, req, "review_result", reviewer_schema(), 0).await
}
