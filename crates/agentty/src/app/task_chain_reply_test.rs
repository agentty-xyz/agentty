use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use ag_contracts::{OneShotError, OneShotRequest, OneShotSubmission, SessionStats};
use ag_protocol::{AgentResponse, TurnPrompt};
use ag_session::{
    GraphExecutionRef, ReplyToTaskChainRequest, SessionError, SessionId, SessionMessageKind,
    SessionService, StartPlanThenImplementRequest, TaskAgentRole, TaskChain, TaskChainPhase,
    TaskChainReplyOutcome,
};
use ag_store::{MockGraphRepository, TaskChainReplyRecord};
use ag_worker::test_support::{AppServerError, AppServerTurnResponse, MockAppServerClient};
use ag_worker::{HeartbeatClock, MockRunClient, RunClient, RunWorker, RuntimeConfig};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::app::App;
use crate::app::command_gateway::{TaskChainReplyAction, TaskChainReplyCommand};
use crate::app::task_chain::test_support::{
    ChainFixture, agent_answer, graph_unavailable, launch_request,
};

/// Fails reply recording before its transcript prompt is written.
const REJECT_REPLY: &str = "CREATE TRIGGER reject_insert BEFORE INSERT ON execution_graph_reply \
                            BEGIN SELECT RAISE(ABORT, 'reply unavailable'); END";

/// Fails continuation admission after the reply was recorded and decided.
const REJECT_CONTINUATION: &str = "CREATE TRIGGER reject_insert BEFORE INSERT ON \
                                   execution_graph_continuation BEGIN SELECT RAISE(ABORT, \
                                   'continuation unavailable'); END";

#[tokio::test]
async fn clear_approval_accepts_the_plan_once_and_later_replies_reach_the_implementer() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    install_decision(&mut fixture, "implement");
    let launch = awaiting_plan(&mut fixture).await;
    let implementation = fixture.register_turn("Implemented", true);

    // Act
    let accepted = reply(&mut fixture, "reply-1", "Looks good, go ahead").await;
    fixture
        .app
        .drive_session_request(implementation.started())
        .await;
    let redelivered = reply(&mut fixture, "reply-1", "Looks good, go ahead").await;
    let conflicting = reply(&mut fixture, "reply-1", "Actually, revise the plan").await;
    let racing_decision = fixture
        .app
        .apply_task_chain_reply(TaskChainReplyAction {
            attachments: Vec::new(),
            chain_id: launch.chain_id.clone(),
            expected_attempt_id: String::new(),
            message: "Looks good, go ahead".to_string(),
            outcome: TaskChainReplyOutcome::PlanRevised,
            reply_id: "reply-1".to_string(),
            session_id: fixture.session_id.clone(),
        })
        .await;
    let busy = reply(&mut fixture, "reply-2", "Also update the docs").await;
    let implementing = fixture.snapshot().await;
    fixture.drain_app_events().await;

    // Assert
    assert_eq!(accepted, Ok(TaskChainReplyOutcome::PlanAccepted));
    assert_eq!(redelivered, Ok(TaskChainReplyOutcome::PlanAccepted));
    assert_eq!(
        conflicting,
        Err(SessionError::Operation(
            "Reply identity was already used with different inputs".to_string()
        ))
    );
    assert_eq!(racing_decision, Ok(TaskChainReplyOutcome::PlanAccepted));
    assert_eq!(
        busy,
        Err(SessionError::TaskChainBusy(TaskChainPhase::Implementing))
    );
    assert_eq!(implementing.phase, TaskChainPhase::Implementing);
    assert_eq!(
        implementing.accepted_plan.expect("accepted plan").text,
        "Exact plan"
    );
    assert!(implementation.prompt().contains("Exact plan"));
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
    let follow_up = fixture.register_turn("Updated", false);

    // Act
    let continued = reply(&mut fixture, "reply-3", "Also update the docs").await;
    fixture.settle(&follow_up).await;
    let finished = fixture.snapshot().await;

    // Assert
    assert_eq!(continued, Ok(TaskChainReplyOutcome::Continued));
    assert_eq!(finished.phase, TaskChainPhase::Succeeded);
    assert_eq!(finished.current_role, TaskAgentRole::Implementer);
    assert!(follow_up.prompt().contains("Also update the docs"));
    assert!(follow_up.prompt().contains("Implemented"));
}

