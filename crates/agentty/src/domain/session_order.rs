//! Pure grouped ordering for session-list selection and rendering.

use crate::domain::session::{Session, Status};

/// Group bucket used to organize sessions in the list before rendering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionGroup {
    /// Sessions that are currently active, reviewable, or draftable.
    Active,
    /// Sessions that are done or canceled.
    Archive,
    /// Sessions waiting for merge or currently merging.
    MergeQueue,
}

/// Tree placement for one selectable session in grouped order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionTreePosition {
    /// Root-level session row with no tree marker.
    Root,
    /// One-level child row connected to its parent.
    Child { is_last: bool },
}

/// One row in the grouped session list model.
pub enum GroupedSessionRow<'a> {
    /// Non-selectable row that labels the following group.
    GroupLabel(SessionGroup),
    /// Non-selectable placeholder shown when a group has no sessions.
    EmptyGroupPlaceholder,
    /// Selectable session row in raw-session-index terms.
    Session {
        /// Raw index into the ungrouped session snapshot.
        index: usize,
        /// Session snapshot for the selectable row.
        session: &'a Session,
        /// Visual stack placement relative to any loaded parent session.
        tree_position: SessionTreePosition,
    },
}

/// Resolves the initial raw-session selection index for list-mode focus.
///
/// Active sessions are preferred so opening the session list lands on ongoing
/// work when both active and archived items are present. If no active sessions
/// exist, this falls back to the first selectable grouped row.
pub fn preferred_initial_session_index(sessions: &[Session]) -> Option<usize> {
    sessions_for_group(sessions, SessionGroup::Active)
        .map(|(index, _)| index)
        .next()
        .or_else(|| selectable_session_indexes(sessions).first().copied())
}

/// Returns the next raw-session selection index in grouped list order.
pub fn next_selectable_session_index(
    sessions: &[Session],
    selected_index: Option<usize>,
) -> Option<usize> {
    let indexes = selectable_session_indexes(sessions);

    if indexes.is_empty() {
        None
    } else {
        let position = selected_index
            .and_then(|selected_index| indexes.iter().position(|index| *index == selected_index));
        let next_position = match position {
            Some(position) if position >= indexes.len() - 1 => 0,
            Some(position) => position + 1,
            None => 0,
        };

        Some(indexes[next_position])
    }
}

/// Returns the previous raw-session selection index in grouped list order.
pub fn previous_selectable_session_index(
    sessions: &[Session],
    selected_index: Option<usize>,
) -> Option<usize> {
    let indexes = selectable_session_indexes(sessions);

    if indexes.is_empty() {
        None
    } else {
        let position = selected_index
            .and_then(|selected_index| indexes.iter().position(|index| *index == selected_index));
        let previous_position = match position {
            Some(0) => indexes.len() - 1,
            Some(position) => position - 1,
            None => 0,
        };

        Some(indexes[previous_position])
    }
}

/// Returns session indexes in the same order as selectable grouped rows.
pub fn selectable_session_indexes(sessions: &[Session]) -> Vec<usize> {
    grouped_session_rows(sessions)
        .into_iter()
        .filter_map(|row| match row {
            GroupedSessionRow::Session { index, .. } => Some(index),
            GroupedSessionRow::GroupLabel(_) | GroupedSessionRow::EmptyGroupPlaceholder => None,
        })
        .collect()
}

/// Returns grouped display rows with merge queue, active, then archive
/// sessions.
pub fn grouped_session_rows(sessions: &[Session]) -> Vec<GroupedSessionRow<'_>> {
    let mut rows = Vec::with_capacity(sessions.len() + 6);
    append_group_rows(&mut rows, sessions, SessionGroup::MergeQueue);
    append_group_rows(&mut rows, sessions, SessionGroup::Active);
    append_group_rows(&mut rows, sessions, SessionGroup::Archive);

    rows
}

/// Adds one group label row and either its sessions or an empty placeholder.
fn append_group_rows<'a>(
    rows: &mut Vec<GroupedSessionRow<'a>>,
    sessions: &'a [Session],
    group: SessionGroup,
) {
    rows.push(GroupedSessionRow::GroupLabel(group));

    let mut group_has_sessions = false;
    for (index, session) in sessions_for_group(sessions, group) {
        if has_loaded_parent_session_in_group(sessions, session, group) {
            continue;
        }

        rows.push(GroupedSessionRow::Session {
            index,
            session,
            tree_position: SessionTreePosition::Root,
        });
        append_stacked_child_rows(rows, sessions, session.id.as_str(), group);
        group_has_sessions = true;
    }

    if !group_has_sessions {
        rows.push(GroupedSessionRow::EmptyGroupPlaceholder);
    }
}

