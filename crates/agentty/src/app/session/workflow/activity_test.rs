//! Turn activity tests covering the turn summary and how activity feeds the
//! live timeline.

use ag_contracts::{
    ActivityEvent, ActivityFacts, ActivityKind, ActivityStatus, OutputEvent, OutputKind,
};
use tokio::sync::mpsc;

use super::TurnActivity;
use crate::app::AppEvent;
use crate::domain::session::SessionId;
use crate::domain::session_usage::{LiveUsageKey, LiveUsageRun};
use crate::test_support::activity_event;

#[test]
fn summary_counts_invocations_not_updates_and_separates_attempts() {
    // Arrange
    let mut activity = TurnActivity::default();
    let event = ActivityEvent {
        attempt_id: "one".into(),
        kind: ActivityKind::Tool,
        name: "Read".into(),
        ..activity_event("call")
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
        status: ActivityStatus::Interrupted,
        ..event
    });

    // Assert
    assert_eq!(
        activity.summary().render(None),
        "turn-summary v1\n3 tools\nFailed\t✗\tRead\tfailed\t\nUsed\tRead ×2 · review ×1\n─ Worked \
         3 tools · 1 failed (1 error) · 1 interrupted"
    );
    assert_eq!(TurnActivity::default().summary().render(None), "");
}

#[test]
fn summary_bounds_unique_calls_but_accepts_terminal_updates() {
    // Arrange
    let mut activity = TurnActivity::default();
    let mut event = ActivityEvent {
        status: ActivityStatus::Interrupted,
        ..activity_event("")
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
        activity.summary().render(None),
        "turn-summary v1\n2048 tools\nRan\t2048 commands\n─ Worked 2048 tools · 2047 interrupted"
    );
}

#[test]
fn piped_failures_count_as_failed() {
    // Arrange
    let mut activity = TurnActivity::default();

    // Act
    activity.observe(ActivityEvent {
        exit_code: Some(0),
        facts: Box::new(ActivityFacts {
            pipe_status: vec![101, 0],
            ..ActivityFacts::default()
        }),
        input: Some("cargo test | tail".into()),
        name: "bash".into(),
        status: ActivityStatus::Completed,
        ..activity_event("call")
    });

    // Assert
    assert_eq!(
        activity.summary().render(None),
        "turn-summary v1\n1 tool\nFailed\t✗\tcargo test | tail\texit 101\t\nRan\t1 command\n─ \
         Worked 1 tool · 1 failed (1 nonzero exit)"
    );
}

#[test]
fn live_activity_feeds_the_timeline_and_interrupts_open_calls_when_events_end() {
    // Arrange
    let mut activity = TurnActivity::default();
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let session_id = SessionId::from("session");

    // Act
    activity.start(0);
    activity.observe_output(
        OutputEvent::Started {
            id: 0,
            kind: OutputKind::Message,
        },
        1_000,
        &sender,
        &session_id,
    );
    activity.observe_live(activity_event("call"), 2_000, &sender, &session_id);
    let running_progress = activity.current_action_text();
    activity.finish_live(63_000, &sender, &session_id);
    let turn = match receiver.try_recv() {
        Ok(AppEvent::SessionTurnReady { turn, .. }) => Some(turn),
        _ => None,
    }
    .expect("timeline notification");
    let snapshot = turn.snapshot();
    activity.clear_live(&sender, &session_id);
    activity.clear_live(&sender, &session_id);

    // Assert
    assert_eq!(running_progress.as_deref(), Some("Running"));
    assert_eq!(
        activity.summary().render(None),
        "turn-summary v1\n1 step · 1 tool · 1m 3s\nRan\t1 command\n─ Worked 1m 3s · 1 step · 1 \
         tool · 1 interrupted"
    );
    assert_eq!(snapshot.steps[0].rows.len(), 1);
    assert_eq!(
        std::iter::from_fn(|| receiver.try_recv().ok()).collect::<Vec<_>>(),
        [AppEvent::SessionTurnCleared {
            key: LiveUsageKey {
                run: LiveUsageRun::SessionTurn,
                session_id,
            },
        }]
    );
}

#[test]
fn current_action_names_the_running_call_and_falls_back_to_progress() {
    // Arrange
    let mut activity = TurnActivity::default();
    let (sender, _receiver) = mpsc::unbounded_channel();
    let session_id = SessionId::from("session");
    let command = ActivityEvent {
        input: Some("cargo test".into()),
        ..activity_event("call")
    };

    // Act
    activity.observe_progress("Waiting for model".into(), 0, &sender, &session_id);
    let before_timeline = activity.current_action_text();
    activity.observe_live(command.clone(), 0, &sender, &session_id);
    let running = activity.current_action_text();
    activity.observe_live(
        ActivityEvent {
            status: ActivityStatus::Completed,
            ..command
        },
        0,
        &sender,
        &session_id,
    );
    let finished = activity.current_action_text();

    // Assert
    assert_eq!(before_timeline, None);
    assert_eq!(running.as_deref(), Some("Running cargo test"));
    assert_eq!(finished.as_deref(), Some("Waiting for model"));
}
