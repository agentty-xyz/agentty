//! Review-session replay helpers.

use std::collections::HashSet;

use super::SessionManager;
use crate::domain::session::{Session, SessionId};

impl SessionManager {
    /// Collects session ids that should replay persisted transcript output on
    /// the next reply after app startup.
    pub(in crate::app::session) fn startup_history_replay_set(
        sessions: &[Session],
    ) -> HashSet<SessionId> {
        sessions
            .iter()
            .filter(|session| session.status.allows_review_actions())
            .map(|session| session.id.clone())
            .collect()
    }

    /// Marks a session id for one-time transcript replay on next reply.
    pub(super) fn mark_history_replay_pending(&mut self, session_id: &str) {
        self.workflow_state
            .pending_history_replay
            .insert(SessionId::from(session_id));
    }

    /// Clears one-time transcript replay tracking for a session id.
    pub(super) fn clear_history_replay_pending(&mut self, session_id: &str) {
        self.workflow_state
            .pending_history_replay
            .remove(session_id);
    }

    /// Returns whether a session should replay transcript output on next
    /// reply.
    pub(super) fn should_replay_history(&self, session_id: &str) -> bool {
        self.workflow_state
            .pending_history_replay
            .contains(session_id)
    }
}

#[cfg(test)]
#[path = "review_test.rs"]
mod tests;
