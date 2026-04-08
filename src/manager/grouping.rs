/// Notification grouping by app and category.
///
/// Generates a group key used to visually group related notifications
/// in the shell panel.

use crate::dbus::server::Notification;

/// Derive a group key for a notification.
///
/// Grouping hierarchy:
/// 1. Category prefix (e.g. "im" from "im.received") if present
/// 2. App name as fallback
///
/// The shell uses this key to collapse notifications from the same
/// source into a single group header.
pub fn derive_group_key(notification: &Notification) -> String {
    if !notification.category.is_empty() {
        // Use the major category (before the first dot).
        let major = notification
            .category
            .split('.')
            .next()
            .unwrap_or(&notification.category);
        format!("{}:{}", notification.app_name, major)
    } else {
        notification.app_name.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dbus::server::Priority;

    fn make(app: &str, category: &str) -> Notification {
        Notification {
            id: 1,
            app_name: app.into(),
            summary: "test".into(),
            body: "".into(),
            app_icon: "".into(),
            actions: vec![],
            priority: Priority::Normal,
            urgency: 1,
            category: category.into(),
            timestamp: "2026-04-09T12:00:00Z".into(),
            expire_timeout: -1,
            read: false,
        }
    }

    #[test]
    fn test_group_by_app_only() {
        assert_eq!(derive_group_key(&make("Firefox", "")), "Firefox");
    }

    #[test]
    fn test_group_by_category() {
        assert_eq!(
            derive_group_key(&make("Discord", "im.received")),
            "Discord:im"
        );
    }

    #[test]
    fn test_group_category_no_dot() {
        assert_eq!(
            derive_group_key(&make("App", "transfer")),
            "App:transfer"
        );
    }

    #[test]
    fn test_group_email() {
        assert_eq!(
            derive_group_key(&make("Thunderbird", "email.arrived")),
            "Thunderbird:email"
        );
    }
}
