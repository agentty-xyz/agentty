use std::sync::Arc;

use ag_protocol::TurnPromptAttachment;
use ag_session::{
    AcceptTaskChainPlanRequest, AcceptTaskPlanRequest, AgentDefinition, AgentSelection,
    ContinueTaskChainRequest, FocusedReviewStatus, ReplyToTaskChainRequest, SessionError,
    SessionId, SessionMessageKind, SessionStatus, StartPlanThenImplementRequest, TaskAgentRole,
    TaskChainPhase,
};
use ag_store::MockGraphRepository;
use ag_worker::MockRunClient;
use mockall::predicate::eq;
use tempfile::TempDir;
use uuid::Uuid;

use super::test_support::{ChainFixture, Script, agent_answer, graph_unavailable, launch_request};
use crate::app::App;
use crate::app::review::ReviewCacheEntry;
use crate::infra::agent_profile::MockAgentCatalogClient;
use crate::infra::db::Database;
use crate::infra::fs::MockFsClient;
use crate::test_support::{SessionFixtureBuilder, new_test_app, set_session_status_for_test};

#[tokio::test]
async fn graph_turns_preserve_launch_history_captured_settings_and_revision_images() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    append_conversation(
        &fixture,
        &[
            (SessionMessageKind::UserPrompt, "Recommend the next step"),
            (
                SessionMessageKind::AssistantAnswer,
                "Build a graph with joins",
            ),
            (SessionMessageKind::WorkflowNotice, "Internal notice"),
        ],
    )
    .await;
    let mut launch = launch_request();
    launch.attachments.push(TurnPromptAttachment {
        local_image_path: "initial.png".into(),
        placeholder: "[Image #1]".into(),
    });
    let plan = fixture.register_turn("Initial plan", false);
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
    let source = fixture
        .snapshot()
        .await
        .current_attempt_id()
        .expect("plan")
        .to_string();
    append_conversation(
        &fixture,
        &[(
            SessionMessageKind::AssistantAnswer,
            "Unrelated later conversation",
        )],
    )
    .await;
    let revised = fixture.register_turn("Plan with tests", false);

    // Act
    fixture
        .app
        .drive_session_request(fixture.service.continue_task_chain(
            "workflow",
            ContinueTaskChainRequest {
                attachments: vec![TurnPromptAttachment {
                    local_image_path: "revision.png".into(),
                    placeholder: "[Image #1]".into(),
                }],
                attempt_id: Uuid::new_v4().to_string(),
                clarification: Some("Use this sketch".into()),
                expected_attempt_id: source,
            },
        ))
        .await
        .expect("revise");
    fixture.settle(&revised).await;
    let accepted = fixture.snapshot().await;
    let implementation = fixture.register_turn("Implemented", false);
    fixture
        .app
        .drive_session_request(fixture.service.accept_task_chain_plan(
            "workflow",
            AcceptTaskChainPlanRequest {
                plan_attempt_id: accepted.current_attempt_id().expect("revised plan").into(),
            },
        ))
        .await
        .expect("accept");
    fixture.settle(&implementation).await;

    // Assert
    assert!(plan.prompt().contains("Recommend the next step"));
    assert!(revised.prompt().contains("Build a graph with joins"));
    assert!(!revised.prompt().contains("Unrelated later conversation"));
    assert!(!plan.prompt().contains("Internal notice"));
    assert!(!implementation.prompt().contains("Build a graph with joins"));
    assert!(implementation.prompt().contains("Plan with tests"));
    for (script, definition, paths) in [
        (&plan, &accepted.agents.planner, vec!["initial.png"]),
        (
            &revised,
            &accepted.agents.planner,
            vec!["initial.png", "revision.png"],
        ),
        (
            &implementation,
            &accepted.agents.implementer,
            vec!["initial.png", "revision.png"],
        ),
    ] {
        assert_role_request(script, launch.agent, definition, &paths);
    }
}

