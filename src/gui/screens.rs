//! Native screen bodies — lists, cards, and side-by-side panels (not a terminal grid).

use egui::{
    Align, Color32, Layout, RichText, ScrollArea, Sense, TextEdit, Ui,
    scroll_area::ScrollBarVisibility,
};

use crate::app::overlay::Overlay;
use crate::app::refine::{BlockKind, BlockToolStatus as ToolStatus};
use crate::app::{Action, App, Screen};
use crate::model::{Chapter, ChapterKind, ChapterStatus, PlanStepStatus, Project, Volume};
use crate::theme;

use super::theme_map::{GuiPalette, card_fill, card_frame, inset_frame};
use super::widgets::primary_button;

/// Vertical scroller that always reserves its bar — avoids content width jiggle
/// when scrolling becomes necessary (or on hover with floating bars).
fn scroll_y(id: &'static str) -> ScrollArea {
    ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink([false, false])
        .scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible)
}

/// A Reader pane. Unwrapped lines run off the edge, so the pane scrolls both
/// ways rather than folding them.
fn reader_scroll(id: &'static str, wrap: bool) -> ScrollArea {
    let area = ScrollArea::new([!wrap, true])
        .id_salt(id)
        .auto_shrink([false, false]);
    if wrap {
        area.scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible)
    } else {
        area
    }
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
    /// Draft answers to the open `ask_user` card, one per question, keyed by
    /// prompt id so a second card cannot inherit the first one's typing.
    refine_answers: (u64, Vec<String>),
    /// The open glossary/character form, if any.
    pub lexicon_form: Option<super::lexicon_form::LexiconForm>,
    /// Parsed markdown per pane, so a redraw is not a re-parse.
    md_ja: super::markdown::MarkdownCache,
    md_translated: super::markdown::MarkdownCache,
    md_style: super::markdown::MarkdownCache,
    md_preview: super::markdown::MarkdownCache,
    /// One per transcript block. A single cache would thrash between them, and
    /// a streamed delta would re-parse every message above it.
    md_blocks: Vec<super::markdown::MarkdownCache>,
    /// The source pane's scroll offset, mirrored into the translation pane
    /// while the two are synced.
    reader_offset: f32,
    /// Volumes the user has folded by hand; absent means the default, which is
    /// that the active volume is open and the rest are not.
    project_folds: std::collections::HashMap<u32, bool>,
    /// Caret offset in the composer, so `/` and `@` know which token to
    /// complete. egui owns the text; this is only where the caret was.
    refine_cursor: usize,
    /// Which completion row is highlighted.
    refine_completion: usize,
    /// Avoid re-parsing GLOSSARY/CHARACTERS/STYLE every egui frame.
    lexicon_cache: LexiconCache,
}

#[derive(Default)]
struct LexiconCache {
    root: std::path::PathBuf,
    glossary: Option<(Option<std::time::SystemTime>, Vec<crate::model::GlossaryTerm>)>,
    characters: Option<(Option<std::time::SystemTime>, Vec<crate::model::Character>)>,
    style: Option<(Option<std::time::SystemTime>, String)>,
}

fn file_mtime(path: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
}

impl GuiNav {
    /// The cache for transcript block `i`, grown on demand.
    fn md_block(&mut self, i: usize) -> &mut super::markdown::MarkdownCache {
        if self.md_blocks.len() <= i {
            self.md_blocks.resize_with(i + 1, Default::default);
        }
        &mut self.md_blocks[i]
    }
}

impl LexiconCache {
    fn ensure_root(&mut self, root: &std::path::Path) {
        if self.root != root {
            self.root = root.to_path_buf();
            self.glossary = None;
            self.characters = None;
            self.style = None;
        }
    }

    fn glossary(&mut self, ws: &crate::workspace::Workspace) -> &[crate::model::GlossaryTerm] {
        self.ensure_root(&ws.root);
        let path = ws.glossary_md();
        let mtime = file_mtime(&path);
        let stale = self
            .glossary
            .as_ref()
            .is_none_or(|(cached, _)| *cached != mtime);
        if stale {
            self.glossary = Some((mtime, crate::workspace::glossary::load(ws)));
        }
        &self.glossary.as_ref().expect("just populated").1
    }

    fn characters(&mut self, ws: &crate::workspace::Workspace) -> &[crate::model::Character] {
        self.ensure_root(&ws.root);
        let path = ws.characters_md();
        let mtime = file_mtime(&path);
        let stale = self
            .characters
            .as_ref()
            .is_none_or(|(cached, _)| *cached != mtime);
        if stale {
            self.characters = Some((mtime, crate::workspace::characters::load(ws)));
        }
        &self.characters.as_ref().expect("just populated").1
    }

    fn style_md(&mut self, ws: &crate::workspace::Workspace) -> &str {
        self.ensure_root(&ws.root);
        let path = ws.style_md();
        let mtime = file_mtime(&path);
        let stale = self
            .style
            .as_ref()
            .is_none_or(|(cached, _)| *cached != mtime);
        if stale {
            self.style = Some((mtime, std::fs::read_to_string(&path).unwrap_or_default()));
        }
        &self.style.as_ref().expect("just populated").1
    }
}

