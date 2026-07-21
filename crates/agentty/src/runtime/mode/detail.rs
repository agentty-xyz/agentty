use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::app::App;
use crate::presentation::app_mode::AppMode;
use crate::runtime::EventResult;
use crate::ui::{RenderCacheStore, page};

/// Non-content rows excluded from issue-detail half-page scrolling.
const ISSUE_DETAIL_PAGE_INSET: u16 = 3;
/// Non-content rows excluded from review-detail half-page scrolling.
const REVIEW_DETAIL_PAGE_INSET: u16 = 5;

/// Handles key input while an issue or requested-review detail page is visible.
pub(crate) async fn handle_with_cache(
    app: &mut App,
    render_cache_store: &RenderCacheStore,
    content_area: Rect,
    key: KeyEvent,
) -> EventResult {
    if key.code == KeyCode::Char('a')
        && key.modifiers == KeyModifiers::NONE
        && let AppMode::IssueDetail {
            detail,
            error,
            issue,
            scroll_offset,
            ..
        } = &app.mode
    {
        let detail = detail.clone();
        let detail_error = error.clone();
        let issue = issue.clone();
        let issue_url = issue.web_url.clone();
        let scroll_offset = *scroll_offset;
        if let Err(error) = app.start_issue_session(&issue_url).await {
            app.mode = AppMode::IssueDetail {
                action_error: Some(format!("Failed to start issue session: {error}")),
                detail,
                error: detail_error,
                issue,
                scroll_offset,
            };
        }

        return EventResult::Continue;
    }

    handle_detail_with_cache(app, render_cache_store, content_area, key)
}

/// Applies read-only navigation and scrolling for issue and review details.
fn handle_detail_with_cache(
    app: &mut App,
    render_cache_store: &RenderCacheStore,
    content_area: Rect,
    key: KeyEvent,
) -> EventResult {
    if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
        app.mode = AppMode::List;

        return EventResult::Continue;
    }

    let (max_scroll_offset, half_page_inset) = match &app.mode {
        AppMode::IssueDetail {
            action_error,
            detail,
            error,
            issue,
            ..
        } => (
            page::issue_detail::issue_detail_max_scroll_offset(
                issue,
                detail.as_ref(),
                error.as_deref(),
                action_error.as_deref(),
                content_area,
                render_cache_store.markdown_render_cache(),
            ),
            ISSUE_DETAIL_PAGE_INSET,
        ),
        AppMode::ReviewDetail {
            comment_error,
            is_loading_comments,
            review,
            ..
        } => (
            page::review_detail::review_detail_max_scroll_offset(
                review,
                comment_error.as_deref(),
                *is_loading_comments,
                content_area,
                render_cache_store.markdown_render_cache(),
            ),
            REVIEW_DETAIL_PAGE_INSET,
        ),
        _ => (0, 0),
    };

    if let AppMode::IssueDetail { scroll_offset, .. }
    | AppMode::ReviewDetail { scroll_offset, .. } = &mut app.mode
    {
        let clamped_scroll_offset = (*scroll_offset).min(max_scroll_offset);
        *scroll_offset = match key.code {
            KeyCode::Down | KeyCode::Char('j') if key.modifiers == KeyModifiers::NONE => {
                clamped_scroll_offset
                    .saturating_add(1)
                    .min(max_scroll_offset)
            }
            KeyCode::Up | KeyCode::Char('k') if key.modifiers == KeyModifiers::NONE => {
                clamped_scroll_offset.saturating_sub(1)
            }
            KeyCode::Char('d') if key.modifiers == KeyModifiers::CONTROL => clamped_scroll_offset
                .saturating_add(detail_half_page_step(content_area, half_page_inset))
                .min(max_scroll_offset),
            KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => clamped_scroll_offset
                .saturating_sub(detail_half_page_step(content_area, half_page_inset)),
            KeyCode::Char('g') if key.modifiers == KeyModifiers::NONE => 0,
            KeyCode::Char('G') if key.modifiers == KeyModifiers::SHIFT => max_scroll_offset,
            KeyCode::Char('G') if key.modifiers == KeyModifiers::NONE => max_scroll_offset,
            _ => clamped_scroll_offset,
        };
    }

    EventResult::Continue
}

