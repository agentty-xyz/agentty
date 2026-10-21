use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ag_contracts::{OneShotSubmission, SessionStats};
use ag_protocol::AgentResponse;
use ag_session::{
    AcceptTaskChainPlanRequest, BuiltinAgentDefinition, SessionError, SessionId,
    SessionMessageKind, SessionStatus, StartPlanThenImplementRequest, TaskChain, TaskChainPhase,
    TaskChainReplyOutcome,
};
use ag_store::{DbError, MockTaskRepository};
use ag_worker::MockRunClient;

use super::{deferred_reply_error, task_chain_reply_failure};
use crate::app::prompt_intent::{PromptSessionMode, PromptSubmission, PromptWorkflowOutcome};
use crate::app::task_chain::test_support::{ChainFixture, Script, launch_request};
use crate::domain::turn_prompt::{TurnPrompt, TurnPromptAttachment, TurnPromptTextSource};
use crate::test_support::set_session_status_for_test;

#[tokio::test]
async fn plan_request_starts_one_planner_chain_with_the_session_agent() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    let session_agent = fixture
        .app
        .sessions
        .session_for_id(&fixture.session_id)
        .expect("session")
        .agent;

    // Act
    let blank = fixture
        .app
        .submit_plan_request(submission(&fixture.session_id, "  "))
        .await;
    let with_image = fixture
        .app
        .submit_plan_request(PromptSubmission {
            prompt: image_prompt("Add search [Image #1]"),
            ..submission(&fixture.session_id, "")
        })
        .await;
    let started = fixture
        .app
        .submit_plan_request(submission(&fixture.session_id, "Add search"))
        .await;
    let repeated = fixture
        .app
        .submit_plan_request(submission(&fixture.session_id, "Add sorting"))
        .await;
    let chains = list_chains(&fixture).await;

    // Assert
    assert_eq!(blank, PromptWorkflowOutcome::KeepPrompt);
    assert_eq!(with_image, PromptWorkflowOutcome::KeepPrompt);
    assert_eq!(
        started,
        PromptWorkflowOutcome::ShowSession {
            session_id: fixture.session_id.clone()
        }
    );
    assert_eq!(repeated, PromptWorkflowOutcome::KeepPrompt);
    assert_eq!(chains.len(), 1);
    assert_eq!(chains[0].agent, session_agent);
    assert_eq!(chains[0].user_request, "Add search");
    let transcript = fixture.persisted_transcript().await;
    assert!(has_notice(
        &transcript,
        "Image attachments are not supported"
    ));
    assert!(has_notice(&transcript, "already runs a plan workflow"));
    assert!(transcript.contains(&(SessionMessageKind::UserPrompt, "Add search".to_string())));
    assert!(transcript.contains(&(SessionMessageKind::AgentGraph, chains[0].id.clone())));
}

#[tokio::test]
async fn plan_request_keeps_the_draft_when_the_planner_cannot_start() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    set_session_status_for_test(
        &mut fixture.app,
        &fixture.session_id,
        SessionStatus::InProgress,
    );

    // Act
    let missing = fixture
        .app
        .submit_plan_request(submission(&SessionId::from("missing"), "Add search"))
        .await;
    let busy = fixture
        .app
        .submit_plan_request(submission(&fixture.session_id, "Add search"))
        .await;

    // Assert
    assert_eq!(missing, PromptWorkflowOutcome::KeepPrompt);
    assert_eq!(busy, PromptWorkflowOutcome::KeepPrompt);
    assert_eq!(list_chains(&fixture).await.len(), 0);
    assert!(has_notice(
        &fixture.persisted_transcript().await,
        "The Planner could not start"
    ));
}

