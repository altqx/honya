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
mod update;
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
    // Subcommands run before the key prompt: update/version must not require an API key.
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

    let mut cfg = config::load();
    if let Err(e) = ensure_api_key(&mut cfg) {
        eprintln!("honya: {e}");
        std::process::exit(1);
    }
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    let mut app = App::new(EventTx(tx), cfg);

    // Offer to resume a run that a crash / power loss left interrupted (raises the
    // recovery overlay over the Shelf when a resumable checkpoint is found).
    app.init_recovery_prompt();

    // Best-effort background update notification; never blocks startup.
    {
        let tx = app.tx.clone();
        tokio::spawn(async move {
            if let Some(version) = update::check_for_update().await {
                tx.send(AppEvent::UpdateAvailable { version });
            }
        });
    }

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
    println!("ENVIRONMENT:");
    println!("    HONYA_API_KEY / OPENROUTER_API_KEY   OpenRouter key (overrides saved config)");
    println!("    HONYA_NO_UPDATE_CHECK                Disable the startup update check");
    println!(
        "    HONYA_SESSION_FILE                   Override the crash-recovery checkpoint path"
    );
}

/// Ensure an OpenRouter API key exists (env → persisted config); else prompt and save it.
fn ensure_api_key(cfg: &mut AppConfig) -> anyhow::Result<()> {
    if config::resolve_api_key(cfg).is_some() {
        return Ok(());
    }
    let key = prompt_api_key()?;
    cfg.api_key = Some(key);
    match config::save(cfg) {
        Ok(()) => eprintln!(
            "honya: saved your key to {}",
            config::config_path().display()
        ),
        Err(e) => eprintln!(
            "honya: warning: could not save config ({e}); you'll be asked again next launch."
        ),
    }
    Ok(())
}

/// Print a short banner and read the OpenRouter key from the terminal.
fn prompt_api_key() -> anyhow::Result<String> {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out)?;
    writeln!(
        out,
        "  本屋 honya — AI-assisted Japanese → Thai translation"
    )?;
    writeln!(out)?;
    writeln!(out, "  honya needs an OpenRouter API key to run.")?;
    writeln!(
        out,
        "  Get one at https://openrouter.ai/keys (it looks like sk-or-v1-…)."
    )?;
    writeln!(
        out,
        "  Tip: export HONYA_API_KEY or OPENROUTER_API_KEY to skip this prompt."
    )?;
    writeln!(out)?;
    let key = read_secret(&mut out, "  Paste your OpenRouter API key: ")?;
    let key = key.trim().to_string();
    if key.is_empty() {
        anyhow::bail!("no API key entered");
    }
    Ok(key)
}

/// Read a line without echoing it (masked with •) using crossterm raw mode.
/// Falls back to a plain echoed read when raw mode is unavailable (no TTY).
fn read_secret(out: &mut impl std::io::Write, prompt: &str) -> anyhow::Result<String> {
    use ratatui::crossterm::event::{self, KeyCode, KeyModifiers};
    use ratatui::crossterm::terminal;

    write!(out, "{prompt}")?;
    out.flush()?;

    if terminal::enable_raw_mode().is_ok() {
        let mut buf = String::new();
        let res = loop {
            match event::read() {
                Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => match k.code {
                    KeyCode::Enter => break Ok(buf),
                    KeyCode::Esc => break Err(anyhow::anyhow!("cancelled")),
                    KeyCode::Char('c' | 'd') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                        break Err(anyhow::anyhow!("cancelled"));
                    }
                    KeyCode::Backspace if buf.pop().is_some() => {
                        let _ = write!(out, "\u{8} \u{8}");
                        let _ = out.flush();
                    }
                    KeyCode::Char(c) => {
                        buf.push(c);
                        let _ = write!(out, "•");
                        let _ = out.flush();
                    }
                    _ => {}
                },
                Ok(_) => {}
                Err(e) => break Err(e.into()),
            }
        };
        let _ = terminal::disable_raw_mode();
        let _ = writeln!(out);
        return res;
    }

    // Fallback: plain echoed read for non-interactive terminals.
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line)? == 0 {
        anyhow::bail!("stdin closed before a key was entered");
    }
    Ok(line)
}
