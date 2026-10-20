use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_contracts::{AgentError, AgentRequestKind, MockAgentChannel, PermissionMode, TurnRequest};
use ag_git::MockGitClient;
use ag_protocol::{AgentResponse, QuestionItem};
use ag_session::{
    AgentKind, AgentModel, AgentSelection, BuiltinAgentDefinition, DurableMessageVisibility,
    SessionId, SessionStatus, SubmitAgentTaskRequest, TaskAgentAssignment, TaskAgentInput,
    TaskAttemptStatus,
};
use ag_worker::{MockRunClient, SessionRunClient};
use sqlx::SqlitePool;
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

use super::support::{
    mock_fs_client_with_existing_directories, queue_helper_context, successful_turn_result,
};
use crate::app::App;
use crate::app::service::MockSessionRunFactory;
use crate::app::session::SessionTaskService;
use crate::app::session::workflow::task_turn;
use crate::app::session::workflow::worker::SessionWorkerContext;
use crate::app::test_support::TestSessionRunFactory;
use crate::domain::session_message::SessionTranscript;
use crate::infra::db::{AppRepositories, TaskAgentTurnMetadata};
use crate::infra::personality::MockPersonalityCatalogClient;
use crate::test_support::{
    SessionFixtureBuilder, new_git_test_app_with_pool, new_test_app, set_session_status_for_test,
};

#[tokio::test]
async fn planner_execution_uses_captured_model_own_history_and_read_only_permissions() {
    // Arrange
    let (mut app, _directory, pool) = new_git_test_app_with_pool().await;
    let session_id = SessionId::from(app.create_session().await.expect("session"));
    app.services
        .db()
        .sessions()
        .update_session_provider_conversation_id(&session_id, Some("ordinary-provider".to_string()))
        .await
        .expect("ordinary provider");
    app.services
        .db()
        .sessions()
        .update_session_instruction_conversation_id(
            &session_id,
            Some("ordinary-instructions".to_string()),
        )
        .await
        .expect("ordinary instructions");
    app.services
        .db()
        .sessions()
        .update_session_questions(
            &session_id,
            &serde_json::json!([QuestionItem::new("Ordinary question?")]).to_string(),
        )
        .await
        .expect("ordinary questions");
    let factory = TestSessionRunFactory::install(&mut app.services);
    let service = app.session_service();
    let before = app
        .drive_session_request(service.get_session(&session_id))
        .await
        .expect("session")
        .expect("present");

    for (attempt, expected_kind) in [
        ("first", AgentRequestKind::SessionStart),
        ("retry", AgentRequestKind::SessionResume),
    ] {
        assert_planner_attempt(
            &mut app,
            Arc::clone(&factory),
            service.clone(),
            session_id.clone(),
            before.clone(),
            attempt,
            expected_kind,
        )
        .await;
    }
    assert_task_admission_rejections(&mut app, &service, &session_id).await;
    assert_unloaded_claim_fails(&mut app, &service, &session_id).await;
    pool.close().await;
    assert!(
        app.drive_session_request(service.get_agent_task_history("planner"))
            .await
            .is_err()
    );
    assert!(
        app.drive_session_request(service.submit_agent_task(&session_id, request("closed")))
            .await
            .is_err()
    );
}

