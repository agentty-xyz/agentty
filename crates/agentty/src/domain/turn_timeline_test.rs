//! Live-timeline reducer and grouping tests covering ordering, the call
//! digest, discards, failure tails, reasoning, the current action, backends
//! without narration, and bounds.

use std::time::Duration;

use ag_contracts::{
    ActivityEvent, ActivityFacts, ActivityFailure, ActivityKind, ActivityStatus, OutputEvent,
    OutputKind,
};

use super::{RUNNING_ROW_LIMIT, ToolEntry, input_preview, reasoning_title};
use crate::domain::turn_timeline::{
    CallStats, CurrentAction, RowResult, RowState, ToolRow, TurnTimeline,
};
use crate::test_support::activity_event;

/// Streams one complete message item `id` at `at` milliseconds.
fn message(timeline: &mut TurnTimeline, id: u64, text: &str, at: u64) {
    timeline.observe_output(
        OutputEvent::Started {
            id,
            kind: OutputKind::Message,
        },
        at,
    );
    timeline.observe_output(
        OutputEvent::Completed {
            id,
            text: text.into(),
        },
        at,
    );
}

/// Returns a finished call `id` of `kind` acting on `target`.
fn call(id: &str, kind: ActivityKind, target: &str, status: ActivityStatus) -> ActivityEvent {
    ActivityEvent {
        facts: Box::new(ActivityFacts {
            target: Some(target.into()),
            ..ActivityFacts::default()
        }),
        kind,
        status,
        ..activity_event(id)
    }
}

/// Returns the row of every call the timeline holds, in arrival order,
/// which the snapshot's digest picks from.
fn call_rows(timeline: &TurnTimeline) -> Vec<ToolRow> {
    timeline
        .entries
        .iter()
        .filter_map(|entry| entry.kind.tool())
        .map(ToolEntry::row)
        .collect()
}

#[test]
fn steps_keep_arrival_order_with_reasoning_under_the_narration_it_precedes() {
    // Arrange
    let mut timeline = TurnTimeline::default();

    // Act
    timeline.observe_output(
        OutputEvent::Started {
            id: 0,
            kind: OutputKind::Reasoning,
        },
        10_000,
    );
    timeline.observe_output(
        OutputEvent::Delta {
            id: 0,
            text: "**Planning**\nLook at the tests".into(),
        },
        10_000,
    );
    timeline.observe_output(
        OutputEvent::Completed {
            id: 0,
            text: "**Planning**\nLook at\nthe **tests**".into(),
        },
        16_500,
    );
    message(&mut timeline, 1, "Reading the tests.", 17_000);
    timeline.observe_activity(
        call("a", ActivityKind::Command, "ls", ActivityStatus::Completed),
        18_000,
    );
    message(&mut timeline, 2, "Now `editing`.", 81_000);
    let snapshot = timeline.snapshot();

    // Assert
    assert_eq!(snapshot.step_count, 2);
    assert_eq!(snapshot.calls.stats.total, 1);
    assert_eq!(snapshot.steps[0].at_seconds, 0);
    let thought = snapshot.steps[0].thought.as_ref().expect("thought");
    assert_eq!(thought.seconds, Some(6));
    assert_eq!(thought.summary, "Look at the tests");
    assert_eq!(
        snapshot.steps[0]
            .narration
            .as_ref()
            .map(|narration| &*narration.text),
        Some("Reading the tests.")
    );
    assert_eq!(snapshot.steps[1].at_seconds, 71);
    assert!(!timeline.is_empty());
}

/// One call update of [`digest_calls`]: its id, kind, target and name,
/// status, and the Unix milliseconds it arrives.
type CallUpdate = (
    &'static str,
    ActivityKind,
    &'static str,
    ActivityStatus,
    u64,
);

/// Returns the calls of a turn that ran every kind of call, with the Unix
/// milliseconds each update arrives: a read that starts first and finishes
/// last, finished calls of each kind, and three commands still running.
fn digest_calls() -> Vec<(ActivityEvent, u64)> {
    let first_read: CallUpdate = (
        "1",
        ActivityKind::Read,
        "a.rs",
        ActivityStatus::Running,
        1_000,
    );
    let last_read: CallUpdate = (
        "1",
        ActivityKind::Read,
        "a.rs",
        ActivityStatus::Completed,
        6_000,
    );
    let running = ["sleep 1", "sleep 2", "sleep 3"]
        .into_iter()
        .zip([3_000, 4_000, 5_000])
        .map(|(command, at)| -> CallUpdate {
            (
                command,
                ActivityKind::Command,
                command,
                ActivityStatus::Running,
                at,
            )
        });

    std::iter::once(first_read)
        .chain(finished_calls())
        .chain(running)
        .chain(std::iter::once(last_read))
        .map(|(id, kind, target, status, at)| {
            let event = ActivityEvent {
                name: target.into(),
                ..call(id, kind, target, status)
            };

            (event, at)
        })
        .collect()
}

