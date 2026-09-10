use ag_session::{CreateSessionMode, CreateSessionRequest, SessionError as ApiSessionError};

use super::support::request_session_creation;

#[tokio::test]
async fn runtime_backend_rejects_session_creation_during_project_sync() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let project_id = app.active_project_id();
    app.project_sync_status = Some(crate::app::sync::ProjectSyncStatus {
        context: crate::app::sync::ProjectSyncContext {
            default_branch: "main".to_string(),
            operation_id: 1,
            project_id,
            project_name: "agentty".to_string(),
        },
        phase: crate::app::sync::ProjectSyncPhase::Running,
    });

    // Act
    let result = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Orchestrator,
            project_id,
        },
    )
    .await;

    // Assert
    assert!(matches!(
        result,
        Err(ApiSessionError::Operation(message))
            if message.contains("is synchronizing `main`")
    ));
}
