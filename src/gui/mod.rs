//! Native window GUI (`honya --gui`).
//!
//! A real desktop layout — menu bar, navigation sidebar, status bar, modal
//! dialogs — over the same `App` state, Action funnel, and theme palettes as
//! the TUI; not a terminal grid in a window.

mod drawer;
mod fonts;
mod inspector;
mod overlays;
mod screens;
mod settings;
mod shell;
mod tabs;
mod theme_map;
mod tree;
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
/// The workspace regions between them are the user's to drag.
const MENUBAR_H: f32 = 36.0;
const FOOTER_H: f32 = 30.0;
const SIDEBAR_RANGE: std::ops::RangeInclusive<f32> = 180.0..=460.0;
const INSPECTOR_RANGE: std::ops::RangeInclusive<f32> = 220.0..=520.0;
const DRAWER_RANGE: std::ops::RangeInclusive<f32> = 110.0..=520.0;

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

    let events: Events = Default::default();
    let gui = GuiApp {
        app,
        events: events.clone(),
        last_tick: Instant::now(),
        tick_every: Duration::from_millis(100),
        nav: GuiNav::default(),
        tree: tree::TreeState::default(),
        tabs: tabs::Tabs::default(),
        qa: drawer::QaCache::default(),
        layout: shell::Layout::load(),
        applied_theme: None,
        fonts_ready: false,
    };

    eframe::run_native(
        "honya",
        options,
        Box::new(move |cc| {
            fonts::install(&cc.egui_ctx);
            // Events wake the window instead of waiting for the next poll: the
            // paint loop used to `try_recv` on a 200 ms timer, so a finished
            // chapter could sit unread for a fifth of a second.
            let ctx = cc.egui_ctx.clone();
            tokio::spawn(async move {
                let mut rx = rx;
                while let Some(ev) = rx.recv().await {
                    if let Ok(mut queue) = events.lock() {
                        queue.push_back(ev);
                    }
                    ctx.request_repaint();
                }
            });
            Ok(Box::new(gui))
        }),
    )
    .map_err(|e| anyhow::anyhow!("gui: {e}"))?;
    Ok(())
}

/// Events parked by the waker task until the next frame drains them.
type Events = std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<AppEvent>>>;

struct GuiApp {
    app: App,
    events: Events,
    last_tick: Instant,
    tick_every: Duration,
    nav: GuiNav,
    tree: tree::TreeState,
    tabs: tabs::Tabs,
    qa: drawer::QaCache,
    layout: shell::Layout,
    applied_theme: Option<ThemeId>,
    fonts_ready: bool,
}

