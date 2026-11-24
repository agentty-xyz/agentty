//! Database connection configuration and error reporting tests.
//!
//! `ag-sqlite` tests directory creation, temporary paths, and open failures.

use tempfile::tempdir;

use crate::connection::{DB_POOL_MAX_CONNECTIONS, Database, INTERACTIVE_BUSY_TIMEOUT};
use crate::error::DbError;

#[tokio::test]
async fn query_on_dropped_table_returns_db_error_query() {
    // Arrange
    let database = Database::in_memory()
        .await
        .expect("failed to open database");
    sqlx::query!("DROP TABLE session")
        .execute(database.pool())
        .await
        .expect("failed to drop table");

    // Act
    let result = database.sessions().load_sessions_metadata(1).await;

    // Assert
    assert!(
        matches!(result, Err(DbError::Query(_))),
        "expected DbError::Query variant"
    );
}

#[tokio::test]
async fn db_error_display_includes_underlying_message() {
    // Arrange
    let database = Database::in_memory()
        .await
        .expect("failed to open database");
    sqlx::query!("DROP TABLE session")
        .execute(database.pool())
        .await
        .expect("failed to drop table");

    // Act
    let result = database.sessions().load_sessions_metadata(1).await;

    // Assert
    let error = result.expect_err("expected query on dropped table to fail");
    let display_text = error.to_string();
    assert!(
        !display_text.is_empty(),
        "DbError Display should produce a non-empty message"
    );
}

#[tokio::test]
async fn open_configures_small_wal_pool_normal_synchronous_mode_and_busy_timeout() {
    // Arrange
    let temp = tempdir().expect("failed to create temp directory");
    let db_path = temp.path().join("agentty.db");

    // Act
    let database = Database::open(&db_path)
        .await
        .expect("failed to open database");
    let journal_mode = sqlx::query_scalar!(
        r#"
SELECT journal_mode || '' AS "journal_mode!: String"
FROM pragma_journal_mode
"#
    )
    .fetch_one(database.pool())
    .await
    .expect("failed to load journal mode pragma");
    let synchronous = sqlx::query_scalar!(
        r#"
SELECT synchronous + 0 AS "synchronous!: i64"
FROM pragma_synchronous
"#
    )
    .fetch_one(database.pool())
    .await
    .expect("failed to load synchronous pragma");
    let busy_timeout = sqlx::query_scalar!(
        r#"
SELECT timeout + 0 AS "timeout!: i64"
FROM pragma_busy_timeout
"#
    )
    .fetch_one(database.pool())
    .await
    .expect("failed to load busy-timeout pragma");

    // Assert
    // `SqliteConnectOptions` are reused for every pooled connection, so
    // checking one pooled connection here is sufficient to prove the
    // configured busy timeout propagates across the on-disk pool.
    assert_eq!(
        database.pool().options().get_max_connections(),
        DB_POOL_MAX_CONNECTIONS
    );
    assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
    assert_eq!(synchronous, 1, "expected PRAGMA synchronous = NORMAL");
    assert_eq!(busy_timeout, expected_busy_timeout_millis());
    assert_eq!(
        database.pool().options().get_acquire_timeout(),
        INTERACTIVE_BUSY_TIMEOUT
    );
}

#[tokio::test]
async fn open_in_memory_uses_single_connection_normal_synchronous_mode_and_busy_timeout() {
    // Arrange, Act
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory database");
    let synchronous = sqlx::query_scalar!(
        r#"
SELECT synchronous + 0 AS "synchronous!: i64"
FROM pragma_synchronous
"#
    )
    .fetch_one(database.pool())
    .await
    .expect("failed to load synchronous pragma");
    let busy_timeout = sqlx::query_scalar!(
        r#"
SELECT timeout + 0 AS "timeout!: i64"
FROM pragma_busy_timeout
"#
    )
    .fetch_one(database.pool())
    .await
    .expect("failed to load busy-timeout pragma");

    // Assert
    assert_eq!(database.pool().options().get_max_connections(), 1);
    assert_eq!(synchronous, 1, "expected PRAGMA synchronous = NORMAL");
    assert_eq!(busy_timeout, expected_busy_timeout_millis());
}

/// Returns `INTERACTIVE_BUSY_TIMEOUT` as `PRAGMA busy_timeout` reports it.
fn expected_busy_timeout_millis() -> i64 {
    i64::try_from(INTERACTIVE_BUSY_TIMEOUT.as_millis()).expect("busy timeout fits in i64")
}
