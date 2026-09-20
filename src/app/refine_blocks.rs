//! The Refine transcript as addressable blocks.
//!
//! The transcript used to be one markdown string rendered as one `Paragraph`,
//! which meant nothing in it could be pointed at: no per-message zone, no
//! per-message fold, no way to open anything. A block owns its own lines, its
//! own fold state and its own cache entry, so a cursor, a click and a copy all
//! have something to land on — and a streamed delta rebuilds one block rather
//! than the whole conversation.

use std::hash::{Hash, Hasher};

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::llm::{Message, Role};
use crate::theme::Theme;
use crate::ui::text::truncate_cols;

/// Kept as the GUI's vocabulary: it draws roles, not folds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnRole {
    User,
    Assistant,
    Reasoning,
    Tool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Ok,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    User,
    Assistant,
    Reasoning,
    Tool {
        name: String,
        /// The tool-call id, so a result arriving later finds its own block.
        call: String,
        status: ToolStatus,
    },
    /// A delegated run; `run` is the sub-agent id the events carry.
    Subagent { run: String },
    /// Compaction, errors, an answered question — things that happened *to*
    /// the conversation rather than in it.
    Notice,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub id: u32,
    /// Bumped by every mutation, so the render cache key is exact rather than
    /// a guess from the body's length.
    rev: u32,
    pub kind: BlockKind,
    /// What the block says when collapsed, and the prose when it is not.
    pub body: String,
    /// What expanding reveals: a tool's arguments and result, a sub-agent's
    /// report. Empty when there is nothing more to show.
    pub detail: String,
    pub open: bool,
    pub streaming: bool,
}

impl Block {
    fn new(id: u32, kind: BlockKind, body: String, open: bool) -> Self {
        Self {
            id,
            rev: 0,
            kind,
            body,
            detail: String::new(),
            open,
            streaming: false,
        }
    }

    pub fn user(id: u32, text: String) -> Self {
        Self::new(id, BlockKind::User, text, true)
    }

    pub fn assistant(id: u32, text: String) -> Self {
        Self::new(id, BlockKind::Assistant, text, true)
    }

    /// Open while it streams, because watching it is the point; it folds itself
    /// away once the answer it was working towards arrives.
    pub fn reasoning(id: u32, text: String) -> Self {
        Self::new(id, BlockKind::Reasoning, text, true)
    }

    pub fn tool(id: u32, name: String, call: String, summary: String) -> Self {
        Self::new(
            id,
            BlockKind::Tool {
                name,
                call,
                status: ToolStatus::Running,
            },
            summary,
            false,
        )
    }

    pub fn subagent(id: u32, run: String, title: String) -> Self {
        Self::new(id, BlockKind::Subagent { run }, title, false)
    }

    pub fn notice(id: u32, text: String) -> Self {
        Self::new(id, BlockKind::Notice, text, true)
    }

    pub fn streaming(mut self) -> Self {
        self.streaming = true;
        self
    }

    pub fn role(&self) -> TurnRole {
        match self.kind {
            BlockKind::User => TurnRole::User,
            BlockKind::Assistant => TurnRole::Assistant,
            BlockKind::Reasoning => TurnRole::Reasoning,
            BlockKind::Tool { .. } | BlockKind::Subagent { .. } | BlockKind::Notice => {
                TurnRole::Tool
            }
        }
    }

    /// True when there is a second state worth having. A notice has one line
    /// and nothing behind it; folding it would only be a thing that does
    /// nothing when you press it.
    pub fn collapsible(&self) -> bool {
        match self.kind {
            BlockKind::Notice => false,
            BlockKind::Reasoning => !self.body.trim().is_empty(),
            BlockKind::Tool { .. } | BlockKind::Subagent { .. } => true,
            BlockKind::User | BlockKind::Assistant => {
                self.body.lines().count() > 1 || !self.detail.is_empty()
            }
        }
    }

