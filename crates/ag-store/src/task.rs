//! Atomic task admission and an operation-backed attempt read model.

use std::sync::Arc;

use ag_session::{
    DurableMessageVisibility, SessionId, SubmitTaskRequest, Task, TaskAttempt, TaskAttemptStatus,
};
use async_trait::async_trait;
use sqlx::{SqliteConnection, SqlitePool};

use crate::{DbError, TimestampSource};

/// Persistence boundary for desired outcomes and immutable attempt identities.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait TaskRepository: Send + Sync {
    /// Atomically creates the task, attempt, and queued operation. Returns
    /// `false` for an identical delivery, including a terminal attempt.
    /// Rejects conflicting payloads, completed tasks, and concurrent attempts.
    async fn claim_attempt(
        &self,
        session_id: &str,
        request: &SubmitTaskRequest,
        operation_kind: &str,
    ) -> Result<bool, DbError>;

    /// Checks an existing delivery before the host's live eligibility checks.
    /// Rejects reuse of an identity with different inputs.
    async fn contains_attempt(
        &self,
        session_id: &str,
        request: &SubmitTaskRequest,
    ) -> Result<bool, DbError>;

    /// Loads an outcome and its operation-backed history in one snapshot.
    async fn load_task(&self, task_id: &str) -> Result<Option<Task>, DbError>;

    /// Explicitly accepts the desired outcome when no attempt is unfinished.
    /// Repeated completion is idempotent and retains the original timestamp.
    async fn complete_task(&self, task_id: &str) -> Result<(), DbError>;
}

/// SQLite task adapter; execution state remains owned by session operations.
pub(crate) struct SqliteTaskRepository {
    pool: SqlitePool,
    timestamp_source: Arc<dyn TimestampSource>,
}

impl SqliteTaskRepository {
    pub(crate) fn new(pool: SqlitePool, timestamp_source: Arc<dyn TimestampSource>) -> Self {
        Self {
            pool,
            timestamp_source,
        }
    }

    fn invalid(reason: &str) -> DbError {
        DbError::InvalidData {
            entity: "task",
            reason: reason.to_string(),
        }
    }

    async fn matching_attempt(
        connection: &mut SqliteConnection,
        session_id: &str,
        request: &SubmitTaskRequest,
    ) -> Result<bool, DbError> {
        let row = sqlx::query!(
            r#"SELECT t.id AS "task_id!", t.session_id, t.objective,
                      a.message, a.is_visible AS "is_visible: bool"
               FROM task_attempt a JOIN task t ON t.id = a.task_id WHERE a.id = ?"#,
            request.attempt_id
        )
        .fetch_optional(connection)
        .await?;
        let Some(row) = row else {
            return Ok(false);
        };
        if row.task_id != request.task_id
            || row.session_id != session_id
            || row.objective != request.objective
            || row.message != request.message
            || row.is_visible != (request.visibility == DurableMessageVisibility::Visible)
        {
            return Err(Self::invalid(
                "Attempt id was already used with different inputs",
            ));
        }

        Ok(true)
    }
}

#[async_trait]
impl TaskRepository for SqliteTaskRepository {
    async fn claim_attempt(
        &self,
        session_id: &str,
        request: &SubmitTaskRequest,
        operation_kind: &str,
    ) -> Result<bool, DbError> {
        if [
            &request.task_id,
            &request.attempt_id,
            &request.objective,
            &request.message,
        ]
        .iter()
        .any(|value| value.trim().is_empty())
        {
            return Err(Self::invalid(
                "Task, attempt, objective, and message must be nonempty",
            ));
        }
        let now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = self.pool.begin().await?;
        // Take the writer lock before checking identity and readiness.
        sqlx::query!(
            "INSERT INTO task (id, session_id, objective, created_at) VALUES (?, ?, ?, ?) ON \
             CONFLICT(id) DO NOTHING",
            request.task_id,
            session_id,
            request.objective,
            now
        )
        .execute(&mut *transaction)
        .await?;
        if Self::matching_attempt(&mut transaction, session_id, request).await? {
            return Ok(false);
        }
        let task = sqlx::query!(
            "SELECT session_id, objective, completed_at FROM task WHERE id = ?",
            request.task_id
        )
        .fetch_one(&mut *transaction)
        .await?;
        if task.session_id != session_id || task.objective != request.objective {
            return Err(Self::invalid("Task session and objective are immutable"));
        }
        if task.completed_at.is_some() {
            return Err(Self::invalid("Task is already complete"));
        }
        let active = sqlx::query!(
            r#"SELECT EXISTS(SELECT 1 FROM task_attempt a
               JOIN session_operation o ON o.id = a.session_operation_id
               WHERE a.task_id = ? AND o.status IN ('queued', 'running')) AS "active!: bool""#,
            request.task_id
        )
        .fetch_one(&mut *transaction)
        .await?;
        if active.active {
            return Err(Self::invalid("Task already has an unfinished attempt"));
        }
        let operation_id = request.operation_id();
        sqlx::query!(
            "INSERT INTO session_operation (id, session_id, kind, status, queued_at) VALUES (?, \
             ?, ?, 'queued', ?)",
            operation_id,
            session_id,
            operation_kind,
            now
        )
        .execute(&mut *transaction)
        .await?;
        let is_visible = request.visibility == DurableMessageVisibility::Visible;
        sqlx::query!(
            "INSERT INTO task_attempt (id, task_id, session_operation_id, message, is_visible) \
             VALUES (?, ?, ?, ?, ?)",
            request.attempt_id,
            request.task_id,
            operation_id,
            request.message,
            is_visible
        )
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;