async fn assert_planner_attempt(
    app: &mut App,
    factory: Arc<TestSessionRunFactory>,
    service: ag_session::SessionService,
    session_id: SessionId,
    before: ag_session::Session,
    attempt: &str,
    expected_kind: AgentRequestKind,
) {
    let (started_tx, mut started_rx) = mpsc::unbounded_channel::<TurnRequest>();
    let mut channel = MockAgentChannel::new();
    channel
        .expect_run_turn()
        .once()
        .returning(move |_, turn, _| {
            started_tx.send(turn).expect("observe request");
            Box::pin(async {
                let mut result = successful_turn_result("A plan");
                result.assistant_message.questions = vec![QuestionItem::new("Task question?")];
                result.provider_conversation_id = Some("task-provider".to_string());
                Ok(result)
            })
        });
    channel
        .expect_shutdown_session()
        .once()
        .returning(|_| Box::pin(async { Ok(()) }));
    factory.register("planner", Arc::new(channel));
    let request = request(attempt);

    // Act
    app.drive_session_request(service.submit_agent_task(&session_id, request.clone()))
        .await
        .expect("admit");
    let turn = tokio::time::timeout(Duration::from_secs(10), started_rx.recv())
        .await
        .expect("guard")
        .expect("started");
    let pause = app
        .sessions
        .pause_session_worker(&session_id)
        .await
        .expect("worker");
    app.drive_session_request(service.submit_agent_task(&session_id, request))
        .await
        .expect("duplicate");
    let history = app
        .drive_session_request(service.get_agent_task_history("planner"))
        .await
        .expect("read")
        .expect("history");
    let after = app
        .drive_session_request(service.get_session(&session_id))
        .await
        .expect("session")
        .expect("present");
    let task = app
        .drive_session_request(service.get_task("planner"))
        .await
        .expect("task")
        .expect("present");

    // Assert
    assert_planner_request(&turn, attempt, &expected_kind);
    assert_eq!(after.messages, before.messages);
    assert_eq!(after.questions, before.questions);
    assert_eq!(after.settings, before.settings);
    assert_eq!(after.status, SessionStatus::Review);
    assert_eq!(task.completed_at, None);
    assert_eq!(
        task.attempts.last().expect("attempt").status,
        TaskAttemptStatus::Succeeded
    );
    assert_eq!(history.turns.len(), if attempt == "first" { 1 } else { 2 });
    assert_ordinary_continuation(app, &session_id).await;
    drop(pause);
}

fn assert_planner_request(turn: &TurnRequest, attempt: &str, expected_kind: &AgentRequestKind) {
    assert_eq!(turn.model, AgentModel::Gpt61Sol.provider_model_str());
    assert_eq!(turn.permission_mode, PermissionMode::ReadOnly);
    assert_eq!(&turn.request_kind, expected_kind);
    assert!(
        turn.prompt
            .agent_text()
            .starts_with(BuiltinAgentDefinition::Planner.instructions())
    );
    assert!(
        turn.prompt
            .agent_text()
            .contains("Explicit task input (JSON data):")
    );
    assert_eq!(
        turn.continuation.provider_conversation_id(),
        if attempt == "first" {
            None
        } else {
            Some("task-provider")
        }
    );
}

async fn assert_ordinary_continuation(app: &App, session_id: &SessionId) {
    let ordinary = (
        app.services
            .db()
            .sessions()
            .get_session_provider_conversation_id(session_id)
            .await
            .expect("ordinary provider"),
        app.services
            .db()
            .sessions()
            .get_session_instruction_conversation_id(session_id)
            .await
            .expect("ordinary instructions"),
    );
    assert_eq!(
        ordinary,
        (
            Some("ordinary-provider".to_string()),
            Some("ordinary-instructions".to_string())
        )
    );
}

async fn assert_task_admission_rejections(
    app: &mut App,
    service: &ag_session::SessionService,
    session_id: &SessionId,
) {
    // Act / Assert: admission never consumes ordinary question state.
    for status in [
        SessionStatus::Question,
        SessionStatus::InProgress,
        SessionStatus::Done,
    ] {
        set_session_status_for_test(app, session_id, status);
        assert!(
            app.drive_session_request(service.submit_agent_task(session_id, request("rejected")))
                .await
                .is_err()
        );
        app.drive_session_request(service.submit_agent_task(session_id, request("first")))
            .await
            .expect("old receipt remains valid");
    }
    set_session_status_for_test(app, session_id, SessionStatus::Review);
    assert!(
        app.drive_session_request(
            service.submit_agent_task(&SessionId::from("missing"), request("missing"))
        )
        .await
        .is_err()
    );
    let mut conflicting = request("first");
    conflicting.objective = "A different outcome".to_string();
    assert!(
        app.drive_session_request(service.submit_agent_task(session_id, conflicting))
            .await
            .is_err()
    );
    let mut unaccepted = request("unaccepted");
    unaccepted.assignment.definition = BuiltinAgentDefinition::Implementer;
    unaccepted.task_id = "unaccepted-implementation".to_string();
    unaccepted.input.accepted_plan = Some(ag_session::AcceptedTaskPlan {
        task_id: "planner".to_string(),
        attempt_id: "first".to_string(),
        text: "Unaccepted plan".to_string(),
    });
    assert!(
        app.drive_session_request(service.submit_agent_task(session_id, unaccepted))
            .await
            .is_err()
    );
    assert!(
        app.services
            .db()
            .tasks()
            .load_task("unaccepted-implementation")
            .await
            .expect("unaccepted task lookup")
            .is_none()
    );
    app.drive_session_request(service.complete_task("planner"))
        .await
        .expect("explicit complete");
    assert!(
        app.drive_session_request(service.submit_agent_task(session_id, request("completed")))
            .await
            .is_err()
    );
}

