//! Native Settings dialog — the full TUI Settings surface (agents, provider
//! keys, pipeline limits, appearance, account) rendered with desktop widgets.
//!
//! Edits mutate the live [`SettingsState`] working copy in place (same as the
//! TUI); Save routes the whole copy through `SettingsState::save_action()` and
//! the usual `apply()` funnel.

use egui::{Align, ComboBox, Context, Layout, RichText, ScrollArea, TextEdit};

use crate::app::Action;
use crate::app::overlay::{SettingsState, SettingsTab};
use crate::app::settings_defs::{self, Group, Kind, SField};
use crate::model::ThemeId;
use crate::remote::protocol::RemoteState;
use crate::theme::ALL_THEMES;

use super::theme_map::GuiPalette;
use super::widgets::{hint, numeric_edit, primary_button, secret_edit, section, theme_swatch};

/// Render the Settings window. `saved_theme` is the persisted `cfg.theme`;
/// `codex_signed_in` mirrors `cfg.codex_auth`. Emits deferred actions.
pub fn render(
    ctx: &Context,
    st: &mut SettingsState,
    saved_theme: ThemeId,
    codex_signed_in: bool,
    pal: &GuiPalette,
    frame: egui::Frame,
    actions: &mut Vec<Action>,
) {
    egui::Window::new("Settings")
        .collapsible(false)
        .resizable(false)
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_width(680.0);

            // Tab strip
            ui.horizontal(|ui| {
                for tab in SettingsTab::ALL {
                    let selected = st.tab == tab;
                    if ui
                        .add_sized(
                            [104.0, 28.0],
                            egui::Button::selectable(selected, tab.title()),
                        )
                        .clicked()
                    {
                        st.tab = tab;
                    }
                }
            });
            ui.separator();

            ScrollArea::vertical()
                .id_salt("settings_body")
                .max_height(400.0)
                .auto_shrink([false, false])
                .show(ui, |ui| match st.tab {
                    SettingsTab::Agents => agents_tab(ui, st, pal),
                    SettingsTab::Providers => providers_tab(ui, st, pal),
                    SettingsTab::Pipeline => pipeline_tab(ui, st, pal),
                    SettingsTab::Appearance => appearance_tab(ui, st, saved_theme, pal),
                    SettingsTab::Account => account_tab(ui, st, codex_signed_in, pal, actions),
                });

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    actions.push(Action::CloseOverlay);
                }
                hint(ui, pal, "Save applies to the active project immediately.");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if primary_button(ui, pal, "Save settings").clicked() {
                        actions.push(st.save_action());
                    }
                });
            });
        });
}

/// Draw every row a group declares, in declared order.
///
/// The form is *generated* from `settings_defs::ORDER` rather than written out
/// a second time. The Pipeline tab once silently omitted six settings that
/// `save_action` still wrote, so a GUI-only user could neither see nor change
/// them; a row cannot go missing here now without being removed from the
/// declarations both front ends read.
fn declared_rows(
    ui: &mut egui::Ui,
    st: &mut SettingsState,
    group: Group,
    pal: &GuiPalette,
    custom: &[SField],
) {
    egui::Grid::new(format!("settings_grid_{group:?}"))
        .num_columns(2)
        .spacing([16.0, 10.0])
        .show(ui, |ui| {
            for i in group.fields() {
                let Some(d) = settings_defs::at(i) else {
                    continue;
                };
                if custom.contains(&d.field) {
                    continue;
                }
                let disabled = st.settings_disabled(d.field);
                ui.label(RichText::new(d.label).color(pal.ink));
                ui.add_enabled_ui(!disabled, |ui| {
                    ui.vertical(|ui| {
                        control(ui, st, d, pal);
                        hint(ui, pal, d.help);
                    });
                });
                ui.end_row();
            }
        });
}

