//! Contention tests: concurrent first opens of one database file create and
//! migrate it once, opening an up-to-date database does not wait behind
//! another writer, and a write transaction that reads first holds the write
//! lock while writers from other pools wait for it instead of failing.

use std::path::Path;
use std::time::Duration;

use ag_session::SettingName;
use sqlx::migrate::{MigrateError, Migration, MigrationType, Migrator};
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqlitePoolOptions};
use sqlx::{Connection as _, SqlSafeStr as _};
use tempfile::tempdir;
use tokio::task::JoinSet;

use crate::connection::{
    Database, MIGRATOR, begin_write, create_wal_database, in_memory_connect_options, migrate,
};
use crate::error::DbError;

const CONCURRENT_OPENS: usize = 8;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_opens_of_a_new_file_migrate_it_once() {
    // Arrange
    let temp_dir = tempdir().expect("temp directory should be created");
    let db_path = temp_dir.path().join("nested/agentty.db");
    let mut opens = JoinSet::new();
    for _ in 0..CONCURRENT_OPENS {
        let db_path = db_path.clone();
        opens.spawn(async move { Database::open(&db_path).await });
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
            .all(|name| name.starts_with("agentty.db")),
        "staged database files should not remain"
    );
}

#[tokio::test]
async fn creating_an_existing_database_keeps_it_and_leaves_no_staged_file() {
    // Arrange
    let temp_dir = tempdir().expect("temp directory should be created");
    let db_path = temp_dir.path().join("agentty.db");
    std::fs::write(&db_path, b"existing").expect("existing file should be written");

    // Act
    let result = create_wal_database(&db_path).await;

    // Assert
    result.expect("an existing database should be kept");
    assert_eq!(
        std::fs::read(&db_path).expect("existing file should be readable"),
        b"existing"
    );
    assert_eq!(
        directory_entries(temp_dir.path()),
        vec!["agentty.db".to_string()]
    );
}

#[tokio::test]
async fn opening_an_up_to_date_database_does_not_take_the_write_lock() {
    // Arrange
    let temp_dir = tempdir().expect("temp directory should be created");
    let db_path = temp_dir.path().join("agentty.db");
    Database::open(&db_path)
        .await
        .expect("first open should migrate the database")
        .pool()
        .close()
        .await;
    let mut lock_holder = hold_write_lock(&db_path).await;

    // Act
    let reopened = Database::open(&db_path).await;

    // Assert
    reopened.expect("an up-to-date database should open while another connection writes");
    sqlx::raw_sql("ROLLBACK")
        .execute(&mut lock_holder)
        .await
        .expect("lock holder should release the write lock");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_transaction_holds_the_write_lock_from_its_first_read() {
    // Arrange
    let temp_dir = tempdir().expect("temp directory should be created");
    let db_path = temp_dir.path().join("agentty.db");
    let first = Database::open(&db_path)
        .await
        .expect("first pool should open");
    let second = Database::open(&db_path)
        .await
        .expect("second pool should open");
    let mut impatient_writer = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&db_path)
            .busy_timeout(Duration::ZERO),
    )
    .await
    .expect("impatient writer should connect");

    // Act
    let mut transaction = begin_write(first.pool())
        .await
        .expect("write transaction should begin");
    let setting_count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM setting")
        .fetch_one(&mut *transaction)
        .await
        .expect("settings should be counted");
    let impatient_result =
        sqlx::query("INSERT INTO setting (name, value) VALUES ('ImpatientWriter', 'x')")
            .execute(&mut impatient_writer)
            .await;
    let waiting_writer = tokio::spawn(async move {
        second
            .settings()
            .upsert_setting(SettingName::ProjectDiscoveryPromptSeen, "true")
            .await
    });
    sqlx::query("INSERT INTO setting (name, value) VALUES ('ReadFirstWriter', ?)")
        .bind(setting_count.to_string())
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
    waiting_result.expect("a writer within the busy timeout should wait, not fail");
    let stored = first
        .settings()
        .get_setting(SettingName::ProjectDiscoveryPromptSeen)
        .await
        .expect("setting should load");
    assert_eq!(stored.as_deref(), Some("true"));
}

#[tokio::test]
async fn migrations_opting_out_of_transactions_are_rejected() {
    // Arrange
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(in_memory_connect_options())
        .await
        .expect("in-memory pool should open");
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
        matches!(error, DbError::Migration(MigrateError::Source(_))),
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

/// Opens a connection to `db_path` that holds the write lock.
async fn hold_write_lock(db_path: &Path) -> SqliteConnection {
    let options = SqliteConnectOptions::new().filename(db_path);
    let mut connection = SqliteConnection::connect_with(&options)
        .await
        .expect("lock holder should connect");
    sqlx::raw_sql("BEGIN IMMEDIATE")
        .execute(&mut connection)
        .await
        .expect("lock holder should take the write lock");

    connection
}
