use ag_protocol::ProtocolRequestProfile;

use crate::agent::app_server::gemini::stream_parser::{
    extract_thought_chunk, select_preferred_assistant_message,
};

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

#[test]
fn thought_chunk_returns_verbatim_reasoning_for_the_session() {
    // Arrange
    let thought = |session_id: &str, update: &str, text: &str| {
        serde_json::json!({
            "method": "session/update",
            "params": {
                "sessionId": session_id,
                "update": {"sessionUpdate": update, "content": {"type": "text", "text": text}},
            },
        })
    };

    // Act
    let reasoning = extract_thought_chunk(
        &thought("session-1", "agent_thought_chunk", "\n\n"),
        "session-1",
    );
    let empty = extract_thought_chunk(
        &thought("session-1", "agent_thought_chunk", ""),
        "session-1",
    );
    let other_session = extract_thought_chunk(
        &thought("session-2", "agent_thought_chunk", "Plan"),
        "session-1",
    );
    let message = extract_thought_chunk(
        &thought("session-1", "agent_message_chunk", "Answer"),
        "session-1",
    );

    // Assert
    assert_eq!(reasoning.as_deref(), Some("\n\n"));
    assert_eq!(empty.as_deref(), Some(""));
    assert_eq!(other_session, None);
    assert_eq!(message, None);
}
