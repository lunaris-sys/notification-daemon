/// Persistent notification storage via SQLite.

pub mod models;
pub mod sqlite;

pub use models::NotificationRow;
pub use sqlite::Database;
