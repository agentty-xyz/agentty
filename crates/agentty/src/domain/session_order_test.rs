//! Grouped session-list ordering, selection navigation, and archive action
//! tests.

use super::{
    ArchiveAction, GroupedSessionRow, archive_actions, grouped_session_rows, next_selectable_index,
    preferred_initial_session_index, previous_selectable_index, selectable_session_indexes,
};
use crate::domain::session::Status;

#[test]
fn test_preferred_initial_session_index_prefers_active_group_when_available() {
    // Arrange
    let sessions = vec![
        crate::test_support::titled_session_fixture("archive-1", Status::Done),
        crate::test_support::titled_session_fixture("active-1", Status::Review),
        crate::test_support::titled_session_fixture("merge-1", Status::Queued),
    ];

    // Act
    let selected_index = preferred_initial_session_index(&sessions);

    // Assert
    assert_eq!(selected_index, Some(1));
}

#[test]
fn test_preferred_initial_session_index_falls_back_to_first_grouped_session() {
    // Arrange
    let sessions = vec![
        crate::test_support::titled_session_fixture("archive-1", Status::Done),
        crate::test_support::titled_session_fixture("merge-1", Status::Queued),
    ];

    // Act
    let selected_index = preferred_initial_session_index(&sessions);

    // Assert
    assert_eq!(selected_index, Some(1));
}

#[test]
fn test_next_selectable_index_advances_in_grouped_order() {
    // Arrange
    let sessions = vec![
        crate::test_support::titled_session_fixture("active-1", Status::Review),
        crate::test_support::titled_session_fixture("queued-1", Status::Queued),
        crate::test_support::titled_session_fixture("archive-1", Status::Done),
    ];

    // Act
    let selected_index = next_selectable_index(&sessions, 0, Some(1));

    // Assert
    assert_eq!(selected_index, Some(0));
}

#[test]
fn test_next_selectable_index_wraps_after_last_grouped_row() {
    // Arrange
    let sessions = vec![
        crate::test_support::titled_session_fixture("active-1", Status::Review),
        crate::test_support::titled_session_fixture("archive-1", Status::Done),
    ];

    // Act
    let selected_index = next_selectable_index(&sessions, 0, Some(1));

    // Assert
    assert_eq!(selected_index, Some(0));
}

#[test]
fn test_previous_selectable_index_moves_back_in_grouped_order() {
    // Arrange
    let sessions = vec![
        crate::test_support::titled_session_fixture("active-1", Status::Review),
        crate::test_support::titled_session_fixture("queued-1", Status::Queued),
        crate::test_support::titled_session_fixture("archive-1", Status::Done),
    ];

    // Act
    let selected_index = previous_selectable_index(&sessions, 0, Some(0));

    // Assert
    assert_eq!(selected_index, Some(1));
}

#[test]
fn test_previous_selectable_index_wraps_before_first_grouped_row() {
    // Arrange
    let sessions = vec![
        crate::test_support::titled_session_fixture("active-1", Status::Review),
        crate::test_support::titled_session_fixture("archive-1", Status::Done),
    ];

    // Act
    let selected_index = previous_selectable_index(&sessions, 0, Some(0));

    // Assert
    assert_eq!(selected_index, Some(1));
}

#[test]
fn test_selectable_session_indexes_orders_sessions_without_headers() {
    // Arrange
    let sessions = vec![
        crate::test_support::titled_session_fixture("active-1", Status::Review),
        crate::test_support::titled_session_fixture("queued-1", Status::Queued),
        crate::test_support::titled_session_fixture("merge-1", Status::Merging),
        crate::test_support::titled_session_fixture("done-1", Status::Done),
        crate::test_support::titled_session_fixture("canceled-1", Status::Canceled),
        crate::test_support::titled_session_fixture("active-2", Status::Draft),
        crate::test_support::titled_session_fixture("merged-1", Status::Merged),
    ];

    // Act
    let indexes = selectable_session_indexes(&sessions);
    let ordered_ids = indexes
        .into_iter()
        .map(|index| sessions[index].id.clone())
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        ordered_ids,
        vec![
            "queued-1".to_string(),
            "merge-1".to_string(),
            "active-1".to_string(),
            "active-2".to_string(),
            "merged-1".to_string(),
            "done-1".to_string(),
            "canceled-1".to_string(),
        ]
    );
}

