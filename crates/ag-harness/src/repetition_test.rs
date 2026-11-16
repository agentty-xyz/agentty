//! Repeated tool call detection tests: nudge and stop thresholds, the
//! counting window, and feedback text.

use serde_json::json;

use crate::repetition::{
    REPETITION_NUDGE_AT, REPETITION_STOP_AT, REPETITION_WINDOW, RepeatedCalls, Repetition,
    repetition_feedback, result_digest, unavailable_repetition_feedback,
};
use crate::tool::ToolCall;

fn read(path: &str) -> ToolCall {
    ToolCall::from_json(
        "read-call".to_string(),
        "read",
        &json!({"path": path}).to_string(),
        None,
    )
    .expect("read call")
}

fn bash(command: &str) -> ToolCall {
    ToolCall::from_json(
        "bash-call".to_string(),
        "bash",
        &json!({"command": command}).to_string(),
        None,
    )
    .expect("bash call")
}

fn write(path: &str) -> ToolCall {
    ToolCall::from_json(
        "write-call".to_string(),
        "write",
        &json!({"path": path, "patch": "@@ -1 +1 @@\n-a\n+b\n"}).to_string(),
        None,
    )
    .expect("write call")
}

/// Records `call` returning `result` `times` times and returns each outcome.
fn record_times<'a>(
    calls: &mut RepeatedCalls,
    call: &'a ToolCall,
    result: &str,
    times: usize,
) -> Vec<Repetition<'a>> {
    (0..times)
        .map(|_| calls.record(call, result_digest(result)))
        .collect()
}

#[test]
fn identical_calls_nudge_once_then_stop() {
    // Arrange
    let mut calls = RepeatedCalls::default();
    let call = read("src/lib.rs");

    // Act
    let outcomes = record_times(&mut calls, &call, "same", REPETITION_STOP_AT);

    // Assert
    assert_eq!(
        outcomes,
        [
            Repetition::None,
            Repetition::None,
            Repetition::Nudge {
                repeats: REPETITION_NUDGE_AT,
                tool: "read",
                unavailable: false,
            },
            Repetition::None,
            Repetition::Stop {
                repeats: REPETITION_STOP_AT,
                tool: "read",
                unavailable: false,
            },
        ]
    );
}

#[test]
fn a_changing_result_never_repeats() {
    // Arrange
    let mut calls = RepeatedCalls::default();
    let call = bash("cargo test");

    // Act
    let outcomes = (0..REPETITION_WINDOW)
        .map(|attempt| calls.record(&call, result_digest(&format!("attempt {attempt}"))))
        .collect::<Vec<_>>();

    // Assert
    assert!(outcomes.iter().all(|outcome| *outcome == Repetition::None));
}

#[test]
fn a_differing_write_restarts_the_count() {
    // Arrange
    let mut calls = RepeatedCalls::default();
    let call = read("src/lib.rs");
    let write = write("src/lib.rs");
    record_times(&mut calls, &call, "before", REPETITION_NUDGE_AT);

    // Act
    let after_write = calls.record(&write, result_digest("applied"));
    let outcomes = record_times(&mut calls, &call, "before", REPETITION_STOP_AT - 1);

    // Assert
    assert_eq!(after_write, Repetition::None);
    assert_eq!(
        outcomes,
        [
            Repetition::None,
            Repetition::None,
            Repetition::Nudge {
                repeats: REPETITION_NUDGE_AT,
                tool: "read",
                unavailable: false,
            },
            Repetition::None,
        ]
    );
}

#[test]
fn an_identical_command_counts_instead_of_restarting() {
    // Arrange
    let mut calls = RepeatedCalls::default();
    let call = bash("git status");

    // Act
    let outcomes = record_times(&mut calls, &call, "clean", REPETITION_STOP_AT);

    // Assert
    assert_eq!(
        outcomes.last(),
        Some(&Repetition::Stop {
            repeats: REPETITION_STOP_AT,
            tool: "bash",
            unavailable: false,
        })
    );
}

