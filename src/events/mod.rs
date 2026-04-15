/// Event types for compositor / Event Bus integration.
///
/// These are consumed by the daemon to track fullscreen state and
/// focus changes. The actual Event Bus connection is Phase 4; for now
/// these types define the interface.

use serde::{Deserialize, Serialize};

/// Events the notification daemon cares about.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SystemEvent {
    /// A window entered fullscreen.
    FullscreenEntered,
    /// A window exited fullscreen.
    FullscreenExited,
    /// DND was toggled from the shell.
    DndChanged { enabled: bool },
    /// Focus Mode activated for a project.
    FocusActivated {
        project_id: String,
        suppress_apps: Vec<String>,
    },
    /// Focus Mode deactivated.
    FocusDeactivated,
}

/// Trait for receiving system events.
///
/// Implemented by the real Event Bus consumer in production, and by
/// a mock channel in tests.
pub trait EventSource: Send {
    /// Try to receive the next event (non-blocking).
    fn try_recv(&self) -> Option<SystemEvent>;
}

/// Channel-based event source for testing and early integration.
pub struct ChannelEventSource {
    rx: std::sync::mpsc::Receiver<SystemEvent>,
}

impl ChannelEventSource {
    /// Create a new channel event source.
    pub fn new() -> (std::sync::mpsc::Sender<SystemEvent>, Self) {
        let (tx, rx) = std::sync::mpsc::channel();
        (tx, Self { rx })
    }
}

impl EventSource for ChannelEventSource {
    fn try_recv(&self) -> Option<SystemEvent> {
        self.rx.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_event_source() {
        let (tx, source) = ChannelEventSource::new();
        assert!(source.try_recv().is_none());

        tx.send(SystemEvent::FullscreenEntered).unwrap();
        let event = source.try_recv().unwrap();
        assert!(matches!(event, SystemEvent::FullscreenEntered));
    }

    #[test]
    fn test_dnd_changed_event() {
        let (tx, source) = ChannelEventSource::new();
        tx.send(SystemEvent::DndChanged { enabled: true }).unwrap();
        let event = source.try_recv().unwrap();
        match event {
            SystemEvent::DndChanged { enabled } => assert!(enabled),
            _ => panic!("wrong event type"),
        }
    }
}
