//! The single source of truth for honya's domain + event types; nothing here depends on other modules.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One imported light-novel project = one directory under the working root.
/// Mirrors: project/{PROJECT.md,CHARACTERS.md,GLOSSARY.md,STYLE.md,/images,/Vol_NN/...}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    /// Stable slug = directory name, e.g. "re-zero".
    pub id: String,
    /// Absolute path to the project directory.
    pub dir: PathBuf,
    /// Display title from PROJECT.md (falls back to id).
    pub title: String,
    #[serde(default)]
    pub created: Option<DateTime<Utc>>,
    #[serde(default)]
    pub touched: Option<DateTime<Utc>>,
    /// Volumes discovered on disk (Vol_01, Vol_02, ...), ascending.
    #[serde(default)]
    pub volumes: Vec<Volume>,
    /// Per-project model overrides (None => use AppConfig defaults).
    #[serde(default)]
    pub models: Option<ModelSet>,
}

/// One volume directory: Vol_NN/{VOLUME.md,/raw/ch_NNN.md,/translated/ch_NNN.md}.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Volume {
    /// 1-based volume number (Vol_01 => 1).
    pub number: u32,
    /// Absolute path to the volume directory.
    pub dir: PathBuf,
    /// Optional volume label from VOLUME.md, e.g. "黎明".
    #[serde(default)]
    pub label: Option<String>,
    /// Chapters in reading order (spine order), ascending by `number`.
    #[serde(default)]
    pub chapters: Vec<Chapter>,
}

/// One chapter unit. `number` maps to ch_{number:03}.md in raw/ and translated/.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chapter {
    /// 1-based chapter number within the volume.
    pub number: u32,
    /// Display title (from EPUB TOC), e.g. "第三章 影の中で".
    pub title: String,
    /// Prose vs illustration-only vs front/back matter.
    pub kind: ChapterKind,
    /// Lifecycle status (derived from disk state + live pipeline events).
    pub status: ChapterStatus,
    /// Source sentence/segment count for display (best-effort).
    #[serde(default)]
    pub source_segments: u32,
    /// Chunk count once chunked (0 until Chunking completes).
    #[serde(default)]
    pub total_chunks: u32,
    /// Chunks fully committed to translated/ch_NNN.md.
    #[serde(default)]
    pub committed_chunks: u32,
    /// Last time this chapter's status changed.
    #[serde(default)]
    pub last_run: Option<DateTime<Utc>>,
}

/// What sort of content a chapter holds — decides whether agents run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChapterKind {
    /// Translatable prose.
    Prose,
    /// Illustration-only page: image links copied straight to translated/, agents skipped.
    ImageOnly,
    /// Blank/front-matter/back-matter with no translatable text.
    Empty,
}

/// Chapter lifecycle (chapter granularity; chunk granularity is `ChunkState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChapterStatus {
    Pending,     // queued, untouched
    Chunking,    // slicing raw md into chunks
    Translating, // active chunk at Translator
    Reviewing,   // active chunk at Reviewer
    Appended,    // all chunks approved + written
    Done,        // metadata finalized (recap etc.)
    Failed,      // a chunk hit max retries / hard error
    Paused,      // run paused with this chapter mid-flight
}

impl ChapterStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, ChapterStatus::Done | ChapterStatus::Failed)
    }
    pub fn is_active(self) -> bool {
        matches!(
            self,
            ChapterStatus::Chunking | ChapterStatus::Translating | ChapterStatus::Reviewing
        )
    }
}

/// Per-chunk sub-state (the inner loop the Translate screen renders as rows).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkState {
    Queued,
    Translating,
    Reviewing,
    Rejected, // transient: feedback received, about to retry
    Approved,
    Committed, // appended to ch_NNN.md
    Failed,    // exceeded max attempts
}

/// The three model ids used by the pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSet {
    pub orchestrator: String,
    pub translator: String,
    pub reviewer: String,
}

impl Default for ModelSet {
    fn default() -> Self {
        Self {
            orchestrator: "google/gemini-3.5-flash".into(),
            translator: "google/gemini-3-flash-preview".into(),
            reviewer: "google/gemini-3.1-flash-lite".into(),
        }
    }
}

