use std::path::PathBuf;

use ag_agent::{SessionStats, SpeedMode};
use ag_session::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary, SessionId, SessionRole,
    SessionStatus as Status,
};

use super::super::super::agent::AgentSelection;
use super::super::{
    PublishBranchAction, Session, SessionSize, can_append_session_to_stack,
    can_reply_to_session_in_stack, can_start_staged_session_in_stack,
};
use super::support::test_session;
use crate::domain::agent::AgentModel;
use crate::domain::transient_message::{
    QueuedAction, TransientMessage, TransientMessageAnchor, TransientMessageBody,
    TransientMessageLifecycle, TransientMessageSlot, TransientMessageStore,
};
use crate::test_support::SessionFixtureBuilder;

// -- can_sync_review_request tests ---------------------------------------

#[test]
fn test_has_review_request_reports_link_presence() {
    // Arrange
    let session_without_review_request = test_session(None);
    let mut session_with_review_request = test_session(None);
    session_with_review_request.review_request = Some(ReviewRequest {
        last_refreshed_at: 0,
        summary: ReviewRequestSummary {
            display_id: "#1".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "wt/session-id".to_string(),
            state: ReviewRequestState::Open,
            status_summary: None,
            target_branch: "main".to_string(),
            title: "feat".to_string(),
            web_url: String::new(),
        },
    });

    // Act
    let has_no_link = session_without_review_request.has_review_request();
    let has_link = session_with_review_request.has_review_request();

    // Assert
    assert!(!has_no_link);
    assert!(has_link);
}

#[test]
fn test_allows_stack_append_accepts_review_states_and_rejects_non_review_or_child() {
    // Arrange
    let review_session = SessionFixtureBuilder::new().status(Status::Review).build();
    let agent_review_session = SessionFixtureBuilder::new()
        .status(Status::AgentReview)
        .build();
    let running_session = SessionFixtureBuilder::new()
        .status(Status::InProgress)
        .build();
    let child_session = SessionFixtureBuilder::new()
        .parent_session_id(Some(SessionId::from("parent-session")))
        .status(Status::Review)
        .build();

    // Act
    let allows_review = review_session.allows_stack_append();
    let allows_agent_review = agent_review_session.allows_stack_append();
    let allows_running = running_session.allows_stack_append();
    let allows_child = child_session.allows_stack_append();

    // Assert
    assert!(allows_review);
    assert!(allows_agent_review);
    assert!(!allows_running);
    assert!(!allows_child);
}

#[test]
fn test_forge_indicator_returns_arrow_for_published_branch_without_review_request() {
    // Arrange
    let mut session = test_session(None);
    session.published_upstream_ref = Some("origin/wt/session-id".to_string());

    // Act
    let indicator = session.forge_indicator();

    // Assert
    assert_eq!(indicator, "↑");
}

#[test]
fn test_forge_indicator_prefers_review_request_over_published_ref() {
    // Arrange
    let mut session = test_session(None);
    session.published_upstream_ref = Some("origin/wt/session-id".to_string());
    session.review_request = Some(ReviewRequest {
        last_refreshed_at: 0,
        summary: ReviewRequestSummary {
            display_id: "#10".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "wt/session-id".to_string(),
            state: ReviewRequestState::Open,
            status_summary: None,
            target_branch: "main".to_string(),
            title: "feat".to_string(),
            web_url: String::new(),
        },
    });

    // Act
    let indicator = session.forge_indicator();

    // Assert
    assert_eq!(indicator, "⊙ #10");
}

#[test]
fn test_status_transition_review_to_queued() {
    // Arrange
    let current_status = Status::Review;

    // Act
    let can_transition = current_status.can_transition_to(Status::Queued);

    // Assert
    assert!(can_transition);
}

#[test]
fn test_status_transition_review_to_agent_review() {
    // Arrange
    let current_status = Status::Review;

    // Act
    let can_transition = current_status.can_transition_to(Status::AgentReview);

    // Assert
    assert!(can_transition);
}

