//! Native modal dialogs for every overlay: welcome, theme, settings, the import
//! wizard, palette, QA, synopsis/title editors, reader tools, export, and more.
//!
//! Dialogs mutate the live overlay state in place (like the TUI's key handlers)
//! and defer every app mutation into an action list routed through `apply()`.

use egui::{Align, Color32, Context, Layout, RichText, ScrollArea, TextEdit, Ui, Window};

use crate::app::overlay::{
    Dialog, ExportState, ImageSourceState, ImportState, JumpKind, Overlay, PaletteState, QaState,
    ReaderEditState, ReaderInspectState, ReaderJumpState, ReaderNoteState, ReaderSearchState,
    SETTINGS_KEY_FIELD, SynPhase, SynopsisEditState, SynopsisState, TitleEditState, WelcomeState,
    prettify_stem,
};
use crate::app::qa::QaKind;
use crate::app::{Action, App};
use crate::model::{LogLevel, TargetLanguage};
use crate::theme::ALL_THEMES;

use super::settings;
use super::theme_map::GuiPalette;
use super::widgets::{hint, primary_button, section, theme_swatch};

/// Draw any open overlay as a centered native window / modal.
/// Returns true if an overlay consumed the frame (dim the background).
pub fn render(ui: &mut Ui, app: &mut App, pal: &GuiPalette) -> bool {
    if matches!(app.overlay, Overlay::None) {
        return false;
    }

    // Dim the full viewport *behind* the modal. Scrim stays on Middle; modal
    // windows use Foreground so the dim never paints over the dialog.
    let screen = ui.ctx().content_rect();
    egui::Area::new(egui::Id::new("honya_modal_scrim"))
        .fixed_pos(screen.min)
        .order(egui::Order::Middle)
        .interactable(true)
        .show(ui.ctx(), |ui| {
            ui.painter()
                .rect_filled(screen, 0.0, Color32::from_black_alpha(140));
            // Swallow clicks on the dimmed backdrop.
            ui.allocate_rect(screen, egui::Sense::click());
        });

    let ctx = ui.ctx().clone();
    let frame = modal_frame(pal);
    let saved_theme = app.cfg.theme;
    let codex_signed_in = app.cfg.codex_auth.is_some();
    let mut actions: Vec<Action> = Vec::new();

    // Log needs a second (read-only) borrow of app state next to the overlay.
    if matches!(app.overlay, Overlay::Log(_)) {
        log_panel(&ctx, &app.log, pal, frame, &mut actions);
    } else {
        match &mut app.overlay {
            Overlay::None | Overlay::Log(_) => {}
            Overlay::Welcome(st) => welcome(&ctx, st, pal, frame, &mut actions),
            Overlay::Theme(st) => theme_picker(&ctx, st, saved_theme, pal, frame, &mut actions),
            Overlay::Settings(st) => settings::render(
                &ctx,
                st,
                saved_theme,
                codex_signed_in,
                pal,
                frame,
                &mut actions,
            ),
            Overlay::Modal(dlg) => confirm(&ctx, dlg, pal, frame, &mut actions),
            Overlay::Help(_) => help_panel(&ctx, pal, frame, &mut actions),
            Overlay::About => about_panel(&ctx, pal, frame, &mut actions),
            Overlay::Export(st) => export_panel(&ctx, st, pal, frame, &mut actions),
            Overlay::Import(st) => import_wizard(&ctx, st, pal, frame, &mut actions),
            Overlay::ImageSource(st) => image_source(&ctx, st, pal, frame, &mut actions),
            Overlay::Palette(st) => palette(&ctx, st, pal, frame, &mut actions),
            Overlay::Qa(st) => qa_panel(&ctx, st, pal, frame, &mut actions),
            Overlay::Synopsis(st) => synopsis_editor(&ctx, st, pal, frame, &mut actions),
            Overlay::ProjectTitle(st) => title_editor(&ctx, st, pal, frame, &mut actions),
            Overlay::ReaderNote(st) => reader_note(&ctx, st, pal, frame, &mut actions),
            Overlay::ReaderInspect(st) => reader_inspect(&ctx, st, pal, frame, &mut actions),
            Overlay::ReaderEdit(st) => reader_edit(&ctx, st, pal, frame, &mut actions),
            Overlay::ReaderSearch(st) => reader_search(&ctx, st, pal, frame, &mut actions),
            Overlay::ReaderJump(st) => reader_jump(&ctx, st, pal, frame, &mut actions),
        }
    }

    for a in actions {
        app.apply(a);
    }
    true
}

/// Modal dialogs always sit above the dim scrim (Middle).
fn modal_window(title: impl Into<egui::WidgetText>) -> Window<'static> {
    Window::new(title)
        .collapsible(false)
        .resizable(false)
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
}

fn modal_frame(pal: &GuiPalette) -> egui::Frame {
    // Tight frame: no large drop-shadow. Big blur radii paint far outside the
    // window rect and make the modal look bigger than its interactive area.
    egui::Frame::NONE
        .fill(pal.bg_panel)
        .stroke(egui::Stroke::new(1.0_f32, pal.rule))
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(egui::Margin::symmetric(16, 14))
        .outer_margin(egui::Margin::ZERO)
        .shadow(egui::Shadow {
            offset: [0, 2],
            blur: 8,
            spread: 0,
            color: Color32::from_black_alpha(70),
        })
}

