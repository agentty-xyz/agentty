use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use mockall::predicate::eq;

use super::super::{
    ViewActionState, ViewContext, ViewPendingUpdate, ViewSessionSnapshot, confirmation_view_mode,
    end_in_progress_turn, handle_open_worktree_key, handle_primary_view_key,
    open_view_help_overlay, open_worktree_for_view_session, show_diff_for_view_session,
    view_context, view_session_snapshot,
};
use super::support::{
    apply_pending_session_diff, attach_open_review_request, handle, new_test_app_with_session,
    new_test_app_with_session_and_tmux_client, reply_enabled_review_snapshot,
};
use crate::domain::orchestration::OrchestrationStatus;
use crate::domain::session::{
    PublishBranchAction, SessionDiffState, SessionId, SessionRole, Status,
};
use crate::infra::tmux::MockTmuxClient;
use crate::presentation::app_mode::{
    AppMode, ConfirmationIntent, ConfirmationViewMode, HelpContext,
};
use crate::presentation::help_action::ViewSessionState;
use crate::runtime::EventResult;
use crate::runtime::mode::confirmation::DEFAULT_OPTION_INDEX;

#[tokio::test]
async fn test_view_context_returns_none_for_non_view_mode() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
    app.mode = AppMode::List;

    // Act
    let context = view_context(&mut app);

    // Assert
    assert!(context.is_none());
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_view_context_falls_back_to_list_when_session_is_missing() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
    app.mode = AppMode::View {
        session_id: "missing-session".into(),
        scroll_offset: Some(2),
    };

    // Act
    let context = view_context(&mut app);

    // Assert
    assert!(context.is_none());
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_view_context_returns_existing_session_details() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: Some(4),
    };

    // Act
    let context = view_context(&mut app);

    // Assert
    assert!(context.is_some());
    let context = context.expect("expected view context");
    assert_eq!(context.session_id, session_id);
    assert_eq!(context.scroll_offset, Some(4));
    assert_eq!(context.session_index, 0);
}

#[tokio::test]
async fn test_linked_canceled_session_routes_c_to_continue_without_comments() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    let session = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == session_id)
        .expect("session should exist");
    attach_open_review_request(session);
    session.status = Status::Canceled;
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: Some(0),
    };
    let view_context = view_context(&mut app).expect("expected view context");
    let pending_update = ViewPendingUpdate::from_context(&view_context);
    let view_session_snapshot =
        view_session_snapshot(&app, &view_context).expect("expected session snapshot");

    // Act
    let continue_result = handle_primary_view_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
        &view_context,
        &view_session_snapshot,
        &pending_update,
    )
    .await;

    // Assert
    assert!(!view_session_snapshot.can_open_review_comments());
    assert_eq!(continue_result, Some(false));
    assert!(matches!(
        app.mode,
        AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::ContinueSession,
            ..
        }
    ));
}

#[tokio::test]
async fn test_q_always_transitions_to_list() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    let view_context = ViewContext {
        scroll_offset: Some(10),
        session_id: session_id.into(),
        session_index: 0,
    };
    let pending_update = ViewPendingUpdate::from_context(&view_context);

    // Act
    app.mode = AppMode::List;

    // Assert
    assert!(matches!(app.mode, AppMode::List));
    assert_eq!(pending_update.scroll_offset, Some(10));
}

#[tokio::test]
async fn diff_snapshot_keeps_managed_worker_inspection_without_controller_action() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    let session = &mut app.sessions.sessions_mut()[0];
    session.role = SessionRole::Orchestrator;
    session.status = Status::Review;
    app.mode = AppMode::View {
        session_id: session_id.into(),
        scroll_offset: Some(1),
    };
    let context = view_context(&mut app).expect("expected view context");

    // Act
    let controller_snapshot =
        view_session_snapshot(&app, &context).expect("expected controller snapshot");
    app.sessions.sessions_mut()[0].role = SessionRole::OrchestrationWorker;
    let managed_worker_snapshot =
        view_session_snapshot(&app, &context).expect("expected managed worker snapshot");

    // Assert
    assert_eq!(controller_snapshot.inspect_diff, ViewActionState::Disabled);
    assert_eq!(
        managed_worker_snapshot.inspect_diff,
        ViewActionState::Enabled
    );
}

