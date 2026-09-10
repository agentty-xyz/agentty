//! Pure grouped ordering for session-list selection and rendering.

use std::collections::HashMap;

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
    /// Nested child row connected to its parent.
    Child {
        /// One-based depth below the root row.
        depth: usize,
        /// Whether this is the final child rendered under the parent.
        is_last: bool,
    },
}

/// One row in the grouped session list model.
pub enum GroupedSessionRow<'a> {
    /// Non-selectable row that labels the following group.
    GroupLabel(SessionGroup),
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
            GroupedSessionRow::GroupLabel(_) => None,
        })
        .collect()
}

/// Returns populated grouped display rows with merge queue, active, then
/// archive sessions.
pub fn grouped_session_rows(sessions: &[Session]) -> Vec<GroupedSessionRow<'_>> {
    let mut rows = Vec::with_capacity(sessions.len() + 3);
    let stacked_children = stacked_child_index(sessions);
    append_group_rows(
        &mut rows,
        sessions,
        &stacked_children,
        SessionGroup::MergeQueue,
    );
    append_group_rows(&mut rows, sessions, &stacked_children, SessionGroup::Active);
    append_group_rows(
        &mut rows,
        sessions,
        &stacked_children,
        SessionGroup::Archive,
    );

    rows
}

/// Lookup of stacked or orchestrated children by display parent session id.
type StackedChildIndex<'a> = HashMap<&'a str, Vec<(usize, &'a Session)>>;

/// Builds a per-render child lookup so grouped rows do not rescan every
/// session for every loaded parent row.
fn stacked_child_index(sessions: &[Session]) -> StackedChildIndex<'_> {
    let mut children_by_parent = HashMap::new();
    for (index, session) in sessions.iter().enumerate() {
        if let Some(parent_session_id) = session
            .parent_session_id
            .as_ref()
            .or(session.controller_session_id.as_ref())
        {
            children_by_parent
                .entry(parent_session_id.as_str())
                .or_insert_with(Vec::new)
                .push((index, session));
        }
    }

    children_by_parent
}

/// Adds one populated group and its sessions.
fn append_group_rows<'a>(
    rows: &mut Vec<GroupedSessionRow<'a>>,
    sessions: &'a [Session],
    stacked_children: &StackedChildIndex<'a>,
    group: SessionGroup,
) {
    let mut group_has_sessions = false;
    for (index, session) in sessions_for_group(sessions, group) {
        if has_loaded_parent_session_in_group(sessions, session, group) {
            continue;
        }

        if !group_has_sessions {
            rows.push(GroupedSessionRow::GroupLabel(group));
            group_has_sessions = true;
        }

        rows.push(GroupedSessionRow::Session {
            index,
            session,
            tree_position: SessionTreePosition::Root,
        });
        append_stacked_child_rows(rows, stacked_children, session.id.as_str(), group, 1);
    }
}

/// Adds stacked descendants that belong in the parent's current display
/// group.
fn append_stacked_child_rows<'a>(
    rows: &mut Vec<GroupedSessionRow<'a>>,
    stacked_children: &StackedChildIndex<'a>,
    parent_session_id: &str,
    group: SessionGroup,
    depth: usize,
) {
    let Some(children) = stacked_children.get(parent_session_id) else {
        return;
    };
    let children = children
        .iter()
        .copied()
        .filter(|(_, session)| session_group(session) == group)
        .collect::<Vec<_>>();

    let child_count = children.len();
    for (child_position, (index, session)) in children.into_iter().enumerate() {
        rows.push(GroupedSessionRow::Session {
            index,
            session,
            tree_position: SessionTreePosition::Child {
                depth,
                is_last: child_position + 1 == child_count,
            },
        });
        append_stacked_child_rows(
            rows,
            stacked_children,
            session.id.as_str(),
            group,
            depth + 1,
        );
    }
}

/// Returns whether a session should be nested under a loaded parent row in
/// the same display group.
fn has_loaded_parent_session_in_group(
    sessions: &[Session],
    session: &Session,
    group: SessionGroup,
) -> bool {
    match session
        .parent_session_id
        .as_ref()
        .or(session.controller_session_id.as_ref())
    {
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
#[path = "session_order_test.rs"]
mod tests;
