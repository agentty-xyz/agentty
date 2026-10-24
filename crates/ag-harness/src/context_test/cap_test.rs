use std::num::NonZeroU64;

use serde_json::json;

use super::support::{
    ByteEstimator, bash_command_call, budget, read_call, read_call_of, record_read_round, request,
    result_json, sent, tool_result, write_call,
};
use crate::context::{
    ContextBudget, ContextEstimator, ContextFit, DROP_TARGET_PERCENT, HeuristicContextEstimator,
    LATEST_ROUND_CAP_PERCENT, RESULT_CAP_PERCENT, cleared_result_notice, read_target,
    repeated_read_result,
};
use crate::model::{ModelMessage, ModelRequest};
use crate::tool::ToolCall;
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
fn fit_caps_the_latest_round_to_half_the_budget_at_one_common_size() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "inspect");
    record_read_round(&mut request, "older0", 950);
    record_read_round(&mut request, "older1", 950);
    let calls = vec![
        read_call("big0"),
        read_call("big1"),
        read_call("mid"),
        read_call("small"),
    ];
    request.record_tool_calls(calls.clone());
    for (call, bytes) in calls.iter().zip([900, 900, 300, 100]) {
        request.record_tool_result(call, "x".repeat(bytes));
    }
    let budget_weight = 4000;

    // Act
    let (fitted, activity) = ContextFit::default()
        .fit(&ByteEstimator, budget(budget_weight), &request, None)
        .map(sent)
        .expect("fits after capping");

    // Assert
    let fitted = fitted.expect("reduced copy");
    let latest = &fitted.messages()[fitted.messages().len() - 4..];
    assert!(weigh(&ByteEstimator, latest) <= budget_weight * LATEST_ROUND_CAP_PERCENT / 100);
    assert!(weigh(&ByteEstimator, &latest[..2]) > 2 * 700);
    assert!(result_json(&latest[0]).get("context_truncated").is_some());
    assert!(result_json(&latest[1]).get("context_truncated").is_some());
    assert_eq!(
        latest[2..],
        request.messages()[request.messages().len() - 2..]
    );
    assert_eq!(activity.truncated_results(), 2);
    assert_eq!(activity.elided_results(), 2);
    assert_eq!(activity.dropped_tool_calls(), 0);
    assert!(weigh(&ByteEstimator, fitted.messages()) <= budget_weight * DROP_TARGET_PERCENT / 100);
}

#[test]
fn capped_read_results_keep_whole_lines_and_say_where_to_continue() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "inspect");
    record_read_round(&mut request, "older", 2600);
    let content = (1..=200)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let call = read_call_of("big", "big.txt");
    request.record_tool_call(call.clone());
    request.record_tool_result(
        &call,
        json!({
            "path": "big.txt",
            "start_line": 1,
            "end_line": 200,
            "total_lines": 200,
            "truncated": false,
            "content": content,
        })
        .to_string(),
    );
    let budget_weight = 4000;

    // Act
    let (fitted, activity) = ContextFit::default()
        .fit(&ByteEstimator, budget(budget_weight), &request, None)
        .map(sent)
        .expect("fits after capping");

    // Assert
    let fitted = fitted.expect("reduced copy");
    let page = result_json(fitted.messages().last().expect("latest result"));
    let shown = page["content"].as_str().expect("content");
    let end_line = page["end_line"].as_u64().expect("end line");
    assert!(page.get("context_truncated").is_none());
    assert!(shown.ends_with(&format!("line {end_line}")));
    assert_eq!(page["next_offset"], end_line + 1);
    assert_eq!(
        page["continuation"],
        format!(
            "Showing lines 1-{end_line} of 200. Use offset={} limit={end_line} to continue.",
            end_line + 1
        )
    );
    assert!(
        ByteEstimator.message_weight(fitted.messages().last().expect("latest result"))
            <= budget_weight * RESULT_CAP_PERCENT / 100
    );
    // The older result was sent before, so it is elided rather than cut.
    assert_eq!(
        result_json(&fitted.messages()[2])["read"],
        json!("file older.txt")
    );
    assert_eq!(activity.truncated_results(), 1);
    assert_eq!(activity.elided_results(), 1);
}

#[test]
fn capped_results_stay_capped_identically_and_are_later_elided_from_the_original() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "inspect");
    record_read_round(&mut request, "large", 5000);
    let budget_weight = 4000;
    let mut fit = ContextFit::default();
    let (first, _) = fit
        .fit(&ByteEstimator, budget(budget_weight), &request, None)
        .map(sent)
        .expect("first fit");
    let first = first.expect("capped copy");

    // Act
    record_read_round(&mut request, "next", 10);
    let (second, second_activity) = fit
        .fit(&ByteEstimator, budget(budget_weight), &request, None)
        .map(sent)
        .expect("second fit");
    for index in 0..4 {
        record_read_round(&mut request, &format!("more{index}"), 900);
    }
    let (third, third_activity) = fit
        .fit(&ByteEstimator, budget(budget_weight), &request, None)
        .map(sent)
        .expect("third fit");

    // Assert
    let second = second.expect("second copy");
    assert_eq!(
        second.messages()[..first.messages().len()],
        *first.messages()
    );
    assert_eq!(second_activity.truncated_results(), 1);
    let third = third.expect("third copy");
    let notice = result_json(&third.messages()[2]);
    assert_eq!(notice["original_bytes"], json!(5000));
    assert_eq!(notice["read"], json!("file large.txt"));
    assert_eq!(third_activity.truncated_results(), 0);
    assert!(third_activity.elided_results() >= 1);
    assert_eq!(third_activity.dropped_tool_calls(), 0);
}

