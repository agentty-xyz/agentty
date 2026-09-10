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
        Self::append_normalized_in_transaction(&mut transaction, id, kind, &content, now).await?;

        transaction
            .commit()
            .await
            .db_context(APPEND_SESSION_MESSAGE)?;

        Ok(())
    }

    /// Appends normalized content within the caller's transaction, preserving
    /// transcript ordering and the owning session's modification timestamp.
    pub(super) async fn append_normalized_in_transaction(
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: &str,
        kind: SessionMessageKind,
        content: &str,
        now: i64,
    ) -> Result<(), DbError> {
        let update_result = sqlx::query!(
            r"
UPDATE session
SET updated_at = ?
WHERE id = ?
",
            now,
            id
        )
        .execute(&mut **transaction)
        .await
        .db_context(APPEND_SESSION_MESSAGE)?;

        if update_result.rows_affected() == 0 {
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
        .execute(&mut **transaction)
        .await
        .db_context(APPEND_SESSION_MESSAGE)?;

        Ok(())
    }
}

#[cfg(test)]
#[path = "session_message_test.rs"]
mod tests;
