//! Turn-summary tests covering edit totals, failure grouping and recovery,
//! exploration counts, steps, and the persisted text.

use ag_contracts::{
    ActivityEvent, ActivityFacts, ActivityFailure, ActivityKind, ActivityStatus, DiffStat,
    OutputEvent, OutputKind,
};

use super::{TurnSummary, command_core};
use crate::domain::turn_timeline::TurnTimeline;
use crate::test_support::activity_event;

/// Returns call `id` of `kind` on `target` that ended in `status`.
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

/// Returns a successful edit of `path` adding and removing lines.
fn edit(id: &str, path: &str, added: u64, removed: u64) -> ActivityEvent {
    let mut event = call(
        id,
        ActivityKind::FileChange,
        path,
        ActivityStatus::Completed,
    );
    event.facts.diff_stat = Some(DiffStat { added, removed });

    event
}

/// Returns an edit of `path` the tool rejected for `reason`.
fn rejected_edit(id: &str, path: &str, reason: &str) -> ActivityEvent {
    let mut event = call(id, ActivityKind::FileChange, path, ActivityStatus::Failed);
    event.facts.failure = Some(ActivityFailure::Rejected);
    event.facts.reason = Some(reason.into());

    event
}

/// Returns command `command` that exited with `code`.
fn command(id: &str, command: &str, code: i64) -> ActivityEvent {
    ActivityEvent {
        exit_code: Some(code),
        input: Some(command.into()),
        ..call(id, ActivityKind::Command, "", ActivityStatus::Completed)
    }
}

/// Returns a failed call of `kind` on `target` that failed as `failure`.
fn failed(id: &str, kind: ActivityKind, target: &str, failure: ActivityFailure) -> ActivityEvent {
    let mut event = call(id, kind, target, ActivityStatus::Failed);
    event.facts.failure = Some(failure);

    event
}

/// A turn's live timeline beside the calls it made, as `TurnActivity`
/// keeps them.
#[derive(Default)]
struct Turn {
    calls: Vec<ActivityEvent>,
    timeline: TurnTimeline,
}

impl Turn {
    /// Records call `event` observed at `at` milliseconds.
    fn observe_activity(&mut self, event: ActivityEvent, at: u64) {
        self.calls.push(event.clone());
        self.timeline.observe_activity(event, at);
    }

    /// Applies streamed output `event` observed at `at` milliseconds.
    fn observe_output(&mut self, event: OutputEvent, at: u64) {
        self.timeline.observe_output(event, at);
    }

    /// Summarizes the turn given the seconds it took.
    fn summary(&self, elapsed_seconds: Option<u64>) -> TurnSummary {
        let calls = self.calls.iter().collect::<Vec<_>>();

        TurnSummary::from_calls(&calls, self.timeline.narrated_steps(), elapsed_seconds)
    }
}

