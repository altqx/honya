//! What the tree is pointing at, in detail.
//!
//! Every number here existed already and was reachable from exactly one
//! screen: a chapter's status and spend only on Project, a volume's roll-up
//! only on its detail card, the project's reference-data health only in the
//! TUI's 文脈 panel. Reading one meant leaving whatever you were reading.

use egui::{RichText, Ui};

use crate::app::{Action, App};
use crate::model::{Chapter, ChapterStatus, Project, Volume};

use super::theme_map::GuiPalette;
use super::tree::Selection;

fn row(ui: &mut Ui, pal: &GuiPalette, key: &str, value: impl Into<String>) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [92.0, 16.0],
            egui::Label::new(RichText::new(key).color(pal.ink_faint).small()),
        );
        ui.label(RichText::new(value.into()).color(pal.ink).small());
    });
}

fn heading(ui: &mut Ui, pal: &GuiPalette, text: impl Into<String>) {
    ui.label(RichText::new(text.into()).color(pal.ink).strong());
    ui.add_space(4.0);
}

fn done_count(chapters: &[Chapter]) -> usize {
    chapters
        .iter()
        .filter(|c| {
            matches!(
                c.status,
                ChapterStatus::Done | ChapterStatus::Appended | ChapterStatus::NeedsReview
            )
        })
        .count()
}

/// `$0.0123 · 4.5k tokens`, or nothing when a chapter was never run.
fn usage_line(u: &crate::model::UsageStats) -> Option<String> {
    if u.is_zero() && u.cost_usd == 0.0 {
        return None;
    }
    Some(format!(
        "${:.4} · {} tokens",
        u.cost_usd,
        fmt_count(u.tokens.total.max(u.tokens.prompt + u.tokens.completion))
    ))
}

fn fmt_count(n: u32) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

pub fn show(
    ui: &mut Ui,
    app: &App,
    selection: Option<&Selection>,
    cache: &mut ContextCache,
    pal: &GuiPalette,
    actions: &mut Vec<Action>,
) {
    let Some(active) = app.active.as_ref() else {
        ui.label(
            RichText::new("Open a project to see its detail here.")
                .color(pal.ink_faint)
                .small(),
        );
        return;
    };
    // With nothing picked the project itself is the subject — the tree always
    // has *something* selected in spirit, even before a click.
    let selection = selection
        .cloned()
        .unwrap_or(Selection::Project(active.project.id.clone()));

    let is_session = matches!(selection, Selection::Session(_));
    match selection {
        Selection::Project(_) => project_detail(ui, &active.project, pal),
        Selection::Volume { vol } => {
            match active.project.volumes.iter().find(|v| v.number == vol) {
                Some(v) => volume_detail(ui, v, pal, actions),
                None => missing(ui, pal, "That volume is no longer in the project."),
            }
        }
        Selection::Chapter { vol, ch } => {
            let found = active
                .project
                .volumes
                .iter()
                .find(|v| v.number == vol)
                .and_then(|v| v.chapters.iter().find(|c| c.number == ch));
            match found {
                Some(c) => chapter_detail(ui, app, vol, c, pal, actions),
                None => missing(ui, pal, "That chapter is no longer in the volume."),
            }
        }
        Selection::Session(id) => session_detail(ui, app, &id, pal),
        Selection::Subagent(id) => subagent_detail(ui, app, &id, pal, actions),
        Selection::Character(jp) => entry_detail(ui, app, &jp, true, pal),
        Selection::Term(jp) => entry_detail(ui, app, &jp, false, pal),
    }
    // Always last: the reference data is about the project whatever is
    // selected inside it.
    if !is_session {
        context_panel(ui, app, cache, pal);
    }
}

fn missing(ui: &mut Ui, pal: &GuiPalette, why: &str) {
    ui.label(RichText::new(why).color(pal.ink_faint).italics().small());
}

/// Counts read off the reference files, kept between frames: the inspector
/// redraws on every animation tick and these are four file parses.
#[derive(Default)]
pub struct ContextCache {
    root: std::path::PathBuf,
    taken_at_frame: u64,
    characters: usize,
    terms: usize,
    has_style: bool,
}

const REFRESH_FRAMES: u64 = 20;

