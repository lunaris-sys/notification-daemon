/// Lunaris notification daemon library.
///
/// Implements the `org.freedesktop.Notifications` D-Bus interface with
/// priority determination, notification storage, and event broadcasting.

pub mod config;
pub mod dbus;
pub mod dnd;
pub mod error;
pub mod events;
pub mod storage;
