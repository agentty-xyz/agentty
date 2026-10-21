use std::sync::Arc;
use std::time::Duration;

use ag_contracts::{OneShotError, OneShotRequest, OneShotSubmission, SessionStats};
use ag_protocol::AgentResponse;
use ag_session::{
    BuiltinAgentDefinition, ReplyToTaskChainRequest, SessionError, SessionId, SessionMessageKind,
    SessionService, StartPlanThenImplementRequest, TaskChainPhase, TaskChainReplyOutcome,
};
use ag_worker::{MockRunClient, RunClient};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::app::command_gateway::TaskChainReplyAction;
use crate::app::task_chain::test_support::{ChainFixture, agent_answer, launch_request};

/// Fails reply recording before its transcript prompt is written.
const REJECT_REPLY: &str = "CREATE TRIGGER reject_insert BEFORE INSERT ON task_chain_reply BEGIN \
                            SELECT RAISE(ABORT, 'task_chain_reply unavailable'); END";

/// Fails continuation admission after the reply was recorded.
const REJECT_CONTINUATION: &str = "CREATE TRIGGER reject_insert BEFORE INSERT ON \
                                   task_chain_continuation BEGIN SELECT RAISE(ABORT, \
                                   'task_chain_continuation unavailable'); END";

#[tokio::test]
async fn clear_approval_accepts_the_plan_once_and_later_replies_reach_the_implementer() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    install_decision(&mut fixture, "implement");
    let launch = awaiting_plan(&mut fixture).await;
    let implementation =
        fixture.register_result(&launch.implementer_task_id(), "Implemented", true);

    // Act
    let accepted = reply(&mut fixture, "reply-1", "Looks good, go ahead").await;
    implementation.started().await;
    let redelivered = reply(&mut fixture, "reply-1", "Looks good, go ahead").await;
    let racing_decision = fixture
        .app
        .apply_task_chain_reply(TaskChainReplyAction {
            chain_id: launch.chain_id.clone(),
            expected_attempt_id: launch.planner_attempt_id.clone(),
            message: "Looks good, go ahead".to_string(),
            outcome: TaskChainReplyOutcome::PlanRevised,
            reply_id: "reply-1".to_string(),
            session_id: fixture.session_id.clone(),
        })
        .await;
    let busy = reply(&mut fixture, "reply-2", "Also update the docs").await;
    let implementing = fixture.snapshot().await;
    fixture
        .app
        .sessions
        .sync_session_from_handle(&fixture.session_id);

    // Assert
    assert_eq!(accepted, Ok(TaskChainReplyOutcome::PlanAccepted));
    assert_eq!(redelivered, Ok(TaskChainReplyOutcome::PlanAccepted));
    assert_eq!(racing_decision, Ok(TaskChainReplyOutcome::PlanAccepted));
    assert_eq!(
        busy,
        Err(SessionError::TaskChainBusy(TaskChainPhase::Implementing))
    );
    assert_eq!(implementing.phase, TaskChainPhase::Implementing);
    assert_eq!(
        implementing.current_request.attempt_id,
        "reply:reply-1:implement"
    );
    assert_eq!(
        implementing.accepted_plan.expect("accepted plan").text,
        "Exact plan"
    );
    let expected_transcript = [
        launch_transcript(),
        vec![
            (
                SessionMessageKind::UserPrompt,
                "Looks good, go ahead".to_string(),
            ),
            (
                SessionMessageKind::WorkflowNotice,
                "Plan accepted · Implementer started".to_string(),
            ),
        ],
    ]
    .concat();
    assert_eq!(fixture.persisted_transcript().await, expected_transcript);
    assert_eq!(fixture.live_transcript(), expected_transcript);

    // Arrange
    implementation.release.cancel();
    fixture.settle(&implementation).await;
    assert_eq!(fixture.snapshot().await.phase, TaskChainPhase::Succeeded);
    let follow_up = fixture.register_result(&launch.implementer_task_id(), "Updated", false);

    // Act
    let continued = reply(&mut fixture, "reply-3", "Also update the docs").await;
    fixture.settle(&follow_up).await;
    let finished = fixture.snapshot().await;

    // Assert
    assert_eq!(continued, Ok(TaskChainReplyOutcome::Continued));
    assert_eq!(finished.phase, TaskChainPhase::Succeeded);
    assert_eq!(
        finished.current_request.assignment.definition,
        BuiltinAgentDefinition::Implementer
    );
    assert_eq!(
        finished.current_request.attempt_id,
        "reply:reply-3:continue"
    );
    assert_eq!(
        finished.current_request.input.clarification.as_deref(),
        Some("Also update the docs")
    );
}

