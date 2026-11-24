//! Open, migration, and transaction tests: concurrent first opens create and
//! migrate a file once, migrations run under the write lock and roll back
//! together, up-to-date databases open without the lock, and transactions
//! take the lock their kind promises.

use std::path::Path;
use std::time::Duration;

use sqlx::migrate::{MigrateError, Migration, MigrationType, Migrator};
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqliteSynchronous};
use sqlx::{Connection as _, SqlSafeStr as _, SqlitePool};
use tempfile::tempdir;
use tokio::task::JoinSet;

use super::{Error, OpenOptions, begin_read, begin_write, create_wal_database, migrate, open};

const CONCURRENT_OPENS: usize = 8;

/// Connection policy for tests that never wait out a lock.
const OPTIONS: OpenOptions = OpenOptions {
    busy_timeout: Duration::from_secs(5),
    max_connections: 4,
    synchronous: SqliteSynchronous::Full,
};

/// Fails a contention regression fast, well inside the slow-test limit and
/// below the busy timeout it would otherwise wait out.
const REGRESSION_BOUND: Duration = Duration::from_secs(3);

/// How long a writer must stay blocked to show it waits for a held lock.
const PENDING_WINDOW: Duration = Duration::from_millis(200);

/// Busy timeout short enough to observe an opener giving up.
const SHORT_BUSY_TIMEOUT: Duration = Duration::from_millis(100);

const CREATE_ITEM: &str = "CREATE TABLE item (id INTEGER PRIMARY KEY, name TEXT NOT NULL)";
const CREATE_TAG: &str = "CREATE TABLE tag (id INTEGER PRIMARY KEY)";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_opens_of_a_new_file_migrate_it_once() {
    // Arrange
    let temp_dir = tempdir().expect("temp directory should be created");
    let path = temp_dir.path().join("nested/store.db");
    let mut opens = JoinSet::new();
    for _ in 0..CONCURRENT_OPENS {
        let path = path.clone();
        opens.spawn(
            async move { open(&path, &migrator(&[CREATE_ITEM, CREATE_TAG]), OPTIONS).await },
        );
    }

    // Act
    let pools = tokio::time::timeout(REGRESSION_BOUND, async {
        let mut pools = Vec::new();
        while let Some(result) = opens.join_next().await {
            pools.push(result.expect("open task should not panic"));
        }

        pools
    })
    .await
    .expect("concurrent first opens should not wait out the busy timeout");

    // Assert
    let pools = pools
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("every concurrent first open should succeed");
    let (applied, distinct, succeeded) = sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT COUNT(*), COUNT(DISTINCT version), SUM(success) FROM _sqlx_migrations",
    )
    .fetch_one(&pools[0])
    .await
    .expect("applied migrations should load");
    assert_eq!((applied, distinct, succeeded), (2, 2, 2));
    assert!(
        directory_entries(&temp_dir.path().join("nested"))
            .iter()
            .all(|name| name.starts_with("store.db")),
        "staged database files should not remain"
    );
}

#[tokio::test]
async fn a_new_file_is_created_in_wal_mode_with_the_connection_policy() {
    // Arrange
    let temp_dir = tempdir().expect("temp directory should be created");
    let path = temp_dir.path().join("store.db");

    // Act
    let pool = open(&path, &migrator(&[CREATE_ITEM]), OPTIONS)
        .await
        .expect("database should open");

    // Assert
    let journal_mode = pragma::<String>(&pool, "journal_mode").await;
    let synchronous = pragma::<i64>(&pool, "synchronous").await;
    let foreign_keys = pragma::<i64>(&pool, "foreign_keys").await;
    let busy_timeout_ms = pragma::<i64>(&pool, "busy_timeout").await;
    assert_eq!(journal_mode, "wal");
    assert_eq!(synchronous, 2);
    assert_eq!(foreign_keys, 1);
    assert_eq!(busy_timeout_ms, 5_000);
    assert_eq!(pool.options().get_max_connections(), 4);
    assert_eq!(pool.options().get_acquire_timeout(), OPTIONS.busy_timeout);
}

