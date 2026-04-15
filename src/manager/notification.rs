/// Central notification manager.
///
/// Coordinates between D-Bus input, DND evaluation, rate limiting,
/// storage, and client broadcasting.

use std::sync::Arc;

use tokio::sync::{broadcast, Mutex};

use crate::config::{Config, DndMode};
use crate::dbus::server::{CloseReason, Notification, NotifyEvent, determine_priority};
use crate::dnd::{DndState, SuppressResult};
use crate::manager::grouping::derive_group_key;
use crate::manager::rate_limiter::RateLimiter;
use crate::manager::validation::sanitize_input;
use crate::storage::Database;

/// Central coordinator for the notification daemon.
pub struct NotificationManager {
    db: Arc<Database>,
    dnd_state: Arc<Mutex<DndState>>,
    dnd_mode: Arc<Mutex<DndMode>>,
    config: Arc<Mutex<Config>>,
    rate_limiter: Mutex<RateLimiter>,
    events: broadcast::Sender<NotifyEvent>,
    /// Queued notifications waiting for fullscreen exit.
    fullscreen_queue: Mutex<Vec<Notification>>,
}

impl NotificationManager {
    /// Create a new notification manager.
    pub fn new(
        db: Arc<Database>,
        config: Arc<Mutex<Config>>,
        events: broadcast::Sender<NotifyEvent>,
    ) -> Self {
        Self {
            db,
            dnd_state: Arc::new(Mutex::new(DndState::default())),
            dnd_mode: Arc::new(Mutex::new(DndMode::Off)),
            config,
            rate_limiter: Mutex::new(RateLimiter::new()),
            events,
            fullscreen_queue: Mutex::new(Vec::new()),
        }
    }

    /// Get the shared DND mode reference (for socket server).
    pub fn dnd_mode(&self) -> Arc<Mutex<DndMode>> {
        self.dnd_mode.clone()
    }

    /// Handle an incoming notification from D-Bus.
    ///
    /// Returns the notification ID if stored, or 0 if rate-limited.
    pub async fn handle_notify(
        &self,
        id: u32,
        app_name: &str,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[String],
        urgency: u8,
        category: &str,
        expire_timeout: i32,
    ) -> u32 {
        // 1. Validate/sanitize input.
        let input = sanitize_input(app_name, summary, body, app_icon, actions);

        // 2. Rate limit.
        {
            let mut rl = self.rate_limiter.lock().await;
            if !rl.check(&input.app_name) {
                tracing::warn!(
                    app = %input.app_name,
                    "rate limited, dropping notification"
                );
                return 0;
            }
        }

        // 3. Determine priority.
        let priority = determine_priority(urgency, expire_timeout, category);

        // 4. Build notification.
        let notification = Notification {
            id,
            app_name: input.app_name.clone(),
            summary: input.summary,
            body: input.body,
            app_icon: input.app_icon,
            actions: input
                .actions
                .chunks(2)
                .filter_map(|c| {
                    if c.len() == 2 {
                        Some((c[0].clone(), c[1].clone()))
                    } else {
                        None
                    }
                })
                .collect(),
            priority,
            urgency,
            category: category.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            expire_timeout,
            read: false,
        };

        // 5. Derive group key (used by shell for visual grouping).
        let group_key = derive_group_key(&notification);

        // 6. Evaluate DND/app rules BEFORE storage so `enabled = false`
        // and `DndMode::Total` can cleanly drop the notification.
        let (suppress_result, history_enabled) = {
            let config = self.config.lock().await;
            let app_override = config.apps.get(&input.app_name).cloned();

            // Mirror current effective mode into the shared Arc so the
            // socket server can answer `GetDnd` without re-reading config.
            let mut mode = self.dnd_mode.lock().await;
            *mode = config.dnd.mode;
            drop(mode);

            let dnd_state = self.dnd_state.lock().await;
            let result =
                dnd_state.should_suppress(&notification, &config.dnd, app_override.as_ref());
            (result, config.history.enabled)
        };

        // 7. Persist unless Drop, OR history is disabled entirely.
        tracing::debug!(
            id,
            app = %input.app_name,
            ?suppress_result,
            history_enabled,
            "manager: about to decide storage"
        );
        if history_enabled && suppress_result != SuppressResult::Drop {
            match self.db.insert_notification(&notification).await {
                Ok(_) => tracing::debug!(id, "manager: stored in SQLite"),
                Err(e) => tracing::error!(id, "manager: insert failed: {e}"),
            }
        } else if !history_enabled {
            tracing::debug!(id, "manager: skipped storage (history disabled)");
        } else {
            tracing::debug!(id, "manager: skipped storage (DND drop)");
        }

        // 8. Act on result.
        match suppress_result {
            SuppressResult::Allow => {
                tracing::info!(id, %group_key, "notification broadcast");
                let _ = self.events.send(NotifyEvent::Added(notification));
            }
            SuppressResult::Suppress => {
                tracing::debug!(id, %group_key, "notification suppressed by DND");
            }
            SuppressResult::Queue => {
                tracing::debug!(id, %group_key, "notification queued (fullscreen)");
                self.fullscreen_queue.lock().await.push(notification);
            }
            SuppressResult::Drop => {
                tracing::debug!(id, %group_key, "notification dropped (blocked)");
            }
        }

        id
    }

    /// Handle closing a notification.
    pub async fn handle_close(&self, id: u32, reason: CloseReason) {
        self.db.dismiss(id, reason).await.ok();
        let _ = self.events.send(NotifyEvent::Closed { id, reason });
    }

    /// Set fullscreen state. Flushes queue on exit.
    pub async fn set_fullscreen(&self, active: bool) {
        self.dnd_state.lock().await.fullscreen_active = active;

        if !active {
            self.flush_fullscreen_queue().await;
        }
    }

