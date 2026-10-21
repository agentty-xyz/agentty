use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ag_contracts::SessionDiffState;
use ag_git::MockGitClient;
use ag_session::{
    AgentKind, AgentModel, AgentSelection, BuiltinAgentDefinition, DurableMessageRequest,
    DurableMessageVisibility, PermissionMode, SessionId, SubmitAgentTaskRequest, SubmitTaskRequest,
    TaskAgentAssignment, TaskAgentInput, TaskAttemptStatus,
};
use ag_worker::MockRunClient;
use tokio::sync::Notify;
use uuid::Uuid;

use crate::app::review::ReviewCacheEntry;
use crate::app::session::TurnAppliedState;
use crate::app::test_support::TestSessionRunFactory;
use crate::app::{App, AppEvent};
use crate::domain::review::FocusedReviewStatus;
use crate::domain::session::{SessionHandles, SessionStats, Status};
use crate::test_support::{
    SessionFixtureBuilder, new_git_test_app_with_pool, new_test_app, set_session_status_for_test,
    test_app_clients,
};

/// Role attempt that owns review suppression in these scenarios.
const ROLE_ATTEMPT: &str = "role-attempt";

#[tokio::test]
async fn implementer_success_starts_one_ordinary_review_after_review_ready_status() {
    // Arrange
    let (mut app, _directory, _) = new_git_test_app_with_pool().await;
    let session_id = SessionId::from(app.create_session().await.expect("session"));
    app.sessions.sessions_mut()[0].stats.diff_state = SessionDiffState::Present;
    app.sessions.sessions_mut()[0].stats.added_lines = 1;
    app.auto_address_review_iterations
        .insert(session_id.clone(), 3);
    app.suppress_agent_task_automation(&session_id, ROLE_ATTEMPT)
        .await
        .expect("suppress");
    set_session_status_for_test(&mut app, &session_id, Status::InProgress);
    let (diff_calls, diff_started) = install_diff_probe(&mut app);

    // Act: the attempt succeeds while its post-turn effects keep it running.
    app.apply_app_events(implementer_success(&session_id, ROLE_ATTEMPT))
        .await;
    let calls_while_running = diff_calls.load(Ordering::Relaxed);
    *app.sessions.session_handles()[&session_id]
        .status
        .lock()
        .expect("status") = Status::Review;
    app.apply_app_events(AppEvent::SessionUpdated {
        session_id: session_id.clone(),
        version: 1,
    })
    .await;
    tokio::time::timeout(Duration::from_secs(1), diff_started.notified())
        .await
        .expect("implementer review starts");
    app.apply_app_events(implementer_success(&session_id, ROLE_ATTEMPT))
        .await;
    tokio::task::yield_now().await;

    // Assert
    assert_eq!(calls_while_running, 0);
    assert!(!app.role_review_suppression.contains_key(&session_id));
    assert!(!app.auto_address_review_iterations.contains_key(&session_id));
    assert_eq!(diff_calls.load(Ordering::Relaxed), 1);
    assert_eq!(app.pending_session_diff_requests.len(), 1);
}

#[tokio::test]
async fn stale_implementer_success_keeps_current_role_review_suppressed() {
    // Arrange
    let (mut app, _directory, _) = new_git_test_app_with_pool().await;
    let session_id = SessionId::from(app.create_session().await.expect("session"));
    app.sessions.sessions_mut()[0].stats.diff_state = SessionDiffState::Present;
    app.sessions.sessions_mut()[0].stats.added_lines = 1;
    app.suppress_agent_task_automation(&session_id, "implementer-attempt")
        .await
        .expect("first implementer");
    app.suppress_agent_task_automation(&session_id, ROLE_ATTEMPT)
        .await
        .expect("current role attempt");
    set_session_status_for_test(&mut app, &session_id, Status::InProgress);
    let (diff_calls, _) = install_diff_probe(&mut app);

    // Act: an earlier attempt's success and one for another session arrive
    // before the current attempt reaches review-ready status.
    app.apply_app_events(implementer_success(&session_id, "implementer-attempt"))
        .await;
    app.apply_app_events(implementer_success(
        &SessionId::from("other-session"),
        ROLE_ATTEMPT,
    ))
    .await;
    *app.sessions.session_handles()[&session_id]
        .status
        .lock()
        .expect("status") = Status::Review;
    app.apply_app_events(AppEvent::SessionUpdated {
        session_id: session_id.clone(),
        version: 1,
    })
    .await;
    tokio::task::yield_now().await;

    // Assert
    assert_eq!(
        app.role_review_suppression
            .get(&session_id)
            .map(String::as_str),
        Some(ROLE_ATTEMPT)
    );
    assert!(matches!(
        app.review_cache.get(&session_id),
        Some(ReviewCacheEntry::Suppressed)
    ));
    assert_eq!(diff_calls.load(Ordering::Relaxed), 0);
    assert_eq!(app.pending_session_diff_requests.len(), 0);
}