#[tokio::test]
async fn composer_messages_reach_the_orchestrator_and_never_the_planner_after_acceptance() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    // The terminal loop consumes runtime commands for its whole lifetime.
    let _terminal_consumer = fixture.app.sessions.foreground_consumer();
    let ordinary = fixture
        .app
        .submit_task_chain_prompt(&fixture.session_id, &TurnPrompt::from_text("Hi".into()))
        .await;
    let launch = awaiting_plan(&mut fixture).await;
    install_decision(&mut fixture, "implement");
    let implementation =
        fixture.register_result(&launch.implementer_task_id(), "Implemented", true);

    // Act
    let with_image = fixture
        .app
        .submit_task_chain_prompt(&fixture.session_id, &image_prompt("Go [Image #1]"))
        .await;
    let approval = fixture
        .app
        .submit_prompt(submission(
            &fixture.session_id,
            "Looks good, start implementing",
        ))
        .await;
    drive_until_started(&mut fixture, &implementation).await;
    let while_busy = fixture
        .app
        .submit_task_chain_prompt(
            &fixture.session_id,
            &TurnPrompt::from_text("Also add docs".into()),
        )
        .await;
    implementation.release.cancel();
    fixture.settle(&implementation).await;
    let follow_up = fixture.register_result(&launch.implementer_task_id(), "Docs added", false);
    let after_success = fixture
        .app
        .submit_task_chain_prompt(
            &fixture.session_id,
            &TurnPrompt::from_text("Also add docs".into()),
        )
        .await;
    fixture.settle(&follow_up).await;
    let finished = fixture.snapshot().await;

    // Assert
    let show_session = Some(PromptWorkflowOutcome::ShowSession {
        session_id: fixture.session_id.clone(),
    });
    assert_eq!(ordinary, None);
    assert_eq!(with_image, Some(PromptWorkflowOutcome::KeepPrompt));
    assert_eq!(Some(approval), show_session);
    assert_eq!(while_busy, Some(PromptWorkflowOutcome::KeepPrompt));
    assert_eq!(after_success, show_session);
    assert_eq!(finished.phase, TaskChainPhase::Succeeded);
    assert_eq!(
        finished.current_request.assignment.definition,
        BuiltinAgentDefinition::Implementer
    );
    assert_eq!(
        finished.current_request.input.clarification.as_deref(),
        Some("Also add docs")
    );
    let transcript = fixture.persisted_transcript().await;
    assert!(has_notice(
        &transcript,
        "Image attachments are not supported"
    ));
    assert!(has_notice(&transcript, "The Implementer is still working"));
}

#[tokio::test]
async fn failed_decided_action_is_reported_after_the_reply_was_accepted() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    let _terminal_consumer = fixture.app.sessions.foreground_consumer();
    awaiting_plan(&mut fixture).await;
    install_decision(&mut fixture, "implement");
    let (missing_sender, missing_receiver) = tokio::sync::oneshot::channel();

    // Act
    let approval = fixture
        .app
        .submit_task_chain_prompt(
            &fixture.session_id,
            &TurnPrompt::from_text("Start implementing".into()),
        )
        .await;
    fixture
        .app
        .report_deferred_task_chain_reply(&SessionId::from("missing"), missing_receiver);
    set_session_status_for_test(
        &mut fixture.app,
        &fixture.session_id,
        SessionStatus::InProgress,
    );
    // Poll the live transcript; polling storage would compete with the
    // foreground for the test pool's connection.
    let transcript = Arc::clone(
        &fixture
            .app
            .sessions
            .session_handles_or_err(&fixture.session_id)
            .expect("handles")
            .transcript,
    );
    fixture
        .app
        .drive_session_request(async move {
            tokio::time::timeout(Duration::from_secs(10), async {
                while !transcript
                    .lock()
                    .expect("transcript")
                    .messages()
                    .iter()
                    .any(|message| message.content.contains("could not route your message"))
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("failure notice guard");
        })
        .await;

    // Assert
    assert!(missing_sender.is_closed());
    assert_eq!(
        approval,
        Some(PromptWorkflowOutcome::ShowSession {
            session_id: fixture.session_id.clone()
        })
    );
    assert_eq!(
        fixture.snapshot().await.phase,
        TaskChainPhase::AwaitingPlanAcceptance
    );
}

#[tokio::test]
async fn generated_instructions_follow_the_session_routing() {
    // Arrange: a merged session rejects ordinary replies before any model run.
    let mut fixture = ChainFixture::new().await;
    set_session_status_for_test(&mut fixture.app, &fixture.session_id, SessionStatus::Merged);

    // Act
    let ordinary = fixture
        .app
        .reply_through_task_chain_or_session(
            &fixture.session_id,
            TurnPrompt::from_text("Fix".into()),
        )
        .await;

    // Assert
    assert!(!ordinary);
    assert_eq!(list_chains(&fixture).await.len(), 0);

    for previous_count in [Some(2), None] {
        // Arrange
        let mut fixture = ChainFixture::new().await;
        let _terminal_consumer = fixture.app.sessions.foreground_consumer();
        let launch = awaiting_plan(&mut fixture).await;
        let implementation =
            fixture.register_result(&launch.implementer_task_id(), "Implemented", false);
        let service = fixture.service.clone();
        fixture
            .app
            .drive_session_request(service.accept_task_chain_plan(
                &launch.chain_id,
                AcceptTaskChainPlanRequest {
                    attempt_id: "implementation".to_string(),
                    plan_attempt_id: launch.planner_attempt_id.clone(),
                },
            ))
            .await
            .expect("accepted plan");
        fixture.settle(&implementation).await;
        let applied =
            fixture.register_result(&launch.implementer_task_id(), "Review applied", true);
        if let Some(previous_count) = previous_count {
            fixture
                .app
                .auto_address_review_iterations
                .insert(fixture.session_id.clone(), previous_count);
        }

        // Act
        let routed = fixture
            .app
            .reply_through_task_chain_or_session(
                &fixture.session_id,
                TurnPrompt::from_agent_data("Apply the review suggestions".to_string()),
            )
            .await;
        drive_until_started(&mut fixture, &applied).await;
        applied.release.cancel();
        fixture.settle(&applied).await;

        // Assert
        assert!(routed);
        let revised = fixture.snapshot().await;
        assert_eq!(revised.phase, TaskChainPhase::Succeeded);
        assert_eq!(
            revised.current_request.assignment.definition,
            BuiltinAgentDefinition::Implementer
        );
        assert_eq!(
            fixture
                .app
                .auto_address_review_iterations
                .get(&fixture.session_id)
                .copied(),
            previous_count
        );
        assert_eq!(
            revised.current_request.input.clarification.as_deref(),
            Some("Apply the review suggestions")
        );
    }
}

#[tokio::test]
async fn cancel_stops_only_a_running_chain_role() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    let without_chain = fixture
        .app
        .cancel_running_task_chain(&fixture.session_id)
        .await;
    let launch = launch_request();
    let planner = fixture.register_result(&launch.planner_request().task_id, "Plan", true);
    start_chain(&mut fixture, launch).await;
    planner.started().await;

    // Act
    let canceled = fixture
        .app
        .cancel_running_task_chain(&fixture.session_id)
        .await;
    let snapshot = fixture.snapshot().await;
    let repeated = fixture
        .app
        .cancel_running_task_chain(&fixture.session_id)
        .await;

    // Assert
    assert!(!without_chain);
    assert!(canceled);
    assert_eq!(snapshot.phase, TaskChainPhase::Canceled);
    assert!(!repeated);
    planner.release.cancel();
}

