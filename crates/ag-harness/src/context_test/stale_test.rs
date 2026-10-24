use serde_json::{Value, json};

use super::support::{
    ByteEstimator, FixedEstimator, bash_call, budget, read_call_of, record_read_round, request,
    result_json, sent, tool_result, write_call,
};
use crate::context::{
    CallRecords, ContextFit, FitMessage, KEEP_RECENT_RESULTS, ReadSpan, Supersession,
    SupersessionCause, deferred_read_result, elided_result, find_superseded, read_shows_path,
    repeated_read_result, rereads,
};
use crate::model::{ModelMessage, ModelRequest};
use crate::tool::{ReadArguments, ToolCall};
use crate::turn::ContextActivity;

fn weigh(request: &ModelRequest) -> u64 {
    request
        .messages()
        .iter()
        .map(|message| u64::try_from(message.retained_bytes()).expect("bytes fit u64"))
        .sum()
}

fn fit_owned(
    fit: &mut ContextFit,
    budget_weight: u64,
    request: &ModelRequest,
) -> (ModelRequest, ContextActivity) {
    let (fitted, activity) = fit
        .fit(&ByteEstimator, budget(budget_weight), request, None)
        .map(sent)
        .expect("request fits");

    (fitted.expect("reduced copy"), activity)
}

fn arguments(arguments: &Value) -> ReadArguments {
    serde_json::from_value(arguments.clone()).expect("read arguments")
}

fn read_of(id: &str, arguments: &Value) -> ToolCall {
    ToolCall::from_json(id.to_string(), "read", &arguments.to_string(), None).expect("read call")
}

fn span(start_line: u64, end_line: Option<u64>) -> ReadSpan {
    ReadSpan {
        end_line,
        start_line,
    }
}

/// Returns a successful `file` read result for `path` over the given lines.
fn file_result(path: &str, start_line: u64, end_line: u64) -> String {
    json!({
        "content": "line\n".repeat(usize::try_from(end_line - start_line + 1).expect("lines")),
        "end_line": end_line,
        "next_offset": null,
        "path": path,
        "start_line": start_line,
        "truncated": false,
    })
    .to_string()
}

fn record(request: &mut ModelRequest, call: &ToolCall, result: String) {
    request.record_tool_call(call.clone());
    request.record_tool_result(call, result);
}

fn cached(messages: &[ModelMessage]) -> Vec<FitMessage> {
    messages
        .iter()
        .map(|message| FitMessage {
            changes: 0,
            reduced: None,
            repeats: None,
            span: ReadSpan::parse(message),
            weight: 0,
        })
        .collect()
}

/// Returns the identifier and cause of each superseded result, by index.
fn superseded(messages: &[ModelMessage]) -> Vec<(usize, String, SupersessionCause)> {
    find_superseded(messages, &cached(messages), 0..messages.len())
        .into_iter()
        .map(|(index, supersession)| (index, supersession.by.to_string(), supersession.cause))
        .collect()
}

/// Builds a request whose stale `b.txt` result is not its oldest result:
/// rounds `a.txt`, `b.txt` (large), `b.txt` again, then the latest `c.txt`.
fn request_with_a_stale_middle_result() -> ModelRequest {
    let mut request = request(Vec::new(), Vec::new(), "inspect");
    record_read_round(&mut request, "a", 500);
    record(&mut request, &read_call_of("b1", "b.txt"), "x".repeat(3000));
    record(&mut request, &read_call_of("b2", "b.txt"), "y".repeat(500));
    record_read_round(&mut request, "c", 500);

    request
}

#[test]
fn fit_elides_superseded_results_before_older_ones() {
    // Arrange
    let request = request_with_a_stale_middle_result();
    let budget_weight = weigh(&request) - 1;

    // Act
    let (fitted, activity) = fit_owned(&mut ContextFit::default(), budget_weight, &request);

    // Assert
    assert_eq!(fitted.messages()[2], request.messages()[2]);
    let notice = result_json(&fitted.messages()[4]);
    assert_eq!(notice["superseded_by"], json!("b2"));
    assert_eq!(notice["read"], json!("file b.txt"));
    assert_eq!(notice["original_bytes"], json!(3000));
    assert!(
        notice["context_cleared"]
            .as_str()
            .expect("notice text")
            .contains("later read of the same target")
    );
    assert_eq!(fitted.messages()[5..], request.messages()[5..]);
    assert_eq!(activity.superseded_results(), 1);
    assert_eq!(activity.elided_results(), 1);
}