#[test]
fn fit_cuts_latest_results_further_only_when_capping_is_not_enough() {
    // Arrange
    let mut capped = request(Vec::new(), Vec::new(), &"i".repeat(1400));
    let calls = vec![read_call("large"), read_call("small")];
    capped.record_tool_calls(calls.clone());
    capped.record_tool_result(&calls[0], "x".repeat(5000));
    capped.record_tool_result(&calls[1], "y".repeat(100));
    let mut whole = request(Vec::new(), Vec::new(), &"i".repeat(1700));
    record_read_round(&mut whole, "medium", 400);
    let mut capped_fitter = ContextFit::default();

    // Act
    let (capped_fit, capped_activity) = capped_fitter
        .fit(&ByteEstimator, budget(2000), &capped, None)
        .map(sent)
        .expect("fits after cutting the capped result further");
    // The cut fills the budget exactly; a little more room fits the next
    // round.
    record_read_round(&mut capped, "next", 10);
    let (later, later_activity) = capped_fitter
        .fit(&ByteEstimator, budget(2100), &capped, None)
        .map(sent)
        .expect("the next request fits");
    let (whole_fit, whole_activity) = ContextFit::default()
        .fit(&ByteEstimator, budget(2000), &whole, None)
        .map(sent)
        .expect("fits after cutting the uncapped result");

    // Assert
    let capped_fit = capped_fit.expect("reduced copy");
    assert!(weigh(&ByteEstimator, capped_fit.messages()) <= 2000);
    assert_eq!(capped_activity.truncated_results(), 1);
    assert!(ByteEstimator.message_weight(&capped_fit.messages()[2]) < 500);
    assert_eq!(capped_fit.messages()[3], capped.messages()[3]);
    // The cut result stays cut, so the next request only appends.
    let later = later.expect("reduced copy");
    assert_eq!(
        later.messages(),
        [capped_fit.messages(), &capped.messages()[4..]].concat()
    );
    assert_eq!(later_activity, capped_activity);
    let whole_fit = whole_fit.expect("reduced copy");
    assert!(weigh(&ByteEstimator, whole_fit.messages()) <= 2000);
    assert_eq!(whole_activity.truncated_results(), 1);
    assert!(
        result_json(&whole_fit.messages()[2])
            .get("context_truncated")
            .is_some()
    );
}

#[test]
fn an_oversized_latest_result_is_capped_without_dropping_earlier_rounds() {
    // Arrange
    let estimator = HeuristicContextEstimator::DEFAULT;
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
            .fit(&estimator, budget, &request, None)
            .map(sent)
            .expect("each part fits");
        activities.push(activity);
    }
    let large = read_of("large", &json!({"path": "large.txt"}));
    request.record_tool_call(large.clone());
    let content = format!(
        "The code word of the large file is ivory-97. {}",
        "x".repeat(36_000)
    );
    request.record_tool_result(&large, json!({"content": content}).to_string());

    // Act
    let (fitted, activity) = fit
        .fit(&estimator, budget, &request, None)
        .map(sent)
        .expect("fits after capping the large result");

    // Assert
    let fitted = fitted.expect("reduced copy");
    assert!(
        activities
            .iter()
            .all(|activity| activity.dropped_tool_calls() == 0)
    );
    assert_eq!(activity.dropped_tool_calls(), 0);
    assert_eq!(activity.dropped_history_turns(), 0);
    assert_eq!(activity.truncated_results(), 1);
    assert!(activity.elided_results() >= 1);
    assert_eq!(calls_in(fitted.messages()).len(), 10);
    let excerpt = result_json(fitted.messages().last().expect("large result"));
    assert!(excerpt["head"].as_str().expect("head").contains("ivory-97"));
    assert!(
        estimator.message_weight(fitted.messages().last().expect("large result"))
            <= 6500 * RESULT_CAP_PERCENT / 100
    );
    assert_ne!(activity, ContextActivity::default());
}

