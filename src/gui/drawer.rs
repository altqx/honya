//! The bottom drawer: what you keep open beside the work.
//!
//! Each of these was a modal you dismissed to get back to the thing it was
//! about — the activity log, the QA inbox — or was buried inside one screen,
//! like the run queue, or had no GUI at all, like the Refine sub-agents.

use egui::{RichText, Ui};

use crate::app::{Action, App};
use crate::model::{LogLevel, RefineSubagentStatus};

use super::shell::DrawerTab;
use super::theme_map::GuiPalette;

/// The QA report is gathered by walking the volume on disk, so it is taken on
/// demand and reused until something plausibly changed it.
#[derive(Default)]
pub struct QaCache {
    root: std::path::PathBuf,
    vol: u32,
    taken_at_frame: u64,
    report: crate::app::qa::QaReport,
}

/// Roughly two seconds at the 100 ms ticker. Often enough that a finished
/// chapter shows up while you watch, rare enough that the pane is not a disk
/// scan on every frame.
const QA_REFRESH_FRAMES: u64 = 20;

impl QaCache {
    fn get(&mut self, app: &App) -> &crate::app::qa::QaReport {
        if let Some(active) = app.active.as_ref() {
            let moved = self.root != active.project.dir || self.vol != active.vol;
            let stale = app.frame.saturating_sub(self.taken_at_frame) >= QA_REFRESH_FRAMES;
            if moved || stale {
                self.root = active.project.dir.clone();
                self.vol = active.vol;
                self.taken_at_frame = app.frame;
                self.report = crate::app::qa::collect(active);
            }
        }
        &self.report
    }
}

fn fmt_elapsed(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

pub fn show(
    ui: &mut Ui,
    app: &App,
    tab: DrawerTab,
    qa: &mut QaCache,
    pal: &GuiPalette,
    actions: &mut Vec<Action>,
) {
    match tab {
        DrawerTab::Activity => activity(ui, &app.log, pal),
        DrawerTab::Tasks => tasks(ui, app, pal, actions),
        DrawerTab::Qa => qa_pane(ui, app, qa, pal, actions),
        DrawerTab::Queue => queue(ui, app, pal, actions),
    }
}

/// Unlike the modal it replaces, this keeps the whole buffer rather than the
/// last four hundred lines: a log you cannot scroll back through hides the
/// thing you opened it for.
fn activity(ui: &mut Ui, log: &[(LogLevel, String)], pal: &GuiPalette) {
    if log.is_empty() {
        empty(ui, pal, "Nothing has happened yet.");
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt("drawer_activity")
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show_rows(
            ui,
            ui.text_style_height(&egui::TextStyle::Small),
            log.len(),
            |ui, rows| {
                for (level, msg) in &log[rows.start..rows.end.min(log.len())] {
                    let (tag, color) = match level {
                        LogLevel::Error => ("ERR", pal.status_failed),
                        LogLevel::Warn => ("WRN", pal.status_warn),
                        LogLevel::Info => ("INF", pal.ink_soft),
                        LogLevel::Trace => ("TRC", pal.ink_faint),
                    };
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(tag).color(color).monospace().small());
                        ui.label(RichText::new(msg).color(pal.ink).small());
                    });
                }
            },
        );
}

