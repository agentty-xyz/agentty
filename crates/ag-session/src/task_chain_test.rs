use std::num::NonZeroU64;

use ag_protocol::{ProtocolRequestProfile, TurnPromptAttachment, TurnPromptTextSource};

use crate::test_support::codex_selection;
use crate::{
    AcceptedTaskPlan, EnvId, ExecutionGraph, ExecutionGraphProgress, ExecutionGraphStatus,
    GraphCompletionGate, GraphContext, GraphExecutionRef, GraphNodeProgress, GraphNodeStatus,
    GraphPostProcessing, GraphWorkspaceAccess, PlanThenImplementAgents, SessionError, SessionId,
    StartPlanThenImplementRequest, TASK_CHAIN_PLAN_OUTPUT, TaskAgentRole, TaskChain,
    TaskChainPhase, TaskChainReplyOutcome,
};

fn start_request() -> StartPlanThenImplementRequest {
    StartPlanThenImplementRequest {
        agent: codex_selection(),
        agents: Box::new(PlanThenImplementAgents::bundled().expect("bundled agents")),
        attachments: vec![TurnPromptAttachment {
            local_image_path: "original.png".into(),
            placeholder: "[Image #1]".into(),
        }],
        chain_id: "chain".into(),
        user_request: "  Original request.\n".into(),
    }
}

fn chain_graph() -> ExecutionGraph {
    start_request().graph(&SessionId::from("session"), Some("User: earlier"))
}

fn node(node_id: &str, status: GraphNodeStatus, attempts: &[&str]) -> GraphNodeProgress {
    GraphNodeProgress {
        blocked: Vec::new(),
        executions: attempts
            .iter()
            .map(|attempt| GraphExecutionRef::TaskAttempt {
                attempt_id: (*attempt).into(),
                operation_id: format!("task-attempt:{attempt}"),
                task_id: format!("chain:chain:{node_id}"),
            })
            .collect(),
        last_error: None,
        node_id: node_id.into(),
        questions: Vec::new(),
        results: Vec::new(),
        status,
    }
}

fn progress(
    status: ExecutionGraphStatus,
    planner: GraphNodeProgress,
    implementer: GraphNodeProgress,
) -> ExecutionGraphProgress {
    ExecutionGraphProgress {
        graph_id: "chain".into(),
        nodes: vec![planner, implementer],
        revision: NonZeroU64::MIN,
        session_id: "session".into(),
        status,
    }
}

fn accepted_plan() -> AcceptedTaskPlan {
    AcceptedTaskPlan {
        attempt_id: "plan-2".into(),
        task_id: "chain:chain:planner".into(),
        text: "Plan".into(),
    }
}

#[test]
fn launch_builds_a_valid_gated_two_node_graph() {
    // Arrange
    let request = start_request();

    // Act
    let graph = chain_graph();

    // Assert
    assert_eq!(graph.validate(), Ok(()));
    assert_eq!(graph.id, "chain");
    assert_eq!(graph.revision, NonZeroU64::MIN);
    assert_eq!(graph.nodes.len(), 2);
    let planner = graph.nodes.first().expect("planner");
    let implementer = graph.nodes.get(1).expect("implementer");
    assert_eq!(
        planner.execution.context,
        GraphContext::TaskConversation {
            task_id: "chain:chain:planner".into()
        }
    );
    assert_eq!(
        planner.execution.workspace_access,
        GraphWorkspaceAccess::SharedRead
    );
    assert_eq!(planner.execution.post_processing, GraphPostProcessing::None);
    assert_eq!(planner.outputs[0].name, TASK_CHAIN_PLAN_OUTPUT);
    assert_eq!(
        planner.outputs[0].schema,
        ProtocolRequestProfile::SessionTurn
    );
    assert_eq!(planner.definition, request.agents.planner);
    assert_eq!(planner.agent, request.agent);
    assert_eq!(planner.objective, request.user_request);
    assert_eq!(planner.env_id, EnvId::SessionWorktree("session".into()));
    assert_eq!(planner.request.attachments, request.attachments);
    assert_eq!(planner.request.text_source, TurnPromptTextSource::AgentData);
    assert!(planner.request.text.contains("Original request."));
    assert!(
        planner
            .request
            .text
            .contains("Session conversation at workflow launch")
    );
    assert_eq!(
        implementer.execution.workspace_access,
        GraphWorkspaceAccess::ExclusiveWrite
    );
    assert_eq!(
        implementer.execution.post_processing,
        GraphPostProcessing::SessionWorkflow
    );
    assert_eq!(implementer.definition, request.agents.implementer);
    assert_eq!(
        implementer.dependencies[0].gate,
        GraphCompletionGate::ResultAccepted {
            output_name: TASK_CHAIN_PLAN_OUTPUT.into()
        }
    );
    assert_eq!(implementer.inputs[0].producer_node_id, planner.id);
    assert!(!implementer.request.text.contains("Session conversation"));
    assert_eq!(implementer.request.attachments, request.attachments);
}

#[test]
fn roles_are_recognized_only_for_their_chain_tasks() {
    // Arrange
    let graph = chain_graph();
    let mut foreign = graph.nodes[0].clone();
    foreign.execution.context = GraphContext::TaskConversation {
        task_id: "other".into(),
    };
    let mut fresh = graph.nodes[1].clone();
    fresh.execution.context = GraphContext::Fresh;

    // Act
    let roles = [
        TaskChain::role("chain", &graph.nodes[0]),
        TaskChain::role("chain", &graph.nodes[1]),
        TaskChain::role("other", &graph.nodes[0]),
        TaskChain::role("chain", &foreign),
        TaskChain::role("chain", &fresh),
    ];

    // Assert
    assert_eq!(
        roles,
        [
            Some(TaskAgentRole::Planner),
            Some(TaskAgentRole::Implementer),
            None,
            None,
            None,
        ]
    );
}

