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
    }
}

fn missing(ui: &mut Ui, pal: &GuiPalette, why: &str) {
    ui.label(RichText::new(why).color(pal.ink_faint).italics().small());
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
