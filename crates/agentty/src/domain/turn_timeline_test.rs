//! Live-timeline reducer and grouping tests covering ordering, exploration
//! folding, reads folded into edits, discards, failure tails, reasoning,
//! the current action, backends without narration, and bounds.

use std::time::Duration;

use ag_contracts::{
    ActivityEvent, ActivityFacts, ActivityFailure, ActivityKind, ActivityStatus, DiffStat,
    LineRange, OutputEvent, OutputKind,
};

use super::{input_preview, reasoning_title};
use crate::domain::turn_timeline::{
    CurrentAction, ExplorePart, RowResult, RowState, TimelineRow, TimelineSnapshot, ToolRow,
    TurnTimeline,
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

/// Returns the rows of every step.
fn rows(snapshot: &TimelineSnapshot) -> Vec<&TimelineRow> {
    snapshot.steps.iter().flat_map(|step| &step.rows).collect()
}

/// Returns how many rows every visible step holds.
fn row_count(snapshot: &TimelineSnapshot) -> usize {
    rows(snapshot).len()
}

/// Returns an exploration row's calls, parts, and whether it runs.
fn explored(row: &TimelineRow) -> Option<(usize, &[ExplorePart], bool)> {
    match row {
        TimelineRow::Explored(group) => Some((
            group.calls,
            group.parts.as_slice(),
            group.running_since.is_some(),
        )),
        TimelineRow::Tool(_) => None,
    }
}

/// Returns the tool row at `index` among every step's rows.
fn tool_row(snapshot: &TimelineSnapshot, index: usize) -> &ToolRow {
    match rows(snapshot)[index] {
        TimelineRow::Tool(row) => row,
        TimelineRow::Explored(_) => unreachable!("expected a tool row"),
    }
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
    assert_eq!(row_count(&snapshot), 1);
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
    assert_eq!(snapshot.steps[1].rows, []);
    assert!(!timeline.is_empty());
}

#[test]
fn consecutive_successful_explorations_fold_and_failures_stay_apart() {
    // Arrange
    let mut timeline = TurnTimeline::default();
    message(&mut timeline, 0, "Exploring.", 0);

    // Act
    for event in [
        call(
            "1",
            ActivityKind::Read,
            "src/model.rs",
            ActivityStatus::Completed,
        ),
        call(
            "2",
            ActivityKind::Read,
            "src/model.rs",
            ActivityStatus::Completed,
        ),
        call(
            "3",
            ActivityKind::Search,
            "fn main",
            ActivityStatus::Completed,
        ),
        call("4", ActivityKind::List, ".", ActivityStatus::Completed),
        call(
            "5",
            ActivityKind::Read,
            "crates/composer.rs",
            ActivityStatus::Completed,
        ),
        ActivityEvent {
            facts: Box::new(ActivityFacts {
                failure: Some(ActivityFailure::Rejected),
                reason: Some("outside the repository".into()),
                target: Some("/etc/passwd".into()),
                ..ActivityFacts::default()
            }),
            ..call("6", ActivityKind::Read, "", ActivityStatus::Failed)
        },
        call("7", ActivityKind::Search, "", ActivityStatus::Completed),
        call("8", ActivityKind::Read, "notes.md", ActivityStatus::Running),
    ] {
        timeline.observe_activity(event, 0);
    }
    let snapshot = timeline.snapshot();

    // Assert
    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 3);
    let parts = [
        ExplorePart {
            items: vec!["model.rs".into(), "composer.rs".into()],
            label: "read",
        },
        ExplorePart {
            items: vec!["\"fn main\"".into()],
            label: "search",
        },
        ExplorePart {
            items: vec![".".into()],
            label: "list",
        },
    ];
    assert_eq!(explored(rows[0]), Some((5, &parts[..], false)));
    let failed = tool_row(&snapshot, 1);
    assert_eq!(failed.verb, "Read");
    assert_eq!(failed.state, RowState::Failed);
    assert_eq!(
        failed.result,
        RowResult::Failed {
            label: "rejected",
            reason: Some("outside the repository".into()),
        }
    );
    let running = [ExplorePart {
        items: vec!["notes.md".into()],
        label: "read",
    }];
    assert_eq!(explored(rows[2]), Some((2, &running[..], true)));
    assert_eq!(rows[2].running_since(), Some(0));
}

