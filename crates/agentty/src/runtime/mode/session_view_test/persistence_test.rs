use super::super::{ViewContext, ViewPendingUpdate, open_or_regenerate_review};
use super::support::new_test_app_with_session;
use crate::app::ReviewCacheEntry;
use crate::presentation::app_mode::AppMode;

#[tokio::test]
async fn test_open_or_regenerate_skips_when_loading_in_progress() {
    // Arrange
    let (mut app, _base_dir, session_id) = new_test_app_with_session().await;
    let review_agent = app.review_agent();
    app.review_cache.insert(
        session_id.clone().into(),
        ReviewCacheEntry::Loading {
            diff_hash: 42,
            review_agent,
        },
    );
    app.mode = AppMode::View {
        scroll_offset: None,
        session_id: session_id.clone().into(),
    };
    let view_context = ViewContext {
        scroll_offset: None,
        session_id: session_id.clone().into(),
        session_index: 0,
    };
    let mut pending_update = ViewPendingUpdate::from_context(&view_context);

    // Act
    open_or_regenerate_review(&mut app, &view_context, &mut pending_update);

    // Assert — cache and loading state are preserved, no duplicate spawned
    assert!(matches!(
        app.review_cache.get(session_id.as_str()),
        Some(ReviewCacheEntry::Loading { diff_hash: 42, .. })
    ));
    assert!(matches!(
        app.mode,
        AppMode::View {
            ref session_id,
            ..
        } if session_id == &view_context.session_id
    ));
}
