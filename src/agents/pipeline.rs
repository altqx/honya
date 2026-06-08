//! src/agents/pipeline.rs — drive the full per-chapter / per-chunk state machine
//! and emit the `AppEvent` stream the UI renders.
//!
//! Flow per chapter (verbatim from the pipeline design):
//!   * ImageOnly chapter → `write_image_only`, skip the agents, `ChapterCompleted`.
//!   * Otherwise: chunk the raw markdown (`ChapterChunked`), then for each chunk
//!     translate → audit → review up to `cfg.max_attempts`. On approve we DETERMINISTICALLY
//!     append the Thai (`workspace::translation::append_chunk`, NOT via an LLM
//!     tool), emit `ChunkCommitted`, then run the Orchestrator metadata turn so
//!     discoveries land in CHARACTERS.md / GLOSSARY.md / VOLUME.md. On exhausting
//!     retries the chunk is committed with a review-needed marker.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use crate::agents::audit::{audit_translation_with_terms, strip_copied_continuity};
use crate::agents::chunk::{Chunk, chunk_chapter};
use crate::agents::continuity;
use crate::agents::prompts::{ORCHESTRATOR_SYSTEM, build_orchestrator_metadata_msg};
use crate::agents::reviewer::review_chunk;
use crate::agents::tools::{WorkspaceTools, orchestrator_tools};
use crate::agents::translator::translate_chunk_streaming;
use crate::cleanse;
use crate::llm::client::LlmClient;
use crate::llm::tool_loop::run_tool_loop;
use crate::llm::{ChatRequest, Message, Tool, Usage};
use crate::model::{
    AgentRole, AppConfig, AppEvent, ChapterStatus, ChunkState, EventTx, GlossaryTerm, LogLevel,
    ModelSet, ReviewVerdict, ReviewerOut, TokenUsage, TranslatorOut, UsageStats,
};
use crate::workspace::{Workspace, characters, data_block, glossary, translation, volume};

/// Shared, cheap-to-clone run control toggled by the UI (p pause / s stop) and
/// polled by the pipeline between chunks. 0 = running, 1 = paused, 2 = stopped.
#[derive(Clone)]
pub struct RunControl(Arc<AtomicU8>);

impl RunControl {
    pub fn new() -> Self {
        Self(Arc::new(AtomicU8::new(0)))
    }
    pub fn stop(&self) {
        self.0.store(2, Ordering::Relaxed);
    }
    /// Pause↔resume toggle (no effect once stopped).
    pub fn toggle_pause(&self) {
        let _ = self
            .0
            .compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed)
            .or_else(|_| {
                self.0
                    .compare_exchange(1, 0, Ordering::Relaxed, Ordering::Relaxed)
            });
    }
    pub fn is_paused(&self) -> bool {
        self.0.load(Ordering::Relaxed) == 1
    }
    pub fn is_stopped(&self) -> bool {
        self.0.load(Ordering::Relaxed) == 2
    }
}

impl Default for RunControl {
    fn default() -> Self {
        Self::new()
    }
}

/// How a chapter finished: ran to completion, completed with ≥1 chunk committed
/// unreviewed (flagged for a human), or the user stopped the run.
enum Outcome {
    Completed,
    NeedsReview,
    Stopped,
}

/// How a single chunk resolved: committed after approval, or committed unreviewed
/// after exhausting its review attempts (the resilient path).
enum ChunkOutcome {
    Committed,
    NeedsReview,
}

/// Everything one pipeline run needs: the shared LLM client, the project
/// workspace, the model set, the runtime config, the UI event channel, and the
/// shared pause/stop control.
pub struct PipelineCtx {
    pub client: Arc<dyn LlmClient>,
    pub ws: Workspace,
    pub models: ModelSet,
    pub cfg: AppConfig,
    pub tx: EventTx,
    pub ctl: RunControl,
}

impl PipelineCtx {
    /// Derive the 1-based volume number from the workspace's `Vol_NN` directory
    /// name so the Orchestrator tool executor can rebuild a fresh `Workspace`.
    fn vol_number(&self) -> u32 {
        self.ws
            .vol_dir
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(|name| name.strip_prefix("Vol_"))
            .and_then(|digits| digits.trim_start_matches('0').parse::<u32>().ok())
            .unwrap_or(1)
    }
}

/// Run the pipeline across `chapters` (in the given order), emitting the full
/// `AppEvent` sequence. A per-chapter failure is reported as `ChapterFailed`
/// but does NOT abort the whole run; `PipelineFinished` always fires at the end.
pub async fn run_pipeline(ctx: PipelineCtx, chapters: Vec<u32>) -> anyhow::Result<()> {
    let mut done = 0u32;
    let mut failed = 0u32;
    let mut need_review = 0u32;
    let mut stopped = false;
    let mut acc = Acc::default();

    for chapter in chapters {
        if ctx.ctl.is_stopped() {
            stopped = true;
            ctx.tx.send(AppEvent::Log {
                level: LogLevel::Warn,
                msg: "run stopped before chapter".to_string(),
            });
            break;
        }
        ctx.tx.send(AppEvent::ChapterStarted { chapter });
        ctx.tx.send(AppEvent::ChapterStateChanged {
            chapter,
            state: ChapterStatus::Chunking,
        });

        let outcome = process_chapter(&ctx, chapter, &mut acc).await;

        // Persist this chapter's spend to VOLUME.md (cumulative lifetime accounting)
        // however it ended, then reset the per-chapter sub-total for the next one.
        if !acc.chapter.is_zero() {
            if let Err(e) = volume::add_chapter_usage(&ctx.ws, chapter, &acc.chapter) {
                ctx.tx.send(AppEvent::Log {
                    level: LogLevel::Warn,
                    msg: format!("could not persist usage for chapter {chapter}: {e}"),
                });
            }
            ctx.tx.send(AppEvent::ChapterUsage {
                chapter,
                delta: acc.chapter,
            });
        }
        acc.chapter = UsageStats::default();

        match outcome {
            Ok(Outcome::Completed) => {
                done += 1;
                ctx.tx.send(AppEvent::ChapterStateChanged {
                    chapter,
                    state: ChapterStatus::Done,
                });
                ctx.tx.send(AppEvent::ChapterCompleted { chapter });
            }
            Ok(Outcome::NeedsReview) => {
                // The chapter is fully written, but ≥1 chunk was committed without
                // passing review. It "completed" (counts toward `done`) yet stays
                // flagged `NeedsReview` instead of `Done` so a human can fix it.
                done += 1;
                need_review += 1;
                ctx.tx.send(AppEvent::ChapterStateChanged {
                    chapter,
                    state: ChapterStatus::NeedsReview,
                });
                ctx.tx.send(AppEvent::Log {
                    level: LogLevel::Warn,
                    msg: format!("chapter {chapter} completed with chunk(s) needing manual review"),
                });
            }
            Ok(Outcome::Stopped) => {
                stopped = true;
                ctx.tx.send(AppEvent::Log {
                    level: LogLevel::Warn,
                    msg: format!("run stopped during chapter {chapter}"),
                });
                break;
            }
            Err(e) => {
                failed += 1;
                let reason = e.to_string();
                ctx.tx.send(AppEvent::ChapterStateChanged {
                    chapter,
                    state: ChapterStatus::Failed,
                });
                ctx.tx.send(AppEvent::ChapterFailed {
                    chapter,
                    reason: reason.clone(),
                });
                ctx.tx.send(AppEvent::Error {
                    context: format!("chapter {chapter}"),
                    msg: reason,
                });
            }
        }
    }

    ctx.tx.send(AppEvent::PipelineFinished {
        chapters_done: done,
        chapters_failed: failed,
        chapters_need_review: need_review,
        stopped,
        run: acc.run,
    });
    Ok(())
}

