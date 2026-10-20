use ag_forge::{
    ReviewComment, ReviewCommentAnchorSide, ReviewCommentSnapshot, ReviewCommentThread,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use super::{
    ReviewCommentNavigationInput, apply_review_comment_resolution_outcome,
    handle_review_comment_navigation, handle_with_cache, next_selected_index,
    previous_selected_index,
};
use crate::app::prompt_intent::ReviewCommentResolutionOutcome;
use crate::domain::session::{SessionId, Status};
use crate::presentation::app_mode::{
    AppMode, DiffFocus, DiffLineComments, DiffPreview, DiffReviewComments, DiffScrollCache,
    DiffSidebarFocus, ReviewCommentSelection, ViewportRect,
};
use crate::test_support::SessionFixtureBuilder;
use crate::ui::{RenderCacheStore, page};

const CONTENT_AREA: Rect = Rect::new(0, 0, 80, 24);

/// Only the in-memory values that comment navigation can change.
struct NavigationState {
    comments: DiffReviewComments,
    scroll_cache: Option<DiffScrollCache>,
    scroll_offset: u16,
}

impl NavigationState {
    fn new(selected_index: usize, scroll_offset: u16) -> Self {
        Self {
            comments: comment_state(selected_index),
            scroll_cache: None,
            scroll_offset,
        }
    }

    fn press(&mut self, key: KeyEvent, content_area: Rect, can_reply: bool) {
        let render_cache_store = RenderCacheStore::default();
        let input = ReviewCommentNavigationInput {
            can_reply,
            content_area,
            diff: "",
            item_count: page::review_comment::review_comment_item_count(
                self.comments.comment_snapshot.as_ref(),
            ),
            render_cache_store: &render_cache_store,
        };
        handle_review_comment_navigation(
            &input,
            key,
            &mut self.comments,
            &mut self.scroll_cache,
            &mut self.scroll_offset,
        );
    }
}

fn comment_state(selected_comment_index: usize) -> DiffReviewComments {
    DiffReviewComments {
        selected_comments: Vec::new(),
        comment_error: None,
        is_loading_comments: false,
        comment_snapshot: Some(comment_snapshot()),
        request_id: 1,
        selected_comment_index,
        sidebar_focus: DiffSidebarFocus::Comments,
    }
}

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
    selected_comment_index: usize,
    scroll_offset: u16,
    selected_comments: Vec<ReviewCommentSelection>,
) -> AppMode {
    let mut comments = comment_state(selected_comment_index);
    comments.selected_comments = selected_comments;

    AppMode::Diff {
        diff: String::new(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: Some(comments),
        restore: None,
        scroll_cache: None,
        session_id: session_id.into(),
        scroll_offset,
    }
}

#[test]
fn navigation_uses_space_as_only_comment_selection_key() {
    // Arrange
    let mut state = NavigationState::new(0, 0);

    // Act
    for code in [KeyCode::Char('a'), KeyCode::Char('d')] {
        state.press(KeyEvent::new(code, KeyModifiers::NONE), CONTENT_AREA, true);
    }

    // Assert
    assert_eq!(
        state.comments.selected_comments,
        Vec::<ReviewCommentSelection>::new()
    );

    // Act
    state.press(
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
        CONTENT_AREA,
        true,
    );

    // Assert
    assert_eq!(
        state.comments.selected_comments,
        [ReviewCommentSelection {
            thread_id: "thread-id".to_string(),
        }]
    );

    // Act
    state.press(
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
        CONTENT_AREA,
        true,
    );

    // Assert
    assert_eq!(
        state.comments.selected_comments,
        Vec::<ReviewCommentSelection>::new()
    );
}

#[test]
fn navigation_selects_adjacent_comments_and_resets_detail_scroll() {
    // Arrange
    for (code, initial, expected) in [(KeyCode::Char('j'), 0, 1), (KeyCode::Char('k'), 1, 0)] {
        let mut state = NavigationState::new(initial, 4);

        // Act
        state.press(KeyEvent::new(code, KeyModifiers::NONE), CONTENT_AREA, true);

        // Assert
        assert_eq!(state.comments.selected_comment_index, expected);
        assert_eq!(state.scroll_offset, 0);
    }
}

#[test]
fn navigation_focuses_files_and_clears_detail_scroll_and_cache() {
    // Arrange
    for code in [KeyCode::Char('f'), KeyCode::Esc] {
        let mut state = NavigationState::new(0, 3);
        state.scroll_cache = Some(DiffScrollCache {
            content_area: ViewportRect {
                height: 24,
                width: 80,
                x: 0,
                y: 0,
            },
            file_explorer_selected_index: 0,
            max_scroll_offset: 4,
        });

        // Act
        state.press(KeyEvent::new(code, KeyModifiers::NONE), CONTENT_AREA, true);

        // Assert
        assert_eq!(state.comments.sidebar_focus, DiffSidebarFocus::Files);
        assert_eq!(state.scroll_offset, 0);
        assert!(state.scroll_cache.is_none());
    }
}

#[test]
fn navigation_down_scrolls_within_rendered_detail() {
    // Arrange
    let mut state = NavigationState::new(0, 0);

    // Act
    state.press(
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        Rect::new(0, 0, 80, 8),
        true,
    );

    // Assert
    assert_eq!(state.scroll_offset, 1);
}

#[test]
fn navigation_up_decrements_scroll_and_other_keys_preserve_it() {
    // Arrange
    let mut state = NavigationState::new(0, 2);

    // Act
    for code in [KeyCode::Up, KeyCode::Char('x')] {
        state.press(KeyEvent::new(code, KeyModifiers::NONE), CONTENT_AREA, true);
    }

    // Assert
    assert_eq!(state.scroll_offset, 1);
}

#[test]
fn navigation_rejects_read_only_rows_and_disabled_replies() {
    // Arrange
    for (selected_index, can_reply) in [(1, true), (0, false)] {
        let mut state = NavigationState::new(selected_index, 3);

        // Act
        state.press(
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
            CONTENT_AREA,
            can_reply,
        );

        // Assert
        assert_eq!(
            state.comments.selected_comments,
            Vec::<ReviewCommentSelection>::new()
        );
        assert_eq!(state.comments.selected_comment_index, selected_index);
        assert_eq!(state.scroll_offset, 3);
    }
}

#[tokio::test]
async fn failed_submission_preserves_selected_comments_and_scroll() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = review_comment_mode(
        "missing-session",
        1,
        3,
        vec![ReviewCommentSelection {
            thread_id: "thread-id".to_string(),
        }],
    );

    // Act
    handle_with_cache(
        &mut app,
        &RenderCacheStore::default(),
        CONTENT_AREA,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            review_comments: Some(DiffReviewComments {
                ref selected_comments,
                selected_comment_index: 1,
                ..
            }),
            scroll_offset: 3,
            ..
        } if selected_comments == &[ReviewCommentSelection { thread_id: "thread-id".to_string() }]
    ));
}

