//! Session usage statistics produced by agent transports.

/// Token and diff usage statistics associated with one agent session or
/// isolated prompt.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SessionStats {
    /// Added diff lines currently attributed to the session worktree.
    pub added_lines: u64,
    /// Deleted diff lines currently attributed to the session worktree.
    pub deleted_lines: u64,
    /// Input/prompt tokens consumed by this session.
    pub input_tokens: u64,
    /// Output/response tokens produced by this session.
    pub output_tokens: u64,
}

impl SessionStats {
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