pub fn render_body(
    ui: &mut Ui,
    app: &mut App,
    nav: &mut GuiNav,
    subject: &mut Option<super::tree::Selection>,
    pal: &GuiPalette,
) {
    match app.screen {
        Screen::Shelf => shelf(ui, app, nav, pal),
        Screen::Project => project(ui, app, nav, pal),
        Screen::Translate => translate(ui, app, nav, pal),
        Screen::Reader => reader(ui, app, nav, pal),
        Screen::Lexicon => lexicon(ui, app, nav, subject, pal),
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
    let foreign = app.foreign_busy_dirs();

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
            // Keyed by the project, not by where it landed: egui flags a widget
            // whose id changes at a rect it already drew, and in a debug build
            // it draws a red box round it.
            ui.push_id(("project_card", p.id.as_str()), |ui| {
                let selected = nav.shelf_sel == i;
                let busy = foreign.iter().any(|d| {
                    crate::workspace::session::same_project_dir(d.as_path(), p.dir.as_path())
                });
                project_card(ui, app, p, selected, busy, pal, |sel| {
                    nav.shelf_sel = sel.unwrap_or(i)
                });
                ui.add_space(8.0);
            });
        }

        // Loose source files, which the window never listed at all — the TUI
        // shows them below a rule and they are the other half of what a shelf
        // holds.
        let loose = app.shelf.unimported().to_vec();
        if !loose.is_empty() {
            ui.add_space(4.0);
            ui.separator();
            ui.label(
                RichText::new(format!("{} importable file(s) here", loose.len()))
                    .color(pal.ink_faint)
                    .small(),
            );
            ui.add_space(4.0);
            for (path, size) in &loose {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                ui.push_id(("loose", name.as_str()), |ui| {
                    inset_frame(pal).show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("＋").color(pal.ink_faint).small());
                            ui.label(RichText::new(&name).color(pal.ink).small());
                            ui.label(
                                RichText::new(fmt_bytes(*size))
                                    .color(pal.ink_faint)
                                    .small(),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.small_button("Import").clicked() {
                                    let projects = app.projects.clone();
                                    let language = app.cfg.preferred_language;
                                    app.apply(Action::show_overlay(Overlay::import(
                                        vec![(path.clone(), *size)],
                                        &projects,
                                        language,
                                    )));
                                }
                            });
                        });
                    });
                });
                ui.add_space(4.0);
            }
        }
    });
}

