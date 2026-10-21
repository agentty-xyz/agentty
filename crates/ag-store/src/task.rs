//! Independent task planning, atomic attempt admission, and attempt history.

use std::sync::Arc;

use ag_session::{
    AcceptTaskChainPlanRequest, AcceptTaskPlanRequest, AcceptedTaskPlan, ContinueTaskChainRequest,
    CreateTaskRequest, DurableMessageVisibility, SessionId, SessionMessageKind,
    StartPlanThenImplementRequest, SubmitAgentTaskRequest, SubmitTaskRequest, Task, TaskAttempt,
    TaskAttemptStatus, TaskChain, stored_message_content,
};
use async_trait::async_trait;
use sqlx::{SqliteConnection, SqlitePool};

use crate::session_message::SessionMessageStore;
use crate::task_agent::{TaskAgentContextRow, TaskAgentStore};
use crate::task_agent_turn::{TaskAgentHistoryRow, TaskAgentTurnMetadata, TaskAgentTurnStore};
use crate::task_chain::TaskChainStore;
use crate::task_plan::TaskPlanStore;
use crate::{DbError, TimestampSource};

/// Persistence boundary for desired outcomes and immutable attempt identities.
///
/// Existing adapters may omit role-aware tasks; their defaults reject those
/// requests explicitly without admitting work or changing ordinary tasks.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait TaskRepository: Send + Sync {
    /// Atomically reserves both role tasks and admits the first Planner.
    /// The launch prompt and graph marker commit with the workflow receipt.
    /// Returns false for identical launch redelivery, without replaying work.
    async fn start_task_chain(
        &self,
        session_id: &str,
        request: &StartPlanThenImplementRequest,
        operation_kind: &str,
    ) -> Result<bool, DbError> {
        let _ = (session_id, request, operation_kind);
        Err(TaskChainStore::unsupported())
    }

    /// Reads a consistent stage projection; never dispatches pending work.
    async fn load_task_chain(&self, chain_id: &str) -> Result<Option<TaskChain>, DbError> {
        let _ = chain_id;
        Err(TaskChainStore::unsupported())
    }

    /// Lists session workflow snapshots in creation order.
    async fn list_task_chains(&self, session_id: &str) -> Result<Vec<TaskChain>, DbError> {
        let _ = session_id;
        Err(TaskChainStore::unsupported())
    }

    /// Reads the workflow already bound to a reply in its owning session.
    /// Returns `None` for a new reply or a reply owned by another session.
    async fn load_task_chain_for_reply(
        &self,
        session_id: &str,
        reply_id: &str,
    ) -> Result<Option<TaskChain>, DbError> {
        let _ = (session_id, reply_id);
        Err(TaskChainStore::unsupported())
    }

    /// Atomically accepts a Planner version and admits its Implementer.
    /// Returns false for identical handoff redelivery.
    async fn accept_task_chain_plan(
        &self,
        chain_id: &str,
        request: &AcceptTaskChainPlanRequest,
        operation_kind: &str,
    ) -> Result<bool, DbError> {
        let _ = (chain_id, request, operation_kind);
        Err(TaskChainStore::unsupported())
    }

    /// Admits an explicit new attempt for the current role. Identical old
    /// delivery never clears cancellation or restarts work.
    async fn continue_task_chain(
        &self,
        chain_id: &str,
        request: &ContinueTaskChainRequest,
        operation_kind: &str,
    ) -> Result<bool, DbError> {
        let _ = (chain_id, request, operation_kind);
        Err(TaskChainStore::unsupported())
    }

    /// Cancels only operations belonging to this workflow; queued attempts
    /// settle immediately and active workers observe their cancellation flag.
    async fn cancel_task_chain(&self, chain_id: &str) -> Result<(), DbError> {
        let _ = chain_id;
        Err(TaskChainStore::unsupported())
    }

    /// Atomically records a natural-language reply and appends it as a
    /// visible user prompt in the owning session transcript. Returns false
    /// for identical redelivery; reusing a reply identity with different
    /// inputs is rejected. Recording never admits work.
    async fn record_task_chain_reply(
        &self,
        chain_id: &str,
        reply_id: &str,
        message: &str,
    ) -> Result<bool, DbError> {
        let _ = (chain_id, reply_id, message);
        Err(TaskChainStore::unsupported())
    }

    /// Accepts the latest successful Planner response by immutable version.
    /// Reads the exact plan from storage; task completion is independent.
    async fn accept_agent_plan(
        &self,
        session_id: &str,
        request: &AcceptTaskPlanRequest,
    ) -> Result<AcceptedTaskPlan, DbError> {
        let _ = (session_id, request);
        Err(TaskAgentStore::invalid(
            "Host does not support plan acceptance",
        ))
    }

    /// Verifies trusted acceptance, exact body, ownership, original request,
    /// and captured selection before Implementer dispatch. Planner is a no-op.
    async fn verify_agent_plan(
        &self,
        session_id: &str,
        request: &SubmitAgentTaskRequest,
    ) -> Result<(), DbError> {
        let _ = (session_id, request);
        Err(TaskAgentStore::invalid(
            "Host does not support plan acceptance",
        ))
    }

    /// Loads one consistent snapshot of task-only history and provider state.
    async fn load_agent_history(
        &self,
        task_id: &str,
    ) -> Result<Option<TaskAgentHistoryRow>, DbError> {
        let _ = task_id;
        Err(TaskAgentStore::invalid(
            "Host does not support role-aware tasks",
        ))
    }

    /// Commits a running, uncanceled attempt's observed response, continuation,
    /// and usage atomically. Identical repeats do not duplicate usage or rewind
    /// continuation. This does not settle the operation or accept the task.
    /// Chain-owned role answers join the session transcript in that same
    /// transaction; standalone role responses remain task-only.
    /// Returns `true` only when this call recorded the response, so callers
    /// can run once-only follow-up effects; identical repeats return `false`.
    async fn persist_agent_turn(
        &self,
        attempt_id: &str,
        metadata: &TaskAgentTurnMetadata,
    ) -> Result<bool, DbError> {
        let _ = (attempt_id, metadata);
        Err(TaskAgentStore::invalid(
            "Host does not support role-aware tasks",
        ))
    }

    /// Atomically assigns an isolated context and admits its first or next
    /// attempt. Legacy attempts cannot acquire an assignment. Repeated
    /// deliveries retain the original assignment, context identity, and
    /// captured model selection.
    async fn claim_agent_attempt(
        &self,
        session_id: &str,
        request: &SubmitAgentTaskRequest,
        operation_kind: &str,
    ) -> Result<bool, DbError> {
        let _ = (session_id, request, operation_kind);
        Err(TaskAgentStore::invalid(
            "Host does not support role-aware tasks",
        ))
    }

    /// Checks a complete role-aware delivery before live session eligibility.
    async fn contains_agent_attempt(
        &self,
        session_id: &str,
        request: &SubmitAgentTaskRequest,
    ) -> Result<bool, DbError> {
        let _ = (session_id, request);
        Err(TaskAgentStore::invalid(
            "Host does not support role-aware tasks",
        ))
    }

    /// Loads task-owned identity and immutable inputs without session replay.
    async fn load_agent_context(
        &self,
        task_id: &str,
    ) -> Result<Option<TaskAgentContextRow>, DbError> {
        let _ = task_id;
        Err(TaskAgentStore::invalid(
            "Host does not support role-aware tasks",
        ))
    }

    /// Creates an outcome with zero attempts in any persisted session.
    /// Identical requests are idempotent; session and objective are immutable.
    async fn create_task(
        &self,
        session_id: &str,
        request: &CreateTaskRequest,
    ) -> Result<(), DbError>;

    /// Loads all tasks and histories for one session in one snapshot, ordered
    /// by creation timestamp and insertion order. Missing sessions return
    /// empty.
    async fn list_tasks(&self, session_id: &str) -> Result<Vec<Task>, DbError>;

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
        agent_request: Option<&SubmitAgentTaskRequest>,
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
        let same_message = if let Some(agent_request) = agent_request {
            let input: ag_session::TaskAgentInput = serde_json::from_str(&row.message)
                .map_err(|_| TaskAgentStore::invalid("Invalid task attempt input"))?;
            input == agent_request.input
        } else {
            row.message == request.message
        };
        if row.task_id != request.task_id
            || row.session_id != session_id
            || row.objective != request.objective
            || !same_message
            || row.is_visible != (request.visibility == DurableMessageVisibility::Visible)
        {
            return Err(Self::invalid(
                "Attempt id was already used with different inputs",
            ));
        }

        Ok(true)
    }

    /// Reads attempt state using the caller's task snapshot.
    async fn load_attempts(
        connection: &mut SqliteConnection,
        task_id: &str,
    ) -> Result<Vec<TaskAttempt>, DbError> {
        let attempts = sqlx::query!(
            r#"SELECT a.id, a.message, a.is_visible AS "is_visible: bool",
                      o.id AS "operation_id!", o.status AS "status!", o.queued_at,
                      o.started_at, o.finished_at, o.last_error
               FROM task_attempt a JOIN session_operation o ON o.id = a.session_operation_id
               WHERE a.task_id = ? ORDER BY a.rowid"#,
            task_id
        )
        .fetch_all(connection)
        .await?;

        attempts
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
            .collect()
    }

    /// Takes the writer lock before checking identity, binding, and readiness.
    async fn claim_attempt_inner(
        &self,
        session_id: &str,
        request: &SubmitTaskRequest,
        operation_kind: &str,
        agent_request: Option<&SubmitAgentTaskRequest>,
    ) -> Result<bool, DbError> {
        let now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = self.pool.begin().await?;
        TaskChainStore::require_unowned(&mut transaction, &request.task_id).await?;
        let claimed = Self::claim_attempt_in_transaction(
            &mut transaction,
            session_id,
            request,
            operation_kind,
            agent_request,
            now,
        )
        .await?;
        transaction.commit().await?;

        Ok(claimed)
    }

    pub(crate) async fn claim_agent_in_transaction(
        connection: &mut SqliteConnection,
        session_id: &str,
        request: &SubmitAgentTaskRequest,
        operation_kind: &str,
        now: i64,
    ) -> Result<bool, DbError> {
        let attempt = TaskAgentStore::attempt(request)?;

        Self::claim_attempt_in_transaction(
            connection,
            session_id,
            &attempt,
            operation_kind,
            Some(request),
            now,
        )
        .await
    }

    async fn claim_attempt_in_transaction(
        connection: &mut SqliteConnection,
        session_id: &str,
        request: &SubmitTaskRequest,
        operation_kind: &str,
        agent_request: Option<&SubmitAgentTaskRequest>,
        now: i64,
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
        sqlx::query!(
            "INSERT INTO task (id, session_id, objective, created_at) VALUES (?, ?, ?, ?) ON \
             CONFLICT(id) DO NOTHING",
            request.task_id,
            session_id,
            request.objective,
            now
        )
        .execute(&mut *connection)
        .await?;
        if let Some(agent_request) = agent_request {
            TaskAgentStore::bind(connection, session_id, agent_request).await?;
        } else {
            TaskAgentStore::require_ordinary(connection, &request.task_id).await?;
        }
        if Self::matching_attempt(connection, session_id, request, agent_request).await? {
            return Ok(false);
        }
        if let Some(agent_request) = agent_request {
            TaskPlanStore::verify(connection, session_id, agent_request).await?;
        }
        let task = sqlx::query!(
            "SELECT session_id, objective, completed_at FROM task WHERE id = ?",
            request.task_id
        )
        .fetch_one(&mut *connection)
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
        .fetch_one(&mut *connection)
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
        .execute(&mut *connection)
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
        .execute(&mut *connection)
        .await?;

        Ok(true)
    }
}

