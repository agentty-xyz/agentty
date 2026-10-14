use std::num::NonZeroU64;

use serde_json::json;

use super::support::{
    ByteEstimator, bash_call, budget, read_call, record_read_round, request, result_json,
    tool_result, write_call,
};
use crate::context::{
    ContextBudget, ContextEstimator, ContextFit, HeuristicContextEstimator, Notice, read_target,
    result_notice,
};
use crate::model::{ModelMessage, ModelRequest};
use crate::tool::ToolCall;
use crate::truncation::{tool_output_limit, truncate_tool_output};
use crate::turn::ContextActivity;

fn weigh(estimator: &dyn ContextEstimator, messages: &[ModelMessage]) -> u64 {
    messages
        .iter()
        .map(|message| estimator.message_weight(message))
        .sum()
}

fn read_of(id: &str, arguments: &serde_json::Value) -> ToolCall {
    ToolCall::from_json(id.to_string(), "read", &arguments.to_string(), None).expect("read call")
}

fn calls_in(messages: &[ModelMessage]) -> Vec<&ToolCall> {
    messages
        .iter()
        .flat_map(|message| match message {
            ModelMessage::AssistantToolCall(call) => std::slice::from_ref(call),
            ModelMessage::AssistantToolCalls(calls) => calls.as_slice(),
            _ => &[],
        })
        .collect()
}

/// Returns the note that stands in for dropped rounds, expected right after
/// the input.
fn dropped_note(fitted: &ModelRequest) -> &str {
    match &fitted.messages()[1] {
        ModelMessage::User(note) => note,
        other => std::panic::resume_unwind(Box::new(format!("note expected, got {other:?}"))),
    }
}

#[test]
fn fit_keeps_latest_results_whole_in_call_order_and_omits_the_rest() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "inspect");
    let calls: Vec<ToolCall> = (0..6)
        .map(|index| {
            read_of(
                &format!("r{index}"),
                &json!({"path": format!("src/f{index}.rs")}),
            )
        })
        .collect();
    request.record_tool_calls(calls.clone());
    for call in &calls {
        request.record_tool_result(call, "x".repeat(600));
    }
    let budget_weight = 2500;

    // Act
    let (fitted, activity) = ContextFit::default()
        .fit(&ByteEstimator, budget(budget_weight), &request)
        .map(|(fitted, activity)| (fitted.cloned(), activity))
        .expect("fits after omitting results");

    // Assert
    let fitted = fitted.expect("reduced copy");
    assert!(weigh(&ByteEstimator, fitted.messages()) <= budget_weight);
    let results = &fitted.messages()[2..];
    let kept = results
        .iter()
        .take_while(|message| {
            !matches!(message, ModelMessage::ToolResult { content, .. } if content.contains("context_omitted"))
        })
        .count();
    assert!(kept >= 2);
    assert_eq!(results[..kept], request.messages()[2..2 + kept]);
    for (index, message) in results.iter().enumerate().skip(kept) {
        let notice = result_json(message);
        assert!(
            notice["context_omitted"]
                .as_str()
                .expect("notice")
                .contains("read it again on its own")
        );
        assert_eq!(notice["read"], json!(format!("file src/f{index}.rs")));
        assert_eq!(notice["original_bytes"], json!(600));
    }
    assert_eq!(activity.truncated_results(), 6 - kept);
    assert_eq!(activity.dropped_tool_calls(), 0);
}

