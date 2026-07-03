//! Persistent WebSocket link to the Cloudflare relay.
//!
//! The task pushes app snapshots/deltas out, forwards browser commands back as
//! [`AppEvent`]s, and reconnects with capped backoff until remote is disabled.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_tungstenite::tungstenite::Message;

use super::{session_ws_url, user_agent};
use crate::model::{AppEvent, EventTx, LogLevel};
use crate::remote::protocol::{Inbound, RemoteOutbound, RemoteState, decode_inbound};

const PING_SECS: u64 = 30;
const MAX_BACKOFF_SECS: u64 = 30;

pub fn spawn_relay(
    tx: EventTx,
    device_token: String,
    session_id: String,
    label: String,
    out_rx: UnboundedReceiver<RemoteOutbound>,
    enabled: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        run_relay(tx, device_token, session_id, label, out_rx, enabled).await;
    });
}

async fn run_relay(
    tx: EventTx,
    device_token: String,
    session_id: String,
    label: String,
    mut out_rx: UnboundedReceiver<RemoteOutbound>,
    enabled: Arc<AtomicBool>,
) {
    // Reconnects reuse the same relay session.
    let url = session_ws_url(&session_id, &label);
    let mut backoff = 1u64;
    loop {
        if !enabled.load(Ordering::Relaxed) {
            break;
        }
        tx.send(AppEvent::RemoteStatus {
            state: RemoteState::Connecting,
            watchers: 0,
        });
        match connect(&url, &device_token).await {
            Ok(ws) => {
                backoff = 1;
                tx.send(AppEvent::RemoteStatus {
                    state: RemoteState::Connected,
                    watchers: 0,
                });
                let graceful = pump(ws, &tx, &mut out_rx, &enabled).await;
                if graceful {
                    break;
                }
                tx.send(AppEvent::RemoteStatus {
                    state: RemoteState::Disconnected,
                    watchers: 0,
                });
            }
            Err(e) => {
                tx.send(AppEvent::RemoteStatus {
                    state: RemoteState::Error,
                    watchers: 0,
                });
                tx.send(AppEvent::Log {
                    level: LogLevel::Warn,
                    msg: format!("remote link: {e}"),
                });
            }
        }
        if !enabled.load(Ordering::Relaxed) {
            break;
        }
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(MAX_BACKOFF_SECS);
    }
    tx.send(AppEvent::RemoteStatus {
        state: RemoteState::Disconnected,
        watchers: 0,
    });
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(url: &str, device_token: &str) -> anyhow::Result<Ws> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = url.into_client_request()?;
    let headers = req.headers_mut();
    headers.insert("Authorization", format!("Bearer {device_token}").parse()?);
    headers.insert("User-Agent", user_agent().parse()?);
    let (ws, _resp) = tokio_tungstenite::connect_async(req).await?;
    Ok(ws)
}

/// Returns `true` when disabled locally, `false` when reconnect should retry.
async fn pump(
    ws: Ws,
    tx: &EventTx,
    out_rx: &mut UnboundedReceiver<RemoteOutbound>,
    enabled: &Arc<AtomicBool>,
) -> bool {
    let (mut sink, mut stream) = ws.split();
    let mut ping = tokio::time::interval(Duration::from_secs(PING_SECS));
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ping.tick().await;

    loop {
        tokio::select! {
            maybe_out = out_rx.recv() => match maybe_out {
                Some(out) => {
                    if sink.send(Message::text(out.encode())).await.is_err() {
                        return false;
                    }
                }
                None => return true,
            },
            maybe_msg = stream.next() => match maybe_msg {
                Some(Ok(Message::Text(text))) => {
                    if let Some(ev) = inbound_to_event(&text) {
                        tx.send(ev);
                    }
                }
                Some(Ok(Message::Ping(payload))) => {
                    let _ = sink.send(Message::Pong(payload)).await;
                }
                Some(Ok(Message::Close(_))) | None => return false,
                Some(Ok(_)) => {}
                Some(Err(_)) => return false,
            },
            _ = ping.tick() => {
                if !enabled.load(Ordering::Relaxed) {
                    let _ = sink.send(Message::Close(None)).await;
                    return true;
                }
                if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return false;
                }
            }
        }
    }
}

fn inbound_to_event(text: &str) -> Option<AppEvent> {
    match decode_inbound(text)? {
        Inbound::Status { watchers } => Some(AppEvent::RemoteStatus {
            state: RemoteState::Connected,
            watchers,
        }),
        Inbound::Command { data } => Some(AppEvent::RemoteCommand(data)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::protocol::RemoteCommand;

    #[test]
    fn status_frame_becomes_connected_event() {
        let ev = inbound_to_event(r#"{"type":"status","watchers":3}"#).unwrap();
        match ev {
            AppEvent::RemoteStatus { state, watchers } => {
                assert_eq!(state, RemoteState::Connected);
                assert_eq!(watchers, 3);
            }
            _ => panic!("expected RemoteStatus"),
        }
    }

    #[test]
    fn command_frame_becomes_remote_command() {
        let ev = inbound_to_event(r#"{"type":"command","data":{"op":"stop"}}"#).unwrap();
        match ev {
            AppEvent::RemoteCommand(cmd) => assert_eq!(cmd, RemoteCommand::Stop),
            _ => panic!("expected RemoteCommand"),
        }
    }

    #[test]
    fn garbage_frame_is_ignored() {
        assert!(inbound_to_event("nonsense").is_none());
    }
}
