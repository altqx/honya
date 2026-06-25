//! Domain and event types shared across honya.

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
    /// Thai title, empty until set.
    #[serde(default)]
    pub title_th: String,
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
    /// Chunks committed flagged-for-review with *no* usable translation — the
    /// Translator never produced acceptable Thai, so the chunk was skipped after
    /// max attempts and only its marker/banner landed. A `NeedsReview` chapter
    /// with `skipped_chunks > 0` is partially translated (content is missing),
    /// not merely unreviewed.
    #[serde(default)]
    pub skipped_chunks: u32,
    /// Last time this chapter's status changed.
    #[serde(default)]
    pub last_run: Option<DateTime<Utc>>,
    /// Cumulative lifetime usage (tokens / cost / tool calls) charged to this
    /// chapter across every run. Loaded from VOLUME.md's data block on scan.
    #[serde(default)]
    pub usage: UsageStats,
}

impl Chapter {
    /// A flagged chapter that is also missing translated content: it reached
    /// `NeedsReview` because one or more chunks were skipped (committed empty)
    /// rather than merely committed unreviewed. Drives the "partial" indicator.
    pub fn is_partial_review(&self) -> bool {
        self.status == ChapterStatus::NeedsReview && self.skipped_chunks > 0
    }

    /// Chunks that carry real Thai (committed total minus skipped-empty chunks).
    pub fn translated_chunks(&self) -> u32 {
        self.committed_chunks.saturating_sub(self.skipped_chunks)
    }
}

impl Volume {
    /// Lifetime usage for the volume = sum of every chapter's usage.
    pub fn usage_total(&self) -> UsageStats {
        let mut t = UsageStats::default();
        for c in &self.chapters {
            t.add(&c.usage);
        }
        t
    }
}

impl Project {
    /// Lifetime usage for the project = sum of every volume's usage.
    pub fn usage_total(&self) -> UsageStats {
        let mut t = UsageStats::default();
        for v in &self.volumes {
            t.add(&v.usage_total());
        }
        t
    }

    /// Live translation progress across every volume, counting only prose chapters
    /// (image-only / empty pages need no agent work, so they never gate "done").
    /// Draft until the first prose chapter completes or a run is active; Done once
    /// every prose chapter is complete.
    pub fn translation_progress(&self) -> TranslationProgress {
        let mut total = 0u32;
        let mut done = 0u32;
        let mut active = false;
        for v in &self.volumes {
            for c in &v.chapters {
                if c.kind != ChapterKind::Prose {
                    continue;
                }
                total += 1;
                if c.status.is_complete() {
                    done += 1;
                } else if c.status.is_active() || c.status == ChapterStatus::Paused {
                    active = true;
                }
            }
        }
        let status = if total == 0 {
            ProjectStatus::Draft
        } else if done == total {
            ProjectStatus::Done
        } else if done > 0 || active {
            ProjectStatus::InProgress
        } else {
            ProjectStatus::Draft
        };
        TranslationProgress {
            status,
            done,
            total,
        }
    }
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
    NeedsReview, // completed, but ≥1 chunk was committed without passing review
    Failed,      // a chunk hit max retries / hard error
    Paused,      // run paused with this chapter mid-flight
    Partial,     // some chunks on disk but not all — run stopped/failed mid-chapter
}

impl ChapterStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            ChapterStatus::Done | ChapterStatus::NeedsReview | ChapterStatus::Failed
        )
    }
    pub fn is_active(self) -> bool {
        matches!(
            self,
            ChapterStatus::Chunking | ChapterStatus::Translating | ChapterStatus::Reviewing
        )
    }
    /// Completed with translated output written — clean (`Done`) or flagged
    /// (`NeedsReview`), but not `Failed`. Drives project-level progress.
    pub fn is_complete(self) -> bool {
        matches!(self, ChapterStatus::Done | ChapterStatus::NeedsReview)
    }
}

/// Project-wide translation progress, derived live from chapter statuses across
/// every volume. Surfaced as the STYLE.md / PROJECT.md status line and the
/// Project tab Context panel — replaces the old hardcoded "draft" stub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectStatus {
    /// Imported, but no prose chapter has been translated yet.
    Draft,
    /// Some prose chapters are translated (or a run is in flight), but not all.
    InProgress,
    /// Every translatable (prose) chapter is complete.
    Done,
}

impl ProjectStatus {
    /// Bilingual label for the Markdown status line (matches the Thai-localized files).
    pub fn label_th(self) -> &'static str {
        match self {
            ProjectStatus::Draft => "ฉบับร่าง (draft)",
            ProjectStatus::InProgress => "กำลังแปล (in progress)",
            ProjectStatus::Done => "เสร็จสมบูรณ์ (done)",
        }
    }
    /// Terse English label for the Context panel.
    pub fn label_en(self) -> &'static str {
        match self {
            ProjectStatus::Draft => "draft",
            ProjectStatus::InProgress => "in progress",
            ProjectStatus::Done => "done",
        }
    }
    /// Machine value persisted to the STYLE.md data block.
    pub fn slug(self) -> &'static str {
        match self {
            ProjectStatus::Draft => "draft",
            ProjectStatus::InProgress => "in_progress",
            ProjectStatus::Done => "done",
        }
    }
}

