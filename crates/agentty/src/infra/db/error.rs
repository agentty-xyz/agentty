//! Database error types shared by persistence adapters.

/// Typed error returned by database operations.
///
/// Wraps the underlying `SQLx`, migration, and I/O failures so callers can
/// distinguish error categories without parsing opaque strings.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// A SQL query or connection-pool operation failed.
    #[error("{0}")]
    Query(#[from] sqlx::Error),

    /// An embedded schema migration failed during database open.
    #[error("{0}")]
    Migration(#[from] sqlx::migrate::MigrateError),

    /// A filesystem operation failed, such as creating the database directory.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}