// ─── Welcome ─────────────────────────────────────────────────────────────────

fn welcome(
    ctx: &Context,
    st: &WelcomeState,
    pal: &GuiPalette,
    frame: egui::Frame,
    actions: &mut Vec<Action>,
) {
    modal_window("Welcome to honya 本屋")
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_min_width(440.0);
            ui.label(
                RichText::new("AI-assisted Japanese → Thai / English light-novel translation")
                    .color(pal.ink_soft),
            );
            ui.add_space(12.0);

            let key_msg = if st.api_key_present {
                "API key configured"
            } else {
                "No API key yet — set one in Settings, or explore offline"
            };
            ui.label(RichText::new(key_msg).color(if st.api_key_present {
                pal.status_done
            } else {
                pal.status_warn
            }));
            ui.add_space(12.0);

            let sample_label = if st.sample_exists {
                "Open sample project"
            } else {
                "Create sample project"
            };
            if ui
                .add_sized([ui.available_width(), 36.0], egui::Button::new(sample_label))
                .clicked()
            {
                actions.push(Action::CreateSample);
            }
            if ui
                .add_sized(
                    [ui.available_width(), 36.0],
                    egui::Button::new("Import a source…"),
                )
                .clicked()
            {
                actions.push(Action::OpenImport);
            }
            if ui
                .add_sized([ui.available_width(), 36.0], egui::Button::new("Settings…"))
                .clicked()
            {
                actions.push(Action::show_overlay(Overlay::settings_at(
                    SETTINGS_KEY_FIELD,
                )));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Continue").clicked() {
                    actions.push(Action::DismissWelcome);
                }
            });
        });
}

// ─── Theme picker ────────────────────────────────────────────────────────────

fn theme_picker(
    ctx: &Context,
    st: &mut crate::app::overlay::ThemePickerState,
    saved: crate::model::ThemeId,
    pal: &GuiPalette,
    frame: egui::Frame,
    actions: &mut Vec<Action>,
) {
    modal_window("Theme").frame(frame).show(ctx, |ui| {
        ui.set_min_width(380.0);
        ui.label(
            RichText::new("Pick a palette — previewed live, saved on confirm.")
                .color(pal.ink_soft)
                .small(),
        );
        ui.add_space(8.0);
        ScrollArea::vertical()
            .id_salt("theme_list")
            .max_height(360.0)
            .show(ui, |ui| {
                for (i, &id) in ALL_THEMES.iter().enumerate() {
                    ui.horizontal(|ui| {
                        theme_swatch(ui, id);
                        let mark = if saved == id { "  ·  saved" } else { "" };
                        let label = format!("{}  ·  {}{mark}", id.label(), id.tone());
                        if ui.selectable_label(st.sel == i, label).clicked() {
                            st.sel = i;
                            actions.push(Action::PreviewTheme(id));
                        }
                    });
                }
            });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                actions.push(Action::CancelTheme);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if primary_button(ui, pal, "Save theme").clicked() {
                    let id = ALL_THEMES.get(st.sel).copied().unwrap_or_default();
                    actions.push(Action::SaveTheme(id));
                }
            });
        });
    });
}

// ─── Confirm dialog ──────────────────────────────────────────────────────────

fn confirm(
    ctx: &Context,
    dlg: &Dialog,
    pal: &GuiPalette,
    frame: egui::Frame,
    actions: &mut Vec<Action>,
) {
    modal_window(dlg.title.clone())
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_min_width(420.0);
            ui.label(RichText::new(&dlg.body).color(pal.ink));
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    actions.push(Action::CloseOverlay);
                }
                if let Some(alt) = &dlg.alternate
                    && ui.button(&alt.label).clicked()
                {
                    actions.push(Action::CloseOverlay);
                    actions.push(alt.action.clone());
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if primary_button(ui, pal, &dlg.confirm_label).clicked() {
                        actions.push(Action::CloseOverlay);
                        actions.push(dlg.confirm.clone());
                    }
                });
            });
        });
}

// ─── Log / Help / About ──────────────────────────────────────────────────────

fn log_panel(
    ctx: &Context,
    log: &[(LogLevel, String)],
    pal: &GuiPalette,
    frame: egui::Frame,
    actions: &mut Vec<Action>,
) {
    modal_window("Activity log")
        .resizable(true)
        .default_size([620.0, 420.0])
        .frame(frame)
        .show(ctx, |ui| {
            if ui.button("Close").clicked() {
                actions.push(Action::CloseOverlay);
            }
            ui.separator();
            ScrollArea::vertical()
                .id_salt("log_scroll")
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    if log.is_empty() {
                        ui.label(RichText::new("Log is empty.").color(pal.ink_faint).italics());
                    }
                    let tail = log.len().saturating_sub(400);
                    for (level, msg) in &log[tail..] {
                        let color = match level {
                            LogLevel::Error => pal.status_failed,
                            LogLevel::Warn => pal.status_warn,
                            LogLevel::Info => pal.ink_soft,
                            LogLevel::Trace => pal.ink_faint,
                        };
                        let tag = match level {
                            LogLevel::Error => "ERR",
                            LogLevel::Warn => "WRN",
                            LogLevel::Info => "INF",
                            LogLevel::Trace => "TRC",
                        };
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(tag).color(color).monospace().small());
                            ui.label(RichText::new(msg).color(pal.ink).small());
                        });
                    }
                });
        });
}