    pub fn set_open(&mut self, open: bool) {
        if self.collapsible() && self.open != open {
            self.open = open;
            self.rev = self.rev.wrapping_add(1);
        }
    }

    pub fn toggle(&mut self) {
        let open = !self.open;
        self.set_open(open);
    }

    pub fn push_body(&mut self, text: &str) {
        self.body.push_str(text);
        self.rev = self.rev.wrapping_add(1);
    }

    pub fn set_detail(&mut self, detail: String) {
        self.detail = detail;
        self.rev = self.rev.wrapping_add(1);
    }

    pub fn set_tool_status(&mut self, next: ToolStatus) {
        if let BlockKind::Tool { status, .. } = &mut self.kind
            && *status != next
        {
            *status = next;
            self.rev = self.rev.wrapping_add(1);
        }
    }

    pub fn settle(&mut self) {
        if self.streaming {
            self.streaming = false;
            self.rev = self.rev.wrapping_add(1);
        }
    }

    #[cfg(test)]
    pub fn tool_name(&self) -> Option<&str> {
        match &self.kind {
            BlockKind::Tool { name, .. } => Some(name),
            _ => None,
        }
    }

    pub fn tool_call(&self) -> Option<&str> {
        match &self.kind {
            BlockKind::Tool { call, .. } => Some(call),
            _ => None,
        }
    }

    pub fn subagent_id(&self) -> Option<&str> {
        match &self.kind {
            BlockKind::Subagent { run } => Some(run),
            _ => None,
        }
    }

    /// What this block says, without the role heading a clipboard copy wants —
    /// for a caller that draws the role itself.
    pub fn text(&self) -> String {
        let head = match &self.kind {
            BlockKind::Tool { name, .. } => format!("{name} — "),
            BlockKind::Subagent { .. } => "sub-agent — ".to_string(),
            _ => String::new(),
        };
        let mut out = format!("{head}{}", self.body.trim_end());
        if !self.detail.trim().is_empty() {
            out.push('\n');
            out.push_str(self.detail.trim_end());
        }
        out
    }

    /// What ⌃B puts on the clipboard: the whole block, expanded, whatever the
    /// fold happens to be showing.
    pub fn to_markdown(&self) -> String {
        let head = match &self.kind {
            BlockKind::User => "### you\n\n".to_string(),
            BlockKind::Assistant => String::new(),
            BlockKind::Reasoning => "> thinking\n\n".to_string(),
            BlockKind::Tool { name, .. } => format!("`{name}`\n\n"),
            BlockKind::Subagent { run } => format!("sub-agent `{run}`\n\n"),
            BlockKind::Notice => String::new(),
        };
        let mut out = format!("{head}{}", self.body.trim_end());
        if !self.detail.trim().is_empty() {
            out.push_str("\n\n");
            out.push_str(self.detail.trim_end());
        }
        out
    }
}

/// Rebuild the transcript from stored messages, so a reloaded session and a
/// live one show the same thing.
pub fn display_blocks(messages: &[Message], compact_prefix: &str) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut next = 0u32;
    let mut id = || {
        next += 1;
        next
    };
    for (i, m) in messages.iter().enumerate() {
        match m.role {
            Role::User => {
                let Some(c) = &m.content else { continue };
                if c.starts_with(compact_prefix) {
                    out.push(Block::notice(
                        id(),
                        "context summarized — earlier messages compacted".to_string(),
                    ));
                } else {
                    out.push(Block::user(id(), user_display(c).to_string()));
                }
            }
            Role::Assistant => {
                if let Some(c) = &m.content
                    && !c.trim().is_empty()
                {
                    out.push(Block::assistant(id(), c.clone()));
                }
                for call in m.tool_calls.iter().flatten() {
                    // Plan calls render in the pinned panel.
                    if call.function.name == "update_plan" {
                        continue;
                    }
                    let result = tool_result_for(messages, i, &call.id);
                    let status = match &result {
                        Some(r) if r.contains("\"ok\":false") => ToolStatus::Failed,
                        Some(_) => ToolStatus::Ok,
                        None => ToolStatus::Running,
                    };
                    let mut block = Block::tool(
                        id(),
                        call.function.name.clone(),
                        call.id.clone(),
                        one_line(&call.function.arguments, 70),
                    );
                    block.set_tool_status(status);
                    block.set_detail(tool_detail(&call.function.arguments, result.as_deref()));
                    out.push(block);
                }
            }
            Role::Tool | Role::System => {}
        }
    }
    out
}

