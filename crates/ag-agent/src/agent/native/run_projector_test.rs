use std::time::Duration;

use ag_contracts::{
    ActivityEvent, ActivityFailure, ActivityKind, ActivityStatus, DiffStat, OutputEvent,
    OutputKind, TurnEvent, TurnNotice,
};
use ag_harness::RunEventKind;
use serde_json::{Value, json};

use super::RunProjector;

/// Decodes one run event from its wire form.
fn event(value: Value) -> RunEventKind {
    serde_json::from_value(value).expect("run event")
}

/// Returns a `tool_call_requested` event for item `id`.
fn requested(id: u64, name: &str, arguments: &str) -> RunEventKind {
    event(json!({
        "type": "tool_call_requested",
        "arguments": arguments,
        "call_id": format!("call-{id}"),
        "id": id,
        "name": name
    }))
}

/// Returns an `item_completed` event for tool call item `id` answered with
/// `status` and `output`, merged with the outcome fields in `extra`.
fn answered(id: u64, name: &str, status: &str, output: &str, extra: &Value) -> RunEventKind {
    let mut item = json!({
        "type": "tool_call",
        "arguments": "{}",
        "call_id": format!("call-{id}"),
        "name": name,
        "output": output,
        "status": status,
        "summary": ""
    });
    for (key, value) in extra.as_object().expect("outcome fields") {
        item[key] = value.clone();
    }

    event(json!({"type": "item_completed", "id": id, "item": item}))
}

/// Returns the `outcome` field of a `bash` call that exited with
/// `exit_code` and `pipe_status`.
fn command_exit(exit_code: Option<i32>, pipe_status: &[i32], signal: Option<i32>) -> Value {
    json!({
        "outcome": {
            "type": "command",
            "exit_code": exit_code,
            "pipe_status": pipe_status,
            "signal": signal
        }
    })
}

/// Returns the result text the harness sends the model for a `bash` call.
fn command_result(exit_code: Option<i32>, stdout: &str, stderr: &str) -> String {
    json!({
        "cleanup_failed": false,
        "cleanup_scope": "process_group_best_effort",
        "execution_failure": null,
        "exit_code": exit_code,
        "signal": null,
        "stderr": stderr,
        "stderr_omitted_bytes": 0,
        "stderr_tail": "",
        "stdout": stdout,
        "stdout_omitted_bytes": 0,
        "stdout_tail": "",
        "termination": "completed",
        "truncated": false
    })
    .to_string()
}

/// Returns the `completed` event that ends a run with `error`.
fn run_completed(error: &Value) -> RunEventKind {
    event(json!({
        "type": "completed",
        "cost": null,
        "error": error,
        "usage": {
            "input": 0,
            "output": 0,
            "cache_read": 0,
            "cache_write": null,
            "reasoning": 0,
            "unreported_requests": 0
        }
    }))
}

/// Projects `events` in order.
fn project(
    projector: &mut RunProjector,
    events: impl IntoIterator<Item = RunEventKind>,
) -> Vec<TurnEvent> {
    events
        .into_iter()
        .flat_map(|event| projector.project(event))
        .collect()
}

/// Returns the activity rows among `events`.
fn rows(events: &[TurnEvent]) -> Vec<&ActivityEvent> {
    events
        .iter()
        .filter_map(|event| match event {
            TurnEvent::Activity(row) => Some(row),
            _ => None,
        })
        .collect()
}

/// Returns the only activity row among `events`.
fn only_row(events: &[TurnEvent]) -> &ActivityEvent {
    let projected_rows = rows(events);
    assert_eq!(projected_rows.len(), 1, "{events:?}");

    projected_rows[0]
}

/// Returns the row a lone tool call answered with `status` reports.
fn answered_row(
    name: &str,
    arguments: &str,
    status: &str,
    output: &str,
    extra: &Value,
) -> ActivityEvent {
    let mut projector = RunProjector::new();
    let events = project(
        &mut projector,
        [
            requested(4, name, arguments),
            answered(4, name, status, output, extra),
        ],
    );

    rows(&events)[1].clone()
}