#[async_trait]
impl TaskRepository for SqliteTaskRepository {
    async fn start_task_chain(
        &self,
        session_id: &str,
        request: &StartPlanThenImplementRequest,
        operation_kind: &str,
    ) -> Result<bool, DbError> {
        let now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = self.pool.begin().await?;
        let claimed =
            TaskChainStore::start(&mut transaction, session_id, request, operation_kind, now)
                .await?;
        if claimed {
            for (kind, raw_content) in [
                (
                    SessionMessageKind::UserPrompt,
                    request.user_request.as_str(),
                ),
                (SessionMessageKind::AgentGraph, request.chain_id.as_str()),
            ] {
                let content = stored_message_content(kind, raw_content);
                SessionMessageStore::append_normalized_in_transaction(
                    &mut transaction,
                    session_id,
                    kind,
                    &content,
                    now,
                )
                .await?;
            }
        }
        transaction.commit().await?;

        Ok(claimed)
    }

    async fn load_task_chain(&self, chain_id: &str) -> Result<Option<TaskChain>, DbError> {
        let mut transaction = self.pool.begin().await?;
        let chain = TaskChainStore::load(&mut transaction, chain_id).await?;
        transaction.commit().await?;

        Ok(chain)
    }

    async fn list_task_chains(&self, session_id: &str) -> Result<Vec<TaskChain>, DbError> {
        let mut transaction = self.pool.begin().await?;
        let chains = TaskChainStore::list(&mut transaction, session_id).await?;
        transaction.commit().await?;

        Ok(chains)
    }

