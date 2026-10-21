use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use ag_contracts::{OneShotError, OneShotRequest, OneShotSubmission, SessionStats};
use ag_protocol::AgentResponse;
use ag_session::{
    BuiltinAgentDefinition, ReplyToTaskChainRequest, SessionError, SessionId, SessionMessageKind,
    SessionService, StartPlanThenImplementRequest, TaskChainPhase, TaskChainReplyOutcome,
};
use ag_worker::test_support::{AppServerError, AppServerTurnResponse, MockAppServerClient};
use ag_worker::{HeartbeatClock, MockRunClient, RunClient, RunWorker, RuntimeConfig};
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
    let conflicting = reply(&mut fixture, "reply-1", "Actually, revise the plan").await;
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
    assert_eq!(
        conflicting,
        Err(SessionError::Operation(
            "Invalid task chain: Reply identity was already used with different inputs".to_string()
        ))
    );
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
async fn admitted_reply_keeps_its_original_workflow_after_a_new_chain_starts() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    install_decision(&mut fixture, "implement");
    let original = awaiting_plan(&mut fixture).await;
    let implementation =
        fixture.register_result(&original.implementer_task_id(), "Implemented", false);
    let accepted = reply(&mut fixture, "reply-1", "Go ahead").await;
    fixture.settle(&implementation).await;
    let newer = StartPlanThenImplementRequest {
        chain_id: "workflow-new".to_string(),
        planner_attempt_id: "new-plan".to_string(),
        ..launch_request()
    };
    let plan = fixture.register_result(&newer.planner_request().task_id, "New plan", false);
    fixture
        .app
        .drive_session_request(
            fixture
                .service
                .start_plan_then_implement(&fixture.session_id, newer.clone()),
        )
        .await
        .expect("new workflow");
    fixture.settle_chain(&plan, &newer.chain_id).await;
    let before = fixture.persisted_transcript().await;

    // Act
    let retried = reply(&mut fixture, "reply-1", "Go ahead").await;
    let conflicting = reply(&mut fixture, "reply-1", "Revise the old plan").await;
    let latest = fixture
        .app
        .drive_session_request(fixture.service.get_task_chain(&newer.chain_id))
        .await
        .expect("new workflow read")
        .expect("new workflow exists");
    fixture
        .app
        .sessions
        .sync_session_from_handle(&fixture.session_id);

    // Assert
    assert_eq!(accepted, Ok(TaskChainReplyOutcome::PlanAccepted));
    assert_eq!(retried, accepted);
    assert_eq!(
        conflicting,
        Err(SessionError::Operation(
            "Invalid task chain: Reply identity was already used with different inputs".to_string()
        ))
    );
    assert_eq!(latest.phase, TaskChainPhase::AwaitingPlanAcceptance);
    assert_eq!(latest.current_request.attempt_id, newer.planner_attempt_id);
    assert_eq!(fixture.persisted_transcript().await, before);
    assert_eq!(fixture.live_transcript(), before);
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
async fn canceling_a_reply_decision_preserves_cancellation_and_later_utility_admission() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    let decision = CancelableDecisionWorker::install(&mut fixture);
    let launch = awaiting_plan(&mut fixture).await;
    let service = fixture.service.clone();
    let session_id = fixture.session_id.clone();

    // Act
    let result = fixture
        .app
        .drive_session_request(async {
            let pending = send_reply(&service, &session_id, "reply-1", "Add tests first");
            tokio::pin!(pending);
            let early = tokio::select! {
                result = &mut pending => Some(result),
                () = decision.started.cancelled() => None,
            };
            assert_eq!(early, None, "decision must still be running");
            service
                .cancel_task_chain(&launch.chain_id)
                .await
                .expect("cancel chain");
            pending.await
        })
        .await;
    let canceled = fixture.snapshot().await;

    // Assert
    assert_eq!(
        result,
        Err(SessionError::Operation(
            "Workflow changed before reply decision completed".to_string()
        ))
    );
    assert_eq!(canceled.phase, TaskChainPhase::Canceled);
    assert!(
        decision.interrupted.is_cancelled(),
        "the worker stopped its provider turn"
    );
    assert_eq!(
        canceled.current_request.attempt_id,
        launch.planner_attempt_id
    );
    assert!(
        !fixture
            .app
            .sessions
            .deciding_task_chain_ids
            .contains_key(&launch.chain_id)
    );
    assert_eq!(
        fixture.persisted_transcript().await,
        [
            launch_transcript(),
            vec![(
                SessionMessageKind::UserPrompt,
                "Add tests first".to_string()
            )],
        ]
        .concat()
    );

    // Arrange: retry the canceled role, then decide a fresh approval through
    // the same session-owned worker to prove utility admission stayed open.
    let retried_plan =
        fixture.register_result(&launch.planner_request().task_id, "Exact plan", false);

    // Act
    let retried = reply(&mut fixture, "reply-2", "Try again").await;
    fixture.settle(&retried_plan).await;
    let implementation =
        fixture.register_result(&launch.implementer_task_id(), "Implemented", false);
    let accepted = reply(&mut fixture, "reply-3", "Go ahead").await;
    fixture.settle(&implementation).await;
    decision.worker.shutdown().await;

    // Assert
    assert_eq!(retried, Ok(TaskChainReplyOutcome::Continued));
    assert_eq!(accepted, Ok(TaskChainReplyOutcome::PlanAccepted));
    assert_eq!(fixture.snapshot().await.phase, TaskChainPhase::Succeeded);
}