/// The result message that answered this call, searched forward from the
/// assistant turn that made it.
fn tool_result_for(messages: &[Message], from: usize, call_id: &str) -> Option<String> {
    messages[from + 1..]
        .iter()
        .take_while(|m| m.role == Role::Tool)
        .find(|m| m.tool_call_id.as_deref() == Some(call_id))
        .and_then(|m| m.content.clone())
}

pub fn tool_detail(args: &str, result: Option<&str>) -> String {
    let mut out = String::new();
    let args = args.trim();
    if !args.is_empty() && args != "{}" {
        out.push_str(args);
    }
    if let Some(result) = result.map(str::trim).filter(|r| !r.is_empty()) {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("→ ");
        out.push_str(result);
    }
    cap(&out, 4_000)
}

pub fn cap(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}\n… truncated")
}

/// Keep the newest lines. A sub-agent's transcript grows for as long as it
/// runs, and the end is the part worth keeping.
pub fn cap_tail(s: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= max_lines {
        return s.to_string();
    }
    let dropped = lines.len() - max_lines;
    let mut out = format!("… {dropped} earlier line(s) dropped\n");
    out.push_str(&lines[dropped..].join("\n"));
    out
}

pub fn one_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        flat.chars().take(max).collect::<String>() + "…"
    }
}

/// The scope, steering and approval-mode suffixes `build_user_message` appends
/// are addressed to the model, not to the reader.
fn user_display(content: &str) -> &str {
    let mut end = content.len();
    for marker in [
        "\n\n(In scope:",
        "\n\n[Mid-run steering:",
        "\n\n[Mid-run report from sub-agent",
        "\n\n[Approval mode:",
    ] {
        if let Some((head, _)) = content.split_once(marker) {
            end = end.min(head.len());
        }
    }
    &content[..end]
}

/// Everything a rendered block depends on. Exact, not inferred from lengths:
/// a status flip or a fold changes no byte of the body.
pub fn cache_key(b: &Block, width: u16, theme: &Theme) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b.id.hash(&mut h);
    b.rev.hash(&mut h);
    b.body.len().hash(&mut h);
    b.detail.len().hash(&mut h);
    b.open.hash(&mut h);
    b.streaming.hash(&mut h);
    width.hash(&mut h);
    crate::ui::markdown::theme_fingerprint(theme).hash(&mut h);
    h.finish()
}

/// The fold marker, which is part of the content because it depends on the
/// fold — unlike the selection rail, which is painted per frame.
fn marker(b: &Block) -> &'static str {
    if !b.collapsible() {
        "  "
    } else if b.open {
        "▾ "
    } else {
        "▸ "
    }
}

