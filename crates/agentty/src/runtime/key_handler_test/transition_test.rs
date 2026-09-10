use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use mockall::predicate::eq;
use ratatui::layout::Rect;

use super::super::{
    append_session_to_selected_stack, content_area_for_terminal,
    handle_cancel_session_confirmation, handle_confirmation_decision,
    handle_launch_configuration_selector_key, handle_merge_confirmation,
    handle_open_managed_worktree_confirmation, handle_pre_commit_hook_warning_key,
    handle_regenerate_review_confirmation, handle_stack_append_parent_key,
    handle_view_info_popup_key, next_launch_configuration_index,
    previous_launch_configuration_index, stack_append_parent_session_ids,
    update_stack_append_parent_selection,
};
use super::support::appendable_stack_test_app;
use crate::domain::session::SessionId;
use crate::infra::tmux::MockTmuxClient;
use crate::presentation::app_mode::{AppMode, ConfirmationIntent, ConfirmationViewMode};
use crate::runtime::EventResult;
use crate::runtime::mode::confirmation::ConfirmationDecision;

#[tokio::test]
async fn integration_choice_without_session_returns_to_restore_target() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
    app.mode = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::ChooseIntegrationApproach,
        confirmation_message: "Choose integration".to_string(),
        confirmation_title: "Integration Approach".to_string(),
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

#[tokio::test]
async fn test_handle_launch_configuration_selector_key_escape_restores_view_mode() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
    app.mode = AppMode::LaunchConfigurationSelector {
        commands: vec!["cargo test".to_string(), "npm run dev".to_string()],
        restore_view: ConfirmationViewMode {
            scroll_offset: Some(3),
            session_id: "session-id".into(),
        },
        selected_command_index: 1,
    };

    // Act
    let event_result = handle_launch_configuration_selector_key(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(event_result, Ok(EventResult::Continue)));
    assert!(matches!(
        app.mode,
        AppMode::View {
            ref session_id,
            scroll_offset: Some(3),
        } if session_id == "session-id"
    ));
}

#[tokio::test]
async fn test_handle_launch_configuration_selector_key_with_empty_commands_keeps_index_zero() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
    app.mode = AppMode::LaunchConfigurationSelector {
        commands: Vec::new(),
        restore_view: ConfirmationViewMode {
            scroll_offset: None,
            session_id: "session-id".into(),
        },
        selected_command_index: 0,
    };

    // Act
    let event_result = handle_launch_configuration_selector_key(
        &mut app,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(event_result, Ok(EventResult::Continue)));
    assert!(matches!(
        app.mode,
        AppMode::LaunchConfigurationSelector {
            selected_command_index: 0,
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_launch_configuration_selector_key_enter_restores_view_without_session() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
    app.mode = AppMode::LaunchConfigurationSelector {
        commands: vec!["cargo test".to_string()],
        restore_view: ConfirmationViewMode {
            scroll_offset: Some(4),
            session_id: "session-id".into(),
        },
        selected_command_index: 0,
    };

    // Act
    let event_result = handle_launch_configuration_selector_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(event_result, Ok(EventResult::Continue)));
    assert!(matches!(
        app.mode,
        AppMode::View {
            ref session_id,
            scroll_offset: Some(4),
        } if session_id == "session-id"
    ));
}

#[tokio::test]
async fn test_handle_launch_configuration_selector_key_enter_runs_selected_command_in_tmux() {
    // Arrange
    let mut mock_tmux_client = MockTmuxClient::new();
    mock_tmux_client
        .expect_open_window_for_folder()
        .times(1)
        .returning(|_| Box::pin(async { Some("@24".to_string()) }));
    mock_tmux_client
        .expect_run_command_in_window()
        .with(eq("@24".to_string()), eq("npm run dev".to_string()))
        .times(1)
        .returning(|_, _| Box::pin(async {}));
    let (mut app, _base_dir) =
        crate::test_support::new_git_test_app_with_tmux_client(Arc::new(mock_tmux_client)).await;
    let expected_session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    app.mode = AppMode::LaunchConfigurationSelector {
        commands: vec!["cargo test".to_string(), "npm run dev".to_string()],
        restore_view: ConfirmationViewMode {
            scroll_offset: Some(2),
            session_id: expected_session_id.clone().into(),
        },
        selected_command_index: 1,
    };

    // Act
    let event_result = handle_launch_configuration_selector_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(event_result, Ok(EventResult::Continue)));
    assert!(matches!(
        app.mode,
        AppMode::View {
            ref session_id,
            scroll_offset: Some(2),
        } if session_id == &expected_session_id
    ));
}