fn implementer_success(session_id: &SessionId, attempt_id: &str) -> AppEvent {
    AppEvent::ImplementerAttemptSucceeded {
        attempt_id: attempt_id.to_string(),
        session_id: session_id.clone(),
    }
}

#[tokio::test]
async fn role_completion_and_late_review_results_cannot_start_ordinary_automation() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let (session_id, old_review) = seed_pending_review(&mut app).await;
    let (diff_calls, diff_started) = install_diff_probe(&mut app);
    app.services.set_run_client(Arc::new(MockRunClient::new()));
    app.services
        .set_review_request_client(Arc::new(ag_forge::MockReviewRequestClient::new()));

    // Act: invalidate an older auto-address review before role dispatch.
    app.suppress_agent_task_automation(&session_id, ROLE_ATTEMPT)
        .await
        .expect("suppress");
    app.sessions.sessions_mut()[0].status = Status::InProgress;
    app.start_auto_review_diff_loads(&std::collections::HashSet::from([session_id.clone()]));
    *app.sessions.session_handles()[&session_id]
        .status
        .lock()
        .expect("status") = Status::Review;
    app.apply_app_events(AppEvent::SessionUpdated {
        session_id: session_id.clone(),
        version: 1,
    })
    .await;
    app.apply_app_events(AppEvent::ReviewPrepared {
        session_id: session_id.clone(),
        request_id: old_review,
        diff_hash: 42,
        review_text: "## Review\n### Suggestions\n- Fix the existing issue.".to_string(),
    })
    .await;
    tokio::task::yield_now().await;

    // Assert: the actual reducer starts no diff, utility, or mutating turn.
    assert_eq!(diff_calls.load(Ordering::Relaxed), 0);
    assert_eq!(app.pending_session_diff_requests.len(), 0);
    assert!(matches!(
        app.review_cache.get(&session_id),
        Some(ReviewCacheEntry::Suppressed)
    ));
    assert!(!app.auto_address_review_iterations.contains_key(&session_id));
    assert!(
        !app.services
            .db()
            .sessions()
            .update_session_focused_review_for_generation(
                &session_id,
                &old_review.to_string(),
                FocusedReviewStatus::Ready,
                Some("42".to_string()),
                Some("Late result".to_string())
            )
            .await
            .expect("fenced writer")
    );

    // Act / Assert: an older ordinary completion cannot release role ownership.
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: session_id.clone(),
        turn_applied_state: TurnAppliedState {
            follow_up_tasks: Vec::new(),
            questions: Vec::new(),
            token_usage_delta: SessionStats::default(),
        },
    })
    .await;
    assert_eq!(diff_calls.load(Ordering::Relaxed), 0);
    app.clear_review_output(&session_id);
    app.start_auto_review_diff_loads(&std::collections::HashSet::from([session_id.clone()]));
    assert!(!app.start_auto_apply_review_diff_load(&session_id, 42, "Old suggestions".to_string()));
    assert!(!app.start_inactive_auto_review_diff_load(&session_id).await);
    assert_eq!(diff_calls.load(Ordering::Relaxed), 0);

    // Act / Assert: explicit manual review releases the ownership barrier.
    assert!(app.start_manual_review_diff_load(&session_id));
    tokio::time::timeout(Duration::from_secs(1), diff_started.notified())
        .await
        .expect("manual review starts");
    assert!(!app.role_review_suppression.contains_key(&session_id));
    assert_eq!(diff_calls.load(Ordering::Relaxed), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_role_receipt_does_not_dispatch_or_release_automation() {
    // Arrange: pause the workspace boundary after the initial receipt lookup.
    let (mut app, _directory, _) = new_git_test_app_with_pool().await;
    let session_id = SessionId::from(app.create_session().await.expect("session"));
    app.sessions.sessions_mut()[0].is_draft = true;
    let db = app.services.db().clone();
    let concurrent_session = session_id.clone();
    let reached_workspace = Arc::new(Notify::new());
    let notify = Arc::clone(&reached_workspace);
    let (settled_tx, settled_rx) = std::sync::mpsc::channel();
    let concurrent_claim = tokio::spawn(async move {
        reached_workspace.notified().await;
        let claimed = db
            .tasks()
            .claim_agent_attempt(&concurrent_session, &planner_request(), "agent_task")
            .await
            .expect("concurrent claim");
        settled_tx.send(claimed).expect("claim receipt");
    });
    let mut filesystem = crate::infra::fs::MockFsClient::new();
    filesystem.expect_is_dir().times(1).returning(move |_| {
        notify.notify_one();
        assert!(
            settled_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("concurrent admission")
        );
        true
    });
    app.services.set_fs_client(Arc::new(filesystem));
    let service = app.session_service();

    // Act
    app.drive_session_request(service.submit_agent_task(&session_id, planner_request()))
        .await
        .expect("same receipt");
    concurrent_claim.await.expect("claim task");

    // Assert: the second claimant neither dispatches nor changes review
    // ownership.
    assert!(!app.role_review_suppression.contains_key(&session_id));
    assert!(!app.review_cache.contains_key(&session_id));
    assert_eq!(
        app.services
            .db()
            .tasks()
            .load_agent_history("automation-task")
            .await
            .expect("history")
            .expect("context")
            .turns
            .len(),
        0
    );
}

#[tokio::test]
async fn new_ordinary_intent_restores_review_after_role_ownership() {
    for durable in [false, true] {
        // Arrange
        let (mut app, _directory, _) = new_git_test_app_with_pool().await;
        let session_id = SessionId::from(app.create_session().await.expect("session"));
        set_session_status_for_test(&mut app, &session_id, Status::Review);
        app.sessions.sessions_mut()[0].stats.diff_state = SessionDiffState::Present;
        app.sessions.sessions_mut()[0].stats.added_lines = 1;
        app.suppress_agent_task_automation(&session_id, ROLE_ATTEMPT)
            .await
            .expect("suppress");
        let started = install_pending_ordinary_turn(&mut app, &session_id);
        let service = app.session_service();

        // Act
        if durable {
            app.drive_session_request(
                service.submit_durable_message(&session_id, ordinary_request("new-intent")),
            )
            .await
            .expect("new admission");
        } else {
            assert!(app.reply(&session_id, "New ordinary intent").await);
        }
        tokio::time::timeout(Duration::from_secs(1), started.notified())
            .await
            .expect("ordinary worker starts");
        let (diff_calls, diff_started) = install_diff_probe(&mut app);
        set_session_status_for_test(&mut app, &session_id, Status::Review);
        app.apply_app_events(AppEvent::AgentResponseReceived {
            session_id: session_id.clone(),
            turn_applied_state: TurnAppliedState {
                follow_up_tasks: Vec::new(),
                questions: Vec::new(),
                token_usage_delta: SessionStats::default(),
            },
        })
        .await;

        // Assert
        assert!(!app.role_review_suppression.contains_key(&session_id));
        tokio::time::timeout(Duration::from_secs(1), diff_started.notified())
            .await
            .expect("ordinary review resumes");
        assert_eq!(diff_calls.load(Ordering::Relaxed), 1);
    }
}

#[tokio::test]
async fn old_ordinary_receipts_and_failed_admission_retain_role_ownership() {
    // Arrange
    let (mut app, _directory, _) = new_git_test_app_with_pool().await;
    let session_id = SessionId::from(app.create_session().await.expect("session"));
    set_session_status_for_test(&mut app, &session_id, Status::Review);
    app.services
        .db()
        .operations()
        .insert_session_operation("old-message", &session_id, "reply")
        .await
        .expect("old operation");
    app.services
        .db()
        .operations()
        .mark_session_operation_done("old-message")
        .await
        .expect("settled receipt");
    app.suppress_agent_task_automation(&session_id, ROLE_ATTEMPT)
        .await
        .expect("suppress");
    let service = app.session_service();

    // Act / Assert: a receipt does not represent new user intent.
    app.drive_session_request(
        service.submit_durable_message(&session_id, ordinary_request("old-message")),
    )
    .await
    .expect("old receipt");
    assert!(app.role_review_suppression.contains_key(&session_id));

    // Act / Assert: generic output clearing during a failed reply cannot
    // release it.
    app.sessions.session_handles_mut().remove(&session_id);
    assert!(!app.reply(&session_id, "Unavailable worker").await);
    assert!(app.role_review_suppression.contains_key(&session_id));
    app.discard_deleted_session_diff_state(&session_id);
    assert!(!app.role_review_suppression.contains_key(&session_id));
}

#[tokio::test]
async fn competing_ordinary_task_receipt_retains_role_review_ownership() {
    // Arrange: a different claimant wins after the preliminary receipt read.
    let (mut app, _directory, _) = new_git_test_app_with_pool().await;
    let session_id = SessionId::from(app.create_session().await.expect("session"));
    set_session_status_for_test(&mut app, &session_id, Status::Review);
    app.suppress_agent_task_automation(&session_id, ROLE_ATTEMPT)
        .await
        .expect("suppress");
    let request = SubmitTaskRequest {
        attempt_id: "competing-attempt".to_string(),
        message: "Ordinary task intent".to_string(),
        objective: "Finish existing work".to_string(),
        task_id: "ordinary-task".to_string(),
        visibility: DurableMessageVisibility::Hidden,
    };
    let mut tasks = ag_store::MockTaskRepository::new();
    let expected_session = session_id.clone();
    let expected_request = request.clone();
    tasks
        .expect_contains_attempt()
        .times(1)
        .withf(move |session, request| {
            session == expected_session.as_str() && request == &expected_request
        })
        .returning(|_, _| Ok(false));
    let expected_session = session_id.clone();
    let expected_request = request.clone();
    tasks
        .expect_claim_attempt()
        .times(1)
        .withf(move |session, request, kind| {
            session == expected_session.as_str() && request == &expected_request && kind == "reply"
        })
        .returning(|_, _, _| Ok(false));
    app.services.set_task_repository(Arc::new(tasks));
    let service = app.session_service();

    // Act
    app.drive_session_request(service.submit_task(&session_id, request))
        .await
        .expect("competing receipt");

    // Assert: no new worker turn can release role ownership or start a model.
    assert!(app.role_review_suppression.contains_key(&session_id));
    assert!(matches!(
        app.review_cache.get(&session_id),
        Some(ReviewCacheEntry::Suppressed)
    ));
}

fn install_pending_ordinary_turn(app: &mut App, session_id: &SessionId) -> Arc<Notify> {
    let started = Arc::new(Notify::new());
    let signal = Arc::clone(&started);
    let mut channel = ag_contracts::MockAgentChannel::new();
    channel.expect_start_session().returning(|request| {
        Box::pin(async move {
            Ok(ag_contracts::SessionRef {
                session_id: request.session_id,
            })
        })
    });
    channel.expect_run_turn().returning(move |_, _, _| {
        signal.notify_one();
        Box::pin(std::future::pending())
    });
    channel
        .expect_shutdown_session()
        .returning(|_| Box::pin(async { Ok(()) }));
    TestSessionRunFactory::install(&mut app.services).register(session_id, Arc::new(channel));

    started
}

fn install_diff_probe(app: &mut App) -> (Arc<AtomicUsize>, Arc<Notify>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(Notify::new());
    let count = Arc::clone(&calls);
    let signal = Arc::clone(&started);
    let mut git = MockGitClient::new();
    git.expect_diff().returning(move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        signal.notify_one();
        Box::pin(std::future::pending())
    });
    app.services.set_git_client(Arc::new(git));

    (calls, started)
}

