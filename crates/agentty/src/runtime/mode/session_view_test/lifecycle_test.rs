use std::path::PathBuf;
use std::sync::Arc;

use tracing::instrument::WithSubscriber;

use super::super::{
    ViewContext, open_draft_prompt_with_pasted_image, show_diff_for_view_session, view_context,
    view_session_snapshot,
};
use super::support::{
    apply_next_session_diff, install_mock_clipboard_image_client, new_test_app_with_session,
};
use crate::domain::session::{SessionRole, Status};
use crate::infra::tmux::MockTmuxClient;
use crate::presentation::app_mode::AppMode;
use crate::presentation::help_action;
use crate::presentation::help_action::ViewSessionState;

#[tokio::test]
async fn test_view_session_snapshot_hides_worktree_open_for_unstarted_draft_session() {
    // Arrange
    let (mut app, _base_dir) =
        crate::test_support::new_git_test_app_with_tmux_client(Arc::new(MockTmuxClient::new()))
            .await;
    let session_id = app
        .create_draft_session()
        .await
        .expect("failed to create draft session");
    app.mode = AppMode::View {
        session_id: session_id.into(),
        scroll_offset: Some(1),
    };
    let context = view_context(&mut app).expect("expected view context");

    // Act
    let snapshot = view_session_snapshot(&app, &context).expect("expected view snapshot");

    // Assert
    assert!(!snapshot.can_open_worktree());
    assert_eq!(snapshot.session_state, ViewSessionState::NewSession);
    assert!(snapshot.can_paste_image_into_draft_composer());
    assert!(!snapshot.can_rebase_session());
}

#[tokio::test]
async fn test_view_session_snapshot_allows_start_for_stacked_draft() {
    // Arrange
    let (mut app, _base_dir, parent_session_id) = new_test_app_with_session().await;
    let session_id = app
        .create_draft_session()
        .await
        .expect("failed to create draft session");
    let parent_session = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == parent_session_id)
        .expect("expected parent session");
    parent_session.status = Status::Review;
    let session = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == session_id)
        .expect("expected draft session");
    session.parent_session_id = Some(parent_session_id.clone().into());
    session.prompt = "staged child draft".to_string();
    app.mode = AppMode::View {
        session_id: session_id.into(),
        scroll_offset: Some(1),
    };
    let context = view_context(&mut app).expect("expected view context");

    // Act
    let snapshot = view_session_snapshot(&app, &context).expect("expected view snapshot");

    // Assert
    assert_eq!(snapshot.session_state, ViewSessionState::StackedDraft);
    assert!(snapshot.can_start_staged_session());
    assert!(snapshot.can_paste_image_into_draft_composer());
    assert!(!snapshot.can_merge_session());
    assert!(!snapshot.can_rebase_session());
}

#[test]
fn test_view_session_state_maps_stacked_draft_status() {
    // Arrange
    let session = crate::test_support::SessionFixtureBuilder::new()
        .status(Status::Draft)
        .draft(true)
        .parent_session_id(Some("parent-session".into()))
        .folder(std::env::temp_dir())
        .project_name("")
        .build();

    // Act
    let state = help_action::session_view_state(&session);

    // Assert
    assert_eq!(state, ViewSessionState::StackedDraft);
}

#[tokio::test]
async fn managed_done_session_loads_archived_diff_without_worktree() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    let archived_diff = "diff --git a/worker.txt b/worker.txt\n+new work\n";
    app.services
        .db()
        .sessions()
        .update_session_archived_diff(&session_id, Some(archived_diff.to_string()))
        .await
        .expect("failed to persist archived diff");
    let session = &mut app.sessions.sessions_mut()[0];
    session.folder = PathBuf::from("missing-managed-worktree");
    session.role = SessionRole::OrchestrationWorker;
    session.status = Status::Done;
    let context = ViewContext {
        scroll_offset: Some(0),
        session_id: session_id.into(),
        session_index: 0,
    };

    // Act
    let opened = show_diff_for_view_session(&mut app, &context);
    apply_next_session_diff(&mut app).await;

    // Assert
    assert!(opened);
    assert!(matches!(
        app.mode,
        AppMode::Diff { ref diff, .. } if diff == archived_diff
    ));

    // Act
    app.services
        .db()
        .sessions()
        .update_session_archived_diff(&context.session_id, None)
        .await
        .expect("failed to clear archived diff");
    let missing_opened = show_diff_for_view_session(&mut app, &context);
    apply_next_session_diff(&mut app).await;

    // Assert
    assert!(missing_opened);
    assert!(matches!(app.mode, AppMode::View { .. }));
}

