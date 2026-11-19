//! Pure grouped ordering for session-list selection and rendering.

use crate::domain::session::{Session, Status};

/// Number of archived sessions loaded per request.
pub(crate) const ARCHIVE_PAGE_SIZE: usize = 10;

/// Selectable archive pagination row rendered below the loaded sessions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveAction {
    /// Loads the next archive page.
    LoadMore,
    /// Collapses the archive back to its first page.
    ShowLess,
}

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

/// Returns the next raw selection index in grouped list order, followed by
/// `archive_action_count` archive action slots.
pub fn next_selectable_index(
    sessions: &[Session],
    archive_action_count: usize,
    selected_index: Option<usize>,
) -> Option<usize> {
    let indexes = selectable_list_indexes(sessions, archive_action_count);

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

/// Returns the previous raw selection index in grouped list order, followed
/// by `archive_action_count` archive action slots.
pub fn previous_selectable_index(
    sessions: &[Session],
    archive_action_count: usize,
    selected_index: Option<usize>,
) -> Option<usize> {
    let indexes = selectable_list_indexes(sessions, archive_action_count);

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

/// Returns available archive pagination actions in display order.
///
/// Action rows follow the loaded sessions, so the action at position `n` is
/// selected through raw index `sessions.len() + n`.
pub fn archive_actions(has_more: bool, is_expanded: bool) -> Vec<ArchiveAction> {
    [
        has_more.then_some(ArchiveAction::LoadMore),
        is_expanded.then_some(ArchiveAction::ShowLess),
    ]
    .into_iter()
    .flatten()
    .collect()
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
    append_group_rows(&mut rows, sessions, SessionGroup::MergeQueue);
    append_group_rows(&mut rows, sessions, SessionGroup::Active);
    append_group_rows(&mut rows, sessions, SessionGroup::Archive);

    rows
}

/// Returns selectable grouped session indexes followed by archive action
/// slots.
fn selectable_list_indexes(sessions: &[Session], archive_action_count: usize) -> Vec<usize> {
    let mut indexes = selectable_session_indexes(sessions);
    indexes.extend(sessions.len()..sessions.len() + archive_action_count);

    indexes
}

/// Adds one populated group and its sessions.
fn append_group_rows<'a>(
    rows: &mut Vec<GroupedSessionRow<'a>>,
    sessions: &'a [Session],
    group: SessionGroup,
) {
    let mut group_has_sessions = false;
    for (index, session) in sessions_for_group(sessions, group) {
        if !group_has_sessions {
            rows.push(GroupedSessionRow::GroupLabel(group));
            group_has_sessions = true;
        }

        rows.push(GroupedSessionRow::Session { index, session });
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