fn ordinary_request(operation_id: &str) -> DurableMessageRequest {
    DurableMessageRequest {
        operation_id: operation_id.to_string(),
        message: "New ordinary intent".to_string(),
        visibility: DurableMessageVisibility::Hidden,
    }
}

#[tokio::test]
async fn role_review_invalidation_removes_the_restart_trigger() {
    // Arrange
    let (mut app, directory) = new_test_app().await;
    let (session_id, _) = seed_pending_review(&mut app).await;
    let project = app.active_project_id();
    let db = app.services.db().clone();
    app.suppress_agent_task_automation(&session_id, ROLE_ATTEMPT)
        .await
        .expect("suppress");
    assert_eq!(
        db.sessions()
            .load_pending_focused_review_session_ids(project)
            .await
            .expect("pending")
            .len(),
        0
    );
    drop(app);

    // Act
    let base = directory.path().to_path_buf();
    let reopened = App::new_with_clients(base.clone(), base, None, db, test_app_clients())
        .await
        .expect("reopen");

    // Assert
    assert_eq!(reopened.pending_session_diff_requests.len(), 0);
    assert!(!reopened.review_cache.contains_key(&session_id));
    assert!(
        !reopened
            .deferred_auto_review_session_ids
            .contains(&session_id)
    );
}

