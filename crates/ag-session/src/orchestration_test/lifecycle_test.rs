use crate::model::SessionStatus;
use crate::orchestration::{
    IntegrationApproach, OrchestrationStatus, OrchestrationTaskKind, OrchestrationTaskStatus,
};

#[test]
/// Round-trips every orchestration status through its persisted form.
fn test_orchestration_status_round_trips_persisted_values() {
    // Arrange
    let statuses = [
        OrchestrationStatus::AwaitingApproval,
        OrchestrationStatus::Running,
        OrchestrationStatus::Canceling,
        OrchestrationStatus::Verifying,
        OrchestrationStatus::AwaitingIntegration,
        OrchestrationStatus::Integrating,
        OrchestrationStatus::Done,
        OrchestrationStatus::Canceled,
    ];

    // Act
    let round_tripped = statuses.map(|status| {
        status
            .to_string()
            .parse::<OrchestrationStatus>()
            .expect("status should parse")
    });

    // Assert
    assert_eq!(round_tripped, statuses);
    assert!("Unknown".parse::<OrchestrationStatus>().is_err());
}

#[test]
fn integration_approach_round_trips_persisted_values() {
    // Arrange
    let approaches = [
        IntegrationApproach::LocalMerge,
        IntegrationApproach::ReviewRequest,
    ];

    // Act
    let round_tripped = approaches.map(|approach| {
        approach
            .to_string()
            .parse::<IntegrationApproach>()
            .expect("approach should parse")
    });

    // Assert
    assert_eq!(round_tripped, approaches);
    assert!("Unknown".parse::<IntegrationApproach>().is_err());
}

#[test]
fn orchestration_task_kind_round_trips_persisted_values() {
    // Arrange
    let kinds = [
        OrchestrationTaskKind::Implementation,
        OrchestrationTaskKind::Research,
    ];

    // Act
    let round_tripped = kinds.map(|kind| {
        kind.to_string()
            .parse::<OrchestrationTaskKind>()
            .expect("kind should parse")
    });

    // Assert
    assert_eq!(round_tripped, kinds);
    assert_eq!(OrchestrationTaskKind::default(), kinds[0]);
    assert!("Unknown".parse::<OrchestrationTaskKind>().is_err());
}

#[test]
/// Restricts restart re-linking to orchestrations that still need work.
fn test_only_unsettled_orchestrations_are_active() {
    // Arrange
    let cases = [
        (OrchestrationStatus::AwaitingApproval, true),
        (OrchestrationStatus::Running, true),
        (OrchestrationStatus::Canceling, true),
        (OrchestrationStatus::Verifying, true),
        (OrchestrationStatus::AwaitingIntegration, true),
        (OrchestrationStatus::Integrating, true),
        (OrchestrationStatus::Done, false),
        (OrchestrationStatus::Canceled, false),
    ];

    // Act
    let results = cases.map(|(status, _)| status.is_active());

    // Assert
    assert_eq!(results, cases.map(|(_, expected)| expected));
}

#[test]
/// Round-trips every task status through its persisted form.
fn test_orchestration_task_status_round_trips_persisted_values() {
    // Arrange
    let statuses = [
        OrchestrationTaskStatus::Proposed,
        OrchestrationTaskStatus::Planned,
        OrchestrationTaskStatus::Creating,
        OrchestrationTaskStatus::Running,
        OrchestrationTaskStatus::Reviewing,
        OrchestrationTaskStatus::ReviewApplying,
        OrchestrationTaskStatus::WaitingForInput,
        OrchestrationTaskStatus::Ready,
        OrchestrationTaskStatus::Reported,
        OrchestrationTaskStatus::ContinuationPending,
        OrchestrationTaskStatus::AwaitingIntegration,
        OrchestrationTaskStatus::Merging,
        OrchestrationTaskStatus::Integrated,
        OrchestrationTaskStatus::ReviewRequested,
        OrchestrationTaskStatus::IntegrationFailed,
        OrchestrationTaskStatus::Detached,
        OrchestrationTaskStatus::Failed,
        OrchestrationTaskStatus::Canceled,
    ];

    // Act
    let round_tripped = statuses.map(|status| {
        status
            .to_string()
            .parse::<OrchestrationTaskStatus>()
            .expect("status should parse")
    });

    // Assert
    assert_eq!(round_tripped, statuses);
    assert!("Unknown".parse::<OrchestrationTaskStatus>().is_err());
}