#[test]
fn a_read_directly_followed_by_an_edit_of_the_same_file_folds_into_the_edit() {
    // Arrange
    let mut timeline = TurnTimeline::default();
    let ranged_read = |id: &str, path: &str, last: Option<u64>| ActivityEvent {
        facts: Box::new(ActivityFacts {
            lines: Some(LineRange { first: 10, last }),
            target: Some(path.into()),
            ..ActivityFacts::default()
        }),
        ..call(id, ActivityKind::Read, path, ActivityStatus::Completed)
    };
    let edit = |id: &str, path: &str| ActivityEvent {
        facts: Box::new(ActivityFacts {
            diff_stat: Some(DiffStat {
                added: 38,
                removed: 4,
            }),
            target: Some(path.into()),
            ..ActivityFacts::default()
        }),
        ..call(
            id,
            ActivityKind::FileChange,
            path,
            ActivityStatus::Completed,
        )
    };

    // Act
    for event in [
        call(
            "0",
            ActivityKind::Search,
            "needle",
            ActivityStatus::Completed,
        ),
        ranged_read("1", "./src/a.rs", Some(42)),
        edit("2", "/repo/src/a.rs"),
        ranged_read("3", "src/b.rs", None),
        edit("4", "src/b.rs"),
        call(
            "5",
            ActivityKind::Read,
            "src/c.rs",
            ActivityStatus::Completed,
        ),
        edit("6", "src/c.rs"),
        call(
            "7",
            ActivityKind::Read,
            "src/d.rs",
            ActivityStatus::Completed,
        ),
        edit("8", "src/other.rs"),
    ] {
        timeline.observe_activity(event, 0);
    }
    let snapshot = timeline.snapshot();

    // Assert
    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 6);
    assert!(matches!(rows[0], TimelineRow::Explored(group) if group.calls == 1));
    assert_eq!(tool_row(&snapshot, 1).read_first.as_deref(), Some(":10-42"));
    assert_eq!(
        tool_row(&snapshot, 1).result,
        RowResult::DiffStat {
            added: 38,
            removed: 4
        }
    );
    assert_eq!(tool_row(&snapshot, 2).read_first.as_deref(), Some(":10-"));
    assert_eq!(tool_row(&snapshot, 3).read_first.as_deref(), Some(""));
    assert!(matches!(rows[4], TimelineRow::Explored(group) if group.calls == 1));
    assert_eq!(tool_row(&snapshot, 5).read_first, None);
    assert_eq!(tool_row(&snapshot, 5).verb, "Edit");
}

