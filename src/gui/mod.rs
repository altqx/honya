//! Native window GUI (`honya --gui`).
//!
//! A real desktop layout (tabs, cards, lists, modals) over the same `App` state,
//! Action funnel, and theme palettes as the TUI — not a terminal grid in a window.

mod fonts;
mod overlays;
mod screens;
mod theme_map;

use std::time::{Duration, Instant};

use egui::{Align, Layout, RichText};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app::{Action, App, Screen};
use crate::model::{AppEvent, ThemeId};
use crate::theme::ALL_THEMES;

use self::screens::GuiNav;
use self::theme_map::GuiPalette;

/// Fixed chrome sizes so the central body never reflows when toast/spinner/tally change.
const HEADER_H: f32 = 52.0;
const TABS_H: f32 = 44.0;
const FOOTER_H: f32 = 52.0;
const TAB_W: f32 = 118.0;
const TAB_BTN_H: f32 = 30.0;

/// Block the current thread on the native window. Tokio multi-thread workers keep
/// running background tasks (pipeline, import, remote) while this thread pumps UI.
pub fn run(app: App, rx: UnboundedReceiver<AppEvent>) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([800.0, 520.0])
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

        // Re-apply egui visuals when the theme changes (picker / settings).
        let theme_id = self.app.cfg.theme;
        if self.applied_theme != Some(theme_id) {
            GuiPalette::from_theme_id(theme_id).apply(ctx);
            self.applied_theme = Some(theme_id);
        }

        // Global shortcuts that don't fight text fields.
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
                                    crate::app::overlay::Overlay::settings_with_field(
                                        &self.app.cfg,
                                        0,
                                    ),
                                ));
                            }
                            _ => {}
                        }
                        continue;
                    }
                    // Digits switch tabs when no overlay is capturing.
                    if matches!(self.app.overlay, crate::app::overlay::Overlay::None) {
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
                        if !matches!(self.app.overlay, crate::app::overlay::Overlay::None) {
                            // Prefer CancelTheme when the theme picker is open.
                            if matches!(self.app.overlay, crate::app::overlay::Overlay::Theme(_)) {
                                self.app.apply(Action::CancelTheme);
                            } else {
                                self.app.overlay = crate::app::overlay::Overlay::None;
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
        let pal = GuiPalette::from_theme_id(self.app.cfg.theme);

        // Full-window background
        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0.0, pal.bg);

        egui::Panel::top("honya_header")
            .exact_size(HEADER_H)
            .frame(
                egui::Frame::NONE
                    .fill(pal.bg_panel)
                    .stroke(egui::Stroke::new(1.0_f32, pal.rule))
                    .inner_margin(egui::Margin::symmetric(16, 8)),
            )
            .show_inside(ui, |ui| {
                self.header(ui, &pal);
            });

        egui::Panel::top("honya_tabs")
            .exact_size(TABS_H)
            .frame(
                egui::Frame::NONE
                    .fill(pal.bg)
                    .inner_margin(egui::Margin::symmetric(12, 6)),
            )
            .show_inside(ui, |ui| {
                self.tabs(ui, &pal);
            });

        // Fixed footer height always — toast must not resize the central pane.
        egui::Panel::bottom("honya_footer")
            .exact_size(FOOTER_H)
            .frame(
                egui::Frame::NONE
                    .fill(pal.bg_panel)
                    .stroke(egui::Stroke::new(1.0_f32, pal.rule))
                    .inner_margin(egui::Margin::symmetric(14, 6)),
            )
            .show_inside(ui, |ui| {
                self.footer(ui, &pal);
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
    fn header(&mut self, ui: &mut egui::Ui, pal: &GuiPalette) {
        let crumb = self.app.crumb();
        let tally = self.app.tally();
        let total = tally.done + tally.working + tally.pending + tally.failed;
        let pct = if total == 0 {
            0
        } else {
            ((tally.done as f64 / total as f64) * 100.0).round() as u16
        };

        // Single fixed-height row: left brand/crumb, right chips + toolbar.
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), ui.available_height()),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.label(
                    RichText::new("本屋")
                        .color(pal.accent)
                        .strong()
                        .size(18.0),
                );
                // Truncate crumb into a stable max width so long titles don't shove toolbar.
                let crumb_w = (ui.available_width() - 520.0).clamp(120.0, 420.0);
                ui.add_sized(
                    [crumb_w, 24.0],
                    egui::Label::new(RichText::new(&crumb).color(pal.ink).size(15.0))
                        .truncate(),
                );

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    // Fixed-width toolbar buttons so label length never reflows the row.
                    toolbar_btn(ui, "Log", || {
                        self.app
                            .apply(Action::show_overlay(crate::app::overlay::Overlay::Log(0)));
                    });
                    toolbar_btn(ui, "Theme", || {
                        self.app.apply(Action::show_overlay(
                            crate::app::overlay::Overlay::theme(self.app.cfg.theme),
                        ));
                    });
                    toolbar_btn(ui, "Settings", || {
                        self.app.apply(Action::show_overlay(
                            crate::app::overlay::Overlay::settings_with_field(&self.app.cfg, 0),
                        ));
                    });
                    toolbar_btn(ui, "Help", || {
                        self.app
                            .apply(Action::show_overlay(crate::app::overlay::Overlay::Help(0)));
                    });

                    egui::ComboBox::from_id_salt("theme_quick")
                        .selected_text(self.app.cfg.theme.label())
                        .width(148.0)
                        .show_ui(ui, |ui| {
                            for &id in ALL_THEMES {
                                if ui
                                    .selectable_label(self.app.cfg.theme == id, id.label())
                                    .clicked()
                                {
                                    self.app.apply(Action::SaveTheme(id));
                                }
                            }
                        });

                    ui.add_space(8.0);

                    // Monospace fixed-width chips (numbers pad so digit changes don't jiggle).
                    chip(ui, "✗", tally.failed, pal.status_failed, pal);
                    chip(ui, "○", tally.pending, pal.status_pending, pal);
                    chip(ui, "◐", tally.working, pal.status_working, pal);
                    chip(ui, "●", tally.done, pal.status_done, pal);
                    ui.add_sized(
                        [40.0, 18.0],
                        egui::Label::new(
                            RichText::new(format!("{pct:>3}%"))
                                .color(pal.ink_soft)
                                .monospace()
                                .small(),
                        ),
                    );
                });
            },
        );
    }

    fn tabs(&mut self, ui: &mut egui::Ui, pal: &GuiPalette) {
        let tabs = [
            (Screen::Shelf, "1  書架"),
            (Screen::Project, "2  構図"),
            (Screen::Translate, "3  訳"),
            (Screen::Reader, "4  読"),
            (Screen::Lexicon, "5  辞"),
            (Screen::Refine, "6  磨"),
        ];
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), ui.available_height()),
            Layout::left_to_right(Align::Center),
            |ui| {
                for (screen, label) in tabs {
                    let selected = self.app.screen == screen;
                    // Always reserve a spinner column so Translate doesn't grow when live.
                    let spin = if screen == Screen::Translate && self.app.run_active {
                        crate::theme::spinner_frame(self.app.frame)
                    } else {
                        " "
                    };
                    // Color only — no bold (strong changes glyph advance and jiggles neighbors).
                    let text = RichText::new(format!("{label}  {spin}")).size(14.0).color(
                        if selected {
                            pal.accent
                        } else {
                            pal.ink_soft
                        },
                    );
                    let response =
                        ui.add_sized([TAB_W, TAB_BTN_H], egui::Button::selectable(selected, text));
                    if response.clicked() {
                        self.app.apply(Action::Goto(screen));
                    }
                    if selected {
                        let rect = response.rect;
                        ui.painter().line_segment(
                            [
                                egui::pos2(rect.left() + 10.0, rect.bottom() - 1.0),
                                egui::pos2(rect.right() - 10.0, rect.bottom() - 1.0),
                            ],
                            egui::Stroke::new(2.0_f32, pal.accent),
                        );
                    }
                }
            },
        );
    }

    fn footer(&mut self, ui: &mut egui::Ui, pal: &GuiPalette) {
        // Two fixed rows inside FOOTER_H: toast slot (always) + status line (always).
        ui.vertical(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), 20.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    if let Some(toast) = self.app.toast.clone() {
                        let color = match toast.level {
                            crate::model::LogLevel::Error => pal.status_failed,
                            crate::model::LogLevel::Warn => pal.status_warn,
                            _ => pal.accent,
                        };
                        ui.add(
                            egui::Label::new(RichText::new(&toast.msg).color(color).small())
                                .truncate(),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.small_button("Dismiss").clicked() {
                                self.app.toast = None;
                            }
                        });
                    } else {
                        // Keep the row height even with no toast.
                        ui.add_space(1.0);
                    }
                },
            );
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), 18.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.label(
                        RichText::new(format!("honya {}", crate::update::version_string()))
                            .color(pal.ink_faint)
                            .small(),
                    );
                    // Fixed-width status slots so run/update labels don't shove the right side.
                    let status = if self.app.run_active {
                        "· translating…"
                    } else if self.app.update_available.is_some() {
                        "· update available"
                    } else {
                        ""
                    };
                    ui.add_sized(
                        [140.0, 16.0],
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
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new("Ctrl+, settings  ·  1–6 tabs  ·  Esc close")
                                .color(pal.ink_faint)
                                .small(),
                        );
                    });
                },
            );
        });
    }
}

fn toolbar_btn(ui: &mut egui::Ui, label: &str, on_click: impl FnOnce()) {
    if ui
        .add_sized([72.0, 26.0], egui::Button::new(label))
        .clicked()
    {
        on_click();
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
