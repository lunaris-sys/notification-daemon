/// Lunaris Notification Daemon.
///
/// Owns `org.freedesktop.Notifications` on the session D-Bus. Stores
/// notifications in SQLite, enforces DND rules, and broadcasts to
/// connected shell clients via a Unix socket.

use std::sync::Arc;

use tokio::sync::Mutex;
use zbus::connection;

use lunaris_notification_daemon::config;
use lunaris_notification_daemon::dbus::NotificationServer;
use lunaris_notification_daemon::events;
use lunaris_notification_daemon::manager::NotificationManager;
use lunaris_notification_daemon::socket::SocketServer;
use lunaris_notification_daemon::storage::Database;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("lunaris_notification_daemon=info".parse()?),
        )
        .init();

    tracing::info!("starting notification daemon");

    // 1. Load config.
    let config_path = config::default_config_path();
    let cfg = config::load_config(&config_path);
    let config = Arc::new(Mutex::new(cfg));

    // 2. Init database.
    let db_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("lunaris");
    let _ = std::fs::create_dir_all(&db_dir);
    let db_file = db_dir.join("notifications.db");
    let db_path = format!("sqlite:{}?mode=rwc", db_file.display());
    tracing::info!("opening database: {db_path}");
    let db = Arc::new(Database::open(&db_path).await?);
    tracing::info!("database opened at {db_path}");

    // 3. Create manager. The D-Bus server delegates every incoming
    // notify() call to `manager.handle_notify()` so DND / rate limits /
    // SQLite storage all run in one place.
    let (dbus_server, event_rx) = NotificationServer::new();
    let event_tx = dbus_server.event_sender();
    let manager = Arc::new(NotificationManager::new(
        db.clone(),
        config.clone(),
        event_tx.clone(),
    ));
    dbus_server.set_manager(manager.clone());

    // 4. Start D-Bus server.
    let _conn = connection::Builder::session()?
        .name("org.freedesktop.Notifications")?
        .serve_at("/org/freedesktop/Notifications", dbus_server)?
        .build()
        .await?;
    tracing::info!("D-Bus server ready");

    // 5. Start socket server in background.
    let socket_path = SocketServer::default_path();
    let socket_server = SocketServer::new(socket_path);
    let dnd_mode = manager.dnd_mode();
    tokio::spawn(async move {
        if let Err(e) = socket_server.start(event_rx, event_tx, db.clone(), dnd_mode).await {
            tracing::error!("socket server error: {e}");
        }
    });

    // 6. Start config watcher.
    let config_for_watcher = config.clone();
    if let Ok((mut config_rx, _watcher)) =
        config::watcher::watch_config(config_path)
    {
        tokio::spawn(async move {
            while let Ok(new_config) = config_rx.recv().await {
                *config_for_watcher.lock().await = new_config;
                tracing::info!("config hot-reloaded");
            }
        });
    }

    // 7. Event Bus consumer: subscribe to `focus.*` and
    // `window.fullscreen_*` events so the DND state machine updates
    // when the shell enters/leaves Focus Mode or the compositor
    // enters/leaves fullscreen. Failures log and retry — the daemon
    // must keep working if the Event Bus is down.
    events::consumer::start(manager.clone());

    // 8. Retention cleanup task (runs daily).
    let manager_for_cleanup = manager.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(86400)).await;
            manager_for_cleanup.cleanup().await;
        }
    });

    // Run initial cleanup on startup.
    manager.cleanup().await;

    tracing::info!("notification daemon ready");

    // Wait for shutdown signal.
    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");

    Ok(())
}