#[tokio::test]
async fn creating_an_existing_database_keeps_it_and_leaves_no_staged_file() {
    // Arrange
    let temp_dir = tempdir().expect("temp directory should be created");
    let path = temp_dir.path().join("store.db");
    std::fs::write(&path, b"existing").expect("existing file should be written");

    // Act
    let result = create_wal_database(&path, &OPTIONS).await;

    // Assert
    result.expect("an existing database should be kept");
    assert_eq!(
        std::fs::read(&path).expect("existing file should be readable"),
        b"existing"
    );
    assert_eq!(
        directory_entries(temp_dir.path()),
        vec!["store.db".to_string()]
    );
}

#[tokio::test]
async fn opening_an_up_to_date_database_does_not_take_the_write_lock() {
    // Arrange
    let temp_dir = tempdir().expect("temp directory should be created");
    let path = temp_dir.path().join("store.db");
    let migrator = migrator(&[CREATE_ITEM]);
    open(&path, &migrator, OPTIONS)
        .await
        .expect("first open should migrate the database")
        .close()
        .await;
    let mut lock_holder = hold_write_lock(&path).await;

    // Act
    let reopened = tokio::time::timeout(REGRESSION_BOUND, open(&path, &migrator, OPTIONS)).await;

    // Assert
    reopened
        .expect("opening an up-to-date database should not wait for the write lock")
        .expect("an up-to-date database should open while another connection writes");
    release_write_lock(&mut lock_holder).await;
}

#[tokio::test]
async fn pending_migrations_wait_for_the_write_lock_and_give_up_at_the_busy_timeout() {
    // Arrange
    let temp_dir = tempdir().expect("temp directory should be created");
    let path = temp_dir.path().join("store.db");
    open(&path, &migrator(&[CREATE_ITEM]), OPTIONS)
        .await
        .expect("first open should migrate the database")
        .close()
        .await;
    let mut lock_holder = hold_write_lock(&path).await;
    let impatient = OpenOptions {
        busy_timeout: SHORT_BUSY_TIMEOUT,
        ..OPTIONS
    };

    // Act
    let result = tokio::time::timeout(
        REGRESSION_BOUND,
        open(&path, &migrator(&[CREATE_ITEM, CREATE_TAG]), impatient),
    )
    .await
    .expect("a pending migration should give up at the busy timeout");

    // Assert
    let error = result.expect_err("a pending migration should not apply while the lock is held");
    assert!(
        matches!(
            &error,
            Error::Sqlx {
                operation: "migrate database",
                source,
            } if source.to_string().contains("database is locked")
        ),
        "unexpected error: {error:?}"
    );
    release_write_lock(&mut lock_holder).await;
}

#[tokio::test]
async fn a_failing_migration_rolls_back_every_migration_of_the_open() {
    // Arrange
    let temp_dir = tempdir().expect("temp directory should be created");
    let path = temp_dir.path().join("store.db");
    let migrator = migrator(&[CREATE_ITEM, "INSERT INTO missing VALUES (1)"]);

    // Act
    let result = open(&path, &migrator, OPTIONS).await;

    // Assert
    let error = result.expect_err("a failing migration should fail the open");
    assert!(
        matches!(
            error,
            Error::Migrate {
                operation: "migrate database",
                source: MigrateError::ExecuteMigration(..),
            }
        ),
        "unexpected error: {error:?}"
    );
    let mut connection = SqliteConnection::connect_with(&OPTIONS.connect_options(&path))
        .await
        .expect("database should still connect");
    let tables = sqlx::query_scalar::<_, String>(
        "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name",
    )
    .fetch_all(&mut connection)
    .await
    .expect("tables should load");
    assert!(tables.is_empty(), "unexpected tables: {tables:?}");
}

#[tokio::test]
async fn migrations_opting_out_of_transactions_are_rejected() {
    // Arrange
    let pool = memory_pool().await;
    let migrator = Migrator::with_migrations(vec![Migration::new(
        1,
        "vacuum".into(),
        MigrationType::Simple,
        "VACUUM".into_sql_str(),
        true,
    )]);

    // Act
    let result = migrate(&pool, &migrator).await;

    // Assert
    let error = result.expect_err("a no-transaction migration should be rejected");
    assert!(
        matches!(
            error,
            Error::Migrate {
                operation: "migrate database",
                source: MigrateError::Source(_),
            }
        ),
        "unexpected error: {error:?}"
    );
    assert!(
        error
            .to_string()
            .contains("migration 1 opts out of transactions")
    );
}

