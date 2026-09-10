use crate::transcript_notice::TranscriptNotice;

#[test]
fn test_transcript_notice_prefixes_match_canonical_labels() {
    // Arrange
    let notices = [
        (TranscriptNotice::Apply, "[Apply]"),
        (TranscriptNotice::BranchPush, "[Branch Push]"),
        (TranscriptNotice::BranchPushError, "[Branch Push Error]"),
        (TranscriptNotice::Commit, "[Commit]"),
        (TranscriptNotice::CommitAssist, "[Commit Assist]"),
        (TranscriptNotice::CommitError, "[Commit Error]"),
        (TranscriptNotice::CommitWarning, "[Commit Warning]"),
        (TranscriptNotice::ContinueError, "[Continue Error]"),
        (TranscriptNotice::Error, "[Error]"),
        (TranscriptNotice::ForkError, "[Fork Error]"),
        (
            TranscriptNotice::FollowUpTaskError,
            "[Follow-Up Task Error]",
        ),
        (TranscriptNotice::Merge, "[Merge]"),
        (TranscriptNotice::MergeError, "[Merge Error]"),
        (
            TranscriptNotice::MainCheckoutWarning,
            "[Main Checkout Warning]",
        ),
        (TranscriptNotice::PasteImageError, "[Paste Image Error]"),
        (TranscriptNotice::Personality, "[Personality]"),
        (TranscriptNotice::QueueError, "[Queue Error]"),
        (TranscriptNotice::Rebase, "[Sync]"),
        (TranscriptNotice::RebaseAssist, "[Sync Assist]"),
        (TranscriptNotice::RebaseError, "[Sync Error]"),
        (TranscriptNotice::ReplyError, "[Reply Error]"),
        (TranscriptNotice::ReviewRequest, "[Review Request]"),
        (TranscriptNotice::ReviewComments, "[Review Comments]"),
        (
            TranscriptNotice::ReviewCommentsWarning,
            "[Review Comments Warning]",
        ),
        (
            TranscriptNotice::ReviewRequestSyncWarning,
            "[Review Request Sync Warning]",
        ),
        (TranscriptNotice::StartError, "[Start Error]"),
        (TranscriptNotice::TurnMetadataError, "[Turn Metadata Error]"),
    ];

    // Act
    let prefixes = notices.map(|(notice, _)| notice.prefix());
    let expected = notices.map(|(_, expected)| expected);

    // Assert
    assert_eq!(prefixes, expected);
}

#[test]
fn test_transcript_notice_format_wraps_detail_as_paragraph() {
    // Arrange
    let notice = TranscriptNotice::RebaseAssist;

    // Act
    let formatted = notice.format("Attempt 1/3. Resolving conflicts in:\n- src/main.rs");

    // Assert
    assert_eq!(
        formatted,
        "\n[Sync Assist] Attempt 1/3. Resolving conflicts in:\n- src/main.rs\n"
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
