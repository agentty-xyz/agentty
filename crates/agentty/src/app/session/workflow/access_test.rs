use std::collections::HashMap;

use crate::app::session::SessionError;
use crate::domain::session::{SessionHandles, Status};
use crate::test_support;

// --- session_index_or_err ---

#[test]
fn test_session_index_or_err_returns_index_for_existing_session() {
    // Arrange
    let session = test_support::session_fixture("sess-1", Status::Review);
    let manager = test_support::session_manager_with_handles(vec![session], HashMap::new());

    // Act
    let index = manager
        .session_index_or_err("sess-1")
        .expect("session should be found");

    // Assert
    assert_eq!(index, 0);
}

#[test]
fn test_session_index_or_err_returns_correct_index_for_second_session() {
    // Arrange
    let session_a = test_support::session_fixture("sess-a", Status::Review);
    let session_b = test_support::session_fixture("sess-b", Status::Draft);
    let manager =
        test_support::session_manager_with_handles(vec![session_a, session_b], HashMap::new());

    // Act
    let index = manager
        .session_index_or_err("sess-b")
        .expect("session should be found");

    // Assert
    assert_eq!(index, 1);
}

#[test]
fn test_session_index_or_err_returns_not_found_for_missing_session() {
    // Arrange
    let manager = test_support::session_manager_with_handles(Vec::new(), HashMap::new());

    // Act
    let result = manager.session_index_or_err("nonexistent");

    // Assert
    assert!(matches!(result, Err(SessionError::NotFound)));
}

// --- session_or_err ---

#[test]
fn test_session_or_err_returns_session_reference() {
    // Arrange
    let session = test_support::session_fixture("sess-1", Status::InProgress);
    let manager = test_support::session_manager_with_handles(vec![session], HashMap::new());

    // Act
    let found = manager
        .session_or_err("sess-1")
        .expect("session should be found");

    // Assert
    assert_eq!(found.id, "sess-1");
    assert_eq!(found.status, Status::InProgress);
}

#[test]
fn test_session_or_err_returns_not_found_for_missing_session() {
    // Arrange
    let manager = test_support::session_manager_with_handles(Vec::new(), HashMap::new());

    // Act
    let result = manager.session_or_err("missing");

    // Assert
    assert!(matches!(result, Err(SessionError::NotFound)));
}

// --- session_handles_or_err ---

#[test]
fn test_session_handles_or_err_returns_handles() {
    // Arrange
    let mut handles = HashMap::new();
    handles.insert("sess-1".into(), SessionHandles::new(Status::Review));
    let manager = test_support::session_manager_with_handles(Vec::new(), handles);

    // Act
    let result = manager.session_handles_or_err("sess-1");

    // Assert
    assert!(result.is_ok());
}

#[test]
fn test_session_handles_or_err_returns_handles_not_found() {
    // Arrange
    let manager = test_support::session_manager_with_handles(Vec::new(), HashMap::new());

    // Act
    let result = manager.session_handles_or_err("missing");

    // Assert
    assert!(matches!(result, Err(SessionError::HandlesNotFound)));
}

// --- session_and_handles_or_err ---

#[test]
fn test_session_and_handles_returns_both() {
    // Arrange
    let session = test_support::session_fixture("sess-1", Status::Review);
    let mut handles = HashMap::new();
    handles.insert(
        "sess-1".into(),
        SessionHandles::new_with_transcript(
            Status::Review,
            crate::test_support::assistant_transcript("output"),
        ),
    );
    let manager = test_support::session_manager_with_handles(vec![session], handles);

    // Act
    let result = manager.session_and_handles_or_err("sess-1");

    // Assert
    assert!(result.is_ok());
    if let Ok((found_session, found_handles)) = result {
        assert_eq!(found_session.id, "sess-1");
        let output = found_handles
            .transcript
            .lock()
            .expect("failed to lock transcript")
            .replay_text()
            .unwrap_or_default();
        assert_eq!(output, "output\n\n");
    }
}

#[test]
fn test_session_and_handles_fails_when_session_missing() {
    // Arrange
    let mut handles = HashMap::new();
    handles.insert("sess-1".into(), SessionHandles::new(Status::Review));
    let manager = test_support::session_manager_with_handles(Vec::new(), handles);

    // Act
    let result = manager.session_and_handles_or_err("sess-1");

    // Assert
    assert!(matches!(result, Err(SessionError::NotFound)));
}

#[test]
fn test_session_and_handles_fails_when_handles_missing() {
    // Arrange
    let session = test_support::session_fixture("sess-1", Status::Review);
    let manager = test_support::session_manager_with_handles(vec![session], HashMap::new());

    // Act
    let result = manager.session_and_handles_or_err("sess-1");

    // Assert
    assert!(matches!(result, Err(SessionError::HandlesNotFound)));
}
