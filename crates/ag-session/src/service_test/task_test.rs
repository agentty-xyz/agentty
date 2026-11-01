use std::collections::BTreeMap;
use std::sync::Arc;

use super::support::FakeBackend;
use crate::{
    AcceptTaskChainPlanRequest, AcceptTaskPlanRequest, AgentKind, AgentModel, AgentSelection,
    ContinueTaskChainRequest, CreateTaskRequest, DurableMessageVisibility, ReplyToTaskChainRequest,
    SessionError, SessionId, SessionService, StartPlanThenImplementRequest, SubmitAgentTaskRequest,
    SubmitTaskRequest, TaskAgentAssignment, TaskAgentInput, TaskAgentRole,
};

#[tokio::test]
async fn existing_hosts_reject_graph_capabilities_without_admitting_work() {
    // Arrange
    let service = SessionService::new(Arc::new(FakeBackend::default()));
    let graph = crate::ExecutionGraph {
        id: "graph".into(),
        nodes: Vec::new(),
        revision: std::num::NonZeroU64::MIN,
        session_id: "session".into(),
    };
    let reference = crate::GraphResultRef {
        execution: crate::GraphExecutionRef::UtilityRun {
            run_id: "run".into(),
        },
        graph_id: "graph".into(),
        graph_revision: std::num::NonZeroU64::MIN,
        id: "result".into(),
        node_id: "node".into(),
        output_name: "output".into(),
        schema: ag_protocol::ProtocolRequestProfile::UtilityPrompt,
        version: std::num::NonZeroU64::MIN,
    };

    // Act
    let started = service
        .start_execution_graph(crate::StartExecutionGraphRequest {
            graph,
            provider_call_limits: BTreeMap::default(),
        })
        .await;
    let action = crate::ContinueGraphNodeRequest {
        action_id: "new-execution".into(),
        execution: reference.execution.clone(),
        graph_id: reference.graph_id.clone(),
        kind: crate::GraphNodeContinuation::Retry,
        node_id: reference.node_id.clone(),
        revision: reference.graph_revision,
    };
    let control = crate::GraphControlRequest {
        graph_id: "graph".into(),
        revision: std::num::NonZeroU64::MIN,
    };
    let progress = service.get_execution_graph("graph").await;
    let cancelled = service.cancel_execution_graph(control.clone()).await;
    let resumed = service
        .resume_execution_graph(crate::ResumeExecutionGraphRequest {
            graph: control,
            provider_call_limits: BTreeMap::new(),
        })
        .await;
    let continued = service.continue_execution_graph_node(action).await;
    let accepted = service.accept_execution_graph_result(reference).await;

    // Assert
    let expected = SessionError::Operation("Host does not support execution graphs".into());
    assert_eq!(started, Err(expected.clone()));
    assert_eq!(progress, Err(expected.clone()));
    assert_eq!(cancelled, Err(expected.clone()));
    assert_eq!(resumed, Err(expected.clone()));
    assert_eq!(continued, Err(expected.clone()));
    assert_eq!(accepted, Err(expected));
}

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
                    role: TaskAgentRole::Planner,
                    definition: crate::AgentDefinition::bundled("planner").expect("bundled agent"),
                },
                attempt_id: "role-attempt".to_string(),
                input: TaskAgentInput {
                    attachments: Vec::new(),
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
        agents: Box::new(crate::PlanThenImplementAgents::bundled().expect("bundled agents")),
        attachments: Vec::new(),
        agent: crate::test_support::codex_selection(),
        chain_id: "chain".into(),
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
                plan_attempt_id: "plan".into(),
            },
        )
        .await;
    let continued = service
        .continue_task_chain(
            "chain",
            ContinueTaskChainRequest {
                attachments: Vec::new(),
                attempt_id: "continued".into(),
                clarification: None,
                expected_attempt_id: "plan".into(),
            },
        )
        .await;
    let canceled = service.cancel_task_chain("chain").await;
    let replied = service
        .reply_to_task_chain(
            &session_id,
            ReplyToTaskChainRequest {
                attachments: Vec::new(),
                message: "Go ahead".into(),
                reply_id: "reply".into(),
            },
        )
        .await;
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
    assert_eq!(replied, Err(expected));
    assert_eq!(
        standalone,
        Err(SessionError::Operation(
            "Host does not support plan acceptance".into()
        ))
    );
}
