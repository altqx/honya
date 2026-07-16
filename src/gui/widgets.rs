//! Small shared building blocks for the native GUI.

use egui::{Response, RichText, TextEdit, Ui};

use super::theme_map::GuiPalette;

/// Small colored section heading used inside cards and dialogs.
pub fn section(ui: &mut Ui, pal: &GuiPalette, title: &str) {
    ui.label(RichText::new(title).color(pal.accent).strong().small());
}

/// Faint one-line explainer under a field.
pub fn hint(ui: &mut Ui, pal: &GuiPalette, text: &str) {
    ui.label(RichText::new(text).color(pal.ink_faint).small());
}

/// Accent-filled call-to-action button.
pub fn primary_button(ui: &mut Ui, pal: &GuiPalette, label: &str) -> Response {
    ui.add(
        egui::Button::new(RichText::new(label).color(pal.bg).strong())
            .fill(pal.accent)
            .corner_radius(egui::CornerRadius::same(8)),
    )
}

/// Single-line edit that keeps only ASCII digits (numeric Settings fields).
pub fn numeric_edit(ui: &mut Ui, value: &mut String, width: f32) -> Response {
    let resp = ui.add(TextEdit::singleline(value).desired_width(width));
    if resp.changed() {
        value.retain(|c| c.is_ascii_digit());
    }
    resp
}

/// Masked secret field, or a read-only note when the environment supplies it.
pub fn secret_edit(ui: &mut Ui, pal: &GuiPalette, value: &mut String, env_locked: bool) {
    if env_locked {
        ui.label(
            RichText::new("set by environment variable (read-only)")
                .color(pal.ink_faint)
                .italics(),
        );
    } else {
        ui.add(
            TextEdit::singleline(value)
                .password(true)
                .desired_width(f32::INFINITY),
        );
    }
}

/// The four-swatch preview strip for a theme row.
pub fn theme_swatch(ui: &mut Ui, id: crate::model::ThemeId) {
    let p = super::theme_map::GuiPalette::from_theme_id(id);
    for c in [p.bg, p.bg_panel, p.accent, p.status_working] {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 3.0, c);
    }
}
