//! Native modal dialogs for Welcome, Theme, Confirm, Log, and simple overlays.

use egui::{Align, Color32, Layout, RichText, ScrollArea, Ui, Window};

use crate::app::overlay::{Dialog, Overlay, WelcomeState};
use crate::app::{Action, App};
use crate::model::{LogLevel, ThemeId};
use crate::theme::ALL_THEMES;

use super::theme_map::GuiPalette;

/// Draw any open overlay as a centered native window / modal.
/// Returns true if an overlay consumed the frame (dim the background).
pub fn render(ui: &mut Ui, app: &mut App, pal: &GuiPalette) -> bool {
    if matches!(app.overlay, Overlay::None) {
        return false;
    }

    // Dim the *full viewport* (not just the central panel). Windows are
    // centered on the ctx screen, so a central-only scrim looked misaligned.
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

    // Clone the variant we need so we can mutate app while reading overlay data.
    match app.overlay.clone() {
        Overlay::Welcome(st) => welcome(ui, app, pal, &st),
        Overlay::Theme(_) => theme_picker(ui, app, pal),
        Overlay::Modal(dlg) => confirm(ui, app, pal, &dlg),
        Overlay::Log(_) => log_panel(ui, app, pal),
        Overlay::Help(_) => help_panel(ui, app, pal),
        Overlay::About => about_panel(ui, app, pal),
        Overlay::Export(st) => export_panel(ui, app, pal, st.vol, st.formats),
        other => generic_overlay(ui, app, pal, &other),
    }
    true
}

fn welcome(ui: &mut Ui, app: &mut App, pal: &GuiPalette, st: &WelcomeState) {
    Window::new("Welcome to honya 本屋")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .frame(modal_frame(pal))
        .show(ui.ctx(), |ui| {
            ui.set_min_width(420.0);
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
            ui.label(
                RichText::new(key_msg).color(if st.api_key_present {
                    pal.status_done
                } else {
                    pal.status_warn
                }),
            );
            ui.add_space(12.0);

            if ui
                .add_sized([ui.available_width(), 36.0], egui::Button::new("Open sample project"))
                .clicked()
            {
                app.apply(Action::CreateSample);
            }
            if ui
                .add_sized([ui.available_width(), 36.0], egui::Button::new("Import a source…"))
                .clicked()
            {
                app.apply(Action::OpenImport);
            }
            if ui
                .add_sized([ui.available_width(), 36.0], egui::Button::new("Settings…"))
                .clicked()
            {
                app.overlay = Overlay::None;
                app.apply(Action::show_overlay(
                    crate::app::overlay::Overlay::settings_with_field(&app.cfg, 0),
                ));
            }
            ui.add_space(8.0);
            if ui.button("Continue").clicked() {
                app.apply(Action::DismissWelcome);
            }
        });
}

fn theme_picker(ui: &mut Ui, app: &mut App, pal: &GuiPalette) {
    Window::new("Theme")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .frame(modal_frame(pal))
        .show(ui.ctx(), |ui| {
            ui.set_min_width(360.0);
            ui.label(
                RichText::new("Pick a palette — applied live, saved on confirm.")
                    .color(pal.ink_soft)
                    .small(),
            );
            ui.add_space(8.0);
            ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for &id in ALL_THEMES {
                    let selected = app.cfg.theme == id;
                    let swatch = theme_swatch(id);
                    ui.horizontal(|ui| {
                        for c in swatch {
                            let (rect, _) =
                                ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                            ui.painter().rect_filled(rect, 3.0, c);
                        }
                        let label = format!("{}  ·  {}", id.label(), id.tone());
                        if ui.selectable_label(selected, label).clicked() {
                            app.apply(Action::PreviewTheme(id));
                        }
                    });
                }
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    app.apply(Action::CancelTheme);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .button(RichText::new("Save theme").strong())
                        .clicked()
                    {
                        app.apply(Action::SaveTheme(app.cfg.theme));
                    }
                });
            });
        });
}

fn theme_swatch(id: ThemeId) -> [Color32; 4] {
    let p = super::theme_map::GuiPalette::from_theme_id(id);
    [p.bg, p.bg_panel, p.accent, p.status_working]
}

fn confirm(ui: &mut Ui, app: &mut App, pal: &GuiPalette, dlg: &Dialog) {
    Window::new(&dlg.title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .frame(modal_frame(pal))
        .show(ui.ctx(), |ui| {
            ui.set_min_width(400.0);
            ui.label(RichText::new(&dlg.body).color(pal.ink));
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    app.overlay = Overlay::None;
                }
                if let Some(alt) = &dlg.alternate {
                    if ui.button(&alt.label).clicked() {
                        let a = alt.action.clone();
                        app.overlay = Overlay::None;
                        app.apply(a);
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .button(RichText::new(&dlg.confirm_label).strong())
                        .clicked()
                    {
                        let a = dlg.confirm.clone();
                        app.overlay = Overlay::None;
                        app.apply(a);
                    }
                });
            });
        });
}