#[test]
fn omitted_results_stay_out_of_the_cached_prefix_and_are_later_elided() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "inspect");
    let calls = vec![read_call("large"), read_call("small")];
    request.record_tool_calls(calls.clone());
    request.record_tool_result(&calls[0], "x".repeat(3000));
    request.record_tool_result(&calls[1], "y".repeat(100));
    let budget_weight = 2000;
    let mut fit = ContextFit::default();
    let (first, first_activity) = fit
        .fit(&ByteEstimator, budget(budget_weight), &request)
        .map(|(fitted, activity)| (fitted.cloned(), activity))
        .expect("first fit");

    // Act
    record_read_round(&mut request, "next", 10);
    let (second, second_activity) = fit
        .fit(&ByteEstimator, budget(budget_weight), &request)
        .map(|(fitted, activity)| (fitted.cloned(), activity))
        .expect("second fit");

    // Assert
    let first = first.expect("first copy");
    assert!(
        result_json(&first.messages()[2])
            .get("context_omitted")
            .is_some()
    );
    assert_eq!(first.messages()[3], request.messages()[3]);
    assert_eq!(first_activity.truncated_results(), 1);
    let second = second.expect("second copy");
    let notice = result_json(&second.messages()[2]);
    assert!(notice.get("context_cleared").is_some());
    assert_eq!(notice["original_bytes"], json!(3000));
    assert_eq!(second_activity.truncated_results(), 0);
    assert!(second_activity.elided_results() >= 1);
}

#[test]
fn an_omitted_command_result_keeps_its_outcome_and_warns_against_repeating_it() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), &"i".repeat(1000));
    let bash = bash_call("b0");
    request.record_tool_call(bash.clone());
    request.record_tool_result(
        &bash,
        json!({"exit_code": 0, "stdout": "y".repeat(2000), "termination": "completed"}).to_string(),
    );

    // Act
    let (fitted, activity) = ContextFit::default()
        .fit(&ByteEstimator, budget(1500), &request)
        .map(|(fitted, activity)| (fitted.cloned(), activity))
        .expect("fits after omitting the result");

    // Assert
    let fitted = fitted.expect("reduced copy");
    let notice = result_json(&fitted.messages()[2]);
    assert!(
        notice["context_omitted"]
            .as_str()
            .expect("notice")
            .contains("do NOT repeat")
    );
    assert_eq!(notice["exit_code"], json!(0));
    assert_eq!(notice["termination"], json!("completed"));
    assert_eq!(activity.truncated_results(), 1);
}