fn help_panel(ctx: &Context, pal: &GuiPalette, frame: egui::Frame, actions: &mut Vec<Action>) {
    modal_window("Help")
        .resizable(true)
        .default_size([520.0, 400.0])
        .frame(frame)
        .show(ctx, |ui| {
            if ui.button("Close").clicked() {
                actions.push(Action::CloseOverlay);
            }
            ui.separator();
            ScrollArea::vertical().id_salt("help_scroll").show(ui, |ui| {
                for (title, body) in [
                    (
                        "Navigation",
                        "Sidebar or keys 1–6 switch screens. Ctrl+P opens the command palette, Ctrl+, opens Settings, Ctrl+Q quits.",
                    ),
                    (
                        "Shelf",
                        "Open a project, import an EPUB / PDF / HTML / Markdown source, or create the sample project.",
                    ),
                    (
                        "Project",
                        "Browse volumes and chapters, start translations, edit the synopsis and title, run QA, export deliverables.",
                    ),
                    (
                        "Translate",
                        "Watch the live pipeline, reorder the chapter queue, pause or stop a run.",
                    ),
                    (
                        "Reader",
                        "Side-by-side source and translation with search and jump-to-chapter.",
                    ),
                    (
                        "Lexicon",
                        "Glossary terms, character roster, and the style guide the agents share.",
                    ),
                    (
                        "Refine",
                        "Chat with the Refine agent to polish existing translations with steering prompts.",
                    ),
                ] {
                    ui.label(RichText::new(title).color(pal.accent).strong());
                    ui.label(RichText::new(body).color(pal.ink));
                    ui.add_space(8.0);
                }
            });
        });
}

fn about_panel(ctx: &Context, pal: &GuiPalette, frame: egui::Frame, actions: &mut Vec<Action>) {
    modal_window("About honya")
        .frame(frame)
        .show(ctx, |ui| {
            ui.label(
                RichText::new(format!("honya 本屋  {}", crate::update::version_string()))
                    .color(pal.ink)
                    .strong()
                    .size(18.0),
            );
            ui.label(
                RichText::new("Japanese → Thai / English light-novel translation")
                    .color(pal.ink_soft),
            );
            ui.add_space(8.0);
            ui.hyperlink_to("honya.altqx.com", "https://honya.altqx.com");
            ui.add_space(12.0);
            if ui.button("Close").clicked() {
                actions.push(Action::CloseOverlay);
            }
        });
}

// ─── Export ──────────────────────────────────────────────────────────────────

fn export_panel(
    ctx: &Context,
    st: &mut ExportState,
    pal: &GuiPalette,
    frame: egui::Frame,
    actions: &mut Vec<Action>,
) {
    modal_window(format!("Export Vol.{}", st.vol))
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_min_width(380.0);
            if let Some((files, warnings)) = &st.done {
                ui.label(RichText::new("Export finished").color(pal.status_done).strong());
                ui.add_space(6.0);
                for f in files {
                    ui.label(
                        RichText::new(f.display().to_string())
                            .color(pal.ink)
                            .monospace()
                            .small(),
                    );
                }
                for w in warnings {
                    ui.label(RichText::new(w).color(pal.status_warn).small());
                }
                ui.add_space(10.0);
                if ui.button("Close").clicked() {
                    actions.push(Action::CloseOverlay);
                }
                return;
            }
            if let Some((done, total, label)) = &st.progress {
                let frac = if *total > 0 {
                    *done as f32 / *total as f32
                } else {
                    0.0
                };
                ui.label(RichText::new(format!("Writing {label}…")).color(pal.ink));
                ui.add(egui::ProgressBar::new(frac).show_percentage());
                return;
            }
            ui.label(
                RichText::new("Choose deliverable formats written under exports/.")
                    .color(pal.ink_soft)
                    .small(),
            );
            ui.add_space(4.0);
            ui.checkbox(&mut st.formats[0], "Markdown (merged volume)");
            ui.checkbox(&mut st.formats[1], "EPUB");
            ui.checkbox(&mut st.formats[2], "DOCX");
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    actions.push(Action::CloseOverlay);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if primary_button(ui, pal, "Export").clicked() {
                        use crate::export::ExportFormat;
                        let formats: Vec<ExportFormat> = ExportFormat::ALL
                            .iter()
                            .zip(st.formats)
                            .filter_map(|(f, on)| on.then_some(*f))
                            .collect();
                        if !formats.is_empty() {
                            actions.push(Action::ExportVolume {
                                vol: st.vol,
                                formats,
                            });
                        }
                    }
                });
            });
        });
}

// ─── Import wizard ───────────────────────────────────────────────────────────

