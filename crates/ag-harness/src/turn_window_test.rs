use std::num::NonZeroU64;

use serde_json::json;

use super::{Fit, MAX_TURN_SUMMARIES, SUMMARY_INSTRUCTIONS, TurnWindow};
use crate::context::{ContextBudget, ContextEstimator};
use crate::model::{ModelMessage, ModelRequest, ReasoningEffort};
use crate::schema_contract::OutputSchema;
use crate::tool::{ToolCall, ToolDefinition};

/// Weighs each message by its retained bytes and tools at nothing, so tests
/// reason directly in payload bytes.
struct ByteEstimator;

impl ContextEstimator for ByteEstimator {
    fn message_weight(&self, message: &ModelMessage) -> u64 {
        u64::try_from(message.retained_bytes()).expect("weight")
    }

    fn tool_definition_weight(&self, _tool: &ToolDefinition) -> u64 {
        0
    }
}

fn budget(max_request_weight: u64) -> ContextBudget {
    ContextBudget::new(NonZeroU64::new(max_request_weight).expect("nonzero budget"))
}

fn request() -> ModelRequest {
    let schema = OutputSchema::new(json!({"type": "object"})).expect("schema");

    ModelRequest::new("task", schema)
}

fn read_call(id: &str, reasoning: Option<&str>) -> ToolCall {
    ToolCall::from_json(
        id.to_string(),
        "read",
        r#"{"action":"file","path":"src/lib.rs"}"#,
        reasoning.map(str::to_string),
    )
    .expect("read call")
}

fn write_call(id: &str, path: &str) -> ToolCall {
    ToolCall::from_json(
        id.to_string(),
        "write",
        &json!({"path": path, "patch": "--- a/x\n+++ b/x\n@@\n+x\n"}).to_string(),
        None,
    )
    .expect("write call")
}

/// Records `count` read groups whose results carry `result_bytes` bytes.
fn record_reads(request: &mut ModelRequest, count: usize, result_bytes: usize) {
    for index in 0..count {
        let call = read_call(&format!("call-{index}"), None);
        request.record_tool_call(call.clone());
        request.record_tool_result(&call, "r".repeat(result_bytes));
    }
}

fn fitted(fit: Fit) -> ModelRequest {
    match fit {
        Fit::Fits(request) => Some(request),
        Fit::NeedsSummary { .. } => None,
    }
    .expect("request should fit")
}

fn elided_results(request: &ModelRequest) -> usize {
    request
        .messages()
        .iter()
        .filter(|message| {
            matches!(message, ModelMessage::ToolResult { content, .. } if content.contains("elided"))
        })
        .count()
}

#[test]
fn fit_returns_the_canonical_request_while_it_fits() {
    // Arrange
    let mut request = request();
    let mut window = TurnWindow::new(&request);
    record_reads(&mut request, 2, 100);

    // Act
    let outgoing = fitted(window.fit(&ByteEstimator, budget(10_000), &request));

    // Assert
    assert_eq!(outgoing, request);
}

#[test]
fn fit_elides_oldest_tool_results_to_the_target_and_keeps_recent_ones() {
    // Arrange
    let mut request = request();
    let mut window = TurnWindow::new(&request);
    record_reads(&mut request, 10, 1_000);
    let canonical = request.clone();

    // Act
    let first = fitted(window.fit(&ByteEstimator, budget(8_000), &request));
    record_reads(&mut request, 1, 10);
    let second = fitted(window.fit(&ByteEstimator, budget(8_000), &request));

    // Assert
    // About 10.5k bytes must shrink to the 4.8k target: six of the ten
    // results go, oldest first.
    assert_eq!(elided_results(&first), 6);
    assert!(matches!(
        &first.messages()[2],
        ModelMessage::ToolResult { content, name, .. }
            if content.contains("1000 bytes of earlier `read` output") && name == "read"
    ));
    assert!(matches!(
        &first.messages()[20],
        ModelMessage::ToolResult { content, .. } if content.len() == 1_000
    ));
    // The boundary never moves back, so the elided prefix stays stable.
    assert_eq!(elided_results(&second), 6);
    assert_eq!(&request.messages()[..21], canonical.messages());
}

#[test]
fn fit_stops_eliding_once_tool_results_reach_the_target() {
    // Arrange
    let mut request = request();
    let mut window = TurnWindow::new(&request);
    for index in 0..20 {
        let call = read_call(&format!("call-{index}"), Some("why"));
        request.record_tool_call(call.clone());
        request.record_tool_result(&call, "r".repeat(1_000));
    }

    // Act
    let outgoing = fitted(window.fit(&ByteEstimator, budget(20_000), &request));

    // Assert
    // Eliding the oldest results reaches the 12k target well before the 16
    // eligible ones run out, so later results and all reasoning stay intact.
    let elided = elided_results(&outgoing);
    assert_eq!(elided, 11);
    assert!(!matches!(
        &outgoing.messages()[2 * elided + 2],
        ModelMessage::ToolResult { content, .. } if content.contains("elided")
    ));
    assert!(outgoing.messages().iter().all(|message| match message {
        ModelMessage::AssistantToolCall(call) => call.reasoning_content() == Some("why"),
        _ => true,
    }));
}

