//! The workspace shell: five persistent regions instead of one screen and a
//! stack of modals.
//!
//! The regions are a menu bar, a resizable workspace tree, the tab body, a
//! collapsible inspector and a drawer. What makes it a workspace rather than a
//! tabbed form is that they are all up at once — the activity log and the QA
//! findings are things you work *beside*, not dialogs you dismiss to get back
//! to what they were about.

use egui::{Align, Layout as EguiLayout, RichText};
use serde::{Deserialize, Serialize};

use super::theme_map::GuiPalette;

/// Sizes the user has dragged to, kept across restarts.
///
/// Written on exit rather than on every drag: a splitter emits a value per
/// frame while it is held, and none of them are worth a file write.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    pub sidebar_w: f32,
    pub inspector_w: f32,
    pub drawer_h: f32,
    pub sidebar_open: bool,
    pub inspector_open: bool,
    pub drawer_open: bool,
    pub drawer_tab: DrawerTab,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            sidebar_w: 230.0,
            inspector_w: 280.0,
            drawer_h: 200.0,
            sidebar_open: true,
            inspector_open: true,
            drawer_open: false,
            drawer_tab: DrawerTab::Activity,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DrawerTab {
    Activity,
    Tasks,
    Qa,
    Queue,
}

impl DrawerTab {
    pub const ALL: [DrawerTab; 4] = [
        DrawerTab::Activity,
        DrawerTab::Tasks,
        DrawerTab::Qa,
        DrawerTab::Queue,
    ];

    pub fn label(self) -> &'static str {
        match self {
            DrawerTab::Activity => "▤  Activity",
            DrawerTab::Tasks => "◇  Tasks",
            DrawerTab::Qa => "⚑  QA",
            DrawerTab::Queue => "≡  Queue",
        }
    }
}

fn layout_path() -> std::path::PathBuf {
    crate::config::config_dir().join("gui-layout.json")
}

impl Layout {
    pub fn load() -> Self {
        std::fs::read_to_string(layout_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Best-effort: a layout that fails to save is not worth interrupting a
    /// quit for, and the next run simply opens at the defaults.
    pub fn save(&self) {
        let path = layout_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }

    pub fn toggle_drawer(&mut self, tab: DrawerTab) {
        if self.drawer_open && self.drawer_tab == tab {
            self.drawer_open = false;
        } else {
            self.drawer_open = true;
            self.drawer_tab = tab;
        }
    }
}

/// The narrowest the body may be squeezed to before a side region gives way.
const MIN_BODY_W: f32 = 420.0;

/// Which side regions actually fit, given how wide the window is right now.
///
/// A tiling window manager will hand the window whatever width it likes, and
/// two fixed side panels can leave no body at all. The regions yield in order
/// — inspector first, then the tree — and the user's own open/closed choice is
/// untouched, so both come back when there is room again.
pub fn fit(available_w: f32, layout: &Layout) -> (bool, bool) {
    let mut sidebar = layout.sidebar_open;
    let mut inspector = layout.inspector_open;
    let used = |s: bool, i: bool| {
        (if s { layout.sidebar_w } else { 0.0 }) + (if i { layout.inspector_w } else { 0.0 })
    };
    if available_w - used(sidebar, inspector) < MIN_BODY_W {
        inspector = false;
    }
    if available_w - used(sidebar, inspector) < MIN_BODY_W {
        sidebar = false;
    }
    (sidebar, inspector)
}

/// A region's chrome, so every pane sits on the same surface with the same rule.
pub fn panel_frame(pal: &GuiPalette) -> egui::Frame {
    egui::Frame::NONE
        .fill(pal.bg_panel)
        .stroke(egui::Stroke::new(1.0_f32, pal.rule))
        .inner_margin(egui::Margin::symmetric(10, 8))
}

/// A pane's title row: a small caps-ish label and an optional trailing control.
pub fn pane_header(
    ui: &mut egui::Ui,
    pal: &GuiPalette,
    title: &str,
    trailing: impl FnOnce(&mut egui::Ui),
) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(title)
                .color(pal.ink_faint)
                .small()
                .strong(),
        );
        ui.with_layout(EguiLayout::right_to_left(Align::Center), trailing);
    });
    ui.add_space(4.0);
}

