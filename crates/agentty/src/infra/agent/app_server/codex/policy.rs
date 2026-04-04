//! Codex app-server policy mapping helpers.

use serde_json::Value;

use crate::domain::agent::{AgentKind, AgentModel, ReasoningLevel};
use crate::domain::permission::PermissionMode;

/// Canonical wire-level policy mapping for one [`PermissionMode`].
///
/// The fields map directly to Codex app-server request/approval payload
/// fields so mode behavior stays consistent across thread start, turn start,
/// and pre-action approval responses.
pub(super) struct PermissionModePolicy {
    approval_policy: &'static str,
    legacy_pre_action_decision: &'static str,
    pre_action_decision: &'static str,
    thread_sandbox_mode: &'static str,
    turn_network_access: bool,
    turn_sandbox_type: &'static str,
    web_search_mode: &'static str,
}

const AUTO_EDIT_POLICY: PermissionModePolicy = PermissionModePolicy {
    approval_policy: "on-request",
    legacy_pre_action_decision: "approved",
    pre_action_decision: "accept",
    thread_sandbox_mode: "workspace-write",
    turn_network_access: true,
    turn_sandbox_type: "workspaceWrite",
    web_search_mode: "live",
};

/// Proactive compaction threshold for Codex models with a 400k context window.
///
/// [`AgentModel::Gpt54`] uses this larger threshold to keep enough room for
/// the active turn while delaying compaction.
pub(super) const AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_400K_CONTEXT: u64 = 300_000;

/// Proactive compaction threshold for Codex Spark models with a 128k context
/// window.
pub(super) const AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_128K_CONTEXT: u64 = 120_000;

/// Returns the proactive compaction threshold for one Codex model name.
///
/// This parses through [`AgentModel`] via [`AgentKind::Codex`] so model
/// mapping remains centralized in the domain enum instead of local string
/// checks. It keeps larger-window Codex models from compacting too early
/// while preserving the tighter threshold required by Spark models.
pub(super) fn auto_compact_input_token_threshold(model: &str) -> u64 {
    let is_400k_context_model =
        matches!(AgentKind::Codex.parse_model(model), Some(AgentModel::Gpt54));
    if is_400k_context_model {
        return AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_400K_CONTEXT;
    }

    AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_128K_CONTEXT
}

/// Returns the app-server approval policy used for one permission mode.
pub(super) fn approval_policy() -> &'static str {
    permission_mode_policy(PermissionMode::default()).approval_policy
}

/// Returns the thread-level sandbox mode used for one permission mode.
pub(super) fn thread_sandbox_mode() -> &'static str {
    permission_mode_policy(PermissionMode::default()).thread_sandbox_mode
}

/// Returns the turn-level sandbox policy object for one permission mode.
pub(super) fn turn_sandbox_policy() -> Value {
    let policy = permission_mode_policy(PermissionMode::default());
    let mut turn_sandbox_policy = serde_json::json!({
        "type": policy.turn_sandbox_type
    });

    if policy.turn_sandbox_type == "workspaceWrite"
        && let Some(policy_object) = turn_sandbox_policy.as_object_mut()
    {
        policy_object.insert(
            "networkAccess".to_string(),
            Value::Bool(policy.turn_network_access),
        );
    }

    turn_sandbox_policy
}

/// Returns per-thread config overrides for one permission mode.
///
/// This keeps overrides minimal while enabling live `web_search` and applying
/// the selected Codex reasoning effort.
pub(super) fn thread_config(reasoning_level: ReasoningLevel) -> Value {
    serde_json::json!({
        "web_search": web_search_mode(),
        "model_reasoning_effort": reasoning_level.codex(),
    })
}

/// Returns the `web_search` mode for one permission mode.
pub(super) fn web_search_mode() -> &'static str {
    permission_mode_policy(PermissionMode::default()).web_search_mode
}

/// Builds a JSON-RPC approval response for known pre-action request methods.
///
/// Returns `None` when the input line is not a supported approval request or
/// does not include a request id.
pub(super) fn build_pre_action_approval_response(response_value: &Value) -> Option<Value> {
    let method = response_value.get("method")?.as_str()?;
    let request_id = response_value.get("id")?.clone();
    let decision = match method {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            pre_action_approval_decision()
        }
        "execCommandApproval" | "applyPatchApproval" => legacy_pre_action_approval_decision(),
        _ => return None,
    };

    Some(serde_json::json!({
        "id": request_id,
        "result": {
            "decision": decision
        }
    }))
}

/// Returns the modern pre-action approval decision for one permission mode.
fn pre_action_approval_decision() -> &'static str {
    permission_mode_policy(PermissionMode::default()).pre_action_decision
}

/// Returns the legacy pre-action approval decision for one permission mode.
fn legacy_pre_action_approval_decision() -> &'static str {
    permission_mode_policy(PermissionMode::default()).legacy_pre_action_decision
}

/// Returns the canonical wire-level policy for one permission mode.
fn permission_mode_policy(permission_mode: PermissionMode) -> &'static PermissionModePolicy {
    match permission_mode {
        PermissionMode::AutoEdit => &AUTO_EDIT_POLICY,
    }
}
