use std::num::NonZeroU64;

use ag_protocol::{ProtocolRequestProfile, TurnPrompt};

use crate::test_support::codex_selection;
use crate::{
    AgentDefinition, EnvId, ExecutionGraph, GraphCompletionGate, GraphContext, GraphDependency,
    GraphExecutionProfile, GraphExecutionRef, GraphInputBinding, GraphNode, GraphOutput,
    GraphPostProcessing, GraphResult, GraphResultRef, GraphResultState, GraphWorkspaceAccess,
};

pub(super) fn reader(id: &str) -> GraphNode {
    GraphNode {
        agent: codex_selection(),
        definition: AgentDefinition {
            description: "Review captured evidence".into(),
            id: "reviewer".into(),
            name: "Reviewer".into(),
            prompt: "Review the supplied diff".into(),
            read_only: true,
        },
        dependencies: Vec::new(),
        env_id: EnvId::SessionWorktree("session".into()),
        execution: GraphExecutionProfile {
            context: GraphContext::Fresh,
            post_processing: GraphPostProcessing::None,
            response_profile: ProtocolRequestProfile::FocusedReview,
            shared_budget_id: Some("review-budget".into()),
            workspace_access: GraphWorkspaceAccess::SharedRead,
        },
        id: id.into(),
        inputs: Vec::new(),
        objective: "Review the change".into(),
        outputs: vec![GraphOutput {
            name: "response".into(),
            schema: ProtocolRequestProfile::FocusedReview,
        }],
        request: TurnPrompt::from_agent_data("Review this captured diff: +changed line".into()),
    }
}

pub(super) fn graph() -> ExecutionGraph {
    let first = reader("first");
    let mut second = reader("second");
    second.dependencies.push(GraphDependency {
        gate: GraphCompletionGate::ResultValidated {
            output_name: "response".into(),
        },
        node_id: first.id.clone(),
    });
    second.inputs.push(GraphInputBinding {
        name: "evidence".into(),
        output_name: "response".into(),
        producer_node_id: first.id.clone(),
    });

    ExecutionGraph {
        id: "graph".into(),
        nodes: vec![first, second],
        revision: NonZeroU64::MIN,
        session_id: "session".into(),
    }
}

pub(super) fn result() -> GraphResult {
    GraphResult {
        reference: GraphResultRef {
            execution: GraphExecutionRef::UtilityRun {
                run_id: "run".into(),
            },
            graph_id: "graph".into(),
            graph_revision: NonZeroU64::MIN,
            id: "result".into(),
            node_id: "first".into(),
            output_name: "response".into(),
            schema: ProtocolRequestProfile::FocusedReview,
            version: NonZeroU64::MIN,
        },
        state: GraphResultState::Validated,
    }
}
