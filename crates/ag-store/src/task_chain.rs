//! Durable static workflow identities and operation-backed stage projection.

use ag_protocol::AgentResponse;
use ag_session::{
    AcceptTaskChainPlanRequest, AcceptTaskPlanRequest, AcceptedTaskPlan, AgentKind, AgentSelection,
    BuiltinAgentDefinition, ContinueTaskChainRequest, DurableMessageVisibility, SessionId,
    StartPlanThenImplementRequest, SubmitAgentTaskRequest, TaskAgentInput, TaskChain,
    TaskChainPhase,
};
use sqlx::SqliteConnection;

use crate::DbError;
use crate::task::SqliteTaskRepository;
use crate::task_agent::TaskAgentStore;
use crate::task_plan::TaskPlanStore;

pub(crate) struct TaskChainStore;

impl TaskChainStore {
    pub(crate) async fn start(
        connection: &mut SqliteConnection,
        session_id: &str,
        request: &StartPlanThenImplementRequest,
        operation_kind: &str,
        now: i64,
    ) -> Result<bool, DbError> {
        if request.chain_id.trim().is_empty() {
            return Err(Self::invalid("Workflow identity is empty"));
        }
        sqlx::query!("UPDATE session SET id = id WHERE id = ?", session_id)
            .execute(&mut *connection)
            .await?;
        let agent = request.agent.kind().to_string();
        let model = request.agent.model().as_str();
        let existing = sqlx::query!(
            "SELECT session_id, agent, model, user_request, planner_task_attempt_id FROM \
             task_chain WHERE id = ?",
            request.chain_id
        )
        .fetch_optional(&mut *connection)
        .await?;
        if let Some(existing) = existing {
            if existing.session_id != session_id
                || existing.agent != agent
                || existing.model != model
                || existing.user_request != request.user_request
                || existing.planner_task_attempt_id != request.planner_attempt_id
            {
                return Err(Self::invalid(
                    "Workflow identity was already used with different inputs",
                ));
            }

            return Ok(false);
        }
        let planner = request.planner_request();
        let implementer_task_id = request.implementer_task_id();
        let occupied = sqlx::query!(
            r#"SELECT EXISTS(SELECT 1 FROM task WHERE id IN (?, ?)) AS "occupied!: bool""#,
            planner.task_id,
            implementer_task_id
        )
        .fetch_one(&mut *connection)
        .await?;
        if occupied.occupied {
            return Err(Self::invalid("Workflow task identity already exists"));
        }
        SqliteTaskRepository::claim_agent_in_transaction(
            connection,
            session_id,
            &planner,
            operation_kind,
            now,
        )
        .await?;
        sqlx::query!(
            "INSERT INTO task (id, session_id, objective, created_at) VALUES (?, ?, ?, ?)",
            implementer_task_id,
            session_id,
            request.user_request,
            now
        )
        .execute(&mut *connection)
        .await?;
        sqlx::query!(
            "INSERT INTO task_chain (id, session_id, agent, model, user_request, planner_task_id, \
             implementer_task_id, planner_task_attempt_id, created_at) VALUES (?, ?, ?, ?, ?, ?, \
             ?, ?, ?)",
            request.chain_id,
            session_id,
            agent,
            model,
            request.user_request,
            planner.task_id,
            implementer_task_id,
            request.planner_attempt_id,
            now
        )
        .execute(&mut *connection)
        .await?;

        Ok(true)
    }