#[tokio::test]
async fn approval_with_requested_changes_revises_the_plan_and_blocks_replies_while_deciding() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    let decision = Arc::new(GatedDecision::default());
    fixture
        .app
        .services
        .set_run_client(Arc::clone(&decision) as Arc<dyn RunClient>);
    let launch = awaiting_plan(&mut fixture).await;
    let revision = fixture.register_result(&launch.planner_request().task_id, "New plan", false);
    let service = fixture.service.clone();
    let session_id = fixture.session_id.clone();

    // Act
    let (revised, concurrent) = fixture
        .app
        .drive_session_request(async {
            let revised = send_reply(
                &service,
                &session_id,
                "reply-1",
                "Looks good, but add tests",
            );
            tokio::pin!(revised);
            let early = tokio::select! {
                result = &mut revised => Some(result),
                () = decision.entered.cancelled() => None,
            };
            assert_eq!(early, None, "decision must still be running");
            let concurrent = send_reply(&service, &session_id, "reply-2", "Go ahead").await;
            decision.release.cancel();

            (revised.await, concurrent)
        })
        .await;
    fixture.settle(&revision).await;
    let awaiting = fixture.snapshot().await;

    // Assert
    assert_eq!(revised, Ok(TaskChainReplyOutcome::PlanRevised));
    assert_eq!(
        concurrent,
        Err(SessionError::TaskChainBusy(
            TaskChainPhase::AwaitingPlanAcceptance
        ))
    );
    assert_eq!(awaiting.phase, TaskChainPhase::AwaitingPlanAcceptance);
    assert_eq!(awaiting.accepted_plan, None);
    assert_eq!(awaiting.current_request.attempt_id, "reply:reply-1:revise");
    assert_eq!(
        awaiting.current_request.input.clarification.as_deref(),
        Some("Looks good, but add tests")
    );
    assert_eq!(
        fixture.persisted_transcript().await,
        [
            launch_transcript(),
            vec![
                (
                    SessionMessageKind::UserPrompt,
                    "Looks good, but add tests".to_string(),
                ),
                agent_answer(BuiltinAgentDefinition::Planner, "New plan"),
            ],
        ]
        .concat()
    );

    // Arrange: an older Planner turn can no longer be decoded.
    sqlx::query("UPDATE task_attempt SET message = 'not json' WHERE id = 'initial'")
        .execute(&fixture.pool)
        .await
        .expect("corrupt planner history");

    // Act
    let unreadable = reply(&mut fixture, "reply-3", "Go ahead").await;

    // Assert
    assert!(
        unreadable
            .expect_err("history read fails")
            .to_string()
            .contains("Invalid task attempt input")
    );
}

#[tokio::test]
async fn replies_without_a_plan_to_accept_skip_the_decision_and_reject_invalid_states() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    let mut run_client = MockRunClient::new();
    run_client.expect_submit().never();
    fixture.app.services.set_run_client(Arc::new(run_client));
    let launch = launch_request();
    let planner_id = launch.planner_request().task_id;

    // Act
    let missing = reply(&mut fixture, "reply-1", "Start").await;
    let blank = reply(&mut fixture, "reply-1", "  ").await;
    let first = fixture.register_result(&planner_id, "A plan", true);
    fixture
        .app
        .drive_session_request(
            fixture
                .service
                .start_plan_then_implement(&fixture.session_id, launch),
        )
        .await
        .expect("launch");
    first.started().await;
    let busy = reply(&mut fixture, "reply-2", "Hurry up").await;
    fixture
        .app
        .drive_session_request(fixture.service.cancel_task_chain("workflow"))
        .await
        .expect("cancel workflow");
    fixture.settle(&first).await;
    let retry = fixture.register_result(&planner_id, "Retried plan", false);
    let unrecorded =
        reply_while_rejecting(&mut fixture, REJECT_REPLY, "Try again, smaller scope").await;
    let unadmitted = reply_while_rejecting(
        &mut fixture,
        REJECT_CONTINUATION,
        "Try again, smaller scope",
    )
    .await;
    let continued = reply(&mut fixture, "reply-3", "Try again, smaller scope").await;
    fixture.settle(&retry).await;
    let retried = fixture.snapshot().await;
    let transcript = fixture.persisted_transcript().await;

    // Assert
    assert_eq!(missing, Err(SessionError::TaskChainNotFound));
    assert!(matches!(blank, Err(SessionError::Operation(_))));
    assert_eq!(
        busy,
        Err(SessionError::TaskChainBusy(TaskChainPhase::Planning))
    );
    assert!(
        unrecorded
            .expect_err("reply persistence fails")
            .to_string()
            .contains("task_chain_reply unavailable")
    );
    assert!(
        unadmitted
            .expect_err("continuation admission fails")
            .to_string()
            .contains("task_chain_continuation unavailable")
    );
    assert_eq!(continued, Ok(TaskChainReplyOutcome::Continued));
    assert_eq!(retried.phase, TaskChainPhase::AwaitingPlanAcceptance);
    assert_eq!(retried.current_request.attempt_id, "reply:reply-3:continue");
    assert_eq!(
        retried.current_request.input.clarification.as_deref(),
        Some("Try again, smaller scope")
    );
    assert_eq!(
        transcript[transcript.len() - 2..],
        [
            (
                SessionMessageKind::UserPrompt,
                "Try again, smaller scope".to_string(),
            ),
            agent_answer(BuiltinAgentDefinition::Planner, "Retried plan"),
        ]
    );
    assert_eq!(
        transcript
            .iter()
            .filter(|(kind, _)| *kind == SessionMessageKind::UserPrompt)
            .count(),
        2
    );
}

