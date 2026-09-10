use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use crate::app::session::{Clock, SESSION_REFRESH_INTERVAL};
use crate::domain::selection::SelectionState;
use crate::domain::session::{Session, SessionDiffStats, SessionHandles, SessionId, Status};
use crate::domain::transient_message::{TransientMessage, TransientMessageSlot};

/// Cached ahead/behind snapshots for one session branch.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SessionGitStatus {
    /// Ahead/behind counts comparing the session branch to its base branch.
    pub base_status: Option<(u32, u32)>,
    /// Whether merging the session branch into its base branch would conflict.
    pub has_merge_conflict: Option<bool>,
    /// Ahead/behind counts comparing the session branch to its tracked remote.
    pub remote_status: Option<(u32, u32)>,
}

/// Application-owned live state for active session workers.
///
/// Keeping synchronization handles behind this boundary distinguishes
/// worker-owned mutable state from the persisted/render-friendly snapshots in
/// [`SessionState::sessions`].
struct SessionRuntimeState {
    handles: HashMap<SessionId, SessionHandles>,
}

impl SessionRuntimeState {
    fn handle(&self, session_id: &str) -> Option<&SessionHandles> {
        self.handles.get(session_id)
    }

    fn handles(&self) -> &HashMap<SessionId, SessionHandles> {
        &self.handles
    }

    fn handles_mut(&mut self) -> &mut HashMap<SessionId, SessionHandles> {
        &mut self.handles
    }

    fn remove_handle(&mut self, session_id: &str) {
        self.handles.remove(session_id);
    }
}

/// Holds all in-memory state related to session listing and refresh tracking.
pub struct SessionState {
    pub(super) clock: Arc<dyn Clock>,
    /// Selected follow-up-task positions keyed by session id for session-view
    /// affordances.
    pub(super) follow_up_task_positions: HashMap<SessionId, usize>,
    pub(super) refresh_deadline: Instant,
    pub(super) row_count: i64,
    /// Cached detected branch names keyed by session id.
    pub(super) session_branch_names: HashMap<SessionId, String>,
    pub(super) session_git_statuses: HashMap<SessionId, SessionGitStatus>,
    /// Cached session list positions keyed by stable session id.
    pub(super) session_index_by_id: HashMap<SessionId, usize>,
    /// Cached worktree-directory availability keyed by session id.
    pub(super) session_worktree_availability: HashMap<SessionId, bool>,
    pub(super) sessions: Vec<Session>,
    pub(super) table_state: SelectionState,
    pub(super) updated_at_max: i64,
    runtime: SessionRuntimeState,
}

impl SessionState {
    /// Creates a new [`SessionState`] with initial refresh metadata.
    ///
    /// Time values are provided by an injected clock so refresh scheduling can
    /// be deterministic in tests.
    pub(crate) fn new(
        handles: HashMap<SessionId, SessionHandles>,
        sessions: Vec<Session>,
        table_state: SelectionState,
        clock: Arc<dyn Clock>,
        row_count: i64,
        updated_at_max: i64,
    ) -> Self {
        let _state_created_at = clock.now_system_time();
        let refresh_deadline = clock.now_instant() + SESSION_REFRESH_INTERVAL;
        let mut state = Self {
            clock,
            follow_up_task_positions: HashMap::new(),
            refresh_deadline,
            row_count,
            runtime: SessionRuntimeState { handles },
            session_branch_names: HashMap::new(),
            session_git_statuses: HashMap::new(),
            session_index_by_id: HashMap::new(),
            session_worktree_availability: HashMap::new(),
            sessions: Vec::new(),
            table_state,
            updated_at_max,
        };

        for session in sessions {
            state.push_session(session);
        }

        state
    }

    /// Returns the current list position for one stable session identifier.
    pub fn session_index_for_id(&self, session_id: &str) -> Option<usize> {
        self.session_index_by_id.get(session_id).copied()
    }

    /// Returns all loaded session snapshots in current list order.
    pub(crate) fn sessions(&self) -> &[Session] {
        &self.sessions
    }

    /// Returns mutable access to loaded session snapshots for reducers that
    /// update render-friendly projections.
    pub(crate) fn sessions_mut(&mut self) -> &mut [Session] {
        &mut self.sessions
    }

    /// Returns runtime handles keyed by stable session id.
    pub(in crate::app::session) fn handles(&self) -> &HashMap<SessionId, SessionHandles> {
        self.runtime.handles()
    }

