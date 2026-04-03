//! Gemini ACP permission policy helpers.

use agent_client_protocol::{
    CLIENT_METHOD_NAMES, PermissionOption, PermissionOptionKind, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome,
};
use serde_json::Value;

/// Builds a `session/request_permission` response for the active session.
///
/// The response follows ACP's `RequestPermissionResponse` shape. When an allow
/// option is available, this selects it to match auto-edit behavior. When no
/// options are provided or parsable, this returns a `cancelled` outcome to
/// avoid leaving the turn blocked indefinitely.
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

        let selected_option_id = select_permission_option(&permission_request.options)
            .map(|option| option.option_id.clone().to_string());

        return Some(build_permission_result_payload(
            &request_id,
            selected_option_id,
        ));
    }

    if params.get("sessionId").and_then(Value::as_str)? != expected_session_id {
        return None;
    }

    let selected_option_id = params
        .get("options")
        .and_then(select_permission_option_id_from_value);

    Some(build_permission_result_payload(
        &request_id,
        selected_option_id,
    ))
}

/// Builds a JSON-RPC `result` payload from a typed ACP permission decision.
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
        return serde_json::json!({
            "outcome": {
                "outcome": "selected",
                "optionId": option_id
            }
        });
    }

    serde_json::json!({
        "outcome": {
            "outcome": "cancelled"
        }
    })
}

/// Selects the preferred allow option from typed ACP permission choices.
fn select_permission_option(options: &[PermissionOption]) -> Option<&PermissionOption> {
    for preferred_kind in [
        PermissionOptionKind::AllowAlways,
        PermissionOptionKind::AllowOnce,
    ] {
        if let Some(option) = options.iter().find(|option| option.kind == preferred_kind) {
            return Some(option);
        }
    }

    options.first()
}

/// Selects the preferred allow option identifier from raw ACP choices.
fn select_permission_option_id_from_value(options: &Value) -> Option<String> {
    let options = options.as_array()?;
    for preferred_kind in ["allow_always", "allow_once"] {
        if let Some(option_id) = options.iter().find_map(|option| {
            if option.get("kind").and_then(Value::as_str) == Some(preferred_kind) {
                return option
                    .get("optionId")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
            }

            None
        }) {
            return Some(option_id);
        }
    }

    options
        .first()
        .and_then(|option| option.get("optionId"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}
