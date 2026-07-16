//! Native screen bodies — lists, cards, and side-by-side panels (not a terminal grid).

use std::collections::HashSet;

use egui::{Align, Color32, Layout, RichText, ScrollArea, Sense, Ui, scroll_area::ScrollBarVisibility};

use crate::app::{Action, App, Screen};
use crate::model::{Chapter, ChapterKind, ChapterStatus, Project, Volume};
use crate::theme;

use super::theme_map::{GuiPalette, card_fill, card_frame, inset_frame};

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

pub struct GuiNav {
    pub shelf_sel: usize,
    pub project_sel: Option<(u32, u32)>,
    pub project_vol: Option<u32>,
    pub lexicon_tab: usize,
    pub lexicon_filter: String,
}

impl Default for GuiNav {
    fn default() -> Self {
        Self {
            shelf_sel: 0,
            project_sel: None,
            project_vol: None,
            lexicon_tab: 0,
            lexicon_filter: String::new(),
        }
    }
}

pub fn render_body(ui: &mut Ui, app: &mut App, nav: &mut GuiNav, pal: &GuiPalette) {
    match app.screen {
        Screen::Shelf => shelf(ui, app, nav, pal),
        Screen::Project => project(ui, app, nav, pal),
        Screen::Translate => translate(ui, app, pal),
        Screen::Reader => reader(ui, app, pal),
        Screen::Lexicon => lexicon(ui, app, nav, pal),
        Screen::Refine => refine(ui, app, pal),
    }
}

// ─── Shelf ───────────────────────────────────────────────────────────────────

fn shelf(ui: &mut Ui, app: &mut App, nav: &mut GuiNav, pal: &GuiPalette) {
    toolbar_row(ui, |ui| {
        ui.heading(RichText::new("書架  Shelf").color(pal.ink));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Import source…").clicked() {
                app.apply(Action::OpenImport);
            }
            if ui.button("Sample project").clicked() {
                app.apply(Action::CreateSample);
            }
            if ui.button("Rescan").clicked() {
                app.shelf.rescan(&std::env::current_dir().unwrap_or_default());
                app.projects = crate::workspace::scan::scan_projects(
                    &std::env::current_dir().unwrap_or_default(),
                );
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
    let foreign = app
        .foreign_run
        .as_ref()
        .map(|cp| cp.project_dir.clone());

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
            project_card(ui, p, selected, busy, pal, |open| {
                nav.shelf_sel = i;
                if open {
                    app.apply(Action::OpenProject(p.id.clone()));
                }
            });
            ui.add_space(8.0);
        }
    });
}

