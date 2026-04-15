/// Focus Mode suppression state.
///
/// When Focus Mode is active for a project, notifications from
/// specified apps are demoted to `Suppress` (stored but no toast).
/// Critical notifications always pass through.

use std::collections::HashSet;

/// Ephemeral focus suppression state (not persisted).
#[derive(Debug, Default)]
pub struct FocusSuppression {
    /// Project UUID when focus is active, `None` otherwise.
    active_project_id: Option<String>,
    /// App identifiers to suppress (lowercase for matching).
    suppressed_apps: HashSet<String>,
}

impl FocusSuppression {
    /// Activate focus suppression.
    pub fn activate(&mut self, project_id: String, suppress_apps: Vec<String>) {
        self.active_project_id = Some(project_id);
        self.suppressed_apps = suppress_apps
            .into_iter()
            .map(|a| a.to_lowercase())
            .collect();
        tracing::info!(
            "focus suppression activated: {} apps",
            self.suppressed_apps.len()
        );
    }

    /// Deactivate focus suppression.
    pub fn deactivate(&mut self) {
        let count = self.suppressed_apps.len();
        self.active_project_id = None;
        self.suppressed_apps.clear();
        tracing::info!("focus suppression deactivated ({count} apps cleared)");
    }

    /// Whether focus mode is active.
    pub fn is_active(&self) -> bool {
        self.active_project_id.is_some()
    }

    /// Whether `app_name` is suppressed by the current focus session.
    ///
    /// Matching is case-insensitive. Returns `false` when focus is inactive.
    pub fn is_suppressed(&self, app_name: &str) -> bool {
        if !self.is_active() {
            return false;
        }
        self.suppressed_apps.contains(&app_name.to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_inactive() {
        let f = FocusSuppression::default();
        assert!(!f.is_active());
        assert!(!f.is_suppressed("any"));
    }

    #[test]
    fn activate_sets_state() {
        let mut f = FocusSuppression::default();
        f.activate("proj-1".into(), vec!["slack".into(), "discord".into()]);
        assert!(f.is_active());
        assert_eq!(f.suppressed_apps.len(), 2);
    }

    #[test]
    fn is_suppressed_exact() {
        let mut f = FocusSuppression::default();
        f.activate("p".into(), vec!["slack".into(), "discord".into()]);
        assert!(f.is_suppressed("slack"));
        assert!(f.is_suppressed("discord"));
        assert!(!f.is_suppressed("signal"));
    }

    #[test]
    fn is_suppressed_case_insensitive() {
        let mut f = FocusSuppression::default();
        f.activate("p".into(), vec!["Slack".into()]);
        assert!(f.is_suppressed("slack"));
        assert!(f.is_suppressed("SLACK"));
        assert!(f.is_suppressed("Slack"));
    }

    #[test]
    fn deactivate_clears() {
        let mut f = FocusSuppression::default();
        f.activate("p".into(), vec!["slack".into()]);
        f.deactivate();
        assert!(!f.is_active());
        assert!(!f.is_suppressed("slack"));
    }

    #[test]
    fn inactive_never_suppresses() {
        let f = FocusSuppression::default();
        assert!(!f.is_suppressed("slack"));
    }

    #[test]
    fn empty_suppress_list() {
        let mut f = FocusSuppression::default();
        f.activate("p".into(), vec![]);
        assert!(f.is_active());
        assert!(!f.is_suppressed("any"));
    }

    #[test]
    fn reactivate_replaces() {
        let mut f = FocusSuppression::default();
        f.activate("p1".into(), vec!["slack".into()]);
        f.activate("p2".into(), vec!["teams".into()]);
        assert!(!f.is_suppressed("slack"));
        assert!(f.is_suppressed("teams"));
    }
}
