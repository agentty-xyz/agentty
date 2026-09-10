use super::{
    GroupedSessionRow, SessionTreePosition, grouped_session_rows, next_selectable_session_index,
    preferred_initial_session_index, previous_selectable_session_index, selectable_session_indexes,
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
fn test_next_selectable_session_index_advances_in_grouped_order() {
    // Arrange
    let sessions = vec![
        crate::test_support::titled_session_fixture("active-1", Status::Review),
        crate::test_support::titled_session_fixture("queued-1", Status::Queued),
        crate::test_support::titled_session_fixture("archive-1", Status::Done),
    ];

    // Act
    let selected_index = next_selectable_session_index(&sessions, Some(1));

    // Assert
    assert_eq!(selected_index, Some(0));
}

#[test]
fn test_next_selectable_session_index_wraps_after_last_grouped_row() {
    // Arrange
    let sessions = vec![
        crate::test_support::titled_session_fixture("active-1", Status::Review),
        crate::test_support::titled_session_fixture("archive-1", Status::Done),
    ];

    // Act
    let selected_index = next_selectable_session_index(&sessions, Some(1));

    // Assert
    assert_eq!(selected_index, Some(0));
}

#[test]
fn test_previous_selectable_session_index_moves_back_in_grouped_order() {
    // Arrange
    let sessions = vec![
        crate::test_support::titled_session_fixture("active-1", Status::Review),
        crate::test_support::titled_session_fixture("queued-1", Status::Queued),
        crate::test_support::titled_session_fixture("archive-1", Status::Done),
    ];

    // Act
    let selected_index = previous_selectable_session_index(&sessions, Some(0));

    // Assert
    assert_eq!(selected_index, Some(1));
}

#[test]
fn test_previous_selectable_session_index_wraps_before_first_grouped_row() {
    // Arrange
    let sessions = vec![
        crate::test_support::titled_session_fixture("active-1", Status::Review),
        crate::test_support::titled_session_fixture("archive-1", Status::Done),
    ];

    // Act
    let selected_index = previous_selectable_session_index(&sessions, Some(0));

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
fn test_selectable_session_indexes_places_stacked_child_after_parent() {
    // Arrange
    let mut child_session = crate::test_support::titled_session_fixture("child-1", Status::Draft);
    child_session.parent_session_id = Some("parent-1".into());
    let sessions = vec![
        child_session,
        crate::test_support::titled_session_fixture("parent-1", Status::Review),
        crate::test_support::titled_session_fixture("sibling-1", Status::Review),
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
            "parent-1".to_string(),
            "child-1".to_string(),
            "sibling-1".to_string(),
        ]
    );
}

#[test]
fn test_selectable_session_indexes_places_orchestration_child_after_controller() {
    // Arrange
    let mut child_session =
        crate::test_support::titled_session_fixture("child-1", Status::InProgress);
    child_session.controller_session_id = Some("controller-1".into());
    let sessions = vec![
        child_session,
        crate::test_support::titled_session_fixture("sibling-1", Status::Review),
        crate::test_support::titled_session_fixture("controller-1", Status::Review),
    ];

    // Act
    let ordered_ids = selectable_session_indexes(&sessions)
        .into_iter()
        .map(|index| sessions[index].id.clone())
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        ordered_ids,
        vec![
            "sibling-1".to_string(),
            "controller-1".to_string(),
            "child-1".to_string(),
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
fn test_grouped_session_rows_marks_stacked_child_with_tree_position() {
    // Arrange
    let mut first_child_session =
        crate::test_support::titled_session_fixture("child-1", Status::Draft);
    first_child_session.parent_session_id = Some("parent-1".into());
    let mut second_child_session =
        crate::test_support::titled_session_fixture("child-2", Status::Draft);
    second_child_session.parent_session_id = Some("parent-1".into());
    let sessions = vec![
        first_child_session,
        crate::test_support::titled_session_fixture("parent-1", Status::Review),
        second_child_session,
    ];

    // Act
    let session_rows = grouped_session_rows(&sessions)
        .into_iter()
        .filter_map(|row| match row {
            GroupedSessionRow::Session {
                session,
                tree_position,
                ..
            } => Some((session.id.to_string(), tree_position)),
            GroupedSessionRow::GroupLabel(_) => None,
        })
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        session_rows,
        vec![
            ("parent-1".to_string(), SessionTreePosition::Root),
            (
                "child-1".to_string(),
                SessionTreePosition::Child {
                    depth: 1,
                    is_last: false,
                },
            ),
            (
                "child-2".to_string(),
                SessionTreePosition::Child {
                    depth: 1,
                    is_last: true,
                },
            ),
        ]
    );
}

#[test]
fn test_grouped_session_rows_archives_canceled_child_below_active_parent() {
    // Arrange
    let mut child_session =
        crate::test_support::titled_session_fixture("child-1", Status::Canceled);
    child_session.parent_session_id = Some("parent-1".into());
    let sessions = vec![
        child_session,
        crate::test_support::titled_session_fixture("parent-1", Status::Review),
    ];

    // Act
    let rows = grouped_session_rows(&sessions);
    let labels_positions_and_ids = rows
        .into_iter()
        .map(|row| match row {
            GroupedSessionRow::GroupLabel(group) => format!("{group:?}"),
            GroupedSessionRow::Session {
                session,
                tree_position,
                ..
            } => format!("{}:{tree_position:?}", session.id),
        })
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        labels_positions_and_ids,
        vec![
            "Active".to_string(),
            "parent-1:Root".to_string(),
            "Archive".to_string(),
            "child-1:Root".to_string(),
        ]
    );
}

#[test]
fn test_grouped_session_rows_keeps_canceled_child_below_canceled_parent() {
    // Arrange
    let mut child_session =
        crate::test_support::titled_session_fixture("child-1", Status::Canceled);
    child_session.parent_session_id = Some("parent-1".into());
    let sessions = vec![
        child_session,
        crate::test_support::titled_session_fixture("parent-1", Status::Canceled),
    ];

    // Act
    let rows = grouped_session_rows(&sessions);
    let labels_positions_and_ids = rows
        .into_iter()
        .map(|row| match row {
            GroupedSessionRow::GroupLabel(group) => format!("{group:?}"),
            GroupedSessionRow::Session {
                session,
                tree_position,
                ..
            } => format!("{}:{tree_position:?}", session.id),
        })
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        labels_positions_and_ids,
        vec![
            "Archive".to_string(),
            "parent-1:Root".to_string(),
            "child-1:Child { depth: 1, is_last: true }".to_string(),
        ]
    );
}