#[tokio::test]
async fn diff_snapshot_hides_known_empty_session_diff() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    let session = &mut app.sessions.sessions_mut()[0];
    session.status = Status::Review;
    session.stats.diff_state = SessionDiffState::Empty;
    app.mode = AppMode::View {
        session_id: session_id.into(),
        scroll_offset: Some(1),
    };
    let context = view_context(&mut app).expect("expected view context");

    // Act
    let snapshot = view_session_snapshot(&app, &context).expect("expected view snapshot");

    // Assert
    assert_eq!(snapshot.inspect_diff, ViewActionState::Disabled);
}

#[tokio::test]
async fn test_open_view_help_overlay_preserves_view_context() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    let view_context = ViewContext {
        scroll_offset: Some(3),
        session_id: session_id.clone().into(),
        session_index: 0,
    };
    let view_session_snapshot = ViewSessionSnapshot {
        branch_actions: ViewActionState::Enabled,
        continue_terminal_session: ViewActionState::Disabled,
        fork_session: ViewActionState::Enabled,
        inspect_diff: ViewActionState::Enabled,
        is_managed: false,
        is_orchestrator: false,
        merge_session_branch: ViewActionState::Enabled,
        mutate_session_branch: ViewActionState::Enabled,
        rebase_session_branch: ViewActionState::Enabled,
        open_worktree: ViewActionState::Enabled,
        reply_to_session: ViewActionState::Enabled,
        review_comments: ViewActionState::Disabled,
        start_staged_session: ViewActionState::Disabled,
        follow_up_task_action: None,
        publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
        session_state: ViewSessionState::Review,
        session_status: Status::Review,
    };

    // Act
    open_view_help_overlay(&mut app, &view_context, &view_session_snapshot);

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Help {
            context: HelpContext::View {
                can_fork_session: true,
                can_merge_session_branch: true,
                can_mutate_session_branch: true,
                can_open_worktree: true,
                can_start_staged_session: false,
                publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
                session_id: ref session_id_in_mode,
                session_state: ViewSessionState::Review,
                scroll_offset: Some(3),
                ..
            },
            scroll_offset: 0,
        } if session_id_in_mode == &session_id
    ));
}

#[tokio::test]
async fn test_show_diff_for_view_session_keeps_view_mode_when_diff_is_empty() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: Some(0),
    };
    let context = ViewContext {
        scroll_offset: Some(0),
        session_id: session_id.clone().into(),
        session_index: 0,
    };

    // Act
    let opened = show_diff_for_view_session(&mut app, &context);
    let loading = matches!(app.mode, AppMode::DiffLoading { .. });
    apply_pending_session_diff(&mut app, &context.session_id, Ok("")).await;

    // Assert
    assert!(opened);
    assert!(loading);
    assert!(matches!(
        app.mode,
        AppMode::View {
            ref session_id,
            scroll_offset: Some(0),
            ..
        } if session_id == &context.session_id
    ));
}

#[tokio::test]
async fn test_show_diff_for_view_session_restores_view_after_git_error() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: Some(0),
    };
    let context = ViewContext {
        scroll_offset: Some(0),
        session_id: session_id.clone().into(),
        session_index: 0,
    };

    // Act
    let opened = show_diff_for_view_session(&mut app, &context);
    apply_pending_session_diff(
        &mut app,
        &context.session_id,
        Err("Failed to run git diff: repository unavailable"),
    )
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
        .expect("diff load failure should be visible in the restored session view");
    assert!(workflow_notice.body.text().contains("Unable to load diff:"));
    assert!(
        workflow_notice
            .body
            .text()
            .contains("Failed to run git diff:")
    );
}

/// Verifies a stale view index cannot start a background diff load.
#[tokio::test]
async fn test_show_diff_for_view_session_rejects_stale_session_index() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    let context = ViewContext {
        scroll_offset: Some(0),
        session_id: session_id.into(),
        session_index: 99,
    };

    // Act
    let opened = show_diff_for_view_session(&mut app, &context);

    // Assert
    assert!(!opened);
    assert!(!matches!(app.mode, AppMode::DiffLoading { .. }));
}

