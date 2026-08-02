//! Session-operation persistence adapters and query helpers.

use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::infra::db::{DbError, unix_timestamp_now};

/// Persisted operation lifecycle state for one session command.
pub struct SessionOperationRow {
    /// Whether the owning workflow requested cancellation.
    pub cancel_requested: bool,
    /// Completion timestamp in Unix seconds, when finished.
    pub finished_at: Option<i64>,
    /// Most recent liveness timestamp in Unix seconds.
    pub heartbeat_at: Option<i64>,
    /// Stable operation identifier.
    pub id: String,
    /// Persisted operation-kind discriminator.
    pub kind: String,
    /// Most recent failure or cancellation reason.
    pub last_error: Option<String>,
    /// Queue-entry timestamp in Unix seconds.
    pub queued_at: i64,
    /// Session that owns the operation.
    pub session_id: String,
    /// Start timestamp in Unix seconds, when running.
    pub started_at: Option<i64>,
    /// Persisted operation-lifecycle status.
    pub status: String,
}

/// Session-operation persistence boundary used by app orchestration and tests.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait OperationRepository: Send + Sync {
    /// Marks unfinished operations as failed after process restart.
    async fn fail_unfinished_session_operations(&self, reason: &str) -> Result<(), DbError>;

    /// Returns whether cancellation is requested for a specific operation.
    async fn is_cancel_requested_for_operation(&self, operation_id: &str) -> Result<bool, DbError>;

    /// Returns whether an operation is still unfinished.
    async fn is_session_operation_unfinished(&self, operation_id: &str) -> Result<bool, DbError>;

    /// Loads operations still waiting in queue or currently running.
    async fn load_unfinished_session_operations(&self)
    -> Result<Vec<SessionOperationRow>, DbError>;

    /// Marks an operation as canceled.
    async fn mark_session_operation_canceled(
        &self,
        operation_id: &str,
        reason: &str,
    ) -> Result<(), DbError>;

    /// Marks an operation as completed successfully.
    async fn mark_session_operation_done(&self, operation_id: &str) -> Result<(), DbError>;

    /// Marks an operation as failed with an error message.
    async fn mark_session_operation_failed(
        &self,
        operation_id: &str,
        error: &str,
    ) -> Result<(), DbError>;

    /// Marks an operation as running and refreshes its heartbeat timestamp.
    async fn mark_session_operation_running(&self, operation_id: &str) -> Result<(), DbError>;

    /// Claims an idempotent queued operation.
    ///
    /// Returns `true` when the caller must enqueue the command. Existing
    /// queued, running, or completed operations return `false`; failed or
    /// canceled attempts are reset and reclaimed for restart recovery.
    async fn claim_session_operation(
        &self,
        operation_id: &str,
        session_id: &str,
        kind: &str,
    ) -> Result<bool, DbError>;

    /// Inserts a queued operation row for a session.
    async fn insert_session_operation(
        &self,
        operation_id: &str,
        session_id: &str,
        kind: &str,
    ) -> Result<(), DbError>;

    /// Requests cancellation for unfinished operations of a session.
    async fn request_cancel_for_session_operations(&self, session_id: &str) -> Result<(), DbError>;
}

/// `SQLite` implementation of [`OperationRepository`].
#[derive(Clone)]
pub(crate) struct SqliteOperationRepository(SqlitePool);

impl SqliteOperationRepository {
    /// Creates an operation repository backed by the provided pool.
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self(pool)
    }
}

/// Row returned when loading one non-null boolean scalar value.
struct RequiredBoolValueRow {
    value: bool,
}

