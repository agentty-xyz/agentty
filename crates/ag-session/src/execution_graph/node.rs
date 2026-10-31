use ag_protocol::{ProtocolRequestProfile, TurnPrompt};
use serde::{Deserialize, Serialize};

use super::GraphContractError;
use crate::{AgentDefinition, AgentSelection, EnvId, PermissionMode};

/// An agent invocation template, independent of built-in workflow roles.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GraphNode {
    /// Captured model/provider, never resolved from mutable settings at
    /// dispatch.
    #[serde(with = "super::selection")]
    pub agent: AgentSelection,
    /// Complete captured instructions, independent of a role or catalog ID.
    pub definition: AgentDefinition,
    /// All gates must be satisfied before a new execution can be admitted.
    pub dependencies: Vec<GraphDependency>,
    /// Stable environment identity; paths are resolved by the host at
    /// execution.
    pub env_id: EnvId,
    /// Context, response, effect, budget, and resource requirements.
    pub execution: GraphExecutionProfile,
    /// Identity unique within this graph and stable across its revisions.
    pub id: String,
    /// Named result slots resolved to exact references at attempt admission.
    pub inputs: Vec<GraphInputBinding>,
    /// Desired outcome, separate from the agent definition and attempt request.
    pub objective: String,
    /// Named outputs with protocol-owned schemas, not implicit conversation
    /// replay.
    pub outputs: Vec<GraphOutput>,
    /// Captured initial request and workflow evidence, excluding bound results.
    /// Hosts supply result bodies explicitly; this is not a template language.
    pub request: TurnPrompt,
}

/// Requirements interpreted by a host before worker submission.
///
/// These values neither grant permissions nor configure a runtime directly.
/// Worker-owned execution policy remains an independent ceiling. Unsupported
/// requirements must be rejected rather than silently downgraded.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GraphExecutionProfile {
    /// Fresh utility context or isolated, resumable task conversation.
    pub context: GraphContext,
    /// Ordered host effects included in execution settlement.
    pub post_processing: GraphPostProcessing,
    /// Existing protocol schema to validate before publishing a result.
    pub response_profile: ProtocolRequestProfile,
    /// Host-owned shared provider-call budget, charged for retries and repairs.
    /// `None` retains worker limits; it does not grant unlimited execution.
    pub shared_budget_id: Option<String>,
    /// Read sharing or exclusive access within the referenced environment.
    pub workspace_access: GraphWorkspaceAccess,
}

impl GraphExecutionProfile {
    /// Intersects node and definition restrictions with session permissions.
    /// This value must still pass through worker/runtime enforcement.
    pub fn permission_mode(
        &self,
        definition: &AgentDefinition,
        session_mode: PermissionMode,
    ) -> PermissionMode {
        if self.workspace_access.is_read() {
            return PermissionMode::ReadOnly;
        }

        definition.permission_mode(session_mode)
    }

    /// Checks execution-path compatibility without invoking a worker.
    ///
    /// # Errors
    /// Rejects blank identities, session schemas on utility contexts, utility
    /// schemas on task contexts, effects on fresh or read-only execution, and
    /// background readers outside fresh utility contexts.
    pub fn validate(&self) -> Result<(), GraphContractError> {
        match &self.context {
            GraphContext::Fresh => {
                if self.response_profile == ProtocolRequestProfile::SessionTurn
                    || self.post_processing != GraphPostProcessing::None
                {
                    return Err(GraphContractError::InvalidProfile(
                        "fresh utilities require a utility schema and no session effects",
                    ));
                }
            }
            GraphContext::TaskConversation { task_id } => {
                GraphContractError::require(task_id, "task_id")?;
                if self.response_profile != ProtocolRequestProfile::SessionTurn {
                    return Err(GraphContractError::InvalidProfile(
                        "task conversations require the session-turn schema",
                    ));
                }
            }
        }

        if self.workspace_access.is_read() && self.post_processing != GraphPostProcessing::None {
            return Err(GraphContractError::InvalidProfile(
                "shared readers cannot perform host mutation effects",
            ));
        }
        if self.workspace_access == GraphWorkspaceAccess::BackgroundRead
            && self.context != GraphContext::Fresh
        {
            return Err(GraphContractError::InvalidProfile(
                "background readers require a fresh utility context",
            ));
        }
        if let Some(budget_id) = &self.shared_budget_id {
            GraphContractError::require(budget_id, "shared_budget_id")?;
        }

        Ok(())
    }
}

/// Provider conversation lifetime and existing execution authority.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum GraphContext {
    /// New context for each utility run; no provider continuation is shared.
    Fresh,
    /// Attempts reuse only this task's own captured assignment and history.
    TaskConversation {
        /// Existing or reserved task identity, unique among graph nodes.
        task_id: String,
    },
}

/// Host mutation effects, independent of model tools and response schema.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum GraphPostProcessing {
    /// No commit, publication, or automatic remediation effects.
    None,
    /// Existing ordered session effects settle before execution succeeds.
    SessionWorkflow,
}

/// Resource requirement within one Env, not a security sandbox.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum GraphWorkspaceAccess {
    /// Read-only execution may share the Env with other readers.
    SharedRead,
    /// Read-only fresh utility that may overlap ordinary host work in the
    /// Env, such as session turns and sync, but never graph writers. Use only
    /// when observing a changing workspace is acceptable.
    BackgroundRead,
    /// Serialize against all other work in the Env, including readers.
    /// Session/worker/definition restrictions can still deny writes.
    ExclusiveWrite,
}

impl GraphWorkspaceAccess {
    /// Whether this access forces read-only execution.
    #[must_use]
    pub fn is_read(self) -> bool {
        matches!(self, Self::SharedRead | Self::BackgroundRead)
    }
}

/// One prerequisite; multiple gates on one producer are permitted.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct GraphDependency {
    /// Condition the host must establish from authoritative records.
    pub gate: GraphCompletionGate,
    /// Producing node in the same graph.
    pub node_id: String,
}

/// Completion condition, distinct from desired-outcome task completion.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum GraphCompletionGate {
    /// Successful authoritative execution including ordered post-processing,
    /// with no unanswered clarification. A provider answer alone is
    /// insufficient.
    ExecutionSucceeded,
    /// Exact output validated after successful, question-free settlement.
    ResultValidated {
        /// Declared output on the producing node.
        output_name: String,
    },
    /// Explicit acceptance of an exact validated output version.
    ResultAccepted {
        /// Declared output on the producing node.
        output_name: String,
    },
}

impl GraphCompletionGate {
    /// Named output needed by this gate, if any.
    pub(super) fn output_name(&self) -> Option<&str> {
        match self {
            Self::ExecutionSucceeded => None,
            Self::ResultValidated { output_name } | Self::ResultAccepted { output_name } => {
                Some(output_name)
            }
        }
    }
}

/// Named output containing one complete response under an existing schema.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GraphOutput {
    /// Output name unique on the producing node.
    pub name: String,
    /// Must match the producing execution's response schema.
    pub schema: ProtocolRequestProfile,
}

/// A planned handoff; no attempt or result needs to exist when planning.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GraphInputBinding {
    /// Slot name unique on the consuming node.
    pub name: String,
    /// Declared output on the producer.
    pub output_name: String,
    /// Node in this graph, protected by a matching result dependency gate.
    pub producer_node_id: String,
}

#[cfg(test)]
#[path = "node_test.rs"]
mod tests;