fn fmt_bytes(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if bytes as f64 >= MB {
        format!("{:.1} MB", bytes as f64 / MB)
    } else {
        format!("{} KB", (bytes / 1024).max(1))
    }
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

    let marked = app.project.marked_count();
    let mut clear_marks = false;
    let mut ran = None;
    toolbar_row(ui, |ui| {
        // The mark set is what batch queue and batch delete act on; without a
        // count there is nothing to tell you a later action is about to touch
        // eight chapters rather than the one under the pointer.
        if marked > 0 {
            ui.label(
                RichText::new(format!("{marked} marked"))
                    .color(pal.accent)
                    .small(),
            );
            if ui.small_button("clear").clicked() {
                clear_marks = true;
            }
            ui.add_space(8.0);
        }
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
            // Drawn from the screen's own table rather than written out: nine
            // of these were literals that had to be kept in step by hand, and
            // the ones nobody remembered are why the window was missing
            // commands the terminal had.
            ran = super::commands::toolbar(ui, app, pal);
        });
    });
    ui.add_space(6.0);

    if clear_marks {
        app.project.clear_marks();
    }
    if let Some(id) = ran {
        let action = app.run_screen_action(id);
        app.apply(action);
    }

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
                    // Folded unless it is the volume being worked on: a
                    // project with eight volumes buried the one you wanted,
                    // and the tree was always fully expanded.
                    let open = nav
                        .project_folds
                        .get(&vol.number)
                        .copied()
                        .unwrap_or(vol.number == active_vol);
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(if open { "▾" } else { "▸" })
                                        .color(pal.ink_faint)
                                        .small(),
                                )
                                .frame(false),
                            )
                            .clicked()
                        {
                            nav.project_folds.insert(vol.number, !open);
                        }
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
                    });
                    if !open {
                        ui.add_space(2.0);
                        continue;
                    }
                    ui.indent(format!("vol_{}", vol.number), |ui| {
                        for ch in &vol.chapters {
                            // Keyed by the chapter: deleting one moves every
                            // row below it into a rect that row already drew,
                            // and egui red-boxes a widget whose id changed
                            // under it.
                            ui.push_id(("chapter", vol.number, ch.number), |ui| {
                                let selected =
                                    nav.project_sel == Some((vol.number, ch.number));
                                let (glyph, color) = status_chip(ch, pal);
                                ui.horizontal(|ui| {
                                    // The mark set is `ProjectScreen`'s, the same
                                    // one Space toggles and the one batch queue
                                    // and batch delete act on. The GUI had no way
                                    // to mark anything, so those were
                                    // single-chapter only.
                                    let mut marked =
                                        app.project.is_marked(vol.number, ch.number);
                                    if ui
                                        .checkbox(&mut marked, "")
                                        .on_hover_text("mark for queue or delete")
                                        .changed()
                                    {
                                        app.project.toggle_mark(vol.number, ch.number);
                                    }
                                    let label = format!(
                                        "{}  ch {:03}  {}",
                                        glyph,
                                        ch.number,
                                        if ch.title.is_empty() { "—" } else { &ch.title }
                                    );
                                    let response = ui.selectable_label(
                                        selected,
                                        RichText::new(label)
                                            .color(if selected { pal.ink } else { color }),
                                    );
                                    if response.clicked() {
                                        nav.project_sel = Some((vol.number, ch.number));
                                        nav.project_vol = Some(vol.number);
                                        app.apply(Action::SetActiveVolume { vol: vol.number });
                                    }
                                    if response.double_clicked() {
                                        app.apply(Action::OpenChapter { chapter: ch.number });
                                    }
                                });
                            });
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
                            // Details can show a chapter whose volume is not the
                            // live active one (e.g. after VolumeStarted); restore it.
                            app.apply(Action::SetActiveVolume { vol: v });
                            app.apply(Action::OpenChapter { chapter: c });
                        }
                        if ui.button("Translate").clicked() {
                            app.apply(Action::SetActiveVolume { vol: v });
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

/// Old against new, for a chapter that has been retranslated.
///
/// Changed lines are tinted rather than prefixed, because the text is prose
/// and a `+`/`-` gutter in the middle of a sentence reads as punctuation.
fn reader_diff(ui: &mut Ui, app: &App, pal: &GuiPalette) {
    let Some(d) = app.reader.diff_view() else {
        return;
    };
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("−{} removed   +{} added", d.removed, d.added))
                .color(pal.ink_soft)
                .small(),
        );
        if let (Some(old), Some(new)) = (d.old_cost, d.new_cost) {
            ui.label(
                RichText::new(format!("${old:.4} → ${new:.4}"))
                    .color(pal.ink_faint)
                    .small(),
            );
        }
    });
    ui.add_space(4.0);

    let body_h = ui.available_height();
    ui.columns(2, |cols| {
        for c in cols.iter_mut() {
            c.set_min_height(body_h);
            c.set_max_height(body_h);
        }
        for (n, (label, text, changed, tint)) in [
            (d.old_label, d.old_translation, d.old_changed, pal.status_failed),
            (d.new_label, d.new_translation, d.new_changed, pal.status_done),
        ]
        .into_iter()
        .enumerate()
        {
            card_fill(&mut cols[n], pal, |ui| {
                ui.label(RichText::new(label).color(pal.ink_soft).strong());
                ui.add_space(4.0);
                scroll_y(if n == 0 { "reader_diff_old" } else { "reader_diff_new" }).show(
                    ui,
                    |ui| {
                        for (i, line) in text.lines().enumerate() {
                            let hot = changed.get(i).copied().unwrap_or(false);
                            ui.label(
                                RichText::new(line)
                                    .color(if hot { tint } else { pal.ink_soft })
                                    .size(15.0),
                            );
                        }
                    },
                );
            });
        }
    });
}