#[tokio::test]
async fn an_up_to_date_database_still_validates_applied_migrations() {
    // Arrange
    let pool = memory_pool().await;
    migrate(&pool, &migrator(&[CREATE_ITEM, CREATE_TAG]))
        .await
        .expect("both migrations should apply");

    // Act
    let result = migrate(&pool, &migrator(&[CREATE_ITEM])).await;

    // Assert
    let error = result.expect_err("an applied migration missing from the source should fail");
    assert!(
        matches!(
            error,
            Error::Migrate {
                source: MigrateError::VersionMissing(2),
                ..
            }
        ),
        "unexpected error: {error:?}"
    );
}

#[tokio::test]
async fn temporary_paths_open_a_migrated_single_connection_database_without_a_file() {
    // Arrange
    let memory_path = Path::new(":memory:");
    let migrator = migrator(&[CREATE_ITEM]);

    // Act
    let memory = open(memory_path, &migrator, OPTIONS)
        .await
        .expect("`:memory:` should open");
    let empty = open(Path::new(""), &migrator, OPTIONS)
        .await
        .expect("an empty path should open");

    // Assert
    for pool in [&memory, &empty] {
        assert_eq!(pool.options().get_max_connections(), 1);
        assert_eq!(pool.options().get_idle_timeout(), None);
        assert_eq!(pool.options().get_max_lifetime(), None);
        sqlx::query("INSERT INTO item (name) VALUES ('first')")
            .execute(pool)
            .await
            .expect("the temporary database should be migrated");
    }
    assert!(!memory_path.exists(), "`:memory:` should not create a file");
}

#[tokio::test]
async fn opening_under_a_regular_file_reports_the_filesystem_error() {
    // Arrange
    let temp_dir = tempdir().expect("temp directory should be created");
    let blocking_file = temp_dir.path().join("not_a_directory");
    std::fs::write(&blocking_file, b"").expect("blocking file should be written");

    // Act
    let result = open(&blocking_file.join("store.db"), &migrator(&[]), OPTIONS).await;

    // Assert
    let error = result.expect_err("a parent that is a file should fail the open");
    assert!(matches!(error, Error::Io(_)), "unexpected error: {error:?}");
}

#[tokio::test]
async fn opening_a_directory_reports_the_failed_connection() {
    // Arrange
    let temp_dir = tempdir().expect("temp directory should be created");

    // Act
    let result = open(temp_dir.path(), &migrator(&[]), OPTIONS).await;

    // Assert
    let error = result.expect_err("a directory should not open as a database");
    assert!(
        matches!(
            error,
            Error::Sqlx {
                operation: "connect to database",
                ..
            }
        ),
        "unexpected error: {error:?}"
    );
    assert!(
        error
            .to_string()
            .starts_with("SQLite operation `connect to database` failed: ")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_transaction_holds_the_write_lock_from_its_first_read() {
    // Arrange
    let temp_dir = tempdir().expect("temp directory should be created");
    let path = temp_dir.path().join("store.db");
    let migrator = migrator(&[CREATE_ITEM]);
    let first = open(&path, &migrator, OPTIONS)
        .await
        .expect("first pool should open");
    let second = open(&path, &migrator, OPTIONS)
        .await
        .expect("second pool should open");
    let mut impatient_writer = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&path)
            .busy_timeout(Duration::ZERO),
    )
    .await
    .expect("impatient writer should connect");

    // Act
    let mut transaction = begin_write(&first)
        .await
        .expect("write transaction should begin");
    let item_count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM item")
        .fetch_one(&mut *transaction)
        .await
        .expect("items should be counted");
    let impatient_result = sqlx::query("INSERT INTO item (name) VALUES ('impatient')")
        .execute(&mut impatient_writer)
        .await;
    let mut waiting_writer = tokio::spawn(async move {
        sqlx::query("INSERT INTO item (name) VALUES ('waiting')")
            .execute(&second)
            .await
    });
    let waited_while_locked = tokio::time::timeout(PENDING_WINDOW, &mut waiting_writer)
        .await
        .is_err();
    sqlx::query("INSERT INTO item (name) VALUES (?)")
        .bind(item_count.to_string())
        .execute(&mut *transaction)
        .await
        .expect("a write after a read should keep the write lock");
    transaction
        .commit()
        .await
        .expect("write transaction should commit");
    let waiting_result = waiting_writer
        .await
        .expect("waiting writer should not panic");

    // Assert
    let impatient_error =
        impatient_result.expect_err("another writer should not get the lock mid-transaction");
    assert!(
        impatient_error.to_string().contains("database is locked"),
        "unexpected error: {impatient_error}"
    );
    assert!(
        waited_while_locked,
        "the other pool's writer should still wait while the transaction holds the lock"
    );
    waiting_result.expect("a writer within the busy timeout should wait, not fail");
    let names = sqlx::query_scalar::<_, String>("SELECT name FROM item ORDER BY id")
        .fetch_all(&first)
        .await
        .expect("items should load");
    assert_eq!(names, ["0", "waiting"]);
}

