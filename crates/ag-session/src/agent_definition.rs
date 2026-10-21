//! Built-in task roles, independent of provider selection and personalities.

use crate::PermissionMode;

/// Built-in role contracts for a planner-to-implementer workflow.
///
/// These definitions do not dispatch work, select a model, or change ordinary
/// session turns. A workflow may use the same session Smart
/// [`crate::AgentSelection`] for both roles. Assignments, isolated
/// conversations, and result handoff must be supplied by the workflow before
/// these contracts can govern execution.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BuiltinAgentDefinition {
    /// Inspects the workspace and proposes a plan without making changes.
    Planner,
    /// Implements an explicitly accepted plan within session permissions.
    Implementer,
}

impl BuiltinAgentDefinition {
    /// Stable role identity, separate from provider kind or personality ID.
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

    /// Static application instructions, without interpolated user or model
    /// data.
    ///
    /// A caller must preserve the protocol's policy and response schema when
    /// delivering these instructions. Prompt text does not enforce permissions.
    pub fn instructions(self) -> &'static str {
        match self {
            Self::Planner => include_str!("template/planner_instructions.md"),
            Self::Implementer => include_str!("template/implementer_instructions.md"),
        }
    }

    /// Explicit inputs required in the role's own conversation context.
    pub fn context_requirement(self) -> AgentContextRequirement {
        match self {
            Self::Planner => AgentContextRequirement::UserRequest,
            Self::Implementer => AgentContextRequirement::UserRequestAndAcceptedPlan,
        }
    }

    /// Expected answer content, using the existing response protocol.
    pub fn output_kind(self) -> AgentOutputKind {
        match self {
            Self::Planner => AgentOutputKind::Plan,
            Self::Implementer => AgentOutputKind::ImplementationReport,
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

/// Inputs a workflow must explicitly supply, in addition to workspace access.
///
/// Each role requires its own conversation; another role's provider
/// conversation must not be implicitly reused. These requirements describe the
/// contract, not a context builder. They neither copy the session transcript
/// nor grant access.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentContextRequirement {
    /// The original user request and any clarification addressed to this role.
    UserRequest,
    /// The original request, explicitly accepted plan, and relevant
    /// clarification. A workflow must retain which planner attempt produced
    /// the accepted plan; a successful attempt by itself does not establish
    /// acceptance.
    UserRequestAndAcceptedPlan,
}

/// Semantic answer content; does not replace the protocol's response schema.
///
/// Clarification uses the existing questions field and leaves the task open.
/// Producing either output does not automatically accept or complete a task.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentOutputKind {
    /// Proposed steps, scope, assumptions, and validation for user input.
    Plan,
    /// Observed changes, verification results, and remaining work or blockers.
    ImplementationReport,
}

#[cfg(test)]
#[path = "agent_definition_test.rs"]
mod tests;
