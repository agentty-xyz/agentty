use super::*;

#[test]
fn valid_direct_focused_review_supersedes_invalid_streamed_message() {
    // Arrange
    let streamed_message = r#"{"project_impact":["Incomplete""#;
    let completion_message = r#"{"project_impact":["Improves reliability."],"suggestions":[]}"#;

    // Act
    let selected_message = select_preferred_assistant_message(
        streamed_message,
        Some(completion_message),
        ProtocolRequestProfile::FocusedReview,
    );

    // Assert
    assert_eq!(selected_message, completion_message);
}