#[test]
fn superseded_results_stay_elided_and_new_staleness_waits_for_the_next_fit() {
    // Arrange
    let mut request = request_with_a_stale_middle_result();
    let budget_weight = weigh(&request) - 1;
    let mut fit = ContextFit::default();
    let (first, _) = fit_owned(&mut fit, budget_weight, &request);

    // Act
    // Re-reading `a.txt` makes its result stale, but this request still
    // fits, so the shared prefix is kept.
    record(&mut request, &read_call_of("a2", "a.txt"), "z".repeat(10));
    let (second, second_activity) = fit_owned(&mut fit, budget_weight, &request);
    for index in 0..3 {
        record_read_round(&mut request, &format!("d{index}"), 1100);
    }
    let (third, third_activity) = fit_owned(&mut fit, budget_weight, &request);

    // Assert
    assert_eq!(
        second.messages()[..first.messages().len()],
        *first.messages()
    );
    assert_eq!(second_activity.superseded_results(), 1);
    assert_eq!(third.messages()[..2], first.messages()[..2]);
    assert_eq!(third.messages()[3..5], first.messages()[3..5]);
    let notice = result_json(&third.messages()[2]);
    assert_eq!(notice["superseded_by"], json!("a2"));
    assert_eq!(third_activity.superseded_results(), 2);
    assert_eq!(third.messages().last(), request.messages().last());
}

#[test]
fn the_latest_round_is_never_elided_for_staleness() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "inspect");
    record_read_round(&mut request, "a", 100);
    let calls = vec![read_call_of("x1", "x.txt"), read_call_of("x2", "x.txt")];
    request.record_tool_calls(calls.clone());
    for call in &calls {
        request.record_tool_result(call, "x".repeat(1500));
    }
    let budget_weight = weigh(&request) - 1;

    // Act
    let (fitted, activity) = fit_owned(&mut ContextFit::default(), budget_weight, &request);

    // Assert
    assert_eq!(activity.superseded_results(), 0);
    for message in &fitted.messages()[4..] {
        assert!(result_json(message).get("superseded_by").is_none());
    }
}

#[test]
fn superseded_results_that_cannot_shrink_are_skipped() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "inspect");
    record(&mut request, &read_call_of("s1", "s.txt"), "ok".to_string());
    record(&mut request, &read_call_of("s2", "s.txt"), "ok".to_string());
    record_read_round(&mut request, "big", 1000);
    record_read_round(&mut request, "latest", 10);
    let budget_weight = weigh(&request) - 1;

    // Act
    let (fitted, activity) = fit_owned(&mut ContextFit::default(), budget_weight, &request);

    // Assert
    assert_eq!(fitted.messages()[2], request.messages()[2]);
    assert_eq!(activity.superseded_results(), 0);
}

#[test]
fn superseded_results_that_weigh_no_less_reduced_are_kept() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "inspect");
    for id in ["s1", "s2", "s3"] {
        record(&mut request, &read_call_of(id, "s.txt"), "x".repeat(2000));
    }
    let mut fit = ContextFit::default();

    // Act
    let (fitted, activity) = fit
        .fit(&FixedEstimator, budget(30), &request, None)
        .map(sent)
        .expect("fits after dropping rounds");

    // Assert
    assert!(fitted.is_some());
    assert_eq!(activity.superseded_results(), 0);
    assert!(activity.dropped_tool_calls() > 0);
}

