use serde_json::json;

use super::support::{
    ByteEstimator, FixedEstimator, bash_call, budget, read_call, reasoned_read_call,
    record_read_round, request, result_json, sent, tool_result, write_call,
};
use crate::context::{
    CLEARED_REASONING, ContextBudget, ContextEstimator, ContextFit, FIT_TARGET_PERCENT,
    HeuristicContextEstimator,
};
use crate::model::{ModelMessage, ModelRequest};
use crate::tool::ToolCall;
use crate::turn::{ContextActivity, TurnError};

fn weigh(request: &ModelRequest) -> u64 {
    request
        .messages()
        .iter()
        .map(|message| ByteEstimator.message_weight(message))
        .sum()
}

fn target(budget_weight: u64) -> u64 {
    budget_weight * FIT_TARGET_PERCENT / 100
}

fn read_rounds(sizes: &[usize]) -> ModelRequest {
    let mut request = request(Vec::new(), Vec::new(), "inspect");
    for (index, bytes) in sizes.iter().enumerate() {
        record_read_round(&mut request, &format!("r{index}"), *bytes);
    }

    request
}

/// Runs one fit and returns an owned copy of the request to send.
fn fit_owned(
    fit: &mut ContextFit,
    budget: ContextBudget,
    request: &ModelRequest,
) -> Result<(Option<ModelRequest>, ContextActivity), TurnError> {
    fit.fit(&ByteEstimator, budget, request, None).map(sent)
}

fn call_of(message: &ModelMessage) -> &ToolCall {
    match message {
        ModelMessage::AssistantToolCall(call) => call,
        other => std::panic::resume_unwind(Box::new(format!("tool call expected, got {other:?}"))),
    }
}

#[test]
fn fit_sends_the_canonical_request_while_it_fits() {
    // Arrange
    let request = read_rounds(&[100]);

    // Act
    let (fitted, activity) =
        fit_owned(&mut ContextFit::default(), budget(10_000), &request).expect("fits");

    // Assert
    assert_eq!(fitted, None);
    assert_eq!(activity, ContextActivity::default());
}

#[test]
fn the_fixed_request_weight_counts_against_the_budget() {
    // Arrange
    let request = read_rounds(&[1000, 1000]);
    let estimator = HeuristicContextEstimator::DEFAULT.with_request_tokens(50);
    let messages: u64 = request
        .messages()
        .iter()
        .map(|message| estimator.message_weight(message))
        .sum();
    let mut fitting = ContextFit::default();
    let mut over = ContextFit::default();

    // Act
    let unchanged = fitting
        .fit(&estimator, budget(messages + 50), &request, None)
        .map(sent);
    let reduced = over
        .fit(&estimator, budget(messages + 49), &request, None)
        .map(sent);

    // Assert
    assert_eq!(unchanged.expect("fits").0, None);
    assert_eq!(fitting.sent_weight(), messages + 50);
    assert!(reduced.expect("fits after reduction").0.is_some());
    assert_eq!(over.unreduced_weight(), messages + 50);
}

#[test]
fn fit_elides_oldest_results_down_to_the_target_and_keeps_the_latest_round() {
    // Arrange
    let request = read_rounds(&[1000, 1000, 1000, 1000]);
    let budget_weight = weigh(&request) - 1;

    // Act
    let (fitted, activity) = fit_owned(&mut ContextFit::default(), budget(budget_weight), &request)
        .expect("fits after elision");

    // Assert
    let fitted = fitted.expect("reduced copy");
    assert!(weigh(&fitted) <= target(budget_weight));
    assert_eq!(activity.elided_results(), 2);
    let notice = result_json(&fitted.messages()[2]);
    assert_eq!(notice["original_bytes"], json!(1000));
    assert_eq!(notice["read"], json!("file r0.txt"));
    assert!(
        notice["context_cleared"]
            .as_str()
            .expect("notice text")
            .contains("offset/limit")
    );
    assert!(
        result_json(&fitted.messages()[4])
            .get("context_cleared")
            .is_some()
    );
    assert_eq!(fitted.messages()[5..], request.messages()[5..]);
    assert_eq!(request.messages()[2].retained_bytes(), 1006);
}