#[async_trait]
impl OperationRepository for SqliteOperationRepository {
    async fn fail_unfinished_session_operations(&self, reason: &str) -> Result<(), DbError> {
        let now = unix_timestamp_now();

        sqlx::query!(
            r"
UPDATE session_operation
SET status = 'failed',
    finished_at = ?,
    heartbeat_at = ?,
    last_error = ?,
    cancel_requested = 1
WHERE status IN ('queued', 'running')
",
            now,
            now,
            reason
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn is_cancel_requested_for_operation(&self, operation_id: &str) -> Result<bool, DbError> {
        let row = sqlx::query_as!(
            RequiredBoolValueRow,
            r#"
SELECT EXISTS(
    SELECT 1
    FROM session_operation
    WHERE id = ?
      AND cancel_requested = 1
      AND status IN ('queued', 'running')
) AS "value!: _"
"#,
            operation_id
        )
        .fetch_one(&self.0)
        .await?;

        Ok(row.value)
    }

    async fn is_session_operation_unfinished(&self, operation_id: &str) -> Result<bool, DbError> {
        let row = sqlx::query_as!(
            RequiredBoolValueRow,
            r#"
SELECT EXISTS(
    SELECT 1
    FROM session_operation
    WHERE id = ?
      AND status IN ('queued', 'running')
) AS "value!: _"
"#,
            operation_id
        )
        .fetch_one(&self.0)
        .await?;

        Ok(row.value)
    }

    async fn load_unfinished_session_operations(
        &self,
    ) -> Result<Vec<SessionOperationRow>, DbError> {
        let rows = sqlx::query_as!(
            SessionOperationRow,
            r#"
SELECT id AS "id!", session_id AS "session_id!", kind AS "kind!", status AS "status!",
       queued_at, started_at, finished_at,
       heartbeat_at, last_error,
       cancel_requested AS "cancel_requested: _"
FROM session_operation
WHERE status IN ('queued', 'running')
ORDER BY queued_at ASC, id ASC
            "#
        )
        .fetch_all(&self.0)
        .await?;

        Ok(rows)
    }

    async fn mark_session_operation_canceled(
        &self,
        operation_id: &str,
        reason: &str,
    ) -> Result<(), DbError> {
        let now = unix_timestamp_now();

        sqlx::query!(
            r"
UPDATE session_operation
SET status = 'canceled',
    finished_at = ?,
    heartbeat_at = ?,
    last_error = ?
WHERE id = ?
",
            now,
            now,
            reason,
            operation_id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn mark_session_operation_done(&self, operation_id: &str) -> Result<(), DbError> {
        let now = unix_timestamp_now();

        sqlx::query!(
            r"
UPDATE session_operation
SET status = 'done',
    finished_at = ?,
    heartbeat_at = ?,
    last_error = NULL
WHERE id = ?
",
            now,
            now,
            operation_id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn mark_session_operation_failed(
        &self,
        operation_id: &str,
        error: &str,
    ) -> Result<(), DbError> {
        let now = unix_timestamp_now();

        sqlx::query!(
            r"
UPDATE session_operation
SET status = 'failed',
    finished_at = ?,
    heartbeat_at = ?,
    last_error = ?
WHERE id = ?
",
            now,
            now,
            error,
            operation_id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn mark_session_operation_running(&self, operation_id: &str) -> Result<(), DbError> {
        let now = unix_timestamp_now();

        sqlx::query!(
            r"
UPDATE session_operation
SET status = 'running',
    started_at = COALESCE(started_at, ?),
    heartbeat_at = ?,
    last_error = NULL
WHERE id = ?
",
            now,
            now,
            operation_id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn claim_session_operation(
        &self,
        operation_id: &str,
        session_id: &str,
        kind: &str,
    ) -> Result<bool, DbError> {
        let queued_at = unix_timestamp_now();

        let claimed = sqlx::query!(
            r#"
INSERT INTO session_operation (id, session_id, kind, status, queued_at)
VALUES (?, ?, ?, 'queued', ?)
ON CONFLICT(id) DO UPDATE SET
    session_id = excluded.session_id,
    kind = excluded.kind,
    status = 'queued',
    queued_at = excluded.queued_at,
    started_at = NULL,
    finished_at = NULL,
    heartbeat_at = NULL,
    last_error = NULL,
    cancel_requested = 0
WHERE session_operation.status IN ('failed', 'canceled')
RETURNING id AS "id!: String"
"#,
            operation_id,
            session_id,
            kind,
            queued_at
        )
        .fetch_optional(&self.0)
        .await?;

        Ok(claimed.is_some())
    }

    async fn insert_session_operation(
        &self,
        operation_id: &str,
        session_id: &str,
        kind: &str,
    ) -> Result<(), DbError> {
        let queued_at = unix_timestamp_now();

        sqlx::query!(
            r"
INSERT INTO session_operation (id, session_id, kind, status, queued_at)
VALUES (?, ?, ?, 'queued', ?)
",
            operation_id,
            session_id,
            kind,
            queued_at
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn request_cancel_for_session_operations(&self, session_id: &str) -> Result<(), DbError> {
        sqlx::query!(
            r"
UPDATE session_operation
SET cancel_requested = 1
WHERE session_id = ?
  AND status IN ('queued', 'running')
",
            session_id
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::infra::db::AppRepositories;

    #[tokio::test]
    /// Claims new and failed idempotent operations while leaving accepted
    /// operations untouched.
    async fn test_claim_session_operation_recovers_only_terminal_failures() {
        // Arrange
        let database = AppRepositories::in_memory().await;
        let project_id = database
            .projects()
            .upsert_project("/tmp/operation-project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        database
            .sessions()
            .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
            .await
            .expect("failed to insert session");

        // Act
        let first_claim = database
            .operations()
            .claim_session_operation("rollup-1", "session-a", "reply")
            .await
            .expect("failed to claim new operation");
        let queued_claim = database
            .operations()
            .claim_session_operation("rollup-1", "session-a", "reply")
            .await
            .expect("failed to inspect queued operation");
        database
            .operations()
            .mark_session_operation_failed("rollup-1", "restart")
            .await
            .expect("failed to mark operation failed");
        let recovered_claim = database
            .operations()
            .claim_session_operation("rollup-1", "session-a", "reply")
            .await
            .expect("failed to reclaim failed operation");
        database
            .operations()
            .mark_session_operation_done("rollup-1")
            .await
            .expect("failed to mark operation done");
        let done_claim = database
            .operations()
            .claim_session_operation("rollup-1", "session-a", "reply")
            .await
            .expect("failed to inspect completed operation");

        // Assert
        assert!(first_claim);
        assert!(!queued_claim);
        assert!(recovered_claim);
        assert!(!done_claim);
    }
}
