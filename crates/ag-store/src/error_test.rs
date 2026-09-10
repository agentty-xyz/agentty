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