async fn assert_unloaded_claim_fails(
    app: &mut App,
    service: &ag_session::SessionService,
    session_id: &SessionId,
) {
    assert!(
        app.drive_session_request(service.get_agent_task_history("missing"))
            .await
            .expect("read missing")
            .is_none()
    );
    // Act / Assert: a coordinator claim with no loaded owner fails durably.
    let project = app
        .services
        .db()
        .sessions()
        .load_session_project_id(session_id)
        .await
        .expect("project lookup")
        .expect("project");
    app.services
        .db()
        .sessions()
        .insert_session("unloaded", "gpt-5.6-sol", "main", "Review", project)
        .await
        .expect("persist unloaded owner");
    let mut orphan = request("orphan");
    orphan.task_id = "orphan-task".to_string();
    app.services
        .db()
        .tasks()
        .claim_agent_attempt("unloaded", &orphan, "agent_task")
        .await
        .expect("claim");
    assert!(
        app.sessions
            .enqueue_claimed_agent_task(&app.services, "unloaded", orphan)
            .await
            .is_err()
    );
    let orphan = app
        .services
        .db()
        .tasks()
        .load_task("orphan-task")
        .await
        .expect("task")
        .expect("present");
    assert_eq!(orphan.attempts[0].status, TaskAttemptStatus::Failed);
}

#[tokio::test]
async fn canceling_one_task_attempt_does_not_interrupt_a_later_attempt() {
    // Arrange
    let (mut app, _directory, _pool) = new_git_test_app_with_pool().await;
    let session_id = SessionId::from(app.create_session().await.expect("session"));
    set_session_status_for_test(&mut app, &session_id, SessionStatus::Review);
    let factory = TestSessionRunFactory::install(&mut app.services);
    let service = app.session_service();
    for attempt in ["stop", "next"] {
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let release = Arc::new(Notify::new());
        let released = Arc::clone(&release);
        let mut channel = MockAgentChannel::new();
        channel.expect_run_turn().once().returning(move |_, _, _| {
            started_tx.send(()).expect("started");
            let release = Arc::clone(&released);
            Box::pin(async move {
                release.notified().await;
                Ok(successful_turn_result("Task report"))
            })
        });
        channel
            .expect_shutdown_session()
            .returning(|_| Box::pin(async { Ok(()) }));
        factory.register("planner", Arc::new(channel));

        // Act
        app.drive_session_request(service.submit_agent_task(&session_id, request(attempt)))
            .await
            .expect("admit");
        tokio::time::timeout(Duration::from_secs(10), started_rx.recv())
            .await
            .expect("guard")
            .expect("running");
        if attempt == "stop" {
            app.services
                .db()
                .operations()
                .request_cancel_for_session_operations(&session_id)
                .await
                .expect("durable cancellation");
        }
        app.sessions.cancel_agent_task_attempt("task-attempt:stop");
        if attempt == "next" {
            release.notify_one();
        }
        let pause = app
            .sessions
            .pause_session_worker(&session_id)
            .await
            .expect("settled");
        let task = app
            .drive_session_request(service.get_task("planner"))
            .await
            .expect("task")
            .expect("present");

        // Assert
        assert_eq!(
            task.attempts.last().expect("attempt").status,
            if attempt == "stop" {
                TaskAttemptStatus::Canceled
            } else {
                TaskAttemptStatus::Succeeded
            }
        );
        assert_eq!(
            app.services
                .db()
                .sessions()
                .load_session_messages(&session_id)
                .await
                .expect("ordinary transcript")
                .len(),
            0
        );
        drop(pause);
    }
    // Act / Assert: an already admitted receipt never creates another client.
    app.sessions
        .enqueue_agent_task(&app.services, &session_id, request("next"))
        .await
        .expect("receipt");
    assert!(
        app.sessions
            .enqueue_claimed_agent_task(&app.services, &session_id, request("absent"))
            .await
            .is_err()
    );

    assert_implementation_reserves_stack(&mut app, factory, service, session_id).await;
}