#[tokio::test]
async fn failed_automation_invalidation_settles_the_claim_without_model_execution() {
    for reject_failure_record in [false, true] {
        // Arrange
        let (mut app, _directory, pool) = new_git_test_app_with_pool().await;
        let session_id = SessionId::from(app.create_session().await.expect("session"));
        sqlx::query(
            "CREATE TRIGGER reject_review_clear BEFORE UPDATE OF focused_review_status ON session \
             WHEN NEW.focused_review_status IS NULL BEGIN SELECT RAISE(ABORT,'review invalidation \
             failed'); END",
        )
        .execute(&pool)
        .await
        .expect("failure injection");
        if reject_failure_record {
            sqlx::query(
                "CREATE TRIGGER reject_failure_record BEFORE UPDATE OF status ON \
                 session_operation WHEN NEW.status='failed' BEGIN SELECT RAISE(ABORT,'failure \
                 recording unavailable'); END",
            )
            .execute(&pool)
            .await
            .expect("failure injection");
        }
        let service = app.session_service();

        // Act
        let result = app
            .drive_session_request(service.submit_agent_task(&session_id, planner_request()))
            .await;

        // Assert
        assert!(result.is_err());
        let task = app
            .services
            .db()
            .tasks()
            .load_task("automation-task")
            .await
            .expect("task")
            .expect("claimed");
        assert_eq!(
            task.attempts[0].status,
            if reject_failure_record {
                TaskAttemptStatus::Queued
            } else {
                TaskAttemptStatus::Failed
            }
        );
        assert_eq!(
            app.services
                .db()
                .tasks()
                .load_agent_history("automation-task")
                .await
                .expect("history")
                .expect("context")
                .turns
                .len(),
            0
        );
    }
}