#[tokio::test]
async fn admitted_reply_keeps_its_original_workflow_after_a_new_chain_starts() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    install_decision(&mut fixture, "implement");
    awaiting_plan(&mut fixture).await;
    let implementation = fixture.register_turn("Implemented", false);
    let accepted = reply(&mut fixture, "reply-1", "Go ahead").await;
    fixture.settle(&implementation).await;
    let newer = StartPlanThenImplementRequest {
        agents: Box::new(ag_session::PlanThenImplementAgents::bundled().expect("bundled agents")),
        chain_id: "workflow-new".to_string(),
        ..launch_request()
    };
    let plan = fixture.register_turn("New plan", false);
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
    fixture.drain_app_events().await;

    // Assert
    assert_eq!(accepted, Ok(TaskChainReplyOutcome::PlanAccepted));
    assert_eq!(retried, accepted);
    assert_eq!(
        conflicting,
        Err(SessionError::Operation(
            "Reply identity was already used with different inputs".to_string()
        ))
    );
    assert_eq!(latest.phase, TaskChainPhase::AwaitingPlanAcceptance);
    assert_eq!(latest.accepted_plan, None);
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
    awaiting_plan(&mut fixture).await;
    let revision = fixture.register_turn("New plan", false);
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
    assert!(revision.prompt().contains("Looks good, but add tests"));
    assert_eq!(
        fixture.persisted_transcript().await,
        [
            launch_transcript(),
            vec![
                (
                    SessionMessageKind::UserPrompt,
                    "Looks good, but add tests".to_string(),
                ),
                agent_answer(TaskAgentRole::Planner, "New plan"),
            ],
        ]
        .concat()
    );
}

#[tokio::test]
async fn canceling_a_reply_decision_keeps_the_plan_and_later_utility_admission() {
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
    let awaiting = fixture.snapshot().await;

    // Assert
    assert_eq!(
        result,
        Err(SessionError::Operation(
            "Workflow changed before reply decision completed".to_string()
        ))
    );
    assert_eq!(awaiting.phase, TaskChainPhase::AwaitingPlanAcceptance);
    assert!(
        decision.interrupted.is_cancelled(),
        "the worker stopped its provider turn"
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

    // Arrange: decide a fresh approval through the same session-owned worker
    // to prove utility admission stayed open.
    let implementation = fixture.register_turn("Implemented", false);

    // Act
    let accepted = reply(&mut fixture, "reply-2", "Go ahead").await;
    fixture.settle(&implementation).await;
    decision.worker.shutdown().await;

    // Assert
    assert_eq!(accepted, Ok(TaskChainReplyOutcome::PlanAccepted));
    assert_eq!(fixture.snapshot().await.phase, TaskChainPhase::Succeeded);
}

#[tokio::test]
async fn queued_approval_cannot_survive_cancellation_or_clear_a_new_decision() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    let decision = Arc::new(GatedDecision {
        approval: true,
        ..GatedDecision::default()
    });
    fixture
        .app
        .services
        .set_run_client(Arc::clone(&decision) as Arc<dyn RunClient>);
    let launch = awaiting_plan(&mut fixture).await;
    let service = fixture.service.clone();
    let session_id = fixture.session_id.clone();
    let _consumer = fixture.app.sessions.foreground_consumer();
    let pending = send_reply(&service, &session_id, "reply-1", "Go ahead");
    tokio::pin!(pending);
    let reply_command = tokio::select! {
        _ = &mut pending => None,
        command = fixture.app.sessions.next_command() => Some(command),
    }
    .expect("reply must await admission");
    fixture
        .app
        .apply_session_runtime_command(reply_command)
        .await;
    decision.entered.cancelled().await;
    let cancellation = service.cancel_task_chain(&launch.chain_id);
    tokio::pin!(cancellation);
    let cancel_command = tokio::select! {
        _ = &mut cancellation => None,
        command = fixture.app.sessions.next_command() => Some(command),
    }
    .expect("cancellation must await admission");
    decision.release.cancel();
    let apply_command = fixture.app.sessions.next_command().await;

    // Act: cancellation is ahead of the already queued classifier result.
    fixture
        .app
        .apply_session_runtime_command(cancel_command)
        .await;
    cancellation.await.expect("cancel decision");
    let newer = Arc::new(CancellationToken::new());
    fixture
        .app
        .sessions
        .deciding_task_chain_ids
        .insert(launch.chain_id.clone(), Arc::clone(&newer));
    fixture
        .app
        .apply_session_runtime_command(apply_command)
        .await;
    let canceled = pending.await;
    let chain = fixture.snapshot().await;
    let recorded = fixture
        .app
        .services
        .db()
        .graphs()
        .load_task_chain_reply(&session_id, "reply-1")
        .await
        .expect("reply")
        .expect("recorded");

    // Assert
    assert_eq!(canceled, Err(App::changed_before_decision()));
    assert_eq!(chain.phase, TaskChainPhase::AwaitingPlanAcceptance);
    assert_eq!(chain.accepted_plan, None);
    assert_eq!(recorded.outcome, None);

    // Act: an uncanceled result from another generation is stale as well.
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    fixture
        .app
        .execute_task_chain_reply(
            TaskChainReplyCommand::Apply {
                action: TaskChainReplyAction {
                    attachments: Vec::new(),
                    chain_id: launch.chain_id.clone(),
                    expected_attempt_id: chain.current_attempt_id().expect("planner").into(),
                    message: "Go ahead".into(),
                    outcome: TaskChainReplyOutcome::PlanAccepted,
                    reply_id: "reply-1".into(),
                    session_id: session_id.clone(),
                },
                cancellation: Arc::new(CancellationToken::new()),
            },
            response_tx,
        )
        .await;

    // Assert
    assert_eq!(
        response_rx.await.expect("response"),
        Err(App::changed_before_decision())
    );
    assert!(Arc::ptr_eq(
        fixture
            .app
            .sessions
            .deciding_task_chain_ids
            .get(&launch.chain_id)
            .expect("new decision"),
        &newer,
    ));
    assert!(!newer.is_cancelled());
}

