//! Chat Completions chunk stream accumulation tests.

use serde_json::json;

use crate::chat_completion::ChunkStream;
use crate::http::{SUCCESS_BODY_LIMIT_BYTES, TransportError};
use crate::stream::{EventReader as _, ResponseDelta};

#[test]
fn reads_a_trailing_chunk_and_keeps_only_the_first_choice() {
    // Arrange
    let mut chunks = ChunkStream::new(false);
    let other = json!({"choices": [{"index": 1, "delta": {"content": "other"}}]});
    let bare = json!({"choices": [{"index": 0, "finish_reason": null}]});
    let call = json!({"choices": [{"index": 0, "delta": {"content": "a",
        "tool_calls": [{"index": 4, "function": {"arguments": "{}"}}]}}]});
    let finished = json!({"choices": [{"index": 0, "finish_reason": "tool_calls"}]});
    let stream = format!("data: {other}\n\ndata: {bare}\n\ndata: {call}\n\ndata: {finished}");

    // Act
    let done = chunks.push(stream.as_bytes()).expect("chunks should read");
    chunks.close().expect("the trailing chunk should read");
    let completion = chunks.finish().expect("the trailing chunk should finish");

    // Assert
    assert!(!done);
    assert_eq!(completion.content.as_deref(), Some("a"));
    assert_eq!(completion.metadata.finish_reason(), "tool_calls");
    assert_eq!(completion.tool_calls.len(), 1);
    assert_eq!(completion.tool_calls[0].kind, "function");
    assert_eq!(completion.tool_calls[0].id, "");
    assert_eq!(
        completion.tool_calls[0].function,
        json!({"arguments": "{}"})
    );
}

#[test]
fn keeps_deltas_only_when_observed() {
    // Arrange
    let chunk = b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"a\"}}]}\n\n";
    let mut observed = ChunkStream::new(true);
    let mut unobserved = ChunkStream::new(false);

    // Act
    observed.push(chunk).expect("the chunk should read");
    unobserved.push(chunk).expect("the chunk should read");

    // Assert
    assert_eq!(observed.deltas, [ResponseDelta::Text("a".to_string())]);
    assert_eq!(unobserved.deltas, []);
}

/// Returns a chunk of the first choice carrying `calls` fragments.
fn calls(calls: &[serde_json::Value]) -> String {
    let chunk = json!({"choices": [{"index": 0, "delta": {"tool_calls": calls}}]});

    format!("data: {chunk}\n\n")
}

#[test]
fn accumulates_interleaved_calls_in_the_order_they_start() {
    // Arrange
    let mut chunks = ChunkStream::new(true);
    let start = |index: u64, id: &str| json!({"index": index, "id": id, "type": "function", "function": {"name": "read"}});
    let fragment =
        |index: u64, arguments: &str| json!({"index": index, "function": {"arguments": arguments}});
    let finished = json!({"choices": [{"index": 0, "finish_reason": "tool_calls"}]});

    // Act
    for chunk in [
        calls(&[start(1, "call_b"), start(0, "call_a")]),
        calls(&[fragment(1, "{\"b\":"), fragment(0, "{\"a\":")]),
        calls(&[fragment(1, "2}"), fragment(0, "1}")]),
        format!("data: {finished}\n\n"),
    ] {
        chunks
            .push(chunk.as_bytes())
            .expect("the chunk should read");
    }
    let deltas = std::mem::take(&mut chunks.deltas);
    let completion = chunks.finish().expect("the choice finished");

    // Assert
    let ids: Vec<_> = completion
        .tool_calls
        .iter()
        .map(|call| call.id.as_str())
        .collect();
    assert_eq!(ids, ["call_b", "call_a"]);
    assert_eq!(
        completion.tool_calls[0].function,
        json!({"arguments": "{\"b\":2}", "name": "read"})
    );
    let arguments = |index: usize, arguments: &str| ResponseDelta::ToolArguments {
        arguments: arguments.to_string(),
        index,
    };
    assert_eq!(
        deltas,
        [
            arguments(0, "{\"b\":"),
            arguments(1, "{\"a\":"),
            arguments(0, "2}"),
            arguments(1, "1}"),
        ]
    );
}

