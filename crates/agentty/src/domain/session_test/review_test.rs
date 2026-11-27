//! Session review tests for review requests, pull-request
//! publishing, review-comment replies, and review watch.

use std::path::PathBuf;

use ag_contracts::{SessionStats, SpeedMode};
use ag_session::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary, SessionStatus as Status,
};

use super::super::super::agent::AgentSelection;
use super::super::{
    PublishBranchAction, ReviewRequestMergeAction, ReviewRequestMergeBlocker, ReviewWatchToggle,
    Session, SessionSize,
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

    // Act
    let action = worker.publish_pull_request_action();

    // Assert
    assert_eq!(action, Some(PublishBranchAction::PublishPullRequest));
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
        has_archived_diff: false,
        id: "session-id".into(),
        in_progress_started_at: None,
        in_progress_total_seconds: 0,
        is_draft: false,
        agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            AgentModel::Gemini38Flash,
        ),
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
        has_archived_diff: false,
        id: "session-id".into(),
        in_progress_started_at: None,
        in_progress_total_seconds: 180,
        is_draft: false,
        agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            AgentModel::Gemini38Flash,
        ),
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
fn test_status_allows_review_actions_for_agent_review() {
    // Arrange
    let status = Status::AgentReview;

    // Act
    let allows_review_actions = status.allows_review_actions();

    // Assert
    assert!(allows_review_actions);
}

#[test]
fn test_status_allows_diff_view_for_working_review_ready_and_read_only_states() {
    // Arrange
    let expected_statuses = [
        Status::InProgress,
        Status::Review,
        Status::AgentReview,
        Status::Watching,
        Status::Merged,
        Status::Done,
        Status::Canceled,
    ];

    // Act
    let allowed_statuses: Vec<Status> = Status::ALL
        .into_iter()
        .filter(|status| status.allows_diff_view())
        .collect();

    // Assert
    assert_eq!(allowed_statuses, expected_statuses);
}

#[test]
fn test_allows_review_comment_reply_accepts_review_question_and_busy_sessions() {
    // Arrange
    let statuses = [
        Status::Review,
        Status::AgentReview,
        Status::Question,
        Status::InProgress,
        Status::Rebasing,
    ];

    // Act
    let reply_permissions = statuses.map(|status| {
        SessionFixtureBuilder::new()
            .status(status)
            .build()
            .allows_review_comment_reply()
    });

    // Assert
    assert_eq!(reply_permissions, [true, true, true, true, true]);
}

#[test]
fn test_allows_review_comment_reply_rejects_non_reply_session() {
    // Arrange
    let session = SessionFixtureBuilder::new().status(Status::Merging).build();

    // Act
    let allows_reply = session.allows_review_comment_reply();

    // Assert
    assert!(!allows_reply);
}

#[test]
fn test_review_comments_reject_duplicate_queued_batch() {
    // Arrange
    let mut session = SessionFixtureBuilder::new()
        .status(Status::InProgress)
        .build();
    session.transient_messages.upsert(TransientMessage {
        anchor: TransientMessageAnchor::Tail,
        body: TransientMessageBody::Queued(QueuedAction::new(1, "resolve comments".to_string())),
        lifecycle: TransientMessageLifecycle::UntilResolved,
        slot: TransientMessageSlot::GeneratedTurnQueue,
        turn_position: None,
    });

    // Act
    let can_reply = session.allows_review_comment_reply();

    // Assert
    assert!(!can_reply);
}

#[test]
fn review_comment_batches_and_worker_idleness_wait_for_publish_and_queued_actions_in_review() {
    // Arrange
    let mut session = SessionFixtureBuilder::new().status(Status::Review).build();
    let idle_is_queued = session.queues_review_comment_reply();
    let idle_has_pending_work = session.has_pending_worker_work();
    let messages = [
        (
            TransientMessageSlot::BranchPublish,
            TransientMessageBody::Loading("Publishing".to_string()),
        ),
        (
            TransientMessageSlot::BranchPublish,
            TransientMessageBody::Queued(QueuedAction::new(1, "publish".to_string())),
        ),
        (
            TransientMessageSlot::SyncQueue,
            TransientMessageBody::Queued(QueuedAction::new(1, "sync".to_string())),
        ),
    ];

    // Act, Assert
    assert!(!idle_is_queued);
    assert!(!idle_has_pending_work);
    for (slot, body) in messages {
        session.transient_messages.upsert(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body,
            lifecycle: TransientMessageLifecycle::UntilResolved,
            slot,
            turn_position: None,
        });
        assert!(session.queues_review_comment_reply());
        assert!(session.has_pending_worker_work());
        session.transient_messages.retract(slot);
    }
}

#[test]
fn review_comment_batches_queue_while_question_without_earlier_work() {
    // Arrange
    let session = SessionFixtureBuilder::new()
        .status(Status::Question)
        .build();

    // Act
    let queues_review = session.queues_review_comment_reply();

    // Assert
    assert!(queues_review);
    assert!(session.allows_review_comment_reply());
    assert_eq!(session.queued_messages.len(), 0);
}

#[test]
fn review_watch_toggle_starts_from_open_review_and_stops_from_watching() {
    // Arrange
    let linked = |state| {
        Some(ReviewRequest {
            last_refreshed_at: 0,
            summary: ReviewRequestSummary {
                display_id: "#42".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "wt/session".to_string(),
                state,
                status_summary: None,
                target_branch: "main".to_string(),
                title: "Add watch".to_string(),
                web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            },
        })
    };
    let cases = [
        (
            Status::Review,
            linked(ReviewRequestState::Open),
            Some(ReviewWatchToggle::Start),
        ),
        (Status::Review, linked(ReviewRequestState::Merged), None),
        (Status::Review, None, None),
        (
            Status::Watching,
            linked(ReviewRequestState::Open),
            Some(ReviewWatchToggle::Stop),
        ),
        (Status::InProgress, linked(ReviewRequestState::Open), None),
    ];

    for (status, review_request, expected) in cases {
        let session = SessionFixtureBuilder::new()
            .status(status)
            .review_request(review_request)
            .build();

        // Act
        let toggle = session.review_watch_toggle();

        // Assert
        assert_eq!(toggle, expected, "{status}");
    }
}

