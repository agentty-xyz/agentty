//! Session-view and failure boundaries of the foreground event reducer.

use super::super::App;
use crate::app::branch_publish::{BranchPublishActionUpdate, BranchPublishTaskSuccess};
use crate::app::core::state::tests::support::{
    insert_review_session_with_data_dir, install_mock_git_client,
    merged_review_request_status_update, new_test_app_with_database_pool,
    test_prompt_mode_snapshot, test_review_request_summary,
};
use crate::app::session::SyncSessionStartError;
use crate::domain::file_entry::FileEntry;
use crate::domain::input::InputState;
use crate::domain::session::{PublishBranchAction, ReviewRequest, ReviewRequestState};
use crate::presentation::app_mode::{AppMode, ChatFocus, ConfirmationViewMode};
use crate::test_support;

#[tokio::test]
async fn session_composers_and_overlays_retain_their_viewed_session() {
    // Arrange
    let mut app = test_support::new_test_app_without_retained_base_dir().await;
    let restore_view = ConfirmationViewMode {
        scroll_offset: Some(3),
        session_id: "viewed-session".into(),
    };
    let modes = [
        test_prompt_mode_snapshot(restore_view.session_id.clone()).into_prompt_mode(),
        AppMode::Question {
            at_mention_state: None,
            current_index: 0,
            focus: ChatFocus::Input,
            input: InputState::default(),
            questions: Vec::new(),
            responses: Vec::new(),
            scroll_offset: None,
            selected_option_index: None,
            session_id: restore_view.session_id.clone(),
        },
        AppMode::LaunchConfigurationSelector {
            commands: vec!["run".to_string()],
            restore_view: restore_view.clone(),
            selected_command_index: 0,
        },
        AppMode::PublishBranchInput {
            default_branch_name: "topic".to_string(),
            input: InputState::default(),
            locked_upstream_ref: None,
            publish_branch_action: PublishBranchAction::Push,
            restore_view: restore_view.clone(),
        },
        App::view_info_popup_mode(
            "Review".to_string(),
            "Loading review".to_string(),
            true,
            "Loading".to_string(),
            restore_view,
        ),
    ];

    for mode in modes {
        // Act
        app.mode = mode;

        // Assert
        assert!(app.is_viewing_session("viewed-session"));
        assert!(!app.is_viewing_session("another-session"));
    }
}

#[tokio::test]
async fn prompt_entries_open_only_for_an_active_query_and_reset_selection() {
    // Arrange
    let mut app = test_support::new_test_app_without_retained_base_dir().await;
    let mut prompt = test_prompt_mode_snapshot("prompt-session".into());
    app.mode = prompt.clone().into_prompt_mode();
    let entries = vec![FileEntry {
        is_dir: false,
        path: "src/main.rs".to_string(),
    }];

    // Act
    app.apply_prompt_at_mention_entries("prompt-session", entries.clone());

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Prompt {
            at_mention_state: None,
            ..
        }
    ));

    // Arrange
    prompt.input = InputState::with_text("@src".to_string());
    app.mode = prompt.into_prompt_mode();

    // Act
    app.apply_prompt_at_mention_entries("prompt-session", entries.clone());

    // Assert
    assert!(
        matches!(&app.mode, AppMode::Prompt { at_mention_state: Some(state), .. }
        if state.all_entries == entries)
    );

    // Arrange
    if let AppMode::Prompt {
        at_mention_state: Some(state),
        ..
    } = &mut app.mode
    {
        state.selected_index = 4;
    }

    // Act
    app.apply_prompt_at_mention_entries("prompt-session", Vec::new());

    // Assert
    assert!(
        matches!(app.mode, AppMode::Prompt { at_mention_state: Some(ref state), .. }
        if state.all_entries.is_empty() && state.selected_index == 0)
    );
}

