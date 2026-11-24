use std::time::Duration;

use ag_contracts::{
    ActivityEvent, ActivityKind, ActivityStatus, DiffStat, LineRange, OutputEvent, OutputKind,
    TurnEvent, TurnNotice,
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

/// Returns the `completed` event that ends a run.
fn run_completed() -> RunEventKind {
    event(json!({
        "type": "completed",
        "cost": null,
        "error": null,
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

    // Act
    let events = project(
        &mut projector,
        narration.into_iter().chain([
            event(json!({"type": "item_started", "id": 2, "item_kind": "tool_call"})),
            requested(2, "bash", r#"{"command": "cargo check\n--tests"}"#),
            answered(
                2,
                "bash",
                "completed",
                "{}",
                &json!({"exit_code": 0, "duration_ms": 36000}),
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
    assert_eq!(running.facts.target.as_deref(), Some("cargo check"));
    assert_eq!(running.input.as_deref(), Some("cargo check\n--tests"));
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
    let outcome = json!({
        "stdout": "running 58 tests\ntest a ... ok\n\nfailures:\n    a\n    b\ntest result: FAILED",
        "stderr": "",
        "exit_code": 0
    })
    .to_string();

    // Act
    let row = answered_row(
        "bash",
        r#"{"command": "cargo test 2>&1 | tail -20"}"#,
        "completed",
        &outcome,
        &json!({"exit_code": 0, "pipe_status": [101, 0]}),
    );

    // Assert
    assert_eq!(row.status, ActivityStatus::Completed);
    assert_eq!(row.outcome(), ActivityStatus::Failed);
    assert_eq!(row.facts.pipe_status, [101, 0]);
    assert_eq!(row.effective_exit_code(), Some(101));
    assert_eq!(row.output.as_deref(), Some("a\nb\ntest result: FAILED"));
}

#[test]
fn failed_command_previews_the_end_of_its_standard_error() {
    // Arrange
    let outcome = json!({
        "stdout": "compiling",
        "stderr": "start",
        "stderr_tail": "error[E0063]: missing field\n  --> src/lib.rs:4:5\nerror: aborting",
        "exit_code": 101
    })
    .to_string();

    // Act
    let killed = answered_row(
        "bash",
        r#"{"command": "sleep 30"}"#,
        "completed",
        r#"{"exit_code": null}"#,
        &json!({"signal": 9}),
    );
    let failed = answered_row(
        "bash",
        r#"{"command": "cargo build"}"#,
        "completed",
        &outcome,
        &json!({"exit_code": 101}),
    );
    let unstructured = answered_row(
        "bash",
        r#"{"command": "cargo build"}"#,
        "completed",
        "command failed",
        &json!({"exit_code": 2}),
    );

    // Assert
    assert_eq!(killed.facts.signal, Some(9));
    assert_eq!(killed.outcome(), ActivityStatus::Failed);
    assert_eq!(killed.output, None);
    assert_eq!(
        failed.output.as_deref(),
        Some("error[E0063]: missing field\n--> src/lib.rs:4:5\nerror: aborting")
    );
    assert_eq!(unstructured.output.as_deref(), Some("command failed"));
}

#[test]
fn rejected_write_fails_with_its_reason() {
    // Arrange
    let output = "hunk 1 context mismatch at line 1216\nexpected: fn render\nfound: fn draw";

    // Act
    let rejected = answered_row(
        "write",
        r#"{"path": "src/composer.rs", "patch": "@@"}"#,
        "rejected",
        output,
        &json!({}),
    );
    let written = answered_row(
        "write",
        r#"{"path": "src/composer.rs", "patch": "@@"}"#,
        "completed",
        "wrote src/composer.rs",
        &json!({"lines_added": 38, "lines_removed": 4}),
    );

    // Assert
    assert_eq!(rejected.kind, ActivityKind::FileChange);
    assert_eq!(rejected.status, ActivityStatus::Failed);
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
    assert_eq!(written.input, None);
}

#[test]
fn answer_statuses_map_onto_row_states() {
    // Arrange
    let statuses = [
        ("skipped", ActivityStatus::Completed),
        ("answered", ActivityStatus::Completed),
        ("unanswered", ActivityStatus::Interrupted),
        ("denied", ActivityStatus::Failed),
        ("unavailable", ActivityStatus::Failed),
    ];

    // Act
    let states = statuses.map(|(status, _)| {
        answered_row("read", r#"{"path": "a.rs"}"#, status, "", &json!({})).status
    });

    // Assert
    assert_eq!(states, statuses.map(|(_, state)| state));
}

#[test]
fn read_actions_project_onto_exploration_kinds_and_targets() {
    // Arrange
    let mut projector = RunProjector::new();
    let calls = [
        requested(
            1,
            "read",
            r#"{"path": "src/a.rs", "offset": "1129", "limit": 100}"#,
        ),
        requested(
            2,
            "read",
            r#"{"action": "search", "query": "fn render", "path": "src"}"#,
        ),
        requested(3, "read", r#"{"action": "list", "path": "crates"}"#),
        requested(
            4,
            "read",
            r#"{"action": "show", "path": "src/b.rs", "side": "base"}"#,
        ),
        requested(5, "lookup", r#"{"path": "src/c.rs"}"#),
    ];

    // Act
    let events = project(&mut projector, calls);

    // Assert
    let projected = rows(&events)
        .into_iter()
        .map(|row| (row.kind, row.facts.target.as_deref(), row.facts.lines))
        .collect::<Vec<_>>();
    assert_eq!(
        projected,
        [
            (
                ActivityKind::Read,
                Some("src/a.rs"),
                Some(LineRange {
                    first: 1129,
                    last: Some(1228)
                })
            ),
            (ActivityKind::Search, Some("fn render"), None),
            (ActivityKind::List, Some("crates"), None),
            (ActivityKind::Read, Some("src/b.rs"), None),
            (ActivityKind::Tool, None, None),
        ]
    );
}

#[test]
fn arguments_that_are_not_json_still_show_the_call() {
    // Arrange
    let mut projector = RunProjector::new();

    // Act
    let events = project(&mut projector, [requested(1, "bash", "cargo test")]);

    // Assert
    let row = only_row(&events);
    assert_eq!(row.kind, ActivityKind::Command);
    assert_eq!(row.name, "bash");
    assert_eq!(row.facts.target, None);
    assert_eq!(row.input, None);
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
fn rows_left_open_when_the_run_ends_are_interrupted() {
    // Arrange
    let mut projector = RunProjector::new();
    let opened = project(
        &mut projector,
        [
            requested(1, "bash", r#"{"command": "sleep 30"}"#),
            requested(2, "read", r#"{"path": "a.rs"}"#),
            answered(2, "read", "completed", "1: fn main() {}", &json!({})),
        ],
    );

    // Act
    let finished = project(&mut projector, [run_completed()]);

    // Assert
    assert_eq!(rows(&opened).len(), 3);
    let row = only_row(&finished);
    assert_eq!(row.id, "1");
    assert_eq!(row.status, ActivityStatus::Interrupted);
}

#[test]
fn rows_of_discarded_items_are_dropped() {
    // Arrange
    let mut projector = RunProjector::new();
    let streamed = project(
        &mut projector,
        [
            event(json!({"type": "item_started", "id": 1, "item_kind": "tool_call"})),
            event(json!({
                "type": "item_delta",
                "id": 1,
                "delta": {"type": "tool_arguments", "arguments": "{"}
            })),
            requested(1, "bash", r#"{"command": "true"}"#),
        ],
    );

    // Act
    let finished = project(&mut projector, [run_completed()]);

    // Assert
    assert_eq!(rows(&streamed).len(), 1);
    assert_eq!(
        finished,
        [TurnEvent::Output(OutputEvent::Discard { ids: vec![1] })]
    );
}

#[test]
fn notices_come_before_the_output_of_the_same_event() {
    // Arrange
    let mut projector = RunProjector::new();
    let denial = answered(
        1,
        "bash",
        "completed",
        "{}",
        &json!({"exit_code": 1, "likely_sandbox_denial": true}),
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
