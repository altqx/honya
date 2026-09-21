//! System One (Jev) review gate.
//!
//! A decisions model answers typed questions with calibrated probabilities but
//! cannot write prose, so it can supply a verdict and never the feedback the
//! translator retries on. The gate therefore produces a [`ReviewerOut`] either
//! way and the pipeline folds it in exactly as it folds the LLM reviewer's —
//! no downstream code changes shape.
//!
//! Returning `None` means **defer**: run the LLM reviewer as before. Every
//! uncertainty resolves that way, so the gate can only ever save a call, never
//! block or fail a run.

use crate::llm::Usage;
use crate::llm::decisions::{Answer, Question, Switch, SystemOneHandle};
use crate::model::{ReviewGateMode, ReviewVerdict, ReviewerOut, TargetLanguage};

/// How an answer is turned into pass/fail.
#[derive(Debug, Clone, Copy)]
enum Check {
    /// Noul where "yes" is the good outcome.
    Affirm { pass_at: f64 },
    /// Noul where "yes" is a defect.
    Deny { pass_below: f64 },
    /// Score over ordered levels; passes at or above `pass_at`.
    Level { pass_at: f64 },
}

struct Axis {
    key: &'static str,
    question: Question,
    check: Check,
    /// Feedback line when the axis fails. Written as an instruction to the
    /// translator, since in Standalone mode this *is* the retry feedback.
    complaint: &'static str,
}

/// The gate's verdict question. Kept separate from the axes: it drives the
/// decision and is the only one whose `confidence` is thresholded.
const VERDICT: &str = "verdict";

fn verdict_question(target: TargetLanguage) -> Question {
    Question::choice(
        format!(
            "Compare the Japanese source with the {} translation. Should the translation be \
             approved as it stands, or sent back for revision?",
            target.label()
        ),
        &[
            (
                "approve",
                "Complete, faithful and natural. No correction needed.",
            ),
            (
                "revise",
                "Has at least one error a translator should correct.",
            ),
        ],
    )
}

fn axes(target: TargetLanguage, has_reference: bool) -> Vec<Axis> {
    let lang = target.label();
    let mut axes = vec![
        Axis {
            key: "completeness",
            question: Question::noul(format!(
                "Every sentence of the Japanese source appears in the {lang} translation, with \
                 nothing omitted and nothing invented that the source does not say."
            )),
            check: Check::Affirm { pass_at: 0.75 },
            complaint: "Coverage: content is missing from or invented against the source — \
                        translate every sentence and add nothing.",
        },
        Axis {
            key: "accuracy",
            question: Question::score(
                format!("How faithfully does the {lang} convey the meaning of the Japanese?"),
                &[
                    "Meaning diverges from the source",
                    "Minor drift in nuance or register",
                    "Faithful to the source",
                ],
            ),
            check: Check::Level { pass_at: 1.5 },
            complaint: "Fidelity: the meaning drifts from the source — re-check nuance, \
                        register and modifier scope against the Japanese.",
        },
        Axis {
            key: "fluency",
            question: Question::score(
                format!("How natural does the {lang} read as prose?"),
                &[
                    "Awkward, reads as machine translation",
                    "Understandable but stiff",
                    "Natural, idiomatic prose",
                ],
            ),
            check: Check::Level { pass_at: 1.5 },
            complaint: "Naturalness: the prose reads stiffly — rewrite it as a native \
                        speaker would phrase it, without changing the meaning.",
        },
        Axis {
            key: "residue",
            question: Question::noul(format!(
                "Untranslated Japanese characters, furigana artefacts or raw markup remain in \
                 the {lang} text."
            )),
            check: Check::Deny { pass_below: 0.25 },
            complaint: "Residue: untranslated Japanese or raw markup is left in the output — \
                        remove it.",
        },
    ];
    // Only worth asking when there is a reference bundle to check against.
    if has_reference {
        axes.push(Axis {
            key: "glossary",
            question: Question::noul(format!(
                "Every character name and glossary term in the {lang} translation matches the \
                 form given in the REFERENCE block."
            )),
            check: Check::Affirm { pass_at: 0.75 },
            complaint: "Glossary: a name or term does not match the REFERENCE block — use the \
                        established forms.",
        });
    }
    axes
}

impl Check {
    /// `None` when the answer is of the wrong primitive for this check, which
    /// is treated as an unusable response.
    fn passes(self, answer: &Answer) -> Option<bool> {
        match self {
            Check::Affirm { pass_at } => Some(answer.as_noul()? >= pass_at),
            Check::Deny { pass_below } => Some(answer.as_noul()? <= pass_below),
            Check::Level { pass_at } => Some(answer.as_score()? >= pass_at),
        }
    }
}

