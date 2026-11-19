//! Chat Completions chunk stream accumulation tests.

use serde_json::json;

use crate::chat_completion::ChunkStream;
use crate::http::{SUCCESS_BODY_LIMIT_BYTES, TransportError};
use crate::stream::ResponseDelta;

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

#[test]
fn rejects_invalid_chunks_and_oversized_content() {
    // Arrange
    let half = "x".repeat(SUCCESS_BODY_LIMIT_BYTES / 2 + 1);
    let content = json!({"choices": [{"index": 0, "delta": {"content": half}}]});
    let reasoning = json!({"choices": [{"index": 0, "delta": {"reasoning_content": half}}]});
    let call = json!({"choices": [{"index": 0,
        "delta": {"tool_calls": [{"index": 0, "function": {"arguments": half}}]}}]});
    let mut invalid = ChunkStream::new(false);
    let mut oversized_text = ChunkStream::new(false);
    let mut oversized_call = ChunkStream::new(false);

    // Act
    let invalid = invalid.push(b"data: {not json}\n\n");
    let first = oversized_text.push(format!("data: {content}\n\n").as_bytes());
    let oversized_text = oversized_text.push(format!("data: {reasoning}\n\n").as_bytes());
    oversized_call
        .push(format!("data: {content}\n\n").as_bytes())
        .expect("half the limit should read");
    let oversized_call = oversized_call.push(format!("data: {call}\n\n").as_bytes());

    // Assert
    assert!(matches!(invalid, Err(TransportError::InvalidResponse(_))));
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