impl GuiApp {
    /// The theme to paint with right now: the picker's live selection while it
    /// is open (preview), else the saved config theme.
    fn effective_theme(&self) -> ThemeId {
        if let Overlay::Theme(st) = &self.app.overlay {
            ALL_THEMES
                .get(st.sel)
                .copied()
                .unwrap_or(self.app.cfg.theme)
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

        while let Some(ev) = self.events.lock().ok().and_then(|mut q| q.pop_front()) {
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
                                self.app
                                    .apply(Action::show_overlay(Overlay::settings_with_field(
                                        &self.app.cfg,
                                        0,
                                    )));
                            }
                            egui::Key::P | egui::Key::K => {
                                self.app.apply(Action::show_overlay(Overlay::palette()));
                            }
                            egui::Key::J => {
                                let tab = self.layout.drawer_tab;
                                self.layout.toggle_drawer(tab);
                            }
                            egui::Key::Num1 => {
                                self.layout.sidebar_open = !self.layout.sidebar_open;
                            }
                            egui::Key::Num2 => {
                                self.layout.inspector_open = !self.layout.inspector_open;
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

        // The ticker only has to run as often as something is moving; an event
        // wakes the window on its own now.
        let animating = self.app.run_active
            || self.app.toast.is_some()
            || self.app.refine.is_in_flight()
            || self.app.refine.has_running_subagents();
        ctx.request_repaint_after(Duration::from_millis(if animating { 100 } else { 200 }));
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

        // The workspace regions, in the order egui claims space: what is left
        // over is the body. Each remembers the width it was dragged to, and a
        // window too narrow to hold them all drops them rather than squeezing
        // the body to nothing.
        let (show_sidebar, show_inspector) = shell::fit(full.width(), &self.layout);
        if show_sidebar {
            let shown = egui::Panel::left("honya_sidebar")
                .resizable(true)
                .default_size(self.layout.sidebar_w)
                .size_range(SIDEBAR_RANGE)
                .frame(shell::panel_frame(&pal))
                .show_inside(ui, |ui| {
                    self.sidebar(ui, &pal);
                });
            self.layout.sidebar_w = shown.response.rect.width();
        }

        if show_inspector {
            let shown = egui::Panel::right("honya_inspector")
                .resizable(true)
                .default_size(self.layout.inspector_w)
                .size_range(INSPECTOR_RANGE)
                .frame(shell::panel_frame(&pal))
                .show_inside(ui, |ui| {
                    self.inspector(ui, &pal);
                });
            self.layout.inspector_w = shown.response.rect.width();
        }

        if self.layout.drawer_open {
            let shown = egui::Panel::bottom("honya_drawer")
                .resizable(true)
                .default_size(self.layout.drawer_h)
                .size_range(DRAWER_RANGE)
                .frame(shell::panel_frame(&pal))
                .show_inside(ui, |ui| {
                    self.drawer(ui, &pal);
                });
            self.layout.drawer_h = shown.response.rect.height();
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(pal.bg)
                    .inner_margin(egui::Margin::symmetric(16, 12)),
            )
            .show_inside(ui, |ui| {
                self.tab_strip(ui, &pal);
                screens::render_body(ui, &mut self.app, &mut self.nav, &pal);
                overlays::render(ui, &mut self.app, &pal);
            });
    }

    fn on_exit(&mut self) {
        self.layout.save();
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
                let mut layout = self.layout.clone();
                ui.checkbox(&mut layout.sidebar_open, "Workspace   Ctrl+1");
                ui.checkbox(&mut layout.inspector_open, "Inspector   Ctrl+2");
                ui.checkbox(&mut layout.drawer_open, "Drawer   Ctrl+J");
                ui.separator();
                for tab in shell::DrawerTab::ALL {
                    if ui.button(tab.label()).clicked() {
                        layout.drawer_open = true;
                        layout.drawer_tab = tab;
                    }
                }
                self.layout = layout;
                ui.separator();
                if ui.button("Command palette…   Ctrl+P").clicked() {
                    actions.push(Action::show_overlay(Overlay::palette()));
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
        // The view switcher first, compact, so the rest of the region is the
        // work rather than six buttons that change which screen is showing.
        ui.horizontal_wrapped(|ui| {
            for (screen, glyph, label) in NAV {
                let selected = self.app.screen == screen;
                let spin = if screen == Screen::Translate && self.app.run_active {
                    crate::theme::spinner_frame(self.app.frame)
                } else {
                    ""
                };
                let text = RichText::new(format!("{glyph}{spin}"))
                    .size(14.0)
                    .color(if selected { pal.accent } else { pal.ink_soft });
                if ui
                    .add(egui::Button::selectable(selected, text))
                    .on_hover_text(label)
                    .clicked()
                {
                    self.app.apply(Action::Goto(screen));
                }
            }
        });
        ui.add_space(6.0);
        ui.separator();

        let tree_h = (ui.available_height() - 70.0).max(80.0);
        ui.allocate_ui(egui::vec2(ui.available_width(), tree_h), |ui| {
            for action in tree::show(ui, &self.app, &mut self.tree, pal) {
                self.dispatch(action);
            }
        });

        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            if ui
                .add_sized(
                    [ui.available_width(), 28.0],
                    egui::Button::new(RichText::new("⚙  Settings").small()),
                )
                .clicked()
            {
                self.app
                    .apply(Action::show_overlay(Overlay::settings_with_field(
                        &self.app.cfg,
                        0,
                    )));
            }
            ui.add_space(2.0);
            if ui
                .add_sized(
                    [ui.available_width(), 28.0],
                    egui::Button::new(RichText::new("▤  Activity").small()),
                )
                .clicked()
            {
                self.layout.toggle_drawer(shell::DrawerTab::Activity);
            }
        });
    }

    /// What the app is showing right now, as a tab identity.
    fn current_tab(&self) -> tabs::TabId {
        match self.app.screen {
            Screen::Reader => tabs::TabId::Chapter {
                vol: self.app.active.as_ref().map(|a| a.vol).unwrap_or(1),
                ch: self.app.reader.chapter,
            },
            Screen::Refine => tabs::TabId::Session(self.app.refine_session_id.clone()),
            other => tabs::TabId::View(other),
        }
    }

    /// Apply an action, and put whatever it opens into a tab.
    ///
    /// A chapter or a conversation already open comes forward with the state
    /// it had — so the action that would reload it is dropped, because
    /// reloading throws away exactly the scroll position the tab is for.
    fn dispatch(&mut self, action: Action) {
        let want = match &action {
            Action::OpenChapter { chapter }
            | Action::OpenChapterAt { chapter, .. }
            | Action::OpenChapterAtChunk { chapter, .. } => Some(tabs::TabId::Chapter {
                vol: self.app.active.as_ref().map(|a| a.vol).unwrap_or(1),
                ch: *chapter,
            }),
            Action::RefineSwitchSession { id } => Some(tabs::TabId::Session(id.clone())),
            Action::Goto(screen) => Some(match screen {
                Screen::Reader | Screen::Refine => self.current_tab(),
                other => tabs::TabId::View(*other),
            }),
            _ => None,
        };
        if let Some(id) = want {
            let title = tabs::title_for(&id, &self.app);
            let fresh = self.tabs.focus(id, title, &mut self.app);
            if !fresh && matches!(action, Action::OpenChapter { .. }) {
                self.app.apply(Action::Goto(Screen::Reader));
                return;
            }
        }
        self.app.apply(action);
    }

    fn tab_strip(&mut self, ui: &mut egui::Ui, pal: &GuiPalette) {
        // Anything that changed the view without going through `dispatch` — a
        // digit key, a slash command, a recovery dialog — still gets a tab.
        let current = self.current_tab();
        let title = tabs::title_for(&current, &self.app);
        self.tabs.ensure(current, title);

        let mut pick = None;
        let mut close = None;
        ui.horizontal(|ui| {
            for (i, tab) in self.tabs.iter() {
                let selected = i == self.tabs.active_index();
                ui.push_id(("tab", i), |ui| {
                    if ui
                        .add(egui::Button::selectable(
                            selected,
                            RichText::new(&tab.title)
                                .small()
                                .color(if selected { pal.ink } else { pal.ink_soft }),
                        ))
                        .clicked()
                    {
                        pick = Some(i);
                    }
                    if selected
                        && ui
                            .add(
                                egui::Button::new(RichText::new("✕").small().color(pal.ink_faint))
                                    .frame(false),
                            )
                            .on_hover_text("close this tab")
                            .clicked()
                    {
                        close = Some(i);
                    }
                });
            }
        });
        ui.add_space(4.0);

        if let Some(i) = close {
            if let Some(id) = self.tabs.close(i, &mut self.app) {
                self.show_tab(&id);
            }
            return;
        }
        let picked_id = pick.and_then(|i| self.tabs.iter().nth(i).map(|(_, t)| t.id.clone()));
        if let Some(id) = picked_id {
            let title = tabs::title_for(&id, &self.app);
            let fresh = self.tabs.focus(id.clone(), title, &mut self.app);
            if fresh {
                self.show_tab(&id);
            } else {
                self.app.apply(Action::Goto(id.view()));
            }
        }
    }

    /// Put the app on what this tab shows.
    fn show_tab(&mut self, id: &tabs::TabId) {
        match id {
            tabs::TabId::View(s) => self.app.apply(Action::Goto(*s)),
            tabs::TabId::Chapter { vol, ch } => {
                self.app.apply(Action::SetActiveVolume { vol: *vol });
                self.app.apply(Action::OpenChapter { chapter: *ch });
            }
            tabs::TabId::Session(sid) => {
                self.app.apply(Action::RefineSwitchSession { id: sid.clone() });
                self.app.apply(Action::Goto(Screen::Refine));
            }
        }
    }

    /// Detail for whatever the tree is pointing at, rather than for whichever
    /// view is showing. The chapter's numbers used to be visible only on the
    /// Project screen, so reading one meant leaving the thing you were reading.
    fn inspector(&mut self, ui: &mut egui::Ui, pal: &GuiPalette) {
        shell::pane_header(ui, pal, "INSPECTOR", |ui| {
            if ui
                .add(egui::Button::new(RichText::new("✕").small()).frame(false))
                .on_hover_text("hide the inspector")
                .clicked()
            {
                self.layout.inspector_open = false;
            }
        });
        let mut actions = Vec::new();
        egui::ScrollArea::vertical()
            .id_salt("inspector_body")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                inspector::show(
                    ui,
                    &self.app,
                    self.tree.selection.as_ref(),
                    pal,
                    &mut actions,
                );
            });
        for a in actions {
            self.dispatch(a);
        }
    }

    /// The panes you keep open beside the work, rather than dialogs that cover
    /// the thing they are about.
    fn drawer(&mut self, ui: &mut egui::Ui, pal: &GuiPalette) {
        let log_len = self.app.log.len() as u32;
        let running = self.app.refine.running_subagent_count() as u32;
        let queued = self
            .app
            .run_queue
            .as_ref()
            .map(|q| {
                let (running, pending) = q.snapshot();
                pending.len() as u32 + u32::from(running.is_some())
            })
            .unwrap_or(0);
        let picked = shell::drawer_tabs(ui, pal, self.layout.drawer_tab, |tab| match tab {
            shell::DrawerTab::Activity => Some(log_len),
            shell::DrawerTab::Tasks => Some(running),
            shell::DrawerTab::Queue => Some(queued),
            shell::DrawerTab::Qa => None,
        });
        if let Some(tab) = picked {
            self.layout.drawer_tab = tab;
        }
        ui.separator();
        let mut actions = Vec::new();
        drawer::show(
            ui,
            &self.app,
            self.layout.drawer_tab,
            &mut self.qa,
            pal,
            &mut actions,
        );
        for a in actions {
            self.dispatch(a);
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui, pal: &GuiPalette) {
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), ui.available_height()),
            Layout::left_to_right(Align::Center),
            |ui| {
                // The breadcrumb reads better down here than above the tree:
                // the tree shows where you are, this says it in words.
                ui.label(RichText::new(self.app.crumb()).color(pal.ink_faint).small());
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
