//! Persistence contracts owned by run execution, independent of a database.

use async_trait::async_trait;

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
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait OperationRepository<E: Send + Sync + 'static>: Send + Sync {
    /// Marks unfinished operations as failed after process restart.
    async fn fail_unfinished_session_operations(&self, reason: &str) -> Result<(), E>;

    /// Returns whether cancellation is requested for a specific operation.
    async fn is_cancel_requested_for_operation(&self, operation_id: &str) -> Result<bool, E>;

    /// Returns whether an operation is still unfinished.
    async fn is_session_operation_unfinished(&self, operation_id: &str) -> Result<bool, E>;

    /// Loads operations still waiting in queue or currently running.
    async fn load_unfinished_session_operations(&self) -> Result<Vec<SessionOperationRow>, E>;

    /// Marks an operation as canceled.
    async fn mark_session_operation_canceled(
        &self,
        operation_id: &str,
        reason: &str,
    ) -> Result<(), E>;

    /// Marks an operation as completed successfully.
    async fn mark_session_operation_done(&self, operation_id: &str) -> Result<(), E>;

    /// Marks an operation as failed with an error message.
    async fn mark_session_operation_failed(&self, operation_id: &str, error: &str)
    -> Result<(), E>;

    /// Marks an operation as running and refreshes its heartbeat timestamp.
    async fn mark_session_operation_running(&self, operation_id: &str) -> Result<(), E>;

    /// Refreshes liveness only while the operation remains running.
    async fn heartbeat(&self, operation_id: &str) -> Result<(), E>;

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
    ) -> Result<bool, E>;

    /// Inserts a queued operation row for a session.
    async fn insert_session_operation(
        &self,
        operation_id: &str,
        session_id: &str,
        kind: &str,
    ) -> Result<(), E>;

    /// Requests cancellation for unfinished operations of a session.
    async fn request_cancel_for_session_operations(&self, session_id: &str) -> Result<(), E>;
}
