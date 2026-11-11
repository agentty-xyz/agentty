use super::{PENDING_AT_MENTION_LOADS, finish_pending_load, track_pending_load};
use crate::domain::session::SessionId;

/// Returns whether the registry still tracks a load for `session_id`.
fn has_pending_load(session_id: &str) -> bool {
    PENDING_AT_MENTION_LOADS
        .lock()
        .expect("at-mention registry")
        .contains_key(session_id)
}

#[tokio::test]
async fn finish_pending_load_removes_only_the_current_request() {
    // Arrange
    let session_id = "at-mention-finish";
    let handle = tokio::spawn(std::future::pending::<()>());
    track_pending_load(SessionId::from(session_id), 2, handle);

    // Act
    finish_pending_load(session_id, 1);
    let kept_after_stale_finish = has_pending_load(session_id);
    finish_pending_load(session_id, 2);
    let kept_after_current_finish = has_pending_load(session_id);

    // Assert
    assert!(kept_after_stale_finish);
    assert!(!kept_after_current_finish);
}