pub fn render(b: &Block, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let w = width.max(4) as usize;
    let faint = Style::default().fg(theme.ink_faint);
    let soft = Style::default().fg(theme.ink_soft);

    match &b.kind {
        BlockKind::User => {
            let mut lines = vec![Line::from(Span::styled(
                format!("{}› you", marker(b)),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ))];
            let shown = if b.open {
                b.body.trim_end().to_string()
            } else {
                first_line(&b.body, w.saturating_sub(4))
            };
            lines.extend(crate::ui::markdown::render(
                &shown,
                theme.translated_text,
                theme,
                w,
            ));
            lines.push(Line::raw(""));
            lines
        }
        BlockKind::Assistant => {
            let shown = if b.open {
                b.body.trim_end().to_string()
            } else {
                first_line(&b.body, w.saturating_sub(4))
            };
            let mut lines =
                crate::ui::markdown::render(&shown, theme.translated_text, theme, w);
            if b.collapsible() && !b.open {
                lines.insert(
                    0,
                    Line::from(Span::styled(format!("{}…", marker(b)), faint)),
                );
            }
            lines.push(Line::raw(""));
            lines
        }
        BlockKind::Reasoning => {
            let body = b.body.trim();
            if body.is_empty() {
                return Vec::new();
            }
            if !b.open {
                let n = body.lines().count().max(1);
                return vec![
                    Line::from(Span::styled(
                        truncate_cols(&format!("{}💭 thinking — {n} line(s)", marker(b)), w),
                        faint.add_modifier(Modifier::ITALIC),
                    )),
                    Line::raw(""),
                ];
            }
            let mut lines = vec![Line::from(Span::styled(
                format!("{}💭 thinking", marker(b)),
                faint.add_modifier(Modifier::ITALIC),
            ))];
            // Through the markdown renderer, not printed raw: reasoning comes
            // back with `**bold**` and `<!-- -->` dividers in it, and a quote
            // is what it is.
            let mut quoted = String::new();
            for line in body.lines() {
                quoted.push_str("> ");
                quoted.push_str(line);
                quoted.push('\n');
            }
            lines.extend(crate::ui::markdown::render(&quoted, theme.ink_faint, theme, w));
            lines.push(Line::raw(""));
            lines
        }
        BlockKind::Tool { name, status, .. } => {
            let (glyph, style) = match status {
                ToolStatus::Running => ("·", soft),
                ToolStatus::Ok => ("✓", Style::default().fg(theme.status_done)),
                ToolStatus::Failed => ("!", Style::default().fg(theme.status_failed)),
            };
            let head = format!("{}{glyph} {name}", marker(b));
            let summary = b.body.trim();
            let head = if summary.is_empty() {
                head
            } else {
                format!("{head} — {summary}")
            };
            let mut lines = vec![Line::from(Span::styled(truncate_cols(&head, w), style))];
            if b.open {
                for line in b.detail.lines() {
                    lines.push(Line::from(Span::styled(
                        truncate_cols(&format!("  │ {line}"), w),
                        faint,
                    )));
                }
            }
            lines
        }
        BlockKind::Subagent { .. } => {
            let mut lines = vec![Line::from(Span::styled(
                truncate_cols(&format!("{}◇ {}", marker(b), b.body.trim()), w),
                Style::default().fg(theme.accent),
            ))];
            if b.open {
                for line in b.detail.lines() {
                    lines.push(Line::from(Span::styled(
                        truncate_cols(&format!("  │ {line}"), w),
                        faint,
                    )));
                }
            }
            lines
        }
        BlockKind::Notice => vec![
            Line::from(Span::styled(
                truncate_cols(&format!("  {}", b.body.trim()), w),
                faint,
            )),
            Line::raw(""),
        ],
    }
}

