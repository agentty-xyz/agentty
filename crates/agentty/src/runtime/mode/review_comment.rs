use ag_forge::ReviewCommentSnapshot;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::app::App;
use crate::app::prompt_intent::ReviewCommentResolutionOutcome;
use crate::presentation::app_mode::{AppMode, ReviewCommentAction, ReviewCommentActionSelection};
use crate::presentation::review_comment;
use crate::runtime::EventResult;
use crate::ui::{RenderCacheStore, page};

/// Handles agent-resolution, selection, detail scrolling, and exit keys for
/// the session review-comment page.
pub(crate) async fn handle_with_cache(
    app: &mut App,
    render_cache_store: &RenderCacheStore,
    content_area: Rect,
    key: KeyEvent,
) -> EventResult {
    if exit_review_comments(app, &key) {
        return EventResult::Continue;
    }

    let mode = std::mem::replace(&mut app.mode, AppMode::List);
    let AppMode::ReviewComments {
        mut comment_actions,
        comment_error,
        comment_snapshot,
        diff,
        is_loading_comments,
        mut selected_comment_index,
        session_id,
        mut scroll_offset,
    } = mode
    else {
        app.mode = mode;

        return EventResult::Continue;
    };
    let item_count = page::review_comment::review_comment_item_count(comment_snapshot.as_ref());
    if key.code == KeyCode::Enter
        && key.modifiers == KeyModifiers::NONE
        && !comment_actions.is_empty()
        && let Some(snapshot) = comment_snapshot.as_ref()
    {
        let snapshot = snapshot.clone();
        let submitted_actions = comment_actions.clone();
        app.mode = AppMode::ReviewComments {
            comment_actions,
            comment_error,
            comment_snapshot,
            diff,
            is_loading_comments,
            selected_comment_index,
            session_id: session_id.clone(),
            scroll_offset,
        };
        let outcome = app
            .resolve_session_review_comments(&session_id, &snapshot, &submitted_actions)
            .await;
        apply_review_comment_resolution_outcome(app, outcome);

        return EventResult::Continue;
    }

    toggle_selected_comment_action(
        &key,
        comment_snapshot.as_ref(),
        selected_comment_index,
        &mut comment_actions,
    );
    match key.code {
        KeyCode::Char('j') if key.modifiers == KeyModifiers::NONE => {
            let next_index = next_selected_index(selected_comment_index, item_count);
            if next_index != selected_comment_index {
                selected_comment_index = next_index;
                scroll_offset = 0;
            }
        }
        KeyCode::Char('k') if key.modifiers == KeyModifiers::NONE => {
            let previous_index = previous_selected_index(selected_comment_index, item_count);
            if previous_index != selected_comment_index {
                selected_comment_index = previous_index;
                scroll_offset = 0;
            }
        }
        KeyCode::Down => {
            let max_scroll_offset = page::review_comment::review_comment_view_max_scroll_offset(
                comment_snapshot.as_ref(),
                comment_error.as_deref(),
                is_loading_comments,
                &diff,
                selected_comment_index,
                content_area,
                render_cache_store.markdown_render_cache(),
            );
            scroll_offset = scroll_offset
                .min(max_scroll_offset)
                .saturating_add(1)
                .min(max_scroll_offset);
        }
        KeyCode::Up => {
            scroll_offset = scroll_offset.saturating_sub(1);
        }
        _ => {}
    }

    app.mode = AppMode::ReviewComments {
        comment_actions,
        comment_error,
        comment_snapshot,
        diff,
        is_loading_comments,
        selected_comment_index,
        session_id,
        scroll_offset,
    };

    EventResult::Continue
}

/// Restores session view when the review-comments exit key is pressed.
fn exit_review_comments(app: &mut App, key: &KeyEvent) -> bool {
    if !matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
        return false;
    }

    let mode = std::mem::replace(&mut app.mode, AppMode::List);
    if let AppMode::ReviewComments { session_id, .. } = mode {
        app.mode = AppMode::View {
            session_id,
            scroll_offset: None,
        };
    } else {
        app.mode = mode;
    }

    true
}

/// Applies an address or deny toggle to the selected actionable thread.
fn toggle_selected_comment_action(
    key: &KeyEvent,
    comment_snapshot: Option<&ReviewCommentSnapshot>,
    selected_comment_index: usize,
    comment_actions: &mut Vec<ReviewCommentActionSelection>,
) {
    let action = match key.code {
        KeyCode::Char('a') if key.modifiers == KeyModifiers::NONE => ReviewCommentAction::Address,
        KeyCode::Char('d') if key.modifiers == KeyModifiers::NONE => ReviewCommentAction::Deny,
        _ => return,
    };
    let Some(thread_id) = comment_snapshot.and_then(|snapshot| {
        review_comment::selected_actionable_thread_id(snapshot, selected_comment_index)
    }) else {
        return;
    };

    review_comment::toggle_action(comment_actions, thread_id, action);
}

/// Applies presentation navigation returned by the review-comment workflow.
fn apply_review_comment_resolution_outcome(app: &mut App, outcome: ReviewCommentResolutionOutcome) {
    match outcome {
        ReviewCommentResolutionOutcome::KeepReviewComments => {}
        ReviewCommentResolutionOutcome::ShowSession { session_id } => {
            app.mode = AppMode::View {
                session_id,
                scroll_offset: None,
            };
        }
    }
}

/// Returns the next wrapped selection index.
fn next_selected_index(selected_index: usize, item_count: usize) -> usize {
    if item_count == 0 {
        return selected_index;
    }

    (selected_index.min(item_count - 1) + 1) % item_count
}