/// Returns finished calls of every other kind, two of one MCP tool among
/// them, all arriving at Unix millisecond 2,000.
fn finished_calls() -> Vec<CallUpdate> {
    [
        ("2", ActivityKind::Command, "ls", ActivityStatus::Completed),
        ("3", ActivityKind::Search, "main", ActivityStatus::Completed),
        ("4", ActivityKind::List, ".", ActivityStatus::Completed),
        (
            "5",
            ActivityKind::WebSearch,
            "ratatui",
            ActivityStatus::Completed,
        ),
        (
            "6",
            ActivityKind::Mcp,
            "posthog/query",
            ActivityStatus::Completed,
        ),
        ("7", ActivityKind::Skill, "review", ActivityStatus::Failed),
        (
            "8",
            ActivityKind::Mcp,
            "posthog/query",
            ActivityStatus::Completed,
        ),
        (
            "9",
            ActivityKind::FileChange,
            "b.rs",
            ActivityStatus::Interrupted,
        ),
    ]
    .into_iter()
    .map(|(id, kind, target, status)| (id, kind, target, status, 2_000))
    .collect()
}
#[test]
fn the_digest_counts_every_call_and_shows_only_the_last_finished_and_the_running_ones() {
    // Arrange
    let mut timeline = TurnTimeline::default();
    message(&mut timeline, 0, "Working.", 0);

    // Act
    for (event, at) in digest_calls() {
        timeline.observe_activity(event, at);
    }
    let snapshot = timeline.snapshot();

    // Assert
    assert_eq!(
        snapshot.calls.stats,
        CallStats {
            commands: 4,
            edits: 1,
            failed: 1,
            interrupted: 1,
            listings: 1,
            reads: 1,
            running: 3,
            searches: 1,
            total: 12,
            used: vec![("posthog/query".into(), 2), ("review".into(), 1)],
            web_searches: 1,
        }
    );
    let latest = snapshot.calls.latest.as_ref().expect("latest call");
    assert_eq!((latest.verb, latest.target.as_str()), ("Read", "a.rs"));
    assert_eq!(latest.state, RowState::Succeeded);
    let running = snapshot
        .calls
        .running
        .iter()
        .map(|row| (row.target.as_str(), row.running_since))
        .collect::<Vec<_>>();
    assert_eq!(
        running,
        [
            ("sleep 1", Some(3_000)),
            ("sleep 2", Some(4_000)),
            ("sleep 3", Some(5_000)),
        ]
    );
    assert_eq!(snapshot.steps.len(), 1);
}

#[test]
fn the_digest_shows_at_most_the_earliest_running_calls_and_counts_the_rest() {
    // Arrange
    let mut timeline = TurnTimeline::default();

    // Act
    for index in 0..=RUNNING_ROW_LIMIT {
        timeline.observe_activity(activity_event(&format!("call-{index}")), 0);
    }
    let snapshot = timeline.snapshot();

    // Assert
    assert_eq!(snapshot.calls.latest, None);
    assert_eq!(snapshot.calls.running.len(), RUNNING_ROW_LIMIT);
    assert_eq!(snapshot.calls.stats.running, RUNNING_ROW_LIMIT + 1);
}

#[test]
fn discarded_items_drop_their_narration_and_calls_and_later_updates_still_apply() {
    // Arrange
    let mut timeline = TurnTimeline::default();
    message(&mut timeline, 0, "Kept.", 0);
    message(&mut timeline, 1, "Void.", 0);
    timeline.observe_output(
        OutputEvent::Started {
            id: 2,
            kind: OutputKind::ToolCall,
        },
        0,
    );
    timeline.observe_activity(activity_event("2"), 0);
    timeline.observe_activity(activity_event("9"), 0);
    timeline.observe_activity(activity_event("kept"), 0);

    // Act
    let unknown = timeline.observe_output(OutputEvent::Discard { ids: vec![7] }, 0);
    let discarded = timeline.observe_output(OutputEvent::Discard { ids: vec![1, 2, 9] }, 0);
    timeline.observe_activity(
        ActivityEvent {
            status: ActivityStatus::Completed,
            ..activity_event("kept")
        },
        0,
    );
    let snapshot = timeline.snapshot();

    // Assert
    assert!(!unknown);
    assert!(discarded);
    assert_eq!(snapshot.step_count, 1);
    assert_eq!(snapshot.calls.stats.total, 1);
    assert_eq!(
        snapshot.calls.latest.map(|row| row.state),
        Some(RowState::Succeeded)
    );
}

