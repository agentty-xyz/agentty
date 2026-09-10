use std::fmt;

/// Canonical labels for bracketed workflow notices appended to a session
/// transcript.
///
/// Session output rendering uses the same labels to recognize trailing
/// workflow notices, so producing notices through this enum keeps new labels
/// aligned with summary render ordering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranscriptNotice {
    /// Prompt `/apply` command status.
    Apply,
    /// Automatic published-branch push result.
    BranchPush,
    /// Automatic published-branch push failure.
    BranchPushError,
    /// Session auto-commit result.
    Commit,
    /// Agent-assisted auto-commit recovery attempt.
    CommitAssist,
    /// Session auto-commit failure.
    CommitError,
    /// Advisory for a commit that ran without a configured pre-commit hook.
    CommitWarning,
    /// Follow-on session creation failure.
    ContinueError,
    /// Generic prompt submission failure.
    Error,
    /// Session fork creation failure.
    ForkError,
    /// Follow-up task execution failure.
    FollowUpTaskError,
    /// Merge workflow progress.
    Merge,
    /// Merge workflow failure.
    MergeError,
    /// Main checkout changed during a provider turn.
    MainCheckoutWarning,
    /// Prompt image-paste failure.
    PasteImageError,
    /// Session personality selection or fallback status.
    Personality,
    /// Queued prompt failure.
    QueueError,
    /// Session sync workflow progress.
    Rebase,
    /// Agent-assisted session sync recovery attempt.
    RebaseAssist,
    /// Session sync workflow failure.
    RebaseError,
    /// Reply submission failure.
    ReplyError,
    /// Review-request creation result.
    ReviewRequest,
    /// Successful forge review-thread replies and resolution.
    ReviewComments,
    /// Partial or failed forge review-thread resolution.
    ReviewCommentsWarning,
    /// Review-request sync warning.
    ReviewRequestSyncWarning,
    /// Draft session start failure.
    StartError,
    /// Completed-turn metadata persistence failure.
    TurnMetadataError,
}

impl TranscriptNotice {
    /// Returns the bracketed transcript prefix for this notice kind.
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Apply => "[Apply]",
            Self::BranchPush => "[Branch Push]",
            Self::BranchPushError => "[Branch Push Error]",
            Self::Commit => "[Commit]",
            Self::CommitAssist => "[Commit Assist]",
            Self::CommitError => "[Commit Error]",
            Self::CommitWarning => "[Commit Warning]",
            Self::ContinueError => "[Continue Error]",
            Self::Error => "[Error]",
            Self::ForkError => "[Fork Error]",
            Self::FollowUpTaskError => "[Follow-Up Task Error]",
            Self::Merge => "[Merge]",
            Self::MergeError => "[Merge Error]",
            Self::MainCheckoutWarning => "[Main Checkout Warning]",
            Self::PasteImageError => "[Paste Image Error]",
            Self::Personality => "[Personality]",
            Self::QueueError => "[Queue Error]",
            Self::Rebase => "[Sync]",
            Self::RebaseAssist => "[Sync Assist]",
            Self::RebaseError => "[Sync Error]",
            Self::ReplyError => "[Reply Error]",
            Self::ReviewRequest => "[Review Request]",
            Self::ReviewComments => "[Review Comments]",
            Self::ReviewCommentsWarning => "[Review Comments Warning]",
            Self::ReviewRequestSyncWarning => "[Review Request Sync Warning]",
            Self::StartError => "[Start Error]",
            Self::TurnMetadataError => "[Turn Metadata Error]",
        }
    }

    /// Formats one transcript notice as a newline-delimited paragraph.
    pub fn format(self, detail: impl fmt::Display) -> String {
        format!("\n{}\n", self.format_line(detail))
    }

    /// Formats one transcript notice as a single display line without
    /// paragraph separators.
    pub fn format_line(self, detail: impl fmt::Display) -> String {
        format!("{} {}", self.prefix(), detail)
    }
}

#[cfg(test)]
#[path = "transcript_notice_test.rs"]
mod tests;