/// The Reader's own status line: what the search found, how many bookmarks and
/// review flags this chapter carries. None of it was visible in the window.
fn reader_status(ui: &mut Ui, app: &App, pal: &GuiPalette) {
    let search = app.reader.search_status();
    let bookmarks = app.reader.bookmark_count();
    let flags = app.reader.review_count();
    if search.is_none() && bookmarks == 0 && flags == 0 {
        return;
    }
    ui.horizontal(|ui| {
        if let Some((query, hit, total)) = search {
            ui.label(
                RichText::new(format!("“{query}”  {hit}/{total}"))
                    .color(pal.accent)
                    .small(),
            );
        }
        if bookmarks > 0 {
            ui.label(
                RichText::new(format!("★ {bookmarks}"))
                    .color(pal.ink_soft)
                    .small(),
            );
        }
        if flags > 0 {
            ui.label(
                RichText::new(format!("⚑ {flags} need review"))
                    .color(pal.status_warn)
                    .small(),
            );
        }
    });
    ui.add_space(4.0);
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

fn translate(ui: &mut Ui, app: &mut App, nav: &mut GuiNav, pal: &GuiPalette) {
    let th = app.theme.clone();
    let mut ran = None;
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
            // Stop now goes through its declared command, which raises the
            // confirm dialog the TUI has always had; this button fired
            // `StopRun` outright.
            ran = super::commands::toolbar(ui, app, pal);
        });
    });
    ui.add_space(6.0);
    if let Some(id) = ran {
        let action = app.run_screen_action(id);
        app.apply(action);
    }

    // What a non-default service tier trades away. The window never said, so a
    // run on Flex looked identical to one on the standard tier.
    if let Some(tier) = app.cfg.service_tier {
        let (label, color) = match tier {
            crate::model::ServiceTier::Flex => ("Flex", pal.status_warn),
            crate::model::ServiceTier::Priority => ("Priority", pal.accent),
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("⚑ {label} tier")).color(color).small().strong());
            ui.label(
                RichText::new(crate::model::ServiceTier::desc(Some(tier)))
                    .color(pal.ink_faint)
                    .small(),
            );
        });
        ui.add_space(4.0);
    }

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
            // A pipeline, not three unrelated rows: the chunk goes through
            // these in order, and which one has it is the thing to see at a
            // glance. The window drew them flat.
            let roles = ["◆ Orchestrator", "▲ Translator", "■ Reviewer"];
            for (i, (role, line)) in roles.iter().zip(agent_lines.iter()).enumerate() {
                let active = i == active_agent && (running || paused);
                let done = (running || paused) && i < active_agent;
                let color = if active {
                    pal.status_working
                } else if done {
                    pal.status_done
                } else {
                    pal.ink_soft
                };
                let prefix = if active {
                    theme::spinner_frame(app.frame)
                } else if done {
                    "✓"
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
                // The flow between them, so the order reads as an order.
                if i + 1 < roles.len() {
                    ui.horizontal(|ui| {
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("│")
                                .color(if done { pal.status_done } else { pal.rule })
                                .monospace()
                                .small(),
                        );
                    });
                }
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
                    // Keyed by the chapter: the queue reorders under the
                    // pointer as ▲▼ move rows and as chapters finish.
                    let _row = ui.push_id(("queue", row.vol, row.number), |ui| {
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
                    super::markdown::show(
                        ui,
                        &mut nav.md_preview,
                        &preview,
                        &th,
                        pal.translated_text,
                    );
                }
            });
        });
    });
}

// ─── Reader ──────────────────────────────────────────────────────────────────

fn reader(ui: &mut Ui, app: &mut App, nav: &mut GuiNav, pal: &GuiPalette) {
    let mut ran = None;
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
            // Twenty declared commands, six of which the window used to draw.
            // Drawn from the table, the other fourteen came with them — diff,
            // bookmarks, the review-flag jump, the layout modes, search
            // next/prev — and the copy button stopped duplicating `A_COPY`
            // badly.
            ran = super::commands::toolbar(ui, app, pal);
        });
    });
    ui.add_space(8.0);
    if let Some(id) = ran {
        let action = app.run_screen_action(id);
        app.apply(action);
    }

    if chapter == 0 && ja.is_empty() && translated.is_empty() {
        empty_state(
            ui,
            pal,
            "No chapter loaded",
            "Pick a chapter above, or open one from the Project tree.",
        );
        return;
    }

    // A retranslated chapter can be read against its previous version. `d`
    // entered that mode already; the window simply kept drawing the panes.
    if app.reader.diff_view().is_some() {
        reader_diff(ui, app, pal);
        return;
    }

    // What the mode cycle asked for. The GUI was permanently 50/50, so `o`
    // toggled something nothing could see.
    let show_ja = app.reader.shows_source();
    let show_tr = app.reader.shows_translation();
    reader_status(ui, app, pal);

    // Wrap, sync and highlight all changed nothing in the window: the panes
    // wrapped unconditionally, scrolled independently, and tinted nothing.
    let wrap = app.reader.is_wrapped();
    let synced = app.reader.is_synced();
    let (hl_ja, hl_tr) = app.reader.highlights();
    let (hl_ja, hl_tr) = (hl_ja.to_vec(), hl_tr.to_vec());

    let body_h = ui.available_height();
    let th = &app.theme;
    let panes = usize::from(show_ja) + usize::from(show_tr);
    ui.columns(panes.max(1), |cols| {
        for c in cols.iter_mut() {
            c.set_min_height(body_h);
            c.set_max_height(body_h);
        }
        let mut next = 0;
        if show_ja {
            card_fill(&mut cols[next], pal, |ui| {
                ui.label(RichText::new("原文  Source").color(pal.ink_soft).strong());
                ui.add_space(4.0);
                let out = reader_scroll("reader_ja", wrap).show(ui, |ui| {
                    // Rendered rather than printed: ruby, image links and review
                    // banners used to arrive as literal markdown.
                    super::markdown::show_with(
                        ui,
                        &mut nav.md_ja,
                        &ja,
                        th,
                        pal.ja_text,
                        &super::markdown::Options {
                            wrap,
                            highlight: &hl_ja,
                            tint: Some(pal.accent),
                        },
                    );
                });
                if synced {
                    nav.reader_offset = out.state.offset.y;
                }
            });
            next += 1;
        }
        if !show_tr {
            return;
        }
        card_fill(&mut cols[next], pal, |ui| {
            ui.label(RichText::new("翻訳  Translation").color(pal.ink_soft).strong());
            ui.add_space(4.0);
            let mut tr = reader_scroll("reader_tr", wrap);
            // Synced panes share one offset, which is what the sync toggle is
            // for: reading a translation against its source line by line.
            if synced {
                tr = tr.vertical_scroll_offset(nav.reader_offset);
            }
            tr.show(ui, |ui| {
                if translated.is_empty() {
                    ui.label(
                        RichText::new("Not translated yet.")
                            .color(pal.ink_faint)
                            .italics(),
                    );
                } else {
                    super::markdown::show_with(
                        ui,
                        &mut nav.md_translated,
                        &translated,
                        th,
                        pal.translated_text,
                        &super::markdown::Options {
                            wrap,
                            highlight: &hl_tr,
                            tint: Some(pal.accent),
                        },
                    );
                }
            });
        });
    });
}

