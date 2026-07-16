//! Map honya [`Theme`] / [`ThemeId`] onto egui visuals for a native look.

use egui::{Color32, CornerRadius, Shadow, Stroke, Style, Visuals};
use ratatui::style::Color as RatColor;

use crate::model::ThemeId;
use crate::theme::Theme;

/// Resolved palette for painting widgets (always concrete RGB — never Reset).
#[derive(Clone, Copy)]
pub struct GuiPalette {
    pub bg: Color32,
    pub bg_panel: Color32,
    pub bg_inset: Color32,
    pub ink: Color32,
    pub ink_soft: Color32,
    pub ink_faint: Color32,
    pub rule: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub accent_bg: Color32,
    pub status_pending: Color32,
    pub status_working: Color32,
    pub status_done: Color32,
    pub status_failed: Color32,
    pub status_warn: Color32,
    pub status_image: Color32,
    pub ja_text: Color32,
    pub translated_text: Color32,
    pub is_dark: bool,
}

impl GuiPalette {
    pub fn from_theme_id(id: ThemeId) -> Self {
        // Terminal theme uses adaptive Reset colors — pin Sumi in the GUI.
        let theme = if matches!(id, ThemeId::Terminal) {
            Theme::sumi()
        } else {
            id.build()
        };
        Self::from_theme(&theme, id.tone() != "light")
    }

    pub fn from_theme(t: &Theme, is_dark: bool) -> Self {
        Self {
            bg: c(t.bg),
            bg_panel: c(t.bg_panel),
            bg_inset: c(t.bg_inset),
            ink: c(t.ink),
            ink_soft: c(t.ink_soft),
            ink_faint: c(t.ink_faint),
            rule: c(t.rule),
            accent: c(t.accent),
            accent_soft: c(t.accent_soft),
            accent_bg: c(t.accent_bg),
            status_pending: c(t.status_pending),
            status_working: c(t.status_working),
            status_done: c(t.status_done),
            status_failed: c(t.status_failed),
            status_warn: c(t.status_warn),
            status_image: c(t.status_image),
            ja_text: c(t.ja_text),
            translated_text: c(t.translated_text),
            is_dark,
        }
    }

    /// Apply this palette as the live egui style (rounded panels, soft shadows).
    pub fn apply(&self, ctx: &egui::Context) {
        let mut style = (*ctx.global_style()).clone();
        style.visuals = self.visuals();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.window_margin = egui::Margin::same(12);
        style.spacing.menu_margin = egui::Margin::same(8);
        // Solid (non-floating) scrollbars: floating bars expand on hover and shove content.
        style.spacing.scroll = egui::style::ScrollStyle::solid();
        style.spacing.scroll.floating = false;
        style.spacing.scroll.bar_width = 10.0;
        style.spacing.scroll.floating_allocated_width = 10.0;
        // Snappier panel/tab transitions — less interpolated layout thrash.
        style.animation_time = 0.0;
        style.interaction.selectable_labels = false;
        style.text_styles.insert(
            egui::TextStyle::Heading,
            egui::FontId::proportional(22.0),
        );
        style.text_styles.insert(
            egui::TextStyle::Body,
            egui::FontId::proportional(14.5),
        );
        style.text_styles.insert(
            egui::TextStyle::Button,
            egui::FontId::proportional(14.0),
        );
        style.text_styles.insert(
            egui::TextStyle::Small,
            egui::FontId::proportional(12.0),
        );
        style.text_styles.insert(
            egui::TextStyle::Monospace,
            egui::FontId::monospace(13.0),
        );
        ctx.set_global_style(style);
    }

    fn visuals(&self) -> Visuals {
        let mut v = if self.is_dark {
            Visuals::dark()
        } else {
            Visuals::light()
        };

        v.dark_mode = self.is_dark;
        v.override_text_color = Some(self.ink);
        v.window_fill = self.bg_panel;
        v.panel_fill = self.bg;
        v.faint_bg_color = self.bg_panel;
        v.extreme_bg_color = self.bg_inset;
        v.code_bg_color = self.bg_inset;
        v.hyperlink_color = self.accent;
        v.warn_fg_color = self.status_warn;
        v.error_fg_color = self.status_failed;
        v.window_stroke = Stroke::new(1.0_f32, self.rule);
        v.window_corner_radius = CornerRadius::same(10);
        v.menu_corner_radius = CornerRadius::same(8);
        // Keep window/popup shadows tight — large blur paints far outside the
        // widget rect and reads as “the card is bigger than its hit area”.
        v.window_shadow = Shadow {
            offset: [0, 2],
            blur: 6,
            spread: 0,
            color: Color32::from_black_alpha(if self.is_dark { 60 } else { 24 }),
        };
        v.popup_shadow = Shadow {
            offset: [0, 2],
            blur: 6,
            spread: 0,
            color: Color32::from_black_alpha(if self.is_dark { 60 } else { 24 }),
        };
        // Avoid hover expansion drawing outside allocated button rects.
        v.widgets.noninteractive.expansion = 0.0;
        v.widgets.inactive.expansion = 0.0;
        v.widgets.hovered.expansion = 0.0;
        v.widgets.active.expansion = 0.0;
        v.widgets.open.expansion = 0.0;
        v.selection.bg_fill = self.accent_bg;
        v.selection.stroke = Stroke::new(1.0_f32, self.accent);

        // Widgets
        let idle = widget_visuals(self.bg_panel, self.rule, self.ink_soft, 8);
        let hovered = widget_visuals(self.accent_bg, self.accent_soft, self.ink, 8);
        let active = widget_visuals(self.accent, self.accent, Color32::WHITE, 8);
        let open = widget_visuals(self.bg_inset, self.accent, self.ink, 8);

        v.widgets.noninteractive = widget_visuals(self.bg, self.rule, self.ink_soft, 6);
        v.widgets.inactive = idle;
        v.widgets.hovered = hovered;
        v.widgets.active = active;
        v.widgets.open = open;

        v
    }
}

