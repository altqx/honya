//! System One entity alignment for the character roster.
//!
//! Name matching decides whether two roster entries are the same person by
//! comparing surfaces, so it can never connect 高橋陽菜, ハル and 先輩 — a light
//! novel's ordinary way of naming one girl. This asks instead.
//!
//! The asymmetry sets the policy: a wrong merge corrupts every fact linked to
//! both entries, while a missed one only leaves a duplicate the model can still
//! consolidate later. So a merge needs a confident answer, an unconfident one
//! becomes a suggestion, and a confident "this is someone new" only ever
//! *withholds* a merge the name rules would have made on their own.

use crate::llm::Usage;
use crate::llm::decisions::{Question, SystemOneHandle};
use crate::model::{Character, SystemOneFeature};
use crate::workspace::characters::Alignment;

/// Probability that the incoming entry already has a roster entry, above which
/// a merge is allowed at all.
const SAME_PERSON_AT: f64 = 0.75;
/// Below this the answer is "a person not on the roster yet", confident enough
/// to hold back a weak name match.
const NEW_PERSON_BELOW: f64 = 0.25;

const WHICH: &str = "which";
const ALREADY: &str = "already_on_roster";

pub struct AlignOutcome {
    pub alignment: Alignment,
    pub usage: Usage,
    /// One-line summary for the activity log.
    pub summary: Option<String>,
}

/// A call that went out and came back unusable.
///
/// It has no opinion — the name rules decide alone, exactly as before — but it
/// still reports what it cost, because it was billed. `None` stays reserved for
/// "nothing was asked".
fn unusable(usage: Usage) -> AlignOutcome {
    AlignOutcome {
        alignment: Alignment::default(),
        usage,
        summary: None,
    }
}

fn describe(c: &Character) -> serde_json::Value {
    serde_json::json!({
        "id": c.id,
        "jp_name": c.jp_name,
        "translated_name": c.translated_name,
        "romaji": c.romaji,
        "gender": c.gender,
        "aliases": c.aliases,
        "also_called": c.also_called.iter().map(|a| &a.jp).collect::<Vec<_>>(),
        "speech_style": c.speech_style,
        "notes": c.notes,
    })
}