/// What the gate decided, ready for the pipeline to fold in.
///
/// `try_review` returning `None` means no call was made. A `GateOutcome` whose
/// `review` is `None` means one was made and the gate is deferring anyway —
/// still a billed call, so it reports what it cost rather than disappearing
/// from the run's totals.
pub struct GateOutcome {
    /// The verdict, or `None` to defer to the LLM reviewer.
    pub review: Option<ReviewerOut>,
    pub usage: Usage,
    /// One-line summary for the activity log.
    pub summary: String,
}

/// A call that went out and settled nothing: the LLM reviewer decides.
fn defer(usage: Usage, why: &str) -> Option<GateOutcome> {
    Some(GateOutcome {
        review: None,
        usage,
        summary: format!("review gate: deferred — {why}"),
    })
}

fn build_state(
    source_jp: &str,
    translated: &str,
    reference_ctx: &str,
    previous_translation: &[String],
) -> serde_json::Value {
    let mut state = serde_json::Map::new();
    state.insert("source_jp".into(), source_jp.trim().into());
    state.insert("translation".into(), translated.trim().into());
    if !reference_ctx.trim().is_empty() {
        state.insert("reference".into(), reference_ctx.trim().into());
    }
    if !previous_translation.is_empty() {
        state.insert("continuity".into(), previous_translation.join("\n").into());
    }
    serde_json::Value::Object(state)
}

