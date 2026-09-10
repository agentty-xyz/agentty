use crate::agent::app_server::codex::stream_parser::{
    ExtractedAgentMessage, camel_to_snake, extract_agent_message, extract_agent_message_delta,
    extract_item_started_progress, extract_turn_id_from_turn_start_response,
    extract_turn_id_from_turn_started_notification, is_context_window_exceeded_error,
};

#[test]
fn extract_turn_id_from_turn_start_response_supports_nested_and_flat_fields() {
    // Arrange
    let nested_response = serde_json::json!({
        "result": {
            "turn": {
                "turnId": "nested-turn"
            }
        }
    });
    let flat_response = serde_json::json!({
        "result": {
            "turn_id": "flat-turn"
        }
    });

    // Act
    let nested_turn_id = extract_turn_id_from_turn_start_response(&nested_response);
    let flat_turn_id = extract_turn_id_from_turn_start_response(&flat_response);

    // Assert
    assert_eq!(nested_turn_id, Some("nested-turn".to_string()));
    assert_eq!(flat_turn_id, Some("flat-turn".to_string()));
}

#[test]
fn extract_item_started_progress_normalizes_camel_case_item_type() {
    // Arrange
    let response_value = serde_json::json!({
        "method": "item/started",
        "params": {
            "item": {
                "type": "commandExecution"
            }
        }
    });

    // Act
    let progress = extract_item_started_progress(&response_value);

    // Assert
    assert_eq!(progress, Some("Running a command".to_string()));
}

#[test]
fn extract_turn_id_from_turn_started_notification_supports_nested_flat_turn_fields() {
    // Arrange
    let response_value = serde_json::json!({
        "method": "turn/started",
        "params": {
            "turn": {
                "turn_id": "turn-nested"
            }
        }
    });

    // Act
    let turn_id = extract_turn_id_from_turn_started_notification(&response_value);

    // Assert
    assert_eq!(turn_id, Some("turn-nested".to_string()));
}

#[test]
fn extract_turn_id_from_turn_started_notification_rejects_other_methods() {
    // Arrange
    let response_value = serde_json::json!({
        "method": "turn/completed",
        "params": {
            "turnId": "turn-flat"
        }
    });

    // Act
    let turn_id = extract_turn_id_from_turn_started_notification(&response_value);

    // Assert
    assert_eq!(turn_id, None);
}

#[test]
fn is_context_window_exceeded_error_detects_codex_camel_case_suffix() {
    // Arrange
    let message = "[contextWindowExceeded] Codex ran out of room in the model's context window.";

    // Act
    let is_overflow = is_context_window_exceeded_error(message);

    // Assert
    assert!(is_overflow);
}

#[test]
fn extract_agent_message_delta_supports_v2_plan_and_reasoning_deltas() {
    // Arrange
    let plan_response = serde_json::json!({
        "method": "item/plan/delta",
        "params": {
            "delta": "plan text"
        }
    });
    let reasoning_response = serde_json::json!({
        "method": "item/reasoning/textDelta",
        "params": {
            "delta": "thought text"
        }
    });

    // Act
    let plan_delta = extract_agent_message_delta(&plan_response);
    let reasoning_delta = extract_agent_message_delta(&reasoning_response);

    // Assert
    assert_eq!(
        plan_delta,
        Some(ExtractedAgentMessage {
            message: "plan text".to_string(),
            phase: Some("plan".to_string()),
        })
    );
    assert_eq!(
        reasoning_delta,
        Some(ExtractedAgentMessage {
            message: "thought text".to_string(),
            phase: Some("thinking".to_string()),
        })
    );
}

#[test]
fn extract_agent_message_delta_ignores_blank_or_non_reasoning_items() {
    // Arrange
    let blank_response = serde_json::json!({
        "method": "item/reasoning/text_delta",
        "params": {
            "delta": "  "
        }
    });
    let non_reasoning_response = serde_json::json!({
        "method": "item/updated",
        "params": {
            "item": {
                "type": "message",
                "delta": "visible"
            }
        }
    });

    // Act
    let blank_delta = extract_agent_message_delta(&blank_response);
    let non_reasoning_delta = extract_agent_message_delta(&non_reasoning_response);

    // Assert
    assert_eq!(blank_delta, None);
    assert_eq!(non_reasoning_delta, None);
}

#[test]
fn extract_agent_message_ignores_intermediate_blank_status_and_unsupported_items() {
    // Arrange
    let thought_response = serde_json::json!({
        "method": "item/completed",
        "params": {
            "item": {
                "type": "assistant_message",
                "phase": "Thinking",
                "text": "private thought"
            }
        }
    });
    let commentary_response = serde_json::json!({
        "method": "item/completed",
        "params": {
            "item": {
                "type": "agentMessage",
                "phase": "commentary",
                "text": "I'll inspect the current code."
            }
        }
    });
    let blank_text_response = serde_json::json!({
        "method": "item/completed",
        "params": {
            "item": {
                "type": "agentMessage",
                "phase": "final_answer",
                "text": "  \n "
            }
        }
    });
    let blank_content_response = serde_json::json!({
        "method": "item/completed",
        "params": {
            "item": {
                "type": "agentMessage",
                "phase": "final_answer",
                "content": [
                    {"text": "   "},
                    {"type": "output_text"}
                ]
            }
        }
    });
    let status_response = serde_json::json!({
        "method": "item/completed",
        "params": {
            "item": {
                "type": "assistant_message",
                "text": "command completed"
            }
        }
    });
    let unsupported_response = serde_json::json!({
        "method": "item/completed",
        "params": {
            "item": {
                "type": "command_execution",
                "text": "command output"
            }
        }
    });

    // Act
    let thought_message = extract_agent_message(&thought_response);
    let commentary_message = extract_agent_message(&commentary_response);
    let blank_text_message = extract_agent_message(&blank_text_response);
    let blank_content_message = extract_agent_message(&blank_content_response);
    let status_message = extract_agent_message(&status_response);
    let unsupported_message = extract_agent_message(&unsupported_response);

    // Assert
    assert_eq!(thought_message, None);
    assert_eq!(commentary_message, None);
    assert_eq!(blank_text_message, None);
    assert_eq!(blank_content_message, None);
    assert_eq!(status_message, None);
    assert_eq!(unsupported_message, None);
}

#[test]
fn camel_to_snake_preserves_lowercase_and_splits_uppercase_boundaries() {
    // Arrange
    let camel_case = "commandExecution";
    let lowercase = "command";

    // Act
    let normalized_camel_case = camel_to_snake(camel_case);
    let normalized_lowercase = camel_to_snake(lowercase);

    // Assert
    assert_eq!(normalized_camel_case, "command_execution");
    assert_eq!(normalized_lowercase, "command");
}

#[test]
fn extract_agent_message_delta_preserves_legacy_phase_labels() {
    // Arrange
    let response_value = serde_json::json!({
        "method": "item/updated",
        "params": {
            "item": {
                "type": "reasoning",
                "phase": "plan",
                "delta": "outline"
            }
        }
    });

    // Act
    let delta = extract_agent_message_delta(&response_value);

    // Assert
    assert_eq!(
        delta,
        Some(ExtractedAgentMessage {
            message: "outline".to_string(),
            phase: Some("plan".to_string()),
        })
    );
}