#[tokio::test]
async fn read_transaction_keeps_its_snapshot_without_blocking_writers() {
    // Arrange
    let temp_dir = tempdir().expect("temp directory should be created");
    let path = temp_dir.path().join("store.db");
    let migrator = migrator(&[CREATE_ITEM]);
    let reader = open(&path, &migrator, OPTIONS)
        .await
        .expect("reader pool should open");
    let mut impatient_writer = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&path)
            .busy_timeout(Duration::ZERO),
    )
    .await
    .expect("impatient writer should connect");

    // Act
    let mut snapshot = begin_read(&reader)
        .await
        .expect("read transaction should begin");
    let before = item_count(&mut snapshot).await;
    let write_result = sqlx::query("INSERT INTO item (name) VALUES ('concurrent')")
        .execute(&mut impatient_writer)
        .await;
    let during = item_count(&mut snapshot).await;
    snapshot
        .commit()
        .await
        .expect("read transaction should end");

    // Assert
    write_result.expect("a writer should not wait for a read transaction");
    assert_eq!((before, during), (0, 0));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM item")
            .fetch_one(&reader)
            .await
            .expect("items should be counted"),
        1
    );
}

/// Returns a migrator applying `statements` as versions 1, 2, and so on.
fn migrator(statements: &[&'static str]) -> Migrator {
    Migrator::with_migrations(
        statements
            .iter()
            .zip(1..)
            .map(|(statement, version)| {
                Migration::new(
                    version,
                    "test migration".into(),
                    MigrationType::Simple,
                    statement.into_sql_str(),
                    false,
                )
            })
            .collect(),
    )
}

/// Opens a one-connection in-memory pool with the test connection policy.
async fn memory_pool() -> SqlitePool {
    sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(OPTIONS.connect_options(Path::new(":memory:")))
        .await
        .expect("in-memory pool should open")
}

/// Reads one `PRAGMA` value from `pool`.
async fn pragma<Value>(pool: &SqlitePool, name: &str) -> Value
where
    Value: Send + Unpin + for<'row> sqlx::Decode<'row, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>,
{
    sqlx::query_scalar::<_, Value>(sqlx::AssertSqlSafe(format!("PRAGMA {name}")))
        .fetch_one(pool)
        .await
        .expect("pragma should load")
}

/// Counts the items visible to `connection`.
async fn item_count(connection: &mut SqliteConnection) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM item")
        .fetch_one(connection)
        .await
        .expect("items should be counted")
}

/// Returns the sorted file names in `directory`.
fn directory_entries(directory: &Path) -> Vec<String> {
    let mut names = std::fs::read_dir(directory)
        .expect("directory should be readable")
        .map(|entry| {
            entry
                .expect("directory entry should be readable")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    names.sort();

    names
}

/// Opens a connection to `path` that holds the write lock.
async fn hold_write_lock(path: &Path) -> SqliteConnection {
    let options = SqliteConnectOptions::new().filename(path);
    let mut connection = SqliteConnection::connect_with(&options)
        .await
        .expect("lock holder should connect");
    sqlx::raw_sql("BEGIN IMMEDIATE")
        .execute(&mut connection)
        .await
        .expect("lock holder should take the write lock");

    connection
}

/// Releases the write lock `lock_holder` took.
async fn release_write_lock(lock_holder: &mut SqliteConnection) {
    sqlx::raw_sql("ROLLBACK")
        .execute(lock_holder)
        .await
        .expect("lock holder should release the write lock");
}
