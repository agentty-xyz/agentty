use ag_session::{CreateSessionMode, CreateSessionRequest};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;

use super::super::{
    current_session_creation_selection, handle_confirmation_decision, handle_key_event,
    handle_pre_commit_hook_warning_key, handle_session_creation_key,
    session_creation_option_is_enabled, update_session_creation_selection,
};
use super::support::appendable_stack_test_app;
use crate::presentation::app_mode::{AppMode, ConfirmationIntent, ConfirmationViewMode};
use crate::runtime::mode::confirmation::ConfirmationDecision;
use crate::runtime::{EventResult, PresentationState};

#[tokio::test]
async fn test_session_creation_rejection_stays_in_terminal_ui() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    let project_id = app.active_project_id();
    app.project_sync_status = Some(crate::app::ProjectSyncStatus {
        context: crate::app::test_support::ProjectSyncContext {
            default_branch: "main".to_string(),
            operation_id: 1,
            project_id,
            project_name: "agentty".to_string(),
        },
        phase: crate::app::ProjectSyncPhase::Running,
    });
    app.mode = AppMode::SessionCreation {
        selected_option_index: 0,
    };

    // Act
    let result =
        handle_session_creation_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await;

    // Assert
    assert!(matches!(result, Ok(EventResult::Continue)));
    assert!(app.sessions.sessions().is_empty());
    assert!(matches!(
        app.mode,
        AppMode::SyncBlockedPopup {
            is_loading: false,
            ref message,
            ref title,
            ..
        } if title == "Session creation unavailable"
            && message.contains("is synchronizing `main`")
    ));
}

#[tokio::test]
async fn test_session_creation_navigation_moves_up_and_clamps_disabled_options() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    app.mode = AppMode::SessionCreation {
        selected_option_index: 4,
    };

    // Act
    update_session_creation_selection(&mut app, 4);
    let clamped_selection = current_session_creation_selection(&app);
    let result =
        handle_session_creation_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)).await;
    let moved_selection = current_session_creation_selection(&app);
    let unknown_option_enabled = session_creation_option_is_enabled(&app, usize::MAX);
    app.mode = AppMode::SessionCreation {
        selected_option_index: 4,
    };
    let disabled_append_result =
        handle_session_creation_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await;

    // Assert
    assert_eq!(clamped_selection, 2);
    assert!(matches!(result, Ok(EventResult::Continue)));
    assert_eq!(moved_selection, 1);
    assert!(!unknown_option_enabled);
    assert!(matches!(disabled_append_result, Ok(EventResult::Continue)));
    assert!(matches!(
        app.mode,
        AppMode::SessionCreation {
            selected_option_index: 4,
        }
    ));
}

#[tokio::test]
async fn test_input_during_creation_keeps_list_navigation_after_completion() {
    for key in [
        KeyCode::Esc,
        KeyCode::Tab,
        KeyCode::BackTab,
        KeyCode::Char('j'),
    ] {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let request = CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Regular,
            project_id: app.active_project_id(),
        };
        let presentation = PresentationState::default();
        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        app.start_session_creation(request, None).await;
        assert!(matches!(app.mode, AppMode::Prompt { .. }));
        app.mode = AppMode::List;

        // Act
        handle_key_event(
            &mut app,
            &presentation,
            &mut terminal,
            KeyEvent::new(key, KeyModifiers::NONE),
        )
        .await
        .expect("list input");
        let selected_tab = app.tabs.current();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !app.pending_session_creations.is_empty() {
                let event = app.next_app_event().await.expect("creation event");
                app.apply_app_events(event).await;
            }
        })
        .await
        .expect("creation should finish");

        // Assert
        assert!(matches!(app.mode, AppMode::List));
        assert_eq!(app.tabs.current(), selected_tab);
        assert_eq!(app.sessions.sessions().len(), 1);
    }
}

#[tokio::test]
async fn test_pre_commit_warning_enter_opens_session_creation_options() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::PreCommitHookWarning {
        message: "Missing pre-commit hook".to_string(),
    };

    // Act
    let result = handle_pre_commit_hook_warning_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(result, EventResult::Continue));
    assert!(app.sessions.sessions().is_empty());
    assert!(matches!(
        app.mode,
        AppMode::SessionCreation {
            selected_option_index: 0,
        }
    ));
}