#[test]
fn later_requests_reuse_the_reduced_prefix_until_the_budget_is_exceeded_again() {
    // Arrange
    let mut request = read_rounds(&[1000, 1000, 1000, 1000]);
    let budget_weight = weigh(&request) - 1;
    let mut fit = ContextFit::default();
    let (first, _) = fit_owned(&mut fit, budget(budget_weight), &request).expect("first fit");
    let first = first.expect("first reduced copy");

    // Act
    record_read_round(&mut request, "small", 600);
    let (second, second_activity) =
        fit_owned(&mut fit, budget(budget_weight), &request).expect("second fit");
    record_read_round(&mut request, "large", 1000);
    let (third, third_activity) =
        fit_owned(&mut fit, budget(budget_weight), &request).expect("third fit");

    // Assert
    // The second request is over the target but within the budget, so it
    // only replays the first request's elisions behind a shared prefix.
    let second = second.expect("second reduced copy");
    assert!(weigh(&second) > target(budget_weight));
    assert_eq!(second_activity.elided_results(), 2);
    assert_eq!(
        second.messages()[..first.messages().len()],
        *first.messages()
    );
    // The third exceeds the budget again and elides older results down to
    // the target, keeping its newest round whole.
    let third = third.expect("third reduced copy");
    assert!(weigh(&third) <= target(budget_weight));
    assert_eq!(third_activity.elided_results(), 4);
    assert_eq!(third.messages()[..5], first.messages()[..5]);
    assert_eq!(third.messages().last(), request.messages().last());
}

#[test]
fn cleared_bash_and_write_results_keep_their_outcome_and_forbid_repeats() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "fix");
    let bash = bash_call("b1");
    request.record_tool_call(bash.clone());
    request.record_tool_result(
        &bash,
        json!({
            "cleanup_failed": false,
            "execution_failure": null,
            "exit_code": 101,
            "signal": null,
            "stdout": "y".repeat(2000),
            "termination": "completed",
        })
        .to_string(),
    );
    let write = write_call("w1", "@@ -1 +1 @@\n-a\n+b\n");
    request.record_tool_call(write.clone());
    request.record_tool_result(
        &write,
        json!({"error": "z".repeat(600), "path": "src/lib.rs", "status": "rejected"}).to_string(),
    );
    for index in 0..3 {
        record_read_round(&mut request, &format!("f{index}"), 2000);
    }
    let read = read_call("r1");
    request.record_tool_call(read.clone());
    request.record_tool_result(&read, "content".to_string());
    let budget_weight = weigh(&request) - 1;

    // Act
    let (fitted, activity) = fit_owned(&mut ContextFit::default(), budget(budget_weight), &request)
        .expect("fits after elision");

    // Assert
    let fitted = fitted.expect("reduced copy");
    assert_eq!(activity.truncated_results(), 0);
    assert_eq!(activity.elided_results(), 3);
    let bash_notice = result_json(&fitted.messages()[2]);
    assert!(
        bash_notice["context_cleared"]
            .as_str()
            .expect("notice text")
            .contains("do NOT repeat")
    );
    assert_eq!(bash_notice["exit_code"], json!(101));
    assert_eq!(bash_notice["termination"], json!("completed"));
    assert_eq!(bash_notice.get("signal"), None);
    assert_eq!(bash_notice.get("stdout"), None);
    let write_notice = result_json(&fitted.messages()[4]);
    assert_eq!(write_notice["status"], json!("rejected"));
    assert_eq!(write_notice["path"], json!("src/lib.rs"));
    assert_eq!(write_notice.get("error"), None);
    assert_eq!(fitted.messages()[1], request.messages()[1]);
}

