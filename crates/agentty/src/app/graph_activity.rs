//! Session status ownership for background graph work.
//!
//! Proposal generation and graph reply decisions run on the utility worker,
//! outside the session worker, yet they are work the session is doing. Each
//! holds a [`GraphActivity`] lease that shows a `Draft` or `Review` session
//! as `InProgress` from the moment the work is requested, before its task
//! starts. Leases and worker turns share one status: a turn or rebase
//! that ends while a lease is held defers its `Review` to the last lease, and
//! the last lease never ends a turn that is still running. Each lease also
//! persists a running session operation, so restart recovery returns a
//! session the lease left `InProgress` to `Review` and keeps any other status.
//! Leases and turns persist the status from separate tasks, so each write
//! re-reads it under one publication lock and the last write stores the
//! latest status.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use uuid::Uuid;

use crate::app::service::AppServices;
use crate::app::session::SessionTaskService;
use crate::domain::session::{SessionId, Status};

/// Session operation kind recorded while a graph lease is held.
pub(crate) const GRAPH_ACTIVITY_OPERATION_KIND: &str = "graph_activity";

/// Live graph leases per session, shared by leases and worker turn
/// transitions.
#[derive(Clone, Default)]
pub(crate) struct GraphActivityRegistry {
    /// Serializes status persistence by leases and worker turns.
    publication: Arc<tokio::sync::Mutex<()>>,
    sessions: Arc<Mutex<HashMap<SessionId, GraphActivityState>>>,
}

impl GraphActivityRegistry {
    /// Holds the status publication lock until the guard drops.
    ///
    /// Re-read the in-memory status while holding it and persist that value,
    /// so a write that started earlier never overwrites a newer status.
    pub(crate) async fn lock_publication(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.publication.lock().await
    }

    /// Applies `next` to the in-memory status, unless a held lease defers the
    /// `Review` that ends a turn or rebase until the last lease ends.
    ///
    /// A deferred rebase returns the session to the lease's `InProgress`.
    /// The registry lock covers the status change, so a lease ending at the
    /// same time observes the turn's start or end exactly once.
    pub(crate) fn transition(
        &self,
        session_id: &SessionId,
        status: &Mutex<Status>,
        next: Status,
    ) -> StatusChange {
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let mut current = status.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(state) = sessions.get_mut(session_id) {
            if next == Status::Review && matches!(*current, Status::InProgress | Status::Rebasing) {
                state.is_review_deferred = true;
                state.is_turn_running = false;
                if *current == Status::Rebasing {
                    *current = Status::InProgress;

                    return StatusChange::Held;
                }

                return StatusChange::Deferred;
            }
            if next == Status::InProgress {
                state.is_turn_running = true;
            }
        }

        StatusChange::set(&mut current, next)
    }

    /// Returns whether a newly queued chat message has no running worker turn
    /// to drain it, because a lease holds the session `InProgress` or the
    /// session is back in `Review`.
    ///
    /// Any other status belongs to work whose worker drains the queue when it
    /// ends. The registry lock covers the check, so a lease starting or
    /// ending at the same time is observed exactly once.
    pub(crate) fn needs_queue_worker(
        &self,
        session_id: &SessionId,
        status: &Mutex<Status>,
    ) -> bool {
        let sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let current = *status.lock().unwrap_or_else(PoisonError::into_inner);

        current == Status::Review
            || (current == Status::InProgress && sessions.contains_key(session_id))
    }

    /// Records one lease. The first lease moves a `Draft` or `Review`
    /// session to `InProgress` and returns `true`; a `Draft` session leaves
    /// the lease in `Review`, like its first finished turn. Any other status
    /// keeps its own meaning, such as a running turn, a pending question, a
    /// focused review, or a watched review request, which `Review` would
    /// discard on release.
    fn acquire(&self, session_id: &SessionId, status: &Mutex<Status>) -> bool {
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let state = sessions.entry(session_id.clone()).or_default();
        state.leases += 1;
        if state.leases > 1 {
            return false;
        }
        let mut current = status.lock().unwrap_or_else(PoisonError::into_inner);
        if !matches!(*current, Status::Draft | Status::Review) {
            return false;
        }
        *current = Status::InProgress;
        state.is_claimed = true;

        true
    }

    /// Releases one lease. The last lease restores `Review` and returns
    /// `true` when it claimed the status or a turn deferred to it, unless a
    /// turn that started meanwhile is still running.
    fn release(&self, session_id: &SessionId, status: &Mutex<Status>) -> bool {
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(state) = sessions.get_mut(session_id) else {
            return false;
        };
        state.leases = state.leases.saturating_sub(1);
        if state.leases > 0 {
            return false;
        }
        let owns_status = (state.is_claimed || state.is_review_deferred) && !state.is_turn_running;
        sessions.remove(session_id);
        let mut current = status.lock().unwrap_or_else(PoisonError::into_inner);
        if !owns_status || *current != Status::InProgress {
            return false;
        }
        *current = Status::Review;

        true
    }
}

/// Outcome of one in-memory status transition request.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum StatusChange {
    /// The status changed and must be published.
    Applied,
    /// A held graph lease will publish `Review` when it ends.
    Deferred,
    /// A held graph lease returned the session to `InProgress`, which must be
    /// published, and will publish `Review` when it ends.
    Held,
    /// The lifecycle graph rejected the transition.
    Rejected,
}