    async fn load_task_chain_for_reply(
        &self,
        session_id: &str,
        reply_id: &str,
    ) -> Result<Option<TaskChain>, DbError> {
        let mut transaction = self.pool.begin().await?;
        let chain = TaskChainStore::load_for_reply(&mut transaction, session_id, reply_id).await?;
        transaction.commit().await?;

        Ok(chain)
    }

    async fn accept_task_chain_plan(
        &self,
        chain_id: &str,
        request: &AcceptTaskChainPlanRequest,
        operation_kind: &str,
    ) -> Result<bool, DbError> {
        let now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = self.pool.begin().await?;
        let claimed =
            TaskChainStore::accept(&mut transaction, chain_id, request, operation_kind, now)
                .await?;
        transaction.commit().await?;

        Ok(claimed)
    }

    async fn continue_task_chain(
        &self,
        chain_id: &str,
        request: &ContinueTaskChainRequest,
        operation_kind: &str,
    ) -> Result<bool, DbError> {
        let now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = self.pool.begin().await?;
        let claimed =
            TaskChainStore::resume(&mut transaction, chain_id, request, operation_kind, now)
                .await?;
        transaction.commit().await?;

        Ok(claimed)
    }

    async fn cancel_task_chain(&self, chain_id: &str) -> Result<(), DbError> {
        let now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = self.pool.begin().await?;
        TaskChainStore::cancel(&mut transaction, chain_id, now).await?;
        transaction.commit().await?;

        Ok(())
    }