#[tokio::test]
async fn test_view_session_snapshot_disables_actions_for_done_session() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.sessions.sessions_mut()[0].status = Status::Done;
    app.mode = AppMode::View {
        session_id: session_id.into(),
        scroll_offset: Some(1),
    };
    let context = view_context(&mut app).expect("expected view context");

    // Act
    let snapshot = view_session_snapshot(&app, &context).expect("expected view snapshot");

    // Assert
    assert!(snapshot.can_continue_terminal_session());
    assert!(!snapshot.can_open_worktree());
    assert_eq!(snapshot.session_state, ViewSessionState::Done);
    assert_eq!(snapshot.session_status, Status::Done);
}

#[tokio::test]
async fn test_view_session_snapshot_enables_continue_for_canceled_session() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.sessions.sessions_mut()[0].status = Status::Canceled;
    app.mode = AppMode::View {
        session_id: session_id.into(),
        scroll_offset: Some(1),
    };
    let context = view_context(&mut app).expect("expected view context");

    // Act
    let snapshot = view_session_snapshot(&app, &context).expect("expected view snapshot");

    // Assert
    assert!(snapshot.can_continue_terminal_session());
    assert!(!snapshot.can_open_worktree());
    assert_eq!(snapshot.session_state, ViewSessionState::Canceled);
    assert_eq!(snapshot.session_status, Status::Canceled);
}

#[tokio::test]
async fn test_view_session_snapshot_blocks_parent_reply_with_running_child() {
    // Arrange
    let (mut app, _base_dir, parent_session_id) = new_test_app_with_session().await;
    let child_session_id = app
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
    let child_session = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == child_session_id)
        .expect("expected child session");
    child_session.parent_session_id = Some(parent_session_id.clone().into());
    child_session.status = Status::InProgress;
    app.mode = AppMode::View {
        session_id: parent_session_id.clone().into(),
        scroll_offset: Some(1),
    };
    let context = view_context(&mut app).expect("expected view context");

    // Act
    let snapshot = view_session_snapshot(&app, &context).expect("expected view snapshot");

    // Assert
    assert_eq!(snapshot.session_state, ViewSessionState::Review);
    assert!(!snapshot.can_open_prompt_composer());
    assert!(!snapshot.can_merge_session());
    assert!(!snapshot.can_rebase_session());
}

#[tokio::test]
async fn test_view_session_snapshot_reads_cached_worktree_availability() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.sessions
        .set_session_worktree_available(&session_id, false);
    app.mode = AppMode::View {
        session_id: session_id.into(),
        scroll_offset: Some(1),
    };
    let context = view_context(&mut app).expect("expected view context");

    // Act
    let snapshot = view_session_snapshot(&app, &context).expect("expected view snapshot");

    // Assert
    assert!(!snapshot.can_open_worktree());
}

#[tokio::test]
async fn test_view_session_snapshot_returns_none_for_stale_session_index() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.mode = AppMode::View {
        session_id: session_id.into(),
        scroll_offset: Some(1),
    };
    let mut context = view_context(&mut app).expect("expected view context");
    context.session_index = 99;

    // Act
    let snapshot = view_session_snapshot(&app, &context);

    // Assert
    assert!(snapshot.is_none());
}

#[tokio::test]
async fn regular_worktree_open_skips_warning_and_opens_selector() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_test_app().await;
    app.settings.launch_configuration = "cargo test\nnpm run dev".to_string();
    let view_context = ViewContext {
        scroll_offset: Some(2),
        session_id: SessionId::from("regular-worker"),
        session_index: 0,
    };
    let snapshot = reply_enabled_review_snapshot();

    // Act
    let should_apply_pending_update =
        handle_open_worktree_key(&mut app, &view_context, &snapshot).await;

    // Assert
    assert!(should_apply_pending_update);
    assert!(matches!(
        app.mode,
        AppMode::LaunchConfigurationSelector {
            ref commands,
            restore_view: ConfirmationViewMode {
                scroll_offset: Some(2),
                ref session_id,
            },
            selected_command_index: 0,
        } if commands == &["cargo test".to_string(), "npm run dev".to_string()]
            && session_id == "regular-worker"
    ));
}

