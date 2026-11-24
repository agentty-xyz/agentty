//! Opening, migrating, and beginning transactions on `SQLite` databases that
//! several processes and pools share.
//!
//! Each store keeps its own schema, busy timeout, durability, and error
//! labels; this crate owns the parts that depend on `SQLite` and `SQLx`
//! internals, so an `SQLx` upgrade is rechecked in one place.

use std::io::{self, ErrorKind};
use std::path::Path;
use std::time::Duration;

use sqlx::migrate::{MigrateError, Migrator};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Connection as _, Sqlite, SqliteConnection, SqlitePool, Transaction};

/// Connection policy for one store's database.
#[derive(Clone, Copy, Debug)]
pub struct OpenOptions {
    /// How long a connection waits for another connection's lock, and a
    /// caller for a pooled connection, before failing.
    pub busy_timeout: Duration,
    /// Labels that name the store in open failures.
    pub labels: OperationLabels,
    /// Maximum pooled connections to an on-disk database.
    pub max_connections: u32,
    /// Durability of commits in WAL mode.
    pub synchronous: SqliteSynchronous,
}

impl OpenOptions {
    /// Returns the connection settings for `path`: WAL, this policy's
    /// durability and busy timeout, and foreign keys.
    pub fn connect_options(&self, path: &Path) -> SqliteConnectOptions {
        SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .busy_timeout(self.busy_timeout)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(self.synchronous)
            .foreign_keys(true)
    }
}

/// Stable operation labels a store gives its open failures, so a failure
/// names the database it concerns.
#[derive(Clone, Copy, Debug)]
pub struct OperationLabels {
    /// Label of a failure while checking for pending migrations.
    pub check_migrations: &'static str,
    /// Label of a failure while connecting the pool.
    pub connect: &'static str,
    /// Label of a failure while creating a missing database file.
    pub create: &'static str,
    /// Label of a failure while applying or validating migrations.
    pub migrate: &'static str,
}

/// Failure to open or migrate a database.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A filesystem operation failed, such as creating the parent directory
    /// or staging a new database file.
    #[error("{0}")]
    Io(#[from] io::Error),

    /// A connection or query failed during the named operation.
    #[error("SQLite operation `{operation}` failed: {source}")]
    Sqlx {
        /// Stable label of the failed operation.
        operation: &'static str,
        /// Underlying `SQLx` failure.
        #[source]
        source: sqlx::Error,
    },

    /// Applying or validating a migration failed during the named operation.
    #[error("SQLite operation `{operation}` failed: {source}")]
    Migrate {
        /// Stable label of the failed operation.
        operation: &'static str,
        /// Underlying `SQLx` migration failure.
        #[source]
        source: MigrateError,
    },
}

/// Opens the database at `path` with `options` and applies `migrator`.
///
/// Other processes and pools may open the same file concurrently, even before
/// it exists: a missing database is created already in WAL mode, and pending
/// migrations are applied once under the write lock. An empty path or
/// `:memory:` opens a temporary database on one connection that the pool
/// never closes.
///
/// # Errors
/// Returns an error if the parent directory cannot be created, the database
/// cannot be created or connected, or a migration fails.
pub async fn open(
    path: &Path,
    migrator: &Migrator,
    options: OpenOptions,
) -> Result<SqlitePool, Error> {
    // A path without a parent creates nothing: `create_dir_all("")` is a
    // successful no-op.
    tokio::fs::create_dir_all(path.parent().unwrap_or(Path::new(""))).await?;

    let temporary = path.as_os_str().is_empty() || path == Path::new(":memory:");
    if !temporary && !tokio::fs::try_exists(path).await? {
        create_wal_database(path, &options).await?;
    }
    let pool_options = SqlitePoolOptions::new().acquire_timeout(options.busy_timeout);
    // SQLite temporary databases belong to one connection. Additional pooled
    // connections or eviction would lose their schema and data.
    let pool_options = if temporary {
        pool_options
            .max_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .test_before_acquire(false)
    } else {
        pool_options.max_connections(options.max_connections)
    };
    let pool = pool_options
        .connect_with(options.connect_options(path))
        .await
        .context(options.labels.connect)?;

    migrate(&pool, migrator, &options.labels).await?;

    Ok(pool)
}

/// Begins a transaction that takes the database write lock up front.
///
/// Use it for every transaction that may write. A deferred transaction that
/// reads before its first write upgrades its read snapshot to the write lock
/// at that write; when another connection committed in between, `SQLite`
/// fails the upgrade at once with `database is locked`, without waiting for
/// the busy timeout. `BEGIN IMMEDIATE` waits for the lock (up to the busy
/// timeout) before the first read instead, so lock waits fail here.
///
/// # Errors
/// Returns an error if no pooled connection is available or the write lock
/// cannot be taken within the busy timeout.
pub async fn begin_write(pool: &SqlitePool) -> Result<Transaction<'static, Sqlite>, sqlx::Error> {
    pool.begin_with("BEGIN IMMEDIATE").await
}

