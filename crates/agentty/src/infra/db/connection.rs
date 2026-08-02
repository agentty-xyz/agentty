//! `SQLite` connection setup for the database layer.

use std::ops::Deref;
use std::path::Path;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

use super::{AppRepositories, DbError};

/// Subdirectory under the agentty home where the database file is stored.
pub const DB_DIR: &str = "db";

/// Default database filename.
pub const DB_FILE: &str = "agentty.db";

/// Maximum number of pooled `SQLite` connections for the on-disk database.
///
/// `SQLite` still serializes writes in WAL mode, so the pool stays small and
/// biased toward a handful of concurrent readers instead of a large number of
/// queued writer contenders.
pub(crate) const DB_POOL_MAX_CONNECTIONS: u32 = 4;

/// Thin wrapper around a `SQLite` connection pool.
#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
    repositories: AppRepositories,
}

impl Database {
    /// Opens the `SQLite` database and runs embedded migrations.
    ///
    /// Uses up to `DB_POOL_MAX_CONNECTIONS` pooled connections so UI reads can
    /// stay responsive without oversizing the `SQLite` pool beyond what WAL
    /// can use effectively. Applies a short busy timeout so bursty reducer
    /// writes wait briefly for the single `SQLite` writer instead of failing
    /// immediately with `SQLITE_BUSY`.
    ///
    /// # Errors
    /// Returns an error if the directory cannot be created, the database
    /// cannot be opened, or migrations fail.
    pub async fn open(db_path: &Path) -> Result<Self, DbError> {
        if let Some(parent) = db_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let options = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true)
            .busy_timeout(Duration::from_secs(2))
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(DB_POOL_MAX_CONNECTIONS)
            .connect_with(options)
            .await?;

        sqlx::migrate!("./migrations").run(&pool).await?;

        let repositories = AppRepositories::from_pool(pool.clone());

        Ok(Self { pool, repositories })
    }

    /// Opens an in-memory `SQLite` database and runs migrations.
    ///
    /// This is primarily used by tests and any ephemeral workflows that need
    /// an isolated database instance while keeping the same durability and
    /// foreign-key settings as the on-disk database. Applies the same short
    /// busy timeout as the on-disk configuration so tests exercise the same
    /// writer wait policy.
    ///
    /// # Errors
    /// Returns an error if the database connection or migrations fail.
    pub async fn open_in_memory() -> Result<Self, DbError> {
        let pool = open_in_memory_pool(1).await?;

        let repositories = AppRepositories::from_pool(pool.clone());

        Ok(Self { pool, repositories })
    }

    /// Returns the shared `SQLite` connection pool for lower-level query
    /// access.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

impl Deref for Database {
    type Target = AppRepositories;

    fn deref(&self) -> &Self::Target {
        &self.repositories
    }
}

impl From<Database> for AppRepositories {
    fn from(database: Database) -> Self {
        database.repositories
    }
}

/// Opens an in-memory `SQLite` pool with migrations applied.
///
/// The caller chooses the connection cap so tests and runtime code can share
/// the same setup logic while keeping their own concurrency requirements.
/// Applies the same 2-second busy timeout as the on-disk database so pooled
/// test connections exercise the same writer wait policy.
///
/// # Errors
/// Returns an error if the in-memory database connection or migrations fail.
pub(crate) async fn open_in_memory_pool(max_connections: u32) -> Result<SqlitePool, DbError> {
    let options = SqliteConnectOptions::new()
        .filename(":memory:")
        .busy_timeout(Duration::from_secs(2))
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections)
        .connect_with(options)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;

    Ok(pool)
}

#[cfg(test)]
#[path = "connection_test.rs"]
mod tests;
