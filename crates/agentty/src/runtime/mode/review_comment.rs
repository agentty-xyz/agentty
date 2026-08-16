use ag_forge::ReviewCommentSnapshot;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::app::App;
use crate::app::prompt_intent::ReviewCommentResolutionOutcome;
use crate::presentation::app_mode::{
    AppMode, DiffReviewComments, DiffScrollCache, DiffSidebarFocus, ReviewCommentAction,
    ReviewCommentActionSelection,
};
use crate::presentation::review_comment;
use crate::runtime::EventResult;
use crate::ui::{RenderCacheStore, page};

/// Handles agent-resolution, selection, and detail scrolling while the
/// Comments section of the unified diff workspace is focused.
pub(crate) async fn handle_with_cache(
    app: &mut App,
    render_cache_store: &RenderCacheStore,
    content_area: Rect,
    key: KeyEvent,
) -> EventResult {
    let mode = std::mem::replace(&mut app.mode, AppMode::List);
    let AppMode::Diff {
        diff,
        file_explorer_selected_index,
        focus,
        line_comments,
        preview,
        review_comments: Some(mut review_comments),
        restore,
        mut scroll_cache,
        session_id,
        mut scroll_offset,
        selected_diff_line_index,
    } = mode
    else {
        app.mode = mode;

        return EventResult::Continue;
    };
    let can_reply = session_allows_review_comment_reply(app, session_id.as_str());
    let item_count =
        page::review_comment::review_comment_item_count(review_comments.comment_snapshot.as_ref());
    if key.code == KeyCode::Enter
        && key.modifiers == KeyModifiers::NONE
        && !review_comments.comment_actions.is_empty()
        && let Some(snapshot) = review_comments.comment_snapshot.as_ref()
    {
        let snapshot = snapshot.clone();
        let submitted_actions = review_comments.comment_actions.clone();
        app.mode = AppMode::Diff {
            diff,
            file_explorer_selected_index,
            focus,
            line_comments,
            preview,
            review_comments: Some(review_comments),
            restore,
            scroll_cache,
            selected_diff_line_index,
            session_id: session_id.clone(),
            scroll_offset,
        };
        let outcome = app
            .resolve_session_review_comments(&session_id, &snapshot, &submitted_actions)
            .await;
        apply_review_comment_resolution_outcome(app, outcome);

        return EventResult::Continue;
    }

    handle_review_comment_navigation(
        &ReviewCommentNavigationInput {
            can_reply,
            content_area,
            diff: &diff,
            item_count,
            render_cache_store,
        },
        key,
        &mut review_comments,
        &mut scroll_cache,
        &mut scroll_offset,
    );

    app.mode = AppMode::Diff {
        diff,
        file_explorer_selected_index,
        focus,
        line_comments,
        preview,
        review_comments: Some(review_comments),
        restore,
        scroll_cache,
        selected_diff_line_index,
        session_id,
        scroll_offset,
    };

    EventResult::Continue
}

/// Immutable inputs used while navigating review comments.
struct ReviewCommentNavigationInput<'a> {
    can_reply: bool,
    content_area: Rect,
    diff: &'a str,
    item_count: usize,
    render_cache_store: &'a RenderCacheStore,
}

/// Applies comment selection, marking, focus, and detail-scroll keys.
fn handle_review_comment_navigation(
    input: &ReviewCommentNavigationInput<'_>,
    key: KeyEvent,
    review_comments: &mut DiffReviewComments,
    scroll_cache: &mut Option<DiffScrollCache>,
    scroll_offset: &mut u16,
) {
    if input.can_reply {
        toggle_selected_comment_action(
            &key,
            review_comments.comment_snapshot.as_ref(),
            review_comments.selected_comment_index,
            &mut review_comments.comment_actions,
        );
    }
    match key.code {
        KeyCode::Char('j') if key.modifiers == KeyModifiers::NONE => {
            let next_index =
                next_selected_index(review_comments.selected_comment_index, input.item_count);
            if next_index != review_comments.selected_comment_index {
                review_comments.selected_comment_index = next_index;
                *scroll_offset = 0;
            }
        }
        KeyCode::Char('k') if key.modifiers == KeyModifiers::NONE => {
            let previous_index =
                previous_selected_index(review_comments.selected_comment_index, input.item_count);
            if previous_index != review_comments.selected_comment_index {
                review_comments.selected_comment_index = previous_index;
                *scroll_offset = 0;
            }
        }
        KeyCode::Down => {
            let max_scroll_offset = review_comment_max_scroll_offset(
                input.render_cache_store,
                input.content_area,
                input.diff,
                review_comments.comment_snapshot.as_ref(),
                review_comments.comment_error.as_deref(),
                review_comments.is_loading_comments,
                review_comments.selected_comment_index,
            );
            *scroll_offset = increment_scroll_offset(*scroll_offset, max_scroll_offset);
        }
        KeyCode::Up => {
            *scroll_offset = scroll_offset.saturating_sub(1);
        }
        KeyCode::Esc => {
            focus_files(review_comments, scroll_cache, scroll_offset);
        }
        KeyCode::Char('f') if key.modifiers == KeyModifiers::NONE => {
            focus_files(review_comments, scroll_cache, scroll_offset);
        }
        _ => {}
    }
}