#[tokio::test]
async fn unreadable_or_failed_chain_storage_keeps_chain_routing_closed() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    let launch = launch_request();
    let planner = fixture.register_result(&launch.planner_request().task_id, "Plan", true);
    start_chain(&mut fixture, launch).await;
    planner.started().await;
    let running = fixture.snapshot().await;
    let mut failing = MockTaskRepository::new();
    failing
        .expect_list_task_chains()
        .times(4)
        .returning(|_| Err(invalid_store()));
    fixture.app.services.set_task_repository(Arc::new(failing));

    // Act
    let unreadable_cancel = fixture
        .app
        .cancel_running_task_chain(&fixture.session_id)
        .await;
    let unreadable_route = fixture
        .app
        .submit_task_chain_prompt(&fixture.session_id, &TurnPrompt::from_text("Hi".into()))
        .await;
    let unreadable_generated = fixture
        .app
        .reply_through_task_chain_or_session(
            &fixture.session_id,
            TurnPrompt::from_agent_data("Apply review".into()),
        )
        .await;
    let unreadable_start = fixture
        .app
        .submit_plan_request(submission(&fixture.session_id, "Add search"))
        .await;
    fixture
        .app
        .services
        .set_task_repository(Arc::new(cancel_failing_store(running)));
    let failed_cancel = fixture
        .app
        .cancel_running_task_chain(&fixture.session_id)
        .await;

    // Assert
    assert!(unreadable_cancel);
    assert_eq!(unreadable_route, Some(PromptWorkflowOutcome::KeepPrompt));
    assert!(!unreadable_generated);
    assert_eq!(unreadable_start, PromptWorkflowOutcome::KeepPrompt);
    assert!(failed_cancel);
    assert!(has_notice(
        &fixture.persisted_transcript().await,
        "Failed to cancel the running agent"
    ));
    planner.release.cancel();
}

#[tokio::test]
async fn deferred_completions_preserve_failures_and_report_lost_responses() {
    // Arrange
    let (sender, receiver) = tokio::sync::oneshot::channel::<()>();
    drop(sender);
    let closed = receiver.await.expect_err("closed response");

    // Act
    let success = deferred_reply_error(Ok(Ok(TaskChainReplyOutcome::Continued)));
    let failed = deferred_reply_error(Ok(Err(SessionError::TaskChainNotFound)));
    let interrupted = deferred_reply_error(Err(closed));

    // Assert
    assert_eq!(success, None);
    assert_eq!(failed, Some(SessionError::TaskChainNotFound));
    assert!(
        interrupted
            .expect("lost response")
            .to_string()
            .contains("response was interrupted")
    );
}

