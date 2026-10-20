use std::sync::Arc;
use std::time::Duration;

use ag_contracts::{MockAgentChannel, TurnResult};
use ag_protocol::AgentResponse;
use ag_session::{
    AcceptTaskChainPlanRequest, AcceptTaskPlanRequest, AgentKind, AgentModel, AgentSelection,
    ContinueTaskChainRequest, FocusedReviewStatus, PermissionMode, SessionError, SessionId,
    SessionService, SessionStatus, StartPlanThenImplementRequest, TaskAgentTurn, TaskChain,
    TaskChainPhase,
};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use crate::app::App;
use crate::app::review::ReviewCacheEntry;
use crate::app::test_support::TestSessionRunFactory;
use crate::infra::db::Database;
use crate::test_support::{
    SessionFixtureBuilder, new_git_test_app_with_pool, new_test_app, set_session_status_for_test,
};

#[tokio::test]
async fn service_coordinates_revision_explicit_acceptance_and_exactly_once_handoff() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    let (launch, revision) = fixture.revised_plan().await;
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
    fixture.assert_repository_errors_surface().await;
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
}

fn launch_request() -> StartPlanThenImplementRequest {
    StartPlanThenImplementRequest {
        agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
        chain_id: "workflow".into(),
        planner_attempt_id: "initial".into(),
        user_request: "Implement a scoped change".into(),
    }
}

struct ChainFixture {
    app: App,
    factory: Arc<TestSessionRunFactory>,
    pool: SqlitePool,
    service: SessionService,
    session_id: SessionId,
    _directory: TempDir,
}

impl ChainFixture {
    async fn new() -> Self {
        let (mut app, directory, pool) = new_git_test_app_with_pool().await;
        let session_id = SessionId::from(app.create_session().await.expect("session"));
        set_session_status_for_test(&mut app, &session_id, SessionStatus::Review);
        app.services
            .db()
            .sessions()
            .update_session_permission_mode(&session_id, PermissionMode::ReadOnly)
            .await
            .expect("read-only workflow fixture");
        let factory = TestSessionRunFactory::install(&mut app.services);
        let service = app.session_service();

        Self {
            app,
            factory,
            pool,
            service,
            session_id,
            _directory: directory,
        }
    }

    async fn revised_plan(&mut self) -> (StartPlanThenImplementRequest, ContinueTaskChainRequest) {
        let launch = launch_request();
        let planner_id = launch.planner_request().task_id;
        self.app
            .services
            .db()
            .sessions()
            .update_session_focused_review(
                &self.session_id,
                Some(FocusedReviewStatus::Pending),
                Some("existing-diff".into()),
                None,
            )
            .await
            .expect("pending ordinary review");
        self.app
            .deferred_auto_review_session_ids
            .insert(self.session_id.clone());
        let first = self.register_result(&planner_id, "First plan", false);
        self.app
            .drive_session_request(
                self.service
                    .start_plan_then_implement(&self.session_id, launch.clone()),
            )
            .await
            .expect("launch");
        assert!(matches!(
            self.app.review_cache.get(&self.session_id),
            Some(ReviewCacheEntry::Suppressed)
        ));
        assert!(
            !self
                .app
                .deferred_auto_review_session_ids
                .contains(&self.session_id)
        );
        assert_eq!(
            self.app
                .services
                .db()
                .sessions()
                .load_pending_focused_review_session_ids(self.app.active_project_id())
                .await
                .expect("startup review triggers"),
            Vec::<String>::new()
        );
        self.settle(&first).await;
        let revision = ContinueTaskChainRequest {
            attempt_id: "revision".to_string(),
            clarification: Some("Revise the scope".to_string()),
            expected_attempt_id: "initial".into(),
        };
        let second = self.register_result(&planner_id, "Exact revised plan", false);
        self.app
            .drive_session_request(
                self.service
                    .continue_task_chain("workflow", revision.clone()),
            )
            .await
            .expect("revise");
        self.settle(&second).await;
        let awaiting = self.snapshot().await;
        assert_eq!(awaiting.phase, TaskChainPhase::AwaitingPlanAcceptance);
        assert_eq!(awaiting.accepted_plan, None);

        (launch, revision)
    }