fn fmt_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.0} KB", (bytes as f64 / 1024.0).max(1.0))
    }
}

fn import_action(st: &mut ImportState, with_synopsis: bool) -> Action {
    let source = st.selected_file().cloned().unwrap_or_default();
    let title = st.name.trim().to_string();
    st.step = 5;
    st.progress = Some((0, 0, "starting".to_string()));
    Action::ImportFile {
        source,
        title,
        translated_title: st.title_syn.translated_text.trim().to_string(),
        vol: st.vol.max(1),
        synopsis_raw: if with_synopsis {
            st.syn.raw.trim().to_string()
        } else {
            String::new()
        },
        translated_synopsis: if with_synopsis {
            st.syn.translated_text.trim().to_string()
        } else {
            String::new()
        },
        target_language: st.effective_target_language(),
        append: st.append_to.is_some(),
    }
}

fn import_wizard(
    ctx: &Context,
    st: &mut ImportState,
    pal: &GuiPalette,
    frame: egui::Frame,
    actions: &mut Vec<Action>,
) {
    let title = if st.append_to.is_some() {
        "Add chapters"
    } else if st.lock_name {
        "Add volume"
    } else {
        "Import a source"
    };
    modal_window(title).frame(frame).show(ctx, |ui| {
        ui.set_width(560.0);

        // Step breadcrumb (append mode has only pick + import).
        let steps: &[&str] = if st.append_to.is_some() {
            &["Source", "Import"]
        } else if st.lock_name {
            &["Source", "Volume", "Synopsis", "Import"]
        } else {
            &["Source", "Name", "Title", "Volume", "Synopsis", "Import"]
        };
        let current = match (st.step, st.lock_name, st.append_to.is_some()) {
            (0, _, _) => 0,
            (_, _, true) => 1,
            (3, true, _) => 1,
            (4, true, _) => 2,
            (_, true, _) => 3,
            (s, false, _) => (s as usize).min(steps.len() - 1),
        };
        ui.horizontal(|ui| {
            for (i, s) in steps.iter().enumerate() {
                let color = if i == current { pal.accent } else { pal.ink_faint };
                ui.label(RichText::new(*s).color(color).small());
                if i + 1 < steps.len() {
                    ui.label(RichText::new("→").color(pal.ink_faint).small());
                }
            }
        });
        ui.separator();

        match st.step {
            0 => import_step_pick(ui, st, pal, actions),
            1 => import_step_name(ui, st, pal),
            2 => import_step_title(ui, st, pal, actions),
            3 => import_step_volume(ui, st, pal),
            4 => import_step_synopsis(ui, st, pal, actions),
            _ => {
                let (done, total, label) = st.progress.clone().unwrap_or((0, 0, "…".into()));
                ui.label(RichText::new("Importing — chapters land as they're cleansed.").color(pal.ink));
                let frac = if total > 0 { done as f32 / total as f32 } else { 0.0 };
                ui.add(egui::ProgressBar::new(frac).text(label.to_string()));
                ui.add_space(8.0);
                if ui.button("Hide").clicked() {
                    actions.push(Action::CloseOverlay);
                }
            }
        }
    });
}

fn import_step_pick(ui: &mut Ui, st: &mut ImportState, pal: &GuiPalette, actions: &mut Vec<Action>) {
    if !st.lock_name {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Target language").color(pal.ink_soft));
            egui::ComboBox::from_id_salt("import_lang")
                .selected_text(st.target_language.label())
                .width(120.0)
                .show_ui(ui, |ui| {
                    for l in [TargetLanguage::Thai, TargetLanguage::English] {
                        ui.selectable_value(&mut st.target_language, l, l.label());
                    }
                });
        });
        ui.add_space(4.0);
    }
    if st.files.is_empty() {
        ui.label(
            RichText::new("No importable sources found.")
                .color(pal.ink_soft)
                .strong(),
        );
        hint(
            ui,
            pal,
            "Drop an EPUB / PDF / HTML / Markdown file into the shelf folder, then rescan.",
        );
    } else {
        ScrollArea::vertical()
            .id_salt("import_files")
            .max_height(240.0)
            .show(ui, |ui| {
                for i in 0..st.files.len() {
                    let (path, size) = st.files[i].clone();
                    let name = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("?")
                        .to_string();
                    let label = format!("{name}   ·   {}", fmt_size(size));
                    if ui.selectable_label(st.sel == i, label).clicked() {
                        st.sel = i;
                    }
                }
            });
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button("Cancel").clicked() {
            actions.push(Action::CloseOverlay);
        }
        if ui.button("Rescan").clicked() {
            actions.push(Action::RescanImports);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let label = if st.append_to.is_some() { "Import" } else { "Continue" };
            if !st.files.is_empty() && primary_button(ui, pal, label).clicked() {
                if st.append_to.is_some() {
                    let a = import_action(st, false);
                    actions.push(a);
                } else {
                    if !st.lock_name && !st.name_touched {
                        // Follow the picked file's stem until the user edits the name.
                        if let Some(stem) = st
                            .selected_file()
                            .and_then(|p| p.file_stem())
                            .and_then(|s| s.to_str())
                        {
                            st.name = prettify_stem(stem);
                        }
                    }
                    st.name_cursor = st.name.len();
                    st.step = if st.lock_name { 3 } else { 1 };
                }
            }
        });
    });
}

