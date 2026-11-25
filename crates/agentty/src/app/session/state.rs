//! `SessionState`, the terminal's session state with its live worker handles,
//! selection, and refresh tracking, and `SessionPage`, one project's loaded
//! session list with its cached branch and git metadata.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::app::session::{Clock, SESSION_REFRESH_INTERVAL};
use crate::domain::selection::SelectionState;
use crate::domain::session::{Session, SessionDiffStats, SessionHandles, SessionId, Status};
use crate::domain::session_message::SessionTranscript;
use crate::domain::session_order::{self, ArchiveAction};
use crate::domain::transient_message::{TransientMessage, TransientMessageSlot};

/// How long a viewed page stays loaded after the last request that named its
/// project or one of its sessions.
pub(crate) const VIEWED_PAGE_IDLE: Duration = Duration::from_mins(10);

/// Most viewed pages held at once; the least recently requested goes first.
pub(crate) const MAX_VIEWED_PAGES: usize = 4;

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
/// [`SessionPage::sessions`].
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

/// Holds the session state: the active project's page, the pages of other
/// listed projects that session-API callers view, live worker handles for
/// every session, the terminal's list selection, and refresh tracking.
///
/// Lookups by session id, and the reducers built on them, search every loaded
/// page. List positions, selection, and the terminal's archive pagination
/// belong to the active page, the one the terminal shows. Each page keeps
/// its own branch, worktree, and git-status caches, which by-id accessors
/// read on the page that holds the session.
///
/// A viewed page stays loaded while its lease, renewed by each request that
/// names its project or one of its sessions, is younger than
/// [`VIEWED_PAGE_IDLE`], and while at most [`MAX_VIEWED_PAGES`] are held.
pub struct SessionState {
    pub(super) clock: Arc<dyn Clock>,
    /// Selected follow-up-task positions keyed by session id for session-view
    /// affordances.
    pub(super) follow_up_task_positions: HashMap<SessionId, usize>,
    pub(super) refresh_deadline: Instant,
    pub(super) table_state: SelectionState,
    /// Session list of the project the terminal shows.
    active: SessionPage,
    runtime: SessionRuntimeState,
    /// Session lists of other listed projects that session-API callers
    /// view, keyed by project id.
    viewed: BTreeMap<i64, SessionPage>,
}

impl SessionState {
    /// Creates a new [`SessionState`] whose active page holds `sessions` of
    /// `project_id`, with initial refresh metadata.
    ///
    /// Time values are provided by an injected clock so refresh scheduling can
    /// be deterministic in tests.
    pub(crate) fn new(
        handles: HashMap<SessionId, SessionHandles>,
        project_id: i64,
        sessions: Vec<Session>,
        table_state: SelectionState,
        clock: Arc<dyn Clock>,
        row_count: i64,
        updated_at_max: i64,
    ) -> Self {
        let _state_created_at = clock.now_system_time();
        let refresh_deadline = clock.now_instant() + SESSION_REFRESH_INTERVAL;

        Self {
            active: SessionPage::new(project_id, sessions, row_count, updated_at_max),
            clock,
            follow_up_task_positions: HashMap::new(),
            refresh_deadline,
            runtime: SessionRuntimeState { handles },
            table_state,
            viewed: BTreeMap::new(),
        }
    }

    /// Returns the session list of the project the terminal shows.
    pub(crate) fn active_page(&self) -> &SessionPage {
        &self.active
    }

    /// Returns mutable access to the session list of the project the
    /// terminal shows.
    pub(crate) fn active_page_mut(&mut self) -> &mut SessionPage {
        &mut self.active
    }

    /// Returns the loaded page of `project_id`: the active page when it
    /// holds that project, otherwise its viewed page, if loaded.
    pub(crate) fn page_for_project(&self, project_id: i64) -> Option<&SessionPage> {
        if self.active.project_id == project_id {
            return Some(&self.active);
        }

        self.viewed.get(&project_id)
    }

    /// Returns the viewed page of `project_id`, never the active page.
    pub(crate) fn viewed_page(&self, project_id: i64) -> Option<&SessionPage> {
        self.viewed.get(&project_id)
    }

