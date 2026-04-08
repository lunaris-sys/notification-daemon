/// SQLite storage backend for notifications.
///
/// Uses sqlx with an async connection pool. Supports in-memory databases
/// for testing and file-based databases for production.

use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use sqlx::Row;

use crate::dbus::server::{CloseReason, Notification, Priority};
use crate::error::NotifyError;
use crate::storage::models::NotificationRow;

const SCHEMA_VERSION: i64 = 1;

/// Persistent notification database.
pub struct Database {
    pool: SqlitePool,
}

impl Database {
    /// Open or create a database at the given path.
    ///
    /// Use `:memory:` for an in-memory database (tests).
    pub async fn open(url: &str) -> Result<Self, NotifyError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect(url)
            .await
            .map_err(|e| NotifyError::Db(e.to_string()))?;

        let db = Self { pool };
        db.init().await?;
        Ok(db)
    }

    /// Open an in-memory database (for tests).
    pub async fn open_memory() -> Result<Self, NotifyError> {
        // Use a unique file URI with shared cache so the pool shares one DB.
        let name = uuid::Uuid::new_v4();
        let url = format!("sqlite:file:{name}?mode=memory&cache=shared");
        Self::open(&url).await
    }

    /// Initialize the schema.
    async fn init(&self) -> Result<(), NotifyError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS schema_version (
                version INTEGER NOT NULL
            )",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| NotifyError::Db(e.to_string()))?;

        let current: Option<i64> =
            sqlx::query_scalar("SELECT version FROM schema_version LIMIT 1")
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| NotifyError::Db(e.to_string()))?;

        if current.is_none() {
            self.create_tables().await?;
            sqlx::query("INSERT INTO schema_version (version) VALUES (?)")
                .bind(SCHEMA_VERSION)
                .execute(&self.pool)
                .await
                .map_err(|e| NotifyError::Db(e.to_string()))?;
        }

        Ok(())
    }

    /// Create all notification tables.
    async fn create_tables(&self) -> Result<(), NotifyError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS notifications (
                id          INTEGER PRIMARY KEY,
                app_name    TEXT NOT NULL DEFAULT '',
                summary     TEXT NOT NULL DEFAULT '',
                body        TEXT NOT NULL DEFAULT '',
                app_icon    TEXT NOT NULL DEFAULT '',
                actions     TEXT NOT NULL DEFAULT '[]',
                priority    TEXT NOT NULL DEFAULT 'normal',
                urgency     INTEGER NOT NULL DEFAULT 1,
                category    TEXT NOT NULL DEFAULT '',
                timestamp   TEXT NOT NULL,
                expire_timeout INTEGER NOT NULL DEFAULT -1,
                read        INTEGER NOT NULL DEFAULT 0,
                dismissed   INTEGER NOT NULL DEFAULT 0,
                close_reason INTEGER
            )",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| NotifyError::Db(e.to_string()))?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_notifications_timestamp
             ON notifications (timestamp DESC)",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| NotifyError::Db(e.to_string()))?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_notifications_app
             ON notifications (app_name)",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| NotifyError::Db(e.to_string()))?;

        Ok(())
    }

    // ── CRUD ─────────────────────────────────────────────────────────────

    /// Insert a notification. If the ID already exists, replace it.
    pub async fn insert_notification(&self, n: &Notification) -> Result<(), NotifyError> {
        let row = NotificationRow::from_notification(n);
        sqlx::query(
            "INSERT OR REPLACE INTO notifications
             (id, app_name, summary, body, app_icon, actions, priority,
              urgency, category, timestamp, expire_timeout, read, dismissed, close_reason)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(row.id)
        .bind(&row.app_name)
        .bind(&row.summary)
        .bind(&row.body)
        .bind(&row.app_icon)
        .bind(&row.actions_json)
        .bind(&row.priority)
        .bind(row.urgency)
        .bind(&row.category)
        .bind(&row.timestamp)
        .bind(row.expire_timeout)
        .bind(row.read as i64)
        .bind(row.dismissed as i64)
        .bind(row.close_reason)
        .execute(&self.pool)
        .await
        .map_err(|e| NotifyError::Db(e.to_string()))?;
        Ok(())
    }

    /// Get a single notification by ID.
    pub async fn get_notification(&self, id: u32) -> Result<Option<Notification>, NotifyError> {
        let row = sqlx::query(
            "SELECT * FROM notifications WHERE id = ?",
        )
        .bind(id as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| NotifyError::Db(e.to_string()))?;

        Ok(row.map(|r| row_to_notification(&r)))
    }

    /// Get all pending (not dismissed) notifications, newest first.
    pub async fn get_pending(&self) -> Result<Vec<Notification>, NotifyError> {
        let rows = sqlx::query(
            "SELECT * FROM notifications WHERE dismissed = 0 ORDER BY timestamp DESC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| NotifyError::Db(e.to_string()))?;

        Ok(rows.iter().map(row_to_notification).collect())
    }

    /// Get notification history with pagination.
    ///
    /// `limit`: max results. `before`: ISO timestamp for cursor pagination.
    /// `app_name`: optional filter by app.
    pub async fn get_history(
        &self,
        limit: u32,
        before: Option<&str>,
        app_name: Option<&str>,
    ) -> Result<Vec<Notification>, NotifyError> {
        let before_ts = before.unwrap_or("9999-12-31T23:59:59Z");

        let rows = if let Some(app) = app_name {
            sqlx::query(
                "SELECT * FROM notifications
                 WHERE timestamp < ? AND app_name = ?
                 ORDER BY timestamp DESC LIMIT ?",
            )
            .bind(before_ts)
            .bind(app)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query(
                "SELECT * FROM notifications
                 WHERE timestamp < ?
                 ORDER BY timestamp DESC LIMIT ?",
            )
            .bind(before_ts)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await
        }
        .map_err(|e| NotifyError::Db(e.to_string()))?;

        Ok(rows.iter().map(row_to_notification).collect())
    }

    /// Mark a notification as read.
    pub async fn mark_read(&self, id: u32) -> Result<bool, NotifyError> {
        let result = sqlx::query("UPDATE notifications SET read = 1 WHERE id = ?")
            .bind(id as i64)
            .execute(&self.pool)
            .await
            .map_err(|e| NotifyError::Db(e.to_string()))?;
        Ok(result.rows_affected() > 0)
    }

    /// Dismiss a notification with a reason.
    pub async fn dismiss(&self, id: u32, reason: CloseReason) -> Result<bool, NotifyError> {
        let result = sqlx::query(
            "UPDATE notifications SET dismissed = 1, close_reason = ? WHERE id = ?",
        )
        .bind(reason as i64)
        .bind(id as i64)
        .execute(&self.pool)
        .await
        .map_err(|e| NotifyError::Db(e.to_string()))?;
        Ok(result.rows_affected() > 0)
    }

    /// Delete notifications older than `max_age_days` or exceeding `max_count`.
    ///
    /// Returns the number of deleted rows.
    pub async fn cleanup(
        &self,
        max_age_days: u32,
        max_count: u32,
    ) -> Result<u64, NotifyError> {
        let mut deleted = 0u64;

        // Delete by age.
        if max_age_days > 0 {
            let cutoff =
                chrono::Utc::now() - chrono::Duration::days(max_age_days as i64);
            let cutoff_str = cutoff.to_rfc3339();
            let result = sqlx::query(
                "DELETE FROM notifications WHERE timestamp < ? AND dismissed = 1",
            )
            .bind(&cutoff_str)
            .execute(&self.pool)
            .await
            .map_err(|e| NotifyError::Db(e.to_string()))?;
            deleted += result.rows_affected();
        }

        // Delete by count (keep newest max_count).
        if max_count > 0 {
            let result = sqlx::query(
                "DELETE FROM notifications WHERE id NOT IN (
                    SELECT id FROM notifications ORDER BY timestamp DESC LIMIT ?
                 )",
            )
            .bind(max_count as i64)
            .execute(&self.pool)
            .await
            .map_err(|e| NotifyError::Db(e.to_string()))?;
            deleted += result.rows_affected();
        }

        Ok(deleted)
    }

    /// Count pending (not dismissed) notifications.
    pub async fn count_pending(&self) -> Result<u32, NotifyError> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE dismissed = 0")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| NotifyError::Db(e.to_string()))?;
        Ok(count as u32)
    }
}

