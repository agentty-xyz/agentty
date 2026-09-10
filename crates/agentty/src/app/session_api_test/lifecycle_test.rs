use std::sync::Arc;

use ag_agent::{
    AgentKind, AgentModel, AgentSelection, AppServerTurnResponse, MockAppServerClient,
    PermissionMode, ReasoningLevel, SpeedMode,
};
use ag_session::{
    CoordinatorMessageRequest, CoordinatorMessageVisibility, CreateSessionMode,
    CreateSessionRequest, SessionError as ApiSessionError, SessionId, SessionMessageKind,
    SessionRole, SessionStatus,
};

use super::support::{
    persist_inherited_launch_settings, request_coordinator_message, request_message,
    request_session, request_session_creation, seed_active_orchestration_session,
};
use crate::app::AppEvent;
use crate::domain::orchestration::OrchestrationTaskKind;

#[tokio::test]
async fn runtime_backend_creates_and_loads_complete_sessions() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let project_id = app.active_project_id();

    // Act
    let session_id = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Regular,
            project_id,
        },
    )
    .await
    .expect("regular session should be created");
    app.services
        .db()
        .sessions()
        .append_session_message(&session_id, SessionMessageKind::UserPrompt, "build it")
        .await
        .expect("message should persist");
    let loaded_session = request_session(&mut app, session_id.clone())
        .await
        .expect("session should load")
        .expect("session should exist");
    let missing_session = request_session(&mut app, SessionId::from("missing"))
        .await
        .expect("missing lookup should succeed");
    let stacked_session_id = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Stacked {
                parent_session_id: session_id.clone(),
            },
            project_id,
        },
    )
    .await
    .expect("stacked session should be created");
    let stacked_session = request_session(&mut app, stacked_session_id)
        .await
        .expect("stacked session should load")
        .expect("stacked session should exist");
    let inherited_stacked_session_id = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: Some(session_id.clone()),
            mode: CreateSessionMode::Stacked {
                parent_session_id: session_id.clone(),
            },
            project_id,
        },
    )
    .await
    .expect("inherited stacked session should be created");
    let inherited_stacked_session = request_session(&mut app, inherited_stacked_session_id)
        .await
        .expect("inherited stacked session should load")
        .expect("inherited stacked session should exist");

    // Assert
    assert_eq!(loaded_session.id, session_id);
    assert_eq!(loaded_session.status, SessionStatus::Draft);
    assert_eq!(loaded_session.messages.len(), 1);
    assert_eq!(loaded_session.messages[0].content, "build it");
    assert_eq!(
        loaded_session.settings.project_id,
        app.projects.active_project_id()
    );
    assert_eq!(missing_session, None);
    assert_eq!(stacked_session.settings.parent_session_id, Some(session_id));
    assert_eq!(
        inherited_stacked_session.settings.parent_session_id,
        Some(loaded_session.id)
    );
}

#[tokio::test]
async fn runtime_backend_rejects_creation_for_inactive_projects() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let inactive_project_id = app
        .services
        .db()
        .projects()
        .upsert_project("/inactive-project", Some("develop".to_string()))
        .await
        .expect("inactive project should persist");

    // Act
    let creation_error = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Draft,
            project_id: inactive_project_id,
        },
    )
    .await
    .expect_err("inactive project creation should fail");

    // Assert
    assert_eq!(
        creation_error,
        ApiSessionError::Operation(format!("Project `{inactive_project_id}` is not active"))
    );
}

