use ag_protocol::ProtocolRequestProfile;

use crate::agent::app_server::codex::stream_parser::{
    ExtractedAgentMessage, extract_agent_message, extract_handoff_turn_id_from_completion,
    extract_turn_completed_agent_message, extract_turn_completed_error_message,
    extract_turn_id_from_turn_completed_notification, is_codex_intermediate_phase,
    is_completed_assistant_message_item_type, is_interrupted_turn_completion_without_error,
    parse_turn_completed, preferred_completed_assistant_message,
};

#[test]
fn preferred_completed_assistant_message_prefers_latest_protocol() {
    // Arrange
    let assistant_messages = vec![
        "plain answer".to_string(),
        r#"{"answer":"first protocol answer","questions":[]}"#.to_string(),
        r#"{"answer":"latest protocol answer","questions":[]}"#.to_string(),
    ];

    // Act
    let preferred = preferred_completed_assistant_message(
        &assistant_messages,
        ProtocolRequestProfile::SessionTurn,
    );

    // Assert
    assert_eq!(
        preferred,
        r#"{"answer":"latest protocol answer","questions":[]}"#
    );
}

#[test]
fn preferred_completed_assistant_message_falls_back_to_protocol_then_plain_text() {
    // Arrange
    let protocol_messages = vec![
        "plain answer".to_string(),
        r#"{"answer":"protocol answer","questions":[]}"#.to_string(),
    ];
    let plain_messages = vec!["  ".to_string(), " plain answer ".to_string()];

    // Act
    let protocol_preferred = preferred_completed_assistant_message(
        &protocol_messages,
        ProtocolRequestProfile::SessionTurn,
    );
    let plain_preferred =
        preferred_completed_assistant_message(&plain_messages, ProtocolRequestProfile::SessionTurn);

    // Assert
    assert_eq!(
        protocol_preferred,
        r#"{"answer":"protocol answer","questions":[]}"#
    );
    assert_eq!(plain_preferred, "plain answer");
}

