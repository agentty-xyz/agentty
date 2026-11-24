//! `SQLite` connection setup for the database layer.

use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ag_sqlite::{OpenOptions, OperationLabels};
use sqlx::sqlite::SqliteSynchronous;
use sqlx::{Sqlite, SqlitePool, Transaction};

use crate::timestamp::system_timestamp_source;
use crate::{AppRepositories, DbError, DbResultExt as _, TimestampSource};

/// How long [`Database::open`] connections wait for another connection's lock,
/// and for a pooled connection, before failing with `database is locked`.
///
/// The TUI awaits some writes on its event loop, and four writers waiting for
/// the lock fill the pool so reads wait for a connection too. This bound is
/// therefore the longest the interface can stall behind a lock another
/// process holds. It still outlasts the multi-second fsync stall of a commit
/// or closing checkpoint on a loaded disk.
pub const INTERACTIVE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Busy and pool-acquire timeout for openers that block no interface, such as
/// fixtures, scripts, and tools; pass it to
/// [`Database::open_with_busy_timeout`].
///
/// A slow lock holder then delays these openers instead of failing them.
pub const BACKGROUND_BUSY_TIMEOUT: Duration = Duration::from_secs(60);

/// Maximum number of pooled `SQLite` connections for the on-disk database.
///
/// `SQLite` still serializes writes in WAL mode, so the pool stays small and
/// biased toward a handful of concurrent readers instead of a large number of
/// queued writer contenders.
pub(crate) const DB_POOL_MAX_CONNECTIONS: u32 = 4;

/// Embedded schema migrations applied to every database.
pub(crate) static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Thin wrapper around a `SQLite` connection pool.
#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
    repositories: AppRepositories,
}

impl Database {
    /// Opens the `SQLite` database for an interactive host and runs embedded
    /// migrations.
    ///
    /// Uses up to `DB_POOL_MAX_CONNECTIONS` pooled connections so UI reads can
    /// stay responsive without oversizing the `SQLite` pool beyond what WAL
    /// can use effectively. Writers, and callers waiting for a pooled
    /// connection, wait up to [`INTERACTIVE_BUSY_TIMEOUT`].
    ///
    /// Other processes and pools may open the same file concurrently, even
    /// before it exists: a missing database is created already in WAL mode,
    /// and pending migrations are applied once under the write lock. An empty
    /// path or `:memory:` opens a temporary database on one connection.
    ///
    /// # Errors
    /// Returns an error if the directory cannot be created, the database
    /// cannot be opened, or migrations fail.
    pub async fn open(db_path: &Path) -> Result<Self, DbError> {
        Self::open_with_timestamp_source(db_path, system_timestamp_source()).await
    }

    /// Opens the `SQLite` database like [`Database::open`] with an injected
    /// persistence timestamp source.
    ///
    /// # Errors
    /// Returns an error if the directory cannot be created, the database
    /// cannot be opened, or migrations fail.
    pub async fn open_with_timestamp_source(
        db_path: &Path,
        timestamp_source: Arc<dyn TimestampSource>,
    ) -> Result<Self, DbError> {
        Self::open_with(db_path, timestamp_source, INTERACTIVE_BUSY_TIMEOUT).await
    }

    /// Opens the `SQLite` database like [`Database::open`], waiting up to
    /// `busy_timeout` for locks and pooled connections.
    ///
    /// Pass [`BACKGROUND_BUSY_TIMEOUT`] when no interface waits on the
    /// database.
    ///
    /// # Errors
    /// Returns an error if the directory cannot be created, the database
    /// cannot be opened, or migrations fail.
    pub async fn open_with_busy_timeout(
        db_path: &Path,
        busy_timeout: Duration,
    ) -> Result<Self, DbError> {
        Self::open_with(db_path, system_timestamp_source(), busy_timeout).await
    }

