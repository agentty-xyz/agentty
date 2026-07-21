use ag_forge::{ReviewCommentSnapshot, ReviewCommentThread};

/// Returns inline review threads in the unresolved-then-resolved order used
/// by the review-comment page.
pub(crate) fn grouped_threads(
    snapshot: &ReviewCommentSnapshot,
) -> impl Iterator<Item = &ReviewCommentThread> {
    snapshot
        .threads
        .iter()
        .filter(|thread| !thread.is_resolved)
        .chain(snapshot.threads.iter().filter(|thread| thread.is_resolved))
}

/// Returns the forge-native identifier for the selected grouped thread row.
pub(crate) fn selected_thread_id(
    snapshot: &ReviewCommentSnapshot,
    selected_comment_index: usize,
) -> Option<&str> {
    grouped_threads(snapshot)
        .nth(selected_comment_index)
        .map(|thread| thread.id.as_str())
}

/// Retargets a positional selection to the same forge thread in an updated
/// snapshot, falling back to the nearest valid row if the thread disappeared.
pub(crate) fn retarget_selected_index(
    previous_snapshot: Option<&ReviewCommentSnapshot>,
    previous_selected_index: usize,
    updated_snapshot: &ReviewCommentSnapshot,
) -> usize {
    let selected_thread_id = previous_snapshot
        .and_then(|snapshot| selected_thread_id(snapshot, previous_selected_index));
    if let Some(updated_index) = selected_thread_id.and_then(|selected_thread_id| {
        grouped_threads(updated_snapshot).position(|thread| thread.id == selected_thread_id)
    }) {
        return updated_index;
    }

    let updated_item_count = updated_snapshot
        .threads
        .len()
        .saturating_add(updated_snapshot.pr_level_comments.len());

    previous_selected_index.min(updated_item_count.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use ag_forge::{ReviewComment, ReviewCommentAnchorSide};

    use super::*;

    #[test]
    fn test_retarget_selected_index_follows_thread_between_resolution_groups() {
        // Arrange
        let previous_snapshot =
            snapshot_with_threads([thread("selected", false), thread("other", false)]);
        let updated_snapshot =
            snapshot_with_threads([thread("selected", true), thread("other", false)]);

        // Act
        let updated_index = retarget_selected_index(Some(&previous_snapshot), 0, &updated_snapshot);

        // Assert
        assert_eq!(updated_index, 1);
        assert_eq!(
            selected_thread_id(&updated_snapshot, updated_index),
            Some("selected")
        );
    }

    #[test]
    fn test_retarget_selected_index_clamps_when_selected_thread_disappears() {
        // Arrange
        let previous_snapshot =
            snapshot_with_threads([thread("first", false), thread("selected", false)]);
        let updated_snapshot = snapshot_with_threads([thread("remaining", false)]);

        // Act
        let updated_index = retarget_selected_index(Some(&previous_snapshot), 1, &updated_snapshot);
        let empty_index = retarget_selected_index(None, 4, &ReviewCommentSnapshot::default());

        // Assert
        assert_eq!(updated_index, 0);
        assert_eq!(empty_index, 0);
    }

    /// Builds a snapshot from inline threads without standalone comments.
    fn snapshot_with_threads<const THREAD_COUNT: usize>(
        threads: [ReviewCommentThread; THREAD_COUNT],
    ) -> ReviewCommentSnapshot {
        ReviewCommentSnapshot {
            pr_level_comments: Vec::new(),
            threads: Vec::from(threads),
        }
    }

    /// Builds one current or resolved inline thread.
    fn thread(id: &str, is_resolved: bool) -> ReviewCommentThread {
        ReviewCommentThread {
            anchor_side: ReviewCommentAnchorSide::New,
            comments: vec![ReviewComment {
                author: "reviewer".to_string(),
                body: "Review comment".to_string(),
            }],
            id: id.to_string(),
            is_outdated: Some(false),
            is_resolved,
            line: Some(1),
            path: "src/main.rs".to_string(),
            start_line: None,
        }
    }
}
