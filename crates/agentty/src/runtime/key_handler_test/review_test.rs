//! Branch and review-request publishing, review regeneration, and review
//! watch key-handler tests.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::{
    current_session_creation_selection, handle_confirmation_decision, handle_key_event,
    handle_publish_branch_input_key, handle_session_creation_key,
};
use crate::app::ReviewCacheEntry;
use crate::domain::session::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary, SessionId, Status,
};
use crate::presentation::app_mode::{
    AppMode, ConfirmationIntent, ConfirmationViewMode, DiffFocus, DiffLineComments, DiffPreview,
    DiffReviewComments, DiffSidebarFocus,
};
use crate::runtime::mode::confirmation::ConfirmationDecision;
use crate::runtime::{EventResult, PresentationState};

#[tokio::test]
async fn test_session_creation_skips_append_option_for_non_review_session() {
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
        crate::domain::session::Status::InProgress,
    );
    let source_index = app
        .sessions
        .sessions()
        .iter()
        .position(|session| session.id == source_session_id)
        .expect("source session should exist");
    app.sessions.select_session_index(Some(source_index));
    app.mode = AppMode::SessionCreation {
        selected_option_index: 2,
    };

    // Act
    handle_session_creation_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
        .await
        .expect("failed to navigate creation options");

    // Assert
    assert_eq!(current_session_creation_selection(&app), 2);
}

#[tokio::test]
async fn test_handle_publish_branch_input_key_escape_restores_view_mode() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
    app.mode = AppMode::PublishBranchInput {
        default_branch_name: "wt/session".to_string(),
        input: crate::domain::input::InputState::with_text("review/custom".to_string()),
        locked_upstream_ref: None,
        publish_branch_action: crate::domain::session::PublishBranchAction::Push,
        restore_view: ConfirmationViewMode {
            scroll_offset: Some(7),
            session_id: "session-id".into(),
        },
    };

    // Act
    let event_result =
        handle_publish_branch_input_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await;

    // Assert
    assert!(matches!(event_result, EventResult::Continue));
    assert!(matches!(
        app.mode,
        AppMode::View {
            ref session_id,
            scroll_offset: Some(7),
        } if session_id == "session-id"
    ));
}

/// Opens the review-request publish popup for one review-ready session and
/// returns its id.
async fn open_review_request_publish_input(
    app: &mut crate::app::App,
    locked_upstream_ref: Option<String>,
) -> String {
    open_publish_branch_input(
        app,
        locked_upstream_ref,
        crate::domain::session::PublishBranchAction::PublishPullRequest,
    )
    .await
}

/// Opens the publish popup with `publish_branch_action` for one review-ready
/// session and returns its id.
async fn open_publish_branch_input(
    app: &mut crate::app::App,
    locked_upstream_ref: Option<String>,
    publish_branch_action: crate::domain::session::PublishBranchAction,
) -> String {
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    crate::test_support::set_session_status_for_test(app, &session_id, Status::Review);
    app.mode = AppMode::PublishBranchInput {
        default_branch_name: "wt/session".to_string(),
        input: crate::domain::input::InputState::with_text("review/custom".to_string()),
        locked_upstream_ref,
        publish_branch_action,
        restore_view: ConfirmationViewMode {
            scroll_offset: Some(4),
            session_id: session_id.clone().into(),
        },
    };

    session_id
}

/// Returns the first session's branch-publish progress text, if any.
fn branch_publish_progress(app: &crate::app::App) -> Option<String> {
    app.sessions
        .session_at(0)
        .and_then(|session| {
            session
                .transient_messages
                .get(crate::domain::transient_message::TransientMessageSlot::BranchPublish)
        })
        .map(|message| message.body.text().to_string())
}

#[tokio::test]
async fn test_handle_publish_branch_input_key_enter_pushes_branch_without_watch_question() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    let session_id = open_publish_branch_input(
        &mut app,
        None,
        crate::domain::session::PublishBranchAction::Push,
    )
    .await;

    // Act
    let event_result = handle_publish_branch_input_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(event_result, EventResult::Continue));
    assert!(matches!(
        app.mode,
        AppMode::View {
            session_id: ref viewed_session_id,
            scroll_offset: Some(4),
        } if viewed_session_id == &session_id
    ));
    assert_eq!(
        branch_publish_progress(&app).as_deref(),
        Some("Pushing branch...")
    );
}

#[tokio::test]
async fn test_handle_publish_branch_input_key_enter_publishes_pushed_branch_right_away() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    let session_id =
        open_review_request_publish_input(&mut app, Some("origin/review/custom".to_string())).await;

    // Act
    let event_result = handle_publish_branch_input_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(event_result, EventResult::Continue));
    assert!(matches!(
        app.mode,
        AppMode::View {
            session_id: ref viewed_session_id,
            scroll_offset: Some(4),
        } if viewed_session_id == &session_id
    ));
    assert_eq!(
        branch_publish_progress(&app).as_deref(),
        Some("Publishing review request...")
    );
}

