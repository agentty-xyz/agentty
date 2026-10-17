use std::num::NonZeroU64;

use serde_json::{Value, json};

use crate::context::{ContextBudget, ContextEstimator, FitStep};
use crate::model::{ModelMessage, ModelRequest};
use crate::schema_contract::OutputSchema;
use crate::tool::{ToolCall, ToolDefinition};
use crate::turn::ContextActivity;

/// Flat weights for admission arithmetic that ignores content size.
pub(super) struct FixedEstimator;

impl ContextEstimator for FixedEstimator {
    fn message_weight(&self, _message: &ModelMessage) -> u64 {
        7
    }

    fn tool_definition_weight(&self, _tool: &ToolDefinition) -> u64 {
        2
    }
}

/// One weight unit per retained byte, so reductions are exact byte counts.
pub(super) struct ByteEstimator;

impl ContextEstimator for ByteEstimator {
    fn message_weight(&self, message: &ModelMessage) -> u64 {
        u64::try_from(message.retained_bytes()).expect("message bytes fit u64")
    }

    fn tool_definition_weight(&self, _tool: &ToolDefinition) -> u64 {
        0
    }
}

pub(super) fn budget(max_request_weight: u64) -> ContextBudget {
    ContextBudget::new(NonZeroU64::new(max_request_weight).expect("nonzero budget"))
}

pub(super) fn read_call(id: &str) -> ToolCall {
    reasoned_read_call(id, None)
}

pub(super) fn reasoned_read_call(id: &str, reasoning_content: Option<&str>) -> ToolCall {
    ToolCall::from_json(
        id.to_string(),
        "read",
        r#"{"action":"file","path":"name.txt"}"#,
        reasoning_content.map(str::to_string),
    )
    .expect("read call")
}

pub(super) fn write_call(id: &str, patch: &str) -> ToolCall {
    ToolCall::from_json(
        id.to_string(),
        "write",
        &json!({"patch": patch, "path": "src/lib.rs"}).to_string(),
        None,
    )
    .expect("write call")
}

pub(super) fn bash_call(id: &str) -> ToolCall {
    bash_command_call(id, "cargo test")
}

pub(super) fn bash_command_call(id: &str, command: &str) -> ToolCall {
    ToolCall::from_json(
        id.to_string(),
        "bash",
        &json!({"command": command}).to_string(),
        Some("run the suite".to_string()),
    )
    .expect("bash call")
}

pub(super) fn result(id: &str) -> ModelMessage {
    tool_result(id, "read", "content")
}

pub(super) fn tool_result(id: &str, name: &str, content: &str) -> ModelMessage {
    ModelMessage::ToolResult {
        call_id: id.to_string(),
        content: content.to_string(),
        name: name.to_string(),
    }
}

/// Returns the JSON content of a tool result message.
pub(super) fn result_json(message: &ModelMessage) -> Value {
    let ModelMessage::ToolResult { content, .. } = message else {
        std::panic::resume_unwind(Box::new(format!("tool result expected, got {message:?}")));
    };

    serde_json::from_str(content).expect("tool result content is JSON")
}

/// Builds a request whose current turn starts with `input` after `leading`
/// messages and history turns.
pub(super) fn request(
    leading: Vec<ModelMessage>,
    history: Vec<Vec<ModelMessage>>,
    input: &str,
) -> ModelRequest {
    let mut messages = leading;
    let mut history_turn_starts = Vec::new();
    for turn in history {
        history_turn_starts.push(messages.len());
        messages.extend(turn);
    }
    let schema = OutputSchema::new(json!({"type": "object"})).expect("schema");

    ModelRequest::with_history(messages, input, schema)
        .with_history_turn_starts(history_turn_starts)
}

/// Returns a `read` call for the file `path`.
pub(super) fn read_call_of(id: &str, path: &str) -> ToolCall {
    ToolCall::from_json(
        id.to_string(),
        "read",
        &json!({"action": "file", "path": path}).to_string(),
        None,
    )
    .expect("read call")
}

/// Records one read round of the file `<id>.txt` whose result carries
/// `bytes` of content.
pub(super) fn record_read_round(request: &mut ModelRequest, id: &str, bytes: usize) {
    let call = read_call_of(id, &format!("{id}.txt"));
    request.record_tool_call(call.clone());
    request.record_tool_result(&call, "x".repeat(bytes));
}

/// Returns an owned copy of the request a fit step sends, with its
/// reductions; a summary step fails the test.
pub(super) fn sent(step: FitStep<'_>) -> (Option<ModelRequest>, ContextActivity) {
    match step {
        FitStep::Send(fitted, activity) => (fitted.cloned(), activity),
        FitStep::Summarize(job) => {
            std::panic::resume_unwind(Box::new(format!("send step expected, got {job:?}")))
        }
    }
}