fn import_step_name(ui: &mut Ui, st: &mut ImportState, pal: &GuiPalette) {
    ui.label(RichText::new("Project name").color(pal.ink).strong());
    let resp = ui.add(TextEdit::singleline(&mut st.name).desired_width(f32::INFINITY));
    if resp.changed() {
        st.name_touched = true;
        st.note = None;
    }
    if let Some(note) = st.note {
        ui.label(RichText::new(note).color(pal.status_warn).small());
    }
    if let Some(target) = st.target_project() {
        ui.label(
            RichText::new(format!(
                "Adds into existing project “{}” ({} volumes)",
                target.title,
                target.volumes.len()
            ))
            .color(pal.status_warn)
            .small(),
        );
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button("Back").clicked() {
            st.step = 0;
            st.note = None;
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if primary_button(ui, pal, "Continue").clicked() {
                if st.name.trim().is_empty() {
                    st.note = Some("a project name is required");
                } else {
                    st.note = None;
                    let raw = st.name.trim().to_string();
                    // A changed name invalidates any earlier translation.
                    if st.title_syn.raw != raw {
                        st.title_syn = SynopsisState::new_title(raw, String::new());
                    }
                    st.step = 2;
                }
            }
        });
    });
}

fn import_step_title(ui: &mut Ui, st: &mut ImportState, pal: &GuiPalette, actions: &mut Vec<Action>) {
    ui.label(RichText::new("Translated title").color(pal.ink).strong());
    hint(ui, pal, "Optional — shown next to the source title on the shelf.");
    ui.label(RichText::new(&st.title_syn.raw).color(pal.ja_text));
    let lang = st.effective_target_language();
    if let Some(a) = synopsis_fields(ui, &mut st.title_syn, pal, false) {
        actions.push(match a {
            SynRequest::Title { raw, attempt } => Action::TranslateProjectTitle {
                raw,
                attempt,
                target_language: lang,
            },
        });
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button("Back").clicked() {
            st.step = 1;
            st.name_cursor = st.name.len();
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if primary_button(ui, pal, "Continue").clicked() {
                st.step = 3;
                st.suggest_volume();
            }
        });
    });
}

fn import_step_volume(ui: &mut Ui, st: &mut ImportState, pal: &GuiPalette) {
    ui.label(RichText::new("Volume number").color(pal.ink).strong());
    let mut vol = st.vol.max(1);
    let resp = ui.add(egui::DragValue::new(&mut vol).range(1..=999).speed(0.1));
    if resp.changed() {
        st.vol = vol;
        st.vol_touched = true;
    }
    if let Some(target) = st.target_project()
        && let Some((_, chapters)) = target.volumes.iter().find(|(n, _)| *n == st.vol)
    {
        ui.label(
            RichText::new(format!(
                "Vol.{} already exists with {chapters} chapters — the import merges into it.",
                st.vol
            ))
            .color(pal.status_warn)
            .small(),
        );
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button("Back").clicked() {
            st.step = if st.lock_name { 0 } else { 2 };
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if primary_button(ui, pal, "Continue").clicked() {
                st.step = 4;
            }
        });
    });
}

fn import_step_synopsis(
    ui: &mut Ui,
    st: &mut ImportState,
    pal: &GuiPalette,
    actions: &mut Vec<Action>,
) {
    ui.label(RichText::new("Volume synopsis").color(pal.ink).strong());
    hint(
        ui,
        pal,
        "Optional — injected into every chunk's reference context so the agents share the arc.",
    );
    let lang = st.effective_target_language();
    if let Some(a) = synopsis_fields(ui, &mut st.syn, pal, true) {
        actions.push(match a {
            SynRequest::Title { raw, attempt } => Action::TranslateSynopsis {
                raw,
                attempt,
                target_language: lang,
            },
        });
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button("Back").clicked() {
            st.step = 3;
        }
        if ui.button("Skip synopsis").clicked() {
            let a = import_action(st, false);
            actions.push(a);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if primary_button(ui, pal, "Import").clicked() {
                let a = import_action(st, true);
                actions.push(a);
            }
        });
    });
}

// ─── Shared synopsis / title editor fields ───────────────────────────────────

enum SynRequest {
    Title { raw: String, attempt: u32 },
}

