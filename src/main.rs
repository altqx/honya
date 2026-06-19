//! honya 本屋 — a Ratatui TUI for AI-assisted Japanese→Thai light-novel translation.
//! Owns config load, terminal lifecycle, and async event fan-in.

// Needed for the refine agent's large `json!` tool schema.
#![recursion_limit = "256"]

mod agents;
mod app;
mod cleanse;
mod codex;
mod config;
mod document_import;
mod epub;
mod error;
mod export;
mod llm;
mod model;
mod remote;
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
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::supports_keyboard_enhancement;

use crate::app::App;
use crate::llm::client::LlmClient;
use crate::model::{AppConfig, AppEvent, EventTx};

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> anyhow::Result<()> {
    // Reap a stale Windows self-update sidecar; no-op elsewhere.
    update::cleanup_stale_old_exe();

    // Subcommands run before the TUI: update/version/help must not require an API key.
    match std::env::args().nth(1).as_deref() {
        Some("update" | "self-update" | "upgrade") => {
            return update::run_self_update(config::load().release_channel).await;
        }
        Some("--version" | "-V" | "version") => {
            println!("honya {}", update::version_string());
            return Ok(());
        }
        Some("--help" | "-h" | "help") => {
            print_help();
            return Ok(());
        }
        _ => {}
    }

    let mut cfg = config::load();
    if cfg.codex_auth.is_none()
        && let Some(auth) = codex::import_codex_cli_auth()
    {
        cfg.codex_auth = Some(auth);
        let _ = config::save(&cfg);
    }
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    let etx = EventTx(tx);
    if let Some(auth) = cfg.codex_auth.clone() {
        codex::models::spawn_fetch_models(auth, etx.clone());
    }
    let mut app = App::new(etx, cfg);

    app.init_recovery_prompt();

    app.init_onboarding();

    update::spawn_background_update(app.tx.clone(), app.cfg.update_mode, app.cfg.release_channel);

    // Remote startup is opt-in so local sessions do not auto-collide.
    let remote_opt_in =
        std::env::args().any(|a| a == "--remote") || config::env_truthy("HONYA_REMOTE");
    app.init_remote(remote_opt_in);

    // Restore the terminal before panic output; normal teardown does not run on panic.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags, DisableMouseCapture);
        ratatui::restore();
        prev_hook(info);
    }));

    let mut terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    // Enables distinct Ctrl+Tab codes where supported; release events are filtered out.
    let kbd_enhanced = supports_keyboard_enhancement().unwrap_or(false);
    if kbd_enhanced {
        let _ = execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    let result = run(&mut terminal, &mut app, rx).await;
    if kbd_enhanced {
        let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();

    if let Err(err) = &result {
        eprintln!("honya: fatal error: {err:?}");
    }
    result
}

async fn run(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) -> anyhow::Result<()> {
    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut full = true;
    while app.running {
        present(terminal, app, full)?;

        tokio::select! {
            _ = ticker.tick() => {
                app.frame = app.frame.wrapping_add(1);
                full = false;
            }
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => app.on_key(key),
                    Some(Ok(Event::Mouse(me))) => app.on_mouse(me),
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => {}
                }
                full = true;
            }
            maybe_app = rx.recv() => {
                if let Some(ev) = maybe_app {
                    app.on_app_event(ev);
                }
                full = true;
            }
        }
    }
    Ok(())
}

fn present(terminal: &mut DefaultTerminal, app: &mut App, full: bool) -> anyhow::Result<()> {
    use ratatui::backend::Backend;
    use ratatui::buffer::{Buffer, Cell};

    terminal.autoresize()?;
    {
        let mut frame = terminal.get_frame();
        app.render(&mut frame);
    }
    if full {
        let buf = terminal.current_buffer_mut().clone();
        let sentinel = Buffer::filled(buf.area, Cell::new("\u{0}"));
        terminal
            .backend_mut()
            .draw(sentinel.diff(&buf).into_iter())?;
    } else {
        terminal.flush()?;
    }
    terminal.hide_cursor()?;
    terminal.swap_buffers();
    terminal.backend_mut().flush()?;
    Ok(())
}

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

pub fn build_clients(cfg: &AppConfig) -> anyhow::Result<llm::ClientSet> {
    Ok(llm::ClientSet::build(cfg)?)
}

fn print_help() {
    println!("honya 本屋 — AI-assisted Japanese → Thai light-novel translation\n");
    println!("USAGE:");
    println!("    honya             Launch the TUI in the current directory");
    println!("    honya --remote    Launch and enable web remote control for this session");
    println!(
        "    honya update      Update honya to the latest release (aliases: self-update, upgrade)"
    );
    println!(
        "                      On the dev channel (Settings), builds the latest git commit from source"
    );
    println!("    honya --version   Print the version");
    println!("    honya --help      Show this help\n");
    println!("FIRST RUN:");
    println!("    No API key? Just launch — the in-app Welcome guides you through setup,");
    println!("    or try the bundled sample project to explore offline.\n");
    println!("ENVIRONMENT:");
    println!("    HONYA_API_KEY / OPENROUTER_API_KEY   OpenRouter key (overrides saved config)");
    println!(
        "    HONYA_TOKENROUTER_API_KEY / TOKENROUTER_API_KEY   Tokenrouter key (overrides saved config)"
    );
    println!(
        "    HONYA_NO_UPDATE_CHECK                Disable the startup update check / auto-update"
    );
    println!(
        "    HONYA_REMOTE                         Enable web remote control for this session (= --remote)"
    );
    println!(
        "    HONYA_SESSION_FILE                   Override the crash-recovery checkpoint path"
    );
}

#[cfg(test)]
mod present_tests {
    use ratatui::buffer::{Buffer, Cell};
    use ratatui::layout::Rect;

    // Full repaint clears stale cells except a wide grapheme's trailing cell.
    #[test]
    fn full_repaint_covers_all_cells_but_wide_char_trailing() {
        let area = Rect::new(0, 0, 6, 1);
        let mut buf = Buffer::filled(area, Cell::new(" "));
        buf[(0, 0)].set_symbol("A"); // narrow
        buf[(1, 0)].set_symbol("の"); // wide: occupies cols 1-2
        // Col 3 is the wide trailing cell; cols 4-5 model stale ghosts.

        let sentinel = Buffer::filled(area, Cell::new("\u{0}"));
        let cols: Vec<u16> = sentinel.diff(&buf).into_iter().map(|(x, _, _)| x).collect();

        assert!(cols.contains(&0), "narrow cell must be emitted");
        assert!(cols.contains(&1), "wide char anchor must be emitted");
        assert!(
            !cols.contains(&2),
            "wide char trailing cell must be skipped"
        );
        assert!(
            cols.contains(&4),
            "blank cell must be emitted to clear ghosts"
        );
        assert!(
            cols.contains(&5),
            "blank cell must be emitted to clear ghosts"
        );
    }
}
