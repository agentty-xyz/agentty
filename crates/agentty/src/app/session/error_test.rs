use super::SessionError;

#[test]
fn not_found_display_shows_canonical_message() {
    // Arrange / Act
    let error = SessionError::NotFound;

    // Assert
    assert_eq!(error.to_string(), "Session not found");
}

#[test]
fn handles_not_found_display_shows_canonical_message() {
    // Arrange / Act
    let error = SessionError::HandlesNotFound;

    // Assert
    assert_eq!(error.to_string(), "Session handles not found");
}

#[test]
fn workflow_display_shows_contextual_message() {
    // Arrange
    let error = SessionError::Workflow("Session must be in review status".to_string());

    // Act / Assert
    assert_eq!(error.to_string(), "Session must be in review status");
}

#[test]
fn git_error_converts_via_from() {
    // Arrange
    let git_error = ag_git::GitError::OutputParse("bad output".to_string());

    // Act
    let error = SessionError::from(git_error);

    // Assert
    assert!(matches!(error, SessionError::Git(_)));
    assert_eq!(error.to_string(), "bad output");
}

#[test]
fn one_shot_error_converts_via_from() {
    // Arrange
    let one_shot_error = ag_agent::OneShotError::new("one-shot failed");

    // Act
    let error = SessionError::from(one_shot_error);

    // Assert
    assert!(matches!(error, SessionError::OneShot(_)));
    assert_eq!(error.to_string(), "one-shot failed");
}

#[test]
fn db_error_converts_via_from() {
    // Arrange
    let db_error = crate::infra::db::DbError::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "db file missing",
    ));

    // Act
    let error = SessionError::from(db_error);

    // Assert
    assert!(matches!(error, SessionError::Db(_)));
    assert!(error.to_string().contains("db file missing"));
}

#[test]
/// Ensures `with_context` prefixes the message of `Workflow` variants so
/// callers can distinguish which assist operation produced the failure.
fn with_context_prefixes_workflow_message() {
    // Arrange
    let error = SessionError::Workflow("agent backend unavailable".to_string());

    // Act
    let contextual = error.with_context("Commit assistance failed");

    // Assert
    assert_eq!(
        contextual.to_string(),
        "Commit assistance failed: agent backend unavailable"
    );
}

#[test]
/// Ensures `with_context` keeps stopped-user errors typed while still
/// adding operation context to their display message.
fn with_context_prefixes_stopped_by_user_message() {
    // Arrange
    let error = SessionError::StoppedByUser("[Stopped] Session interrupted by user.".to_string());

    // Act
    let contextual = error.with_context("Turn failed");

    // Assert
    assert!(matches!(contextual, SessionError::StoppedByUser(_)));
    assert_eq!(
        contextual.to_string(),
        "Turn failed: [Stopped] Session interrupted by user."
    );
}

#[test]
/// Ensures `with_context` passes typed infrastructure variants through
/// unchanged because their type already identifies the failure origin.
fn with_context_preserves_typed_infrastructure_variants() {
    // Arrange
    let error = SessionError::Git(ag_git::GitError::OutputParse("bad".to_string()));

    // Act
    let contextual = error.with_context("Rebase assistance failed");

    // Assert
    assert!(
        matches!(contextual, SessionError::Git(_)),
        "expected Git variant, got: {contextual:?}"
    );
    assert_eq!(contextual.to_string(), "bad");
}
