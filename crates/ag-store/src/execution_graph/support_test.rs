use std::num::NonZeroU64;

use ag_protocol::{ProtocolRequestProfile, TurnPrompt};
use ag_session::{
    AgentDefinition, AgentKind, AgentModel, AgentSelection, EnvId, ExecutionGraph,
    GraphCompletionGate, GraphContext, GraphDependency, GraphExecutionProfile, GraphExecutionRef,
    GraphInputBinding, GraphNode, GraphOutput, GraphPostProcessing, GraphResultRef,
    GraphWorkspaceAccess,
};
use ag_worker::{RunInfo, RunState};

use crate::{Database, GraphEvidence, GraphExecutionRecord};

pub(super) fn graph() -> ExecutionGraph {
    let first = reader("first");
    let mut second = reader("second");
    second.dependencies.push(GraphDependency {
        gate: GraphCompletionGate::ResultAccepted {
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

pub(super) fn reader(id: &str) -> GraphNode {
    GraphNode {
        agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
        definition: AgentDefinition {
            description: "Review".into(),
            id: "reviewer".into(),
            name: "Reviewer".into(),
            prompt: "Read evidence".into(),
            read_only: true,
        },
        dependencies: Vec::new(),
        env_id: EnvId::SessionWorktree("session".into()),
        execution: GraphExecutionProfile {
            context: GraphContext::Fresh,
            post_processing: GraphPostProcessing::None,
            response_profile: ProtocolRequestProfile::UtilityPrompt,
            shared_budget_id: Some("budget".into()),
            workspace_access: GraphWorkspaceAccess::SharedRead,
        },
        id: id.into(),
        inputs: Vec::new(),
        objective: "Review".into(),
        outputs: vec![GraphOutput {
            name: "response".into(),
            schema: ProtocolRequestProfile::UtilityPrompt,
        }],
        request: TurnPrompt::from_agent_data("Captured evidence".into()),
    }
}

pub(super) fn record(run_id: &str) -> GraphExecutionRecord {
    GraphExecutionRecord {
        execution: GraphExecutionRef::UtilityRun {
            run_id: run_id.into(),
        },
        graph_id: "graph".into(),
        graph_revision: NonZeroU64::MIN,
        inputs: Vec::new(),
        node_id: "first".into(),
    }
}

pub(super) fn reference() -> GraphResultRef {
    GraphResultRef {
        execution: record("run").execution,
        graph_id: "graph".into(),
        graph_revision: NonZeroU64::MIN,
        id: "result".into(),
        node_id: "first".into(),
        output_name: "response".into(),
        schema: ProtocolRequestProfile::UtilityPrompt,
        version: NonZeroU64::MIN,
    }
}

pub(super) fn evidence() -> GraphEvidence {
    GraphEvidence {
        actor: "host".into(),
        reason: "Verified exact output".into(),
    }
}

pub(super) async fn run(database: &Database, id: &str, session: Option<&str>, state: RunState) {
    database
        .runs()
        .create(&RunInfo {
            folder: "workspace".into(),
            id: id.into(),
            parent_id: None,
            project_id: None,
            purpose: "graph fixture".into(),
            session_id: session.map(str::to_string),
        })
        .await
        .expect("run");
    if state != RunState::Queued {
        database
            .runs()
            .transition(id, state, None)
            .await
            .expect("settle");
    }
}

pub(super) async fn produced() -> Database {
    let database = crate::test_support::task_database_fixture().await;
    database
        .graphs()
        .save_graph(&graph(), None)
        .await
        .expect("graph");
    run(&database, "run", Some("session"), RunState::Completed).await;
    database
        .graphs()
        .link_execution(&record("run"))
        .await
        .expect("link");
    database
        .graphs()
        .produce_result(&reference(), "{\"answer\":\"original\"}")
        .await
        .expect("output");

    database
}