#[tokio::test]
async fn test_handle_launch_configuration_selector_key_unknown_key_preserves_state() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
    app.mode = AppMode::LaunchConfigurationSelector {
        commands: vec!["cargo test".to_string(), "npm run dev".to_string()],
        restore_view: ConfirmationViewMode {
            scroll_offset: Some(1),
            session_id: "session-id".into(),
        },
        selected_command_index: 1,
    };

    // Act
    let event_result = handle_launch_configuration_selector_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(event_result, Ok(EventResult::Continue)));
    assert!(matches!(
        app.mode,
        AppMode::LaunchConfigurationSelector {
            selected_command_index: 1,
            ref commands,
            restore_view:
                ConfirmationViewMode {
                    scroll_offset: Some(1),
                    ref session_id,
                },
        } if commands == &vec!["cargo test".to_string(), "npm run dev".to_string()]
            && session_id == "session-id"
    ));
}

#[tokio::test]
async fn test_cancel_session_confirmation_tolerates_a_missing_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;

    // Act
    let event_result =
        handle_cancel_session_confirmation(&mut app, Some("missing-session".into())).await;

    // Assert
    assert!(matches!(event_result, Ok(EventResult::Continue)));
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_handle_view_info_popup_key_restores_view_mode() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
    app.mode = AppMode::ViewInfoPopup {
        is_loading: false,
        loading_label: "Refreshing review request...".to_string(),
        message: "Review request refreshed.".to_string(),
        restore_view: ConfirmationViewMode {
            scroll_offset: Some(2),
            session_id: "session-id".into(),
        },
        title: "Review request refreshed".to_string(),
    };

    // Act
    let event_result =
        handle_view_info_popup_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    // Assert
    assert!(matches!(event_result, EventResult::Continue));
    assert!(matches!(
        app.mode,
        AppMode::View {
            ref session_id,
            scroll_offset: Some(2),
            ..
        } if session_id == "session-id"
    ));
}

#[tokio::test]
async fn detach_confirmation_restores_view_with_or_without_a_session_target() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::DetachManagedSession,
        confirmation_message: "Detach?".to_string(),
        confirmation_title: "Confirm Detach".to_string(),
        restore_view: Some(ConfirmationViewMode {
            scroll_offset: Some(3),
            session_id: SessionId::from("worker"),
        }),
        session_id: None,
        selected_confirmation_index: 0,
    };

    // Act
    let without_target =
        handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

    // Assert
    assert!(matches!(without_target, Ok(EventResult::Continue)));
    assert!(matches!(
        app.mode,
        AppMode::View {
            scroll_offset: Some(3),
            ref session_id,
        } if session_id == "worker"
    ));

    // Arrange
    app.mode = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::DetachManagedSession,
        confirmation_message: "Detach?".to_string(),
        confirmation_title: "Confirm Detach".to_string(),
        restore_view: None,
        session_id: Some(SessionId::from("missing-worker")),
        selected_confirmation_index: 0,
    };
    let with_target = handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

    // Assert
    assert!(matches!(with_target, Ok(EventResult::Continue)));
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_pre_commit_warning_escape_returns_to_session_list() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::PreCommitHookWarning {
        message: "Missing pre-commit hook".to_string(),
    };

    // Act
    let result = handle_pre_commit_hook_warning_key(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(result, EventResult::Continue));
    assert!(matches!(app.mode, AppMode::List));
}

#[test]
fn test_next_launch_configuration_index_wraps_to_start() {
    // Arrange
    let commands = vec!["cargo test".to_string(), "npm run dev".to_string()];

    // Act
    let index = next_launch_configuration_index(1, &commands);

    // Assert
    assert_eq!(index, 0);
}

#[test]
fn test_previous_launch_configuration_index_wraps_to_end() {
    // Arrange
    let commands = vec!["cargo test".to_string(), "npm run dev".to_string()];

    // Act
    let index = previous_launch_configuration_index(0, &commands);

    // Assert
    assert_eq!(index, 1);
}

#[tokio::test]
async fn test_stack_parent_selector_appends_session_and_returns_to_list() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    let parent_session_id = app.create_session().await.expect("failed to create parent");
    let source_session_id = app.create_session().await.expect("failed to create source");
    crate::test_support::set_session_status_for_test(
        &mut app,
        &parent_session_id,
        crate::domain::session::Status::Review,
    );
    crate::test_support::set_session_status_for_test(
        &mut app,
        &source_session_id,
        crate::domain::session::Status::Review,
    );
    app.mode = AppMode::StackAppendParentSelection {
        selected_parent_index: 0,
        session_id: source_session_id.clone().into(),
    };

    // Act
    let result =
        handle_stack_append_parent_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await;

    // Assert
    let source_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == source_session_id)
        .expect("source session should remain loaded");
    assert!(matches!(result, Ok(EventResult::Continue)));
    assert!(matches!(app.mode, AppMode::List));
    assert_eq!(
        source_session.parent_session_id.as_deref(),
        Some(parent_session_id.as_str())
    );
}

