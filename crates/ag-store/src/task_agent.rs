//! Immutable role assignment and task-owned conversation identity.

use ag_session::{
    AgentKind, AgentSelection, SessionId, SubmitAgentTaskRequest, SubmitTaskRequest,
    TaskAgentAssignment, TaskAgentContext, TaskAgentInput, TaskAgentRole,
};
use sqlx::SqliteConnection;

use crate::DbError;

/// Persisted task conversation identity and immutable original inputs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskAgentContextRow {
    /// Task-owned identity, assignment, and session environment owner.
    pub context: TaskAgentContext,
    /// Original request and accepted-plan provenance; clarification is per
    /// attempt.
    pub input: TaskAgentInput,
}

/// Transactional binding shared by role-aware admission and delivery checks.
pub(crate) struct TaskAgentStore;

impl TaskAgentStore {
    pub(crate) fn invalid(reason: &str) -> DbError {
        DbError::InvalidData {
            entity: "task agent context",
            reason: reason.to_string(),
        }
    }

    pub(crate) fn attempt(request: &SubmitAgentTaskRequest) -> Result<SubmitTaskRequest, DbError> {
        request
            .input
            .validate(request.assignment.role)
            .map_err(|error| Self::invalid(&error.to_string()))?;

        Ok(SubmitTaskRequest {
            attempt_id: request.attempt_id.clone(),
            message: serde_json::json!(&request.input).to_string(),
            objective: request.objective.clone(),
            task_id: request.task_id.clone(),
            visibility: request.visibility,
        })
    }

    pub(crate) async fn load(
        connection: &mut SqliteConnection,
        task_id: &str,
    ) -> Result<Option<TaskAgentContextRow>, DbError> {
        let row = sqlx::query!(
            r#"SELECT c.id, c.role, c.agent, c.model, c.input, c.definition, t.session_id
               FROM task_agent_context c JOIN task t ON t.id = c.task_id
               WHERE c.task_id = ?"#,
            task_id
        )
        .fetch_optional(connection)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let role = TaskAgentRole::from_id(&row.role)
            .ok_or_else(|| Self::invalid("Unknown task agent role"))?;
        let definition = match row.definition {
            Some(json) => serde_json::from_str::<ag_session::AgentDefinition>(&json)
                .map_err(|_| Self::invalid("Invalid task agent definition"))?,
            None => ag_session::AgentDefinition::bundled(role.id())
                .ok_or(Self::invalid("Missing legacy task agent definition"))?,
        };
        let kind = row
            .agent
            .parse::<AgentKind>()
            .map_err(|_| Self::invalid("Unknown task agent provider"))?;
        let model = kind
            .parse_model(&row.model)
            .ok_or_else(|| Self::invalid("Invalid task agent model"))?;
        let input: TaskAgentInput = serde_json::from_str(&row.input)
            .map_err(|_| Self::invalid("Invalid task agent input"))?;
        input
            .validate(role)
            .map_err(|error| Self::invalid(&error.to_string()))?;

        Ok(Some(TaskAgentContextRow {
            context: TaskAgentContext {
                assignment: TaskAgentAssignment {
                    agent: AgentSelection::new(kind, model),
                    definition,
                    role,
                },
                id: row.id,
                session_id: SessionId::from(row.session_id),
                task_id: task_id.to_string(),
            },
            input,
        }))
    }

    /// Checks immutable assignment independently of per-attempt clarification.
    pub(crate) fn matches(
        row: &TaskAgentContextRow,
        session_id: &str,
        request: &SubmitAgentTaskRequest,
    ) -> Result<(), DbError> {
        if row.context.session_id.as_str() != session_id
            || row.context.assignment != request.assignment
            || row.input.user_request != request.input.user_request
            || row.input.accepted_plan != request.input.accepted_plan
        {
            return Err(Self::invalid(
                "Task agent assignment and original inputs are immutable",
            ));
        }

        Ok(())
    }

    /// Binds only tasks that have never executed in the ordinary session
    /// context.
    pub(crate) async fn bind(
        connection: &mut SqliteConnection,
        session_id: &str,
        request: &SubmitAgentTaskRequest,
    ) -> Result<(), DbError> {
        if let Some(row) = Self::load(connection, &request.task_id).await? {
            return Self::matches(&row, session_id, request);
        }
        let previous = sqlx::query!(
            r#"SELECT EXISTS(SELECT 1 FROM task_attempt WHERE task_id = ?) AS "exists!: bool""#,
            request.task_id
        )
        .fetch_one(&mut *connection)
        .await?;
        if previous.exists {
            return Err(Self::invalid(
                "Task already uses the ordinary session conversation",
            ));
        }
        let mut original = request.input.clone();
        original.clarification = None;
        let input = serde_json::json!(original).to_string();
        let role = request.assignment.role.id();
        let agent = request.assignment.agent.kind().to_string();
        let model = request.assignment.agent.model().as_str();
        let definition = serde_json::json!(request.assignment.definition).to_string();
        sqlx::query!(
            "INSERT INTO task_agent_context (task_id, role, agent, model, input, definition) \
             VALUES (?, ?, ?, ?, ?, ?)",
            request.task_id,
            role,
            agent,
            model,
            input,
            definition
        )
        .execute(connection)
        .await?;

        Ok(())
    }

    pub(crate) async fn require_ordinary(
        connection: &mut SqliteConnection,
        task_id: &str,
    ) -> Result<(), DbError> {
        if Self::load(connection, task_id).await?.is_some() {
            return Err(Self::invalid("Task requires role-aware submission"));
        }

        Ok(())
    }
}

#[cfg(test)]
#[path = "task_agent_test.rs"]
mod tests;
