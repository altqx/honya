//! The 3-agent translation pipeline (Orchestrator / Translator / Reviewer).
//!
//! `pipeline.rs` owns chunking, dispatch, the Translator↔Reviewer retry loop,
//! and the idempotent append to `translated/`. The Orchestrator LLM runs once
//! per committed chunk as a metadata turn that persists discoveries via tools.

pub mod chunk;
pub mod continuity;
pub mod pipeline;
pub mod prompts;
pub mod reviewer;
pub mod synopsis;
pub mod tokenize;
pub mod tools;
pub mod translator;

pub use pipeline::run_pipeline;
