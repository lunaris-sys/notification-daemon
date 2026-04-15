/// Unix socket server for shell clients.
///
/// Listens on a Unix socket, accepts multiple clients, broadcasts
/// server messages to all, and routes client messages to handlers.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::{AsyncWriteExt, WriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, Mutex};

use crate::dbus::server::{CloseReason, NotifyEvent};
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
    pub async fn start(
        &self,
        mut event_rx: broadcast::Receiver<NotifyEvent>,
        event_tx: broadcast::Sender<NotifyEvent>,
        db: Arc<Database>,
        dnd_mode: Arc<Mutex<crate::config::DndMode>>,
    ) -> Result<(), NotifyError> {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::remove_file(&self.path);

        let listener = UnixListener::bind(&self.path).map_err(NotifyError::Io)?;
        tracing::info!("socket server listening on {}", self.path.display());

        // Shared list of client writers for broadcasting.
        let writers: Arc<Mutex<Vec<Arc<Mutex<WriteHalf<UnixStream>>>>>> =
            Arc::new(Mutex::new(Vec::new()));

        // Broadcast task: forwards D-Bus events to all connected clients.
        let writers_for_broadcast = writers.clone();
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
                    NotifyEvent::Read { id } => proto::ServerMessage {
                        msg: Some(proto::server_message::Msg::NotificationRead(
                            proto::NotificationRead { id },
                        )),
                    },
                    NotifyEvent::AllCleared => proto::ServerMessage {
                        msg: Some(proto::server_message::Msg::AllCleared(
                            proto::AllCleared {},
                        )),
                    },
                    NotifyEvent::DndChanged { mode } => proto::ServerMessage {
                        msg: Some(proto::server_message::Msg::DndChanged(
                            proto::DndStateChanged {
                                mode: dnd_mode_to_proto(mode),
                            },
                        )),
                    },
                };

                let encoded = match crate::socket::protocol::encode_message(&server_msg) {
                    Ok(buf) => buf,
                    Err(_) => continue,
                };

                let mut ws = writers_for_broadcast.lock().await;
                let mut dead = Vec::new();
                for (i, w) in ws.iter().enumerate() {
                    let mut writer = w.lock().await;
                    if writer.write_all(&encoded).await.is_err()
                        || writer.flush().await.is_err()
                    {
                        dead.push(i);
                    }
                }
                for i in dead.into_iter().rev() {
                    ws.remove(i);
                }
            }
        });

        // Accept loop.
        loop {
            let (stream, _addr) = listener.accept().await.map_err(NotifyError::Io)?;

            // Split the stream: reader for the handler, writer shared for
            // both the handler (responses) and the broadcast task.
            let (reader, write_half) = tokio::io::split(stream);
            let writer = Arc::new(Mutex::new(write_half));

            // Register writer for broadcasts.
            writers.lock().await.push(writer.clone());

            let db = db.clone();
            let dnd = dnd_mode.clone();
            let tx = event_tx.clone();

            tokio::spawn(async move {
                if let Err(e) = handle_client(reader, writer, db, dnd, tx).await {
                    tracing::debug!("client disconnected: {e}");
                }
            });
        }
    }
}

