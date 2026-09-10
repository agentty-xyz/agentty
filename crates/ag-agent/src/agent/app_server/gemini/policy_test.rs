use super::*;

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
