//! Native Settings dialog — the full TUI Settings surface (agents, provider
//! keys, pipeline limits, appearance, account) rendered with desktop widgets.
//!
//! Edits mutate the live [`SettingsState`] working copy in place (same as the
//! TUI); Save routes the whole copy through `SettingsState::save_action()` and
//! the usual `apply()` funnel.

use egui::{Align, ComboBox, Context, Layout, RichText, ScrollArea, TextEdit};

use crate::app::Action;
use crate::app::overlay::{SettingsState, SettingsTab};
use crate::model::{Effort, Provider, ServiceTier, TargetLanguage, ThemeId};
use crate::remote::protocol::RemoteState;
use crate::theme::ALL_THEMES;

use super::theme_map::GuiPalette;
use super::widgets::{hint, numeric_edit, primary_button, secret_edit, section, theme_swatch};

const PROVIDERS: [Provider; 5] = [
    Provider::OpenRouter,
    Provider::Tokenrouter,
    Provider::Google,
    Provider::Cloudflare,
    Provider::Codex,
];

const EFFORTS: [Option<Effort>; 6] = [
    None,
    Some(Effort::Minimal),
    Some(Effort::Low),
    Some(Effort::Medium),
    Some(Effort::High),
    Some(Effort::Xhigh),
];

const TIERS: [Option<ServiceTier>; 3] = [None, Some(ServiceTier::Flex), Some(ServiceTier::Priority)];

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
                    SettingsTab::Appearance => appearance_tab(ui, st, saved_theme, pal, actions),
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

fn agents_tab(ui: &mut egui::Ui, st: &mut SettingsState, pal: &GuiPalette) {
    section(ui, pal, "Agents — provider · model · reasoning effort");
    hint(
        ui,
        pal,
        "Each pipeline agent picks its own provider and model. Effort is sent as the request's reasoning parameter when set.",
    );
    ui.add_space(6.0);

    let codex_models = st.codex_models.clone();
    for (i, label) in [
        (0, "◆ Orchestrator"),
        (1, "▲ Translator"),
        (2, "■ Reviewer"),
        (3, "◇ Refine"),
    ] {
        let provider = agent(st, i).provider;
        ui.label(RichText::new(label).color(pal.ink).strong());
        ui.horizontal(|ui| {
            let mut next = provider;
            ComboBox::from_id_salt(format!("prov_{i}"))
                .selected_text(provider.label())
                .width(130.0)
                .show_ui(ui, |ui| {
                    for p in PROVIDERS {
                        ui.selectable_value(&mut next, p, p.label());
                    }
                });
            if next != provider {
                st.switch_agent_provider(i, next);
            }

            let a = agent(st, i);
            if a.provider == Provider::Codex {
                let mut model = a.model.clone();
                ComboBox::from_id_salt(format!("model_{i}"))
                    .selected_text(model.clone())
                    .width(240.0)
                    .show_ui(ui, |ui| {
                        for m in &codex_models {
                            ui.selectable_value(&mut model, m.clone(), m);
                        }
                    });
                if model != a.model {
                    a.set_model(model);
                }
            } else {
                let mut model = a.model.clone();
                let resp = ui.add(TextEdit::singleline(&mut model).desired_width(240.0));
                if resp.changed() {
                    a.set_model(model);
                }
            }

            let a = agent(st, i);
            ComboBox::from_id_salt(format!("effort_{i}"))
                .selected_text(Effort::label(a.effort))
                .width(90.0)
                .show_ui(ui, |ui| {
                    for e in EFFORTS {
                        ui.selectable_value(&mut a.effort, e, Effort::label(e));
                    }
                });
        });
        ui.add_space(6.0);
    }
}

fn agent(st: &mut SettingsState, i: usize) -> &mut crate::model::AgentModel {
    match i {
        0 => &mut st.models.orchestrator,
        1 => &mut st.models.translator,
        2 => &mut st.models.reviewer,
        _ => &mut st.models.refine,
    }
}

fn providers_tab(ui: &mut egui::Ui, st: &mut SettingsState, pal: &GuiPalette) {
    section(ui, pal, "Provider credentials");
    hint(
        ui,
        pal,
        "Keys are stored in ~/.config/honya/config.json (mode 0600). Environment variables always win over saved keys.",
    );
    ui.add_space(6.0);

    ui.label(RichText::new("OpenRouter API key").color(pal.ink).strong());
    secret_edit(ui, pal, &mut st.openrouter_key, st.api_key_env);
    hint(ui, pal, "HONYA_API_KEY / OPENROUTER_API_KEY");
    ui.add_space(8.0);

    ui.label(RichText::new("Tokenrouter API key").color(pal.ink).strong());
    secret_edit(ui, pal, &mut st.tokenrouter_key, st.tokenrouter_key_env);
    hint(ui, pal, "HONYA_TOKENROUTER_API_KEY / TOKENROUTER_API_KEY");
    ui.add_space(8.0);

    ui.label(RichText::new("Google API key").color(pal.ink).strong());
    secret_edit(ui, pal, &mut st.google_key, st.google_key_env);
    ui.add_space(8.0);

    ui.label(
        RichText::new("Cloudflare account ID")
            .color(pal.ink)
            .strong(),
    );
    if st.cloudflare_account_id_env {
        hint(ui, pal, "set by environment variable (read-only)");
    } else {
        ui.add(TextEdit::singleline(&mut st.cloudflare_account_id).desired_width(f32::INFINITY));
    }
    ui.add_space(8.0);

    ui.label(
        RichText::new("Cloudflare API token")
            .color(pal.ink)
            .strong(),
    );
    secret_edit(
        ui,
        pal,
        &mut st.cloudflare_api_token,
        st.cloudflare_api_token_env,
    );
    ui.add_space(8.0);

    section(ui, pal, "Codex (ChatGPT)");
    hint(
        ui,
        pal,
        "Codex signs in with ChatGPT instead of a key — see the Account tab.",
    );
}

