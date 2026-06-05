//! src/agents/pipeline.rs — drive the full per-chapter / per-chunk state machine
//! and emit the `AppEvent` stream the UI renders.
//!
//! Flow per chapter (verbatim from the pipeline design):
//!   * ImageOnly chapter → `write_image_only`, skip the agents, `ChapterCompleted`.
//!   * Otherwise: chunk the raw markdown (`ChapterChunked`), then for each chunk
//!     translate → review up to `cfg.max_attempts`. On approve we DETERMINISTICALLY
//!     append the Thai (`workspace::translation::append_chunk`, NOT via an LLM
//!     tool), emit `ChunkCommitted`, then run the Orchestrator metadata turn so
//!     discoveries land in CHARACTERS.md / GLOSSARY.md / VOLUME.md. On exhausting
//!     retries the chunk and chapter fail.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use crate::agents::chunk::{Chunk, chunk_chapter};
use crate::agents::continuity;
use crate::agents::prompts::{ORCHESTRATOR_SYSTEM, build_orchestrator_metadata_msg};
use crate::agents::reviewer::review_chunk;
use crate::agents::tools::{WorkspaceTools, orchestrator_tools};
use crate::agents::translator::translate_chunk;
use crate::cleanse;
use crate::llm::client::LlmClient;
use crate::llm::tool_loop::run_tool_loop;
use crate::llm::{ChatRequest, Message, Tool, Usage};
use crate::model::{
    AppConfig, AppEvent, ChapterStatus, ChunkState, EventTx, LogLevel, ModelSet, ReviewVerdict,
    TokenUsage, TranslatorOut,
};
use crate::workspace::{Workspace, characters, data_block, glossary, translation};

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

/// How a chapter finished: ran to completion, or the user stopped the run.
enum Outcome {
    Completed,
    Stopped,
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
    let mut acc = TokenUsage::default();