async fn assert_implementation_reserves_stack(
    app: &mut App,
    factory: Arc<TestSessionRunFactory>,
    service: ag_session::SessionService,
    session_id: SessionId,
) {
    // Arrange: the successful Planner response is explicitly accepted.
    let plan = app
        .services
        .db()
        .tasks()
        .accept_agent_plan(
            &session_id,
            &ag_session::AcceptTaskPlanRequest {
                task_id: "planner".to_string(),
                attempt_id: "next".to_string(),
            },
        )
        .await
        .expect("accepted version");
    let mut implementation = request("implementation");
    implementation.assignment.definition = BuiltinAgentDefinition::Implementer;
    implementation.task_id = "implementation".to_string();
    implementation.input.accepted_plan = Some(plan);
    app.sessions.push_session(
        SessionFixtureBuilder::new()
            .id("child")
            .parent_session_id(Some(session_id.clone()))
            .status(SessionStatus::Review)
            .build(),
    );
    let paused = app
        .sessions
        .pause_session_worker(&session_id)
        .await
        .expect("hold worker");
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let mut channel = MockAgentChannel::new();
    channel
        .expect_run_turn()
        .once()
        .returning(move |_, turn, _| {
            assert_eq!(turn.permission_mode, PermissionMode::AutoEdit);
            started_tx.send(()).expect("implementation started");
            Box::pin(async { Ok(successful_turn_result("Implementation report")) })
        });
    channel
        .expect_shutdown_session()
        .once()
        .returning(|_| Box::pin(async { Ok(()) }));
    factory.register("implementation", Arc::new(channel));

    // Act / Assert: queued implementation reserves the stack before dispatch.
    app.drive_session_request(service.submit_agent_task(&session_id, implementation))
        .await
        .expect("implementation admitted");
    assert!(!app.sessions.can_reply_to_session_in_stack("child"));
    drop(paused);
    tokio::time::timeout(Duration::from_secs(10), started_rx.recv())
        .await
        .expect("guard")
        .expect("started");
    let settled = app
        .sessions
        .pause_session_worker(&session_id)
        .await
        .expect("settled");
    let task = app
        .services
        .db()
        .tasks()
        .load_task("implementation")
        .await
        .expect("task")
        .expect("present");
    assert_eq!(task.attempts[0].status, TaskAttemptStatus::Succeeded);
    assert!(app.sessions.can_reply_to_session_in_stack("child"));
    assert_eq!(
        app.services
            .db()
            .sessions()
            .load_session_messages(&session_id)
            .await
            .expect("ordinary history")
            .len(),
        0
    );
    drop(settled);
}