#[test]
fn dropping_a_round_releases_its_superseded_result() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "loop");
    for index in 0..12 {
        record(
            &mut request,
            &read_call_of(&format!("r{index}"), "same.txt"),
            "x".repeat(400),
        );
    }
    record_read_round(&mut request, "latest", 10);

    // Act
    let (fitted, activity) = fit_owned(&mut ContextFit::default(), 3000, &request);

    // Assert
    assert!(activity.dropped_tool_calls() > 0);
    let notices = fitted
        .messages()
        .iter()
        .filter(|message| {
            matches!(
                message,
                ModelMessage::ToolResult { content, .. } if content.contains("superseded_by")
            )
        })
        .count();
    assert!(notices > 0);
    assert_eq!(activity.superseded_results(), notices);
    assert!(activity.elided_results() >= notices);
}

#[test]
fn rereads_requires_the_same_target_and_a_covering_result() {
    // Arrange
    let file = arguments(&json!({"path": "src/lib.rs"}));
    let other_file = arguments(&json!({"path": "src/main.rs"}));
    let show = arguments(&json!({"action": "show", "path": "src/lib.rs", "side": "head"}));
    let show_base = arguments(&json!({"action": "show", "path": "src/lib.rs", "side": "base"}));
    let search = |limit: Option<u64>| {
        arguments(&json!({"action": "search", "query": "fn fit", "path": "src", "limit": limit}))
    };
    let other_query = arguments(&json!({"action": "search", "query": "fn other", "path": "src"}));
    let list = arguments(&json!({"action": "list", "path": "src"}));
    let other_list = arguments(&json!({"action": "list", "path": "docs"}));
    let cases = [
        (
            &file,
            Some(span(10, Some(20))),
            &file,
            Some(span(1, Some(50))),
            true,
        ),
        (
            &file,
            Some(span(10, Some(20))),
            &file,
            Some(span(15, Some(50))),
            false,
        ),
        (
            &file,
            Some(span(10, Some(20))),
            &file,
            Some(span(1, Some(15))),
            false,
        ),
        (
            &file,
            Some(span(10, Some(20))),
            &file,
            Some(span(1, None)),
            false,
        ),
        (&file, None, &file, None, true),
        (&file, Some(span(10, Some(20))), &file, None, false),
        (&file, Some(span(5, None)), &file, Some(span(1, None)), true),
        (&file, None, &other_file, None, false),
        (
            &show,
            Some(span(1, Some(5))),
            &show,
            Some(span(1, Some(5))),
            true,
        ),
        (&show, None, &show_base, None, false),
        (&search(None), None, &search(None), None, true),
        (&search(Some(10)), None, &search(Some(20)), None, true),
        (&search(Some(20)), None, &search(Some(10)), None, false),
        (&search(None), None, &search(Some(10)), None, false),
        (&search(None), None, &other_query, None, false),
        (&list, None, &list, None, true),
        (&list, None, &other_list, None, false),
    ];

    // Act
    let results: Vec<bool> = cases
        .iter()
        .map(|(earlier, earlier_span, later, later_span, _)| {
            rereads(earlier, *earlier_span, later, *later_span)
        })
        .collect();

    // Assert
    let expected: Vec<bool> = cases.iter().map(|case| case.4).collect();
    assert_eq!(results, expected);
}

#[test]
fn writes_supersede_file_reads_and_diffs_that_show_their_path() {
    // Arrange
    let cases = [
        (json!({"path": "src/lib.rs"}), true),
        (json!({"path": "src/main.rs"}), false),
        (json!({"action": "diff"}), true),
        (json!({"action": "diff", "path": "src"}), true),
        (json!({"action": "diff", "path": "src/lib.rs"}), true),
        (json!({"action": "diff", "path": "sr"}), false),
        (json!({"action": "list", "path": "src"}), false),
        (
            json!({"action": "search", "query": "fit", "path": "src"}),
            false,
        ),
        (
            json!({"action": "show", "path": "src/lib.rs", "side": "head"}),
            false,
        ),
    ];

    // Act
    let results: Vec<bool> = cases
        .iter()
        .map(|(read, _)| read_shows_path(&arguments(read), "src/lib.rs"))
        .collect();

    // Assert
    let expected: Vec<bool> = cases.iter().map(|case| case.1).collect();
    assert_eq!(results, expected);
}

