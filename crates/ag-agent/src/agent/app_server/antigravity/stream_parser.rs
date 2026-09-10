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
        return Some(structured_output.to_string());
    }

    let response = result.get("response").and_then(value_text)?;
    let Ok(response_value) = serde_json::from_str::<Value>(&response) else {
        return Some(response);
    };

    Some(response_value.to_string())
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

#[cfg(test)]
#[path = "stream_parser_test.rs"]
mod tests;