#[test]
fn fit_elides_older_reasoning_after_tool_results() {
    // Arrange
    let mut request = request();
    let mut window = TurnWindow::new(&request);
    for index in 0..8 {
        let call = read_call(&format!("call-{index}"), Some(&"why".repeat(400)));
        request.record_tool_call(call.clone());
        request.record_tool_result(&call, "ok".to_string());
    }

    // Act
    let outgoing = fitted(window.fit(&ByteEstimator, budget(8_000), &request));

    // Assert
    let reasoning: Vec<Option<&str>> = outgoing
        .messages()
        .iter()
        .filter_map(|message| match message {
            ModelMessage::AssistantToolCall(call) => Some(call.reasoning_content()),
            _ => None,
        })
        .collect();
    assert!(
        reasoning[..4]
            .iter()
            .all(|text| *text == Some("[earlier reasoning elided to fit the context window]"))
    );
    assert!(
        reasoning[4..]
            .iter()
            .all(|text| text.is_some_and(|text| text.len() == 1_200))
    );
}

#[test]
fn summary_replaces_earlier_steps_and_lists_written_files() {
    // Arrange
    let mut request = request().with_model_reasoning_effort(ReasoningEffort::Low);
    request.mark_lifecycle_observed();
    let mut window = TurnWindow::new(&request);
    let write = write_call("write-1", "src/lib.rs");
    request.record_tool_call(write.clone());
    request.record_tool_result(
        &write,
        r#"{"path":"src/lib.rs","status":"applied"}"#.to_string(),
    );
    let rejected = write_call("write-2", "src/main.rs");
    request.record_tool_call(rejected.clone());
    request.record_tool_result(&rejected, r#"{"status":"rejected"}"#.to_string());
    request.record_tool_calls(vec![
        read_call("read-3", None),
        write_call("write-3", "src/lib.rs"),
    ]);
    request.record_tool_result(&read_call("read-3", None), "ok".to_string());
    request.record_tool_result(
        &write_call("write-3", "src/lib.rs"),
        r#"{"status":"applied"}"#.to_string(),
    );
    let last = read_call("last", None);
    request.record_tool_call(last.clone());
    request.record_tool_result(&last, "x".repeat(3_000));

    // Act
    let needs_summary = window.fit(&ByteEstimator, budget(2_000), &request);
    let summary_request = window
        .summary_request(&ByteEstimator, budget(2_000), &request)
        .expect("summary request");
    window.record_summary(&request, &summary_request, &json!({"state": "edited lib"}));
    let outgoing = window.fit(&ByteEstimator, budget(4_000), &request);

    // Assert
    assert!(matches!(needs_summary, Fit::NeedsSummary { .. }));
    let generation = &summary_request.request;
    assert_eq!(
        generation.messages()[0],
        ModelMessage::System(SUMMARY_INSTRUCTIONS.to_string())
    );
    assert!(generation.prompt().contains("user: task"));
    assert!(generation.prompt().contains("[tool call write"));
    assert!(!generation.prompt().contains("xxx"));
    assert_eq!(
        generation.model_reasoning_effort(),
        Some(ReasoningEffort::Low)
    );
    assert!(generation.lifecycle_observed());
    assert_eq!(generation.tools(), []);
    let outgoing = fitted(outgoing);
    assert_eq!(outgoing.messages().len(), 4);
    assert!(matches!(
        &outgoing.messages()[1],
        ModelMessage::User(note)
            if note.contains("Progress summary: {\"state\":\"edited lib\"}")
                && note.ends_with("Files written earlier in this turn: src/lib.rs")
    ));
    assert_eq!(
        outgoing.messages()[2],
        ModelMessage::AssistantToolCall(last)
    );
}

#[test]
fn summary_carries_the_previous_summary_and_lists_no_files_without_writes() {
    // Arrange
    let mut request = request();
    let mut window = TurnWindow::new(&request);
    record_reads(&mut request, 2, 100);
    let first = window
        .summary_request(&ByteEstimator, budget(100_000), &request)
        .expect("first summary request");
    window.record_summary(&request, &first, &json!({"state": "first"}));
    record_reads(&mut request, 2, 100);

    // Act
    let second = window
        .summary_request(&ByteEstimator, budget(100_000), &request)
        .expect("second summary request");
    window.record_summary(&request, &second, &json!({"state": "second"}));
    let outgoing = fitted(window.fit(&ByteEstimator, budget(100_000), &request));

    // Assert
    assert!(
        second
            .request
            .prompt()
            .contains("carry its content forward")
    );
    assert!(second.request.prompt().contains(r#"{"state":"first"}"#));
    assert!(matches!(
        &outgoing.messages()[1],
        ModelMessage::User(note)
            if note.contains(r#"{"state":"second"}"#)
                && note.ends_with("Files written earlier in this turn: none")
    ));
    assert_eq!(outgoing.messages().len(), 4);
}

#[test]
fn summary_request_is_unavailable_without_new_steps_or_remaining_summaries() {
    // Arrange
    let mut single = request();
    let single_window = TurnWindow::new(&single);
    record_reads(&mut single, 1, 100);
    let mut repeated = request();
    let mut repeated_window = TurnWindow::new(&repeated);
    for _ in 0..MAX_TURN_SUMMARIES {
        record_reads(&mut repeated, 2, 10);
        let summary_request = repeated_window
            .summary_request(&ByteEstimator, budget(100_000), &repeated)
            .expect("summary request");
        repeated_window.record_summary(&repeated, &summary_request, &json!({}));
    }
    record_reads(&mut repeated, 2, 10);

    // Act
    let without_steps = single_window.summary_request(&ByteEstimator, budget(100_000), &single);
    let exhausted = repeated_window.summary_request(&ByteEstimator, budget(100_000), &repeated);

    // Assert
    assert!(without_steps.is_none());
    assert!(exhausted.is_none());
}

#[test]
fn shedding_drops_the_oldest_history_turns_that_crowd_out_tool_output() {
    // Arrange
    let schema = OutputSchema::new(json!({"type": "object"})).expect("schema");
    let history = vec![
        ModelMessage::User("a".repeat(400)),
        ModelMessage::Assistant("b".repeat(400)),
        ModelMessage::User("c".repeat(400)),
        ModelMessage::Assistant("d".repeat(400)),
    ];
    let mut request = ModelRequest::with_history(history.clone(), "task", schema);
    let mut window = TurnWindow::new(&request).with_history_turns(vec![0..2, 2..4]);
    record_reads(&mut request, 1, 300);
    let budget = budget(1_500);

    // Act
    let crowded = window.fit(&ByteEstimator, budget, &request);
    let summary_request = window.summary_request(&ByteEstimator, budget, &request);
    let shed_oldest = window.shed_history_turn();
    let outgoing = fitted(window.fit(&ByteEstimator, budget, &request));
    let shed_newer = window.shed_history_turn();
    let exhausted = window.shed_history_turn();
    let bare = fitted(window.fit(&ByteEstimator, budget, &request));

    // Assert
    // History alone weighs 1,600 of the 1,500 budget, and the single tool
    // group leaves no earlier step to summarize.
    assert!(matches!(crowded, Fit::NeedsSummary { .. }));
    assert!(summary_request.is_none());
    assert!(shed_oldest);
    assert_eq!(outgoing.messages(), &request.messages()[2..]);
    assert!(shed_newer);
    assert!(!exhausted);
    assert_eq!(bare.messages(), &request.messages()[4..]);
    assert_eq!(&request.messages()[..4], history.as_slice());
}

#[test]
fn summary_covers_the_oldest_groups_that_fit_with_their_full_output() {
    // Arrange
    let mut request = request();
    let mut window = TurnWindow::new(&request);
    for index in 0..6 {
        let call = read_call(&format!("call-{index}"), None);
        request.record_tool_call(call.clone());
        request.record_tool_result(&call, format!("result-{index} {}", "r".repeat(1_500)));
    }
    let tight = budget(2_500);

    // Act
    let first = window
        .summary_request(&ByteEstimator, tight, &request)
        .expect("first summary request");
    window.record_summary(&request, &first, &json!({"state": "read part zero"}));
    let second = window
        .summary_request(&ByteEstimator, budget(3_000), &request)
        .expect("second summary request");

    // Assert
    let first_prompt = first.request.prompt();
    assert!(first_prompt.contains(&format!("result-0 {}", "r".repeat(1_500))));
    assert!(!first_prompt.contains("result-1"));
    let second_prompt = second.request.prompt();
    assert!(second_prompt.contains(r#"{"state":"read part zero"}"#));
    assert!(second_prompt.contains("result-1"));
    assert!(!second_prompt.contains("result-0"));
    assert!(!second_prompt.contains("result-5"));
}

#[test]
fn summary_cuts_payloads_of_a_single_group_too_large_to_fit() {
    // Arrange
    let mut request = request();
    let window = TurnWindow::new(&request);
    for index in 0..2 {
        let call = read_call(&format!("call-{index}"), None);
        request.record_tool_call(call.clone());
        request.record_tool_result(&call, format!("result-{index} {}", "r".repeat(8_000)));
    }

    // Act
    let bounded = window
        .summary_request(&ByteEstimator, budget(2_500), &request)
        .expect("bounded summary request");
    let unavailable = window.summary_request(&ByteEstimator, budget(400), &request);

    // Assert
    let prompt = bounded.request.prompt();
    assert!(prompt.contains("result-0"));
    assert!(prompt.contains("more bytes omitted"));
    assert!(!prompt.contains("result-1"));
    assert!(unavailable.is_none());
}