#[test]
fn later_rereads_writes_and_reruns_supersede_history_and_current_results() {
    // Arrange
    let history_read = read_call_of("r1", "src/lib.rs");
    let history = vec![
        ModelMessage::User("earlier".to_string()),
        // A result without a preceding call pairs with nothing.
        tool_result("orphan", "read", "{}"),
        ModelMessage::AssistantToolCall(history_read),
        tool_result("r1", "read", &file_result("src/lib.rs", 1, 10)),
        ModelMessage::Assistant("{}".to_string()),
    ];
    let mut request = request(Vec::new(), vec![history], "continue");
    let bash = bash_call("b1");
    record(&mut request, &bash, json!({"exit_code": 0}).to_string());
    // The provider reuses the history call's identifier in this turn.
    let reread = read_call_of("r1", "src/lib.rs");
    record(&mut request, &reread, file_result("src/lib.rs", 1, 20));
    let docs = read_call_of("r3", "docs.md");
    let batch = vec![
        write_call("w1", "@@ -1 +1 @@\n-a\n+b\n"),
        docs,
        ToolCall::from_json(
            "w2".to_string(),
            "write",
            &json!({"patch": "@@ -1 +1 @@\n-a\n+b\n", "path": "docs.md"}).to_string(),
            None,
        )
        .expect("write call"),
    ];
    request.record_tool_calls(batch.clone());
    request.record_tool_result(
        &batch[0],
        json!({"path": "src/lib.rs", "status": "applied"}).to_string(),
    );
    request.record_tool_result(&batch[1], file_result("docs.md", 1, 3));
    request.record_tool_result(
        &batch[2],
        json!({"path": "docs.md", "status": "rejected"}).to_string(),
    );
    record(
        &mut request,
        &bash_call("b2"),
        json!({"exit_code": 1}).to_string(),
    );
    record(
        &mut request,
        &read_call_of("r5", "docs.md"),
        repeated_read_result("r3"),
    );
    // A search its round had no room for supersedes nothing either.
    let search = json!({"action": "search", "query": "fn fit"});
    record(
        &mut request,
        &read_of("s1", &search),
        json!({"action": "search", "result": ["src/lib.rs:1:fn fit"], "truncated": false})
            .to_string(),
    );
    record(
        &mut request,
        &read_of("s2", &search),
        deferred_read_result(&arguments(&search)),
    );
    // Nor is one superseded when the model fetches it later: only the run
    // search before it is.
    record(
        &mut request,
        &read_of("s3", &search),
        json!({"action": "search", "result": ["src/lib.rs:1:fn fit"], "truncated": false})
            .to_string(),
    );
    let notes = json!({"path": "notes.md"});
    record(
        &mut request,
        &read_of("n1", &notes),
        deferred_read_result(&arguments(&notes)),
    );
    record(
        &mut request,
        &read_of("n2", &notes),
        file_result("notes.md", 1, 4),
    );

    // Act
    let superseded = superseded(request.messages());

    // Assert
    assert_eq!(
        superseded,
        [
            (3, "r1".to_string(), SupersessionCause::Reread),
            (7, "b2".to_string(), SupersessionCause::Rerun),
            (9, "w1".to_string(), SupersessionCause::Write),
            (19, "s3".to_string(), SupersessionCause::Reread),
        ]
    );
}

#[test]
fn superseded_notices_name_the_cause_and_keep_command_outcomes() {
    // Arrange
    let result = tool_result(
        "b1",
        "bash",
        &json!({"exit_code": 2, "stdout": "y".repeat(2000), "termination": "completed"})
            .to_string(),
    );
    let read = tool_result("r1", "read", &"x".repeat(2000));
    let rerun = Supersession {
        by: "b2",
        cause: SupersessionCause::Rerun,
    };
    let written = Supersession {
        by: "w1",
        cause: SupersessionCause::Write,
    };
    let records = CallRecords::default();

    // Act
    let rerun_notice = elided_result(&result, &records, Some(&rerun)).expect("notice");
    let write_notice = elided_result(&read, &records, Some(&written)).expect("notice");
    let note = elided_result(
        &tool_result("r2", "read", &repeated_read_result("r1")),
        &records,
        Some(&written),
    );

    // Assert
    let rerun_notice = result_json(&rerun_notice);
    assert_eq!(rerun_notice["superseded_by"], json!("b2"));
    assert_eq!(rerun_notice["exit_code"], json!(2));
    assert_eq!(rerun_notice["termination"], json!("completed"));
    assert!(
        rerun_notice["context_cleared"]
            .as_str()
            .expect("notice text")
            .contains("do NOT repeat")
    );
    let write_notice = result_json(&write_notice);
    assert_eq!(write_notice["superseded_by"], json!("w1"));
    assert!(
        write_notice["context_cleared"]
            .as_str()
            .expect("notice text")
            .contains("changed the file")
    );
    assert_eq!(note, None);
}

