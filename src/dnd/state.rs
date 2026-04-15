/// DND state machine and suppression logic.
///
/// Evaluates whether a notification should be shown, suppressed, or
/// queued based on DND mode, schedule, per-app overrides, and
/// fullscreen state.

use chrono::{DateTime, Datelike, Local, NaiveTime, Utc, Weekday};

use crate::config::{AppOverride, DndConfig, DndMode, ScheduleMode};
use crate::dbus::server::{Notification, Priority};
use crate::dnd::focus::FocusSuppression;

/// Result of checking whether a notification should be suppressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuppressResult {
    /// Show the notification normally.
    Allow,
    /// Suppress the toast but store the notification in history.
    Suppress,
    /// Queue for later (e.g. fullscreen exit). Store and defer toast.
    Queue,
    /// Drop entirely: skip storage, broadcast and history. Used for
    /// per-app `enabled = false` and `DndMode::Total` (except always_allow).
    Drop,
}

/// Runtime DND state.
#[derive(Debug, Default)]
pub struct DndState {
    /// Whether a fullscreen window is currently active.
    pub fullscreen_active: bool,
    /// Focus Mode suppression (ephemeral, not persisted).
    pub focus: FocusSuppression,
}

impl DndState {
    /// Determine whether a notification should be suppressed.
    ///
    /// Checks in order (first match wins):
    /// 1. Per-app `enabled = false` → Drop (no storage)
    /// 2. Per-app suppress override → Suppress
    /// 3. Always-suppress list → Suppress
    /// 4. Always-allow list → bypass all DND/focus, only check fullscreen
    /// 5. Per-app bypass_dnd → bypass all DND/focus, only check fullscreen
    /// 6. Effective DND mode (honours TTL + schedule) applied to the
    ///    notification priority / category
    /// 7. Focus Mode suppression
    /// 8. Fullscreen suppression
    pub fn should_suppress(
        &self,
        notification: &Notification,
        dnd_config: &DndConfig,
        app_override: Option<&AppOverride>,
    ) -> SuppressResult {
        // 1. Per-app hard block.
        if let Some(ovr) = app_override {
            if ovr.enabled == Some(false) {
                return SuppressResult::Drop;
            }
        }

        // 2. Per-app force suppress.
        if let Some(ovr) = app_override {
            if ovr.suppress == Some(true) {
                return SuppressResult::Suppress;
            }
        }

        // 3. Always-suppress list.
        if dnd_config
            .always_suppress
            .iter()
            .any(|a| a == &notification.app_name)
        {
            return SuppressResult::Suppress;
        }

        // 4. Always-allow list — bypasses ALL DND/focus regardless of mode.
        if dnd_config
            .always_allow
            .iter()
            .any(|a| a == &notification.app_name)
        {
            return self.check_fullscreen(dnd_config);
        }

        // 5. Per-app bypass DND.
        if let Some(ovr) = app_override {
            if ovr.bypass_dnd == Some(true) {
                return self.check_fullscreen(dnd_config);
            }
        }

        // 6. DND mode (with TTL and schedule resolution).
        let effective = effective_mode(dnd_config);
        let allowed_by_mode = match effective {
            EffectiveMode::Off => true,
            EffectiveMode::Priority => notification.priority == Priority::Critical,
            EffectiveMode::Alarms => is_alarm_category(&notification.category),
            EffectiveMode::Total => false,
        };
        if !allowed_by_mode {
            // Total silence drops; Priority/Alarms keep in history.
            return match effective {
                EffectiveMode::Total => SuppressResult::Drop,
                _ => SuppressResult::Suppress,
            };
        }

        // 7. Focus Mode suppression. Critical still always passes.
        if notification.priority != Priority::Critical
            && self.focus.is_suppressed(&notification.app_name)
        {
            return SuppressResult::Suppress;
        }

        // 8. Fullscreen suppression. Critical bypasses the queue so a
        // genuine alarm can interrupt an immersive app.
        if notification.priority == Priority::Critical {
            return SuppressResult::Allow;
        }
        self.check_fullscreen(dnd_config)
    }

    /// Check fullscreen suppression (queues instead of hard suppress).
    fn check_fullscreen(&self, dnd_config: &DndConfig) -> SuppressResult {
        if dnd_config.suppress_fullscreen && self.fullscreen_active {
            SuppressResult::Queue
        } else {
            SuppressResult::Allow
        }
    }
}