fn first_line(body: &str, width: usize) -> String {
    let head = body.trim().lines().next().unwrap_or("").trim();
    truncate_cols(head, width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{FunctionCall, ToolCall};

    fn assistant_with_call(name: &str, args: &str, id: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: id.to_string(),
                kind: "function".to_string(),
                function: FunctionCall {
                    name: name.to_string(),
                    arguments: args.to_string(),
                },
            }]),
            tool_call_id: None,
            name: None,
        }
    }

    #[test]
    fn a_tool_block_carries_its_arguments_and_its_result() {
        let messages = vec![
            Message::user("fix chapter 3"),
            assistant_with_call("read_chapter", r#"{"vol":1,"ch":3}"#, "c1"),
            Message::tool_result("c1".to_string(), r#"{"ok":true,"message":"read"}"#.to_string()),
        ];
        let blocks = display_blocks(&messages, "[compacted]");
        let tool = blocks.iter().find(|b| b.tool_name() == Some("read_chapter")).unwrap();
        assert!(tool.detail.contains(r#""ch":3"#), "{}", tool.detail);
        assert!(tool.detail.contains("→"), "{}", tool.detail);
        assert!(matches!(
            tool.kind,
            BlockKind::Tool {
                status: ToolStatus::Ok,
                ..
            }
        ));
    }

    #[test]
    fn a_failed_tool_says_so_without_being_expanded() {
        let messages = vec![
            assistant_with_call("edit_chapter", "{}", "c1"),
            Message::tool_result("c1".to_string(), r#"{"ok":false,"message":"no match"}"#.to_string()),
        ];
        let blocks = display_blocks(&messages, "[compacted]");
        assert!(matches!(
            blocks[0].kind,
            BlockKind::Tool {
                status: ToolStatus::Failed,
                ..
            }
        ));
    }

    #[test]
    fn a_plan_call_stays_out_of_the_transcript() {
        let messages = vec![assistant_with_call("update_plan", r#"{"steps":[]}"#, "c1")];
        assert!(display_blocks(&messages, "[compacted]").is_empty());
    }

    #[test]
    fn the_models_only_suffixes_are_not_shown_back_to_the_user() {
        let messages = vec![Message::user(
            "tighten chapter 3\n\n(In scope: v1/c3)\n\n[Approval mode: auto]",
        )];
        let blocks = display_blocks(&messages, "[compacted]");
        assert_eq!(blocks[0].body, "tighten chapter 3");
    }

    #[test]
    fn a_compacted_summary_reads_as_a_notice_not_as_something_you_said() {
        let messages = vec![Message::user("[compacted] ...older turns...")];
        let blocks = display_blocks(&messages, "[compacted]");
        assert_eq!(blocks[0].kind, BlockKind::Notice);
        assert!(!blocks[0].collapsible());
    }

    #[test]
    fn folding_changes_the_cache_key_although_no_byte_of_the_body_did() {
        let theme = Theme::washi();
        let mut b = Block::tool(1, "read_chapter".into(), "c1".into(), "v1/c3".into());
        b.set_detail("{}".into());
        let before = cache_key(&b, 80, &theme);
        b.toggle();
        assert_ne!(before, cache_key(&b, 80, &theme));
        // ...and so does a status that arrives without any new text.
        let open = cache_key(&b, 80, &theme);
        b.set_tool_status(ToolStatus::Failed);
        assert_ne!(open, cache_key(&b, 80, &theme));
    }

    #[test]
    fn a_collapsed_block_draws_fewer_rows_than_an_open_one() {
        let theme = Theme::washi();
        let mut b = Block::tool(1, "grep_chapter".into(), "c1".into(), "先輩".into());
        b.set_detail("line one\nline two\nline three".into());
        let closed = render(&b, 60, &theme).len();
        b.toggle();
        assert!(render(&b, 60, &theme).len() > closed);
    }

    #[test]
    fn a_notice_cannot_be_folded_into_nothing() {
        let mut b = Block::notice(1, "context summarized".into());
        b.toggle();
        assert!(b.open, "a notice has no second state to fold into");
        assert!(!render(&b, 40, &Theme::washi()).is_empty());
    }

    #[test]
    fn copying_a_block_takes_what_the_fold_is_hiding_too() {
        let mut b = Block::tool(1, "read_chapter".into(), "c1".into(), "v1/c3".into());
        b.set_detail(r#"{"ch":3}"#.into());
        assert!(!b.open);
        let md = b.to_markdown();
        assert!(md.contains("read_chapter"));
        assert!(md.contains(r#""ch":3"#), "{md}");
    }
}