/// Source + translated fields with a translate/reroll round-trip. Returns a
/// request when the user asked for a translation (caller picks the action).
fn synopsis_fields(
    ui: &mut Ui,
    syn: &mut SynopsisState,
    pal: &GuiPalette,
    show_source: bool,
) -> Option<SynRequest> {
    let mut request = None;
    if show_source {
        section(ui, pal, "Source (Japanese)");
        if syn.multiline {
            ui.add(
                TextEdit::multiline(&mut syn.raw)
                    .desired_rows(4)
                    .desired_width(f32::INFINITY),
            );
        } else {
            ui.add(TextEdit::singleline(&mut syn.raw).desired_width(f32::INFINITY));
        }
    }

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if syn.phase == SynPhase::Translating {
            ui.spinner();
            ui.label(RichText::new("translating…").color(pal.status_working));
            if ui.button("Cancel").clicked() {
                syn.phase = SynPhase::Editing;
            }
        } else {
            let label = if syn.translated_text.trim().is_empty() {
                "Translate"
            } else {
                "Reroll ↻"
            };
            if ui.button(label).clicked() && !syn.raw.trim().is_empty() {
                if matches!(syn.phase, SynPhase::Done | SynPhase::Failed) {
                    syn.attempt += 1;
                }
                syn.phase = SynPhase::Translating;
                request = Some(SynRequest::Title {
                    raw: syn.raw.clone(),
                    attempt: syn.attempt,
                });
            }
        }
    });
    if syn.phase == SynPhase::Failed && !syn.error.is_empty() {
        ui.label(RichText::new(&syn.error).color(pal.status_failed).small());
    }

    ui.add_space(4.0);
    section(ui, pal, "Translation (hand-editable)");
    if syn.multiline {
        ui.add(
            TextEdit::multiline(&mut syn.translated_text)
                .desired_rows(4)
                .desired_width(f32::INFINITY),
        );
    } else {
        ui.add(TextEdit::singleline(&mut syn.translated_text).desired_width(f32::INFINITY));
    }
    request
}

// ─── Standalone synopsis / title editors ─────────────────────────────────────

fn synopsis_editor(
    ctx: &Context,
    st: &mut SynopsisEditState,
    pal: &GuiPalette,
    frame: egui::Frame,
    actions: &mut Vec<Action>,
) {
    modal_window(format!("Synopsis — {} · Vol.{}", st.title, st.vol))
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_width(560.0);
            let lang = st.target_language;
            if let Some(SynRequest::Title { raw, attempt }) =
                synopsis_fields(ui, &mut st.syn, pal, true)
            {
                actions.push(Action::TranslateSynopsis {
                    raw,
                    attempt,
                    target_language: lang,
                });
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    actions.push(Action::CloseOverlay);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if primary_button(ui, pal, "Save synopsis").clicked() {
                        actions.push(Action::SaveSynopsis {
                            raw: st.syn.raw.clone(),
                            translated_synopsis: st.syn.translated_text.clone(),
                        });
                    }
                });
            });
        });
}

fn title_editor(
    ctx: &Context,
    st: &mut TitleEditState,
    pal: &GuiPalette,
    frame: egui::Frame,
    actions: &mut Vec<Action>,
) {
    modal_window("Project title").frame(frame).show(ctx, |ui| {
        ui.set_width(520.0);
        section(ui, pal, "Source title");
        ui.add(TextEdit::singleline(&mut st.syn.raw).desired_width(f32::INFINITY));
        let lang = st.target_language;
        if let Some(SynRequest::Title { raw, attempt }) =
            synopsis_fields(ui, &mut st.syn, pal, false)
        {
            actions.push(Action::TranslateProjectTitle {
                raw,
                attempt,
                target_language: lang,
            });
        }
        hint(ui, pal, "Renaming keeps the project folder (slug) unchanged.");
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                actions.push(Action::CloseOverlay);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if primary_button(ui, pal, "Save title").clicked() {
                    actions.push(Action::SaveProjectTitle {
                        id: st.id.clone(),
                        raw: st.syn.raw.clone(),
                        translated_title: st.syn.translated_text.clone(),
                    });
                }
            });
        });
    });
}

// ─── Image source ────────────────────────────────────────────────────────────

fn image_source(
    ctx: &Context,
    st: &mut ImageSourceState,
    pal: &GuiPalette,
    frame: egui::Frame,
    actions: &mut Vec<Action>,
) {
    modal_window(format!("Update Vol.{:02} images", st.vol))
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_width(520.0);
            hint(
                ui,
                pal,
                "Pick the source file to re-import illustrations from. Translation prose stays unchanged.",
            );
            ui.add_space(4.0);
            if st.files.is_empty() {
                ui.label(RichText::new("No source files found.").color(pal.ink_soft));
            } else {
                ScrollArea::vertical()
                    .id_salt("imgsrc_files")
                    .max_height(220.0)
                    .show(ui, |ui| {
                        for i in 0..st.files.len() {
                            let (path, size) = st.files[i].clone();
                            let name = path
                                .file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or("?")
                                .to_string();
                            if ui
                                .selectable_label(
                                    st.sel == i,
                                    format!("{name}   ·   {}", fmt_size(size)),
                                )
                                .clicked()
                            {
                                st.sel = i;
                            }
                        }
                    });
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    actions.push(Action::CloseOverlay);
                }
                if ui.button("Rescan").clicked() {
                    actions.push(Action::RescanImageSources { vol: st.vol });
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if !st.files.is_empty() && primary_button(ui, pal, "Use this file").clicked() {
                        actions.push(Action::RefreshVolumeImagesFromFile {
                            vol: st.vol,
                            source: st.selected_file().cloned().unwrap_or_default(),
                        });
                    }
                });
            });
        });
}

// ─── Command palette ─────────────────────────────────────────────────────────

