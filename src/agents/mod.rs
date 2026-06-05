//! src/agents/mod.rs — the 3-agent translation pipeline (Orchestrator / Translator / Reviewer).
//!
//! The deterministic Rust pipeline (`pipeline.rs`) owns chunking, dispatch, the
//! Translator↔Reviewer retry loop, and the idempotent append to `translated/`.
//! The Orchestrator LLM is invoked once per committed chunk as a *metadata turn*
//! that persists discoveries (characters / glossary terms / continuity notes /
//! volume recap) through the backend tools.

pub mod chunk;
pub mod continuity;
pub mod pipeline;
pub mod prompts;
pub mod reviewer;
pub mod tokenize;
pub mod tools;
pub mod translator;

pub use pipeline::run_pipeline;
