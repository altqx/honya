//! Native window GUI (`honya --gui`).
//!
//! A real desktop layout — menu bar, navigation sidebar, status bar, modal
//! dialogs — over the same `App` state, Action funnel, and theme palettes as
//! the TUI; not a terminal grid in a window.

mod fonts;
mod overlays;
mod screens;
mod settings;
mod theme_map;
mod widgets;

use std::time::{Duration, Instant};

use egui::{Align, Layout, RichText};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app::overlay::Overlay;
use crate::app::{Action, App, Screen};
use crate::model::{AppEvent, ThemeId};
use crate::theme::ALL_THEMES;

use self::screens::GuiNav;
use self::theme_map::GuiPalette;

/// Fixed chrome sizes so the central body never reflows when toast/spinner/tally change.
const MENUBAR_H: f32 = 36.0;
const FOOTER_H: f32 = 30.0;
const SIDEBAR_W: f32 = 172.0;

const NAV: [(Screen, &str, &str); 6] = [
    (Screen::Shelf, "書架", "Shelf"),
    (Screen::Project, "構図", "Project"),
    (Screen::Translate, "訳", "Translate"),
    (Screen::Reader, "読", "Reader"),
    (Screen::Lexicon, "辞", "Lexicon"),
    (Screen::Refine, "磨", "Refine"),
];

/// Block the current thread on the native window. Tokio multi-thread workers keep
/// running background tasks (pipeline, import, remote) while this thread pumps UI.
pub fn run(app: App, rx: UnboundedReceiver<AppEvent>) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1220.0, 780.0])
            .with_min_inner_size([840.0, 540.0])
            .with_title(format!("honya 本屋 {}", crate::update::version_string())),
        ..Default::default()
    };

    let gui = GuiApp {
        app,
        rx,
        last_tick: Instant::now(),
        tick_every: Duration::from_millis(100),
        nav: GuiNav::default(),
        applied_theme: None,
        fonts_ready: false,
    };

    eframe::run_native(
        "honya",
        options,
        Box::new(|cc| {
            fonts::install(&cc.egui_ctx);
            Ok(Box::new(gui))
        }),
    )
    .map_err(|e| anyhow::anyhow!("gui: {e}"))?;
    Ok(())
}

struct GuiApp {
    app: App,
    rx: UnboundedReceiver<AppEvent>,
    last_tick: Instant,
    tick_every: Duration,
    nav: GuiNav,
    applied_theme: Option<ThemeId>,
    fonts_ready: bool,
}

impl GuiApp {
    /// The theme to paint with right now: the picker's live selection while it
    /// is open (preview), else the saved config theme.
    fn effective_theme(&self) -> ThemeId {
        if let Overlay::Theme(st) = &self.app.overlay {
            ALL_THEMES.get(st.sel).copied().unwrap_or(self.app.cfg.theme)
        } else {
            self.app.cfg.theme
        }
    }
}