impl StatusChange {
    /// Sets `next` when the lifecycle graph allows it.
    pub(crate) fn set(current: &mut Status, next: Status) -> Self {
        if !current.can_transition_to(next) {
            return Self::Rejected;
        }
        *current = next;

        Self::Applied
    }
}

/// Lease and turn bookkeeping for one session with live graph work.
#[derive(Default)]
struct GraphActivityState {
    /// Whether the first lease moved a `Review` session to `InProgress`.
    is_claimed: bool,
    /// Whether a turn or rebase ended while a lease was held.
    is_review_deferred: bool,
    /// Whether a turn started while a lease was held and has not ended.
    is_turn_running: bool,
    /// Number of live leases.
    leases: usize,
}

/// One background graph task's claim on its session status.
///
/// Call [`GraphActivity::record`] when the task starts and
/// [`GraphActivity::finish`] when it ends. A lease that is never finished,
/// such as one aborted at shutdown, leaves its running operation for restart
/// recovery.
pub(crate) struct GraphActivity {
    /// Whether this lease moved the session to `InProgress`.
    is_claimed: bool,
    operation_id: String,
    session_id: SessionId,
    /// Status handle of the loaded session; an unloaded session keeps its
    /// status.
    status: Option<Arc<Mutex<Status>>>,
}

impl GraphActivity {
    /// Persists the lease's running operation, then the `InProgress` status
    /// it claimed, so a crash never leaves a stored `InProgress` without
    /// recoverable work.
    pub(crate) async fn record(&self, services: &AppServices) {
        let operations = services.db().operations();
        // Best-effort: without the row, only restart recovery is lost.
        let _ = operations
            .insert_session_operation(
                &self.operation_id,
                &self.session_id,
                GRAPH_ACTIVITY_OPERATION_KIND,
            )
            .await;
        let _ = operations
            .mark_session_operation_running(&self.operation_id)
            .await;
        if self.is_claimed
            && let Some(status) = &self.status
        {
            services
                .publish_latest_session_status(&self.session_id, status)
                .await;
        }
    }

    /// Restores the session status when this is the last lease and settles
    /// the lease operation.
    ///
    /// Like an active utility run, the running operation blocks graph
    /// workspace claims, so settling it wakes waiting graph drivers.
    pub(crate) async fn finish(self, services: &AppServices) {
        if let Some(status) = &self.status
            && services.graph_activity.release(&self.session_id, status)
        {
            services
                .publish_latest_session_status(&self.session_id, status)
                .await;
        }
        // Best-effort: a failed write leaves the operation for restart
        // recovery, which also restores `Review`.
        let _ = services
            .db()
            .operations()
            .mark_session_operation_done(&self.operation_id)
            .await;
        services.wake_graphs();
    }
}

impl AppServices {
    /// Starts one graph lease for `session_id`, holding `status` when the
    /// session is loaded.
    ///
    /// Call it before spawning the graph task: a claimed session shows
    /// `InProgress` at once, without waiting for the writes that
    /// [`GraphActivity::record`] makes.
    pub(crate) fn begin_graph_activity(
        &self,
        session_id: &SessionId,
        status: Option<Arc<Mutex<Status>>>,
    ) -> GraphActivity {
        let is_claimed = status
            .as_ref()
            .is_some_and(|status| self.graph_activity.acquire(session_id, status));
        if is_claimed {
            SessionTaskService::emit_session_updated(
                &self.event_sender(),
                &self.session_update_versions(),
                session_id,
            );
        }

        GraphActivity {
            is_claimed,
            operation_id: Uuid::new_v4().to_string(),
            session_id: session_id.clone(),
            status,
        }
    }

    /// Stops the session's background graph work when the user stops the
    /// session.
    ///
    /// Proposal generation is canceled like an explicit proposal cancel,
    /// and undecided replies are abandoned without acting. Each stopped task
    /// releases its lease as it ends.
    pub(crate) async fn stop_graph_activity(&self, session_id: &str) {
        let generations: Vec<_> = self
            .proposal_generations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extract_if(|_, generation| generation.session_id.as_str() == session_id)
            .collect();
        for (proposal_id, generation) in generations {
            // Best-effort: the canceled run still fails the version.
            let _ = self
                .db()
                .graph_proposals()
                .cancel(&proposal_id, generation.version)
                .await;
            generation.cancellation.cancel();
        }
        self.cancel_proposal_reply_decisions(Some(session_id));
        for decision in self
            .output_reply_decisions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter(|decision| decision.session_id.as_str() == session_id)
        {
            decision.cancellation.cancel();
        }
    }

    /// Persists and announces the latest in-memory status under the
    /// publication lock.
    async fn publish_latest_session_status(&self, session_id: &SessionId, status: &Mutex<Status>) {
        let _publication = self.graph_activity.lock_publication().await;
        let status = *status.lock().unwrap_or_else(PoisonError::into_inner);
        SessionTaskService::publish_status(
            self.clock().as_ref(),
            self.db(),
            &self.event_sender(),
            &self.session_update_versions(),
            session_id,
            status,
        )
        .await;
    }
}

#[cfg(test)]
#[path = "graph_activity_test.rs"]
mod tests;
