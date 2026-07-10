use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::app::App;
use crate::presentation::app_mode::AppMode;
use crate::runtime::EventResult;
use crate::ui::{RenderCacheStore, page};

/// Handles key input while a requested-review detail page is visible.
pub(crate) fn handle(
    app: &mut App,
    content_area: Rect,
    key: KeyEvent,
    render_cache_store: &RenderCacheStore,
) -> EventResult {
    if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
        app.mode = AppMode::List;

        return EventResult::Continue;
    }

    let max_scroll_offset = match &app.mode {
        AppMode::ReviewDetail {
            comment_error,
            is_loading_comments,
            review,
            ..
        } => page::review_detail::review_detail_max_scroll_offset(
            review,
            comment_error.as_deref(),
            *is_loading_comments,
            content_area,
            render_cache_store.markdown_render_cache(),
        ),
        _ => 0,
    };

    if let AppMode::ReviewDetail { scroll_offset, .. } = &mut app.mode {
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
                .saturating_add(review_detail_half_page_step(content_area))
                .min(max_scroll_offset),
            KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
                clamped_scroll_offset.saturating_sub(review_detail_half_page_step(content_area))
            }
            KeyCode::Char('g') if key.modifiers == KeyModifiers::NONE => 0,
            KeyCode::Char('G') if key.modifiers == KeyModifiers::SHIFT => max_scroll_offset,
            KeyCode::Char('G') if key.modifiers == KeyModifiers::NONE => max_scroll_offset,
            _ => clamped_scroll_offset,
        };
    }

    EventResult::Continue
}

/// Returns the half-page scroll step for the review detail viewport.
fn review_detail_half_page_step(content_area: Rect) -> u16 {
    content_area
        .height
        .saturating_sub(5)
        .saturating_div(2)
        .max(1)
}

#[cfg(test)]
mod tests {
    use ag_forge::{ForgeKind, RequestedReview, RequestedReviewAudience};
    use crossterm::event::KeyModifiers;

    use super::*;

    /// Routes one test key through a fresh render-cache boundary.
    fn handle(app: &mut App, content_area: Rect, key: KeyEvent) -> EventResult {
        let render_cache_store = RenderCacheStore::default();

        super::handle(app, content_area, key, &render_cache_store)
    }

    #[tokio::test]
    async fn test_handle_q_returns_to_list_mode() {
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