/// One row's control, chosen by the `Kind` the row was declared with.
fn control(ui: &mut egui::Ui, st: &mut SettingsState, d: &settings_defs::Def, pal: &GuiPalette) {
    let id = format!("set_{:?}", d.field);
    // A Codex model row is picked from the signed-in account's list rather
    // than typed, so it is a choice row whatever its declared kind says.
    let as_choice = matches!(d.kind, Kind::Select) || st.is_codex_model_of(d.field);

    if as_choice {
        let options = st.select_domain(d.field);
        if options.is_empty() {
            ui.label(RichText::new("unavailable").color(pal.ink_faint).italics());
            return;
        }
        let current = st.select_index(d.field);
        let mut picked = current;
        ComboBox::from_id_salt(id)
            .selected_text(st.settings_select_label(d.field))
            .width(240.0)
            .show_ui(ui, |ui| {
                for (i, o) in options.iter().enumerate() {
                    ui.selectable_value(&mut picked, i, o);
                }
            });
        if picked != current {
            st.set_select(d.field, picked);
        }
        return;
    }

    match d.kind {
        Kind::Toggle => {
            // `cycle_field` is the one write path for a choice row, so the
            // checkbox asks it to advance rather than assigning the flag.
            let mut on = st.settings_toggle(d.field);
            if ui.checkbox(&mut on, "").changed() {
                st.cycle_field(d.field, true);
            }
        }
        Kind::Secret => {
            let (_, from_env) = st.settings_secret(d.field);
            if let Some(buf) = st.text_field_mut_of(d.field) {
                secret_edit(ui, pal, buf, from_env);
            }
        }
        Kind::Number { .. } => {
            if let Some(buf) = st.text_field_mut_of(d.field) {
                numeric_edit(ui, buf, 90.0);
            }
        }
        Kind::Text | Kind::Select => {
            if let Some(buf) = st.text_field_mut_of(d.field) {
                ui.add(TextEdit::singleline(buf).desired_width(240.0));
            }
        }
    }
}

fn agents_tab(ui: &mut egui::Ui, st: &mut SettingsState, pal: &GuiPalette) {
    section(ui, pal, "Agents — provider · model · reasoning effort");
    hint(
        ui,
        pal,
        "Each pipeline agent picks its own provider and model. Effort is sent as the request's reasoning parameter when set.",
    );
    ui.add_space(6.0);
    declared_rows(ui, st, Group::Agents, pal, &[]);
}

fn providers_tab(ui: &mut egui::Ui, st: &mut SettingsState, pal: &GuiPalette) {
    section(ui, pal, "Provider credentials");
    hint(
        ui,
        pal,
        "Keys are stored in ~/.config/honya/config.json (mode 0600). Environment variables always win over saved keys.",
    );
    ui.add_space(6.0);
    declared_rows(ui, st, Group::Providers, pal, &[]);
}

fn pipeline_tab(ui: &mut egui::Ui, st: &mut SettingsState, pal: &GuiPalette) {
    section(ui, pal, "Pipeline");
    hint(
        ui,
        pal,
        "How a run is shaped: retries, chunking, and the System One judgements. With the master switch off, every judgement below it is inert.",
    );
    ui.add_space(6.0);
    declared_rows(ui, st, Group::Pipeline, pal, &[]);
}

/// Theme is the one row drawn by hand: a list of swatches says more about a
/// theme than its name does, and that is worth a custom control.
const CUSTOM_APPEARANCE_ROWS: [SField; 1] = [SField::Theme];

fn appearance_tab(
    ui: &mut egui::Ui,
    st: &mut SettingsState,
    saved_theme: ThemeId,
    pal: &GuiPalette,
) {
    section(ui, pal, "Updates");
    ui.add_space(4.0);
    declared_rows(ui, st, Group::Appearance, pal, &CUSTOM_APPEARANCE_ROWS);

    ui.add_space(10.0);
    section(ui, pal, "Theme");
    // One rule for a theme change, whichever surface makes it: the choice
    // previews and the form commits it. This tab used to save on the click,
    // so the same operation was final in one place and cancellable in another.
    hint(
        ui,
        pal,
        if st.theme == saved_theme {
            "Previewed as you pick; saved with the form."
        } else {
            "Previewing — Save settings to keep it, Cancel to revert."
        },
    );
    ui.add_space(4.0);
    for &id in ALL_THEMES {
        ui.horizontal(|ui| {
            theme_swatch(ui, id);
            let label = format!("{}  ·  {}", id.label(), id.tone());
            if ui.selectable_label(st.theme == id, label).clicked() {
                st.theme = id;
            }
        });
    }
}