#[test]
/// Treats a canceled straggler as settled so fan-in is not blocked by
/// out-of-band cancellation.
fn test_settled_task_statuses_include_cancellation() {
    // Arrange
    let cases = [
        (OrchestrationTaskStatus::Ready, true),
        (OrchestrationTaskStatus::Reported, true),
        (OrchestrationTaskStatus::Integrated, true),
        (OrchestrationTaskStatus::ReviewRequested, true),
        (OrchestrationTaskStatus::IntegrationFailed, true),
        (OrchestrationTaskStatus::Failed, true),
        (OrchestrationTaskStatus::Canceled, true),
        (OrchestrationTaskStatus::Planned, false),
        (OrchestrationTaskStatus::Creating, false),
        (OrchestrationTaskStatus::Running, false),
        (OrchestrationTaskStatus::Reviewing, false),
        (OrchestrationTaskStatus::ReviewApplying, false),
        (OrchestrationTaskStatus::WaitingForInput, false),
    ];

    // Act
    let results = cases.map(|(status, _)| status.is_settled());

    // Assert
    assert_eq!(results, cases.map(|(_, expected)| expected));
}

#[test]
/// Counts a task waiting for user input against the parallelism cap
/// because it still owns a live child session and worktree.
fn test_parallelism_slots_cover_every_live_child() {
    // Arrange
    let cases = [
        (OrchestrationTaskStatus::Creating, true),
        (OrchestrationTaskStatus::Running, true),
        (OrchestrationTaskStatus::Reviewing, true),
        (OrchestrationTaskStatus::ReviewApplying, true),
        (OrchestrationTaskStatus::WaitingForInput, true),
        (OrchestrationTaskStatus::Planned, false),
        (OrchestrationTaskStatus::Ready, false),
        (OrchestrationTaskStatus::Failed, false),
        (OrchestrationTaskStatus::Canceled, false),
    ];

    // Act
    let results = cases.map(|(status, _)| status.occupies_parallelism_slot());

    // Assert
    assert_eq!(results, cases.map(|(_, expected)| expected));
}

#[test]
/// Allows the fan-out, question, settle, and retry transitions the
/// coordinator drives, and rejects skipping creation.
fn test_task_status_transitions_cover_fan_out_and_retry() {
    // Arrange
    let cases = [
        (
            OrchestrationTaskStatus::Planned,
            OrchestrationTaskStatus::Creating,
            true,
        ),
        (
            OrchestrationTaskStatus::Creating,
            OrchestrationTaskStatus::Running,
            true,
        ),
        (
            OrchestrationTaskStatus::Running,
            OrchestrationTaskStatus::WaitingForInput,
            true,
        ),
        (
            OrchestrationTaskStatus::WaitingForInput,
            OrchestrationTaskStatus::Running,
            true,
        ),
        (
            OrchestrationTaskStatus::Running,
            OrchestrationTaskStatus::Ready,
            true,
        ),
        (
            OrchestrationTaskStatus::Running,
            OrchestrationTaskStatus::Reported,
            true,
        ),
        (
            OrchestrationTaskStatus::Running,
            OrchestrationTaskStatus::Reviewing,
            true,
        ),
        (
            OrchestrationTaskStatus::Reviewing,
            OrchestrationTaskStatus::ReviewApplying,
            true,
        ),
        (
            OrchestrationTaskStatus::ReviewApplying,
            OrchestrationTaskStatus::Reviewing,
            true,
        ),
        (
            OrchestrationTaskStatus::Running,
            OrchestrationTaskStatus::Canceled,
            true,
        ),
        (
            OrchestrationTaskStatus::Failed,
            OrchestrationTaskStatus::Creating,
            true,
        ),
        (
            OrchestrationTaskStatus::Ready,
            OrchestrationTaskStatus::Ready,
            true,
        ),
        (
            OrchestrationTaskStatus::Planned,
            OrchestrationTaskStatus::Running,
            false,
        ),
        (
            OrchestrationTaskStatus::Canceled,
            OrchestrationTaskStatus::Ready,
            false,
        ),
    ];

    // Act
    let transitions = cases.map(|(status, next, _)| status.can_transition_to(next));

    // Assert
    assert_eq!(transitions, cases.map(|(_, _, expected)| expected));
}

