//! Transactional session-transcript persistence store.

use std::sync::Arc;

use ag_session::{SessionMessageKind, stored_message_content};
use sqlx::SqlitePool;

use crate::timestamp::TimestampSource;
use crate::{DbError, DbResultExt};

const APPEND_SESSION_MESSAGE: &str = "append session message";

/// Internal store that owns transcript ordering and its session timestamp.
#[derive(Clone)]
pub(super) struct SessionMessageStore {
    pool: SqlitePool,
    timestamp_source: Arc<dyn TimestampSource>,
}

impl SessionMessageStore {
    /// Creates a transcript store backed by one pool and timestamp source.
    pub(super) fn new(pool: SqlitePool, timestamp_source: Arc<dyn TimestampSource>) -> Self {
        Self {
            pool,
            timestamp_source,
        }
    }

    /// Appends one normalized message and updates the owning session
    /// atomically.
    pub(super) async fn append(
        &self,
        id: &str,
        kind: SessionMessageKind,
        content: &str,
    ) -> Result<(), DbError> {
        let content = stored_message_content(kind, content);
        if content.trim().is_empty() {
            return Ok(());
        }

        let now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = self.pool.begin().await.db_context(APPEND_SESSION_MESSAGE)?;
        let update_result = sqlx::query!(
            r"
UPDATE session
SET updated_at = ?
WHERE id = ?
",
            now,
            id
        )
        .execute(&mut *transaction)
        .await
        .db_context(APPEND_SESSION_MESSAGE)?;

        if update_result.rows_affected() == 0 {
            transaction
                .commit()
                .await
                .db_context(APPEND_SESSION_MESSAGE)?;

            return Ok(());
        }

        sqlx::query(
            r"
INSERT INTO session_message (session_id, position, kind, content, created_at)
SELECT ?, COALESCE(MAX(position), -1) + 1, ?, ?, ?
FROM session_message
WHERE session_id = ?
",
        )
        .bind(id)
        .bind(kind.as_str())
        .bind(content)
        .bind(now)
        .bind(id)
        .execute(&mut *transaction)
        .await
        .db_context(APPEND_SESSION_MESSAGE)?;

        transaction
            .commit()
            .await
            .db_context(APPEND_SESSION_MESSAGE)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use ag_session::SessionMessageKind;

    use super::*;
    use crate::AppRepositories;
    use crate::connection::open_in_memory_pool;

    #[tokio::test]
    async fn blank_message_returns_without_persistence() {
        // Arrange
        let pool = open_in_memory_pool(1)
            .await
            .expect("failed to open in-memory db");
        let repositories = AppRepositories::from_pool(pool);

        // Act
        let result = repositories
            .sessions()
            .append_session_message("missing-session", SessionMessageKind::UserPrompt, " \n ")
            .await;

        // Assert
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn append_failure_reports_semantic_operation_context() {
        // Arrange
        let pool = open_in_memory_pool(1)
            .await
            .expect("failed to open in-memory db");
        let repositories = AppRepositories::from_pool(pool.clone());
        let project_id = repositories
            .projects()
            .upsert_project("/tmp/message-context", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        repositories
            .sessions()
            .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        sqlx::query("DROP TABLE session_message")
            .execute(&pool)
            .await
            .expect("failed to drop message table");

        // Act
        let error = repositories
            .sessions()
            .append_session_message("session-a", SessionMessageKind::UserPrompt, "Persist this")
            .await
            .expect_err("append should fail");

        // Assert
        assert!(matches!(
            error,
            DbError::QueryContext {
                operation: APPEND_SESSION_MESSAGE,
                ..
            }
        ));
    }
}
