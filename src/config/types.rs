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
    /// History retention. Parses from either `[history]` (current) or
    /// `[retention]` (legacy) TOML section.
    #[serde(alias = "retention")]
    pub history: HistoryConfig,
    pub grouping: GroupingConfig,
    /// Per-app overrides keyed by app_name or app_id.
    #[serde(default)]
    pub apps: HashMap<String, AppOverride>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            general: GeneralConfig::default(),
            dnd: DndConfig::default(),
            history: HistoryConfig::default(),
            grouping: GroupingConfig::default(),
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
    /// ISO-8601 UTC timestamp when the current mode should auto-expire
    /// back to Off. Set by Quick Actions like "enable DND for 1 hour".
    /// None = no expiry (mode stays until the user changes it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Schedule for automatic DND (only used when mode = "scheduled").
    pub schedule: DndSchedule,
    /// Suppress notifications from these apps even when DND is off.
    #[serde(default)]
    pub always_suppress: Vec<String>,
    /// Allow notifications from these apps even when DND is on.
    /// Always-allow is honoured by every mode including Total.
    #[serde(default)]
    pub always_allow: Vec<String>,
    /// Suppress toasts when any window is fullscreen.
    pub suppress_fullscreen: bool,
}

impl Default for DndConfig {
    fn default() -> Self {
        Self {
            mode: DndMode::Off,
            expires_at: None,
            schedule: DndSchedule::default(),
            always_suppress: Vec::new(),
            always_allow: Vec::new(),
            suppress_fullscreen: true,
        }
    }
}

/// DND operating mode.
///
/// Modes are checked after `always_allow` (whitelist) has been applied,
/// so a whitelisted app always passes regardless of mode. The legacy
/// `"on"` value is accepted as an alias for `Priority`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DndMode {
    /// DND off. All notifications pass.
    Off,
    /// Only critical-urgency notifications pass through (plus always_allow).
    #[serde(alias = "on")]
    Priority,
    /// Only alarm / reminder category notifications pass through.
    Alarms,
    /// Nothing passes except always_allow.
    Total,
    /// DND follows a time schedule; the `schedule.mode` field chooses
    /// which suppression flavour is active during the scheduled window.
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
    /// Suppression flavour to apply during the scheduled window.
    /// Defaults to `Priority` to match legacy behaviour.
    #[serde(default = "default_schedule_mode")]
    pub mode: ScheduleMode,
}

fn default_schedule_mode() -> ScheduleMode {
    ScheduleMode::Priority
}

/// Sub-mode for scheduled DND. A strict subset of `DndMode` without
/// `Off`/`Scheduled` because those would not make sense inside a
/// scheduled window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScheduleMode {
    Priority,
    Alarms,
    Total,
}

impl Default for ScheduleMode {
    fn default() -> Self {
        Self::Priority
    }
}

impl Default for DndSchedule {
    fn default() -> Self {
        Self {
            start: "22:00".into(),
            end: "07:00".into(),
            days: Vec::new(),
            mode: ScheduleMode::Priority,
        }
    }
}

/// History retention settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HistoryConfig {
    /// Keep notifications in the history panel at all. When `false`
    /// notifications are dropped after broadcast (no SQLite persistence).
    pub enabled: bool,
    /// Maximum age of dismissed notifications in days.
    pub max_age_days: u32,
    /// Maximum total notification count.
    pub max_count: u32,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_age_days: 30,
            max_count: 1000,
        }
    }
}

/// Visual grouping settings consumed by the shell toast renderer and
/// notification panel. The daemon only reads `stack_similar` for its
/// own dedup pass; `by_app` and `auto_collapse_after` are pure UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupingConfig {
    /// Group notifications by source app in the history panel.
    pub by_app: bool,
    /// Merge near-duplicate notifications (same app + same summary
    /// within a short window) into a single stacked entry.
    pub stack_similar: bool,
    /// Collapse a group automatically once it holds this many entries.
    pub auto_collapse_after: u32,
}

impl Default for GroupingConfig {
    fn default() -> Self {
        Self {
            by_app: true,
            stack_similar: true,
            auto_collapse_after: 3,
        }
    }
}

/// Per-app notification overrides.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AppOverride {
    /// Accept notifications from this app at all. When `Some(false)`
    /// the notification is dropped before storage and broadcast.
    /// `None` (the default) behaves as enabled.
    pub enabled: Option<bool>,
    /// Override priority for this app ("low", "normal", "high", "critical").
    pub priority: Option<String>,
    /// Store silently: notifications go into the history panel but no
    /// toast is shown. Weaker than `enabled = false` which drops entirely.
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
        assert_eq!(c.history.max_age_days, 30);
        assert!(c.history.enabled);
        assert!(c.grouping.by_app);
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
mode = "total"

[history]
enabled = true
max_age_days = 14
max_count = 500

[grouping]
by_app = true
stack_similar = false
auto_collapse_after = 5

[apps.firefox]
priority = "low"

[apps.discord]
suppress = true

[apps.spammy]
enabled = false
"#;
        let c: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(c.dnd.mode, DndMode::Scheduled);
        assert_eq!(c.dnd.schedule.start, "23:00");
        assert_eq!(c.dnd.schedule.days, vec![0, 1, 2, 3, 4]);
        assert_eq!(c.dnd.schedule.mode, ScheduleMode::Total);
        assert!(!c.dnd.suppress_fullscreen);
        assert_eq!(c.dnd.always_suppress, vec!["slack"]);
        assert_eq!(c.history.max_age_days, 14);
        assert!(!c.grouping.stack_similar);
        assert_eq!(c.grouping.auto_collapse_after, 5);
        assert_eq!(c.apps.len(), 3);
        assert_eq!(
            c.apps.get("firefox").unwrap().priority.as_deref(),
            Some("low")
        );
        assert_eq!(c.apps.get("discord").unwrap().suppress, Some(true));
        assert_eq!(c.apps.get("spammy").unwrap().enabled, Some(false));
    }

    #[test]
    fn test_parse_legacy_retention_alias() {
        // Old configs used `[retention]`; serde alias maps it to `history`.
        let toml_str = r#"
[retention]
max_age_days = 7
max_count = 200
"#;
        let c: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(c.history.max_age_days, 7);
        assert_eq!(c.history.max_count, 200);
    }

    #[test]
    fn test_parse_legacy_dnd_mode_on_alias() {
        // Old configs used `mode = "on"`; serde alias maps it to Priority.
        let toml_str = r#"
[dnd]
mode = "on"
"#;
        let c: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(c.dnd.mode, DndMode::Priority);
    }

    #[test]
    fn test_dnd_mode_serialization() {
        assert_eq!(
            serde_json::to_string(&DndMode::Off).unwrap(),
            "\"off\""
        );
        assert_eq!(
            serde_json::to_string(&DndMode::Priority).unwrap(),
            "\"priority\""
        );
        assert_eq!(
            serde_json::to_string(&DndMode::Alarms).unwrap(),
            "\"alarms\""
        );
        assert_eq!(
            serde_json::to_string(&DndMode::Total).unwrap(),
            "\"total\""
        );
        assert_eq!(
            serde_json::to_string(&DndMode::Scheduled).unwrap(),
            "\"scheduled\""
        );
    }
}
