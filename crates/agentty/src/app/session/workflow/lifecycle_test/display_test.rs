use crate::app::SessionManager;

#[test]
/// Ensures first-person progress output cannot overwrite fallback
/// titles.
fn test_parse_generated_session_title_rejects_first_person_progress_output() {
    // Arrange
    let response_content =
        r#"{"answer":"I am checking the exact commit-message constraints.","questions":[]}"#;

    // Act
    let parsed_title = SessionManager::parse_generated_session_title(response_content);

    // Assert
    assert_eq!(parsed_title, None);
}