/// Screen one chunk. `None` means defer to the LLM reviewer.
///
/// Callers pass a [`SystemOneHandle`] rather than a whole `PipelineCtx`, so the
/// decision logic stays unit-testable without a live pipeline.
#[allow(clippy::too_many_arguments)]
pub async fn try_review(
    system_one: Option<&SystemOneHandle>,
    target_language: TargetLanguage,
    source_jp: &str,
    translated: &str,
    reference_ctx: &str,
    previous_translation: &[String],
    audit_findings: &[String],
) -> Option<GateOutcome> {
    let s1 = system_one?;
    let mode = s1.config.review_gate_mode();
    // The deterministic audit already forces a reject downstream, and only the
    // LLM reviewer can say how to fix it — so the call would be wasted.
    if !audit_findings.is_empty() {
        return None;
    }

    let state = build_state(source_jp, translated, reference_ctx, previous_translation);

    let axes = axes(target_language, !reference_ctx.trim().is_empty());
    let mut questions = std::collections::BTreeMap::new();
    questions.insert(VERDICT.to_string(), verdict_question(target_language));
    for axis in &axes {
        questions.insert(axis.key.to_string(), axis.question.clone());
    }

    let resp = s1.ask(Switch::ReviewGate, state, questions).await?;
    let usage = resp.usage;

    let Some(verdict) = resp.answers.get(VERDICT) else {
        return defer(usage, "no verdict in the answer");
    };
    let Some(choice) = verdict.as_choice() else {
        return defer(usage, "the verdict was not a choice");
    };
    let approved_verdict = choice == "approve";
    let confidence = verdict.confidence();
    let threshold = s1.confidence_threshold();

    // Any axis answered with the wrong primitive makes the whole response
    // unusable — defer rather than guess.
    let mut failures = Vec::new();
    for axis in &axes {
        let Some(answer) = resp.answers.get(axis.key) else {
            return defer(usage, "an axis went unanswered");
        };
        let Some(passed) = axis.check.passes(answer) else {
            return defer(usage, "an axis came back as the wrong primitive");
        };
        if !passed {
            failures.push(axis.complaint);
        }
    }

    let clean = approved_verdict && failures.is_empty();
    if clean && confidence >= threshold {
        return Some(GateOutcome {
            review: Some(ReviewerOut {
                status: ReviewVerdict::Approve,
                feedback: Vec::new(),
            }),
            usage,
            summary: format!("review gate: approved (confidence {confidence:.2})"),
        });
    }

    match mode {
        // Anything short of a confident clean pass goes to the real reviewer.
        ReviewGateMode::Gate | ReviewGateMode::Off => defer(usage, "not a confident clean pass"),
        ReviewGateMode::Standalone => {
            // A confident-but-unclean result is a reject; an *unconfident* one
            // has no prose reviewer to fall back on here, so it is also a
            // reject — re-translating is the safe direction.
            let mut feedback: Vec<String> = failures.iter().map(|c| (*c).to_string()).collect();
            if feedback.is_empty() {
                feedback.push(
                    "The reviewer was not confident this chunk is correct. Re-check it against \
                     the source for meaning, coverage and naturalness."
                        .to_string(),
                );
            }
            let n = feedback.len();
            Some(GateOutcome {
                review: Some(ReviewerOut {
                    status: ReviewVerdict::Reject,
                    feedback,
                }),
                usage,
                summary: format!(
                    "review gate: rejected, {n} issue(s) (confidence {confidence:.2})"
                ),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::decisions::{
        DecisionsBackend, DecisionsRequest, DecisionsResponse, DecisionsUsage, MAX_STATE_CHARS,
    };
    use crate::model::{DecisionsProvider, SystemOne};
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeBackend {
        answers: BTreeMap<String, Answer>,
        calls: AtomicUsize,
        fail: bool,
    }

    impl FakeBackend {
        /// A clean, confident pass on every axis.
        fn passing() -> Self {
            let mut answers = BTreeMap::new();
            answers.insert(
                "verdict".to_string(),
                Answer::Choice {
                    choice: "approve".to_string(),
                    confidence: Some(0.95),
                },
            );
            answers.insert("completeness".to_string(), Answer::Noul { noul: 0.98 });
            answers.insert("residue".to_string(), Answer::Noul { noul: 0.01 });
            answers.insert("glossary".to_string(), Answer::Noul { noul: 0.97 });
            for k in ["accuracy", "fluency"] {
                answers.insert(
                    k.to_string(),
                    Answer::Score {
                        score: 1.9,
                        confidence: Some(0.9),
                    },
                );
            }
            Self {
                answers,
                calls: AtomicUsize::new(0),
                fail: false,
            }
        }

        fn failing_backend() -> Self {
            Self {
                answers: BTreeMap::new(),
                calls: AtomicUsize::new(0),
                fail: true,
            }
        }

        fn with(mut self, key: &str, answer: Answer) -> Self {
            self.answers.insert(key.to_string(), answer);
            self
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
                    input_tokens: 400,
                    output_tokens: 60,
                    cost: Some(0.000018),
                },
            })
        }
    }

    fn gate(mode: ReviewGateMode) -> SystemOne {
        SystemOne {
            enabled: true,
            review_gate: mode,
            provider: DecisionsProvider::OpenRouter,
            model: "typesafe/jev-1.13".to_string(),
            min_confidence: 0.8,
            ..SystemOne::default()
        }
    }

    fn handle(backend: &Arc<FakeBackend>, mode: ReviewGateMode) -> SystemOneHandle {
        SystemOneHandle {
            backend: backend.clone(),
            config: gate(mode),
        }
    }

    /// The gate's verdict, flattening "never asked" and "asked and deferred" —
    /// a distinction most of these tests do not care about. The one that does
    /// asserts on `run` directly.
    async fn verdict(
        backend: &Arc<FakeBackend>,
        mode: ReviewGateMode,
        audit: &[String],
    ) -> Option<ReviewerOut> {
        run(backend, mode, audit).await.and_then(|o| o.review)
    }

    async fn run(
        backend: &Arc<FakeBackend>,
        mode: ReviewGateMode,
        audit: &[String],
    ) -> Option<GateOutcome> {
        try_review(
            Some(&handle(backend, mode)),
            TargetLanguage::Thai,
            "猫が窓辺で眠っている。",
            "แมวกำลังนอนอยู่ริมหน้าต่าง",
            "GLOSSARY: 猫 = แมว",
            &[],
            audit,
        )
        .await
    }

    #[tokio::test]
    async fn confident_clean_pass_approves_and_reports_usage() {
        let b = Arc::new(FakeBackend::passing());
        let out = run(&b, ReviewGateMode::Gate, &[]).await.unwrap();
        let review = out.review.expect("a confident clean pass approves");
        assert!(review.approved());
        assert!(review.feedback.is_empty());
        assert_eq!(out.usage.prompt_tokens, 400);
        assert_eq!(b.calls(), 1);
    }

    #[tokio::test]
    async fn low_confidence_defers_even_when_every_axis_passes() {
        let b = Arc::new(FakeBackend::passing().with(
            "verdict",
            Answer::Choice {
                choice: "approve".to_string(),
                confidence: Some(0.42),
            },
        ));
        assert!(
            verdict(&b, ReviewGateMode::Gate, &[]).await.is_none(),
            "an approval below the threshold must fall through to the LLM reviewer"
        );
    }

    #[tokio::test]
    async fn failing_axis_defers_in_gate_mode() {
        let b = Arc::new(FakeBackend::passing().with("residue", Answer::Noul { noul: 0.9 }));
        assert!(verdict(&b, ReviewGateMode::Gate, &[]).await.is_none());
    }

    /// Deferring is not the same as never asking. A gate that called out and
    /// then handed the chunk to the LLM reviewer has still been billed, so it
    /// reports what it cost; only `None` means no call was made.
    #[tokio::test]
    async fn a_gate_that_defers_still_reports_what_the_call_cost() {
        let b = Arc::new(FakeBackend::passing().with(
            "verdict",
            Answer::Choice {
                choice: "approve".to_string(),
                confidence: Some(0.42),
            },
        ));
        let out = run(&b, ReviewGateMode::Gate, &[])
            .await
            .expect("a call went out");
        assert!(out.review.is_none(), "the LLM reviewer decides this one");
        assert_eq!(out.usage.prompt_tokens, 400, "and it was still billed");
        assert_eq!(b.calls(), 1);

        // Nothing asked, nothing to report.
        let quiet = Arc::new(FakeBackend::passing());
        assert!(run(&quiet, ReviewGateMode::Off, &[]).await.is_none());
        assert_eq!(quiet.calls(), 0);
    }

    #[tokio::test]
    async fn audit_findings_skip_the_call_entirely() {
        let b = Arc::new(FakeBackend::passing());
        let audit = vec!["Japanese punctuation residue".to_string()];
        assert!(run(&b, ReviewGateMode::Gate, &audit).await.is_none());
        assert_eq!(b.calls(), 0, "a chunk the audit already rejected must not cost a gate call");
    }

    #[tokio::test]
    async fn mode_off_never_calls_the_backend() {
        let b = Arc::new(FakeBackend::passing());
        assert!(run(&b, ReviewGateMode::Off, &[]).await.is_none());
        assert_eq!(b.calls(), 0);
    }

    #[tokio::test]
    async fn backend_error_defers() {
        let b = Arc::new(FakeBackend::failing_backend());
        assert!(
            run(&b, ReviewGateMode::Gate, &[]).await.is_none(),
            "a gate failure must degrade to the existing reviewer, never fail the chunk"
        );
    }

    #[tokio::test]
    async fn standalone_synthesizes_feedback_naming_the_failing_axis() {
        let b = Arc::new(
            FakeBackend::passing()
                .with(
                    "verdict",
                    Answer::Choice {
                        choice: "revise".to_string(),
                        confidence: Some(0.91),
                    },
                )
                .with("residue", Answer::Noul { noul: 0.88 }),
        );

        let out = run(&b, ReviewGateMode::Standalone, &[]).await.unwrap();
        let review = out.review.expect("standalone always reaches a verdict");
        assert!(!review.approved());
        assert_eq!(review.feedback.len(), 1);
        assert!(
            review.feedback[0].starts_with("Residue:"),
            "feedback must name the failing axis: {:?}",
            review.feedback
        );
    }

    #[tokio::test]
    async fn standalone_rejects_rather_than_approving_on_low_confidence() {
        let b = Arc::new(FakeBackend::passing().with(
            "verdict",
            Answer::Choice {
                choice: "approve".to_string(),
                confidence: Some(0.3),
            },
        ));
        let out = run(&b, ReviewGateMode::Standalone, &[]).await.unwrap();
        let review = out.review.expect("standalone always reaches a verdict");
        assert!(!review.approved());
        assert!(!review.feedback.is_empty(), "a reject must carry actionable feedback");
    }

    #[tokio::test]
    async fn wrong_primitive_in_an_answer_defers() {
        // A score answer where a noul was asked for: unusable, so defer.
        let b = Arc::new(FakeBackend::passing().with(
            "residue",
            Answer::Score {
                score: 1.0,
                confidence: Some(0.9),
            },
        ));
        assert!(verdict(&b, ReviewGateMode::Gate, &[]).await.is_none());
    }

    #[tokio::test]
    async fn oversized_chunk_defers_without_calling() {
        let b = Arc::new(FakeBackend::passing());
        let huge = "猫".repeat(MAX_STATE_CHARS + 1);
        let out = try_review(
            Some(&handle(&b, ReviewGateMode::Gate)),
            TargetLanguage::Thai,
            &huge,
            "แมว",
            "",
            &[],
            &[],
        )
        .await;
        assert!(out.is_none());
        assert_eq!(b.calls(), 0);
    }

    #[tokio::test]
    async fn glossary_axis_is_omitted_without_a_reference_bundle() {
        // No reference => the glossary answer is never required, so a response
        // lacking it still yields a decision.
        let mut fb = FakeBackend::passing();
        fb.answers.remove("glossary");
        let b = Arc::new(fb);
        let out = try_review(
            Some(&handle(&b, ReviewGateMode::Gate)),
            TargetLanguage::English,
            "猫",
            "cat",
            "",
            &[],
            &[],
        )
        .await;
        assert!(out.and_then(|o| o.review).is_some_and(|r| r.approved()));
    }
}