#[test]
fn dropping_a_round_releases_its_capped_result() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "loop");
    let outcome = "o".repeat(250);
    let mut fit = ContextFit::default();
    // Each result is capped while its round is the latest, and its notice,
    // which keeps the long outcome fields, never weighs less than the cut.
    for index in 0..4 {
        let bash = bash_command_call(&format!("b{index}"), &format!("cargo test {index}"));
        request.record_tool_call(bash.clone());
        request.record_tool_result(
            &bash,
            json!({
                "execution_failure": outcome,
                "path": outcome,
                "signal": outcome,
                "status": outcome,
                "stdout": "y".repeat(1800),
                "termination": outcome,
            })
            .to_string(),
        );
        fit.fit(&ByteEstimator, budget(2500), &request, None)
            .map(sent)
            .expect("each round fits after capping");
    }
    record_read_round(&mut request, "latest", 10);

    // Act
    let (fitted, activity) = fit
        .fit(&ByteEstimator, budget(2500), &request, None)
        .map(sent)
        .expect("fits after dropping rounds");

    // Assert
    let fitted = fitted.expect("reduced copy");
    assert_eq!(activity.elided_results(), 0);
    assert!(activity.dropped_tool_calls() >= 1);
    assert_eq!(
        activity.truncated_results(),
        4 - activity.dropped_tool_calls()
    );
    assert!(dropped_note(&fitted).contains("Commands run: `cargo test 0`"));
    assert!(!dropped_note(&fitted).contains("Reads whose results"));
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
        .fit(&ByteEstimator, budget(1500), &request, None)
        .map(sent)
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
    let notice = cleared_result_notice("read", content, None, None);

    // Assert
    let notice: serde_json::Value = serde_json::from_str(&notice).expect("notice json");
    assert!(notice.get("read").is_none());
    assert_eq!(notice["original_bytes"], content.len());
}

#[test]
fn a_refit_under_a_lower_budget_caps_the_latest_round_instead_of_dropping_rounds() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "inspect");
    record_read_round(&mut request, "older0", 300);
    record_read_round(&mut request, "older1", 300);
    let bash = bash_command_call("b0", "cargo test");
    request.record_tool_call(bash.clone());
    request.record_tool_result(
        &bash,
        json!({"exit_code": 0, "stdout": "y".repeat(2000)}).to_string(),
    );
    let mut fit = ContextFit::default();
    let (whole, _) = fit
        .fit(&ByteEstimator, budget(3000), &request, None)
        .map(sent)
        .expect("the request fits whole");

    // Act
    // The provider rejected the request, so the same request is fitted again
    // under a lower ceiling.
    let (fitted, activity) = fit
        .fit(&ByteEstimator, budget(2400), &request, None)
        .map(sent)
        .expect("fits after capping the latest result");

    // Assert
    assert_eq!(whole, None);
    let fitted = fitted.expect("reduced copy");
    assert_eq!(activity.truncated_results(), 1);
    assert_eq!(activity.dropped_tool_calls(), 0);
    assert_eq!(calls_in(fitted.messages()).len(), 3);
    assert!(ByteEstimator.message_weight(fitted.messages().last().expect("bash result")) <= 600);
}

#[test]
fn a_repeat_of_a_cut_result_is_cleared() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "inspect");
    let calls = vec![read_call_of("a1", "a.txt"), read_call_of("a2", "a.txt")];
    request.record_tool_calls(calls.clone());
    request.record_tool_result(&calls[0], "x".repeat(5000));
    request.record_tool_result(&calls[1], repeated_read_result("a1"));

    // Act
    let (fitted, activity) = ContextFit::default()
        .fit(&ByteEstimator, budget(2000), &request, None)
        .map(sent)
        .expect("fits after capping");

    // Assert
    let fitted = fitted.expect("reduced copy");
    assert_eq!(activity.truncated_results(), 1);
    assert!(
        result_json(&fitted.messages()[2])
            .get("context_truncated")
            .is_some()
    );
    let repeat = result_json(&fitted.messages()[3]);
    assert!(
        repeat["context_cleared"]
            .as_str()
            .expect("notice text")
            .contains("cleared or cut")
    );
}

#[test]
fn a_repeat_of_a_result_cut_as_the_last_resort_is_cleared() {
    // Arrange
    // A long call identifier makes the repeat note weigh more than the
    // notice that replaces it.
    let first = format!("call_{}", "1".repeat(80));
    let mut request = request(Vec::new(), Vec::new(), &"i".repeat(1100));
    let calls = vec![read_call_of(&first, "a.txt"), read_call_of("a2", "a.txt")];
    request.record_tool_calls(calls.clone());
    request.record_tool_result(&calls[0], "x".repeat(400));
    request.record_tool_result(&calls[1], repeated_read_result(&first));

    // Act
    let (fitted, activity) = ContextFit::default()
        .fit(&ByteEstimator, budget(2000), &request, None)
        .map(sent)
        .expect("fits after cutting the latest result");

    // Assert
    let fitted = fitted.expect("reduced copy");
    assert!(weigh(&ByteEstimator, fitted.messages()) <= 2000);
    assert_eq!(activity.truncated_results(), 1);
    assert!(
        result_json(&fitted.messages()[2])
            .get("context_truncated")
            .is_some()
    );
    assert!(
        result_json(&fitted.messages()[3])["context_cleared"]
            .as_str()
            .expect("notice text")
            .contains("cleared or cut")
    );
}