#[test]
fn starts_a_new_call_for_another_id_at_a_used_index() {
    // Arrange
    let mut chunks = ChunkStream::new(false);
    let call = |id: &str, arguments: &str| {
        json!({"index": 0, "id": id, "type": "function",
               "function": {"name": "read", "arguments": arguments}})
    };
    let finished = json!({"choices": [{"index": 0, "finish_reason": "tool_calls"}]});

    // Act
    chunks
        .push(calls(&[call("call_a", "{\"a\":1}")]).as_bytes())
        .expect("the first call should read");
    chunks
        .push(calls(&[call("call_a", "")]).as_bytes())
        .expect("a repeated id should read");
    chunks
        .push(calls(&[call("call_b", "{\"b\":2}")]).as_bytes())
        .expect("the second call should read");
    chunks
        .push(format!("data: {finished}\n\n").as_bytes())
        .expect("the finish should read");
    let completion = chunks.finish().expect("the choice finished");

    // Assert
    let calls: Vec<_> = completion
        .tool_calls
        .iter()
        .map(|call| (call.id.as_str(), call.function["arguments"].clone()))
        .collect();
    assert_eq!(
        calls,
        [
            ("call_a", json!("{\"a\":1}")),
            ("call_b", json!("{\"b\":2}"))
        ]
    );
}

#[test]
fn stops_reading_at_done() {
    // Arrange
    let mut chunks = ChunkStream::new(false);

    // Act
    let done = chunks.push(b"data: [DONE]\n\ndata: {not json}\n\n");

    // Assert
    assert!(matches!(done, Ok(true)));
}

#[test]
fn closes_a_stream_that_ends_between_events_without_done() {
    // Arrange
    let mut chunks = ChunkStream::new(false);
    let finished = json!({"choices": [{"index": 0, "delta": {"content": "a"},
        "finish_reason": "stop"}]});
    chunks
        .push(format!("data: {finished}\n\n").as_bytes())
        .expect("the chunk should read");

    // Act
    chunks.close().expect("nothing is left to read");
    let completion = chunks.finish().expect("the choice finished");

    // Assert
    assert_eq!(completion.content.as_deref(), Some("a"));
}

#[test]
fn rejects_invalid_chunks() {
    // Arrange
    let mut chunks = ChunkStream::new(false);

    // Act
    let invalid = chunks.push(b"data: {not json}\n\n");

    // Assert
    assert!(matches!(invalid, Err(TransportError::InvalidResponse(_))));
}

#[test]
fn bounds_the_accumulated_content() {
    // Arrange
    let half = "x".repeat(SUCCESS_BODY_LIMIT_BYTES / 2 + 1);
    let content = json!({"choices": [{"index": 0, "delta": {"content": half}}]});
    let reasoning = json!({"choices": [{"index": 0, "delta": {"reasoning_content": half}}]});
    let call = json!({"choices": [{"index": 0,
        "delta": {"tool_calls": [{"index": 0, "function": {"arguments": half}}]}}]});
    let mut oversized_text = ChunkStream::new(false);
    let mut oversized_call = ChunkStream::new(false);

    // Act
    let first = oversized_text.push(format!("data: {content}\n\n").as_bytes());
    let oversized_text = oversized_text.push(format!("data: {reasoning}\n\n").as_bytes());
    oversized_call
        .push(format!("data: {content}\n\n").as_bytes())
        .expect("half the limit should read");
    let oversized_call = oversized_call.push(format!("data: {call}\n\n").as_bytes());

    // Assert
    assert!(matches!(first, Ok(false)));
    assert!(matches!(
        oversized_text,
        Err(TransportError::ResponseBodyTooLarge)
    ));
    assert!(matches!(
        oversized_call,
        Err(TransportError::ResponseBodyTooLarge)
    ));
}
