//! System One screening for the audit's judgement-shaped checks.
//!
//! [`audit::semantic_candidates`] over-finds the spans a semantic check could
//! apply to; this decides them. Each candidate carries the hand-tuned verdict
//! that used to *be* the check, so every uncertainty resolves back to it: the
//! feature off, no key, an oversized state, a backend error, a missing answer,
//! the wrong primitive, or a probability too close to call. A judgement can
//! therefore only ever sharpen a finding, never invent or lose one.

use crate::agents::audit::{SemanticCandidate, SemanticCheck, SemanticFinding};
use crate::llm::Usage;
use crate::llm::decisions::{DecisionsBackend, DecisionsRequest, Question};
use crate::model::{SystemOne, SystemOneFeature};

/// Same budget as the review gate: Jev's window is 32k tokens and Thai runs
/// near one token per char. An oversized chunk falls back rather than being
/// truncated.
const MAX_STATE_CHARS: usize = 24_000;

/// Probability bands, matching the review gate's `Affirm`/`Deny` axes. Between
/// them the answer is not worth acting on, and the heuristic decides.
const CONFIRM_AT: f64 = 0.75;
const DISMISS_BELOW: f64 = 0.25;

pub struct JudgeOutcome {
    pub findings: Vec<SemanticFinding>,
    pub usage: Usage,
    /// One-line summary for the activity log, or `None` when nothing was asked.
    pub summary: Option<String>,
}

/// The Noul put to System One for one candidate. Phrased so "yes" always means
/// "this really is the defect", matching `SemanticCandidate::heuristic`.
fn question(check: SemanticCheck, index: usize) -> Question {
    let path = format!("candidates[{index}]");
    Question::noul(match check {
        SemanticCheck::Gloss => format!(
            "In `{path}.context`, the parenthetical `{path}.span` merely repeats the preceding \
             Thai word in another script or spelling — a reading, romanization or \
             original-spelling gloss — rather than adding meaning the Thai does not already \
             carry."
        ),
        SemanticCheck::SelfPronoun => format!(
            "In `{path}.context`, `กู` is the speaker's first-person pronoun, rather than part \
             of a name, place or loanword such as กูรู, กูเกิล or อากู."
        ),
        SemanticCheck::CasualParticle => format!(
            "In `{path}.context`, `{path}.span` is a sentence-final casual Thai particle, \
             rather than a syllable inside a word or a name."
        ),
    })
}

fn build_state(
    source_jp: &str,
    translated: &str,
    candidates: &[SemanticCandidate],
) -> serde_json::Value {
    serde_json::json!({
        "source_jp": source_jp.trim(),
        "translation": translated.trim(),
        "candidates": candidates
            .iter()
            .map(|c| serde_json::json!({ "span": c.span, "context": c.context }))
            .collect::<Vec<_>>(),
    })
}

