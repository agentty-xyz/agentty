//! Gemini ACP stream parsing helpers.

use agent_client_protocol::schema::CLIENT_METHOD_NAMES;
use serde_json::Value;

use super::usage;
use crate::infra::agent;

/// Selects the most reliable final assistant payload for one Gemini turn.
///
/// Gemini ACP can stream partial assistant chunks before it returns the final
/// `session/prompt` completion payload. When the streamed accumulation is not
/// valid protocol JSON but the completion payload is, prefer the completion
/// payload so strict protocol validation sees the fully structured response.
pub(super) fn select_preferred_assistant_message(
    streamed_message: &str,
    completion_message: Option<&str>,
) -> String {
    let Some(completion_message) = completion_message.filter(|message| !message.trim().is_empty())
    else {
        return streamed_message.to_string();
    };

    if streamed_message.trim().is_empty() {
        return completion_message.to_string();
    }

    let streamed_is_protocol =
        agent::protocol::parse_agent_response_strict(streamed_message).is_ok();
    let completion_is_protocol =
        agent::protocol::parse_agent_response_strict(completion_message).is_ok();

    if completion_is_protocol && !streamed_is_protocol {
        return completion_message.to_string();
    }

    streamed_message.to_string()
}

/// Extracts assistant text chunks from ACP `session/update` events.
pub(super) fn extract_assistant_message_chunk(
    response_value: &Value,
    expected_session_id: &str,
) -> Option<String> {
    if extract_session_update_kind(response_value, expected_session_id)? != "agent_message_chunk" {
        return None;
    }

    let content = response_value
        .get("params")
        .and_then(|params| params.get("update"))
        .and_then(|update| update.get("content"))
        .and_then(usage::extract_text_from_content_value)?;
    if content.trim().is_empty() {
        return None;
    }

    Some(content)
}

/// Extracts a short user-facing progress label from ACP `session/update`.
pub(super) fn extract_progress_update(
    response_value: &Value,
    expected_session_id: &str,
) -> Option<String> {
    let session_update = extract_session_update_kind(response_value, expected_session_id)?;
    match session_update {
        "agent_thought_chunk" => Some("Thinking".to_string()),
        "tool_call" | "tool_call_update" => {
            let update = response_value.get("params")?.get("update")?;
            if let Some(status) = update.get("status").and_then(Value::as_str)
                && status.eq_ignore_ascii_case("completed")
            {
                return Some("Tool completed".to_string());
            }

            if let Some(title) = update.get("title").and_then(Value::as_str) {
                return Some(format!("Using tool: {title}"));
            }

            if let Some(kind) = update.get("kind").and_then(Value::as_str) {
                return Some(format!("Using tool: {kind}"));
            }

            Some("Using tool".to_string())
        }
        _ => None,
    }
}

/// Returns the ACP `sessionUpdate` kind for the matching session update.
pub(super) fn extract_session_update_kind<'value>(
    response_value: &'value Value,
    expected_session_id: &str,
) -> Option<&'value str> {
    if response_value.get("method").and_then(Value::as_str)
        != Some(CLIENT_METHOD_NAMES.session_update)
    {
        return None;
    }

    let params = response_value.get("params")?;
    if params.get("sessionId").and_then(Value::as_str)? != expected_session_id {
        return None;
    }

    params
        .get("update")
        .and_then(|update| update.get("sessionUpdate"))
        .and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_progress_update_returns_tool_completed_for_completed_tool_call_update() {
        // Arrange
        let response_value = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "status": "completed",
                    "title": "read_file"
                }
            }
        });

        // Act
        let progress = extract_progress_update(&response_value, "session-1");

        // Assert
        assert_eq!(progress, Some("Tool completed".to_string()));
    }

    #[test]
    fn extract_assistant_message_chunk_ignores_mismatched_session_id() {
        // Arrange
        let response_value = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "session-2",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {
                        "text": "hello"
                    }
                }
            }
        });

        // Act
        let chunk = extract_assistant_message_chunk(&response_value, "session-1");

        // Assert
        assert_eq!(chunk, None);
    }
}
