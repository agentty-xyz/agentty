use ag_forge::{
    ReviewComment, ReviewCommentAnchorSide, ReviewCommentSnapshot, ReviewCommentThread,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use super::{
    apply_review_comment_resolution_outcome, handle_with_cache, next_selected_index,
    previous_selected_index,
};
use crate::app::prompt_intent::ReviewCommentResolutionOutcome;
use crate::domain::session::{SessionId, SessionRole, Status};
use crate::presentation::app_mode::{
    AppMode, DiffFocus, DiffLineComments, DiffPreview, DiffReviewComments, DiffSidebarFocus,
    ReviewCommentSelection,
};
use crate::test_support::SessionFixtureBuilder;
use crate::ui::RenderCacheStore;

fn comment_snapshot() -> ReviewCommentSnapshot {
    ReviewCommentSnapshot {
        pr_level_comments: vec![ReviewComment {
            author: "alice".to_string(),
            authored_by_current_user: false,
            body: "General comment".to_string(),
        }],
        threads: vec![ReviewCommentThread {
            anchor_side: ReviewCommentAnchorSide::New,
            comments: vec![ReviewComment {
                author: "bob".to_string(),
                authored_by_current_user: false,
                body: "Inline comment".to_string(),
            }],
            id: "thread-id".to_string(),
            is_outdated: Some(false),
            is_resolved: false,
            line: Some(2),
            path: "src/main.rs".to_string(),
            start_line: None,
        }],
    }
}

fn review_comment_mode(
    session_id: &str,
    comment_snapshot: Option<ReviewCommentSnapshot>,
    selected_comments: Vec<ReviewCommentSelection>,
    selected_comment_index: usize,
    scroll_offset: u16,
) -> AppMode {
    AppMode::Diff {
        diff: String::new(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: Some(DiffReviewComments {
            selected_comments,
            comment_error: None,
            is_loading_comments: comment_snapshot.is_none(),
            comment_snapshot,
            request_id: 1,
            selected_comment_index,
            sidebar_focus: DiffSidebarFocus::Comments,
        }),
        restore: None,
        scroll_cache: None,
        session_id: session_id.into(),
        scroll_offset,
    }
}

#[tokio::test]
async fn test_handle_uses_space_as_only_comment_selection_key() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.sessions.push_session(
        SessionFixtureBuilder::new()
            .id("session-id")
            .status(Status::Review)
            .build(),
    );
    app.mode = review_comment_mode("session-id", Some(comment_snapshot()), Vec::new(), 0, 0);

    // Act, Assert
    for key_code in [KeyCode::Char('a'), KeyCode::Char('d')] {
        handle_with_cache(
            &mut app,
            &RenderCacheStore::default(),
            Rect::new(0, 0, 80, 24),
            KeyEvent::new(key_code, KeyModifiers::NONE),
        )
        .await;
    }
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            review_comments: Some(DiffReviewComments {
                ref selected_comments,
                ..
            }),
            ..
        } if selected_comments.is_empty()
    ));
    handle_with_cache(
        &mut app,
        &RenderCacheStore::default(),
        Rect::new(0, 0, 80, 24),
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
    )
    .await;
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            review_comments: Some(DiffReviewComments {
                ref selected_comments,
                ..
            }),
            ..
        } if selected_comments == &[ReviewCommentSelection {
            thread_id: "thread-id".to_string(),
        }]
    ));
    handle_with_cache(
        &mut app,
        &RenderCacheStore::default(),
        Rect::new(0, 0, 80, 24),
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
    )
    .await;
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            review_comments: Some(DiffReviewComments {
                ref selected_comments,
                ..
            }),
            ..
        } if selected_comments.is_empty()
    ));
}

#[tokio::test]
async fn managed_session_cannot_mark_review_comments() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.sessions.push_session(
        SessionFixtureBuilder::new()
            .id("session-id")
            .role(SessionRole::OrchestrationWorker)
            .status(Status::Review)
            .build(),
    );
    app.mode = review_comment_mode("session-id", Some(comment_snapshot()), Vec::new(), 0, 0);

    // Act
    handle_with_cache(
        &mut app,
        &RenderCacheStore::default(),
        Rect::new(0, 0, 80, 24),
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            review_comments: Some(DiffReviewComments {
                ref selected_comments,
                ..
            }),
            ..
        } if selected_comments.is_empty()
    ));
}

