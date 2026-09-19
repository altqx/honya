//! Settings, described once.
//!
//! The settings modal used to be 536 lines of imperative `Vec<Line>` building,
//! where every row restated how a label is padded, how a secret is masked and
//! where the caret goes — and a second, parallel list decided which rows
//! belonged to which tab. Here each setting is declared once, and the modal is
//! generated from the declarations.
//!
//! **Declaration order is the on-screen order**, which makes [`ORDER`] the
//! single source of truth it always claimed to be. The two indices other code
//! needs — the API-key row and the first per-feature toggle — are now derived
//! from that order rather than written down separately and kept in step by
//! hand, which is the kind of pairing that silently rots when a row is
//! inserted.

/// One focusable settings row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SField {
    OrchProvider,
    OrchModel,
    OrchEffort,
    TransProvider,
    TransModel,
    TransEffort,
    ReviewProvider,
    ReviewModel,
    ReviewEffort,
    RefineProvider,
    RefineModel,
    RefineEffort,
    OpenRouterKey,
    TokenrouterKey,
    GoogleKey,
    CloudflareAccount,
    CloudflareToken,
    PreferredLanguageField,
    MaxAttempts,
    ContinuitySentences,
    LoopStall,
    Retranslates,
    ServiceTierField,
    ParallelLookahead,
    ChunkTargetTokens,
    ChunkHardCapTokens,
    PrepassExtract,
    CoherenceCheck,
    SystemOneEnabled,
    GateMode,
    GateProvider,
    GateModel,
    GateKey,
    GateConfidence,
    FeatAudit,
    FeatContinuity,
    FeatEntityAlignment,
    FeatSegmentation,
    FeatReferenceScope,
    UpdateModeField,
    ReleaseChannelField,
}

/// Which category rail entry a row sits under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Agents,
    Providers,
    Pipeline,
    Appearance,
    Account,
}

impl Group {
    pub const ALL: [Group; 5] = [
        Group::Agents,
        Group::Providers,
        Group::Pipeline,
        Group::Appearance,
        Group::Account,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Group::Agents => "Agents",
            Group::Providers => "Providers",
            Group::Pipeline => "Pipeline",
            Group::Appearance => "Appearance",
            Group::Account => "Account",
        }
    }

    /// Rows belonging to this group, as indices into [`ORDER`].
    pub fn fields(self) -> Vec<u8> {
        ORDER
            .iter()
            .enumerate()
            .filter(|(_, d)| d.group == self)
            .map(|(i, _)| i as u8)
            .collect()
    }

    /// The first row of this group, if it has any. Account is actions only.
    pub fn first_field(self) -> Option<u8> {
        ORDER
            .iter()
            .position(|d| d.group == self)
            .map(|i| i as u8)
    }

    pub fn of_field(field: u8) -> Group {
        ORDER
            .get(field as usize)
            .map(|d| d.group)
            .unwrap_or(Group::Agents)
    }

    pub fn cycled(self, forward: bool) -> Group {
        let i = Group::ALL.iter().position(|g| *g == self).unwrap_or(0);
        let n = Group::ALL.len();
        Group::ALL[if forward { (i + 1) % n } else { (i + n - 1) % n }]
    }
}

/// How a row is edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Free text.
    Text,
    /// A masked secret, editable at the end only.
    Secret,
    /// Digits within a range, with stepper arrows.
    Number { min: i64, max: i64 },
    /// One of a list the caller resolves, since some lists are live.
    Select,
    /// On or off.
    Toggle,
}

/// One declared setting.
#[derive(Debug, Clone, Copy)]
pub struct Def {
    pub field: SField,
    pub label: &'static str,
    /// Shown under the form while this row is focused.
    pub help: &'static str,
    pub group: Group,
    pub kind: Kind,
}

const fn def(
    field: SField,
    label: &'static str,
    group: Group,
    kind: Kind,
    help: &'static str,
) -> Def {
    Def {
        field,
        label,
        help,
        group,
        kind,
    }
}

