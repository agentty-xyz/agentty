//! Gemini ACP permission policy helpers.

use agent_client_protocol::schema::{
    CLIENT_METHOD_NAMES, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse,
};
use serde_json::Value;

/// Builds a `session/request_permission` response for the active session.
///
/// Gemini ACP permission payloads do not provide a reliable session-local
/// filesystem write set, so Agentty cancels the request instead of granting an
/// unscoped provider permission.
pub(super) fn build_permission_response(
    response_value: &Value,
    expected_session_id: &str,
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

        return Some(build_cancelled_permission_result_payload(&request_id));
    }

    if params.get("sessionId").and_then(Value::as_str)? != expected_session_id {
        return None;
    }

    Some(build_cancelled_permission_result_payload(&request_id))
}

/// Builds a JSON-RPC `result` payload for a cancelled ACP permission request.
fn build_cancelled_permission_result_payload(request_id: &Value) -> Value {
    let outcome = RequestPermissionOutcome::Cancelled;
    let permission_response = RequestPermissionResponse::new(outcome);
    let result_value = match serde_json::to_value(permission_response) {
        Ok(result_value) => result_value,
        Err(_) => build_cancelled_permission_result_value_fallback(),
    };

    serde_json::json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "result": result_value
    })
}

/// Builds a fallback ACP permission response result payload.
fn build_cancelled_permission_result_value_fallback() -> Value {
    serde_json::json!({
        "outcome": {
            "outcome": "cancelled"
        }
    })
}
