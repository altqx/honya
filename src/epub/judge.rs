//! System One classification of an EPUB spine.
//!
//! [`segment::classify`] decides what a spine page is with per-publisher
//! constants: a `toc` substring in the body class, "eight or more internal links
//! and fewer than forty prose characters each", and a hardcoded list of Japanese
//! chrome labels (表紙 / 扉 / 目次 / 奥付 …). Those keep needing another entry
//! for the next publisher, and one misread page collapses a whole book into a
//! single chapter.
//!
//! This asks instead, following TypeSafe's structure-recovery recipe: code keeps
//! the direct evidence — spine order, `is_image_only`, image relocation — and
//! the model answers only the ambiguous call, one Choice per document over a
//! tagged table of the whole spine. A document whose answer is missing or
//! unconfident falls back to the heuristics on its own, so a partial answer is
//! still worth having.

use crate::llm::decisions::{DecisionsRequest, Question, SystemOneHandle};
use crate::model::SystemOneFeature;

use super::segment::{DocInput, DocRole};

/// Questions per request. The spine table is the same for all of them and
/// dominates the cost, so batching is what makes this affordable; the batch size
/// only bounds how large one response has to be.
const QUESTIONS_PER_REQUEST: usize = 32;
/// Beyond this a spine is not a light novel, and the table alone would not fit
/// the state budget even with excerpts dropped entirely. A volume runs to a few
/// dozen spine documents; this is generous.
const MAX_DOCS: usize = 120;
/// Leading characters of each document's cleansed markdown put in the table.
const EXCERPT_CHARS: usize = 160;
/// Char budget for the shared state, matching the other judgements.
const MAX_STATE_CHARS: usize = 24_000;

pub struct SpineOutcome {
    /// One entry per input document, in order; `None` means "use the heuristic".
    pub roles: Vec<Option<DocRole>>,
    pub summary: String,
}

fn role_criteria() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "front_matter",
            "Cover, title page, colophon-style credits at the front, or an insert \
             illustration plate — book chrome before the story proper.",
        ),
        (
            "nav_toc",
            "A table of contents or navigation list: mostly links or entry labels \
             pointing at the other documents, with little or no prose of its own.",
        ),
        (
            "chapter_start",
            "Opens a chapter or a titled section — a chapter title page, a title \
             header image, or prose beginning a new chapter. The documents after it \
             continue it.",
        ),
        (
            "continuation",
            "Continues the chapter already running: prose picking up where the \
             previous document left off, or a mid-chapter illustration.",
        ),
        (
            "back_matter",
            "Afterword plates, colophon, publisher advertisements — chrome after the \
             story ends.",
        ),
    ]
}

fn role_from(choice: &str) -> Option<DocRole> {
    Some(match choice {
        "front_matter" => DocRole::FrontMatter,
        "nav_toc" => DocRole::NavToc,
        "chapter_start" => DocRole::ChapterStart,
        "continuation" => DocRole::Continuation,
        "back_matter" => DocRole::BackMatter,
        _ => return None,
    })
}

fn excerpt(md: &str, max: usize) -> String {
    let t = md.trim();
    if t.chars().count() <= max {
        t.to_string()
    } else {
        t.chars().take(max).collect::<String>() + "…"
    }
}

fn describe(i: usize, doc: &DocInput, excerpt_chars: usize) -> serde_json::Value {
    let md = doc.markdown.trim();
    serde_json::json!({
        "id": format!("d{i}"),
        "file": doc.archive_path.rsplit('/').next().unwrap_or(&doc.archive_path),
        "toc_label": doc.toc_title,
        "body_class": doc.body_class,
        "images": md.matches("![").count(),
        "internal_links": doc.internal_link_count,
        "text_chars": md.chars().filter(|c| !c.is_whitespace()).count(),
        "excerpt": excerpt(md, excerpt_chars),
    })
}