/// Every setting, in the order it appears on screen.
pub const ORDER: &[Def] = &[
    // --- Agents: each of the four picks its own provider, model and effort.
    def(SField::OrchProvider, "Orchestrator provider", Group::Agents, Kind::Select,
        "Who serves the metadata turn that records new characters and terms."),
    def(SField::OrchModel, "Orchestrator model", Group::Agents, Kind::Text,
        "Model id for the metadata turn. It calls tools, so it needs tool support."),
    def(SField::OrchEffort, "Orchestrator effort", Group::Agents, Kind::Select,
        "Reasoning effort, sent as the request's reasoning parameter."),
    def(SField::TransProvider, "Translator provider", Group::Agents, Kind::Select,
        "Who drafts each chunk. This is where most of the run's tokens go."),
    def(SField::TransModel, "Translator model", Group::Agents, Kind::Text,
        "Model id for drafting. Prose quality here sets the ceiling for the run."),
    def(SField::TransEffort, "Translator effort", Group::Agents, Kind::Select,
        "Higher effort costs more per chunk; the Reviewer can often make up for less."),
    def(SField::ReviewProvider, "Reviewer provider", Group::Agents, Kind::Select,
        "Who checks each draft and writes the feedback a retry gets."),
    def(SField::ReviewModel, "Reviewer model", Group::Agents, Kind::Text,
        "Model id for review. Also used for the coherence sweep."),
    def(SField::ReviewEffort, "Reviewer effort", Group::Agents, Kind::Select,
        "Reasoning effort for review."),
    def(SField::RefineProvider, "Refine provider", Group::Agents, Kind::Select,
        "Who answers on the Refine tab."),
    def(SField::RefineModel, "Refine model", Group::Agents, Kind::Text,
        "Model id for the Refine chat agent."),
    def(SField::RefineEffort, "Refine effort", Group::Agents, Kind::Select,
        "Reasoning effort for Refine."),

    // --- Providers: keys. Any set by the environment win and show read-only.
    def(SField::OpenRouterKey, "OpenRouter key", Group::Providers, Kind::Secret,
        "HONYA_API_KEY or OPENROUTER_API_KEY override this."),
    def(SField::TokenrouterKey, "Tokenrouter key", Group::Providers, Kind::Secret,
        "Same wire format as OpenRouter, different base URL and key."),
    def(SField::GoogleKey, "Google key", Group::Providers, Kind::Secret,
        "For Gemini models served directly rather than through a router."),
    def(SField::CloudflareAccount, "Cloudflare account", Group::Providers, Kind::Text,
        "Account id for Workers AI."),
    def(SField::CloudflareToken, "Cloudflare token", Group::Providers, Kind::Secret,
        "API token for Workers AI."),

    // --- Pipeline: how a run is shaped.
    def(SField::PreferredLanguageField, "Preferred language", Group::Pipeline, Kind::Select,
        "Seeds the first choice in the new-project wizard. Each project then owns its own."),
    def(SField::MaxAttempts, "Retry attempts", Group::Pipeline, Kind::Number { min: 1, max: 9 },
        "How many times a chunk is redrafted before it is flagged for review."),
    def(SField::ContinuitySentences, "Continuity sentences", Group::Pipeline, Kind::Number { min: 0, max: 12 },
        "How much of the previous chunk's translation the next one is shown."),
    def(SField::LoopStall, "Loop watchdog (s)", Group::Pipeline, Kind::Number { min: 0, max: 3600 },
        "Seconds of silence before a stuck chunk is retried. Zero disables it."),
    def(SField::Retranslates, "Loop re-translates", Group::Pipeline, Kind::Number { min: 0, max: 9 },
        "How many times a whole chapter may be redrafted before giving up."),
    def(SField::ServiceTierField, "Service tier", Group::Pipeline, Kind::Select,
        "Provider queue to request. Flex trades latency for cost."),
    def(SField::ParallelLookahead, "Parallel lookahead", Group::Pipeline, Kind::Toggle,
        "Start the next chunk's draft while the current one is still in review."),
    def(SField::ChunkTargetTokens, "Chunk target tokens", Group::Pipeline, Kind::Number { min: 200, max: 8000 },
        "The size a chunk aims for. Larger chunks hold more context but retry more expensively."),
    def(SField::ChunkHardCapTokens, "Chunk hard cap", Group::Pipeline, Kind::Number { min: 200, max: 16000 },
        "A chunk is never allowed past this, whatever the target says."),
    def(SField::PrepassExtract, "Prepass extract", Group::Pipeline, Kind::Toggle,
        "Seed characters and glossary terms from the raw text before translating."),
    def(SField::CoherenceCheck, "Coherence sweep", Group::Pipeline, Kind::Toggle,
        "Re-read each finished chapter end to end and flag drift."),

    // --- System One: one master switch, one transport, a toggle per judgement.
    def(SField::SystemOneEnabled, "System One (Jev)", Group::Pipeline, Kind::Toggle,
        "Master switch. Off restores the deterministic path everywhere at once."),
    def(SField::GateMode, "Review gate", Group::Pipeline, Kind::Select,
        "Off · advisory · standalone. The gate can only save a reviewer call, never block a chunk."),
    def(SField::GateProvider, "Judgement provider", Group::Pipeline, Kind::Select,
        "OpenRouter's decisions endpoint, or TypeSafe directly."),
    def(SField::GateModel, "Judgement model", Group::Pipeline, Kind::Text,
        "Decisions model id: typesafe/jev-1.13 via OpenRouter, jev-latest via TypeSafe."),
    def(SField::GateKey, "TypeSafe key", Group::Pipeline, Kind::Secret,
        "Only needed when talking to TypeSafe directly."),
    def(SField::GateConfidence, "Min confidence", Group::Pipeline, Kind::Number { min: 50, max: 99 },
        "Below this, a judgement defers to the deterministic answer."),
    def(SField::FeatAudit, "· audit checks", Group::Pipeline, Kind::Toggle,
        "Decide the audit's three judgement calls instead of matching word lists."),
    def(SField::FeatContinuity, "· continuity echo", Group::Pipeline, Kind::Toggle,
        "Catch reworded echoes of the previous chunk, not just verbatim ones."),
    def(SField::FeatEntityAlignment, "· character alignment", Group::Pipeline, Kind::Toggle,
        "Connect a character's names, titles and nicknames by judgement rather than by spelling."),
    def(SField::FeatSegmentation, "· spine classification", Group::Pipeline, Kind::Toggle,
        "Ask what each EPUB page is instead of matching per-publisher constants."),
    def(SField::FeatReferenceScope, "· reference scope", Group::Pipeline, Kind::Toggle,
        "Add what the substring test missed from a chunk's reference bundle."),

    // --- Appearance.
    def(SField::UpdateModeField, "Auto-update", Group::Appearance, Kind::Select,
        "Whether a new release is fetched and staged automatically."),
    def(SField::ReleaseChannelField, "Update channel", Group::Appearance, Kind::Select,
        "Stable, or pre-release builds."),
];