impl eframe::App for GuiApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.fonts_ready {
            fonts::install(ctx);
            self.fonts_ready = true;
        }

        while let Ok(ev) = self.rx.try_recv() {
            self.app.on_app_event(ev);
        }

        if self.last_tick.elapsed() >= self.tick_every {
            self.app.on_tick();
            self.last_tick = Instant::now();
        }

        // Re-apply egui visuals when the (effective) theme changes.
        let theme_id = self.effective_theme();
        if self.applied_theme != Some(theme_id) {
            GuiPalette::from_theme_id(theme_id).apply(ctx);
            self.applied_theme = Some(theme_id);
        }

        // Global shortcuts that don't fight text fields.
        // Snapshot before the input borrow — digit keys must not steal focus from edits.
        let text_focused = ctx.text_edit_focused();
        ctx.input(|i| {
            for ev in &i.events {
                if let egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } = ev
                {
                    if modifiers.ctrl || modifiers.command {
                        match key {
                            egui::Key::Q => {
                                self.app.apply(Action::Quit);
                            }
                            egui::Key::Comma => {
                                self.app.apply(Action::show_overlay(
                                    Overlay::settings_with_field(&self.app.cfg, 0),
                                ));
                            }
                            egui::Key::P | egui::Key::K => {
                                self.app.apply(Action::show_overlay(Overlay::palette()));
                            }
                            _ => {}
                        }
                        continue;
                    }
                    // Digits switch tabs when no overlay / text field is capturing.
                    if matches!(self.app.overlay, Overlay::None) && !text_focused {
                        let screen = match key {
                            egui::Key::Num1 => Some(Screen::Shelf),
                            egui::Key::Num2 => Some(Screen::Project),
                            egui::Key::Num3 => Some(Screen::Translate),
                            egui::Key::Num4 => Some(Screen::Reader),
                            egui::Key::Num5 => Some(Screen::Lexicon),
                            egui::Key::Num6 => Some(Screen::Refine),
                            _ => None,
                        };
                        if let Some(s) = screen {
                            self.app.apply(Action::Goto(s));
                        }
                    }
                    if *key == egui::Key::Escape {
                        if !matches!(self.app.overlay, Overlay::None) {
                            // Prefer CancelTheme when the theme picker is open.
                            if matches!(self.app.overlay, Overlay::Theme(_)) {
                                self.app.apply(Action::CancelTheme);
                            } else {
                                self.app.apply(Action::CloseOverlay);
                            }
                        } else if self.app.toast.is_some() {
                            self.app.toast = None;
                        }
                    }
                }
            }
        });

        if !self.app.running {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        if self.app.run_active || self.app.toast.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        } else {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let pal = GuiPalette::from_theme_id(self.effective_theme());

        // Full-window background
        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0.0, pal.bg);

        egui::Panel::top("honya_menubar")
            .exact_size(MENUBAR_H)
            .frame(
                egui::Frame::NONE
                    .fill(pal.bg_panel)
                    .stroke(egui::Stroke::new(1.0_f32, pal.rule))
                    .inner_margin(egui::Margin::symmetric(10, 4)),
            )
            .show_inside(ui, |ui| {
                self.menu_bar(ui, &pal);
            });

        egui::Panel::bottom("honya_statusbar")
            .exact_size(FOOTER_H)
            .frame(
                egui::Frame::NONE
                    .fill(pal.bg_panel)
                    .stroke(egui::Stroke::new(1.0_f32, pal.rule))
                    .inner_margin(egui::Margin::symmetric(12, 4)),
            )
            .show_inside(ui, |ui| {
                self.status_bar(ui, &pal);
            });

        egui::Panel::left("honya_sidebar")
            .exact_size(SIDEBAR_W)
            .frame(
                egui::Frame::NONE
                    .fill(pal.bg_panel)
                    .stroke(egui::Stroke::new(1.0_f32, pal.rule))
                    .inner_margin(egui::Margin::symmetric(10, 12)),
            )
            .show_inside(ui, |ui| {
                self.sidebar(ui, &pal);
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(pal.bg)
                    .inner_margin(egui::Margin::symmetric(16, 12)),
            )
            .show_inside(ui, |ui| {
                screens::render_body(ui, &mut self.app, &mut self.nav, &pal);
                overlays::render(ui, &mut self.app, &pal);
            });
    }

    fn on_exit(&mut self) {
        self.app.running = false;
    }
}

