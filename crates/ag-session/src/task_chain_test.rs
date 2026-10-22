use ag_protocol::TurnPromptAttachment;

use crate::test_support::codex_selection;
use crate::{
    ContinueTaskChainRequest, DurableMessageVisibility, SessionError, SessionId,
    StartPlanThenImplementRequest, TaskChain, TaskChainPhase, TaskChainReplyOutcome,
};

#[test]
fn workflow_requests_capture_selection_and_keep_continuations_task_local() {
    // Arrange
    let start = StartPlanThenImplementRequest {
        agents: Box::new(crate::PlanThenImplementAgents::bundled().expect("bundled agents")),
        attachments: vec![TurnPromptAttachment {
            local_image_path: "original.png".into(),
            placeholder: "[Image #1]".into(),
        }],
        agent: codex_selection(),
        chain_id: "chain".into(),
        planner_attempt_id: "first".into(),
        user_request: "  Original request.\n".into(),
    };

    // Act
    let planner = start.planner_request();
    let chain = TaskChain {
        agents: crate::PlanThenImplementAgents::bundled().expect("bundled agents"),
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
        attachments: vec![TurnPromptAttachment {
            local_image_path: "revision.png".into(),
            placeholder: "[Image #1]".into(),
        }],
        attempt_id: "second".into(),
        clarification: Some("  Answer.\n".into()),
        expected_attempt_id: "first".into(),
    });

    // Assert
    assert_eq!(planner.task_id, "chain:chain:planner");
    assert_eq!(chain.implementer_task_id, "chain:chain:implementer");
    assert_eq!(planner.visibility, DurableMessageVisibility::Hidden);
    assert_eq!(planner.assignment.agent, start.agent);
    assert_eq!(planner.objective, start.user_request);
    assert_eq!(planner.input.user_request, start.user_request);
    assert_eq!(planner.input.attachments, start.attachments);
    assert_eq!(continuation.input.attachments.len(), 2);
    assert_eq!(continuation.input.attachments[0], start.attachments[0]);
    assert_eq!(
        continuation.input.attachments[1].local_image_path.to_str(),
        Some("revision.png")
    );
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

#[test]
fn reply_outcomes_derive_distinct_deterministic_attempt_identities() {
    // Arrange
    let reply_id = "reply-1";

    // Act
    let attempt_ids = TaskChainReplyOutcome::ALL.map(|outcome| outcome.attempt_id(reply_id));

    // Assert
    assert_eq!(
        attempt_ids,
        [
            "reply:reply-1:implement".to_string(),
            "reply:reply-1:revise".to_string(),
            "reply:reply-1:continue".to_string(),
        ]
    );
    assert_eq!(
        TaskChainReplyOutcome::PlanAccepted.attempt_id(reply_id),
        attempt_ids[0]
    );
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