#[tokio::test]
async fn test_handle_key_event_routes_stack_parent_escape_to_creation_selector() {
    // Arrange
    let (mut app, _base_dir, _parent_session_id, source_session_id) =
        appendable_stack_test_app().await;
    app.mode = AppMode::StackAppendParentSelection {
        selected_parent_index: 0,
        session_id: source_session_id.into(),
    };
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    let result = handle_key_event(
        &mut app,
        &PresentationState::default(),
        &mut terminal,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(result, Ok(EventResult::Continue)));
    assert!(matches!(
        app.mode,
        AppMode::SessionCreation {
            selected_option_index: 4,
        }
    ));
}

#[tokio::test]
async fn test_handle_session_creation_key_creates_each_root_session_type() {
    for (selected_option_index, is_draft, role) in [
        (0, false, ag_session::SessionRole::Worker),
        (1, true, ag_session::SessionRole::Worker),
        (2, false, ag_session::SessionRole::Orchestrator),
    ] {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        app.mode = AppMode::SessionCreation {
            selected_option_index: 0,
        };
        update_session_creation_selection(&mut app, selected_option_index);

        // Act
        let result = handle_session_creation_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !app.pending_session_creations.is_empty() {
                let event = app.next_app_event().await.expect("creation event");
                app.apply_app_events(event).await;
            }
        })
        .await
        .expect("creation should finish");

        // Assert
        assert!(matches!(result, Ok(EventResult::Continue)));
        assert_eq!(app.sessions.sessions().len(), 1);
        assert_eq!(app.sessions.sessions()[0].is_draft_session(), is_draft);
        assert_eq!(app.sessions.sessions()[0].role, role);
        assert!(matches!(
            app.mode,
            AppMode::Prompt {
                ref session_id,
                scroll_offset: None,
                ..
            } if !session_id.is_empty()
        ));
    }
}

#[tokio::test]
async fn test_handle_session_creation_key_creates_stacked_session() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    let parent_session_id = app
        .create_session()
        .await
        .expect("parent session should be created");
    crate::test_support::set_session_status_for_test(
        &mut app,
        &parent_session_id,
        crate::domain::session::Status::Review,
    );
    app.sessions.select_session_index(Some(0));
    app.mode = AppMode::SessionCreation {
        selected_option_index: 2,
    };

    // Act
    handle_session_creation_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
        .await
        .expect("failed to select stacked session");
    let result =
        handle_session_creation_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await;

    // Assert
    let child_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.parent_session_id.as_deref() == Some(parent_session_id.as_str()))
        .expect("stacked child should be created");
    let child_session_id = child_session.id.clone();
    assert!(matches!(result, Ok(EventResult::Continue)));
    assert!(child_session.is_draft_session());
    assert!(matches!(
        app.mode,
        AppMode::Prompt {
            ref session_id,
            scroll_offset: None,
            ..
        } if session_id == &child_session_id
    ));
}

#[tokio::test]
async fn test_handle_session_creation_key_escape_returns_to_list() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    app.mode = AppMode::SessionCreation {
        selected_option_index: 0,
    };

    // Act
    let result =
        handle_session_creation_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await;

    // Assert
    assert!(matches!(result, Ok(EventResult::Continue)));
    assert!(app.sessions.sessions().is_empty());
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_handle_confirmation_decision_confirm_opens_continuation_draft_prompt() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    let merged_commit_hash = "704de31d0f4b5a1234567890abcdef1234567890";
    let source_session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    app.services
        .db()
        .sessions()
        .update_session_merged_commit_hash(&source_session_id, Some(merged_commit_hash.to_string()))
        .await
        .expect("failed to persist merged commit hash");
    crate::test_support::set_session_status_for_test(
        &mut app,
        &source_session_id,
        crate::domain::session::Status::Done,
    );
    app.mode = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::ContinueSession,
        confirmation_message: "Create a new draft session with initial context from this session?"
            .to_string(),
        confirmation_title: "Confirm Continue".to_string(),
        restore_view: Some(ConfirmationViewMode {
            scroll_offset: Some(4),
            session_id: source_session_id.clone().into(),
        }),
        session_id: Some(source_session_id.clone().into()),
        selected_confirmation_index: 0,
    };

    // Act
    let event_result = handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

    // Assert
    assert!(matches!(event_result, Ok(EventResult::Continue)));
    assert!(matches!(
        app.mode,
        AppMode::Prompt {
            ref input,
            ref session_id,
            ..
        } if session_id.as_str() != source_session_id
            && input.text().is_empty()
    ));
    let continued_session_id = match &app.mode {
        AppMode::Prompt { session_id, .. } => session_id.as_str().to_string(),
        _ => unreachable!("expected prompt mode"),
    };
    let continued_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == continued_session_id)
        .expect("expected created continuation draft");
    assert_eq!(
        continued_session.prompt,
        format!("Use {merged_commit_hash} commit as an initial context for this session")
    );
}