#[cfg(test)]
fn handle(app: &mut App, content_area: Rect, key: KeyEvent) -> EventResult {
    handle_detail_with_cache(app, &RenderCacheStore::default(), content_area, key)
}

/// Returns the half-page scroll step for one detail viewport.
fn detail_half_page_step(content_area: Rect, page_inset: u16) -> u16 {
    content_area
        .height
        .saturating_sub(page_inset)
        .saturating_div(2)
        .max(1)
}

#[cfg(test)]
mod tests {
    use ag_forge::{
        AssignedIssue, ForgeKind, IssueDetail, RequestedReview, RequestedReviewAudience,
    };
    use crossterm::event::KeyModifiers;

    use super::*;

    #[tokio::test]
    async fn test_handle_q_returns_from_issue_detail_to_list_mode() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::IssueDetail {
            action_error: None,
            detail: None,
            error: None,
            issue: assigned_issue(),
            scroll_offset: 0,
        };

        // Act
        let event_result = handle(
            &mut app,
            Rect::new(0, 0, 80, 20),
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_a_starts_session_for_issue_url() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let issue = assigned_issue();
        let expected_prompt = format!("Address this issue: {}", issue.web_url);
        app.mode = AppMode::IssueDetail {
            action_error: None,
            detail: None,
            error: None,
            issue,
            scroll_offset: 0,
        };

        // Act
        let event_result = handle_with_cache(
            &mut app,
            &RenderCacheStore::default(),
            Rect::new(0, 0, 80, 20),
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert_eq!(app.sessions.sessions().len(), 1);
        assert_eq!(app.sessions.sessions()[0].prompt, expected_prompt);
        assert!(matches!(app.mode, AppMode::View { .. }));
    }

    #[tokio::test]
    async fn test_handle_a_preserves_issue_detail_when_session_creation_fails() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        let issue = assigned_issue();
        let detail = issue_detail("Issue description remains visible.");
        app.mode = AppMode::IssueDetail {
            action_error: None,
            detail: Some(detail),
            error: None,
            issue,
            scroll_offset: 2,
        };

        // Act
        let event_result = handle_with_cache(
            &mut app,
            &RenderCacheStore::default(),
            Rect::new(0, 0, 80, 20),
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(app.sessions.sessions().is_empty());
        assert!(matches!(
            app.mode,
            AppMode::IssueDetail {
                action_error: Some(ref error),
                detail: Some(_),
                error: None,
                scroll_offset: 2,
                ..
            } if error.contains("Failed to start issue session:")
        ));
    }

    #[tokio::test]
    async fn test_handle_scrolls_issue_detail_page_to_bottom() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        let content_area = Rect::new(0, 0, 80, 8);
        let issue = assigned_issue();
        let detail = issue_detail("line 1\nline 2\nline 3\nline 4\nline 5\nline 6");
        let render_cache_store = RenderCacheStore::default();
        let expected_scroll_offset = page::issue_detail::issue_detail_max_scroll_offset(
            &issue,
            Some(&detail),
            None,
            None,
            content_area,
            render_cache_store.markdown_render_cache(),
        );
        app.mode = AppMode::IssueDetail {
            action_error: None,
            detail: Some(detail),
            error: None,
            issue,
            scroll_offset: 0,
        };

        // Act
        handle(
            &mut app,
            content_area,
            KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT),
        );