#[test]
fn calls_older_than_the_window_stop_counting() {
    // Arrange
    let mut calls = RepeatedCalls::default();
    let call = read("src/lib.rs");
    record_times(&mut calls, &call, "same", REPETITION_NUDGE_AT - 1);
    for index in 0..REPETITION_WINDOW {
        calls.record(
            &read(&format!("src/other_{index}.rs")),
            result_digest("other"),
        );
    }

    // Act
    let outcome = calls.record(&call, result_digest("same"));

    // Assert
    assert_eq!(outcome, Repetition::None);
}

#[test]
fn a_call_that_left_the_window_is_nudged_again_when_it_loops_again() {
    // Arrange
    let mut calls = RepeatedCalls::default();
    let call = read("src/lib.rs");
    record_times(&mut calls, &call, "same", REPETITION_NUDGE_AT);
    for index in 0..REPETITION_WINDOW {
        calls.record(
            &read(&format!("src/other_{index}.rs")),
            result_digest("other"),
        );
    }

    // Act
    let outcomes = record_times(&mut calls, &call, "same", REPETITION_NUDGE_AT);

    // Assert
    assert_eq!(
        outcomes.last(),
        Some(&Repetition::Nudge {
            repeats: REPETITION_NUDGE_AT,
            tool: "read",
            unavailable: false,
        })
    );
}

#[test]
fn malformed_calls_are_not_counted() {
    // Arrange
    let mut calls = RepeatedCalls::default();
    let malformed = ToolCall::from_json("bad".to_string(), "read", r#"{"path":"#, None)
        .expect("malformed call");

    // Act
    let outcomes = record_times(&mut calls, &malformed, "rejected", REPETITION_STOP_AT);

    // Assert
    assert!(outcomes.iter().all(|outcome| *outcome == Repetition::None));
}

#[test]
fn feedback_names_the_tool_and_the_stop_limit() {
    // Arrange
    let tool = "read";

    // Act
    let feedback = repetition_feedback(tool, REPETITION_NUDGE_AT);

    // Assert
    assert!(feedback.starts_with("[harness] You have called `read` 3 times"));
    assert!(feedback.contains(&format!("reaches {REPETITION_STOP_AT} repeats")));
}

#[test]
fn unavailable_calls_count_together_whatever_their_names_and_arguments() {
    // Arrange
    let mut calls = RepeatedCalls::default();
    let attempts = ["read_file", "open_file", "cat", "view_file", "write"]
        .map(|name| ToolCall::from_json(format!("{name}-call"), name, "{}", None).expect("call"));

    // Act
    let outcomes = attempts
        .iter()
        .map(|call| calls.record_unavailable(call))
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        outcomes,
        [
            Repetition::None,
            Repetition::None,
            Repetition::Nudge {
                repeats: REPETITION_NUDGE_AT,
                tool: "cat",
                unavailable: true,
            },
            Repetition::None,
            Repetition::Stop {
                repeats: REPETITION_STOP_AT,
                tool: "write",
                unavailable: true,
            },
        ]
    );
}

#[test]
fn an_unavailable_call_does_not_restart_the_count() {
    // Arrange
    let mut calls = RepeatedCalls::default();
    let call = read("src/lib.rs");
    let denied = bash("rm -rf target");
    record_times(&mut calls, &call, "same", REPETITION_NUDGE_AT - 1);

    // Act
    calls.record_unavailable(&denied);
    let outcome = calls.record(&call, result_digest("same"));

    // Assert
    assert_eq!(
        outcome,
        Repetition::Nudge {
            repeats: REPETITION_NUDGE_AT,
            tool: "read",
            unavailable: false,
        }
    );
}

#[test]
fn unavailable_feedback_says_the_calls_did_not_run() {
    // Arrange
    let tool = "read_file";

    // Act
    let feedback = unavailable_repetition_feedback(tool, REPETITION_NUDGE_AT);

    // Assert
    assert!(
        feedback.starts_with("[harness] 3 of your recent tool calls, most recently `read_file`")
    );
    assert!(feedback.contains("not available in this turn, so none of them ran"));
    assert!(feedback.contains(&format!(
        "reach {REPETITION_STOP_AT} among your recent calls"
    )));
}
