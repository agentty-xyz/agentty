//! Antigravity NDJSON event parsing helpers.

use serde_json::Value;

use crate::app_server::AppServerStreamEvent;

/// Returns the provider conversation id carried by any supported event shape.
pub(super) fn conversation_id(payload: &Value) -> Option<&str> {
    payload
        .get("conversation_id")
        .or_else(|| {
            payload
                .get("init")
                .and_then(|init| init.get("conversation_id"))
        })
        .or_else(|| {
            payload
                .get("step_update")
                .and_then(|step| step.get("conversation_id"))
        })
        .or_else(|| {
            payload
                .get("result")
                .and_then(|result| result.get("conversation_id"))
        })
        .and_then(Value::as_str)
        .filter(|conversation_id| !conversation_id.trim().is_empty())
}

/// Returns the nested step update from one stream event.
pub(super) fn step_update(payload: &Value) -> Option<&Value> {
    (payload.get("event").and_then(Value::as_str) == Some("step_update"))
        .then(|| payload.get("step_update"))
        .flatten()
}

/// Returns the terminal result payload from one stream event.
pub(super) fn result(payload: &Value) -> Option<&Value> {
    (payload.get("event").and_then(Value::as_str) == Some("result"))
        .then(|| payload.get("result"))
        .flatten()
}

/// Extracts the schema-constrained final assistant response.
pub(super) fn result_response(result: &Value) -> Option<String> {
    if let Some(structured_output) = result.get("structured_output")
        && !structured_output.is_null()
    {
        return Some(normalize_response_value(structured_output.clone()).to_string());
    }

    let response = result.get("response").and_then(value_text)?;
    let Ok(response_value) = serde_json::from_str(&response) else {
        return Some(response);
    };

    Some(normalize_response_value(response_value).to_string())
}

/// Validates the terminal provider status.
pub(super) fn result_succeeded(result: &Value) -> bool {
    result
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| status.eq_ignore_ascii_case("success"))
}

/// Extracts a provider error from a failed result.
pub(super) fn result_error(result: &Value) -> Option<&str> {
    result
        .get("error")
        .and_then(Value::as_str)
        .filter(|error| !error.trim().is_empty())
}

/// Maps one step update to the normalized runtime stream event surface.
pub(super) fn stream_event(step_update: &Value) -> Option<AppServerStreamEvent> {
    let step_type = step_update.get("step_type").and_then(Value::as_str)?;
    if step_type.eq_ignore_ascii_case("agent_response") {
        let text_delta = step_update
            .get("text_delta")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())?;

        return Some(AppServerStreamEvent::AssistantMessage {
            is_delta: true,
            message: text_delta.to_string(),
            phase: None,
        });
    }
    if !step_update
        .get("state")
        .and_then(Value::as_str)
        .is_some_and(|state| state.eq_ignore_ascii_case("active"))
    {
        return None;
    }

    let normalized_step_type = step_type.to_ascii_lowercase().replace('-', "_");
    let progress = match normalized_step_type.as_str() {
        "context_compaction" | "context_compression" => "Compacting context".to_string(),
        "reasoning" | "thought" => "Reasoning".to_string(),
        "tool" => step_update
            .get("tool_name")
            .and_then(Value::as_str)
            .filter(|tool_name| !tool_name.trim().is_empty())
            .map_or_else(
                || "Running tool".to_string(),
                |tool_name| format!("Running {tool_name}"),
            ),
        _ => return None,
    };

    Some(AppServerStreamEvent::ProgressUpdate(progress))
}

fn value_text(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str().filter(|text| !text.trim().is_empty()) {
        return Some(text.to_string());
    }
    if value.is_object() || value.is_array() {
        return Some(value.to_string());
    }

    None
}

