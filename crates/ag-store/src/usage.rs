//! Session-usage persistence adapters and query helpers.

use std::sync::Arc;

use ag_agent::SessionStats;
use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::DbError;
use crate::timestamp::TimestampSource;

/// Row returned when loading per-model token usage from the `session_usage`
/// table.
pub struct SessionUsageRow {
    /// Row creation timestamp in Unix seconds.
    pub created_at: i64,
    /// Accumulated input-token count.
    pub input_tokens: i64,
    /// Number of agent invocations included in the totals.
    pub invocation_count: i64,
    /// Provider model identifier.
    pub model: String,
    /// Accumulated output-token count.
    pub output_tokens: i64,
    /// Owning session identifier, when present.
    pub session_id: Option<String>,
}

/// Session-usage persistence boundary used by app orchestration and tests.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait UsageRepository: Send + Sync {
    /// Loads per-model token usage rows for a session, ordered by model name.
    async fn load_session_usage(&self, session_id: &str) -> Result<Vec<SessionUsageRow>, DbError>;

    /// Accumulates per-model token usage for a session.
    async fn upsert_session_usage(
        &self,
        session_id: &str,
        model: &str,
        stats: &SessionStats,
    ) -> Result<(), DbError>;
}

/// `SQLite` implementation of [`UsageRepository`].
#[derive(Clone)]
pub(crate) struct SqliteUsageRepository {
    pool: SqlitePool,
    timestamp_source: Arc<dyn TimestampSource>,
}

impl SqliteUsageRepository {
    /// Creates a usage repository backed by the provided pool and timestamp
    /// source.
    pub(crate) fn new(pool: SqlitePool, timestamp_source: Arc<dyn TimestampSource>) -> Self {
        Self {
            pool,
            timestamp_source,
        }
    }

    fn now(&self) -> i64 {
        self.timestamp_source.now_timestamp_seconds()
    }
}

#[async_trait]
impl UsageRepository for SqliteUsageRepository {
    async fn load_session_usage(&self, session_id: &str) -> Result<Vec<SessionUsageRow>, DbError> {
        let rows = sqlx::query_as!(
            SessionUsageRow,
            r#"
SELECT session_id, model, created_at, input_tokens, invocation_count, output_tokens
FROM session_usage
WHERE session_id = ?
ORDER BY model
            "#,
            session_id
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }

    async fn upsert_session_usage(
        &self,
        session_id: &str,
        model: &str,
        stats: &SessionStats,
    ) -> Result<(), DbError> {
        if stats.input_tokens == 0 && stats.output_tokens == 0 {
            return Ok(());
        }

        let now = self.now();

        sqlx::query!(
            r"
INSERT INTO session_usage (
    session_id, model, created_at, input_tokens, output_tokens, invocation_count
)
VALUES (?, ?, ?, ?, ?, 1)
ON CONFLICT(session_id, model) DO UPDATE SET
    input_tokens = input_tokens + excluded.input_tokens,
    output_tokens = output_tokens + excluded.output_tokens,
    invocation_count = invocation_count + 1
",
            session_id,
            model,
            now,
            stats.input_tokens.cast_signed(),
            stats.output_tokens.cast_signed()
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}
