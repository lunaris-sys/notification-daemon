/// Per-app and global rate limiting for notifications.
///
/// Prevents notification spam by enforcing:
/// - Per-app: max 10 notifications per second
/// - Global: max 50 notifications per second

use std::collections::HashMap;
use std::time::{Duration, Instant};

const PER_APP_LIMIT: u32 = 10;
const GLOBAL_LIMIT: u32 = 50;
const WINDOW: Duration = Duration::from_secs(1);

/// Sliding-window rate limiter.
pub struct RateLimiter {
    per_app: HashMap<String, Vec<Instant>>,
    global: Vec<Instant>,
}

impl RateLimiter {
    /// Create a new rate limiter.
    pub fn new() -> Self {
        Self {
            per_app: HashMap::new(),
            global: Vec::new(),
        }
    }

    /// Check whether a notification from `app_name` is allowed.
    ///
    /// Returns `true` if allowed, `false` if rate-limited.
    /// Automatically records the attempt if allowed.
    pub fn check(&mut self, app_name: &str) -> bool {
        let now = Instant::now();
        self.prune(now);

        // Global check.
        if self.global.len() >= GLOBAL_LIMIT as usize {
            return false;
        }

        // Per-app check.
        let app_times = self.per_app.entry(app_name.to_string()).or_default();
        if app_times.len() >= PER_APP_LIMIT as usize {
            return false;
        }

        // Record.
        self.global.push(now);
        app_times.push(now);
        true
    }

    /// Remove timestamps older than the window.
    fn prune(&mut self, now: Instant) {
        let cutoff = now - WINDOW;
        self.global.retain(|t| *t > cutoff);
        for times in self.per_app.values_mut() {
            times.retain(|t| *t > cutoff);
        }
    }

    /// Check with custom limits (for testing).
    pub fn check_with_limits(
        &mut self,
        app_name: &str,
        per_app_limit: u32,
        global_limit: u32,
    ) -> bool {
        let now = Instant::now();
        self.prune(now);

        if self.global.len() >= global_limit as usize {
            return false;
        }
        let app_times = self.per_app.entry(app_name.to_string()).or_default();
        if app_times.len() >= per_app_limit as usize {
            return false;
        }
        self.global.push(now);
        app_times.push(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allows_normal_rate() {
        let mut rl = RateLimiter::new();
        for _ in 0..10 {
            assert!(rl.check("app"));
        }
    }

    #[test]
    fn test_per_app_limit() {
        let mut rl = RateLimiter::new();
        for _ in 0..10 {
            assert!(rl.check("spammy"));
        }
        assert!(!rl.check("spammy"), "11th should be rejected");
        // Different app still allowed.
        assert!(rl.check("other"));
    }

    #[test]
    fn test_global_limit() {
        let mut rl = RateLimiter::new();
        // Fill global with different apps (each under per-app limit).
        for i in 0..50 {
            assert!(rl.check_with_limits(&format!("app{i}"), 10, 50));
        }
        assert!(!rl.check_with_limits("app-extra", 10, 50));
    }

    #[test]
    fn test_window_reset() {
        let mut rl = RateLimiter::new();
        // Exhaust limit.
        for _ in 0..10 {
            rl.check("app");
        }
        assert!(!rl.check("app"));

        // Simulate time passing by clearing timestamps manually.
        rl.per_app.get_mut("app").unwrap().clear();
        rl.global.clear();

        assert!(rl.check("app"), "should allow after window reset");
    }
}