/// Every Refine sub-agent this session started, with what it is allowed to do,
/// what it costs and what it is doing — the things you need before deciding to
/// stop one. The GUI could not see these at all.
fn tasks(ui: &mut Ui, app: &App, pal: &GuiPalette, actions: &mut Vec<Action>) {
    let runs = app.refine.subagent_views();
    if runs.is_empty() {
        empty(ui, pal, "No sub-agents have run in this conversation.");
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt("drawer_tasks")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for run in runs {
                ui.push_id(("task", run.id), |ui| {
                    let (mark, color) = match run.status {
                        RefineSubagentStatus::Running => ("▶", pal.accent),
                        RefineSubagentStatus::Succeeded => ("✓", pal.status_done),
                        RefineSubagentStatus::Failed => ("!", pal.status_failed),
                        RefineSubagentStatus::Canceled => ("×", pal.ink_faint),
                    };
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(mark).color(color).monospace().small());
                        ui.label(RichText::new(run.title).color(pal.ink).small());
                        let mut tags = Vec::new();
                        if !run.role.is_empty() {
                            tags.push(run.role.to_string());
                        }
                        if !run.model.is_empty() {
                            tags.push(short_model(run.model));
                        }
                        if run.background {
                            tags.push("bg".into());
                        }
                        tags.push(fmt_elapsed(run.elapsed));
                        ui.label(
                            RichText::new(tags.join(" · "))
                                .color(pal.ink_faint)
                                .small(),
                        );
                        if run.status == RefineSubagentStatus::Running {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .small_button("Stop")
                                        .on_hover_text(
                                            "stops at its next round and keeps a checkpoint",
                                        )
                                        .clicked()
                                    {
                                        actions.push(Action::RefineCancelSubagent {
                                            id: run.id.to_string(),
                                        });
                                    }
                                },
                            );
                        }
                    });
                    let detail = if run.activity.trim().is_empty() {
                        run.summary.trim()
                    } else {
                        run.activity.trim()
                    };
                    if !detail.is_empty() {
                        ui.horizontal(|ui| {
                            ui.add_space(16.0);
                            ui.label(RichText::new(detail).color(pal.ink_soft).small());
                        });
                    }
                    // A child keeps its own checklist, and where it has got to
                    // says more about progress than an elapsed time does.
                    if !run.plan.is_empty() {
                        let done = run
                            .plan
                            .iter()
                            .filter(|s| s.status == crate::model::PlanStepStatus::Completed)
                            .count();
                        let step = run
                            .plan
                            .iter()
                            .find(|s| s.status == crate::model::PlanStepStatus::InProgress)
                            .map(|s| s.step.trim())
                            .unwrap_or("");
                        ui.horizontal(|ui| {
                            ui.add_space(16.0);
                            ui.label(
                                RichText::new(format!(
                                    "plan {done}/{}{}{step}",
                                    run.plan.len(),
                                    if step.is_empty() { "" } else { " · " }
                                ))
                                .color(pal.ink_faint)
                                .small(),
                            );
                        });
                    }
                    ui.add_space(4.0);
                });
            }
        });
}

fn short_model(model: &str) -> String {
    model.rsplit('/').next().unwrap_or(model).to_string()
}

/// The QA inbox as a pane you keep open beside the Reader, rather than a
/// dialog that covers the chapter it is telling you about.
fn qa_pane(ui: &mut Ui, app: &App, cache: &mut QaCache, pal: &GuiPalette, actions: &mut Vec<Action>) {
    if app.active.is_none() {
        empty(ui, pal, "Open a project to see its findings.");
        return;
    }
    let report = cache.get(app).clone();
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!(
                "{} done · {} need review · {} failed",
                report.done, report.review, report.failed
            ))
            .color(pal.ink_soft)
            .small(),
        );
        if let Some(pct) = report.clean_pct() {
            ui.label(RichText::new(format!("· {pct}% clean")).color(pal.ink_faint).small());
        }
    });
    ui.separator();
    if report.issues.is_empty() {
        empty(ui, pal, "Nothing flagged in this volume.");
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt("drawer_qa")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (i, issue) in report.issues.iter().enumerate() {
                ui.push_id(("qa", i), |ui| {
                    ui.horizontal(|ui| {
                        let where_ = match issue.chapter {
                            Some(ch) => format!("c{ch:03}"),
                            None => "—".to_string(),
                        };
                        ui.label(
                            RichText::new(where_)
                                .color(pal.status_warn)
                                .monospace()
                                .small(),
                        );
                        let detail = if issue.detail.trim().is_empty() {
                            issue.title.trim()
                        } else {
                            issue.detail.trim()
                        };
                        if ui
                            .add(
                                egui::Label::new(RichText::new(detail).color(pal.ink).small())
                                    .sense(egui::Sense::click())
                                    .truncate(),
                            )
                            .on_hover_text("open the chapter at this finding")
                            .clicked()
                            && let Some(ch) = issue.chapter
                        {
                            actions.push(open_issue(ch, issue));
                        }
                    });
                });
            }
        });
}

