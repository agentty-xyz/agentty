//! Contention tests: concurrent first opens of one database file create and
//! migrate it once, and opening an up-to-date database does not wait behind
//! another writer.

use std::path::Path;

use sqlx::migrate::{MigrateError, Migration, MigrationType, Migrator};
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqlitePoolOptions};
use sqlx::{Connection as _, SqlSafeStr as _, SqlitePool};
use tempfile::tempdir;
use tokio::task::JoinSet;

use crate::session::SessionError;
use crate::sqlite_store::{Database, MIGRATOR, connect_options, create_wal_database, migrate};

const CONCURRENT_OPENS: usize = 8;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_opens_of_a_new_file_migrate_it_once() {
    // Arrange
    let temp_dir = tempdir().expect("temp directory should be created");
    let database_path = temp_dir.path().join("nested/harness.db");
    let mut opens = JoinSet::new();
    for _ in 0..CONCURRENT_OPENS {
        let database_path = database_path.clone();
        opens.spawn(async move { Database::open(&database_path).await });
    }

    // Act
    let mut databases = Vec::new();
    while let Some(result) = opens.join_next().await {
        databases.push(result.expect("open task should not panic"));
    }

    // Assert
    let databases = databases
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("every concurrent first open should succeed");
    let (applied, distinct, succeeded) = sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT COUNT(*), COUNT(DISTINCT version), SUM(success) FROM _sqlx_migrations",
    )
    .fetch_one(databases[0].pool())
    .await
    .expect("applied migrations should load");
    let expected = i64::try_from(MIGRATOR.iter().count()).expect("migration count fits");
    assert_eq!(
        (applied, distinct, succeeded),
        (expected, expected, expected)
    );
    assert!(
        directory_entries(&temp_dir.path().join("nested"))
            .iter()
            .all(|name| name.starts_with("harness.db")),
        "staged database files should not remain"
    );
}

#[tokio::test]
async fn creating_an_existing_database_keeps_it_and_leaves_no_staged_file() {
    // Arrange
    let temp_dir = tempdir().expect("temp directory should be created");
    let database_path = temp_dir.path().join("harness.db");
    std::fs::write(&database_path, b"existing").expect("existing file should be written");

    // Act
    let result = create_wal_database(&database_path).await;

    // Assert
    result.expect("an existing database should be kept");
    assert_eq!(
        std::fs::read(&database_path).expect("existing file should be readable"),
        b"existing"
    );
    assert_eq!(
        directory_entries(temp_dir.path()),
        vec!["harness.db".to_string()]
    );
}

#[tokio::test]
async fn opening_an_up_to_date_database_does_not_take_the_write_lock() {
    // Arrange
    let temp_dir = tempdir().expect("temp directory should be created");
    let database_path = temp_dir.path().join("harness.db");
    Database::open(&database_path)
        .await
        .expect("first open should migrate the database")
        .pool()
        .close()
        .await;
    let mut lock_holder = hold_write_lock(&database_path).await;

    // Act
    let reopened = Database::open(&database_path).await;

    // Assert
    reopened.expect("an up-to-date database should open while another connection writes");
    sqlx::raw_sql("ROLLBACK")
        .execute(&mut lock_holder)
        .await
        .expect("lock holder should release the write lock");
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
            &error,
            SessionError::Store { source, .. }
                if matches!(source.downcast_ref(), Some(MigrateError::Source(_)))
        ),
        "unexpected error: {error:?}"
    );
    assert!(
        error
            .to_string()
            .contains("migration 1 opts out of transactions")
    );
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

/// Opens a connection to `database_path` that holds the write lock.
async fn hold_write_lock(database_path: &Path) -> SqliteConnection {
    let options = SqliteConnectOptions::new().filename(database_path);
    let mut connection = SqliteConnection::connect_with(&options)
        .await
        .expect("lock holder should connect");
    sqlx::raw_sql("BEGIN IMMEDIATE")
        .execute(&mut connection)
        .await
        .expect("lock holder should take the write lock");

    connection
}

/// Opens a one-connection in-memory pool with the harness connection policy.
async fn memory_pool() -> SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(connect_options(Path::new(":memory:")))
        .await
        .expect("in-memory pool should open")
}
