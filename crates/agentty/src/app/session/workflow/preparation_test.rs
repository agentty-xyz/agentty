use ag_agent as agent;

use super::super::{session_branch, session_folder};
use crate::app::SessionManager;
use crate::infra::db::SessionPreparationState;

#[tokio::test]
async fn retry_canceled_before_worker_start_removes_its_existing_checkout() {
    // Arrange
    let (mut app, directory) = crate::test_support::new_git_test_app().await;
    let session_id = app.create_session().await.expect("session");
    let folder = session_folder(app.services.base_path(), &session_id);
    app.services
        .db()
        .sessions()
        .update_session_preparation(&session_id, SessionPreparationState::Preparing, None)
        .await
        .expect("retry");
    app.refresh_workspace_preparation(&session_id).await;

    // Act
    app.cancel_session(&session_id).await.expect("cancel");
    let result = SessionManager::prepare_reserved_session(&app.services, &session_id).await;
    app.wait_for_background_cleanup_tasks().await;
    let branch = app
        .services
        .git_client()
        .ref_hash(directory.path().to_path_buf(), session_branch(&session_id))
        .await;

    // Assert
    assert!(
        result
            .expect_err("canceled")
            .to_string()
            .contains("canceled")
    );
    assert!(!folder.exists());
    assert!(branch.is_err());
}

#[tokio::test]
async fn retry_validates_owned_checkout_and_preserves_it_on_backend_failure() {
    // Arrange
    let (mut app, _directory) = crate::test_support::new_git_test_app().await;
    let id = app.create_session().await.expect("session");
    let row = app
        .services
        .db()
        .sessions()
        .load_session(&id)
        .await
        .expect("load")
        .expect("row");
    let preparation = app
        .services
        .db()
        .sessions()
        .load_session_preparation(&id)
        .await
        .expect("load")
        .expect("preparation");
    let mut backend = agent::MockAgentBackend::new();
    backend.expect_setup().once().returning(|_| {
        Err(agent::AgentBackendError::Setup(
            "retry backend failed".to_string(),
        ))
    });

    // Act
    let inactive = SessionManager::prepare_reserved_session(&app.services, &id).await;
    let failed =
        SessionManager::prepare_workspace(&app.services, &preparation, &row, &backend).await;
    app.services
        .db()
        .sessions()
        .update_session_preparation(&id, SessionPreparationState::Preparing, None)
        .await
        .expect("retry");
    let retried = SessionManager::prepare_reserved_session(&app.services, &id).await;

    // Assert
    assert!(
        inactive
            .expect_err("inactive")
            .to_string()
            .contains("not active")
    );
    assert!(
        failed
            .expect_err("backend")
            .to_string()
            .contains("retry backend failed")
    );
    assert!(retried.is_ok());
    assert!(session_folder(app.services.base_path(), &id).is_dir());
    assert_eq!(
        app.services
            .db()
            .sessions()
            .load_session_preparation(&id)
            .await
            .expect("load")
            .expect("preparation")
            .state,
        SessionPreparationState::Ready
    );
}
