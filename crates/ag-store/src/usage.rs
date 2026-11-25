//! Session-usage persistence adapters and query helpers.

use std::sync::Arc;

use ag_contracts::{TurnUsage, UsagePurpose};
use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::DbError;
use crate::connection::begin_write;
use crate::session::persist_turn_usage;
use crate::timestamp::TimestampSource;

/// Row returned when loading one model's token usage for one purpose from
/// the `session_usage` table.
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
    /// Persisted label of the work the usage served, as
    /// [`UsagePurpose::as_str`] writes it.
    pub purpose: String,
    /// Owning session identifier, when present.
    pub session_id: Option<String>,
}

/// Session-usage persistence boundary used by app orchestration and tests.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait UsageRepository: Send + Sync {
    /// Loads a session's token usage rows, one per model and purpose,
    /// ordered by model and purpose.
    async fn load_session_usage(&self, session_id: &str) -> Result<Vec<SessionUsageRow>, DbError>;

    /// Records one invocation's usage of every model under `purpose`, and
    /// adds its tokens to the session's totals.
    async fn upsert_session_usage(
        &self,
        session_id: &str,
        purpose: UsagePurpose,
        usage: &TurnUsage,
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
}

#[async_trait]
impl UsageRepository for SqliteUsageRepository {
    async fn load_session_usage(&self, session_id: &str) -> Result<Vec<SessionUsageRow>, DbError> {
        let rows = sqlx::query_as!(
            SessionUsageRow,
            r#"
SELECT session_id, model, purpose, created_at, input_tokens, invocation_count, output_tokens
FROM session_usage
WHERE session_id = ?
ORDER BY model, purpose
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
        purpose: UsagePurpose,
        usage: &TurnUsage,
    ) -> Result<(), DbError> {
        let now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = begin_write(&self.pool, "record session usage").await?;
        persist_turn_usage(&mut transaction, session_id, purpose, now, usage).await?;
        transaction.commit().await?;

        Ok(())
    }
}