impl ContextCache {
    fn get(&mut self, app: &App) -> (usize, usize, bool) {
        if let Some(active) = app.active.as_ref() {
            let moved = self.root != active.project.dir;
            let stale = app.frame.saturating_sub(self.taken_at_frame) >= REFRESH_FRAMES;
            if moved || stale {
                let ws = &active.workspace;
                self.root = active.project.dir.clone();
                self.taken_at_frame = app.frame;
                self.characters = crate::workspace::characters::load(ws).len();
                self.terms = crate::workspace::glossary::load(ws).len();
                self.has_style = std::fs::read_to_string(ws.style_md())
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false);
            }
        }
        (self.characters, self.terms, self.has_style)
    }
}

/// What the TUI calls 文脈: whether the reference data the agents read is
/// actually there, and what the project has cost. It lived on one screen.
fn context_panel(ui: &mut Ui, app: &App, cache: &mut ContextCache, pal: &GuiPalette) {
    let (characters, terms, has_style) = cache.get(app);
    ui.add_space(10.0);
    ui.label(RichText::new("文脈  CONTEXT").color(pal.ink_faint).small().strong());
    ui.add_space(4.0);
    for (label, state) in [
        ("CHARACTERS", format!("{characters} entries")),
        ("GLOSSARY", format!("{terms} terms")),
        (
            "STYLE",
            if has_style { "present" } else { "empty" }.to_string(),
        ),
    ] {
        ui.horizontal(|ui| {
            let ok = !state.starts_with('0') && state != "empty";
            ui.label(
                RichText::new(if ok { "●" } else { "○" })
                    .color(if ok { pal.status_done } else { pal.ink_faint })
                    .monospace()
                    .small(),
            );
            ui.add_sized(
                [92.0, 16.0],
                egui::Label::new(RichText::new(label).color(pal.ink_soft).small()),
            );
            ui.label(RichText::new(state).color(pal.ink).small());
        });
    }

    if let Some(active) = app.active.as_ref() {
        let mut cost = 0.0_f64;
        let mut tokens = 0_u32;
        for v in &active.project.volumes {
            for c in &v.chapters {
                cost += c.usage.cost_usd;
                tokens = tokens.saturating_add(
                    c.usage.tokens.total.max(c.usage.tokens.prompt + c.usage.tokens.completion),
                );
            }
        }
        if tokens > 0 || cost > 0.0 {
            ui.add_space(6.0);
            row(
                ui,
                pal,
                "Σ project",
                format!("${cost:.4} · {} tokens", fmt_count(tokens)),
            );
        }
    }
}

fn project_detail(ui: &mut Ui, p: &Project, pal: &GuiPalette) {
    heading(ui, pal, &p.title);
    if !p.translated_title.trim().is_empty() {
        ui.label(
            RichText::new(&p.translated_title)
                .color(pal.ink_soft)
                .small(),
        );
        ui.add_space(4.0);
    }
    row(ui, pal, "language", p.target_language.label());
    row(ui, pal, "volumes", p.volumes.len().to_string());
    let chapters: usize = p.volumes.iter().map(|v| v.chapters.len()).sum();
    let done: usize = p.volumes.iter().map(|v| done_count(&v.chapters)).sum();
    row(ui, pal, "chapters", format!("{done} / {chapters} done"));
    if chapters > 0 {
        ui.add_space(6.0);
        ui.add(egui::ProgressBar::new(done as f32 / chapters as f32).desired_height(6.0));
    }
}

fn volume_detail(ui: &mut Ui, v: &Volume, pal: &GuiPalette, actions: &mut Vec<Action>) {
    let label = match &v.label {
        Some(l) if !l.trim().is_empty() => format!("Vol {} · {l}", v.number),
        _ => format!("Vol {}", v.number),
    };
    heading(ui, pal, label);
    let done = done_count(&v.chapters);
    row(
        ui,
        pal,
        "chapters",
        format!("{done} / {}", v.chapters.len()),
    );
    let flagged = v
        .chapters
        .iter()
        .filter(|c| c.status == ChapterStatus::NeedsReview)
        .count();
    if flagged > 0 {
        row(ui, pal, "needs review", flagged.to_string());
    }
    if !v.chapters.is_empty() {
        ui.add_space(6.0);
        ui.add(egui::ProgressBar::new(done as f32 / v.chapters.len() as f32).desired_height(6.0));
    }
    ui.add_space(8.0);
    if ui.button("Translate this volume").clicked() {
        actions.push(Action::SetActiveVolume { vol: v.number });
        actions.push(Action::StartTranslation {
            chapters: v.chapters.iter().map(|c| c.number).collect(),
        });
    }
}