// -- status transition: Review/AgentReview/Question → Done ---------------

#[test]
fn test_status_transition_review_to_done() {
    // Arrange
    let current_status = Status::Review;

    // Act
    let can_transition = current_status.can_transition_to(Status::Done);

    // Assert
    assert!(can_transition);
}

#[test]
fn test_status_transition_agent_review_to_done() {
    // Arrange
    let current_status = Status::AgentReview;

    // Act
    let can_transition = current_status.can_transition_to(Status::Done);

    // Assert
    assert!(can_transition);
}

#[test]
fn test_publish_pull_request_action_respects_review_session_capabilities() {
    // Arrange
    let worker = SessionFixtureBuilder::new().status(Status::Review).build();
    let orchestrator = SessionFixtureBuilder::new()
        .role(SessionRole::Orchestrator)
        .status(Status::Review)
        .build();
    let managed_worker = SessionFixtureBuilder::new()
        .role(SessionRole::OrchestrationWorker)
        .status(Status::Review)
        .build();

    // Act
    let actions = [
        worker.publish_pull_request_action(),
        orchestrator.publish_pull_request_action(),
        managed_worker.publish_pull_request_action(),
    ];

    // Assert
    assert_eq!(
        actions,
        [Some(PublishBranchAction::PublishPullRequest), None, None]
    );
}

#[test]
fn test_publish_pull_request_action_returns_publish_for_agent_review_session() {
    // Arrange
    let session = Session {
        base_branch: "main".to_string(),
        created_at: 0,
        draft_attachments: Vec::new(),
        folder: PathBuf::new(),
        follow_up_tasks: Vec::new(),
        id: "session-id".into(),
        in_progress_started_at: None,
        in_progress_total_seconds: 0,
        is_draft: false,
        controller_session_id: None,
        orchestration_progress: None,
        role: SessionRole::default(),
        agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            AgentModel::Gemini38Flash,
        ),
        parent_session_id: None,
        permission_mode: crate::domain::permission::PermissionMode::AutoEdit,
        personality_id: None,
        project_name: "project".to_string(),
        prompt: String::new(),
        queued_messages: Vec::new(),
        reasoning_level_override: None,
        response_style: crate::domain::agent::ResponseStyle::default(),
        published_upstream_ref: None,
        questions: Vec::new(),
        review_request: None,
        size: SessionSize::Xs,
        speed_mode: SpeedMode::default(),
        stats: SessionStats::default(),
        status: Status::AgentReview,
        title: None,
        transcript: None,
        updated_at: 0,
        transient_messages: TransientMessageStore::default(),
    };

    // Act
    let action = session.publish_pull_request_action();

    // Assert
    assert_eq!(action, Some(PublishBranchAction::PublishPullRequest));
}

#[test]
fn test_publish_pull_request_action_returns_none_while_publish_is_active() {
    for body in [
        TransientMessageBody::Queued(QueuedAction::new(0, "publish after this turn".to_string())),
        TransientMessageBody::Loading("Publishing review request...".to_string()),
    ] {
        // Arrange
        let mut session = crate::test_support::session_fixture("session-id", Status::Review);
        session.transient_messages.upsert(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body,
            lifecycle: TransientMessageLifecycle::UntilResolved,
            slot: TransientMessageSlot::BranchPublish,
            turn_position: None,
        });

        // Act
        let action = session.publish_pull_request_action();

        // Assert
        assert_eq!(action, None);
    }
}

#[test]
fn test_publish_pull_request_action_queues_for_active_session() {
    // Arrange
    let sessions = [Status::InProgress, Status::Rebasing]
        .map(|status| SessionFixtureBuilder::new().status(status).build());

    // Act
    let actions = sessions.map(|session| session.publish_pull_request_action());

    // Assert
    assert_eq!(
        actions,
        [
            Some(PublishBranchAction::PublishPullRequest),
            Some(PublishBranchAction::PublishPullRequest),
        ]
    );
}

