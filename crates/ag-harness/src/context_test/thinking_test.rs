//! Preserved-thinking regression tests: however deeply a request is reduced
//! to fit its budget, the signed thinking of the assistant turn its tool
//! loop continues, the latest round's call message, is sent byte for byte,
//! as Anthropic Messages models require of replayed thinking.

use serde_json::json;

use super::support::{ByteEstimator, budget, request, sent};
use crate::context::{ContextFit, FitStep};
use crate::lifecycle::ContextSummaryTrigger;
use crate::model::{ModelMessage, ModelRequest};
use crate::tool::ToolCall;

/// Returns the Anthropic reasoning record of a round whose call `id`
/// followed signed thinking of `bytes` length.
fn signed_thinking(id: &str, bytes: usize) -> String {
    json!([
        {"type": "thinking", "thinking": "t".repeat(bytes), "signature": format!("sig-{id}")},
        {"type": "tool_use", "id": id}
    ])
    .to_string()
}

/// Records a round of one call to `name` with `arguments` after signed
/// thinking, answered by a result of `result_bytes`.
fn record_thinking_round(
    request: &mut ModelRequest,
    id: &str,
    name: &str,
    arguments: &serde_json::Value,
    result_bytes: usize,
) {
    let call = ToolCall::from_json(
        id.to_string(),
        name,
        &arguments.to_string(),
        Some(signed_thinking(id, 1500)),
    )
    .expect("thinking call");
    request.record_tool_call(call.clone());
    request.record_tool_result(&call, "x".repeat(result_bytes));
}

/// Returns the reasoning of the request's last tool-call message, the
/// round its tool loop continues.
fn latest_reasoning(request: &ModelRequest) -> Option<&str> {
    request
        .messages()
        .iter()
        .rev()
        .find_map(|message| match message {
            ModelMessage::AssistantToolCall(call) => Some(call.reasoning_content()),
            ModelMessage::AssistantToolCalls(calls) => Some(calls[0].reasoning_content()),
            _ => None,
        })
        .flatten()
}

#[test]
fn every_reduction_sends_the_latest_rounds_signed_thinking_unchanged() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "inspect");
    let mut fit = ContextFit::default();
    let mut cleared_reasoning = 0;
    let mut dropped_tool_calls = 0;
    let mut elided_results = 0;
    let mut shrunk_latest_writes = 0;

    // Act / Assert
    for index in 0..9 {
        let id = format!("r{index}");
        if index < 8 {
            let path = json!({"action": "file", "path": format!("{id}.txt")});
            record_thinking_round(&mut request, &id, "read", &path, 2000);
        } else {
            let patch = format!("@@ -1 +1 @@\n-a\n+{}\n", "b".repeat(7000));
            let write = json!({"patch": patch, "path": "src/lib.rs"});
            record_thinking_round(&mut request, &id, "write", &write, 60);
        }
        let (fitted, activity) = fit
            .fit(&ByteEstimator, budget(9000), &request, None)
            .map(sent)
            .expect("the request fits after reductions");
        cleared_reasoning = cleared_reasoning.max(activity.cleared_reasoning());
        dropped_tool_calls = dropped_tool_calls.max(activity.dropped_tool_calls());
        elided_results = elided_results.max(activity.elided_results());
        shrunk_latest_writes += activity.shrunk_latest_write_calls();
        let sent = fitted.unwrap_or_else(|| request.clone());
        assert_eq!(
            latest_reasoning(&sent),
            Some(signed_thinking(&id, 1500).as_str()),
            "round {index}"
        );
    }
    // Results were elided, reasoning cleared, and rounds dropped before the
    // latest write's patch was shrunk as the last resort.
    assert!(elided_results > 0);
    assert!(cleared_reasoning > 0);
    assert!(dropped_tool_calls > 0);
    assert_eq!(shrunk_latest_writes, 1);
}

#[test]
fn an_overflow_summary_keeps_the_latest_rounds_signed_thinking() {
    // Arrange
    let mut request = request(Vec::new(), Vec::new(), "inspect");
    for index in 0..4 {
        let id = format!("r{index}");
        let path = json!({"action": "file", "path": format!("{id}.txt")});
        record_thinking_round(&mut request, &id, "read", &path, 500);
    }
    let overflow = Some(ContextSummaryTrigger::ContextOverflow);
    let mut fit = ContextFit::default();
    let job = match fit.fit(&ByteEstimator, budget(100_000), &request, overflow) {
        Ok(FitStep::Summarize(job)) => *job,
        other => {
            std::panic::resume_unwind(Box::new(format!("summary step expected, got {other:?}")))
        }
    };
    let summary = json!({"context": "read r0 to r2", "decisions": [], "state": "reading"});

    // Act
    fit.install_summary(&ByteEstimator, &request, &job, summary);
    let (fitted, activity) = fit
        .fit(&ByteEstimator, budget(100_000), &request, overflow)
        .map(sent)
        .expect("the request fits after its summary");

    // Assert
    assert!(activity.summarized_tool_calls() > 0);
    let fitted = fitted.expect("summarized copy");
    assert_eq!(
        latest_reasoning(&fitted),
        Some(signed_thinking("r3", 1500).as_str())
    );
}
