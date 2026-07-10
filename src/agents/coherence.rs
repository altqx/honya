//! Whole-chapter coherence sweep.
//!
//! The per-chunk Reviewer only ever sees one chunk plus a short continuity tail, so
//! it structurally cannot catch drift ACROSS chunks — a self-pronoun that shifts
//! between chunk 2 and chunk 9, a name rendered two ways, a term that drifts. After
//! a chapter's chunks are all written, this runs one pass over the assembled translation to
//! surface those chapter-wide inconsistencies. Findings are recorded as continuity
//! notes (the QA inbox already surfaces warning/conflict notes), never auto-fixed.

use serde::Deserialize;

use crate::agents::prompts::coherence_system;
use crate::llm::client::{LlmClient, Result};
use crate::llm::structured::{chat_structured, coherence_schema};
use crate::llm::{ChatRequest, Message, Usage};
use crate::model::TargetLanguage;

/// Cap on the assembled translation fed to the sweep; longer chapters are head-truncated and
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
    /// Structured fix hint; empty unless one canonical target form is clear.
    #[serde(default)]
    pub resolve_kind: String,
    #[serde(default)]
    pub resolve_jp: String,
    #[serde(default, alias = "resolve_canonical_th")]
    pub resolve_canonical_translation: String,
}

/// Run the coherence sweep over one assembled chapter translation. `reference_ctx` is the
/// same glossary/character/style bundle the chunks were translated against, scoped
/// to this chapter.
pub async fn coherence_sweep(
    client: &dyn LlmClient,
    model: &crate::model::AgentModel,
    chapter_translation: &str,
    reference_ctx: &str,
    target_language: TargetLanguage,
) -> Result<(CoherenceOut, Usage, bool)> {
    let trimmed = chapter_translation.trim();
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
    let (open, close, task) = match target_language {
        TargetLanguage::Thai => (
            "<<CHAPTER_TRANSLATION: the fully-assembled Thai translation of this chapter>>\n",
            "\n<<END_CHAPTER_TRANSLATION>>\n\n",
            "Audit this chapter for cross-chunk inconsistencies and return coherence_result.",
        ),
        TargetLanguage::English => (
            "<<CHAPTER_EN: the fully assembled English translation of this chapter>>\n",
            "\n<<END_CHAPTER_EN>>\n\n",
            "Audit only cross-chunk inconsistencies and return coherence_result.",
        ),
    };
    user.push_str(open);
    user.push_str(&body);
    user.push_str(close);
    user.push_str(task);

    let req = ChatRequest {
        model: model.model.clone(),
        messages: vec![
            Message::system(coherence_system(target_language)),
            Message::user(user),
        ],
        temperature: Some(0.0),
        reasoning: model.reasoning_param(),
        ..ChatRequest::default()
    };

    let (out, usage) =
        chat_structured::<CoherenceOut>(client, req, "coherence_result", coherence_schema(), 1)
            .await?;
    Ok((out, usage, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coherence_accepts_legacy_resolution_field() {
        let out: CoherenceOut = serde_json::from_value(serde_json::json!({
            "issues": [{
                "severity": "warning",
                "note": "name drift",
                "resolve_canonical_th": "ริน"
            }]
        }))
        .unwrap();

        assert_eq!(out.issues[0].resolve_canonical_translation, "ริน");
    }
}