#[test]
fn discarded_items_drop_their_narration_and_tool_rows_and_later_updates_still_apply() {
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
    assert_eq!(row_count(&snapshot), 1);
    assert_eq!(tool_row(&snapshot, 0).state, RowState::Succeeded);
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
    assert_eq!(tool_row(&preparing, 0).verb, "Preparing");
    assert_eq!(
        preparing.current,
        Some(CurrentAction {
            label_width: 9,
            since: 1_000,
            text: "Preparing a tool call".into(),
        })
    );
    assert_eq!(row_count(&running), 1);
    assert_eq!(tool_row(&running, 0).verb, "Running");
    assert_eq!(tool_row(&running, 0).target, "cargo test");
    assert_eq!(
        running.current,
        Some(CurrentAction {
            label_width: 7,
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
    let row = tool_row(&snapshot, 0);
    assert_eq!(row.verb, "Ran");
    assert_eq!(row.state, RowState::Failed);
    assert_eq!(row.result, RowResult::Exit(101));
    assert_eq!(row.target, "cargo test 2>&1 | tail -20");
    assert!(row.target_is_command);
    assert_eq!(row.tail, ["two", "three", "four"]);
}

#[test]
fn rows_report_each_outcome_and_slow_durations() {
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
    let snapshot = timeline.snapshot();

    // Assert
    let results = (0..6)
        .map(|index| tool_row(&snapshot, index).result.clone())
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
    assert_eq!(tool_row(&snapshot, 0).duration_seconds, Some(7));
    assert_eq!(tool_row(&snapshot, 1).duration_seconds, Some(6));
    assert_eq!(tool_row(&snapshot, 4).state, RowState::Interrupted);
    assert_eq!(tool_row(&snapshot, 5).verb, "Called");
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

    // Assert
    let described = (0..6)
        .map(|index| {
            let row = tool_row(&snapshot, index);
            (row.verb, row.target.as_str())
        })
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
    assert_eq!(
        snapshot.current.map(|current| current.text).as_deref(),
        Some("Editing src/a.rs")
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
            label_width: 20,
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
fn backends_without_narration_show_rows_in_one_step_without_a_header() {
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
    assert_eq!(snapshot.steps[0].rows.len(), 3);
}

#[test]
fn the_validated_answer_replaces_its_streamed_message_or_follows_the_rows() {
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
fn rows_steps_and_text_stay_bounded() {
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
    assert_eq!(row_count(&tools), 2_048);
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
    // first, and only the evicted steps that narrated still count.
    assert_eq!(snapshot.step_count, 4_097);
    assert_eq!(timeline.narrated_steps(), Some(4_096));
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
    let other = TurnTimeline::default().snapshot();

    // Assert
    assert_eq!(first, unchanged);
    assert_ne!(first, changed);
    assert_ne!(other, TurnTimeline::default().snapshot());
    assert!(std::sync::Arc::ptr_eq(&first.steps[0], &changed.steps[0]));
    assert!(!std::sync::Arc::ptr_eq(&first.steps[1], &changed.steps[1]));
    assert!(changed.steps[1].header_revision > first.steps[1].header_revision);
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
    assert_eq!(tool_row(&snapshot, 0).verb, "Preparing");
}

#[test]
fn a_folded_exploration_runs_while_any_of_its_calls_runs_from_the_earliest_start() {
    // Arrange
    let mut timeline = TurnTimeline::default();

    // Act
    timeline.observe_activity(
        call("read", ActivityKind::Read, "a.rs", ActivityStatus::Running),
        1_000,
    );
    timeline.observe_activity(
        call(
            "grep",
            ActivityKind::Search,
            "main",
            ActivityStatus::Running,
        ),
        2_000,
    );
    timeline.observe_activity(
        call(
            "grep",
            ActivityKind::Search,
            "main",
            ActivityStatus::Completed,
        ),
        3_000,
    );
    let one_running = timeline.snapshot();
    timeline.observe_activity(
        call("list", ActivityKind::List, ".", ActivityStatus::Running),
        4_000,
    );
    let two_running = timeline.snapshot();

    // Assert
    assert_eq!(row_count(&one_running), 1);
    assert_eq!(rows(&one_running)[0].running_since(), Some(1_000));
    assert_eq!(rows(&two_running)[0].running_since(), Some(1_000));
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
    let snapshot = timeline.snapshot();
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
    let labels = (0..4)
        .map(|index| match &tool_row(&snapshot, index).result {
            RowResult::Failed { label, .. } => *label,
            _ => "",
        })
        .collect::<Vec<_>>();
    assert_eq!(labels, ["denied", "timed out", "unavailable", "failed"]);
    assert_eq!(tool_row(&snapshot, 4).verb, "Searched");
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
    assert_eq!(row_count(&snapshot), 3);
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