#[tokio::test]
async fn test_open_worktree_for_view_session_opens_command_selector_for_multiple_commands() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.settings.launch_configuration = "cargo test\nnpm run dev".to_string();
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: Some(4),
    };
    let context = view_context(&mut app).expect("expected view context");

    // Act
    open_worktree_for_view_session(&mut app, confirmation_view_mode(&context)).await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::LaunchConfigurationSelector {
            ref commands,
            restore_view:
                ConfirmationViewMode {
                    session_id: ref restored_session_id,
        scroll_offset: Some(4),
                },
            selected_command_index: 0,
        } if commands == &vec!["cargo test".to_string(), "npm run dev".to_string()]
            && restored_session_id == &session_id
    ));
}

#[tokio::test]
async fn test_open_worktree_for_view_session_keeps_view_mode_for_single_command() {
    // Arrange
    let mut mock_tmux_client = MockTmuxClient::new();
    mock_tmux_client
        .expect_open_window_for_folder()
        .times(1)
        .returning(|_| Box::pin(async { Some("@42".to_string()) }));
    mock_tmux_client
        .expect_run_command_in_window()
        .with(eq("@42".to_string()), eq("cargo test".to_string()))
        .times(1)
        .returning(|_, _| Box::pin(async {}));
    let (mut app, _base_dir, session_id) =
        new_test_app_with_session_and_tmux_client(Arc::new(mock_tmux_client)).await;
    app.settings.launch_configuration = "cargo test".to_string();
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: Some(2),
    };
    let context = view_context(&mut app).expect("expected view context");

    // Act
    open_worktree_for_view_session(&mut app, confirmation_view_mode(&context)).await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::View {
            session_id: ref mode_session_id,
        scroll_offset: Some(2),
        } if mode_session_id == &session_id
    ));
}

#[tokio::test]
async fn test_linked_done_session_routes_c_to_continue_without_comments() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    let session = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == session_id)
        .expect("session should exist");
    attach_open_review_request(session);
    session.status = Status::Done;
    app.mode = AppMode::View {
        session_id: session_id.clone().into(),
        scroll_offset: Some(0),
    };
    let view_context = view_context(&mut app).expect("expected view context");
    let pending_update = ViewPendingUpdate::from_context(&view_context);
    let view_session_snapshot =
        view_session_snapshot(&app, &view_context).expect("expected session snapshot");

    // Act
    let uppercase_result = handle_primary_view_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('C'), KeyModifiers::SHIFT),
        &view_context,
        &view_session_snapshot,
        &pending_update,
    )
    .await;

    // Assert
    assert_eq!(uppercase_result, None);
    assert!(!view_session_snapshot.can_open_review_comments());
    assert!(matches!(app.mode, AppMode::View { .. }));

    // Act
    let continue_result = handle_primary_view_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
        &view_context,
        &view_session_snapshot,
        &pending_update,
    )
    .await;

    // Assert
    assert_eq!(continue_result, Some(false));
    assert!(matches!(
        app.mode,
        AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::ContinueSession,
            ..
        }
    ));
}