/// Decision client that waits for the test before revising the plan.
#[derive(Default)]
struct GatedDecision {
    entered: CancellationToken,
    release: CancellationToken,
}

#[async_trait]
impl RunClient for GatedDecision {
    async fn submit(&self, _request: OneShotRequest) -> Result<OneShotSubmission, OneShotError> {
        self.entered.cancel();
        tokio::time::timeout(Duration::from_secs(10), self.release.cancelled())
            .await
            .expect("decision release guard");

        Ok(OneShotSubmission {
            response: AgentResponse::plain("revise"),
            stats: SessionStats::default(),
        })
    }
}

/// Starts the fixture workflow and settles its Planner on `Exact plan`.
async fn awaiting_plan(fixture: &mut ChainFixture) -> StartPlanThenImplementRequest {
    let launch = launch_request();
    let plan = fixture.register_result(&launch.planner_request().task_id, "Exact plan", false);
    fixture
        .app
        .drive_session_request(
            fixture
                .service
                .start_plan_then_implement(&fixture.session_id, launch.clone()),
        )
        .await
        .expect("launch");
    fixture.settle(&plan).await;
    assert_eq!(
        fixture.snapshot().await.phase,
        TaskChainPhase::AwaitingPlanAcceptance
    );

    launch
}

/// Transcript written by launch and the settled `Exact plan` Planner turn.
fn launch_transcript() -> Vec<(SessionMessageKind, String)> {
    vec![
        (
            SessionMessageKind::UserPrompt,
            "Implement a scoped change".to_string(),
        ),
        (SessionMessageKind::AgentGraph, "workflow".to_string()),
        agent_answer(BuiltinAgentDefinition::Planner, "Exact plan"),
    ]
}

/// Answers exactly one orchestrator decision about `Exact plan`.
fn install_decision(fixture: &mut ChainFixture, answer: &'static str) {
    let mut run_client = MockRunClient::new();
    run_client.expect_submit().once().returning(move |request| {
        assert!(request.prompt.contains(r#""Exact plan""#));

        Ok(OneShotSubmission {
            response: AgentResponse::plain(answer),
            stats: SessionStats::default(),
        })
    });
    fixture.app.services.set_run_client(Arc::new(run_client));
}

/// Sends `reply-3` while the `reject_insert` trigger is installed, then drops
/// it.
async fn reply_while_rejecting(
    fixture: &mut ChainFixture,
    trigger_sql: &'static str,
    message: &str,
) -> Result<TaskChainReplyOutcome, SessionError> {
    sqlx::query(trigger_sql)
        .execute(&fixture.pool)
        .await
        .expect("reject inserts");
    let result = reply(fixture, "reply-3", message).await;
    sqlx::query("DROP TRIGGER reject_insert")
        .execute(&fixture.pool)
        .await
        .expect("restore inserts");

    result
}

/// Sends one reply through the public service while the fixture drives it.
async fn reply(
    fixture: &mut ChainFixture,
    reply_id: &str,
    message: &str,
) -> Result<TaskChainReplyOutcome, SessionError> {
    let service = fixture.service.clone();
    let session_id = fixture.session_id.clone();

    fixture
        .app
        .drive_session_request(send_reply(&service, &session_id, reply_id, message))
        .await
}

/// Builds one public reply request.
async fn send_reply(
    service: &SessionService,
    session_id: &SessionId,
    reply_id: &str,
    message: &str,
) -> Result<TaskChainReplyOutcome, SessionError> {
    service
        .reply_to_task_chain(
            session_id,
            ReplyToTaskChainRequest {
                message: message.to_string(),
                reply_id: reply_id.to_string(),
            },
        )
        .await
}