fn widget_visuals(
    bg: Color32,
    stroke: Color32,
    fg: Color32,
    rounding: u8,
) -> egui::style::WidgetVisuals {
    egui::style::WidgetVisuals {
        bg_fill: bg,
        weak_bg_fill: bg,
        bg_stroke: Stroke::new(1.0_f32, stroke),
        fg_stroke: Stroke::new(1.0_f32, fg),
        corner_radius: CornerRadius::same(rounding),
        expansion: 0.0,
    }
}

fn c(color: RatColor) -> Color32 {
    match color {
        RatColor::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        RatColor::Reset => Color32::from_rgb(232, 228, 220),
        RatColor::Black => Color32::from_rgb(0, 0, 0),
        RatColor::Red => Color32::from_rgb(205, 49, 49),
        RatColor::Green => Color32::from_rgb(13, 188, 121),
        RatColor::Yellow => Color32::from_rgb(229, 229, 16),
        RatColor::Blue => Color32::from_rgb(36, 114, 200),
        RatColor::Magenta => Color32::from_rgb(188, 63, 188),
        RatColor::Cyan => Color32::from_rgb(17, 168, 205),
        RatColor::Gray => Color32::from_rgb(204, 204, 204),
        RatColor::DarkGray => Color32::from_rgb(118, 118, 118),
        RatColor::LightRed => Color32::from_rgb(241, 76, 76),
        RatColor::LightGreen => Color32::from_rgb(35, 209, 139),
        RatColor::LightYellow => Color32::from_rgb(245, 245, 67),
        RatColor::LightBlue => Color32::from_rgb(59, 142, 234),
        RatColor::LightMagenta => Color32::from_rgb(214, 112, 214),
        RatColor::LightCyan => Color32::from_rgb(41, 184, 219),
        RatColor::White => Color32::from_rgb(229, 229, 229),
        RatColor::Indexed(i) => {
            // Rough ANSI 16 for Terminal theme leftovers.
            const ANSI: [[u8; 3]; 16] = [
                [0, 0, 0],
                [205, 49, 49],
                [13, 188, 121],
                [229, 229, 16],
                [36, 114, 200],
                [188, 63, 188],
                [17, 168, 205],
                [229, 229, 229],
                [102, 102, 102],
                [241, 76, 76],
                [35, 209, 139],
                [245, 245, 67],
                [59, 142, 234],
                [214, 112, 214],
                [41, 184, 219],
                [255, 255, 255],
            ];
            let rgb = ANSI.get(i as usize).copied().unwrap_or([180, 180, 180]);
            Color32::from_rgb(rgb[0], rgb[1], rgb[2])
        }
    }
}

/// Shared card frame for content panels. No shadow — shadows paint outside the
/// allocated rect and look like the card is larger than its layout box.
pub fn card_frame(pal: &GuiPalette) -> egui::Frame {
    egui::Frame::NONE
        .fill(pal.bg_panel)
        .stroke(Stroke::new(1.0_f32, pal.rule))
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .outer_margin(egui::Margin::ZERO)
        .shadow(egui::Shadow::NONE)
}

pub fn inset_frame(pal: &GuiPalette) -> egui::Frame {
    egui::Frame::NONE
        .fill(pal.bg_inset)
        .stroke(Stroke::new(1.0_f32, pal.rule))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(8, 6))
        .outer_margin(egui::Margin::ZERO)
        .shadow(egui::Shadow::NONE)
}

/// Fill the *remaining* space in `ui` with a card that cannot grow past it.
///
/// Paints the panel background into an **exact** allocated rect (stroke drawn
/// *inside* the rect) and runs children in the shrunk content area. This avoids
/// the common Frame pitfall where content min-height + outer margins overflow
/// the parent ("card larger than the area it covers").
pub fn card_fill(ui: &mut egui::Ui, pal: &GuiPalette, add: impl FnOnce(&mut egui::Ui)) {
    let size = ui.available_size();
    if size.x <= 0.0 || size.y <= 0.0 {
        return;
    }
    let (rect, _resp) = ui.allocate_exact_size(size, egui::Sense::hover());

    // Background + border strictly inside `rect` (not straddling its edge).
    ui.painter().rect(
        rect,
        egui::CornerRadius::same(10),
        pal.bg_panel,
        Stroke::new(1.0_f32, pal.rule),
        egui::StrokeKind::Inside,
    );

    const PAD_X: f32 = 12.0;
    const PAD_Y: f32 = 10.0;
    let inner = egui::Rect::from_min_max(
        egui::pos2(rect.min.x + PAD_X, rect.min.y + PAD_Y),
        egui::pos2(rect.max.x - PAD_X, rect.max.y - PAD_Y),
    );
    if !inner.is_positive() {
        return;
    }

    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.set_clip_rect(inner.intersect(ui.clip_rect()));
    add(&mut child);
}

// Silence unused Style import if only Visuals paths used in some builds.
#[allow(dead_code)]
fn _style_ty(_: Style) {}