#[test]
fn a_streamed_tool_call_row_becomes_its_activity_row() {
    // Arrange
    let mut timeline = TurnTimeline::default();
    timeline.observe_output(
        OutputEvent::Started {
            id: 3,
            kind: OutputKind::ToolCall,
        },
        1_000,
    );

    // Act
    let preparing = timeline.snapshot();
    let delta = timeline.observe_output(
        OutputEvent::Delta {
            id: 3,
            text: "{".into(),
        },
        1_000,
    );
    let completed = timeline.observe_output(
        OutputEvent::Completed {
            id: 3,
            text: String::new(),
        },
        1_000,
    );
    timeline.observe_activity(
        ActivityEvent {
            input: Some("cargo test".into()),
            ..activity_event("3")
        },
        2_000,
    );
    let running = timeline.snapshot();

    // Assert
    assert!(!delta);
    assert!(!completed);
    assert_eq!(preparing.calls.running[0].verb, "Preparing");
    assert_eq!(
        preparing.current,
        Some(CurrentAction {
            label: "Preparing a tool call".into(),
            since: 1_000,
            text: "Preparing a tool call".into(),
        })
    );
    assert_eq!(running.calls.stats.total, 1);
    assert_eq!(running.calls.running[0].verb, "Running");
    assert_eq!(running.calls.running[0].target, "cargo test");
    assert_eq!(
        running.current,
        Some(CurrentAction {
            label: "Running a command".into(),
            since: 2_000,
            text: "Running cargo test".into(),
        })
    );
}

#[test]
fn failed_commands_report_their_exit_and_keep_their_last_output_lines() {
    // Arrange
    let mut timeline = TurnTimeline::default();
    let command = ActivityEvent {
        exit_code: Some(0),
        facts: Box::new(ActivityFacts {
            pipe_status: vec![101, 0],
            ..ActivityFacts::default()
        }),
        input: Some("cargo test 2>&1 |\n tail -20".into()),
        output: Some("one\n\ntwo\nthree\nfour\n".into()),
        status: ActivityStatus::Completed,
        ..activity_event("1")
    };

    // Act
    timeline.observe_activity(command, 0);
    let snapshot = timeline.snapshot();

    // Assert
    let row = snapshot.calls.latest.expect("finished command");
    assert_eq!(row.verb, "Ran");
    assert_eq!(row.state, RowState::Failed);
    assert_eq!(row.result, RowResult::Exit(101));
    assert_eq!(row.target, "cargo test 2>&1 | tail -20");
    assert!(row.target_is_command);
    assert_eq!(row.tail, ["two", "three", "four"]);
}

#[test]
fn calls_report_each_outcome_and_slow_durations() {
    // Arrange
    let mut timeline = TurnTimeline::default();
    let events = [
        ActivityEvent {
            exit_code: Some(0),
            facts: Box::new(ActivityFacts {
                duration: Some(Duration::from_millis(7_400)),
                ..ActivityFacts::default()
            }),
            status: ActivityStatus::Completed,
            ..activity_event("exit")
        },
        ActivityEvent {
            facts: Box::new(ActivityFacts {
                signal: Some(9),
                ..ActivityFacts::default()
            }),
            status: ActivityStatus::Completed,
            ..activity_event("signal")
        },
        ActivityEvent {
            facts: Box::new(ActivityFacts {
                failure: Some(ActivityFailure::Rejected),
                reason: Some("hunk 1 context mismatch".into()),
                target: Some("src/a.rs".into()),
                ..ActivityFacts::default()
            }),
            kind: ActivityKind::FileChange,
            status: ActivityStatus::Failed,
            ..activity_event("rejected")
        },
        ActivityEvent {
            status: ActivityStatus::Failed,
            ..activity_event("failed")
        },
        ActivityEvent {
            status: ActivityStatus::Interrupted,
            ..activity_event("interrupted")
        },
        ActivityEvent {
            kind: ActivityKind::Tool,
            name: "lookup".into(),
            status: ActivityStatus::Completed,
            ..activity_event("tool")
        },
    ];

    // Act
    for event in events {
        timeline.observe_activity(
            ActivityEvent {
                status: ActivityStatus::Running,
                ..event.clone()
            },
            1_000,
        );
        timeline.observe_activity(event, 7_000);
    }
    let rows = call_rows(&timeline);

    // Assert
    let results = rows
        .iter()
        .map(|row| row.result.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        results,
        [
            RowResult::Exit(0),
            RowResult::Signal(9),
            RowResult::Failed {
                label: "rejected",
                reason: Some("hunk 1 context mismatch".into()),
            },
            RowResult::Failed {
                label: "failed",
                reason: None,
            },
            RowResult::Interrupted,
            RowResult::None,
        ]
    );
    assert_eq!(rows[0].duration_seconds, Some(7));
    assert_eq!(rows[1].duration_seconds, Some(6));
    assert_eq!(rows[4].state, RowState::Interrupted);
    assert_eq!(rows[5].verb, "Called");
}