#[test]
fn fit_clears_older_reasoning_once_results_cannot_shrink() {
    // Arrange
    let reasoning = "r".repeat(2000);
    let history = vec![
        ModelMessage::User("earlier".to_string()),
        ModelMessage::AssistantReasoning {
            content: "{}".to_string(),
            reasoning_content: reasoning.clone(),
        },
    ];
    let mut request = request(Vec::new(), vec![history], "inspect");
    let batch = vec![
        reasoned_read_call("r1", Some(&reasoning)),
        reasoned_read_call("r2", None),
    ];
    request.record_tool_calls(batch.clone());
    for call in &batch {
        request.record_tool_result(call, "ok".to_string());
    }
    let latest = reasoned_read_call("r3", Some(&reasoning));
    request.record_tool_call(latest.clone());
    request.record_tool_result(&latest, "ok".to_string());

    // Act
    let (fitted, activity) = fit_owned(&mut ContextFit::default(), budget(4000), &request)
        .expect("fits after clearing reasoning");

    // Assert
    let fitted = fitted.expect("reduced copy");
    assert_eq!(activity.cleared_reasoning(), 2);
    assert_eq!(activity.elided_results(), 0);
    assert_eq!(
        fitted.messages()[1],
        ModelMessage::AssistantReasoning {
            content: "{}".to_string(),
            reasoning_content: CLEARED_REASONING.to_string(),
        }
    );
    let ModelMessage::AssistantToolCalls(calls) = &fitted.messages()[3] else {
        std::panic::resume_unwind(Box::new("batch expected"));
    };
    assert_eq!(calls[0].reasoning_content(), Some(CLEARED_REASONING));
    assert_eq!(
        calls[0].arguments_json().ok(),
        batch[0].arguments_json().ok()
    );
    assert_eq!(calls[1], batch[1]);
    assert_eq!(fitted.messages()[6], request.messages()[6]);
}

#[test]
fn fit_shrinks_older_write_patches_to_their_recorded_outcome() {
    // Arrange
    let patch = format!("@@ -1 +1 @@\n-a\n+{}\n", "b".repeat(3000));
    let mut request = request(Vec::new(), Vec::new(), "edit");
    let outcomes = [
        json!({"bytes_written": 3000, "path": "src/lib.rs", "status": "applied"}).to_string(),
        json!({"path": "src/lib.rs", "status": "rejected"}).to_string(),
        "{}".to_string(),
        json!({"bytes_written": 3000, "path": "src/lib.rs", "status": "applied"}).to_string(),
    ];
    for (index, outcome) in outcomes.iter().enumerate() {
        let call = write_call(&format!("w{index}"), &patch);
        request.record_tool_call(call.clone());
        request.record_tool_result(&call, outcome.clone());
    }

    // Act
    let (fitted, activity) = fit_owned(&mut ContextFit::default(), budget(6000), &request)
        .expect("fits after shrinking patches");

    // Assert
    let fitted = fitted.expect("reduced copy");
    assert_eq!(activity.shrunk_write_calls(), 3);
    let patches: Vec<String> = [1, 3, 5]
        .iter()
        .map(|index| {
            call_of(&fitted.messages()[*index])
                .write_arguments()
                .expect("write arguments")
                .patch()
                .to_string()
        })
        .collect();
    assert!(patches[0].contains("was applied"));
    assert!(patches[1].contains("was rejected"));
    assert!(patches[2].contains("see this call's result"));
    assert_eq!(
        call_of(&fitted.messages()[1])
            .write_arguments()
            .expect("write arguments")
            .path(),
        "src/lib.rs"
    );
    assert_eq!(fitted.messages()[7], request.messages()[7]);
}

#[test]
fn fit_drops_the_oldest_history_turns_and_keeps_doing_so() {
    // Arrange
    let turn = |text: &str| {
        vec![
            ModelMessage::User(text.repeat(3000)),
            ModelMessage::Assistant("{}".to_string()),
        ]
    };
    let request = request(
        vec![ModelMessage::System("system".to_string())],
        vec![turn("a"), turn("b")],
        "now",
    );
    let mut fit = ContextFit::default();

    // Act
    let (first, first_activity) =
        fit_owned(&mut fit, budget(5000), &request).expect("fits after dropping a turn");
    let (second, second_activity) = fit_owned(&mut fit, budget(5000), &request).expect("refits");

    // Assert
    let first = first.expect("reduced copy");
    assert_eq!(first_activity.dropped_history_turns(), 1);
    assert_eq!(
        first.messages(),
        [
            [request.messages()[0].clone()].as_slice(),
            &request.messages()[3..],
        ]
        .concat()
    );
    assert_eq!(second, Some(first));
    assert_eq!(second_activity, first_activity);
}

