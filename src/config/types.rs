/// Configuration types for the notification daemon.
///
/// Loaded from `~/.config/lunaris/notifications.toml`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Root configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: GeneralConfig,
    pub dnd: DndConfig,
    pub retention: RetentionConfig,
    /// Per-app overrides keyed by app_name or app_id.
    #[serde(default)]
    pub apps: HashMap<String, AppOverride>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            general: GeneralConfig::default(),
            dnd: DndConfig::default(),
            retention: RetentionConfig::default(),
            apps: HashMap::new(),
        }
    }
}

/// General daemon settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    /// Default toast duration for normal priority (ms).
    pub toast_duration_normal: u32,
    /// Default toast duration for high priority (ms).
    pub toast_duration_high: u32,
    /// Maximum number of visible toasts at once.
    pub max_visible_toasts: u32,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            toast_duration_normal: 4000,
            toast_duration_high: 8000,
            max_visible_toasts: 5,
        }
    }
}

/// Do Not Disturb configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DndConfig {
    /// Current DND mode.
    pub mode: DndMode,
    /// Schedule for automatic DND (only used when mode = "scheduled").
    pub schedule: DndSchedule,
    /// Suppress notifications from these apps even when DND is off.
    #[serde(default)]
    pub always_suppress: Vec<String>,
    /// Allow notifications from these apps even when DND is on.
    #[serde(default)]
    pub always_allow: Vec<String>,
    /// Suppress toasts when any window is fullscreen.
    pub suppress_fullscreen: bool,
}

impl Default for DndConfig {
    fn default() -> Self {
        Self {
            mode: DndMode::Off,
            schedule: DndSchedule::default(),
            always_suppress: Vec::new(),
            always_allow: Vec::new(),
            suppress_fullscreen: true,
        }
    }
}

/// DND operating mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DndMode {
    /// DND is off. All notifications shown.
    Off,
    /// DND is on. Only critical and always_allow pass through.
    On,
    /// DND follows a time schedule.
    Scheduled,
}

impl Default for DndMode {
    fn default() -> Self {
        Self::Off
    }
}

/// Time schedule for DND.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DndSchedule {
    /// Start time in HH:MM format (24h).
    pub start: String,
    /// End time in HH:MM format (24h). Can be before start for overnight.
    pub end: String,
    /// Days of the week (0=Monday, 6=Sunday). Empty = every day.
    #[serde(default)]
    pub days: Vec<u8>,
}

impl Default for DndSchedule {
    fn default() -> Self {
        Self {
            start: "22:00".into(),
            end: "07:00".into(),
            days: Vec::new(),
        }
    }
}

/// Retention / cleanup settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RetentionConfig {
    /// Maximum age of dismissed notifications in days.
    pub max_age_days: u32,
    /// Maximum total notification count.
    pub max_count: u32,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            max_age_days: 30,
            max_count: 1000,
        }
    }
}

/// Per-app notification overrides.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AppOverride {
    /// Override priority for this app ("low", "normal", "high", "critical").
    pub priority: Option<String>,
    /// Suppress all notifications from this app.
    pub suppress: Option<bool>,
    /// Override toast duration (ms).
    pub toast_duration: Option<u32>,
    /// Bypass DND for this app.
    pub bypass_dnd: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let c = Config::default();
        assert_eq!(c.dnd.mode, DndMode::Off);
        assert_eq!(c.general.toast_duration_normal, 4000);
        assert_eq!(c.retention.max_age_days, 30);
        assert!(c.apps.is_empty());
    }

    #[test]
    fn test_parse_minimal_toml() {
        let toml_str = r#"
[general]
toast_duration_normal = 3000
"#;
        let c: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(c.general.toast_duration_normal, 3000);
        // Defaults for unspecified fields.
        assert_eq!(c.general.toast_duration_high, 8000);
        assert_eq!(c.dnd.mode, DndMode::Off);
    }

    #[test]
    fn test_parse_full_toml() {
        let toml_str = r#"
[general]
toast_duration_normal = 3000
toast_duration_high = 6000
max_visible_toasts = 3

[dnd]
mode = "scheduled"
suppress_fullscreen = false
always_suppress = ["slack"]
always_allow = ["phone-app"]

[dnd.schedule]
start = "23:00"
end = "06:00"
days = [0, 1, 2, 3, 4]

[retention]
max_age_days = 14
max_count = 500

[apps.firefox]
priority = "low"

[apps.discord]
suppress = true
"#;
        let c: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(c.dnd.mode, DndMode::Scheduled);
        assert_eq!(c.dnd.schedule.start, "23:00");
        assert_eq!(c.dnd.schedule.days, vec![0, 1, 2, 3, 4]);
        assert!(!c.dnd.suppress_fullscreen);
        assert_eq!(c.dnd.always_suppress, vec!["slack"]);
        assert_eq!(c.apps.len(), 2);
        assert_eq!(
            c.apps.get("firefox").unwrap().priority.as_deref(),
            Some("low")
        );
        assert_eq!(c.apps.get("discord").unwrap().suppress, Some(true));
    }

    #[test]
    fn test_dnd_mode_serialization() {
        assert_eq!(
            serde_json::to_string(&DndMode::Off).unwrap(),
            "\"off\""
        );
        assert_eq!(
            serde_json::to_string(&DndMode::On).unwrap(),
            "\"on\""
        );
        assert_eq!(
            serde_json::to_string(&DndMode::Scheduled).unwrap(),
            "\"scheduled\""
        );
    }
}