#[tokio::test]
async fn runtime_backend_inherits_launch_settings_for_regular_and_draft_sessions() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let project_id = app.active_project_id();
    let default_session_model = app.sessions.default_session_model();
    let source_session_id = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Draft,
            project_id,
        },
    )
    .await
    .expect("source session should be created");
    persist_inherited_launch_settings(&app, &source_session_id).await;

    // Act
    let inherited_session_id = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: Some(source_session_id.clone()),
            mode: CreateSessionMode::Draft,
            project_id,
        },
    )
    .await
    .expect("inherited session should be created");
    let inherited_session = request_session(&mut app, inherited_session_id)
        .await
        .expect("inherited session should load")
        .expect("inherited session should exist");
    let inherited_regular_session_id = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: Some(source_session_id.clone()),
            mode: CreateSessionMode::Regular,
            project_id,
        },
    )
    .await
    .expect("inherited regular session should be created");
    let inherited_regular_session = request_session(&mut app, inherited_regular_session_id)
        .await
        .expect("inherited regular session should load")
        .expect("inherited regular session should exist");
    let ordinary_session_id = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Draft,
            project_id,
        },
    )
    .await
    .expect("ordinary session should be created");
    let ordinary_session = request_session(&mut app, ordinary_session_id)
        .await
        .expect("ordinary session should load")
        .expect("ordinary session should exist");

    // Assert
    assert_eq!(
        inherited_session.settings.agent,
        ag_agent::AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5)
    );
    assert_eq!(
        inherited_session.settings.reasoning_level,
        ReasoningLevel::High
    );
    assert_eq!(inherited_session.settings.speed_mode, SpeedMode::Fast);
    assert_eq!(
        inherited_session.settings.permission_mode,
        PermissionMode::ReadOnly
    );
    assert_eq!(
        inherited_session.settings.personality_id.as_deref(),
        Some("inherited-personality")
    );
    assert_eq!(
        inherited_regular_session.settings.agent,
        inherited_session.settings.agent
    );
    assert_eq!(
        inherited_regular_session.settings.reasoning_level,
        inherited_session.settings.reasoning_level
    );
    assert_eq!(
        inherited_regular_session.settings.speed_mode,
        inherited_session.settings.speed_mode
    );
    assert_eq!(
        inherited_regular_session.settings.personality_id.as_deref(),
        Some("inherited-personality")
    );
    assert_eq!(
        ordinary_session.settings.agent.model(),
        default_session_model
    );
    assert_eq!(ordinary_session.settings.speed_mode, SpeedMode::Normal);
    assert_eq!(
        ordinary_session.settings.permission_mode,
        PermissionMode::AutoEdit
    );
    assert_eq!(app.sessions.default_session_model(), default_session_model);
}

#[tokio::test]
async fn runtime_backend_starts_regular_and_staged_draft_messages() {
    // Arrange
    let (turn_started_tx, mut turn_started_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app_server = MockAppServerClient::new();
    app_server
        .expect_run_turn()
        .times(2)
        .returning(move |_, _| {
            let turn_started_tx = turn_started_tx.clone();

            Box::pin(async move {
                let _ = turn_started_tx.send(());

                Ok(AppServerTurnResponse {
                    assistant_message: r#"{"answer":"ready","questions":[]}"#.to_string(),
                    context_reset: false,
                    input_tokens: 0,
                    output_tokens: 0,
                    pid: None,
                    provider_conversation_id: None,
                })
            })
        });
    app_server
        .expect_shutdown_session()
        .times(0..)
        .returning(|_| Box::pin(async {}));
    let clients = crate::test_support::test_app_clients()
        .with_app_server_client_override(Arc::new(app_server));
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app_with_clients(clients).await;
    let project_id = app.active_project_id();
    let regular_session_id = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Regular,
            project_id,
        },
    )
    .await
    .expect("regular session should be created");
    let draft_session_id = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Draft,
            project_id,
        },
    )
    .await
    .expect("draft session should be created");
    for session_id in [&regular_session_id, &draft_session_id] {
        app.set_session_model(
            session_id,
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
        )
        .await
        .expect("session model should update");
    }

    // Act
    request_message(&mut app, regular_session_id.clone(), "regular prompt")
        .await
        .expect("regular session should start");
    request_message(&mut app, draft_session_id.clone(), "draft prompt")
        .await
        .expect("staged draft should start");
    for _ in 0..2 {
        tokio::time::timeout(std::time::Duration::from_secs(1), turn_started_rx.recv())
            .await
            .expect("agent turn should start")
            .expect("agent turn signal should be available");
    }

    // Assert
    assert_eq!(
        app.sessions
            .session_for_id(&regular_session_id)
            .map(|session| session.prompt.as_str()),
        Some("regular prompt")
    );
    assert_eq!(
        app.sessions
            .session_for_id(&draft_session_id)
            .map(|session| session.prompt.as_str()),
        Some("draft prompt")
    );
}