#[test]
fn projection_follows_the_planner_until_a_plan_is_accepted() {
    // Arrange
    let graph = chain_graph();
    let cases = [
        (GraphNodeStatus::Pending, &[][..], TaskChainPhase::Planning),
        (
            GraphNodeStatus::Running,
            &["plan-1"][..],
            TaskChainPhase::Planning,
        ),
        (
            GraphNodeStatus::AwaitingAnswers,
            &["plan-1"][..],
            TaskChainPhase::PlannerQuestion,
        ),
        (
            GraphNodeStatus::Succeeded,
            &["plan-1"][..],
            TaskChainPhase::AwaitingPlanAcceptance,
        ),
        (
            GraphNodeStatus::Failed,
            &["plan-1"][..],
            TaskChainPhase::Failed,
        ),
        (
            GraphNodeStatus::Cancelled,
            &["plan-1"][..],
            TaskChainPhase::Canceled,
        ),
    ];

    for (status, attempts, expected) in cases {
        // Act
        let chain = TaskChain::project(
            &graph,
            &progress(
                ExecutionGraphStatus::Blocked,
                node("planner", status, attempts),
                node("implementer", GraphNodeStatus::Pending, &[]),
            ),
            None,
        )
        .expect("chain graph");

        // Assert
        assert_eq!(chain.phase, expected, "{status:?}");
        assert_eq!(chain.current_role, TaskAgentRole::Planner);
        assert_eq!(chain.current_attempt_id(), attempts.last().copied());
        assert_eq!(chain.planner_task_id, "chain:chain:planner");
        assert_eq!(chain.implementer_task_id, "chain:chain:implementer");
        assert_eq!(chain.user_request, "  Original request.\n");
    }
}

#[test]
fn projection_follows_the_implementer_after_acceptance() {
    // Arrange
    let graph = chain_graph();
    let cases = [
        (
            GraphNodeStatus::Pending,
            &[][..],
            TaskChainPhase::Implementing,
        ),
        (
            GraphNodeStatus::Queued,
            &["impl-1"][..],
            TaskChainPhase::Implementing,
        ),
        (
            GraphNodeStatus::AwaitingAnswers,
            &["impl-1"][..],
            TaskChainPhase::ImplementerQuestion,
        ),
        (
            GraphNodeStatus::Succeeded,
            &["impl-1"][..],
            TaskChainPhase::Succeeded,
        ),
        (
            GraphNodeStatus::Failed,
            &["impl-1"][..],
            TaskChainPhase::Failed,
        ),
    ];

    for (status, attempts, expected) in cases {
        // Act
        let chain = TaskChain::project(
            &graph,
            &progress(
                ExecutionGraphStatus::Running,
                node("planner", GraphNodeStatus::Succeeded, &["plan-2"]),
                node("implementer", status, attempts),
            ),
            Some(accepted_plan()),
        )
        .expect("chain graph");

        // Assert
        assert_eq!(chain.phase, expected, "{status:?}");
        assert_eq!(chain.current_role, TaskAgentRole::Implementer);
        assert_eq!(chain.current_attempt_id(), attempts.last().copied());
        assert_eq!(chain.accepted_plan, Some(accepted_plan()));
    }
}

#[test]
fn projection_reports_graph_cancellation_and_rejects_other_shapes() {
    // Arrange
    let graph = chain_graph();
    let cancelled = progress(
        ExecutionGraphStatus::Cancelled,
        node("planner", GraphNodeStatus::Succeeded, &["plan-1"]),
        node("implementer", GraphNodeStatus::Cancelled, &[]),
    );
    let mut reordered = graph.clone();
    reordered.nodes.reverse();
    let mut single = graph.clone();
    single.nodes.truncate(1);
    let mut incomplete = cancelled.clone();
    incomplete.nodes.pop();

    // Act
    let phase = TaskChain::project(&graph, &cancelled, None).map(|chain| chain.phase);
    let reordered = TaskChain::project(&reordered, &cancelled, None);
    let single = TaskChain::project(&single, &cancelled, None);
    let incomplete = TaskChain::project(&graph, &incomplete, None);
    let mut chain = TaskChain::project(&graph, &cancelled, None).expect("chain");
    chain.current_execution = Some(GraphExecutionRef::UtilityRun {
        run_id: "utility".into(),
    });
    let utility_attempt = chain.current_attempt_id();

    // Assert
    assert_eq!(phase, Some(TaskChainPhase::Canceled));
    assert_eq!(reordered, None);
    assert_eq!(single, None);
    assert_eq!(incomplete, None);
    assert_eq!(utility_attempt, None);
}

#[test]
fn reply_outcomes_round_trip_their_persisted_identities() {
    // Arrange
    let outcomes = [
        TaskChainReplyOutcome::PlanAccepted,
        TaskChainReplyOutcome::PlanRevised,
        TaskChainReplyOutcome::Continued,
    ];

    // Act
    let decoded = outcomes.map(|outcome| TaskChainReplyOutcome::from_id(outcome.id()));

    // Assert
    assert_eq!(decoded, outcomes.map(Some));
    assert_eq!(TaskChainReplyOutcome::from_id("unknown"), None);
}

#[test]
fn task_chain_reply_errors_explain_how_to_proceed() {
    // Arrange
    let busy = SessionError::TaskChainBusy(TaskChainPhase::Planning);

    // Act
    let messages = [
        busy.to_string(),
        SessionError::TaskChainNotFound.to_string(),
    ];

    // Assert
    assert_eq!(
        messages,
        [
            "The task chain agent is still working; wait for it to finish before replying"
                .to_string(),
            "Session has no task chain".to_string(),
        ]
    );
}
