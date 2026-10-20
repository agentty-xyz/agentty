use ag_session::{
    AcceptedTaskPlan, AgentKind, AgentModel, AgentSelection, BuiltinAgentDefinition,
    StartPlanThenImplementRequest, TaskChain, TaskChainPhase,
};

use crate::domain::session::SessionId;
use crate::presentation::task_chain_graph::{
    ACCEPTED_PLAN_EDGE_LABEL, TASK_CHAIN_GRAPH_CACHE_LIMIT, TaskChainGraph, TaskChainGraphCache,
    TaskChainNodeStatus,
};

#[test]
fn projection_maps_every_chain_phase_to_node_statuses_and_edge() {
    // Arrange
    let cases = [
        (
            TaskChainPhase::Planning,
            BuiltinAgentDefinition::Planner,
            TaskChainNodeStatus::Working,
            TaskChainNodeStatus::Waiting,
        ),
        (
            TaskChainPhase::AwaitingPlanAcceptance,
            BuiltinAgentDefinition::Planner,
            TaskChainNodeStatus::AwaitingApproval,
            TaskChainNodeStatus::Waiting,
        ),
        (
            TaskChainPhase::PlannerQuestion,
            BuiltinAgentDefinition::Planner,
            TaskChainNodeStatus::NeedsInput,
            TaskChainNodeStatus::Waiting,
        ),
        (
            TaskChainPhase::Implementing,
            BuiltinAgentDefinition::Implementer,
            TaskChainNodeStatus::Done,
            TaskChainNodeStatus::Working,
        ),
        (
            TaskChainPhase::ImplementerQuestion,
            BuiltinAgentDefinition::Implementer,
            TaskChainNodeStatus::Done,
            TaskChainNodeStatus::NeedsInput,
        ),
        (
            TaskChainPhase::Failed,
            BuiltinAgentDefinition::Planner,
            TaskChainNodeStatus::Failed,
            TaskChainNodeStatus::Waiting,
        ),
        (
            TaskChainPhase::Failed,
            BuiltinAgentDefinition::Implementer,
            TaskChainNodeStatus::Done,
            TaskChainNodeStatus::Failed,
        ),
        (
            TaskChainPhase::Canceled,
            BuiltinAgentDefinition::Planner,
            TaskChainNodeStatus::Canceled,
            TaskChainNodeStatus::Waiting,
        ),
        (
            TaskChainPhase::Canceled,
            BuiltinAgentDefinition::Implementer,
            TaskChainNodeStatus::Done,
            TaskChainNodeStatus::Canceled,
        ),
        (
            TaskChainPhase::Succeeded,
            BuiltinAgentDefinition::Implementer,
            TaskChainNodeStatus::Done,
            TaskChainNodeStatus::Done,
        ),
    ];

    for (phase, current_role, planner, implementer) in cases {
        let chain = chain(phase, current_role);

        // Act
        let graph = TaskChainGraph::from_chain(&chain);

        // Assert
        assert_eq!(graph.chain_id, "chain");
        assert_eq!(
            graph
                .nodes
                .iter()
                .map(|node| (node.agent, node.status))
                .collect::<Vec<_>>(),
            [
                (BuiltinAgentDefinition::Planner, planner),
                (BuiltinAgentDefinition::Implementer, implementer),
            ],
            "{phase:?} with current role {current_role:?}"
        );
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].label, ACCEPTED_PLAN_EDGE_LABEL);
        assert_eq!(
            graph.edges[0].is_satisfied,
            current_role == BuiltinAgentDefinition::Implementer
        );
    }
}

#[test]
fn node_status_labels_match_user_vocabulary() {
    // Arrange
    let statuses = [
        TaskChainNodeStatus::Waiting,
        TaskChainNodeStatus::Working,
        TaskChainNodeStatus::NeedsInput,
        TaskChainNodeStatus::AwaitingApproval,
        TaskChainNodeStatus::Done,
        TaskChainNodeStatus::Failed,
        TaskChainNodeStatus::Canceled,
    ];

    // Act
    let labels = statuses.map(TaskChainNodeStatus::label);

    // Assert
    assert_eq!(
        labels,
        [
            "Waiting",
            "Working",
            "Needs your input",
            "Awaiting your approval",
            "Done",
            "Failed",
            "Canceled",
        ]
    );
}

#[test]
fn cache_reloads_on_version_change_and_ignores_stale_completions() {
    // Arrange
    let session_id = SessionId::from("session");
    let graph = TaskChainGraph::from_chain(&chain(
        TaskChainPhase::Planning,
        BuiltinAgentDefinition::Planner,
    ));
    let mut cache = TaskChainGraphCache::default();

    // Act / Assert: the first version loads once while in flight, ignoring
    // stale completions.
    assert!(cache.needs_load("session", 1));
    cache.begin_load(&session_id, 1);
    assert!(!cache.needs_load("session", 2));
    cache.complete_load("session", 0, Ok(Some(graph.clone())));
    assert_eq!(cache.graph("session"), None);
    assert!(!cache.needs_load("session", 2));
    cache.complete_load("session", 1, Ok(Some(graph.clone())));
    assert_eq!(cache.graph("session"), Some(&graph));
    assert!(!cache.needs_load("session", 1));

    // Act / Assert: a newer version keeps the old graph until a load
    // succeeds, and a failed load retains it.
    assert!(cache.needs_load("session", 2));
    cache.begin_load(&session_id, 2);
    assert_eq!(cache.graph("session"), Some(&graph));
    cache.complete_load("session", 2, Err("database closed".into()));
    assert_eq!(cache.graph("session"), Some(&graph));
    cache.begin_load(&session_id, 3);
    cache.complete_load("session", 3, Ok(None));
    assert_eq!(cache.graph("session"), None);
    assert_eq!(cache.graph("missing"), None);
}

#[test]
fn cache_retains_only_the_most_recent_sessions() {
    // Arrange
    let mut cache = TaskChainGraphCache::default();

    // Act
    for index in 0..=TASK_CHAIN_GRAPH_CACHE_LIMIT {
        cache.begin_load(&SessionId::from(format!("session-{index}")), 1);
    }

    // Assert
    assert!(cache.needs_load("session-0", 1));
    assert!(!cache.needs_load("session-1", 1));
}

fn chain(phase: TaskChainPhase, current_role: BuiltinAgentDefinition) -> TaskChain {
    let start = StartPlanThenImplementRequest {
        agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
        chain_id: "chain".into(),
        planner_attempt_id: "plan".into(),
        user_request: "Add search".into(),
    };
    let mut current_request = start.planner_request();
    current_request.assignment.definition = current_role;
    let accepted_plan =
        (current_role == BuiltinAgentDefinition::Implementer).then(|| AcceptedTaskPlan {
            attempt_id: "plan".into(),
            task_id: current_request.task_id.clone(),
            text: "Plan".into(),
        });

    TaskChain {
        accepted_plan,
        agent: start.agent,
        current_request: current_request.clone(),
        id: start.chain_id.clone(),
        implementer_task_id: start.implementer_task_id(),
        phase,
        planner_task_id: current_request.task_id,
        session_id: "session".into(),
        user_request: start.user_request,
    }
}