#[tokio::test]
async fn implementation_admission_preserves_stack_branch_exclusivity() {
    // Arrange
    let (mut app, directory) = new_test_app().await;
    app.sessions.push_session(
        SessionFixtureBuilder::new()
            .id("parent")
            .status(SessionStatus::Review)
            .build(),
    );
    app.sessions.push_session(
        SessionFixtureBuilder::new()
            .id("child")
            .parent_session_id(Some("parent".into()))
            .status(SessionStatus::InProgress)
            .build(),
    );
    let mut implementation = request("blocked");
    implementation.assignment.definition = BuiltinAgentDefinition::Implementer;

    // Act
    let result = app
        .sessions
        .enqueue_agent_task(&app.services, "parent", implementation)
        .await;

    // Assert
    assert!(
        result
            .expect_err("active child blocks mutation")
            .to_string()
            .contains("Stacked task execution")
    );
    app.sessions.push_session(
        SessionFixtureBuilder::new()
            .id("deferred-child")
            .parent_session_id(Some("parent".into()))
            .draft(true)
            .status(SessionStatus::Draft)
            .build(),
    );
    let deferred_child = app
        .sessions
        .validate_agent_task_admission("deferred-child", BuiltinAgentDefinition::Planner)
        .expect_err("active stack blocks workspace preparation");
    assert!(deferred_child.to_string().contains("idle stack"));
    // Act / Assert: deferred workspaces fail before admitting model work.
    app.sessions.push_session(
        SessionFixtureBuilder::new()
            .id("deferred")
            .draft(true)
            .status(SessionStatus::Draft)
            .folder(directory.path().join("unprepared-task"))
            .build(),
    );
    let deferred = app
        .sessions
        .enqueue_agent_task(&app.services, "deferred", request("deferred"))
        .await
        .expect_err("unprepared workspace");
    assert!(deferred.to_string().contains("prepared workspace"));
    assert!(
        app.services
            .db()
            .tasks()
            .load_task("planner")
            .await
            .expect("read")
            .is_none()
    );
}

#[tokio::test]
async fn task_execution_failures_and_cancellation_do_not_save_or_leak_responses() {
    for scenario in [
        "provider",
        "shutdown",
        "factory",
        "pre-canceled",
        "during-turn",
        "missing-context",
        "closed-storage",
        "poison-token",
        "invalid-status",
        "canceled-cleanup",
    ] {
        // Arrange
        let (context, pool) = execution_context().await;
        let request = request(scenario);
        if scenario != "missing-context" {
            context
                .db
                .tasks()
                .claim_agent_attempt("sess", &request, "agent_task")
                .await
                .expect("claim");
            context
                .db
                .operations()
                .mark_session_operation_running(&format!("task-attempt:{scenario}"))
                .await
                .expect("running");
        }
        let cancellation = CancellationToken::new();
        let factory = failure_factory(scenario, &cancellation);
        if scenario == "pre-canceled" {
            cancellation.cancel();
        } else if scenario == "closed-storage" {
            pool.close().await;
        } else if scenario == "poison-token" {
            let token = Arc::clone(&context.cancel_token);
            let _ = std::thread::spawn(move || {
                let _guard = token.lock().expect("guard");
                std::panic::resume_unwind(Box::new("poison cancellation lock"));
            })
            .join();
        } else if scenario == "invalid-status" {
            *context.status.lock().expect("status") = SessionStatus::Done;
        }

        // Act
        let result = task_turn::run(
            &context,
            Arc::new(MockRunClient::new()),
            &request,
            &factory,
            cancellation,
        )
        .await;

        // Assert
        assert!(result.is_err(), "{scenario}");
        assert!(
            context
                .transcript
                .lock()
                .expect("ordinary transcript")
                .is_empty()
        );
        if scenario != "closed-storage" {
            let history = context
                .db
                .tasks()
                .load_agent_history("planner")
                .await
                .expect("history");
            assert!(
                history.is_none_or(|history| history.turns.is_empty()),
                "{scenario}"
            );
        }
    }
}

