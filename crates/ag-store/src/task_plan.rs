//! Trusted acceptance ledger and exact transactional implementation handoff.

use ag_protocol::AgentResponse;
use ag_session::{AcceptTaskPlanRequest, AcceptedTaskPlan, SubmitAgentTaskRequest, TaskAgentRole};
use sqlx::SqliteConnection;

use crate::DbError;
use crate::task_agent::{TaskAgentContextRow, TaskAgentStore};

pub(crate) struct TaskPlanStore;

impl TaskPlanStore {
    pub(crate) async fn accept(
        connection: &mut SqliteConnection,
        session_id: &str,
        request: &AcceptTaskPlanRequest,
        now: i64,
    ) -> Result<AcceptedTaskPlan, DbError> {
        // Take the writer lock before reading the latest version. A concurrent
        // revision cannot slip between eligibility and acceptance.
        sqlx::query!(
            "UPDATE task SET objective = objective WHERE id = ?",
            request.task_id
        )
        .execute(&mut *connection)
        .await?;
        let (_, plan) = Self::eligible(connection, session_id, request).await?;
        sqlx::query!(
            "INSERT INTO task_plan_acceptance (task_attempt_id, plan, accepted_at) VALUES (?, ?, \
             ?) ON CONFLICT(task_attempt_id) DO NOTHING",
            request.attempt_id,
            plan.text,
            now
        )
        .execute(&mut *connection)
        .await?;
        let accepted = sqlx::query!(
            "SELECT plan FROM task_plan_acceptance WHERE task_attempt_id = ?",
            request.attempt_id
        )
        .fetch_one(&mut *connection)
        .await?;
        if accepted.plan != plan.text {
            return Err(Self::invalid(
                "Accepted plan differs from its immutable response",
            ));
        }

        Ok(plan)
    }

    pub(crate) async fn verify(
        connection: &mut SqliteConnection,
        session_id: &str,
        request: &SubmitAgentTaskRequest,
    ) -> Result<(), DbError> {
        if request.assignment.role == TaskAgentRole::Planner {
            return Ok(());
        }
        let supplied = request
            .input
            .accepted_plan
            .as_ref()
            .ok_or_else(|| Self::invalid("Implementer requires an explicitly accepted plan"))?;
        let (source, plan) = Self::eligible(
            connection,
            session_id,
            &AcceptTaskPlanRequest {
                attempt_id: supplied.attempt_id.clone(),
                task_id: supplied.task_id.clone(),
            },
        )
        .await?;
        let accepted = sqlx::query!(
            "SELECT plan FROM task_plan_acceptance WHERE task_attempt_id = ?",
            supplied.attempt_id
        )
        .fetch_optional(&mut *connection)
        .await?;
        let Some(accepted) = accepted else {
            return Err(Self::invalid(
                "Planner response has not been explicitly accepted",
            ));
        };
        if supplied != &plan
            || accepted.plan != plan.text
            || source.context.assignment.agent != request.assignment.agent
            || source.input.user_request != request.input.user_request
        {
            return Err(Self::invalid(
                "Implementation input does not match the accepted plan and captured selection",
            ));
        }

        Ok(())
    }

    async fn eligible(
        connection: &mut SqliteConnection,
        session_id: &str,
        request: &AcceptTaskPlanRequest,
    ) -> Result<(TaskAgentContextRow, AcceptedTaskPlan), DbError> {
        let context = TaskAgentStore::load(connection, &request.task_id)
            .await?
            .ok_or_else(|| Self::invalid("Plan source has no isolated context"))?;
        if context.context.session_id.as_str() != session_id
            || context.context.assignment.role != TaskAgentRole::Planner
        {
            return Err(Self::invalid(
                "Plan source must be a Planner in the same session",
            ));
        }
        let version = sqlx::query!(
            r#"SELECT a.id AS "attempt_id!", o.status, o.cancel_requested AS "canceled: bool", r.assistant_message
               FROM task_attempt a JOIN session_operation o ON o.id = a.session_operation_id
               LEFT JOIN task_agent_turn r ON r.task_attempt_id = a.id
               WHERE a.task_id = ? ORDER BY a.rowid DESC LIMIT 1"#,
            request.task_id
        ).fetch_optional(&mut *connection).await?
            .ok_or_else(|| Self::invalid("Planner has no response version"))?;
        if version.attempt_id != request.attempt_id || version.status != "done" || version.canceled
        {
            return Err(Self::invalid(
                "Only the latest successful uncanceled Planner attempt can be accepted",
            ));
        }
        let response: AgentResponse = serde_json::from_str(
            version
                .assistant_message
                .as_deref()
                .ok_or_else(|| Self::invalid("Planner response is missing"))?,
        )
        .map_err(|_| Self::invalid("Planner response is invalid"))?;
        if response.answer.trim().is_empty()
            || !response.questions.is_empty()
            || !response.review_comment_outcomes.is_empty()
        {
            return Err(Self::invalid(
                "A plan must have a nonempty answer and no outstanding questions or review actions",
            ));
        }

        Ok((
            context,
            AcceptedTaskPlan {
                attempt_id: request.attempt_id.clone(),
                task_id: request.task_id.clone(),
                text: response.answer,
            },
        ))
    }

    fn invalid(reason: &str) -> DbError {
        DbError::InvalidData {
            entity: "task plan",
            reason: reason.to_string(),
        }
    }
}

#[cfg(test)]
#[path = "task_plan_test.rs"]
mod tests;