async fn seed_pending_review(app: &mut App) -> (SessionId, Uuid) {
    let session_id = SessionId::from("role-session");
    let request_id = Uuid::new_v4();
    let db = app.services.db();
    db.sessions()
        .insert_session(
            &session_id,
            "gpt-6.1-sol",
            "main",
            "Review",
            app.active_project_id(),
        )
        .await
        .expect("session");
    db.sessions()
        .update_session_permission_mode(&session_id, PermissionMode::AutoEditAddressComments)
        .await
        .expect("permission");
    db.sessions()
        .defer_session_focused_review(&session_id)
        .await
        .expect("pending review");
    db.sessions()
        .begin_review_generation(&session_id, "prior-generation", &request_id.to_string())
        .await
        .expect("generation");
    let mut session = SessionFixtureBuilder::new()
        .id(session_id.clone())
        .status(Status::AgentReview)
        .build();
    session.permission_mode = PermissionMode::AutoEditAddressComments;
    session.stats.added_lines = 1;
    session.stats.diff_state = SessionDiffState::Present;
    app.sessions.push_session(session);
    app.sessions
        .session_handles_mut()
        .insert(session_id.clone(), SessionHandles::new(Status::AgentReview));
    let review_agent = app.review_agent();
    app.review_cache.insert(
        session_id.clone(),
        ReviewCacheEntry::Loading {
            request_id,
            diff_hash: 42,
            review_agent,
            progress: None,
        },
    );
    app.deferred_auto_review_session_ids
        .insert(session_id.clone());

    (session_id, request_id)
}

fn planner_request() -> SubmitAgentTaskRequest {
    SubmitAgentTaskRequest {
        assignment: TaskAgentAssignment {
            agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
            definition: BuiltinAgentDefinition::Planner,
        },
        attempt_id: "automation-attempt".to_string(),
        input: TaskAgentInput {
            accepted_plan: None,
            clarification: None,
            user_request: "Plan without ordinary follow-ups".to_string(),
        },
        objective: "Produce a plan".to_string(),
        task_id: "automation-task".to_string(),
        visibility: DurableMessageVisibility::Hidden,
    }
}
