//! In-memory cache for review-request inline comment threads and PR-level
//! conversation comments.
//!
//! The cache is populated by the background review-request sync task and read
//! by the unified diff/comments page on every frame. Writes and reads only
//! hold the lock for the duration of an in-memory copy, so a plain
//! [`std::sync::Mutex`] is sufficient.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ag_forge::ReviewCommentSnapshot;

use crate::domain::session::SessionId;

/// Shared review-comment cache handle used by the background sync task and
/// the UI render path.
#[derive(Clone, Default)]
pub struct ReviewCommentCache {
    inner: Arc<Mutex<HashMap<SessionId, ReviewCommentSnapshot>>>,
}

impl ReviewCommentCache {
    /// Returns the cached review-comment snapshot for `session_id`.
    pub fn snapshot(&self, session_id: &SessionId) -> Option<ReviewCommentSnapshot> {
        let guard = self.inner.lock().ok()?;

        guard.get(session_id).cloned()
    }

    /// Stores `snapshot` for `session_id`, sorting the inline threads by
    /// `(path, line, side)` so the UI does not sort during render. Returns
    /// whether the cached content changed since the previous write.
    pub fn record_snapshot(
        &self,
        session_id: SessionId,
        mut snapshot: ReviewCommentSnapshot,
    ) -> bool {
        snapshot.threads.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.line.unwrap_or(0).cmp(&right.line.unwrap_or(0)))
                .then_with(|| {
                    anchor_side_order(left.anchor_side).cmp(&anchor_side_order(right.anchor_side))
                })
        });

        let Ok(mut guard) = self.inner.lock() else {
            return false;
        };
        let changed = guard.get(&session_id) != Some(&snapshot);
        guard.insert(session_id, snapshot);

        changed
    }

    /// Drops any cached state for `session_id`, for example when a session is
    /// deleted or when its review request transitions out of the open state.
    pub fn forget(&self, session_id: &SessionId) -> bool {
        let Ok(mut guard) = self.inner.lock() else {
            return false;
        };

        guard.remove(session_id).is_some()
    }
}

/// Returns a stable ordering for review-comment anchor sides.
fn anchor_side_order(anchor_side: ag_forge::ReviewCommentAnchorSide) -> u8 {
    match anchor_side {
        ag_forge::ReviewCommentAnchorSide::File => 0,
        ag_forge::ReviewCommentAnchorSide::Old => 1,
        ag_forge::ReviewCommentAnchorSide::New => 2,
    }
}

#[cfg(test)]
mod tests {
    use ag_forge::{
        ReviewComment, ReviewCommentAnchorSide, ReviewCommentSnapshot, ReviewCommentThread,
    };

    use super::*;

    fn review_thread(path: &str, line: u32, resolved: bool) -> ReviewCommentThread {
        ReviewCommentThread {
            anchor_side: ReviewCommentAnchorSide::New,
            comments: vec![ReviewComment {
                author: "alice".to_string(),
                body: format!("Comment on {path}"),
            }],
            is_outdated: Some(false),
            is_resolved: resolved,
            line: Some(line),
            path: path.to_string(),
            start_line: None,
        }
    }

    fn snapshot_from_threads(threads: Vec<ReviewCommentThread>) -> ReviewCommentSnapshot {
        ReviewCommentSnapshot {
            pr_level_comments: Vec::new(),
            threads,
        }
    }

    #[test]
    fn record_snapshot_sorts_threads_by_path_and_line_before_caching() {
        // Arrange
        let cache = ReviewCommentCache::default();
        let session_id: SessionId = "session-1".into();
        let snapshot = snapshot_from_threads(vec![
            review_thread("src/zzz.rs", 10, false),
            review_thread("src/aaa.rs", 99, false),
            review_thread("src/aaa.rs", 5, false),
        ]);

        // Act
        cache.record_snapshot(session_id.clone(), snapshot);
        let stored = cache
            .snapshot(&session_id)
            .expect("cache should expose the inserted snapshot");

        // Assert
        assert_eq!(
            stored
                .threads
                .iter()
                .map(|thread| (thread.path.clone(), thread.line))
                .collect::<Vec<_>>(),
            vec![
                ("src/aaa.rs".to_string(), Some(5)),
                ("src/aaa.rs".to_string(), Some(99)),
                ("src/zzz.rs".to_string(), Some(10)),
            ]
        );
    }

    #[test]
    fn record_snapshot_preserves_pr_level_comments() {
        // Arrange
        let cache = ReviewCommentCache::default();
        let session_id: SessionId = "session-pr-level".into();
        let snapshot = ReviewCommentSnapshot {
            pr_level_comments: vec![ReviewComment {
                author: "carol".to_string(),
                body: "Overall looks good.".to_string(),
            }],
            threads: Vec::new(),
        };

        // Act
        cache.record_snapshot(session_id.clone(), snapshot);
        let stored = cache
            .snapshot(&session_id)
            .expect("cache should expose the inserted snapshot");

        // Assert
        assert_eq!(stored.pr_level_comments.len(), 1);
        assert_eq!(stored.pr_level_comments[0].author, "carol");
    }

    #[test]
    fn record_snapshot_reports_whether_content_changed() {
        // Arrange
        let cache = ReviewCommentCache::default();
        let session_id: SessionId = "session-change".into();
        let baseline = snapshot_from_threads(vec![review_thread("src/a.rs", 1, false)]);

        // Act
        let first_change = cache.record_snapshot(session_id.clone(), baseline.clone());
        let second_change = cache.record_snapshot(session_id.clone(), baseline);
        let mutated = snapshot_from_threads(vec![review_thread("src/a.rs", 1, true)]);
        let third_change = cache.record_snapshot(session_id, mutated);

        // Assert
        assert!(first_change, "initial insert should report change");
        assert!(!second_change, "identical insert should not report change");
        assert!(third_change, "resolution flag change should report change");
    }

    #[test]
    fn forget_removes_cached_state_and_reports_change() {
        // Arrange
        let cache = ReviewCommentCache::default();
        let session_id: SessionId = "session-drop".into();
        cache.record_snapshot(
            session_id.clone(),
            snapshot_from_threads(vec![review_thread("src/a.rs", 1, false)]),
        );

        // Act
        let removed = cache.forget(&session_id);
        let stored = cache.snapshot(&session_id);

        // Assert
        assert!(removed, "forget should report removal of an existing entry");
        assert!(stored.is_none());
    }

    #[test]
    fn forget_returns_false_when_no_cached_entry_exists() {
        // Arrange
        let cache = ReviewCommentCache::default();
        let session_id: SessionId = "session-missing".into();

        // Act
        let removed = cache.forget(&session_id);

        // Assert
        assert!(!removed, "forget should report nothing to remove");
    }
}