/// Process one chapter end to end. Image-only chapters short-circuit (the image
/// markdown is copied straight to `translated/`); prose chapters are chunked and
/// each chunk is translated + reviewed + committed.
async fn process_chapter(
    ctx: &PipelineCtx,
    chapter: u32,
    acc: &mut Acc,
) -> anyhow::Result<Outcome> {
    let raw_path = ctx.ws.raw(chapter);
    let raw = tokio::fs::read_to_string(&raw_path)
        .await
        .map_err(|e| anyhow::anyhow!("read {}: {e}", raw_path.display()))?;
    if raw.trim().is_empty() {
        anyhow::bail!("chapter {chapter} has no raw source");
    }

    // Image-only chapters skip the agents entirely.
    if cleanse::is_image_only(&raw) {
        translation::write_image_only(&ctx.ws, chapter, &raw)?;
        ctx.tx.send(AppEvent::Log {
            level: LogLevel::Info,
            msg: format!("chapter {chapter}: image-only, copied straight to translated/"),
        });
        return Ok(Outcome::Completed);
    }

    let chunks = chunk_chapter(
        &raw,
        ctx.cfg.chunk_target_tokens,
        ctx.cfg.chunk_hard_cap_tokens,
    );
    if chunks.is_empty() {
        // No translatable prose after chunking — treat as image-only passthrough.
        translation::write_image_only(&ctx.ws, chapter, &raw)?;
        return Ok(Outcome::Completed);
    }

    let est_total: usize = chunks.iter().map(|c| c.est_tokens).sum();
    ctx.tx.send(AppEvent::ChapterChunked {
        chapter,
        total_chunks: chunks.len(),
        est_tokens_total: est_total,
    });

    let total = chunks.len();

    // Resume support: translated files are append-only, chunk-marked logs. If a
    // previous run failed after committing chunk N, a re-run should start at the
    // next missing marker instead of re-spending tokens on chunks already on disk.
    let existing_translation = translation::read_translated(&ctx.ws, chapter).await;
    let committed = translation::committed_chunk_indices_in(&existing_translation);
    let needs_review = translation::review_needed_chunk_indices_in(&existing_translation);
    let clean_committed: std::collections::BTreeSet<u32> =
        committed.difference(&needs_review).copied().collect();
    let skipped = chunks
        .iter()
        .filter(|chunk| clean_committed.contains(&(chunk.index as u32)))
        .count();
    if skipped > 0 {
        ctx.tx.send(AppEvent::Log {
            level: LogLevel::Info,
            msg: format!(
                "chapter {chapter}: resuming from translated file · skipping {skipped}/{total} committed chunk(s)"
            ),
        });
    }

    if !needs_review.is_empty() {
        ctx.tx.send(AppEvent::Log {
            level: LogLevel::Info,
            msg: format!(
                "chapter {chapter}: rechecking {} review-needed chunk(s)",
                needs_review.len()
            ),
        });
    }

    for chunk in &chunks {
        if clean_committed.contains(&(chunk.index as u32)) {
            continue;
        }

        // Honor pause/stop between chunks ("current chunk finishes, then halts").
        if !gate(ctx, chapter).await {
            return Ok(Outcome::Stopped);
        }
        ctx.tx.send(AppEvent::ChunkStarted {
            chapter,
            chunk: chunk.index,
            total,
            est_tokens: chunk.est_tokens,
        });
        match process_chunk(ctx, chapter, chunk, acc).await? {
            ChunkOutcome::Committed | ChunkOutcome::NeedsReview => {}
        }
    }

    let any_needs_review = translation::read_translated(&ctx.ws, chapter)
        .await
        .contains(translation::REVIEW_NEEDED_MARKER);

    // All chunks are written either way; the run loop maps the outcome to the
    // chapter's final status (Done vs NeedsReview).
    ctx.tx.send(AppEvent::ChapterStateChanged {
        chapter,
        state: ChapterStatus::Appended,
    });
    if any_needs_review {
        Ok(Outcome::NeedsReview)
    } else {
        Ok(Outcome::Completed)
    }
}

/// Block while paused; return `false` if the run is (or becomes) stopped so the
/// caller aborts. Emits `PipelinePaused`/`PipelineResumed` and flips the active
/// chapter to `Paused` so the UI reflects the held state.
async fn gate(ctx: &PipelineCtx, chapter: u32) -> bool {
    if ctx.ctl.is_stopped() {
        return false;
    }
    if ctx.ctl.is_paused() {
        ctx.tx.send(AppEvent::PipelinePaused);
        ctx.tx.send(AppEvent::ChapterStateChanged {
            chapter,
            state: ChapterStatus::Paused,
        });
        while ctx.ctl.is_paused() {
            tokio::time::sleep(Duration::from_millis(120)).await;
        }
        if ctx.ctl.is_stopped() {
            return false;
        }
        ctx.tx.send(AppEvent::PipelineResumed);
    }
    true
}

