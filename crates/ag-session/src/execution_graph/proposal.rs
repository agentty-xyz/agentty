//! Pure resolution of untrusted logical proposals against captured host inputs.

use std::num::NonZeroU64;

use ag_protocol::{
    GraphProposal, GraphProposalContext, GraphProposalGate, GraphProposalNode,
    GraphProposalPostProcessing, GraphProposalWorkspaceAccess, ProtocolRequestProfile, TurnPrompt,
};
use serde::{Deserialize, Serialize};

use super::{
    ExecutionGraph, GraphCompletionGate, GraphContext, GraphDependency, GraphExecutionProfile,
    GraphInputBinding, GraphNode, GraphOutput, GraphPostProcessing, GraphWorkspaceAccess,
};
use crate::{AgentDefinition, AgentSelection, EnvId, PermissionMode, SessionId};

/// Captured ceilings shared by generation, inspection, and approval.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GraphProposalLimits {
    /// Maximum serialized response size, in UTF-8 bytes.
    pub bytes: usize,
    /// Maximum prerequisite edges, including distinct gates.
    pub edges: usize,
    /// Maximum explicit generation requests for this proposal, across restarts.
    pub generations: u32,
    /// Provider calls per generation, including transport retries and repairs.
    pub generation_provider_calls: usize,
    /// Maximum nodes in one proposal.
    pub nodes: usize,
    /// Shared provider-call ceiling for the executable graph.
    pub provider_calls: usize,
}

impl Default for GraphProposalLimits {
    fn default() -> Self {
        Self {
            bytes: 65_536,
            edges: 32,
            generations: 3,
            generation_provider_calls: 3,
            nodes: 16,
            provider_calls: 48,
        }
    }
}

/// Host-owned inputs captured before generation; no external work occurs here.
pub struct GraphProposalResolution<'a> {
    /// Exact provider and model shared by every proposed node.
    pub agent: AgentSelection,
    /// Saved question-and-answer history, encoded as JSON data for each node.
    pub answers: &'a [String],
    /// Enabled definitions captured from the session's catalog.
    pub definitions: &'a [AgentDefinition],
    /// Fresh host-allocated graph namespace, never supplied by the model.
    pub graph_id: &'a str,
    /// Captured generation and execution ceilings.
    pub limits: &'a GraphProposalLimits,
    /// Session permission ceiling captured by the host.
    pub permission: PermissionMode,
    /// Original prompt; resolution appends answers and retains its source
    /// classification.
    pub request: &'a TurnPrompt,
    /// Session whose shared Env is bound to every node.
    pub session_id: &'a SessionId,
}