impl GuiApp {
    fn menu_bar(&mut self, ui: &mut egui::Ui, pal: &GuiPalette) {
        let mut actions: Vec<Action> = Vec::new();
        let active = self.app.active.is_some();
        let active_vol = self.app.active.as_ref().map(|a| a.vol).unwrap_or(1);
        let running = self.app.run_active;
        let has_recovery = self.app.pending_recovery.is_some();

        egui::MenuBar::new().ui(ui, |ui| {
            ui.label(RichText::new("本屋").color(pal.accent).strong().size(17.0));
            ui.add_space(6.0);

            ui.menu_button("File", |ui| {
                if ui.button("Import source…").clicked() {
                    actions.push(Action::OpenImport);
                }
                if ui.button("Create sample project").clicked() {
                    actions.push(Action::CreateSample);
                }
                if ui.button("Rescan shelf").clicked() {
                    actions.push(Action::Goto(Screen::Shelf));
                    self.nav.rescan_requested = true;
                }
                ui.separator();
                if ui
                    .add_enabled(active, egui::Button::new("Export volume…"))
                    .clicked()
                {
                    actions.push(Action::show_overlay(Overlay::export(active_vol)));
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    actions.push(Action::Quit);
                }
            });

            ui.menu_button("Project", |ui| {
                if !active {
                    ui.label(
                        RichText::new("open a project first")
                            .color(pal.ink_faint)
                            .italics(),
                    );
                    return;
                }
                if ui.button("QA review…").clicked() {
                    actions.push(Action::show_overlay(Overlay::qa_placeholder()));
                }
                if ui.button("Edit volume synopsis…").clicked()
                    && let Some(a) = self.app.active.as_ref()
                {
                    let data = crate::workspace::volume::load(&a.workspace);
                    actions.push(Action::show_overlay(Overlay::synopsis_edit(
                        data.synopsis_raw,
                        data.translated_synopsis,
                        a.vol,
                        a.project.title.clone(),
                        a.project.target_language,
                    )));
                }
                if ui.button("Edit project title…").clicked()
                    && let Some(a) = self.app.active.as_ref()
                {
                    actions.push(Action::show_overlay(Overlay::project_title_edit(
                        a.project.id.clone(),
                        a.project.title.clone(),
                        a.project.translated_title.clone(),
                        a.project.target_language,
                    )));
                }
                ui.separator();
                if ui.button("Add volume…").clicked() {
                    actions.push(Action::AddVolume);
                }
                if ui.button("Add chapters to volume…").clicked() {
                    actions.push(Action::AddChapters { vol: active_vol });
                }
                if ui.button("Update volume images…").clicked() {
                    actions.push(Action::show_overlay(Overlay::confirm(
                        "Update volume images",
                        format!(
                            "Re-import the source for Vol.{active_vol:02} and rewrite image links. Translation prose stays unchanged."
                        ),
                        Action::RefreshVolumeImages { vol: active_vol },
                    )));
                }
            });

            ui.menu_button("Run", |ui| {
                if ui
                    .add_enabled(active && !running, egui::Button::new("Translate volume"))
                    .clicked()
                {
                    actions.push(Action::StartVolumeTranslation { vol: active_vol });
                }
                if ui
                    .add_enabled(active && !running, egui::Button::new("Translate whole project"))
                    .clicked()
                {
                    actions.push(Action::StartProjectTranslation);
                }
                ui.separator();
                if ui
                    .add_enabled(running, egui::Button::new("Pause / resume"))
                    .clicked()
                {
                    actions.push(Action::PauseRun);
                }
                if ui.add_enabled(running, egui::Button::new("Stop run")).clicked() {
                    actions.push(Action::StopRun);
                }
                if has_recovery {
                    ui.separator();
                    if ui.button("Resume interrupted session").clicked() {
                        actions.push(Action::ResumeSession);
                    }
                    if ui.button("Discard interrupted session").clicked() {
                        actions.push(Action::DiscardSession);
                    }
                }
            });

            ui.menu_button("View", |ui| {
                for (screen, glyph, label) in NAV {
                    if ui.button(format!("{glyph}  {label}")).clicked() {
                        actions.push(Action::Goto(screen));
                    }
                }
                ui.separator();
                if ui.button("Command palette…   Ctrl+P").clicked() {
                    actions.push(Action::show_overlay(Overlay::palette()));
                }
                if ui.button("Activity log…").clicked() {
                    actions.push(Action::show_overlay(Overlay::Log(0)));
                }
                if ui.button("Theme…").clicked() {
                    actions.push(Action::show_overlay(Overlay::theme(self.app.cfg.theme)));
                }
                if ui.button("Settings…   Ctrl+,").clicked() {
                    actions.push(Action::show_overlay(Overlay::settings_with_field(
                        &self.app.cfg,
                        0,
                    )));
                }
            });

            ui.menu_button("Help", |ui| {
                if ui.button("Help…").clicked() {
                    actions.push(Action::show_overlay(Overlay::Help(0)));
                }
                if ui.button("About honya…").clicked() {
                    actions.push(Action::show_overlay(Overlay::About));
                }
            });

            // Right side: progress chips + remote badge — fixed widths, no jiggle.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let tally = self.app.tally();
                let total = tally.done + tally.working + tally.pending + tally.failed;
                let pct = if total == 0 {
                    0
                } else {
                    ((tally.done as f64 / total as f64) * 100.0).round() as u16
                };
                ui.add_sized(
                    [40.0, 18.0],
                    egui::Label::new(
                        RichText::new(format!("{pct:>3}%"))
                            .color(pal.ink_soft)
                            .monospace()
                            .small(),
                    ),
                );
                chip(ui, "●", tally.done, pal.status_done, pal);
                chip(ui, "◐", tally.working, pal.status_working, pal);
                chip(ui, "○", tally.pending, pal.status_pending, pal);
                chip(ui, "✗", tally.failed, pal.status_failed, pal);

                let (state, watchers) = self.app.remote_status();
                if state != crate::remote::protocol::RemoteState::Disconnected {
                    let color = match state {
                        crate::remote::protocol::RemoteState::Connected => pal.accent,
                        crate::remote::protocol::RemoteState::Error => pal.status_failed,
                        _ => pal.status_working,
                    };
                    ui.label(
                        RichText::new(format!("⇄{watchers}"))
                            .color(color)
                            .monospace()
                            .small(),
                    )
                    .on_hover_text(format!("remote: {}", state.label()));
                }
            });
        });

        for a in actions {
            self.app.apply(a);
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui, pal: &GuiPalette) {
        let crumb = self.app.crumb();
        ui.add(
            egui::Label::new(
                RichText::new(crumb.trim_start_matches("honya 本屋").trim())
                    .color(pal.ink_soft)
                    .small(),
            )
            .truncate(),
        );
        ui.add_space(10.0);

        for (screen, glyph, label) in NAV {
            let selected = self.app.screen == screen;
            // Reserve the spinner column so Translate doesn't grow when live.
            let spin = if screen == Screen::Translate && self.app.run_active {
                crate::theme::spinner_frame(self.app.frame)
            } else {
                " "
            };
            let color = if selected { pal.accent } else { pal.ink_soft };
            let text = RichText::new(format!("{glyph}  {label}  {spin}"))
                .size(15.0)
                .color(color);
            let resp = ui.add_sized(
                [ui.available_width(), 34.0],
                egui::Button::selectable(selected, text),
            );
            if resp.clicked() {
                self.app.apply(Action::Goto(screen));
            }
            if selected {
                let rect = resp.rect;
                ui.painter().line_segment(
                    [
                        egui::pos2(rect.left() + 2.0, rect.top() + 6.0),
                        egui::pos2(rect.left() + 2.0, rect.bottom() - 6.0),
                    ],
                    egui::Stroke::new(2.0_f32, pal.accent),
                );
            }
            ui.add_space(2.0);
        }

        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            if ui
                .add_sized(
                    [ui.available_width(), 30.0],
                    egui::Button::new("⚙  Settings"),
                )
                .clicked()
            {
                self.app.apply(Action::show_overlay(Overlay::settings_with_field(
                    &self.app.cfg,
                    0,
                )));
            }
            ui.add_space(2.0);
            if ui
                .add_sized([ui.available_width(), 30.0], egui::Button::new("▤  Log"))
                .clicked()
            {
                self.app.apply(Action::show_overlay(Overlay::Log(0)));
            }
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui, pal: &GuiPalette) {
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), ui.available_height()),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.label(
                    RichText::new(format!("honya {}", crate::update::version_string()))
                        .color(pal.ink_faint)
                        .small(),
                );
                // Fixed-width status slot so run/update labels don't shove the toast.
                let status = if self.app.run_active {
                    "· translating…"
                } else if self.app.update_installed.is_some() {
                    "· update installed — restart"
                } else if self.app.update_available.is_some() {
                    "· update available"
                } else {
                    ""
                };
                ui.add_sized(
                    [170.0, 16.0],
                    egui::Label::new(
                        RichText::new(status)
                            .color(if self.app.run_active {
                                pal.status_working
                            } else {
                                pal.status_warn
                            })
                            .small(),
                    ),
                );

                if let Some(toast) = self.app.toast.clone() {
                    let color = match toast.level {
                        crate::model::LogLevel::Error => pal.status_failed,
                        crate::model::LogLevel::Warn => pal.status_warn,
                        _ => pal.accent,
                    };
                    if ui
                        .add(
                            egui::Label::new(RichText::new(&toast.msg).color(color).small())
                                .truncate()
                                .sense(egui::Sense::click()),
                        )
                        .on_hover_text("click to dismiss")
                        .clicked()
                    {
                        self.app.toast = None;
                    }
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new("Ctrl+P palette · Ctrl+, settings · 1–6 screens · Esc close")
                            .color(pal.ink_faint)
                            .small(),
                    );
                });
            },
        );
    }
}

fn chip(ui: &mut egui::Ui, glyph: &str, n: u32, color: egui::Color32, pal: &GuiPalette) {
    // Pad counts so 9 → 10 never changes chip width.
    let text = format!("{glyph}{n:>3}");
    let c = if n == 0 { pal.ink_faint } else { color };
    ui.add_sized(
        [42.0, 18.0],
        egui::Label::new(RichText::new(text).color(c).monospace().small()),
    );
}