#[tokio::test]
async fn acceptance_without_live_session_handles_leaves_the_plan_unaccepted() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    let launch = awaiting_plan(&mut fixture).await;
    let chain = fixture.snapshot().await;
    fixture
        .app
        .services
        .db()
        .graphs()
        .record_task_chain_reply(
            &launch.chain_id,
            "reply-1",
            &TurnPrompt::from_text("Go ahead".into()),
            chain.current_execution.as_ref().expect("plan execution"),
        )
        .await
        .expect("recorded reply");
    fixture
        .app
        .sessions
        .session_handles_mut()
        .remove(&fixture.session_id);

    // Act
    let result = fixture
        .app
        .apply_task_chain_reply(TaskChainReplyAction {
            attachments: Vec::new(),
            chain_id: launch.chain_id.clone(),
            expected_attempt_id: String::new(),
            message: "Go ahead".to_string(),
            outcome: TaskChainReplyOutcome::PlanAccepted,
            reply_id: "reply-1".to_string(),
            session_id: fixture.session_id.clone(),
        })
        .await;
    let missing = fixture
        .app
        .apply_task_chain_reply(TaskChainReplyAction {
            attachments: Vec::new(),
            chain_id: launch.chain_id,
            expected_attempt_id: String::new(),
            message: "Go ahead".to_string(),
            outcome: TaskChainReplyOutcome::PlanAccepted,
            reply_id: "missing".to_string(),
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
    assert_eq!(
        missing,
        Err(SessionError::Operation(
            "Task chain reply is missing".to_string()
        ))
    );
    assert_eq!(chain.phase, TaskChainPhase::AwaitingPlanAcceptance);
    assert_eq!(chain.accepted_plan, None);
}

#[tokio::test]
async fn new_replies_report_workflow_lookup_failures_without_recording_a_prompt() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    sqlx::query("DROP TABLE execution_graph_reply")
        .execute(&fixture.pool)
        .await
        .expect("make reply lookup unavailable");

    // Act
    let result = reply(&mut fixture, "reply-1", "Go ahead").await;

    // Assert
    assert!(
        result
            .expect_err("reply lookup must fail")
            .to_string()
            .contains("no such table: execution_graph_reply")
    );
    assert_eq!(fixture.persisted_transcript().await, vec![]);
}