    /// Returns the projects of every loaded page: the active page's first,
    /// then the viewed pages' in ascending order.
    pub(crate) fn loaded_project_ids(&self) -> Vec<i64> {
        self.pages().map(SessionPage::project_id).collect()
    }

    /// Returns the projects whose viewed pages are loaded, in ascending
    /// order.
    pub(crate) fn viewed_project_ids(&self) -> Vec<i64> {
        self.viewed.keys().copied().collect()
    }

    /// Returns the project of the loaded page that holds `session_id`.
    pub(crate) fn session_project_id(&self, session_id: &str) -> Option<i64> {
        self.page_of_session(session_id)
            .map(SessionPage::project_id)
    }

    /// Renews the lease of `project_id`'s loaded page at `now`.
    pub(crate) fn renew_project_page(&mut self, project_id: i64, now: Instant) {
        let page = if self.active.project_id == project_id {
            Some(&mut self.active)
        } else {
            self.viewed.get_mut(&project_id)
        };
        if let Some(page) = page {
            page.last_requested_at = Some(now);
        }
    }

    /// Renews, at `now`, the lease of the loaded page that holds
    /// `session_id`.
    pub(crate) fn renew_session_page(&mut self, session_id: &str, now: Instant) {
        if let Some(project_id) = self.session_project_id(session_id) {
            self.renew_project_page(project_id, now);
        }
    }

    /// Returns the viewed page of `project_id`, adding an empty one with no
    /// lease when none is loaded, for a loader to fill.
    pub(in crate::app::session) fn viewed_page_mut_or_insert(
        &mut self,
        project_id: i64,
    ) -> &mut SessionPage {
        self.viewed
            .entry(project_id)
            .or_insert_with(|| SessionPage::new(project_id, Vec::new(), 0, 0))
    }

    /// Moves the render transcripts out of `project_id`'s viewed page, as
    /// [`Self::take_session_transcripts`] does for the active page.
    pub(in crate::app::session) fn take_viewed_page_transcripts(
        &mut self,
        project_id: i64,
    ) -> HashMap<SessionId, SessionTranscript> {
        self.viewed
            .get_mut(&project_id)
            .map_or_default(SessionPage::take_session_transcripts)
    }

    /// Returns transcripts taken by [`Self::take_viewed_page_transcripts`]
    /// to the viewed page of `project_id` after a failed reload.
    pub(in crate::app::session) fn restore_viewed_page_transcripts(
        &mut self,
        project_id: i64,
        transcripts: HashMap<SessionId, SessionTranscript>,
    ) {
        if let Some(page) = self.viewed.get_mut(&project_id) {
            page.restore_session_transcripts(transcripts);
        }
    }

    /// Drops every viewed page whose project `is_listed` rejects.
    pub(crate) fn retain_viewed_pages(&mut self, is_listed: impl Fn(i64) -> bool) {
        self.viewed.retain(|project_id, _| is_listed(*project_id));
    }

    /// Drops every viewed page whose lease is at least [`VIEWED_PAGE_IDLE`]
    /// old at `now`.
    pub(crate) fn evict_idle_viewed_pages(&mut self, now: Instant) {
        self.viewed.retain(|_, page| page.is_leased_at(now));
    }

    /// Drops the least recently requested viewed pages until at most
    /// [`MAX_VIEWED_PAGES`] remain.
    pub(crate) fn evict_excess_viewed_pages(&mut self) {
        let excess_count = self.viewed.len().saturating_sub(MAX_VIEWED_PAGES);
        let mut pages_by_lease = self
            .viewed
            .iter()
            .map(|(project_id, page)| (page.last_requested_at, *project_id))
            .collect::<Vec<_>>();
        pages_by_lease.sort_unstable();

        for (_, project_id) in pages_by_lease.into_iter().take(excess_count) {
            self.viewed.remove(&project_id);
        }
    }

    /// Drops every viewed page.
    pub(crate) fn clear_viewed_pages(&mut self) {
        self.viewed.clear();
    }