fn account_tab(
    ui: &mut egui::Ui,
    st: &mut SettingsState,
    codex_signed_in: bool,
    pal: &GuiPalette,
    actions: &mut Vec<Action>,
) {
    section(ui, pal, "GitHub account · web remote control");
    hint(
        ui,
        pal,
        "Link this app to your GitHub account to monitor and control runs from the web dashboard.",
    );
    ui.add_space(6.0);

    match st.account_login.clone() {
        Some(login) => {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("Signed in as {login}"))
                        .color(pal.status_done)
                        .strong(),
                );
                if ui.button("Sign out").clicked() {
                    actions.push(Action::RemoteLogout);
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let state_color = match st.remote_state {
                    RemoteState::Connected => pal.status_done,
                    RemoteState::Error => pal.status_failed,
                    RemoteState::Disconnected => pal.ink_faint,
                    _ => pal.status_working,
                };
                ui.label(RichText::new(format!("relay: {}", st.remote_state.label())).color(state_color));
                if st.remote_watchers > 0 {
                    ui.label(
                        RichText::new(format!("⇄ {} watching", st.remote_watchers))
                            .color(pal.accent),
                    );
                }
                if let Some(label) = &st.session_label {
                    ui.label(RichText::new(label.as_str()).color(pal.ink_faint).small());
                }
            });
            ui.add_space(4.0);
            if st.remote_enabled {
                if ui.button("Disconnect remote").clicked() {
                    actions.push(Action::DisableRemote);
                }
            } else if ui.button("Connect remote").clicked() {
                actions.push(Action::EnableRemote);
            }
        }
        None => {
            if ui.button("Sign in with GitHub…").clicked() {
                actions.push(Action::StartRemoteLogin);
            }
        }
    }

    if let Some(prompt) = st.remote_auth_code.clone() {
        ui.add_space(8.0);
        egui::Frame::NONE
            .fill(pal.bg_inset)
            .stroke(egui::Stroke::new(1.0_f32, pal.accent))
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.label(RichText::new("Enter this code on GitHub:").color(pal.ink_soft));
                ui.label(
                    RichText::new(&prompt.code)
                        .color(pal.accent)
                        .monospace()
                        .size(22.0),
                );
                ui.horizontal(|ui| {
                    if ui.button("Open github.com/login/device").clicked() {
                        actions.push(Action::OpenAuthUrl);
                    }
                    if ui.button("Copy code").clicked() {
                        ui.ctx().copy_text(prompt.code.clone());
                    }
                });
            });
    }

    ui.add_space(12.0);
    section(ui, pal, "Codex — Sign in with ChatGPT");
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let status = if codex_signed_in {
            RichText::new("Signed in").color(pal.status_done)
        } else {
            RichText::new("Signed out").color(pal.ink_faint)
        };
        ui.label(status);
        let label = if codex_signed_in {
            "Sign out of ChatGPT"
        } else {
            "Sign in with ChatGPT…"
        };
        if ui.button(label).clicked() {
            actions.push(Action::ToggleCodexSignIn);
        }
    });
    hint(
        ui,
        pal,
        "PKCE OAuth in your browser; ~/.codex/auth.json is imported automatically when present.",
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::settings_defs::{Kind, ORDER};

    /// The Pipeline tab once silently omitted six settings that `save_action`
    /// still wrote, so a GUI-only user could neither see nor change them.
    ///
    /// The old guard against that was a substring grep over this file's own
    /// source text, which covered one group of five and passed on a label that
    /// appeared only in a comment. Now the form is generated, so the thing
    /// worth asserting is that every declared row is *renderable*: a choice row
    /// has choices to offer, and an editable row has a buffer to edit.
    #[test]
    fn every_declared_row_can_be_rendered_by_the_generic_form() {
        let mut st = SettingsState::for_test(0);
        // The Codex model lists are empty until a signed-in account supplies
        // them, and a row with nothing to pick is drawn as unavailable.
        st.codex_models = vec!["gpt-5-codex".to_string()];

        for d in ORDER {
            if d.group == Group::Account {
                continue; // actions only: sign in, sign out, remote toggle
            }
            let field = d.field;
            if matches!(d.kind, Kind::Select) || st.is_codex_model_of(field) {
                let options = st.select_domain(field);
                assert!(
                    !options.is_empty(),
                    "{:?} is a choice row with no choices",
                    field
                );
                assert!(
                    options.contains(&st.settings_select_label(field)),
                    "{:?} shows {:?}, which is not one of {:?}",
                    field,
                    st.settings_select_label(field),
                    options
                );
                continue;
            }
            if matches!(d.kind, Kind::Toggle) {
                continue; // driven by cycle_field, no buffer needed
            }
            assert!(
                st.text_field_mut_of(field).is_some(),
                "{:?} is an editable row with no buffer behind it",
                field
            );
        }
    }

    /// Theme is drawn by hand because swatches say more than a name does.
    /// Anything else claiming a custom control has to say so here.
    #[test]
    fn theme_is_the_only_row_the_appearance_tab_draws_itself() {
        assert_eq!(CUSTOM_APPEARANCE_ROWS, [SField::Theme]);
    }

    /// Picking an option puts that option on the row, for every choice row.
    /// `set_select` walks the cycle rather than assigning, so this also pins
    /// that the cycle reaches every member of the domain it advertises.
    #[test]
    fn choosing_an_option_selects_it() {
        for d in ORDER {
            if !matches!(d.kind, Kind::Select) {
                continue;
            }
            let mut st = SettingsState::for_test(0);
            let options = st.select_domain(d.field);
            for (i, expected) in options.iter().enumerate() {
                st.set_select(d.field, i);
                assert_eq!(
                    &st.settings_select_label(d.field),
                    expected,
                    "{:?} could not be set to option {i}",
                    d.field
                );
            }
        }
    }
}
