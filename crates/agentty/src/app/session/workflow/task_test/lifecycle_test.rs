use super::super::{SESSION_COMMIT_COAUTHORED_BY_AGENTTY_TRAILER, strip_agentty_coauthor_trailer};

#[test]
/// Verifies trailer stripping removes the Agentty trailer from reused
/// commit-message continuity.
fn test_strip_agentty_coauthor_trailer_removes_trailer_line() {
    // Arrange
    let commit_message =
        format!("Refine settings page\n\n{SESSION_COMMIT_COAUTHORED_BY_AGENTTY_TRAILER}");

    // Act
    let stripped_commit_message = strip_agentty_coauthor_trailer(&commit_message);

    // Assert
    assert_eq!(stripped_commit_message, "Refine settings page\n");
}