    async fn record_task_chain_reply(
        &self,
        chain_id: &str,
        reply_id: &str,
        message: &str,
    ) -> Result<bool, DbError> {
        let now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = self.pool.begin().await?;
        let session_id =
            TaskChainStore::record_reply(&mut transaction, chain_id, reply_id, message, now)
                .await?;
        if let Some(session_id) = &session_id {
            let content = stored_message_content(SessionMessageKind::UserPrompt, message);
            SessionMessageStore::append_normalized_in_transaction(
                &mut transaction,
                session_id,
                SessionMessageKind::UserPrompt,
                &content,
                now,
            )
            .await?;
        }
        transaction.commit().await?;

        Ok(session_id.is_some())
    }

    async fn accept_agent_plan(
        &self,
        session_id: &str,
        request: &AcceptTaskPlanRequest,
    ) -> Result<AcceptedTaskPlan, DbError> {
        let now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = self.pool.begin().await?;
        let plan = TaskPlanStore::accept(&mut transaction, session_id, request, now).await?;
        transaction.commit().await?;

        Ok(plan)
    }

    async fn verify_agent_plan(
        &self,
        session_id: &str,
        request: &SubmitAgentTaskRequest,
    ) -> Result<(), DbError> {
        let mut transaction = self.pool.begin().await?;
        TaskPlanStore::verify(&mut transaction, session_id, request).await?;
        transaction.commit().await?;

        Ok(())
    }

    async fn load_agent_history(
        &self,
        task_id: &str,
    ) -> Result<Option<TaskAgentHistoryRow>, DbError> {
        let mut transaction = self.pool.begin().await?;
        let history = TaskAgentTurnStore::load(&mut transaction, task_id).await?;
        transaction.commit().await?;

        Ok(history)
    }

