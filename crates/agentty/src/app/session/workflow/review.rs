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
        self.pending_history_replay
            .insert(SessionId::from(session_id));
    }

    /// Clears one-time transcript replay tracking for a session id.
    pub(super) fn clear_history_replay_pending(&mut self, session_id: &str) {
        self.pending_history_replay.remove(session_id);
    }

    /// Returns whether a session should replay transcript output on next
    /// reply.
    pub(super) fn should_replay_history(&self, session_id: &str) -> bool {
        self.pending_history_replay.contains(session_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::session::Status;
    use crate::test_support;

    // --- startup_history_replay_set ---

    #[test]
    fn test_startup_replay_set_collects_review_sessions() {
        // Arrange
        let sessions = vec![
            test_support::session_fixture("review-1", Status::Review),
            test_support::session_fixture("in-progress", Status::InProgress),
            test_support::session_fixture("review-2", Status::Review),
        ];

        // Act
        let replay_set = SessionManager::startup_history_replay_set(&sessions);

        // Assert
        assert_eq!(replay_set.len(), 2);
        assert!(replay_set.contains("review-1"));
        assert!(replay_set.contains("review-2"));
    }

    #[test]
    fn test_startup_replay_set_collects_agent_review_sessions() {
        // Arrange
        let sessions = vec![test_support::session_fixture(
            "review-1",
            Status::AgentReview,
        )];

        // Act
        let replay_set = SessionManager::startup_history_replay_set(&sessions);

        // Assert
        assert_eq!(replay_set.len(), 1);
        assert!(replay_set.contains("review-1"));
    }

    #[test]
    fn test_startup_replay_set_returns_empty_when_no_review_sessions() {
        // Arrange
        let sessions = vec![
            test_support::session_fixture("new-1", Status::Draft),
            test_support::session_fixture("done-1", Status::Done),
        ];

        // Act
        let replay_set = SessionManager::startup_history_replay_set(&sessions);

        // Assert
        assert!(replay_set.is_empty());
    }

    #[test]
    fn test_startup_replay_set_returns_empty_for_empty_list() {
        // Arrange / Act
        let replay_set = SessionManager::startup_history_replay_set(&[]);

        // Assert
        assert!(replay_set.is_empty());
    }

    // --- mark_history_replay_pending / should_replay_history ---

    #[test]
    fn test_mark_and_check_replay_pending() {
        // Arrange
        let mut manager = test_support::session_manager_with_sessions(Vec::new());

        // Act
        manager.mark_history_replay_pending("sess-1");

        // Assert
        assert!(manager.should_replay_history("sess-1"));
    }

    #[test]
    fn test_should_replay_returns_false_when_not_marked() {
        // Arrange
        let manager = test_support::session_manager_with_sessions(Vec::new());

        // Act / Assert
        assert!(!manager.should_replay_history("unknown"));
    }

    // --- clear_history_replay_pending ---

    #[test]
    fn test_clear_removes_pending_replay() {
        // Arrange
        let mut manager = test_support::session_manager_with_sessions(Vec::new());
        manager.mark_history_replay_pending("sess-1");

        // Act
        manager.clear_history_replay_pending("sess-1");

        // Assert
        assert!(!manager.should_replay_history("sess-1"));
    }

    #[test]
    fn test_clear_is_idempotent_for_unmarked_session() {
        // Arrange
        let mut manager = test_support::session_manager_with_sessions(Vec::new());

        // Act / Assert — does not panic
        manager.clear_history_replay_pending("nonexistent");
    }

    // --- constructor auto-marks review sessions ---

    #[test]
    fn test_constructor_marks_review_sessions_for_replay() {
        // Arrange
        let sessions = vec![
            test_support::session_fixture("review-sess", Status::Review),
            test_support::session_fixture("new-sess", Status::Draft),
        ];

        // Act
        let manager = test_support::session_manager_with_sessions(sessions);

        // Assert
        assert!(manager.should_replay_history("review-sess"));
        assert!(!manager.should_replay_history("new-sess"));
    }
}
