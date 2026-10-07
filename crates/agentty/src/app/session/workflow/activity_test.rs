use std::time::SystemTime;

use ag_contracts::{ActivityEvent, ActivityKind, ActivityStatus};

use super::TurnActivity;

#[test]
fn summary_counts_invocations_not_updates_and_separates_attempts_and_skills() {
    // Arrange
    let mut activity = TurnActivity::default();
    let event = ActivityEvent {
        attempt_id: "one".into(),
        exit_code: None,
        id: "call".into(),
        kind: ActivityKind::Tool,
        name: "Read".into(),
        observed_at: SystemTime::UNIX_EPOCH,
        parent_id: None,
        status: ActivityStatus::Running,
    };

    // Act
    activity.observe(event.clone());
    activity.observe(ActivityEvent {
        status: ActivityStatus::Completed,
        ..event.clone()
    });
    activity.observe(ActivityEvent {
        attempt_id: "retry".into(),
        status: ActivityStatus::Failed,
        ..event.clone()
    });
    activity.observe(ActivityEvent {
        id: "skill".into(),
        kind: ActivityKind::Skill,
        name: "review".into(),
        ..event
    });

    // Assert
    assert_eq!(
        activity.summary(),
        "Tools: Read ×2 (1 failed)\nSkills: review ×1 (1 interrupted)\n"
    );
    assert_eq!(TurnActivity::default().summary(), "");
}

#[test]
fn summary_bounds_unique_calls_but_accepts_terminal_updates() {
    // Arrange
    let mut activity = TurnActivity::default();
    let mut event = ActivityEvent {
        attempt_id: "one".into(),
        exit_code: None,
        id: String::new(),
        kind: ActivityKind::Command,
        name: "shell".into(),
        observed_at: SystemTime::UNIX_EPOCH,
        parent_id: None,
        status: ActivityStatus::Interrupted,
    };

    // Act
    for index in 0..2049 {
        event.id = index.to_string();
        activity.observe(event.clone());
    }
    event.id = "0".into();
    event.status = ActivityStatus::Completed;
    activity.observe(event);

    // Assert
    assert_eq!(
        activity.summary(),
        "Tools: shell ×2048 (2047 interrupted)\n"
    );
}