#[test]
fn fit_caps_each_oversized_latest_result_to_a_quarter_of_the_budget() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "inspect");
    let calls = vec![read_call("large"), read_call("medium")];
    request.record_tool_calls(calls.clone());
    request.record_tool_result(&calls[0], "x".repeat(5000));
    request.record_tool_result(&calls[1], "é".repeat(1500));

    // Act
    let (fitted, activity) = fit_owned(&mut ContextFit::default(), budget(2000), &request)
        .expect("fits after truncation");

    // Assert
    let fitted = fitted.expect("reduced copy");
    assert!(weigh(&fitted) <= target(2000));
    assert_eq!(activity.truncated_results(), 2);
    assert_eq!(activity.elided_results(), 0);
    for message in &fitted.messages()[2..] {
        assert!(ByteEstimator.message_weight(message) <= 500);
    }
    let large = result_json(&fitted.messages()[2]);
    assert!(
        large["context_truncated"]
            .as_str()
            .expect("notice")
            .contains("narrower")
    );
    assert_ne!(large["head"].as_str().expect("head"), "");
    assert!(large["omitted_bytes"].as_u64().expect("omitted") < 5000);
    let medium = result_json(&fitted.messages()[3]);
    let head = medium["head"].as_str().expect("head");
    let tail = medium["tail"].as_str().expect("tail");
    assert!(!head.is_empty() && !tail.is_empty());
    assert_eq!(
        head.len()
            + tail.len()
            + usize::try_from(medium["omitted_bytes"].as_u64().expect("omitted")).expect("usize"),
        3000
    );
    assert_eq!(
        request.messages()[2],
        tool_result("large", "read", &"x".repeat(5000))
    );
}

#[test]
fn fit_fails_typed_when_mandatory_content_cannot_fit() {
    // Arrange
    let request = request(Vec::new(), Vec::new(), &"x".repeat(100));

    // Act
    let rejected = fit_owned(&mut ContextFit::default(), budget(50), &request);

    // Assert
    assert!(matches!(
        rejected,
        Err(TurnError::ContextBudgetExceeded {
            budget: 50,
            required: 100,
        })
    ));
}

#[test]
fn fit_truncates_only_as_many_latest_results_as_needed() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "inspect");
    let calls = vec![read_call("large"), read_call("small")];
    request.record_tool_calls(calls.clone());
    request.record_tool_result(&calls[0], "x".repeat(5000));
    request.record_tool_result(&calls[1], "y".repeat(500));

    // Act
    let (fitted, activity) = fit_owned(&mut ContextFit::default(), budget(3000), &request)
        .expect("fits after truncating one result");

    // Assert
    let fitted = fitted.expect("reduced copy");
    assert_eq!(activity.truncated_results(), 1);
    assert!(
        result_json(&fitted.messages()[2])
            .get("context_truncated")
            .is_some()
    );
    assert_eq!(fitted.messages()[3], request.messages()[3]);
    assert!(weigh(&fitted) <= 3000);
}

fn count_calls(messages: &[ModelMessage]) -> usize {
    messages
        .iter()
        .map(|message| match message {
            ModelMessage::AssistantToolCall(_) => 1,
            ModelMessage::AssistantToolCalls(calls) => calls.len(),
            _ => 0,
        })
        .sum()
}

