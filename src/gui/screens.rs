//! Native screen bodies — lists, cards, and side-by-side panels (not a terminal grid).

use egui::{
    Align, Color32, Layout, RichText, ScrollArea, Sense, TextEdit, Ui,
    scroll_area::ScrollBarVisibility,
};

use crate::app::overlay::Overlay;
use crate::app::refine::TurnRole;
use crate::app::{Action, App, Screen};
use crate::model::{Chapter, ChapterKind, ChapterStatus, PlanStepStatus, Project, Volume};
use crate::theme;

use super::theme_map::{GuiPalette, card_fill, card_frame, inset_frame};
use super::widgets::{hint, primary_button};

/// Vertical scroller that always reserves its bar — avoids content width jiggle
/// when scrolling becomes necessary (or on hover with floating bars).
fn scroll_y(id: &'static str) -> ScrollArea {
    ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink([false, false])
        .scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible)
}

/// Fixed-height toolbar row so action buttons can't reflow the body below.
fn toolbar_row(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), 32.0),
        Layout::left_to_right(Align::Center),
        add,
    );
}

#[derive(Default)]
pub struct GuiNav {
    pub shelf_sel: usize,
    pub project_sel: Option<(u32, u32)>,
    pub project_vol: Option<u32>,
    pub lexicon_tab: usize,
    pub lexicon_filter: String,
    /// Set by the File menu; the Shelf rescans + clears it on its next frame.
    pub rescan_requested: bool,
    /// Draft text in the Refine input box (App's RefineScreen input is TUI-only).
    pub refine_input: String,
}

pub fn render_body(ui: &mut Ui, app: &mut App, nav: &mut GuiNav, pal: &GuiPalette) {
    match app.screen {
        Screen::Shelf => shelf(ui, app, nav, pal),
        Screen::Project => project(ui, app, nav, pal),
        Screen::Translate => translate(ui, app, pal),
        Screen::Reader => reader(ui, app, pal),
        Screen::Lexicon => lexicon(ui, app, nav, pal),
        Screen::Refine => refine(ui, app, nav, pal),
    }
}

// ─── Shelf ───────────────────────────────────────────────────────────────────

fn rescan_shelf(app: &mut App) {
    let root = std::env::current_dir().unwrap_or_default();
    app.shelf.rescan(&root);
    app.projects = crate::workspace::scan::scan_projects(&root);
}

fn shelf(ui: &mut Ui, app: &mut App, nav: &mut GuiNav, pal: &GuiPalette) {
    if nav.rescan_requested {
        nav.rescan_requested = false;
        rescan_shelf(app);
    }

    toolbar_row(ui, |ui| {
        ui.heading(RichText::new("書架  Shelf").color(pal.ink));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if primary_button(ui, pal, "Import source…").clicked() {
                app.apply(Action::OpenImport);
            }
            if ui.button("Sample project").clicked() {
                app.apply(Action::CreateSample);
            }
            if ui.button("Rescan").clicked() {
                rescan_shelf(app);
            }
        });
    });
    ui.add_space(4.0);
    ui.label(
        RichText::new("Open a project or import an EPUB / PDF / HTML / Markdown source.")
            .color(pal.ink_soft)
            .small(),
    );
    ui.add_space(8.0);

    let projects = app.projects.clone();
    let foreign = app.foreign_run.as_ref().map(|cp| cp.project_dir.clone());

    if projects.is_empty() {
        card_frame(pal).show(ui, |ui| {
            ui.label(RichText::new("No projects yet").color(pal.ink_soft).strong());
            ui.label(
                RichText::new("Drop a light-novel source into this folder, then Import.")
                    .color(pal.ink_faint),
            );
            ui.add_space(8.0);
            if ui.button("Create sample project").clicked() {
                app.apply(Action::CreateSample);
            }
        });
        return;
    }

    scroll_y("shelf_list").show(ui, |ui| {
        for (i, p) in projects.iter().enumerate() {
            let selected = nav.shelf_sel == i;
            let busy = foreign.as_ref().is_some_and(|d| d == &p.dir);
            project_card(ui, app, p, selected, busy, pal, |sel| nav.shelf_sel = sel.unwrap_or(i));
            ui.add_space(8.0);
        }
    });
}

fn project_card(
    ui: &mut Ui,
    app: &mut App,
    p: &Project,
    selected: bool,
    busy: bool,
    pal: &GuiPalette,
    mut select: impl FnMut(Option<usize>),
) {
    let fill = if selected { pal.accent_bg } else { pal.bg_panel };
    let stroke = if selected { pal.accent } else { pal.rule };
    // Fixed 1px stroke always — thicker selection stroke changes outer size and jiggles.
    let response = egui::Frame::NONE
        .fill(fill)
        .stroke(egui::Stroke::new(1.0_f32, stroke))
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .outer_margin(egui::Margin::ZERO)
        .shadow(egui::Shadow::NONE)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.set_max_width(ui.available_width());
            ui.horizontal(|ui| {
                let (glyph, color) = overall_status(p, busy, pal);
                ui.label(RichText::new(glyph.to_string()).color(color).size(18.0));
                ui.vertical(|ui| {
                    let title = if p.translated_title.is_empty() {
                        p.title.clone()
                    } else {
                        format!("{}  ·  {}", p.title, p.translated_title)
                    };
                    ui.label(RichText::new(title).color(pal.ink).strong().size(16.0));
                    let vols = p.volumes.len();
                    let chs: usize = p.volumes.iter().map(|v| v.chapters.len()).sum();
                    let done: usize = p
                        .volumes
                        .iter()
                        .flat_map(|v| v.chapters.iter())
                        .filter(|c| {
                            matches!(
                                c.status,
                                ChapterStatus::Done
                                    | ChapterStatus::Appended
                                    | ChapterStatus::NeedsReview
                            )
                        })
                        .count();
                    ui.label(
                        RichText::new(format!(
                            "{} · {} vol · {} ch · {} done · {}{}",
                            p.id,
                            vols,
                            chs,
                            done,
                            p.target_language.label(),
                            if busy { " · running elsewhere" } else { "" }
                        ))
                        .color(pal.ink_faint)
                        .small(),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Open").clicked() {
                        select(None);
                        app.apply(Action::OpenProject(p.id.clone()));
                    }
                    if ui
                        .button(RichText::new("Delete…").color(pal.status_failed))
                        .clicked()
                    {
                        select(None);
                        app.apply(Action::show_overlay(Overlay::confirm(
                            "Delete project",
                            format!(
                                "Permanently delete “{}” — raw chapters, translations, and metadata?",
                                p.title
                            ),
                            Action::DeleteProject { id: p.id.clone() },
                        )));
                    }
                });
            });
        })
        .response
        .interact(Sense::click());
    if response.clicked() {
        select(None);
    }
    if response.double_clicked() {
        select(None);
        app.apply(Action::OpenProject(p.id.clone()));
    }
}

