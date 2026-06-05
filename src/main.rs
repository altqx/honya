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
mod llm;
mod model;
mod theme;
mod ui;
mod workspace;

#[cfg(test)]
mod tests_e2e;

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{Event, EventStream, KeyEventKind};

use crate::app::App;
use crate::llm::client::LlmClient;
use crate::model::{AppConfig, AppEvent, EventTx};

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> anyhow::Result<()> {
    let cfg = config::load();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    let mut app = App::new(EventTx(tx), cfg);

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app, rx).await;
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

/// Pick the LLM backend: the real OpenRouter client when an API key is present,
/// otherwise the offline MockClient so the TUI is always fully usable.
pub fn build_client(cfg: &AppConfig) -> Arc<dyn LlmClient> {
    if config::api_key().is_some() {
        if let Ok(client) = llm::client::OpenRouterClient::from_env() {
            return Arc::new(client);
        }
        // Never write to stderr here — the TUI owns the alternate screen. Silently
        // fall back to the mock client; the user sees mock behavior regardless.
    }
    let _ = cfg;
    Arc::new(llm::mock::MockClient::default())
}