#[tokio::test]
async fn acceptance_without_live_session_handles_leaves_the_plan_unaccepted() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    let launch = awaiting_plan(&mut fixture).await;
    fixture
        .app
        .sessions
        .session_handles_mut()
        .remove(&fixture.session_id);
    let before = fixture.persisted_transcript().await;

    // Act
    let result = fixture
        .app
        .apply_task_chain_reply(TaskChainReplyAction {
            chain_id: launch.chain_id,
            expected_attempt_id: launch.planner_attempt_id,
            message: "Go ahead".to_string(),
            outcome: TaskChainReplyOutcome::PlanAccepted,
            reply_id: "reply-1".to_string(),
            session_id: fixture.session_id.clone(),
        })
        .await;
    let chain = fixture.snapshot().await;

    // Assert
    assert_eq!(
        result,
        Err(SessionError::Operation(
            "Session handles not found".to_string()
        ))
    );
    assert_eq!(chain.phase, TaskChainPhase::AwaitingPlanAcceptance);
    assert_eq!(chain.accepted_plan, None);
    assert_eq!(fixture.persisted_transcript().await, before);
}

#[tokio::test]
async fn new_replies_report_workflow_lookup_failures_without_recording_a_prompt() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    sqlx::query("DROP TABLE task_chain")
        .execute(&fixture.pool)
        .await
        .expect("make workflow listing unavailable");

    // Act
    let result = reply(&mut fixture, "reply-1", "Go ahead").await;

    // Assert
    assert!(
        result
            .expect_err("workflow lookup must fail")
            .to_string()
            .contains("no such table: task_chain")
    );
    assert_eq!(fixture.persisted_transcript().await, vec![]);
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

/// Real utility worker whose first provider turn waits for cancellation and
/// whose next turn approves the plan.
struct CancelableDecisionWorker {
    interrupted: CancellationToken,
    started: CancellationToken,
    worker: Arc<RunWorker>,
}

impl CancelableDecisionWorker {
    fn install(fixture: &mut ChainFixture) -> Self {
        let started = CancellationToken::new();
        let interrupted = CancellationToken::new();
        let mut provider = MockAppServerClient::new();
        let started_observer = started.clone();
        let interrupted_observer = interrupted.clone();
        provider
            .expect_run_isolated_turn()
            .once()
            .return_once(move |_, _| {
                Box::pin(async move {
                    started_observer.cancel();
                    interrupted_observer.cancelled().await;

                    Err(AppServerError::InterruptedByUser(
                        "decision canceled".to_string(),
                    ))
                })
            });
        provider
            .expect_run_isolated_turn()
            .once()
            .returning(|_, _| {
                Box::pin(async {
                    Ok(AppServerTurnResponse {
                        assistant_message: r#"{"answer":"implement"}"#.to_string(),
                        context_reset: false,
                        input_tokens: 1,
                        output_tokens: 1,
                        pid: None,
                        provider_conversation_id: None,
                    })
                })
            });
        let interrupted_observer = interrupted.clone();
        provider
            .expect_shutdown_session()
            .times(1..)
            .returning(move |_| {
                interrupted_observer.cancel();

                Box::pin(async {})
            });
        let worker = Arc::new(RunWorker::new(
            &RuntimeConfig::with_app_server(Arc::new(provider)),
            fixture.app.services.db().runs(),
            Arc::new(HeartbeatClock),
            NonZeroUsize::MIN,
        ));
        fixture
            .app
            .services
            .set_run_client(Arc::clone(&worker) as Arc<dyn RunClient>);

        Self {
            interrupted,
            started,
            worker,
        }
    }
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