fn overall_status(p: &Project, busy: bool, pal: &GuiPalette) -> (char, Color32) {
    if busy {
        return ('↻', pal.status_working);
    }
    let mut any_fail = false;
    let mut any_work = false;
    let mut any_pending = false;
    let mut any_done = false;
    for ch in p.volumes.iter().flat_map(|v| v.chapters.iter()) {
        match ch.status {
            ChapterStatus::Failed => any_fail = true,
            s if s.is_active() || s == ChapterStatus::Paused => any_work = true,
            ChapterStatus::Done | ChapterStatus::Appended | ChapterStatus::NeedsReview => {
                any_done = true
            }
            _ => any_pending = true,
        }
    }
    if any_fail {
        ('✗', pal.status_failed)
    } else if any_work {
        ('◐', pal.status_working)
    } else if any_pending && any_done {
        ('◑', pal.status_warn)
    } else if any_done {
        ('●', pal.status_done)
    } else {
        ('○', pal.status_pending)
    }
}

// ─── Project ─────────────────────────────────────────────────────────────────

fn project(ui: &mut Ui, app: &mut App, nav: &mut GuiNav, pal: &GuiPalette) {
    let Some(active) = app.active.as_ref() else {
        empty_state(ui, pal, "No project open", "Open a project from the Shelf.");
        return;
    };
    let project = active.project.clone();
    let active_vol = active.vol;

    toolbar_row(ui, |ui| {
        // Truncate the title into a stable max width so the toolbar never reflows.
        let title = if project.translated_title.is_empty() {
            project.title.clone()
        } else {
            format!("{} · {}", project.title, project.translated_title)
        };
        let title_w = (ui.available_width() - 480.0).clamp(120.0, 420.0);
        ui.add_sized(
            [title_w, 26.0],
            egui::Label::new(RichText::new(title).color(pal.ink).strong().size(17.0)).truncate(),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if primary_button(ui, pal, &format!("Translate Vol.{active_vol}")).clicked() {
                app.apply(Action::StartVolumeTranslation { vol: active_vol });
            }
            if ui.button("Translate all").clicked() {
                app.apply(Action::StartProjectTranslation);
            }
            if ui.button("QA").clicked() {
                app.apply(Action::show_overlay(Overlay::qa_placeholder()));
            }
            if ui.button("Export…").clicked() {
                app.apply(Action::show_overlay(Overlay::export(active_vol)));
            }
            ui.menu_button("⋯", |ui| {
                if ui.button("Edit volume synopsis…").clicked() {
                    let data = crate::workspace::volume::load(
                        &app.active.as_ref().expect("active project").workspace,
                    );
                    app.apply(Action::show_overlay(Overlay::synopsis_edit(
                        data.synopsis_raw,
                        data.translated_synopsis,
                        active_vol,
                        project.title.clone(),
                        project.target_language,
                    )));
                }
                if ui.button("Edit project title…").clicked() {
                    app.apply(Action::show_overlay(Overlay::project_title_edit(
                        project.id.clone(),
                        project.title.clone(),
                        project.translated_title.clone(),
                        project.target_language,
                    )));
                }
                ui.separator();
                if ui.button("Add volume…").clicked() {
                    app.apply(Action::AddVolume);
                }
                if ui.button("Add chapters to this volume…").clicked() {
                    app.apply(Action::AddChapters { vol: active_vol });
                }
                if ui.button("Update volume images…").clicked() {
                    app.apply(Action::show_overlay(Overlay::confirm(
                        "Update volume images",
                        format!(
                            "Re-import the source for Vol.{active_vol:02} and rewrite image links. Translation prose stays unchanged."
                        ),
                        Action::RefreshVolumeImages { vol: active_vol },
                    )));
                }
            });
        });
    });
    ui.add_space(6.0);

    let body_h = ui.available_height();
    ui.columns(2, |cols| {
        // Size each column exactly — cards must not grow past this box.
        for c in cols.iter_mut() {
            c.set_min_height(body_h);
            c.set_max_height(body_h);
        }

        card_fill(&mut cols[0], pal, |ui| {
            ui.label(RichText::new("Volumes & chapters").color(pal.ink_soft).strong());
            ui.add_space(6.0);
            scroll_y("project_tree").show(ui, |ui| {
                for vol in &project.volumes {
                    let vol_selected = nav.project_vol == Some(vol.number)
                        || (nav.project_sel.is_some_and(|(v, _)| v == vol.number));
                    let header = vol_label(vol);
                    let response = ui.selectable_label(
                        vol_selected && nav.project_sel.is_none(),
                        RichText::new(header).color(if vol.number == active_vol {
                            pal.accent
                        } else {
                            pal.ink
                        }),
                    );
                    if response.clicked() {
                        nav.project_vol = Some(vol.number);
                        nav.project_sel = None;
                        app.apply(Action::SetActiveVolume { vol: vol.number });
                    }
                    ui.indent(format!("vol_{}", vol.number), |ui| {
                        for ch in &vol.chapters {
                            let selected = nav.project_sel == Some((vol.number, ch.number));
                            let (glyph, color) = status_chip(ch, pal);
                            let label = format!(
                                "{}  ch {:03}  {}",
                                glyph,
                                ch.number,
                                if ch.title.is_empty() {
                                    "—"
                                } else {
                                    &ch.title
                                }
                            );
                            let response = ui.selectable_label(
                                selected,
                                RichText::new(label).color(if selected { pal.ink } else { color }),
                            );
                            if response.clicked() {
                                nav.project_sel = Some((vol.number, ch.number));
                                nav.project_vol = Some(vol.number);
                                app.apply(Action::SetActiveVolume { vol: vol.number });
                            }
                            if response.double_clicked() {
                                app.apply(Action::OpenChapter { chapter: ch.number });
                            }
                        }
                    });
                    ui.add_space(4.0);
                }
            });
        });

        card_fill(&mut cols[1], pal, |ui| {
            ui.label(RichText::new("Details").color(pal.ink_soft).strong());
            ui.add_space(6.0);
            if let Some((v, c)) = nav.project_sel {
                if let Some(ch) = project
                    .volumes
                    .iter()
                    .find(|vol| vol.number == v)
                    .and_then(|vol| vol.chapters.iter().find(|ch| ch.number == c))
                {
                    detail_chapter(ui, v, ch, pal);
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Open in Reader").clicked() {
                            app.apply(Action::OpenChapter { chapter: c });
                        }
                        if ui.button("Translate").clicked() {
                            app.apply(Action::StartTranslation { chapters: vec![c] });
                        }
                        if ui.button("Enqueue").clicked() {
                            app.apply(Action::EnqueueChapters {
                                chapters: vec![(v, c)],
                            });
                        }
                        if ui
                            .button(RichText::new("Delete…").color(pal.status_failed))
                            .clicked()
                        {
                            app.apply(Action::show_overlay(Overlay::confirm(
                                "Delete chapters",
                                format!("Delete chapter {c:03} from Vol.{v:02}?"),
                                Action::DeleteChapters {
                                    vol: v,
                                    chapters: vec![c],
                                },
                            )));
                        }
                    });
                }
            } else {
                let vol_n = nav.project_vol.unwrap_or(active_vol);
                if let Some(vol) = project.volumes.iter().find(|v| v.number == vol_n) {
                    detail_volume(ui, vol, pal);
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button(format!("Translate Vol.{vol_n}")).clicked() {
                            app.apply(Action::StartVolumeTranslation { vol: vol_n });
                        }
                        if ui.button("Export…").clicked() {
                            app.apply(Action::show_overlay(Overlay::export(vol_n)));
                        }
                    });
                } else {
                    ui.label(RichText::new("Select a volume or chapter.").color(pal.ink_faint));
                }
            }
        });
    });
}

