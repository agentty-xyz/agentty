//! Session-usage persistence adapters and query helpers.

use std::sync::Arc;

use ag_contracts::{TurnUsage, UsagePurpose};
use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::DbError;
use crate::connection::begin_write;
use crate::session::persist_turn_usage;
use crate::timestamp::TimestampSource;

/// Row returned when loading per-model and per-purpose token usage from the
/// `session_usage` table.
///
/// A `None` detail is unknown: some invocation of the row did not report it.
/// Invocations recorded before detailed usage count in `invocation_count`
/// but not in `detailed_invocation_count`, and report no detail.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SessionUsageRow {
    /// Persisted label of the subscription login that billed the
    /// plan-billed invocations, as [`ag_contracts::PlanLogin::as_str`]
    /// writes it.
    pub api_equivalent_login: Option<String>,
    /// API equivalent of the plan-billed invocations in micro-USD, kept
    /// apart from the charge of metered ones.
    pub api_equivalent_micro_usd: Option<i64>,
    /// Accumulated input tokens served from the provider's prompt cache.
    pub cache_read_tokens: Option<i64>,
    /// Accumulated input tokens written to the provider's prompt cache.
    pub cache_write_tokens: Option<i64>,
    /// Largest request input observed.
    pub context_peak_tokens: Option<i64>,
    /// Context window of the model, when known.
    pub context_window_tokens: Option<i64>,
    /// Persisted label of the charge's price basis, `mixed` when invocations
    /// differ.
    pub cost_basis: Option<String>,
    /// Estimated charge of the metered invocations in micro-USD.
    pub cost_micro_usd: Option<i64>,
    /// Row creation timestamp in Unix seconds.
    pub created_at: i64,
    /// Invocations recorded with detailed usage.
    pub detailed_invocation_count: i64,
    /// Accumulated input-token count, cache included.
    pub input_tokens: i64,
    /// Number of agent invocations included in the totals.
    pub invocation_count: i64,
    /// Whether the API equivalent is an upper bound, because some
    /// plan-billed invocation's cache reads were priced at the input price.
    pub is_api_equivalent_upper_bound: bool,
    /// Whether the estimated charge is an upper bound, because some metered
    /// invocation's cache reads were priced at the input price.
    pub is_cost_upper_bound: bool,
    /// Provider model identifier.
    pub model: String,
    /// Accumulated output-token count, reasoning included.
    pub output_tokens: i64,
    /// Persisted label of the work the usage served, as
    /// [`UsagePurpose::as_str`] writes it.
    pub purpose: String,
    /// Accumulated reasoning tokens.
    pub reasoning_tokens: Option<i64>,
    /// Provider requests of the detailed invocations, unreported included.
    pub request_count: i64,
    /// Owning session identifier, when present.
    pub session_id: Option<String>,
    /// Detailed invocations that had no price.
    pub unpriced_invocation_count: i64,
    /// Requests whose backend reported no input or output count.
    pub unreported_request_count: i64,
}

/// Session-level context and turn figures loaded beside its usage rows.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SessionUsageContextRow {
    /// Input tokens of the session's latest model request.
    pub context_tokens: Option<i64>,
    /// Context window of the model that answered that request.
    pub context_window_tokens: Option<i64>,
    /// Prompts that started a user-visible or generated turn.
    pub turn_count: i64,
}

/// Session-usage persistence boundary used by app orchestration and tests.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait UsageRepository: Send + Sync {
    /// Loads a session's token usage rows, one per model and purpose,
    /// ordered by model and purpose.
    async fn load_session_usage(&self, session_id: &str) -> Result<Vec<SessionUsageRow>, DbError>;

    /// Loads the session's latest context fill and its turn count; an
    /// unknown session reads as empty.
    async fn load_session_usage_context(
        &self,
        session_id: &str,
    ) -> Result<SessionUsageContextRow, DbError>;

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
SELECT api_equivalent_login, api_equivalent_micro_usd, cache_read_tokens, cache_write_tokens,
       context_peak_tokens, context_window_tokens, cost_basis, cost_micro_usd, created_at,
       detailed_invocation_count, input_tokens, invocation_count,
       is_api_equivalent_upper_bound AS "is_api_equivalent_upper_bound: bool",
       is_cost_upper_bound AS "is_cost_upper_bound: bool", model, output_tokens, purpose,
       reasoning_tokens, request_count, session_id, unpriced_invocation_count,
       unreported_request_count
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

    async fn load_session_usage_context(
        &self,
        session_id: &str,
    ) -> Result<SessionUsageContextRow, DbError> {
        let row = sqlx::query_as!(
            SessionUsageContextRow,
            r#"
SELECT session.context_tokens AS "context_tokens?",
       session.context_window_tokens AS "context_window_tokens?",
       (
           SELECT COUNT(*)
           FROM session_message
           WHERE session_message.session_id = session.id
             AND session_message.kind IN ('user_prompt', 'agent_prompt')
       ) AS "turn_count!: i64"
FROM session
WHERE session.id = ?
            "#,
            session_id
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.unwrap_or_default())
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
