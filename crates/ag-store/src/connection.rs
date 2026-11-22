//! `SQLite` connection setup for the database layer.

use std::io::ErrorKind;
use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use sqlx::migrate::{MigrateError, Migrator};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Connection as _, Sqlite, SqliteConnection, SqlitePool, Transaction};

use crate::timestamp::system_timestamp_source;
use crate::{AppRepositories, DbError, DbResultExt as _, TimestampSource};

/// Maximum number of pooled `SQLite` connections for the on-disk database.
///
/// `SQLite` still serializes writes in WAL mode, so the pool stays small and
/// biased toward a handful of concurrent readers instead of a large number of
/// queued writer contenders.
pub(crate) const DB_POOL_MAX_CONNECTIONS: u32 = 4;

/// How long a connection waits for another connection's lock before failing
/// with `database is locked`, and how long a caller waits for a pooled
/// connection.
///
/// Several processes and pools share the database file: the TUI, a second
/// instance, and tools that seed or inspect it. A commit, or the checkpoint a
/// closing pool runs, holds the lock while it fsyncs, which can stall for many
/// seconds on a loaded disk. A slow lock holder therefore delays other
/// writers instead of failing them.
pub(crate) const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(60);

/// Embedded schema migrations applied to every database.
pub(crate) static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

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
    /// can use effectively. Writers wait up to `SQLITE_BUSY_TIMEOUT` for the
    /// single `SQLite` writer instead of failing with `SQLITE_BUSY`.
    ///
    /// Other processes and pools may open the same file concurrently, even
    /// before it exists: a missing database is created already in WAL mode,
    /// and pending migrations are applied once under the write lock.
    ///
    /// # Errors
    /// Returns an error if the directory cannot be created, the database
    /// cannot be opened, or migrations fail.
    pub async fn open(db_path: &Path) -> Result<Self, DbError> {
        Self::open_with_timestamp_source(db_path, system_timestamp_source()).await
    }

    /// Opens the `SQLite` database with an injected persistence timestamp
    /// source.
    ///
    /// # Errors
    /// Returns an error if the directory cannot be created, the database
    /// cannot be opened, or migrations fail.
    pub async fn open_with_timestamp_source(
        db_path: &Path,
        timestamp_source: Arc<dyn TimestampSource>,
    ) -> Result<Self, DbError> {
        // A path without a parent creates nothing: `create_dir_all("")` is a
        // successful no-op.
        tokio::fs::create_dir_all(db_path.parent().unwrap_or(Path::new(""))).await?;
        if !tokio::fs::try_exists(db_path).await? {
            create_wal_database(db_path).await?;
        }

        let pool = SqlitePoolOptions::new()
            .max_connections(DB_POOL_MAX_CONNECTIONS)
            .acquire_timeout(SQLITE_BUSY_TIMEOUT)
            .connect_with(on_disk_connect_options(db_path))
            .await?;

        migrate(&pool, &MIGRATOR).await?;

        Ok(Self::from_pool(pool, timestamp_source))
    }

    /// Opens an in-memory `SQLite` database and runs migrations.
    ///
    /// This is primarily used by tests and any ephemeral workflows that need
    /// an isolated database instance while keeping the same durability and
    /// foreign-key settings as the on-disk database. Applies the same busy
    /// timeout as the on-disk configuration so tests exercise the same writer
    /// wait policy.
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
        let pool = open_in_memory_pool(1).await?;

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

/// Begins a transaction that takes the database write lock up front.
///
/// Use it for every transaction that may write. A deferred transaction that
/// reads before its first write upgrades its read snapshot to the write lock
/// at that write; when another connection committed in between, `SQLite`
/// fails the upgrade at once with `database is locked`, without waiting for
/// the busy timeout. `BEGIN IMMEDIATE` waits for the lock (up to the busy
/// timeout) before the first read instead.
///
/// # Errors
/// Returns an error if no pooled connection is available or the write lock
/// cannot be taken within the busy timeout.
pub(crate) async fn begin_write(
    pool: &SqlitePool,
) -> Result<Transaction<'static, Sqlite>, DbError> {
    Ok(pool.begin_with("BEGIN IMMEDIATE").await?)
}

/// Opens an in-memory `SQLite` pool with migrations applied.
///
/// The caller chooses the connection cap so tests and runtime code can share
/// the same setup logic while keeping their own concurrency requirements.
/// Applies the same busy timeout as the on-disk database so pooled test
/// connections exercise the same writer wait policy.
///
/// # Errors
/// Returns an error if the in-memory database connection or migrations fail.
pub(crate) async fn open_in_memory_pool(max_connections: u32) -> Result<SqlitePool, DbError> {
    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(SQLITE_BUSY_TIMEOUT)
        .connect_with(in_memory_connect_options())
        .await?;

    migrate(&pool, &MIGRATOR).await?;

    Ok(pool)
}