#[tokio::test]
async fn reply_routing_rejects_missing_sources_and_unavailable_workflows() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    awaiting_plan(&mut fixture).await;
    let original = fixture.snapshot().await;
    let before = fixture.live_transcript();

    for fault in ["list-error", "missing-source", "unavailable-driver"] {
        let mut repository = MockGraphRepository::new();
        repository
            .expect_load_task_chain_reply()
            .returning(|_, _| Ok(None));
        if fault == "list-error" {
            repository
                .expect_list_task_chains()
                .returning(|_| Err(graph_unavailable()));
        } else {
            let mut chain = original.clone();
            chain.current_execution = None;
            chain.phase = if fault == "missing-source" {
                TaskChainPhase::Canceled
            } else {
                TaskChainPhase::Planning
            };
            repository
                .expect_list_task_chains()
                .returning(move |_| Ok(vec![chain.clone()]));
            if fault == "unavailable-driver" {
                repository
                    .expect_load_graph()
                    .returning(|_, _| Err(graph_unavailable()));
            }
        }
        fixture
            .app
            .services
            .set_graph_repository(Arc::new(repository));

        // Act
        let result = reply(&mut fixture, "reply", "Guidance").await;

        // Assert
        let error = result.expect_err("workflow is unavailable").to_string();
        let expected = match fault {
            "list-error" => "injected graph outage",
            "missing-source" => "no execution to reply to",
            _ => "still working",
        };
        assert!(error.contains(expected), "{fault}: {error}");
        assert_eq!(fixture.live_transcript(), before);
    }
}

#[tokio::test]
async fn plan_lookup_rejects_missing_versions_and_repository_failures() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    awaiting_plan(&mut fixture).await;
    let chain = fixture.snapshot().await;
    for missing in [false, true] {
        let mut repository = MockGraphRepository::new();
        repository.expect_list_results().returning(move |_, _, _| {
            if missing {
                Ok(Vec::new())
            } else {
                Err(graph_unavailable())
            }
        });
        fixture
            .app
            .services
            .set_graph_repository(Arc::new(repository));

        // Act
        let result = fixture.app.task_chain_plan(&chain).await;

        // Assert
        assert!(
            result
                .expect_err("plan unavailable")
                .to_string()
                .contains(if missing {
                    "Plan version is missing"
                } else {
                    "injected graph outage"
                })
        );
    }
}

#[tokio::test]
async fn applying_replies_fails_closed_on_stale_or_inconsistent_decision_records() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    awaiting_plan(&mut fixture).await;
    let chain = fixture.snapshot().await;
    let action = TaskChainReplyAction {
        attachments: Vec::new(),
        chain_id: chain.id.clone(),
        expected_attempt_id: chain.current_attempt_id().expect("planner").into(),
        message: "Guidance".into(),
        outcome: TaskChainReplyOutcome::PlanAccepted,
        reply_id: "reply".into(),
        session_id: fixture.session_id.clone(),
    };
    for fault in [
        "load-error",
        "stale",
        "decision-error",
        "undecided",
        "utility-source",
        "missing-action",
    ] {
        let (repository, expected) = failed_reply_repository(&chain, fault);
        fixture
            .app
            .services
            .set_graph_repository(Arc::new(repository));

        // Act
        let result = fixture.app.apply_task_chain_reply(action.clone()).await;

        // Assert
        let error = result.expect_err("invalid reply state").to_string();
        assert!(error.contains(expected), "{fault}: {error}");
    }
}

/// Injects a failure at one boundary of foreground reply application.
fn failed_reply_repository(
    chain: &TaskChain,
    fault: &'static str,
) -> (MockGraphRepository, &'static str) {
    let mut repository = MockGraphRepository::new();
    let mut record = TaskChainReplyRecord {
        action_id: None,
        chain_id: chain.id.clone(),
        outcome: None,
        prompt: TurnPrompt::from_text("Guidance".into()),
        source: chain.current_execution.clone().expect("planner"),
    };
    if fault == "load-error" {
        repository
            .expect_load_task_chain_reply()
            .returning(|_, _| Err(graph_unavailable()));
        return (repository, "injected graph outage");
    }
    if fault == "utility-source" {
        record.source = GraphExecutionRef::UtilityRun {
            run_id: "utility".into(),
        };
        record.outcome = Some(TaskChainReplyOutcome::PlanAccepted);
    } else if fault == "missing-action" {
        record.outcome = Some(TaskChainReplyOutcome::Continued);
    }
    let stored = record.clone();
    repository
        .expect_load_task_chain_reply()
        .returning(move |_, _| Ok(Some(stored.clone())));
    if record.outcome.is_none() {
        let mut chain = chain.clone();
        if fault == "stale" {
            chain.phase = TaskChainPhase::Planning;
        }
        repository
            .expect_load_task_chain()
            .returning(move |_| Ok(Some(chain.clone())));
    }
    if fault != "stale" {
        repository
            .expect_decide_task_chain_reply()
            .returning(move |_, _, _| {
                if fault == "decision-error" {
                    Err(graph_unavailable())
                } else {
                    Ok(record.clone())
                }
            });
    }
    let expected = match fault {
        "stale" => "Workflow changed before reply decision",
        "decision-error" => "injected graph outage",
        "undecided" => "Reply decision was not recorded",
        "utility-source" => "Reply source is not a task",
        _ => "Reply continuation identity is missing",
    };

    (repository, expected)
}