#[tokio::test]
async fn managed_done_session_restores_view_after_archived_diff_load_failure() {
    // Arrange
    let (mut app, _base_dir, pool) = crate::test_support::new_git_test_app_with_pool().await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    let session = &mut app.sessions.sessions_mut()[0];
    session.role = SessionRole::OrchestrationWorker;
    session.status = Status::Done;
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: Some(0),
    };
    let context = ViewContext {
        scroll_offset: Some(0),
        session_id: session_id.into(),
        session_index: 0,
    };
    pool.close().await;

    // Act
    let opened = show_diff_for_view_session(&mut app, &context);
    apply_next_session_diff(&mut app)
        .with_subscriber(crate::test_support::TestSubscriber)
        .await;

    // Assert
    assert!(opened);
    assert!(matches!(
        app.mode,
        AppMode::View {
            ref session_id,
            scroll_offset: Some(0),
            ..
        } if session_id == &context.session_id
    ));
    let workflow_notice = app.sessions.sessions()[0]
        .transient_messages
        .get(crate::domain::transient_message::TransientMessageSlot::WorkflowNotice)
        .expect("archived diff load failure should be visible in the restored view");
    assert!(
        workflow_notice
            .body
            .text()
            .contains("Failed to load archived diff:")
    );
}

#[tokio::test]
async fn canceled_research_session_loads_archived_diff_without_worktree() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    let archived_diff = "diff --git a/policy.txt b/policy.txt\n+unexpected write\n";
    app.services
        .db()
        .sessions()
        .update_session_archived_diff(&session_id, Some(archived_diff.to_string()))
        .await
        .expect("failed to persist archived diff");
    let session = &mut app.sessions.sessions_mut()[0];
    session.folder = PathBuf::from("reclaimed-research-worktree");
    session.role = SessionRole::OrchestrationResearcher;
    session.status = Status::Canceled;
    let context = ViewContext {
        scroll_offset: Some(0),
        session_id: session_id.into(),
        session_index: 0,
    };

    // Act
    let opened = show_diff_for_view_session(&mut app, &context);
    apply_next_session_diff(&mut app).await;

    // Assert
    assert!(opened);
    assert!(matches!(app.mode, AppMode::Diff { ref diff, .. } if diff == archived_diff));
}

#[tokio::test]
async fn test_open_draft_prompt_with_pasted_image_inserts_clipboard_image() {
    // Arrange
    let (mut app, _base_dir) =
        crate::test_support::new_git_test_app_with_tmux_client(Arc::new(MockTmuxClient::new()))
            .await;
    let session_id = app
        .create_draft_session()
        .await
        .expect("failed to create draft session");
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: Some(2),
    };
    let view_context = view_context(&mut app).expect("expected view context");
    let expected_session_id = session_id.clone();
    let expected_session_id_for_mock = expected_session_id.clone();
    let mut clipboard_image_client = crate::infra::clipboard_image::MockClipboardImageClient::new();
    clipboard_image_client
        .expect_persist_clipboard_image()
        .once()
        .withf(move |session_id, attachment_number| {
            session_id == &expected_session_id_for_mock && *attachment_number == 1
        })
        .returning(|_, _| {
            Box::pin(async {
                Ok(crate::infra::clipboard_image::PersistedClipboardImage {
                    local_image_path: std::path::PathBuf::from("/tmp/draft-image.png"),
                })
            })
        });
    install_mock_clipboard_image_client(&mut app, clipboard_image_client);

    // Act
    open_draft_prompt_with_pasted_image(&mut app, &view_context, Some(2)).await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Prompt {
            ref input,
            ref session_id,
            scroll_offset: Some(2),
            ..
        } if input.text() == "[Image #1]"
            && session_id.as_str() == expected_session_id.as_str()
    ));
}

#[tokio::test]
async fn test_open_draft_prompt_with_pasted_image_ignores_missing_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let view_context = ViewContext {
        scroll_offset: Some(2),
        session_id: "missing-session".into(),
        session_index: usize::MAX,
    };

    // Act
    open_draft_prompt_with_pasted_image(&mut app, &view_context, Some(2)).await;

    // Assert
    assert!(matches!(app.mode, AppMode::List));
}
