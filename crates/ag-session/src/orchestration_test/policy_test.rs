use crate::orchestration::{
    OrchestrationPolicy, OrchestrationScheduleDecision, OrchestrationTaskStatus,
};

#[test]
/// Derives fan-out capacity and roll-up readiness from typed task states.
fn test_orchestration_policy_schedules_available_slots_and_settlement() {
    // Arrange
    let active_statuses = [
        Some(OrchestrationTaskStatus::Running),
        Some(OrchestrationTaskStatus::WaitingForInput),
        Some(OrchestrationTaskStatus::Planned),
        Some(OrchestrationTaskStatus::Planned),
    ];
    let settled_statuses = [
        Some(OrchestrationTaskStatus::Ready),
        Some(OrchestrationTaskStatus::Failed),
        Some(OrchestrationTaskStatus::Canceled),
    ];
    let invalid_statuses = [Some(OrchestrationTaskStatus::Ready), None];

    // Act
    let active_decision = OrchestrationPolicy::schedule(3, &active_statuses);
    let settled_decision = OrchestrationPolicy::schedule(3, &settled_statuses);
    let empty_decision = OrchestrationPolicy::schedule(3, &[]);
    let invalid_decision = OrchestrationPolicy::schedule(3, &invalid_statuses);

    // Assert
    assert_eq!(
        active_decision,
        OrchestrationScheduleDecision {
            spawn_count: 1,
            should_submit: false,
        }
    );
    assert_eq!(
        settled_decision,
        OrchestrationScheduleDecision {
            spawn_count: 0,
            should_submit: true,
        }
    );
    assert_eq!(
        empty_decision,
        OrchestrationScheduleDecision {
            spawn_count: 0,
            should_submit: false,
        }
    );
    assert!(!invalid_decision.should_submit);
}