fn vol_label(vol: &Volume) -> String {
    match &vol.label {
        Some(l) => format!("Vol.{}  {}", vol.number, l),
        None => format!("Vol.{}", vol.number),
    }
}

fn detail_volume(ui: &mut Ui, vol: &Volume, pal: &GuiPalette) {
    ui.label(RichText::new(vol_label(vol)).color(pal.ink).strong().size(16.0));
    let total = vol.chapters.len();
    let done = vol
        .chapters
        .iter()
        .filter(|c| {
            matches!(
                c.status,
                ChapterStatus::Done | ChapterStatus::Appended | ChapterStatus::NeedsReview
            )
        })
        .count();
    ui.label(RichText::new(format!("{done} / {total} chapters done")).color(pal.ink_soft));
    if total > 0 {
        let frac = done as f32 / total as f32;
        let progress = egui::ProgressBar::new(frac).show_percentage().desired_width(220.0);
        ui.add(progress);
    }
}

fn detail_chapter(ui: &mut Ui, vol: u32, ch: &Chapter, pal: &GuiPalette) {
    let (glyph, color) = status_chip(ch, pal);
    ui.horizontal(|ui| {
        ui.label(RichText::new(glyph.to_string()).color(color).size(20.0));
        ui.vertical(|ui| {
            ui.label(
                RichText::new(format!("Vol.{vol} · ch {:03}", ch.number))
                    .color(pal.ink_faint)
                    .small(),
            );
            ui.label(
                RichText::new(if ch.title.is_empty() {
                    "Untitled chapter"
                } else {
                    &ch.title
                })
                .color(pal.ink)
                .strong()
                .size(16.0),
            );
        });
    });
    ui.add_space(8.0);
    ui.label(RichText::new(format!("Status: {}", status_label(ch.status))).color(color));
    if ch.total_chunks > 0 {
        ui.label(
            RichText::new(format!(
                "Chunks: {} / {} committed",
                ch.committed_chunks, ch.total_chunks
            ))
            .color(pal.ink_soft),
        );
    }
    if !ch.usage.is_zero() {
        ui.label(
            RichText::new(format!(
                "Usage: {} tokens · ${:.4}",
                ch.usage.tokens.total, ch.usage.cost_usd
            ))
            .color(pal.ink_faint)
            .small(),
        );
    }
}