#[test]
fn a_recorded_result_cut_to_its_limit_fits_without_dropping_earlier_rounds() {
    // Arrange
    let estimator = HeuristicContextEstimator;
    let budget = ContextBudget::new(NonZeroU64::new(7_000).expect("nonzero"))
        .with_reserved_output(500)
        .expect("budget");
    let write = write_call("w0", "@@ -0,0 +1 @@\n+context fitting check\n");
    let history = vec![
        ModelMessage::User("Create notes.txt".to_string()),
        ModelMessage::AssistantToolCall(write),
        tool_result(
            "w0",
            "write",
            &json!({"path": "notes.txt", "status": "applied"}).to_string(),
        ),
        ModelMessage::Assistant(r#"{"answer":"ok"}"#.to_string()),
    ];
    let mut request = request(
        vec![ModelMessage::System("s".repeat(3000))],
        vec![history],
        "Read part1.txt through part8.txt, then large.txt.",
    );
    let line = "ledger harbor signal meadow copper lantern orbit quartz ".repeat(2);
    let part = (0..72)
        .map(|_| line.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let mut fit = ContextFit::default();
    let mut activities = Vec::new();
    for index in 1..=8 {
        let call = read_of(
            &format!("r{index}"),
            &json!({"path": format!("part{index}.txt")}),
        );
        request.record_tool_call(call.clone());
        request.record_tool_result(
            &call,
            json!({"content": part, "path": format!("part{index}.txt")}).to_string(),
        );
        let (_, activity) = fit
            .fit(&estimator, budget, &request)
            .expect("each part fits");
        activities.push(activity);
    }
    let large = read_of("large", &json!({"path": "large.txt"}));
    request.record_tool_call(large.clone());
    let content = format!(
        "The code word of the large file is ivory-97. {}",
        "x".repeat(36_000)
    );
    let (content, cut) = truncate_tool_output(
        &estimator,
        &large,
        json!({"content": content}).to_string(),
        tool_output_limit(budget),
    );
    request.record_tool_result(&large, content);

    // Act
    let (fitted, activity) = fit
        .fit(&estimator, budget, &request)
        .map(|(fitted, activity)| (fitted.cloned(), activity))
        .expect("fits after eliding older results");

    // Assert
    assert!(cut);
    let fitted = fitted.expect("reduced copy");
    assert!(
        activities
            .iter()
            .all(|activity| activity.dropped_tool_calls() == 0)
    );
    assert_eq!(activity.dropped_tool_calls(), 0);
    assert_eq!(activity.dropped_history_turns(), 0);
    assert_eq!(activity.truncated_results(), 0);
    assert!(activity.elided_results() >= 1);
    assert_eq!(calls_in(fitted.messages()).len(), 10);
    let latest = fitted.messages().last().expect("large result");
    assert_eq!(latest, request.messages().last().expect("recorded result"));
    assert!(
        result_json(latest)["content"]
            .as_str()
            .expect("content")
            .starts_with("The code word of the large file is ivory-97.")
    );
    assert_ne!(activity, ContextActivity::default());
}

#[test]
fn dropped_round_note_lists_distinct_read_targets_within_a_bound() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "loop");
    for index in 0..12 {
        let call = read_of(
            &format!("r{index}"),
            &json!({"path": format!("p{}.txt", index.max(1) - 1), "offset": 3, "limit": 5}),
        );
        request.record_tool_call(call.clone());
        request.record_tool_result(&call, "x".repeat(150));
    }
    let mut fit = ContextFit::default();

    // Act
    let (fitted, activity) = fit
        .fit(&ByteEstimator, budget(1500), &request)
        .map(|(fitted, activity)| (fitted.cloned(), activity))
        .expect("fits after dropping rounds");

    // Assert
    let fitted = fitted.expect("reduced copy");
    assert_eq!(activity.dropped_tool_calls(), 10);
    let note = dropped_note(&fitted);
    assert!(note.contains("removed with their results to fit the context window"));
    assert!(note.contains("Reads whose results were removed: `file p0.txt offset=3 limit=5`, "));
    assert!(note.contains("`file p7.txt offset=3 limit=5`, and 1 more."));
    assert_eq!(note.matches("p0.txt").count(), 1);
    assert!(note.contains("Re-read only what you still need"));
}

#[test]
fn read_targets_name_the_action_query_path_side_and_window() {
    // Arrange
    let cases = [
        (json!({"path": "src/lib.rs"}), "file src/lib.rs"),
        (
            json!({"path": "src/lib.rs", "offset": 40, "limit": 20}),
            "file src/lib.rs offset=40 limit=20",
        ),
        (
            json!({"action": "search", "query": "fit(", "path": "src"}),
            r#"search "fit(" in src"#,
        ),
        (json!({"action": "search", "query": "x"}), r#"search "x""#),
        (json!({"action": "list"}), "list"),
        (
            json!({"action": "show", "path": "a.rs", "side": "base", "offset": 2}),
            "show a.rs base offset=2",
        ),
    ];

    for (arguments, expected) in cases {
        // Act
        let call = read_of("r", &arguments);
        let target = read_target(call.read_arguments().expect("read arguments"));

        // Assert
        assert_eq!(target, expected);
    }
}

#[test]
fn read_notice_without_a_known_target_omits_the_target() {
    // Arrange
    let content = r#"{"content":"fn main() {}","path":"src/main.rs"}"#;

    // Act
    let notice = result_notice(Notice::Cleared, "read", content, None);

    // Assert
    let notice: serde_json::Value = serde_json::from_str(&notice).expect("notice json");
    assert!(notice.get("read").is_none());
    assert_eq!(notice["original_bytes"], content.len());
}