#[tokio::test]
async fn test_stack_parent_selector_handles_navigation_close_and_unbound_keys() {
    // Arrange
    let (mut app, _base_dir, _parent_session_id, source_session_id) =
        appendable_stack_test_app().await;
    app.mode = AppMode::StackAppendParentSelection {
        selected_parent_index: 0,
        session_id: source_session_id.clone().into(),
    };

    // Act
    for key_code in [KeyCode::Down, KeyCode::Up, KeyCode::Char('x')] {
        handle_stack_append_parent_key(&mut app, KeyEvent::new(key_code, KeyModifiers::NONE))
            .await
            .expect("parent navigation should continue");
    }
    assert!(matches!(
        app.mode,
        AppMode::StackAppendParentSelection {
            selected_parent_index: 0,
            ..
        }
    ));
    handle_stack_append_parent_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
    )
    .await
    .expect("parent selector should close");

    // Assert
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_stack_parent_selector_selects_parent_beyond_compact_viewport() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    for _ in 0..8 {
        let parent_session_id = app.create_session().await.expect("failed to create parent");
        crate::test_support::set_session_status_for_test(
            &mut app,
            &parent_session_id,
            crate::domain::session::Status::Review,
        );
    }
    let source_session_id = app.create_session().await.expect("failed to create source");
    crate::test_support::set_session_status_for_test(
        &mut app,
        &source_session_id,
        crate::domain::session::Status::Review,
    );
    app.mode = AppMode::StackAppendParentSelection {
        selected_parent_index: 0,
        session_id: source_session_id.clone().into(),
    };
    let eligible_parent_ids =
        stack_append_parent_session_ids(&app, &SessionId::from(source_session_id.as_str()));
    let expected_parent_id = eligible_parent_ids
        .last()
        .expect("expected eligible parents")
        .clone();

    // Act
    for _ in 1..eligible_parent_ids.len() {
        handle_stack_append_parent_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .await
            .expect("failed to move parent selection");
    }
    handle_stack_append_parent_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .expect("failed to append to selected parent");
    let source_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == source_session_id)
        .expect("source session should remain loaded");

    // Assert
    assert_eq!(
        source_session.parent_session_id.as_deref(),
        Some(expected_parent_id.as_str())
    );
}

#[tokio::test]
async fn test_stack_parent_helpers_ignore_inactive_or_empty_selection() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;

    // Act
    update_stack_append_parent_selection(&mut app, true);
    append_session_to_selected_stack(&mut app).await;
    app.mode = AppMode::StackAppendParentSelection {
        selected_parent_index: 0,
        session_id: "missing-source".into(),
    };
    let result =
        handle_stack_append_parent_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await;

    // Assert
    assert!(matches!(result, Ok(EventResult::Continue)));
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_session_confirmation_handlers_accept_no_selected_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;

    // Act
    let cancel_result = handle_cancel_session_confirmation(&mut app, None).await;
    let merge_result = handle_merge_confirmation(&mut app, None, None).await;
    let open_result = handle_open_managed_worktree_confirmation(&mut app, None, None).await;
    let regenerate_result = handle_regenerate_review_confirmation(&mut app, None, None);
    let missing_regenerate_result = handle_regenerate_review_confirmation(
        &mut app,
        Some("missing-session".into()),
        Some(ConfirmationViewMode {
            scroll_offset: Some(5),
            session_id: "restore-session".into(),
        }),
    );

    // Assert
    assert!(matches!(cancel_result, Ok(EventResult::Continue)));
    assert!(matches!(merge_result, Ok(EventResult::Continue)));
    assert!(matches!(open_result, Ok(EventResult::Continue)));
    assert!(matches!(regenerate_result, EventResult::Continue));
    assert!(matches!(missing_regenerate_result, EventResult::Continue));
    assert!(matches!(
        app.mode,
        AppMode::View {
            ref session_id,
            scroll_offset: Some(5),
        } if session_id == "restore-session"
    ));
}

#[test]
fn test_content_area_for_terminal_excludes_global_bars() {
    // Arrange
    let terminal_rect = Rect::new(0, 0, 120, 30);

    // Act
    let content_area = content_area_for_terminal(terminal_rect);

    // Assert
    assert_eq!(content_area, Rect::new(0, 1, 120, 28));
}