#[test]
fn targets_and_verbs_follow_the_activity_kind() {
    // Arrange
    let mut timeline = TurnTimeline::default();
    let events = [
        ActivityEvent {
            input: Some(r#"{"query":"SELECT 1"}"#.into()),
            kind: ActivityKind::Mcp,
            name: "posthog/query".into(),
            ..activity_event("mcp")
        },
        ActivityEvent {
            kind: ActivityKind::Skill,
            name: "review".into(),
            ..activity_event("skill")
        },
        ActivityEvent {
            kind: ActivityKind::Tool,
            name: "lookup".into(),
            ..activity_event("tool")
        },
        ActivityEvent {
            input: Some(r#"{"query":"ratatui"}"#.into()),
            kind: ActivityKind::WebSearch,
            ..activity_event("web")
        },
        ActivityEvent {
            facts: Box::new(ActivityFacts {
                target: Some("{ cargo fmt;".into()),
                ..ActivityFacts::default()
            }),
            ..activity_event("command")
        },
        ActivityEvent {
            input: Some(r#"{"path":"src/a.rs"}"#.into()),
            kind: ActivityKind::FileChange,
            ..activity_event("edit")
        },
    ];

    // Act
    for event in events {
        timeline.observe_activity(event, 0);
    }
    let snapshot = timeline.snapshot();
    let rows = call_rows(&timeline);

    // Assert
    let described = rows
        .iter()
        .map(|row| (row.verb, row.target.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        described,
        [
            ("MCP", "posthog/query · SELECT 1"),
            ("Skill", "review"),
            ("Calling", "lookup"),
            ("Searching", "ratatui"),
            ("Running", "{ cargo fmt;"),
            ("Edit", "src/a.rs"),
        ]
    );
    // The progress text names the target; the status line only the kind,
    // of the latest call that has a row.
    let current = snapshot.current.expect("running tool");
    assert_eq!(current.text, "Calling lookup");
    assert_eq!(current.label, "Calling a tool");
    let labels = [
        ActivityKind::Read,
        ActivityKind::Search,
        ActivityKind::WebSearch,
        ActivityKind::List,
        ActivityKind::Mcp,
        ActivityKind::Skill,
        ActivityKind::Tool,
        ActivityKind::FileChange,
    ]
    .map(|kind| {
        let mut running = TurnTimeline::default();
        running.observe_activity(call("x", kind, "", ActivityStatus::Running), 0);

        running
            .current_action()
            .map_or_default(|current| current.label)
    });
    assert_eq!(
        labels,
        [
            "Reading a file",
            "Searching",
            "Searching the web",
            "Listing files",
            "Calling an MCP tool",
            "Using a skill",
            "Calling a tool",
            "Editing a file",
        ]
    );
}

#[test]
fn the_current_action_prefers_an_open_reasoning_title_then_a_running_call_then_progress() {
    // Arrange
    let mut timeline = TurnTimeline::default();
    let progress_only = timeline.observe_progress("Waiting for model".into(), 500);
    let repeated = timeline.observe_progress("Waiting for model".into(), 600);
    let waiting = timeline.current_action();
    timeline.observe_activity(
        call("1", ActivityKind::Read, "a.rs", ActivityStatus::Running),
        1_000,
    );
    let reading = timeline.current_action();
    for (id, text) in [
        (0, "untitled thinking"),
        (1, "**Weighing the options**\nbody"),
    ] {
        timeline.observe_output(
            OutputEvent::Started {
                id,
                kind: OutputKind::Reasoning,
            },
            2_000,
        );
        timeline.observe_output(
            OutputEvent::Delta {
                id,
                text: text.into(),
            },
            2_000,
        );
    }

    // Act
    let reasoning = timeline.current_action();
    timeline.observe_output(
        OutputEvent::Completed {
            id: 1,
            text: "**Weighing the options**".into(),
        },
        3_000,
    );
    let after_reasoning = timeline.current_action();
    timeline.interrupt_running(4_000);
    let after_interrupt = timeline.current_action();
    let snapshot = timeline.snapshot();

    // Assert
    assert!(progress_only);
    assert!(repeated);
    assert_eq!(
        waiting.map(|current| (current.text, current.since)),
        Some(("Waiting for model".to_string(), 600))
    );
    assert_eq!(
        reading.map(|current| current.text).as_deref(),
        Some("Reading a.rs")
    );
    assert_eq!(
        reasoning,
        Some(CurrentAction {
            label: "Weighing the options".into(),
            since: 2_000,
            text: "Weighing the options".into(),
        })
    );
    assert_eq!(
        after_reasoning.map(|current| current.text).as_deref(),
        Some("Reading a.rs")
    );
    assert_eq!(
        after_interrupt.map(|current| current.text).as_deref(),
        Some("Waiting for model")
    );
    let thoughts = snapshot
        .steps
        .iter()
        .filter_map(|step| step.thought.as_ref())
        .map(|thought| (thought.seconds, thought.summary.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        thoughts,
        [
            (None, "untitled thinking"),
            (Some(1), "Weighing the options")
        ]
    );
    assert!(!timeline.interrupt_running(5_000));
}

#[test]
fn backends_without_narration_keep_their_calls_in_one_step_without_a_header() {
    // Arrange
    let mut timeline = TurnTimeline::default();

    // Act
    for id in ["a", "b", "c"] {
        timeline.observe_activity(activity_event(id), 0);
    }
    let snapshot = timeline.snapshot();

    // Assert
    assert_eq!(snapshot.steps.len(), 1);
    assert_eq!(snapshot.steps[0].narration, None);
    assert_eq!(snapshot.calls.running.len(), 3);
}

#[test]
fn the_validated_answer_replaces_its_streamed_message_or_follows_the_calls() {
    // Arrange
    let mut timeline = TurnTimeline::default();
    timeline.observe_output(
        OutputEvent::Started {
            id: 0,
            kind: OutputKind::Message,
        },
        0,
    );
    timeline.observe_output(
        OutputEvent::Delta {
            id: 0,
            text: "{\"answer\":".into(),
        },
        0,
    );
    let unknown_delta = timeline.observe_output(
        OutputEvent::Delta {
            id: 5,
            text: "x".into(),
        },
        0,
    );
    let unknown_completion = timeline.observe_output(
        OutputEvent::Completed {
            id: 5,
            text: "x".into(),
        },
        0,
    );

    // Act
    timeline.observe_output(
        OutputEvent::Answer {
            replaces: Some(0),
            text: "Done.".into(),
        },
        0,
    );
    let late_delta = timeline.observe_output(
        OutputEvent::Delta {
            id: 0,
            text: "late".into(),
        },
        0,
    );
    timeline.observe_activity(activity_event("call"), 0);
    timeline.observe_output(
        OutputEvent::Answer {
            replaces: None,
            text: "Second.".into(),
        },
        0,
    );
    let snapshot = timeline.snapshot();

    // Assert
    assert!(!unknown_delta);
    assert!(!unknown_completion);
    assert!(!late_delta);
    let narrations = snapshot
        .steps
        .iter()
        .filter_map(|step| step.narration.as_ref())
        .map(|narration| (&*narration.text, narration.is_answer))
        .collect::<Vec<_>>();
    assert_eq!(narrations, [("Done.", true), ("Second.", true)]);
}

#[test]
fn calls_steps_and_text_stay_bounded() {
    // Arrange
    let mut timeline = TurnTimeline::default();
    timeline.observe_output(
        OutputEvent::Started {
            id: 0,
            kind: OutputKind::Message,
        },
        0,
    );
    let filled = timeline.observe_output(
        OutputEvent::Delta {
            id: 0,
            text: "x".repeat(20_000),
        },
        0,
    );
    let past_limit = timeline.observe_output(
        OutputEvent::Delta {
            id: 0,
            text: "y".into(),
        },
        0,
    );

    // Act
    for index in 0..2_047 {
        timeline.observe_activity(activity_event(&format!("call-{index}")), 0);
    }
    let announced = timeline.observe_output(
        OutputEvent::Started {
            id: 1,
            kind: OutputKind::ToolCall,
        },
        0,
    );
    let announced_past_limit = timeline.observe_output(
        OutputEvent::Started {
            id: 2,
            kind: OutputKind::ToolCall,
        },
        0,
    );
    let ignored_activity = timeline.observe_activity(activity_event("2"), 0);
    let dropped = timeline.observe_activity(activity_event("one-more"), 0);
    let tools = timeline.snapshot();
    for id in 3..=452 {
        message(&mut timeline, id, "step", 0);
    }
    let snapshot = timeline.snapshot();

    // Assert
    assert!(filled);
    assert!(!past_limit);
    assert!(announced);
    assert!(!announced_past_limit);
    assert!(!ignored_activity);
    assert!(!dropped);
    assert_eq!(tools.calls.stats.total, 2_048);
    assert_eq!(tools.calls.running.len(), RUNNING_ROW_LIMIT);
    assert_eq!(
        tools.steps[0]
            .narration
            .as_ref()
            .map(|narration| narration.text.chars().count()),
        Some(16 * 1024)
    );
    assert_eq!(snapshot.step_count, 451);
    assert_eq!(snapshot.hidden_steps, 51);
    assert_eq!(snapshot.steps.len(), 400);
}

#[test]
fn the_oldest_steps_are_evicted_once_the_timeline_holds_too_many_entries() {
    // Arrange
    let mut timeline = TurnTimeline::default();

    // Act
    for id in 0..4_097 {
        message(&mut timeline, id, &format!("Step {id}."), id * 1_000);
    }
    let snapshot = timeline.snapshot();
    timeline.observe_output(
        OutputEvent::Completed {
            id: 4_096,
            text: "Updated.".into(),
        },
        0,
    );
    let updated = timeline.snapshot();

    // Assert
    assert_eq!(snapshot.step_count, 4_097);
    assert_eq!(snapshot.hidden_steps, 4_097 - 400);
    assert_eq!(snapshot.steps[0].at_seconds, 3_697);
    assert_eq!(
        updated
            .steps
            .last()
            .and_then(|step| step.narration.as_ref())
            .map(|narration| &*narration.text),
        Some("Updated.")
    );
}

#[test]
fn narrated_steps_count_only_evicted_steps_that_narrated() {
    // Arrange
    let mut timeline = TurnTimeline::default();
    timeline.observe_activity(
        call("a", ActivityKind::Command, "ls", ActivityStatus::Completed),
        0,
    );

    // Act
    for id in 0..4_096 {
        message(&mut timeline, id, &format!("Step {id}."), id * 1_000);
    }
    let snapshot = timeline.snapshot();

    // Assert: the 4,097th entry evicts the oldest steps, the call-only one
    // first, and only the evicted steps that narrated still count. The
    // evicted call still counts in the totals.
    assert_eq!(snapshot.step_count, 4_097);
    assert_eq!(timeline.narrated_steps(), Some(4_096));
    assert_eq!(
        (snapshot.calls.stats.total, snapshot.calls.stats.commands),
        (1, 1)
    );
    assert_eq!(snapshot.calls.latest, None);
}

#[test]
fn a_call_still_running_when_its_step_is_evicted_settles_once() {
    // Arrange
    let mut timeline = TurnTimeline::default();
    timeline.observe_activity(
        call("a", ActivityKind::Command, "sleep", ActivityStatus::Running),
        0,
    );
    for id in 0..4_096 {
        message(&mut timeline, id, &format!("Step {id}."), id * 1_000);
    }
    let evicted = timeline.snapshot();

    // Act
    timeline.observe_activity(
        call("a", ActivityKind::Command, "sleep", ActivityStatus::Failed),
        5_000_000,
    );
    let settled = timeline.snapshot();

    // Assert: the running call survives eviction without its step
    // counting twice, and its result replaces it instead of adding a call.
    assert_eq!(evicted.step_count, 4_097);
    assert_eq!(timeline.narrated_steps(), Some(4_096));
    assert_eq!(
        (evicted.calls.stats.total, evicted.calls.stats.running),
        (1, 1)
    );
    assert_eq!(evicted.calls.running.len(), 1);
    assert_eq!(settled.step_count, 4_097);
    assert_eq!(
        (
            settled.calls.stats.total,
            settled.calls.stats.running,
            settled.calls.stats.failed,
            settled.calls.stats.commands,
        ),
        (1, 0, 1, 1)
    );
    assert_eq!(
        settled.calls.latest.map(|row| row.state),
        Some(RowState::Failed)
    );
}

#[test]
fn the_status_line_names_a_running_call_that_has_a_row() {
    // Arrange
    let mut timeline = TurnTimeline::default();
    for index in 0..RUNNING_ROW_LIMIT {
        timeline.observe_activity(
            call(
                &format!("read-{index}"),
                ActivityKind::Read,
                &format!("{index}.rs"),
                ActivityStatus::Running,
            ),
            0,
        );
    }

    // Act
    timeline.observe_activity(
        call(
            "command",
            ActivityKind::Command,
            "ls",
            ActivityStatus::Running,
        ),
        1_000,
    );
    let snapshot = timeline.snapshot();

    // Assert
    let last_row = snapshot.calls.running.last().map(|row| row.target.clone());
    assert_eq!(last_row.as_deref(), Some("2.rs"));
    assert_eq!(
        snapshot.current.map(|current| current.text).as_deref(),
        Some("Reading 2.rs")
    );
}

#[test]
fn snapshots_reuse_unchanged_steps_and_compare_by_revision() {
    // Arrange
    let mut timeline = TurnTimeline::default();
    message(&mut timeline, 0, "First.", 0);
    message(&mut timeline, 1, "Second.", 0);
    let first = timeline.snapshot();

    // Act
    let unchanged = timeline.snapshot();
    timeline.observe_output(
        OutputEvent::Completed {
            id: 1,
            text: "Second, revised.".into(),
        },
        0,
    );
    let changed = timeline.snapshot();
    timeline.observe_activity(activity_event("call"), 0);
    let called = timeline.snapshot();
    let other = TurnTimeline::default().snapshot();

    // Assert
    assert_eq!(first, unchanged);
    assert_ne!(first, changed);
    assert_ne!(changed, called);
    assert_ne!(other, TurnTimeline::default().snapshot());
    assert!(std::sync::Arc::ptr_eq(&first.steps[0], &changed.steps[0]));
    assert!(!std::sync::Arc::ptr_eq(&first.steps[1], &changed.steps[1]));
    assert!(changed.steps[1].header_revision > first.steps[1].header_revision);
    // A call changes only the digest, so its step is reused.
    assert!(std::sync::Arc::ptr_eq(&changed.steps[1], &called.steps[1]));
    assert_eq!(called.calls.stats.running, 1);
}

#[test]
fn regrouping_after_a_discard_keeps_finished_reasoning_answers_and_prepared_calls() {
    // Arrange
    let mut timeline = TurnTimeline::default();
    timeline.observe_output(
        OutputEvent::Started {
            id: 0,
            kind: OutputKind::Reasoning,
        },
        0,
    );
    let filled = timeline.observe_output(
        OutputEvent::Delta {
            id: 0,
            text: "z".repeat(5_000),
        },
        0,
    );
    let past_limit = timeline.observe_output(
        OutputEvent::Delta {
            id: 0,
            text: "**Late title**".into(),
        },
        0,
    );
    timeline.observe_output(
        OutputEvent::Completed {
            id: 0,
            text: "**Done thinking**".into(),
        },
        1_000,
    );
    timeline.observe_output(
        OutputEvent::Answer {
            replaces: None,
            text: "Answer.".into(),
        },
        2_000,
    );
    message(&mut timeline, 1, "Void.", 3_000);
    timeline.observe_output(
        OutputEvent::Started {
            id: 2,
            kind: OutputKind::ToolCall,
        },
        3_000,
    );

    // Act
    let discarded = timeline.observe_output(OutputEvent::Discard { ids: vec![1] }, 4_000);
    let preparing = timeline.current_action().map(|current| current.text);
    let interrupted = timeline.interrupt_running(5_000);
    let snapshot = timeline.snapshot();

    // Assert
    assert!(filled);
    assert!(!past_limit);
    assert!(discarded);
    assert_eq!(preparing.as_deref(), Some("Preparing a tool call"));
    assert!(!interrupted);
    assert_eq!(timeline.current_action(), None);
    assert_eq!(snapshot.step_count, 1);
    assert_eq!(
        snapshot.steps[0]
            .thought
            .as_ref()
            .map(|thought| thought.summary.as_str()),
        Some("Done thinking")
    );
    assert_eq!(
        snapshot.steps[0]
            .narration
            .as_ref()
            .map(|narration| &*narration.text),
        Some("Answer.")
    );
    assert_eq!(snapshot.calls.running[0].verb, "Preparing");
}

#[test]
fn a_repeated_progress_text_restarts_its_timer_after_a_call_finishes() {
    // Arrange
    let mut timeline = TurnTimeline::default();
    timeline.observe_progress("Waiting for model".into(), 500);
    timeline.observe_activity(
        ActivityEvent {
            status: ActivityStatus::Completed,
            ..activity_event("call")
        },
        1_000,
    );

    // Act
    timeline.observe_progress("Waiting for model".into(), 5_000);

    // Assert
    assert_eq!(
        timeline.current_action().map(|current| current.since),
        Some(5_000)
    );
}

#[test]
fn failure_kinds_name_their_result_and_running_explorations_name_their_action() {
    // Arrange
    let failed = |id: &str, failure: ActivityFailure| ActivityEvent {
        facts: Box::new(ActivityFacts {
            failure: Some(failure),
            ..ActivityFacts::default()
        }),
        kind: ActivityKind::Tool,
        status: ActivityStatus::Failed,
        ..activity_event(id)
    };
    let mut timeline = TurnTimeline::default();
    for event in [
        failed("denied", ActivityFailure::Denied),
        failed("timed-out", ActivityFailure::TimedOut),
        failed("unavailable", ActivityFailure::Unavailable),
        failed("error", ActivityFailure::Error),
        ActivityEvent {
            kind: ActivityKind::WebSearch,
            status: ActivityStatus::Completed,
            ..activity_event("web")
        },
    ] {
        timeline.observe_activity(event, 0);
    }

    // Act
    let rows = call_rows(&timeline);
    let actions = [
        (ActivityKind::Search, "needle"),
        (ActivityKind::List, "src"),
    ]
    .map(|(kind, target)| {
        let mut exploring = TurnTimeline::default();
        exploring.observe_activity(call("x", kind, target, ActivityStatus::Running), 0);

        exploring.current_action().map(|current| current.text)
    });

    // Assert
    let labels = rows[..4]
        .iter()
        .map(|row| match &row.result {
            RowResult::Failed { label, .. } => *label,
            _ => "",
        })
        .collect::<Vec<_>>();
    assert_eq!(labels, ["denied", "timed out", "unavailable", "failed"]);
    assert_eq!(rows[4].verb, "Searched");
    assert_eq!(
        actions,
        [
            Some("Searching needle".to_string()),
            Some("Listing src".to_string())
        ]
    );
}

#[test]
fn numeric_activity_ids_join_only_a_streamed_call_still_being_prepared() {
    // Arrange
    let mut timeline = TurnTimeline::default();
    message(&mut timeline, 0, "Narration.", 0);
    timeline.observe_output(
        OutputEvent::Started {
            id: 1,
            kind: OutputKind::ToolCall,
        },
        0,
    );

    // Act
    timeline.observe_activity(activity_event("0"), 0);
    timeline.observe_activity(activity_event("1"), 0);
    timeline.observe_activity(
        ActivityEvent {
            attempt_id: "retry".into(),
            ..activity_event("1")
        },
        0,
    );
    let snapshot = timeline.snapshot();

    // Assert
    assert_eq!(snapshot.calls.stats.total, 3);
    assert_eq!(
        snapshot.steps[0]
            .narration
            .as_ref()
            .map(|narration| &*narration.text),
        Some("Narration.")
    );
}

#[test]
fn reasoning_titles_are_bold_lines_only() {
    // Arrange, Act, Assert
    assert_eq!(
        reasoning_title("intro\n  **Title here**  \nbody").as_deref(),
        Some("Title here")
    );
    assert_eq!(reasoning_title("**not closed\n****"), None);
    assert_eq!(reasoning_title("plain"), None);
}

#[test]
fn readable_inputs_extract_useful_fields_and_hide_payloads() {
    // Arrange, Act, Assert
    for (input, expected) in [
        ("cargo\n test", Some("cargo test")),
        ("true", Some("true")),
        (r#"{"file_path":"src/\u001bmain.rs"}"#, Some("src/main.rs")),
        (
            r#"{"command":"cargo test","description":"hidden"}"#,
            Some("cargo test"),
        ),
        (r#"{"CommandLine":"pwd"}"#, Some("pwd")),
        (r#"{"path":"README.md"}"#, Some("README.md")),
        (r#"{"TargetFile":"src/main.rs"}"#, Some("src/main.rs")),
        (r#"{"target_file":"src/main.rs"}"#, Some("src/main.rs")),
        (r#"{"query":"search term"}"#, Some("search term")),
        (r#"{"pattern":"fn main"}"#, Some("fn main")),
        (r#""quoted input""#, Some("quoted input")),
        (r#"{"content":"hidden"}"#, None),
        (r#"{"file_path":"truncated"#, None),
        (r#"[{"content":"hidden"}]"#, None),
    ] {
        assert_eq!(
            input_preview(input, ActivityKind::Tool).as_deref(),
            expected,
            "{input}"
        );
    }
    assert_eq!(
        input_preview("{\n cargo fmt;\n}", ActivityKind::Command).as_deref(),
        Some("{ cargo fmt; }")
    );
}
