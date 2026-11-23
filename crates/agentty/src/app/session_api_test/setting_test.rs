//! Session API tests that build sessions with complete settings and
//! messages, and that change a session's settings.

use ag_contracts::{ReasoningLevel, ResponseStyle, SpeedMode};
use ag_forge::ForgeKind;
use ag_session::{
    AgentKind, AgentModel, AgentSelection, CreateSessionMode, CreateSessionRequest,
    SessionError as ApiSessionError, SessionId, SessionOperation, SessionSettingsChange,
    SessionStatus, test_support as model_fixture,
};

use super::super::build_api_session;
use super::support::{
    request_host_context, request_session_creation, request_settings, session_row,
};
use crate::app::App;
use crate::infra::db::{SessionMessageRow, SessionRow};

/// Creates one regular session in `app`'s active project.
async fn create_regular_session(app: &mut App) -> SessionId {
    let project_id = app.active_project_id();

    request_session_creation(
        app,
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Regular,
            project_id,
        },
    )
    .await
    .expect("session should be created")
}

/// Returns another model the session's agent can switch to.
async fn other_model(app: &mut App, current: AgentSelection) -> AgentSelection {
    request_host_context(app)
        .await
        .expect("host context should load")
        .models
        .into_iter()
        .find(|model| model.kind() == current.kind() && *model != current)
        .expect("the session's agent should offer another model")
}

#[tokio::test]
async fn settings_change_applies_every_setting_to_an_idle_session() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let session_id = create_regular_session(&mut app).await;
    crate::test_support::set_session_status_for_test(&mut app, &session_id, SessionStatus::Review);
    let current = app
        .sessions
        .session_for_id(&session_id)
        .expect("session")
        .agent;
    let target = other_model(&mut app, current).await;

    // Act
    let supported = app
        .session_service()
        .supports(SessionOperation::SessionSettings);
    let changed = request_settings(
        &mut app,
        session_id.clone(),
        SessionSettingsChange {
            model: Some(target.model()),
            reasoning_level: Some(ReasoningLevel::Low),
            response_style: Some(ResponseStyle::Concise),
            speed_mode: Some(SpeedMode::Normal),
        },
    )
    .await;

    // Assert
    let session = app.sessions.session_for_id(&session_id).expect("session");
    assert!(supported);
    assert_eq!(changed, Ok(()));
    assert_eq!(session.agent, target);
    assert_eq!(session.effective_reasoning_level(), ReasoningLevel::Low);
    assert_eq!(session.response_style, ResponseStyle::Concise);
    assert_eq!(session.speed_mode, SpeedMode::Normal);
}

#[tokio::test]
async fn settings_change_waits_behind_a_running_turn_like_the_terminal() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let session_id = create_regular_session(&mut app).await;
    let current = model_fixture::codex_selection();
    app.set_session_model(&session_id, current)
        .await
        .expect("session should start on a model with fast mode");
    let target = other_model(&mut app, current).await;
    crate::test_support::set_session_status_for_test(
        &mut app,
        &session_id,
        SessionStatus::InProgress,
    );

    // Act
    let settings_changed = request_settings(
        &mut app,
        session_id.clone(),
        SessionSettingsChange {
            reasoning_level: Some(ReasoningLevel::Low),
            response_style: Some(ResponseStyle::Concise),
            speed_mode: Some(SpeedMode::Fast),
            ..SessionSettingsChange::default()
        },
    )
    .await;
    let speed_during_turn = app.sessions.session_for_id(&session_id).map(|session| {
        (
            session.effective_reasoning_level(),
            session.response_style,
            session.speed_mode,
        )
    });
    let deferred_speed = app
        .deferred_session_settings
        .get(&session_id)
        .and_then(|settings| settings.speed_mode);
    let model_changed = request_settings(
        &mut app,
        session_id.clone(),
        SessionSettingsChange {
            model: Some(target.model()),
            ..SessionSettingsChange::default()
        },
    )
    .await;
    // Checked against the pending model, the current speed cancels the
    // pending fast mode.
    let speed_restored = request_settings(
        &mut app,
        session_id.clone(),
        SessionSettingsChange {
            speed_mode: Some(SpeedMode::Normal),
            ..SessionSettingsChange::default()
        },
    )
    .await;

    // Assert
    assert_eq!(speed_restored, Ok(()));
    assert_eq!(
        app.deferred_session_settings
            .get(&session_id)
            .and_then(|settings| settings.speed_mode),
        None
    );
    assert_eq!(settings_changed, Ok(()));
    assert_eq!(
        speed_during_turn,
        Some((
            ReasoningLevel::Low,
            ResponseStyle::Concise,
            SpeedMode::Normal
        ))
    );
    assert_eq!(deferred_speed, Some(SpeedMode::Fast));
    assert_eq!(model_changed, Ok(()));
    assert_eq!(
        app.sessions
            .session_for_id(&session_id)
            .expect("session")
            .agent,
        current
    );
    assert_eq!(
        app.deferred_session_settings
            .get(&session_id)
            .and_then(|settings| settings.agent),
        Some(target)
    );
}

