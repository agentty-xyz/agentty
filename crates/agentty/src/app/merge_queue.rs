//! App-level merge queue state and transition rules.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::domain::session::{SessionId, Status};

/// Queue progression outcome after applying a status update batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MergeQueueProgress {
    /// No additional merge should be started.
    NoAction,
    /// Queue state allows starting the next queued merge.
    StartNext,
}

/// Stores FIFO merge queue state and active merge tracking.
#[derive(Default)]
pub(crate) struct MergeQueue {
    active_session_id: Option<SessionId>,
    queued_session_ids: VecDeque<SessionId>,
}

impl MergeQueue {
    /// Returns whether a session is already active or pending in the queue.
    pub(crate) fn is_queued_or_active(&self, session_id: &str) -> bool {
        if self.active_session_id.as_deref() == Some(session_id) {
            return true;
        }

        self.queued_session_ids
            .iter()
            .any(|queued_session_id| queued_session_id == session_id)
    }

    /// Adds one session to the end of the FIFO queue.
    pub(crate) fn enqueue(&mut self, session_id: SessionId) {
        self.queued_session_ids.push_back(session_id);
    }

    /// Returns whether a merge is currently active.
    pub(crate) fn has_active(&self) -> bool {
        self.active_session_id.is_some()
    }

    /// Returns whether a merge is active or waiting to start.
    pub(crate) fn has_work(&self) -> bool {
        self.has_active() || !self.queued_session_ids.is_empty()
    }

    /// Pops the next queued session id from the queue head.
    pub(crate) fn pop_next(&mut self) -> Option<SessionId> {
        self.queued_session_ids.pop_front()
    }

    /// Marks a session id as the active merge.
    pub(crate) fn set_active(&mut self, session_id: SessionId) {
        self.active_session_id = Some(session_id);
    }

    /// Resolves queue progression for one reduced app-event batch.
    ///
    /// This clears an active merge once it transitions away from `Merging`.
    /// It returns `StartNext` when the active merge either:
    /// - Transitions from `Merging` to any other state, or
    /// - Is touched in a reducer batch while already no longer `Merging`, or
    /// - Disappears from the session list after processing.
    pub(crate) fn progress_from_status_updates(
        &mut self,
        current_active_status: Option<Status>,
        session_ids: &HashSet<SessionId>,
        previous_session_states: &HashMap<SessionId, Status>,
    ) -> MergeQueueProgress {
        let Some(active_session_id) = self.active_session_id.clone() else {
            return MergeQueueProgress::NoAction;
        };
        if !session_ids.contains(&active_session_id) {
            if current_active_status.is_none() {
                self.active_session_id = None;

                return MergeQueueProgress::StartNext;
            }

            return MergeQueueProgress::NoAction;
        }

        let previous_status = previous_session_states.get(&active_session_id).copied();
        if previous_status != Some(Status::Merging) {
            if current_active_status != Some(Status::Merging) {
                self.active_session_id = None;

                return MergeQueueProgress::StartNext;
            }

            return MergeQueueProgress::NoAction;
        }

        if current_active_status == Some(Status::Merging) {
            return MergeQueueProgress::NoAction;
        }

        self.active_session_id = None;

        MergeQueueProgress::StartNext
    }

    /// Returns the currently active merge session id, if any.
    pub(crate) fn active_session_id(&self) -> Option<&str> {
        self.active_session_id.as_deref()
    }
}

#[cfg(test)]
#[path = "merge_queue_test.rs"]
mod tests;
