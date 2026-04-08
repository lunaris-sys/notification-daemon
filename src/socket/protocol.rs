/// Length-prefixed protobuf framing.
///
/// Wire format: 4-byte big-endian length + protobuf body.
/// Maximum message size: 1 MB.

use bytes::{Buf, BufMut, BytesMut};
use prost::Message;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::NotifyError;

/// Maximum message size (1 MB).
pub const MAX_MESSAGE_SIZE: usize = 1024 * 1024;

/// Length of the framing header (4 bytes).
pub const HEADER_SIZE: usize = 4;

/// Encode a protobuf message into a length-prefixed buffer.
pub fn encode_message<M: Message>(msg: &M) -> Result<Vec<u8>, NotifyError> {
    let body_len = msg.encoded_len();
    if body_len > MAX_MESSAGE_SIZE {
        return Err(NotifyError::Invalid(format!(
            "message too large: {body_len} bytes (max {MAX_MESSAGE_SIZE})"
        )));
    }

    let mut buf = Vec::with_capacity(HEADER_SIZE + body_len);
    buf.put_u32(body_len as u32);
    msg.encode(&mut buf)
        .map_err(|e| NotifyError::Invalid(format!("encode error: {e}")))?;

    Ok(buf)
}

/// Read a single length-prefixed message from an async reader.
///
/// Returns `None` on clean EOF (client disconnected).
pub async fn read_message<R, M>(reader: &mut R) -> Result<Option<M>, NotifyError>
where
    R: AsyncReadExt + Unpin,
    M: Message + Default,
{
    // Read 4-byte length header.
    let mut len_buf = [0u8; HEADER_SIZE];
    match reader.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(NotifyError::Io(e)),
    }

    let body_len = u32::from_be_bytes(len_buf) as usize;
    if body_len > MAX_MESSAGE_SIZE {
        return Err(NotifyError::Invalid(format!(
            "message too large: {body_len} bytes"
        )));
    }

    // Read body.
    let mut body = vec![0u8; body_len];
    reader
        .read_exact(&mut body)
        .await
        .map_err(NotifyError::Io)?;

    let msg = M::decode(&body[..])
        .map_err(|e| NotifyError::Invalid(format!("decode error: {e}")))?;

    Ok(Some(msg))
}

/// Write a length-prefixed message to an async writer.
pub async fn write_message<W, M>(writer: &mut W, msg: &M) -> Result<(), NotifyError>
where
    W: AsyncWriteExt + Unpin,
    M: Message,
{
    let buf = encode_message(msg)?;
    writer.write_all(&buf).await.map_err(NotifyError::Io)?;
    writer.flush().await.map_err(NotifyError::Io)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Conversion helpers: Notification <-> proto Notification
// ---------------------------------------------------------------------------

use crate::dbus::server::{Notification, Priority};

/// Generated proto types.
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/lunaris.notification.rs"));
}

impl From<&Notification> for proto::Notification {
    fn from(n: &Notification) -> Self {
        proto::Notification {
            id: n.id,
            app_name: n.app_name.clone(),
            summary: n.summary.clone(),
            body: n.body.clone(),
            app_icon: n.app_icon.clone(),
            actions: n
                .actions
                .iter()
                .map(|(k, l)| proto::Action {
                    key: k.clone(),
                    label: l.clone(),
                })
                .collect(),
            priority: match n.priority {
                Priority::Low => proto::Priority::Low as i32,
                Priority::Normal => proto::Priority::Normal as i32,
                Priority::High => proto::Priority::High as i32,
                Priority::Critical => proto::Priority::Critical as i32,
            },
            category: n.category.clone(),
            timestamp: n.timestamp.clone(),
            read: n.read,
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
    fn test_encode_decode_roundtrip() {
        let msg = proto::ServerMessage {
            msg: Some(proto::server_message::Msg::CountUpdate(
                proto::CountUpdate {
                    pending_count: 5,
                    unread_count: 3,
                },
            )),
        };

        let encoded = encode_message(&msg).unwrap();
        assert_eq!(
            u32::from_be_bytes(encoded[..4].try_into().unwrap()) as usize,
            msg.encoded_len()
        );

        let decoded =
            proto::ServerMessage::decode(&encoded[HEADER_SIZE..]).unwrap();
        match decoded.msg {
            Some(proto::server_message::Msg::CountUpdate(cu)) => {
                assert_eq!(cu.pending_count, 5);
                assert_eq!(cu.unread_count, 3);
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_encode_oversized_rejected() {
        // Create a message with a very large body.
        let msg = proto::ServerMessage {
            msg: Some(proto::server_message::Msg::Sync(proto::SyncResponse {
                pending: (0..100_000)
                    .map(|i| proto::Notification {
                        id: i,
                        summary: "x".repeat(100),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            })),
        };
        let result = encode_message(&msg);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_read_write_message() {
        let msg = proto::ClientMessage {
            msg: Some(proto::client_message::Msg::Hello(proto::ClientHello {
                client_name: "test-shell".into(),
            })),
        };

        // Write to buffer.
        let mut buf = Vec::new();
        write_message(&mut buf, &msg).await.unwrap();

        // Read back.
        let mut cursor = std::io::Cursor::new(buf);
        let decoded: Option<proto::ClientMessage> =
            read_message(&mut cursor).await.unwrap();

        let decoded = decoded.unwrap();
        match decoded.msg {
            Some(proto::client_message::Msg::Hello(h)) => {
                assert_eq!(h.client_name, "test-shell");
            }
            _ => panic!("wrong message type"),
        }
    }

    #[tokio::test]
    async fn test_read_eof_returns_none() {
        let mut cursor = std::io::Cursor::new(Vec::<u8>::new());
        let result: Result<Option<proto::ClientMessage>, _> =
            read_message(&mut cursor).await;
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_read_oversized_rejected() {
        // Write a fake header claiming 2MB body.
        let mut buf = Vec::new();
        buf.put_u32(2 * 1024 * 1024);
        buf.extend_from_slice(&[0u8; 100]); // partial body

        let mut cursor = std::io::Cursor::new(buf);
        let result: Result<Option<proto::ClientMessage>, _> =
            read_message(&mut cursor).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_notification_to_proto() {
        let n = Notification {
            id: 42,
            app_name: "Firefox".into(),
            summary: "Done".into(),
            body: "file.zip".into(),
            app_icon: "firefox".into(),
            actions: vec![("open".into(), "Open".into())],
            priority: Priority::High,
            urgency: 1,
            category: "transfer.complete".into(),
            timestamp: "2026-04-09T12:00:00Z".into(),
            expire_timeout: 5000,
            read: false,
        };

        let p: proto::Notification = (&n).into();
        assert_eq!(p.id, 42);
        assert_eq!(p.app_name, "Firefox");
        assert_eq!(p.priority, proto::Priority::High as i32);
        assert_eq!(p.actions.len(), 1);
        assert_eq!(p.actions[0].key, "open");
    }
}