#[test]
fn test_grouped_session_rows_orders_merge_queue_before_active_and_archive_sessions() {
    // Arrange
    let sessions = vec![
        crate::test_support::titled_session_fixture("active-1", Status::Review),
        crate::test_support::titled_session_fixture("queued-1", Status::Queued),
        crate::test_support::titled_session_fixture("merge-1", Status::Merging),
        crate::test_support::titled_session_fixture("done-1", Status::Done),
        crate::test_support::titled_session_fixture("canceled-1", Status::Canceled),
        crate::test_support::titled_session_fixture("active-2", Status::Draft),
        crate::test_support::titled_session_fixture("merged-1", Status::Merged),
    ];

    // Act
    let labels_and_ids = grouped_session_rows(&sessions)
        .into_iter()
        .map(|row| match row {
            GroupedSessionRow::GroupLabel(group) => format!("{group:?}"),
            GroupedSessionRow::Session { session, .. } => session.id.to_string(),
        })
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        labels_and_ids,
        vec![
            "MergeQueue".to_string(),
            "queued-1".to_string(),
            "merge-1".to_string(),
            "Active".to_string(),
            "active-1".to_string(),
            "active-2".to_string(),
            "merged-1".to_string(),
            "Archive".to_string(),
            "done-1".to_string(),
            "canceled-1".to_string(),
        ]
    );
}

#[test]
fn test_grouped_session_rows_omits_groups_without_sessions() {
    // Arrange
    let sessions = vec![
        crate::test_support::titled_session_fixture("active-1", Status::Review),
        crate::test_support::titled_session_fixture("active-2", Status::InProgress),
    ];

    // Act
    let labels_and_ids = grouped_session_rows(&sessions)
        .into_iter()
        .map(|row| match row {
            GroupedSessionRow::GroupLabel(group) => format!("{group:?}"),
            GroupedSessionRow::Session { session, .. } => session.id.to_string(),
        })
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        labels_and_ids,
        vec![
            "Active".to_string(),
            "active-1".to_string(),
            "active-2".to_string(),
        ]
    );
}

#[test]
fn test_grouped_session_rows_returns_no_rows_without_sessions() {
    // Arrange
    let sessions = Vec::new();

    // Act
    let rows = grouped_session_rows(&sessions);

    // Assert
    assert!(rows.is_empty());
}

#[test]
fn test_archive_actions_list_load_more_before_show_less() {
    for (has_more, is_expanded, expected) in [
        (false, false, Vec::new()),
        (true, false, vec![ArchiveAction::LoadMore]),
        (false, true, vec![ArchiveAction::ShowLess]),
        (
            true,
            true,
            vec![ArchiveAction::LoadMore, ArchiveAction::ShowLess],
        ),
    ] {
        // Act
        let actions = archive_actions(has_more, is_expanded);

        // Assert
        assert_eq!(actions, expected);
    }
}

#[test]
fn test_selectable_index_navigation_includes_archive_action_slots() {
    // Arrange
    let sessions = vec![
        crate::test_support::titled_session_fixture("archive-1", Status::Done),
        crate::test_support::titled_session_fixture("active-1", Status::Review),
    ];

    // Act
    let after_last_session = next_selectable_index(&sessions, 2, Some(0));
    let after_first_action = next_selectable_index(&sessions, 2, Some(2));
    let wrapped_forward = next_selectable_index(&sessions, 2, Some(3));
    let wrapped_backward = previous_selectable_index(&sessions, 2, Some(1));

    // Assert
    assert_eq!(after_last_session, Some(2));
    assert_eq!(after_first_action, Some(3));
    assert_eq!(wrapped_forward, Some(1));
    assert_eq!(wrapped_backward, Some(3));
}