/// Decide whether `incoming` is someone already on the roster. `None` means the
/// name rules decide alone, exactly as before.
pub async fn align(
    system_one: Option<&SystemOneHandle>,
    incoming: &Character,
    roster: &[Character],
) -> Option<AlignOutcome> {
    let s1 = system_one?;
    if roster.is_empty() {
        return None;
    }

    let state = serde_json::json!({
        "incoming": describe(incoming),
        "roster": roster.iter().map(describe).collect::<Vec<_>>(),
    });
    // The options are the roster ids themselves, so the answer is always an
    // entry that exists; the companion Noul decides whether any of them applies,
    // which keeps "nobody" from having to win a ranking it cannot win.
    let criteria: Vec<(&str, &str)> = roster
        .iter()
        .map(|c| (c.id.as_str(), c.jp_name.as_str()))
        .collect();
    let questions = [
        (
            WHICH.to_string(),
            Question::choice(
                "Which roster entry is the same person as `incoming`? Light novels name one \
                 person many ways — surname, given name, full name, nickname, title or address \
                 form — so judge the person, not the spelling.",
                &criteria,
            ),
        ),
        (
            ALREADY.to_string(),
            Question::noul(
                "`incoming` is a person who already appears in `roster` under a different name \
                 form, rather than someone new to the story.",
            ),
        ),
    ]
    .into_iter()
    .collect();

    let resp = s1
        .ask(SystemOneFeature::EntityAlignment, state, questions)
        .await?;
    let usage = resp.usage;

    let Some(already) = resp.answers.get(ALREADY).and_then(|a| a.as_noul()) else {
        return Some(unusable(usage));
    };
    let Some(which) = resp.answers.get(WHICH) else {
        return Some(unusable(usage));
    };
    // An id the model invented is not an alignment.
    let best = which
        .as_choice()
        .and_then(|id| roster.iter().find(|c| c.id == id))
        .map(|c| c.id.clone());
    let Some(best) = best else {
        return Some(unusable(usage));
    };
    let confident = which.confidence() >= s1.confidence_threshold();

    let (alignment, verdict) = if already >= SAME_PERSON_AT && confident {
        (
            Alignment {
                same_as: Some(best.clone()),
                ..Alignment::default()
            },
            format!("same as {best}"),
        )
    } else if already >= SAME_PERSON_AT {
        (
            Alignment {
                maybe: vec![best.clone()],
                ..Alignment::default()
            },
            format!("possibly {best}, left for review"),
        )
    } else if already <= NEW_PERSON_BELOW {
        (
            Alignment {
                ruled_out: roster.iter().map(|c| c.id.clone()).collect(),
                ..Alignment::default()
            },
            "a new person; weak name matches withheld".to_string(),
        )
    } else {
        (Alignment::default(), "undecided".to_string())
    };

    let tokens = usage.total_tokens;
    Some(AlignOutcome {
        alignment,
        usage,
        summary: Some(format!(
            "alignment: {} — {verdict} ({tokens} tok)",
            incoming.jp_name
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::decisions::{
        Answer, DecisionsBackend, DecisionsRequest, DecisionsResponse, DecisionsUsage,
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
        fn new(choice: &str, confidence: f64, already: f64) -> Self {
            let mut answers = BTreeMap::new();
            answers.insert(
                WHICH.to_string(),
                Answer::Choice {
                    choice: choice.to_string(),
                    confidence: Some(confidence),
                },
            );
            answers.insert(ALREADY.to_string(), Answer::Noul { noul: already });
            Self {
                answers,
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
                    input_tokens: 500,
                    output_tokens: 30,
                    cost: Some(0.00002),
                },
            })
        }
    }

    fn system_one(on: bool) -> SystemOne {
        SystemOne {
            enabled: true,
            entity_alignment: on,
            provider: DecisionsProvider::OpenRouter,
            model: "typesafe/jev-1.13".to_string(),
            min_confidence: 0.8,
            ..SystemOne::default()
        }
    }

    fn handle(backend: FakeBackend, on: bool) -> (SystemOneHandle, Arc<FakeBackend>) {
        let backend = Arc::new(backend);
        let handle = SystemOneHandle {
            backend: backend.clone(),
            config: system_one(on),
        };
        (handle, backend)
    }

    fn character(id: &str, jp: &str, translated: &str) -> Character {
        Character {
            id: id.to_string(),
            jp_name: jp.to_string(),
            translated_name: translated.to_string(),
            romaji: None,
            gender: None,
            honorific: None,
            speech_style: None,
            relationships: Vec::new(),
            aliases: Vec::new(),
            also_called: Vec::new(),
            notes: None,
            first_seen_chapter: Some(1),
        }
    }

    /// The case name matching cannot reach: a nickname sharing no surface with
    /// the full name already on the roster.
    fn nickname_case() -> (Character, Vec<Character>) {
        (
            character("haru", "ハル", "ฮารุ"),
            vec![
                character("takahashi-hina", "高橋陽菜", "ทาคาฮาชิ ฮินะ"),
                character("sensei", "先生", "อาจารย์"),
            ],
        )
    }

    #[tokio::test]
    async fn a_confident_match_merges_a_nickname_into_the_full_name() {
        let (inc, roster) = nickname_case();
        let (h, _b) = handle(FakeBackend::new("takahashi-hina", 0.93, 0.95), true);
        let out = align(Some(&h), &inc, &roster).await.unwrap();
        assert_eq!(out.alignment.same_as.as_deref(), Some("takahashi-hina"));
        assert!(out.alignment.maybe.is_empty());
        assert!(out.summary.unwrap().contains("530 tok"));
    }

    #[tokio::test]
    async fn an_unconfident_match_only_suggests() {
        let (inc, roster) = nickname_case();
        let (h, _b) = handle(FakeBackend::new("takahashi-hina", 0.44, 0.9), true);
        let out = align(Some(&h), &inc, &roster).await.unwrap();
        assert_eq!(
            out.alignment.same_as, None,
            "a wrong merge is the costly direction, so it needs confidence"
        );
        assert_eq!(out.alignment.maybe, vec!["takahashi-hina".to_string()]);
    }

    #[tokio::test]
    async fn a_confident_new_person_withholds_weak_name_matches() {
        let (inc, roster) = nickname_case();
        let (h, _b) = handle(FakeBackend::new("takahashi-hina", 0.9, 0.03), true);
        let out = align(Some(&h), &inc, &roster).await.unwrap();
        assert_eq!(out.alignment.same_as, None);
        assert_eq!(out.alignment.ruled_out.len(), roster.len());
    }

    #[tokio::test]
    async fn a_middling_answer_leaves_the_name_rules_alone() {
        let (inc, roster) = nickname_case();
        let (h, _b) = handle(FakeBackend::new("takahashi-hina", 0.9, 0.5), true);
        let out = align(Some(&h), &inc, &roster).await.unwrap();
        assert_eq!(out.alignment, Alignment::default());
    }

    /// An id the model invented settles nothing, but the call still went out
    /// and was still billed — so it reports what it cost rather than vanishing
    /// from the run's totals.
    #[tokio::test]
    async fn an_invented_id_settles_nothing_but_still_reports_its_cost() {
        let (inc, roster) = nickname_case();
        let (h, b) = handle(FakeBackend::new("someone-else", 0.99, 0.99), true);
        let out = align(Some(&h), &inc, &roster).await.unwrap();
        assert_eq!(out.alignment, Alignment::default(), "no opinion");
        assert_eq!(out.usage.total_tokens, 530, "but a billed one");
        assert_eq!(b.calls(), 1);
    }

    #[tokio::test]
    async fn backend_failure_leaves_the_name_rules_alone() {
        let (inc, roster) = nickname_case();
        let (h, _b) = handle(FakeBackend::broken(), true);
        assert!(align(Some(&h), &inc, &roster).await.is_none());
    }

    #[tokio::test]
    async fn feature_off_or_empty_roster_never_calls() {
        let (inc, roster) = nickname_case();
        let (off, b_off) = handle(FakeBackend::new("takahashi-hina", 0.99, 0.99), false);
        assert!(align(Some(&off), &inc, &roster).await.is_none());
        let (on, b_on) = handle(FakeBackend::new("takahashi-hina", 0.99, 0.99), true);
        assert!(align(Some(&on), &inc, &[]).await.is_none());
        assert!(align(None, &inc, &roster).await.is_none());
        assert_eq!(b_off.calls(), 0);
        assert_eq!(b_on.calls(), 0);
    }
}