/// Snapshot of translation progress: status + completed/total translatable chapters.
#[derive(Debug, Clone, Copy)]
pub struct TranslationProgress {
    pub status: ProjectStatus,
    /// Prose chapters complete (`Done` or `NeedsReview`).
    pub done: u32,
    /// Translatable (prose) chapters total.
    pub total: u32,
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
    Committed,   // appended to ch_NNN.md
    NeedsReview, // committed unreviewed after exhausting attempts (flagged in-file)
}

/// LLM provider an agent routes to. OpenRouter and Tokenrouter share the
/// chat/completions client; Codex uses ChatGPT's Responses API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    #[default]
    OpenRouter,
    Tokenrouter,
    Codex,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Provider::OpenRouter => "OpenRouter",
            Provider::Tokenrouter => "Tokenrouter",
            Provider::Codex => "Codex",
        }
    }

    pub fn cycled(self) -> Self {
        match self {
            Provider::OpenRouter => Provider::Tokenrouter,
            Provider::Tokenrouter => Provider::Codex,
            Provider::Codex => Provider::OpenRouter,
        }
    }
}

/// Reasoning effort for a reasoning-capable model. Stored as `Option` on an
/// [`AgentModel`]: `None` omits the `reasoning` param; `Some(_)` sends the effort.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Minimal => "minimal",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
        }
    }

    /// Short label for the Settings line (`Off` when unset).
    pub fn label(this: Option<Self>) -> &'static str {
        match this {
            None => "Off",
            Some(e) => e.as_str(),
        }
    }

    /// Cycle Off → minimal → low → medium → high → xhigh → Off.
    pub fn cycled(this: Option<Self>) -> Option<Self> {
        match this {
            None => Some(Effort::Minimal),
            Some(Effort::Minimal) => Some(Effort::Low),
            Some(Effort::Low) => Some(Effort::Medium),
            Some(Effort::Medium) => Some(Effort::High),
            Some(Effort::High) => Some(Effort::Xhigh),
            Some(Effort::Xhigh) => None,
        }
    }
}

/// One agent's provider + model id + optional reasoning effort.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentModel {
    pub provider: Provider,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
}

impl AgentModel {
    pub fn new(provider: Provider, model: impl Into<String>, effort: Option<Effort>) -> Self {
        Self {
            provider,
            model: model.into(),
            effort,
        }
    }

    /// A plain OpenRouter model with no reasoning effort (the legacy/default shape).
    pub fn openrouter(model: impl Into<String>) -> Self {
        Self::new(Provider::OpenRouter, model, None)
    }

    /// The `reasoning` request param for this agent, if an effort is configured.
    pub fn reasoning_param(&self) -> Option<serde_json::Value> {
        self.effort
            .map(|e| serde_json::json!({ "effort": e.as_str() }))
    }
}

/// Accept either a bare model-id string (legacy configs, where every agent ran on
/// OpenRouter) or the full `{provider, model, effort}` object.
impl<'de> Deserialize<'de> for AgentModel {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Str(String),
            Obj {
                #[serde(default)]
                provider: Provider,
                model: String,
                #[serde(default)]
                effort: Option<Effort>,
            },
        }
        Ok(match Raw::deserialize(deserializer)? {
            Raw::Str(model) => AgentModel::openrouter(model),
            Raw::Obj {
                provider,
                model,
                effort,
            } => AgentModel {
                provider,
                model,
                effort,
            },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSet {
    pub orchestrator: AgentModel,
    pub translator: AgentModel,
    pub reviewer: AgentModel,
    /// Default keeps older configs loadable.
    #[serde(default = "default_refine_model")]
    pub refine: AgentModel,
}

fn default_refine_model() -> AgentModel {
    AgentModel::openrouter("openai/gpt-5.5")
}

impl Default for ModelSet {
    fn default() -> Self {
        Self {
            orchestrator: AgentModel::openrouter("google/gemini-3.5-flash"),
            translator: AgentModel::openrouter("google/gemini-3-flash-preview"),
            reviewer: AgentModel::openrouter("google/gemini-3.1-flash-lite"),
            refine: default_refine_model(),
        }
    }
}

/// Selectable color theme. Pure data (keeps `model.rs` dependency-free); the
/// palettes and labels live in `theme.rs`, keyed by `ThemeId::build`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeId {
    #[default]
    Washi,
    SolarizedLight,
    Sumi,
    /// Adaptive: inherits the host terminal's own ANSI colors.
    Terminal,
    Gruvbox,
    Nord,
    TokyoNight,
    Dracula,
    Catppuccin,
    SolarizedDark,
    Everforest,
    RosePine,
}

/// How honya handles a newer release found at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateMode {
    /// Download, verify, and install the update in the background at launch; the
    /// new binary goes live on the next start. This is the default.
    #[default]
    Auto,
    /// Leave the install to the user — only surface a "honya update" hint when a
    /// newer release exists (the pre-0.1.12 behavior).
    Notify,
}