#[tokio::test]
async fn test_handle_publish_branch_input_key_enter_asks_to_watch_before_pushing() {
    for (decision, answer) in [
        (ConfirmationDecision::Confirm, "yes"),
        (ConfirmationDecision::Reject, "no"),
    ] {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let session_id = open_review_request_publish_input(&mut app, None).await;

        // Act
        handle_publish_branch_input_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;
        let asks_before_pushing = matches!(
            app.mode,
            AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::WatchPublishedReviewRequest,
                ..
            }
        ) && branch_publish_progress(&app).is_none();
        let event_result = handle_confirmation_decision(&mut app, decision).await;

        // Assert
        assert!(asks_before_pushing, "{answer}");
        assert!(matches!(event_result, Ok(EventResult::Continue)));
        assert!(matches!(
            app.mode,
            AppMode::View {
                session_id: ref viewed_session_id,
                scroll_offset: Some(4),
            } if viewed_session_id == &session_id
        ));
        assert_eq!(
            branch_publish_progress(&app).as_deref(),
            Some("Publishing review request..."),
            "{answer}"
        );
    }
}

#[tokio::test]
async fn test_canceling_the_publish_watch_question_publishes_nothing() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    let session_id = open_review_request_publish_input(&mut app, None).await;
    handle_publish_branch_input_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await;

    // Act
    let event_result = handle_confirmation_decision(&mut app, ConfirmationDecision::Cancel).await;

    // Assert
    assert!(matches!(event_result, Ok(EventResult::Continue)));
    assert!(matches!(
        app.mode,
        AppMode::View {
            session_id: ref viewed_session_id,
            scroll_offset: Some(4),
        } if viewed_session_id == &session_id
    ));
    assert_eq!(branch_publish_progress(&app), None);
}

#[tokio::test]
async fn test_handle_publish_branch_input_key_char_updates_input_state() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
    app.mode = AppMode::PublishBranchInput {
        default_branch_name: "wt/session".to_string(),
        input: crate::domain::input::InputState::default(),
        locked_upstream_ref: None,
        publish_branch_action: crate::domain::session::PublishBranchAction::Push,
        restore_view: ConfirmationViewMode {
            scroll_offset: None,
            session_id: "session-id".into(),
        },
    };

    // Act
    let event_result = handle_publish_branch_input_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(event_result, EventResult::Continue));
    assert!(matches!(
        app.mode,
        AppMode::PublishBranchInput {
            input: ref input_state,
            ..
        } if input_state.cursor == 1 && input_state.text() == "r"
    ));
    let AppMode::PublishBranchInput { input, .. } = &app.mode else {
        unreachable!("mode should remain publish-branch input");
    };
    assert_eq!(input.text(), "r");
}

#[tokio::test]
async fn test_handle_publish_branch_input_key_shortcut_chars_are_inserted() {
    // Arrange
    let typed_shortcut_characters = ['q', 'p', 'd', 'f', 'm', 'r', 'j', 'k', 'g', 'G', '?'];
    let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;

    for character in typed_shortcut_characters {
        app.mode = AppMode::PublishBranchInput {
            default_branch_name: "wt/session".to_string(),
            input: crate::domain::input::InputState::default(),
            locked_upstream_ref: None,
            publish_branch_action: crate::domain::session::PublishBranchAction::Push,
            restore_view: ConfirmationViewMode {
                scroll_offset: None,
                session_id: "session-id".into(),
            },
        };
        let modifiers = if character.is_ascii_uppercase() || character == '?' {
            KeyModifiers::SHIFT
        } else {
            KeyModifiers::NONE
        };

        // Act
        let event_result = handle_publish_branch_input_key(
            &mut app,
            KeyEvent::new(KeyCode::Char(character), modifiers),
        )
        .await;

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::PublishBranchInput {
                input: ref input_state,
                ..
            } if input_state.cursor == 1 && input_state.text() == character.to_string()
        ));
    }
}

#[tokio::test]
async fn test_handle_publish_branch_input_key_left_moves_cursor() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
    app.mode = AppMode::PublishBranchInput {
        default_branch_name: "wt/session".to_string(),
        input: crate::domain::input::InputState::with_text("review/custom".to_string()),
        locked_upstream_ref: None,
        publish_branch_action: crate::domain::session::PublishBranchAction::Push,
        restore_view: ConfirmationViewMode {
            scroll_offset: None,
            session_id: "session-id".into(),
        },
    };

    // Act
    let event_result =
        handle_publish_branch_input_key(&mut app, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))
            .await;

    // Assert
    assert!(matches!(event_result, EventResult::Continue));
    let AppMode::PublishBranchInput { input, .. } = &app.mode else {
        unreachable!("mode should remain publish-branch input");
    };
    assert_eq!(input.text(), "review/custom");
    assert_eq!(input.cursor, "review/custom".chars().count() - 1);
}