#[test]
fn preferred_completed_assistant_message_accepts_any_recognized_protocol_payload() {
    // Arrange
    let assistant_messages = vec![
        "plain answer".to_string(),
        r#"{"verification_verdicts":[]}"#.to_string(),
    ];

    // Act
    let preferred = preferred_completed_assistant_message(
        &assistant_messages,
        ProtocolRequestProfile::SessionTurn,
    );

    // Assert
    assert_eq!(preferred, r#"{"verification_verdicts":[]}"#);
}

#[test]
fn preferred_completed_assistant_message_accepts_direct_focused_review() {
    // Arrange
    let assistant_messages = vec![
        r#"{"project_impact":[],"suggestions":[]}"#.to_string(),
        "later status text".to_string(),
    ];

    // Act
    let preferred = preferred_completed_assistant_message(
        &assistant_messages,
        ProtocolRequestProfile::FocusedReview,
    );

    // Assert
    assert_eq!(preferred, r#"{"project_impact":[],"suggestions":[]}"#);
}

#[test]
fn preferred_completed_assistant_message_rejects_cross_profile_payload() {
    // Arrange
    let assistant_messages = vec![
        r#"{"project_impact":[],"suggestions":[]}"#.to_string(),
        "later status text".to_string(),
    ];

    // Act
    let preferred = preferred_completed_assistant_message(
        &assistant_messages,
        ProtocolRequestProfile::SessionTurn,
    );

    // Assert
    assert_eq!(preferred, "later status text");
}

#[test]
fn extract_agent_message_returns_completed_text_and_content_parts() {
    // Arrange
    let text_response = serde_json::json!({
        "method": "item/completed",
        "params": {
            "item": {
                "type": "assistant_message",
                "phase": "final",
                "text": "final text"
            }
        }
    });
    let content_response = serde_json::json!({
        "method": "item/completed",
        "params": {
            "item": {
                "type": "agentMessage",
                "content": [
                    {"text": "part one"},
                    {"text": "part two"}
                ]
            }
        }
    });

    // Act
    let text_message = extract_agent_message(&text_response);
    let content_message = extract_agent_message(&content_response);

    // Assert
    assert_eq!(
        text_message,
        Some(ExtractedAgentMessage {
            message: "final text".to_string(),
            phase: Some("final".to_string()),
        })
    );
    assert_eq!(
        content_message,
        Some(ExtractedAgentMessage {
            message: "part one\n\npart two".to_string(),
            phase: None,
        })
    );
}

#[test]
fn completed_item_type_and_phase_helpers_recognize_supported_variants() {
    // Arrange, Act
    let agent_message_supported = is_completed_assistant_message_item_type("agent_message");
    let assistant_message_supported = is_completed_assistant_message_item_type("assistantmessage");
    let command_unsupported = is_completed_assistant_message_item_type("command_execution");
    let commentary_phase = is_codex_intermediate_phase(Some(" commentary "));
    let thought_phase = is_codex_intermediate_phase(Some(" reasoning "));
    let final_phase = is_codex_intermediate_phase(Some("final_answer"));
    let missing_phase = is_codex_intermediate_phase(None);

    // Assert
    assert!(agent_message_supported);
    assert!(assistant_message_supported);
    assert!(!command_unsupported);
    assert!(commentary_phase);
    assert!(thought_phase);
    assert!(!final_phase);
    assert!(!missing_phase);
}

#[test]
fn extract_turn_completed_agent_message_uses_matching_terminal_item() {
    // Arrange
    let response_value = serde_json::json!({
        "method": "turn/completed",
        "params": {
            "turnId": "turn-123",
            "turn": {
                "status": "completed",
                "items": [
                    {
                        "type": "agentMessage",
                        "phase": "commentary",
                        "text": "I'll inspect the current code."
                    },
                    {
                        "type": "agentMessage",
                        "phase": "final_answer",
                        "text": "Final review result."
                    }
                ]
            }
        }
    });

    // Act
    let matching_message = extract_turn_completed_agent_message(&response_value, Some("turn-123"));
    let other_turn_message =
        extract_turn_completed_agent_message(&response_value, Some("turn-other"));
    let missing_turn_message = extract_turn_completed_agent_message(&response_value, None);
    let non_completion_message = extract_turn_completed_agent_message(
        &serde_json::json!({"method": "turn/started"}),
        Some("turn-123"),
    );

    // Assert
    assert_eq!(
        matching_message,
        Some(ExtractedAgentMessage {
            message: "Final review result.".to_string(),
            phase: Some("final_answer".to_string()),
        })
    );
    assert_eq!(other_turn_message, None);
    assert_eq!(missing_turn_message, None);
    assert_eq!(non_completion_message, None);
}

#[test]
fn extract_turn_completed_agent_message_rejects_commentary_only_turn() {
    // Arrange
    let response_value = serde_json::json!({
        "method": "turn/completed",
        "params": {
            "turn": {
                "id": "turn-123",
                "status": "completed",
                "items": [{
                    "type": "agentMessage",
                    "phase": "commentary",
                    "text": "I'll inspect the current code."
                }]
            }
        }
    });

    // Act
    let message = extract_turn_completed_agent_message(&response_value, Some("turn-123"));

    // Assert
    assert_eq!(message, None);
}

#[test]
fn parse_turn_completed_ignores_other_turn_ids() {
    // Arrange
    let response_value = serde_json::json!({
        "method": "turn/completed",
        "params": {
            "turn": {
                "id": "delegated-turn",
                "status": "completed"
            }
        }
    });

    // Act
    let turn_result = parse_turn_completed(&response_value, Some("active-turn"));

    // Assert
    assert_eq!(turn_result, None);
}

#[test]
fn parse_turn_completed_returns_success_and_error_statuses() {
    // Arrange
    let completed_response = serde_json::json!({
        "method": "turn/completed",
        "params": {
            "turn": {
                "id": "active-turn",
                "status": "completed"
            }
        }
    });
    let failed_response = serde_json::json!({
        "method": "turn/completed",
        "params": {
            "turn": {
                "id": "active-turn",
                "status": "failed",
                "error": {
                    "message": "tool failed",
                    "codexErrorInfo": "tool_error"
                }
            }
        }
    });

    // Act
    let completed = parse_turn_completed(&completed_response, Some("active-turn"));
    let failed = parse_turn_completed(&failed_response, Some("active-turn"));

    // Assert
    assert_eq!(completed, Some(Ok(())));
    assert_eq!(failed, Some(Err("[tool_error] tool failed".to_string())));
}

#[test]
fn parse_turn_completed_reports_missing_and_unknown_statuses() {
    // Arrange
    let missing_response = serde_json::json!({
        "method": "turn/completed",
        "params": {
            "turn": {
                "id": "active-turn"
            }
        }
    });
    let interrupted_response = serde_json::json!({
        "method": "turn/completed",
        "params": {
            "turn": {
                "id": "active-turn",
                "status": "interrupted"
            }
        }
    });

    // Act
    let missing = parse_turn_completed(&missing_response, Some("active-turn"));
    let interrupted = parse_turn_completed(&interrupted_response, Some("active-turn"));

    // Assert
    assert_eq!(
        missing,
        Some(Err("Codex app-server `turn/completed` response is \
                  missing `turn.status`"
            .to_string(),))
    );
    assert_eq!(
        interrupted,
        Some(Err("Codex app-server turn ended with non-completed \
                  status `interrupted`"
            .to_string(),))
    );
}

#[test]
fn interrupted_turn_completion_without_error_requires_matching_turn_and_status() {
    // Arrange
    let response_value = serde_json::json!({
        "method": "turn/completed",
        "params": {
            "turn": {
                "id": "active-turn",
                "status": "interrupted"
            }
        }
    });
    let errored_response = serde_json::json!({
        "method": "turn/completed",
        "params": {
            "turn": {
                "id": "active-turn",
                "status": "interrupted",
                "error": {
                    "message": "interrupted with error"
                }
            }
        }
    });

    // Act
    let interrupted =
        is_interrupted_turn_completion_without_error(&response_value, Some("active-turn"));
    let wrong_turn =
        is_interrupted_turn_completion_without_error(&response_value, Some("other-turn"));
    let errored =
        is_interrupted_turn_completion_without_error(&errored_response, Some("active-turn"));

    // Assert
    assert!(interrupted);
    assert!(!wrong_turn);
    assert!(!errored);
}

#[test]
fn extract_handoff_turn_id_from_completion_requires_waiting_without_expected_turn() {
    // Arrange
    let response_value = serde_json::json!({
        "method": "turn/completed",
        "params": {
            "turn_id": "handoff-turn"
        }
    });

    // Act
    let handoff_turn_id = extract_handoff_turn_id_from_completion(&response_value, None, true);
    let expected_turn_blocks_handoff =
        extract_handoff_turn_id_from_completion(&response_value, Some("active-turn"), true);
    let idle_wait_blocks_handoff =
        extract_handoff_turn_id_from_completion(&response_value, None, false);

    // Assert
    assert_eq!(handoff_turn_id, Some("handoff-turn".to_string()));
    assert_eq!(expected_turn_blocks_handoff, None);
    assert_eq!(idle_wait_blocks_handoff, None);
}

#[test]
fn extract_turn_completed_error_message_returns_plain_message_without_error_info() {
    // Arrange
    let response_value = serde_json::json!({
        "params": {
            "turn": {
                "error": {
                    "message": "plain failure"
                }
            }
        }
    });

    // Act
    let message = extract_turn_completed_error_message(&response_value);

    // Assert
    assert_eq!(message, Some("plain failure".to_string()));
}

#[test]
fn extract_turn_id_from_turn_completed_notification_supports_nested_flat_fields() {
    // Arrange
    let nested_response = serde_json::json!({
        "params": {
            "turn": {
                "turnId": "nested-turn"
            }
        }
    });
    let flat_response = serde_json::json!({
        "params": {
            "turn_id": "flat-turn"
        }
    });

    // Act
    let nested_turn_id = extract_turn_id_from_turn_completed_notification(&nested_response);
    let flat_turn_id = extract_turn_id_from_turn_completed_notification(&flat_response);

    // Assert
    assert_eq!(nested_turn_id, Some("nested-turn"));
    assert_eq!(flat_turn_id, Some("flat-turn"));
}