/// Returns the rows a run that ends with `error` closes while a `bash`
/// call is still open.
fn rows_closed_by(error: &Value) -> Vec<ActivityEvent> {
    let mut projector = RunProjector::new();
    project(
        &mut projector,
        [requested(1, "bash", r#"{"command": "sleep 30"}"#)],
    );
    let events = project(&mut projector, [run_completed(error)]);

    rows(&events).into_iter().cloned().collect()
}

#[test]
fn tool_rows_follow_the_narration_that_precedes_them() {
    // Arrange
    let mut projector = RunProjector::new();
    let narration = [
        event(json!({"type": "item_started", "id": 1, "item_kind": "message"})),
        event(json!({
            "type": "item_completed",
            "id": 1,
            "item": {"type": "message", "text": "Checking the build"}
        })),
    ];
    let mut exit = command_exit(Some(0), &[], None);
    exit["duration_ms"] = json!(36_000);

    // Act
    let events = project(
        &mut projector,
        narration.into_iter().chain([
            event(json!({"type": "item_started", "id": 2, "item_kind": "tool_call"})),
            requested(
                2,
                "bash",
                r#"{"command": "cargo check -p agentty\n  --tests"}"#,
            ),
            answered(
                2,
                "bash",
                "completed",
                &command_result(Some(0), "", ""),
                &exit,
            ),
        ]),
    );

    // Assert
    assert!(matches!(
        &events[0],
        TurnEvent::Output(OutputEvent::Started {
            id: 1,
            kind: OutputKind::Message
        })
    ));
    assert!(matches!(
        &events[1],
        TurnEvent::Output(OutputEvent::Completed { id: 1, .. })
    ));
    assert_eq!(events.len(), 4, "{events:?}");
    let projected_rows = rows(&events[2..]);
    let (running, finished) = (projected_rows[0], projected_rows[1]);
    assert_eq!(running.id, "2");
    assert_eq!(running.status, ActivityStatus::Running);
    assert_eq!(running.kind, ActivityKind::Command);
    assert_eq!(
        running.facts.target.as_deref(),
        Some("cargo check -p agentty --tests")
    );
    assert_eq!(
        running.input.as_deref(),
        Some("cargo check -p agentty\n  --tests")
    );
    assert_eq!(finished.attempt_id, running.attempt_id);
    assert_eq!(finished.status, ActivityStatus::Completed);
    assert_eq!(finished.outcome(), ActivityStatus::Completed);
    assert_eq!(finished.exit_code, Some(0));
    assert_eq!(finished.facts.duration, Some(Duration::from_secs(36)));
    assert_eq!(finished.output, None);
}

#[test]
fn piped_command_fails_on_its_first_failing_stage_with_its_output_tail() {
    // Arrange
    let result = command_result(
        Some(0),
        "running 58 tests\ntest a ... ok\n\nfailures:\n    a\n    b\ntest result: FAILED",
        "",
    );

    // Act
    let row = answered_row(
        "bash",
        r#"{"command": "cargo test 2>&1 | tail -20"}"#,
        "completed",
        &result,
        &command_exit(Some(0), &[101, 0], None),
    );

    // Assert
    assert_eq!(row.status, ActivityStatus::Completed);
    assert_eq!(row.outcome(), ActivityStatus::Failed);
    assert_eq!(row.facts.pipe_status, [101, 0]);
    assert_eq!(row.effective_exit_code(), Some(101));
    assert_eq!(row.facts.failure, None);
    assert_eq!(row.output.as_deref(), Some("a\nb\ntest result: FAILED"));
}

#[test]
fn failed_command_previews_the_end_of_its_standard_error() {
    // Arrange
    let mut tailed =
        serde_json::from_str::<Value>(&command_result(Some(101), "compiling", "start"))
            .expect("command result");
    tailed["stderr_tail"] =
        json!("error[E0063]: missing field\n  --> src/lib.rs:4:5\nerror: aborting");

    // Act
    let killed = answered_row(
        "bash",
        r#"{"command": "sleep 30"}"#,
        "completed",
        &command_result(None, "", ""),
        &command_exit(None, &[], Some(9)),
    );
    let failed = answered_row(
        "bash",
        r#"{"command": "cargo build"}"#,
        "completed",
        &tailed.to_string(),
        &command_exit(Some(101), &[], None),
    );

    // Assert
    assert_eq!(killed.facts.signal, Some(9));
    assert_eq!(killed.outcome(), ActivityStatus::Failed);
    assert_eq!(killed.output, None);
    assert_eq!(
        failed.output.as_deref(),
        Some("error[E0063]: missing field\n--> src/lib.rs:4:5\nerror: aborting")
    );
}

#[test]
fn rejected_command_tail_leaves_out_its_reason() {
    // Arrange
    let output = "malformed bash arguments: missing field `command`";

    // Act
    let short = answered_row("bash", "{}", "rejected", output, &json!({}));
    let long = answered_row(
        "bash",
        "{}",
        "rejected",
        "malformed bash arguments\nexpected an object\nwith a command",
        &json!({}),
    );

    // Assert
    assert_eq!(short.facts.failure, Some(ActivityFailure::Rejected));
    assert_eq!(short.facts.reason.as_deref(), Some(output));
    assert_eq!(short.output, None);
    assert_eq!(
        long.output.as_deref(),
        Some("expected an object\nwith a command")
    );
}

#[test]
fn rejected_write_fails_with_its_reason() {
    // Arrange
    let output = "hunk 1 context mismatch at line 1216\nexpected: fn render\nfound: fn draw";
    let arguments = r#"{"path": "src/composer.rs", "patch": "@@ -1 +1 @@\n-a\n+b\n"}"#;

    // Act
    let rejected = answered_row("write", arguments, "rejected", output, &json!({}));
    let written = answered_row(
        "write",
        arguments,
        "completed",
        "Wrote src/composer.rs",
        &json!({"outcome": {"type": "write", "lines_added": 38, "lines_removed": 4}}),
    );

    // Assert
    assert_eq!(rejected.kind, ActivityKind::FileChange);
    assert_eq!(rejected.status, ActivityStatus::Failed);
    assert_eq!(rejected.facts.failure, Some(ActivityFailure::Rejected));
    assert_eq!(rejected.facts.target.as_deref(), Some("src/composer.rs"));
    assert_eq!(
        rejected.facts.reason.as_deref(),
        Some("hunk 1 context mismatch at line 1216")
    );
    assert_eq!(
        rejected.output.as_deref(),
        Some("expected: fn render\nfound: fn draw")
    );
    assert_eq!(rejected.facts.diff_stat, None);
    assert_eq!(written.status, ActivityStatus::Completed);
    assert_eq!(
        written.facts.diff_stat,
        Some(DiffStat {
            added: 38,
            removed: 4
        })
    );
    assert_eq!(written.facts.reason, None);
    assert_eq!(written.facts.failure, None);
}

#[test]
fn answer_statuses_map_onto_row_states_and_failures() {
    // Arrange
    let statuses = [
        ("skipped", ActivityStatus::Completed, None),
        ("answered", ActivityStatus::Completed, None),
        ("unanswered", ActivityStatus::Interrupted, None),
        (
            "denied",
            ActivityStatus::Failed,
            Some(ActivityFailure::Denied),
        ),
        (
            "unavailable",
            ActivityStatus::Failed,
            Some(ActivityFailure::Unavailable),
        ),
    ];

    // Act
    let states = statuses.map(|(status, _, _)| {
        let row = answered_row("read", r#"{"path": "a.rs"}"#, status, "", &json!({}));

        (row.status, row.facts.failure)
    });

    // Assert
    assert_eq!(states, statuses.map(|(_, state, failure)| (state, failure)));
}

#[test]
fn an_answer_without_a_request_still_finishes_a_row() {
    // Arrange
    let mut projector = RunProjector::new();

    // Act
    let events = project(
        &mut projector,
        [answered(
            3,
            "write",
            "rejected",
            "path escapes the repository",
            &json!({}),
        )],
    );

    // Assert
    let row = only_row(&events);
    assert_eq!(row.id, "3");
    assert_eq!(row.status, ActivityStatus::Failed);
    assert_eq!(row.output, None);
}

#[test]
fn rows_left_open_by_a_finished_or_cancelled_run_are_interrupted() {
    // Arrange
    let cancelled =
        json!({"kind": "cancelled", "message": "the run was cancelled", "retryable": false});

    // Act
    let finished = rows_closed_by(&Value::Null);
    let stopped = rows_closed_by(&cancelled);

    // Assert
    for rows in [&finished, &stopped] {
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "1");
        assert_eq!(rows[0].status, ActivityStatus::Interrupted);
        assert_eq!(rows[0].facts.failure, None);
        assert_eq!(rows[0].facts.reason, None);
    }
}

#[test]
fn rows_left_open_by_a_failed_run_fail_with_its_error() {
    // Arrange
    let timed_out = json!({
        "kind": "approval_timeout",
        "message": "the approval handler did not decide in time\ncall-1",
        "retryable": false
    });
    let failed =
        json!({"kind": "tool_failed", "message": "bash cleanup failed", "retryable": false});

    // Act
    let approval = rows_closed_by(&timed_out);
    let tool = rows_closed_by(&failed);

    // Assert
    assert_eq!(approval[0].status, ActivityStatus::Failed);
    assert_eq!(approval[0].facts.failure, Some(ActivityFailure::TimedOut));
    assert_eq!(
        approval[0].facts.reason.as_deref(),
        Some("the approval handler did not decide in time")
    );
    assert_eq!(tool[0].status, ActivityStatus::Failed);
    assert_eq!(tool[0].facts.failure, Some(ActivityFailure::Error));
    assert_eq!(tool[0].facts.reason.as_deref(), Some("bash cleanup failed"));
}

#[test]
fn notices_come_before_the_output_of_the_same_event() {
    // Arrange
    let mut projector = RunProjector::new();
    let mut extra = command_exit(Some(1), &[], None);
    extra["likely_sandbox_denial"] = json!(true);
    let denial = answered(
        1,
        "bash",
        "completed",
        &command_result(Some(1), "", ""),
        &extra,
    );

    // Act
    let events = project(&mut projector, [denial]);

    // Assert
    assert_eq!(
        events[0],
        TurnEvent::Notice(TurnNotice::LikelySandboxDenial)
    );
    assert!(matches!(&events[1], TurnEvent::Activity(row) if row.name == "bash"));
}