impl UpdateMode {
    /// Short label for the Settings line.
    pub fn label(self) -> &'static str {
        match self {
            UpdateMode::Auto => "On startup",
            UpdateMode::Notify => "Notify only",
        }
    }

    /// The other mode (for the Settings toggle).
    pub fn toggled(self) -> Self {
        match self {
            UpdateMode::Auto => UpdateMode::Notify,
            UpdateMode::Notify => UpdateMode::Auto,
        }
    }
}

/// Which line of builds the self-updater tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseChannel {
    /// Prebuilt binaries from GitHub Releases (the default).
    #[default]
    Stable,
    /// Latest `main` commit, built from source on this machine with the local
    /// Rust toolchain. Updates arrive per-commit, same auto/notify behavior.
    Dev,
}

impl ReleaseChannel {
    /// Short label for the Settings line.
    pub fn label(self) -> &'static str {
        match self {
            ReleaseChannel::Stable => "Stable",
            ReleaseChannel::Dev => "Dev (latest git)",
        }
    }

    /// The other channel (for the Settings toggle).
    pub fn toggled(self) -> Self {
        match self {
            ReleaseChannel::Stable => ReleaseChannel::Dev,
            ReleaseChannel::Dev => ReleaseChannel::Stable,
        }
    }
}

/// OpenRouter/OpenAI `service_tier` request parameter, trading latency for cost.
/// Stored as `Option` in [`AppConfig`]: `None` omits the field entirely so the
/// provider applies its own default; `Some(_)` sends the chosen tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceTier {
    /// Slower, cheaper batch tier (`"flex"`).
    Flex,
    /// Faster, higher-priority tier (`"priority"`).
    Priority,
}

impl ServiceTier {
    /// Short label for the Settings line.
    pub fn label(this: Option<Self>) -> &'static str {
        match this {
            None => "Off",
            Some(ServiceTier::Flex) => "Flex",
            Some(ServiceTier::Priority) => "Priority",
        }
    }

    /// Cycle through Off → Flex → Priority for the Settings toggle.
    pub fn cycled(this: Option<Self>) -> Option<Self> {
        match this {
            None => Some(ServiceTier::Flex),
            Some(ServiceTier::Flex) => Some(ServiceTier::Priority),
            Some(ServiceTier::Priority) => None,
        }
    }

    /// One-line trade-off hint shown under the Settings line.
    pub fn desc(this: Option<Self>) -> &'static str {
        match this {
            None => "requests use each provider's standard tier",
            Some(ServiceTier::Flex) => "~50% cheaper, slower; standard rate if model lacks tiers",
            Some(ServiceTier::Priority) => "faster, costs more; standard rate if model lacks tiers",
        }
    }
}

/// Global, persisted app configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
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
    /// Loop watchdog: a chapter that makes no pipeline progress for this many
    /// seconds — a degenerate repetition loop or a dead stall — is abandoned and
    /// re-translated whole. `0` disables the time/stall arm of the watchdog (the
    /// streamed-output repetition detector still runs). Used by the auto
    /// project-translate run; see [`crate::agents::pipeline`].
    #[serde(default = "default_loop_stall_secs")]
    pub loop_stall_secs: u64,
    /// How many times a single chapter may be re-translated from scratch after the
    /// loop watchdog trips before the whole run aborts. `0` aborts on the first
    /// detected loop (no re-translate).
    #[serde(default = "default_max_chapter_retranslates")]
    pub max_chapter_retranslates: u32,
    /// HTTP referer/title sent to OpenRouter (ranking headers).
    pub referer: Option<String>,
    pub title: Option<String>,
    /// Persisted OpenRouter API key, captured at first launch. The environment
    /// variables HONYA_API_KEY / OPENROUTER_API_KEY override this when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Persisted Tokenrouter API key. Env HONYA_TOKENROUTER_API_KEY /
    /// TOKENROUTER_API_KEY override this when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokenrouter_api_key: Option<String>,
    /// Active color theme (serde default keeps pre-theme configs loading).
    #[serde(default)]
    pub theme: ThemeId,
    /// True once the user has been through (or dismissed) first-run onboarding.
    /// Drives whether the in-app Welcome overlay auto-opens at launch.
    #[serde(default)]
    pub onboarded: bool,
    /// What to do when a newer release is found at startup (serde default keeps
    /// pre-update-mode configs loading, and defaults them to auto-install).
    #[serde(default)]
    pub update_mode: UpdateMode,
    /// Which builds the self-updater tracks: stable releases or latest git
    /// (built from source on this machine).
    #[serde(default)]
    pub release_channel: ReleaseChannel,
    /// OpenRouter `service_tier` sent on every chat request (`flex` = cheaper and
    /// slower, `priority` = faster). `None` omits the field — the provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<ServiceTier>,
    /// Run a one-time terminology/character pre-extraction pass over a volume's raw
    /// chapters before translating chunk 1, seeding CHARACTERS.md / GLOSSARY.md and a
    /// few style exemplars so early chapters get the same context as late ones.
    #[serde(default = "default_true")]
    pub prepass_extract: bool,
    /// After a chapter's chunks are all written, run a whole-chapter coherence sweep
    /// (pronoun / term / name consistency across chunks) and record findings as
    /// continuity notes for the QA inbox.
    #[serde(default = "default_true")]
    pub coherence_check: bool,
    /// Holds the relay `device_token` secret, so config.json's 0600 handling matters.
    /// Remote enablement is per-session, not persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<RemoteAccount>,
    /// Codex OAuth tokens; config.json is written 0600.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_auth: Option<crate::codex::CodexAuth>,
}