/// Streams one complete message item `id` at `at` milliseconds.
fn message(timeline: &mut Turn, id: u64, text: &str, at: u64) {
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

/// Returns `summary`'s persisted text after its format marker line.
fn body(summary: &TurnSummary, usage: Option<&str>) -> String {
    let text = summary.render(usage);
    let (marker, body) = text.split_once('\n').expect("marker line");
    assert_eq!(marker, TurnSummary::FORMAT_MARKER);

    body.to_string()
}

/// Summarizes `calls` from a backend without narration.
fn render(calls: &[ActivityEvent], elapsed_seconds: Option<u64>) -> String {
    let calls = calls.iter().collect::<Vec<_>>();

    body(
        &TurnSummary::from_calls(&calls, None, elapsed_seconds),
        None,
    )
}

#[test]
fn design_turn_summarizes_edits_recovered_failures_and_exploration() {
    // Arrange
    let mut timeline = Turn::default();
    message(&mut timeline, 0, "Reading the composer.", 0);
    for (index, path) in ["src/composer.rs", "src/prompt.rs"].iter().enumerate() {
        timeline.observe_activity(
            call(
                &format!("r{index}"),
                ActivityKind::Read,
                path,
                ActivityStatus::Completed,
            ),
            1_000,
        );
    }
    timeline.observe_activity(
        call(
            "s",
            ActivityKind::Search,
            "Provider",
            ActivityStatus::Completed,
        ),
        1_000,
    );
    message(&mut timeline, 1, "Editing.", 2_000);
    timeline.observe_activity(
        rejected_edit("e0", "src/composer.rs", "hunk context mismatch"),
        2_000,
    );
    timeline.observe_activity(
        rejected_edit("e1", "./src/composer.rs", "hunk context mismatch"),
        2_000,
    );
    timeline.observe_activity(
        call(
            "r2",
            ActivityKind::Read,
            "src/composer.rs",
            ActivityStatus::Completed,
        ),
        2_000,
    );
    timeline.observe_activity(edit("e2", "src/composer.rs", 70, 10), 2_000);
    timeline.observe_activity(edit("e3", "src/composer.rs", 9, 4), 2_000);
    timeline.observe_activity(edit("e4", "src/prompt.rs", 24, 6), 2_000);
    message(&mut timeline, 2, "Testing.", 3_000);
    timeline.observe_activity(
        command("c0", "cargo test -p agentty 2>&1 | tail -20", 101),
        3_000,
    );
    timeline.observe_activity(command("c1", "cargo test -p agentty | head -5", 0), 3_000);
    timeline.observe_output(
        OutputEvent::Answer {
            replaces: None,
            text: "Done.".into(),
        },
        4_000,
    );

    // Act
    let summary = body(&timeline.summary(Some(1_223)), None);

    // Assert
    assert_eq!(
        summary,
        "3 steps · 11 tools · 20m 23s\nEdited\t✓\tsrc/composer.rs\t+79 −14\t2 \
         edits\n\t✓\tsrc/prompt.rs\t+24 −6\t1 edit\nFailed\t✓\t2 edits to composer.rs rejected \
         (hunk context mismatch)\t\t→ retried after re-reading\n\t✓\tcargo test -p agentty 2>&1 | \
         tail -20\texit 101\t→ rerun exit 0\nExplored\t3 reads · 1 search · ran 2 commands\n─ \
         Worked 20m 23s · 3 steps · 11 tools · 3 failed (2 rejected edits, 1 nonzero exit), all \
         recovered"
    );
}

#[test]
fn backend_without_narration_omits_steps_and_answer_does_not_count() {
    // Arrange
    let mut timeline = Turn::default();
    timeline.observe_activity(command("c", "ls", 0), 0);
    timeline.observe_output(
        OutputEvent::Answer {
            replaces: None,
            text: "Done.".into(),
        },
        0,
    );

    // Act
    let summary = body(&timeline.summary(Some(62)), None);
    let hours = body(&timeline.summary(Some(3_605)), None);

    // Assert
    assert_eq!(
        summary,
        "1 tool · 1m 2s\nRan\t1 command\n─ Worked 1m 2s · 1 tool"
    );
    // The time reads as the live status line shows it.
    assert!(hours.starts_with("1 tool · 1h 5s\n"), "{hours}");
}

#[test]
fn reasoning_counts_as_narration_for_steps() {
    // Arrange
    let mut timeline = Turn::default();
    timeline.observe_output(
        OutputEvent::Started {
            id: 0,
            kind: OutputKind::Reasoning,
        },
        0,
    );
    timeline.observe_activity(command("c", "ls", 0), 0);

    // Act
    let summary = body(&timeline.summary(None), None);

    // Assert
    assert!(summary.starts_with("1 step · 1 tool\n"), "{summary}");
}

#[test]
fn turn_without_calls_renders_nothing() {
    // Arrange
    let mut timeline = Turn::default();
    message(&mut timeline, 0, "Only talking.", 0);

    // Act
    let summary = timeline.summary(Some(3)).render(None);

    // Assert
    assert_eq!(summary, "");
}

#[test]
fn utility_turn_without_start_omits_elapsed_and_ends_with_usage() {
    // Arrange
    let calls = [
        call("m0", ActivityKind::Mcp, "q", ActivityStatus::Completed),
        call("m1", ActivityKind::Mcp, "q", ActivityStatus::Completed),
        ActivityEvent {
            name: "review".into(),
            ..call("k", ActivityKind::Skill, "", ActivityStatus::Running)
        },
        call("l", ActivityKind::List, "src", ActivityStatus::Completed),
        call(
            "w",
            ActivityKind::WebSearch,
            "rust",
            ActivityStatus::Completed,
        ),
    ];
    let calls = calls.iter().collect::<Vec<_>>();

    // Act
    let summary = body(
        &TurnSummary::from_calls(&calls, None, None),
        Some("↑1k ↓2k"),
    );

    // Assert
    assert_eq!(
        summary,
        "5 tools\nExplored\t1 listing · 1 web search\nUsed\tshell ×2 · review ×1\n─ Worked 5 \
         tools · 1 interrupted · ↑1k ↓2k"
    );
}

#[test]
fn unrecovered_failures_keep_a_cross_and_partial_recovery_is_counted() {
    // Arrange
    let calls = [
        command("c0", "cargo test", 1),
        command("c1", "cargo test", 1),
        command("c2", "cargo clippy", 2),
        command("c3", "cargo clippy 2>&1", 0),
    ];

    // Act
    let summary = render(&calls, Some(0));

    // Assert
    assert_eq!(
        summary,
        "4 tools · 0s\nFailed\t✗\tcargo test\texit 1 ×2\t\n\t✓\tcargo clippy\texit 2\t→ rerun \
         exit 0\nRan\t4 commands\n─ Worked 0s · 4 tools · 3 failed (3 nonzero exits), 1 recovered"
    );
}

#[test]
fn unrecovered_only_failures_name_no_recovery() {
    // Arrange
    let calls = [failed(
        "t",
        ActivityKind::Tool,
        "lookup",
        ActivityFailure::Unavailable,
    )];

    // Act
    let summary = render(&calls, None);

    // Assert
    assert!(
        summary.ends_with("─ Worked 1 tool · 1 failed (1 unavailable tool)"),
        "{summary}"
    );
}

#[test]
fn failures_report_their_class_reason_and_recovery() {
    // Arrange
    let mut signaled = command("c0", "sleep 9", 0);
    signaled.facts.signal = Some(9);
    let rerun = ActivityEvent {
        exit_code: None,
        ..command("c1", "sleep 9 | tail -1", 0)
    };
    let mut denied = failed("t0", ActivityKind::Tool, "deploy", ActivityFailure::Denied);
    denied.facts.reason = Some("needs\tapproval".into());
    let retried = call(
        "t1",
        ActivityKind::Tool,
        "deploy",
        ActivityStatus::Completed,
    );
    let timed_out = failed(
        "t2",
        ActivityKind::Search,
        "needle",
        ActivityFailure::TimedOut,
    );
    let error = failed("t3", ActivityKind::Mcp, "q", ActivityFailure::Error);
    let rejected = failed("t4", ActivityKind::Tool, "x", ActivityFailure::Rejected);
    let calls = [signaled, rerun, denied, retried, timed_out, error, rejected];

    // Act
    let summary = render(&calls, None);

    // Assert
    let lines = summary.lines().collect::<Vec<_>>();
    assert_eq!(lines[1], "Failed\t✓\tsleep 9\tsignal 9\t→ rerun succeeded");
    assert_eq!(
        lines[2],
        "\t✓\tshell · deploy\tdenied · needs approval\t→ retried"
    );
    assert_eq!(lines[3], "\t✗\tneedle\ttimed out\t");
    assert_eq!(lines[4], "\t✗\tshell · q\tfailed\t");
    assert_eq!(lines[5], "\t✗\tshell · x\trejected\t");
    assert!(
        lines[lines.len() - 1].ends_with(
            "5 failed (1 signal, 1 denied call, 1 timeout, 1 error, 1 rejected call), 2 recovered"
        ),
        "{summary}"
    );
}

#[test]
fn edit_failures_name_the_file_and_retry_without_reading() {
    // Arrange
    let denied = failed(
        "e0",
        ActivityKind::FileChange,
        "src/a.rs",
        ActivityFailure::Denied,
    );
    let errored = failed(
        "e1",
        ActivityKind::FileChange,
        "src/b.rs",
        ActivityFailure::Error,
    );
    let mut exited = call(
        "e2",
        ActivityKind::FileChange,
        "src/c.rs",
        ActivityStatus::Completed,
    );
    exited.exit_code = Some(1);
    let mut killed = call(
        "e3",
        ActivityKind::FileChange,
        "src/d.rs",
        ActivityStatus::Completed,
    );
    killed.facts.signal = Some(15);
    let unstated = call(
        "e4",
        ActivityKind::FileChange,
        "src/a.rs",
        ActivityStatus::Completed,
    );
    let calls = [denied, errored, exited, killed, unstated];

    // Act
    let summary = render(&calls, None);

    // Assert
    let lines = summary.lines().collect::<Vec<_>>();
    assert_eq!(lines[1], "Edited\t✓\tsrc/a.rs\t\t1 edit");
    assert_eq!(lines[2], "Failed\t✓\tedit to a.rs denied\t\t→ retried");
    assert_eq!(lines[3], "\t✗\tedit to b.rs failed\t\t");
    assert_eq!(lines[4], "\t✗\tedit to c.rs exited nonzero\t\t");
    assert_eq!(lines[5], "\t✗\tedit to d.rs killed\t\t");
    assert!(
        lines[lines.len() - 1].ends_with(
            "4 failed (1 denied edit, 1 failed edit, 1 nonzero exit, 1 signal), 1 recovered"
        ),
        "{summary}"
    );
}

#[test]
fn command_core_strips_trailing_output_filters_only() {
    // Arrange, Act, Assert
    assert_eq!(command_core(" cargo test 2>&1 | tail -20 "), "cargo test");
    assert_eq!(command_core("make 2>&1"), "make");
    assert_eq!(command_core("git log | head -n 3 | tail -1"), "git log");
    assert_eq!(command_core("a | head"), "a");
    assert_eq!(command_core("a || tail -1"), "a || tail -1");
    assert_eq!(command_core("grep x | tailor"), "grep x | tailor");
    assert_eq!(command_core("ls"), "ls");
}

#[test]
fn command_core_keeps_stages_chained_or_quoted_after_a_filter() {
    // Arrange, Act, Assert
    assert_eq!(
        command_core("cargo test | tail -5 && cargo clippy"),
        "cargo test | tail -5 && cargo clippy"
    );
    assert_eq!(command_core("x | head -1; y"), "x | head -1; y");
    assert_eq!(
        command_core(r#"rg "a | head -1" src"#),
        r#"rg "a | head -1" src"#
    );
    assert_eq!(command_core("x | head +3"), "x");
}

#[test]
fn a_chained_command_is_not_recovered_by_its_first_part() {
    // Arrange
    let calls = [
        command("c0", "cargo test | tail -5 && cargo clippy", 1),
        command("c1", "cargo test", 0),
    ];

    // Act
    let summary = render(&calls, None);

    // Assert
    assert!(
        summary.contains("Failed\t✗\tcargo test | tail -5 && cargo clippy\texit 1\t\n"),
        "{summary}"
    );
}
