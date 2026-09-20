//! Sub-agent roles: what a delegated child is allowed to do, and what it is
//! told it is for.
//!
//! A role is a capability gate, not a hint. The role decides which tools the
//! child's request advertises *and* is checked again when a call arrives, so a
//! child that invents a tool name it was never offered gets a refusal rather
//! than an edit.

use crate::llm::Tool;
use crate::model::TargetLanguage;

/// Tools every role gets: reading, searching, planning, asking.
const READ: &[&str] = &[
    "list_volumes",
    "list_chapters",
    "read_chapter",
    "grep_chapter",
    "read_meta",
    "list_flagged_chunks",
    "read_lexicon",
    "search_project",
    "update_plan",
    "ask_user",
    "message_subagent",
];

/// Chapter prose.
const EDIT: &[&str] = &[
    "replace_chapter_text",
    "edit_chapter",
    "multi_edit_chapter",
    "replace_across_project",
    "retranslate_chapter",
    "refine_chapter_with_feedback",
];

/// Reference data: who people are, what terms mean, how the book reads.
const LEXICON: &[&str] = &[
    "upsert_character",
    "merge_character",
    "remove_character",
    "upsert_glossary_term",
    "remove_glossary_term",
    "set_recap",
    "set_chapter_summary",
    "set_synopsis",
    "append_style_note",
    "add_style_example",
    "add_continuity_note",
];

/// Delegation. Only `General` may fan out further.
const SPAWN: &[&str] = &[
    "task",
    "list_interrupted_subagents",
    "resume_subagent",
    "subagent_output",
    "cancel_subagent",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SubagentRole {
    /// Reads and reports. Cannot change anything.
    Explore,
    /// Reads and rewrites chapter prose.
    Editor,
    /// Reads and maintains characters, glossary, style and recaps.
    Lexicon,
    #[default]
    General,
}

impl SubagentRole {
    /// Unknown names fall back to `General` rather than failing the call: a
    /// wrong role should cost capability breadth, not the whole task.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "explore" => Self::Explore,
            "editor" => Self::Editor,
            "lexicon" => Self::Lexicon,
            _ => Self::General,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Explore => "explore",
            Self::Editor => "editor",
            Self::Lexicon => "lexicon",
            Self::General => "general",
        }
    }

    #[cfg(test)]
    pub const ALL: [SubagentRole; 4] = [
        SubagentRole::Explore,
        SubagentRole::Editor,
        SubagentRole::Lexicon,
        SubagentRole::General,
    ];

    pub fn allows(self, tool: &str) -> bool {
        if READ.contains(&tool) {
            return true;
        }
        match self {
            Self::Explore => false,
            Self::Editor => EDIT.contains(&tool),
            Self::Lexicon => LEXICON.contains(&tool),
            Self::General => EDIT.contains(&tool) || LEXICON.contains(&tool) || SPAWN.contains(&tool),
        }
    }

    /// The child's advertised tool list.
    pub fn tools(self) -> Vec<Tool> {
        super::refine::refine_tools_vec()
            .into_iter()
            .filter(|t| self.allows(&t.function.name))
            .collect()
    }

    /// Why a tool was refused, in terms the model can act on.
    pub fn refusal(self, tool: &str) -> String {
        format!(
            "tool `{tool}` is not available to a `{}` sub-agent; report what you found and let the parent make the change",
            self.label()
        )
    }

    pub fn system_prompt(self, target: TargetLanguage) -> String {
        let base = match target {
            TargetLanguage::Thai => SUBAGENT_SYSTEM_THAI,
            TargetLanguage::English => SUBAGENT_SYSTEM_ENGLISH,
        };
        format!("{base}\n\n{}", self.charge())
    }

    fn charge(self) -> &'static str {
        match self {
            Self::Explore => "Your role is `explore`: gather evidence and report it. You have no editing tools at all — not for chapters and not for reference data — so do not plan an edit, do not promise one, and do not ask for one. Read widely, quote what you find with chapter and line, and end with findings the parent can act on.",
            Self::Editor => "Your role is `editor`: change chapter prose and nothing else. You cannot touch characters, glossary, style or recaps, so when an edit implies a reference-data change, make the prose edit and name the metadata change in your report for the parent to apply.",
            Self::Lexicon => "Your role is `lexicon`: maintain characters, glossary, style notes, recaps and summaries. You cannot edit chapter text, so when the reference data you fix implies a prose change, record the reference change and name the affected chapters in your report.",
            Self::General => "Your role is `general`: you hold the full tool set, including delegation. Spawn sub-agents only for large independent disjoint scopes; nesting is app-bounded.",
        }
    }
}