#[test]
fn test_publish_pull_request_action_returns_none_for_done_session() {
    // Arrange
    let session = Session {
        base_branch: "main".to_string(),
        created_at: 0,
        draft_attachments: Vec::new(),
        folder: PathBuf::new(),
        follow_up_tasks: Vec::new(),
        id: "session-id".into(),
        in_progress_started_at: None,
        in_progress_total_seconds: 180,
        is_draft: false,
        controller_session_id: None,
        orchestration_progress: None,
        role: SessionRole::default(),
        agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            AgentModel::Gemini38Flash,
        ),
        parent_session_id: None,
        permission_mode: crate::domain::permission::PermissionMode::AutoEdit,
        personality_id: None,
        project_name: "project".to_string(),
        prompt: String::new(),
        queued_messages: Vec::new(),
        reasoning_level_override: None,
        response_style: crate::domain::agent::ResponseStyle::default(),
        published_upstream_ref: Some("origin/wt/session-id".to_string()),
        questions: Vec::new(),
        review_request: None,
        size: SessionSize::Xs,
        speed_mode: SpeedMode::default(),
        stats: SessionStats::default(),
        status: Status::Done,
        title: None,
        transcript: None,
        updated_at: 0,
        transient_messages: TransientMessageStore::default(),
    };

    // Act
    let action = session.publish_pull_request_action();

    // Assert
    assert_eq!(action, None);
}

#[test]
fn test_allows_fork_action_accepts_review_ready_materialized_sessions() {
    // Arrange
    let review_session = SessionFixtureBuilder::new()
        .draft(false)
        .status(Status::Review)
        .build();
    let agent_review_session = SessionFixtureBuilder::new()
        .draft(false)
        .status(Status::AgentReview)
        .build();

    // Act
    let allows_review_fork = review_session.allows_fork_action();
    let allows_agent_review_fork = agent_review_session.allows_fork_action();

    // Assert
    assert!(allows_review_fork);
    assert!(allows_agent_review_fork);
}

#[test]
fn test_can_start_staged_session_in_stack_requires_parent_review() {
    // Arrange
    let parent_session = SessionFixtureBuilder::new()
        .id("parent-session")
        .draft(false)
        .status(Status::InProgress)
        .build();
    let child_session = SessionFixtureBuilder::new()
        .id("child-session")
        .draft(true)
        .status(Status::Draft)
        .prompt("Ready child draft")
        .parent_session_id(Some(SessionId::from("parent-session")))
        .build();
    let sessions = vec![parent_session, child_session];

    // Act
    let can_start_child = can_start_staged_session_in_stack(&sessions, "child-session");

    // Assert
    assert!(!can_start_child);
}

#[test]
fn test_can_start_staged_session_in_stack_allows_review_ready_parent() {
    // Arrange
    let parent_session = SessionFixtureBuilder::new()
        .id("parent-session")
        .draft(false)
        .status(Status::Review)
        .build();
    let child_session = SessionFixtureBuilder::new()
        .id("child-session")
        .draft(true)
        .status(Status::Draft)
        .prompt("Ready child draft")
        .parent_session_id(Some(SessionId::from("parent-session")))
        .build();
    let sessions = vec![parent_session, child_session];

    // Act
    let can_start_child = can_start_staged_session_in_stack(&sessions, "child-session");

    // Assert
    assert!(can_start_child);
}

#[test]
fn test_can_start_nested_staged_session_uses_immediate_parent_review_state() {
    // Arrange
    let root_session = SessionFixtureBuilder::new()
        .id("root-session")
        .draft(false)
        .status(Status::Draft)
        .build();
    let parent_session = SessionFixtureBuilder::new()
        .id("parent-session")
        .draft(true)
        .status(Status::Review)
        .parent_session_id(Some(SessionId::from("root-session")))
        .build();
    let child_session = SessionFixtureBuilder::new()
        .id("child-session")
        .draft(true)
        .status(Status::Draft)
        .prompt("Ready nested draft")
        .parent_session_id(Some(SessionId::from("parent-session")))
        .build();
    let sessions = vec![root_session, parent_session, child_session];

    // Act
    let can_start_child = can_start_staged_session_in_stack(&sessions, "child-session");

    // Assert
    assert!(can_start_child);
}