#[tokio::test]
async fn interrupted_decisions_resume_only_against_their_bound_source() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    install_decision(&mut fixture, "revise");
    let launch = awaiting_plan(&mut fixture).await;
    let chain = fixture.snapshot().await;
    let source = chain.current_execution.clone().expect("plan execution");
    fixture
        .app
        .services
        .db()
        .graphs()
        .record_task_chain_reply(
            &launch.chain_id,
            "interrupted",
            &TurnPrompt::from_text("Add tests".into()),
            &source,
        )
        .await
        .expect("recorded before a crash");
    let revision = fixture.register_turn("Plan with tests", false);

    // Act: redelivery decides the recorded reply against its own source.
    let revised = reply(&mut fixture, "interrupted", "Add tests").await;
    fixture.settle(&revision).await;
    fixture
        .app
        .services
        .db()
        .graphs()
        .record_task_chain_reply(
            &launch.chain_id,
            "stale",
            &TurnPrompt::from_text("Ship it".into()),
            &source,
        )
        .await
        .expect("recorded against the old plan");
    let stale = reply(&mut fixture, "stale", "Ship it").await;

    // Assert
    assert_eq!(revised, Ok(TaskChainReplyOutcome::PlanRevised));
    assert_eq!(
        stale,
        Err(SessionError::Operation(
            "Workflow changed before reply decision completed".to_string()
        ))
    );
    assert_eq!(
        fixture.snapshot().await.phase,
        TaskChainPhase::AwaitingPlanAcceptance
    );
}

#[tokio::test]
async fn replies_without_a_plan_to_accept_skip_the_decision_and_reject_invalid_states() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    let mut run_client = MockRunClient::new();
    run_client.expect_submit().never();
    fixture.app.services.set_run_client(Arc::new(run_client));

    // Act
    let missing = reply(&mut fixture, "reply-1", "Start").await;
    let blank = reply(&mut fixture, "reply-1", "  ").await;
    let first = fixture.register_turn("A plan", true);
    fixture
        .app
        .drive_session_request(
            fixture
                .service
                .start_plan_then_implement(&fixture.session_id, launch_request()),
        )
        .await
        .expect("launch");
    fixture.app.drive_session_request(first.started()).await;
    let busy = reply(&mut fixture, "reply-2", "Hurry up").await;
    fixture
        .app
        .drive_session_request(fixture.service.cancel_task_chain("workflow"))
        .await
        .expect("cancel workflow");
    fixture
        .wait_for(|chain| chain.phase == TaskChainPhase::Canceled)
        .await;
    first.release.cancel();
    let retry = fixture.register_turn("Retried plan", false);
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
            .contains("reply unavailable")
    );
    assert!(
        unadmitted
            .expect_err("continuation admission fails")
            .to_string()
            .contains("continuation unavailable")
    );
    assert_eq!(continued, Ok(TaskChainReplyOutcome::Continued));
    assert_eq!(retried.phase, TaskChainPhase::AwaitingPlanAcceptance);
    assert!(retry.prompt().contains("Try again, smaller scope"));
    assert_eq!(
        transcript[transcript.len() - 2..],
        [
            (
                SessionMessageKind::UserPrompt,
                "Try again, smaller scope".to_string(),
            ),
            agent_answer(TaskAgentRole::Planner, "Retried plan"),
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
    approval: bool,
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
            response: AgentResponse::plain(if self.approval { "implement" } else { "revise" }),
            stats: SessionStats::default(),
        })
    }
}

/// Starts the fixture workflow and settles its Planner on `Exact plan`.
async fn awaiting_plan(fixture: &mut ChainFixture) -> StartPlanThenImplementRequest {
    let launch = launch_request();
    let plan = fixture.register_turn("Exact plan", false);
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
        agent_answer(TaskAgentRole::Planner, "Exact plan"),
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
                attachments: Vec::new(),
                message: message.to_string(),
                reply_id: reply_id.to_string(),
            },
        )
        .await
}