    /// Opens an in-memory `SQLite` database and runs migrations.
    ///
    /// This is primarily used by tests and any ephemeral workflows that need
    /// an isolated database instance while keeping the same durability and
    /// foreign-key settings as the on-disk database. Applies the same busy
    /// timeout as [`Database::open`] so tests exercise the same writer wait
    /// policy.
    ///
    /// # Errors
    /// Returns an error if the database connection or migrations fail.
    pub async fn open_in_memory() -> Result<Self, DbError> {
        Self::open_in_memory_with_timestamp_source(system_timestamp_source()).await
    }

    /// Opens an in-memory `SQLite` database with an injected timestamp source.
    ///
    /// # Errors
    /// Returns an error if the database connection or migrations fail.
    pub async fn open_in_memory_with_timestamp_source(
        timestamp_source: Arc<dyn TimestampSource>,
    ) -> Result<Self, DbError> {
        let pool = open_in_memory_pool().await?;

        Ok(Self::from_pool(pool, timestamp_source))
    }

    /// Wraps an initialized pool with repositories using `timestamp_source`.
    pub(crate) fn from_pool(pool: SqlitePool, timestamp_source: Arc<dyn TimestampSource>) -> Self {
        let repositories =
            AppRepositories::from_pool_and_timestamp_source(pool.clone(), timestamp_source);

        Self { pool, repositories }
    }

    /// Returns the shared `SQLite` connection pool for lower-level query
    /// access.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Opens `db_path` with `busy_timeout` for locks and pooled connections.
    async fn open_with(
        db_path: &Path,
        timestamp_source: Arc<dyn TimestampSource>,
        busy_timeout: Duration,
    ) -> Result<Self, DbError> {
        let pool = ag_sqlite::open(db_path, &MIGRATOR, open_options(busy_timeout))
            .await
            .map_err(DbError::from_open)?;

        Ok(Self::from_pool(pool, timestamp_source))
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

/// Begins a transaction that may write with [`ag_sqlite::begin_write`], and
/// labels a failure with `operation`.
///
/// # Errors
/// Returns [`DbError::QueryContext`] if no pooled connection is available or
/// the write lock cannot be taken within the busy timeout.
pub(crate) async fn begin_write(
    pool: &SqlitePool,
    operation: &'static str,
) -> Result<Transaction<'static, Sqlite>, DbError> {
    ag_sqlite::begin_write(pool).await.db_context(operation)
}

/// Begins a read-only snapshot with [`ag_sqlite::begin_read`], and labels a
/// failure with `operation`.
///
/// # Errors
/// Returns [`DbError::QueryContext`] if no pooled connection is available.
pub(crate) async fn begin_read(
    pool: &SqlitePool,
    operation: &'static str,
) -> Result<Transaction<'static, Sqlite>, DbError> {
    ag_sqlite::begin_read(pool).await.db_context(operation)
}

/// Opens a migrated in-memory `SQLite` pool on one connection that the pool
/// never evicts, with the interactive busy timeout.
///
/// # Errors
/// Returns an error if the in-memory database connection or migrations fail.
pub(crate) async fn open_in_memory_pool() -> Result<SqlitePool, DbError> {
    ag_sqlite::open(
        Path::new(":memory:"),
        &MIGRATOR,
        open_options(INTERACTIVE_BUSY_TIMEOUT),
    )
    .await
    .map_err(DbError::from_open)
}

/// Returns the store's connection policy: `NORMAL` durability, at most
/// [`DB_POOL_MAX_CONNECTIONS`] pooled connections, `busy_timeout`, and labels
/// naming the app database.
pub(crate) fn open_options(busy_timeout: Duration) -> OpenOptions {
    OpenOptions {
        busy_timeout,
        labels: OperationLabels {
            check_migrations: "check app database migrations",
            connect: "open app database",
            create: "create app database",
            migrate: "migrate app database",
        },
        max_connections: DB_POOL_MAX_CONNECTIONS,
        synchronous: SqliteSynchronous::Normal,
    }
}

#[cfg(test)]
#[path = "connection_test.rs"]
mod tests;
