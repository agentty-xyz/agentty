use std::num::NonZeroU64;
use std::time::Instant;

use ag_session::{
    AcceptedTaskPlan, AgentKind, AgentModel, AgentSelection, GraphExecutionRef,
    StartPlanThenImplementRequest, TaskAgentRole, TaskChain, TaskChainPhase,
};

use crate::domain::session::SessionId;
use crate::presentation::task_chain_graph::{
    ACCEPTED_PLAN_EDGE_LABEL, TASK_CHAIN_GRAPH_CACHE_LIMIT, TASK_CHAIN_GRAPH_RETRY_DELAY,
    TaskChainGraph, TaskChainGraphCache, TaskChainNodeStatus,
};

#[test]
fn projection_maps_every_chain_phase_to_node_statuses_and_edge() {
    // Arrange
    let cases = [
        (
            TaskChainPhase::Planning,
            TaskAgentRole::Planner,
            TaskChainNodeStatus::Working,
            TaskChainNodeStatus::Waiting,
        ),
        (
            TaskChainPhase::AwaitingPlanAcceptance,
            TaskAgentRole::Planner,
            TaskChainNodeStatus::AwaitingApproval,
            TaskChainNodeStatus::Waiting,
        ),
        (
            TaskChainPhase::PlannerQuestion,
            TaskAgentRole::Planner,
            TaskChainNodeStatus::NeedsInput,
            TaskChainNodeStatus::Waiting,
        ),
        (
            TaskChainPhase::Implementing,
            TaskAgentRole::Implementer,
            TaskChainNodeStatus::Done,
            TaskChainNodeStatus::Working,
        ),
        (
            TaskChainPhase::ImplementerQuestion,
            TaskAgentRole::Implementer,
            TaskChainNodeStatus::Done,
            TaskChainNodeStatus::NeedsInput,
        ),
        (
            TaskChainPhase::Failed,
            TaskAgentRole::Planner,
            TaskChainNodeStatus::Failed,
            TaskChainNodeStatus::Waiting,
        ),
        (
            TaskChainPhase::Failed,
            TaskAgentRole::Implementer,
            TaskChainNodeStatus::Done,
            TaskChainNodeStatus::Failed,
        ),
        (
            TaskChainPhase::Canceled,
            TaskAgentRole::Planner,
            TaskChainNodeStatus::Canceled,
            TaskChainNodeStatus::Waiting,
        ),
        (
            TaskChainPhase::Canceled,
            TaskAgentRole::Implementer,
            TaskChainNodeStatus::Done,
            TaskChainNodeStatus::Canceled,
        ),
        (
            TaskChainPhase::Succeeded,
            TaskAgentRole::Implementer,
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
                (TaskAgentRole::Planner, planner),
                (TaskAgentRole::Implementer, implementer),
            ],
            "{phase:?} with current role {current_role:?}"
        );
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].label, ACCEPTED_PLAN_EDGE_LABEL);
        assert_eq!(
            graph.edges[0].is_satisfied,
            current_role == TaskAgentRole::Implementer
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
    let now = Instant::now();
    let session_id = SessionId::from("session");
    let graph =
        TaskChainGraph::from_chain(&chain(TaskChainPhase::Planning, TaskAgentRole::Planner));
    let mut cache = TaskChainGraphCache::default();

    // Act / Assert: the first version loads once while in flight, ignoring
    // stale completions.
    assert!(cache.needs_load("session", 1, now));
    cache.begin_load(&session_id, 1);
    assert!(!cache.needs_load("session", 2, now));
    cache.complete_load("session", 0, Ok(Some(graph.clone())), now);
    assert_eq!(cache.graph("session"), None);
    assert!(!cache.needs_load("session", 2, now));
    cache.complete_load("session", 1, Ok(Some(graph.clone())), now);
    assert_eq!(cache.graph("session"), Some(&graph));
    assert!(!cache.needs_load("session", 1, now));

    // Act / Assert: a newer version keeps the old graph until a load
    // succeeds, and a failed load retains it.
    assert!(cache.needs_load("session", 2, now));
    cache.begin_load(&session_id, 2);
    assert_eq!(cache.graph("session"), Some(&graph));
    cache.complete_load("session", 2, Err("database closed".into()), now);
    assert_eq!(cache.graph("session"), Some(&graph));
    assert!(!cache.needs_load("session", 2, now));
    let retry_at = now + TASK_CHAIN_GRAPH_RETRY_DELAY;
    assert!(!cache.needs_load("session", 2, now + TASK_CHAIN_GRAPH_RETRY_DELAY / 2));
    assert!(cache.needs_load("session", 2, retry_at));
    cache.begin_load(&session_id, 2);
    assert!(!cache.needs_load("session", 2, retry_at));
    cache.complete_load("session", 2, Ok(Some(graph)), retry_at);
    assert!(!cache.needs_load("session", 2, retry_at + TASK_CHAIN_GRAPH_RETRY_DELAY));

    // Act / Assert: a newer update can load during the retry delay.
    cache.begin_load(&session_id, 2);
    cache.complete_load("session", 2, Err("database busy".into()), retry_at);
    assert!(cache.needs_load("session", 3, retry_at));
    cache.begin_load(&session_id, 3);
    cache.complete_load("session", 3, Ok(None), retry_at);
    assert_eq!(cache.graph("session"), None);
    assert_eq!(cache.graph("missing"), None);
}

#[test]
fn cache_retains_only_the_most_recent_sessions() {
    // Arrange
    let now = Instant::now();
    let mut cache = TaskChainGraphCache::default();

    // Act
    for index in 0..=TASK_CHAIN_GRAPH_CACHE_LIMIT {
        cache.begin_load(&SessionId::from(format!("session-{index}")), 1);
    }

    // Assert
    assert!(cache.needs_load("session-0", 1, now));
    assert!(!cache.needs_load("session-1", 1, now));
}

fn chain(phase: TaskChainPhase, current_role: TaskAgentRole) -> TaskChain {
    let start = StartPlanThenImplementRequest {
        agents: Box::new(ag_session::PlanThenImplementAgents::bundled().expect("bundled agents")),
        attachments: Vec::new(),
        agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
        chain_id: "chain".into(),
        user_request: "Add search".into(),
    };
    let accepted_plan = (current_role == TaskAgentRole::Implementer).then(|| AcceptedTaskPlan {
        attempt_id: "plan".into(),
        task_id: TaskChain::task_id(&start.chain_id, TaskAgentRole::Planner),
        text: "Plan".into(),
    });

    TaskChain {
        agents: ag_session::PlanThenImplementAgents::bundled().expect("bundled agents"),
        accepted_plan,
        agent: start.agent,
        current_execution: Some(GraphExecutionRef::TaskAttempt {
            attempt_id: "attempt".into(),
            operation_id: "task-attempt:attempt".into(),
            task_id: TaskChain::task_id(&start.chain_id, current_role),
        }),
        current_role,
        id: start.chain_id.clone(),
        implementer_task_id: TaskChain::task_id(&start.chain_id, TaskAgentRole::Implementer),
        phase,
        planner_task_id: TaskChain::task_id(&start.chain_id, TaskAgentRole::Planner),
        revision: NonZeroU64::MIN,
        session_id: "session".into(),
        user_request: start.user_request,
    }
}