impl GraphProposalResolution<'_> {
    /// Resolves only supported profiles and rejects escalation before approval.
    ///
    /// # Errors
    /// Rejects size limits, attachments (currently unsupported by graph
    /// utilities), unknown definitions, unsupported profiles, permissions, and
    /// invalid graphs.
    pub fn resolve(&self, proposal: &GraphProposal) -> Result<ExecutionGraph, String> {
        if self.request.has_attachments() {
            return Err("Graph proposals do not yet support attachment paths".into());
        }
        if self.limits.generations == 0
            || self.limits.generation_provider_calls == 0
            || self.limits.provider_calls == 0
            || self.request.text.trim().is_empty()
            || serde_json::json!(proposal).to_string().len() > self.limits.bytes
            || proposal.nodes.len() > self.limits.nodes
            || proposal
                .nodes
                .iter()
                .map(|node| node.dependencies.len())
                .sum::<usize>()
                > self.limits.edges
        {
            return Err("Graph proposal exceeds captured limits or has an empty request".into());
        }
        let mut request = self.request.clone();
        if !self.answers.is_empty() {
            request
                .text
                .push_str("\n\nComplete clarification answer history (JSON data):\n");
            request
                .text
                .push_str(&serde_json::json!(self.answers).to_string());
        }
        let mut nodes = Vec::with_capacity(proposal.nodes.len());
        for (index, node) in proposal.nodes.iter().enumerate() {
            let definition = self
                .definitions
                .iter()
                .find(|definition| definition.id == node.agent_id)
                .ok_or_else(|| format!("Unavailable agent definition: {}", node.agent_id))?;
            nodes.push(GraphNode {
                agent: self.agent,
                definition: definition.clone(),
                dependencies: node
                    .dependencies
                    .iter()
                    .map(|dependency| GraphDependency {
                        gate: match &dependency.gate {
                            GraphProposalGate::ExecutionSucceeded {} => {
                                GraphCompletionGate::ExecutionSucceeded
                            }
                            GraphProposalGate::ResultValidated { output_name } => {
                                GraphCompletionGate::ResultValidated {
                                    output_name: output_name.clone(),
                                }
                            }
                            GraphProposalGate::ResultAccepted { output_name } => {
                                GraphCompletionGate::ResultAccepted {
                                    output_name: output_name.clone(),
                                }
                            }
                        },
                        node_id: dependency.node_id.clone(),
                    })
                    .collect(),
                env_id: EnvId::SessionWorktree(self.session_id.clone()),
                execution: self.profile(node, definition, index)?,
                id: node.id.clone(),
                inputs: node
                    .inputs
                    .iter()
                    .map(|input| GraphInputBinding {
                        name: input.name.clone(),
                        output_name: input.output_name.clone(),
                        producer_node_id: input.producer_node_id.clone(),
                    })
                    .collect(),
                objective: node.objective.clone(),
                outputs: node
                    .outputs
                    .iter()
                    .map(|output| GraphOutput {
                        name: output.name.clone(),
                        schema: output.schema,
                    })
                    .collect(),
                request: request.clone(),
            });
        }
        let graph = ExecutionGraph {
            id: self.graph_id.into(),
            nodes,
            revision: NonZeroU64::MIN,
            session_id: self.session_id.clone(),
        };
        graph.validate().map_err(|error| error.to_string())?;

        Ok(graph)
    }

    fn profile(
        &self,
        node: &GraphProposalNode,
        definition: &AgentDefinition,
        index: usize,
    ) -> Result<GraphExecutionProfile, String> {
        let execution = &node.execution;
        let workspace_access = match execution.workspace_access {
            GraphProposalWorkspaceAccess::SharedRead => GraphWorkspaceAccess::SharedRead,
            GraphProposalWorkspaceAccess::ExclusiveWrite => GraphWorkspaceAccess::ExclusiveWrite,
            GraphProposalWorkspaceAccess::BackgroundRead => {
                return Err("Background graph readers are unsupported".into());
            }
        };
        if workspace_access == GraphWorkspaceAccess::ExclusiveWrite
            && (definition.read_only || self.permission == PermissionMode::ReadOnly)
        {
            return Err("Graph proposal requests access beyond host or agent permissions".into());
        }
        let context = match execution.context {
            GraphProposalContext::Fresh => {
                if workspace_access != GraphWorkspaceAccess::SharedRead
                    || !matches!(
                        execution.response_profile,
                        ProtocolRequestProfile::UtilityPrompt
                            | ProtocolRequestProfile::FocusedReview
                            | ProtocolRequestProfile::ReviewMetadata
                    )
                {
                    return Err("Unsupported graph utility profile".into());
                }
                GraphContext::Fresh
            }
            GraphProposalContext::TaskConversation => GraphContext::TaskConversation {
                task_id: format!("{}:task:{index}", self.graph_id),
            },
        };

        Ok(GraphExecutionProfile {
            context,
            post_processing: match execution.post_processing {
                GraphProposalPostProcessing::None => GraphPostProcessing::None,
                GraphProposalPostProcessing::SessionWorkflow => {
                    GraphPostProcessing::SessionWorkflow
                }
            },
            response_profile: execution.response_profile,
            shared_budget_id: Some(format!("{}:calls", self.graph_id)),
            workspace_access,
        })
    }
}

#[cfg(test)]
#[path = "proposal_test.rs"]
mod tests;