/// Linked GitHub identity and relay credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteAccount {
    pub github_login: String,
    pub device_id: String,
    pub device_token: String,
}

/// Pending GitHub device-flow prompt shown while sign-in is in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthCodePrompt {
    pub code: String,
    pub uri: String,
    pub uri_complete: String,
}

/// Serde default for opt-in-by-default booleans (so older configs enable them).
fn default_true() -> bool {
    true
}

/// Default loop-watchdog stall window (seconds). Sits above the 120 s per-request
/// HTTP timeout so a single slow-but-legit call never trips it — only a genuine
/// multi-call stall or a stream that never converges does.
fn default_loop_stall_secs() -> u64 {
    180
}

/// Default whole-chapter re-translations allowed before a looping chapter aborts
/// the run.
fn default_max_chapter_retranslates() -> u32 {
    2
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            models: ModelSet::default(),
            max_attempts: 3,
            chunk_target_tokens: 1000,
            chunk_hard_cap_tokens: 1200,
            continuity_sentences: 5,
            loop_stall_secs: default_loop_stall_secs(),
            max_chapter_retranslates: default_max_chapter_retranslates(),
            referer: Some("https://github.com/altqx/honya".into()),
            title: Some("honya".into()),
            api_key: None,
            tokenrouter_api_key: None,
            theme: ThemeId::default(),
            onboarded: false,
            update_mode: UpdateMode::default(),
            release_channel: ReleaseChannel::default(),
            service_tier: None,
            prepass_extract: true,
            coherence_check: true,
            account: None,
            codex_auth: None,
        }
    }
}

/// Translator strict-schema output for `translation_result`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslatorOut {
    pub thought_process: ThoughtProcess,
    pub translated_text: String,
    /// Who is narrating at the END of this chunk — the first-person POV character
    /// and their Thai self-pronoun, or third-person. Carried into the next chunk so
    /// a POV section that spans a chunk boundary keeps the right "I". Empty when
    /// unknown/unchanged.
    #[serde(default)]
    pub pov: String,
    #[serde(default)]
    pub new_characters: Vec<NewCharacter>,
    #[serde(default)]
    pub new_terms: Vec<NewTerm>,
    #[serde(default)]
    pub continuity_notes: Vec<String>,
}

/// Concise pre-translation analysis; never draft the translation here.
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
    #[serde(default)]
    pub policy: Option<TermPolicy>,
    #[serde(default)]
    pub forbidden_thai: Vec<String>,
    #[serde(default)]
    pub context_rule: Option<String>,
    #[serde(default)]
    pub do_not_translate: Option<bool>,
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
    /// JP variants that render to this character's canonical Thai name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Address forms for this character, each with its own Thai rendering.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also_called: Vec<AltName>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub first_seen_chapter: Option<u32>,
}

/// Alternate address form with a fixed Thai rendering.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AltName {
    pub jp: String,
    #[serde(default)]
    pub thai: String,
    /// Who uses this name, when that context matters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relationship {
    pub target_id: String,
    pub relation: String,
}

/// How a glossary entry should constrain terminology in translation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TermPolicy {
    /// Use the saved rendering exactly whenever the source term appears.
    HardLocked,
    /// Use the saved rendering by default, but allow natural context-sensitive variation.
    Preferred,
    /// The saved/forbidden renderings must not appear for this source term.
    Forbidden,
    /// The rendering depends on context; `context_rule` explains when/how to choose.
    ContextDependent,
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
    /// New terminology-control model. When absent, legacy `protected=true` maps
    /// to [`TermPolicy::HardLocked`]; otherwise the entry is treated as preferred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<TermPolicy>,
    /// Thai renderings that must not be used for this Japanese term.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbidden_thai: Vec<String>,
    /// Context rule for [`TermPolicy::ContextDependent`] entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_rule: Option<String>,
    /// Back-compat / manual protection flag: automatic Orchestrator upserts must
    /// not rewrite controlled human-confirmed terms.
    #[serde(default, alias = "locked")]
    pub protected: Option<bool>,
    #[serde(default)]
    pub do_not_translate: Option<bool>,
    #[serde(default)]
    pub first_seen_chapter: Option<u32>,
}

