//! Database error query-context and open-failure conversion unit tests.

use std::io;

use sqlx::migrate::MigrateError;

use crate::error::{DbError, DbResultExt};

#[test]
fn query_context_preserves_operation_and_source() {
    // Arrange
    let result = Err::<(), _>(sqlx::Error::RowNotFound);

    // Act
    let error = result
        .db_context("load session")
        .expect_err("query should fail");

    // Assert
    assert!(matches!(
        error,
        DbError::QueryContext {
            operation: "load session",
            source: sqlx::Error::RowNotFound,
        }
    ));
    assert_eq!(
        error.to_string(),
        "Database operation `load session` failed: no rows returned by a query that expected to \
         return at least one row"
    );
}

#[test]
fn open_failures_keep_their_category() {
    // Arrange
    let io_error = ag_sqlite::Error::Io(io::Error::other("disk"));
    let query_error = ag_sqlite::Error::Sqlx {
        operation: "open app database",
        source: sqlx::Error::PoolTimedOut,
    };
    let migration_error = ag_sqlite::Error::Migrate {
        operation: "migrate app database",
        source: MigrateError::VersionMissing(7),
    };

    // Act
    let errors = [io_error, query_error, migration_error].map(DbError::from_open);

    // Assert
    assert!(matches!(errors[0], DbError::Io(_)));
    assert!(matches!(
        errors[1],
        DbError::QueryContext {
            operation: "open app database",
            source: sqlx::Error::PoolTimedOut,
        }
    ));
    assert!(matches!(
        errors[2],
        DbError::Migration(MigrateError::VersionMissing(7))
    ));
}