fn failure_factory(
    scenario: &'static str,
    cancellation: &CancellationToken,
) -> MockSessionRunFactory {
    let mut factory = MockSessionRunFactory::new();
    if ["provider", "shutdown", "during-turn", "canceled-cleanup"].contains(&scenario) {
        let mut channel = MockAgentChannel::new();
        let turn_cancellation = cancellation.clone();
        channel.expect_run_turn().once().returning(move |_, _, _| {
            if scenario == "during-turn" {
                turn_cancellation.cancel();
                return Box::pin(std::future::pending());
            }
            Box::pin(async move {
                if scenario == "provider" {
                    Err(AgentError::Backend("provider failed".to_string()))
                } else {
                    Ok(successful_turn_result("unsaved response"))
                }
            })
        });
        let cleanup_cancellation = cancellation.clone();
        channel.expect_shutdown_session().returning(move |_| {
            if scenario == "canceled-cleanup" {
                cleanup_cancellation.cancel();
            }
            Box::pin(async move {
                if scenario == "shutdown" {
                    Err(AgentError::Backend("cleanup failed".to_string()))
                } else {
                    Ok(())
                }
            })
        });
        let client = SessionRunClient::from_channel("task-context".to_string(), Arc::new(channel));
        factory
            .expect_create_task()
            .once()
            .returning(move |_| Ok(client.clone()));
    } else if scenario == "factory" {
        factory
            .expect_create_task()
            .once()
            .returning(|_| Err(AgentError::Runtime("composition failed".to_string())));
    }

    factory
}

#[tokio::test]
async fn task_host_notices_stay_in_ephemeral_projection() {
    // Arrange
    let (context, _pool) = execution_context().await;
    let transcript = Arc::new(Mutex::new(SessionTranscript::ephemeral(Vec::new())));

    // Act
    SessionTaskService::append_workflow_notice(
        &transcript,
        &context.db,
        &context.app_event_tx,
        &context.session_update_versions,
        "sess",
        "Task effect failed",
    )
    .await;

    // Assert
    assert_eq!(
        context
            .db
            .sessions()
            .load_session_messages("sess")
            .await
            .expect("ordinary messages")
            .len(),
        0
    );
    assert!(
        context
            .transcript
            .lock()
            .expect("ordinary projection")
            .is_empty()
    );
    assert!(
        transcript
            .lock()
            .expect("task projection")
            .replay_text()
            .expect("diagnostic")
            .contains("Task effect failed")
    );
}

#[tokio::test]
async fn implementer_enforces_accepted_plan_permissions_and_isolated_effect_context() {
    for scenario in [
        "writable",
        "read-only",
        "questions",
        "persist-failure",
        "stale-plan",
        "invalid-permission",
        "personality",
        "missing-personality",
        "stateless",
        "commit-failure",
    ] {
        // Arrange
        let (mut context, pool) = execution_context().await;
        let mut planner = request("plan");
        if scenario == "stateless" {
            planner.assignment.agent =
                AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash);
        }
        let request = accepted_implementation(&context, planner).await;
        prepare_implementation_scenario(&mut context, &pool, &request, scenario).await;
        let (request_tx, mut request_rx) = mpsc::unbounded_channel();
        let factory = implementation_factory(scenario, request_tx);

        // Act
        let result = ag_worker::execute(
            context.db.operations(),
            &ag_worker::HeartbeatClock,
            "task-attempt:execute",
            task_turn::run(
                &context,
                Arc::new(MockRunClient::new()),
                &request,
                &factory,
                CancellationToken::new(),
            ),
            |error| matches!(error, crate::app::SessionError::StoppedByUser(_)),
            |_| {},
        )
        .await;

        // Assert
        let should_fail = [
            "persist-failure",
            "stale-plan",
            "invalid-permission",
            "commit-failure",
        ]
        .contains(&scenario);
        assert_eq!(result.is_err(), should_fail, "{scenario}: {result:?}");
        assert_eq!(
            context
                .db
                .sessions()
                .load_session_messages("sess")
                .await
                .expect("ordinary history")
                .len(),
            0
        );
        assert!(
            context
                .transcript
                .lock()
                .expect("ordinary projection")
                .is_empty()
        );
        if let Ok(turn) = request_rx.try_recv() {
            assert_implementation_request(&turn, scenario);
        }
        let history = context
            .db
            .tasks()
            .load_agent_history("implementation")
            .await
            .expect("history")
            .expect("context");
        let response_saved =
            !["persist-failure", "stale-plan", "invalid-permission"].contains(&scenario);
        assert_eq!(
            history.turns.len(),
            usize::from(response_saved),
            "{scenario}"
        );
        assert_eq!(history.provider_conversation_id, None);
        let task = context
            .db
            .tasks()
            .load_task("implementation")
            .await
            .expect("task")
            .expect("present");
        assert_eq!(
            task.attempts[0].status,
            if should_fail {
                TaskAttemptStatus::Failed
            } else {
                TaskAttemptStatus::Succeeded
            }
        );
    }
}