/// Begins a deferred transaction for a consistent read-only snapshot.
///
/// It takes no lock until its first read, and must not write: use
/// [`begin_write`] for a transaction that may write.
///
/// # Errors
/// Returns an error if no pooled connection is available.
pub async fn begin_read(pool: &SqlitePool) -> Result<Transaction<'static, Sqlite>, sqlx::Error> {
    pool.begin().await
}

/// Labels a `SQLx` failure with the operation it interrupted.
trait ResultExt<T> {
    /// Maps the failure into an [`Error`] carrying `operation`.
    fn context(self, operation: &'static str) -> Result<T, Error>;
}

impl<T> ResultExt<T> for Result<T, sqlx::Error> {
    fn context(self, operation: &'static str) -> Result<T, Error> {
        self.map_err(|source| Error::Sqlx { operation, source })
    }
}

impl<T> ResultExt<T> for Result<T, MigrateError> {
    fn context(self, operation: &'static str) -> Result<T, Error> {
        self.map_err(|source| Error::Migrate { operation, source })
    }
}

/// Creates the database at `path` already in WAL mode, unless another opener
/// created it first.
///
/// `SQLite` does not apply the busy timeout to the switch into WAL mode: it
/// upgrades a read transaction to the write lock (`sqlite3BtreeSetVersion`),
/// so concurrent first opens of one file fail at once with `database is
/// locked`. Switching a private file and moving it into place without
/// replacing anything gives every connection to `path` a file that is already
/// in WAL mode.
async fn create_wal_database(path: &Path, options: &OpenOptions) -> Result<(), Error> {
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let staged = tempfile::Builder::new()
        .prefix(".sqlite-staged-")
        .suffix(".db")
        .tempfile_in(directory)?
        .into_temp_path();
    SqliteConnection::connect_with(&options.connect_options(&staged))
        .await
        .context(options.labels.create)?
        .close()
        .await
        .context(options.labels.create)?;

    match staged.persist_noclobber(path).map_err(|error| error.error) {
        Err(error) if error.kind() != ErrorKind::AlreadyExists => Err(error.into()),
        _ => Ok(()),
    }
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
async fn migrate(
    pool: &SqlitePool,
    migrator: &Migrator,
    labels: &OperationLabels,
) -> Result<(), Error> {
    // A migration opting out of transactions would silently run inside the
    // lock below, where `VACUUM` fails and `PRAGMA foreign_keys` is ignored.
    if let Some(migration) = migrator.iter().find(|migration| migration.no_tx) {
        return Err(MigrateError::Source(
            format!(
                "migration {} opts out of transactions, which the locked migration path cannot \
                 apply",
                migration.version
            )
            .into(),
        ))
        .context(labels.migrate);
    }
    if !has_pending_migration(pool, migrator, labels).await? {
        return migrator.run(pool).await.context(labels.migrate);
    }

    let mut transaction = begin_write(pool).await.context(labels.migrate)?;
    // `run_direct` is hidden from `SQLx`'s docs and semver. The public
    // `migrator.run(&mut *transaction)` does not compile here: its `Acquire`
    // bound trips rust-lang/rust#100013 in this `Send` future, and `SQLx`
    // keeps `run_direct` for exactly that error. Inside the transaction,
    // `SQLx`'s `SQLite` `Migrate::apply` nests each migration in a
    // `SAVEPOINT`. Recheck both assumptions, and whether `Migrate::lock`
    // still does nothing for `SQLite`, when upgrading `SQLx`; this crate's
    // contention tests fail if either breaks.
    migrator
        .run_direct(None, &mut *transaction, false)
        .await
        .context(labels.migrate)?;

    transaction.commit().await.context(labels.migrate)
}

/// Returns whether `migrator` has a migration the database has not applied
/// successfully, using reads only.
async fn has_pending_migration(
    pool: &SqlitePool,
    migrator: &Migrator,
    labels: &OperationLabels,
) -> Result<bool, Error> {
    let mut connection = pool.acquire().await.context(labels.check_migrations)?;
    // Unchecked queries: the migrations table belongs to `SQLx`, not to the
    // store's schema, and may not exist yet.
    let has_table = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = \
         '_sqlx_migrations')",
    )
    .fetch_one(&mut *connection)
    .await
    .context(labels.check_migrations)?;
    if !has_table {
        return Ok(true);
    }

    let applied =
        sqlx::query_scalar::<_, i64>("SELECT version FROM _sqlx_migrations WHERE success")
            .fetch_all(&mut *connection)
            .await
            .context(labels.check_migrations)?;

    Ok(migrator.iter().any(|migration| {
        !migration.migration_type.is_down_migration() && !applied.contains(&migration.version)
    }))
}

#[cfg(test)]
#[path = "lib_test.rs"]
mod tests;