fn log_panel(ui: &mut Ui, app: &mut App, pal: &GuiPalette) {
    Window::new("Activity log")
        .collapsible(false)
        .resizable(true)
        .default_size([560.0, 400.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .frame(modal_frame(pal))
        .show(ui.ctx(), |ui| {
            if ui.button("Close").clicked() {
                app.overlay = Overlay::None;
            }
            ui.separator();
            ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                if app.log.is_empty() {
                    ui.label(RichText::new("Log is empty.").color(pal.ink_faint).italics());
                }
                for (level, msg) in app.log.iter().rev().take(400).collect::<Vec<_>>().into_iter().rev()
                {
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

fn help_panel(ui: &mut Ui, app: &mut App, pal: &GuiPalette) {
    Window::new("Help")
        .collapsible(false)
        .resizable(true)
        .default_size([480.0, 360.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .frame(modal_frame(pal))
        .show(ui.ctx(), |ui| {
            if ui.button("Close").clicked() {
                app.overlay = Overlay::None;
            }
            ui.separator();
            ui.label(RichText::new("Navigation").color(pal.accent).strong());
            ui.label("Use the top tabs or keys 1–6 to switch screens.");
            ui.label("Theme, Settings, and Log are available from the toolbar.");
            ui.add_space(8.0);
            ui.label(RichText::new("Shelf").color(pal.accent).strong());
            ui.label("Open a project card, import a source, or create the sample project.");
            ui.add_space(8.0);
            ui.label(RichText::new("Project").color(pal.accent).strong());
            ui.label("Browse volumes/chapters, start translation, export.");
            ui.add_space(8.0);
            ui.label(RichText::new("Translate").color(pal.accent).strong());
            ui.label("Watch the live pipeline, pause/stop a run.");
            ui.add_space(8.0);
            ui.label(RichText::new("Reader").color(pal.accent).strong());
            ui.label("Side-by-side source and translation.");
        });
}

fn about_panel(ui: &mut Ui, app: &mut App, pal: &GuiPalette) {
    Window::new("About honya")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .frame(modal_frame(pal))
        .show(ui.ctx(), |ui| {
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
            ui.label(RichText::new("https://honya.altqx.com").color(pal.accent));
            ui.add_space(12.0);
            if ui.button("Close").clicked() {
                app.overlay = Overlay::None;
            }
        });
}

fn export_panel(ui: &mut Ui, app: &mut App, pal: &GuiPalette, vol: u32, mut formats: [bool; 3]) {
    Window::new(format!("Export Vol.{vol}"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .frame(modal_frame(pal))
        .show(ui.ctx(), |ui| {
            ui.label(
                RichText::new("Choose deliverable formats written under exports/.")
                    .color(pal.ink_soft)
                    .small(),
            );
            ui.checkbox(&mut formats[0], "Markdown");
            ui.checkbox(&mut formats[1], "EPUB");
            ui.checkbox(&mut formats[2], "DOCX");
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    app.overlay = Overlay::None;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button(RichText::new("Export").strong()).clicked() {
                        use crate::export::ExportFormat;
                        let mut out = Vec::new();
                        if formats[0] {
                            out.push(ExportFormat::Markdown);
                        }
                        if formats[1] {
                            out.push(ExportFormat::Epub);
                        }
                        if formats[2] {
                            out.push(ExportFormat::Docx);
                        }
                        app.overlay = Overlay::None;
                        if !out.is_empty() {
                            app.apply(Action::ExportVolume { vol, formats: out });
                        }
                    }
                });
            });
        });
}

fn generic_overlay(ui: &mut Ui, app: &mut App, pal: &GuiPalette, ov: &Overlay) {
    let title = match ov {
        Overlay::Import(_) => "Import",
        Overlay::Settings(_) => "Settings",
        Overlay::Palette(_) => "Command palette",
        Overlay::Synopsis(_) => "Synopsis",
        Overlay::ProjectTitle(_) => "Project title",
        Overlay::Qa(_) => "QA",
        Overlay::ReaderNote(_) => "Reader note",
        Overlay::ReaderInspect(_) => "Inspect chunk",
        Overlay::ReaderEdit(_) => "Edit chunk",
        Overlay::ReaderSearch(_) => "Search",
        Overlay::ReaderJump(_) => "Jump",
        Overlay::ImageSource(_) => "Image source",
        _ => "Dialog",
    };
    Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .frame(modal_frame(pal))
        .show(ui.ctx(), |ui| {
            ui.set_min_width(380.0);
            ui.label(
                RichText::new(
                    "This dialog is fully interactive in the terminal UI.\n\
                     Close here and use  honya  (TUI) for the multi-step wizard,\n\
                     or use the toolbar actions available in this window.",
                )
                .color(pal.ink_soft),
            );
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if matches!(ov, Overlay::Import(_)) {
                    if ui.button("Open import again").clicked() {
                        app.apply(Action::OpenImport);
                    }
                }
                if matches!(ov, Overlay::Settings(_)) {
                    ui.label(
                        RichText::new("Edit ~/.config/honya/config.json or use the TUI Settings.")
                            .color(pal.ink_faint)
                            .small(),
                    );
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        app.overlay = Overlay::None;
                    }
                });
            });
        });
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