fn project_card(
    ui: &mut Ui,
    p: &Project,
    selected: bool,
    busy: bool,
    pal: &GuiPalette,
    mut on_click: impl FnMut(bool),
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
                            "{} · {} vol · {} ch · {} done · {}",
                            p.id,
                            vols,
                            chs,
                            done,
                            p.target_language.label()
                        ))
                        .color(pal.ink_faint)
                        .small(),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Open").clicked() {
                        on_click(true);
                    }
                });
            });
        })
        .response
        .interact(Sense::click());
    if response.clicked() {
        on_click(false);
    }
    if response.double_clicked() {
        on_click(true);
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
        ui.heading(RichText::new(&project.title).color(pal.ink));
        if !project.translated_title.is_empty() {
            ui.label(RichText::new(&project.translated_title).color(pal.ink_soft));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Translate all…").clicked() {
                app.apply(Action::StartProjectTranslation);
            }
            if ui
                .button(format!("Translate Vol.{}…", active_vol))
                .clicked()
            {
                app.apply(Action::StartVolumeTranslation { vol: active_vol });
            }
            if ui.button("Export…").clicked() {
                app.apply(Action::show_overlay(crate::app::overlay::Overlay::Export(
                    crate::app::overlay::ExportState {
                        vol: active_vol,
                        formats: [true, true, true],
                        sel: 0,
                        progress: None,
                        done: None,
                    },
                )));
            }
            if ui.button("Add volume…").clicked() {
                app.apply(Action::AddVolume);
            }
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
                                nav.project_sel = Some((vol.number, ch.number));
                                nav.project_vol = Some(vol.number);
                                app.apply(Action::SetActiveVolume { vol: vol.number });
                                app.apply(Action::OpenChapter {
                                    chapter: ch.number,
                                });
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
                        if ui.button("Translate chapter").clicked() {
                            app.apply(Action::StartTranslation {
                                chapters: vec![c],
                            });
                        }
                    });
                }
            } else {
                let vol_n = nav.project_vol.unwrap_or(active_vol);
                if let Some(vol) = project.volumes.iter().find(|v| v.number == vol_n) {
                    detail_volume(ui, vol, pal);
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
    ui.label(
        RichText::new(format!("{done} / {total} chapters done"))
            .color(pal.ink_soft),
    );
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
    ui.label(
        RichText::new(format!("Status: {}", status_label(ch.status)))
            .color(color),
    );
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
    let run = app.translate.run.clone();
    let ch_usage = app.translate.chapter.clone();
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
            if can_pause && ui.button("Pause").clicked() {
                app.apply(Action::PauseRun);
            } else if can_resume && ui.button("Resume").clicked() {
                app.apply(Action::PauseRun);
            } else if !can_pause && !can_resume {
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
            ui.add(
                egui::Label::new(RichText::new(&note).color(pal.ink_soft).small()).truncate(),
            );
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
            scroll_y("thoughts").max_height(160.0).show(ui, |ui| {
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
            ui.label(RichText::new("Queue").color(pal.ink_soft).strong());
            scroll_y("queue").max_height(120.0).show(ui, |ui| {
                if queue.is_empty() {
                    ui.label(RichText::new("empty").color(pal.ink_faint).small());
                }
                for row in &queue {
                    let mark = if row.running { "▶" } else { "·" };
                    ui.label(
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
                    );
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
                    ui.label(
                        RichText::new(&preview)
                            .color(pal.translated_text)
                            .size(15.0),
                    );
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
            .width(280.0)
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
            ui.label(
                RichText::new("翻訳  Translation")
                    .color(pal.ink_soft)
                    .strong(),
            );
            ui.add_space(4.0);
            scroll_y("reader_tr").show(ui, |ui| {
                if translated.is_empty() {
                    ui.label(
                        RichText::new("Not translated yet.")
                            .color(pal.ink_faint)
                            .italics(),
                    );
                } else {
                    ui.label(
                        RichText::new(&translated)
                            .color(pal.translated_text)
                            .size(15.0),
                    );
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
    let ws = &active.workspace;

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
            let terms = crate::workspace::glossary::load(ws);
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
                        .num_columns(3)
                        .striped(true)
                        .spacing([16.0, 6.0])
                        .show(ui, |ui| {
                            ui.label(RichText::new("Japanese").color(pal.ink_soft).strong());
                            ui.label(RichText::new("Translation").color(pal.ink_soft).strong());
                            ui.label(RichText::new("Category").color(pal.ink_soft).strong());
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
                                ui.end_row();
                            }
                        });
                });
            });
        }
        1 => {
            let chars = crate::workspace::characters::load(ws);
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

fn refine(ui: &mut Ui, app: &mut App, pal: &GuiPalette) {
    if app.active.is_none() {
        empty_state(ui, pal, "No project open", "Open a project to refine translations.");
        return;
    }

    ui.heading(RichText::new("磨  Refine").color(pal.ink));
    ui.add_space(6.0);
    card_frame(pal).show(ui, |ui| {
        ui.label(
            RichText::new(
                "The Refine agent polishes existing translations with steering prompts.\n\
                 Full multi-turn refine is available in the TUI; use the terminal for long refine sessions.",
            )
            .color(pal.ink_soft),
        );
        ui.add_space(10.0);
        ui.label(RichText::new("Quick actions").color(pal.ink_soft).strong());
        ui.horizontal(|ui| {
            if ui.button("Open Reader").clicked() {
                app.apply(Action::Goto(Screen::Reader));
            }
            if ui.button("Open Lexicon").clicked() {
                app.apply(Action::Goto(Screen::Lexicon));
            }
            if ui.button("View activity log").clicked() {
                app.apply(Action::show_overlay(crate::app::overlay::Overlay::Log(0)));
            }
        });
        if !app.refine_sessions.is_empty() {
            ui.add_space(10.0);
            ui.label(RichText::new("Recent refine sessions").color(pal.ink_soft).strong());
            for s in app.refine_sessions.iter().take(8) {
                ui.label(
                    RichText::new(format!("· {}", s.id))
                        .color(pal.ink_faint)
                        .small(),
                );
            }
        }
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

// Keep HashSet import used if we expand multi-select later.
#[allow(dead_code)]
fn _hs() -> HashSet<u32> {
    HashSet::new()
}
