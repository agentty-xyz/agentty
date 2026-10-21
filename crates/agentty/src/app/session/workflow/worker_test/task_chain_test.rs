use std::sync::Arc;
use std::time::Duration;

use ag_contracts::MockAgentChannel;
use ag_session::{
    AgentKind, AgentModel, AgentSelection, ContinueTaskChainRequest, SessionId, SessionStatus,
    StartPlanThenImplementRequest, TaskChainPhase,
};
use tokio_util::sync::CancellationToken;

use super::support::successful_turn_result;
use crate::app::test_support::TestSessionRunFactory;
use crate::test_support::{new_git_test_app, set_session_status_for_test};

#[tokio::test]
async fn queued_cancellation_followed_by_retry_never_replays_the_old_attempt() {
    // Arrange
    let (mut app, _directory) = new_git_test_app().await;
    let session_id = SessionId::from(app.create_session().await.expect("session"));
    set_session_status_for_test(&mut app, &session_id, SessionStatus::Review);
    let factory = TestSessionRunFactory::install(&mut app.services);
    let service = app.session_service();
    let launch = StartPlanThenImplementRequest {
        attachments: Vec::new(),
        agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
        chain_id: "workflow".into(),
        planner_attempt_id: "initial".into(),
        user_request: "Plan a scoped change".into(),
    };
    let planner_id = launch.planner_request().task_id;
    let initial = register_response(&factory, &planner_id);
    app.drive_session_request(service.start_plan_then_implement(&session_id, launch))
        .await
        .expect("launch");
    tokio::time::timeout(Duration::from_secs(10), initial.cancelled())
        .await
        .expect("initial start");
    let hold = app
        .sessions
        .pause_session_worker(&session_id)
        .await
        .expect("worker hold");
    app.sessions.sync_session_from_handle(&session_id);
    // Only the new request may consume this scripted provider.
    let retry = register_response(&factory, &planner_id);

    // Act
    app.drive_session_request(service.continue_task_chain(
        "workflow",
        ContinueTaskChainRequest {
            attachments: Vec::new(),
            attempt_id: "queued-old".into(),
            clarification: Some("old guidance".into()),
            expected_attempt_id: "initial".into(),
        },
    ))
    .await
    .expect("queued revision");
    app.drive_session_request(service.cancel_task_chain("workflow"))
        .await
        .expect("cancel queued");
    app.drive_session_request(service.continue_task_chain(
        "workflow",
        ContinueTaskChainRequest {
            attachments: Vec::new(),
            attempt_id: "queued-new".into(),
            clarification: None,
            expected_attempt_id: "queued-old".into(),
        },
    ))
    .await
    .expect("explicit new retry");
    drop(hold);
    tokio::time::timeout(Duration::from_secs(10), retry.cancelled())
        .await
        .expect("retry start");
    let settled = app
        .sessions
        .pause_session_worker(&session_id)
        .await
        .expect("settled worker");
    let history = app
        .drive_session_request(service.get_agent_task_history(&planner_id))
        .await
        .expect("history")
        .expect("context");
    let chain = app
        .drive_session_request(service.get_task_chain("workflow"))
        .await
        .expect("snapshot")
        .expect("workflow");

    // Assert
    assert_eq!(history.turns.len(), 2);
    assert_eq!(chain.current_request.attempt_id, "queued-new");
    assert_eq!(chain.phase, TaskChainPhase::AwaitingPlanAcceptance);
    drop(settled);
}

fn register_response(factory: &TestSessionRunFactory, task_id: &str) -> CancellationToken {
    let started = CancellationToken::new();
    let signal = started.clone();
    let mut channel = MockAgentChannel::new();
    channel.expect_run_turn().once().returning(move |_, _, _| {
        signal.cancel();
        Box::pin(async { Ok(successful_turn_result("A concrete plan")) })
    });
    channel
        .expect_shutdown_session()
        .once()
        .returning(|_| Box::pin(async { Ok(()) }));
    factory.register(task_id, Arc::new(channel));

    started
}