    /// Makes `project_id`'s page the active one when the terminal switches
    /// to that project, and returns whether the previous active page stayed
    /// loaded as a viewed page.
    ///
    /// A viewed page of `project_id` is promoted with its sessions and
    /// lease; otherwise the active page starts empty for the reload that
    /// follows. The previous active page is demoted while its lease is live
    /// at `now` and its archive window is on its first page, since a viewed
    /// page keeps that window, and it drops its git statuses, which only the
    /// active project's poll keeps current. Otherwise it is dropped.
    /// Switching to the active project changes nothing.
    pub(crate) fn switch_active_page(&mut self, project_id: i64, now: Instant) -> bool {
        if self.active.project_id == project_id {
            return false;
        }

        let next_page = self
            .viewed
            .remove(&project_id)
            .unwrap_or_else(|| SessionPage::new(project_id, Vec::new(), 0, 0));
        let mut previous_page = std::mem::replace(&mut self.active, next_page);
        let is_demoted = previous_page.is_leased_at(now) && !previous_page.is_archive_expanded();
        if is_demoted {
            previous_page.session_git_statuses.clear();
            self.viewed.insert(previous_page.project_id, previous_page);
            self.evict_excess_viewed_pages();
        }

        is_demoted
    }

    /// Returns the active page's list position for one stable session
    /// identifier.
    pub fn session_index_for_id(&self, session_id: &str) -> Option<usize> {
        self.active.session_index_for_id(session_id)
    }

    /// Returns whether `session_id` is on the active page, the one the
    /// terminal shows.
    ///
    /// Automation that applies the active project's settings, or that is
    /// scoped to the terminal's project, checks this rather than whether
    /// the session is loaded at all.
    pub(crate) fn is_in_active_page(&self, session_id: &str) -> bool {
        self.active.session_index_for_id(session_id).is_some()
    }

    /// Returns one session snapshot by identifier only when it is on the
    /// active page, for the gates that [`Self::is_in_active_page`] describes
    /// and that also read the session.
    pub(crate) fn active_page_session(&self, session_id: &str) -> Option<&Session> {
        self.active.session_for_id(session_id)
    }

    /// Returns the active page's session snapshots in current list order.
    pub(crate) fn sessions(&self) -> &[Session] {
        self.active.sessions()
    }

    /// Returns mutable access to the active page's session snapshots for
    /// reducers that update render-friendly projections.
    pub(crate) fn sessions_mut(&mut self) -> &mut [Session] {
        &mut self.active.sessions
    }

    /// Returns the session snapshots of every loaded page, the active page's
    /// first, for sweeps that do not depend on the terminal's project.
    pub(crate) fn all_sessions(&self) -> impl Iterator<Item = &Session> {
        self.pages().flat_map(|page| page.sessions.iter())
    }

