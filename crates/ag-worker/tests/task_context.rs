//! Task runtime identity through the public worker boundary.

use std::sync::{Arc, Mutex};

use ag_contracts::{
    AgentDefinitionPrompt, AgentRequestKind, PermissionMode, ReasoningLevel, ResponseStyle,
    SpeedMode, TurnContinuation, TurnRequest,
};
use ag_protocol::TurnPrompt;
use ag_session::{
    AgentKind, AgentModel, AgentSelection, TaskAgentAssignment, TaskAgentContext, TaskAgentRole,
};
use ag_worker::test_support::{AppServerTurnResponse, MockAppServerClient};
use ag_worker::{RuntimeConfig, SessionRunClient};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn task_runtime_keys_are_stable_and_separate_from_ordinary_chat() {
    // Arrange
    let dispatched = Arc::new(Mutex::new(Vec::new()));
    let cleaned = Arc::new(Mutex::new(Vec::new()));
    let mut server = MockAppServerClient::new();
    let turn_ids = Arc::clone(&dispatched);
    server
        .expect_run_turn()
        .times(4)
        .returning(move |request, _| {
            turn_ids.lock().expect("turn IDs").push(request.session_id);
            Box::pin(async {
                Ok(AppServerTurnResponse {
                    assistant_message: r#"{"answer":"done","questions":[]}"#.to_string(),
                    context_reset: false,
                    input_tokens: 1,
                    output_tokens: 1,
                    pid: None,
                    provider_conversation_id: None,
                })
            })
        });
    let cleanup_ids = Arc::clone(&cleaned);
    server
        .expect_shutdown_session()
        .times(3)
        .returning(move |id| {
            cleanup_ids.lock().expect("cleanup IDs").push(id);
            Box::pin(async {})
        });
    let config = RuntimeConfig::with_app_server(Arc::new(server));
    let context = TaskAgentContext {
        assignment: TaskAgentAssignment {
            agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
            role: TaskAgentRole::Planner,
            definition: ag_session::AgentDefinition::bundled("planner").expect("bundled agent"),
        },
        id: "aaaaaaaa-0000-0000-0000-000000000001".to_string(),
        session_id: "owning-session".into(),
        task_id: "arbitrary/user/task-id".to_string(),
    };
    let another_context = TaskAgentContext {
        id: "aaaaaaaa-0000-0000-0000-000000000002".to_string(),
        task_id: "another-task".to_string(),
        ..context.clone()
    };
    let ordinary = SessionRunClient::new("owning-session".into(), AgentKind::Codex, &config);
    let first = SessionRunClient::for_task_context(&context, &config).expect("first context");
    let second =
        SessionRunClient::for_task_context(&another_context, &config).expect("second context");

    // Act
    for client in [&first, &second, &ordinary] {
        client
            .submit(
                request(),
                mpsc::unbounded_channel().0,
                CancellationToken::new(),
            )
            .await
            .expect("scripted turn");
    }
    let resumed = SessionRunClient::for_task_context(&context, &config).expect("retained identity");
    resumed
        .submit(
            request(),
            mpsc::unbounded_channel().0,
            CancellationToken::new(),
        )
        .await
        .expect("next attempt");
    for client in [&resumed, &second, &ordinary] {
        client.shutdown().await.expect("cleanup");
    }

    // Assert
    let first_key = SessionRunClient::task_runtime_id(&context).expect("cleanup identity");
    let second_key = SessionRunClient::task_runtime_id(&another_context).expect("cleanup identity");
    assert_eq!(
        *dispatched.lock().expect("turn IDs"),
        [
            first_key.clone(),
            second_key.clone(),
            "owning-session".to_string(),
            first_key.clone(),
        ]
    );
    assert_eq!(
        *cleaned.lock().expect("cleanup IDs"),
        [first_key, second_key, "owning-session".to_string(),]
    );
}

fn request() -> TurnRequest {
    TurnRequest {
        continuation: TurnContinuation::fresh(),
        execution_policy: ag_contracts::ExecutionPolicy::default(),
        folder: ".".into(),
        main_checkout_root: None,
        model: "gpt-6.1-sol".to_string(),
        permission_mode: PermissionMode::ReadOnly,
        agent_profile: AgentDefinitionPrompt::default(),
        prompt: TurnPrompt::from("inspect the workspace"),
        reasoning_level: ReasoningLevel::default(),
        request_kind: AgentRequestKind::SessionStart,
        response_style: ResponseStyle::default(),
        speed_mode: SpeedMode::default(),
    }
}