/// Number of focusable rows.
pub fn count() -> u8 {
    ORDER.len() as u8
}

/// The declaration for `field`.
pub fn at(field: u8) -> Option<&'static Def> {
    ORDER.get(field as usize)
}

/// Index of `f` within [`ORDER`].
pub fn index_of(f: SField) -> u8 {
    ORDER.iter().position(|d| d.field == f).unwrap_or(0) as u8
}

/// Index of the OpenRouter key row, derived rather than written down twice.
pub fn key_field() -> u8 {
    index_of(SField::OpenRouterKey)
}

/// The per-feature toggle a row drives, if it is one.
pub fn feature_of(f: SField) -> Option<crate::model::SystemOneFeature> {
    use crate::model::SystemOneFeature as F;
    Some(match f {
        SField::FeatAudit => F::Audit,
        SField::FeatContinuity => F::Continuity,
        SField::FeatEntityAlignment => F::EntityAlignment,
        SField::FeatSegmentation => F::Segmentation,
        SField::FeatReferenceScope => F::ReferenceScope,
        _ => return None,
    })
}

impl SField {
    /// Free text or a secret — typed into rather than stepped through.
    pub fn is_text(self) -> bool {
        matches!(
            at(index_of(self)).map(|d| d.kind),
            Some(Kind::Text) | Some(Kind::Secret) | Some(Kind::Number { .. })
        )
    }

