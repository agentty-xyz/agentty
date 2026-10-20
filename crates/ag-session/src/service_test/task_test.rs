use std::sync::Arc;

use super::support::FakeBackend;
use crate::{
    AcceptTaskChainPlanRequest, AcceptTaskPlanRequest, AgentKind, AgentModel, AgentSelection,
    BuiltinAgentDefinition, ContinueTaskChainRequest, CreateTaskRequest, DurableMessageVisibility,
    SessionError, SessionId, SessionService, StartPlanThenImplementRequest, SubmitAgentTaskRequest,
    SubmitTaskRequest, TaskAgentAssignment, TaskAgentInput,
};

#[tokio::test]
async fn existing_hosts_report_unsupported_task_capability() {
    // Arrange
    let service = SessionService::new(Arc::new(FakeBackend::default()));
    let request = SubmitTaskRequest {
        attempt_id: "attempt".to_string(),
        message: "Continue".to_string(),
        objective: "Desired outcome".to_string(),
        task_id: "task".to_string(),
        visibility: DurableMessageVisibility::Hidden,
    };

    // Act
    let created = service
        .create_task(
            &SessionId::from("session"),
            CreateTaskRequest {
                objective: request.objective.clone(),
                task_id: request.task_id.clone(),
            },
        )
        .await;
    let listed = service.list_tasks(&SessionId::from("session")).await;
    let submitted = service
        .submit_task(&SessionId::from("session"), request)
        .await;
    let loaded = service.get_task("task").await;
    let completed = service.complete_task("task").await;
    let role_submitted = service
        .submit_agent_task(
            &SessionId::from("session"),
            SubmitAgentTaskRequest {
                assignment: TaskAgentAssignment {
                    agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
                    definition: BuiltinAgentDefinition::Planner,
                },
                attempt_id: "role-attempt".to_string(),
                input: TaskAgentInput {
                    accepted_plan: None,
                    clarification: None,
                    user_request: "Plan".to_string(),
                },
                objective: "Plan".to_string(),
                task_id: "planner".to_string(),
                visibility: DurableMessageVisibility::Hidden,
            },
        )
        .await;
    let role_history = service.get_agent_task_history("planner").await;

    // Assert
    let expected = SessionError::Operation("Host does not support tasks".to_string());
    assert_eq!(created, Err(expected.clone()));
    assert_eq!(listed, Err(expected.clone()));
    assert_eq!(submitted, Err(expected.clone()));
    assert_eq!(loaded, Err(expected.clone()));
    assert_eq!(completed, Err(expected));
    let unsupported = SessionError::Operation("Host does not support role-aware tasks".to_string());
    assert_eq!(role_submitted, Err(unsupported.clone()));
    assert_eq!(role_history, Err(unsupported));
}

#[tokio::test]
async fn existing_hosts_report_unsupported_workflow_capability() {
    // Arrange
    let service = SessionService::new(Arc::new(FakeBackend::default()));
    let session_id = SessionId::from("session");
    let start = StartPlanThenImplementRequest {
        agent: crate::test_support::codex_selection(),
        chain_id: "chain".into(),
        planner_attempt_id: "plan".into(),
        user_request: "Objective".into(),
    };

    // Act
    let started = service.start_plan_then_implement(&session_id, start).await;
    let loaded = service.get_task_chain("chain").await;
    let listed = service.list_task_chains(&session_id).await;
    let accepted = service
        .accept_task_chain_plan(
            "chain",
            AcceptTaskChainPlanRequest {
                attempt_id: "implement".into(),
                plan_attempt_id: "plan".into(),
            },
        )
        .await;
    let continued = service
        .continue_task_chain(
            "chain",
            ContinueTaskChainRequest {
                attempt_id: "continued".into(),
                clarification: None,
            },
        )
        .await;
    let canceled = service.cancel_task_chain("chain").await;
    let standalone = service
        .accept_agent_plan(
            &session_id,
            AcceptTaskPlanRequest {
                attempt_id: "plan".into(),
                task_id: "planner".into(),
            },
        )
        .await;

    // Assert
    let expected = SessionError::Operation("Host does not support task chains".into());
    assert_eq!(loaded, Err(expected.clone()));
    assert_eq!(listed, Err(expected.clone()));
    for result in [started, accepted, continued, canceled] {
        assert_eq!(result, Err(expected.clone()));
    }
    assert_eq!(
        standalone,
        Err(SessionError::Operation(
            "Host does not support plan acceptance".into()
        ))
    );
}