#[test]
fn reply_failures_explain_how_to_proceed() {
    // Arrange
    let cases = [
        (
            SessionError::TaskChainBusy(TaskChainPhase::Planning),
            "The Planner is still working",
        ),
        (
            SessionError::TaskChainBusy(TaskChainPhase::Implementing),
            "The Implementer is still working",
        ),
        (
            SessionError::TaskChainBusy(TaskChainPhase::AwaitingPlanAcceptance),
            "still deciding on your previous reply",
        ),
        (
            SessionError::TaskChainNotFound,
            "could not route your message: Session has no task chain",
        ),
    ];

    for (error, expected) in cases {
        // Act
        let text = task_chain_reply_failure(&error);

        // Assert
        assert!(text.contains(expected), "{text}");
    }
}

/// Builds an existing-session submission for `text`.
fn submission(session_id: &SessionId, text: &str) -> PromptSubmission {
    PromptSubmission {
        prompt: TurnPrompt::from_text(text.to_string()),
        session_id: session_id.clone(),
        session_mode: PromptSessionMode::Existing,
    }
}

/// Builds a prompt carrying one pasted image placeholder.
fn image_prompt(text: &str) -> TurnPrompt {
    TurnPrompt {
        attachments: vec![TurnPromptAttachment {
            local_image_path: PathBuf::from("/tmp/image.png"),
            placeholder: "[Image #1]".to_string(),
        }],
        text: text.to_string(),
        text_source: TurnPromptTextSource::UserPrompt,
    }
}

/// Returns whether a reply-error notice containing `text` was recorded.
fn has_notice(transcript: &[(SessionMessageKind, String)], text: &str) -> bool {
    transcript.iter().any(|(kind, content)| {
        *kind == SessionMessageKind::WorkflowNotice && content.contains(text)
    })
}

/// Lists the fixture session's chains from storage.
async fn list_chains(fixture: &ChainFixture) -> Vec<TaskChain> {
    fixture
        .app
        .services
        .db()
        .tasks()
        .list_task_chains(&fixture.session_id)
        .await
        .expect("chains")
}

/// Starts `launch` through the public service.
async fn start_chain(fixture: &mut ChainFixture, launch: StartPlanThenImplementRequest) {
    let service = fixture.service.clone();
    let session_id = fixture.session_id.clone();
    fixture
        .app
        .drive_session_request(async move {
            service.start_plan_then_implement(&session_id, launch).await
        })
        .await
        .expect("launch");
}

/// Starts the fixture workflow and settles its Planner on `Exact plan`.
async fn awaiting_plan(fixture: &mut ChainFixture) -> StartPlanThenImplementRequest {
    let launch = launch_request();
    let plan = fixture.register_result(&launch.planner_request().task_id, "Exact plan", false);
    start_chain(fixture, launch.clone()).await;
    fixture.settle(&plan).await;

    launch
}

/// Answers exactly one orchestrator decision.
fn install_decision(fixture: &mut ChainFixture, answer: &'static str) {
    let mut run_client = MockRunClient::new();
    run_client.expect_submit().once().returning(move |_| {
        Ok(OneShotSubmission {
            response: AgentResponse::plain(answer),
            stats: SessionStats::default(),
        })
    });
    fixture.app.services.set_run_client(Arc::new(run_client));
}

/// Processes runtime commands, including a decided reply action, until the
/// scripted role turn starts.
async fn drive_until_started(fixture: &mut ChainFixture, script: &Script) {
    fixture.app.drive_session_request(script.started()).await;
}

/// Store that reports `running` and rejects its cancellation.
fn cancel_failing_store(running: TaskChain) -> MockTaskRepository {
    let mut store = MockTaskRepository::new();
    let listed = running.clone();
    store
        .expect_list_task_chains()
        .returning(move |_| Ok(vec![listed.clone()]));
    store
        .expect_load_task_chain()
        .returning(move |_| Ok(Some(running.clone())));
    store
        .expect_cancel_task_chain()
        .returning(|_| Err(invalid_store()));

    store
}

/// Builds a deterministic store failure.
fn invalid_store() -> DbError {
    DbError::InvalidData {
        entity: "task chain",
        reason: "unavailable".to_string(),
    }
}