    pub fn is_numeric(self) -> bool {
        matches!(at(index_of(self)).map(|d| d.kind), Some(Kind::Number { .. }))
    }

    pub fn is_secret(self) -> bool {
        matches!(at(index_of(self)).map(|d| d.kind), Some(Kind::Secret))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_order_is_the_screen_order_and_has_no_duplicates() {
        let mut seen: Vec<SField> = ORDER.iter().map(|d| d.field).collect();
        let total = seen.len();
        seen.sort_by_key(|f| format!("{f:?}"));
        seen.dedup();
        assert_eq!(seen.len(), total, "a setting is declared twice");
        for (i, d) in ORDER.iter().enumerate() {
            assert_eq!(index_of(d.field), i as u8, "{:?} is not at its own index", d.field);
        }
    }

    #[test]
    fn derived_indices_track_the_order_rather_than_being_written_down_twice() {
        assert_eq!(ORDER[key_field() as usize].field, SField::OpenRouterKey);
        // Every feature toggle is contiguous from the first, which is what
        // lets a caller walk them alongside `SystemOneFeature::ALL`.
        let start = index_of(SField::FeatAudit) as usize;
        for (n, _) in crate::model::SystemOneFeature::ALL.iter().enumerate() {
            assert!(
                feature_of(ORDER[start + n].field).is_some(),
                "row {} is not a feature toggle",
                start + n
            );
        }
    }

    #[test]
    fn every_group_except_account_owns_a_contiguous_run_of_rows() {
        for g in Group::ALL {
            let fields = g.fields();
            if g == Group::Account {
                assert!(fields.is_empty(), "Account holds actions, not rows");
                assert!(g.first_field().is_none());
                continue;
            }
            assert!(!fields.is_empty(), "{:?} has no rows", g);
            for pair in fields.windows(2) {
                assert_eq!(
                    pair[1],
                    pair[0] + 1,
                    "{:?} is not contiguous, so arrowing through it would jump groups",
                    g
                );
            }
            assert_eq!(g.first_field(), Some(fields[0]));
        }
    }

    #[test]
    fn every_row_knows_its_own_group() {
        for (i, d) in ORDER.iter().enumerate() {
            assert_eq!(Group::of_field(i as u8), d.group, "{:?}", d.field);
        }
    }

    #[test]
    fn every_row_says_what_it_is_for() {
        for d in ORDER {
            assert!(!d.label.is_empty(), "{:?} has no label", d.field);
            assert!(
                d.help.len() > 20,
                "{:?} has no useful help text: {:?}",
                d.field,
                d.help
            );
            assert!(
                d.help.ends_with('.'),
                "{:?} help should read as a sentence: {:?}",
                d.field,
                d.help
            );
        }
    }

    #[test]
    fn numeric_ranges_are_sane() {
        for d in ORDER {
            if let Kind::Number { min, max } = d.kind {
                assert!(min < max, "{:?} has an empty range", d.field);
            }
        }
    }

    #[test]
    fn group_cycling_wraps_both_ways() {
        assert_eq!(Group::Agents.cycled(false), Group::Account);
        assert_eq!(Group::Account.cycled(true), Group::Agents);
        assert_eq!(Group::Agents.cycled(true), Group::Providers);
    }

    #[test]
    fn the_four_previously_unreachable_settings_are_now_declared() {
        // These exist in AppConfig but had no row, so they could only be
        // changed by editing config.json by hand.
        for f in [
            SField::ChunkTargetTokens,
            SField::ChunkHardCapTokens,
            SField::PrepassExtract,
            SField::CoherenceCheck,
        ] {
            let d = at(index_of(f)).expect("declared");
            assert_eq!(d.group, Group::Pipeline);
        }
    }
}
