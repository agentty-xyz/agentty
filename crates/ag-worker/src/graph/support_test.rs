use std::num::NonZeroU64;

use ag_protocol::{ProtocolRequestProfile, TurnPrompt};
use ag_session::{
    AgentDefinition, AgentKind, AgentModel, AgentSelection, EnvId, ExecutionGraph, GraphContext,
    GraphExecutionProfile, GraphExecutionRef, GraphNode, GraphPostProcessing, GraphWorkspaceAccess,
};

use crate::graph::{GraphDispatch, GraphExecutionRecord};

pub(super) fn graph() -> ExecutionGraph {
    ExecutionGraph {
        id: "graph".into(),
        nodes: vec![dispatch().node],
        revision: NonZeroU64::MIN,
        session_id: "session".into(),
    }
}

pub(super) fn dispatch() -> GraphDispatch {
    GraphDispatch {
        inputs: Vec::new(),
        node: GraphNode {
            agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
            definition: AgentDefinition {
                description: "Read evidence".into(),
                id: "reader".into(),
                name: "Reader".into(),
                prompt: "Read evidence".into(),
                read_only: true,
            },
            dependencies: Vec::new(),
            env_id: EnvId::SessionWorktree("session".into()),
            execution: GraphExecutionProfile {
                context: GraphContext::Fresh,
                post_processing: GraphPostProcessing::None,
                response_profile: ProtocolRequestProfile::UtilityPrompt,
                shared_budget_id: None,
                workspace_access: GraphWorkspaceAccess::SharedRead,
            },
            id: "reader".into(),
            inputs: Vec::new(),
            objective: "Review".into(),
            outputs: Vec::new(),
            request: TurnPrompt::from_agent_data("Evidence".into()),
        },
        record: GraphExecutionRecord {
            execution: GraphExecutionRef::UtilityRun {
                run_id: "run".into(),
            },
            graph_id: "graph".into(),
            graph_revision: NonZeroU64::MIN,
            inputs: Vec::new(),
            node_id: "reader".into(),
        },
    }
}
