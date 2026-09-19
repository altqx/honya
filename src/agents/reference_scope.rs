//! System One scoping for the per-chunk reference bundle.
//!
//! `build_reference_ctx` picks the glossary terms and characters to inject by
//! testing whether their Japanese form literally appears in the chunk, then
//! truncating to a cap. That leaves two gaps the string test cannot close:
//!
//! - A character the passage carries entirely by pronoun, title or role —
//!   彼女, 先輩, あの人 — is invisible to `contains`, so their pronouns and
//!   register never reach the Translator for the chunk that needs them most.
//!   (The existing `prev_chunk_text` fallback is a partial workaround for this.)
//! - When more entries match than the cap allows, `truncate` drops by roster
//!   order, which has nothing to do with what this passage is about.
//!
//! Code still does the recall pass — `contains` is cheap and exact, and a
//! literal match is never second-guessed. This only adds what the test missed
//! and orders what it over-produced.
//!
//! Unlike the other judgements this one sits *before* the Translator call, so it
//! is a round trip on the critical path. It is asked only when the answer could
//! change the bundle: nothing to add and nothing to trim means no call.

use crate::llm::decisions::{DecisionsRequest, Question, SystemOneHandle};
use crate::model::{Character, GlossaryTerm, SystemOneFeature};

/// Roster members put to a presence question per chunk. The bundle caps at 40
/// characters, so asking about far more than that cannot change it much.
const MAX_PRESENCE_CANDIDATES: usize = 24;
/// Probability above which an unnamed reference counts as present. Deliberately
/// above a coin flip: a spurious character in the bundle spends context and can
/// mislead the Translator's pronoun choice.
const PRESENT_AT: f64 = 0.7;
/// Char budget for the state, matching the other judgements.
const MAX_STATE_CHARS: usize = 24_000;

pub struct ScopeOutcome {
    /// Roster ids the passage refers to without naming them.
    pub implied: Vec<String>,
    /// `(index into the ranked terms, probability)`, most relevant first.
    pub term_order: Vec<usize>,
    pub summary: String,
}

fn describe_character(c: &Character) -> serde_json::Value {
    serde_json::json!({
        "id": c.id,
        "jp_name": c.jp_name,
        "translated_name": c.translated_name,
        "gender": c.gender,
        "also_called": c.also_called.iter().map(|a| &a.jp).collect::<Vec<_>>(),
        "notes": c.notes,
    })
}