#[test]
fn fit_drops_the_oldest_rounds_of_a_long_turn_behind_one_note() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "loop");
    let bash = bash_call("b0");
    request.record_tool_call(bash.clone());
    request.record_tool_result(
        &bash,
        json!({"exit_code": 0, "stdout": "y".repeat(500)}).to_string(),
    );
    let plain = bash_call("b1");
    request.record_tool_call(plain.clone());
    request.record_tool_result(&plain, "plain output ".repeat(40));
    for (id, status) in [("w1", "applied"), ("w2", "rejected")] {
        let write = write_call(id, "@@ -1 +1 @@\n-a\n+b\n");
        request.record_tool_call(write.clone());
        request.record_tool_result(
            &write,
            json!({"path": "src/lib.rs", "status": status}).to_string(),
        );
    }
    for index in 0..40 {
        let read = read_call(&format!("r{index}"));
        request.record_tool_call(read.clone());
        request.record_tool_result(&read, "ok".to_string());
    }
    let budget_weight = 1000;
    let mut fit = ContextFit::default();

    // Act
    let (first, first_activity) =
        fit_owned(&mut fit, budget(budget_weight), &request).expect("fits after dropping rounds");
    let first = first.expect("reduced copy");
    record_read_round(&mut request, "next", 2);
    let (second, second_activity) =
        fit_owned(&mut fit, budget(budget_weight), &request).expect("fits with the same prefix");

    // Assert
    assert!(weigh(&first) <= target(budget_weight));
    let ModelMessage::User(note) = &first.messages()[1] else {
        std::panic::resume_unwind(Box::new("dropped-round note expected"));
    };
    assert!(note.contains("earlier tool call(s) of this turn"));
    assert!(note.contains("Writes applied: `src/lib.rs`."));
    assert!(note.contains("Commands run: `cargo test`, `cargo test`."));
    assert_eq!(note.matches("src/lib.rs").count(), 1);
    assert!(matches!(
        first.messages()[2],
        ModelMessage::AssistantToolCall(_)
    ));
    let kept = first.messages().len() - 2;
    assert_eq!(
        first.messages()[2..],
        request.messages()[request.messages().len() - 2 - kept..request.messages().len() - 2]
    );
    let dropped = count_calls(request.messages()) - 1 - count_calls(first.messages());
    assert!(dropped >= 3);
    assert_eq!(first_activity.dropped_tool_calls(), dropped);
    assert!(note.starts_with(&format!("[harness] {dropped} earlier")));
    assert_eq!(first_activity.elided_results(), 0);
    let second = second.expect("second reduced copy");
    assert_eq!(
        second.messages()[..first.messages().len()],
        *first.messages()
    );
    assert_eq!(second_activity, first_activity);
}

#[test]
fn fit_shrinks_the_latest_write_patch_only_as_the_last_resort() {
    // Arrange
    let patch = format!("@@ -1 +1 @@\n-a\n+{}\n", "b".repeat(5000));
    let applied = json!({"path": "src/lib.rs", "status": "applied"}).to_string();
    let mut single = request(Vec::new(), Vec::new(), "edit");
    let write = write_call("w1", &patch);
    single.record_tool_call(write.clone());
    single.record_tool_result(&write, applied.clone());
    let mut batch = request(Vec::new(), Vec::new(), "edit");
    let calls = vec![write_call("w2", &patch), read_call("r1")];
    batch.record_tool_calls(calls.clone());
    batch.record_tool_result(&calls[0], applied);
    batch.record_tool_result(&calls[1], "ok".to_string());
    let mut fit = ContextFit::default();

    // Act
    let (first, first_activity) =
        fit_owned(&mut fit, budget(1000), &single).expect("fits after shrinking the patch");
    let (again, again_activity) =
        fit_owned(&mut fit, budget(1000), &single).expect("refits the same request");
    let (batched, batch_activity) = fit_owned(&mut ContextFit::default(), budget(1000), &batch)
        .expect("fits after shrinking the batched patch");

    // Assert
    let first = first.expect("reduced copy");
    assert!(weigh(&first) <= 1000);
    assert_eq!(first_activity.shrunk_latest_write_calls(), 1);
    assert_eq!(first_activity.truncated_results(), 0);
    let shrunk = call_of(&first.messages()[1])
        .write_arguments()
        .expect("write arguments");
    assert!(shrunk.patch().contains("was applied"));
    assert_eq!(shrunk.path(), "src/lib.rs");
    assert_eq!(first.messages()[2], single.messages()[2]);
    assert_eq!(again, Some(first));
    assert_eq!(again_activity, first_activity);
    let batched = batched.expect("reduced batch copy");
    assert_eq!(batch_activity.shrunk_latest_write_calls(), 1);
    let ModelMessage::AssistantToolCalls(sent) = &batched.messages()[1] else {
        std::panic::resume_unwind(Box::new("batch expected"));
    };
    assert!(
        sent[0]
            .write_arguments()
            .expect("write arguments")
            .patch()
            .contains("was applied")
    );
    assert_eq!(sent[1], calls[1]);
}

