use ag_session::{
    AcceptTaskChainPlanRequest, AcceptTaskPlanRequest, BuiltinAgentDefinition,
    ContinueTaskChainRequest, FocusedReviewStatus, ReplyToTaskChainRequest, SessionError,
    SessionId, SessionMessageKind, SessionStatus, StartPlanThenImplementRequest, TaskAgentTurn,
    TaskChainPhase,
};
use tempfile::TempDir;

use super::test_support::{ChainFixture, agent_answer, launch_request};
use crate::app::App;
use crate::app::review::ReviewCacheEntry;
use crate::infra::db::Database;
use crate::test_support::{SessionFixtureBuilder, new_test_app, set_session_status_for_test};

#[tokio::test]
async fn service_coordinates_revision_explicit_acceptance_and_exactly_once_handoff() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    let (launch, revision) = revised_plan(&mut fixture).await;
    let planner_id = launch.planner_request().task_id;

    // Act: standalone acceptance records intent without starting
    // implementation.
    fixture
        .app
        .drive_session_request(fixture.service.accept_agent_plan(
            &fixture.session_id,
            AcceptTaskPlanRequest {
                task_id: planner_id,
                attempt_id: "revision".into(),
            },
        ))
        .await
        .expect("standalone acceptance");
    let implementation =
        fixture.register_result(&launch.implementer_task_id(), "Implemented", true);
    let acceptance = AcceptTaskChainPlanRequest {
        attempt_id: "implementation".to_string(),
        plan_attempt_id: "revision".to_string(),
    };
    fixture
        .app
        .drive_session_request(
            fixture
                .service
                .accept_task_chain_plan("workflow", acceptance.clone()),
        )
        .await
        .expect("accepted handoff");
    implementation.started().await;

    // Assert: old receipts remain idempotent while execution is busy.
    fixture
        .app
        .drive_session_request(
            fixture
                .service
                .start_plan_then_implement(&fixture.session_id, launch),
        )
        .await
        .expect("launch receipt");
    fixture
        .app
        .drive_session_request(fixture.service.continue_task_chain("workflow", revision))
        .await
        .expect("old revision receipt");
    fixture
        .app
        .drive_session_request(
            fixture
                .service
                .accept_task_chain_plan("workflow", acceptance),
        )
        .await
        .expect("handoff receipt");
    let running = fixture.snapshot().await;
    assert_eq!(running.phase, TaskChainPhase::Implementing);
    assert_eq!(
        running.accepted_plan.expect("accepted version").text,
        "Exact revised plan"
    );

    // Act
    implementation.release.cancel();
    fixture.settle(&implementation).await;
    let finished = fixture.snapshot().await;
    let listed = fixture
        .app
        .drive_session_request(fixture.service.list_task_chains(&fixture.session_id))
        .await
        .expect("list workflows");

    // Assert
    assert_eq!(finished.phase, TaskChainPhase::Succeeded);
    assert_eq!(listed, vec![finished]);
    let expected_transcript = vec![
        (
            SessionMessageKind::UserPrompt,
            "Implement a scoped change".to_string(),
        ),
        (SessionMessageKind::AgentGraph, "workflow".to_string()),
        agent_answer(BuiltinAgentDefinition::Planner, "First plan"),
        agent_answer(BuiltinAgentDefinition::Planner, "Exact revised plan"),
        agent_answer(BuiltinAgentDefinition::Implementer, "Implemented"),
    ];
    assert_eq!(fixture.persisted_transcript().await, expected_transcript);
    assert_eq!(fixture.live_transcript(), expected_transcript);
    assert!(
        fixture
            .app
            .drive_session_request(fixture.service.continue_task_chain(
                "workflow",
                ContinueTaskChainRequest {
                    attempt_id: "stale-planner-guidance".into(),
                    clarification: Some("Answering the old Planner".into()),
                    expected_attempt_id: "revision".into(),
                },
            ))
            .await
            .is_err()
    );
    assert_repository_errors_surface(&mut fixture).await;
}

