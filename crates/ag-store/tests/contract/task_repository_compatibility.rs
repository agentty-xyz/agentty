//! Existing task adapters can omit the optional role-aware capability.

use ag_session::{
    AcceptTaskPlanRequest, AgentKind, AgentModel, AgentSelection, CreateTaskRequest,
    DurableMessageVisibility, SubmitAgentTaskRequest, SubmitTaskRequest, Task, TaskAgentAssignment,
    TaskAgentInput, TaskAgentRole,
};
use ag_store::{DbError, TaskAgentTurnMetadata, TaskRepository};
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
            role: TaskAgentRole::Planner,
            definition: ag_session::AgentDefinition::bundled("planner").expect("bundled agent"),
        },
        attempt_id: "plan".to_string(),
        input: TaskAgentInput {
            attachments: Vec::new(),
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
    let history = adapter
        .load_agent_history("planner-task")
        .await
        .expect_err("unsupported role history");
    let response = adapter
        .persist_agent_turn(
            "plan",
            &TaskAgentTurnMetadata {
                assistant_message: "observed output".to_string(),
                harness_turn_report: None,
                instruction_conversation_id: None,
                input_tokens: 0,
                output_tokens: 0,
                provider_conversation_id: None,
            },
        )
        .await
        .expect_err("unsupported role response");

    let acceptance = adapter
        .accept_agent_plan(
            "session",
            &AcceptTaskPlanRequest {
                attempt_id: "plan".to_string(),
                task_id: "planner-task".to_string(),
            },
        )
        .await
        .expect_err("unsupported acceptance");
    let verification = adapter
        .verify_agent_plan("session", &agent)
        .await
        .expect_err("unsupported verification");

    // Assert
    assert!(admitted);
    for error in [claim, lookup, context, history, response] {
        assert_eq!(
            error.to_string(),
            "Invalid task agent context: Host does not support role-aware tasks"
        );
    }
    for error in [acceptance, verification] {
        assert_eq!(
            error.to_string(),
            "Invalid task agent context: Host does not support plan acceptance"
        );
    }
}