    fn register_result(&self, task_id: &str, answer: &str, blocked: bool) -> Script {
        let script = Script {
            entered: CancellationToken::new(),
            release: CancellationToken::new(),
        };
        if !blocked {
            script.release.cancel();
        }
        let entered = script.entered.clone();
        let release = script.release.clone();
        let response = AgentResponse::plain(answer);
        let mut channel = MockAgentChannel::new();
        channel.expect_run_turn().once().returning(move |_, _, _| {
            let entered = entered.clone();
            let release = release.clone();
            let response = response.clone();

            Box::pin(async move {
                entered.cancel();
                release.cancelled().await;
                Ok(TurnResult {
                    assistant_message: response,
                    context_reset: false,
                    harness_report: None,
                    input_tokens: 1,
                    output_tokens: 1,
                    provider_conversation_id: Some("task-thread".into()),
                })
            })
        });
        channel
            .expect_shutdown_session()
            .times(1..)
            .returning(|_| Box::pin(async { Ok(()) }));
        self.factory.register(task_id, Arc::new(channel));

        script
    }

    async fn settle(&mut self, script: &Script) {
        script.started().await;
        let request = self.snapshot().await.current_request;
        let operation_id = format!("task-attempt:{}", request.attempt_id);
        tokio::time::timeout(Duration::from_secs(10), async {
            while self
                .app
                .services
                .db()
                .operations()
                .is_session_operation_unfinished(&operation_id)
                .await
                .expect("operation state")
            {
                let event = self.app.next_app_event().await.expect("worker event");
                self.app.apply_app_events(event).await;
            }
        })
        .await
        .expect("settle guard");
        self.app.sessions.sync_session_from_handle(&self.session_id);
    }

    async fn snapshot(&mut self) -> TaskChain {
        self.app
            .drive_session_request(self.service.get_task_chain("workflow"))
            .await
            .expect("read workflow")
            .expect("workflow exists")
    }

    async fn assert_repository_errors_surface(&mut self) {
        // Arrange: snapshot reads succeed, but cancellation persistence fails.
        sqlx::query(
            "CREATE TRIGGER reject_workflow_cancel BEFORE UPDATE OF canceled_at ON task_chain \
             BEGIN SELECT RAISE(ABORT, 'cancellation unavailable'); END",
        )
        .execute(&self.pool)
        .await
        .expect("reject cancellation persistence");

        // Act / Assert
        let error = self
            .app
            .drive_session_request(self.service.cancel_task_chain("workflow"))
            .await
            .expect_err("surface failed cancellation");
        assert!(error.to_string().contains("cancellation unavailable"));
        sqlx::query("DROP TRIGGER reject_workflow_cancel")
            .execute(&self.pool)
            .await
            .expect("restore cancellation persistence");

        // Arrange: the current Implementer snapshot remains readable while a
        // historical Planner receipt cannot be decoded.
        sqlx::query(
            "UPDATE session_operation SET status = 'invalid' WHERE id = 'task-attempt:initial'",
        )
        .execute(&self.pool)
        .await
        .expect("corrupt historical receipt");

        // Act / Assert
        let error = self
            .app
            .drive_session_request(self.service.continue_task_chain(
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
        .execute(&self.pool)
        .await
        .expect("reject review suppression");
        let launch = StartPlanThenImplementRequest {
            chain_id: "rejected-review".into(),
            planner_attempt_id: "review-unavailable".into(),
            ..launch_request()
        };

        // Act
        let error = self
            .app
            .drive_session_request(
                self.service
                    .start_plan_then_implement(&self.session_id, launch.clone()),
            )
            .await
            .expect_err("surface suppression failure");
        let failed = self
            .app
            .drive_session_request(self.service.get_task_chain(&launch.chain_id))
            .await
            .expect("failed workflow read")
            .expect("durable workflow");
        let history = self
            .app
            .drive_session_request(
                self.service
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
}

struct Script {
    entered: CancellationToken,
    release: CancellationToken,
}

impl Script {
    async fn started(&self) {
        tokio::time::timeout(Duration::from_secs(10), self.entered.cancelled())
            .await
            .expect("execution start guard");
    }
}
