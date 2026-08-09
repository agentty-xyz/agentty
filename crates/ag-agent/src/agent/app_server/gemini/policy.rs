//! Gemini ACP permission policy helpers.

use agent_client_protocol::schema::v1::{
    CLIENT_METHOD_NAMES, PermissionOption, PermissionOptionKind, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome,
};
use serde_json::Value;

use crate::model::permission::PermissionMode;

/// Builds a `session/request_permission` response for the active session.
///
/// Gemini ACP uses client-selected permission options to unblock tools.
/// Agentty selects an explicit one-shot allow option for auto-edit turns and
/// cancels every request for read-only turns.
pub(super) fn build_permission_response(
    response_value: &Value,
    expected_session_id: &str,
    permission_mode: PermissionMode,
) -> Option<Value> {
    if response_value.get("method").and_then(Value::as_str)
        != Some(CLIENT_METHOD_NAMES.session_request_permission)
    {
        return None;
    }

    let params = response_value.get("params")?;
    let request_id = response_value.get("id")?.clone();
    if let Ok(permission_request) =
        serde_json::from_value::<RequestPermissionRequest>(params.clone())
    {
        if permission_request.session_id.to_string() != expected_session_id {
            return None;
        }

        let selected_option_id = (!permission_mode.is_read_only())
            .then(|| select_permission_option(&permission_request.options))
            .flatten()
            .map(|option| option.option_id.to_string());

        return Some(build_permission_result_payload(
            &request_id,
            selected_option_id,
        ));
    }

    if params.get("sessionId").and_then(Value::as_str)? != expected_session_id {
        return None;
    }

    let selected_option_id = (!permission_mode.is_read_only())
        .then(|| {
            params
                .get("options")
                .and_then(select_permission_option_id_from_value)
        })
        .flatten();

    Some(build_permission_result_payload(
        &request_id,
        selected_option_id,
    ))
}

/// Builds a JSON-RPC `result` payload from one ACP permission decision.
fn build_permission_result_payload(
    request_id: &Value,
    selected_option_id: Option<String>,
) -> Value {
    let outcome =
        selected_option_id
            .as_ref()
            .map_or(RequestPermissionOutcome::Cancelled, |option_id| {
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                    option_id.clone(),
                ))
            });
    let permission_response = RequestPermissionResponse::new(outcome);
    let result_value = match serde_json::to_value(permission_response) {
        Ok(result_value) => result_value,
        Err(_) => build_permission_result_value_fallback(selected_option_id),
    };

    serde_json::json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "result": result_value
    })
}

/// Builds a fallback ACP permission response result payload from raw values.
fn build_permission_result_value_fallback(selected_option_id: Option<String>) -> Value {
    if let Some(option_id) = selected_option_id {
        serde_json::json!({
            "outcome": {
                "outcome": "selected",
                "optionId": option_id
            }
        })
    } else {
        serde_json::json!({
            "outcome": {
                "outcome": "cancelled"
            }
        })
    }
}

/// Selects the preferred allow option from typed ACP permission choices.
fn select_permission_option(options: &[PermissionOption]) -> Option<&PermissionOption> {
    for preferred_kind in preferred_allow_option_kinds() {
        if let Some(option) = options.iter().find(|option| option.kind == preferred_kind) {
            return Some(option);
        }
    }

    None
}

/// Selects the preferred allow option identifier from raw ACP choices.
fn select_permission_option_id_from_value(options: &Value) -> Option<String> {
    let options = options.as_array()?;
    preferred_allow_option_kinds()
        .into_iter()
        .find_map(|preferred_kind| {
            let preferred_kind_value = permission_option_kind_value(preferred_kind)?;
            options.iter().find_map(|option| {
                if option.get("kind") == Some(&preferred_kind_value) {
                    option
                        .get("optionId")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                } else {
                    None
                }
            })
        })
}

/// Returns allow option kinds in Agentty's safety-preserving preference order.
fn preferred_allow_option_kinds() -> [PermissionOptionKind; 2] {
    [
        PermissionOptionKind::AllowOnce,
        PermissionOptionKind::AllowAlways,
    ]
}

/// Serializes one ACP permission kind to the wire representation.
fn permission_option_kind_value(kind: PermissionOptionKind) -> Option<Value> {
    serde_json::to_value(kind).ok()
}

#[cfg(test)]
mod tests {
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
}