// ─── Lexicon ─────────────────────────────────────────────────────────────────

fn lexicon(
    ui: &mut Ui,
    app: &mut App,
    nav: &mut GuiNav,
    subject: &mut Option<super::tree::Selection>,
    pal: &GuiPalette,
) {
    let th = &app.theme;
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
            // The GUI could not create either of these at all.
            if nav.lexicon_tab < 2 && ui.button("＋ New").clicked() {
                nav.lexicon_form = Some(super::lexicon_form::LexiconForm::new(
                    if nav.lexicon_tab == 0 {
                        crate::app::lexicon_defs::DraftEntry::Glossary(Box::default())
                    } else {
                        crate::app::lexicon_defs::DraftEntry::Character(Box::default())
                    },
                    true,
                ));
            }
        });
    });
    ui.add_space(8.0);

    let filter = nav.lexicon_filter.to_lowercase();
    match nav.lexicon_tab {
        0 => {
            let terms: Vec<_> = nav
                .lexicon_cache
                .glossary(&ws)
                .iter()
                .filter(|t| {
                    filter.is_empty()
                        || t.jp_term.to_lowercase().contains(&filter)
                        || t.translated_term.to_lowercase().contains(&filter)
                })
                .cloned()
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
                                // Picking a row puts the whole entry in the
                                // inspector — the table shows three columns of
                                // what a term actually carries.
                                if ui
                                    .selectable_label(
                                        *subject
                                            == Some(super::tree::Selection::Term(
                                                t.jp_term.clone(),
                                            )),
                                        RichText::new(&t.jp_term).color(pal.ja_text),
                                    )
                                    .clicked()
                                {
                                    *subject = Some(super::tree::Selection::Term(
                                        t.jp_term.clone(),
                                    ));
                                }
                                ui.label(
                                    RichText::new(&t.translated_term).color(pal.translated_text),
                                );
                                ui.label(
                                    RichText::new(t.category.as_deref().unwrap_or("—"))
                                        .color(pal.ink_faint),
                                );
                                if ui.small_button("✎").on_hover_text("edit").clicked() {
                                    nav.lexicon_form =
                                        Some(super::lexicon_form::LexiconForm::new(
                                            crate::app::lexicon_defs::DraftEntry::Glossary(
                                                Box::new(t.clone()),
                                            ),
                                            false,
                                        ));
                                }
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
            let chars: Vec<_> = nav
                .lexicon_cache
                .characters(&ws)
                .iter()
                .filter(|c| {
                    filter.is_empty()
                        || c.jp_name.to_lowercase().contains(&filter)
                        || c.translated_name.to_lowercase().contains(&filter)
                        || c.id.to_lowercase().contains(&filter)
                })
                .cloned()
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
                                if ui
                                    .selectable_label(
                                        *subject
                                            == Some(super::tree::Selection::Character(
                                                c.jp_name.clone(),
                                            )),
                                        RichText::new(&c.jp_name).color(pal.ja_text).strong(),
                                    )
                                    .clicked()
                                {
                                    *subject = Some(super::tree::Selection::Character(
                                        c.jp_name.clone(),
                                    ));
                                }
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
                                    if ui.small_button("✎").on_hover_text("edit").clicked() {
                                        nav.lexicon_form =
                                            Some(super::lexicon_form::LexiconForm::new(
                                                crate::app::lexicon_defs::DraftEntry::Character(
                                                    Box::new(c.clone()),
                                                ),
                                                false,
                                            ));
                                    }
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
            let style_md = nav.lexicon_cache.style_md(&ws).to_owned();
            card_frame(pal).show(ui, |ui| {
                scroll_y("style_md").show(ui, |ui| {
                    if style_md.is_empty() {
                        ui.label(
                            RichText::new("No STYLE.md yet.")
                                .color(pal.ink_faint)
                                .italics(),
                        );
                    } else {
                        super::markdown::show(
                            ui,
                            &mut nav.md_style,
                            &style_md,
                            th,
                            pal.ink,
                        );
                    }
                });
            });
        }
    }

    lexicon_form_modal(ui, app, nav, pal);
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
    // The TUI transcript is blocks with fold state; the GUI draws roles, so it
    // takes the block's role and whatever the block would say when open.
    let block_count = app.refine.blocks.len();
    let th = app.theme.clone();

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
                nav.refine_input.clear();
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
                        // Keyed by the conversation: renaming or deleting one
                        // shifts the rest into rects they did not draw.
                        ui.push_id(("session", s.id.as_str()), |ui| {
                            let label =
                                format!("{}  ·  {} msgs", s.title, s.message_count);
                            if ui.selectable_label(s.id == active_id, label).clicked() {
                                nav.refine_input.clear();
                                app.apply(Action::RefineSwitchSession { id: s.id.clone() });
                            }
                        });
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
                        if block_count == 0 {
                            ui.label(
                                RichText::new(
                                    "Steer the Refine agent: “soften ch 3's dialogue”, “fix the honorifics in vol 2”…",
                                )
                                .color(pal.ink_faint)
                                .italics(),
                            );
                        }
                        // Blocks, not flattened roles: a tool call keeps its
                        // status and its detail behind a fold, a sub-agent gets
                        // a row of its own, and a notice reads as a notice.
                        for i in 0..block_count {
                            let Some(b) = app.refine.blocks.get(i) else {
                                break;
                            };
                            let open = b.open;
                            let collapsible = b.collapsible();
                            let role = b.role();
                            let body = b.body.clone();
                            let detail = b.detail.clone();
                            let streaming = b.streaming;
                            let kind = b.kind.clone();
                            let mut toggle = false;
                            let mut take = false;
                            let chosen = app.refine.selected_block() == Some(i);

                            let shown = ui.push_id(("block", b.id), |ui| match &kind {
                                BlockKind::User => {
                                    ui.with_layout(Layout::top_down(Align::Max), |ui| {
                                        inset_frame(pal).show(ui, |ui| {
                                            ui.label(RichText::new(&body).color(pal.ink));
                                        });
                                    });
                                }
                                BlockKind::Assistant => {
                                    super::markdown::show(
                                        ui,
                                        nav.md_block(i),
                                        &body,
                                        &th,
                                        pal.ink,
                                    );
                                    if streaming {
                                        ui.label(
                                            RichText::new(theme::spinner_frame(app.frame))
                                                .color(pal.status_working),
                                        );
                                    }
                                }
                                BlockKind::Reasoning => {
                                    let head = if open {
                                        "▾ 💭 thinking".to_string()
                                    } else {
                                        format!(
                                            "▸ 💭 thinking — {} line(s)",
                                            body.lines().count().max(1)
                                        )
                                    };
                                    if ui
                                        .add(
                                            egui::Label::new(
                                                RichText::new(head)
                                                    .color(pal.ink_faint)
                                                    .italics()
                                                    .small(),
                                            )
                                            .sense(egui::Sense::click()),
                                        )
                                        .clicked()
                                    {
                                        toggle = true;
                                    }
                                    if open {
                                        ui.label(
                                            RichText::new(&body)
                                                .color(pal.ink_faint)
                                                .italics()
                                                .small(),
                                        );
                                    }
                                }
                                BlockKind::Tool { name, status, .. } => {
                                    let (mark, color) = match status {
                                        ToolStatus::Running => ("·", pal.ink_soft),
                                        ToolStatus::Ok => ("✓", pal.status_done),
                                        ToolStatus::Failed => ("!", pal.status_failed),
                                    };
                                    let caret = if !collapsible {
                                        " "
                                    } else if open {
                                        "▾"
                                    } else {
                                        "▸"
                                    };
                                    if ui
                                        .add(
                                            egui::Label::new(
                                                RichText::new(format!(
                                                    "{caret} {mark} {name}  {}",
                                                    body.trim()
                                                ))
                                                .color(color)
                                                .monospace()
                                                .small(),
                                            )
                                            .sense(egui::Sense::click())
                                            .truncate(),
                                        )
                                        .clicked()
                                    {
                                        toggle = true;
                                    }
                                    if open && !detail.trim().is_empty() {
                                        ui.horizontal(|ui| {
                                            ui.add_space(14.0);
                                            ui.label(
                                                RichText::new(detail.trim())
                                                    .color(pal.ink_faint)
                                                    .monospace()
                                                    .small(),
                                            );
                                        });
                                    }
                                }
                                BlockKind::Subagent { .. } => {
                                    let caret = if open { "▾" } else { "▸" };
                                    if ui
                                        .add(
                                            egui::Label::new(
                                                RichText::new(format!(
                                                    "{caret} ◇ {}",
                                                    body.trim()
                                                ))
                                                .color(pal.accent)
                                                .small(),
                                            )
                                            .sense(egui::Sense::click()),
                                        )
                                        .on_hover_text("its own conversation")
                                        .clicked()
                                    {
                                        toggle = true;
                                    }
                                    if open && !detail.trim().is_empty() {
                                        ui.horizontal(|ui| {
                                            ui.add_space(14.0);
                                            ui.label(
                                                RichText::new(detail.trim())
                                                    .color(pal.ink_soft)
                                                    .monospace()
                                                    .small(),
                                            );
                                        });
                                    }
                                }
                                BlockKind::Notice => {
                                    ui.label(
                                        RichText::new(body.trim())
                                            .color(pal.ink_faint)
                                            .small(),
                                    );
                                }
                            });
                            let _ = role;
                            // A click anywhere in a block selects it, which is
                            // what ⌃B copies — the window had no block cursor,
                            // so that command had nothing to act on.
                            if shown.response.interact(egui::Sense::click()).clicked() {
                                take = true;
                            }
                            if chosen {
                                ui.painter().rect_stroke(
                                    shown.response.rect.expand(2.0),
                                    4.0,
                                    egui::Stroke::new(1.0_f32, pal.accent),
                                    egui::StrokeKind::Outside,
                                );
                            }
                            if take {
                                app.refine.select_block(Some(i));
                            }
                            if toggle && let Some(b) = app.refine.blocks.get_mut(i) {
                                b.toggle();
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
        // Each card gets its own drafts: the id changes, so a second question
        // cannot arrive pre-filled with what was typed at the first.
        if nav.refine_answers.0 != prompt.id {
            nav.refine_answers = (prompt.id, vec![String::new(); prompt.questions.len()]);
        }
        egui::Frame::NONE
            .fill(pal.bg_inset)
            .stroke(egui::Stroke::new(1.0_f32, pal.status_warn))
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                if prompt.is_approval {
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
                    });
                    return;
                }
                scroll_y("refine_questions").max_height(220.0).show(ui, |ui| {
                    for (i, q) in prompt.questions.iter().enumerate() {
                        if i > 0 {
                            ui.add_space(6.0);
                        }
                        let counter = if prompt.questions.len() > 1 {
                            format!("{}/{}  ", i + 1, prompt.questions.len())
                        } else {
                            String::new()
                        };
                        ui.label(
                            RichText::new(format!("{counter}{}", q.question))
                                .color(pal.ink)
                                .strong(),
                        );
                        ui.horizontal_wrapped(|ui| {
                            for opt in &q.options {
                                let picked = nav.refine_answers.1.get(i) == Some(opt);
                                if ui.selectable_label(picked, opt).clicked()
                                    && let Some(slot) = nav.refine_answers.1.get_mut(i)
                                {
                                    *slot = opt.clone();
                                }
                            }
                        });
                        if let Some(slot) = nav.refine_answers.1.get_mut(i) {
                            ui.add(
                                TextEdit::singleline(slot)
                                    .hint_text("or type your own answer")
                                    .desired_width(f32::INFINITY),
                            );
                        }
                    }
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let any = nav.refine_answers.1.iter().any(|a| !a.trim().is_empty());
                    if ui
                        .add_enabled_ui(any, |ui| primary_button(ui, pal, "Send answers"))
                        .inner
                        .clicked()
                        && any
                    {
                        let answers: Vec<String> = nav
                            .refine_answers
                            .1
                            .iter()
                            .map(|a| a.trim().to_string())
                            .collect();
                        app.apply(Action::RefineRespondInteraction {
                            id: prompt.id,
                            answer: serde_json::to_string(&answers).unwrap_or_default(),
                        });
                    }
                    if ui.button("Dismiss").clicked() {
                        app.apply(Action::RefineRespondInteraction {
                            id: prompt.id,
                            answer: String::new(),
                        });
                    }
                });
            });
        ui.add_space(4.0);
    }

    // Input strip. The card above owns answering now, so Send is only ever
    // Send — it used to double as "Answer" and quietly steal the message.
    ui.horizontal(|ui| {
        let send_w = 90.0;
        let out = TextEdit::multiline(&mut nav.refine_input)
            .hint_text("Message the Refine agent…  (@ch3, @vol2 to scope · ⇧↵ for a new line)")
            .desired_rows(3)
            .show(ui);
        let field = out.response;
        // egui owns the text; we only need to know where the caret is, so the
        // same completion lists the terminal offers can be offered here.
        if let Some(range) = out.cursor_range {
            nav.refine_cursor = range.primary.index.min(nav.refine_input.len());
        }
        // What `/` and `@` offer for the token under the caret — the same
        // lists the terminal offers, from the same function. The window had no
        // completion at all: typing `/model` just sent it as prose.
        let project = app.active.as_ref().map(|a| &a.project);
        let offers = if field.has_focus() {
            crate::app::refine::completions(&nav.refine_input, nav.refine_cursor, project)
        } else {
            Vec::new()
        };
        let completing = !offers.is_empty();
        if completing {
            nav.refine_completion = nav.refine_completion.min(offers.len() - 1);
        } else {
            nav.refine_completion = 0;
        }

        // Enter sends, Shift-Enter breaks the line. Send used to be
        // button-only: the field had no key handling at all, so Enter simply
        // inserted a newline and there was no way to send from the keyboard.
        // While a completion is up, Enter and Tab take it instead.
        let (enter, tab, step) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::Enter) && !i.modifiers.shift,
                i.key_pressed(egui::Key::Tab),
                i.key_pressed(egui::Key::ArrowDown) as isize
                    - i.key_pressed(egui::Key::ArrowUp) as isize,
            )
        });
        let mut send = field.lost_focus() && enter && !completing;
        if completing {
            if step != 0 {
                let last = offers.len() - 1;
                nav.refine_completion =
                    (nav.refine_completion as isize + step).clamp(0, last as isize) as usize;
            }
            let mut accepted = (enter || tab)
                .then(|| offers[nav.refine_completion].insert.clone());
            completion_popup(ui, &offers, nav.refine_completion, pal, &mut accepted);
            if let Some(insert) = accepted {
                crate::app::refine::accept_completion(
                    &mut nav.refine_input,
                    &mut nav.refine_cursor,
                    &insert,
                );
                nav.refine_completion = 0;
                field.request_focus();
            }
        }
        ui.vertical(|ui| {
            let can_send = !nav.refine_input.trim().is_empty();
            if in_flight {
                if ui.add_sized([send_w, 38.0], egui::Button::new("Cancel")).clicked() {
                    app.apply(Action::RefineCancel);
                }
                send = false;
            } else if ui
                .add_enabled_ui(can_send, |ui| primary_button(ui, pal, "Send"))
                .inner
                .clicked()
            {
                send = true;
            }
            if send && can_send {
                let text = nav.refine_input.trim().to_string();
                nav.refine_input.clear();
                nav.refine_cursor = 0;
                app.apply(Action::RefineSubmit { text });
                field.request_focus();
            }
        });
    });
}