fn palette(
    ctx: &Context,
    st: &mut PaletteState,
    pal: &GuiPalette,
    frame: egui::Frame,
    actions: &mut Vec<Action>,
) {
    modal_window("Command palette")
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_width(420.0);
            let resp = ui.add(
                TextEdit::singleline(&mut st.query)
                    .hint_text("Type a command…")
                    .desired_width(f32::INFINITY),
            );
            resp.request_focus();
            if resp.changed() {
                st.sel = 0;
            }
            let matches = st.matches();
            // The palette is modal, so a bare Enter can only mean "run the top match".
            let submit = ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.add_space(4.0);
            ScrollArea::vertical()
                .id_salt("palette_list")
                .max_height(280.0)
                .show(ui, |ui| {
                    for (row, &i) in matches.iter().enumerate() {
                        let item = &st.items[i];
                        if ui.selectable_label(row == st.sel, item.label).clicked() {
                            actions.push(Action::CloseOverlay);
                            actions.push(item.action.clone());
                        }
                    }
                    if matches.is_empty() {
                        ui.label(RichText::new("no matches").color(pal.ink_faint).italics());
                    }
                });
            if submit && let Some(&i) = matches.first() {
                actions.push(Action::CloseOverlay);
                actions.push(st.items[i].action.clone());
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("Close").clicked() {
                    actions.push(Action::CloseOverlay);
                }
            });
        });
}

// ─── QA review ───────────────────────────────────────────────────────────────

fn qa_panel(
    ctx: &Context,
    st: &mut QaState,
    pal: &GuiPalette,
    frame: egui::Frame,
    actions: &mut Vec<Action>,
) {
    modal_window(format!("QA review — {}", st.title))
        .resizable(true)
        .default_size([640.0, 440.0])
        .frame(frame)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("● {} clean", st.report.done)).color(pal.status_done),
                );
                ui.label(
                    RichText::new(format!("⚑ {} need review", st.report.review))
                        .color(pal.status_warn),
                );
                ui.label(
                    RichText::new(format!("✗ {} failed", st.report.failed))
                        .color(pal.status_failed),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        actions.push(Action::CloseOverlay);
                    }
                });
            });
            ui.separator();
            if st.report.issues.is_empty() {
                ui.label(
                    RichText::new("No findings — the volume is clean.")
                        .color(pal.status_done)
                        .italics(),
                );
                return;
            }
            ScrollArea::vertical().id_salt("qa_list").show(ui, |ui| {
                for (i, issue) in st.report.issues.iter().enumerate() {
                    let (glyph, color, tag) = match &issue.kind {
                        QaKind::ReviewChunk { .. } => ("⚑", pal.status_warn, "review"),
                        QaKind::ChapterFailed => ("✗", pal.status_failed, "failed"),
                        QaKind::Continuity { .. } => ("↝", pal.status_warn, "continuity"),
                        QaKind::Consistency => ("≠", pal.status_image, "consistency"),
                    };
                    let selected = st.sel == i;
                    let head = match issue.chapter {
                        Some(ch) => format!("{glyph} ch {ch:03}  {}  [{tag}]", issue.title),
                        None => format!("{glyph} {}  [{tag}]", issue.title),
                    };
                    let resp = ui.selectable_label(selected, RichText::new(head).color(color));
                    if !issue.detail.is_empty() {
                        ui.indent(format!("qa_{i}"), |ui| {
                            ui.label(RichText::new(&issue.detail).color(pal.ink_soft).small());
                        });
                    }
                    if resp.clicked() {
                        st.sel = i;
                    }
                    if resp.double_clicked()
                        && let Some(ch) = issue.chapter
                    {
                        actions.push(Action::CloseOverlay);
                        actions.push(match &issue.kind {
                            QaKind::ReviewChunk { chunk } => Action::OpenChapterAtChunk {
                                chapter: ch,
                                chunk: *chunk,
                            },
                            _ => Action::OpenChapter { chapter: ch },
                        });
                    }
                    ui.add_space(2.0);
                }
            });
            ui.add_space(4.0);
            hint(ui, pal, "Double-click a finding to open it in the Reader.");
        });
}

// ─── Reader overlays ─────────────────────────────────────────────────────────

fn reader_note(
    ctx: &Context,
    st: &mut ReaderNoteState,
    pal: &GuiPalette,
    frame: egui::Frame,
    actions: &mut Vec<Action>,
) {
    modal_window(format!("Note — ch {:03} · line {}", st.chapter, st.line))
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_width(460.0);
            hint(ui, pal, "A proofreading note anchored to this translated line.");
            ui.add(TextEdit::singleline(&mut st.text).desired_width(f32::INFINITY));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    actions.push(Action::CloseOverlay);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if primary_button(ui, pal, "Save note").clicked() {
                        actions.push(Action::SaveReaderNote {
                            chapter: st.chapter,
                            line: st.line,
                            note: st.text.clone(),
                        });
                    }
                });
            });
        });
}