#[tokio::test]
async fn service_coordinates_revision_explicit_acceptance_and_exactly_once_handoff() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    let (launch, first_plan, revision) = revised_plan(&mut fixture).await;
    let awaiting = fixture.snapshot().await;
    let revised_attempt = awaiting
        .current_attempt_id()
        .expect("revised plan attempt")
        .to_string();

    // Act: a superseded plan version cannot be accepted.
    let stale = fixture
        .app
        .drive_session_request(fixture.service.accept_task_chain_plan(
            "workflow",
            AcceptTaskChainPlanRequest {
                plan_attempt_id: first_plan,
            },
        ))
        .await;
    let implementation = fixture.register_turn("Implemented", true);
    let acceptance = AcceptTaskChainPlanRequest {
        plan_attempt_id: revised_attempt.clone(),
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
    fixture
        .app
        .drive_session_request(implementation.started())
        .await;

    // Assert: receipts remain idempotent while execution is busy.
    assert!(stale.is_err());
    assert_historical_receipts(&mut fixture, launch, revision, acceptance).await;
    let running = fixture.snapshot().await;
    assert_eq!(running.phase, TaskChainPhase::Implementing);
    assert_eq!(running.current_role, TaskAgentRole::Implementer);
    let accepted = running.accepted_plan.expect("accepted version");
    assert_eq!(accepted.text, "Exact revised plan");
    assert_eq!(accepted.attempt_id, revised_attempt);
    assert!(implementation.prompt().contains("Exact revised plan"));
    assert!(
        implementation
            .prompt()
            .contains("Implement a scoped change")
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
        agent_answer(TaskAgentRole::Planner, "First plan"),
        agent_answer(TaskAgentRole::Planner, "Exact revised plan"),
        agent_answer(TaskAgentRole::Implementer, "Implemented"),
    ];
    assert_eq!(fixture.persisted_transcript().await, expected_transcript);
    assert_eq!(fixture.live_transcript(), expected_transcript);
    assert!(
        fixture
            .app
            .drive_session_request(fixture.service.continue_task_chain(
                "workflow",
                ContinueTaskChainRequest {
                    attachments: Vec::new(),
                    attempt_id: Uuid::new_v4().to_string(),
                    clarification: Some("Answering the old Planner".into()),
                    expected_attempt_id: revised_attempt,
                },
            ))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn service_cancels_and_retries_only_the_current_workflow_stage() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
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
    let running = fixture.snapshot().await;

    // Act
    fixture
        .app
        .drive_session_request(fixture.service.cancel_task_chain("workflow"))
        .await
        .expect("cancel workflow");
    let canceled = fixture
        .wait_for(|chain| chain.phase == TaskChainPhase::Canceled)
        .await;
    first.release.cancel();

    // Assert
    assert_eq!(canceled.current_execution, running.current_execution);

    // Act
    let retry = fixture.register_turn("Retried plan", false);
    fixture
        .app
        .drive_session_request(
            fixture.service.continue_task_chain(
                "workflow",
                ContinueTaskChainRequest {
                    attachments: Vec::new(),
                    attempt_id: Uuid::new_v4().to_string(),
                    clarification: None,
                    expected_attempt_id: running
                        .current_attempt_id()
                        .expect("canceled attempt")
                        .into(),
                },
            ),
        )
        .await
        .expect("explicit retry");
    fixture.settle(&retry).await;

    // Assert
    assert_eq!(
        fixture.snapshot().await.phase,
        TaskChainPhase::AwaitingPlanAcceptance
    );
    assert!(retry.prompt().contains("Explicitly retry"));
    assert_eq!(
        fixture
            .app
            .drive_session_request(fixture.service.get_task_chain("missing"))
            .await
            .expect("missing workflow"),
        None
    );
    fixture
        .app
        .drive_session_request(fixture.service.cancel_task_chain("workflow"))
        .await
        .expect("idle cancellation is a no-op");
}

#[tokio::test]
async fn service_reports_missing_busy_and_invalid_workflows_without_dispatch() {
    // Arrange
    let mut fixture = ChainFixture::new_untitled().await;
    let mut client = MockRunClient::new();
    client.expect_submit().never();
    fixture.app.services.set_run_client(Arc::new(client));
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
                    .start_plan_then_implement(&fixture.session_id, launch.clone())
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
                    attachments: Vec::new(),
                    attempt_id: Uuid::new_v4().to_string(),
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
async fn invalid_actions_and_conflicting_launches_are_rejected() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    let launch = launch_request();
    let planner = fixture.register_turn("A plan", false);
    fixture
        .app
        .drive_session_request(
            fixture
                .service
                .start_plan_then_implement(&fixture.session_id, launch.clone()),
        )
        .await
        .expect("launch");
    fixture.settle(&planner).await;
    let awaiting = fixture.snapshot().await;
    let plan_attempt = awaiting.current_attempt_id().expect("plan").to_string();

    // Act
    let unknown_source = fixture
        .app
        .drive_session_request(fixture.service.continue_task_chain(
            "workflow",
            ContinueTaskChainRequest {
                attachments: Vec::new(),
                attempt_id: Uuid::new_v4().to_string(),
                clarification: Some("Guidance".into()),
                expected_attempt_id: "unknown".into(),
            },
        ))
        .await;
    let silent_images = fixture
        .app
        .drive_session_request(fixture.service.continue_task_chain(
            "workflow",
            ContinueTaskChainRequest {
                attachments: vec![TurnPromptAttachment {
                    local_image_path: "sketch.png".into(),
                    placeholder: "[Image #1]".into(),
                }],
                attempt_id: Uuid::new_v4().to_string(),
                clarification: None,
                expected_attempt_id: plan_attempt.clone(),
            },
        ))
        .await;
    let retry_success = fixture
        .app
        .drive_session_request(fixture.service.continue_task_chain(
            "workflow",
            ContinueTaskChainRequest {
                attachments: Vec::new(),
                attempt_id: Uuid::new_v4().to_string(),
                clarification: None,
                expected_attempt_id: plan_attempt,
            },
        ))
        .await;
    let mut conflicts = Vec::new();
    for changed in [
        StartPlanThenImplementRequest {
            user_request: "Conflicting launch".into(),
            ..launch.clone()
        },
        {
            let mut changed = launch.clone();
            changed.agents.planner.id = "other-planner".into();
            changed
        },
        {
            let mut changed = launch.clone();
            changed.agents.implementer.id = "other-implementer".into();
            changed
        },
    ] {
        conflicts.push(
            fixture
                .app
                .drive_session_request(
                    fixture
                        .service
                        .start_plan_then_implement(&fixture.session_id, changed),
                )
                .await,
        );
    }

    // Assert
    assert!(unknown_source.is_err());
    assert!(silent_images.is_err());
    assert!(retry_success.is_err());
    for conflict in conflicts {
        assert!(matches!(conflict, Err(SessionError::Operation(_))));
    }
    assert_eq!(fixture.snapshot().await, awaiting);
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
    assert!(app.ensure_task_chain_driver("workflow").await.is_err());
    assert!(matches!(
        app.drive_session_request(service.reply_to_task_chain(
            &SessionId::from("missing"),
            ReplyToTaskChainRequest {
                attachments: Vec::new(),
                message: "Go ahead".into(),
                reply_id: "reply".into(),
            },
        ))
        .await,
        Err(SessionError::Operation(_))
    ));
}

#[tokio::test]
async fn workflow_launch_failures_and_competing_redelivery_leave_chat_unchanged() {
    for fault in ["workspace", "history", "admission", "redelivery"] {
        // Arrange
        let mut fixture = ChainFixture::new().await;
        let mut repository = MockGraphRepository::new();
        repository.expect_load_graph().returning(|_, _| Ok(None));
        match fault {
            "workspace" => {
                fixture.app.sessions.sessions_mut()[0].is_draft = true;
                let mut filesystem = MockFsClient::new();
                filesystem.expect_is_dir().return_const(false);
                fixture.app.services.set_fs_client(Arc::new(filesystem));
            }
            "history" => {
                sqlx::query("DROP TABLE session_message")
                    .execute(&fixture.pool)
                    .await
                    .expect("unavailable history");
            }
            "admission" => {
                repository
                    .expect_start_task_chain()
                    .returning(|_, _| Err(graph_unavailable()));
            }
            "redelivery" => {
                repository
                    .expect_start_task_chain()
                    .returning(|_, _| Ok(false));
            }
            _ => {}
        }
        fixture
            .app
            .services
            .set_graph_repository(Arc::new(repository));
        // Act
        let result = fixture
            .app
            .drive_session_request(
                fixture
                    .service
                    .start_plan_then_implement(&fixture.session_id, launch_request()),
            )
            .await;
        fixture.app.mirror_task_chain_transcript_message(
            &SessionId::from("missing"),
            SessionMessageKind::UserPrompt,
            "Do not append",
        );

        // Assert
        assert!(
            fixture
                .app
                .sessions
                .session_handles()
                .get(&fixture.session_id)
                .is_none_or(|handles| {
                    handles
                        .transcript
                        .lock()
                        .expect("live transcript")
                        .messages()
                        .is_empty()
                })
        );
        if fault == "redelivery" {
            assert_eq!(result, Ok(()));
        } else {
            let error = result.expect_err("launch failure").to_string();
            let expected = match fault {
                "workspace" => "prepared workspace",
                "history" => "session_message",
                _ => "injected graph outage",
            };
            assert!(error.contains(expected), "{fault}: {error}");
        }
    }
}

#[tokio::test]
async fn workflow_actions_report_failed_or_missing_repository_snapshots() {
    // Arrange
    let mut fixture = ChainFixture::new().await;
    let plan = fixture.register_turn("Plan", false);
    fixture
        .app
        .drive_session_request(
            fixture
                .service
                .start_plan_then_implement(&fixture.session_id, launch_request()),
        )
        .await
        .expect("launch");
    fixture.settle(&plan).await;
    let chain = fixture.snapshot().await;
    let attempt_id = chain.current_attempt_id().expect("plan").to_string();

    for fault in ["lookup", "results", "missing-result", "progress"] {
        let mut repository = MockGraphRepository::new();
        if fault == "lookup" {
            repository
                .expect_load_task_chain()
                .returning(|_| Err(graph_unavailable()));
        } else {
            let chain = chain.clone();
            repository
                .expect_load_task_chain()
                .returning(move |_| Ok(Some(chain.clone())));
        }
        match fault {
            "results" => {
                repository
                    .expect_list_results()
                    .returning(|_, _, _| Err(graph_unavailable()));
            }
            "missing-result" => {
                repository
                    .expect_list_results()
                    .returning(|_, _, _| Ok(Vec::new()));
            }
            "progress" => {
                repository
                    .expect_progress()
                    .returning(|_| Err(graph_unavailable()));
            }
            _ => {}
        }
        fixture
            .app
            .services
            .set_graph_repository(Arc::new(repository));

        // Act
        let result = match fault {
            "lookup" => fixture.app.require_task_chain("workflow").await.map(|_| ()),
            "progress" => {
                fixture
                    .app
                    .continue_api_task_chain(
                        "workflow",
                        ContinueTaskChainRequest {
                            attachments: Vec::new(),
                            attempt_id: Uuid::new_v4().to_string(),
                            clarification: None,
                            expected_attempt_id: attempt_id.clone(),
                        },
                    )
                    .await
            }
            _ => fixture
                .app
                .accept_api_task_chain_plan(
                    "workflow",
                    AcceptTaskChainPlanRequest {
                        plan_attempt_id: attempt_id.clone(),
                    },
                )
                .await
                .map(|_| ()),
        };

        // Assert
        let error = result.expect_err("repository failure").to_string();
        assert!(
            error.contains(if fault == "missing-result" {
                "Plan version is missing"
            } else {
                "injected graph outage"
            }),
            "{fault}: {error}"
        );
    }
}

#[tokio::test]
async fn task_agent_resolution_rejects_missing_sessions_and_unavailable_definitions() {
    // Arrange
    let (mut app, directory) = new_test_app().await;
    let session_id = SessionId::from("owner");
    let folder = directory.path().join("session-worktree");
    app.sessions.push_session(
        SessionFixtureBuilder::new()
            .id(session_id.clone())
            .folder(folder.clone())
            .build(),
    );
    let mut catalog = MockAgentCatalogClient::new();
    catalog
        .expect_resolve()
        .with(eq(folder), eq("disabled-agent".to_string()))
        .times(1)
        .return_once(|_, _| Box::pin(async { None }));
    app.services
        .set_agent_profile_catalog_client(Arc::new(catalog));

    // Act
    let missing = app
        .resolve_task_agent_definition(&SessionId::from("missing"), "disabled-agent")
        .await;
    let unavailable = app
        .resolve_task_agent_definition(&session_id, "disabled-agent")
        .await;

    // Assert
    assert_eq!(missing, Err(SessionError::NotFound));
    assert_eq!(
        unavailable,
        Err(SessionError::Operation(
            "Agent definition `disabled-agent` is unavailable or invalid".into()
        ))
    );
}

/// Launches the fixture workflow, then revises its first plan. Returns the
/// launch, the superseded plan attempt, and the revision request.
async fn revised_plan(
    fixture: &mut ChainFixture,
) -> (
    StartPlanThenImplementRequest,
    String,
    ContinueTaskChainRequest,
) {
    let launch = launch_request();
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
    let first = fixture.register_turn("First plan", false);
    fixture
        .app
        .drive_session_request(
            fixture
                .service
                .start_plan_then_implement(&fixture.session_id, launch.clone()),
        )
        .await
        .expect("launch");
    fixture.settle(&first).await;
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
    let first_attempt = fixture
        .snapshot()
        .await
        .current_attempt_id()
        .expect("first plan attempt")
        .to_string();
    let revision = ContinueTaskChainRequest {
        attachments: Vec::new(),
        attempt_id: Uuid::new_v4().to_string(),
        clarification: Some("Revise the scope".to_string()),
        expected_attempt_id: first_attempt.clone(),
    };
    let second = fixture.register_turn("Exact revised plan", false);
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
    assert!(second.prompt().contains("Revise the scope"));
    assert!(second.prompt().contains("First plan"));

    (launch, first_attempt, revision)
}

async fn append_conversation(fixture: &ChainFixture, messages: &[(SessionMessageKind, &str)]) {
    for (kind, text) in messages {
        fixture
            .app
            .services
            .db()
            .sessions()
            .append_session_message(&fixture.session_id, *kind, text)
            .await
            .expect("history");
    }
}

fn assert_role_request(
    script: &Script,
    agent: AgentSelection,
    definition: &AgentDefinition,
    paths: &[&str],
) {
    let request = script.request();
    assert_eq!(request.model, agent.model().provider_model_str());
    assert_eq!(
        request.agent_profile.current(),
        Some(definition.prompt.as_str())
    );
    assert_eq!(
        request
            .prompt
            .attachments
            .iter()
            .map(|image| image.local_image_path.to_str().expect("path"))
            .collect::<Vec<_>>(),
        paths
    );
}

async fn assert_historical_receipts(
    fixture: &mut ChainFixture,
    launch: StartPlanThenImplementRequest,
    revision: ContinueTaskChainRequest,
    acceptance: AcceptTaskChainPlanRequest,
) {
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
        .drive_session_request(
            fixture
                .service
                .continue_task_chain("workflow", revision.clone()),
        )
        .await
        .expect("old revision receipt");
    let conflicting = fixture
        .app
        .drive_session_request(fixture.service.continue_task_chain(
            "workflow",
            ContinueTaskChainRequest {
                clarification: Some("Conflicting revision".into()),
                ..revision
            },
        ))
        .await;
    assert!(conflicting.is_err());
    let wrong_plan = fixture
        .app
        .accept_api_task_chain_plan(
            "workflow",
            AcceptTaskChainPlanRequest {
                plan_attempt_id: "another-version".into(),
            },
        )
        .await;
    assert!(
        wrong_plan
            .expect_err("accepted plan is immutable")
            .to_string()
            .contains("already accepted a different plan")
    );
    fixture
        .app
        .drive_session_request(
            fixture
                .service
                .accept_task_chain_plan("workflow", acceptance),
        )
        .await
        .expect("handoff receipt");
}