fn status_chip(ch: &Chapter, pal: &GuiPalette) -> (char, Color32) {
    if matches!(ch.kind, ChapterKind::ImageOnly) {
        return ('▣', pal.status_image);
    }
    match ch.status {
        ChapterStatus::Pending => ('○', pal.status_pending),
        ChapterStatus::Chunking => ('◔', pal.status_working),
        ChapterStatus::Translating => ('◐', pal.status_working),
        ChapterStatus::Reviewing => ('◑', pal.status_working),
        ChapterStatus::Appended => ('◕', pal.status_working),
        ChapterStatus::Done => ('●', pal.status_done),
        ChapterStatus::NeedsReview => ('⚑', pal.status_warn),
        ChapterStatus::Failed => ('✗', pal.status_failed),
        ChapterStatus::Paused => ('‖', pal.status_warn),
        ChapterStatus::Partial => ('◒', pal.status_warn),
    }
}

fn status_label(s: ChapterStatus) -> &'static str {
    match s {
        ChapterStatus::Pending => "Pending",
        ChapterStatus::Chunking => "Chunking",
        ChapterStatus::Translating => "Translating",
        ChapterStatus::Reviewing => "Reviewing",
        ChapterStatus::Appended => "Appended",
        ChapterStatus::Done => "Done",
        ChapterStatus::NeedsReview => "Needs review",
        ChapterStatus::Failed => "Failed",
        ChapterStatus::Paused => "Paused",
        ChapterStatus::Partial => "Partial",
    }
}

// ─── Translate ───────────────────────────────────────────────────────────────