#[tokio::test]
async fn settings_change_refuses_what_the_session_cannot_use() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let session_id = create_regular_session(&mut app).await;
    crate::test_support::set_session_status_for_test(&mut app, &session_id, SessionStatus::Review);
    let antigravity = AgentSelection::new(
        AgentKind::Antigravity,
        AgentKind::Antigravity.default_model(),
    );

    // Act
    let missing = request_settings(
        &mut app,
        SessionId::from("missing"),
        SessionSettingsChange::default(),
    )
    .await;
    let harness = request_settings(
        &mut app,
        session_id.clone(),
        SessionSettingsChange {
            model: Some(AgentModel::KimiK3),
            ..SessionSettingsChange::default()
        },
    )
    .await;
    app.sessions
        .sessions_mut()
        .iter_mut()
        .filter(|session| session.id == session_id.as_str())
        .for_each(|session| session.agent = antigravity);
    let fast = request_settings(
        &mut app,
        session_id.clone(),
        SessionSettingsChange {
            speed_mode: Some(SpeedMode::Fast),
            ..SessionSettingsChange::default()
        },
    )
    .await;
    let mut without_composer = Vec::new();
    for status in [
        SessionStatus::Queued,
        SessionStatus::Merging,
        SessionStatus::Done,
    ] {
        crate::test_support::set_session_status_for_test(&mut app, &session_id, status);
        let refused = request_settings(
            &mut app,
            session_id.clone(),
            SessionSettingsChange {
                reasoning_level: Some(ReasoningLevel::Low),
                ..SessionSettingsChange::default()
            },
        )
        .await;
        without_composer.push(refused);
    }

    // Assert
    assert_eq!(missing, Err(ApiSessionError::NotFound));
    assert_eq!(
        harness,
        Err(ApiSessionError::Operation(format!(
            "Model `{}` is not available to session `{session_id}`",
            AgentModel::KimiK3.as_str()
        )))
    );
    assert_eq!(
        fast,
        Err(ApiSessionError::Operation(format!(
            "Fast mode is not supported by `{}`",
            antigravity.model().as_str()
        )))
    );
    assert_eq!(
        without_composer,
        ["Queued", "Merging", "Done"].map(|status| Err(ApiSessionError::Operation(format!(
            "Session `{session_id}` cannot change settings in status `{status}`"
        ))))
    );
    assert_ne!(
        app.sessions
            .session_for_id(&session_id)
            .map(crate::domain::session::Session::effective_reasoning_level),
        Some(ReasoningLevel::Low)
    );
}

#[tokio::test]
async fn settings_change_reports_a_model_switch_that_cannot_persist() {
    // Arrange
    let (mut app, _temp_dir, pool) = crate::test_support::new_git_test_app_with_pool().await;
    let session_id = create_regular_session(&mut app).await;
    crate::test_support::set_session_status_for_test(&mut app, &session_id, SessionStatus::Review);
    let current = app
        .sessions
        .session_for_id(&session_id)
        .expect("session")
        .agent;
    let target = other_model(&mut app, current).await;
    sqlx::query("DROP TABLE session")
        .execute(&pool)
        .await
        .expect("session table should be dropped");

    // Act
    let changed = request_settings(
        &mut app,
        session_id.clone(),
        SessionSettingsChange {
            model: Some(target.model()),
            ..SessionSettingsChange::default()
        },
    )
    .await;

    // Assert
    assert_eq!(
        changed,
        Err(ApiSessionError::Operation(format!(
            "Session `{session_id}` could not switch to `{}`",
            target.model().as_str()
        )))
    );
}

