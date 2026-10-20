//! Existing task adapters can omit the optional role-aware capability.

use ag_session::{
    AgentKind, AgentModel, AgentSelection, BuiltinAgentDefinition, CreateTaskRequest,
    DurableMessageVisibility, SubmitAgentTaskRequest, SubmitTaskRequest, Task, TaskAgentAssignment,
    TaskAgentInput,
};
use ag_store::{DbError, TaskRepository};
use async_trait::async_trait;

struct OrdinaryTaskAdapter;

#[async_trait]
impl TaskRepository for OrdinaryTaskAdapter {
    async fn create_task(
        &self,
        _session_id: &str,
        _request: &CreateTaskRequest,
    ) -> Result<(), DbError> {
        Ok(())
    }

    async fn list_tasks(&self, _session_id: &str) -> Result<Vec<Task>, DbError> {
        Ok(Vec::new())
    }

    async fn claim_attempt(
        &self,
        _session_id: &str,
        _request: &SubmitTaskRequest,
        _operation_kind: &str,
    ) -> Result<bool, DbError> {
        Ok(true)
    }

    async fn contains_attempt(
        &self,
        _session_id: &str,
        _request: &SubmitTaskRequest,
    ) -> Result<bool, DbError> {
        Ok(false)
    }

    async fn load_task(&self, _task_id: &str) -> Result<Option<Task>, DbError> {
        Ok(None)
    }

    async fn complete_task(&self, _task_id: &str) -> Result<(), DbError> {
        Ok(())
    }
}

#[tokio::test]
async fn ordinary_adapter_keeps_its_contract_and_explicitly_rejects_role_tasks() {
    // Arrange
    let adapter = OrdinaryTaskAdapter;
    let ordinary = SubmitTaskRequest {
        attempt_id: "ordinary".to_string(),
        message: "ordinary input".to_string(),
        objective: "ordinary outcome".to_string(),
        task_id: "ordinary-task".to_string(),
        visibility: DurableMessageVisibility::Visible,
    };
    let agent = SubmitAgentTaskRequest {
        assignment: TaskAgentAssignment {
            agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
            definition: BuiltinAgentDefinition::Planner,
        },
        attempt_id: "plan".to_string(),
        input: TaskAgentInput {
            accepted_plan: None,
            clarification: None,
            user_request: "plan the change".to_string(),
        },
        objective: "a plan".to_string(),
        task_id: "planner-task".to_string(),
        visibility: DurableMessageVisibility::Visible,
    };

    // Act
    let admitted = adapter
        .claim_attempt("session", &ordinary, "reply")
        .await
        .expect("ordinary admission");
    let claim = adapter
        .claim_agent_attempt("session", &agent, "reply")
        .await
        .expect_err("unsupported role admission");
    let lookup = adapter
        .contains_agent_attempt("session", &agent)
        .await
        .expect_err("unsupported role delivery lookup");
    let context = adapter
        .load_agent_context("planner-task")
        .await
        .expect_err("unsupported role context");

    // Assert
    assert!(admitted);
    for error in [claim, lookup, context] {
        assert_eq!(
            error.to_string(),
            "Invalid task agent context: Host does not support role-aware tasks"
        );
    }
}