fn translate(ui: &mut Ui, app: &mut App, pal: &GuiPalette) {
    if app.active.is_none() {
        empty_state(ui, pal, "No project open", "Open a project to start translating.");
        return;
    }

    let phase = app.translate.phase_label();
    let title = app.translate.chapter_title.clone();
    let chapter = app.translate.current_chapter;
    let chunk = app.translate.chunk;
    let agent_lines = app.translate.agent_lines.clone();
    let active_agent = app.translate.active_agent;
    let preview = app.translate.preview.clone();
    let reasoning = app.translate.thought_reasoning.clone();
    let scene = app.translate.thought_scene.clone();
    let glossary = app.translate.thought_glossary.clone();
    let run = app.translate.run;
    let ch_usage = app.translate.chapter;
    let retries = app.translate.retries;
    let note = app.translate.last_note.clone();
    let queue = app.translate.queue.clone();
    let running = app.translate.is_running();
    let paused = app.translate.is_paused();

    toolbar_row(ui, |ui| {
        ui.heading(RichText::new("訳  Translate").color(pal.ink));
        let phase_color = match phase {
            "Running" => pal.status_working,
            "Paused" => pal.status_warn,
            "Preparing" => pal.accent_soft,
            _ => pal.ink_faint,
        };
        // Fixed-width phase slot so Idle ↔ Running doesn't shove the buttons.
        ui.add_sized(
            [100.0, 20.0],
            egui::Label::new(RichText::new(phase).color(phase_color).strong()),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // Always allocate pause/stop slots so the bar doesn't reflow mid-run.
            let can_pause = running;
            let can_resume = paused;
            let can_stop = running || paused;
            if can_stop && ui.button(RichText::new("Stop").color(pal.status_failed)).clicked() {
                app.apply(Action::StopRun);
            } else if !can_stop {
                ui.add_enabled(false, egui::Button::new("Stop"));
            }
            if can_pause || can_resume {
                let label = if can_resume { "Resume" } else { "Pause" };
                if ui.button(label).clicked() {
                    app.apply(Action::PauseRun);
                }
            } else {
                ui.add_enabled(false, egui::Button::new("Pause"));
            }
        });
    });
    ui.add_space(6.0);

    // Chapter + progress strip — always same height (progress bar always shown).
    card_frame(pal).show(ui, |ui| {
        let ch_label = match chapter {
            Some(n) => format!("ch {n:03}  {title}"),
            None => {
                if title.is_empty() {
                    "Waiting for a run…".into()
                } else {
                    title
                }
            }
        };
        ui.label(RichText::new(ch_label).color(pal.ink).strong());
        let frac = if chunk.1 > 0 {
            (chunk.0 as f32 / chunk.1 as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let bar_text = if chunk.1 > 0 {
            format!("chunk {} / {}", chunk.0, chunk.1)
        } else {
            "no active chunk".into()
        };
        ui.add(
            egui::ProgressBar::new(frac)
                .text(bar_text)
                .desired_width(ui.available_width()),
        );
        ui.horizontal(|ui| {
            ui.add_sized(
                [200.0, 16.0],
                egui::Label::new(
                    RichText::new(if run.is_zero() {
                        String::new()
                    } else {
                        format!("run {} tok · ${:.4}", run.tokens.total, run.cost_usd)
                    })
                    .color(pal.ink_faint)
                    .small(),
                ),
            );
            ui.add_sized(
                [120.0, 16.0],
                egui::Label::new(
                    RichText::new(if ch_usage.is_zero() {
                        String::new()
                    } else {
                        format!("chapter {} tok", ch_usage.tokens.total)
                    })
                    .color(pal.ink_faint)
                    .small(),
                ),
            );
            ui.add_sized(
                [80.0, 16.0],
                egui::Label::new(
                    RichText::new(if retries > 0 {
                        format!("retries {retries}")
                    } else {
                        String::new()
                    })
                    .color(pal.status_warn)
                    .small(),
                ),
            );
            ui.add(egui::Label::new(RichText::new(&note).color(pal.ink_soft).small()).truncate());
        });
    });
    ui.add_space(8.0);

    let body_h = ui.available_height();
    ui.columns(2, |cols| {
        for c in cols.iter_mut() {
            c.set_min_height(body_h);
            c.set_max_height(body_h);
        }

        card_fill(&mut cols[0], pal, |ui| {
            ui.label(RichText::new("Agents").color(pal.ink_soft).strong());
            ui.add_space(4.0);
            let roles = ["◆ Orchestrator", "▲ Translator", "■ Reviewer"];
            for (i, (role, line)) in roles.iter().zip(agent_lines.iter()).enumerate() {
                let active = i == active_agent && (running || paused);
                let color = if active {
                    pal.status_working
                } else {
                    pal.ink_soft
                };
                let prefix = if active {
                    theme::spinner_frame(app.frame)
                } else {
                    "·"
                };
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [16.0, 16.0],
                        egui::Label::new(RichText::new(prefix).color(color).monospace()),
                    );
                    ui.vertical(|ui| {
                        ui.label(RichText::new(*role).color(color).strong().small());
                        ui.label(RichText::new(line).color(pal.ink));
                    });
                });
                ui.add_space(4.0);
            }

            ui.separator();
            ui.label(RichText::new("Thoughts").color(pal.ink_soft).strong());
            scroll_y("thoughts").max_height(140.0).show(ui, |ui| {
                if scene.is_empty() && glossary.is_empty() && reasoning.is_empty() {
                    ui.label(RichText::new("—").color(pal.ink_faint).small());
                }
                if !scene.is_empty() {
                    ui.label(RichText::new("Scene").color(pal.accent).small());
                    ui.label(RichText::new(&scene).color(pal.ink_soft));
                }
                if !glossary.is_empty() {
                    ui.label(RichText::new("Glossary").color(pal.accent).small());
                    ui.label(RichText::new(&glossary).color(pal.ink_soft));
                }
                if !reasoning.is_empty() {
                    ui.label(RichText::new("Reasoning").color(pal.accent).small());
                    ui.label(RichText::new(&reasoning).color(pal.ink_soft));
                }
            });

            ui.separator();
            ui.horizontal(|ui| {
                ui.label(RichText::new("Queue").color(pal.ink_soft).strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if !queue.is_empty() && ui.small_button("Sort").clicked() {
                        app.apply(Action::SortQueue);
                    }
                });
            });
            scroll_y("queue").show(ui, |ui| {
                if queue.is_empty() {
                    ui.label(RichText::new("empty").color(pal.ink_faint).small());
                }
                for row in &queue {
                    ui.horizontal(|ui| {
                        let mark = if row.running { "▶" } else { "·" };
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!(
                                    "{mark} V{} ch {:03}  {}",
                                    row.vol, row.number, row.title
                                ))
                                .color(if row.running {
                                    pal.status_working
                                } else {
                                    pal.ink_soft
                                })
                                .small(),
                            )
                            .truncate(),
                        );
                        if !row.running {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.small_button("✕").clicked() {
                                    app.apply(Action::DequeueChapter {
                                        vol: row.vol,
                                        ch: row.number,
                                    });
                                }
                                if ui.small_button("▼").clicked() {
                                    app.apply(Action::QueueMoveDown {
                                        vol: row.vol,
                                        ch: row.number,
                                    });
                                }
                                if ui.small_button("▲").clicked() {
                                    app.apply(Action::QueueMoveUp {
                                        vol: row.vol,
                                        ch: row.number,
                                    });
                                }
                            });
                        }
                    });
                }
            });
        });

        card_fill(&mut cols[1], pal, |ui| {
            ui.label(RichText::new("Live translation").color(pal.ink_soft).strong());
            ui.add_space(4.0);
            scroll_y("preview").stick_to_bottom(true).show(ui, |ui| {
                if preview.is_empty() {
                    ui.label(
                        RichText::new("Streamed translation will appear here.")
                            .color(pal.ink_faint)
                            .italics(),
                    );
                } else {
                    ui.label(RichText::new(&preview).color(pal.translated_text).size(15.0));
                }
            });
        });
    });
}

// ─── Reader ──────────────────────────────────────────────────────────────────