fn implementation_factory(
    scenario: &'static str,
    request_tx: mpsc::UnboundedSender<TurnRequest>,
) -> MockSessionRunFactory {
    let mut factory = MockSessionRunFactory::new();
    if !["stale-plan", "invalid-permission"].contains(&scenario) {
        let mut channel = MockAgentChannel::new();
        channel
            .expect_run_turn()
            .once()
            .returning(move |_, turn, _| {
                request_tx.send(turn).expect("observe");
                Box::pin(async move {
                    let mut result = successful_turn_result("Implemented accepted scope");
                    result.context_reset = true;
                    result.provider_conversation_id = None;
                    if scenario == "questions" {
                        result.assistant_message.questions =
                            vec![QuestionItem::new("Task-only clarification?")];
                    }
                    Ok(result)
                })
            });
        channel
            .expect_shutdown_session()
            .once()
            .returning(|_| Box::pin(async { Ok(()) }));
        let client =
            SessionRunClient::from_channel("implementation".to_string(), Arc::new(channel));
        factory
            .expect_create_task()
            .once()
            .returning(move |_| Ok(client.clone()));
    }

    factory
}

async fn prepare_implementation_scenario(
    context: &mut SessionWorkerContext,
    pool: &SqlitePool,
    request: &SubmitAgentTaskRequest,
    scenario: &'static str,
) {
    if scenario == "commit-failure" {
        context.git_client = Arc::new(execution_git_client(true));
    }
    if scenario == "read-only" {
        context
            .db
            .sessions()
            .update_session_permission_mode("sess", PermissionMode::ReadOnly)
            .await
            .expect("permission");
    } else if scenario == "invalid-permission" {
        sqlx::query("UPDATE session SET permission_mode='invalid' WHERE id='sess'")
            .execute(pool)
            .await
            .expect("corrupt permission");
    } else if scenario == "persist-failure" {
        sqlx::query(
            "CREATE TRIGGER reject_task_turn BEFORE INSERT ON task_agent_turn WHEN \
             NEW.task_attempt_id='execute' BEGIN SELECT RAISE(ABORT,'failed task persistence'); \
             END",
        )
        .execute(pool)
        .await
        .expect("trigger");
    } else if scenario == "stale-plan" {
        let mut revision = request.clone();
        revision.assignment.definition = BuiltinAgentDefinition::Planner;
        revision.input.accepted_plan = None;
        revision.attempt_id = "revision".to_string();
        revision.task_id = "planner".to_string();
        revision.objective = "Propose a plan".to_string();
        context
            .db
            .tasks()
            .claim_agent_attempt("sess", &revision, "agent_task")
            .await
            .expect("new plan version");
    }
    if ["personality", "missing-personality"].contains(&scenario) {
        context
            .db
            .sessions()
            .update_session_personality_id("sess", Some("presentation".to_string()))
            .await
            .expect("selected personality");
        let mut catalog = MockPersonalityCatalogClient::new();
        catalog
            .expect_resolve()
            .withf(|_, id| id == "presentation")
            .once()
            .returning(move |_, _| {
                Box::pin(async move {
                    (scenario == "personality").then(|| ag_session::Personality {
                        description: String::new(),
                        id: "presentation".to_string(),
                        name: "Presentation".to_string(),
                        prompt: "Use concise prose".to_string(),
                    })
                })
            });
        context.personality_catalog_client = Arc::new(catalog);
    }
}

fn assert_implementation_request(turn: &TurnRequest, scenario: &str) {
    assert_eq!(
        turn.permission_mode,
        if scenario == "read-only" {
            PermissionMode::ReadOnly
        } else {
            PermissionMode::AutoEdit
        }
    );
    assert_eq!(
        turn.personality.current(),
        if scenario == "personality" {
            Some("Use concise prose")
        } else {
            None
        }
    );
    assert!(turn.prompt.agent_text().contains("Accepted plan"));
    assert_eq!(turn.continuation.provider_conversation_id(), None);
}