    /// Returns mutable runtime handles for workflow loading and test setup.
    pub(in crate::app::session) fn handles_mut(
        &mut self,
    ) -> &mut HashMap<SessionId, SessionHandles> {
        self.runtime.handles_mut()
    }

    /// Returns one active session's live runtime handles.
    pub(in crate::app::session) fn handle(&self, session_id: &str) -> Option<&SessionHandles> {
        self.runtime.handle(session_id)
    }

    /// Removes live runtime state for one session that left the snapshot.
    pub(in crate::app::session) fn remove_handle(&mut self, session_id: &str) {
        self.runtime.remove_handle(session_id);
    }

    /// Applies one optimistic status transition to both the render snapshot
    /// and live runtime handle when each still matches `current_status`.
    pub(crate) fn transition_status_if_current(
        &mut self,
        session_id: &str,
        current_status: Status,
        next_status: Status,
    ) {
        if let Some(session) = self.session_mut_for_id(session_id)
            && session.status == current_status
        {
            session.status = next_status;
        }

        if let Some(handles) = self.runtime.handle(session_id)
            && let Ok(mut handle_status) = handles.status.lock()
            && *handle_status == current_status
        {
            *handle_status = next_status;
        }
    }

    /// Returns the current wall-clock value from the injected clock.
    pub(crate) fn now_system_time(&self) -> SystemTime {
        self.clock.now_system_time()
    }

    /// Returns one immutable session snapshot by identifier.
    pub fn session_for_id(&self, session_id: &str) -> Option<&Session> {
        self.session_index_for_id(session_id)
            .and_then(|session_index| self.sessions.get(session_index))
    }

    /// Replaces persisted session snapshots while retaining in-process
    /// transient output for sessions that remain loaded.
    ///
    /// Database refreshes can observe intermediate workflow persistence, such
    /// as a published upstream branch before its review-request URL is ready.
    /// Carrying transient output across that refresh keeps active loaders
    /// visible until their owning reducer resolves them.
    pub(crate) fn replace_sessions(&mut self, mut sessions: Vec<Session>) {
        let transient_state_by_session_id: HashMap<SessionId, _> = self
            .sessions
            .iter()
            .map(|session| {
                (
                    session.id.clone(),
                    (session.status, session.transient_messages.clone()),
                )
            })
            .collect();
        for session in &mut sessions {
            if let Some((previous_status, transient_messages)) =
                transient_state_by_session_id.get(&session.id)
            {
                let preparation = session
                    .transient_messages
                    .get(TransientMessageSlot::WorkspacePreparation)
                    .cloned();
                session.transient_messages.clone_from(transient_messages);
                session
                    .transient_messages
                    .retract(TransientMessageSlot::WorkspacePreparation);
                if let Some(preparation) = preparation {
                    session.transient_messages.upsert(preparation);
                }
                session.reconcile_status_transition(*previous_status);
            }
        }

        self.session_index_by_id = Self::build_session_index_by_id(&sessions);
        self.sessions = sessions;
    }

    /// Appends one session snapshot and records its new stable identifier
    /// lookup entry.
    pub(crate) fn push_session(&mut self, session: Session) {
        let session_index = self.sessions.len();
        self.session_index_by_id
            .insert(session.id.clone(), session_index);
        self.sessions.push(session);
    }

    /// Removes one session snapshot by list index and rebuilds the cached
    /// identifier index when removal succeeds.
    pub(crate) fn remove_session_at(&mut self, session_index: usize) -> Option<Session> {
        if session_index >= self.sessions.len() {
            return None;
        }

        let session = self.sessions.remove(session_index);
        self.rebuild_session_index_by_id();

        Some(session)
    }

    /// Copies current values from one runtime handle into its `Session`
    /// snapshot.
    pub fn sync_session_from_handle(&mut self, session_id: &str) {
        let Some(session_index) = self.session_index_for_id(session_id) else {
            return;
        };
        let Some(session_handles) = self.runtime.handle(session_id) else {
            return;
        };
        let Some(session) = self.sessions.get_mut(session_index) else {
            return;
        };

        let previous_status = session.status;
        Self::sync_session_with_handles(session, session_handles);
        session.reconcile_status_transition(previous_status);
    }

    /// Mirrors one queued workflow row into live handles and the active render
    /// snapshot when that project is currently loaded.
    pub(crate) fn upsert_queued_action(&mut self, session_id: &str, message: TransientMessage) {
        if let Some(session_handles) = self.runtime.handle(session_id) {
            session_handles.upsert_queued_action(message.clone());
        }

        if let Some(session) = self.session_mut_for_id(session_id) {
            session.transient_messages.upsert(message);
        }
    }

