use ag_protocol::ProtocolRequestProfile;

use crate::execution_graph::test_support::{graph, result};
use crate::{
    AgentKind, AgentModel, AgentSelection, ExecutionGraph, GraphCompletionGate, GraphContext,
    GraphExecutionRef, GraphPostProcessing, GraphResolvedInput, GraphResult, GraphResultState,
    GraphWorkspaceAccess,
};

#[test]
fn graph_snapshots_round_trip_all_execution_profiles_and_exact_references() {
    // Arrange
    let mut graph = graph();
    for profile in [
        ProtocolRequestProfile::FocusedReview,
        ProtocolRequestProfile::UtilityPrompt,
        ProtocolRequestProfile::ReviewMetadata,
        ProtocolRequestProfile::SessionTurn,
    ] {
        graph.nodes[0].execution.response_profile = profile;
        graph.nodes[0].execution.workspace_access = GraphWorkspaceAccess::ExclusiveWrite;
        graph.nodes[0].execution.post_processing = GraphPostProcessing::SessionWorkflow;
        graph.nodes[0].execution.context = GraphContext::TaskConversation {
            task_id: "task".into(),
        };
        graph.nodes[1].dependencies[0].gate = GraphCompletionGate::ExecutionSucceeded;

        // Act
        let decoded: ExecutionGraph =
            serde_json::from_value(serde_json::json!(graph)).expect("graph snapshot");

        // Assert
        assert_eq!(decoded, graph);
    }
    for execution in [
        GraphExecutionRef::UtilityRun {
            run_id: "run".into(),
        },
        GraphExecutionRef::TaskAttempt {
            attempt_id: "attempt".into(),
            operation_id: "operation".into(),
            task_id: "task".into(),
        },
    ] {
        for state in [
            GraphResultState::Produced,
            GraphResultState::Validated,
            GraphResultState::Accepted,
        ] {
            let mut result = result();
            result.reference.execution = execution.clone();
            result.state = state;
            let input = GraphResolvedInput {
                name: "input".into(),
                result: result.reference.clone(),
            };
            let decoded: GraphResult =
                serde_json::from_value(serde_json::json!(result)).expect("result snapshot");
            let decoded_input: GraphResolvedInput =
                serde_json::from_value(serde_json::json!(input)).expect("input snapshot");
            assert_eq!(decoded, result);
            assert_eq!(decoded_input, input);
        }
    }
}

#[test]
fn graph_selection_decoding_rejects_unknown_or_incoherent_provider_model_pairs() {
    // Arrange
    let mut value = serde_json::json!(graph());

    // Act / Assert
    for pair in [
        ("unknown", "gpt-6.1-sol"),
        ("Codex", "unknown"),
        ("Claude", "gpt-6.1-sol"),
    ] {
        value["nodes"][0]["agent"] = serde_json::json!(pair);
        assert!(serde_json::from_value::<ExecutionGraph>(value.clone()).is_err());
    }
    let mut snapshot = graph();
    snapshot.nodes[0].agent = AgentSelection::new(AgentKind::Harness, AgentModel::Gpt61Sol);
    let decoded: ExecutionGraph =
        serde_json::from_value(serde_json::json!(snapshot)).expect("harness snapshot");
    assert_eq!(decoded, snapshot);
}