/// Decide which of `absent` the passage refers to anyway, and order `overflow`
/// by how much this passage needs it. `None` leaves the string test in charge.
pub async fn scope(
    system_one: Option<&SystemOneHandle>,
    chunk_text: &str,
    absent: &[Character],
    overflow: &[GlossaryTerm],
) -> Option<ScopeOutcome> {
    let s1 = system_one?;
    if !s1.config.feature(SystemOneFeature::ReferenceScope) {
        return None;
    }
    let absent = &absent[..absent.len().min(MAX_PRESENCE_CANDIDATES)];
    if absent.is_empty() && overflow.is_empty() {
        return None;
    }

    let state = serde_json::json!({
        "passage": chunk_text.trim(),
        "absent_characters": absent.iter().map(describe_character).collect::<Vec<_>>(),
        "terms": overflow
            .iter()
            .map(|t| serde_json::json!({ "jp_term": t.jp_term, "translated": t.translated_term }))
            .collect::<Vec<_>>(),
    });
    if state.to_string().chars().count() > MAX_STATE_CHARS {
        return None;
    }

    let mut questions = std::collections::BTreeMap::new();
    for i in 0..absent.len() {
        questions.insert(
            format!("p{i}"),
            Question::noul(format!(
                "`passage` refers to the person in `absent_characters[{i}]` — as a speaker, as \
                 the viewpoint, or by a pronoun, title or role rather than by name. Their name \
                 does not appear in the passage, so judge it from who is present and what is \
                 happening."
            )),
        );
    }
    for j in 0..overflow.len() {
        questions.insert(
            format!("t{j}"),
            Question::noul(format!(
                "Getting the rendering of `terms[{j}].jp_term` right matters for translating \
                 `passage` — the term carries weight here, rather than appearing in passing."
            )),
        );
    }

    let resp = s1
        .backend
        .decide(&DecisionsRequest {
            model: s1.config.model.clone(),
            state,
            questions,
        })
        .await
        .ok()?;

    let implied: Vec<String> = absent
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            resp.answers
                .get(&format!("p{i}"))
                .and_then(|a| a.as_noul())
                .is_some_and(|p| p >= PRESENT_AT)
        })
        .map(|(_, c)| c.id.clone())
        .collect();

    // Unanswered terms sort last rather than being dropped: the cap still
    // decides how many survive, and an unranked term is not a rejected one.
    let mut ranked: Vec<(usize, f64)> = overflow
        .iter()
        .enumerate()
        .map(|(j, _)| {
            let p = resp
                .answers
                .get(&format!("t{j}"))
                .and_then(|a| a.as_noul())
                .unwrap_or(0.0);
            (j, p)
        })
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));

    let n_implied = implied.len();
    let n_ranked = ranked.len();
    Some(ScopeOutcome {
        implied,
        term_order: ranked.into_iter().map(|(j, _)| j).collect(),
        summary: format!("reference scope: +{n_implied} implied character(s), {n_ranked} term(s) ranked"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::decisions::{Answer, DecisionsBackend, DecisionsResponse, DecisionsUsage};
    use crate::model::{DecisionsProvider, SystemOne, TermPolicy};
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeBackend {
        nouls: BTreeMap<String, f64>,
        calls: AtomicUsize,
        fail: bool,
    }

    impl FakeBackend {
        fn new(nouls: &[(&str, f64)]) -> Self {
            Self {
                nouls: nouls.iter().map(|(k, v)| ((*k).to_string(), *v)).collect(),
                calls: AtomicUsize::new(0),
                fail: false,
            }
        }

        fn broken() -> Self {
            Self {
                nouls: BTreeMap::new(),
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
            req: &DecisionsRequest,
        ) -> crate::llm::client::Result<DecisionsResponse> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.fail {
                return Err(crate::llm::LlmError::Api {
                    status: 503,
                    message: "down".to_string(),
                });
            }
            Ok(DecisionsResponse {
                model: "typesafe/jev-1.13".to_string(),
                answers: req
                    .questions
                    .keys()
                    .filter_map(|k| Some((k.clone(), Answer::Noul { noul: *self.nouls.get(k)? })))
                    .collect(),
                usage: DecisionsUsage::default(),
            })
        }
    }

    fn handle(backend: FakeBackend, on: bool) -> (SystemOneHandle, Arc<FakeBackend>) {
        let backend = Arc::new(backend);
        let handle = SystemOneHandle {
            backend: backend.clone(),
            config: SystemOne {
                enabled: true,
                reference_scope: on,
                provider: DecisionsProvider::OpenRouter,
                model: "typesafe/jev-1.13".to_string(),
                ..SystemOne::default()
            },
        };
        (handle, backend)
    }

    fn character(id: &str, jp: &str) -> Character {
        Character {
            id: id.to_string(),
            jp_name: jp.to_string(),
            translated_name: String::new(),
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

    fn term(jp: &str) -> GlossaryTerm {
        GlossaryTerm {
            jp_term: jp.to_string(),
            translated_term: String::new(),
            romaji: None,
            category: None,
            gloss: None,
            policy: Some(TermPolicy::Preferred),
            forbidden_translations: Vec::new(),
            context_rule: None,
            protected: None,
            do_not_translate: None,
            first_seen_chapter: None,
        }
    }

    /// The gap the string test cannot close: a chunk that carries its viewpoint
    /// character entirely by pronoun.
    #[tokio::test]
    async fn adds_a_character_the_passage_only_implies() {
        let absent = vec![character("hina", "陽菜"), character("kenji", "健二")];
        let b = FakeBackend::new(&[("p0", 0.93), ("p1", 0.06)]);
        let (h, _fake) = handle(b, true);
        let out = scope(Some(&h), "そして彼女は静かに歩き出した。", &absent, &[])
            .await
            .unwrap();
        assert_eq!(out.implied, vec!["hina".to_string()]);
    }

    #[tokio::test]
    async fn an_unconfident_presence_is_not_added() {
        let absent = vec![character("hina", "陽菜")];
        let b = FakeBackend::new(&[("p0", 0.55)]);
        let (h, _fake) = handle(b, true);
        let out = scope(Some(&h), "そして彼女は歩き出した。", &absent, &[])
            .await
            .unwrap();
        assert!(
            out.implied.is_empty(),
            "a spurious character spends context and can mislead pronoun choice"
        );
    }

    #[tokio::test]
    async fn ranks_overflowing_terms_by_what_the_passage_needs() {
        let terms = vec![term("聖剣"), term("学園"), term("魔力")];
        let b = FakeBackend::new(&[("t0", 0.2), ("t1", 0.95), ("t2", 0.6)]);
        let (h, _fake) = handle(b, true);
        let out = scope(Some(&h), "学園の門をくぐる。", &[], &terms).await.unwrap();
        assert_eq!(out.term_order, vec![1, 2, 0]);
    }

    #[tokio::test]
    async fn an_unranked_term_sorts_last_rather_than_vanishing() {
        let terms = vec![term("聖剣"), term("学園")];
        let b = FakeBackend::new(&[("t1", 0.9)]);
        let (h, _fake) = handle(b, true);
        let out = scope(Some(&h), "学園の門をくぐる。", &[], &terms).await.unwrap();
        assert_eq!(out.term_order, vec![1, 0]);
    }

    #[tokio::test]
    async fn nothing_to_add_or_trim_costs_no_call() {
        let (h, fake) = handle(FakeBackend::new(&[("p0", 0.99)]), true);
        assert!(scope(Some(&h), "本文。", &[], &[]).await.is_none());
        assert_eq!(fake.calls(), 0);
    }

    #[tokio::test]
    async fn feature_off_or_dead_backend_leaves_the_string_test_in_charge() {
        let absent = vec![character("hina", "陽菜")];
        let (off, fake) = handle(FakeBackend::new(&[("p0", 0.99)]), false);
        assert!(scope(Some(&off), "彼女は。", &absent, &[]).await.is_none());
        assert_eq!(fake.calls(), 0);

        let (broken, _) = handle(FakeBackend::broken(), true);
        assert!(scope(Some(&broken), "彼女は。", &absent, &[]).await.is_none());
        assert!(scope(None, "彼女は。", &absent, &[]).await.is_none());
    }

}