#[test]
fn repeated_read_results_point_to_the_earlier_call() {
    // Arrange
    let id = "call \"7\"";

    // Act
    let result = repeated_read_result(id);

    // Assert
    let parsed: Value = serde_json::from_str(&result).expect("JSON result");
    assert!(result.starts_with(r#"{"unchanged":true,"#));
    assert_eq!(parsed["same_as_call"], json!(id));
    assert!(
        parsed["note"]
            .as_str()
            .expect("note")
            .contains("nothing was written or run since")
    );
}

#[test]
fn a_repeat_result_is_cleared_with_the_result_it_points_to() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "inspect");
    record(&mut request, &read_call_of("a1", "a.txt"), "x".repeat(3000));
    record(
        &mut request,
        &read_call_of("a2", "a.txt"),
        repeated_read_result("a1"),
    );
    for index in 0..KEEP_RECENT_RESULTS {
        record_read_round(&mut request, &format!("n{index}"), 10);
    }
    let budget_weight = weigh(&request) - 1;
    let mut fit = ContextFit::default();

    // Act
    let (fitted, activity) = fit_owned(&mut fit, budget_weight, &request);

    // Assert
    assert_eq!(
        result_json(&fitted.messages()[2])["read"],
        json!("file a.txt")
    );
    let repeat = result_json(&fitted.messages()[4]);
    assert!(
        repeat["context_cleared"]
            .as_str()
            .expect("notice text")
            .contains("repeated an earlier read whose result was since cleared")
    );
    assert_eq!(repeat["read"], json!("file a.txt"));
    assert_eq!(activity.elided_results(), 2);
    assert!(!fit.sends_whole(4));
}

#[test]
fn a_cut_repeat_result_is_cleared_with_the_result_it_points_to() {
    // Arrange
    // A long call identifier makes the repeat note heavy enough to cut.
    let first = format!("call_{}", "1".repeat(80));
    let mut request = request(Vec::new(), Vec::new(), "inspect");
    record(
        &mut request,
        &read_call_of(&first, "a.txt"),
        "x".repeat(3000),
    );
    record(
        &mut request,
        &read_call_of("a2", "a.txt"),
        repeated_read_result(&first),
    );
    let mut fit = ContextFit::default();

    // Act
    // The repeat is the new latest round, so it is cut to a quarter of the
    // budget before the result it points to is elided.
    let (fitted, activity) = fit_owned(&mut fit, 1000, &request);

    // Assert
    let repeat = result_json(&fitted.messages()[4]);
    assert!(
        repeat["context_cleared"]
            .as_str()
            .expect("notice text")
            .contains("repeated an earlier read")
    );
    assert_eq!(activity.elided_results(), 2);
    assert_eq!(activity.truncated_results(), 0);
}

#[test]
fn a_repeat_of_a_superseded_result_points_to_the_newer_read() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "inspect");
    record(&mut request, &read_call_of("a1", "a.txt"), "x".repeat(3000));
    record(
        &mut request,
        &read_call_of("a2", "a.txt"),
        repeated_read_result("a1"),
    );
    record(&mut request, &read_call_of("a3", "a.txt"), "y".repeat(500));
    for index in 0..KEEP_RECENT_RESULTS {
        record_read_round(&mut request, &format!("n{index}"), 10);
    }
    let budget_weight = weigh(&request) - 1;

    // Act
    let (fitted, activity) = fit_owned(&mut ContextFit::default(), budget_weight, &request);

    // Assert
    assert_eq!(activity.superseded_results(), 1);
    let repeat = result_json(&fitted.messages()[4]);
    assert_eq!(repeat["superseded_by"], json!("a3"));
    assert!(
        repeat["context_cleared"]
            .as_str()
            .expect("notice text")
            .contains("Use that newer result")
    );
    assert_eq!(fitted.messages()[6], request.messages()[6]);
}