const MAX_GLOSSARY_IN_CTX: usize = 80;
const MAX_CHARACTERS_IN_CTX: usize = 40;
const MAX_PROTECTED_TERMS_FOR_ORCH: usize = 40;

fn glossary_terms_for_chunk(ws: &Workspace, chunk_text: &str, max: usize) -> Vec<GlossaryTerm> {
    let mut terms = glossary::load(ws);
    // Keep only terms the chunk actually uses, so the injected glossary tracks
    // the chunk rather than the whole, ever-growing volume.
    terms.retain(|t| {
        let jp = t.jp_term.trim();
        !jp.is_empty() && chunk_text.contains(jp)
    });
    terms.truncate(max);
    terms
}

/// Assemble the reference context bundled into every Translator/Reviewer call:
/// the scoped terminology policies, the character roster (pronouns/register), and the
/// PROJECT/STYLE prose — each in its own clearly-delimited section. Re-read per
/// chunk so mid-chapter glossary/character additions take effect immediately.
fn build_reference_ctx(ws: &Workspace, chunk_text: &str) -> String {
    fn section(out: &mut String, open: &str, body: &str, close: &str) {
        let b = body.trim();
        if !b.is_empty() {
            out.push_str(open);
            out.push('\n');
            out.push_str(b);
            out.push('\n');
            out.push_str(close);
            out.push_str("\n\n");
        }
    }
    fn excerpt(s: String, max: usize) -> String {
        let t = s.trim();
        if t.chars().count() <= max {
            t.to_string()
        } else {
            t.chars().take(max).collect::<String>() + "…"
        }
    }

    let mut s = String::new();
    let terms = glossary_terms_for_chunk(ws, chunk_text, MAX_GLOSSARY_IN_CTX);
    section(
        &mut s,
        "<<GLOSSARY: นโยบายคำศัพท์ (hard lock / preferred / forbidden / context)>>",
        &glossary::render_context_blurb(&terms),
        "<<END_GLOSSARY>>",
    );
    let mut chars = characters::load(ws);
    chars.retain(|c| {
        let jp = c.jp_name.trim();
        let by_name = !jp.is_empty() && chunk_text.contains(jp);
        // Match alias forms too, so a chunk using a bare given name still pulls in
        // the one canonical entry instead of looking like an unknown character.
        let by_alias = c
            .aliases
            .iter()
            .any(|a| !a.trim().is_empty() && chunk_text.contains(a.trim()));
        by_name || by_alias
    });
    chars.truncate(MAX_CHARACTERS_IN_CTX);
    section(
        &mut s,
        "<<CHARACTERS: สรรพนาม/น้ำเสียงที่กำหนด>>",
        &characters::render_context_blurb(&chars),
        "<<END_CHARACTERS>>",
    );
    section(
        &mut s,
        "<<VOLUME_SYNOPSIS: เรื่องย่อของเล่มนี้ ใช้เป็นบริบทภาพรวม>>",
        &excerpt(volume::load(ws).synopsis_th, 1200),
        "<<END_VOLUME_SYNOPSIS>>",
    );
    section(
        &mut s,
        "<<PROJECT: บริบท/โครงเรื่องโดยรวม>>",
        &excerpt(data_block::read_body(&ws.project_md()), 1400),
        "<<END_PROJECT>>",
    );
    section(
        &mut s,
        "<<STYLE: แนวทางโทน/สำนวน>>",
        &excerpt(data_block::read_body(&ws.style_md()), 1400),
        "<<END_STYLE>>",
    );
    s
}

/// Convert API token `Usage` into the UI's `TokenUsage`. Falls back to
/// `prompt + completion` when a provider omits `total_tokens` (some BYOK
/// providers do) so the running total never silently stalls.
fn to_tokens(u: &Usage) -> TokenUsage {
    let total = if u.total_tokens != 0 {
        u.total_tokens
    } else {
        u.prompt_tokens.saturating_add(u.completion_tokens)
    };
    TokenUsage {
        prompt: u.prompt_tokens,
        completion: u.completion_tokens,
        total,
    }
}

fn effective_feedback_text(audit_findings: &[String], review: &ReviewerOut) -> String {
    let mut feedback = Vec::new();
    feedback.extend(
        audit_findings
            .iter()
            .map(|f| format!("Local audit: {}", f.trim()))
            .filter(|f| !f.trim().is_empty()),
    );
    let reviewer_feedback = review.feedback_text();
    if !reviewer_feedback.trim().is_empty() {
        feedback.push(reviewer_feedback);
    }
    feedback.join("; ")
}

/// Build a `UsageStats` from one API call's token + BYOK-aware cost usage.
fn stats_from_usage(u: &Usage) -> UsageStats {
    UsageStats {
        tokens: to_tokens(u),
        cost_usd: u.cost_usd(),
        tool_calls: 0,
    }
}

/// The two running totals one pipeline run maintains in parallel: `run` spans the
/// whole run (drives the run meter), `chapter` resets at each chapter boundary
/// (drives the chapter meter and the persisted per-chapter total).
#[derive(Default)]
struct Acc {
    run: UsageStats,
    chapter: UsageStats,
}

impl Acc {
    /// Fold one API call's token + cost usage into both totals.
    fn fold(&mut self, u: &Usage) {
        let s = stats_from_usage(u);
        self.run.add(&s);
        self.chapter.add(&s);
    }

    /// Fold `n` Orchestrator tool calls into both totals.
    fn add_tool_calls(&mut self, n: u32) {
        self.run.tool_calls = self.run.tool_calls.saturating_add(n);
        self.chapter.tool_calls = self.chapter.tool_calls.saturating_add(n);
    }
}

