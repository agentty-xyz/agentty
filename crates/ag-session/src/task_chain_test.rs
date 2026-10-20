use crate::test_support::codex_selection;
use crate::{
    ContinueTaskChainRequest, DurableMessageVisibility, SessionId, StartPlanThenImplementRequest,
    TaskChain, TaskChainPhase,
};

#[test]
fn workflow_requests_capture_selection_and_keep_continuations_task_local() {
    // Arrange
    let start = StartPlanThenImplementRequest {
        agent: codex_selection(),
        chain_id: "chain".into(),
        planner_attempt_id: "first".into(),
        user_request: "  Original request.\n".into(),
    };

    // Act
    let planner = start.planner_request();
    let chain = TaskChain {
        accepted_plan: None,
        agent: start.agent,
        current_request: planner.clone(),
        id: start.chain_id.clone(),
        implementer_task_id: start.implementer_task_id(),
        phase: TaskChainPhase::PlannerQuestion,
        planner_task_id: planner.task_id.clone(),
        session_id: SessionId::from("session"),
        user_request: start.user_request.clone(),
    };
    let continuation = chain.continuation_request(&ContinueTaskChainRequest {
        attempt_id: "second".into(),
        clarification: Some("  Answer.\n".into()),
    });

    // Assert
    assert_eq!(planner.task_id, "chain:chain:planner");
    assert_eq!(chain.implementer_task_id, "chain:chain:implementer");
    assert_eq!(planner.visibility, DurableMessageVisibility::Hidden);
    assert_eq!(planner.assignment.agent, start.agent);
    assert_eq!(planner.objective, start.user_request);
    assert_eq!(planner.input.user_request, start.user_request);
    assert_eq!(continuation.attempt_id, "second");
    assert_eq!(
        continuation.input.clarification.as_deref(),
        Some("  Answer.\n")
    );
    assert_eq!(continuation.assignment, planner.assignment);
    assert_eq!(continuation.task_id, planner.task_id);
    assert_eq!(continuation.input.accepted_plan, None);
    assert_eq!(continuation.input.user_request, planner.input.user_request);
}
