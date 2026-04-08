/// Error types for the notification daemon.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum NotifyError {
    #[error("D-Bus error: {0}")]
    Dbus(#[from] zbus::Error),

    #[error("invalid notification: {0}")]
    Invalid(String),

    #[error("notification not found: {0}")]
    NotFound(u32),

    #[error("database error: {0}")]
    Db(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
