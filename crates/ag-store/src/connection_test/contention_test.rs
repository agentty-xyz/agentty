//! Contention tests: writers give up at the busy timeout the database was
//! opened with, and a labeled write transaction reports its operation.
//!
//! `ag-sqlite` tests the shared open, migration, and transaction locking.

use std::path::Path;
use std::time::Duration;

use ag_session::SettingName;
use sqlx::Connection as _;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use tempfile::tempdir;

use crate::connection::{Database, begin_write};
use crate::error::DbError;

/// Fails a contention regression fast, well inside the slow-test limit and
/// below the busy timeout it would otherwise wait out.
const REGRESSION_BOUND: Duration = Duration::from_secs(3);

/// Busy timeout short enough to observe a writer giving up.
const SHORT_BUSY_TIMEOUT: Duration = Duration::from_millis(100);

#[tokio::test]
async fn writes_give_up_at_the_busy_timeout_while_another_process_holds_the_lock() {
    // Arrange
    let temp_dir = tempdir().expect("temp directory should be created");
    let db_path = temp_dir.path().join("agentty.db");
    let database = Database::open_with_busy_timeout(&db_path, SHORT_BUSY_TIMEOUT)
        .await
        .expect("database should open");
    let mut lock_holder = hold_write_lock(&db_path).await;

    // Act
    let (write_result, begin_result) = tokio::time::timeout(REGRESSION_BOUND, async {
        let write_result = database
            .settings()
            .upsert_setting(SettingName::ActiveTab, "Sessions")
            .await;
        let begin_result = begin_write(database.pool(), "labeled writer").await;

        (write_result, begin_result.map(|_| ()))
    })
    .await
    .expect("writes should give up at the busy timeout instead of waiting for the lock");

    // Assert
    let write_error = write_result.expect_err("a write should fail while the lock is held");
    assert!(
        write_error.to_string().contains("database is locked"),
        "unexpected error: {write_error}"
    );
    let begin_error = begin_result.expect_err("a write transaction should not begin");
    assert!(
        matches!(
            begin_error,
            DbError::QueryContext {
                operation: "labeled writer",
                ..
            }
        ),
        "unexpected error: {begin_error:?}"
    );
    sqlx::raw_sql("ROLLBACK")
        .execute(&mut lock_holder)
        .await
        .expect("lock holder should release the write lock");
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