/// The DND mode actually applied to incoming notifications right now.
/// Flattens `Scheduled` into its sub-mode and honours `expires_at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveMode {
    Off,
    Priority,
    Alarms,
    Total,
}

/// Resolve `dnd_config.mode` into an `EffectiveMode`:
///   * If `expires_at` is set and in the past, return `Off`.
///   * `Scheduled` + inside window → `schedule.mode`.
///   * `Scheduled` + outside window → `Off`.
pub fn effective_mode(dnd_config: &DndConfig) -> EffectiveMode {
    if let Some(ref ts) = dnd_config.expires_at {
        if is_expired(ts) {
            return EffectiveMode::Off;
        }
    }
    match dnd_config.mode {
        DndMode::Off => EffectiveMode::Off,
        DndMode::Priority => EffectiveMode::Priority,
        DndMode::Alarms => EffectiveMode::Alarms,
        DndMode::Total => EffectiveMode::Total,
        DndMode::Scheduled => {
            if is_in_schedule(&dnd_config.schedule) {
                match dnd_config.schedule.mode {
                    ScheduleMode::Priority => EffectiveMode::Priority,
                    ScheduleMode::Alarms => EffectiveMode::Alarms,
                    ScheduleMode::Total => EffectiveMode::Total,
                }
            } else {
                EffectiveMode::Off
            }
        }
    }
}

/// True if the timestamp (ISO-8601) is strictly in the past relative to now.
/// Invalid timestamps are treated as not expired so a typo cannot silently
/// flip DND off.
fn is_expired(iso: &str) -> bool {
    match DateTime::parse_from_rfc3339(iso) {
        Ok(ts) => ts.with_timezone(&Utc) <= Utc::now(),
        Err(_) => false,
    }
}

/// True if a notification's category hint identifies it as an alarm
/// or reminder per the freedesktop notification spec (`x-alarm*`,
/// `alarm*`, `reminder*`).
pub fn is_alarm_category(category: &str) -> bool {
    let c = category.to_ascii_lowercase();
    c.starts_with("alarm")
        || c.starts_with("x-alarm")
        || c.starts_with("reminder")
        || c.starts_with("x-reminder")
}

/// Check if the current time falls within the DND schedule.
///
/// Handles overnight schedules (e.g. 22:00 - 07:00) and weekday
/// filtering. Empty `days` list means every day.
pub fn is_in_schedule(schedule: &crate::config::DndSchedule) -> bool {
    let now = Local::now();
    let current_time = now.time();

    // Parse start and end times.
    let Some(start) = parse_time(&schedule.start) else {
        return false;
    };
    let Some(end) = parse_time(&schedule.end) else {
        return false;
    };

    // Check weekday filter.
    if !schedule.days.is_empty() {
        let weekday = match now.weekday() {
            Weekday::Mon => 0,
            Weekday::Tue => 1,
            Weekday::Wed => 2,
            Weekday::Thu => 3,
            Weekday::Fri => 4,
            Weekday::Sat => 5,
            Weekday::Sun => 6,
        };
        if !schedule.days.contains(&weekday) {
            return false;
        }
    }

    // Check time range.
    if start <= end {
        // Same-day range (e.g. 09:00 - 17:00).
        current_time >= start && current_time < end
    } else {
        // Overnight range (e.g. 22:00 - 07:00).
        current_time >= start || current_time < end
    }
}

/// Check a schedule against a specific time (for testing).
pub fn is_in_schedule_at(
    schedule: &crate::config::DndSchedule,
    hour: u32,
    minute: u32,
    weekday: u8,
) -> bool {
    let Some(start) = parse_time(&schedule.start) else {
        return false;
    };
    let Some(end) = parse_time(&schedule.end) else {
        return false;
    };

    if !schedule.days.is_empty() && !schedule.days.contains(&weekday) {
        return false;
    }

    let current = NaiveTime::from_hms_opt(hour, minute, 0).unwrap();
    if start <= end {
        current >= start && current < end
    } else {
        current >= start || current < end
    }
}