/// Builds one linked pull request on `forge_kind` in `state` with
/// `status_summary`.
fn linked_pull_request(
    forge_kind: ForgeKind,
    state: ReviewRequestState,
    status_summary: &str,
) -> ReviewRequest {
    ReviewRequest {
        last_refreshed_at: 0,
        summary: ReviewRequestSummary {
            display_id: "#42".to_string(),
            forge_kind,
            source_branch: "wt/session-id".to_string(),
            state,
            status_summary: Some(status_summary.to_string()),
            target_branch: "main".to_string(),
            title: "feat".to_string(),
            web_url: String::new(),
        },
    }
}

/// Returns one loading row in `slot`.
fn loading_message(slot: TransientMessageSlot) -> TransientMessage {
    TransientMessage {
        anchor: TransientMessageAnchor::Tail,
        body: TransientMessageBody::Loading("Merging PR #42 into `main`...".to_string()),
        lifecycle: TransientMessageLifecycle::UntilResolved,
        slot,
        turn_position: None,
    }
}

#[test]
fn review_request_merge_follows_the_latest_github_readiness() {
    // Arrange
    let ready = "Approved, Mergeable, PR ready";
    let bypass_ready = "Review required, Blocked, PR ready with bypass";
    let open = |forge_kind, status_summary| {
        Some(linked_pull_request(
            forge_kind,
            ReviewRequestState::Open,
            status_summary,
        ))
    };
    let cases = [
        (
            Status::Review,
            open(ForgeKind::GitHub, ready),
            Ok(ReviewRequestMergeAction::Merge),
        ),
        (
            Status::Watching,
            open(ForgeKind::GitHub, bypass_ready),
            Ok(ReviewRequestMergeAction::BypassMerge),
        ),
        (
            Status::Review,
            open(ForgeKind::GitHub, "Checks pending"),
            Err(ReviewRequestMergeBlocker::NotReady),
        ),
        (
            Status::AgentReview,
            open(ForgeKind::GitHub, ready),
            Err(ReviewRequestMergeBlocker::Busy),
        ),
        (
            Status::Review,
            open(ForgeKind::GitLab, ready),
            Err(ReviewRequestMergeBlocker::NoPullRequest),
        ),
        (
            Status::Review,
            Some(linked_pull_request(
                ForgeKind::GitHub,
                ReviewRequestState::Merged,
                ready,
            )),
            Err(ReviewRequestMergeBlocker::NoPullRequest),
        ),
        (
            Status::Review,
            None,
            Err(ReviewRequestMergeBlocker::NoPullRequest),
        ),
        (
            Status::InProgress,
            open(ForgeKind::GitHub, ready),
            Err(ReviewRequestMergeBlocker::Busy),
        ),
        (
            Status::Queued,
            open(ForgeKind::GitHub, ready),
            Err(ReviewRequestMergeBlocker::Busy),
        ),
    ];

    for (status, review_request, expected) in cases {
        let session = SessionFixtureBuilder::new()
            .status(status)
            .review_request(review_request)
            .build();

        // Act
        let availability = session.review_request_merge_availability();

        // Assert
        assert_eq!(availability, expected, "{status}");
    }
}

#[test]
fn review_request_merge_waits_for_a_running_publish_or_merge() {
    // Arrange
    let ready_session = || {
        SessionFixtureBuilder::new()
            .status(Status::Review)
            .review_request(Some(linked_pull_request(
                ForgeKind::GitHub,
                ReviewRequestState::Open,
                "PR ready",
            )))
            .build()
    };
    let mut publishing = ready_session();
    publishing
        .transient_messages
        .upsert(loading_message(TransientMessageSlot::BranchPublish));
    let mut merging = ready_session();
    merging
        .transient_messages
        .upsert(loading_message(TransientMessageSlot::ReviewRequestMerge));

    // Act
    let publishing_merge = publishing.review_request_merge_availability();
    let merging_merge = merging.review_request_merge_availability();
    let merging_publish = merging.publish_pull_request_action();

    // Assert
    assert_eq!(publishing_merge, Err(ReviewRequestMergeBlocker::Busy));
    assert_eq!(merging_merge, Err(ReviewRequestMergeBlocker::Merging));
    assert_eq!(merging_publish, None);
}

#[test]
fn review_request_merge_blockers_and_actions_describe_themselves() {
    // Arrange
    let blockers = [
        ReviewRequestMergeBlocker::Busy,
        ReviewRequestMergeBlocker::Merging,
        ReviewRequestMergeBlocker::NoPullRequest,
        ReviewRequestMergeBlocker::NotReady,
    ];

    // Act
    let messages = blockers.map(ReviewRequestMergeBlocker::message);
    let bypasses = [
        ReviewRequestMergeAction::Merge,
        ReviewRequestMergeAction::BypassMerge,
    ]
    .map(ReviewRequestMergeAction::bypasses_protections);

    // Assert
    assert_eq!(
        messages,
        [
            "The session has work in progress; merge its pull request once it finishes",
            "The session's pull request is already merging",
            "The session has no open GitHub pull request",
            "The session's pull request is not ready to merge",
        ]
    );
    assert_eq!(bypasses, [false, true]);
}