fn chapter_detail(
    ui: &mut Ui,
    app: &App,
    vol: u32,
    c: &Chapter,
    pal: &GuiPalette,
    actions: &mut Vec<Action>,
) {
    let (glyph, color) = crate::theme::status_glyph(c.kind, c.status, &app.theme);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(glyph.to_string())
                .color(super::theme_map::color(color))
                .monospace(),
        );
        ui.label(
            RichText::new(format!("v{vol}/c{}", c.number))
                .color(pal.ink)
                .strong(),
        );
    });
    if !c.title.trim().is_empty() {
        ui.label(RichText::new(c.title.trim()).color(pal.ink_soft).small());
    }
    ui.add_space(6.0);
    row(ui, pal, "status", status_label(c.status));
    if c.total_chunks > 0 {
        row(
            ui,
            pal,
            "chunks",
            format!("{} / {}", c.committed_chunks, c.total_chunks),
        );
    }
    if c.skipped_chunks > 0 {
        row(ui, pal, "skipped", c.skipped_chunks.to_string());
    }
    if c.source_segments > 0 {
        row(ui, pal, "segments", c.source_segments.to_string());
    }
    if let Some(usage) = usage_line(&c.usage) {
        row(ui, pal, "spend", usage);
    }
    if let Some(run) = c.last_run {
        row(
            ui,
            pal,
            "last run",
            run.format("%Y-%m-%d %H:%M").to_string(),
        );
    }

    ui.add_space(10.0);
    ui.horizontal_wrapped(|ui| {
        if ui.button("Read").clicked() {
            actions.push(Action::SetActiveVolume { vol });
            actions.push(Action::OpenChapter { chapter: c.number });
        }
        if ui.button("Queue").clicked() {
            actions.push(Action::EnqueueChapters {
                chapters: vec![(vol, c.number)],
            });
        }
    });
}

fn status_label(s: ChapterStatus) -> &'static str {
    match s {
        ChapterStatus::Pending => "pending",
        ChapterStatus::Chunking => "chunking",
        ChapterStatus::Translating => "translating",
        ChapterStatus::Reviewing => "reviewing",
        ChapterStatus::Appended => "appended",
        ChapterStatus::Done => "done",
        ChapterStatus::NeedsReview => "needs review",
        ChapterStatus::Failed => "failed",
        ChapterStatus::Paused => "paused",
        ChapterStatus::Partial => "partial",
    }
}

/// A sub-agent, and its own conversation. The window could see neither.
fn subagent_detail(
    ui: &mut Ui,
    app: &App,
    id: &str,
    pal: &GuiPalette,
    actions: &mut Vec<Action>,
) {
    let Some(run) = app.refine.subagent_views().into_iter().find(|r| r.id == id) else {
        missing(ui, pal, "That sub-agent is no longer listed.");
        return;
    };
    heading(ui, pal, run.title);
    row(ui, pal, "role", if run.role.is_empty() { "—" } else { run.role });
    row(ui, pal, "model", if run.model.is_empty() { "—" } else { run.model });
    row(
        ui,
        pal,
        "status",
        match run.status {
            crate::model::RefineSubagentStatus::Running => "running",
            crate::model::RefineSubagentStatus::Succeeded => "done",
            crate::model::RefineSubagentStatus::Failed => "failed",
            crate::model::RefineSubagentStatus::Canceled => "cancelled",
        },
    );
    row(ui, pal, "elapsed", format!("{}s", run.elapsed.as_secs()));
    if run.background {
        row(ui, pal, "", "running in the background");
    }

    if !run.plan.is_empty() {
        ui.add_space(6.0);
        for step in run.plan {
            let (mark, color) = match step.status {
                crate::model::PlanStepStatus::Completed => ("✓", pal.status_done),
                crate::model::PlanStepStatus::InProgress => ("▸", pal.accent),
                crate::model::PlanStepStatus::Pending => ("◻", pal.ink_soft),
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new(mark).color(color).monospace().small());
                ui.label(RichText::new(step.step.trim()).color(pal.ink_soft).small());
            });
        }
    }

    if run.status == crate::model::RefineSubagentStatus::Running {
        ui.add_space(8.0);
        if ui.button("Stop this sub-agent").clicked() {
            actions.push(Action::RefineCancelSubagent { id: id.to_string() });
        }
    }

    // Its own conversation, which only ever existed inside the block's fold.
    let transcript = app
        .refine
        .blocks
        .iter()
        .find(|b| b.subagent_id() == Some(id))
        .map(|b| b.detail.clone())
        .unwrap_or_default();
    ui.add_space(8.0);
    ui.label(RichText::new("ITS CONVERSATION").color(pal.ink_faint).small().strong());
    ui.add_space(2.0);
    if transcript.trim().is_empty() {
        missing(ui, pal, "Nothing reported yet.");
    } else {
        for line in transcript.lines() {
            ui.label(RichText::new(line).color(pal.ink_soft).monospace().small());
        }
    }
}