// ─── helpers ─────────────────────────────────────────────────────────────────

/// The open form, over the lexicon it is editing.
///
/// A write rather than a merge: the draft *is* the entry, so a field the user
/// cleared actually clears instead of coming back from the stored copy.
fn lexicon_form_modal(ui: &mut Ui, app: &mut App, nav: &mut GuiNav, pal: &GuiPalette) {
    use super::lexicon_form::Outcome;
    let Some(form) = nav.lexicon_form.as_mut() else {
        return;
    };
    let ws = app.active.as_ref().map(|a| &a.workspace);
    let title = form.title();
    let mut outcome = Outcome::Open;
    egui::Modal::new(egui::Id::new("lexicon_form_modal")).show(ui.ctx(), |ui| {
        ui.set_width(560.0);
        ui.heading(RichText::new(title).color(pal.ink));
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("lexicon_form_scroll")
            .max_height(460.0)
            .show(ui, |ui| {
                outcome = super::lexicon_form::show(ui, form, ws, pal);
            });
    });

    match outcome {
        Outcome::Open => {}
        Outcome::Cancel => nav.lexicon_form = None,
        Outcome::Save => {
            if form.missing_key().is_some() {
                return;
            }
            let Some(ws) = app.active.as_ref().map(|a| a.workspace.clone()) else {
                return;
            };
            let saved = match &form.draft {
                crate::app::lexicon_defs::DraftEntry::Glossary(t) => {
                    crate::workspace::glossary::replace(&ws, (**t).clone())
                }
                crate::app::lexicon_defs::DraftEntry::Character(c) => {
                    crate::workspace::characters::replace(&ws, (**c).clone())
                }
                crate::app::lexicon_defs::DraftEntry::StyleNote(text) => {
                    crate::workspace::style::append_note(&ws, text)
                }
            };
            match saved {
                Ok(()) => {
                    nav.lexicon_form = None;
                    nav.lexicon_cache = LexiconCache::default();
                    app.apply(Action::Notify {
                        level: crate::model::LogLevel::Info,
                        msg: "saved".to_string(),
                    });
                }
                Err(e) => app.apply(Action::Notify {
                    level: crate::model::LogLevel::Error,
                    msg: format!("could not save: {e}"),
                }),
            }
        }
    }
}

