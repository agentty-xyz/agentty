//! Session usage statistics produced by agent transports.

/// Known availability of a session worktree diff.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SessionDiffState {
    /// Diff availability could not be determined, so callers should preserve
    /// access to diagnostic diff output.
    #[default]
    Unknown,
    /// The latest successful diff refresh returned no content.
    Empty,
    /// The latest successful diff refresh returned content.
    Present,
}

/// Usage percentage and reset metadata for one Codex quota window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RateLimitWindow {
    /// Unix timestamp when the quota window resets, when reported.
    pub resets_at: Option<i64>,
    /// Percentage of the quota window currently consumed.
    pub used_percent: u8,
    /// Length of the quota window in minutes, when reported.
    pub window_duration_mins: Option<u64>,
}

/// Latest account-level Codex rate-limit snapshot observed by a session.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct CodexRateLimits {
    /// The shorter or primary quota window, when reported by Codex.
    pub primary: Option<RateLimitWindow>,
    /// The longer or secondary quota window, when reported by Codex.
    pub secondary: Option<RateLimitWindow>,
}

/// Token and diff usage statistics associated with one agent session or
/// isolated prompt.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SessionStats {
    /// Added diff lines currently attributed to the session worktree.
    pub added_lines: u64,
    /// Deleted diff lines currently attributed to the session worktree.
    pub deleted_lines: u64,
    /// Availability derived from the latest worktree diff refresh.
    pub diff_state: SessionDiffState,
    /// Input/prompt tokens consumed by this session.
    pub input_tokens: u64,
    /// Output/response tokens produced by this session.
    pub output_tokens: u64,
    /// Latest account-level Codex rate-limit snapshot, when available.
    pub rate_limits: Option<CodexRateLimits>,
}

impl SessionStats {
    /// Returns whether the UI should advertise access to the session diff.
    ///
    /// Unknown state retains the shortcut so a subsequent diff attempt can
    /// surface the underlying Git diagnostic instead of hiding it.
    pub fn should_show_diff(&self) -> bool {
        self.diff_state != SessionDiffState::Empty
    }

    /// Counts added and deleted lines in one git patch while ignoring file
    /// header markers such as `+++` and `---`.
    pub fn line_change_counts(diff: &str) -> (u64, u64) {
        diff.lines()
            .fold((0_u64, 0_u64), |(added_lines, deleted_lines), line| {
                if line.starts_with('+') && !line.starts_with("+++") {
                    return (added_lines.saturating_add(1), deleted_lines);
                }

                if line.starts_with('-') && !line.starts_with("---") {
                    return (added_lines, deleted_lines.saturating_add(1));
                }

                (added_lines, deleted_lines)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_show_diff_hides_only_known_empty_diffs() {
        // Arrange
        let unknown = SessionStats::default();
        let empty = SessionStats {
            diff_state: SessionDiffState::Empty,
            ..SessionStats::default()
        };
        let present = SessionStats {
            diff_state: SessionDiffState::Present,
            ..SessionStats::default()
        };

        // Act, Assert
        assert!(unknown.should_show_diff());
        assert!(!empty.should_show_diff());
        assert!(present.should_show_diff());
    }
}