        // Assert
        assert!(expected_scroll_offset > 0);
        assert!(matches!(
            app.mode,
            AppMode::IssueDetail {
                scroll_offset,
                ..
            } if scroll_offset == expected_scroll_offset
        ));
    }

    #[tokio::test]
    async fn test_handle_q_returns_from_review_detail_to_list_mode() {
        // Arrange
        let mut app = new_test_app().await;

        // Act
        let event_result = handle(
            &mut app,
            Rect::new(0, 0, 80, 20),
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_scrolls_detail_page_down_and_up() {
        // Arrange
        let mut app = new_test_app().await;
        app.mode = AppMode::ReviewDetail {
            comment_error: None,
            is_loading_comments: false,
            review: requested_review("line 1\nline 2\nline 3\nline 4\nline 5\nline 6"),
            scroll_offset: 0,
        };

        // Act
        handle(
            &mut app,
            Rect::new(0, 0, 80, 8),
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );
        handle(
            &mut app,
            Rect::new(0, 0, 80, 8),
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::ReviewDetail {
                scroll_offset: 0,
                ..
            }
        ));
    }

    /// Verifies `G` moves the requested-review detail page to its rendered
    /// bottom offset.
    #[tokio::test]
    async fn test_handle_scrolls_detail_page_to_bottom() {
        // Arrange
        let mut app = new_test_app().await;
        let content_area = Rect::new(0, 0, 80, 8);
        let review = requested_review("line 1\nline 2\nline 3\nline 4\nline 5\nline 6");
        let render_cache_store = RenderCacheStore::default();
        let expected_scroll_offset = page::review_detail::review_detail_max_scroll_offset(
            &review,
            None,
            false,
            content_area,
            render_cache_store.markdown_render_cache(),
        );
        app.mode = AppMode::ReviewDetail {
            comment_error: None,
            is_loading_comments: false,
            review,
            scroll_offset: 0,
        };

        // Act
        handle(
            &mut app,
            content_area,
            KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT),
        );

        // Assert
        assert!(expected_scroll_offset > 0);
        assert!(matches!(
            app.mode,
            AppMode::ReviewDetail {
                scroll_offset,
                ..
            } if scroll_offset == expected_scroll_offset
        ));
    }

    /// Builds one assigned-issue row for shared detail navigation tests.
    fn assigned_issue() -> AssignedIssue {
        AssignedIssue {
            display_id: "#42".to_string(),
            repository: "agentty-xyz/agentty".to_string(),
            title: "Load issue details".to_string(),
            updated_at: None,
            web_url: "https://github.com/agentty-xyz/agentty/issues/42".to_string(),
        }
    }

    /// Builds one loaded issue detail for shared navigation tests.
    fn issue_detail(body: &str) -> IssueDetail {
        IssueDetail {
            assignees: Vec::new(),
            author: "octocat".to_string(),
            body: Some(body.to_string()),
            created_at: None,
            display_id: "#42".to_string(),
            labels: Vec::new(),
            repository: "agentty-xyz/agentty".to_string(),
            state: "OPEN".to_string(),
            title: "Load issue details".to_string(),
            updated_at: None,
            web_url: "https://github.com/agentty-xyz/agentty/issues/42".to_string(),
        }
    }

    /// Builds a minimal app in review-detail mode for key handling tests.
    async fn new_test_app() -> App {
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        crate::test_support::set_review_detail_mode(&mut app, requested_review("Detail body"));

        app
    }

    /// Builds one requested review snapshot for detail-mode tests.
    fn requested_review(body: &str) -> RequestedReview {
        RequestedReview {
            audience: RequestedReviewAudience::Personal,
            author: "octocat".to_string(),
            body: Some(body.to_string()),
            comment_snapshot: None,
            display_id: "#42".to_string(),
            forge_kind: ForgeKind::GitHub,
            repository: "agentty-xyz/agentty".to_string(),
            status_summary: None,
            title: "Add review detail page".to_string(),
            updated_at: Some("2026-04-27T21:30:00Z".to_string()),
            web_url: "https://example.com/42".to_string(),
        }
    }
}