/// VOLUME.md honya:data payload.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VolumeData {
    /// User-provided volume synopsis (เรื่องย่อ), raw untranslated source text.
    #[serde(default)]
    pub synopsis_raw: String,
    /// Thai translation of `synopsis_raw` — injected into every chunk's reference
    /// context so the agents share the volume's overall arc.
    #[serde(default)]
    pub synopsis_th: String,
    #[serde(default)]
    pub running_recap: String,
    /// True once the pre-extraction pass has seeded the roster/glossary for this
    /// volume, so a re-run or resume does not repeat it.
    #[serde(default)]
    pub prepass_done: bool,
    /// A few short JP→TH exemplar pairs demonstrating the target register, injected
    /// into every Translator call as few-shot style anchors. Seeded by the
    /// pre-extraction pass and editable by hand in VOLUME.md.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub style_examples: Vec<StyleExample>,
    /// Source-file metadata discovered at import time (title, author, language, etc.).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub source_metadata: BTreeMap<String, String>,
    /// chapter number (as string key) -> one-line summary.
    #[serde(default)]
    pub chapters: BTreeMap<String, String>,
    /// chapter number (as string key) -> cumulative lifetime usage. The volume
    /// total is the sum of these; the project total is the sum across volumes.
    #[serde(default)]
    pub chapter_usage: BTreeMap<String, UsageStats>,
    /// Append-only-ish audit trail of translation runs for this volume. Updated at
    /// run start/finish so crash recovery can leave a durable breadcrumb instead
    /// of only an ephemeral TUI log.
    #[serde(default)]
    pub run_history: Vec<RunHistoryEntry>,
    /// Per-chapter run outcomes retained for rerun comparison (cost / QA / glossary
    /// deltas + archived Thai). Trimmed to the most recent few runs per chapter.
    #[serde(default)]
    pub chapter_runs: Vec<ChapterRun>,
    #[serde(default)]
    pub notes: Vec<ContinuityNote>,
    /// Human proofreading notes anchored to Reader lines in translated chapters.
    #[serde(default)]
    pub annotations: Vec<ReaderAnnotation>,
    /// User navigation bookmarks anchored to Reader lines.
    #[serde(default)]
    pub bookmarks: Vec<ReaderBookmark>,
}

/// One short JP→TH exemplar pair anchoring the target translation register. A
/// handful of these injected into the Translator prompt steer voice/tone far more
/// concretely than abstract STYLE.md prose.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StyleExample {
    /// Source Japanese sentence/snippet.
    pub jp: String,
    /// Its target-register Thai rendering.
    pub th: String,
    /// Optional one-line note on what the pair demonstrates (register, pronoun, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReaderAnnotation {
    /// Chapter number within this volume.
    pub chapter: u32,
    /// 1-based translated-file line anchor. The Reader inserts the note after this line.
    pub line: u32,
    /// Human note text, e.g. "awkward phrasing" or "check honorific".
    pub note: String,
    /// Creation timestamp for sorting/display. Optional keeps old/manual data tolerant.
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
}

/// A user navigation bookmark anchored to a Reader line in a chapter. Like
/// [`ReaderAnnotation`] but carries no note body — just a jump target plus a short
/// label (the bookmarked line's text) so the jump picker reads well.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReaderBookmark {
    /// Chapter number within this volume.
    pub chapter: u32,
    /// 1-based line anchor within the chapter (same basis as `ReaderAnnotation.line`).
    pub line: u32,
    /// Short label for the picker — typically a preview of the bookmarked line.
    #[serde(default)]
    pub label: String,
    /// Creation timestamp for sorting/display. Optional keeps old/manual data tolerant.
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
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

/// Persisted usage accounting at one aggregation level (chapter; summed for
/// volume/project). Costs are cumulative "lifetime spend" — re-translating a
/// chapter adds to it.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct UsageStats {
    #[serde(default)]
    pub tokens: TokenUsage,
    /// Total USD (BYOK-aware: OpenRouter fee + upstream provider charge).
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub tool_calls: u32,
}

impl UsageStats {
    /// True when nothing has been recorded yet (drives "hide if empty" UI).
    pub fn is_zero(&self) -> bool {
        self.tokens.total == 0
            && self.tokens.prompt == 0
            && self.tokens.completion == 0
            && self.tool_calls == 0
            && self.cost_usd == 0.0
    }

    /// Fold another record into this one (saturating tokens/tool-calls, summed USD).
    pub fn add(&mut self, o: &UsageStats) {
        self.tokens.prompt = self.tokens.prompt.saturating_add(o.tokens.prompt);
        self.tokens.completion = self.tokens.completion.saturating_add(o.tokens.completion);
        self.tokens.total = self.tokens.total.saturating_add(o.tokens.total);
        self.cost_usd += o.cost_usd;
        self.tool_calls = self.tool_calls.saturating_add(o.tool_calls);
    }
}

/// Durable lifecycle state for one translation run in a volume's run history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunHistoryStatus {
    /// Run has been started and a recovery checkpoint should also exist.
    #[default]
    Running,
    /// Every queued chapter finished cleanly.
    Completed,
    /// All queued chapters finished, but at least one chunk was committed with a
    /// visible review-needed marker.
    NeedsReview,
    /// The run completed some work but one or more chapters failed.
    Partial,
    /// No queued chapter completed successfully.
    Failed,
    /// The user stopped the run cooperatively from the Translate screen.
    Stopped,
    /// The user discarded an interrupted checkpoint instead of resuming it.
    Discarded,
}