#[tokio::test]
async fn campaign_and_managed_worker_keys_route_through_primary_view_actions() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_test_app().await;
    let view_context = ViewContext {
        scroll_offset: Some(2),
        session_id: SessionId::from("campaign"),
        session_index: 0,
    };
    let pending_update = ViewPendingUpdate::from_context(&view_context);
    let mut snapshot = reply_enabled_review_snapshot();
    snapshot.is_orchestrator = true;
    let campaign_keys = ['a'];

    // Act
    let mut results = Vec::new();
    for key in campaign_keys {
        results.push(
            handle_primary_view_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
                &view_context,
                &snapshot,
                &pending_update,
            )
            .await,
        );
    }
    let unknown_campaign_key = handle_primary_view_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        &view_context,
        &snapshot,
        &pending_update,
    )
    .await;
    snapshot.is_orchestrator = false;
    snapshot.is_managed = true;
    let unrelated = handle_primary_view_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        &view_context,
        &snapshot,
        &pending_update,
    )
    .await;
    let direct_cancel = handle_primary_view_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        &view_context,
        &snapshot,
        &pending_update,
    )
    .await;
    let detach = handle_primary_view_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT),
        &view_context,
        &snapshot,
        &pending_update,
    )
    .await;

    // Assert
    assert_eq!(results, vec![Some(true); campaign_keys.len()]);
    assert_eq!(unknown_campaign_key, None);
    assert_eq!(unrelated, None);
    assert_eq!(direct_cancel, Some(false));
    assert_eq!(detach, Some(false));
    assert!(matches!(
        app.mode,
        AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::DetachManagedSession,
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_continue_key_opens_confirmation_for_done_session() {
    // Arrange
    let (mut app, _base_dir, source_session_id) = new_test_app_with_session().await;
    let source_session = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == source_session_id)
        .expect("expected source session in session list");
    source_session.status = Status::Done;
    source_session.title = Some("Done source".to_string());
    app.mode = AppMode::View {
        session_id: source_session_id.clone().into(),
        scroll_offset: Some(0),
    };
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    let result = handle(
        &mut app,
        &mut terminal,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
    )
    .await
    .expect("continue key should be handled");

    // Assert
    assert!(matches!(result, EventResult::Continue));
    assert!(matches!(
        app.mode,
        AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::ContinueSession,
            ref confirmation_title,
            ref confirmation_message,
            ref restore_view,
            ref session_id,
            ..
        } if confirmation_title == "Confirm Continue"
            && confirmation_message
                == "Create a new draft session with initial context from this session?"
            && matches!(restore_view, Some(restore_view) if restore_view.session_id == source_session_id)
            && matches!(session_id, Some(session_id) if session_id.as_str() == source_session_id)
    ));
}

#[tokio::test]
async fn test_handle_continue_key_opens_confirmation_for_canceled_session() {
    // Arrange
    let (mut app, _base_dir, source_session_id) = new_test_app_with_session().await;
    app.sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == source_session_id)
        .expect("expected source session")
        .status = Status::Canceled;
    app.mode = AppMode::View {
        session_id: source_session_id.clone().into(),
        scroll_offset: Some(0),
    };
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    let result = handle(
        &mut app,
        &mut terminal,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
    )
    .await
    .expect("c key should be handled");

    // Assert
    assert!(matches!(result, EventResult::Continue));
    assert!(matches!(
        app.mode,
        AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::ContinueSession,
            ref session_id,
            ..
        } if matches!(session_id, Some(session_id) if session_id.as_str() == source_session_id)
    ));
}

#[tokio::test]
async fn integration_approval_opens_approach_choice() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.services
        .db()
        .orchestrations()
        .insert_orchestration(
            &session_id,
            &OrchestrationStatus::AwaitingIntegration.to_string(),
            2,
        )
        .await
        .expect("failed to insert orchestration");
    let view_context = ViewContext {
        scroll_offset: Some(2),
        session_id: session_id.clone().into(),
        session_index: 0,
    };
    let pending_update = ViewPendingUpdate::from_context(&view_context);
    let mut snapshot = reply_enabled_review_snapshot();
    snapshot.is_orchestrator = true;

    // Act
    let result = handle_primary_view_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        &view_context,
        &snapshot,
        &pending_update,
    )
    .await;

    // Assert
    assert_eq!(result, Some(true));
    assert!(matches!(
        app.mode,
        AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::ChooseIntegrationApproach,
            ref confirmation_title,
            restore_view: Some(ConfirmationViewMode {
                scroll_offset: Some(2),
                ref session_id,
            }),
            selected_confirmation_index: 0,
            ..
        } if confirmation_title == "Integration Approach" && session_id == &view_context.session_id
    ));
}

