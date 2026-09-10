use ag_agent::{AgentKind, AgentModel};
use ag_session::{
    CreateSessionMode, CreateSessionRequest, SessionError as ApiSessionError, SessionId,
};

use super::support::{request_session, request_session_creation};

#[tokio::test]
async fn runtime_backend_migrates_retired_model_for_inactive_project_lookup() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let inactive_project_id = app
        .services
        .db()
        .projects()
        .upsert_project("/inactive-project", Some("main".to_string()))
        .await
        .expect("inactive project should persist");
    let session_id = SessionId::from("inactive-retired-session");
    app.services
        .db()
        .sessions()
        .insert_session(
            &session_id,
            "gemini-3.5-flash",
            "main",
            "Review",
            inactive_project_id,
        )
        .await
        .expect("retired-model session should persist");

    // Act
    let loaded_session = request_session(&mut app, session_id.clone())
        .await
        .expect("session lookup should succeed")
        .expect("session should exist");
    let persisted_row = app
        .services
        .db()
        .sessions()
        .load_session(&session_id)
        .await
        .expect("migrated session should load")
        .expect("migrated session should exist");

    // Assert
    assert_eq!(loaded_session.settings.project_id, inactive_project_id);
    assert_eq!(
        loaded_session.settings.agent,
        ag_agent::AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini35FlashLite)
    );
    assert_eq!(persisted_row.agent, "antigravity");
    assert_eq!(persisted_row.model, "gemini-3.5-flash-lite");
}

#[tokio::test]
async fn runtime_backend_rejects_cross_project_inheritance() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let project_id = app.active_project_id();
    let inactive_project_id = app
        .services
        .db()
        .projects()
        .upsert_project("/inactive-project", Some("develop".to_string()))
        .await
        .expect("inactive project should persist");
    let source_session_id = SessionId::from("inactive-source");
    app.services
        .db()
        .sessions()
        .insert_session(
            &source_session_id,
            "gpt-5.6-sol",
            "develop",
            "Draft",
            inactive_project_id,
        )
        .await
        .expect("inactive source session should persist");

    // Act
    let project_mismatch_error = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: Some(source_session_id),
            mode: CreateSessionMode::Draft,
            project_id,
        },
    )
    .await
    .expect_err("cross-project inheritance should fail");

    // Assert
    assert_eq!(
        project_mismatch_error,
        ApiSessionError::Operation(format!(
            "Session `inactive-source` belongs to project `{inactive_project_id}`, not \
             `{project_id}`"
        ))
    );
}

#[tokio::test]
async fn runtime_backend_rejects_stacked_parent_from_another_project() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let project_id = app.active_project_id();
    let inactive_project_id = app
        .services
        .db()
        .projects()
        .upsert_project("/inactive-project", Some("develop".to_string()))
        .await
        .expect("inactive project should persist");
    let parent_session_id = SessionId::from("inactive-parent");
    app.services
        .db()
        .sessions()
        .insert_session(
            &parent_session_id,
            "gpt-5.6-sol",
            "develop",
            "Review",
            inactive_project_id,
        )
        .await
        .expect("inactive parent session should persist");

    // Act
    let project_mismatch_error = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Stacked {
                parent_session_id: parent_session_id.clone(),
            },
            project_id,
        },
    )
    .await
    .expect_err("cross-project stacked creation should fail");

    // Assert
    assert_eq!(
        project_mismatch_error,
        ApiSessionError::Operation(format!(
            "Parent session `{parent_session_id}` belongs to project `{inactive_project_id}`, not \
             `{project_id}`"
        ))
    );
}