fn reader_inspect(
    ctx: &Context,
    st: &mut ReaderInspectState,
    pal: &GuiPalette,
    frame: egui::Frame,
    actions: &mut Vec<Action>,
) {
    modal_window(format!("Chunk {} — ch {:03}", st.chunk, st.chapter))
        .resizable(true)
        .default_size([620.0, 460.0])
        .frame(frame)
        .show(ctx, |ui| {
            ScrollArea::vertical()
                .id_salt("inspect_scroll")
                .max_height(380.0)
                .show(ui, |ui| {
                    section(ui, pal, "原文  Source");
                    ui.label(RichText::new(&st.source_jp).color(pal.ja_text));
                    ui.add_space(8.0);
                    section(ui, pal, "翻訳  Translation");
                    ui.label(RichText::new(&st.translated_text).color(pal.translated_text));
                    if let Some(review) = &st.review {
                        ui.add_space(8.0);
                        section(ui, pal, "Reviewer note");
                        ui.label(RichText::new(review).color(pal.status_warn));
                    }
                });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Close").clicked() {
                    actions.push(Action::CloseOverlay);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if primary_button(ui, pal, "Edit this chunk").clicked() {
                        actions.push(Action::CloseOverlay);
                        actions.push(Action::OpenReaderEdit {
                            chapter: st.chapter,
                            chunk: st.chunk,
                        });
                    }
                });
            });
        });
}

fn reader_edit(
    ctx: &Context,
    st: &mut ReaderEditState,
    pal: &GuiPalette,
    frame: egui::Frame,
    actions: &mut Vec<Action>,
) {
    modal_window(format!("Edit chunk {} — ch {:03}", st.chunk, st.chapter))
        .resizable(true)
        .default_size([620.0, 460.0])
        .frame(frame)
        .show(ctx, |ui| {
            hint(ui, pal, "Saving clears the chunk's review-needed flag.");
            ScrollArea::vertical()
                .id_salt("chunk_edit")
                .max_height(360.0)
                .show(ui, |ui| {
                    ui.add(
                        TextEdit::multiline(&mut st.text)
                            .desired_rows(14)
                            .desired_width(f32::INFINITY),
                    );
                });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    actions.push(Action::CloseOverlay);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if primary_button(ui, pal, "Save chunk").clicked() {
                        actions.push(Action::SaveReaderEdit {
                            chapter: st.chapter,
                            chunk: st.chunk,
                            text: st.text.clone(),
                        });
                    }
                });
            });
        });
}

fn reader_search(
    ctx: &Context,
    st: &mut ReaderSearchState,
    pal: &GuiPalette,
    frame: egui::Frame,
    actions: &mut Vec<Action>,
) {
    modal_window("Search").frame(frame).show(ctx, |ui| {
        ui.set_width(420.0);
        hint(ui, pal, "Searches both the source and translation panes.");
        let resp = ui.add(
            TextEdit::singleline(&mut st.query)
                .hint_text("Query…")
                .desired_width(f32::INFINITY),
        );
        resp.request_focus();
        let submit = ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.add_space(8.0);
        let mut go = submit;
        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                actions.push(Action::CloseOverlay);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if primary_button(ui, pal, "Search").clicked() {
                    go = true;
                }
            });
        });
        if go {
            if st.query.trim().is_empty() {
                actions.push(Action::CloseOverlay);
            } else {
                actions.push(Action::CloseOverlay);
                actions.push(Action::ReaderSearch {
                    query: st.query.clone(),
                });
            }
        }
    });
}

fn reader_jump(
    ctx: &Context,
    st: &mut ReaderJumpState,
    pal: &GuiPalette,
    frame: egui::Frame,
    actions: &mut Vec<Action>,
) {
    modal_window(format!("Jump — {}", st.title))
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_width(460.0);
            let resp = ui.add(
                TextEdit::singleline(&mut st.query)
                    .hint_text("Filter chapters · sections · bookmarks…")
                    .desired_width(f32::INFINITY),
            );
            resp.request_focus();
            if resp.changed() {
                st.sel = 0;
            }
            ui.add_space(4.0);
            let matches = st.matches();
            if ui.input(|i| i.key_pressed(egui::Key::Enter))
                && let Some(item) = matches.first().and_then(|&i| st.items.get(i))
            {
                actions.push(Action::CloseOverlay);
                actions.push(Action::OpenChapterAt {
                    chapter: item.chapter,
                    line: item.line,
                });
            }
            ScrollArea::vertical()
                .id_salt("jump_list")
                .max_height(300.0)
                .show(ui, |ui| {
                    for (row, &i) in matches.iter().enumerate() {
                        let item = &st.items[i];
                        let glyph = match item.kind {
                            JumpKind::Chapter => "▤",
                            JumpKind::Section => "§",
                            JumpKind::Bookmark => "◈",
                        };
                        if ui
                            .selectable_label(row == st.sel, format!("{glyph}  {}", item.label))
                            .clicked()
                        {
                            actions.push(Action::CloseOverlay);
                            actions.push(Action::OpenChapterAt {
                                chapter: item.chapter,
                                line: item.line,
                            });
                        }
                    }
                    if matches.is_empty() {
                        ui.label(RichText::new("no matches").color(pal.ink_faint).italics());
                    }
                });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("Close").clicked() {
                    actions.push(Action::CloseOverlay);
                }
            });
        });
}
