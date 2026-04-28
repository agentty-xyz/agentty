//! Requested-review list state for the top-level review tab.

use ag_forge::RequestedReview;

/// Project-scoped cache backing the top-level `Review` tab.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum RequestedReviewState {
    /// No requested-review fetch has started for the active project.
    #[default]
    Idle,
    /// A requested-review fetch is currently running for the project id.
    Loading {
        /// Monotonic refresh generation used to ignore superseded task
        /// results.
        generation: u64,
        /// Project id whose requested reviews are loading.
        project_id: i64,
    },
    /// Requested reviews loaded successfully for the project id.
    Loaded {
        /// Requested PR/MR rows returned by the forge CLI.
        items: Vec<RequestedReview>,
        /// Project id whose requested reviews were loaded.
        project_id: i64,
    },
    /// Requested-review fetch failed for the project id.
    Failed {
        /// User-facing load failure detail.
        message: String,
        /// Project id whose requested reviews failed to load.
        project_id: i64,
    },
}

impl RequestedReviewState {
    /// Returns `true` when the cache already represents `project_id` without a
    /// forced refresh.
    pub(crate) fn is_current_for_project(&self, project_id: i64) -> bool {
        match self {
            Self::Idle => false,
            Self::Loading {
                project_id: cached_project_id,
                ..
            }
            | Self::Loaded {
                project_id: cached_project_id,
                ..
            } => *cached_project_id == project_id,
            Self::Failed { .. } => false,
        }
    }

    /// Returns `true` when an arriving task result still matches the latest
    /// requested-review load.
    pub(crate) fn matches_loading_request(&self, project_id: i64, generation: u64) -> bool {
        match self {
            Self::Loading {
                generation: cached_generation,
                project_id: cached_project_id,
            } => *cached_project_id == project_id && *cached_generation == generation,
            Self::Idle | Self::Loaded { .. } | Self::Failed { .. } => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_failed_state_is_not_current_for_project() {
        // Arrange
        let state = RequestedReviewState::Failed {
            message: "temporary auth failure".to_string(),
            project_id: 42,
        };

        // Act
        let is_current = state.is_current_for_project(42);

        // Assert
        assert!(!is_current);
    }

    #[test]
    fn test_loading_state_matches_only_current_generation() {
        // Arrange
        let state = RequestedReviewState::Loading {
            generation: 7,
            project_id: 42,
        };

        // Act
        let matches_current_generation = state.matches_loading_request(42, 7);
        let matches_stale_generation = state.matches_loading_request(42, 6);

        // Assert
        assert!(matches_current_generation);
        assert!(!matches_stale_generation);
    }
}
