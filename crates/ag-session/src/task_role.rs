//! Host-enforced stages of the plan-and-implement workflow.

use crate::PermissionMode;

/// Built-in role contracts for a planner-to-implementer workflow.
///
/// These definitions do not dispatch work, select a model, or change ordinary
/// session turns. A workflow may use the same session Smart
/// [`crate::AgentSelection`] for both roles. Assignments, isolated
/// conversations, and result handoff must be supplied by the workflow before
/// these contracts can govern execution.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TaskAgentRole {
    /// Inspects the workspace and proposes a plan without making changes.
    Planner,
    /// Implements an explicitly accepted plan within session permissions.
    Implementer,
}

impl TaskAgentRole {
    /// Stable role identity, separate from provider kind or agent ID.
    pub fn id(self) -> &'static str {
        match self {
            Self::Planner => "planner",
            Self::Implementer => "implementer",
        }
    }

    /// Resolves a stable role identity produced by [`Self::id`].
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "planner" => Some(Self::Planner),
            "implementer" => Some(Self::Implementer),
            _ => None,
        }
    }

    /// Human-readable role name for transcripts and status displays.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Planner => "Planner",
            Self::Implementer => "Implementer",
        }
    }

    /// Restricts session permissions for this role without granting new access.
    ///
    /// Callers must pass this mode through worker/runtime permission
    /// enforcement; resolving a value alone does not sandbox a run. Worker
    /// execution policy remains an independent restriction. Implementers
    /// retain read-only sessions and the session's existing
    /// auto-address-comments behavior unchanged.
    pub fn permission_mode(self, session_permission_mode: PermissionMode) -> PermissionMode {
        match self {
            Self::Planner => PermissionMode::ReadOnly,
            Self::Implementer => session_permission_mode,
        }
    }
}

#[cfg(test)]
#[path = "task_role_test.rs"]
mod tests;