    for chapter in chapters {
        if ctx.ctl.is_stopped() {
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

        match process_chapter(&ctx, chapter, &mut acc).await {
            Ok(Outcome::Completed) => {
                done += 1;
                ctx.tx.send(AppEvent::ChapterStateChanged {
                    chapter,
                    state: ChapterStatus::Done,
                });
                ctx.tx.send(AppEvent::ChapterCompleted { chapter });
            }
            Ok(Outcome::Stopped) => {
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
    });
    Ok(())
}

/// Process one chapter end to end. Image-only chapters short-circuit (the image
/// markdown is copied straight to `translated/`); prose chapters are chunked and
/// each chunk is translated + reviewed + committed.
async fn process_chapter(
    ctx: &PipelineCtx,
    chapter: u32,
    acc: &mut TokenUsage,
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
    for chunk in &chunks {
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
        process_chunk(ctx, chapter, chunk, acc).await?;
    }

    ctx.tx.send(AppEvent::ChapterStateChanged {
        chapter,
        state: ChapterStatus::Appended,
    });
    Ok(Outcome::Completed)
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

/// Assemble the reference context bundled into every Translator/Reviewer call:
/// the locked glossary, the character roster (pronouns/register), and the
/// PROJECT/STYLE prose — each in its own clearly-delimited section. Re-read per
/// chunk so mid-chapter glossary/character additions take effect immediately.
fn build_reference_ctx(ws: &Workspace) -> String {
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
    let terms = glossary::load(ws);
    section(
        &mut s,
        "<<GLOSSARY: คำศัพท์ที่ล็อกไว้ ต้องใช้ให้ตรง>>",
        &glossary::render_context_blurb(&terms),
        "<<END_GLOSSARY>>",
    );
    let chars = characters::load(ws);
    section(
        &mut s,
        "<<CHARACTERS: สรรพนาม/น้ำเสียงที่กำหนด>>",
        &characters::render_context_blurb(&chars),
        "<<END_CHARACTERS>>",
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

/// Convert API token `Usage` into the UI's `TokenUsage`.
fn to_tokens(u: &Usage) -> TokenUsage {
    TokenUsage {
        prompt: u.prompt_tokens,
        completion: u.completion_tokens,
        total: u.total_tokens,
    }
}

/// Fold a delta into the running accumulator (saturating).
fn add_tokens(acc: &mut TokenUsage, d: &TokenUsage) {
    acc.prompt = acc.prompt.saturating_add(d.prompt);
    acc.completion = acc.completion.saturating_add(d.completion);
    acc.total = acc.total.saturating_add(d.total);
}

/// Translate → review one chunk, retrying up to `cfg.max_attempts`. On approval
/// the Thai is deterministically appended and the Orchestrator metadata turn
/// runs. Exhausting the attempts fails the chunk (and bubbles to chapter fail).
async fn process_chunk(
    ctx: &PipelineCtx,
    chapter: u32,
    chunk: &Chunk,
    acc: &mut TokenUsage,
) -> anyhow::Result<()> {
    ctx.tx.send(AppEvent::ChunkStateChanged {
        chapter,
        chunk: chunk.index,
        state: ChunkState::Queued,
    });

    // Reference context (glossary + characters + project + style) and the
    // continuity tail are stable across this chunk's attempts.
    let reference_ctx = build_reference_ctx(&ctx.ws);
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

        let (out, t_usage): (TranslatorOut, Usage) = match translate_chunk(
            ctx.client.as_ref(),
            &ctx.models.translator,
            &reference_ctx,
            &prev_thai,
            &chunk.text,
            feedback.as_deref(),
        )
        .await
        {
            Ok(o) => o,
            Err(e) => {
                ctx.tx.send(AppEvent::Error {
                    context: format!("translator ch{chapter} chunk{}", chunk.index),
                    msg: e.to_string(),
                });
                anyhow::bail!("translator failed on chunk {}: {e}", chunk.index);
            }
        };

        let thai = out.translated_text.clone();
        let tok = to_tokens(&t_usage);
        add_tokens(acc, &tok);
        ctx.tx.send(AppEvent::TranslatorReturned {
            chapter,
            chunk: chunk.index,
            attempt,
            thai_preview: preview(&thai),
            tokens: tok,
        });
        ctx.tx.send(AppEvent::UsageUpdate {
            total: *acc,
            cost_usd: 0.0,
        });

        // ---- Reviewer ----
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
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                ctx.tx.send(AppEvent::Error {
                    context: format!("reviewer ch{chapter} chunk{}", chunk.index),
                    msg: e.to_string(),
                });
                anyhow::bail!("reviewer failed on chunk {}: {e}", chunk.index);
            }
        };
        add_tokens(acc, &to_tokens(&r_usage));
        ctx.tx.send(AppEvent::UsageUpdate {
            total: *acc,
            cost_usd: 0.0,
        });

        let approved = review.approved();
        let fb_text = review.feedback_text();
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
            if let Err(e) = run_orchestrator_metadata_turn(ctx, chapter, &out).await {
                // Metadata persistence is best-effort; never fail the chunk on it.
                ctx.tx.send(AppEvent::Error {
                    context: format!("orchestrator ch{chapter} chunk{}", chunk.index),
                    msg: e.to_string(),
                });
            }

            return Ok(());
        }

        // ---- Rejected ----
        if attempt < max {
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
                feedback: fb_text.clone(),
            });
            feedback = Some(fb_text);
        } else {
            ctx.tx.send(AppEvent::ChunkStateChanged {
                chapter,
                chunk: chunk.index,
                state: ChunkState::Failed,
            });
            ctx.tx.send(AppEvent::ChunkFailed {
                chapter,
                chunk: chunk.index,
                attempts: attempt,
                reason: if fb_text.is_empty() {
                    "reviewer rejected after max attempts".to_string()
                } else {
                    fb_text
                },
            });
            anyhow::bail!("chunk {} rejected after {max} attempt(s)", chunk.index);
        }
    }

    // Unreachable: the loop either returns Ok on approve or bails on final reject.
    anyhow::bail!(
        "chunk {} exhausted attempts without resolution",
        chunk.index
    )
}

/// Run the Orchestrator metadata turn for a just-approved chunk: a single tool
/// loop that lets the Orchestrator persist new characters / terms / continuity
/// notes and advance the volume recap through the backend tools.
async fn run_orchestrator_metadata_turn(
    ctx: &PipelineCtx,
    chapter: u32,
    out: &TranslatorOut,
) -> anyhow::Result<()> {
    let user = build_orchestrator_metadata_msg(chapter, out);

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

    run_tool_loop(ctx.client.as_ref(), req, &executor, 8)
        .await
        .map_err(|e| anyhow::anyhow!("orchestrator tool loop failed: {e}"))?;

    Ok(())
}

/// A short single-line preview of Thai output for the UI event stream. Uses
/// char boundaries (Thai text has no inter-word spaces) and caps length.
fn preview(thai: &str) -> String {
    const MAX_CHARS: usize = 80;
    let flat = thai.replace(['\n', '\r'], " ");
    let flat = flat.trim();
    let mut out: String = flat.chars().take(MAX_CHARS).collect();
    if flat.chars().count() > MAX_CHARS {
        out.push('…');
    }
    out
}
