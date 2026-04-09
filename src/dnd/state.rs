/// DND state machine and suppression logic.
///
/// Evaluates whether a notification should be shown, suppressed, or
/// queued based on DND mode, schedule, per-app overrides, and
/// fullscreen state.

use chrono::{Datelike, Local, NaiveTime, Weekday};

use crate::config::{AppOverride, DndConfig, DndMode};
use crate::dbus::server::{Notification, Priority};

/// Result of checking whether a notification should be suppressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuppressResult {
    /// Show the notification normally.
    Allow,
    /// Suppress the toast but store the notification.
    Suppress,
    /// Queue for later (e.g. fullscreen exit). Store and defer toast.
    Queue,
}

/// Runtime DND state.
#[derive(Debug)]
pub struct DndState {
    /// Whether a fullscreen window is currently active.
    pub fullscreen_active: bool,
}

impl Default for DndState {
    fn default() -> Self {
        Self {
            fullscreen_active: false,
        }
    }
}

impl DndState {
    /// Determine whether a notification should be suppressed.
    ///
    /// Checks in order:
    /// 1. Per-app suppress override
    /// 2. Per-app bypass_dnd override
    /// 3. Critical notifications always pass
    /// 4. DND mode (off/on/scheduled)
    /// 5. Fullscreen suppression
    pub fn should_suppress(
        &self,
        notification: &Notification,
        dnd_config: &DndConfig,
        app_override: Option<&AppOverride>,
    ) -> SuppressResult {
        // 1. Per-app force suppress.
        if let Some(ovr) = app_override {
            if ovr.suppress == Some(true) {
                return SuppressResult::Suppress;
            }
        }

        // 2. Always-suppress list.
        if dnd_config
            .always_suppress
            .iter()
            .any(|a| a == &notification.app_name)
        {
            return SuppressResult::Suppress;
        }

        // 3. Critical notifications always pass (unless per-app suppressed above).
        if notification.priority == Priority::Critical {
            return SuppressResult::Allow;
        }

        // 4. Per-app bypass DND.
        if let Some(ovr) = app_override {
            if ovr.bypass_dnd == Some(true) {
                // Skip DND check, but still check fullscreen below.
                return self.check_fullscreen(dnd_config);
            }
        }

        // 5. Always-allow list bypasses DND.
        if dnd_config
            .always_allow
            .iter()
            .any(|a| a == &notification.app_name)
        {
            return self.check_fullscreen(dnd_config);
        }

        // 6. DND mode check.
        let dnd_active = match dnd_config.mode {
            DndMode::Off => false,
            DndMode::On => true,
            DndMode::Scheduled => is_in_schedule(&dnd_config.schedule),
        };

        if dnd_active {
            return SuppressResult::Suppress;
        }

        // 7. Fullscreen suppression.
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
        config.mode = DndMode::On;
        let n = make_notification("app", Priority::Normal);
        assert_eq!(state.should_suppress(&n, &config, None), SuppressResult::Suppress);
    }

    #[test]
    fn test_dnd_on_allows_critical() {
        let state = DndState::default();
        let mut config = default_dnd();
        config.mode = DndMode::On;
        let n = make_notification("app", Priority::Critical);
        assert_eq!(state.should_suppress(&n, &config, None), SuppressResult::Allow);
    }

    #[test]
    fn test_dnd_on_allows_always_allow_app() {
        let state = DndState::default();
        let mut config = default_dnd();
        config.mode = DndMode::On;
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
        config.mode = DndMode::On;
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
        };
        assert!(!is_in_schedule_at(&schedule, 23, 0, 0));
    }

    // ── Scheduled Mode Integration ───────────────────────────────────────

    #[test]
    fn test_dnd_scheduled_uses_schedule() {
        // This test uses is_in_schedule_at via the state machine indirectly.
        // We can't easily control Local::now(), so we test the schedule
        // function directly above and trust the integration.
        let state = DndState::default();
        let mut config = default_dnd();
        config.mode = DndMode::Scheduled;
        // Schedule is 22:00-07:00 by default. Whether DND is active
        // depends on the current local time.
        let n = make_notification("app", Priority::Normal);
        // Just verify it doesn't panic.
        let _result = state.should_suppress(&n, &config, None);
    }
}