#[test]
fn test_status_allows_review_actions_for_agent_review() {
    // Arrange
    let status = Status::AgentReview;

    // Act
    let allows_review_actions = status.allows_review_actions();

    // Assert
    assert!(allows_review_actions);
}

#[test]
fn test_status_allows_diff_view_for_review_ready_and_read_only_states() {
    // Arrange
    let expected_statuses = [Status::Review, Status::AgentReview, Status::Merged];

    // Act
    let allowed_statuses: Vec<Status> = Status::ALL
        .into_iter()
        .filter(|status| status.allows_diff_view())
        .collect();

    // Assert
    assert_eq!(allowed_statuses, expected_statuses);
}

#[test]
fn test_can_append_session_to_stack_accepts_idle_review_ready_root_and_parent() {
    // Arrange
    let source_session = SessionFixtureBuilder::new()
        .id("source-session")
        .status(Status::AgentReview)
        .build();
    let parent_session = SessionFixtureBuilder::new()
        .id("parent-session")
        .status(Status::Review)
        .build();
    let sessions = vec![source_session, parent_session];

    // Act
    let can_append = can_append_session_to_stack(&sessions, "source-session", "parent-session");

    // Assert
    assert!(can_append);
}

#[test]
fn test_can_reply_to_session_in_stack_allows_parent_with_review_child() {
    // Arrange
    let parent_session = SessionFixtureBuilder::new()
        .id("parent-session")
        .draft(false)
        .status(Status::Review)
        .build();
    let child_session = SessionFixtureBuilder::new()
        .id("child-session")
        .draft(true)
        .status(Status::Review)
        .parent_session_id(Some(SessionId::from("parent-session")))
        .build();
    let sessions = vec![parent_session, child_session];

    // Act
    let can_reply_to_parent = can_reply_to_session_in_stack(&sessions, "parent-session");

    // Assert
    assert!(can_reply_to_parent);
}

#[test]
fn test_allows_review_comment_reply_accepts_review_or_question_sessions() {
    // Arrange
    let statuses = [Status::Review, Status::AgentReview, Status::Question];

    // Act
    let reply_permissions = statuses.map(|status| {
        SessionFixtureBuilder::new()
            .status(status)
            .build()
            .allows_review_comment_reply()
    });

    // Assert
    assert_eq!(reply_permissions, [true, true, true]);
}

#[test]
fn test_allows_review_comment_reply_rejects_non_reply_or_managed_session() {
    // Arrange
    let session = SessionFixtureBuilder::new()
        .status(Status::InProgress)
        .build();
    let managed_session = SessionFixtureBuilder::new()
        .role(SessionRole::OrchestrationWorker)
        .status(Status::Review)
        .build();

    // Act
    let allows_reply = session.allows_review_comment_reply();
    let managed_allows_reply = managed_session.allows_review_comment_reply();

    // Assert
    assert!(!allows_reply);
    assert!(!managed_allows_reply);
}

#[test]
fn test_allows_worktree_open_action_accepts_managed_worker_only_in_review() {
    // Arrange
    let statuses = [Status::InProgress, Status::Review, Status::AgentReview];

    // Act
    let open_permissions = statuses.map(|status| {
        SessionFixtureBuilder::new()
            .role(SessionRole::OrchestrationWorker)
            .status(status)
            .build()
            .allows_worktree_open_action()
    });
    let research_permission = SessionFixtureBuilder::new()
        .role(SessionRole::OrchestrationResearcher)
        .status(Status::Review)
        .build()
        .allows_worktree_open_action();

    // Assert
    assert_eq!(open_permissions, [false, true, false]);
    assert!(!research_permission);
}
