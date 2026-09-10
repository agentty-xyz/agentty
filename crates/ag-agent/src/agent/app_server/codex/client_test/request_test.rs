use super::*;

#[test]
/// Verifies Codex auto-edit accepts command approvals when the app-server
/// emits them despite the non-interactive approval policy.
fn build_pre_action_approval_response_accepts_command_requests() {
    // Arrange
    let response_value = serde_json::json!({
        "id": "approval-1",
        "method": "item/commandExecution/requestApproval",
    });
    let session_folder = Path::new("/tmp/session");

    // Act
    let approval_response = policy::build_server_request_response(
        &response_value,
        crate::model::permission::PermissionMode::AutoEdit,
        session_folder,
    )
    .expect("approval response should be generated");

    // Assert
    assert_eq!(
        approval_response,
        serde_json::json!({
            "id": "approval-1",
            "result": {
                "decision": "accept"
            }
        })
    );
}

#[test]
/// Verifies Codex auto-edit starts with the same effective unrestricted
/// command access as Claude auto-edit and avoids interactive approvals.
fn build_thread_start_payload_uses_unrestricted_auto_edit_policy() {
    // Arrange
    let folder = tempdir().expect("temporary folder should be created");

    // Act
    let payload = lifecycle::build_thread_start_payload(
        folder.path(),
        AgentModel::Gpt56Sol.as_str(),
        crate::model::permission::PermissionMode::AutoEdit,
        ReasoningLevel::default(),
        SpeedMode::default(),
        "thread-start-1",
    );

    // Assert
    assert_eq!(
        payload
            .get("params")
            .and_then(|params| params.get("approvalPolicy"))
            .and_then(Value::as_str),
        Some("never")
    );
    assert_eq!(
        payload
            .get("params")
            .and_then(|params| params.get("sandbox"))
            .and_then(Value::as_str),
        Some("danger-full-access")
    );
}

#[test]
fn build_pre_action_approval_response_accepts_session_local_file_change() {
    // Arrange
    let response_value = serde_json::json!({
        "id": "approval-1",
        "method": "item/fileChange/requestApproval",
        "params": {
            "changes": [{
                "path": "/tmp/session/src/main.rs"
            }]
        }
    });
    let session_folder = Path::new("/tmp/session");

    // Act
    let approval_response = policy::build_server_request_response(
        &response_value,
        crate::model::permission::PermissionMode::AutoEdit,
        session_folder,
    )
    .expect("approval response should be generated");

    // Assert
    assert_eq!(
        approval_response
            .get("result")
            .and_then(|result| result.get("decision"))
            .and_then(Value::as_str),
        Some("accept")
    );
}

#[test]
fn build_pre_action_approval_response_rejects_outside_file_change() {
    // Arrange
    let response_value = serde_json::json!({
        "id": "approval-1",
        "method": "item/fileChange/requestApproval",
        "params": {
            "changes": [{
                "path": "/tmp/project/src/main.rs"
            }]
        }
    });
    let session_folder = Path::new("/tmp/session");

    // Act
    let approval_response = policy::build_server_request_response(
        &response_value,
        crate::model::permission::PermissionMode::AutoEdit,
        session_folder,
    )
    .expect("approval response should be generated");

    // Assert
    assert_eq!(
        approval_response
            .get("result")
            .and_then(|result| result.get("decision"))
            .and_then(Value::as_str),
        Some("reject")
    );
}

#[test]
fn build_server_request_response_grants_no_additional_permissions() {
    // Arrange
    let response_value = serde_json::json!({
        "id": "permission-1",
        "method": "item/permissions/requestApproval",
        "params": {
            "permissions": {
                "network": { "enabled": true }
            }
        }
    });
    let session_folder = Path::new("/tmp/session");

    // Act
    let response = policy::build_server_request_response(
        &response_value,
        crate::model::permission::PermissionMode::AutoEdit,
        session_folder,
    )
    .expect("permission response should be generated");

    // Assert
    assert_eq!(
        response,
        serde_json::json!({
            "id": "permission-1",
            "result": {
                "permissions": {},
                "scope": "turn"
            }
        })
    );
}

#[test]
fn build_server_request_response_declines_mcp_elicitation() {
    // Arrange
    let response_value = serde_json::json!({
        "id": "elicitation-1",
        "method": "mcpServer/elicitation/request",
        "params": {
            "message": "Allow this MCP action?"
        }
    });
    let session_folder = Path::new("/tmp/session");

    // Act
    let response = policy::build_server_request_response(
        &response_value,
        crate::model::permission::PermissionMode::AutoEdit,
        session_folder,
    )
    .expect("elicitation response should be generated");

    // Assert
    assert_eq!(
        response,
        serde_json::json!({
            "id": "elicitation-1",
            "result": {
                "action": "decline"
            }
        })
    );
}

#[test]
fn build_turn_start_payload_sets_structured_output_schema() {
    // Arrange
    let folder = tempdir().expect("temporary folder should be created");

    // Act
    let payload = lifecycle::build_turn_start_payload(&lifecycle::CodexTurnStartPayloadInput {
        folder: folder.path(),
        model: AgentModel::Gpt56Sol.as_str(),
        permission_mode: crate::model::permission::PermissionMode::AutoEdit,
        prompt: "Implement the task".into(),
        protocol_profile: ProtocolRequestProfile::SessionTurn,
        reasoning_level: ReasoningLevel::default(),
        speed_mode: SpeedMode::default(),
        thread_id: "thread-123",
        turn_start_id: "turn-start-1",
    });

    // Assert
    assert_eq!(
        payload
            .get("params")
            .and_then(|params| params.get("outputSchema"))
            .and_then(|schema| schema.get("type"))
            .and_then(Value::as_str),
        Some("object")
    );
}

#[test]
fn build_turn_start_payload_sets_direct_focused_review_schema() {
    // Arrange
    let folder = tempdir().expect("temporary folder should be created");

    // Act
    let payload = lifecycle::build_turn_start_payload(&lifecycle::CodexTurnStartPayloadInput {
        folder: folder.path(),
        model: AgentModel::Gpt56Sol.as_str(),
        permission_mode: crate::model::permission::PermissionMode::ReadOnly,
        prompt: "Review the task".into(),
        protocol_profile: ProtocolRequestProfile::FocusedReview,
        reasoning_level: ReasoningLevel::default(),
        speed_mode: SpeedMode::default(),
        thread_id: "thread-123",
        turn_start_id: "turn-start-1",
    });

    // Assert
    let properties = payload
        .pointer("/params/outputSchema/properties")
        .and_then(Value::as_object)
        .expect("focused-review schema properties should exist");
    assert!(properties.contains_key("project_impact"));
    assert!(properties.contains_key("suggestions"));
    assert!(!properties.contains_key("answer"));
    assert_eq!(
        payload.pointer("/params/outputSchema/required"),
        Some(&serde_json::json!(["project_impact", "suggestions"]))
    );
}