/// The drawer's tab strip. Returns the tab the user clicked, if any.
pub fn drawer_tabs(
    ui: &mut egui::Ui,
    pal: &GuiPalette,
    active: DrawerTab,
    counts: impl Fn(DrawerTab) -> Option<u32>,
) -> Option<DrawerTab> {
    let mut picked = None;
    ui.horizontal(|ui| {
        for tab in DrawerTab::ALL {
            let label = match counts(tab) {
                Some(n) if n > 0 => format!("{}  {n}", tab.label()),
                _ => tab.label().to_string(),
            };
            let selected = tab == active;
            let text = RichText::new(label)
                .small()
                .color(if selected { pal.accent } else { pal.ink_soft });
            if ui.add(egui::Button::selectable(selected, text)).clicked() {
                picked = Some(tab);
            }
        }
    });
    picked
}

/// The activity log, as a pane rather than a dialog.
///
/// Unlike the modal it replaces, this honours the whole buffer instead of the
/// last four hundred lines: a log you cannot scroll back through is a log that
/// hides the thing you opened it for.
pub fn activity_pane(ui: &mut egui::Ui, log: &[(crate::model::LogLevel, String)], pal: &GuiPalette) {
    use crate::model::LogLevel;
    if log.is_empty() {
        ui.label(
            RichText::new("Nothing has happened yet.")
                .color(pal.ink_faint)
                .italics()
                .small(),
        );
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt("drawer_activity")
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show_rows(ui, ui.text_style_height(&egui::TextStyle::Small), log.len(), |ui, rows| {
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
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_layout_round_trips_through_json() {
        let l = Layout {
            sidebar_w: 311.0,
            drawer_open: true,
            drawer_tab: DrawerTab::Qa,
            ..Default::default()
        };
        let json = serde_json::to_string(&l).unwrap();
        let back: Layout = serde_json::from_str(&json).unwrap();
        assert_eq!(back.sidebar_w, 311.0);
        assert!(back.drawer_open);
        assert_eq!(back.drawer_tab, DrawerTab::Qa);
    }

    #[test]
    fn a_layout_file_from_an_older_build_keeps_its_defaults() {
        // `#[serde(default)]` on the struct: a field added later must not make
        // an existing layout file unreadable and throw the user's sizes away.
        let back: Layout = serde_json::from_str(r#"{"sidebar_w":190.0}"#).unwrap();
        assert_eq!(back.sidebar_w, 190.0);
        assert_eq!(back.inspector_w, Layout::default().inspector_w);
        assert!(back.sidebar_open);
    }

    #[test]
    fn a_narrow_window_gives_the_body_back_its_room() {
        let layout = Layout::default(); // 230 tree + 280 inspector
        // Wide: both fit.
        assert_eq!(fit(1200.0, &layout), (true, true));
        // Tight: the inspector goes first — it is the one you consult, not the
        // one you navigate with.
        assert_eq!(fit(800.0, &layout), (true, false));
        // Narrower still: the tree goes too, rather than leaving no body.
        assert_eq!(fit(500.0, &layout), (false, false));
    }

    #[test]
    fn yielding_to_a_narrow_window_does_not_close_the_panes_for_good() {
        let layout = Layout::default();
        assert_eq!(fit(396.0, &layout), (false, false));
        // The user's own choice is untouched, so widening brings them back.
        assert!(layout.sidebar_open && layout.inspector_open);
        assert_eq!(fit(1200.0, &layout), (true, true));
    }

    #[test]
    fn a_pane_the_user_closed_stays_closed_when_there_is_room() {
        let layout = Layout {
            inspector_open: false,
            ..Default::default()
        };
        assert_eq!(fit(1600.0, &layout), (true, false));
    }

    #[test]
    fn the_drawer_toggle_closes_only_the_tab_that_is_showing() {
        let mut l = Layout::default();
        assert!(!l.drawer_open);

        l.toggle_drawer(DrawerTab::Activity);
        assert!(l.drawer_open);
        assert_eq!(l.drawer_tab, DrawerTab::Activity);

        // A different tab switches rather than closing — otherwise reaching for
        // QA while the log is up would shut the drawer instead.
        l.toggle_drawer(DrawerTab::Qa);
        assert!(l.drawer_open);
        assert_eq!(l.drawer_tab, DrawerTab::Qa);

        l.toggle_drawer(DrawerTab::Qa);
        assert!(!l.drawer_open);
    }
}