/// Translate → review one chunk, retrying up to `cfg.max_attempts`. On approval
/// the Thai is deterministically appended and the Orchestrator metadata turn
/// runs (`ChunkOutcome::Committed`). Exhausting the attempts no longer fails the
/// chapter: the last attempt is committed unreviewed and flagged in-file
/// (`ChunkOutcome::NeedsReview`). A hard Translator/Reviewer error is treated the
/// same way — retried while attempts remain, then, if a translation already
/// exists, committed flagged `NeedsReview` rather than failing the whole chapter.
/// Only a Translator that never produces *any* translation bails.
async fn process_chunk(
    ctx: &PipelineCtx,
    chapter: u32,
    chunk: &Chunk,
    acc: &mut Acc,
) -> anyhow::Result<ChunkOutcome> {
    ctx.tx.send(AppEvent::ChunkStateChanged {
        chapter,
        chunk: chunk.index,
        state: ChunkState::Queued,
    });

    // Reference context (glossary + characters + project + style) and the
    // continuity tail are stable across this chunk's attempts.
    let reference_ctx = build_reference_ctx(&ctx.ws, &chunk.text);
    let mut prev_thai =
        continuity::last_thai_sentences(&ctx.ws, chapter, ctx.cfg.continuity_sentences).await;
    // Seed chunk 0 from the previous chapter's tail when this chapter has no
    // committed Thai yet (continuity across chapter boundaries).
    if prev_thai.is_empty() && chunk.index == 0 && chapter > 1 {
        prev_thai =
            continuity::last_thai_sentences(&ctx.ws, chapter - 1, ctx.cfg.continuity_sentences)
                .await;
    }

    let max = ctx.cfg.max_attempts.max(1);
    let mut feedback: Option<String> = None;
    // Best translation produced so far. A transient hard error from the Reviewer
    // (or a later Translator attempt) must not throw away a translation we already
    // have: we fall back to committing it flagged NeedsReview instead of failing
    // the whole chapter on one chunk.
    let mut candidate: Option<String> = None;

    for attempt in 1..=max {
        // ---- Translator ----
        ctx.tx.send(AppEvent::ChunkStateChanged {
            chapter,
            chunk: chunk.index,
            state: ChunkState::Translating,
        });
        ctx.tx.send(AppEvent::ChapterStateChanged {
            chapter,
            state: ChapterStatus::Translating,
        });
        ctx.tx.send(AppEvent::TranslatorRequested {
            chapter,
            chunk: chunk.index,
            attempt,
        });

        let tx = ctx.tx.clone();
        let (out, t_usage, streamed_preview): (TranslatorOut, Usage, bool) =
            match translate_chunk_streaming(
                ctx.client.as_ref(),
                &ctx.models.translator,
                &reference_ctx,
                &prev_thai,
                &chunk.text,
                feedback.as_deref(),
                move |delta| {
                    tx.send(AppEvent::StreamDelta {
                        chapter,
                        chunk: chunk.index,
                        role: AgentRole::Translator,
                        delta: delta.to_string(),
                    });
                },
            )
            .await
            {
                Ok(o) => o,
                Err(e) => {
                    // A transient Translator failure shouldn't sink the chapter.
                    // Retry while attempts remain; on the final attempt fall back
                    // to an earlier good translation (if any) rather than failing.
                    ctx.tx.send(AppEvent::Error {
                        context: format!("translator ch{chapter} chunk{}", chunk.index),
                        msg: e.to_string(),
                    });
                    if attempt < max {
                        emit_attempt_failed_retry(
                            ctx,
                            chapter,
                            chunk,
                            attempt,
                            max,
                            &format!("translator error, retrying: {e}"),
                        );
                        continue;
                    }
                    match candidate {
                        Some(thai) => {
                            return commit_chunk_needs_review(
                                ctx,
                                chapter,
                                chunk,
                                &thai,
                                attempt,
                                format!("translator failed on the final attempt: {e}"),
                            )
                            .await;
                        }
                        None => anyhow::bail!(
                            "translator failed on chunk {} with no translation to keep: {e}",
                            chunk.index
                        ),
                    }
                }
            };

        // Deterministically drop any continuity tail the Translator echoed back
        // before it reaches the audit/Reviewer/append — a disobedient copy costs
        // no retry this way (matches the app-side, "everything deterministic but
        // the metadata turn" append rule).
        let thai = strip_copied_continuity(&prev_thai, &out.translated_text);
        candidate = Some(thai.clone());
        let tok = to_tokens(&t_usage);
        acc.fold(&t_usage);
        ctx.tx.send(AppEvent::TranslatorReturned {
            chapter,
            chunk: chunk.index,
            attempt,
            // If the streaming path emitted translated_text deltas, avoid
            // appending the same chunk again when the final schema lands.
            thai_preview: if streamed_preview {
                String::new()
            } else {
                thai.clone()
            },
            tokens: tok,
        });
        ctx.tx.send(AppEvent::UsageUpdate {
            run: acc.run,
            chapter: acc.chapter,
        });

        // ---- Deterministic audit + Reviewer ----
        let audit_terms = glossary_terms_for_chunk(&ctx.ws, &chunk.text, MAX_GLOSSARY_IN_CTX);
        let audit_findings =
            audit_translation_with_terms(&chunk.text, &thai, &prev_thai, &audit_terms);
        ctx.tx.send(AppEvent::ChunkStateChanged {
            chapter,
            chunk: chunk.index,
            state: ChunkState::Reviewing,
        });
        ctx.tx.send(AppEvent::ChapterStateChanged {
            chapter,
            state: ChapterStatus::Reviewing,
        });
        ctx.tx.send(AppEvent::ReviewerRequested {
            chapter,
            chunk: chunk.index,
            attempt,
        });

        let (review, r_usage) = match review_chunk(
            ctx.client.as_ref(),
            &ctx.models.reviewer,
            &chunk.text,
            &thai,
            &reference_ctx,
            &audit_findings,
            &prev_thai,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                // The Reviewer couldn't return a verdict. Don't discard the
                // translation we have: retry while attempts remain, otherwise
                // commit it flagged NeedsReview instead of failing the chapter.
                ctx.tx.send(AppEvent::Error {
                    context: format!("reviewer ch{chapter} chunk{}", chunk.index),
                    msg: e.to_string(),
                });
                if attempt < max {
                    emit_attempt_failed_retry(
                        ctx,
                        chapter,
                        chunk,
                        attempt,
                        max,
                        &format!("reviewer error, retrying: {e}"),
                    );
                    continue;
                }
                return commit_chunk_needs_review(
                    ctx,
                    chapter,
                    chunk,
                    &thai,
                    attempt,
                    format!("reviewer unavailable; committed without review: {e}"),
                )
                .await;
            }
        };
        acc.fold(&r_usage);
        ctx.tx.send(AppEvent::UsageUpdate {
            run: acc.run,
            chapter: acc.chapter,
        });

        let approved = review.approved() && audit_findings.is_empty();
        let fb_text = effective_feedback_text(&audit_findings, &review);
        ctx.tx.send(AppEvent::ReviewerReturned {
            chapter,
            chunk: chunk.index,
            attempt,
            verdict: if approved {
                ReviewVerdict::Approve
            } else {
                ReviewVerdict::Reject
            },
            feedback: if fb_text.is_empty() {
                None
            } else {
                Some(fb_text.clone())
            },
        });

        if approved {
            // ---- Approved: deterministic append (app-side, NOT via LLM tool) ----
            ctx.tx.send(AppEvent::ChunkStateChanged {
                chapter,
                chunk: chunk.index,
                state: ChunkState::Approved,
            });

            let bytes = translation::append_chunk(&ctx.ws, chapter, chunk.index as u32, &thai)
                .await
                .map_err(|e| anyhow::anyhow!("append chunk {} failed: {e}", chunk.index))?;

            ctx.tx.send(AppEvent::ChunkCommitted {
                chapter,
                chunk: chunk.index,
                bytes_written: bytes,
            });
            ctx.tx.send(AppEvent::ChunkStateChanged {
                chapter,
                chunk: chunk.index,
                state: ChunkState::Committed,
            });

            // ---- Orchestrator metadata turn (everything-uses-tools) ----
            match run_orchestrator_metadata_turn(ctx, chapter, &out).await {
                Ok((o_usage, n_tool_calls)) => {
                    acc.fold(&o_usage);
                    acc.add_tool_calls(n_tool_calls as u32);
                    ctx.tx.send(AppEvent::UsageUpdate {
                        run: acc.run,
                        chapter: acc.chapter,
                    });
                }
                // Metadata persistence is best-effort; never fail the chunk on it.
                Err(e) => {
                    ctx.tx.send(AppEvent::Error {
                        context: format!("orchestrator ch{chapter} chunk{}", chunk.index),
                        msg: e.to_string(),
                    });
                }
            }

            return Ok(ChunkOutcome::Committed);
        }

        // ---- Rejected ----
        if attempt < max {
            emit_attempt_failed_retry(ctx, chapter, chunk, attempt, max, &fb_text);
            feedback = Some(fb_text);
        } else {
            // ---- Retries exhausted: commit the last attempt unreviewed ----
            let reason = if fb_text.is_empty() {
                "reviewer rejected after max attempts".to_string()
            } else {
                fb_text
            };
            return commit_chunk_needs_review(ctx, chapter, chunk, &thai, max, reason).await;
        }
    }

    // Unreachable: the loop returns on approve, on the final rejection, and on a
    // terminal Translator/Reviewer error.
    anyhow::bail!(
        "chunk {} exhausted attempts without resolution",
        chunk.index
    )
}