/// Convert a sqlx Row to a Notification.
fn row_to_notification(row: &sqlx::sqlite::SqliteRow) -> Notification {
    let actions_json: String = row.get("actions");
    let actions: Vec<(String, String)> =
        serde_json::from_str(&actions_json).unwrap_or_default();
    let priority_str: String = row.get("priority");
    let read_i: i64 = row.get("read");

    Notification {
        id: row.get::<i64, _>("id") as u32,
        app_name: row.get("app_name"),
        summary: row.get("summary"),
        body: row.get("body"),
        app_icon: row.get("app_icon"),
        actions,
        priority: match priority_str.as_str() {
            "low" => Priority::Low,
            "high" => Priority::High,
            "critical" => Priority::Critical,
            _ => Priority::Normal,
        },
        urgency: row.get::<i64, _>("urgency") as u8,
        category: row.get("category"),
        timestamp: row.get("timestamp"),
        expire_timeout: row.get::<i64, _>("expire_timeout") as i32,
        read: read_i != 0,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_notification(id: u32, app: &str, summary: &str) -> Notification {
        Notification {
            id,
            app_name: app.into(),
            summary: summary.into(),
            body: "body".into(),
            app_icon: "".into(),
            actions: vec![],
            priority: Priority::Normal,
            urgency: 1,
            category: "".into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            expire_timeout: -1,
            read: false,
        }
    }

    #[tokio::test]
    async fn test_open_memory() {
        let db = Database::open_memory().await.unwrap();
        let count = db.count_pending().await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_insert_and_get() {
        let db = Database::open_memory().await.unwrap();
        let n = make_notification(1, "Firefox", "Download done");
        db.insert_notification(&n).await.unwrap();

        let fetched = db.get_notification(1).await.unwrap().unwrap();
        assert_eq!(fetched.id, 1);
        assert_eq!(fetched.app_name, "Firefox");
        assert_eq!(fetched.summary, "Download done");
    }

    #[tokio::test]
    async fn test_get_nonexistent() {
        let db = Database::open_memory().await.unwrap();
        let fetched = db.get_notification(999).await.unwrap();
        assert!(fetched.is_none());
    }

    #[tokio::test]
    async fn test_replace_notification() {
        let db = Database::open_memory().await.unwrap();
        let mut n = make_notification(1, "App", "Old");
        db.insert_notification(&n).await.unwrap();

        n.summary = "New".into();
        db.insert_notification(&n).await.unwrap();

        let fetched = db.get_notification(1).await.unwrap().unwrap();
        assert_eq!(fetched.summary, "New");

        // Should still be only 1 row.
        assert_eq!(db.count_pending().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn test_get_pending() {
        let db = Database::open_memory().await.unwrap();
        db.insert_notification(&make_notification(1, "A", "a"))
            .await
            .unwrap();
        db.insert_notification(&make_notification(2, "B", "b"))
            .await
            .unwrap();

        // Dismiss one.
        db.dismiss(1, CloseReason::Dismissed).await.unwrap();

        let pending = db.get_pending().await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, 2);
    }

    #[tokio::test]
    async fn test_mark_read() {
        let db = Database::open_memory().await.unwrap();
        db.insert_notification(&make_notification(1, "A", "a"))
            .await
            .unwrap();

        assert!(db.mark_read(1).await.unwrap());
        let n = db.get_notification(1).await.unwrap().unwrap();
        assert!(n.read);

        // Non-existent returns false.
        assert!(!db.mark_read(999).await.unwrap());
    }

    #[tokio::test]
    async fn test_dismiss() {
        let db = Database::open_memory().await.unwrap();
        db.insert_notification(&make_notification(1, "A", "a"))
            .await
            .unwrap();

        assert!(db.dismiss(1, CloseReason::Expired).await.unwrap());
        assert_eq!(db.count_pending().await.unwrap(), 0);

        // Still in DB (just dismissed).
        let n = db.get_notification(1).await.unwrap();
        assert!(n.is_some());
    }

    #[tokio::test]
    async fn test_get_history_with_limit() {
        let db = Database::open_memory().await.unwrap();
        for i in 1..=10 {
            let mut n = make_notification(i, "App", &format!("msg {i}"));
            // Offset timestamps so ordering is deterministic.
            n.timestamp = format!("2026-04-09T12:00:{:02}Z", i);
            db.insert_notification(&n).await.unwrap();
        }

        let history = db.get_history(5, None, None).await.unwrap();
        assert_eq!(history.len(), 5);
        // Newest first.
        assert_eq!(history[0].id, 10);
        assert_eq!(history[4].id, 6);
    }

    #[tokio::test]
    async fn test_get_history_by_app() {
        let db = Database::open_memory().await.unwrap();
        db.insert_notification(&make_notification(1, "Firefox", "a"))
            .await
            .unwrap();
        db.insert_notification(&make_notification(2, "Spotify", "b"))
            .await
            .unwrap();
        db.insert_notification(&make_notification(3, "Firefox", "c"))
            .await
            .unwrap();

        let history = db
            .get_history(10, None, Some("Firefox"))
            .await
            .unwrap();
        assert_eq!(history.len(), 2);
        assert!(history.iter().all(|n| n.app_name == "Firefox"));
    }

    #[tokio::test]
    async fn test_get_history_cursor_pagination() {
        let db = Database::open_memory().await.unwrap();
        for i in 1..=5 {
            let mut n = make_notification(i, "App", &format!("msg {i}"));
            n.timestamp = format!("2026-04-09T12:00:{:02}Z", i);
            db.insert_notification(&n).await.unwrap();
        }

        // Get page 1 (newest 2).
        let page1 = db.get_history(2, None, None).await.unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].id, 5);
        assert_eq!(page1[1].id, 4);

        // Get page 2 (before oldest of page 1).
        let page2 = db
            .get_history(2, Some(&page1[1].timestamp), None)
            .await
            .unwrap();
        assert_eq!(page2.len(), 2);
        assert_eq!(page2[0].id, 3);
        assert_eq!(page2[1].id, 2);
    }

    #[tokio::test]
    async fn test_cleanup_by_count() {
        let db = Database::open_memory().await.unwrap();
        for i in 1..=10 {
            let mut n = make_notification(i, "App", &format!("msg {i}"));
            n.timestamp = format!("2026-04-09T12:00:{:02}Z", i);
            db.insert_notification(&n).await.unwrap();
        }

        // Keep only 5.
        let deleted = db.cleanup(0, 5).await.unwrap();
        assert_eq!(deleted, 5);

        let all = db.get_history(100, None, None).await.unwrap();
        assert_eq!(all.len(), 5);
        // Kept the newest 5 (ids 6-10).
        assert_eq!(all[0].id, 10);
    }

    #[tokio::test]
    async fn test_cleanup_by_age() {
        let db = Database::open_memory().await.unwrap();

        // Insert an old dismissed notification.
        let mut old = make_notification(1, "App", "old");
        old.timestamp = "2020-01-01T00:00:00Z".into();
        db.insert_notification(&old).await.unwrap();
        db.dismiss(1, CloseReason::Expired).await.unwrap();

        // Insert a recent notification.
        db.insert_notification(&make_notification(2, "App", "new"))
            .await
            .unwrap();

        let deleted = db.cleanup(30, 0).await.unwrap();
        assert_eq!(deleted, 1);

        // Recent one still there.
        assert!(db.get_notification(2).await.unwrap().is_some());
        // Old one gone.
        assert!(db.get_notification(1).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_cleanup_skips_undismissed() {
        let db = Database::open_memory().await.unwrap();

        let mut old = make_notification(1, "App", "old but pending");
        old.timestamp = "2020-01-01T00:00:00Z".into();
        db.insert_notification(&old).await.unwrap();
        // NOT dismissed.

        let deleted = db.cleanup(30, 0).await.unwrap();
        assert_eq!(deleted, 0, "should not delete undismissed notifications");
    }

    #[tokio::test]
    async fn test_count_pending() {
        let db = Database::open_memory().await.unwrap();
        db.insert_notification(&make_notification(1, "A", "a"))
            .await
            .unwrap();
        db.insert_notification(&make_notification(2, "B", "b"))
            .await
            .unwrap();
        assert_eq!(db.count_pending().await.unwrap(), 2);

        db.dismiss(1, CloseReason::Dismissed).await.unwrap();
        assert_eq!(db.count_pending().await.unwrap(), 1);
    }
}