/// Global, persisted app configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// OpenRouter base URL.
    pub base_url: String,
    /// Default models (per-project ModelSet overrides this).
    pub models: ModelSet,
    /// Max Translator<->Reviewer retry attempts per chunk before Failed.
    pub max_attempts: u32,
    /// Target chunk size in tokens.
    pub chunk_target_tokens: usize,
    /// Hard ceiling for a single chunk.
    pub chunk_hard_cap_tokens: usize,
    /// Sentences of prior Thai injected for continuity.
    pub continuity_sentences: usize,
    /// HTTP referer/title sent to OpenRouter (ranking headers).
    pub referer: Option<String>,
    pub title: Option<String>,
    /// Persisted OpenRouter API key, captured at first launch. The environment
    /// variables HONYA_API_KEY / OPENROUTER_API_KEY override this when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            base_url: "https://openrouter.ai/api/v1".into(),
            models: ModelSet::default(),
            max_attempts: 3,
            chunk_target_tokens: 1000,
            chunk_hard_cap_tokens: 1200,
            continuity_sentences: 5,
            referer: Some("https://github.com/altqx/honya".into()),
            title: Some("honya".into()),
            api_key: None,
        }
    }
}

/// Translator strict-schema output ("translation_result"): brief `thought_process` plus
/// `translated_text`; discovery arrays let the Orchestrator persist new entities via tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslatorOut {
    pub thought_process: ThoughtProcess,
    pub translated_text: String,
    #[serde(default)]
    pub new_characters: Vec<NewCharacter>,
    #[serde(default)]
    pub new_terms: Vec<NewTerm>,
    #[serde(default)]
    pub continuity_notes: Vec<String>,
}

/// Concise pre-translation analysis. Spec rule: never draft the translation here (token thrift).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThoughtProcess {
    /// อารมณ์ / ความสัมพันธ์ / การเลือกสรรพนาม.
    pub scene_analysis: String,
    /// การอ้างอิงคำศัพท์จาก GLOSSARY.
    pub glossary_check: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewCharacter {
    pub jp_name: String,
    pub thai_name: String,
    pub gender: String,
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewTerm {
    pub jp_term: String,
    pub thai_term: String,
    pub category: String,
    pub gloss: String,
}

/// Reviewer strict-schema output ("review_result") — matches the product spec exactly:
/// a binary verdict plus an itemized feedback list (empty when approved).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewerOut {
    pub status: ReviewVerdict,
    #[serde(default)]
    pub feedback: Vec<String>,
}

