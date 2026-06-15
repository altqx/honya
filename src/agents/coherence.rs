//! Whole-chapter coherence sweep.
//!
//! The per-chunk Reviewer only ever sees one chunk plus a short continuity tail, so
//! it structurally cannot catch drift ACROSS chunks — a self-pronoun that shifts
//! between chunk 2 and chunk 9, a name rendered two ways, a term that drifts. After
//! a chapter's chunks are all written, this runs one pass over the assembled Thai to
//! surface those chapter-wide inconsistencies. Findings are recorded as continuity
//! notes (the QA inbox already surfaces warning/conflict notes), never auto-fixed.

use serde::Deserialize;

use crate::agents::prompts::COHERENCE_SYSTEM;
use crate::llm::client::{LlmClient, Result};
use crate::llm::structured::{chat_structured, coherence_schema};
use crate::llm::{ChatRequest, Message, Usage};

/// Cap on assembled Thai fed to the sweep; longer chapters are head-truncated and
/// the caller logs that coverage was bounded.
pub const MAX_CHAPTER_CHARS: usize = 24_000;

#[derive(Debug, Clone, Deserialize)]
pub struct CoherenceOut {
    #[serde(default)]
    pub issues: Vec<CoherenceIssue>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CoherenceIssue {
    pub severity: String,
    pub note: String,
    /// Structured fix hint; empty unless one canonical Thai form is clear.
    #[serde(default)]
    pub resolve_kind: String,
    #[serde(default)]
    pub resolve_jp: String,
    #[serde(default)]
    pub resolve_canonical_th: String,
}

/// Run the coherence sweep over one assembled chapter's Thai. `reference_ctx` is the
/// same glossary/character/style bundle the chunks were translated against, scoped
/// to this chapter.
pub async fn coherence_sweep(
    client: &dyn LlmClient,
    model: &str,
    chapter_thai: &str,
    reference_ctx: &str,
) -> Result<(CoherenceOut, Usage, bool)> {
    let trimmed = chapter_thai.trim();
    let truncated = trimmed.chars().count() > MAX_CHAPTER_CHARS;
    let body: String = if truncated {
        trimmed.chars().take(MAX_CHAPTER_CHARS).collect()
    } else {
        trimmed.to_string()
    };

    let mut user = String::new();
    let rctx = reference_ctx.trim();
    if !rctx.is_empty() {
        user.push_str(
            "<<REFERENCE: terminology policies / character pronouns / style for this chapter>>\n",
        );
        user.push_str(rctx);
        user.push_str("\n<<END_REFERENCE>>\n\n");
    }
    user.push_str("<<CHAPTER_TH: the fully-assembled Thai translation of this chapter>>\n");
    user.push_str(&body);
    user.push_str("\n<<END_CHAPTER_TH>>\n\nAudit this chapter for cross-chunk inconsistencies and return coherence_result.");

    let req = ChatRequest {
        model: model.to_string(),
        messages: vec![Message::system(COHERENCE_SYSTEM), Message::user(user)],
        temperature: Some(0.0),
        ..ChatRequest::default()
    };

    let (out, usage) =
        chat_structured::<CoherenceOut>(client, req, "coherence_result", coherence_schema(), 1)
            .await?;
    Ok((out, usage, truncated))
}