    /// Removes one queued workflow row from live handles and the active
    /// render snapshot when that project is currently loaded.
    pub(crate) fn resolve_queued_action(&mut self, session_id: &str, slot: TransientMessageSlot) {
        if let Some(session_handles) = self.runtime.handle(session_id) {
            session_handles.resolve_queued_action(slot);
        }

        if let Some(session) = self.session_mut_for_id(session_id) {
            session.transient_messages.retract(slot);
        }
    }

    /// Copies current values from runtime handles into plain `Session` fields.
    ///
    /// The runtime uses targeted `sync_session_from_handle()` calls for
    /// queued `SessionUpdated` events. This full sweep remains for explicit
    /// catch-up paths and focused tests.
    pub fn sync_from_handles(&mut self) {
        let handles = self.runtime.handles();

        for session in &mut self.sessions {
            let Some(session_handles) = handles.get(&session.id) else {
                continue;
            };

            let previous_status = session.status;
            Self::sync_session_with_handles(session, session_handles);
            session.reconcile_status_transition(previous_status);
        }
    }

    /// Applies one recomputed diff metadata result to the matching snapshot.
    pub fn apply_session_diff_stats_updated(
        &mut self,
        session_id: &str,
        diff_stats: SessionDiffStats,
    ) {
        let Some(session_index) = self.session_index_for_id(session_id) else {
            return;
        };
        let Some(session) = self.sessions.get_mut(session_index) else {
            return;
        };

        session.stats.diff_state = diff_stats.diff_state();
        if let SessionDiffStats::Known {
            added_lines,
            deleted_lines,
            session_size,
            ..
        } = diff_stats
        {
            session.stats.added_lines = added_lines;
            session.stats.deleted_lines = deleted_lines;
            session.size = session_size;
        }
    }

    /// Returns the currently selected follow-up task position for one session.
    pub fn selected_follow_up_task_position(&self, session_id: &str) -> Option<usize> {
        let session = self.session_for_id(session_id)?;
        if session.follow_up_tasks.is_empty() {
            return None;
        }

        let selected_position = self
            .follow_up_task_positions
            .get(session_id)
            .copied()
            .unwrap_or(0);

        Some(selected_position.min(session.follow_up_tasks.len().saturating_sub(1)))
    }

    /// Advances the selected follow-up task for one session to the next item,
    /// wrapping at the end of the task list.
    pub fn select_next_follow_up_task(&mut self, session_id: &str) {
        self.advance_follow_up_task_selection(session_id, true);
    }

    /// Moves the selected follow-up task for one session to the previous item,
    /// wrapping at the beginning of the task list.
    pub fn select_previous_follow_up_task(&mut self, session_id: &str) {
        self.advance_follow_up_task_selection(session_id, false);
    }

    /// Sets the launched sibling-session identifier for the matching cached
    /// follow-up task.
    pub fn set_follow_up_task_launched_session_id(
        &mut self,
        session_id: &str,
        position: usize,
        launched_session_id: Option<SessionId>,
    ) {
        let Some(session) = self.session_mut_for_id(session_id) else {
            return;
        };

        let Some(follow_up_task) = session
            .follow_up_tasks
            .iter_mut()
            .find(|task| task.position == position)
        else {
            return;
        };

        follow_up_task.launched_session_id = launched_session_id;
    }

    /// Replaces all cached session git-status snapshots with one fresh poll
    /// result.
    pub(crate) fn replace_session_git_statuses(
        &mut self,
        session_git_statuses: HashMap<SessionId, SessionGitStatus>,
    ) {
        self.session_git_statuses = session_git_statuses;
    }

    /// Replaces cached worktree-availability snapshots with one fresh reload
    /// result.
    pub(crate) fn replace_session_worktree_availability(
        &mut self,
        session_worktree_availability: HashMap<SessionId, bool>,
    ) {
        self.session_worktree_availability = session_worktree_availability;
    }

    /// Replaces cached detected session branch names with one fresh reload
    /// result.
    pub(crate) fn replace_session_branch_names(
        &mut self,
        session_branch_names: HashMap<SessionId, String>,
    ) {
        self.session_branch_names = session_branch_names;
    }

    /// Updates cached worktree availability for one session after a lifecycle
    /// transition materializes or removes its worktree.
    pub(crate) fn set_session_worktree_available(&mut self, session_id: &str, is_available: bool) {
        self.session_worktree_availability
            .insert(SessionId::from(session_id), is_available);
    }