impl ReviewerOut {
    pub fn approved(&self) -> bool {
        matches!(self.status, ReviewVerdict::Approve)
    }
    /// Itemized feedback collapsed to one string for retry prompts / log lines.
    pub fn feedback_text(&self) -> String {
        self.feedback.join("; ")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewVerdict {
    Approve,
    Reject,
}

// Workspace metadata types, tool-mutated in CHARACTERS.md / GLOSSARY.md / VOLUME.md.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Character {
    pub id: String,
    pub jp_name: String,
    pub thai_name: String,
    #[serde(default)]
    pub romaji: Option<String>,
    #[serde(default)]
    pub gender: Option<String>,
    #[serde(default)]
    pub honorific: Option<String>,
    #[serde(default)]
    pub speech_style: Option<String>,
    #[serde(default)]
    pub relationships: Vec<Relationship>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub first_seen_chapter: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relationship {
    pub target_id: String,
    pub relation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlossaryTerm {
    pub jp_term: String,
    pub thai_term: String,
    #[serde(default)]
    pub romaji: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub gloss: Option<String>,
    #[serde(default)]
    pub do_not_translate: Option<bool>,
    #[serde(default)]
    pub first_seen_chapter: Option<u32>,
}

/// VOLUME.md honya:data payload.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VolumeData {
    #[serde(default)]
    pub running_recap: String,
    /// chapter number (as string key) -> one-line summary.
    #[serde(default)]
    pub chapters: BTreeMap<String, String>,
    #[serde(default)]
    pub notes: Vec<ContinuityNote>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinuityNote {
    #[serde(default)]
    pub chapter: Option<u32>,
    pub severity: String, // info|warning|conflict
    #[serde(default)]
    pub kind: Option<String>,
    pub note: String,
}

/// Result every backend tool handler returns (serialized into the role:"tool" message).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub ok: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl ToolResult {
    pub fn ok(msg: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: msg.into(),
            data: None,
        }
    }
    pub fn data(msg: impl Into<String>, d: serde_json::Value) -> Self {
        Self {
            ok: true,
            message: msg.into(),
            data: Some(d),
        }
    }
    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: msg.into(),
            data: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentRole {
    Orchestrator,
    Translator,
    Reviewer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogLevel {
    Trace,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt: u32,
    pub completion: u32,
    pub total: u32,
}

/// The background -> UI channel payload, sent over `tokio::sync::mpsc`. Raw crossterm
/// input is NOT here (crossterm::Event isn't Serialize); it's matched in the select! arm.
#[derive(Debug, Clone)]
#[allow(dead_code)] // event payloads are consumed selectively across screens
pub enum AppEvent {
    ChapterQueued {
        chapter: u32,
    },
    ChapterStarted {
        chapter: u32,
    },
    ChapterChunked {
        chapter: u32,
        total_chunks: usize,
        est_tokens_total: usize,
    },
    ChapterStateChanged {
        chapter: u32,
        state: ChapterStatus,
    },
    ChapterCompleted {
        chapter: u32,
    },
    ChapterFailed {
        chapter: u32,
        reason: String,
    },

    ChunkStarted {
        chapter: u32,
        chunk: usize,
        total: usize,
        est_tokens: usize,
    },
    ChunkStateChanged {
        chapter: u32,
        chunk: usize,
        state: ChunkState,
    },
    TranslatorRequested {
        chapter: u32,
        chunk: usize,
        attempt: u32,
    },
    TranslatorReturned {
        chapter: u32,
        chunk: usize,
        attempt: u32,
        thai_preview: String,
        tokens: TokenUsage,
    },
    ReviewerRequested {
        chapter: u32,
        chunk: usize,
        attempt: u32,
    },
    ReviewerReturned {
        chapter: u32,
        chunk: usize,
        attempt: u32,
        verdict: ReviewVerdict,
        feedback: Option<String>,
    },
    ChunkRetry {
        chapter: u32,
        chunk: usize,
        attempt: u32,
        max: u32,
        feedback: String,
    },
    ChunkCommitted {
        chapter: u32,
        chunk: usize,
        bytes_written: usize,
    },
    ChunkFailed {
        chapter: u32,
        chunk: usize,
        attempts: u32,
        reason: String,
    },

    ToolInvoked {
        chapter: u32,
        tool: String,
        summary: String,
    },
    CharacterUpserted {
        id: String,
        jp_name: String,
        thai_name: String,
    },
    GlossaryUpserted {
        jp_term: String,
        thai_term: String,
    },
    VolumeRecapUpdated {
        chapter: u32,
    },
    ContinuityFlag {
        chapter: u32,
        severity: String,
        kind: String,
        note: String,
    },

    StreamDelta {
        chapter: u32,
        chunk: usize,
        role: AgentRole,
        delta: String,
    },
    UsageUpdate {
        total: TokenUsage,
        cost_usd: f64,
    },

    Log {
        level: LogLevel,
        msg: String,
    },
    PipelinePaused,
    PipelineResumed,
    PipelineFinished {
        chapters_done: u32,
        chapters_failed: u32,
    },
    Error {
        context: String,
        msg: String,
    },

    UpdateAvailable {
        version: String,
    },

    ImportProgress {
        done: usize,
        total: usize,
        label: String,
    },
    ImportFinished {
        project_id: String,
    },
}

/// Clonable sender handle background tasks use to talk to the UI.
#[derive(Clone)]
pub struct EventTx(pub tokio::sync::mpsc::UnboundedSender<AppEvent>);

impl EventTx {
    pub fn send(&self, e: AppEvent) {
        let _ = self.0.send(e);
    }
}