fn pipeline_tab(ui: &mut egui::Ui, st: &mut SettingsState, pal: &GuiPalette) {
    section(ui, pal, "Pipeline");
    ui.add_space(6.0);

    egui::Grid::new("pipeline_grid")
        .num_columns(2)
        .spacing([16.0, 10.0])
        .show(ui, |ui| {
            ui.label(RichText::new("New-project language").color(pal.ink));
            ComboBox::from_id_salt("pref_lang")
                .selected_text(st.preferred_language.label())
                .width(140.0)
                .show_ui(ui, |ui| {
                    for l in [TargetLanguage::Thai, TargetLanguage::English] {
                        ui.selectable_value(&mut st.preferred_language, l, l.label());
                    }
                });
            ui.end_row();

            ui.label(RichText::new("Max retry attempts / chunk").color(pal.ink));
            numeric_edit(ui, &mut st.max_attempts, 80.0);
            ui.end_row();

            ui.label(RichText::new("Continuity sentences").color(pal.ink));
            numeric_edit(ui, &mut st.continuity_sentences, 80.0);
            ui.end_row();

            ui.label(RichText::new("Loop watchdog stall (s)").color(pal.ink));
            numeric_edit(ui, &mut st.loop_stall_secs, 80.0);
            ui.end_row();

            ui.label(RichText::new("Max chapter re-translates").color(pal.ink));
            numeric_edit(ui, &mut st.max_chapter_retranslates, 80.0);
            ui.end_row();

            ui.label(RichText::new("Service tier").color(pal.ink));
            ComboBox::from_id_salt("tier")
                .selected_text(ServiceTier::label(st.service_tier))
                .width(140.0)
                .show_ui(ui, |ui| {
                    for t in TIERS {
                        ui.selectable_value(&mut st.service_tier, t, ServiceTier::label(t));
                    }
                });
            ui.end_row();

            ui.label(RichText::new("Parallel lookahead").color(pal.ink));
            ui.checkbox(&mut st.parallel_lookahead, "speculative next-chunk draft");
            ui.end_row();
        });

    ui.add_space(4.0);
    hint(ui, pal, ServiceTier::desc(st.service_tier));
    hint(
        ui,
        pal,
        "Retries cap at 20 · continuity at 100 sentences · stall at 3600 s (0 disables) · re-translates at 10.",
    );
}

fn appearance_tab(
    ui: &mut egui::Ui,
    st: &mut SettingsState,
    saved_theme: ThemeId,
    pal: &GuiPalette,
    actions: &mut Vec<Action>,
) {
    section(ui, pal, "Updates");
    ui.add_space(4.0);
    egui::Grid::new("appearance_grid")
        .num_columns(2)
        .spacing([16.0, 10.0])
        .show(ui, |ui| {
            ui.label(RichText::new("Update install").color(pal.ink));
            ComboBox::from_id_salt("update_mode")
                .selected_text(st.update_mode.label())
                .width(160.0)
                .show_ui(ui, |ui| {
                    for m in [
                        crate::model::UpdateMode::Auto,
                        crate::model::UpdateMode::Notify,
                    ] {
                        ui.selectable_value(&mut st.update_mode, m, m.label());
                    }
                });
            ui.end_row();

            ui.label(RichText::new("Release channel").color(pal.ink));
            ComboBox::from_id_salt("channel")
                .selected_text(st.release_channel.label())
                .width(160.0)
                .show_ui(ui, |ui| {
                    for c in [
                        crate::model::ReleaseChannel::Stable,
                        crate::model::ReleaseChannel::Dev,
                    ] {
                        ui.selectable_value(&mut st.release_channel, c, c.label());
                    }
                });
            ui.end_row();
        });

    ui.add_space(10.0);
    section(ui, pal, "Theme");
    hint(ui, pal, "Applied and saved immediately.");
    ui.add_space(4.0);
    for &id in ALL_THEMES {
        ui.horizontal(|ui| {
            theme_swatch(ui, id);
            let label = format!("{}  ·  {}", id.label(), id.tone());
            if ui.selectable_label(saved_theme == id, label).clicked() {
                actions.push(Action::SaveTheme(id));
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