/// Adds the one-level stacked children that belong in the parent's current
/// display group.
fn append_stacked_child_rows<'a>(
    rows: &mut Vec<GroupedSessionRow<'a>>,
    sessions: &'a [Session],
    parent_session_id: &str,
    group: SessionGroup,
) {
    let children = sessions
        .iter()
        .enumerate()
        .filter(|(_, session)| {
            session_group(session) == group
                && session
                    .parent_session_id
                    .as_ref()
                    .is_some_and(|parent_id| parent_id.as_str() == parent_session_id)
        })
        .collect::<Vec<_>>();

    let child_count = children.len();
    for (child_position, (index, session)) in children.into_iter().enumerate() {
        rows.push(GroupedSessionRow::Session {
            index,
            session,
            tree_position: SessionTreePosition::Child {
                is_last: child_position + 1 == child_count,
            },
        });
    }
}

/// Returns whether a session should be nested under a loaded parent row in
/// the same display group.
fn has_loaded_parent_session_in_group(
    sessions: &[Session],
    session: &Session,
    group: SessionGroup,
) -> bool {
    match session.parent_session_id.as_ref() {
        Some(parent_session_id) => sessions.iter().any(|candidate| {
            candidate.id.as_str() == parent_session_id.as_str() && session_group(candidate) == group
        }),
        None => false,
    }
}

/// Returns session indexes and snapshots for one grouped section.
fn sessions_for_group(
    sessions: &[Session],
    group: SessionGroup,
) -> impl Iterator<Item = (usize, &Session)> {
    sessions
        .iter()
        .enumerate()
        .filter(move |(_, session)| session_group(session) == group)
}