#[tokio::test]
async fn service_cancels_and_retries_only_the_current_workflow_attempt() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    let launch = launch_request();
    let first = fixture.register_result(&launch.planner_request().task_id, "A plan", true);
    fixture
        .app
        .drive_session_request(
            fixture
                .service
                .start_plan_then_implement(&fixture.session_id, launch.clone()),
        )
        .await
        .expect("launch");
    first.started().await;

    // Act
    fixture
        .app
        .drive_session_request(fixture.service.cancel_task_chain("workflow"))
        .await
        .expect("cancel workflow");
    fixture.settle(&first).await;
    let canceled = fixture.snapshot().await;

    // Assert
    assert_eq!(canceled.phase, TaskChainPhase::Canceled);

    // Act
    let retry = fixture.register_result(&launch.planner_request().task_id, "Retried plan", false);
    fixture
        .app
        .drive_session_request(fixture.service.continue_task_chain(
            "workflow",
            ContinueTaskChainRequest {
                attempt_id: "retry".to_string(),
                clarification: None,
                expected_attempt_id: "initial".into(),
            },
        ))
        .await
        .expect("explicit retry");
    fixture.settle(&retry).await;

    // Assert
    assert_eq!(
        fixture.snapshot().await.phase,
        TaskChainPhase::AwaitingPlanAcceptance
    );
    assert_eq!(
        fixture
            .app
            .drive_session_request(fixture.service.get_task_chain("missing"))
            .await
            .expect("missing workflow"),
        None
    );
}

