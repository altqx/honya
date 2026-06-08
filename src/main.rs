//! honya 本屋 — a Ratatui TUI for AI-assisted Japanese→Thai light-novel translation.
//!
//! Entry point: load config, build the App, init the terminal, and run an async
//! `select!` event loop that fans in (a) an animation tick, (b) terminal input via
//! crossterm's EventStream, and (c) background pipeline/import events over an mpsc
//! channel. The terminal is always restored before any error is printed.

mod agents;
mod app;
mod cleanse;
mod config;
mod epub;
mod error;
mod export;
mod llm;
mod model;
mod theme;
mod ui;
mod update;
mod workspace;

#[cfg(test)]
mod tests_e2e;

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyEventKind,
};
use ratatui::crossterm::execute;

use crate::app::App;
use crate::llm::client::LlmClient;
use crate::model::{AppConfig, AppEvent, EventTx};

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> anyhow::Result<()> {
    // Subcommands run before the TUI: update/version/help must not require an API key.
    match std::env::args().nth(1).as_deref() {
        Some("update" | "self-update" | "upgrade") => return update::run_self_update().await,
        Some("--version" | "-V" | "version") => {
            println!("honya {}", update::current_version());
            return Ok(());
        }
        Some("--help" | "-h" | "help") => {
            print_help();
            return Ok(());
        }
        _ => {}
    }

    let cfg = config::load();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    let mut app = App::new(EventTx(tx), cfg);

    // Offer to resume a run that a crash / power loss left interrupted (raises the
    // recovery overlay over the Shelf when a resumable checkpoint is found).
    app.init_recovery_prompt();

    // First-run onboarding happens in-app (not via a pre-TUI prompt): the Welcome
    // overlay guides setting an API key, creating the sample, or importing. It
    // defers to a pending recovery prompt and is skipped for returning users.
    app.init_onboarding();

    // Best-effort background update handling; never blocks startup. Auto mode
    // (the default) downloads + installs the latest release in place and flags a
    // restart; Notify mode only surfaces the "honya update" hint. Both honor
    // HONYA_NO_UPDATE_CHECK (it makes the check a no-op).
    {
        let tx = app.tx.clone();
        let mode = app.cfg.update_mode;
        tokio::spawn(async move {
            use crate::model::UpdateMode;
            match mode {
                UpdateMode::Auto => match update::auto_update().await {
                    update::AutoUpdate::Installed(version) => {
                        tx.send(AppEvent::UpdateInstalled { version });
                    }
                    // Found but couldn't install (e.g. read-only install dir);
                    // fall back to notifying so `honya update` is still offered.
                    update::AutoUpdate::Available(version) => {
                        tx.send(AppEvent::UpdateAvailable { version });
                    }
                    update::AutoUpdate::UpToDate => {}
                },
                UpdateMode::Notify => {
                    if let Some(version) = update::check_for_update().await {
                        tx.send(AppEvent::UpdateAvailable { version });
                    }
                }
            }
        });
    }

    let mut terminal = ratatui::init();
    // Mouse reporting is opt-in; enable it so the TUI is fully click/scroll
    // driven. Best-effort — a terminal that rejects it just stays keyboard-only.
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let result = run(&mut terminal, &mut app, rx).await;
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();

    if let Err(err) = &result {
        eprintln!("honya: fatal error: {err:?}");
    }
    result
}

/// The render-then-await loop. Returns when `app.running` goes false.
async fn run(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) -> anyhow::Result<()> {
    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    while app.running {
        terminal.draw(|frame| app.render(frame))?;

        tokio::select! {
            _ = ticker.tick() => {
                app.frame = app.frame.wrapping_add(1);
            }
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => app.on_key(key),
                    Some(Ok(Event::Mouse(me))) => app.on_mouse(me),
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => {}
                }
            }
            maybe_app = rx.recv() => {
                if let Some(ev) = maybe_app {
                    app.on_app_event(ev);
                }
            }
        }
    }
    Ok(())
}

/// Build the live OpenRouter client; errors only if the key is gone or the HTTP stack fails.
pub fn build_client(cfg: &AppConfig) -> anyhow::Result<Arc<dyn LlmClient>> {
    let api_key = config::resolve_api_key(cfg).ok_or_else(|| {
        anyhow::anyhow!(
            "no OpenRouter API key configured (set HONYA_API_KEY or OPENROUTER_API_KEY)"
        )
    })?;
    let client = llm::client::OpenRouterClient::new(llm::client::ClientConfig::from_app_config(
        cfg, api_key,
    ))?;
    Ok(Arc::new(client))
}

/// Print CLI usage for `honya --help`.
fn print_help() {
    println!("honya 本屋 — AI-assisted Japanese → Thai light-novel translation\n");
    println!("USAGE:");
    println!("    honya             Launch the TUI in the current directory");
    println!(
        "    honya update      Update honya to the latest release (aliases: self-update, upgrade)"
    );
    println!("    honya --version   Print the version");
    println!("    honya --help      Show this help\n");
    println!("FIRST RUN:");
    println!("    No API key? Just launch — the in-app Welcome guides you through setup,");
    println!("    or try the bundled sample project to explore offline.\n");
    println!("ENVIRONMENT:");
    println!("    HONYA_API_KEY / OPENROUTER_API_KEY   OpenRouter key (overrides saved config)");
    println!(
        "    HONYA_NO_UPDATE_CHECK                Disable the startup update check / auto-update"
    );
    println!(
        "    HONYA_SESSION_FILE                   Override the crash-recovery checkpoint path"
    );
}
