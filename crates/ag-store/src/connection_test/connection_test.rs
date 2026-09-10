use std::path::Path;

use tempfile::tempdir;

use crate::connection::{DB_POOL_MAX_CONNECTIONS, Database};
use crate::error::DbError;

/// Verifies `open()` creates missing parent directories before opening the
/// on-disk database.
#[tokio::test]
async fn test_open_creates_missing_parent_directory() {
    // Arrange
    let temp_dir = tempdir().expect("temp dir should be created");
    let db_path = temp_dir.path().join("nested/store.db");

    // Act
    let database = Database::open(&db_path)
        .await
        .expect("database should open with missing parent directories");

    // Assert
    assert!(db_path.parent().is_some_and(Path::is_dir));
    assert!(!database.pool().is_closed());
}

#[tokio::test]
async fn query_on_dropped_table_returns_db_error_query() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open database");
    sqlx::query!("DROP TABLE session")
        .execute(database.pool())
        .await
        .expect("failed to drop table");

    // Act
    let result = database.sessions().load_sessions_metadata().await;

    // Assert
    assert!(
        matches!(result, Err(DbError::Query(_))),
        "expected DbError::Query variant"
    );
}

#[tokio::test]
async fn db_error_display_includes_underlying_message() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open database");
    sqlx::query!("DROP TABLE session")
        .execute(database.pool())
        .await
        .expect("failed to drop table");

    // Act
    let result = database.sessions().load_sessions_metadata().await;

    // Assert
    let error = result.expect_err("expected query on dropped table to fail");
    let display_text = error.to_string();
    assert!(
        !display_text.is_empty(),
        "DbError Display should produce a non-empty message"
    );
}

#[tokio::test]
async fn open_with_unwritable_parent_returns_db_error_io() {
    // Arrange — place the database path under a regular file so
    // `create_dir_all` fails with an I/O error.
    let temp = tempdir().expect("failed to create temp directory");
    let blocking_file = temp.path().join("not_a_dir");
    std::fs::write(&blocking_file, b"").expect("failed to create blocking file");
    let db_path = blocking_file.join("nested").join("db.sqlite");

    // Act
    let result = Database::open(&db_path).await;

    // Assert
    assert!(
        matches!(result, Err(DbError::Io(_))),
        "expected DbError::Io variant"
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
    assert_eq!(busy_timeout, 2_000, "expected PRAGMA busy_timeout = 2000");
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
    assert_eq!(busy_timeout, 2_000, "expected PRAGMA busy_timeout = 2000");
}
