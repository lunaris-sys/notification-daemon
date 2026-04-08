/// Unix socket server for shell clients.
///
/// Listens on a Unix socket, accepts multiple clients, broadcasts
/// server messages to all, and routes client messages to handlers.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use prost::Message;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, Mutex};

use crate::dbus::server::{CloseReason, Notification, NotifyEvent};
use crate::error::NotifyError;
use crate::socket::protocol::{proto, read_message, write_message};
use crate::storage::Database;

/// Socket server that manages shell client connections.
pub struct SocketServer {
    path: PathBuf,
}

impl SocketServer {
    /// Create a new socket server.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Default socket path: `/run/user/{uid}/lunaris/notification.sock`.
    pub fn default_path() -> PathBuf {
        let uid = unsafe { libc::getuid() };
        PathBuf::from(format!("/run/user/{uid}/lunaris/notification.sock"))
    }

    /// Start listening for connections.
    ///
    /// `event_rx` receives events from the D-Bus server to broadcast.
    /// `db` is the notification database for query handling.
    pub async fn start(
        &self,
        mut event_rx: broadcast::Receiver<NotifyEvent>,
        db: Arc<Database>,
        dnd_mode: Arc<Mutex<crate::config::DndMode>>,
    ) -> Result<(), NotifyError> {
        // Ensure parent directory exists.
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Remove stale socket.
        let _ = std::fs::remove_file(&self.path);

        let listener = UnixListener::bind(&self.path).map_err(NotifyError::Io)?;
        tracing::info!("socket server listening on {}", self.path.display());

        let clients: Arc<Mutex<Vec<Arc<Mutex<UnixStream>>>>> =
            Arc::new(Mutex::new(Vec::new()));

        // Broadcast task: forwards D-Bus events to all clients.
        let clients_for_broadcast = clients.clone();
        tokio::spawn(async move {
            while let Ok(event) = event_rx.recv().await {
                let server_msg = match event {
                    NotifyEvent::Added(ref n) => proto::ServerMessage {
                        msg: Some(proto::server_message::Msg::Added(
                            proto::NotificationAdded {
                                notification: Some(n.into()),
                            },
                        )),
                    },
                    NotifyEvent::Closed { id, reason } => proto::ServerMessage {
                        msg: Some(proto::server_message::Msg::Closed(
                            proto::NotificationClosed {
                                id,
                                reason: reason as i32,
                            },
                        )),
                    },
                    NotifyEvent::ActionInvoked {
                        id,
                        ref action_key,
                    } => proto::ServerMessage {
                        msg: Some(proto::server_message::Msg::ActionInvoked(
                            proto::ActionInvoked {
                                id,
                                action_key: action_key.clone(),
                            },
                        )),
                    },
                };

                let encoded = match crate::socket::protocol::encode_message(&server_msg) {
                    Ok(buf) => buf,
                    Err(_) => continue,
                };

                let mut clients = clients_for_broadcast.lock().await;
                let mut dead = Vec::new();
                for (i, client) in clients.iter().enumerate() {
                    let mut stream = client.lock().await;
                    if stream.write_all(&encoded).await.is_err() {
                        dead.push(i);
                    }
                }
                // Remove dead clients in reverse order.
                for i in dead.into_iter().rev() {
                    clients.remove(i);
                }
            }
        });

        // Accept loop.
        loop {
            let (stream, _addr) = listener.accept().await.map_err(NotifyError::Io)?;
            let client = Arc::new(Mutex::new(stream));
            clients.lock().await.push(client.clone());

            let db = db.clone();
            let dnd = dnd_mode.clone();

            tokio::spawn(async move {
                if let Err(e) = handle_client(client, db, dnd).await {
                    tracing::debug!("client disconnected: {e}");
                }
            });
        }
    }
}

/// Handle a single client connection.
async fn handle_client(
    client: Arc<Mutex<UnixStream>>,
    db: Arc<Database>,
    dnd_mode: Arc<Mutex<crate::config::DndMode>>,
) -> Result<(), NotifyError> {
    loop {
        let msg: Option<proto::ClientMessage> = {
            let mut stream = client.lock().await;
            read_message(&mut *stream).await?
        };

        let Some(msg) = msg else {
            return Ok(()); // Clean disconnect.
        };

        let Some(inner) = msg.msg else { continue };

        let response: Option<proto::ServerMessage> = match inner {
            proto::client_message::Msg::Hello(hello) => {
                tracing::info!("client connected: {}", hello.client_name);
                // Sync response with pending notifications.
                let pending = db.get_pending().await.unwrap_or_default();
                let count = pending.len() as u32;
                let unread = pending.iter().filter(|n| !n.read).count() as u32;
                let mode = *dnd_mode.lock().await;
                Some(proto::ServerMessage {
                    msg: Some(proto::server_message::Msg::Sync(proto::SyncResponse {
                        pending: pending.iter().map(|n| n.into()).collect(),
                        unread_count: unread,
                        dnd_mode: match mode {
                            crate::config::DndMode::Off => proto::DndMode::DndOff as i32,
                            crate::config::DndMode::On => proto::DndMode::DndOn as i32,
                            crate::config::DndMode::Scheduled => {
                                proto::DndMode::DndScheduled as i32
                            }
                        },
                    })),
                })
            }
            proto::client_message::Msg::Dismiss(d) => {
                db.dismiss(d.id, CloseReason::Dismissed).await.ok();
                None // Broadcast handled separately.
            }
            proto::client_message::Msg::MarkRead(mr) => {
                db.mark_read(mr.id).await.ok();
                None
            }
            proto::client_message::Msg::ClearAll(_) => {
                // Dismiss all pending.
                let pending = db.get_pending().await.unwrap_or_default();
                for n in &pending {
                    db.dismiss(n.id, CloseReason::Dismissed).await.ok();
                }
                None
            }
            proto::client_message::Msg::SetDnd(sd) => {
                let new_mode = match sd.mode {
                    x if x == proto::DndMode::DndOn as i32 => crate::config::DndMode::On,
                    x if x == proto::DndMode::DndScheduled as i32 => {
                        crate::config::DndMode::Scheduled
                    }
                    _ => crate::config::DndMode::Off,
                };
                *dnd_mode.lock().await = new_mode;
                None
            }
            proto::client_message::Msg::GetHistory(gh) => {
                let before = if gh.before_timestamp.is_empty() {
                    None
                } else {
                    Some(gh.before_timestamp.as_str())
                };
                let app = if gh.app_name.is_empty() {
                    None
                } else {
                    Some(gh.app_name.as_str())
                };
                let notifications = db
                    .get_history(gh.limit, before, app)
                    .await
                    .unwrap_or_default();
                let has_more = notifications.len() == gh.limit as usize;
                Some(proto::ServerMessage {
                    msg: Some(proto::server_message::Msg::History(
                        proto::HistoryResponse {
                            notifications: notifications.iter().map(|n| n.into()).collect(),
                            has_more,
                        },
                    )),
                })
            }
            proto::client_message::Msg::InvokeAction(_ia) => {
                // Action invocation is forwarded via D-Bus signal.
                // That integration requires the D-Bus connection reference.
                // For now, just acknowledge.
                None
            }
        };

        if let Some(resp) = response {
            let mut stream = client.lock().await;
            write_message(&mut *stream, &resp).await?;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_socket_path() {
        let path = SocketServer::default_path();
        let path_str = path.to_string_lossy();
        assert!(path_str.contains("/run/user/"));
        assert!(path_str.ends_with("/lunaris/notification.sock"));
    }
}