/// Returns the previous wrapped selection index.
fn previous_selected_index(selected_index: usize, item_count: usize) -> usize {
    if item_count == 0 {
        return selected_index;
    }
    let selected_index = selected_index.min(item_count - 1);
    if selected_index == 0 {
        return item_count - 1;
    }

    selected_index - 1
}

#[cfg(test)]
mod tests {
    use ag_forge::{
        ReviewComment, ReviewCommentAnchorSide, ReviewCommentSnapshot, ReviewCommentThread,
    };

    use super::*;
    use crate::domain::session::SessionId;

    fn comment_snapshot() -> ReviewCommentSnapshot {
        ReviewCommentSnapshot {
            pr_level_comments: vec![ReviewComment {
                author: "alice".to_string(),
                body: "General comment".to_string(),
            }],
            threads: vec![ReviewCommentThread {
                anchor_side: ReviewCommentAnchorSide::New,
                comments: vec![ReviewComment {
                    author: "bob".to_string(),
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

    #[tokio::test]
    async fn test_handle_marks_address_replaces_with_deny_and_toggles_off() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::ReviewComments {
            comment_actions: Vec::new(),
            comment_error: None,
            comment_snapshot: Some(comment_snapshot()),
            diff: String::new(),
            is_loading_comments: false,
            selected_comment_index: 0,
            session_id: "session-id".into(),
            scroll_offset: 0,
        };

        // Act
        for key_code in [KeyCode::Char('a'), KeyCode::Char('d'), KeyCode::Char('d')] {
            handle_with_cache(
                &mut app,
                &RenderCacheStore::default(),
                Rect::new(0, 0, 80, 24),
                KeyEvent::new(key_code, KeyModifiers::NONE),
            )
            .await;
        }

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::ReviewComments {
                ref comment_actions,
                ..
            } if comment_actions.is_empty()
        ));
    }

    #[tokio::test]
    async fn test_handle_selects_next_comment_and_resets_detail_scroll() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::ReviewComments {
            comment_actions: Vec::new(),
            comment_error: None,
            comment_snapshot: Some(comment_snapshot()),
            diff: String::new(),
            is_loading_comments: false,
            selected_comment_index: 0,
            session_id: "session-id".into(),
            scroll_offset: 4,
        };

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
            AppMode::ReviewComments {
                selected_comment_index: 1,
                scroll_offset: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_q_restores_session_view() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::ReviewComments {
            comment_actions: Vec::new(),
            comment_error: None,
            comment_snapshot: None,
            diff: String::new(),
            is_loading_comments: true,
            selected_comment_index: 0,
            session_id: "session-id".into(),
            scroll_offset: 0,
        };

        // Act
        handle_with_cache(
            &mut app,
            &RenderCacheStore::default(),
            Rect::new(0, 0, 80, 24),
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: None,
            } if session_id == "session-id"
        ));
    }

    #[tokio::test]
    async fn test_handle_selects_previous_comment_and_resets_detail_scroll() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::ReviewComments {
            comment_actions: Vec::new(),
            comment_error: None,
            comment_snapshot: Some(comment_snapshot()),
            diff: String::new(),
            is_loading_comments: false,
            selected_comment_index: 1,
            session_id: "session-id".into(),
            scroll_offset: 4,
        };

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
            AppMode::ReviewComments {
                selected_comment_index: 0,
                scroll_offset: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_down_scrolls_within_rendered_detail() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::ReviewComments {
            comment_actions: Vec::new(),
            comment_error: None,
            comment_snapshot: Some(comment_snapshot()),
            diff: String::new(),
            is_loading_comments: false,
            selected_comment_index: 0,
            session_id: "session-id".into(),
            scroll_offset: 0,
        };

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
            AppMode::ReviewComments {
                scroll_offset: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_up_decrements_scroll_and_other_keys_preserve_mode() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::ReviewComments {
            comment_actions: Vec::new(),
            comment_error: None,
            comment_snapshot: Some(comment_snapshot()),
            diff: String::new(),
            is_loading_comments: false,
            selected_comment_index: 0,
            session_id: "session-id".into(),
            scroll_offset: 2,
        };

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
            AppMode::ReviewComments {
                scroll_offset: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_batch_keys_reject_read_only_rows_and_preserve_failed_submission() {
        // Arrange
        let mut selected_app = crate::test_support::new_test_app_without_retained_base_dir().await;
        selected_app.mode = AppMode::ReviewComments {
            comment_actions: Vec::new(),
            comment_error: None,
            comment_snapshot: Some(comment_snapshot()),
            diff: String::new(),
            is_loading_comments: false,
            selected_comment_index: 1,
            session_id: "missing-session".into(),
            scroll_offset: 3,
        };
        let mut submit_app = crate::test_support::new_test_app_without_retained_base_dir().await;
        submit_app.mode = AppMode::ReviewComments {
            comment_actions: vec![ReviewCommentActionSelection {
                action: ReviewCommentAction::Address,
                thread_id: "thread-id".to_string(),
            }],
            comment_error: None,
            comment_snapshot: Some(comment_snapshot()),
            diff: String::new(),
            is_loading_comments: false,
            selected_comment_index: 1,
            session_id: "missing-session".into(),
            scroll_offset: 3,
        };

        // Act
        handle_with_cache(
            &mut selected_app,
            &RenderCacheStore::default(),
            Rect::new(0, 0, 80, 24),
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
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
            AppMode::ReviewComments {
                ref comment_actions,
                selected_comment_index: 1,
                scroll_offset: 3,
                ..
            } if comment_actions.is_empty()
        ));
        assert!(matches!(
            submit_app.mode,
            AppMode::ReviewComments {
                ref comment_actions,
                selected_comment_index: 1,
                scroll_offset: 3,
                ..
            } if comment_actions.len() == 1
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
}
