/// Lunaris Notification Daemon.
///
/// Owns `org.freedesktop.Notifications` on the session D-Bus.
/// Receives notifications from applications, stores them in memory,
/// and broadcasts events for the desktop shell to consume.

use lunaris_notification_daemon::dbus::NotificationServer;
use zbus::connection;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("lunaris_notification_daemon=info".parse()?),
        )
        .init();

    tracing::info!("starting notification daemon");

    let (server, mut _event_rx) = NotificationServer::new();

    let _conn = connection::Builder::session()?
        .name("org.freedesktop.Notifications")?
        .serve_at("/org/freedesktop/Notifications", server)?
        .build()
        .await?;

    tracing::info!("D-Bus server ready on org.freedesktop.Notifications");

    // Wait for shutdown signal.
    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");

    Ok(())
}