fn reader(ui: &mut Ui, app: &mut App, pal: &GuiPalette) {
    let Some(active) = app.active.as_ref() else {
        empty_state(ui, pal, "No project open", "Open a project to read translations.");
        return;
    };
    let chapter = app.reader.chapter;
    let ja = app.reader.ja.clone();
    let translated = app.reader.translated_text.clone();
    let project = active.project.clone();
    let vol = active.vol;

    // Chapter navigation
    let chapters: Vec<(u32, String)> = project
        .volumes
        .iter()
        .find(|v| v.number == vol)
        .map(|v| {
            v.chapters
                .iter()
                .map(|c| (c.number, c.title.clone()))
                .collect()
        })
        .unwrap_or_default();

    toolbar_row(ui, |ui| {
        ui.heading(RichText::new("読  Reader").color(pal.ink));
        ui.add_space(12.0);
        if ui.button("◀").clicked() {
            app.apply(Action::ReaderStepChapter { forward: false });
        }
        let current_label = chapters
            .iter()
            .find(|(n, _)| *n == chapter)
            .map(|(n, t)| {
                if t.is_empty() {
                    format!("ch {n:03}")
                } else {
                    format!("ch {n:03} · {t}")
                }
            })
            .unwrap_or_else(|| {
                if chapter == 0 {
                    "Select a chapter".into()
                } else {
                    format!("ch {chapter:03}")
                }
            });
        egui::ComboBox::from_id_salt("reader_ch")
            .selected_text(current_label)
            .width(190.0)
            .show_ui(ui, |ui| {
                for (n, t) in &chapters {
                    let label = if t.is_empty() {
                        format!("ch {n:03}")
                    } else {
                        format!("ch {n:03} · {t}")
                    };
                    if ui.selectable_label(*n == chapter, label).clicked() {
                        app.apply(Action::OpenChapter { chapter: *n });
                    }
                }
            });
        if ui.button("▶").clicked() {
            app.apply(Action::ReaderStepChapter { forward: true });
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Copy translation").clicked() && !translated.is_empty() {
                ui.ctx().copy_text(translated.clone());
            }
            if ui.button("QA").clicked() {
                app.apply(Action::show_overlay(Overlay::qa_placeholder()));
            }
            if ui.button("Jump…").clicked() {
                app.apply(Action::show_overlay(Overlay::reader_jump_placeholder()));
            }
            if ui.button("Search…").clicked() {
                app.apply(Action::show_overlay(Overlay::reader_search()));
            }
        });
    });
    ui.add_space(8.0);

    if chapter == 0 && ja.is_empty() && translated.is_empty() {
        empty_state(
            ui,
            pal,
            "No chapter loaded",
            "Pick a chapter above, or open one from the Project tree.",
        );
        return;
    }

    let body_h = ui.available_height();
    ui.columns(2, |cols| {
        for c in cols.iter_mut() {
            c.set_min_height(body_h);
            c.set_max_height(body_h);
        }
        card_fill(&mut cols[0], pal, |ui| {
            ui.label(RichText::new("原文  Source").color(pal.ink_soft).strong());
            ui.add_space(4.0);
            scroll_y("reader_ja").show(ui, |ui| {
                ui.label(RichText::new(&ja).color(pal.ja_text).size(15.0));
            });
        });
        card_fill(&mut cols[1], pal, |ui| {
            ui.label(RichText::new("翻訳  Translation").color(pal.ink_soft).strong());
            ui.add_space(4.0);
            scroll_y("reader_tr").show(ui, |ui| {
                if translated.is_empty() {
                    ui.label(
                        RichText::new("Not translated yet.")
                            .color(pal.ink_faint)
                            .italics(),
                    );
                } else {
                    ui.label(RichText::new(&translated).color(pal.translated_text).size(15.0));
                }
            });
        });
    });
}

// ─── Lexicon ─────────────────────────────────────────────────────────────────

fn lexicon(ui: &mut Ui, app: &mut App, nav: &mut GuiNav, pal: &GuiPalette) {
    let Some(active) = app.active.as_ref() else {
        empty_state(ui, pal, "No project open", "Open a project to browse the lexicon.");
        return;
    };
    let ws = active.workspace.clone();

    toolbar_row(ui, |ui| {
        ui.heading(RichText::new("辞  Lexicon").color(pal.ink));
        ui.add_space(12.0);
        for (i, label) in ["Glossary", "Characters", "Style"].iter().enumerate() {
            if ui
                .add_sized(
                    [96.0, 26.0],
                    egui::Button::selectable(nav.lexicon_tab == i, *label),
                )
                .clicked()
            {
                nav.lexicon_tab = i;
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut nav.lexicon_filter)
                    .hint_text("Filter…")
                    .desired_width(180.0),
            );
        });
    });
    ui.add_space(8.0);

    let filter = nav.lexicon_filter.to_lowercase();
    match nav.lexicon_tab {
        0 => {
            let terms = crate::workspace::glossary::load(&ws);
            let terms: Vec<_> = terms
                .into_iter()
                .filter(|t| {
                    filter.is_empty()
                        || t.jp_term.to_lowercase().contains(&filter)
                        || t.translated_term.to_lowercase().contains(&filter)
                })
                .collect();
            card_frame(pal).show(ui, |ui| {
                ui.label(
                    RichText::new(format!("{} terms", terms.len()))
                        .color(pal.ink_faint)
                        .small(),
                );
                scroll_y("glossary_list").show(ui, |ui| {
                    egui::Grid::new("glossary_grid")
                        .num_columns(4)
                        .striped(true)
                        .spacing([16.0, 6.0])
                        .show(ui, |ui| {
                            ui.label(RichText::new("Japanese").color(pal.ink_soft).strong());
                            ui.label(RichText::new("Translation").color(pal.ink_soft).strong());
                            ui.label(RichText::new("Category").color(pal.ink_soft).strong());
                            ui.label("");
                            ui.end_row();
                            for t in &terms {
                                ui.label(RichText::new(&t.jp_term).color(pal.ja_text));
                                ui.label(
                                    RichText::new(&t.translated_term).color(pal.translated_text),
                                );
                                ui.label(
                                    RichText::new(t.category.as_deref().unwrap_or("—"))
                                        .color(pal.ink_faint),
                                );
                                if ui.small_button("✕").clicked() {
                                    app.apply(Action::show_overlay(Overlay::confirm(
                                        "Delete glossary term",
                                        format!("Delete “{}” → “{}”?", t.jp_term, t.translated_term),
                                        Action::DeleteGlossary {
                                            jp_term: t.jp_term.clone(),
                                        },
                                    )));
                                }
                                ui.end_row();
                            }
                        });
                });
            });
        }
        1 => {
            let chars = crate::workspace::characters::load(&ws);
            let chars: Vec<_> = chars
                .into_iter()
                .filter(|c| {
                    filter.is_empty()
                        || c.jp_name.to_lowercase().contains(&filter)
                        || c.translated_name.to_lowercase().contains(&filter)
                        || c.id.to_lowercase().contains(&filter)
                })
                .collect();
            card_frame(pal).show(ui, |ui| {
                ui.label(
                    RichText::new(format!("{} characters", chars.len()))
                        .color(pal.ink_faint)
                        .small(),
                );
                scroll_y("chars_list").show(ui, |ui| {
                    for c in &chars {
                        inset_frame(pal).show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(&c.jp_name).color(pal.ja_text).strong());
                                ui.label(RichText::new("→").color(pal.ink_faint));
                                ui.label(
                                    RichText::new(&c.translated_name)
                                        .color(pal.translated_text)
                                        .strong(),
                                );
                                ui.label(
                                    RichText::new(format!("({})", c.id))
                                        .color(pal.ink_faint)
                                        .small(),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui.small_button("✕").clicked() {
                                        app.apply(Action::show_overlay(Overlay::confirm(
                                            "Delete character",
                                            format!(
                                                "Delete “{}” ({})?",
                                                c.translated_name, c.id
                                            ),
                                            Action::DeleteCharacter { id: c.id.clone() },
                                        )));
                                    }
                                });
                            });
                            if let Some(style) = &c.speech_style {
                                ui.label(RichText::new(style).color(pal.ink_soft).small());
                            }
                        });
                        ui.add_space(4.0);
                    }
                });
            });
        }
        _ => {
            let style_md = std::fs::read_to_string(ws.style_md()).unwrap_or_default();
            card_frame(pal).show(ui, |ui| {
                scroll_y("style_md").show(ui, |ui| {
                    if style_md.is_empty() {
                        ui.label(
                            RichText::new("No STYLE.md yet.")
                                .color(pal.ink_faint)
                                .italics(),
                        );
                    } else {
                        ui.label(RichText::new(style_md).color(pal.ink));
                    }
                });
            });
        }
    }
}