    /// Returns mutable access to the session snapshots of every loaded page,
    /// the active page's first, for sweeps that do not depend on the
    /// terminal's project.
    pub(crate) fn all_sessions_mut(&mut self) -> impl Iterator<Item = &mut Session> {
        let (pages, _) = self.pages_and_runtime_mut();

        pages.flat_map(|page| page.sessions.iter_mut())
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

    /// Returns one immutable session snapshot by identifier from any loaded
    /// page.
    pub fn session_for_id(&self, session_id: &str) -> Option<&Session> {
        self.pages()
            .find_map(|page| page.session_for_id(session_id))
    }

    /// Replaces persisted session snapshots while retaining in-process
    /// transient output for sessions that remain loaded.
    ///
    /// See [`SessionPage::replace_sessions`].
    pub(crate) fn replace_sessions(&mut self, sessions: Vec<Session>) {
        self.active.replace_sessions(sessions);
    }

    /// Moves render transcripts out of the loaded snapshots, keyed by session
    /// id, so a reload can reuse unchanged ones instead of cloning them again
    /// from live handles.
    ///
    /// Pair with [`Self::replace_sessions`] on success or
    /// [`Self::restore_session_transcripts`] when the reload fails.
    pub(in crate::app::session) fn take_session_transcripts(
        &mut self,
    ) -> HashMap<SessionId, SessionTranscript> {
        self.active.take_session_transcripts()
    }

    /// Returns transcripts taken by [`Self::take_session_transcripts`] to the
    /// snapshots that are still loaded after a failed reload.
    pub(in crate::app::session) fn restore_session_transcripts(
        &mut self,
        transcripts: HashMap<SessionId, SessionTranscript>,
    ) {
        self.active.restore_session_transcripts(transcripts);
    }

    /// Returns whether the archive window extends beyond its first page.
    pub(crate) fn is_archive_expanded(&self) -> bool {
        self.active.is_archive_expanded()
    }

    /// Returns available archive pagination actions in display order.
    pub(crate) fn archive_actions(&self) -> Vec<ArchiveAction> {
        self.active.archive_actions()
    }

    /// Returns the archive pagination action under the current selection.
    pub(crate) fn selected_archive_action(&self) -> Option<ArchiveAction> {
        let action_position = self
            .table_state
            .selected()?
            .checked_sub(self.active.sessions.len())?;

        self.archive_actions().get(action_position).copied()
    }

    /// Selects one archive pagination action when it is available.
    ///
    /// Returns `false` without changing selection when the action is hidden.
    pub(crate) fn select_archive_action(&mut self, action: ArchiveAction) -> bool {
        let Some(action_position) = self
            .archive_actions()
            .iter()
            .position(|available_action| *available_action == action)
        else {
            return false;
        };
        self.table_state
            .select(Some(self.active.sessions.len() + action_position));

        true
    }

    /// Appends one session snapshot and records its new stable identifier
    /// lookup entry, preserving selection on archive pagination actions.
    pub(crate) fn push_session(&mut self, session: Session) {
        let selected_action = self.selected_archive_action();
        self.active.push_session(session);
        if let Some(action) = selected_action {
            self.select_archive_action(action);
        }
    }

    /// Removes one session snapshot by list index and rebuilds the cached
    /// identifier index when removal succeeds.
    pub(crate) fn remove_session_at(&mut self, session_index: usize) -> Option<Session> {
        self.active.remove_session_at(session_index)
    }

    /// Copies current values from one runtime handle into its `Session`
    /// snapshot on whichever loaded page holds it.
    pub fn sync_session_from_handle(&mut self, session_id: &str) {
        let (mut pages, runtime) = self.pages_and_runtime_mut();
        let Some(session_handles) = runtime.handle(session_id) else {
            return;
        };
        let Some(session) = pages.find_map(|page| page.session_mut_for_id(session_id)) else {
            return;
        };

        let previous_status = session.status;
        Self::sync_session_with_handles(session, session_handles);
        session.reconcile_status_transition(previous_status);
    }

    /// Mirrors one queued workflow row into live handles and the session's
    /// render snapshot when its page is loaded.
    pub(crate) fn upsert_queued_action(&mut self, session_id: &str, message: TransientMessage) {
        if let Some(session_handles) = self.runtime.handle(session_id) {
            session_handles.upsert_queued_action(message.clone());
        }

        if let Some(session) = self.session_mut_for_id(session_id) {
            session.transient_messages.upsert(message);
        }
    }

    /// Removes one queued workflow row from live handles and the session's
    /// render snapshot when its page is loaded.
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
    /// queued `SessionUpdated` events. This full sweep of every loaded page
    /// remains for explicit catch-up paths and focused tests.
    pub fn sync_from_handles(&mut self) {
        let (pages, runtime) = self.pages_and_runtime_mut();

        for session in pages.flat_map(|page| page.sessions.iter_mut()) {
            let Some(session_handles) = runtime.handle(&session.id) else {
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
        let Some(session) = self.session_mut_for_id(session_id) else {
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

    /// Replaces the active page's cached session git-status snapshots with
    /// one fresh poll result of the active project.
    pub(crate) fn replace_session_git_statuses(
        &mut self,
        session_git_statuses: HashMap<SessionId, SessionGitStatus>,
    ) {
        self.active.session_git_statuses = session_git_statuses;
    }

    /// Replaces the active page's cached worktree-availability snapshots
    /// with one fresh reload result.
    pub(crate) fn replace_session_worktree_availability(
        &mut self,
        session_worktree_availability: HashMap<SessionId, bool>,
    ) {
        self.active.session_worktree_availability = session_worktree_availability;
    }

    /// Replaces the active page's cached detected session branch names with
    /// one fresh reload result.
    pub(crate) fn replace_session_branch_names(
        &mut self,
        session_branch_names: HashMap<SessionId, String>,
    ) {
        self.active.session_branch_names = session_branch_names;
    }

    /// Returns the cached detected branch name of one session, from the
    /// loaded page that holds it.
    pub(crate) fn session_branch_name(&self, session_id: &str) -> Option<&str> {
        self.page_of_session(session_id)?
            .session_branch_names
            .get(session_id)
            .map(String::as_str)
    }

    /// Returns whether one session's worktree is available, as the loaded
    /// page that holds it caches it.
    pub(crate) fn session_worktree_available(&self, session_id: &str) -> Option<bool> {
        self.page_of_session(session_id)?
            .session_worktree_availability
            .get(session_id)
            .copied()
    }

    /// Updates cached worktree availability for one session after a lifecycle
    /// transition materializes or removes its worktree, on the loaded page
    /// that holds it, or on the active page for a session no page holds yet.
    pub(crate) fn set_session_worktree_available(&mut self, session_id: &str, is_available: bool) {
        // The active project never has a viewed page.
        let page = self
            .session_project_id(session_id)
            .and_then(|project_id| self.viewed.get_mut(&project_id))
            .unwrap_or(&mut self.active);
        page.session_worktree_availability
            .insert(SessionId::from(session_id), is_available);
    }

    /// Drops cached worktree availability for one removed session from
    /// every loaded page.
    pub(crate) fn remove_session_worktree_availability(&mut self, session_id: &str) {
        let (pages, _) = self.pages_and_runtime_mut();

        for page in pages {
            page.session_worktree_availability.remove(session_id);
        }
    }

    /// Drops cached branch-name entries for sessions that are no longer active
    /// in memory.
    pub(crate) fn retain_session_branch_names(&mut self, active_session_ids: &HashSet<SessionId>) {
        self.active
            .session_branch_names
            .retain(|session_id, _| active_session_ids.contains(session_id));
    }

    /// Drops cached git-status entries for sessions that are no longer active
    /// in memory.
    pub(crate) fn retain_session_git_statuses(&mut self, active_session_ids: &HashSet<SessionId>) {
        self.active
            .session_git_statuses
            .retain(|session_id, _| active_session_ids.contains(session_id));
    }

    /// Drops or clamps cached follow-up-task selection for sessions that no
    /// longer exist after a reload.
    pub(crate) fn retain_follow_up_task_positions(
        &mut self,
        active_session_ids: &HashSet<SessionId>,
    ) {
        let follow_up_task_counts = self
            .active
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

        let current_position = self
            .follow_up_task_positions
            .get(session_id)
            .copied()
            .unwrap_or(0)
            .min(follow_up_task_count - 1);
        let next_position = if move_forward {
            (current_position + 1) % follow_up_task_count
        } else if current_position == 0 {
            follow_up_task_count - 1
        } else {
            current_position - 1
        };
        self.follow_up_task_positions
            .insert(SessionId::from(session_id), next_position);
    }

    /// Returns one mutable session snapshot by identifier from any loaded
    /// page.
    pub(crate) fn session_mut_for_id(&mut self, session_id: &str) -> Option<&mut Session> {
        let (mut pages, _) = self.pages_and_runtime_mut();

        pages.find_map(|page| page.session_mut_for_id(session_id))
    }

    /// Returns the loaded page that holds `session_id`.
    fn page_of_session(&self, session_id: &str) -> Option<&SessionPage> {
        self.pages()
            .find(|page| page.session_index_for_id(session_id).is_some())
    }

    /// Returns every loaded page, the active page first, then the viewed
    /// pages in ascending project order.
    fn pages(&self) -> impl Iterator<Item = &SessionPage> {
        std::iter::once(&self.active).chain(self.viewed.values())
    }

    /// Returns mutable access to every loaded page, in the order of
    /// [`Self::pages`], beside the live handles, so a snapshot can be synced
    /// from its handle while both are borrowed.
    fn pages_and_runtime_mut(
        &mut self,
    ) -> (impl Iterator<Item = &mut SessionPage>, &SessionRuntimeState) {
        (
            std::iter::once(&mut self.active).chain(self.viewed.values_mut()),
            &self.runtime,
        )
    }
}

/// One project's loaded session list: its snapshots in list order, the
/// archive window, the per-session caches derived for them, and the persisted
/// metadata that tells a refresh whether the list changed.
pub(crate) struct SessionPage {
    /// Archive window retained until the active project changes. A viewed
    /// page keeps the first archive page.
    pub(crate) archive_limit: usize,
    /// Whether the archive window has older rows available.
    pub(crate) has_more_archived_sessions: bool,
    /// Session count across every project when the page was last checked.
    pub(super) row_count: i64,
    pub(super) sessions: Vec<Session>,
    /// Latest session update in the project when the page was last checked.
    pub(super) updated_at_max: i64,
    /// Full archive total for the project, independent of pagination.
    archived_session_count: usize,
    /// When a session-API request last named the project or one of its
    /// sessions; `None` until one does.
    last_requested_at: Option<Instant>,
    /// Project whose sessions the page holds.
    project_id: i64,
    /// Cached detected branch names keyed by session id.
    session_branch_names: HashMap<SessionId, String>,
    session_git_statuses: HashMap<SessionId, SessionGitStatus>,
    /// Cached session list positions keyed by stable session id.
    session_index_by_id: HashMap<SessionId, usize>,
    /// Cached worktree-directory availability keyed by session id.
    session_worktree_availability: HashMap<SessionId, bool>,
}

impl SessionPage {
    /// Returns all loaded session snapshots in current list order.
    pub(crate) fn sessions(&self) -> &[Session] {
        &self.sessions
    }

    /// Returns the project whose sessions the page holds.
    pub(crate) fn project_id(&self) -> i64 {
        self.project_id
    }

    /// Records the project whose rows a reload just placed in the page.
    pub(crate) fn set_project_id(&mut self, project_id: i64) {
        self.project_id = project_id;
    }

    /// Returns the persisted `(row_count, updated_at_max)` metadata the page
    /// was last checked against.
    pub(crate) fn sessions_metadata(&self) -> (i64, i64) {
        (self.row_count, self.updated_at_max)
    }

    /// Records the persisted `(row_count, updated_at_max)` metadata the page
    /// now reflects.
    pub(crate) fn set_sessions_metadata(&mut self, sessions_metadata: (i64, i64)) {
        (self.row_count, self.updated_at_max) = sessions_metadata;
    }

    /// Returns the project's archive total across every page.
    pub(crate) fn archived_session_count(&self) -> usize {
        self.archived_session_count
    }

    /// Records the archive total and whether older rows remain, as one load
    /// reported them.
    pub(crate) fn set_archive_page(
        &mut self,
        archived_session_count: usize,
        has_more_archived_sessions: bool,
    ) {
        self.archived_session_count = archived_session_count;
        self.has_more_archived_sessions = has_more_archived_sessions;
    }

    /// Collapses the archive window back to its first page.
    pub(crate) fn reset_archive_page(&mut self) {
        self.archive_limit = session_order::ARCHIVE_PAGE_SIZE;
        self.has_more_archived_sessions = false;
    }

    /// Returns cached detected branch names keyed by session id.
    pub(crate) fn session_branch_names(&self) -> &HashMap<SessionId, String> {
        &self.session_branch_names
    }

    /// Returns cached ahead/behind snapshots keyed by session id.
    pub(crate) fn session_git_statuses(&self) -> &HashMap<SessionId, SessionGitStatus> {
        &self.session_git_statuses
    }

    /// Returns cached list positions keyed by session id.
    pub(crate) fn session_index_by_id(&self) -> &HashMap<SessionId, usize> {
        &self.session_index_by_id
    }

    /// Replaces cached worktree availability with one fresh load result.
    pub(in crate::app::session) fn set_session_worktree_availability(
        &mut self,
        session_worktree_availability: HashMap<SessionId, bool>,
    ) {
        self.session_worktree_availability = session_worktree_availability;
    }

    /// Replaces cached detected branch names with one fresh load result.
    pub(in crate::app::session) fn set_session_branch_names(
        &mut self,
        session_branch_names: HashMap<SessionId, String>,
    ) {
        self.session_branch_names = session_branch_names;
    }

    /// Replaces persisted session snapshots while retaining in-process
    /// transient output for sessions that remain loaded.
    ///
    /// Database refreshes can observe intermediate workflow persistence, such
    /// as a published upstream branch before its review-request URL is ready.
    /// Carrying transient output across that refresh keeps active loaders
    /// visible until their owning reducer resolves them.
    pub(in crate::app::session) fn replace_sessions(&mut self, mut sessions: Vec<Session>) {
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

    /// Creates a page holding `sessions` of `project_id` on the first archive
    /// page, counting the archived rows it holds, with no lease.
    fn new(project_id: i64, sessions: Vec<Session>, row_count: i64, updated_at_max: i64) -> Self {
        let archived_session_count = sessions
            .iter()
            .filter(|session| matches!(session.status, Status::Done | Status::Canceled))
            .count();
        let mut page = Self {
            archive_limit: session_order::ARCHIVE_PAGE_SIZE,
            archived_session_count,
            has_more_archived_sessions: false,
            last_requested_at: None,
            project_id,
            row_count,
            session_branch_names: HashMap::new(),
            session_git_statuses: HashMap::new(),
            session_index_by_id: HashMap::new(),
            session_worktree_availability: HashMap::new(),
            sessions: Vec::new(),
            updated_at_max,
        };

        for session in sessions {
            page.push_session(session);
        }

        page
    }

    /// Returns the current list position for one stable session identifier.
    fn session_index_for_id(&self, session_id: &str) -> Option<usize> {
        self.session_index_by_id.get(session_id).copied()
    }

    /// Returns one immutable session snapshot by identifier.
    fn session_for_id(&self, session_id: &str) -> Option<&Session> {
        self.session_index_for_id(session_id)
            .and_then(|session_index| self.sessions.get(session_index))
    }

    /// Returns one mutable session snapshot by identifier.
    fn session_mut_for_id(&mut self, session_id: &str) -> Option<&mut Session> {
        let session_index = self.session_index_for_id(session_id)?;

        self.sessions.get_mut(session_index)
    }

    /// Returns whether the page's lease is younger than
    /// [`VIEWED_PAGE_IDLE`] at `now`. A page never requested holds none.
    fn is_leased_at(&self, now: Instant) -> bool {
        self.last_requested_at.is_some_and(|requested_at| {
            now.saturating_duration_since(requested_at) < VIEWED_PAGE_IDLE
        })
    }

    /// Moves render transcripts out of the loaded snapshots, keyed by session
    /// id.
    fn take_session_transcripts(&mut self) -> HashMap<SessionId, SessionTranscript> {
        self.sessions
            .iter_mut()
            .filter_map(|session| Some((session.id.clone(), session.transcript.take()?)))
            .collect()
    }

    /// Returns taken transcripts to the snapshots that are still loaded.
    fn restore_session_transcripts(
        &mut self,
        mut transcripts: HashMap<SessionId, SessionTranscript>,
    ) {
        for session in &mut self.sessions {
            if let Some(transcript) = transcripts.remove(&session.id) {
                session.transcript = Some(transcript);
            }
        }
    }

    /// Returns whether the archive window extends beyond its first page.
    fn is_archive_expanded(&self) -> bool {
        self.archive_limit > session_order::ARCHIVE_PAGE_SIZE
    }

    /// Returns available archive pagination actions in display order.
    fn archive_actions(&self) -> Vec<ArchiveAction> {
        session_order::archive_actions(self.has_more_archived_sessions, self.is_archive_expanded())
    }

    /// Appends one session snapshot and records its stable identifier lookup
    /// entry.
    fn push_session(&mut self, session: Session) {
        self.session_index_by_id
            .insert(session.id.clone(), self.sessions.len());
        self.sessions.push(session);
    }

    /// Removes one session snapshot by list index and rebuilds the cached
    /// identifier index when removal succeeds.
    fn remove_session_at(&mut self, session_index: usize) -> Option<Session> {
        if session_index >= self.sessions.len() {
            return None;
        }

        let session = self.sessions.remove(session_index);
        self.session_index_by_id = Self::build_session_index_by_id(&self.sessions);

        Some(session)
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
