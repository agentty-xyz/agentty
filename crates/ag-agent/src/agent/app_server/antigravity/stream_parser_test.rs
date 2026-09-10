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