/// What a delegated child is, as one value: it travels from the `task` call
/// into the checkpoint, and back out again on resume.
#[derive(Debug, Clone)]
pub struct SubagentSpec {
    pub task: String,
    pub scope: Option<String>,
    pub role: SubagentRole,
    pub depth: usize,
}

const SUBAGENT_SYSTEM_THAI: &str = "You are a focused sub-agent inside honya's Refine system completing ONE self-contained parent-delegated task. Use the project tools to gather evidence, make surgical changes, verify them, then report chapters/terms/characters touched. Keep Thai idiomatic; preserve scene breaks, image links, and Markdown. When a female character uses `僕/ぼく/ボク` as her self-pronoun, render it as `เรา`, never `ผม` or `โบคุ`; identify the character from context rather than inferring gender from `僕` alone.";

const SUBAGENT_SYSTEM_ENGLISH: &str = "You are a focused sub-agent inside honya's Refine system, completing one self-contained task delegated by a parent agent. Read the real Japanese source, English translation, and reference data before editing. Make only evidence-backed surgical changes, keep the English idiomatic and publication-ready for native light-novel readers, preserve scene breaks, image links, and Markdown, verify the result, then report the chapters and metadata changed.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explore_subagent_cannot_edit_anything() {
        let r = SubagentRole::Explore;
        assert!(r.allows("read_chapter"));
        assert!(r.allows("grep_chapter"));
        assert!(!r.allows("edit_chapter"));
        assert!(!r.allows("multi_edit_chapter"));
        assert!(!r.allows("upsert_character"));
        assert!(!r.allows("task"));
        let names: Vec<_> = r.tools().into_iter().map(|t| t.function.name).collect();
        assert!(names.contains(&"read_chapter".to_string()));
        assert!(!names.contains(&"edit_chapter".to_string()));
    }

    #[test]
    fn editor_and_lexicon_do_not_overlap_outside_reads() {
        assert!(SubagentRole::Editor.allows("edit_chapter"));
        assert!(!SubagentRole::Editor.allows("upsert_character"));
        assert!(SubagentRole::Lexicon.allows("upsert_character"));
        assert!(!SubagentRole::Lexicon.allows("edit_chapter"));
        // Reading is common ground; neither can work blind.
        for r in [SubagentRole::Editor, SubagentRole::Lexicon] {
            assert!(r.allows("read_chapter"));
            assert!(r.allows("read_lexicon"));
        }
    }

    #[test]
    fn only_general_delegates_further() {
        for r in SubagentRole::ALL {
            assert_eq!(r.allows("task"), r == SubagentRole::General);
        }
    }

    #[test]
    fn every_advertised_tool_is_one_the_role_allows() {
        for r in SubagentRole::ALL {
            for t in r.tools() {
                assert!(r.allows(&t.function.name), "{} advertised {}", r.label(), t.function.name);
            }
        }
    }

    #[test]
    fn an_unknown_role_name_is_general() {
        assert_eq!(SubagentRole::parse("wizard"), SubagentRole::General);
        assert_eq!(SubagentRole::parse("EXPLORE"), SubagentRole::Explore);
        assert_eq!(SubagentRole::parse(" editor "), SubagentRole::Editor);
    }

    #[test]
    fn a_role_says_what_it_is_for_in_its_prompt() {
        for r in SubagentRole::ALL {
            let p = r.system_prompt(TargetLanguage::Thai);
            assert!(p.contains("sub-agent"));
            assert!(p.contains(r.label()), "{} prompt omits its own role", r.label());
        }
    }
}