#[tokio::test]
async fn managed_worktree_confirmation_validates_target_then_opens_selector() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    app.settings.launch_configuration = "cargo test\nnpm run dev".to_string();
    let restore_view = ConfirmationViewMode {
        scroll_offset: Some(3),
        session_id: session_id.clone().into(),
    };

    // Act
    let mismatched_result = handle_open_managed_worktree_confirmation(
        &mut app,
        Some(SessionId::from("other-session")),
        Some(restore_view.clone()),
    )
    .await;
    app.mode = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::OpenManagedWorktree,
        confirmation_message: "Open?".to_string(),
        confirmation_title: "Open Managed Worktree".to_string(),
        restore_view: Some(restore_view),
        session_id: Some(session_id.clone().into()),
        selected_confirmation_index: 0,
    };
    let confirmed_result =
        handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

    // Assert
    assert!(matches!(mismatched_result, Ok(EventResult::Continue)));
    assert!(matches!(confirmed_result, Ok(EventResult::Continue)));
    assert!(matches!(
        app.mode,
        AppMode::LaunchConfigurationSelector {
            ref commands,
            ref restore_view,
            selected_command_index: 0,
        } if commands == &["cargo test".to_string(), "npm run dev".to_string()]
            && restore_view.session_id == session_id
            && restore_view.scroll_offset == Some(3)
    ));
}

#[tokio::test]
async fn test_handle_confirmation_decision_confirm_quits_when_no_session_context() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
    app.mode = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::Quit,
        confirmation_message: "Quit agentty?".to_string(),
        confirmation_title: "Confirm Quit".to_string(),
        restore_view: None,
        session_id: None,
        selected_confirmation_index: 0,
    };

    // Act
    let event_result = handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

    // Assert
    assert!(matches!(event_result, Ok(EventResult::Quit)));
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_handle_confirmation_decision_reject_and_cancel_return_to_list() {
    for decision in [ConfirmationDecision::Reject, ConfirmationDecision::Cancel] {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::Quit,
            confirmation_message: "Quit agentty?".to_string(),
            confirmation_title: "Confirm Quit".to_string(),
            restore_view: None,
            session_id: None,
            selected_confirmation_index: 0,
        };

        // Act
        let event_result = handle_confirmation_decision(&mut app, decision).await;

        // Assert
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(app.mode, AppMode::List));
    }
}

#[tokio::test]
async fn test_handle_confirmation_decision_confirm_cancels_session_when_context_exists() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    crate::test_support::set_session_status_for_test(
        &mut app,
        &session_id,
        crate::domain::session::Status::Review,
    );
    app.mode = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::CancelSession,
        confirmation_message: "Cancel session \"test\"?".to_string(),
        confirmation_title: "Confirm Cancel".to_string(),
        restore_view: None,
        session_id: Some(session_id.clone().into()),
        selected_confirmation_index: 0,
    };

    // Act
    let event_result = handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

    // Assert
    assert!(matches!(event_result, Ok(EventResult::Continue)));
    assert!(matches!(app.mode, AppMode::List));
    app.sessions.sync_from_handles();
    assert!(matches!(
        app.sessions.sessions().first(),
        Some(session) if session.id == session_id
            && session.status == crate::domain::session::Status::Canceled
    ));
}

#[tokio::test]
async fn test_handle_confirmation_decision_cancel_restores_view_for_continue_confirmation() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    app.mode = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::ContinueSession,
        confirmation_message: "Create a new draft session with initial context from this session?"
            .to_string(),
        confirmation_title: "Confirm Continue".to_string(),
        restore_view: Some(ConfirmationViewMode {
            scroll_offset: Some(4),
            session_id: session_id.clone().into(),
        }),
        session_id: Some(session_id.clone().into()),
        selected_confirmation_index: 1,
    };

    // Act
    let event_result = handle_confirmation_decision(&mut app, ConfirmationDecision::Cancel).await;

    // Assert
    assert!(matches!(event_result, Ok(EventResult::Continue)));
    assert!(matches!(
        app.mode,
        AppMode::View {
            session_id: ref session_id_in_mode,
            scroll_offset: Some(4),
            ..
        } if session_id_in_mode == &session_id
    ));
}

#[tokio::test]
async fn test_handle_confirmation_decision_cancel_restores_view_for_regenerate_confirmation() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    app.mode = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::RegenerateReview,
        confirmation_message: "Regenerate focused review?".to_string(),
        confirmation_title: "Confirm Regenerate".to_string(),
        restore_view: Some(ConfirmationViewMode {
            scroll_offset: Some(4),
            session_id: session_id.clone().into(),
        }),
        session_id: Some(session_id.clone().into()),
        selected_confirmation_index: 1,
    };

    // Act
    let event_result = handle_confirmation_decision(&mut app, ConfirmationDecision::Cancel).await;

    // Assert
    assert!(matches!(event_result, Ok(EventResult::Continue)));
    assert!(matches!(
        app.mode,
        AppMode::View {
            scroll_offset: Some(4),
            ..
        }
    ));
}
