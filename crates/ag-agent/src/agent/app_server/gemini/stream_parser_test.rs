use ag_protocol::ProtocolRequestProfile;
use serde_json::{Value, json};

use crate::agent::app_server::gemini::stream_parser::{
    extract_assistant_message_chunk, extract_progress_update, select_preferred_assistant_message,
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
fn preferred_assistant_message_keeps_streamed_text_unless_only_completion_is_protocol() {
    for (streamed_message, completion_message, expected_message) in [
        ("Streamed", None, "Streamed"),
        ("Streamed", Some("  "), "Streamed"),
        ("", Some("Completion"), "Completion"),
        ("Streamed", Some("Completion"), "Streamed"),
    ] {
        // Act
        let selected_message = select_preferred_assistant_message(
            streamed_message,
            completion_message,
            ProtocolRequestProfile::SessionTurn,
        );

        // Assert
        assert_eq!(selected_message, expected_message);
    }
}

#[test]
fn assistant_chunks_require_nonblank_text_for_the_active_session() {
    for (response_value, expected_chunk) in [
        (
            session_update(
                "session-1",
                json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Hello"}}),
            ),
            Some("Hello"),
        ),
        (
            session_update(
                "session-1",
                json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "  "}}),
            ),
            None,
        ),
        (
            session_update("session-1", json!({"sessionUpdate": "agent_message_chunk"})),
            None,
        ),
        (
            session_update(
                "session-1",
                json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "Hmm"}}),
            ),
            None,
        ),
        (
            session_update(
                "other-session",
                json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Hello"}}),
            ),
            None,
        ),
    ] {
        // Act
        let chunk = extract_assistant_message_chunk(&response_value, "session-1");

        // Assert
        assert_eq!(
            chunk.as_deref(),
            expected_chunk,
            "unexpected chunk for {response_value}"
        );
    }
}

#[test]
fn progress_updates_describe_thinking_and_tool_activity() {
    for (response_value, expected_progress) in [
        (
            session_update("session-1", json!({"sessionUpdate": "agent_thought_chunk"})),
            Some("Thinking"),
        ),
        (
            session_update(
                "session-1",
                json!({"sessionUpdate": "tool_call_update", "status": "COMPLETED", "title": "Read"}),
            ),
            Some("Tool completed"),
        ),
        (
            session_update(
                "session-1",
                json!({"sessionUpdate": "tool_call", "status": "in_progress", "title": "Read file"}),
            ),
            Some("Using tool: Read file"),
        ),
        (
            session_update(
                "session-1",
                json!({"sessionUpdate": "tool_call", "kind": "execute"}),
            ),
            Some("Using tool: execute"),
        ),
        (
            session_update("session-1", json!({"sessionUpdate": "tool_call"})),
            Some("Using tool"),
        ),
        (
            session_update("session-1", json!({"sessionUpdate": "user_message_chunk"})),
            None,
        ),
        (
            json!({"method": "session/request_permission", "params": {"sessionId": "session-1"}}),
            None,
        ),
    ] {
        // Act
        let progress = extract_progress_update(&response_value, "session-1");

        // Assert
        assert_eq!(
            progress.as_deref(),
            expected_progress,
            "unexpected progress for {response_value}"
        );
    }
}

/// Builds one ACP `session/update` notification for `session_id`.
fn session_update(session_id: &str, update: Value) -> Value {
    let mut notification = json!({
        "method": "session/update",
        "params": {"sessionId": session_id}
    });
    notification["params"]["update"] = update;

    notification
}