#[test]
fn fit_fails_typed_when_the_latest_round_cannot_shrink_enough() {
    // Arrange
    let mut reads = request(Vec::new(), Vec::new(), "x");
    record_read_round(&mut reads, "r1", 5000);
    let mut writes = request(Vec::new(), Vec::new(), "x");
    record_read_round(&mut writes, "r1", 500);
    let write = write_call("w1", &"b".repeat(500));
    writes.record_tool_call(write.clone());
    writes.record_tool_result(&write, "{}".to_string());

    // Act
    let reads_rejected = fit_owned(&mut ContextFit::default(), budget(100), &reads);
    let mut fixed_fit = ContextFit::default();
    let writes_rejected = fixed_fit.fit(&FixedEstimator, budget(20), &writes, None);

    // Assert
    assert!(matches!(
        reads_rejected,
        Err(TurnError::ContextBudgetExceeded { budget: 100, .. })
    ));
    assert!(matches!(
        writes_rejected,
        Err(TurnError::ContextBudgetExceeded { budget: 20, .. })
    ));
}

#[test]
fn fit_reduces_history_or_fails_typed_when_feedback_overflows_a_tool_free_request() {
    // Arrange
    let history = vec![vec![
        ModelMessage::User("a".repeat(3000)),
        ModelMessage::Assistant("{}".to_string()),
    ]];
    let mut with_history = request(Vec::new(), history, "reply");
    with_history.record_feedback("f".repeat(500));
    let mut without_history = request(Vec::new(), Vec::new(), "reply");
    without_history.record_feedback("f".repeat(500));

    // Act
    let (fitted, activity) = fit_owned(&mut ContextFit::default(), budget(1000), &with_history)
        .expect("fits after dropping the history turn");
    let rejected = fit_owned(&mut ContextFit::default(), budget(100), &without_history);

    // Assert
    assert_eq!(activity.dropped_history_turns(), 1);
    assert_eq!(
        fitted.expect("reduced copy").messages(),
        &with_history.messages()[2..]
    );
    assert!(matches!(
        rejected,
        Err(TurnError::ContextBudgetExceeded {
            budget: 100,
            required: 505,
        })
    ));
}

#[test]
fn fit_drops_feedback_that_precedes_a_dropped_round_with_that_round() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "loop");
    request.record_feedback("f".repeat(100));
    for index in 0..4 {
        record_read_round(&mut request, &format!("r{index}"), 300);
    }
    let budget_weight = weigh(&request) - 1;

    // Act
    let (fitted, activity) = fit_owned(&mut ContextFit::default(), budget(budget_weight), &request)
        .expect("fits after dropping rounds");

    // Assert
    let fitted = fitted.expect("reduced copy");
    assert!(weigh(&fitted) <= target(budget_weight));
    let ModelMessage::User(note) = &fitted.messages()[1] else {
        std::panic::resume_unwind(Box::new("dropped-round note expected"));
    };
    assert!(note.starts_with(&format!(
        "[harness] {} earlier tool call(s)",
        activity.dropped_tool_calls()
    )));
    assert!(activity.dropped_tool_calls() >= 1);
    assert_eq!(
        count_calls(fitted.messages()) + activity.dropped_tool_calls(),
        4
    );
    assert!(
        !fitted
            .messages()
            .iter()
            .any(|message| matches!(message, ModelMessage::User(text) if text.starts_with('f')))
    );
}