#[tokio::test]
async fn managed_running_session_snapshot_hides_worktree_open() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    let session = &mut app.sessions.sessions_mut()[0];
    session.role = SessionRole::OrchestrationWorker;
    session.status = Status::InProgress;
    app.mode = AppMode::View {
        session_id: session_id.into(),
        scroll_offset: Some(1),
    };
    let context = view_context(&mut app).expect("expected view context");

    // Act
    let snapshot = view_session_snapshot(&app, &context).expect("expected view snapshot");

    // Assert
    assert!(!snapshot.can_open_worktree());
}

#[tokio::test]
async fn test_handle_ignores_key_when_mode_is_not_view() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
    app.mode = AppMode::List;
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    let result = handle(
        &mut app,
        &mut terminal,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
    )
    .await
    .expect("non-view key should be handled");

    // Assert
    assert!(matches!(result, EventResult::Continue));
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn managed_worktree_open_requires_write_access_confirmation() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_test_app().await;
    let view_context = ViewContext {
        scroll_offset: Some(2),
        session_id: SessionId::from("managed-worker"),
        session_index: 0,
    };
    let pending_update = ViewPendingUpdate::from_context(&view_context);
    let mut snapshot = reply_enabled_review_snapshot();
    snapshot.is_managed = true;

    // Act
    let result = handle_primary_view_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE),
        &view_context,
        &snapshot,
        &pending_update,
    )
    .await;

    // Assert
    assert_eq!(result, Some(false));
    assert!(matches!(
        app.mode,
        AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::OpenManagedWorktree,
            ref confirmation_message,
            ref confirmation_title,
            restore_view: Some(ConfirmationViewMode {
                scroll_offset: Some(2),
                ref session_id,
            }),
            selected_confirmation_index: DEFAULT_OPTION_INDEX,
            ..
        } if confirmation_title == "Open Managed Worktree"
            && confirmation_message.contains("writable shell")
            && session_id == "managed-worker"
    ));
}

#[tokio::test]
async fn test_end_in_progress_turn_does_not_send_sigterm_directly() {
    // Arrange — spawn a child and store its PID in the handles.
    // SIGTERM is now sent by the worker's cancellation path, not
    // `end_in_progress_turn`, so the child should remain alive.
    let mut child = tokio::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("failed to spawn sleep");
    let child_pid = child.id().expect("child has no pid");

    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.sessions.sessions_mut()[0].status = Status::InProgress;
    let _ = app
        .services
        .db()
        .sessions()
        .update_session_status_with_timing_at(&session_id, &Status::InProgress.to_string(), 0)
        .await;
    let handles = crate::domain::session::SessionHandles::new(Status::InProgress);
    if let Ok(mut guard) = handles.child_pid.lock() {
        *guard = Some(child_pid);
    }
    app.sessions
        .session_handles_mut()
        .insert(session_id.clone().into(), handles);

    // Act
    end_in_progress_turn(&mut app, &session_id).await;

    // Assert — child is still alive because the UI no longer sends
    // SIGTERM; the worker owns process termination.
    assert!(
        child.try_wait().expect("try_wait failed").is_none(),
        "child should still be running — UI must not send SIGTERM"
    );

    // Cleanup
    child.kill().await.expect("failed to kill child");
}

#[tokio::test]
async fn test_end_in_progress_turn_cancels_token() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    app.sessions.sessions_mut()[0].status = Status::InProgress;
    let _ = app
        .services
        .db()
        .sessions()
        .update_session_status_with_timing_at(&session_id, &Status::InProgress.to_string(), 0)
        .await;
    let handles = crate::domain::session::SessionHandles::new(Status::InProgress);
    let cancel_token = std::sync::Arc::clone(&handles.cancel_token);
    app.sessions
        .session_handles_mut()
        .insert(session_id.clone().into(), handles);

    // Act
    end_in_progress_turn(&mut app, &session_id).await;

    // Assert — the token must be cancelled so the worker's `select!`
    // branch fires.
    let is_cancelled = cancel_token
        .lock()
        .expect("cancel token lock")
        .is_cancelled();
    assert!(
        is_cancelled,
        "cancel_token should be cancelled by end_in_progress_turn"
    );
}
