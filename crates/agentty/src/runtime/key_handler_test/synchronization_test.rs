//! Merge confirmation key-handler tests.

use std::sync::Arc;

use super::super::{handle_confirmation_decision, handle_merge_confirmation};
use super::support::session_replay_text;
use crate::app::App;
use crate::infra::tmux::MockTmuxClient;
use crate::presentation::app_mode::{AppMode, ConfirmationIntent, ConfirmationViewMode};
use crate::runtime::EventResult;
use crate::runtime::mode::confirmation::ConfirmationDecision;

#[tokio::test]
async fn test_merge_confirmation_accepts_an_already_active_merge() {
    // Arrange
    let base_dir = tempfile::tempdir().expect("failed to create base dir");
    let project_dir = tempfile::tempdir().expect("failed to create project dir");
    crate::test_support::setup_test_git_repo(project_dir.path());
    let repositories = crate::infra::db::AppRepositories::in_memory()
        .await
        .expect("db should open");
    let clients = crate::test_support::test_app_clients_with_mock_app_server()
        .with_tmux_client(Arc::new(MockTmuxClient::new()));
    let mut app = App::new_with_clients(
        base_dir.path().to_path_buf(),
        project_dir.path().to_path_buf(),
        Some("main".to_string()),
        repositories,
        clients,
    )
    .await
    .expect("failed to create app");
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    crate::test_support::set_session_status_for_test(
        &mut app,
        &session_id,
        crate::domain::session::Status::Review,
    );
    app.merge_session(&session_id)
        .await
        .expect("merge should become active");

    // Act
    let event_result = handle_merge_confirmation(&mut app, Some(session_id.into()), None).await;

    // Assert
    assert!(matches!(event_result, Ok(EventResult::Continue)));
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_handle_confirmation_decision_cancel_restores_view_for_merge_confirmation() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    app.mode = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::MergeSession,
        confirmation_message: "Add this session to merge queue?".to_string(),
        confirmation_title: "Confirm Merge".to_string(),
        restore_view: Some(ConfirmationViewMode {
            scroll_offset: Some(6),
            session_id: session_id.clone().into(),
        }),
        session_id: Some(session_id.clone().into()),
        selected_confirmation_index: 0,
    };

    // Act
    let event_result = handle_confirmation_decision(&mut app, ConfirmationDecision::Cancel).await;

    // Assert
    assert!(matches!(event_result, Ok(EventResult::Continue)));
    assert!(matches!(
        app.mode,
        AppMode::View {
            session_id: ref session_id_in_mode,
            scroll_offset: Some(6),
        } if session_id_in_mode == &session_id
    ));
}

#[tokio::test]
async fn test_handle_confirmation_decision_confirm_queues_merge_with_view_restore() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    app.mode = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::MergeSession,
        confirmation_message: "Add this session to merge queue?".to_string(),
        confirmation_title: "Confirm Merge".to_string(),
        restore_view: Some(ConfirmationViewMode {
            scroll_offset: Some(2),
            session_id: session_id.clone().into(),
        }),
        session_id: Some(session_id.clone().into()),
        selected_confirmation_index: 0,
    };

    // Act
    let event_result = handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

    // Assert
    assert!(matches!(event_result, Ok(EventResult::Continue)));
    assert!(matches!(
        app.mode,
        AppMode::View {
            session_id: ref session_id_in_mode,
            scroll_offset: Some(2),
        } if session_id_in_mode == &session_id
    ));
    app.sessions.sync_from_handles();
    let output = session_replay_text(&app.sessions.sessions()[0]);
    assert!(output.contains("[Merge Error]"));
}

/// Opens the pull-request merge confirmation of `session_id`, bypassing
/// branch protections when `bypass` is true.
fn review_request_merge_confirmation(session_id: &str, bypass: bool) -> AppMode {
    AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::MergeReviewRequest { bypass },
        confirmation_message: "Merge pull request #42 into `main`?".to_string(),
        confirmation_title: "Confirm Merge".to_string(),
        restore_view: Some(ConfirmationViewMode {
            scroll_offset: Some(3),
            session_id: session_id.into(),
        }),
        session_id: Some(session_id.into()),
        selected_confirmation_index: 0,
    }
}

#[tokio::test]
async fn confirmed_pull_request_merge_reports_a_refusal_in_the_view() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    app.mode = review_request_merge_confirmation(&session_id, true);

    // Act
    let event_result = handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

    // Assert
    assert!(matches!(event_result, Ok(EventResult::Continue)));
    assert!(matches!(
        app.mode,
        AppMode::View {
            session_id: ref session_id_in_mode,
            scroll_offset: Some(3),
        } if session_id_in_mode == &session_id
    ));
    app.sessions.sync_from_handles();
    let output = session_replay_text(&app.sessions.sessions()[0]);
    assert!(
        output.contains("[Merge Error] The session has no open GitHub pull request"),
        "{output}"
    );
}

#[tokio::test]
async fn canceled_pull_request_merge_restores_the_view() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    app.mode = review_request_merge_confirmation(&session_id, false);

    // Act
    let event_result = handle_confirmation_decision(&mut app, ConfirmationDecision::Cancel).await;

    // Assert
    assert!(matches!(event_result, Ok(EventResult::Continue)));
    assert!(matches!(
        app.mode,
        AppMode::View {
            scroll_offset: Some(3),
            ..
        }
    ));
}

#[tokio::test]
async fn confirmed_pull_request_merge_without_a_session_returns_to_the_list() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    app.mode = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::MergeReviewRequest { bypass: false },
        confirmation_message: "Merge pull request #42 into `main`?".to_string(),
        confirmation_title: "Confirm Merge".to_string(),
        restore_view: None,
        session_id: None,
        selected_confirmation_index: 0,
    };

    // Act
    let event_result = handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

    // Assert
    assert!(matches!(event_result, Ok(EventResult::Continue)));
    assert!(matches!(app.mode, AppMode::List));
}