    async fn persist_agent_turn(
        &self,
        attempt_id: &str,
        metadata: &TaskAgentTurnMetadata,
    ) -> Result<bool, DbError> {
        let now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = self.pool.begin().await?;
        let recorded =
            TaskAgentTurnStore::save(&mut transaction, attempt_id, metadata, now).await?;
        transaction.commit().await?;

        Ok(recorded)
    }

    async fn claim_agent_attempt(
        &self,
        session_id: &str,
        request: &SubmitAgentTaskRequest,
        operation_kind: &str,
    ) -> Result<bool, DbError> {
        let attempt = TaskAgentStore::attempt(request)?;
        self.claim_attempt_inner(session_id, &attempt, operation_kind, Some(request))
            .await
    }

    async fn contains_agent_attempt(
        &self,
        session_id: &str,
        request: &SubmitAgentTaskRequest,
    ) -> Result<bool, DbError> {
        let attempt = TaskAgentStore::attempt(request)?;
        let mut transaction = self.pool.begin().await?;
        let context = TaskAgentStore::load(&mut transaction, &request.task_id).await?;
        if let Some(context) = &context {
            TaskAgentStore::matches(context, session_id, request)?;
        }
        let found =
            Self::matching_attempt(&mut transaction, session_id, &attempt, Some(request)).await?;
        if found && context.is_none() {
            return Err(TaskAgentStore::invalid(
                "Task already uses the ordinary session conversation",
            ));
        }

        Ok(found)
    }

    async fn load_agent_context(
        &self,
        task_id: &str,
    ) -> Result<Option<TaskAgentContextRow>, DbError> {
        TaskAgentStore::load(&mut *self.pool.acquire().await?, task_id).await
    }

    async fn create_task(
        &self,
        session_id: &str,
        request: &CreateTaskRequest,
    ) -> Result<(), DbError> {
        if request.task_id.trim().is_empty() || request.objective.trim().is_empty() {
            return Err(Self::invalid("Task and objective must be nonempty"));
        }
        let now = self.timestamp_source.now_timestamp_seconds();
        let mut transaction = self.pool.begin().await?;
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
        let task = sqlx::query!(
            "SELECT session_id, objective, completed_at FROM task WHERE id = ?",
            request.task_id
        )
        .fetch_one(&mut *transaction)
        .await?;
        if task.session_id != session_id || task.objective != request.objective {
            return Err(Self::invalid("Task session and objective are immutable"));
        }
        transaction.commit().await?;

        Ok(())
    }

    async fn list_tasks(&self, session_id: &str) -> Result<Vec<Task>, DbError> {
        let mut transaction = self.pool.begin().await?;
        let rows = sqlx::query!(
            "SELECT id, session_id, objective, created_at, completed_at FROM task WHERE \
             session_id = ? ORDER BY created_at, rowid",
            session_id
        )
        .fetch_all(&mut *transaction)
        .await?;
        let mut tasks = Vec::with_capacity(rows.len());
        for row in rows {
            tasks.push(Task {
                attempts: Self::load_attempts(&mut transaction, &row.id).await?,
                completed_at: row.completed_at,
                created_at: row.created_at,
                id: row.id,
                objective: row.objective,
                session_id: SessionId::from(row.session_id),
            });
        }
        transaction.commit().await?;

        Ok(tasks)
    }

    async fn claim_attempt(
        &self,
        session_id: &str,
        request: &SubmitTaskRequest,
        operation_kind: &str,
    ) -> Result<bool, DbError> {
        self.claim_attempt_inner(session_id, request, operation_kind, None)
            .await
    }

    async fn contains_attempt(
        &self,
        session_id: &str,
        request: &SubmitTaskRequest,
    ) -> Result<bool, DbError> {
        let mut transaction = self.pool.begin().await?;
        TaskAgentStore::require_ordinary(&mut transaction, &request.task_id).await?;

        Self::matching_attempt(&mut transaction, session_id, request, None).await
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
        let attempts = Self::load_attempts(&mut transaction, task_id).await?;
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