fn parse_time(s: &str) -> Option<NaiveTime> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 2 {
        return None;
    }
    let h: u32 = parts[0].parse().ok()?;
    let m: u32 = parts[1].parse().ok()?;
    NaiveTime::from_hms_opt(h, m, 0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DndConfig, DndSchedule};
    use crate::dbus::server::Priority;

    fn make_notification(app: &str, priority: Priority) -> Notification {
        Notification {
            id: 1,
            app_name: app.into(),
            summary: "test".into(),
            body: "".into(),
            app_icon: "".into(),
            actions: vec![],
            priority,
            urgency: 1,
            category: "".into(),
            timestamp: "2026-04-09T12:00:00Z".into(),
            expire_timeout: -1,
            read: false,
        }
    }

    fn default_dnd() -> DndConfig {
        DndConfig::default()
    }

    // ── DND Mode Tests ───────────────────────────────────────────────────

    #[test]
    fn test_dnd_off_allows_all() {
        let state = DndState::default();
        let config = default_dnd();
        let n = make_notification("app", Priority::Normal);
        assert_eq!(state.should_suppress(&n, &config, None), SuppressResult::Allow);
    }

    #[test]
    fn test_dnd_on_suppresses_normal() {
        let state = DndState::default();
        let mut config = default_dnd();
        config.mode = DndMode::Priority;
        let n = make_notification("app", Priority::Normal);
        assert_eq!(state.should_suppress(&n, &config, None), SuppressResult::Suppress);
    }

    #[test]
    fn test_dnd_on_allows_critical() {
        let state = DndState::default();
        let mut config = default_dnd();
        config.mode = DndMode::Priority;
        let n = make_notification("app", Priority::Critical);
        assert_eq!(state.should_suppress(&n, &config, None), SuppressResult::Allow);
    }

    #[test]
    fn test_dnd_on_allows_always_allow_app() {
        let state = DndState::default();
        let mut config = default_dnd();
        config.mode = DndMode::Priority;
        config.always_allow = vec!["phone".into()];
        let n = make_notification("phone", Priority::Normal);
        assert_eq!(state.should_suppress(&n, &config, None), SuppressResult::Allow);
    }

    #[test]
    fn test_always_suppress_overrides_dnd_off() {
        let state = DndState::default();
        let mut config = default_dnd();
        config.always_suppress = vec!["spammy-app".into()];
        let n = make_notification("spammy-app", Priority::Normal);
        assert_eq!(state.should_suppress(&n, &config, None), SuppressResult::Suppress);
    }

    #[test]
    fn test_dnd_total_drops_normal() {
        let state = DndState::default();
        let mut config = default_dnd();
        config.mode = DndMode::Total;
        let n = make_notification("app", Priority::Normal);
        assert_eq!(state.should_suppress(&n, &config, None), SuppressResult::Drop);
    }

    #[test]
    fn test_dnd_total_drops_critical() {
        let state = DndState::default();
        let mut config = default_dnd();
        config.mode = DndMode::Total;
        let n = make_notification("app", Priority::Critical);
        assert_eq!(state.should_suppress(&n, &config, None), SuppressResult::Drop);
    }

    #[test]
    fn test_dnd_total_respects_always_allow() {
        let state = DndState::default();
        let mut config = default_dnd();
        config.mode = DndMode::Total;
        config.always_allow = vec!["phone".into()];
        let n = make_notification("phone", Priority::Normal);
        assert_eq!(state.should_suppress(&n, &config, None), SuppressResult::Allow);
    }

    #[test]
    fn test_dnd_alarms_allows_alarm_category() {
        let state = DndState::default();
        let mut config = default_dnd();
        config.mode = DndMode::Alarms;
        let mut n = make_notification("app", Priority::Normal);
        n.category = "alarm.clock".into();
        assert_eq!(state.should_suppress(&n, &config, None), SuppressResult::Allow);
    }

    #[test]
    fn test_dnd_alarms_suppresses_non_alarm() {
        let state = DndState::default();
        let mut config = default_dnd();
        config.mode = DndMode::Alarms;
        let n = make_notification("app", Priority::Normal);
        assert_eq!(state.should_suppress(&n, &config, None), SuppressResult::Suppress);
    }

    #[test]
    fn test_dnd_expires_at_past_equals_off() {
        let state = DndState::default();
        let mut config = default_dnd();
        config.mode = DndMode::Total;
        // One second ago.
        config.expires_at = Some("2000-01-01T00:00:00Z".into());
        let n = make_notification("app", Priority::Normal);
        assert_eq!(state.should_suppress(&n, &config, None), SuppressResult::Allow);
    }

    #[test]
    fn test_dnd_expires_at_future_keeps_mode() {
        let state = DndState::default();
        let mut config = default_dnd();
        config.mode = DndMode::Total;
        config.expires_at = Some("2099-12-31T23:59:59Z".into());
        let n = make_notification("app", Priority::Normal);
        assert_eq!(state.should_suppress(&n, &config, None), SuppressResult::Drop);
    }

    #[test]
    fn test_dnd_expires_at_invalid_does_not_flip() {
        let state = DndState::default();
        let mut config = default_dnd();
        config.mode = DndMode::Total;
        config.expires_at = Some("not-a-date".into());
        let n = make_notification("app", Priority::Normal);
        // Invalid TTL kept as-is (fail-safe: stays silenced, user can fix).
        assert_eq!(state.should_suppress(&n, &config, None), SuppressResult::Drop);
    }

    #[test]
    fn test_app_enabled_false_drops() {
        let state = DndState::default();
        let config = default_dnd();
        let ovr = AppOverride {
            enabled: Some(false),
            ..Default::default()
        };
        let n = make_notification("blocked", Priority::Normal);
        assert_eq!(
            state.should_suppress(&n, &config, Some(&ovr)),
            SuppressResult::Drop
        );
    }

    #[test]
    fn test_app_enabled_false_drops_critical() {
        let state = DndState::default();
        let config = default_dnd();
        let ovr = AppOverride {
            enabled: Some(false),
            ..Default::default()
        };
        let n = make_notification("blocked", Priority::Critical);
        assert_eq!(
            state.should_suppress(&n, &config, Some(&ovr)),
            SuppressResult::Drop
        );
    }

    // ── Per-app Override Tests ───────────────────────────────────────────

    #[test]
    fn test_app_override_suppress() {
        let state = DndState::default();
        let config = default_dnd();
        let ovr = AppOverride {
            suppress: Some(true),
            ..Default::default()
        };
        let n = make_notification("app", Priority::Normal);
        assert_eq!(
            state.should_suppress(&n, &config, Some(&ovr)),
            SuppressResult::Suppress
        );
    }

    #[test]
    fn test_app_override_bypass_dnd() {
        let state = DndState::default();
        let mut config = default_dnd();
        config.mode = DndMode::Priority;
        let ovr = AppOverride {
            bypass_dnd: Some(true),
            ..Default::default()
        };
        let n = make_notification("important-app", Priority::Normal);
        assert_eq!(
            state.should_suppress(&n, &config, Some(&ovr)),
            SuppressResult::Allow
        );
    }

    #[test]
    fn test_app_override_suppress_beats_critical() {
        // Per-app suppress applies even to normal priority.
        // But critical should still pass? No: per-app suppress is checked
        // BEFORE priority. If app is suppressed, even critical is suppressed.
        let state = DndState::default();
        let config = default_dnd();
        let ovr = AppOverride {
            suppress: Some(true),
            ..Default::default()
        };
        let n = make_notification("app", Priority::Critical);
        assert_eq!(
            state.should_suppress(&n, &config, Some(&ovr)),
            SuppressResult::Suppress
        );
    }

    // ── Fullscreen Tests ─────────────────────────────────────────────────

    #[test]
    fn test_fullscreen_queues() {
        let mut state = DndState::default();
        state.fullscreen_active = true;
        let config = default_dnd(); // suppress_fullscreen = true by default
        let n = make_notification("app", Priority::Normal);
        assert_eq!(state.should_suppress(&n, &config, None), SuppressResult::Queue);
    }

    #[test]
    fn test_fullscreen_disabled() {
        let mut state = DndState::default();
        state.fullscreen_active = true;
        let mut config = default_dnd();
        config.suppress_fullscreen = false;
        let n = make_notification("app", Priority::Normal);
        assert_eq!(state.should_suppress(&n, &config, None), SuppressResult::Allow);
    }

    #[test]
    fn test_fullscreen_critical_passes() {
        let mut state = DndState::default();
        state.fullscreen_active = true;
        let config = default_dnd();
        let n = make_notification("app", Priority::Critical);
        // Critical is checked before fullscreen.
        assert_eq!(state.should_suppress(&n, &config, None), SuppressResult::Allow);
    }

    // ── Schedule Tests ───────────────────────────────────────────────────

    #[test]
    fn test_schedule_same_day_inside() {
        let schedule = DndSchedule {
            start: "09:00".into(),
            end: "17:00".into(),
            days: vec![],
            mode: ScheduleMode::Priority,
        };
        assert!(is_in_schedule_at(&schedule, 12, 0, 0)); // noon
        assert!(is_in_schedule_at(&schedule, 9, 0, 0));   // start
        assert!(!is_in_schedule_at(&schedule, 17, 0, 0)); // end (exclusive)
        assert!(!is_in_schedule_at(&schedule, 8, 59, 0)); // before
        assert!(!is_in_schedule_at(&schedule, 20, 0, 0)); // after
    }

    #[test]
    fn test_schedule_overnight() {
        let schedule = DndSchedule {
            start: "22:00".into(),
            end: "07:00".into(),
            days: vec![],
            mode: ScheduleMode::Priority,
        };
        assert!(is_in_schedule_at(&schedule, 23, 0, 0));  // late evening
        assert!(is_in_schedule_at(&schedule, 0, 0, 0));   // midnight
        assert!(is_in_schedule_at(&schedule, 3, 30, 0));  // middle of night
        assert!(is_in_schedule_at(&schedule, 6, 59, 0));  // just before end
        assert!(!is_in_schedule_at(&schedule, 7, 0, 0));  // end (exclusive)
        assert!(!is_in_schedule_at(&schedule, 12, 0, 0)); // daytime
        assert!(!is_in_schedule_at(&schedule, 21, 59, 0)); // just before start
    }

    #[test]
    fn test_schedule_weekday_filter() {
        let schedule = DndSchedule {
            start: "22:00".into(),
            end: "07:00".into(),
            days: vec![0, 1, 2, 3, 4], // Mon-Fri
            mode: ScheduleMode::Priority,
        };
        assert!(is_in_schedule_at(&schedule, 23, 0, 0));  // Monday
        assert!(is_in_schedule_at(&schedule, 23, 0, 4));  // Friday
        assert!(!is_in_schedule_at(&schedule, 23, 0, 5)); // Saturday
        assert!(!is_in_schedule_at(&schedule, 23, 0, 6)); // Sunday
    }

    #[test]
    fn test_schedule_invalid_time() {
        let schedule = DndSchedule {
            start: "invalid".into(),
            end: "07:00".into(),
            days: vec![],
            mode: ScheduleMode::Priority,
        };
        assert!(!is_in_schedule_at(&schedule, 23, 0, 0));
    }

    // ── Scheduled Mode Integration ───────────────────────────────────────

    #[test]
    fn test_dnd_scheduled_uses_schedule() {
        let state = DndState::default();
        let mut config = default_dnd();
        config.mode = DndMode::Scheduled;
        let n = make_notification("app", Priority::Normal);
        let _result = state.should_suppress(&n, &config, None);
    }

    // ── Focus Mode Suppression ──────────────────────────────────────────

    #[test]
    fn test_focus_suppresses_matching_app() {
        let mut state = DndState::default();
        state.focus.activate("proj".into(), vec!["slack".into()]);
        let config = default_dnd();
        let n = make_notification("slack", Priority::Normal);
        assert_eq!(state.should_suppress(&n, &config, None), SuppressResult::Suppress);
    }

    #[test]
    fn test_focus_allows_non_matching_app() {
        let mut state = DndState::default();
        state.focus.activate("proj".into(), vec!["slack".into()]);
        let config = default_dnd();
        let n = make_notification("signal", Priority::Normal);
        assert_eq!(state.should_suppress(&n, &config, None), SuppressResult::Allow);
    }

    #[test]
    fn test_focus_never_suppresses_critical() {
        let mut state = DndState::default();
        state.focus.activate("proj".into(), vec!["slack".into()]);
        let config = default_dnd();
        let n = make_notification("slack", Priority::Critical);
        assert_eq!(state.should_suppress(&n, &config, None), SuppressResult::Allow);
    }

    #[test]
    fn test_focus_inactive_allows_all() {
        let state = DndState::default();
        let config = default_dnd();
        let n = make_notification("slack", Priority::Normal);
        assert_eq!(state.should_suppress(&n, &config, None), SuppressResult::Allow);
    }

    #[test]
    fn test_focus_case_insensitive() {
        let mut state = DndState::default();
        state.focus.activate("proj".into(), vec!["Slack".into()]);
        let config = default_dnd();
        let n = make_notification("slack", Priority::Normal);
        assert_eq!(state.should_suppress(&n, &config, None), SuppressResult::Suppress);
    }

    #[test]
    fn test_always_allow_overrides_focus() {
        let mut state = DndState::default();
        state.focus.activate("proj".into(), vec!["phone".into()]);
        let mut config = default_dnd();
        config.always_allow = vec!["phone".into()];
        let n = make_notification("phone", Priority::Normal);
        // always_allow is checked before focus.
        assert_eq!(state.should_suppress(&n, &config, None), SuppressResult::Allow);
    }
}