/// Returns in-memory connection settings matching the on-disk busy timeout,
/// durability, and foreign-key policy.
pub(crate) fn in_memory_connect_options() -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(":memory:")
        .busy_timeout(SQLITE_BUSY_TIMEOUT)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
}

/// Creates the database at `db_path` already in WAL mode, unless another
/// opener created it first.
///
/// `SQLite` does not apply the busy timeout to the switch into WAL mode: it
/// upgrades a read transaction to the write lock (`sqlite3BtreeSetVersion`),
/// so concurrent first opens of one file fail at once with `database is
/// locked`. Switching a private file and moving it into place without
/// replacing anything gives every connection to `db_path` a file that is
/// already in WAL mode.
async fn create_wal_database(db_path: &Path) -> Result<(), DbError> {
    let operation = "create app database";
    let directory = db_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let staged = tempfile::Builder::new()
        .prefix(".agentty-")
        .suffix(".db")
        .tempfile_in(directory)?
        .into_temp_path();
    SqliteConnection::connect_with(&on_disk_connect_options(&staged))
        .await
        .db_context(operation)?
        .close()
        .await
        .db_context(operation)?;

    match staged
        .persist_noclobber(db_path)
        .map_err(|error| error.error)
    {
        Err(error) if error.kind() != ErrorKind::AlreadyExists => Err(error.into()),
        _ => Ok(()),
    }
}

/// Returns the on-disk connection settings: WAL, `NORMAL` durability,
/// foreign keys, and the shared busy timeout.
fn on_disk_connect_options(db_path: &Path) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .busy_timeout(SQLITE_BUSY_TIMEOUT)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
}

/// Applies `migrator`'s pending migrations while holding the database write
/// lock, and only validates applied ones when none is pending.
///
/// `SQLx` does not lock `SQLite` migrations (its `Migrate::lock` for `SQLite`
/// is a no-op), so concurrent opens would each list the same pending
/// migrations and apply them twice. `BEGIN IMMEDIATE` serializes them: later
/// openers wait (up to the busy timeout) for the lock, then find every
/// migration applied. An up-to-date database is checked with reads alone, so
/// opening it never waits behind another writer.
async fn migrate(pool: &SqlitePool, migrator: &Migrator) -> Result<(), DbError> {
    // A migration opting out of transactions would silently run inside the
    // lock below, where `VACUUM` fails and `PRAGMA foreign_keys` is ignored.
    if let Some(migration) = migrator.iter().find(|migration| migration.no_tx) {
        return Err(DbError::Migration(MigrateError::Source(
            format!(
                "migration {} opts out of transactions, which the locked migration path cannot \
                 apply",
                migration.version
            )
            .into(),
        )));
    }
    if !has_pending_migration(pool, migrator).await? {
        migrator.run(pool).await?;

        return Ok(());
    }

    let operation = "migrate app database";
    let mut transaction = begin_write(pool).await?;
    // `run_direct` is hidden from `SQLx`'s docs and semver. The public
    // `migrator.run(&mut *transaction)` does not compile here: its `Acquire`
    // bound trips rust-lang/rust#100013 in this `Send` future, and `SQLx`
    // keeps `run_direct` for exactly that error. Inside the transaction,
    // `SQLx`'s `SQLite` `Migrate::apply` nests each migration in a
    // `SAVEPOINT`. Recheck both assumptions, and whether `Migrate::lock`
    // still does nothing for `SQLite`, when upgrading `SQLx`; the contention
    // tests fail if either breaks.
    migrator.run_direct(None, &mut *transaction, false).await?;

    transaction.commit().await.db_context(operation)
}

/// Returns whether `migrator` has a migration the database has not applied
/// successfully, using reads only.
async fn has_pending_migration(pool: &SqlitePool, migrator: &Migrator) -> Result<bool, DbError> {
    let operation = "check app database migrations";
    let mut connection = pool.acquire().await.db_context(operation)?;
    // Unchecked queries: `_sqlx_migrations` belongs to `SQLx`, not to the
    // embedded schema, and may not exist yet.
    let has_table = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = \
         '_sqlx_migrations')",
    )
    .fetch_one(&mut *connection)
    .await
    .db_context(operation)?;
    if !has_table {
        return Ok(true);
    }

    let applied =
        sqlx::query_scalar::<_, i64>("SELECT version FROM _sqlx_migrations WHERE success")
            .fetch_all(&mut *connection)
            .await
            .db_context(operation)?;

    Ok(migrator.iter().any(|migration| {
        !migration.migration_type.is_down_migration() && !applied.contains(&migration.version)
    }))
}

#[cfg(test)]
#[path = "connection_test.rs"]
mod tests;
