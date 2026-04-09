/// Database row model for notifications.

use serde::{Deserialize, Serialize};

use crate::dbus::server::{Notification, Priority};

/// SQLite row representation of a notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationRow {
    pub id: i64,
    pub app_name: String,
    pub summary: String,
    pub body: String,
    pub app_icon: String,
    /// JSON-encoded actions: `[["key","label"], ...]`
    pub actions_json: String,
    /// "low", "normal", "high", "critical"
    pub priority: String,
    pub urgency: i64,
    pub category: String,
    /// ISO 8601 timestamp.
    pub timestamp: String,
    pub expire_timeout: i64,
    pub read: bool,
    /// Whether the notification has been dismissed.
    pub dismissed: bool,
    /// Close reason if dismissed (1-4), or NULL.
    pub close_reason: Option<i64>,
}

impl NotificationRow {
    /// Convert from a D-Bus `Notification` to a database row.
    pub fn from_notification(n: &Notification) -> Self {
        let actions_json =
            serde_json::to_string(&n.actions).unwrap_or_else(|_| "[]".to_string());
        Self {
            id: n.id as i64,
            app_name: n.app_name.clone(),
            summary: n.summary.clone(),
            body: n.body.clone(),
            app_icon: n.app_icon.clone(),
            actions_json,
            priority: priority_to_str(n.priority),
            urgency: n.urgency as i64,
            category: n.category.clone(),
            timestamp: n.timestamp.clone(),
            expire_timeout: n.expire_timeout as i64,
            read: n.read,
            dismissed: false,
            close_reason: None,
        }
    }

    /// Convert from a database row back to a `Notification`.
    pub fn to_notification(&self) -> Notification {
        let actions: Vec<(String, String)> =
            serde_json::from_str(&self.actions_json).unwrap_or_default();
        Notification {
            id: self.id as u32,
            app_name: self.app_name.clone(),
            summary: self.summary.clone(),
            body: self.body.clone(),
            app_icon: self.app_icon.clone(),
            actions,
            priority: priority_from_str(&self.priority),
            urgency: self.urgency as u8,
            category: self.category.clone(),
            timestamp: self.timestamp.clone(),
            expire_timeout: self.expire_timeout as i32,
            read: self.read,
        }
    }
}

fn priority_to_str(p: Priority) -> String {
    match p {
        Priority::Low => "low",
        Priority::Normal => "normal",
        Priority::High => "high",
        Priority::Critical => "critical",
    }
    .to_string()
}

fn priority_from_str(s: &str) -> Priority {
    match s {
        "low" => Priority::Low,
        "high" => Priority::High,
        "critical" => Priority::Critical,
        _ => Priority::Normal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_notification() -> Notification {
        Notification {
            id: 1,
            app_name: "Firefox".into(),
            summary: "Download complete".into(),
            body: "file.zip finished".into(),
            app_icon: "firefox".into(),
            actions: vec![("open".into(), "Open".into())],
            priority: Priority::High,
            urgency: 1,
            category: "transfer.complete".into(),
            timestamp: "2026-04-09T12:00:00Z".into(),
            expire_timeout: 5000,
            read: false,
        }
    }

    #[test]
    fn test_roundtrip_conversion() {
        let n = sample_notification();
        let row = NotificationRow::from_notification(&n);
        let back = row.to_notification();

        assert_eq!(back.id, n.id);
        assert_eq!(back.app_name, n.app_name);
        assert_eq!(back.summary, n.summary);
        assert_eq!(back.body, n.body);
        assert_eq!(back.priority, n.priority);
        assert_eq!(back.actions.len(), 1);
        assert_eq!(back.actions[0].0, "open");
    }

    #[test]
    fn test_priority_str_roundtrip() {
        for p in [Priority::Low, Priority::Normal, Priority::High, Priority::Critical] {
            assert_eq!(priority_from_str(&priority_to_str(p)), p);
        }
    }

    #[test]
    fn test_dismissed_defaults() {
        let n = sample_notification();
        let row = NotificationRow::from_notification(&n);
        assert!(!row.dismissed);
        assert!(row.close_reason.is_none());
    }
}
