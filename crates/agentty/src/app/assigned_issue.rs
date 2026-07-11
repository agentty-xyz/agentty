//! Assigned GitHub issue list state for the top-level Issues tab.

use ag_forge::AssignedIssue;

/// Project-scoped cache backing the top-level `Issues` tab.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum AssignedIssueState {
    /// No assigned-issue fetch has started.
    #[default]
    Idle,
    /// An assigned-issue fetch is currently running.
    Loading {
        /// Monotonic refresh generation used to ignore superseded task results.
        generation: u64,
        /// Project id whose assigned issues are loading.
        project_id: i64,
    },
    /// Assigned issues loaded successfully.
    Loaded {
        /// Open GitHub issues assigned to the authenticated user.
        items: Vec<AssignedIssue>,
        /// Project id whose assigned issues were loaded.
        project_id: i64,
    },
    /// The assigned-issue fetch failed.
    Failed {
        /// User-facing load failure detail.
        message: String,
        /// Project id whose assigned issues failed to load.
        project_id: i64,
    },
}

impl AssignedIssueState {
    /// Returns `true` when a completed task matches the latest loading request.
    pub(crate) fn matches_loading_request(&self, project_id: i64, generation: u64) -> bool {
        matches!(
            self,
            Self::Loading {
                generation: current_generation,
                project_id: current_project_id,
            } if *current_generation == generation && *current_project_id == project_id
        )
    }

    /// Returns `true` when the cache already represents `project_id`.
    ///
    /// Failed loads remain current so tab revisits keep the actionable error
    /// visible; users can explicitly retry with the refresh action.
    pub(crate) fn is_current_for_project(&self, project_id: i64) -> bool {
        matches!(
            self,
            Self::Loading {
                project_id: current_project_id,
                ..
            } | Self::Loaded {
                project_id: current_project_id,
                ..
            } | Self::Failed {
                project_id: current_project_id,
                ..
            } if *current_project_id == project_id
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_loading_state_matches_only_current_project_and_generation() {
        // Arrange
        let state = AssignedIssueState::Loading {
            generation: 7,
            project_id: 42,
        };

        // Act
        let matches_current = state.matches_loading_request(42, 7);
        let matches_stale = state.matches_loading_request(42, 6);
        let matches_other_project = state.matches_loading_request(41, 7);
        let is_current_project = state.is_current_for_project(42);
        let is_other_project = state.is_current_for_project(41);

        // Assert
        assert!(matches_current);
        assert!(!matches_stale);
        assert!(!matches_other_project);
        assert!(is_current_project);
        assert!(!is_other_project);
    }

    #[test]
    fn test_failed_state_remains_current_until_explicit_refresh() {
        // Arrange
        let state = AssignedIssueState::Failed {
            message: "gh is not installed".to_string(),
            project_id: 42,
        };

        // Act
        let is_current_project = state.is_current_for_project(42);
        let is_other_project = state.is_current_for_project(41);

        // Assert
        assert!(is_current_project);
        assert!(!is_other_project);
    }
}