#[tokio::test]
async fn test_handle_publish_branch_input_key_supports_word_delete_undo_and_redo() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
    app.mode = AppMode::PublishBranchInput {
        default_branch_name: "wt/session".to_string(),
        input: crate::domain::input::InputState::with_text("review custom".to_string()),
        locked_upstream_ref: None,
        publish_branch_action: crate::domain::session::PublishBranchAction::Push,
        restore_view: ConfirmationViewMode {
            scroll_offset: None,
            session_id: "session-id".into(),
        },
    };

    // Act
    handle_publish_branch_input_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
    )
    .await;
    handle_publish_branch_input_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
    )
    .await;
    assert!(matches!(
        &app.mode,
        AppMode::PublishBranchInput { input, .. } if input.text() == "review custom"
    ));
    handle_publish_branch_input_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
    )
    .await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::PublishBranchInput { input, .. } if input.text() == "review"
    ));
}

#[tokio::test]
async fn test_handle_publish_branch_input_key_preserves_mode_for_unmapped_key() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
    app.mode = AppMode::PublishBranchInput {
        default_branch_name: "wt/session".to_string(),
        input: crate::domain::input::InputState::with_text("review/custom".to_string()),
        locked_upstream_ref: None,
        publish_branch_action: crate::domain::session::PublishBranchAction::Push,
        restore_view: ConfirmationViewMode {
            scroll_offset: None,
            session_id: "session-id".into(),
        },
    };

    // Act
    let result =
        handle_publish_branch_input_key(&mut app, KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE))
            .await;

    // Assert
    assert!(matches!(result, EventResult::Continue));
    assert!(matches!(
        &app.mode,
        AppMode::PublishBranchInput { input, .. } if input.text() == "review/custom"
    ));
}

#[tokio::test]
async fn test_handle_publish_branch_input_key_char_keeps_locked_branch_name() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
    app.mode = AppMode::PublishBranchInput {
        default_branch_name: "wt/session".to_string(),
        input: crate::domain::input::InputState::with_text("review/custom".to_string()),
        locked_upstream_ref: Some("origin/review/custom".to_string()),
        publish_branch_action: crate::domain::session::PublishBranchAction::Push,
        restore_view: ConfirmationViewMode {
            scroll_offset: None,
            session_id: "session-id".into(),
        },
    };

    // Act
    let event_result = handle_publish_branch_input_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(event_result, EventResult::Continue));
    let AppMode::PublishBranchInput {
        input,
        locked_upstream_ref,
        ..
    } = &app.mode
    else {
        unreachable!("mode should remain publish-branch input");
    };
    assert_eq!(locked_upstream_ref.as_deref(), Some("origin/review/custom"));
    assert_eq!(input.text(), "review/custom");
}

#[tokio::test]
async fn test_handle_key_event_routes_review_comment_escape_to_files() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Diff {
        diff: String::new(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: Some(DiffReviewComments {
            sidebar_focus: DiffSidebarFocus::Comments,
            ..DiffReviewComments::loading(1)
        }),
        restore: None,
        scroll_cache: None,
        session_id: "session-id".into(),
        scroll_offset: 0,
    };
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    let event_result = handle_key_event(
        &mut app,
        &PresentationState::default(),
        &mut terminal,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(event_result, Ok(EventResult::Continue)));
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            review_comments: Some(DiffReviewComments {
                sidebar_focus: DiffSidebarFocus::Files,
                ..
            }),
            ref session_id,
            ..
        } if session_id == "session-id"
    ));
}

#[tokio::test]
async fn test_handle_key_event_routes_publish_branch_input() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app_with_mock_tmux_client().await;
    app.mode = AppMode::PublishBranchInput {
        default_branch_name: "wt/session".to_string(),
        input: crate::domain::input::InputState::with_text("review/custom".to_string()),
        locked_upstream_ref: None,
        publish_branch_action: crate::domain::session::PublishBranchAction::Push,
        restore_view: ConfirmationViewMode {
            scroll_offset: Some(7),
            session_id: "session-id".into(),
        },
    };
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    let event_result = handle_key_event(
        &mut app,
        &PresentationState::default(),
        &mut terminal,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(event_result, Ok(EventResult::Continue)));
    assert!(matches!(
        app.mode,
        AppMode::View {
            ref session_id,
            scroll_offset: Some(7),
        } if session_id == "session-id"
    ));
}