/// Handle a single client connection.
async fn handle_client(
    mut reader: tokio::io::ReadHalf<UnixStream>,
    writer: Arc<Mutex<WriteHalf<UnixStream>>>,
    db: Arc<Database>,
    dnd_mode: Arc<Mutex<crate::config::DndMode>>,
    event_tx: broadcast::Sender<NotifyEvent>,
) -> Result<(), NotifyError> {
    loop {
        // Read from the reader half (does NOT hold the writer lock).
        let msg: Option<proto::ClientMessage> = read_message(&mut reader).await?;

        let Some(msg) = msg else {
            return Ok(());
        };

        let Some(inner) = msg.msg else { continue };

        let response: Option<proto::ServerMessage> = match inner {
            proto::client_message::Msg::Hello(hello) => {
                tracing::info!("client connected: {}", hello.client_name);
                let pending = db.get_pending().await.unwrap_or_default();
                let unread = pending.iter().filter(|n| !n.read).count() as u32;
                let mode = *dnd_mode.lock().await;
                Some(proto::ServerMessage {
                    msg: Some(proto::server_message::Msg::Sync(proto::SyncResponse {
                        pending: pending.iter().map(|n| n.into()).collect(),
                        unread_count: unread,
                        dnd_mode: dnd_mode_to_proto(mode),
                    })),
                })
            }
            proto::client_message::Msg::Dismiss(d) => {
                db.dismiss(d.id, CloseReason::Dismissed).await.ok();
                let _ = event_tx.send(NotifyEvent::Closed {
                    id: d.id,
                    reason: CloseReason::Dismissed,
                });
                None
            }
            proto::client_message::Msg::MarkRead(mr) => {
                db.mark_read(mr.id).await.ok();
                let _ = event_tx.send(NotifyEvent::Read { id: mr.id });
                None
            }
            proto::client_message::Msg::ClearAll(_) => {
                let pending = db.get_pending().await.unwrap_or_default();
                for n in &pending {
                    db.dismiss(n.id, CloseReason::Dismissed).await.ok();
                }
                let _ = event_tx.send(NotifyEvent::AllCleared);
                None
            }
            proto::client_message::Msg::SetDnd(sd) => {
                let new_mode = proto_to_dnd_mode(sd.mode);
                *dnd_mode.lock().await = new_mode;
                let _ = event_tx.send(NotifyEvent::DndChanged { mode: new_mode });
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
            proto::client_message::Msg::InvokeAction(ia) => {
                let _ = event_tx.send(NotifyEvent::ActionInvoked {
                    id: ia.id,
                    action_key: ia.action_key,
                });
                None
            }
            proto::client_message::Msg::GetKnownApps(_) => {
                let app_names = db.get_known_apps().await.unwrap_or_default();
                Some(proto::ServerMessage {
                    msg: Some(proto::server_message::Msg::KnownApps(
                        proto::KnownAppsResponse { app_names },
                    )),
                })
            }
        };

        if let Some(resp) = response {
            let mut w = writer.lock().await;
            write_message(&mut *w, &resp).await?;
        }
    }
}

/// Map a `DndMode` to its proto wire value.
///
/// Each Lunaris mode now has its own dedicated proto enum value.
/// `DndOn` is no longer used by this daemon (kept in the proto for
/// backwards compatibility with old shell builds) — new clients
/// should expect `DndPriority` / `DndAlarms` / `DndTotal` instead.
fn dnd_mode_to_proto(mode: crate::config::DndMode) -> i32 {
    use crate::config::DndMode;
    match mode {
        DndMode::Off => proto::DndMode::DndOff as i32,
        DndMode::Priority => proto::DndMode::DndPriority as i32,
        DndMode::Alarms => proto::DndMode::DndAlarms as i32,
        DndMode::Total => proto::DndMode::DndTotal as i32,
        DndMode::Scheduled => proto::DndMode::DndScheduled as i32,
    }
}

fn proto_to_dnd_mode(value: i32) -> crate::config::DndMode {
    use crate::config::DndMode;
    match value {
        x if x == proto::DndMode::DndPriority as i32 => DndMode::Priority,
        x if x == proto::DndMode::DndAlarms as i32 => DndMode::Alarms,
        x if x == proto::DndMode::DndTotal as i32 => DndMode::Total,
        x if x == proto::DndMode::DndScheduled as i32 => DndMode::Scheduled,
        // Legacy `DndOn` from old shell builds collapses to `Priority`,
        // matching the serde alias on the config side.
        x if x == proto::DndMode::DndOn as i32 => DndMode::Priority,
        _ => DndMode::Off,
    }
}

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