#[test]
fn resolution_outcome_shows_session() {
    // Arrange
    let mut mode = AppMode::List;
    let session_id = SessionId::from("session-id");

    // Act
    apply_review_comment_resolution_outcome(
        &mut mode,
        ReviewCommentResolutionOutcome::ShowSession {
            session_id: session_id.clone(),
        },
    );

    // Assert
    assert!(matches!(mode, AppMode::View {
        session_id: viewed_session_id,
        scroll_offset: None,
    } if viewed_session_id == session_id));
}

#[tokio::test]
async fn dispatch_preserves_non_review_comment_modes() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;

    // Act, Assert
    for code in [KeyCode::Esc, KeyCode::Char('x')] {
        handle_with_cache(
            &mut app,
            &RenderCacheStore::default(),
            CONTENT_AREA,
            KeyEvent::new(code, KeyModifiers::NONE),
        )
        .await;
        assert!(matches!(app.mode, AppMode::List));
    }
}

#[test]
fn selection_helpers_wrap_clamp_and_preserve_empty_selection() {
    // Arrange, Act, Assert
    assert_eq!(next_selected_index(0, 0), 0);
    assert_eq!(next_selected_index(1, 2), 0);
    assert_eq!(next_selected_index(usize::MAX, 2), 0);
    assert_eq!(previous_selected_index(0, 0), 0);
    assert_eq!(previous_selected_index(0, 2), 1);
    assert_eq!(previous_selected_index(usize::MAX, 2), 0);
}

#[tokio::test]
async fn dispatch_allows_busy_session_selection_and_rejects_a_missing_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.sessions.push_session(
        SessionFixtureBuilder::new()
            .id("session-id")
            .status(Status::Rebasing)
            .build(),
    );
    for (session_id, expected_count) in [("session-id", 1), ("missing-session", 0)] {
        app.mode = review_comment_mode(session_id, 0, 0, Vec::new());

        // Act
        handle_with_cache(
            &mut app,
            &RenderCacheStore::default(),
            CONTENT_AREA,
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(app.mode,
            AppMode::Diff { review_comments: Some(DiffReviewComments { ref selected_comments, .. }), .. }
            if selected_comments.len() == expected_count
        ));
    }
}
