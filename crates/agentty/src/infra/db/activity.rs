//! Session-activity persistence adapters and query helpers.

use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::infra::db::DbError;

/// Session-activity persistence boundary used by app orchestration and tests.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait ActivityRepository: Send + Sync {
    #[cfg(test)]
    /// Rebuilds `session_activity` rows from current `session.created_at`.
    async fn backfill_session_activity_from_sessions(&self) -> Result<(), DbError>;

    #[cfg(test)]
    /// Deletes all rows from `session_activity`.
    async fn clear_session_activity(&self) -> Result<(), DbError>;

    /// Persists one session-creation activity event at a specific Unix
    /// timestamp.
    async fn insert_session_creation_activity_at(
        &self,
        session_id: &str,
        timestamp_seconds: i64,
    ) -> Result<(), DbError>;

    /// Loads persisted session-creation timestamps for clock-aware activity
    /// aggregation.
    async fn load_session_activity_timestamps(&self) -> Result<Vec<i64>, DbError>;
}

/// `SQLite` implementation of [`ActivityRepository`].
#[derive(Clone)]
pub(crate) struct SqliteActivityRepository(SqlitePool);

impl SqliteActivityRepository {
    /// Creates an activity repository backed by the provided pool.
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self(pool)
    }
}

/// Row returned when loading one session activity timestamp.
struct TimestampValueRow {
    created_at: i64,
}

#[async_trait]
impl ActivityRepository for SqliteActivityRepository {
    #[cfg(test)]
    async fn backfill_session_activity_from_sessions(&self) -> Result<(), DbError> {
        sqlx::query!(
            r"
INSERT INTO session_activity (session_id, created_at)
SELECT id, created_at
FROM session
"
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    #[cfg(test)]
    async fn clear_session_activity(&self) -> Result<(), DbError> {
        sqlx::query!(
            r"
DELETE FROM session_activity
"
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn insert_session_creation_activity_at(
        &self,
        session_id: &str,
        timestamp_seconds: i64,
    ) -> Result<(), DbError> {
        sqlx::query!(
            r"
INSERT INTO session_activity (session_id, created_at)
VALUES (?, ?)
ON CONFLICT(session_id) DO NOTHING
",
            session_id,
            timestamp_seconds
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn load_session_activity_timestamps(&self) -> Result<Vec<i64>, DbError> {
        let rows = sqlx::query_as!(
            TimestampValueRow,
            r"
SELECT created_at
FROM session_activity
ORDER BY id
",
        )
        .fetch_all(&self.0)
        .await?;

        Ok(rows.into_iter().map(|row| row.created_at).collect())
    }
}