/// Decide `candidates`. `None` means fall back to the heuristic verdict for all
/// of them; an individual answer that is unusable falls back on its own.
pub async fn judge(
    backend: &dyn DecisionsBackend,
    system_one: &SystemOne,
    source_jp: &str,
    translated: &str,
    candidates: &[SemanticCandidate],
) -> Option<JudgeOutcome> {
    if !system_one.feature(SystemOneFeature::Audit) {
        return None;
    }
    // A clean chunk is the common case; it costs nothing and agrees with the
    // heuristic trivially, since the scan is strictly wider than the predicates.
    if candidates.is_empty() {
        return Some(JudgeOutcome {
            findings: Vec::new(),
            usage: Usage::default(),
            summary: None,
        });
    }

    let state = build_state(source_jp, translated, candidates);
    if state.to_string().chars().count() > MAX_STATE_CHARS {
        return None;
    }

    let questions = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| (format!("c{i}"), question(c.check, i)))
        .collect();

    let resp = backend
        .decide(&DecisionsRequest {
            model: system_one.model.clone(),
            state,
            questions,
        })
        .await
        .ok()?;

    let mut deferred = 0usize;
    let verdicts: Vec<bool> = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| {
            match resp
                .answers
                .get(&format!("c{i}"))
                .and_then(|a| a.as_noul())
            {
                Some(p) if p >= CONFIRM_AT => true,
                Some(p) if p <= DISMISS_BELOW => false,
                _ => {
                    deferred += 1;
                    c.heuristic
                }
            }
        })
        .collect();

    let findings = crate::agents::audit::semantic_findings(candidates, |i, _| verdicts[i]);
    let n = findings.len();
    let asked = candidates.len();
    Some(JudgeOutcome {
        findings,
        usage: resp.usage.to_usage(),
        summary: Some(format!(
            "audit judge: {asked} candidate(s) → {n} finding(s), {deferred} deferred to heuristic"
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::audit;
    use crate::llm::decisions::{Answer, DecisionsResponse, DecisionsUsage};
    use crate::model::{DecisionsProvider, TargetLanguage};
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeBackend {
        answers: BTreeMap<String, Answer>,
        calls: AtomicUsize,
        fail: bool,
    }

    impl FakeBackend {
        fn answering(nouls: &[(&str, f64)]) -> Self {
            Self {
                answers: nouls
                    .iter()
                    .map(|(k, p)| ((*k).to_string(), Answer::Noul { noul: *p }))
                    .collect(),
                calls: AtomicUsize::new(0),
                fail: false,
            }
        }

        fn broken() -> Self {
            Self {
                answers: BTreeMap::new(),
                calls: AtomicUsize::new(0),
                fail: true,
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }
    }

    #[async_trait::async_trait]
    impl DecisionsBackend for FakeBackend {
        async fn decide(
            &self,
            _req: &DecisionsRequest,
        ) -> crate::llm::client::Result<DecisionsResponse> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.fail {
                return Err(crate::llm::LlmError::Api {
                    status: 503,
                    message: "upstream down".to_string(),
                });
            }
            Ok(DecisionsResponse {
                model: "typesafe/jev-1.13".to_string(),
                answers: self.answers.clone(),
                usage: DecisionsUsage {
                    input_tokens: 300,
                    output_tokens: 20,
                    cost: Some(0.00001),
                },
            })
        }
    }

    fn system_one(audit: bool) -> SystemOne {
        SystemOne {
            enabled: true,
            audit,
            provider: DecisionsProvider::OpenRouter,
            model: "typesafe/jev-1.13".to_string(),
            ..SystemOne::default()
        }
    }

    /// A transliteration built from syllables the word list never learned. The
    /// scan finds it; only the judgement can call it.
    const UNLISTED_GLOSS: &str = "ยัยคุณหนู (โอโจซามะ) ยิ้มให้";

    fn candidates(text: &str) -> Vec<SemanticCandidate> {
        audit::semantic_candidates(TargetLanguage::Thai, text)
    }

    #[tokio::test]
    async fn confirms_a_gloss_the_word_list_would_have_missed() {
        let c = candidates(UNLISTED_GLOSS);
        assert_eq!(c.len(), 1);
        assert!(
            !c[0].heuristic,
            "precondition: the hand-tuned predicate misses this one"
        );

        let b = FakeBackend::answering(&[("c0", 0.94)]);
        let out = judge(&b, &system_one(true), "お嬢様は微笑んだ。", UNLISTED_GLOSS, &c)
            .await
            .unwrap();
        assert_eq!(out.findings.len(), 1);
        assert!(out.findings[0].message.contains("โอโจซามะ"));
        assert_eq!(out.usage.prompt_tokens, 300);
    }

    #[tokio::test]
    async fn dismisses_a_candidate_the_heuristic_would_have_flagged() {
        // `กู` inside a loanword: the frame rules already try to spare it, but
        // a confident "no" settles it without another suffix in the list.
        let text = "เขาเป็นกูรูด้านการตลาด";
        let c = candidates(text);
        assert_eq!(c.len(), 1);

        let b = FakeBackend::answering(&[("c0", 0.02)]);
        let out = judge(&b, &system_one(true), "彼はマーケティングの達人だ。", text, &c)
            .await
            .unwrap();
        assert!(
            out.findings.is_empty(),
            "a confident dismissal must drop the finding: {:?}",
            out.findings
        );
    }

    #[tokio::test]
    async fn an_unconfident_answer_falls_back_to_the_heuristic() {
        let text = "กูจะไปเอง";
        let c = candidates(text);
        assert!(c[0].heuristic, "precondition: the heuristic flags this");

        let b = FakeBackend::answering(&[("c0", 0.5)]);
        let out = judge(&b, &system_one(true), "俺が行く。", text, &c)
            .await
            .unwrap();
        assert_eq!(out.findings.len(), 1, "a coin flip must not drop a finding");
        assert!(out.summary.unwrap().contains("1 deferred"));
    }

    #[tokio::test]
    async fn a_missing_or_mistyped_answer_falls_back_per_candidate() {
        let text = "กูจะไปเอง";
        let c = candidates(text);
        let b = FakeBackend::answering(&[]);
        let out = judge(&b, &system_one(true), "俺が行く。", text, &c)
            .await
            .unwrap();
        assert_eq!(out.findings.len(), 1);
    }

    #[tokio::test]
    async fn backend_failure_defers_the_whole_set() {
        let c = candidates("กูจะไปเอง");
        let b = FakeBackend::broken();
        assert!(
            judge(&b, &system_one(true), "俺が行く。", "กูจะไปเอง", &c)
                .await
                .is_none(),
            "a failed judgement must hand back to the heuristic, never fail the chunk"
        );
    }

    #[tokio::test]
    async fn feature_off_never_calls_the_backend() {
        let c = candidates("กูจะไปเอง");
        let b = FakeBackend::answering(&[("c0", 0.99)]);
        assert!(judge(&b, &system_one(false), "俺が行く。", "กูจะไปเอง", &c)
            .await
            .is_none());
        assert_eq!(b.calls(), 0);
    }

    #[tokio::test]
    async fn a_clean_chunk_costs_no_call() {
        let c = candidates("แมวกำลังนอนอยู่ริมหน้าต่าง");
        assert!(c.is_empty());
        let b = FakeBackend::answering(&[]);
        let out = judge(&b, &system_one(true), "猫が眠っている。", "แมว", &c)
            .await
            .unwrap();
        assert!(out.findings.is_empty());
        assert_eq!(b.calls(), 0, "no candidate means nothing to ask");
        assert_eq!(out.usage.total_tokens, 0);
    }

    #[tokio::test]
    async fn oversized_state_defers_without_calling() {
        let huge = format!("{}{}", "ก".repeat(MAX_STATE_CHARS), "กูจะไปเอง");
        let c = candidates(&huge);
        let b = FakeBackend::answering(&[("c0", 0.99)]);
        assert!(judge(&b, &system_one(true), "俺", &huge, &c).await.is_none());
        assert_eq!(b.calls(), 0);
    }
}