#[tokio::test]
async fn runtime_backend_loads_inherited_creation_before_acknowledging_event_backlog() {
    // Arrange
    let (turn_started_tx, mut turn_started_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app_server = MockAppServerClient::new();
    app_server.expect_run_turn().once().returning(move |_, _| {
        let turn_started_tx = turn_started_tx.clone();

        Box::pin(async move {
            let _ = turn_started_tx.send(());

            Ok(AppServerTurnResponse {
                assistant_message: r#"{"answer":"ready","questions":[]}"#.to_string(),
                context_reset: false,
                input_tokens: 0,
                output_tokens: 0,
                pid: None,
                provider_conversation_id: None,
            })
        })
    });
    app_server
        .expect_shutdown_session()
        .times(0..)
        .returning(|_| Box::pin(async {}));
    let clients = crate::test_support::test_app_clients()
        .with_app_server_client_override(Arc::new(app_server));
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app_with_clients(clients).await;
    let project_id = app.active_project_id();
    let source_session_id = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Draft,
            project_id,
        },
    )
    .await
    .expect("source session should be created");
    for _ in 0..crate::app::reducer::APP_EVENT_DRAIN_BUDGET {
        app.services
            .emit_app_event(crate::app::AppEvent::RefreshProjects);
    }

    // Act
    let inherited_session_id = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: Some(source_session_id),
            mode: CreateSessionMode::Draft,
            project_id,
        },
    )
    .await
    .expect("inherited session should be created");
    let send_result =
        request_message(&mut app, inherited_session_id.clone(), "inherited prompt").await;
    tokio::time::timeout(std::time::Duration::from_secs(1), turn_started_rx.recv())
        .await
        .expect("agent turn should start")
        .expect("agent turn signal should be available");

    // Assert
    assert_eq!(send_result, Ok(()));
    assert_eq!(
        app.sessions
            .session_for_id(&inherited_session_id)
            .map(|session| session.prompt.as_str()),
        Some("inherited prompt")
    );
}

#[tokio::test]
async fn finishing_api_creation_schedules_registration_retry_after_load_failure() {
    // Arrange
    let (mut app, _temp_dir, pool) = crate::test_support::new_git_test_app_with_pool().await;
    let project_id = app.active_project_id();
    app.services
        .db()
        .sessions()
        .insert_session(
            "persisted-session",
            "gpt-5.6-sol",
            "main",
            "Draft",
            project_id,
        )
        .await
        .expect("session should persist before registration");
    sqlx::query("DROP TABLE session")
        .execute(&pool)
        .await
        .expect("session reads should fail");

    // Act
    app.finish_api_session_creation("persisted-session").await;
    let retry_event = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let event = app
                .next_app_event()
                .await
                .expect("app event channel should remain open");
            if event == AppEvent::RefreshSessions {
                break event;
            }
        }
    })
    .await
    .expect("registration retry should be scheduled");

    // Assert
    assert_eq!(retry_event, AppEvent::RefreshSessions);
}

#[tokio::test]
async fn orchestration_research_mode_creates_a_managed_researcher() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;

    // Act
    let fixture =
        seed_active_orchestration_session(&mut app, true, OrchestrationTaskKind::Research).await;
    let child = app
        .sessions
        .session_for_id(&fixture.child)
        .expect("research child should be loaded");
    let persisted_task = app
        .services
        .db()
        .orchestrations()
        .load_orchestration_tasks(fixture.orchestration)
        .await
        .expect("research task should load from persistence")
        .into_iter()
        .find(|task| task.id == fixture.task)
        .expect("research task should exist in persistence");

    // Assert
    assert_eq!(child.role, SessionRole::OrchestrationResearcher);
    assert_eq!(
        persisted_task.child_session_id.as_deref(),
        Some(fixture.child.as_str())
    );
}

#[tokio::test]
async fn visible_coordinator_turn_persists_continuation_prompt() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let project_id = app.active_project_id();
    let session_id = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Orchestrator,
            project_id,
        },
    )
    .await
    .expect("orchestrator should be created");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, SessionStatus::Review);

    // Act
    request_coordinator_message(
        &mut app,
        session_id.clone(),
        CoordinatorMessageRequest {
            message: "Apply the requested correction".to_string(),
            operation_id: "orchestration-continuation-1-1".to_string(),
            visibility: CoordinatorMessageVisibility::Visible,
        },
    )
    .await
    .expect("visible coordinator turn should be accepted");
    let messages = app
        .services
        .db()
        .sessions()
        .load_session_messages(&session_id)
        .await
        .expect("continuation transcript should load");

    // Assert
    assert!(messages.iter().any(|message| {
        message.kind == SessionMessageKind::UserPrompt.to_string()
            && message.content == "Apply the requested correction"
    }));
}

#[tokio::test]
async fn project_creation_context_requires_a_base_branch() {
    // Arrange
    let (app, _temp_dir) = crate::test_support::new_test_app().await;

    // Act
    let active_error = app.api_project_creation_context(None).err();

    // Assert
    let expected =
        ApiSessionError::Operation("Git branch is required to create a session".to_string());
    assert_eq!(active_error.as_ref(), Some(&expected));
}