/// One persisted run-history row in `VOLUME.md`'s data block. This is not used as
/// the resume substrate (translated chunk markers are); it is the human/audit
/// trail that explains what happened across crashes, stops, retries, and reruns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunHistoryEntry {
    /// Stable id shared with the crash-recovery checkpoint.
    pub id: String,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub status: RunHistoryStatus,
    /// Chapter queue requested for this run, in order.
    #[serde(default)]
    pub chapters: Vec<u32>,
    #[serde(default)]
    pub chapters_done: u32,
    #[serde(default)]
    pub chapters_failed: u32,
    #[serde(default)]
    pub chapters_need_review: u32,
    /// Whole-run usage/cost total emitted by the pipeline at finish.
    #[serde(default)]
    pub usage: UsageStats,
    /// honya version that created the run entry.
    #[serde(default)]
    pub honya_version: String,
}

impl RunHistoryEntry {
    pub fn started(
        id: String,
        started_at: DateTime<Utc>,
        chapters: Vec<u32>,
        honya_version: String,
    ) -> Self {
        Self {
            id,
            started_at,
            finished_at: None,
            status: RunHistoryStatus::Running,
            chapters,
            chapters_done: 0,
            chapters_failed: 0,
            chapters_need_review: 0,
            usage: UsageStats::default(),
            honya_version,
        }
    }
}

/// One chapter's outcome from a single translation run, retained per-chapter in
/// VOLUME.md so reruns can be compared (cost / QA / glossary deltas, plus a path
/// to the archived Thai this run produced once a later rerun displaces it). Unlike
/// `chapter_usage` (which is the *cumulative* lifetime total), `usage` here is the
/// spend of this one run — exactly the "cost difference" a rerun comparison needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChapterRun {
    pub chapter: u32,
    /// Run id shared with the recovery checkpoint / run-history row. The sentinel
    /// `"(prior)"` marks a version translated before this feature existed, whose
    /// per-run cost was never recorded (`usage_unknown`).
    pub run_id: String,
    pub finished_at: DateTime<Utc>,
    /// This run's spend on this chapter (per-run delta, NOT the lifetime total).
    #[serde(default)]
    pub usage: UsageStats,
    /// True when `usage` was never captured (a pre-feature version) — display n/a
    /// rather than a misleading `$0.0000`.
    #[serde(default)]
    pub usage_unknown: bool,
    /// QA signal: chunks left flagged review-needed after this run (0 = clean).
    #[serde(default)]
    pub review_needed: u32,
    /// QA signal: the chapter ended `Failed`.
    #[serde(default)]
    pub failed: bool,
    #[serde(default)]
    pub total_chunks: u32,
    #[serde(default)]
    pub committed_chunks: u32,
    /// jp_terms inserted into the glossary during this run.
    #[serde(default)]
    pub glossary_added: Vec<String>,
    /// jp_terms whose Thai rendering changed during this run.
    #[serde(default)]
    pub glossary_changed: Vec<String>,
    /// Path (relative to the volume dir, e.g. `reruns/ch_003/<run>.md`) to the Thai
    /// this run produced, archived when a later rerun displaced it. `None` while
    /// this run is still the live version in `translated/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<String>,
}