#[tokio::test]
async fn merged_review_persistence_warning_reaches_the_session_transcript() {
    // Arrange
    let (mut app, pool, _base_dir) = new_test_app_with_database_pool().await;
    let session_id = "merged-warning";
    insert_review_session_with_data_dir(&app, session_id).await;
    app.refresh_sessions_now().await;
    sqlx::query!(
        "CREATE TRIGGER fail_merged_hash BEFORE UPDATE OF merged_commit_hash ON session BEGIN \
         SELECT RAISE(FAIL, 'merged hash failed'); END"
    )
    .execute(&pool)
    .await
    .expect("failure trigger should install");

    // Act
    app.apply_review_request_status_update(merged_review_request_status_update(
        session_id, "#42", "abc1234", "main",
    ))
    .await;

    // Assert
    let messages = app
        .services
        .db()
        .sessions()
        .load_session_messages(session_id)
        .await
        .expect("messages should load");
    assert!(messages.iter().any(|message| {
        message
            .content
            .contains("Merged commit hash persistence failed")
    }));
}

#[tokio::test]
async fn failed_merged_worktree_cleanup_persists_a_warning() {
    // Arrange
    let (mut app, _pool, _base_dir) = new_test_app_with_database_pool().await;
    let session_id = "cleanup-warning";
    insert_review_session_with_data_dir(&app, session_id).await;
    app.refresh_sessions_now().await;
    let mut git_client = ag_git::MockGitClient::new();
    git_client.expect_main_repo_root().once().returning(|_| {
        Box::pin(async {
            Err(ag_git::GitError::OutputParse(
                "cleanup unavailable".to_string(),
            ))
        })
    });
    git_client.expect_remove_worktree().once().returning(|_| {
        Box::pin(async {
            Err(ag_git::GitError::OutputParse(
                "cleanup unavailable".to_string(),
            ))
        })
    });
    install_mock_git_client(&mut app, git_client);
    let folder = app
        .sessions
        .session_or_err(session_id)
        .expect("session should exist")
        .folder
        .clone();
    let handles = app
        .sessions
        .session_handles_or_err(session_id)
        .expect("handles should exist");

    // Act
    app.spawn_externally_merged_session_cleanup(session_id, folder, "topic".to_string(), handles);
    app.services.wait_for_cleanup_tasks().await;

    // Assert
    let messages = app
        .services
        .db()
        .sessions()
        .load_session_messages(session_id)
        .await
        .expect("messages should load");
    assert!(messages.iter().any(|message| {
        message.content.contains("Worktree cleanup failed:")
            && message.content.contains("cleanup unavailable")
    }));
}

#[tokio::test]
async fn review_results_for_a_removed_session_leave_loaded_sessions_unchanged() {
    // Arrange
    let mut app = test_support::new_test_app_without_retained_base_dir().await;
    let review_request = ReviewRequest {
        last_refreshed_at: 0,
        summary: test_review_request_summary("#42", ReviewRequestState::Open),
    };

    // Act
    app.cancel_externally_closed_session("removed-session")
        .await;
    app.apply_branch_publish_action_update(BranchPublishActionUpdate {
        result: Ok(BranchPublishTaskSuccess::PullRequestPublished {
            branch_name: "topic".to_string(),
            review_request,
            upstream_reference: "origin/topic".to_string(),
        }),
        session_id: "removed-session".into(),
    })
    .await;

    // Assert
    assert!(app.sessions.state().sessions().is_empty());
    assert!(matches!(app.mode, AppMode::List));
}

#[test]
fn sync_authentication_failures_explain_how_to_reauthenticate_and_retry() {
    // Arrange
    let failure = SyncSessionStartError::Other(
        "Git push failed: fatal: Authentication failed for 'https://github.com/example/repo.git'"
            .to_string(),
    );

    // Act
    let message = App::sync_failure_message(&failure);

    // Assert
    assert!(message.contains("gh auth login"));
    assert!(message.contains("run sync again"));
}