// ─── Refine ──────────────────────────────────────────────────────────────────

fn refine(ui: &mut Ui, app: &mut App, nav: &mut GuiNav, pal: &GuiPalette) {
    if app.active.is_none() {
        empty_state(ui, pal, "No project open", "Open a project to refine translations.");
        return;
    }

    let in_flight = app.refine.is_in_flight();
    let mode = app.refine.approval_mode();
    let (ctx_used, ctx_max) = app.refine.context_meter();
    let plan = app.refine.plan().to_vec();
    let pending = app.refine.pending_prompt();
    let sessions = app.refine_sessions.clone();
    let turns: Vec<(TurnRole, String, bool)> = app
        .refine
        .conversation
        .iter()
        .map(|t| (t.role, t.text.clone(), t.streaming))
        .collect();

    toolbar_row(ui, |ui| {
        ui.heading(RichText::new("磨  Refine").color(pal.ink));
        ui.add_space(8.0);
        if ui
            .button(format!("approval: {}", mode.label()))
            .on_hover_text("cycle always-approve → ask → auto")
            .clicked()
        {
            app.apply(Action::RefineCycleApprovalMode);
        }
        if ctx_used > 0 {
            let pct = (ctx_used as f32 / ctx_max.max(1) as f32 * 100.0).round() as u32;
            ui.label(
                RichText::new(format!("context {pct}%"))
                    .color(pal.ink_faint)
                    .small(),
            );
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.menu_button("⋯", |ui| {
                if ui.button("Undo last edit").clicked() {
                    app.apply(Action::RefineUndo);
                }
                if ui.button("Show diff").clicked() {
                    app.apply(Action::RefineOpenDiff);
                }
                if ui.button("Compact context").clicked() {
                    app.apply(Action::RefineCompact);
                }
                if ui.button("Export conversation").clicked() {
                    app.apply(Action::RefineExport);
                }
                ui.separator();
                if ui.button("Clear conversation").clicked() {
                    app.apply(Action::RefineClear);
                }
            });
            if ui.button("New session").clicked() {
                app.apply(Action::RefineNewSession);
            }
            let active_id = app.refine.active_session_id().to_string();
            let current = sessions
                .iter()
                .find(|s| s.id == active_id)
                .map(|s| s.title.clone())
                .unwrap_or_else(|| "session".to_string());
            egui::ComboBox::from_id_salt("refine_sessions")
                .selected_text(current)
                .width(200.0)
                .show_ui(ui, |ui| {
                    for s in &sessions {
                        let label = format!("{}  ·  {} msgs", s.title, s.message_count);
                        if ui.selectable_label(s.id == active_id, label).clicked() {
                            app.apply(Action::RefineSwitchSession { id: s.id.clone() });
                        }
                    }
                });
        });
    });
    ui.add_space(6.0);

    // Reserve the input strip + optional prompt card at the bottom.
    let prompt_h = if pending.is_some() { 150.0 } else { 0.0 };
    let input_h = 92.0;
    let transcript_h = (ui.available_height() - input_h - prompt_h - 8.0).max(120.0);

    ui.allocate_ui(egui::vec2(ui.available_width(), transcript_h), |ui| {
        let has_plan = !plan.is_empty();
        let plan_w = 240.0;
        ui.horizontal(|ui| {
            let transcript_w = if has_plan {
                ui.available_width() - plan_w - 8.0
            } else {
                ui.available_width()
            };
            ui.allocate_ui(egui::vec2(transcript_w, transcript_h), |ui| {
                card_fill(ui, pal, |ui| {
                    scroll_y("refine_chat").stick_to_bottom(true).show(ui, |ui| {
                        if turns.is_empty() {
                            ui.label(
                                RichText::new(
                                    "Steer the Refine agent: “soften ch 3's dialogue”, “fix the honorifics in vol 2”…",
                                )
                                .color(pal.ink_faint)
                                .italics(),
                            );
                        }
                        for (role, text, streaming) in &turns {
                            match role {
                                TurnRole::User => {
                                    ui.with_layout(Layout::top_down(Align::Max), |ui| {
                                        inset_frame(pal).show(ui, |ui| {
                                            ui.label(RichText::new(text).color(pal.ink));
                                        });
                                    });
                                }
                                TurnRole::Assistant => {
                                    ui.label(RichText::new(text).color(pal.ink));
                                    if *streaming {
                                        ui.label(
                                            RichText::new(theme::spinner_frame(app.frame))
                                                .color(pal.status_working),
                                        );
                                    }
                                }
                                TurnRole::Reasoning => {
                                    ui.label(
                                        RichText::new(text).color(pal.ink_faint).italics().small(),
                                    );
                                }
                                TurnRole::Tool => {
                                    ui.label(
                                        RichText::new(format!("⚒ {text}"))
                                            .color(pal.status_image)
                                            .monospace()
                                            .small(),
                                    );
                                }
                            }
                            ui.add_space(6.0);
                        }
                        if in_flight {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(
                                    RichText::new("working…").color(pal.status_working).small(),
                                );
                            });
                        }
                    });
                });
            });
            if has_plan {
                ui.allocate_ui(egui::vec2(plan_w, transcript_h), |ui| {
                    card_fill(ui, pal, |ui| {
                        ui.label(RichText::new("Plan").color(pal.ink_soft).strong());
                        ui.add_space(4.0);
                        scroll_y("refine_plan").show(ui, |ui| {
                            for step in &plan {
                                let (glyph, color) = match step.status {
                                    PlanStepStatus::Pending => ("○", pal.ink_faint),
                                    PlanStepStatus::InProgress => ("◐", pal.status_working),
                                    PlanStepStatus::Completed => ("●", pal.status_done),
                                };
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(glyph).color(color));
                                    ui.label(RichText::new(&step.step).color(pal.ink).small());
                                });
                            }
                        });
                    });
                });
            }
        });
    });
    ui.add_space(4.0);

    if let Some(prompt) = pending {
        egui::Frame::NONE
            .fill(pal.bg_inset)
            .stroke(egui::Stroke::new(1.0_f32, pal.status_warn))
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.label(RichText::new(&prompt.question).color(pal.ink).strong());
                if !prompt.detail.is_empty() {
                    scroll_y("refine_diff").max_height(60.0).show(ui, |ui| {
                        ui.label(
                            RichText::new(&prompt.detail)
                                .color(pal.ink_soft)
                                .monospace()
                                .small(),
                        );
                    });
                }
                ui.horizontal(|ui| {
                    if prompt.is_approval {
                        if primary_button(ui, pal, "Approve").clicked() {
                            app.apply(Action::RefineRespondInteraction {
                                id: prompt.id,
                                answer: "approve".to_string(),
                            });
                        }
                        if ui.button("Reject").clicked() {
                            app.apply(Action::RefineRespondInteraction {
                                id: prompt.id,
                                answer: String::new(),
                            });
                        }
                    } else if prompt.options.is_empty() {
                        hint(ui, pal, "Answer in the input box below and press Send.");
                    } else {
                        for opt in &prompt.options {
                            if ui.button(opt).clicked() {
                                app.apply(Action::RefineRespondInteraction {
                                    id: prompt.id,
                                    answer: opt.clone(),
                                });
                            }
                        }
                    }
                });
            });
        ui.add_space(4.0);
    }

    // Input strip.
    let free_text_prompt = app
        .refine
        .pending_prompt()
        .map(|p| !p.is_approval && p.options.is_empty())
        .unwrap_or(false);
    ui.horizontal(|ui| {
        let send_w = 90.0;
        ui.add_sized(
            [ui.available_width() - send_w - 8.0, 80.0],
            TextEdit::multiline(&mut nav.refine_input)
                .hint_text("Message the Refine agent…  (@ch3, @vol2 to scope)")
                .desired_rows(3),
        );
        ui.vertical(|ui| {
            let can_send = !nav.refine_input.trim().is_empty();
            if in_flight && !free_text_prompt {
                if ui.add_sized([send_w, 38.0], egui::Button::new("Cancel")).clicked() {
                    app.apply(Action::RefineCancel);
                }
            } else if ui
                .add_enabled_ui(can_send, |ui| {
                    primary_button(ui, pal, if free_text_prompt { "Answer" } else { "Send" })
                })
                .inner
                .clicked()
                && can_send
            {
                let text = nav.refine_input.trim().to_string();
                nav.refine_input.clear();
                if free_text_prompt {
                    if let Some(p) = app.refine.pending_prompt() {
                        app.apply(Action::RefineRespondInteraction {
                            id: p.id,
                            answer: text,
                        });
                    }
                } else {
                    app.apply(Action::RefineSubmit { text });
                }
            }
        });
    });
}

// ─── helpers ─────────────────────────────────────────────────────────────────

fn empty_state(ui: &mut Ui, pal: &GuiPalette, title: &str, body: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space(48.0);
        ui.label(RichText::new(title).color(pal.ink_soft).strong().size(18.0));
        ui.label(RichText::new(body).color(pal.ink_faint));
    });
}
