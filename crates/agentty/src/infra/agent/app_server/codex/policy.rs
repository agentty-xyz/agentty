//! Codex app-server policy mapping helpers.

use std::path::{Component, Path, PathBuf};

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
    legacy_pre_action_rejection_decision: &'static str,
    pre_action_decision: &'static str,
    pre_action_rejection_decision: &'static str,
    thread_sandbox_mode: &'static str,
    turn_network_access: bool,
    turn_sandbox_type: &'static str,
    web_search_mode: &'static str,
}

const AUTO_EDIT_POLICY: PermissionModePolicy = PermissionModePolicy {
    approval_policy: "on-request",
    legacy_pre_action_decision: "approved",
    legacy_pre_action_rejection_decision: "denied",
    pre_action_decision: "accept",
    pre_action_rejection_decision: "reject",
    thread_sandbox_mode: "workspace-write",
    turn_network_access: true,
    turn_sandbox_type: "workspaceWrite",
    web_search_mode: "live",
};

/// Pre-action approval request categories understood by Agentty.
enum PreActionApprovalKind {
    /// Modern Codex command-execution approval request.
    Command,
    /// Modern Codex file-change approval request.
    FileChange,
    /// Legacy Codex command-execution approval request.
    LegacyCommand,
    /// Legacy Codex apply-patch approval request.
    LegacyPatch,
}

impl PreActionApprovalKind {
    /// Returns the approval kind encoded by one JSON-RPC method.
    fn from_method(method: &str) -> Option<Self> {
        match method {
            "item/commandExecution/requestApproval" => Some(Self::Command),
            "item/fileChange/requestApproval" => Some(Self::FileChange),
            "execCommandApproval" => Some(Self::LegacyCommand),
            "applyPatchApproval" => Some(Self::LegacyPatch),
            _ => None,
        }
    }

    /// Returns whether this request uses the legacy decision vocabulary.
    fn is_legacy(&self) -> bool {
        matches!(self, Self::LegacyCommand | Self::LegacyPatch)
    }

    /// Returns whether this request represents command execution.
    fn is_command(&self) -> bool {
        matches!(self, Self::Command | Self::LegacyCommand)
    }
}

/// Proactive compaction threshold for Codex models with a 400k context window.
///
/// [`AgentModel::Gpt54`] and [`AgentModel::Gpt55`] use this larger threshold
/// to keep enough room for the active turn while delaying compaction.
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
    let is_400k_context_model = matches!(
        AgentKind::Codex.parse_model(model),
        Some(AgentModel::Gpt54 | AgentModel::Gpt55)
    );
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
pub(super) fn build_pre_action_approval_response(
    response_value: &Value,
    session_folder: &Path,
) -> Option<Value> {
    let method = response_value.get("method")?.as_str()?;
    let request_id = response_value.get("id")?.clone();
    let approval_kind = PreActionApprovalKind::from_method(method)?;
    let decision = scoped_pre_action_decision(response_value, session_folder, &approval_kind);

    Some(serde_json::json!({
        "id": request_id,
        "result": {
            "decision": decision
        }
    }))
}

/// Returns the scoped decision for one Codex pre-action request.
///
/// Agentty only auto-approves file-change approvals whose declared paths stay
/// inside the session worktree. Command approvals are denied because the Codex
/// request payload does not provide a reliable path-scoped write set.
fn scoped_pre_action_decision(
    response_value: &Value,
    session_folder: &Path,
    approval_kind: &PreActionApprovalKind,
) -> &'static str {
    if approval_kind.is_command() {
        return pre_action_rejection_decision(approval_kind);
    }

    if approval_request_paths_are_session_local(response_value, session_folder) {
        return pre_action_approval_decision(approval_kind);
    }

    pre_action_rejection_decision(approval_kind)
}

/// Returns whether every declared file path remains under `session_folder`.
fn approval_request_paths_are_session_local(response_value: &Value, session_folder: &Path) -> bool {
    let mut candidate_paths = Vec::new();
    collect_candidate_paths(response_value, None, &mut candidate_paths);
    !candidate_paths.is_empty()
        && candidate_paths
            .iter()
            .all(|path| path_is_session_local(path, session_folder))
}

/// Recursively collects strings stored under path-bearing object keys.
fn collect_candidate_paths(
    value: &Value,
    key_hint: Option<&str>,
    candidate_paths: &mut Vec<String>,
) {
    match value {
        Value::Object(object) => {
            for (key, nested_value) in object {
                collect_candidate_paths(nested_value, Some(key), candidate_paths);
            }
        }
        Value::Array(values) => {
            for nested_value in values {
                collect_candidate_paths(nested_value, key_hint, candidate_paths);
            }
        }
        Value::String(text) if key_hint.is_some_and(path_key_may_contain_file_path) => {
            candidate_paths.push(text.clone());
        }
        _ => {}
    }
}

/// Returns whether an object key is likely to carry a filesystem path.
fn path_key_may_contain_file_path(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("path") || key.contains("file") || key == "cwd"
}

/// Returns whether `path_text` resolves inside `session_folder`.
fn path_is_session_local(path_text: &str, session_folder: &Path) -> bool {
    let candidate_path = PathBuf::from(path_text);
    if candidate_path.is_absolute() {
        return candidate_path.starts_with(session_folder);
    }

    normalize_session_relative_path(session_folder, &candidate_path)
        .is_some_and(|normalized_path| normalized_path.starts_with(session_folder))
}

/// Lexically normalizes one relative path under the session worktree.
fn normalize_session_relative_path(session_folder: &Path, relative_path: &Path) -> Option<PathBuf> {
    let mut normalized_path = session_folder.to_path_buf();
    for component in relative_path.components() {
        match component {
            Component::Normal(path_component) => normalized_path.push(path_component),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }

    Some(normalized_path)
}

/// Returns the modern pre-action approval decision for one permission mode.
fn pre_action_approval_decision(approval_kind: &PreActionApprovalKind) -> &'static str {
    let policy = permission_mode_policy(PermissionMode::default());
    if approval_kind.is_legacy() {
        return policy.legacy_pre_action_decision;
    }

    policy.pre_action_decision
}

/// Returns the pre-action rejection decision for one permission mode.
fn pre_action_rejection_decision(approval_kind: &PreActionApprovalKind) -> &'static str {
    let policy = permission_mode_policy(PermissionMode::default());
    if approval_kind.is_legacy() {
        return policy.legacy_pre_action_rejection_decision;
    }

    policy.pre_action_rejection_decision
}

/// Returns the canonical wire-level policy for one permission mode.
fn permission_mode_policy(permission_mode: PermissionMode) -> &'static PermissionModePolicy {
    match permission_mode {
        PermissionMode::AutoEdit => &AUTO_EDIT_POLICY,
    }
}