/// The background -> UI channel payload, sent over `tokio::sync::mpsc`. Raw crossterm
/// input is NOT here (crossterm::Event isn't Serialize); it's matched in the select! arm.
#[derive(Debug, Clone)]
#[allow(dead_code)] // event payloads are consumed selectively across screens
pub enum AppEvent {
    ChapterQueued {
        chapter: u32,
    },
    /// Coalesced signal that the live queue mirror should refresh.
    QueueChanged,
    ChapterStarted {
        chapter: u32,
    },
    /// The auto project-translate run advanced to a new volume. The UI re-points
    /// its "running volume" here so subsequent per-chapter events (whose numbers
    /// repeat across volumes) land on the right volume.
    VolumeStarted {
        vol: u32,
        label: Option<String>,
    },
    /// A volume-level pre-extraction pass is running before chapter translation.
    PrepassStarted {
        vol: u32,
    },
    PrepassFinished {
        vol: u32,
        characters: usize,
        terms: usize,
        examples: usize,
    },
    PrepassFailed {
        vol: u32,
        reason: String,
    },
    /// The loop watchdog tripped on a chapter (degenerate repetition or a stall):
    /// the whole chapter is being re-translated from scratch. `attempt` is the
    /// 1-based re-translate count, `max` the configured ceiling.
    ChapterLooping {
        chapter: u32,
        reason: String,
        attempt: u32,
        max: u32,
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
    /// A chunk exhausted its review attempts but was committed anyway (the last
    /// attempt's Thai, flagged in-file with a `[REVIEW NEEDED]` banner) so the
    /// chapter can still complete. `reason` is the reviewer's final objection.
    /// `salvaged` is false when no usable Thai existed and an empty chunk was
    /// committed — the chunk is skipped, leaving the chapter partially translated.
    ChunkNeedsReview {
        chapter: u32,
        chunk: usize,
        attempts: u32,
        reason: String,
        salvaged: bool,
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
        /// Whole-run cumulative usage (drives the run meter).
        run: UsageStats,
        /// Current chapter's running sub-total (drives the chapter meter).
        chapter: UsageStats,
    },
    /// One chapter finished a run: fold `delta` (this run's spend on the chapter)
    /// into the in-memory chapter total. Mirrors the VOLUME.md persistence.
    ChapterUsage {
        chapter: u32,
        delta: UsageStats,
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
        /// Of the `chapters_done`, how many completed with ≥1 chunk needing review.
        chapters_need_review: u32,
        /// True when the run ended because the user requested Stop.
        stopped: bool,
        /// Whole-run usage/cost total, used to finalize the durable run history.
        run: UsageStats,
    },
    Error {
        context: String,
        msg: String,
    },

    UpdateAvailable {
        version: String,
    },
    /// Auto-update finished: the new binary is installed and goes live on the
    /// next launch. Carries the version just installed.
    UpdateInstalled {
        version: String,
    },

    ImportProgress {
        done: usize,
        total: usize,
        label: String,
    },
    ImportFinished {
        project_id: String,
        /// The volume that was just imported, so the UI can land on it (a fresh
        /// import is Vol.01; an "add volume" lands on the new volume).
        vol: u32,
    },

    /// A volume-synopsis translation finished — folded into the open synopsis
    /// editor (import wizard step or standalone overlay).
    SynopsisTranslated {
        text: String,
    },
    SynopsisFailed {
        msg: String,
    },

    /// Per-format progress while exporting a volume to deliverable files.
    ExportProgress {
        done: usize,
        total: usize,
        /// The format currently being written (e.g. "EPUB").
        label: String,
    },
    /// A volume export finished: the files written and any non-fatal warnings
    /// (chapters still NeedsReview / missing a translation / dangling images).
    ExportFinished {
        paths: Vec<PathBuf>,
        warnings: Vec<String>,
    },

    RemoteAuthCode {
        user_code: String,
        verification_uri: String,
        /// GitHub's code-prefilled URL, if provided.
        verification_uri_complete: String,
    },
    RemoteAuthPending,
    RemotePaired {
        login: String,
        device_id: String,
        device_token: String,
    },
    RemoteAuthError {
        msg: String,
    },
    /// Browser URL for Codex OAuth.
    CodexAuthUrl {
        url: String,
    },
    /// New Codex credentials to persist.
    CodexSignedIn {
        auth: Box<crate::codex::CodexAuth>,
    },
    CodexAuthError {
        msg: String,
    },
    /// Model ids fetched from the Codex backend for the Settings picker.
    CodexModels {
        models: Vec<String>,
    },
    RemoteStatus {
        state: crate::remote::protocol::RemoteState,
        watchers: u32,
    },
    RemoteCommand(crate::remote::protocol::RemoteCommand),

    // Refine chat agent (src/agents/refine.rs -> UI).
    RefineDelta {
        delta: String,
    },
    /// Streamed reasoning shown collapsibly.
    RefineReasoning {
        delta: String,
    },
    /// Per-round token usage for the live turn (accumulates input/output counts).
    RefineUsage {
        prompt_tokens: u32,
        completion_tokens: u32,
    },
    RefineMessageDone,
    RefineToolInvoked {
        tool: String,
        summary: String,
    },
    RefineEditApplied {
        kind: String,
        summary: String,
    },
    /// Drives `/undo`, `/diff`, and one coalesced project re-scan.
    RefineChapterEdited {
        vol: u32,
        ch: u32,
    },
    RefineError {
        msg: String,
    },
    /// `session` lets App drop updates from a stale agent.
    RefineThreadUpdated {
        session: String,
        messages: Vec<crate::llm::Message>,
    },
    /// Current task checklist; replaces the prior plan.
    RefinePlanUpdated {
        steps: Vec<PlanStep>,
    },
    /// Edit approval prompt for a paused mutating tool.
    RefineApprovalRequest {
        id: u64,
        summary: String,
        diff: String,
    },
    /// Blocking `ask_user` prompt.
    RefineDecisionRequest {
        id: u64,
        question: String,
        options: Vec<String>,
    },
    RefineRequest(RefineRequest),
}

/// Heavier Refine operations routed back through `App`.
#[derive(Debug, Clone)]
pub enum RefineRequest {
    Retranslate { vol: u32, chapters: Vec<u32> },
    RefineChapter { vol: u32, ch: u32, feedback: String },
}

/// One Refine plan step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    pub step: String,
    #[serde(default)]
    pub status: PlanStepStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
}

/// Clonable sender handle background tasks use to talk to the UI.
#[derive(Clone)]
pub struct EventTx(pub tokio::sync::mpsc::UnboundedSender<AppEvent>);

impl EventTx {
    pub fn send(&self, e: AppEvent) {
        let _ = self.0.send(e);
    }
}

#[cfg(test)]
mod progress_tests {
    use super::*;

    fn ch(number: u32, kind: ChapterKind, status: ChapterStatus) -> Chapter {
        Chapter {
            number,
            title: String::new(),
            kind,
            status,
            source_segments: 0,
            total_chunks: 0,
            committed_chunks: 0,
            skipped_chunks: 0,
            last_run: None,
            usage: UsageStats::default(),
        }
    }