    /// Flush queued notifications (max 5, for fullscreen exit).
    async fn flush_fullscreen_queue(&self) {
        let mut queue = self.fullscreen_queue.lock().await;
        let to_send: Vec<Notification> = queue.drain(..).take(5).collect();
        drop(queue);

        for n in to_send {
            let _ = self.events.send(NotifyEvent::Added(n));
        }
    }

    /// Get unread count.
    pub async fn unread_count(&self) -> u32 {
        self.db.count_pending().await.unwrap_or(0)
    }

    /// Run retention cleanup.
    ///
    /// When `history.enabled = false` the database is wiped on every
    /// tick so nothing ever accumulates. Otherwise the configured
    /// age/count limits are enforced.
    pub async fn cleanup(&self) {
        let (enabled, max_age, max_count) = {
            let config = self.config.lock().await;
            (
                config.history.enabled,
                config.history.max_age_days,
                config.history.max_count,
            )
        };

        if !enabled {
            match self.db.cleanup(0, 0).await {
                Ok(n) if n > 0 => tracing::info!("history disabled: wiped {n} notifications"),
                Ok(_) => {}
                Err(e) => tracing::warn!("history wipe failed: {e}"),
            }
            return;
        }

        match self.db.cleanup(max_age, max_count).await {
            Ok(n) if n > 0 => tracing::info!("retention cleanup: removed {n} notifications"),
            Ok(_) => {}
            Err(e) => tracing::warn!("retention cleanup failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn make_manager() -> (NotificationManager, broadcast::Receiver<NotifyEvent>) {
        let db = Arc::new(Database::open_memory().await.unwrap());
        let config = Arc::new(Mutex::new(Config::default()));
        let (tx, rx) = broadcast::channel(64);
        let mgr = NotificationManager::new(db, config, tx);
        (mgr, rx)
    }

    #[tokio::test]
    async fn test_handle_notify_stores_and_broadcasts() {
        let (mgr, mut rx) = make_manager().await;

        let id = mgr
            .handle_notify(1, "Firefox", "", "Done", "file.zip", &[], 1, "", -1)
            .await;
        assert_eq!(id, 1);

        // Should be in DB.
        let n = mgr.db.get_notification(1).await.unwrap().unwrap();
        assert_eq!(n.summary, "Done");

        // Should have broadcast.
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, NotifyEvent::Added(_)));
    }

    #[tokio::test]
    async fn test_handle_notify_dnd_suppresses() {
        let db = Arc::new(Database::open_memory().await.unwrap());
        let mut config = Config::default();
        config.dnd.mode = DndMode::Priority;
        let config = Arc::new(Mutex::new(config));
        let (tx, mut rx) = broadcast::channel(64);
        let mgr = NotificationManager::new(db, config, tx);

        let id = mgr
            .handle_notify(1, "App", "", "Hello", "", &[], 1, "", -1)
            .await;
        assert_eq!(id, 1);

        // Should be in DB (stored even if suppressed).
        assert!(mgr.db.get_notification(1).await.unwrap().is_some());

        // Should NOT have broadcast.
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_handle_notify_critical_bypasses_dnd() {
        let db = Arc::new(Database::open_memory().await.unwrap());
        let mut config = Config::default();
        config.dnd.mode = DndMode::Priority;
        let config = Arc::new(Mutex::new(config));
        let (tx, mut rx) = broadcast::channel(64);
        let mgr = NotificationManager::new(db, config, tx);

        mgr.handle_notify(1, "App", "", "ALERT", "", &[], 2, "", -1)
            .await;

        // Critical should broadcast even with DND on.
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, NotifyEvent::Added(_)));
    }

    #[tokio::test]
    async fn test_handle_notify_rate_limited() {
        let (mgr, _rx) = make_manager().await;

        for i in 1..=10 {
            let id = mgr
                .handle_notify(i, "Spammy", "", "msg", "", &[], 1, "", -1)
                .await;
            assert_eq!(id, i);
        }

        // 11th should be rate-limited (returns 0).
        let id = mgr
            .handle_notify(11, "Spammy", "", "msg", "", &[], 1, "", -1)
            .await;
        assert_eq!(id, 0);
    }

    #[tokio::test]
    async fn test_handle_close() {
        let (mgr, mut rx) = make_manager().await;
        mgr.handle_notify(1, "App", "", "Hello", "", &[], 1, "", -1)
            .await;
        let _ = rx.try_recv(); // Drain the Added event.

        mgr.handle_close(1, CloseReason::Dismissed).await;

        let event = rx.try_recv().unwrap();
        assert!(matches!(event, NotifyEvent::Closed { id: 1, .. }));

        // Should be dismissed in DB.
        assert_eq!(mgr.db.count_pending().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_fullscreen_queues_and_flushes() {
        let (mgr, mut rx) = make_manager().await;
        mgr.set_fullscreen(true).await;

        mgr.handle_notify(1, "App", "", "Hello", "", &[], 1, "", -1)
            .await;

        // Should NOT broadcast (queued).
        assert!(rx.try_recv().is_err());

        // Exit fullscreen -> flush.
        mgr.set_fullscreen(false).await;

        let event = rx.try_recv().unwrap();
        assert!(matches!(event, NotifyEvent::Added(_)));
    }

    #[tokio::test]
    async fn test_input_sanitization() {
        let (mgr, _rx) = make_manager().await;
        let long_name = "X".repeat(200);

        mgr.handle_notify(1, &long_name, "", "", "body", &[], 1, "", -1)
            .await;

        let n = mgr.db.get_notification(1).await.unwrap().unwrap();
        assert_eq!(n.app_name.len(), 50); // Truncated.
        assert_eq!(n.summary, n.app_name); // Empty summary -> app_name.
    }
}