#[test]
fn deferred_read_results_repeat_the_exact_call() {
    // Arrange
    let arguments = arguments(&json!({"path": "src/lib.rs", "offset": 40, "limit": 20}));

    // Act
    let result = deferred_read_result(&arguments);

    // Assert
    let parsed: Value = serde_json::from_str(&result).expect("JSON result");
    assert!(result.starts_with(r#"{"deferred":"#));
    assert_eq!(
        parsed["call"],
        json!({"limit": 20, "offset": 40, "path": "src/lib.rs"})
    );
    assert!(
        parsed["deferred"]
            .as_str()
            .expect("note")
            .contains("Call read with these arguments in a later response")
    );
}

#[test]
fn sends_whole_reports_results_the_last_request_did_not_reduce() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "inspect");
    record_read_round(&mut request, "large", 5000);
    let mut fit = ContextFit::default();
    let _ = fit.fit(&ByteEstimator, budget(4000), &request, None);
    record_read_round(&mut request, "next", 10);
    let mut within = ContextFit::default();
    let _ = within.fit(&ByteEstimator, budget(100_000), &request, None);

    // Act
    let capped = fit.sends_whole(2);
    let appended = fit.sends_whole(4);
    let unreduced = within.sends_whole(2);

    // Assert
    assert!(!capped);
    assert!(appended);
    assert!(unreduced);
}

#[test]
fn sends_whole_is_false_for_cut_latest_and_dropped_results() {
    // Arrange
    let mut latest = request(Vec::new(), Vec::new(), "inspect");
    let calls = vec![
        read_call_of("large", "l.txt"),
        read_call_of("small", "s.txt"),
    ];
    latest.record_tool_calls(calls.clone());
    latest.record_tool_result(&calls[0], "x".repeat(5000));
    latest.record_tool_result(&calls[1], "y".repeat(500));
    let mut cut = ContextFit::default();
    let _ = cut.fit(&ByteEstimator, budget(3000), &latest, None);
    let mut rounds = request(Vec::new(), Vec::new(), "loop");
    for index in 0..40 {
        record(
            &mut rounds,
            &read_of(
                &format!("r{index}"),
                &json!({"path": format!("{index}.txt")}),
            ),
            "ok".to_string(),
        );
    }
    let mut dropped = ContextFit::default();
    let (_, activity) = dropped
        .fit(&ByteEstimator, budget(1000), &rounds, None)
        .map(sent)
        .expect("fits after dropping rounds");

    // Act
    let cut_large = cut.sends_whole(2);
    let first_round = dropped.sends_whole(2);
    let latest_round = dropped.sends_whole(rounds.messages().len() - 1);

    // Assert
    assert!(!cut_large);
    assert!(activity.dropped_tool_calls() > 0);
    assert!(!first_round);
    assert!(latest_round);
}

#[test]
fn superseded_elision_stops_once_the_request_reaches_the_target() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "inspect");
    record(&mut request, &read_call_of("a1", "a.txt"), "x".repeat(3000));
    record(&mut request, &read_call_of("b1", "b.txt"), "y".repeat(300));
    record(&mut request, &read_call_of("a2", "a.txt"), "z".repeat(10));
    record(&mut request, &read_call_of("b2", "b.txt"), "w".repeat(10));
    record_read_round(&mut request, "latest", 10);
    let budget_weight = weigh(&request) - 1;

    // Act
    let (fitted, activity) = fit_owned(&mut ContextFit::default(), budget_weight, &request);

    // Assert
    assert_eq!(
        result_json(&fitted.messages()[2])["superseded_by"],
        json!("a2")
    );
    assert_eq!(fitted.messages()[4], request.messages()[4]);
    assert_eq!(activity.superseded_results(), 1);
}
