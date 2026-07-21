use ag_forge::{ReviewComment, ReviewCommentSnapshot, ReviewCommentThread};

/// One row in the grouped review-comment selector projection.
pub(crate) enum GroupedReviewCommentRow<'a> {
    /// Selectable standalone comment or inline thread.
    Entry(ReviewCommentEntry<'a>),
    /// Non-selectable heading rendered before one populated group.
    GroupLabel(&'static str),
}

/// One selectable review-comment entry and its detail source.
#[derive(Clone, Copy)]
pub(crate) enum ReviewCommentEntry<'a> {
    /// Review-request-wide discussion comment without an inline thread ID.
    General(&'a ReviewComment),
    /// Forge review thread attached to a file or line range.
    Thread(&'a ReviewCommentThread),
}

/// Returns the complete selector projection in unresolved, resolved, then
/// standalone order, including labels for each populated group.
pub(crate) fn grouped_review_comment_rows(
    snapshot: &ReviewCommentSnapshot,
) -> Vec<GroupedReviewCommentRow<'_>> {
    let mut rows = Vec::with_capacity(
        snapshot
            .threads
            .len()
            .saturating_add(snapshot.pr_level_comments.len())
            .saturating_add(3),
    );
    append_group_rows(
        &mut rows,
        "Unresolved",
        snapshot
            .threads
            .iter()
            .filter(|thread| !thread.is_resolved)
            .map(ReviewCommentEntry::Thread),
    );
    append_group_rows(
        &mut rows,
        "Resolved",
        snapshot
            .threads
            .iter()
            .filter(|thread| thread.is_resolved)
            .map(ReviewCommentEntry::Thread),
    );
    append_group_rows(
        &mut rows,
        "Standalone",
        snapshot
            .pr_level_comments
            .iter()
            .map(ReviewCommentEntry::General),
    );

    rows
}

/// Returns only selectable entries from one materialized grouped projection.
pub(crate) fn selectable_entries<'rows, 'snapshot>(
    rows: &'rows [GroupedReviewCommentRow<'snapshot>],
) -> impl Iterator<Item = ReviewCommentEntry<'snapshot>> + 'rows
where
    'snapshot: 'rows,
{
    rows.iter().filter_map(|row| match row {
        GroupedReviewCommentRow::Entry(entry) => Some(*entry),
        GroupedReviewCommentRow::GroupLabel(_) => None,
    })
}

/// Returns the selected standalone comment or inline thread from one
/// materialized grouped projection.
pub(crate) fn selected_entry<'snapshot>(
    rows: &[GroupedReviewCommentRow<'snapshot>],
    selected_comment_index: usize,
) -> Option<ReviewCommentEntry<'snapshot>> {
    selectable_entries(rows).nth(selected_comment_index)
}

/// Returns the forge-native identifier for the selected grouped thread row.
pub(crate) fn selected_thread_id(
    snapshot: &ReviewCommentSnapshot,
    selected_comment_index: usize,
) -> Option<&str> {
    let rows = grouped_review_comment_rows(snapshot);

    selected_entry(&rows, selected_comment_index).and_then(|entry| match entry {
        ReviewCommentEntry::General(_) => None,
        ReviewCommentEntry::Thread(thread) => Some(thread.id.as_str()),
    })
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
    let updated_rows = grouped_review_comment_rows(updated_snapshot);
    if let Some(updated_index) = selected_thread_id.and_then(|selected_thread_id| {
        selectable_entries(&updated_rows).position(
            |entry| matches!(entry, ReviewCommentEntry::Thread(thread) if thread.id == selected_thread_id),
        )
    }) {
        return updated_index;
    }

    let updated_item_count = updated_snapshot
        .threads
        .len()
        .saturating_add(updated_snapshot.pr_level_comments.len());

    previous_selected_index.min(updated_item_count.saturating_sub(1))
}

/// Adds one heading and its entries only when the group is populated.
fn append_group_rows<'a>(
    rows: &mut Vec<GroupedReviewCommentRow<'a>>,
    label: &'static str,
    entries: impl Iterator<Item = ReviewCommentEntry<'a>>,
) {
    let mut group_has_entries = false;
    for entry in entries {
        if !group_has_entries {
            rows.push(GroupedReviewCommentRow::GroupLabel(label));
            group_has_entries = true;
        }
        rows.push(GroupedReviewCommentRow::Entry(entry));
    }
}

#[cfg(test)]
mod tests {
    use ag_forge::{ReviewComment, ReviewCommentAnchorSide};

    use super::*;

    #[test]
    fn test_grouped_review_comment_rows_include_populated_labels_and_entries() {
        // Arrange
        let mut snapshot =
            snapshot_with_threads([thread("resolved", true), thread("unresolved", false)]);
        snapshot.pr_level_comments.push(ReviewComment {
            author: "reviewer".to_string(),
            body: "Standalone comment".to_string(),
        });

        // Act
        let rows = grouped_review_comment_rows(&snapshot);
        let labels_and_entries = rows
            .iter()
            .map(|row| match row {
                GroupedReviewCommentRow::Entry(ReviewCommentEntry::General(comment)) => {
                    format!("comment:{}", comment.body)
                }
                GroupedReviewCommentRow::Entry(ReviewCommentEntry::Thread(thread)) => {
                    format!("thread:{}", thread.id)
                }
                GroupedReviewCommentRow::GroupLabel(label) => format!("label:{label}"),
            })
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(
            labels_and_entries,
            vec![
                "label:Unresolved",
                "thread:unresolved",
                "label:Resolved",
                "thread:resolved",
                "label:Standalone",
                "comment:Standalone comment",
            ]
        );
    }

    #[test]
    fn test_grouped_review_comment_rows_omit_empty_group_labels() {
        // Arrange
        let snapshot = snapshot_with_threads([thread("unresolved", false)]);

        // Act
        let labels = grouped_review_comment_rows(&snapshot)
            .into_iter()
            .filter_map(|row| match row {
                GroupedReviewCommentRow::GroupLabel(label) => Some(label),
                GroupedReviewCommentRow::Entry(_) => None,
            })
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(labels, vec!["Unresolved"]);
    }

    #[test]
    fn test_selectable_entries_reuses_materialized_grouped_rows() {
        // Arrange
        let mut snapshot = snapshot_with_threads([thread("thread", false)]);
        snapshot.pr_level_comments.push(ReviewComment {
            author: "reviewer".to_string(),
            body: "Standalone comment".to_string(),
        });
        let rows = grouped_review_comment_rows(&snapshot);

        // Act
        let selected_entries = selectable_entries(&rows).collect::<Vec<_>>();

        // Assert
        assert!(matches!(
            selected_entries[0],
            ReviewCommentEntry::Thread(thread) if thread.id == "thread"
        ));
        assert!(matches!(
            selected_entries[1],
            ReviewCommentEntry::General(comment) if comment.body == "Standalone comment"
        ));
    }

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
