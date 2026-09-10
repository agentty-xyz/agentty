use ag_forge::{
    ReviewComment, ReviewCommentAnchorSide, ReviewCommentSnapshot, ReviewCommentThread,
};

use super::super::app_mode::ReviewCommentSelection;
use super::{
    GroupedReviewCommentRow, ReviewCommentEntry, grouped_review_comment_rows, is_selected,
    retain_actionable_selections, retarget_selected_index, selectable_entries, selected_thread_id,
    toggle_selection,
};

#[test]
fn test_grouped_review_comment_rows_include_populated_labels_and_entries() {
    // Arrange
    let mut outdated = thread("outdated", false);
    outdated.is_outdated = Some(true);
    let mut resolved_outdated = thread("resolved-outdated", true);
    resolved_outdated.is_outdated = Some(true);
    let mut snapshot = snapshot_with_threads([
        thread("resolved", true),
        outdated,
        resolved_outdated,
        thread("unresolved", false),
    ]);
    snapshot.pr_level_comments.push(ReviewComment {
        author: "reviewer".to_string(),
        authored_by_current_user: false,
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
            "label:Outdated",
            "thread:outdated",
            "label:Resolved",
            "thread:resolved",
            "thread:resolved-outdated",
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
        authored_by_current_user: false,
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

#[test]
fn test_toggle_selection_adds_and_removes_thread() {
    // Arrange
    let mut selections = Vec::new();

    // Act
    toggle_selection(&mut selections, "thread");
    let selected = selections.clone();
    toggle_selection(&mut selections, "thread");

    // Assert
    assert_eq!(
        selected,
        vec![ReviewCommentSelection {
            thread_id: "thread".to_string(),
        }]
    );
    assert_eq!(
        selections,
        [] as [crate::presentation::app_mode::ReviewCommentSelection; 0]
    );
}

#[test]
fn test_retain_actionable_selections_removes_stale_threads() {
    // Arrange
    let snapshot = snapshot_with_threads([thread("current", false), thread("resolved", true)]);
    let mut selections = vec![
        ReviewCommentSelection {
            thread_id: "current".to_string(),
        },
        ReviewCommentSelection {
            thread_id: "resolved".to_string(),
        },
        ReviewCommentSelection {
            thread_id: "missing".to_string(),
        },
    ];

    // Act
    retain_actionable_selections(&mut selections, &snapshot);

    // Assert
    assert_eq!(selections.len(), 1);
    assert!(is_selected(&selections, "current"));
    assert!(!is_selected(&selections, "resolved"));
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
            authored_by_current_user: false,
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