#[tokio::test]
async fn test_handle_selects_next_comment_and_resets_detail_scroll() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = review_comment_mode("session-id", Some(comment_snapshot()), Vec::new(), 0, 4);

    // Act
    handle_with_cache(
        &mut app,
        &RenderCacheStore::default(),
        Rect::new(0, 0, 80, 24),
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            review_comments: Some(DiffReviewComments {
                selected_comment_index: 1,
                ..
            }),
            scroll_offset: 0,
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_f_focuses_files_and_resets_detail_scroll() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = review_comment_mode("session-id", Some(comment_snapshot()), Vec::new(), 0, 3);

    // Act
    handle_with_cache(
        &mut app,
        &RenderCacheStore::default(),
        Rect::new(0, 0, 80, 24),
        KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            review_comments: Some(DiffReviewComments {
                sidebar_focus: DiffSidebarFocus::Files,
                ..
            }),
            scroll_offset: 0,
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_selects_previous_comment_and_resets_detail_scroll() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = review_comment_mode("session-id", Some(comment_snapshot()), Vec::new(), 1, 4);

    // Act
    handle_with_cache(
        &mut app,
        &RenderCacheStore::default(),
        Rect::new(0, 0, 80, 24),
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            review_comments: Some(DiffReviewComments {
                selected_comment_index: 0,
                ..
            }),
            scroll_offset: 0,
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_down_scrolls_within_rendered_detail() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = review_comment_mode("session-id", Some(comment_snapshot()), Vec::new(), 0, 0);

    // Act
    handle_with_cache(
        &mut app,
        &RenderCacheStore::default(),
        Rect::new(0, 0, 80, 8),
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            scroll_offset: 1,
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_up_decrements_scroll_and_other_keys_preserve_mode() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = review_comment_mode("session-id", Some(comment_snapshot()), Vec::new(), 0, 2);

    // Act
    handle_with_cache(
        &mut app,
        &RenderCacheStore::default(),
        Rect::new(0, 0, 80, 24),
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
    )
    .await;
    handle_with_cache(
        &mut app,
        &RenderCacheStore::default(),
        Rect::new(0, 0, 80, 24),
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            scroll_offset: 1,
            ..
        }
    ));
}

#[tokio::test]
async fn test_handle_batch_keys_reject_read_only_rows_and_preserve_failed_submission() {
    // Arrange
    let mut selected_app = crate::test_support::new_test_app_without_retained_base_dir().await;
    selected_app.mode = review_comment_mode(
        "missing-session",
        Some(comment_snapshot()),
        Vec::new(),
        1,
        3,
    );
    let mut submit_app = crate::test_support::new_test_app_without_retained_base_dir().await;
    submit_app.mode = review_comment_mode(
        "missing-session",
        Some(comment_snapshot()),
        vec![ReviewCommentSelection {
            thread_id: "thread-id".to_string(),
        }],
        1,
        3,
    );

    // Act
    handle_with_cache(
        &mut selected_app,
        &RenderCacheStore::default(),
        Rect::new(0, 0, 80, 24),
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
    )
    .await;
    handle_with_cache(
        &mut submit_app,
        &RenderCacheStore::default(),
        Rect::new(0, 0, 80, 24),
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(
        selected_app.mode,
        AppMode::Diff {
            review_comments: Some(DiffReviewComments {
                ref selected_comments,
                selected_comment_index: 1,
                ..
            }),
            scroll_offset: 3,
            ..
        } if selected_comments.is_empty()
    ));
    assert!(matches!(
        submit_app.mode,
        AppMode::Diff {
            review_comments: Some(DiffReviewComments {
                ref selected_comments,
                selected_comment_index: 1,
                ..
            }),
            scroll_offset: 3,
            ..
        } if selected_comments.len() == 1
    ));
}

#[tokio::test]
async fn test_apply_review_comment_resolution_outcome_shows_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let session_id = SessionId::from("session-id");

    // Act
    apply_review_comment_resolution_outcome(
        &mut app,
        ReviewCommentResolutionOutcome::ShowSession {
            session_id: session_id.clone(),
        },
    );

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::View {
            session_id: viewed_session_id,
            scroll_offset: None,
        } if viewed_session_id == session_id
    ));
}

#[tokio::test]
async fn test_handle_preserves_non_review_comment_modes() {
    // Arrange
    let mut exit_app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let mut other_app = crate::test_support::new_test_app_without_retained_base_dir().await;

    // Act
    handle_with_cache(
        &mut exit_app,
        &RenderCacheStore::default(),
        Rect::new(0, 0, 80, 24),
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    )
    .await;
    handle_with_cache(
        &mut other_app,
        &RenderCacheStore::default(),
        Rect::new(0, 0, 80, 24),
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(exit_app.mode, AppMode::List));
    assert!(matches!(other_app.mode, AppMode::List));
}

#[test]
fn test_selection_helpers_wrap_clamp_and_preserve_empty_selection() {
    // Arrange, Act, Assert
    assert_eq!(next_selected_index(0, 0), 0);
    assert_eq!(next_selected_index(1, 2), 0);
    assert_eq!(next_selected_index(usize::MAX, 2), 0);
    assert_eq!(previous_selected_index(0, 0), 0);
    assert_eq!(previous_selected_index(0, 2), 1);
    assert_eq!(previous_selected_index(usize::MAX, 2), 0);
}