/// One glossary term or character, by its Japanese surface.
fn entry_detail(ui: &mut Ui, app: &App, jp: &str, character: bool, pal: &GuiPalette) {
    let Some(active) = app.active.as_ref() else {
        return;
    };
    let ws = &active.workspace;
    if character {
        let Some(c) = crate::workspace::characters::load(ws)
            .into_iter()
            .find(|c| c.jp_name == jp)
        else {
            missing(ui, pal, "That character is gone.");
            return;
        };
        heading(ui, pal, &c.jp_name);
        row(ui, pal, "translated", &c.translated_name);
        for (label, v) in [
            ("romaji", &c.romaji),
            ("gender", &c.gender),
            ("honorific", &c.honorific),
            ("speech", &c.speech_style),
            ("notes", &c.notes),
        ] {
            if let Some(v) = v.as_ref().filter(|v| !v.trim().is_empty()) {
                row(ui, pal, label, v.clone());
            }
        }
        if !c.aliases.is_empty() {
            row(ui, pal, "aliases", c.aliases.join(", "));
        }
        if !c.also_called.is_empty() {
            ui.add_space(4.0);
            ui.label(RichText::new("ALSO CALLED").color(pal.ink_faint).small().strong());
            for a in &c.also_called {
                row(ui, pal, &a.jp, &a.translated_name);
            }
        }
        return;
    }
    let Some(t) = crate::workspace::glossary::load(ws)
        .into_iter()
        .find(|t| t.jp_term == jp)
    else {
        missing(ui, pal, "That term is gone.");
        return;
    };
    heading(ui, pal, &t.jp_term);
    row(ui, pal, "translated", &t.translated_term);
    for (label, v) in [
        ("romaji", &t.romaji),
        ("category", &t.category),
        ("gloss", &t.gloss),
    ] {
        if let Some(v) = v.as_ref().filter(|v| !v.trim().is_empty()) {
            row(ui, pal, label, v.clone());
        }
    }
    if !t.forbidden_translations.is_empty() {
        row(ui, pal, "forbidden", t.forbidden_translations.join(", "));
    }
}

fn session_detail(ui: &mut Ui, app: &App, id: &str, pal: &GuiPalette) {
    let Some(meta) = app.refine_sessions.iter().find(|s| s.id == id) else {
        missing(ui, pal, "That conversation is gone.");
        return;
    };
    heading(
        ui,
        pal,
        if meta.title.trim().is_empty() {
            meta.id.clone()
        } else {
            meta.title.clone()
        },
    );
    row(ui, pal, "messages", meta.message_count.to_string());
    row(
        ui,
        pal,
        "updated",
        meta.updated.format("%Y-%m-%d %H:%M").to_string(),
    );
    if app.refine_session_id == id {
        ui.add_space(6.0);
        ui.label(
            RichText::new("this is the live conversation")
                .color(pal.accent)
                .small(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chapter_that_was_never_run_shows_no_spend() {
        let none = crate::model::UsageStats::default();
        assert!(usage_line(&none).is_none());
    }

    #[test]
    fn a_chapter_that_cost_nothing_but_ran_still_shows_its_tokens() {
        let used = crate::model::UsageStats {
            tokens: crate::model::TokenUsage {
                total: 2000,
                ..Default::default()
            },
            ..Default::default()
        };
        let line = usage_line(&used).expect("a run with tokens is a run");
        assert!(line.contains("2.0k"), "{line}");
    }

    #[test]
    fn counts_read_at_a_glance_rather_than_in_full() {
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1_500), "1.5k");
        assert_eq!(fmt_count(2_400_000), "2.4M");
    }

    #[test]
    fn every_chapter_status_has_a_word_for_it() {
        // A `_ =>` arm here would silently label a new status "pending".
        for s in [
            ChapterStatus::Pending,
            ChapterStatus::Chunking,
            ChapterStatus::Translating,
            ChapterStatus::Reviewing,
            ChapterStatus::Appended,
            ChapterStatus::Done,
            ChapterStatus::NeedsReview,
            ChapterStatus::Failed,
            ChapterStatus::Paused,
            ChapterStatus::Partial,
        ] {
            assert!(!status_label(s).is_empty());
        }
    }
}