/// Returns the grouped section where a session should be displayed.
fn session_group(session: &Session) -> SessionGroup {
    match session.status {
        Status::Queued | Status::Merging => SessionGroup::MergeQueue,
        Status::Done | Status::Canceled => SessionGroup::Archive,
        _ => SessionGroup::Active,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::domain::agent::AgentModel;
    use crate::domain::session::{PublishedBranchSyncStatus, SessionSize, SessionStats};

    /// Returns one session snapshot with a custom id and status.
    fn test_session(id: &str, status: Status) -> Session {
        Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: PathBuf::new(),
            follow_up_tasks: Vec::new(),
            id: id.into(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentModel::AntigravityGemini3FlashPreview,
            output: String::new(),
            parent_session_id: None,
            project_name: "project".to_string(),
            prompt: String::new(),
            queued_messages: Vec::new(),
            reasoning_level_override: None,
            published_upstream_ref: None,
            published_branch_sync_status: PublishedBranchSyncStatus::Idle,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status,
            summary: None,
            title: Some(id.to_string()),
            updated_at: 0,
            workflow_notice: None,
        }
    }

    #[test]
    fn test_preferred_initial_session_index_prefers_active_group_when_available() {
        // Arrange
        let sessions = vec![
            test_session("archive-1", Status::Done),
            test_session("active-1", Status::Review),
            test_session("merge-1", Status::Queued),
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
            test_session("archive-1", Status::Done),
            test_session("merge-1", Status::Queued),
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
            test_session("active-1", Status::Review),
            test_session("queued-1", Status::Queued),
            test_session("archive-1", Status::Done),
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
            test_session("active-1", Status::Review),
            test_session("archive-1", Status::Done),
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
            test_session("active-1", Status::Review),
            test_session("queued-1", Status::Queued),
            test_session("archive-1", Status::Done),
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
            test_session("active-1", Status::Review),
            test_session("archive-1", Status::Done),
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
            test_session("active-1", Status::Review),
            test_session("queued-1", Status::Queued),
            test_session("merge-1", Status::Merging),
            test_session("done-1", Status::Done),
            test_session("canceled-1", Status::Canceled),
            test_session("active-2", Status::Draft),
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
                "done-1".to_string(),
                "canceled-1".to_string(),
            ]
        );
    }

    #[test]
    fn test_selectable_session_indexes_places_stacked_child_after_parent() {
        // Arrange
        let mut child_session = test_session("child-1", Status::Draft);
        child_session.parent_session_id = Some("parent-1".into());
        let sessions = vec![
            child_session,
            test_session("parent-1", Status::Review),
            test_session("sibling-1", Status::Review),
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
    fn test_grouped_session_rows_orders_merge_queue_before_active_and_archive_sessions() {
        // Arrange
        let sessions = vec![
            test_session("active-1", Status::Review),
            test_session("queued-1", Status::Queued),
            test_session("merge-1", Status::Merging),
            test_session("done-1", Status::Done),
            test_session("canceled-1", Status::Canceled),
            test_session("active-2", Status::Draft),
        ];

        // Act
        let labels_and_ids = grouped_session_rows(&sessions)
            .into_iter()
            .map(|row| match row {
                GroupedSessionRow::GroupLabel(group) => format!("{group:?}"),
                GroupedSessionRow::EmptyGroupPlaceholder => "empty".to_string(),
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
                "Archive".to_string(),
                "done-1".to_string(),
                "canceled-1".to_string(),
            ]
        );
    }

    #[test]
    fn test_grouped_session_rows_includes_placeholder_for_groups_without_sessions() {
        // Arrange
        let sessions = vec![
            test_session("active-1", Status::Review),
            test_session("active-2", Status::InProgress),
        ];

        // Act
        let labels_and_ids = grouped_session_rows(&sessions)
            .into_iter()
            .map(|row| match row {
                GroupedSessionRow::GroupLabel(group) => format!("{group:?}"),
                GroupedSessionRow::EmptyGroupPlaceholder => "empty".to_string(),
                GroupedSessionRow::Session { session, .. } => session.id.to_string(),
            })
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(
            labels_and_ids,
            vec![
                "MergeQueue".to_string(),
                "empty".to_string(),
                "Active".to_string(),
                "active-1".to_string(),
                "active-2".to_string(),
                "Archive".to_string(),
                "empty".to_string(),
            ]
        );
    }

    #[test]
    fn test_grouped_session_rows_marks_stacked_child_with_tree_position() {
        // Arrange
        let mut first_child_session = test_session("child-1", Status::Draft);
        first_child_session.parent_session_id = Some("parent-1".into());
        let mut second_child_session = test_session("child-2", Status::Draft);
        second_child_session.parent_session_id = Some("parent-1".into());
        let sessions = vec![
            first_child_session,
            test_session("parent-1", Status::Review),
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
                GroupedSessionRow::GroupLabel(_) | GroupedSessionRow::EmptyGroupPlaceholder => None,
            })
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(
            session_rows,
            vec![
                ("parent-1".to_string(), SessionTreePosition::Root),
                (
                    "child-1".to_string(),
                    SessionTreePosition::Child { is_last: false },
                ),
                (
                    "child-2".to_string(),
                    SessionTreePosition::Child { is_last: true },
                ),
            ]
        );
    }

    #[test]
    fn test_grouped_session_rows_archives_canceled_child_below_active_parent() {
        // Arrange
        let mut child_session = test_session("child-1", Status::Canceled);
        child_session.parent_session_id = Some("parent-1".into());
        let sessions = vec![child_session, test_session("parent-1", Status::Review)];

        // Act
        let rows = grouped_session_rows(&sessions);
        let labels_positions_and_ids = rows
            .into_iter()
            .map(|row| match row {
                GroupedSessionRow::GroupLabel(group) => format!("{group:?}"),
                GroupedSessionRow::EmptyGroupPlaceholder => "empty".to_string(),
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
                "MergeQueue".to_string(),
                "empty".to_string(),
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
        let mut child_session = test_session("child-1", Status::Canceled);
        child_session.parent_session_id = Some("parent-1".into());
        let sessions = vec![child_session, test_session("parent-1", Status::Canceled)];

        // Act
        let rows = grouped_session_rows(&sessions);
        let labels_positions_and_ids = rows
            .into_iter()
            .map(|row| match row {
                GroupedSessionRow::GroupLabel(group) => format!("{group:?}"),
                GroupedSessionRow::EmptyGroupPlaceholder => "empty".to_string(),
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
                "MergeQueue".to_string(),
                "empty".to_string(),
                "Active".to_string(),
                "empty".to_string(),
                "Archive".to_string(),
                "parent-1:Root".to_string(),
                "child-1:Child { is_last: true }".to_string(),
            ]
        );
    }
}