/// The completion list, floating above the composer.
fn completion_popup(
    ui: &mut Ui,
    offers: &[crate::app::refine::Completion],
    sel: usize,
    pal: &GuiPalette,
    accepted: &mut Option<String>,
) {
    let anchor = ui.min_rect();
    egui::Area::new(egui::Id::new("refine_completions"))
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(anchor.left(), anchor.top() - 8.0))
        .pivot(egui::Align2::LEFT_BOTTOM)
        .show(ui.ctx(), |ui| {
            inset_frame(pal).show(ui, |ui| {
                ui.set_max_width(420.0);
                for (i, c) in offers.iter().take(10).enumerate() {
                    if ui
                        .selectable_label(i == sel, RichText::new(&c.label).small())
                        .clicked()
                    {
                        *accepted = Some(c.insert.clone());
                    }
                }
                if offers.len() > 10 {
                    ui.label(
                        RichText::new(format!("… {} more", offers.len() - 10))
                            .color(pal.ink_faint)
                            .small(),
                    );
                }
            });
        });
}

fn empty_state(ui: &mut Ui, pal: &GuiPalette, title: &str, body: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space(48.0);
        ui.label(RichText::new(title).color(pal.ink_soft).strong().size(18.0));
        ui.label(RichText::new(body).color(pal.ink_faint));
    });
}