/// The spine table, shrunk until it fits the state budget. `None` when even bare
/// entries are too large to send.
fn build_state(docs: &[DocInput]) -> Option<serde_json::Value> {
    for chars in [EXCERPT_CHARS, 80, 40, 0] {
        let state = serde_json::json!({
            "spine": docs
                .iter()
                .enumerate()
                .map(|(i, d)| describe(i, d, chars))
                .collect::<Vec<_>>(),
        });
        if state.to_string().chars().count() <= MAX_STATE_CHARS {
            return Some(state);
        }
    }
    None
}

/// Classify every document in `docs`. `None` means the heuristics decide the
/// whole spine, as before.
pub async fn classify_spine(
    system_one: Option<&SystemOneHandle>,
    docs: &[DocInput],
) -> Option<SpineOutcome> {
    let s1 = system_one?;
    if !s1.config.feature(SystemOneFeature::Segmentation) || docs.is_empty() {
        return None;
    }
    if docs.len() > MAX_DOCS {
        return None;
    }
    let state = build_state(docs)?;
    let criteria = role_criteria();
    let threshold = s1.config.confidence_threshold();

    let mut roles: Vec<Option<DocRole>> = vec![None; docs.len()];
    let mut decided = 0usize;
    for batch in (0..docs.len()).collect::<Vec<_>>().chunks(QUESTIONS_PER_REQUEST) {
        let questions = batch
            .iter()
            .map(|&i| {
                (
                    format!("d{i}"),
                    Question::choice(
                        format!(
                            "What is document `spine[{i}]` doing in this book? Read it in the \
                             order the spine gives, using the documents before and after it to \
                             tell a chapter opening from its continuation."
                        ),
                        &criteria,
                    ),
                )
            })
            .collect();

        // A failed batch is not fatal: its documents keep `None` and fall back.
        let Ok(resp) = s1
            .backend
            .decide(&DecisionsRequest {
                model: s1.config.model.clone(),
                state: state.clone(),
                questions,
            })
            .await
        else {
            continue;
        };

        for &i in batch {
            let Some(answer) = resp.answers.get(&format!("d{i}")) else {
                continue;
            };
            if answer.confidence() < threshold {
                continue;
            }
            if let Some(role) = answer.as_choice().and_then(role_from) {
                roles[i] = Some(role);
                decided += 1;
            }
        }
    }

    if decided == 0 {
        return None;
    }
    let total = docs.len();
    Some(SpineOutcome {
        roles,
        summary: format!("spine: {decided}/{total} document(s) classified"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::decisions::{
        Answer, DecisionsBackend, DecisionsResponse, DecisionsUsage, SystemOneHandle,
    };
    use crate::model::{DecisionsProvider, SystemOne};
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeBackend {
        /// `d{i}` → (choice, confidence).
        by_key: BTreeMap<String, (String, f64)>,
        calls: AtomicUsize,
        fail: bool,
    }

    impl FakeBackend {
        fn new(pairs: &[(&str, &str, f64)]) -> Self {
            Self {
                by_key: pairs
                    .iter()
                    .map(|(k, c, conf)| ((*k).to_string(), ((*c).to_string(), *conf)))
                    .collect(),
                calls: AtomicUsize::new(0),
                fail: false,
            }
        }

        fn broken() -> Self {
            Self {
                by_key: BTreeMap::new(),
                calls: AtomicUsize::new(0),
                fail: true,
            }
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
            let answers = req
                .questions
                .keys()
                .filter_map(|k| {
                    let (choice, confidence) = self.by_key.get(k)?;
                    Some((
                        k.clone(),
                        Answer::Choice {
                            choice: choice.clone(),
                            confidence: Some(*confidence),
                        },
                    ))
                })
                .collect();
            Ok(DecisionsResponse {
                model: "typesafe/jev-1.13".to_string(),
                answers,
                usage: DecisionsUsage::default(),
            })
        }
    }

    fn handle(backend: FakeBackend, on: bool) -> SystemOneHandle {
        SystemOneHandle {
            backend: Arc::new(backend),
            config: SystemOne {
                enabled: true,
                segmentation: on,
                provider: DecisionsProvider::OpenRouter,
                model: "typesafe/jev-1.13".to_string(),
                min_confidence: 0.8,
                ..SystemOne::default()
            },
        }
    }

    fn doc(path: &str, md: &str, label: Option<&str>) -> DocInput {
        DocInput {
            archive_path: path.to_string(),
            markdown: md.to_string(),
            toc_title: label.map(|s| s.to_string()),
            internal_link_count: 0,
            body_class: None,
        }
    }

    fn spine() -> Vec<DocInput> {
        vec![
            doc("xhtml/p-cover.xhtml", "![cover](../images/cover.jpg)", Some("表紙")),
            doc("xhtml/p-toc.xhtml", "- ch1\n- ch2", Some("目次")),
            doc("xhtml/p-001.xhtml", "![m001](../images/m001.png)", Some("第一章")),
            doc("xhtml/p-002.xhtml", "本文が続く。", None),
        ]
    }

    #[tokio::test]
    async fn classifies_each_document_and_reports_coverage() {
        let b = FakeBackend::new(&[
            ("d0", "front_matter", 0.97),
            ("d1", "nav_toc", 0.99),
            ("d2", "chapter_start", 0.93),
            ("d3", "continuation", 0.91),
        ]);
        let h = handle(b, true);
        let out = classify_spine(Some(&h), &spine()).await.unwrap();
        assert_eq!(
            out.roles,
            vec![
                Some(DocRole::FrontMatter),
                Some(DocRole::NavToc),
                Some(DocRole::ChapterStart),
                Some(DocRole::Continuation),
            ]
        );
        assert!(out.summary.contains("4/4"));
    }

    #[tokio::test]
    async fn an_unconfident_or_unknown_answer_falls_back_for_that_document_only() {
        let b = FakeBackend::new(&[
            ("d0", "front_matter", 0.99),
            ("d1", "nav_toc", 0.40),
            ("d2", "not_a_role", 0.99),
            ("d3", "continuation", 0.95),
        ]);
        let h = handle(b, true);
        let out = classify_spine(Some(&h), &spine()).await.unwrap();
        assert_eq!(out.roles[0], Some(DocRole::FrontMatter));
        assert_eq!(out.roles[1], None, "below the confidence threshold");
        assert_eq!(out.roles[2], None, "an option that is not a role");
        assert_eq!(out.roles[3], Some(DocRole::Continuation));
    }

    #[tokio::test]
    async fn a_dead_backend_leaves_the_heuristics_in_charge() {
        let h = handle(FakeBackend::broken(), true);
        assert!(classify_spine(Some(&h), &spine()).await.is_none());
    }

    #[tokio::test]
    async fn feature_off_or_absent_never_classifies() {
        let b = FakeBackend::new(&[("d0", "front_matter", 0.99)]);
        let h = handle(b, false);
        assert!(classify_spine(Some(&h), &spine()).await.is_none());
        assert!(classify_spine(None, &spine()).await.is_none());
    }

    /// The table must survive a long book by shrinking excerpts, not by
    /// dropping documents — a missing row is a document that cannot be judged.
    #[tokio::test]
    async fn a_long_spine_shrinks_excerpts_to_fit() {
        let docs: Vec<DocInput> = (0..MAX_DOCS)
            .map(|i| doc(&format!("xhtml/p-{i:03}.xhtml"), &"本文。".repeat(200), None))
            .collect();
        let state = build_state(&docs).expect("a full-length spine must still fit");
        assert_eq!(state["spine"].as_array().unwrap().len(), MAX_DOCS);
        assert!(state.to_string().chars().count() <= MAX_STATE_CHARS);
    }

    #[tokio::test]
    async fn an_implausibly_long_spine_defers() {
        let docs: Vec<DocInput> = (0..MAX_DOCS + 1)
            .map(|i| doc(&format!("xhtml/p-{i:03}.xhtml"), "本文。", None))
            .collect();
        let b = FakeBackend::new(&[("d0", "front_matter", 0.99)]);
        let h = handle(b, true);
        assert!(classify_spine(Some(&h), &docs).await.is_none());
    }
}