        Ok(true)
    }

    async fn contains_attempt(
        &self,
        session_id: &str,
        request: &SubmitTaskRequest,
    ) -> Result<bool, DbError> {
        Self::matching_attempt(&mut *self.pool.acquire().await?, session_id, request).await
    }

    async fn load_task(&self, task_id: &str) -> Result<Option<Task>, DbError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query!(
            "SELECT id, session_id, objective, created_at, completed_at FROM task WHERE id = ?",
            task_id
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let attempts = sqlx::query!(
            r#"SELECT a.id, a.message, a.is_visible AS "is_visible: bool",
                      o.id AS "operation_id!", o.status AS "status!", o.queued_at,
                      o.started_at, o.finished_at, o.last_error
               FROM task_attempt a JOIN session_operation o ON o.id = a.session_operation_id
               WHERE a.task_id = ? ORDER BY a.rowid"#,
            task_id
        )
        .fetch_all(&mut *transaction)
        .await?;
        let attempts = attempts
            .into_iter()
            .map(|attempt| {
                let status = match attempt.status.as_str() {
                    "queued" => TaskAttemptStatus::Queued,
                    "running" => TaskAttemptStatus::Running,
                    "done" => TaskAttemptStatus::Succeeded,
                    "failed" => TaskAttemptStatus::Failed,
                    "canceled" => TaskAttemptStatus::Canceled,
                    _ => {
                        return Err(DbError::InvalidStatus {
                            entity: "task attempt",
                            value: attempt.status,
                        });
                    }
                };

                Ok(TaskAttempt {
                    finished_at: attempt.finished_at,
                    id: attempt.id,
                    last_error: attempt.last_error,
                    message: attempt.message,
                    operation_id: attempt.operation_id,
                    queued_at: attempt.queued_at,
                    started_at: attempt.started_at,
                    status,
                    visibility: if attempt.is_visible {
                        DurableMessageVisibility::Visible
                    } else {
                        DurableMessageVisibility::Hidden
                    },
                })
            })
            .collect::<Result<Vec<_>, DbError>>()?;
        transaction.commit().await?;

        Ok(Some(Task {
            attempts,
            completed_at: row.completed_at,
            created_at: row.created_at,
            id: row.id,
            objective: row.objective,
            session_id: SessionId::from(row.session_id),
        }))
    }

    async fn complete_task(&self, task_id: &str) -> Result<(), DbError> {
        let now = self.timestamp_source.now_timestamp_seconds();
        let updated = sqlx::query!(
            "UPDATE task SET completed_at = COALESCE(completed_at, ?) WHERE id = ?
             AND NOT EXISTS (SELECT 1 FROM task_attempt a
                 JOIN session_operation o ON o.id = a.session_operation_id
                 WHERE a.task_id = task.id AND o.status IN ('queued', 'running'))",
            now,
            task_id
        )
        .execute(&self.pool)
        .await?;
        if updated.rows_affected() == 0 {
            return Err(Self::invalid(
                "Task does not exist or has an unfinished attempt",
            ));
        }

        Ok(())
    }
}