#[tokio::test]
async fn settings_change_reports_a_speed_that_cannot_persist_after_earlier_settings_applied() {
    // Arrange
    let (mut app, _temp_dir, pool) = crate::test_support::new_git_test_app_with_pool().await;
    let session_id = create_regular_session(&mut app).await;
    app.set_session_model(&session_id, model_fixture::codex_selection())
        .await
        .expect("session should start on a model with fast mode");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, SessionStatus::Review);
    sqlx::query(
        "CREATE TRIGGER fail_speed_mode_update BEFORE UPDATE OF speed_mode ON session BEGIN \
         SELECT RAISE(FAIL, 'forced speed mode failure'); END",
    )
    .execute(&pool)
    .await
    .expect("failure trigger should be installed");

    // Act
    let changed = request_settings(
        &mut app,
        session_id.clone(),
        SessionSettingsChange {
            reasoning_level: Some(ReasoningLevel::Low),
            speed_mode: Some(SpeedMode::Fast),
            ..SessionSettingsChange::default()
        },
    )
    .await;

    // Assert
    let session = app.sessions.session_for_id(&session_id).expect("session");
    assert!(matches!(changed, Err(ApiSessionError::Operation(_))));
    assert_eq!(session.effective_reasoning_level(), ReasoningLevel::Low);
    assert_eq!(session.speed_mode, SpeedMode::Normal);
}

#[tokio::test]
async fn host_lists_harness_models_as_the_terminal_model_picker_does() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;

    // Act
    let unlisted = request_host_context(&mut app)
        .await
        .expect("host context should load")
        .models;
    crate::test_support::set_test_harness_models(&mut app.services, Some(AgentModel::KimiK3));
    let listed = request_host_context(&mut app)
        .await
        .expect("host context should load")
        .models;

    // Assert
    let harness = |models: &[AgentSelection]| {
        models
            .iter()
            .filter(|model| model.kind() == AgentKind::Harness)
            .map(|model| model.model())
            .collect::<Vec<_>>()
    };
    assert!(
        unlisted
            .iter()
            .any(|model| model.kind() != AgentKind::Harness)
    );
    // Before the harness lists its models, `/model` offers the fixed catalog.
    assert_eq!(harness(&unlisted), AgentKind::Harness.models());
    assert_eq!(harness(&listed), [AgentModel::KimiK3]);
}

#[test]
fn build_api_session_returns_complete_settings_and_messages() {
    // Arrange
    let row = session_row();
    let message_rows = vec![
        SessionMessageRow {
            content: "first".to_string(),
            kind: "user_prompt".to_string(),
            position: 0,
        },
        SessionMessageRow {
            content: "done".to_string(),
            kind: "assistant_answer".to_string(),
            position: 1,
        },
    ];

    // Act
    let session = build_api_session(row, message_rows, vec!["queued message".to_string()])
        .expect("row should convert");

    // Assert
    assert_eq!(session.id, "session-1");
    assert_eq!(session.draft_prompt.as_deref(), Some("staged prompt"));
    assert_eq!(session.messages.len(), 2);
    assert_eq!(session.questions[0].text, "Which target?");
    assert_eq!(session.queued_messages, ["queued message"]);
    assert_eq!(session.settings.project_id, 7);
    assert_eq!(session.settings.agent, model_fixture::codex_selection());
    assert_eq!(session.settings.reasoning_level, ReasoningLevel::XHigh);
    assert_eq!(session.settings.speed_mode, SpeedMode::Normal);
    assert_eq!(
        session
            .review_request
            .expect("review request should convert")
            .summary
            .forge_kind,
        ForgeKind::GitHub
    );
}

#[test]
fn build_api_session_stages_no_draft_prompt_once_the_draft_starts() {
    // Arrange
    let row = SessionRow {
        status: "InProgress".to_string(),
        ..session_row()
    };

    // Act
    let session = build_api_session(row, Vec::new(), Vec::new()).expect("row should convert");

    // Assert
    assert!(session.settings.is_draft);
    assert_eq!(session.draft_prompt, None);
}