/// Emit the per-attempt "rejected, will retry" event pair the UI renders when an
/// attempt fails — either a reviewer rejection or a transient hard error — and at
/// least one more attempt remains.
fn emit_attempt_failed_retry(
    ctx: &PipelineCtx,
    chapter: u32,
    chunk: &Chunk,
    attempt: u32,
    max: u32,
    feedback: &str,
) {
    ctx.tx.send(AppEvent::ChunkStateChanged {
        chapter,
        chunk: chunk.index,
        state: ChunkState::Rejected,
    });
    ctx.tx.send(AppEvent::ChunkRetry {
        chapter,
        chunk: chunk.index,
        attempt,
        max,
        feedback: feedback.to_string(),
    });
}

/// Commit a chunk's best-available translation flagged for manual review, emitting
/// the same event sequence whether we got here by exhausting review rejections or
/// by hitting a transient Translator/Reviewer error we couldn't recover from. The
/// `[REVIEW NEEDED]` banner lets a human find and fix this one spot later; the
/// Orchestrator metadata turn is deliberately SKIPPED so an unreviewed translation
/// can't pollute the glossary/character roster.
async fn commit_chunk_needs_review(
    ctx: &PipelineCtx,
    chapter: u32,
    chunk: &Chunk,
    thai: &str,
    attempts: u32,
    reason: String,
) -> anyhow::Result<ChunkOutcome> {
    let bytes = translation::append_chunk_needs_review(
        &ctx.ws,
        chapter,
        chunk.index as u32,
        thai,
        attempts,
        &reason,
    )
    .await
    .map_err(|e| anyhow::anyhow!("append needs-review chunk {} failed: {e}", chunk.index))?;

    ctx.tx.send(AppEvent::ChunkStateChanged {
        chapter,
        chunk: chunk.index,
        state: ChunkState::NeedsReview,
    });
    // Count it as committed (it IS on disk) so the chapter's chunk progress reads
    // as fully written.
    ctx.tx.send(AppEvent::ChunkCommitted {
        chapter,
        chunk: chunk.index,
        bytes_written: bytes,
    });
    ctx.tx.send(AppEvent::ChunkNeedsReview {
        chapter,
        chunk: chunk.index,
        attempts,
        reason,
    });

    Ok(ChunkOutcome::NeedsReview)
}

fn controlled_terms_for_orchestrator(ws: &Workspace, out: &TranslatorOut) -> Vec<GlossaryTerm> {
    if out.new_terms.is_empty() {
        return Vec::new();
    }

    let mut terms: Vec<GlossaryTerm> = glossary::load(ws)
        .into_iter()
        .filter(glossary::blocks_automatic_update)
        .collect();

    // Prioritize controlled terms that resemble this chunk's reported discoveries,
    // then include a bounded fallback list so the Orchestrator can still reason
    // about nearby terminology without ballooning the prompt.
    terms.sort_by_key(|t| !controlled_term_matches_discovery(t, out));
    terms.truncate(MAX_PROTECTED_TERMS_FOR_ORCH);
    terms
}

fn controlled_term_matches_discovery(term: &GlossaryTerm, out: &TranslatorOut) -> bool {
    let jp = term.jp_term.trim();
    let th = term.thai_term.trim();
    out.new_terms.iter().any(|new| {
        let new_jp = new.jp_term.trim();
        let new_th = new.thai_term.trim();
        (!jp.is_empty() && !new_jp.is_empty() && (jp.contains(new_jp) || new_jp.contains(jp)))
            || (!th.is_empty()
                && !new_th.is_empty()
                && (th.contains(new_th) || new_th.contains(th)))
    })
}

