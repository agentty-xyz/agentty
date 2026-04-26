use std::fmt;

/// Canonical labels for bracketed workflow notices appended to a session
/// transcript.
///
/// Session output rendering uses the same labels to recognize trailing
/// workflow notices, so producing notices through this enum keeps new labels
/// aligned with summary render ordering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TranscriptNotice {
    /// Automatic published-branch push failure.
    BranchPushError,
    /// Session auto-commit result.
    Commit,
    /// Agent-assisted auto-commit recovery attempt.
    CommitAssist,
    /// Session auto-commit failure.
    CommitError,
    /// Follow-on session creation failure.
    ContinueError,
    /// Generic prompt submission failure.
    Error,
    /// Follow-up task execution failure.
    FollowUpTaskError,
    /// Merge workflow progress.
    Merge,
    /// Merge workflow failure.
    MergeError,
    /// Queued prompt failure.
    QueueError,
    /// Rebase workflow progress.
    Rebase,
    /// Agent-assisted rebase recovery attempt.
    RebaseAssist,
    /// Rebase workflow failure.
    RebaseError,
    /// Reply submission failure.
    ReplyError,
    /// Review-request sync warning.
    ReviewRequestSyncWarning,
    /// Draft session start failure.
    StartError,
    /// Completed-turn metadata persistence failure.
    TurnMetadataError,
}

impl TranscriptNotice {
    /// Returns the bracketed transcript prefix for this notice kind.
    pub(crate) const fn prefix(self) -> &'static str {
        match self {
            Self::BranchPushError => "[Branch Push Error]",
            Self::Commit => "[Commit]",
            Self::CommitAssist => "[Commit Assist]",
            Self::CommitError => "[Commit Error]",
            Self::ContinueError => "[Continue Error]",
            Self::Error => "[Error]",
            Self::FollowUpTaskError => "[Follow-Up Task Error]",
            Self::Merge => "[Merge]",
            Self::MergeError => "[Merge Error]",
            Self::QueueError => "[Queue Error]",
            Self::Rebase => "[Rebase]",
            Self::RebaseAssist => "[Rebase Assist]",
            Self::RebaseError => "[Rebase Error]",
            Self::ReplyError => "[Reply Error]",
            Self::ReviewRequestSyncWarning => "[Review Request Sync Warning]",
            Self::StartError => "[Start Error]",
            Self::TurnMetadataError => "[Turn Metadata Error]",
        }
    }

    /// Formats one transcript notice as a newline-delimited paragraph.
    pub(crate) fn format(self, detail: impl fmt::Display) -> String {
        format!("\n{}\n", self.format_line(detail))
    }

    /// Formats one transcript notice as a single display line without
    /// paragraph separators.
    pub(crate) fn format_line(self, detail: impl fmt::Display) -> String {
        format!("{} {}", self.prefix(), detail)
    }
}

/// Prefixes that identify trailing transcript notices during session output
/// rendering.
pub(crate) const TRAILING_TRANSCRIPT_NOTICE_PREFIXES: &[&str] = &[
    TranscriptNotice::BranchPushError.prefix(),
    TranscriptNotice::Commit.prefix(),
    TranscriptNotice::CommitAssist.prefix(),
    TranscriptNotice::CommitError.prefix(),
    TranscriptNotice::ContinueError.prefix(),
    TranscriptNotice::Error.prefix(),
    TranscriptNotice::FollowUpTaskError.prefix(),
    TranscriptNotice::Merge.prefix(),
    TranscriptNotice::MergeError.prefix(),
    TranscriptNotice::QueueError.prefix(),
    TranscriptNotice::Rebase.prefix(),
    TranscriptNotice::RebaseAssist.prefix(),
    TranscriptNotice::RebaseError.prefix(),
    TranscriptNotice::ReplyError.prefix(),
    TranscriptNotice::ReviewRequestSyncWarning.prefix(),
    TranscriptNotice::StartError.prefix(),
    TranscriptNotice::TurnMetadataError.prefix(),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transcript_notice_format_wraps_detail_as_paragraph() {
        // Arrange
        let notice = TranscriptNotice::RebaseAssist;

        // Act
        let formatted = notice.format("Attempt 1/3. Resolving conflicts in:\n- src/main.rs");

        // Assert
        assert_eq!(
            formatted,
            "\n[Rebase Assist] Attempt 1/3. Resolving conflicts in:\n- src/main.rs\n"
        );
    }

    #[test]
    fn test_transcript_notice_format_line_omits_paragraph_spacing() {
        // Arrange
        let notice = TranscriptNotice::Commit;

        // Act
        let formatted = notice.format_line("No changes to commit.");

        // Assert
        assert_eq!(formatted, "[Commit] No changes to commit.");
    }

    #[test]
    fn test_trailing_transcript_notice_prefixes_include_generated_labels() {
        // Arrange
        let generated_labels = [
            TranscriptNotice::BranchPushError,
            TranscriptNotice::Commit,
            TranscriptNotice::CommitAssist,
            TranscriptNotice::CommitError,
            TranscriptNotice::ContinueError,
            TranscriptNotice::Error,
            TranscriptNotice::FollowUpTaskError,
            TranscriptNotice::Merge,
            TranscriptNotice::MergeError,
            TranscriptNotice::QueueError,
            TranscriptNotice::Rebase,
            TranscriptNotice::RebaseAssist,
            TranscriptNotice::RebaseError,
            TranscriptNotice::ReplyError,
            TranscriptNotice::ReviewRequestSyncWarning,
            TranscriptNotice::StartError,
            TranscriptNotice::TurnMetadataError,
        ];

        // Act & Assert
        for notice in generated_labels {
            assert!(TRAILING_TRANSCRIPT_NOTICE_PREFIXES.contains(&notice.prefix()));
        }
    }
}