fn normalize_response_value(mut response: Value) -> Value {
    if let Some(summary) = response.get_mut("summary")
        && let Some(summary_text) = summary.as_str()
    {
        *summary = serde_json::json!({
            "session": summary_text,
            "turn": summary_text,
        });
    }

    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_prefers_structured_output_and_conversation_id() {
        // Arrange
        let payload = serde_json::json!({
            "event": "result",
            "result": {
                "conversation_id": "conversation-1",
                "status": "SUCCESS",
                "response": "legacy",
                "structured_output": {"answer": "ready"},
            },
        });
        let result = result(&payload).expect("result event should parse");

        // Act
        let response = result_response(result);
        let conversation_id = conversation_id(&payload);

        // Assert
        assert_eq!(response.as_deref(), Some("{\"answer\":\"ready\"}"));
        assert_eq!(conversation_id, Some("conversation-1"));
        assert!(result_succeeded(result));
    }

    #[test]
    fn step_updates_map_assistant_and_compaction_events() {
        // Arrange
        let assistant = serde_json::json!({
            "step_type": "agent_response",
            "state": "ACTIVE",
            "text_delta": "partial",
        });
        let compaction = serde_json::json!({
            "step_type": "context-compaction",
            "state": "ACTIVE",
        });

        // Act
        let assistant_event = stream_event(&assistant);
        let compaction_event = stream_event(&compaction);

        // Assert
        assert_eq!(
            assistant_event,
            Some(AppServerStreamEvent::AssistantMessage {
                is_delta: true,
                message: "partial".to_string(),
                phase: None,
            })
        );
        assert_eq!(
            compaction_event,
            Some(AppServerStreamEvent::ProgressUpdate(
                "Compacting context".to_string()
            ))
        );
    }

    #[test]
    fn conversation_id_supports_init_step_and_top_level_shapes() {
        // Arrange
        let top_level = serde_json::json!({"conversation_id": "top-level"});
        let init = serde_json::json!({"init": {"conversation_id": "from-init"}});
        let step = serde_json::json!({
            "step_update": {"conversation_id": "from-step"},
        });
        let empty = serde_json::json!({"conversation_id": "  "});

        // Act / Assert
        assert_eq!(conversation_id(&top_level), Some("top-level"));
        assert_eq!(conversation_id(&init), Some("from-init"));
        assert_eq!(conversation_id(&step), Some("from-step"));
        assert_eq!(conversation_id(&empty), None);
    }

    #[test]
    fn event_extractors_reject_unrelated_or_incomplete_payloads() {
        // Arrange
        let unrelated = serde_json::json!({"event": "init"});
        let incomplete_step = serde_json::json!({"event": "step_update"});
        let incomplete_result = serde_json::json!({"event": "result"});

        // Act / Assert
        assert_eq!(step_update(&unrelated), None);
        assert_eq!(step_update(&incomplete_step), None);
        assert_eq!(result(&unrelated), None);
        assert_eq!(result(&incomplete_result), None);
    }

    #[test]
    fn result_response_accepts_text_and_json_but_rejects_empty_scalars() {
        // Arrange
        let text = serde_json::json!({"response": "answer"});
        let object = serde_json::json!({"response": {"answer": "ready"}});
        let array = serde_json::json!({"response": ["ready"]});
        let empty = serde_json::json!({"response": "  "});
        let number = serde_json::json!({"response": 42});

        // Act / Assert
        assert_eq!(result_response(&text).as_deref(), Some("answer"));
        assert_eq!(
            result_response(&object).as_deref(),
            Some("{\"answer\":\"ready\"}")
        );
        assert_eq!(result_response(&array).as_deref(), Some("[\"ready\"]"));
        assert_eq!(result_response(&empty), None);
        assert_eq!(result_response(&number), None);
    }

    #[test]
    fn result_response_normalizes_antigravity_string_summary() {
        // Arrange
        let structured = serde_json::json!({
            "structured_output": {
                "answer": "done",
                "summary": "Completed the Antigravity turn.",
            },
        });
        let serialized = serde_json::json!({
            "response": "{\"answer\":\"done\",\"summary\":\"Completed the Antigravity turn.\"}",
        });
        let expected = serde_json::json!({
            "answer": "done",
            "summary": {
                "session": "Completed the Antigravity turn.",
                "turn": "Completed the Antigravity turn.",
            },
        })
        .to_string();

        // Act
        let structured_response = result_response(&structured);
        let serialized_response = result_response(&serialized);

        // Assert
        assert_eq!(structured_response.as_deref(), Some(expected.as_str()));
        assert_eq!(serialized_response.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn result_status_and_error_are_case_insensitive_and_nonempty() {
        // Arrange
        let success = serde_json::json!({"status": "success"});
        let failure = serde_json::json!({"status": "failed", "error": "quota"});
        let blank_error = serde_json::json!({"error": "  "});

        // Act / Assert
        assert!(result_succeeded(&success));
        assert!(!result_succeeded(&failure));
        assert_eq!(result_error(&failure), Some("quota"));
        assert_eq!(result_error(&blank_error), None);
    }

    #[test]
    fn active_reasoning_and_tool_steps_map_to_progress() {
        // Arrange
        let reasoning = serde_json::json!({"step_type": "thought", "state": "active"});
        let named_tool = serde_json::json!({
            "step_type": "tool",
            "state": "ACTIVE",
            "tool_name": "shell",
        });
        let unnamed_tool = serde_json::json!({"step_type": "tool", "state": "ACTIVE"});
        let inactive = serde_json::json!({"step_type": "reasoning", "state": "DONE"});
        let unknown = serde_json::json!({"step_type": "unknown", "state": "ACTIVE"});

        // Act / Assert
        assert_eq!(
            stream_event(&reasoning),
            Some(AppServerStreamEvent::ProgressUpdate(
                "Reasoning".to_string()
            ))
        );
        assert_eq!(
            stream_event(&named_tool),
            Some(AppServerStreamEvent::ProgressUpdate(
                "Running shell".to_string()
            ))
        );
        assert_eq!(
            stream_event(&unnamed_tool),
            Some(AppServerStreamEvent::ProgressUpdate(
                "Running tool".to_string()
            ))
        );
        assert_eq!(stream_event(&inactive), None);
        assert_eq!(stream_event(&unknown), None);
        assert_eq!(stream_event(&serde_json::json!({})), None);
    }
}