/// A review flag knows its chunk, so the Reader can land on it rather than at
/// the top of the chapter.
fn open_issue(chapter: u32, issue: &crate::app::qa::QaIssue) -> Action {
    match issue.kind {
        crate::app::qa::QaKind::ReviewChunk { chunk } => {
            Action::OpenChapterAtChunk { chapter, chunk }
        }
        _ => Action::OpenChapter { chapter },
    }
}

/// The run queue, which was reachable only from inside the Translate screen.
fn queue(ui: &mut Ui, app: &App, pal: &GuiPalette, actions: &mut Vec<Action>) {
    let Some(q) = app.run_queue.as_ref() else {
        empty(ui, pal, "No run is active.");
        return;
    };
    let (running, pending) = q.snapshot();
    if running.is_none() && pending.is_empty() {
        empty(ui, pal, "The queue is empty.");
        return;
    }
    let title_of = |vol: u32, ch: u32| -> String {
        app.active
            .as_ref()
            .and_then(|a| a.project.volumes.iter().find(|v| v.number == vol))
            .and_then(|v| v.chapters.iter().find(|c| c.number == ch))
            .map(|c| c.title.clone())
            .unwrap_or_default()
    };
    egui::ScrollArea::vertical()
        .id_salt("drawer_queue")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if let Some((vol, ch)) = running {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("▶")
                            .color(pal.status_working)
                            .monospace()
                            .small(),
                    );
                    ui.label(
                        RichText::new(format!("v{vol}/c{ch:03}  {}", title_of(vol, ch)))
                            .color(pal.ink)
                            .small(),
                    );
                });
            }
            for (vol, ch) in pending {
                ui.push_id(("queued", vol, ch), |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("·").color(pal.ink_faint).monospace().small());
                        ui.label(
                            RichText::new(format!("v{vol}/c{ch:03}  {}", title_of(vol, ch)))
                                .color(pal.ink_soft)
                                .small(),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("✕").clicked() {
                                actions.push(Action::DequeueChapter { vol, ch });
                            }
                            if ui.small_button("▼").clicked() {
                                actions.push(Action::QueueMoveDown { vol, ch });
                            }
                            if ui.small_button("▲").clicked() {
                                actions.push(Action::QueueMoveUp { vol, ch });
                            }
                        });
                    });
                });
            }
        });
}

fn empty(ui: &mut Ui, pal: &GuiPalette, text: &str) {
    ui.label(RichText::new(text).color(pal.ink_faint).italics().small());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_reads_at_a_glance() {
        use std::time::Duration;
        assert_eq!(fmt_elapsed(Duration::from_secs(9)), "9s");
        assert_eq!(fmt_elapsed(Duration::from_secs(64)), "1m04s");
        assert_eq!(fmt_elapsed(Duration::from_secs(3_700)), "1h01m");
    }

    #[test]
    fn a_model_id_drops_the_vendor_prefix_nobody_is_choosing_between() {
        assert_eq!(short_model("anthropic/claude-sonnet-5"), "claude-sonnet-5");
        assert_eq!(short_model("gpt-5.5"), "gpt-5.5");
    }

    #[test]
    fn a_review_flag_opens_at_its_chunk_and_everything_else_at_the_top() {
        use crate::app::qa::{QaIssue, QaKind};
        let flagged = QaIssue {
            chapter: Some(12),
            title: String::new(),
            kind: QaKind::ReviewChunk { chunk: 4 },
            detail: String::new(),
        };
        assert!(matches!(
            open_issue(12, &flagged),
            Action::OpenChapterAtChunk {
                chapter: 12,
                chunk: 4
            }
        ));
        let failed = QaIssue {
            kind: QaKind::ChapterFailed,
            ..flagged
        };
        assert!(matches!(
            open_issue(12, &failed),
            Action::OpenChapter { chapter: 12 }
        ));
    }
}
