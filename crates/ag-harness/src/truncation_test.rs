use std::num::NonZeroU64;

use serde_json::{Value, json};

use super::{
    TOOL_OUTPUT_CAPACITY_PERCENT, TOOL_OUTPUT_WEIGHT_LIMIT, TRUNCATION_KEY, tool_output_limit,
    truncate_tool_output,
};
use crate::context::{ContextBudget, ContextEstimator, HeuristicContextEstimator};
use crate::model::ModelMessage;
use crate::tool::ToolCall;

fn call(name: &str, arguments: &Value) -> ToolCall {
    ToolCall::from_json("call".to_string(), name, &arguments.to_string(), None).expect("tool call")
}

fn read() -> ToolCall {
    call("read", &json!({"path": "src/lib.rs"}))
}

fn bash() -> ToolCall {
    call("bash", &json!({"command": "cargo test"}))
}

fn weight(call: &ToolCall, content: &str) -> u64 {
    HeuristicContextEstimator.message_weight(&ModelMessage::ToolResult {
        call_id: call.id().to_string(),
        content: content.to_string(),
        name: call.name().to_string(),
    })
}

fn numbered_lines(count: usize) -> String {
    (1..=count)
        .map(|line| format!("line {line:04} of the generated file"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn warning(result: &Value) -> &str {
    result[TRUNCATION_KEY].as_str().expect("warning")
}

#[test]
fn tool_output_limit_is_a_quarter_of_a_small_capacity_and_fixed_for_large_ones() {
    // Arrange
    let small = ContextBudget::new(NonZeroU64::new(32_000).expect("nonzero"))
        .with_reserved_output(16_000)
        .expect("small budget");
    let large = ContextBudget::new(NonZeroU64::new(1_000_000).expect("nonzero"))
        .with_reserved_output(16_000)
        .expect("large budget");

    // Act
    let small_limit = tool_output_limit(small);
    let large_limit = tool_output_limit(large);

    // Assert
    assert_eq!(small_limit, 16_000 * TOOL_OUTPUT_CAPACITY_PERCENT / 100);
    assert_eq!(large_limit, TOOL_OUTPUT_WEIGHT_LIMIT);
}

#[test]
fn a_result_within_the_limit_is_recorded_unchanged() {
    // Arrange
    let content = json!({"content": "fn main() {}", "path": "src/main.rs"}).to_string();

    // Act
    let (recorded, cut) =
        truncate_tool_output(&HeuristicContextEstimator, &read(), content.clone(), 1_000);

    // Assert
    assert!(!cut);
    assert_eq!(recorded, content);
}

#[test]
fn a_read_keeps_its_fields_and_names_the_missing_lines_to_request() {
    // Arrange
    let file = numbered_lines(400);
    let content = json!({
        "content": file,
        "end_line": 409,
        "next_offset": null,
        "path": "src/lib.rs",
        "start_line": 10,
        "truncated": false,
    })
    .to_string();
    let limit = 1_000;

    // Act
    let (recorded, cut) = truncate_tool_output(&HeuristicContextEstimator, &read(), content, limit);

    // Assert
    assert!(cut);
    assert!(weight(&read(), &recorded) <= limit);
    let result: Value = serde_json::from_str(&recorded).expect("cut result is JSON");
    assert_eq!(result["path"], "src/lib.rs");
    assert_eq!(result["start_line"], 10);
    let excerpt = result["content"].as_str().expect("content");
    let (head, rest) = excerpt.split_once('…').expect("marker start");
    let (marker, tail) = rest.split_once('…').expect("marker end");
    assert!(head.starts_with("line 0001") && head.ends_with('\n'));
    assert!(tail.starts_with("line ") && tail.ends_with("line 0400 of the generated file"));
    let first = 10 + head.lines().count();
    let last = 10 + 400 - tail.lines().count() - 1;
    let omitted = last - first + 1;
    assert!(marker.starts_with(&format!("{omitted} lines (")));
    let warning = warning(&result);
    assert!(warning.starts_with("Warning: truncated output (original weight about "));
    assert!(warning.contains("`content` (400 lines)"));
    assert!(warning.contains(&format!(
        "lines {first}-{last} are missing (offset={first} limit={omitted})"
    )));
    assert!(warning.contains("offset/limit"));
}

#[test]
fn a_command_keeps_its_outcome_and_cuts_its_largest_outputs_first() {
    // Arrange
    let content = json!({
        "exit_code": 101,
        "stderr": "e".repeat(3_000),
        "stdout": "o".repeat(20_000),
        "termination": "completed",
    })
    .to_string();

    // Act
    let (one_cut, _) =
        truncate_tool_output(&HeuristicContextEstimator, &bash(), content.clone(), 2_000);
    let (both_cut, _) = truncate_tool_output(&HeuristicContextEstimator, &bash(), content, 600);

    // Assert
    let one_cut: Value = serde_json::from_str(&one_cut).expect("JSON");
    assert_eq!(one_cut["exit_code"], 101);
    assert_eq!(one_cut["termination"], "completed");
    assert_eq!(one_cut["stderr"], "e".repeat(3_000));
    assert!(
        one_cut["stdout"]
            .as_str()
            .expect("stdout")
            .contains(" bytes truncated…")
    );
    assert!(warning(&one_cut).contains("The middle of `stdout` (1 line) was cut."));
    assert!(warning(&one_cut).contains("a command that filters its output"));
    let both_cut: Value = serde_json::from_str(&both_cut).expect("JSON");
    assert_eq!(both_cut["exit_code"], 101);
    assert!(warning(&both_cut).contains("`stdout` (1 line) was cut. The middle of `stderr`"));
}

#[test]
fn content_that_is_not_a_json_object_is_cut_inside_one_output_field() {
    // Arrange
    let content = format!("{}é", "ü".repeat(5_000));

    // Act
    let (recorded, cut) = truncate_tool_output(&HeuristicContextEstimator, &bash(), content, 500);

    // Assert
    assert!(cut);
    let result: Value = serde_json::from_str(&recorded).expect("JSON");
    let output = result["output"].as_str().expect("output");
    assert!(output.starts_with('ü') && output.ends_with('é'));
    assert!(warning(&result).contains("The middle of `output` (1 line) was cut."));
    assert!(weight(&bash(), &recorded) <= 500);
}

#[test]
fn a_limit_below_even_empty_excerpts_records_the_smallest_form() {
    // Arrange
    let content = json!({"stdout": "o".repeat(4_000), "status": "x".repeat(400)}).to_string();

    // Act
    let (recorded, cut) = truncate_tool_output(&HeuristicContextEstimator, &bash(), content, 10);

    // Assert
    assert!(cut);
    let result: Value = serde_json::from_str(&recorded).expect("JSON");
    assert_eq!(result["stdout"], "…4000 bytes truncated…");
    assert_eq!(result["status"], "…400 bytes truncated…");
}