#[test]
fn test_grouped_session_rows_nests_descendants_to_depth_five() {
    // Arrange
    let root_session = crate::test_support::titled_session_fixture("root", Status::Review);
    let mut level_1 = crate::test_support::titled_session_fixture("level-1", Status::Review);
    level_1.parent_session_id = Some("root".into());
    let mut level_2 = crate::test_support::titled_session_fixture("level-2", Status::Review);
    level_2.parent_session_id = Some("level-1".into());
    let mut level_3 = crate::test_support::titled_session_fixture("level-3", Status::Review);
    level_3.parent_session_id = Some("level-2".into());
    let mut level_4 = crate::test_support::titled_session_fixture("level-4", Status::Review);
    level_4.parent_session_id = Some("level-3".into());
    let mut level_5 = crate::test_support::titled_session_fixture("level-5", Status::Review);
    level_5.parent_session_id = Some("level-4".into());
    let sessions = vec![level_5, level_3, root_session, level_1, level_2, level_4];

    // Act
    let ordered_rows = grouped_session_rows(&sessions)
        .into_iter()
        .filter_map(|row| match row {
            GroupedSessionRow::Session {
                session,
                tree_position,
                ..
            } => Some((session.id.to_string(), tree_position)),
            GroupedSessionRow::GroupLabel(_) => None,
        })
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        ordered_rows,
        vec![
            ("root".to_string(), SessionTreePosition::Root),
            (
                "level-1".to_string(),
                SessionTreePosition::Child {
                    depth: 1,
                    is_last: true,
                },
            ),
            (
                "level-2".to_string(),
                SessionTreePosition::Child {
                    depth: 2,
                    is_last: true,
                },
            ),
            (
                "level-3".to_string(),
                SessionTreePosition::Child {
                    depth: 3,
                    is_last: true,
                },
            ),
            (
                "level-4".to_string(),
                SessionTreePosition::Child {
                    depth: 4,
                    is_last: true,
                },
            ),
            (
                "level-5".to_string(),
                SessionTreePosition::Child {
                    depth: 5,
                    is_last: true,
                },
            ),
        ]
    );
}