#[test]
fn integration_settlement_waits_for_review_request_merge() {
    // Arrange
    let settled = [
        OrchestrationTaskStatus::Integrated,
        OrchestrationTaskStatus::Detached,
        OrchestrationTaskStatus::Canceled,
        OrchestrationTaskStatus::Failed,
    ];
    let pending = [
        OrchestrationTaskStatus::Reported,
        OrchestrationTaskStatus::AwaitingIntegration,
        OrchestrationTaskStatus::ReviewRequested,
    ];
    let transitions = [
        (
            OrchestrationTaskStatus::Merging,
            OrchestrationTaskStatus::ReviewRequested,
        ),
        (
            OrchestrationTaskStatus::ReviewRequested,
            OrchestrationTaskStatus::Integrated,
        ),
        (
            OrchestrationTaskStatus::ReviewRequested,
            OrchestrationTaskStatus::IntegrationFailed,
        ),
    ];

    // Act
    let settled_results = settled.map(OrchestrationTaskStatus::is_integration_settled);
    let pending_results = pending.map(OrchestrationTaskStatus::is_integration_settled);
    let transition_results = transitions.map(|(status, next)| status.can_transition_to(next));

    // Assert
    assert_eq!(settled_results, [true; 4]);
    assert_eq!(pending_results, [false; 3]);
    assert_eq!(transition_results, [true; 3]);
}

#[test]
fn campaign_labels_cover_every_task_status() {
    // Arrange
    let statuses = [
        OrchestrationTaskStatus::Proposed,
        OrchestrationTaskStatus::Planned,
        OrchestrationTaskStatus::Creating,
        OrchestrationTaskStatus::Running,
        OrchestrationTaskStatus::Reviewing,
        OrchestrationTaskStatus::ReviewApplying,
        OrchestrationTaskStatus::WaitingForInput,
        OrchestrationTaskStatus::Ready,
        OrchestrationTaskStatus::Reported,
        OrchestrationTaskStatus::ContinuationPending,
        OrchestrationTaskStatus::AwaitingIntegration,
        OrchestrationTaskStatus::Merging,
        OrchestrationTaskStatus::Integrated,
        OrchestrationTaskStatus::ReviewRequested,
        OrchestrationTaskStatus::IntegrationFailed,
        OrchestrationTaskStatus::Detached,
        OrchestrationTaskStatus::Failed,
        OrchestrationTaskStatus::Canceled,
    ];

    // Act
    let labels = statuses.map(OrchestrationTaskStatus::campaign_label);

    // Assert
    assert_eq!(
        labels,
        [
            "awaiting approval",
            "waiting",
            "starting",
            "running",
            "reviewing",
            "applying review",
            "waiting on you",
            "ready",
            "reported",
            "continuing",
            "awaiting integration",
            "integrating",
            "integrated",
            "review requested",
            "integration failed",
            "detached",
            "failed",
            "canceled",
        ]
    );
}

#[test]
/// Maps every child-session lifecycle family into orchestration policy.
fn test_task_status_from_child_status_covers_session_lifecycle() {
    // Arrange
    let cases = [
        (SessionStatus::Draft, OrchestrationTaskStatus::Running),
        (SessionStatus::InProgress, OrchestrationTaskStatus::Running),
        (SessionStatus::Queued, OrchestrationTaskStatus::Running),
        (SessionStatus::Rebasing, OrchestrationTaskStatus::Running),
        (SessionStatus::Merging, OrchestrationTaskStatus::Running),
        (
            SessionStatus::Question,
            OrchestrationTaskStatus::WaitingForInput,
        ),
        (SessionStatus::Review, OrchestrationTaskStatus::Reviewing),
        (
            SessionStatus::AgentReview,
            OrchestrationTaskStatus::Reviewing,
        ),
        (SessionStatus::Merged, OrchestrationTaskStatus::Ready),
        (SessionStatus::Done, OrchestrationTaskStatus::Ready),
        (SessionStatus::Canceled, OrchestrationTaskStatus::Failed),
    ];

    // Act / Assert
    for (session_status, expected_task_status) in cases {
        assert_eq!(
            OrchestrationTaskStatus::from_child_status(session_status),
            expected_task_status
        );
    }
}
