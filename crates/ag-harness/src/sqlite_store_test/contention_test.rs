//! Contention tests: concurrent opens, resumes, and writes wait out another
//! connection's write lock instead of failing with `database is locked`.

use std::path::{Path, PathBuf};

use sqlx::Connection as _;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqliteJournalMode};
use tempfile::tempdir;
use tokio::task::JoinSet;

use super::support::schema;
use crate::session::{NewSession, SessionError};
use crate::sqlite_store::{Database, MIGRATOR};
use crate::store::SessionStore as _;

const CONCURRENT_HANDLES: usize = 4;

#[tokio::test]
async fn concurrent_opens_wait_for_a_held_write_lock_and_migrate_once() {
    // Arrange
    let temp_dir = tempdir().expect("temp directory should be created");
    let database_path = temp_dir.path().join("harness.db");
    let mut lock_holder = hold_write_lock(&database_path).await;
    let mut opens = JoinSet::new();
    for _ in 0..CONCURRENT_HANDLES {
        let database_path = database_path.clone();
        opens.spawn(async move { Database::open(&database_path).await });
    }
    tokio::task::yield_now().await;

    // Act
    release_write_lock(&mut lock_holder).await;
    let databases = join_all(opens).await;

    // Assert
    let databases = databases.expect("every concurrent open should succeed");
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
}

#[tokio::test]
async fn concurrent_writes_and_resumes_wait_for_a_held_write_lock() {
    // Arrange
    let temp_dir = tempdir().expect("temp directory should be created");
    let database_path = temp_dir.path().join("harness.db");
    let first = Database::open(&database_path)
        .await
        .expect("first handle should open");
    let second = Database::open(&database_path)
        .await
        .expect("second handle should open");
    first
        .create_session(&NewSession::new("existing", schema()), None, 100)
        .await
        .expect("existing session should be created");
    let mut lock_holder = hold_write_lock(&database_path).await;
    let mut writes = JoinSet::new();
    for index in 0..CONCURRENT_HANDLES {
        let database = if index % 2 == 0 {
            first.clone()
        } else {
            second.clone()
        };
        writes.spawn(async move {
            let session = NewSession::new(format!("session-{index}"), schema());
            database.create_session(&session, None, 100).await?;

            database.load_session("existing").await.map(|_| ())
        });
    }
    tokio::task::yield_now().await;

    // Act
    release_write_lock(&mut lock_holder).await;
    let results = join_all(writes).await;

    // Assert
    results.expect("every concurrent write and resume should succeed");
    let sessions = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM session")
        .fetch_one(second.pool())
        .await
        .expect("sessions should be counted");
    assert_eq!(
        sessions,
        i64::try_from(CONCURRENT_HANDLES + 1).expect("count fits")
    );
}

/// Opens a WAL connection to `database_path` that holds the write lock.
async fn hold_write_lock(database_path: &Path) -> SqliteConnection {
    let options = SqliteConnectOptions::new()
        .filename(PathBuf::from(database_path))
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);
    let mut connection = SqliteConnection::connect_with(&options)
        .await
        .expect("lock holder should connect");
    sqlx::raw_sql("BEGIN IMMEDIATE")
        .execute(&mut connection)
        .await
        .expect("lock holder should take the write lock");

    connection
}

/// Releases the write lock taken by [`hold_write_lock`].
async fn release_write_lock(connection: &mut SqliteConnection) {
    sqlx::raw_sql("COMMIT")
        .execute(connection)
        .await
        .expect("lock holder should release the write lock");
}

/// Waits for every task and returns their results in completion order.
async fn join_all<T: 'static>(
    mut tasks: JoinSet<Result<T, SessionError>>,
) -> Result<Vec<T>, SessionError> {
    let mut values = Vec::new();
    while let Some(result) = tasks.join_next().await {
        values.push(result.expect("task should not panic")?);
    }

    Ok(values)
}