#[tokio::test]
async fn test_handle_session_creation_key_opens_parent_selector_for_review_session() {
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
        crate::domain::session::Status::AgentReview,
    );
    let source_index = app
        .sessions
        .sessions()
        .iter()
        .position(|session| session.id == source_session_id)
        .expect("source session should exist");
    app.sessions.select_session_index(Some(source_index));
    app.mode = AppMode::SessionCreation {
        selected_option_index: 2,
    };

    // Act
    handle_session_creation_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
        .await
        .expect("failed to select append option");
    let result =
        handle_session_creation_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await;

    // Assert
    assert!(matches!(result, Ok(EventResult::Continue)));
    assert!(matches!(
        app.mode,
        AppMode::StackAppendParentSelection {
            selected_parent_index: 0,
            ref session_id,
        } if session_id.as_str() == source_session_id
    ));
}

#[tokio::test]
async fn test_handle_confirmation_decision_confirm_regenerates_review() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    let session_folder = app.sessions.sessions()[0].folder.clone();
    std::fs::write(session_folder.join("README.md"), "regenerate test\n").expect("failed to write");
    app.review_cache.insert(
        session_id.clone().into(),
        ReviewCacheEntry::Ready {
            request_id: uuid::Uuid::nil(),
            text: "Old review".to_string(),
            diff_hash: 99,
        },
    );
    app.mode = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::RegenerateReview,
        confirmation_message: "Regenerate focused review?".to_string(),
        confirmation_title: "Confirm Regenerate".to_string(),
        restore_view: Some(ConfirmationViewMode {
            scroll_offset: None,
            session_id: session_id.clone().into(),
        }),
        session_id: Some(session_id.clone().into()),
        selected_confirmation_index: 0,
    };

    // Act
    let event_result = handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

    // Assert — view is restored with loading state, cache shows new Loading
    // entry
    assert!(matches!(event_result, Ok(EventResult::Continue)));
    assert!(matches!(app.mode, AppMode::View { .. }));
    assert!(matches!(
        app.review_cache.get(session_id.as_str()),
        Some(ReviewCacheEntry::Loading { .. })
    ));
}

/// Opens the watch confirmation for a review session with an open pull
/// request and returns that session's identifier.
async fn open_watch_confirmation(app: &mut crate::app::App) -> SessionId {
    let session_id = SessionId::from(app.create_session().await.expect("session"));
    crate::test_support::set_session_status_for_test(app, &session_id, Status::Review);
    app.sessions.apply_review_request(
        &session_id,
        ReviewRequest {
            last_refreshed_at: 0,
            summary: ReviewRequestSummary {
                display_id: "#42".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "wt/session".to_string(),
                state: ReviewRequestState::Open,
                status_summary: None,
                target_branch: "main".to_string(),
                title: "Add watch".to_string(),
                web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            },
        },
    );
    let opened = app.open_review_watch_confirmation(
        &session_id,
        Some(ConfirmationViewMode {
            scroll_offset: Some(4),
            session_id: session_id.clone(),
        }),
    );
    assert!(opened, "watch confirmation should open");

    session_id
}

#[tokio::test]
async fn watch_confirmation_confirm_starts_watching_and_restores_view() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    let session_id = open_watch_confirmation(&mut app).await;

    // Act
    let event_result = handle_confirmation_decision(&mut app, ConfirmationDecision::Confirm).await;

    // Assert
    assert!(matches!(event_result, Ok(EventResult::Continue)));
    assert!(matches!(
        app.mode,
        AppMode::View {
            session_id: ref session_id_in_mode,
            scroll_offset: Some(4),
        } if *session_id_in_mode == session_id
    ));
    assert_eq!(app.sessions.sessions()[0].status, Status::Watching);
}

#[tokio::test]
async fn watch_confirmation_cancel_restores_view_without_watching() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    let session_id = open_watch_confirmation(&mut app).await;

    // Act
    let event_result = handle_confirmation_decision(&mut app, ConfirmationDecision::Reject).await;

    // Assert
    assert!(matches!(event_result, Ok(EventResult::Continue)));
    assert!(matches!(
        app.mode,
        AppMode::View {
            session_id: ref session_id_in_mode,
            scroll_offset: Some(4),
        } if *session_id_in_mode == session_id
    ));
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
}

#[tokio::test]
async fn watch_confirmation_without_a_session_returns_to_the_list() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    app.mode = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::WatchReviewRequest,
        confirmation_message: "Auto-resolve checks, comments, conflicts?".to_string(),
        confirmation_title: "Watch PR #42".to_string(),
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
