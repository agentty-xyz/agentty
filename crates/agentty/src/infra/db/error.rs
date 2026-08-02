//! Database error types shared by persistence adapters.

/// Typed error returned by database operations.
///
/// Wraps the underlying `SQLx`, migration, and I/O failures so callers can
/// distinguish error categories without parsing opaque strings.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// A SQL query or connection-pool operation failed.
    #[error("{0}")]
    Query(#[from] sqlx::Error),

    /// An embedded schema migration failed during database open.
    #[error("{0}")]
    Migration(#[from] sqlx::migrate::MigrateError),

    /// A filesystem operation failed, such as creating the database directory.
    #[error("{0}")]
    Io(#[from] std::io::Error),

    /// A persisted lifecycle value could not be decoded by its owning adapter.
    #[error("Invalid {entity} lifecycle status `{value}`")]
    InvalidStatus {
        /// Persistence entity whose status failed validation.
        entity: &'static str,
        /// Unrecognized stored or caller-provided value.
        value: String,
    },

    /// A query failed during a named persistence operation.
    #[error("Database operation `{operation}` failed: {source}")]
    QueryContext {
        /// Stable semantic label for the failed operation.
        operation: &'static str,
        /// Underlying `SQLx` failure.
        #[source]
        source: sqlx::Error,
    },
}

/// Adds a semantic persistence-operation label to a query result.
pub(crate) trait DbResultExt<T> {
    /// Maps a raw `SQLx` error into [`DbError::QueryContext`].
    fn db_context(self, operation: &'static str) -> Result<T, DbError>;
}

impl<T> DbResultExt<T> for Result<T, sqlx::Error> {
    fn db_context(self, operation: &'static str) -> Result<T, DbError> {
        self.map_err(|source| DbError::QueryContext { operation, source })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            "Database operation `load session` failed: no rows returned by a query that expected \
             to return at least one row"
        );
    }
}