async fn accepted_implementation(
    context: &SessionWorkerContext,
    planner: SubmitAgentTaskRequest,
) -> SubmitAgentTaskRequest {
    context
        .db
        .tasks()
        .claim_agent_attempt("sess", &planner, "agent_task")
        .await
        .expect("planner claim");
    context
        .db
        .operations()
        .mark_session_operation_running("task-attempt:plan")
        .await
        .expect("planner running");
    context
        .db
        .tasks()
        .persist_agent_turn(
            "plan",
            &TaskAgentTurnMetadata {
                assistant_message: serde_json::json!(AgentResponse::plain("Accepted plan"))
                    .to_string(),
                harness_turn_report: None,
                instruction_conversation_id: None,
                input_tokens: 0,
                output_tokens: 0,
                provider_conversation_id: None,
            },
        )
        .await
        .expect("plan response");
    context
        .db
        .operations()
        .mark_session_operation_done("task-attempt:plan")
        .await
        .expect("planner done");
    let accepted = context
        .db
        .tasks()
        .accept_agent_plan(
            "sess",
            &ag_session::AcceptTaskPlanRequest {
                attempt_id: "plan".to_string(),
                task_id: "planner".to_string(),
            },
        )
        .await
        .expect("accept plan");
    let mut request = planner;
    request.assignment.definition = BuiltinAgentDefinition::Implementer;
    request.attempt_id = "execute".to_string();
    request.task_id = "implementation".to_string();
    request.input.accepted_plan = Some(accepted);
    context
        .db
        .tasks()
        .claim_agent_attempt("sess", &request, "agent_task")
        .await
        .expect("implementation claim");
    context
        .db
        .operations()
        .mark_session_operation_running("task-attempt:execute")
        .await
        .expect("implementation running");

    request
}

async fn execution_context() -> (SessionWorkerContext, SqlitePool) {
    let queue = Arc::new(Mutex::new(VecDeque::new()));
    let mut context = queue_helper_context(queue).await;
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("database");
    let project = db
        .projects()
        .upsert_project("/project", None)
        .await
        .expect("project");
    db.sessions()
        .insert_session("sess", "gpt-5.6-sol", "main", "Review", project)
        .await
        .expect("session");
    context.db = db;
    context.folder = PathBuf::from("/task-workspace");
    context.fs_client = Arc::new(mock_fs_client_with_existing_directories());
    context.git_client = Arc::new(execution_git_client(false));
    *context.status.lock().expect("status") = SessionStatus::Review;

    (context, pool)
}

fn execution_git_client(commit_failure: bool) -> MockGitClient {
    let mut git = MockGitClient::new();
    git.expect_detect_git_info()
        .returning(|_| Box::pin(async { Some("wt/sess".to_string()) }));
    git.expect_main_checkout_working_tree()
        .returning(|_| Box::pin(async { Ok(None) }));
    git.expect_diff()
        .returning(|_, _| Box::pin(async { Ok(String::new()) }));
    git.expect_is_worktree_clean().returning(move |_| {
        Box::pin(async move {
            if commit_failure {
                Err(ag_git::GitError::CommandFailed {
                    command: "git status".to_string(),
                    stderr: "fatal: Unable to create '/repo/.git/index.lock': File exists."
                        .to_string(),
                })
            } else {
                Ok(true)
            }
        })
    });

    git
}

fn request(attempt: &str) -> SubmitAgentTaskRequest {
    SubmitAgentTaskRequest {
        assignment: TaskAgentAssignment {
            agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
            definition: BuiltinAgentDefinition::Planner,
        },
        attempt_id: attempt.to_string(),
        input: TaskAgentInput {
            accepted_plan: None,
            clarification: None,
            user_request: "Plan this request".to_string(),
        },
        objective: "Propose a plan".to_string(),
        task_id: "planner".to_string(),
        visibility: DurableMessageVisibility::Visible,
    }
}