    pub(crate) async fn load(
        connection: &mut SqliteConnection,
        chain_id: &str,
    ) -> Result<Option<TaskChain>, DbError> {
        let row = sqlx::query!(r#"SELECT c.id AS "id!", c.session_id, c.agent, c.model, c.user_request, c.planner_task_id, c.implementer_task_id, c.accepted_task_attempt_id, c.canceled_at, p.plan FROM task_chain c LEFT JOIN task_plan_acceptance p ON p.task_attempt_id = c.accepted_task_attempt_id WHERE c.id = ?"#, chain_id).fetch_optional(&mut *connection).await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let kind = row
            .agent
            .parse::<AgentKind>()
            .map_err(|_| Self::invalid("Unknown captured workflow provider"))?;
        let model = kind
            .parse_model(&row.model)
            .ok_or_else(|| Self::invalid("Unknown captured workflow model"))?;
        let agent = AgentSelection::new(kind, model);
        let accepted_plan = match (row.accepted_task_attempt_id, row.plan) {
            (Some(attempt_id), Some(text)) => Some(AcceptedTaskPlan {
                attempt_id,
                task_id: row.planner_task_id.clone(),
                text,
            }),
            (None, None) => None,
            _ => return Err(Self::invalid("Workflow acceptance ledger is missing")),
        };
        let (task_id, definition) = if accepted_plan.is_some() {
            (
                &row.implementer_task_id,
                BuiltinAgentDefinition::Implementer,
            )
        } else {
            (&row.planner_task_id, BuiltinAgentDefinition::Planner)
        };
        let context = TaskAgentStore::load(connection, task_id)
            .await?
            .ok_or_else(|| Self::invalid("Workflow task context is missing"))?;
        if context.context.session_id.as_str() != row.session_id
            || context.context.assignment.agent != agent
            || context.context.assignment.definition != definition
            || context.input.user_request != row.user_request
            || context.input.accepted_plan != accepted_plan
        {
            return Err(Self::invalid(
                "Workflow task binding does not match its immutable inputs",
            ));
        }
        let attempt = sqlx::query!(r#"SELECT a.id AS "id!", a.message, a.is_visible AS "is_visible!: bool", t.objective, o.status AS "status!", o.cancel_requested AS "canceled!: bool", r.assistant_message FROM task_attempt a JOIN task t ON t.id = a.task_id JOIN session_operation o ON o.id = a.session_operation_id LEFT JOIN task_agent_turn r ON r.task_attempt_id = a.id WHERE a.task_id = ? ORDER BY a.rowid DESC LIMIT 1"#, task_id).fetch_optional(&mut *connection).await?.ok_or_else(|| Self::invalid("Workflow attempt is missing"))?;
        let input: TaskAgentInput = serde_json::from_str(&attempt.message)
            .map_err(|_| Self::invalid("Workflow attempt input is invalid"))?;
        if input.user_request != row.user_request
            || input.accepted_plan != accepted_plan
            || attempt.objective != row.user_request
            || attempt.is_visible
        {
            return Err(Self::invalid(
                "Workflow attempt does not match its immutable inputs",
            ));
        }
        let phase = Self::phase(
            definition,
            &attempt.status,
            row.canceled_at.is_some() || (attempt.canceled && attempt.status != "failed"),
            attempt.assistant_message.as_deref(),
        )?;

        Ok(Some(TaskChain {
            accepted_plan,
            agent,
            current_request: SubmitAgentTaskRequest {
                assignment: context.context.assignment,
                attempt_id: attempt.id,
                input,
                objective: attempt.objective,
                task_id: task_id.clone(),
                visibility: DurableMessageVisibility::Hidden,
            },
            id: row.id,
            implementer_task_id: row.implementer_task_id,
            phase,
            planner_task_id: row.planner_task_id,
            session_id: SessionId::from(row.session_id),
            user_request: row.user_request,
        }))
    }

    pub(crate) async fn list(
        connection: &mut SqliteConnection,
        session_id: &str,
    ) -> Result<Vec<TaskChain>, DbError> {
        let rows = sqlx::query!(
            "SELECT id FROM task_chain WHERE session_id = ? ORDER BY created_at, rowid",
            session_id
        )
        .fetch_all(&mut *connection)
        .await?;
        let mut chains = Vec::with_capacity(rows.len());
        for row in rows {
            chains.push(Self::required(connection, &row.id).await?);
        }

        Ok(chains)
    }

    pub(crate) async fn accept(
        connection: &mut SqliteConnection,
        chain_id: &str,
        request: &AcceptTaskChainPlanRequest,
        operation_kind: &str,
        now: i64,
    ) -> Result<bool, DbError> {
        Self::lock_chain(connection, chain_id).await?;
        let chain = Self::required(connection, chain_id).await?;
        if let Some(plan) = &chain.accepted_plan {
            if plan.attempt_id != request.plan_attempt_id {
                return Err(Self::invalid(
                    "Workflow already accepted a different plan version",
                ));
            }
            let continuation = sqlx::query!(
                r#"SELECT EXISTS(SELECT 1 FROM task_chain_continuation WHERE task_attempt_id = ?) AS "exists!: bool""#,
                request.attempt_id
            ).fetch_one(&mut *connection).await?;
            if continuation.exists {
                return Err(Self::invalid(
                    "Attempt identity belongs to a continuation, not plan acceptance",
                ));
            }
            if Self::matches_delivery(
                connection,
                &chain,
                &ContinueTaskChainRequest {
                    attempt_id: request.attempt_id.clone(),
                    clarification: None,
                    expected_attempt_id: request.plan_attempt_id.clone(),
                },
                Some(&chain.implementer_task_id),
            )
            .await?
            {
                return Ok(false);
            }

            return Err(Self::invalid(
                "Implementation already started; use an explicit continuation",
            ));
        }
        if chain.phase == TaskChainPhase::Canceled {
            return Err(Self::invalid(
                "Canceled workflow requires an explicit new Planner attempt",
            ));
        }
        let plan = TaskPlanStore::accept(
            connection,
            chain.session_id.as_str(),
            &AcceptTaskPlanRequest {
                attempt_id: request.plan_attempt_id.clone(),
                task_id: chain.planner_task_id.clone(),
            },
            now,
        )
        .await?;
        let mut implementation = chain.current_request;
        implementation.assignment.definition = BuiltinAgentDefinition::Implementer;
        implementation.task_id = chain.implementer_task_id;
        implementation.attempt_id = request.attempt_id.clone();
        implementation.input.accepted_plan = Some(plan);
        implementation.input.clarification = None;
        let claimed = SqliteTaskRepository::claim_agent_in_transaction(
            connection,
            chain.session_id.as_str(),
            &implementation,
            operation_kind,
            now,
        )
        .await?;
        sqlx::query!(
            "UPDATE task_chain SET accepted_task_attempt_id = ? WHERE id = ?",
            request.plan_attempt_id,
            chain_id
        )
        .execute(&mut *connection)
        .await?;

        Ok(claimed)
    }

    pub(crate) async fn resume(
        connection: &mut SqliteConnection,
        chain_id: &str,
        request: &ContinueTaskChainRequest,
        operation_kind: &str,
        now: i64,
    ) -> Result<bool, DbError> {
        Self::lock_chain(connection, chain_id).await?;
        let chain = Self::required(connection, chain_id).await?;
        if Self::matches_continuation_receipt(connection, &chain, request).await? {
            return Ok(false);
        }
        if chain.current_request.attempt_id != request.expected_attempt_id {
            return Err(Self::invalid(
                "Workflow advanced after the continuation source was observed",
            ));
        }
        let continuation = chain.continuation_request(request);
        let claimed = SqliteTaskRepository::claim_agent_in_transaction(
            connection,
            chain.session_id.as_str(),
            &continuation,
            operation_kind,
            now,
        )
        .await?;
        sqlx::query!(
            "INSERT INTO task_chain_continuation (task_attempt_id, expected_task_attempt_id) \
             VALUES (?, ?)",
            request.attempt_id,
            request.expected_attempt_id
        )
        .execute(&mut *connection)
        .await?;
        sqlx::query!(
            "UPDATE task_chain SET canceled_at = NULL WHERE id = ?",
            chain_id
        )
        .execute(&mut *connection)
        .await?;

        Ok(claimed)
    }

    pub(crate) async fn cancel(
        connection: &mut SqliteConnection,
        chain_id: &str,
        now: i64,
    ) -> Result<(), DbError> {
        Self::lock_chain(connection, chain_id).await?;
        let chain = Self::required(connection, chain_id).await?;
        sqlx::query!(
            "UPDATE task_chain SET canceled_at = COALESCE(canceled_at, ?) WHERE id = ?",
            now,
            chain_id
        )
        .execute(&mut *connection)
        .await?;
        sqlx::query!(
            "UPDATE session_operation SET cancel_requested = 1, status = CASE WHEN status = \
             'queued' THEN 'canceled' ELSE status END, finished_at = CASE WHEN status = 'queued' \
             THEN ? ELSE finished_at END WHERE id IN (SELECT session_operation_id FROM \
             task_attempt WHERE task_id IN (?, ?)) AND status IN ('queued', 'running')",
            now,
            chain.planner_task_id,
            chain.implementer_task_id
        )
        .execute(&mut *connection)
        .await?;

        Ok(())
    }

    /// Loads a reply's bound workflow without exposing another session's
    /// snapshot. New replies have no binding yet.
    pub(crate) async fn load_for_reply(
        connection: &mut SqliteConnection,
        session_id: &str,
        reply_id: &str,
    ) -> Result<Option<TaskChain>, DbError> {
        let reply = sqlx::query!(
            "SELECT task_chain_id, message FROM task_chain_reply WHERE id = ?",
            reply_id
        )
        .fetch_optional(&mut *connection)
        .await?;
        let Some(reply) = reply else {
            return Ok(None);
        };
        let owner = sqlx::query!(
            "SELECT session_id FROM task_chain WHERE id = ?",
            reply.task_chain_id
        )
        .fetch_one(&mut *connection)
        .await?;
        if owner.session_id != session_id {
            return Ok(None);
        }

        Self::load(connection, &reply.task_chain_id).await
    }

    /// Records one reply identity for an existing workflow. Returns the owning
    /// session when newly recorded, or `None` for identical redelivery.
    pub(crate) async fn record_reply(
        connection: &mut SqliteConnection,
        chain_id: &str,
        reply_id: &str,
        message: &str,
        now: i64,
    ) -> Result<Option<String>, DbError> {
        if reply_id.trim().is_empty() || message.trim().is_empty() {
            return Err(Self::invalid("Reply identity and message must be nonempty"));
        }
        let existing = sqlx::query!(
            "SELECT task_chain_id, message FROM task_chain_reply WHERE id = ?",
            reply_id
        )
        .fetch_optional(&mut *connection)
        .await?;
        if let Some(existing) = existing {
            if existing.task_chain_id != chain_id || existing.message != message {
                return Err(Self::invalid(
                    "Reply identity was already used with different inputs",
                ));
            }

            return Ok(None);
        }
        let chain = sqlx::query!("SELECT session_id FROM task_chain WHERE id = ?", chain_id)
            .fetch_optional(&mut *connection)
            .await?
            .ok_or_else(|| Self::invalid("Workflow does not exist"))?;
        sqlx::query!(
            "INSERT INTO task_chain_reply (id, task_chain_id, message, created_at) VALUES (?, ?, \
             ?, ?)",
            reply_id,
            chain_id,
            message,
            now
        )
        .execute(&mut *connection)
        .await?;

        Ok(Some(chain.session_id))
    }

    pub(crate) async fn require_unowned(
        connection: &mut SqliteConnection,
        task_id: &str,
    ) -> Result<(), DbError> {
        if Self::owns_task(connection, task_id).await? {
            return Err(Self::invalid("Workflow tasks require workflow admission"));
        }

        Ok(())
    }

    /// Checks durable role ownership without loading workflow projections.
    pub(crate) async fn owns_task(
        connection: &mut SqliteConnection,
        task_id: &str,
    ) -> Result<bool, DbError> {
        let row = sqlx::query!(r#"SELECT EXISTS(SELECT 1 FROM task_chain WHERE planner_task_id = ? OR implementer_task_id = ?) AS "owned!: bool""#, task_id, task_id).fetch_one(connection).await?;

        Ok(row.owned)
    }

    pub(crate) fn unsupported() -> DbError {
        Self::invalid("Host does not support task chains")
    }

    async fn lock_chain(connection: &mut SqliteConnection, chain_id: &str) -> Result<(), DbError> {
        sqlx::query!("UPDATE task_chain SET id = id WHERE id = ?", chain_id)
            .execute(connection)
            .await?;

        Ok(())
    }

    async fn required(
        connection: &mut SqliteConnection,
        chain_id: &str,
    ) -> Result<TaskChain, DbError> {
        Self::load(connection, chain_id)
            .await?
            .ok_or_else(|| Self::invalid("Workflow does not exist"))
    }

    async fn matches_delivery(
        connection: &mut SqliteConnection,
        chain: &TaskChain,
        request: &ContinueTaskChainRequest,
        expected_task: Option<&str>,
    ) -> Result<bool, DbError> {
        let row = sqlx::query!(r#"SELECT a.task_id, a.message, a.is_visible AS "is_visible!: bool", t.session_id FROM task_attempt a JOIN task t ON t.id = a.task_id WHERE a.id = ?"#, request.attempt_id).fetch_optional(connection).await?;
        let Some(row) = row else {
            return Ok(false);
        };
        let input: TaskAgentInput = serde_json::from_str(&row.message)
            .map_err(|_| Self::invalid("Workflow delivery input is invalid"))?;
        let expected_plan = if row.task_id == chain.planner_task_id {
            None
        } else {
            chain.accepted_plan.clone()
        };
        if (row.task_id != chain.planner_task_id && row.task_id != chain.implementer_task_id)
            || expected_task.is_some_and(|id| id != row.task_id)
            || row.session_id != chain.session_id.as_str()
            || input.user_request != chain.user_request
            || input.accepted_plan != expected_plan
            || input.clarification != request.clarification
            || row.is_visible
        {
            return Err(Self::invalid(
                "Attempt identity was already used with different workflow inputs",
            ));
        }

        Ok(true)
    }

    async fn matches_continuation_receipt(
        connection: &mut SqliteConnection,
        chain: &TaskChain,
        request: &ContinueTaskChainRequest,
    ) -> Result<bool, DbError> {
        let previous = sqlx::query!(
            "SELECT r.expected_task_attempt_id FROM task_attempt a LEFT JOIN \
             task_chain_continuation r ON r.task_attempt_id = a.id WHERE a.id = ?",
            request.attempt_id
        )
        .fetch_optional(&mut *connection)
        .await?;
        let Some(previous) = previous else {
            return Ok(false);
        };
        let expected = previous.expected_task_attempt_id.ok_or_else(|| {
            Self::invalid("Attempt identity belongs to a different workflow action")
        })?;
        if expected != request.expected_attempt_id {
            return Err(Self::invalid(
                "Continuation identity was already used with a different source attempt",
            ));
        }

        Self::matches_delivery(connection, chain, request, None).await
    }

    fn phase(
        definition: BuiltinAgentDefinition,
        status: &str,
        canceled: bool,
        response: Option<&str>,
    ) -> Result<TaskChainPhase, DbError> {
        if canceled {
            return Ok(TaskChainPhase::Canceled);
        }
        let planner = definition == BuiltinAgentDefinition::Planner;
        match status {
            "queued" | "running" => Ok(if planner {
                TaskChainPhase::Planning
            } else {
                TaskChainPhase::Implementing
            }),
            "failed" => Ok(TaskChainPhase::Failed),
            "canceled" => Ok(TaskChainPhase::Canceled),
            "done" => {
                let response: AgentResponse = serde_json::from_str(
                    response
                        .ok_or_else(|| Self::invalid("Successful workflow response is missing"))?,
                )
                .map_err(|_| Self::invalid("Successful workflow response is invalid"))?;
                if !response.questions.is_empty() {
                    return Ok(if planner {
                        TaskChainPhase::PlannerQuestion
                    } else {
                        TaskChainPhase::ImplementerQuestion
                    });
                }
                if response.answer.trim().is_empty() || !response.review_comment_outcomes.is_empty()
                {
                    return Ok(TaskChainPhase::Failed);
                }

                Ok(if planner {
                    TaskChainPhase::AwaitingPlanAcceptance
                } else {
                    TaskChainPhase::Succeeded
                })
            }
            _ => Err(Self::invalid("Unknown workflow operation status")),
        }
    }

    fn invalid(reason: &str) -> DbError {
        DbError::InvalidData {
            entity: "task chain",
            reason: reason.to_string(),
        }
    }
}

#[cfg(test)]
#[path = "task_chain_test.rs"]
mod tests;
