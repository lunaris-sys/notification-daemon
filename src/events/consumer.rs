/// Event Bus consumer.
///
/// Connects to the Lunaris Event Bus consumer socket, subscribes to
/// `focus.*` (desktop-shell emits these when the user enters/leaves
/// Focus Mode for a project) and `window.fullscreen_*` (compositor
/// will emit these once the wiring in `compositor/src/event_bus.rs`
/// is plumbed through the shell::Workspace fullscreen transitions),
/// decodes the protobuf payloads, and drives the corresponding state
/// changes on the `NotificationManager`.
///
/// Failures never abort the daemon: connection errors log and retry
/// every 2s, malformed messages log and are skipped. The notification
/// daemon must keep functioning even when the event bus is down.

use std::sync::Arc;
use std::time::Duration;

use prost::Message;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::manager::NotificationManager;

pub mod proto {
    #![allow(clippy::doc_markdown)]
    include!(concat!(env!("OUT_DIR"), "/lunaris.eventbus.rs"));
}

/// Default consumer-socket path matching `event-bus::main::DEFAULT_CONSUMER_SOCKET`.
/// Override with `LUNARIS_CONSUMER_SOCKET` for dev sessions.
pub const DEFAULT_CONSUMER_SOCKET: &str = "/run/lunaris/event-bus-consumer.sock";
const CONSUMER_ID: &str = "notification-daemon";
/// Prefix subscriptions. The registry supports `*` for all, exact type,
/// or `<prefix>.` for prefix match. Using two prefixes keeps the
/// registration readable.
const SUBSCRIPTIONS: &str = "focus.,window.fullscreen_";

const MAX_MESSAGE_BYTES: u32 = 1024 * 1024;

/// Starts the Event Bus consumer on the current tokio runtime.
///
/// Spawns a dedicated task that reconnects indefinitely; errors log at
/// `warn`. Callers typically wire this up from `main.rs` alongside the
/// other daemon tasks.
pub fn start(manager: Arc<NotificationManager>) {
    tokio::spawn(async move {
        let socket_path = std::env::var("LUNARIS_CONSUMER_SOCKET")
            .unwrap_or_else(|_| DEFAULT_CONSUMER_SOCKET.to_string());
        loop {
            if let Err(e) = run_once(&socket_path, &manager).await {
                tracing::warn!(
                    "event bus consumer: disconnected ({e}), retrying in 2s"
                );
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

async fn run_once(
    socket_path: &str,
    manager: &Arc<NotificationManager>,
) -> Result<(), String> {
    let mut stream = UnixStream::connect(socket_path)
        .await
        .map_err(|e| format!("connect {socket_path}: {e}"))?;

    // 3-line registration (Phase 3.1: added UID line).
    let uid = unsafe { libc::getuid() };
    let registration = format!("{CONSUMER_ID}\n{SUBSCRIPTIONS}\n{uid}\n");
    stream
        .write_all(registration.as_bytes())
        .await
        .map_err(|e| format!("register: {e}"))?;
    stream
        .flush()
        .await
        .map_err(|e| format!("flush registration: {e}"))?;

    tracing::info!("event bus consumer: registered (subscribe to {SUBSCRIPTIONS})");

    // Read loop: 4-byte BE length + protobuf body.
    loop {
        let mut len_buf = [0u8; 4];
        stream
            .read_exact(&mut len_buf)
            .await
            .map_err(|e| format!("read length: {e}"))?;
        let len = u32::from_be_bytes(len_buf);
        if len == 0 || len > MAX_MESSAGE_BYTES {
            return Err(format!("invalid message length: {len}"));
        }

        let mut buf = vec![0u8; len as usize];
        stream
            .read_exact(&mut buf)
            .await
            .map_err(|e| format!("read body: {e}"))?;

        match proto::Event::decode(&buf[..]) {
            Ok(event) => dispatch(event, manager).await,
            Err(e) => tracing::warn!("event bus: decode failed: {e}"),
        }
    }
}

async fn dispatch(event: proto::Event, manager: &Arc<NotificationManager>) {
    match event.r#type.as_str() {
        "focus.activated" => {
            match proto::FocusActivatedPayload::decode(&event.payload[..]) {
                Ok(payload) => {
                    tracing::info!(
                        project = %payload.project_name,
                        apps = payload.suppress_notifications_from.len(),
                        "focus mode activated via event bus"
                    );
                    manager
                        .activate_focus(
                            payload.project_id,
                            payload.suppress_notifications_from,
                        )
                        .await;
                }
                Err(e) => tracing::warn!("focus.activated decode failed: {e}"),
            }
        }
        "focus.deactivated" => {
            tracing::info!("focus mode deactivated via event bus");
            manager.deactivate_focus().await;
        }
        "window.fullscreen_entered" => {
            tracing::debug!("fullscreen entered via event bus");
            manager.set_fullscreen(true).await;
        }
        "window.fullscreen_exited" => {
            tracing::debug!("fullscreen exited via event bus");
            manager.set_fullscreen(false).await;
        }
        other => {
            // Shouldn't happen given our prefix subscriptions, but logs
            // catch compositor-side misconfig quickly.
            tracing::debug!("event bus: ignoring unknown type '{other}'");
        }
    }
}
