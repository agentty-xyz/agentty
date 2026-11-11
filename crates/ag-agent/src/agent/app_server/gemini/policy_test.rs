use ag_contracts::PermissionMode;
use agent_client_protocol::v1::schema::CLIENT_METHOD_NAMES;
use serde_json::Value;

use crate::agent::app_server::gemini::policy::build_permission_response;

#[test]
fn read_only_mode_cancels_an_acp_permission_request() {
    // Arrange
    let permission_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "permission-1",
        "method": CLIENT_METHOD_NAMES.session_request_permission,
        "params": {
            "sessionId": "session-1",
            "toolCall": {"toolCallId": "tool-1"},
            "options": [{
                "optionId": "allow-once",
                "name": "Allow once",
                "kind": "allow_once"
            }]
        }
    });

    // Act
    let response =
        build_permission_response(&permission_request, "session-1", PermissionMode::ReadOnly)
            .expect("permission response should be generated");

    // Assert
    assert_eq!(
        response.pointer("/result/outcome/outcome"),
        Some(&Value::String("cancelled".to_string()))
    );
}

#[test]
fn auto_edit_mode_selects_an_acp_allow_option() {
    // Arrange
    let permission_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "permission-1",
        "method": CLIENT_METHOD_NAMES.session_request_permission,
        "params": {
            "sessionId": "session-1",
            "toolCall": {"toolCallId": "tool-1"},
            "options": [{
                "optionId": "allow-once",
                "name": "Allow once",
                "kind": "allow_once"
            }]
        }
    });

    // Act
    let response =
        build_permission_response(&permission_request, "session-1", PermissionMode::AutoEdit)
            .expect("permission response should be generated");

    // Assert
    assert_eq!(
        response.pointer("/result/outcome/optionId"),
        Some(&Value::String("allow-once".to_string()))
    );
}

#[test]
fn auto_edit_mode_selects_an_allow_option_from_legacy_raw_params() {
    // Arrange
    let permission_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "permission-1",
        "method": CLIENT_METHOD_NAMES.session_request_permission,
        "params": {
            "sessionId": "session-1",
            "options": [{
                "optionId": "allow-always",
                "kind": "allow_always"
            }]
        }
    });

    // Act
    let response =
        build_permission_response(&permission_request, "session-1", PermissionMode::AutoEdit)
            .expect("legacy permission response should be generated");

    // Assert
    assert_eq!(
        response.pointer("/result/outcome/optionId"),
        Some(&Value::String("allow-always".to_string()))
    );
}

#[test]
fn permission_requests_are_answered_only_for_the_active_session() {
    for (params, permission_mode, expected_outcome) in [
        (
            serde_json::json!({
                "sessionId": "other-session",
                "toolCall": {"toolCallId": "tool-1"},
                "options": [{"optionId": "allow-once", "name": "Allow once", "kind": "allow_once"}]
            }),
            PermissionMode::AutoEdit,
            None,
        ),
        (
            serde_json::json!({
                "sessionId": "other-session",
                "options": [{"optionId": "allow-once", "kind": "allow_once"}]
            }),
            PermissionMode::AutoEdit,
            None,
        ),
        (
            serde_json::json!({
                "sessionId": "session-1",
                "toolCall": {"toolCallId": "tool-1"},
                "options": [{"optionId": "allow-always", "name": "Always allow", "kind": "allow_always"}]
            }),
            PermissionMode::AutoEdit,
            Some(serde_json::json!({"outcome": "selected", "optionId": "allow-always"})),
        ),
        (
            serde_json::json!({
                "sessionId": "session-1",
                "toolCall": {"toolCallId": "tool-1"},
                "options": [{"optionId": "reject-once", "name": "Reject", "kind": "reject_once"}]
            }),
            PermissionMode::AutoEdit,
            Some(serde_json::json!({"outcome": "cancelled"})),
        ),
    ] {
        // Arrange
        let permission_request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "permission-1",
            "method": CLIENT_METHOD_NAMES.session_request_permission,
            "params": params,
        });

        // Act
        let response = build_permission_response(&permission_request, "session-1", permission_mode);

        // Assert
        assert_eq!(
            response.and_then(|response| response.pointer("/result/outcome").cloned()),
            expected_outcome,
            "unexpected permission response for {permission_request}"
        );
    }
}