fn focus_files(
    review_comments: &mut DiffReviewComments,
    scroll_cache: &mut Option<DiffScrollCache>,
    scroll_offset: &mut u16,
) {
    review_comments.sidebar_focus = DiffSidebarFocus::Files;
    *scroll_cache = None;
    *scroll_offset = 0;
}

fn review_comment_max_scroll_offset(
    render_cache_store: &RenderCacheStore,
    content_area: Rect,
    diff: &str,
    comment_snapshot: Option<&ReviewCommentSnapshot>,
    comment_error: Option<&str>,
    is_loading_comments: bool,
    selected_comment_index: usize,
) -> u16 {
    page::review_comment::review_comment_view_max_scroll_offset(
        comment_snapshot,
        comment_error,
        is_loading_comments,
        diff,
        page::review_comment::ReviewCommentRenderCaches {
            diff_layout: render_cache_store.diff_layout_cache(),
            markdown: render_cache_store.markdown_render_cache(),
        },
        selected_comment_index,
        content_area,
    )
}

/// Advances one detail row while clamping stale and terminal offsets.
fn increment_scroll_offset(scroll_offset: u16, max_scroll_offset: u16) -> u16 {
    scroll_offset
        .min(max_scroll_offset)
        .saturating_add(1)
        .min(max_scroll_offset)
}

/// Returns whether the review-comments page still belongs to a session that
/// may accept direct user-driven comment work.
fn session_allows_review_comment_reply(app: &App, session_id: &str) -> bool {
    app.sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .is_some_and(crate::domain::session::Session::allows_review_comment_reply)
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
    use crate::domain::session::{SessionId, SessionRole, Status};
    use crate::presentation::app_mode::{
        DiffFocus, DiffLineComments, DiffPreview, DiffReviewComments,
    };
    use crate::test_support::SessionFixtureBuilder;

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

    fn review_comment_mode(
        session_id: &str,
        comment_snapshot: Option<ReviewCommentSnapshot>,
        comment_actions: Vec<ReviewCommentActionSelection>,
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
                comment_actions,
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
    async fn test_handle_marks_address_replaces_with_deny_and_toggles_off() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.sessions.push_session(
            SessionFixtureBuilder::new()
                .id("session-id")
                .status(Status::Review)
                .build(),
        );
        app.mode = review_comment_mode("session-id", Some(comment_snapshot()), Vec::new(), 0, 0);

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
            AppMode::Diff {
                review_comments: Some(DiffReviewComments {
                    ref comment_actions,
                    ..
                }),
                ..
            } if comment_actions.is_empty()
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
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                review_comments: Some(DiffReviewComments {
                    ref comment_actions,
                    ..
                }),
                ..
            } if comment_actions.is_empty()
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
            vec![ReviewCommentActionSelection {
                action: ReviewCommentAction::Address,
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
            AppMode::Diff {
                review_comments: Some(DiffReviewComments {
                    ref comment_actions,
                    selected_comment_index: 1,
                    ..
                }),
                scroll_offset: 3,
                ..
            } if comment_actions.is_empty()
        ));
        assert!(matches!(
            submit_app.mode,
            AppMode::Diff {
                review_comments: Some(DiffReviewComments {
                    ref comment_actions,
                    selected_comment_index: 1,
                    ..
                }),
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