/// Run the Orchestrator metadata turn for a just-approved chunk: a single tool
/// loop that lets the Orchestrator persist new characters / terms / continuity
/// notes and advance the volume recap through the backend tools.
async fn run_orchestrator_metadata_turn(
    ctx: &PipelineCtx,
    chapter: u32,
    out: &TranslatorOut,
) -> anyhow::Result<(Usage, usize)> {
    let controlled_terms = controlled_terms_for_orchestrator(&ctx.ws, out);
    let user = build_orchestrator_metadata_msg(chapter, out, &controlled_terms);

    let tools: Vec<Tool> = serde_json::from_value(orchestrator_tools())
        .map_err(|e| anyhow::anyhow!("failed to build orchestrator tools: {e}"))?;

    // tools present + tool_choice unset => OpenRouter defaults to "auto".
    // Leaving tool_choice at its Default avoids coupling to its exact field type.
    let req = ChatRequest {
        model: ctx.models.orchestrator.clone(),
        messages: vec![Message::system(ORCHESTRATOR_SYSTEM), Message::user(user)],
        temperature: Some(0.2),
        tools: Some(tools),
        ..ChatRequest::default()
    };

    let executor = WorkspaceTools::new(
        ctx.ws.root.clone(),
        ctx.vol_number(),
        ctx.tx.clone(),
        chapter,
    );

    let outcome = run_tool_loop(ctx.client.as_ref(), req, &executor, 8)
        .await
        .map_err(|e| anyhow::anyhow!("orchestrator tool loop failed: {e}"))?;

    Ok((outcome.usage, outcome.tool_calls))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Character, GlossaryTerm};

    fn temp_ws(tag: &str) -> (std::path::PathBuf, Workspace) {
        let base = std::env::temp_dir().join(format!("honya_ctx_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let ws = Workspace::new(base.clone(), 1);
        (base, ws)
    }

    fn term(jp: &str, th: &str) -> GlossaryTerm {
        GlossaryTerm {
            jp_term: jp.into(),
            thai_term: th.into(),
            romaji: None,
            category: None,
            gloss: None,
            policy: None,
            forbidden_thai: Vec::new(),
            context_rule: None,
            protected: None,
            do_not_translate: None,
            first_seen_chapter: None,
        }
    }

    #[derive(Default)]
    struct CountingClient {
        schemas: std::sync::Mutex<Vec<Option<String>>>,
    }

    impl CountingClient {
        fn schema_calls(&self, name: &str) -> usize {
            self.schemas
                .lock()
                .unwrap()
                .iter()
                .filter(|schema| schema.as_deref() == Some(name))
                .count()
        }
    }

    struct AuditRetryClient {
        schemas: std::sync::Mutex<Vec<Option<String>>>,
        translations: std::sync::Mutex<Vec<String>>,
    }

    impl AuditRetryClient {
        fn new(translations: Vec<&str>) -> Self {
            Self {
                schemas: std::sync::Mutex::new(Vec::new()),
                translations: std::sync::Mutex::new(
                    translations.into_iter().map(str::to_string).collect(),
                ),
            }
        }

        fn schema_calls(&self, name: &str) -> usize {
            self.schemas
                .lock()
                .unwrap()
                .iter()
                .filter(|schema| schema.as_deref() == Some(name))
                .count()
        }
    }

    #[async_trait::async_trait]
    impl crate::llm::client::LlmClient for AuditRetryClient {
        async fn chat(
            &self,
            req: &crate::llm::ChatRequest,
        ) -> crate::llm::client::Result<crate::llm::ChatResponse> {
            let schema_name = match &req.response_format {
                Some(crate::llm::ResponseFormat::JsonSchema { json_schema }) => {
                    Some(json_schema.name.clone())
                }
                _ => None,
            };
            self.schemas.lock().unwrap().push(schema_name.clone());

            let content = match schema_name.as_deref() {
                Some("translation_result") => {
                    let next = self.translations.lock().unwrap().remove(0);
                    serde_json::json!({
                        "thought_process": {
                            "scene_analysis": "(test)",
                            "glossary_check": "(test)"
                        },
                        "translated_text": next,
                        "new_characters": [],
                        "new_terms": [],
                        "continuity_notes": []
                    })
                    .to_string()
                }
                Some("review_result") => serde_json::json!({
                    "status": "approve",
                    "feedback": []
                })
                .to_string(),
                _ => "(test orchestrator: no tools)".to_string(),
            };

            Ok(crate::llm::ChatResponse {
                id: Some("audit-retry-client".to_string()),
                model: Some("honya/test".to_string()),
                choices: vec![crate::llm::Choice {
                    index: 0,
                    message: crate::llm::ResponseMessage {
                        role: Some("assistant".to_string()),
                        content: Some(content),
                        tool_calls: None,
                    },
                    finish_reason: Some("stop".to_string()),
                }],
                usage: Some(crate::llm::Usage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                    cost: 0.0,
                    cost_details: None,
                }),
            })
        }
    }

    #[async_trait::async_trait]
    impl crate::llm::client::LlmClient for CountingClient {
        async fn chat(
            &self,
            req: &crate::llm::ChatRequest,
        ) -> crate::llm::client::Result<crate::llm::ChatResponse> {
            let schema_name = match &req.response_format {
                Some(crate::llm::ResponseFormat::JsonSchema { json_schema }) => {
                    Some(json_schema.name.clone())
                }
                _ => None,
            };
            self.schemas.lock().unwrap().push(schema_name.clone());

            let content = match schema_name.as_deref() {
                Some("translation_result") => serde_json::json!({
                    "thought_process": {
                        "scene_analysis": "(test)",
                        "glossary_check": "(test)"
                    },
                    "translated_text": "ข้อความแปลต่อ",
                    "new_characters": [],
                    "new_terms": [],
                    "continuity_notes": []
                })
                .to_string(),
                Some("review_result") => serde_json::json!({
                    "status": "approve",
                    "feedback": []
                })
                .to_string(),
                _ => "(test orchestrator: no tools)".to_string(),
            };

            Ok(crate::llm::ChatResponse {
                id: Some("counting-client".to_string()),
                model: Some("honya/test".to_string()),
                choices: vec![crate::llm::Choice {
                    index: 0,
                    message: crate::llm::ResponseMessage {
                        role: Some("assistant".to_string()),
                        content: Some(content),
                        tool_calls: None,
                    },
                    finish_reason: Some("stop".to_string()),
                }],
                usage: Some(crate::llm::Usage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                    cost: 0.0,
                    cost_details: None,
                }),
            })
        }
    }

    #[tokio::test]
    async fn local_audit_forces_retry_even_if_reviewer_approves() {
        let (base, ws) = temp_ws("audit_retry");
        let raw = "一文目。\n\n---\n\n二文目。";
        translation::write_raw(&ws, 1, raw).unwrap();

        let client = std::sync::Arc::new(AuditRetryClient::new(vec![
            "<div>一文目。</div>\n\n二文目。",
            "ประโยคแรก\n\n---\n\nประโยคที่สอง",
        ]));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = PipelineCtx {
            client: client.clone() as std::sync::Arc<dyn crate::llm::client::LlmClient>,
            ws: ws.clone(),
            models: crate::model::ModelSet::default(),
            cfg: crate::model::AppConfig {
                max_attempts: 2,
                ..crate::model::AppConfig::default()
            },
            tx: crate::model::EventTx(tx),
            ctl: RunControl::new(),
        };
        let mut acc = Acc::default();
        let chunk = Chunk {
            index: 0,
            text: raw.to_string(),
            est_tokens: 1,
        };

        match process_chunk(&ctx, 1, &chunk, &mut acc)
            .await
            .expect("process_chunk")
        {
            ChunkOutcome::Committed => {}
            ChunkOutcome::NeedsReview => panic!("clean retry should be approved"),
        }

        assert_eq!(
            client.schema_calls("translation_result"),
            2,
            "audit findings should route back to the Translator"
        );
        assert_eq!(
            client.schema_calls("review_result"),
            2,
            "both attempts still pass through the Reviewer"
        );

        let translated = translation::read_translated(&ws, 1).await;
        assert!(translated.contains("ประโยคแรก"));
        assert!(!translated.contains("<div>"));

        let mut saw_audit_feedback = false;
        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::ChunkRetry { feedback, .. } = ev
                && feedback.contains("Local audit")
            {
                saw_audit_feedback = true;
            }
        }
        assert!(
            saw_audit_feedback,
            "retry feedback should include local audit findings"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn pipeline_resumes_from_committed_chunk_markers() {
        let (base, ws) = temp_ws("resume");
        let raw =
            "# 第一章\n\n一文目。\n\n二文目。\n\n三文目。\n\n四文目。\n\n五文目。\n\n六文目。";
        let cfg = crate::model::AppConfig {
            chunk_target_tokens: 4,
            chunk_hard_cap_tokens: 8,
            ..crate::model::AppConfig::default()
        };
        let chunks = chunk_chapter(raw, cfg.chunk_target_tokens, cfg.chunk_hard_cap_tokens);
        assert!(
            chunks.len() >= 3,
            "test raw should create multiple chunks: {chunks:?}"
        );

        translation::write_raw(&ws, 1, raw).unwrap();
        translation::append_chunk(&ws, 1, 0, "ข้อความเดิม")
            .await
            .unwrap();
        translation::append_chunk_needs_review(&ws, 1, 1, "คำแปลที่ต้องตรวจ", 3, "still rough")
            .await
            .unwrap();

        let client = std::sync::Arc::new(CountingClient::default());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = PipelineCtx {
            client: client.clone() as std::sync::Arc<dyn crate::llm::client::LlmClient>,
            ws: ws.clone(),
            models: crate::model::ModelSet::default(),
            cfg,
            tx: crate::model::EventTx(tx),
            ctl: RunControl::new(),
        };

        run_pipeline(ctx, vec![1]).await.expect("run_pipeline");

        assert_eq!(
            client.schema_calls("translation_result"),
            chunks.len() - 1,
            "only the clean existing marker should be skipped; review-needed chunks rerun"
        );
        let translated = translation::read_translated(&ws, 1).await;
        assert!(translated.contains("ข้อความเดิม"));
        assert!(
            !translated.contains(translation::REVIEW_NEEDED_MARKER),
            "approved retranslation should remove stale review-needed markers"
        );
        assert!(translation::review_needed_chunk_indices_in(&translated).is_empty());
        let committed = translation::committed_chunk_indices_in(&translated);
        assert_eq!(
            committed.len(),
            chunks.len(),
            "all chunks should be present after resume"
        );

        let mut saw_resume_log = false;
        let mut saw_recheck_log = false;
        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::Log { msg, .. } = ev {
                if msg.contains("resuming from translated file") {
                    saw_resume_log = true;
                }
                if msg.contains("rechecking") {
                    saw_recheck_log = true;
                }
            }
        }
        assert!(saw_resume_log, "resume should be visible in the run log");
        assert!(
            saw_recheck_log,
            "review-needed chunks should be visibly rerun"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// The reference context injected per chunk must scope to terms/characters the
    /// chunk actually uses — otherwise it balloons with the whole accumulated
    /// roster as a volume progresses.
    #[test]
    fn reference_ctx_scopes_to_chunk() {
        let (base, ws) = temp_ws("ref");
        glossary::upsert(&ws, term("聖剣", "ดาบศักดิ์สิทธิ์")).unwrap();
        glossary::upsert(&ws, term("王都", "ราชธานี")).unwrap();
        characters::upsert(
            &ws,
            Character {
                id: "subaru".into(),
                jp_name: "スバル".into(),
                thai_name: "สบารุ".into(),
                romaji: None,
                gender: None,
                honorific: None,
                speech_style: None,
                relationships: Vec::new(),
                aliases: Vec::new(),
                notes: None,
                first_seen_chapter: None,
            },
        )
        .unwrap();

        // The chunk references 聖剣 and スバル, but never 王都.
        let ctx = build_reference_ctx(&ws, "スバルは聖剣を抜いた。");
        assert!(
            ctx.contains("聖剣"),
            "in-chunk term must be injected:\n{ctx}"
        );
        assert!(
            ctx.contains("スバル"),
            "in-chunk character must be injected"
        );
        assert!(
            !ctx.contains("王都") && !ctx.contains("ราชธานี"),
            "absent term must NOT balloon the context:\n{ctx}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A chunk that uses only a character's alias (bare given name) must still pull
    /// in the one canonical entry, so the agents don't see them as a new person.
    #[test]
    fn reference_ctx_matches_alias() {
        let (base, ws) = temp_ws("ref_alias");
        let yuu = Character {
            id: "yuu".into(),
            jp_name: "有月勇".into(),
            thai_name: "อาริทสึกิ ยู".into(),
            romaji: Some("Aritsuki Yuu".into()),
            gender: None,
            honorific: None,
            speech_style: None,
            relationships: Vec::new(),
            aliases: vec!["勇".into()],
            notes: None,
            first_seen_chapter: None,
        };
        // Persist the canonical entry with its alias.
        characters::upsert(&ws, yuu).unwrap();

        // The chunk only ever says 勇, never the full 有月勇.
        let ctx = build_reference_ctx(&ws, "勇は立ち上がった。");
        assert!(
            ctx.contains("อาริทสึกิ ยู"),
            "alias match must inject the canonical character:\n{ctx}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A client whose translator always succeeds but whose reviewer always errors
    /// (a transient hard failure), to exercise the resilience path.
    struct ReviewerErrorClient;

    #[async_trait::async_trait]
    impl crate::llm::client::LlmClient for ReviewerErrorClient {
        async fn chat(
            &self,
            req: &crate::llm::ChatRequest,
        ) -> crate::llm::client::Result<crate::llm::ChatResponse> {
            let schema_name = match &req.response_format {
                Some(crate::llm::ResponseFormat::JsonSchema { json_schema }) => {
                    Some(json_schema.name.as_str())
                }
                _ => None,
            };
            // The reviewer hard-errors every time (e.g. transport / empty choices).
            if schema_name == Some("review_result") {
                return Err(crate::llm::client::LlmError::EmptyChoices);
            }
            let content = match schema_name {
                Some("translation_result") => serde_json::json!({
                    "thought_process": {"scene_analysis": "(t)", "glossary_check": "(t)"},
                    "translated_text": "ข้อความแปลภาษาไทย",
                    "new_characters": [],
                    "new_terms": [],
                    "continuity_notes": []
                })
                .to_string(),
                _ => "(orchestrator: no tools)".to_string(),
            };
            Ok(crate::llm::ChatResponse {
                id: Some("reviewer-error-client".to_string()),
                model: Some("honya/test".to_string()),
                choices: vec![crate::llm::Choice {
                    index: 0,
                    message: crate::llm::ResponseMessage {
                        role: Some("assistant".to_string()),
                        content: Some(content),
                        tool_calls: None,
                    },
                    finish_reason: Some("stop".to_string()),
                }],
                usage: Some(crate::llm::Usage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                    cost: 0.0,
                    cost_details: None,
                }),
            })
        }
    }

    /// Regression: a single-chunk chapter whose only chunk hits a transient hard
    /// Reviewer error must NOT fail the whole chapter. The translation we already
    /// produced is committed flagged `NeedsReview` so the chapter completes (and
    /// the Thai is on disk) instead of showing ✗ Failed.
    #[tokio::test]
    async fn reviewer_hard_error_degrades_to_needs_review_not_failed() {
        let (base, ws) = temp_ws("reviewer_err");
        let raw = "# 第一章\n\nこれは短い章です。";
        translation::write_raw(&ws, 1, raw).unwrap();
        // Sanity: this raw really is a single chunk.
        let cfg = crate::model::AppConfig::default();
        assert_eq!(
            chunk_chapter(raw, cfg.chunk_target_tokens, cfg.chunk_hard_cap_tokens).len(),
            1,
            "test fixture must produce exactly one chunk"
        );

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = PipelineCtx {
            client: std::sync::Arc::new(ReviewerErrorClient)
                as std::sync::Arc<dyn crate::llm::client::LlmClient>,
            ws: ws.clone(),
            models: crate::model::ModelSet::default(),
            cfg,
            tx: crate::model::EventTx(tx),
            ctl: RunControl::new(),
        };
        run_pipeline(ctx, vec![1]).await.expect("run_pipeline");

        let mut saw_failed = false;
        let mut final_state = None;
        let mut finished = None;
        let mut retries = 0u32;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AppEvent::ChapterFailed { .. } => saw_failed = true,
                AppEvent::ChapterStateChanged { state, .. } => final_state = Some(state),
                AppEvent::ChunkRetry { .. } => retries += 1,
                AppEvent::PipelineFinished {
                    chapters_done,
                    chapters_failed,
                    chapters_need_review,
                    ..
                } => finished = Some((chapters_done, chapters_failed, chapters_need_review)),
                _ => {}
            }
        }

        assert!(
            !saw_failed,
            "a transient reviewer error must not fail the chapter"
        );
        assert_eq!(
            final_state,
            Some(ChapterStatus::NeedsReview),
            "chapter should complete flagged NeedsReview"
        );
        assert_eq!(
            finished,
            Some((1, 0, 1)),
            "1 done (completed), 0 failed, 1 needs review"
        );
        assert!(
            retries >= 2,
            "the reviewer error should be retried before degrading (got {retries})"
        );

        // The translation we produced is on disk, flagged for manual review.
        let translated = translation::read_translated(&ws, 1).await;
        assert!(
            translated.contains("ข้อความแปลภาษาไทย"),
            "the produced translation must be committed, not discarded"
        );
        assert!(
            translated.contains(translation::REVIEW_NEEDED_MARKER),
            "the committed chunk must carry the review-needed marker"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn synopsis_injected_into_context_and_round_trips() {
        let (base, ws) = temp_ws("syn");
        volume::set_synopsis(&ws, "原文のあらすじ", "เรื่องย่อสำหรับบริบท").unwrap();

        // Round-trips both fields on disk.
        let loaded = volume::load(&ws);
        assert_eq!(loaded.synopsis_raw, "原文のあらすじ");
        assert_eq!(loaded.synopsis_th, "เรื่องย่อสำหรับบริบท");

        // The Thai synopsis is injected into every chunk's reference context.
        let ctx = build_reference_ctx(&ws, "無関係なテキスト");
        assert!(
            ctx.contains("VOLUME_SYNOPSIS") && ctx.contains("เรื่องย่อสำหรับบริบท"),
            "synopsis must be injected as context:\n{ctx}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