#[tokio::test]
async fn service_reports_missing_busy_and_invalid_workflows_without_dispatch() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    let launch = launch_request();

    // Act / Assert
    let missing = fixture
        .app
        .drive_session_request(
            fixture
                .service
                .start_plan_then_implement(&SessionId::from("missing"), launch.clone()),
        )
        .await
        .expect_err("missing session");
    assert_eq!(missing, SessionError::NotFound);
    set_session_status_for_test(
        &mut fixture.app,
        &fixture.session_id,
        SessionStatus::Question,
    );
    assert!(
        fixture
            .app
            .drive_session_request(
                fixture
                    .service
                    .start_plan_then_implement(&fixture.session_id, launch.clone(),)
            )
            .await
            .is_err()
    );
    assert_eq!(
        fixture
            .app
            .drive_session_request(fixture.service.accept_task_chain_plan(
                "missing",
                AcceptTaskChainPlanRequest {
                    attempt_id: "impl".into(),
                    plan_attempt_id: "plan".into()
                },
            ))
            .await,
        Err(SessionError::NotFound)
    );
    assert_eq!(
        fixture
            .app
            .drive_session_request(fixture.service.continue_task_chain(
                "missing",
                ContinueTaskChainRequest {
                    attempt_id: "retry".into(),
                    clarification: None,
                    expected_attempt_id: "missing".into(),
                },
            ))
            .await,
        Err(SessionError::NotFound)
    );
    assert_eq!(
        fixture
            .app
            .drive_session_request(fixture.service.cancel_task_chain("missing"))
            .await,
        Err(SessionError::NotFound)
    );
    assert!(
        fixture
            .app
            .drive_session_request(fixture.service.accept_agent_plan(
                &fixture.session_id,
                AcceptTaskPlanRequest {
                    task_id: "missing".into(),
                    attempt_id: "missing".into()
                },
            ))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn enqueue_failure_is_durable_and_requires_an_explicit_new_attempt() {
    // Arrange: a persisted session with no live handles cannot own a worker.
    let (mut app, _directory) = new_test_app().await;
    let session_id = SessionId::from("unbound");
    app.services
        .db()
        .sessions()
        .insert_session(
            &session_id,
            "gpt-6.1-sol",
            "main",
            "Review",
            app.active_project_id(),
        )
        .await
        .expect("persisted owner");
    app.sessions.push_session(
        SessionFixtureBuilder::new()
            .id(session_id.clone())
            .status(SessionStatus::Review)
            .build(),
    );
    let service = app.session_service();
    let launch = launch_request();

    // Act
    let failure = app
        .drive_session_request(service.start_plan_then_implement(&session_id, launch.clone()))
        .await;
    let failed = app
        .drive_session_request(service.get_task_chain("workflow"))
        .await
        .expect("read failure")
        .expect("durable workflow");
    let receipt = app
        .drive_session_request(service.start_plan_then_implement(&session_id, launch.clone()))
        .await;
    let conflict = app
        .drive_session_request(service.start_plan_then_implement(
            &session_id,
            StartPlanThenImplementRequest {
                user_request: "Conflicting launch".into(),
                ..launch
            },
        ))
        .await;

    // Assert
    assert!(failure.is_err());
    assert!(receipt.is_ok());
    assert!(conflict.is_err());
    assert_eq!(failed.phase, TaskChainPhase::Failed);
    assert!(
        app.dispatch_task_chain("workflow", "different-delivery")
            .await
            .expect_err("cannot dispatch a different attempt")
            .to_string()
            .contains("changed before delivery")
    );

    // Act / Assert: neither version acceptance nor retry hides admission
    // errors.
    assert!(
        app.drive_session_request(service.accept_task_chain_plan(
            "workflow",
            AcceptTaskChainPlanRequest {
                attempt_id: "impl".into(),
                plan_attempt_id: "initial".into(),
            }
        ))
        .await
        .is_err()
    );
    assert!(
        app.drive_session_request(service.continue_task_chain(
            "workflow",
            ContinueTaskChainRequest {
                attempt_id: "explicit-retry".into(),
                clarification: None,
                expected_attempt_id: "initial".into(),
            }
        ))
        .await
        .is_err()
    );
}

#[tokio::test]
async fn workflow_queries_and_admission_surface_database_failures() {
    // Arrange
    let directory = TempDir::new().expect("app directory");
    let database = Database::in_memory().await.expect("database");
    let mut app = App::new_with_clients(
        directory.path().into(),
        directory.path().into(),
        None,
        database.clone(),
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("app");
    let service = app.session_service();
    database.pool().close().await;

    // Act / Assert
    assert!(
        app.dispatch_task_chain("workflow", "initial")
            .await
            .is_err()
    );
    assert!(
        app.drive_session_request(service.get_task_chain("workflow"))
            .await
            .is_err()
    );
    assert!(
        app.drive_session_request(service.list_task_chains(&SessionId::from("missing")))
            .await
            .is_err()
    );
    assert!(
        app.drive_session_request(
            service.start_plan_then_implement(&SessionId::from("missing"), launch_request())
        )
        .await
        .is_err()
    );
    assert!(matches!(
        app.drive_session_request(service.reply_to_task_chain(
            &SessionId::from("missing"),
            ReplyToTaskChainRequest {
                message: "Go ahead".into(),
                reply_id: "reply".into(),
            },
        ))
        .await,
        Err(SessionError::Operation(_))
    ));
}

async fn revised_plan(
    fixture: &mut ChainFixture,
) -> (StartPlanThenImplementRequest, ContinueTaskChainRequest) {
    let launch = launch_request();
    let planner_id = launch.planner_request().task_id;
    fixture
        .app
        .services
        .db()
        .sessions()
        .update_session_focused_review(
            &fixture.session_id,
            Some(FocusedReviewStatus::Pending),
            Some("existing-diff".into()),
            None,
        )
        .await
        .expect("pending ordinary review");
    fixture
        .app
        .deferred_auto_review_session_ids
        .insert(fixture.session_id.clone());
    let first = fixture.register_result(&planner_id, "First plan", false);
    fixture
        .app
        .drive_session_request(
            fixture
                .service
                .start_plan_then_implement(&fixture.session_id, launch.clone()),
        )
        .await
        .expect("launch");
    assert!(matches!(
        fixture.app.review_cache.get(&fixture.session_id),
        Some(ReviewCacheEntry::Suppressed)
    ));
    assert!(
        !fixture
            .app
            .deferred_auto_review_session_ids
            .contains(&fixture.session_id)
    );
    assert_eq!(
        fixture
            .app
            .services
            .db()
            .sessions()
            .load_pending_focused_review_session_ids(fixture.app.active_project_id())
            .await
            .expect("startup review triggers"),
        Vec::<String>::new()
    );
    fixture.settle(&first).await;
    let revision = ContinueTaskChainRequest {
        attempt_id: "revision".to_string(),
        clarification: Some("Revise the scope".to_string()),
        expected_attempt_id: "initial".into(),
    };
    let second = fixture.register_result(&planner_id, "Exact revised plan", false);
    fixture
        .app
        .drive_session_request(
            fixture
                .service
                .continue_task_chain("workflow", revision.clone()),
        )
        .await
        .expect("revise");
    fixture.settle(&second).await;
    let awaiting = fixture.snapshot().await;
    assert_eq!(awaiting.phase, TaskChainPhase::AwaitingPlanAcceptance);
    assert_eq!(awaiting.accepted_plan, None);

    (launch, revision)
}

async fn assert_repository_errors_surface(fixture: &mut ChainFixture) {
    // Arrange: snapshot reads succeed, but cancellation persistence fails.
    sqlx::query(
        "CREATE TRIGGER reject_workflow_cancel BEFORE UPDATE OF canceled_at ON task_chain BEGIN \
         SELECT RAISE(ABORT, 'cancellation unavailable'); END",
    )
    .execute(&fixture.pool)
    .await
    .expect("reject cancellation persistence");

    // Act / Assert
    let error = fixture
        .app
        .drive_session_request(fixture.service.cancel_task_chain("workflow"))
        .await
        .expect_err("surface failed cancellation");
    assert!(error.to_string().contains("cancellation unavailable"));
    sqlx::query("DROP TRIGGER reject_workflow_cancel")
        .execute(&fixture.pool)
        .await
        .expect("restore cancellation persistence");

    // Arrange: the current Implementer snapshot remains readable while a
    // historical Planner receipt cannot be decoded.
    sqlx::query(
        "UPDATE session_operation SET status = 'invalid' WHERE id = 'task-attempt:initial'",
    )
    .execute(&fixture.pool)
    .await
    .expect("corrupt historical receipt");

    // Act / Assert
    let error = fixture
        .app
        .drive_session_request(fixture.service.continue_task_chain(
            "workflow",
            ContinueTaskChainRequest {
                attempt_id: "unreadable-receipt".into(),
                clarification: None,
                expected_attempt_id: "implementation".into(),
            },
        ))
        .await
        .expect_err("surface failed receipt read");
    assert!(error.to_string().contains("invalid"));

    // Arrange: a newly admitted workflow cannot clear the durable ordinary
    // review trigger. Its attempt must fail before worker dispatch.
    sqlx::query(
        "CREATE TRIGGER reject_review_suppression BEFORE UPDATE OF focused_review_status ON \
         session BEGIN SELECT RAISE(ABORT, 'review suppression unavailable'); END",
    )
    .execute(&fixture.pool)
    .await
    .expect("reject review suppression");
    let launch = StartPlanThenImplementRequest {
        chain_id: "rejected-review".into(),
        planner_attempt_id: "review-unavailable".into(),
        ..launch_request()
    };

    // Act
    let error = fixture
        .app
        .drive_session_request(
            fixture
                .service
                .start_plan_then_implement(&fixture.session_id, launch.clone()),
        )
        .await
        .expect_err("surface suppression failure");
    let failed = fixture
        .app
        .drive_session_request(fixture.service.get_task_chain(&launch.chain_id))
        .await
        .expect("failed workflow read")
        .expect("durable workflow");
    let history = fixture
        .app
        .drive_session_request(
            fixture
                .service
                .get_agent_task_history(&launch.planner_request().task_id),
        )
        .await
        .expect("undispatched history")
        .expect("task context");

    // Assert
    assert!(error.to_string().contains("review suppression unavailable"));
    assert_eq!(failed.phase, TaskChainPhase::Failed);
    assert_eq!(history.turns, Vec::<TaskAgentTurn>::new());
}