    /// Drops cached worktree availability for one removed session.
    pub(crate) fn remove_session_worktree_availability(&mut self, session_id: &str) {
        self.session_worktree_availability.remove(session_id);
    }

    /// Drops cached branch-name entries for sessions that are no longer active
    /// in memory.
    pub(crate) fn retain_session_branch_names(&mut self, active_session_ids: &HashSet<SessionId>) {
        self.session_branch_names
            .retain(|session_id, _| active_session_ids.contains(session_id));
    }

    /// Drops cached git-status entries for sessions that are no longer active
    /// in memory.
    pub(crate) fn retain_session_git_statuses(&mut self, active_session_ids: &HashSet<SessionId>) {
        self.session_git_statuses
            .retain(|session_id, _| active_session_ids.contains(session_id));
    }

    /// Drops or clamps cached follow-up-task selection for sessions that no
    /// longer exist after a reload.
    pub(crate) fn retain_follow_up_task_positions(
        &mut self,
        active_session_ids: &HashSet<SessionId>,
    ) {
        let follow_up_task_counts = self
            .sessions
            .iter()
            .map(|session| (session.id.as_str(), session.follow_up_tasks.len()))
            .collect::<HashMap<_, _>>();

        self.follow_up_task_positions
            .retain(|session_id, position| {
                if !active_session_ids.contains(session_id) {
                    return false;
                }

                let Some(follow_up_task_count) =
                    follow_up_task_counts.get(session_id.as_str()).copied()
                else {
                    return false;
                };
                if follow_up_task_count == 0 {
                    return false;
                }

                *position = (*position).min(follow_up_task_count.saturating_sub(1));

                true
            });
    }

    /// Synchronizes one session snapshot from shared runtime handles.
    ///
    /// Re-projects per-session render state from the runtime handles into the
    /// snapshot.
    ///
    /// The handles are the single source of truth for `status`, the typed
    /// transcript, and the in-memory chat queue. `queued_messages` is rebuilt
    /// from `SessionHandles::queued_message_snapshot()` so any
    /// handle-driven mutation (lifecycle enqueue, worker drain between
    /// turns, runtime LIFO pop) becomes visible on the very next sync
    /// without callers also having to mirror the change into the snapshot
    /// field.
    fn sync_session_with_handles(session: &mut Session, session_handles: &SessionHandles) {
        if let Ok(status) = session_handles.status.lock() {
            session.status = *status;
        }

        if let Ok(transcript) = session_handles.transcript.lock() {
            session.transcript = (!transcript.is_empty()).then(|| transcript.clone());
        }

        session.queued_messages = session_handles.queued_message_snapshot();
        for queued_action in session_handles.queued_action_snapshot() {
            session.transient_messages.upsert(queued_action);
        }
    }

    /// Advances the selected follow-up task for one session in the requested
    /// direction when at least one task exists.
    fn advance_follow_up_task_selection(&mut self, session_id: &str, move_forward: bool) {
        let Some(session) = self.session_for_id(session_id) else {
            return;
        };
        let follow_up_task_count = session.follow_up_tasks.len();
        if follow_up_task_count <= 1 {
            if follow_up_task_count == 1 {
                self.follow_up_task_positions
                    .insert(SessionId::from(session_id), 0);
            }

            return;
        }

        let next_position = match self.selected_follow_up_task_position(session_id) {
            Some(current_position) if move_forward => (current_position + 1) % follow_up_task_count,
            Some(0) => follow_up_task_count.saturating_sub(1),
            Some(current_position) => current_position.saturating_sub(1),
            None => 0,
        };
        self.follow_up_task_positions
            .insert(SessionId::from(session_id), next_position);
    }

    /// Returns one mutable session snapshot by identifier.
    pub(crate) fn session_mut_for_id(&mut self, session_id: &str) -> Option<&mut Session> {
        let session_index = self.session_index_by_id.get(session_id).copied()?;

        self.sessions.get_mut(session_index)
    }

    /// Rebuilds the cached session-id lookup map from the current session
    /// ordering.
    fn rebuild_session_index_by_id(&mut self) {
        self.session_index_by_id = Self::build_session_index_by_id(&self.sessions);
    }

    /// Builds a session-id lookup map from one ordered session slice.
    fn build_session_index_by_id(sessions: &[Session]) -> HashMap<SessionId, usize> {
        sessions
            .iter()
            .enumerate()
            .map(|(session_index, session)| (session.id.clone(), session_index))
            .collect()
    }
}

#[cfg(test)]
#[path = "state_test.rs"]
mod tests;
