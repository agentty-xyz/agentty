//! Workspace preparation retry tests covering checkout removal on
//! cancellation and preservation on backend failure.

use std::sync::Arc;

use ag_worker::test_support::{self as agent, AgentBackend};
use tokio::sync::mpsc;

use super::super::{session_branch, session_folder};
use crate::app::SessionManager;
use crate::infra::cargo_cache::MockCargoCacheClient;
use crate::infra::db::SessionPreparationState;

#[tokio::test]
/// Starts refreshing the project's trusted Cargo cache once a Harness
/// session's workspace is ready, for its main checkout and worktree.
async fn ready_harness_workspace_warms_the_project_cargo_cache() {
    // Arrange
    let harness_root = tempfile::tempdir().expect("harness root");
    let (checked_tx, mut checked_rx) = mpsc::unbounded_channel();
    let mut cargo_cache = MockCargoCacheClient::new();
    cargo_cache
        .expect_refresh()
        .returning(move |main_checkout, worktree, _| {
            let _ = checked_tx.send((main_checkout, worktree));

            Ok(())
        });
    let clients = crate::test_support::harness_only_test_app_clients(harness_root.path().into())
        .with_cargo_cache_client(Arc::new(cargo_cache));
    let (mut app, directory) = crate::test_support::new_git_test_app_with_clients(clients).await;

    // Act
    let session_id = app.create_session().await.expect("session");
    let (main_checkout, worktree) = checked_rx.recv().await.expect("cache check");

    // Assert
    assert!(app.services.cargo_cache().is_enabled());
    assert_eq!(
        main_checkout,
        std::fs::canonicalize(directory.path()).expect("main checkout")
    );
    assert_eq!(
        worktree,
        session_folder(app.services.base_path(), &session_id)
    );
}

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
    let failed = SessionManager::prepare_workspace(&app.services, &preparation, &row, |folder| {
        backend.setup(folder)
    })
    .await;
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