    fn project(volumes: Vec<Vec<Chapter>>) -> Project {
        Project {
            id: "p".into(),
            dir: PathBuf::from("/tmp/p"),
            title: "p".into(),
            title_th: String::new(),
            created: None,
            touched: None,
            volumes: volumes
                .into_iter()
                .enumerate()
                .map(|(i, chapters)| Volume {
                    number: i as u32 + 1,
                    dir: PathBuf::from("/tmp/p"),
                    label: None,
                    chapters,
                })
                .collect(),
            models: None,
        }
    }

    #[test]
    fn draft_when_nothing_translated() {
        let p = project(vec![vec![
            ch(1, ChapterKind::Prose, ChapterStatus::Pending),
            ch(2, ChapterKind::Prose, ChapterStatus::Pending),
        ]]);
        let pr = p.translation_progress();
        assert_eq!(pr.status, ProjectStatus::Draft);
        assert_eq!((pr.done, pr.total), (0, 2));
    }

    #[test]
    fn in_progress_when_some_done_or_active() {
        let partial = project(vec![vec![
            ch(1, ChapterKind::Prose, ChapterStatus::Done),
            ch(2, ChapterKind::Prose, ChapterStatus::Pending),
        ]]);
        assert_eq!(
            partial.translation_progress().status,
            ProjectStatus::InProgress
        );

        let running = project(vec![vec![
            ch(1, ChapterKind::Prose, ChapterStatus::Translating),
            ch(2, ChapterKind::Prose, ChapterStatus::Pending),
        ]]);
        assert_eq!(
            running.translation_progress().status,
            ProjectStatus::InProgress
        );
    }

    #[test]
    fn done_spans_all_volumes_and_ignores_non_prose() {
        // Image-only / empty pages don't gate completion; NeedsReview counts as done.
        let p = project(vec![
            vec![
                ch(1, ChapterKind::Prose, ChapterStatus::Done),
                ch(2, ChapterKind::ImageOnly, ChapterStatus::Pending),
            ],
            vec![ch(1, ChapterKind::Prose, ChapterStatus::NeedsReview)],
        ]);
        let pr = p.translation_progress();
        assert_eq!(pr.status, ProjectStatus::Done);
        assert_eq!((pr.done, pr.total), (2, 2));
    }

    #[test]
    fn second_volume_pending_keeps_project_in_progress() {
        // Finishing vol 1 but adding an untranslated vol 2 is not "done".
        let p = project(vec![
            vec![ch(1, ChapterKind::Prose, ChapterStatus::Done)],
            vec![ch(1, ChapterKind::Prose, ChapterStatus::Pending)],
        ]);
        assert_eq!(p.translation_progress().status, ProjectStatus::InProgress);
    }
}

#[cfg(test)]
mod provider_model_tests {
    use super::*;

    #[test]
    fn legacy_string_config_deserializes_to_openrouter_agent() {
        // Pre-multi-provider configs stored each agent as a bare model-id string.
        let json = r#""google/gemini-3-flash-preview""#;
        let a: AgentModel = serde_json::from_str(json).unwrap();
        assert_eq!(a.provider, Provider::OpenRouter);
        assert_eq!(a.model, "google/gemini-3-flash-preview");
        assert_eq!(a.effort, None);
    }

    #[test]
    fn full_agent_object_round_trips() {
        let a = AgentModel::new(Provider::Tokenrouter, "google/gemini-3-flash-preview", Some(Effort::High));
        let json = serde_json::to_string(&a).unwrap();
        let back: AgentModel = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
        // Provider serializes kebab-case, effort lowercase.
        assert!(json.contains("\"tokenrouter\""));
        assert!(json.contains("\"high\""));
    }

    #[test]
    fn object_defaults_provider_and_effort_when_omitted() {
        let a: AgentModel = serde_json::from_str(r#"{"model":"x"}"#).unwrap();
        assert_eq!(a.provider, Provider::OpenRouter);
        assert_eq!(a.effort, None);
    }

    #[test]
    fn reasoning_param_maps_effort_or_is_none() {
        assert!(AgentModel::openrouter("x").reasoning_param().is_none());
        let p = AgentModel::new(Provider::Codex, "x", Some(Effort::Xhigh))
            .reasoning_param()
            .unwrap();
        assert_eq!(p, serde_json::json!({"effort": "xhigh"}));
    }

    #[test]
    fn legacy_modelset_with_bare_strings_still_loads() {
        // A whole ModelSet written by an old build (all strings, no `refine`).
        let json = r#"{
            "orchestrator": "a/o",
            "translator": "a/t",
            "reviewer": "a/r"
        }"#;
        let ms: ModelSet = serde_json::from_str(json).unwrap();
        assert_eq!(ms.translator, AgentModel::openrouter("a/t"));
        assert_eq!(ms.refine, default_refine_model()); // serde default fills it
    }

    #[test]
    fn effort_cycles_through_all_levels_and_back_to_off() {
        let mut e = None;
        let order = [
            Some(Effort::Minimal),
            Some(Effort::Low),
            Some(Effort::Medium),
            Some(Effort::High),
            Some(Effort::Xhigh),
            None,
        ];
        for want in order {
            e = Effort::cycled(e);
            assert_eq!(e, want);
        }
    }
}
