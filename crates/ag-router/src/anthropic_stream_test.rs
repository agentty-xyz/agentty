//! Messages event stream accumulation and cache breakpoint tests; the
//! conformance suite compares whole streamed responses with unstreamed ones.

use serde_json::{Value, json};

use super::{MessagePayload, MessageStream, mark_cache_breakpoints};
use crate::http::{SUCCESS_BODY_LIMIT_BYTES, TransportError};
use crate::stream::{EventReader as _, ResponseDelta};

/// Returns the server-sent event carrying `event`.
fn event(event: &Value) -> String {
    format!("data: {event}\n\n")
}

/// Returns a `content_block_start` event for `block` at `index`.
fn start(index: u64, block: &Value) -> String {
    event(&json!({"type": "content_block_start", "index": index, "content_block": block}))
}

/// Returns a `content_block_delta` event of `delta` for the block at
/// `index`.
fn delta(index: u64, delta: &Value) -> String {
    event(&json!({"type": "content_block_delta", "index": index, "delta": delta}))
}

/// Returns the `thinking_delta` event of `thinking` at `index`.
fn thought(index: u64, thinking: &str) -> String {
    delta(
        index,
        &json!({"type": "thinking_delta", "thinking": thinking}),
    )
}

/// Reads `events` into an observed stream that reads forced-tool output.
fn read(events: &[String]) -> Result<MessageStream, TransportError> {
    let mut stream = MessageStream::new(true, true);
    stream.push(events.concat().as_bytes())?;

    Ok(stream)
}

#[test]
fn separates_thinking_blocks_and_skips_empty_and_unknown_content() {
    // Arrange
    let thinking = json!({"type": "thinking", "thinking": "", "signature": ""});
    let events = [
        event(&json!({"type": "ping"})),
        event(&json!({"type": "novel_event"})),
        start(0, &thinking),
        thought(0, "First."),
        thought(0, " Done."),
        start(1, &thinking),
        thought(1, ""),
        start(2, &thinking),
        thought(2, "Second."),
        delta(2, &json!({"type": "citations_delta"})),
        delta(7, &json!({"type": "text_delta", "text": "lost"})),
        start(
            3,
            &json!({"type": "server_tool_use", "id": "srv_1", "name": "web_search", "input": {}}),
        ),
        delta(
            3,
            &json!({"type": "input_json_delta", "partial_json": "{}"}),
        ),
    ];

    // Act
    let stream = read(&events).expect("the events should read");

    // Assert
    let reasoning = |text: &str| ResponseDelta::Reasoning(text.to_string());
    assert_eq!(
        stream.deltas,
        [
            reasoning("First."),
            reasoning(" Done."),
            reasoning("\n\n"),
            reasoning("Second."),
        ]
    );
    assert!(!stream.stopped);
}

#[test]
fn reads_the_answer_tool_input_as_text_and_numbers_only_calls() {
    // Arrange
    let call =
        |id: &str, name: &str| json!({"type": "tool_use", "id": id, "name": name, "input": {}});
    let input = |json: &str| json!({"type": "input_json_delta", "partial_json": json});
    let events = [
        start(0, &call("toolu_answer", "structured_output")),
        delta(0, &input("{\"name\":\"Ada\"}")),
        start(1, &call("toolu_read", "read")),
        delta(1, &input("{}")),
        event(&json!({"type": "message_stop"})),
    ];

    // Act
    let stream = read(&events).expect("the events should read");
    let message = stream.finish().expect("the message stopped");

    // Assert
    let content = message.content.expect("content");
    assert_eq!(content[0]["input"], json!({"name": "Ada"}));
    assert_eq!(content[1]["input"], json!({}));
}

#[test]
fn keeps_deltas_only_when_observed() {
    // Arrange
    let text = start(0, &json!({"type": "text", "text": ""}))
        + &delta(0, &json!({"type": "text_delta", "text": "a"}));
    let mut unobserved = MessageStream::new(false, false);

    // Act
    unobserved
        .push(text.as_bytes())
        .expect("the events should read");

    // Assert
    assert_eq!(unobserved.deltas, []);
}

#[test]
fn reads_a_trailing_stop_and_fails_a_stream_closed_before_it() {
    // Arrange
    let stop = json!({"type": "message_stop"});
    let mut stopped = MessageStream::new(false, false);
    let mut open = MessageStream::new(false, false);
    stopped
        .push(format!("data: {stop}").as_bytes())
        .expect("the partial event should read");
    open.push(b"data: {\"type\":\"ping\"}\n\n")
        .expect("the ping should read");

    // Act
    stopped.close().expect("the trailing event should read");
    open.close().expect("nothing is left to read");
    let message = stopped.finish();
    let closed = open.finish();

    // Assert
    assert_eq!(
        message.ok().and_then(|message| message.content),
        Some(Vec::new())
    );
    assert!(matches!(closed, Err(TransportError::Transport(_))));
}

#[test]
fn fails_invalid_events_and_call_input() {
    // Arrange
    let call = json!({"type": "tool_use", "id": "toolu_1", "name": "read", "input": {}});
    let truncated = [
        start(0, &call),
        delta(
            0,
            &json!({"type": "input_json_delta", "partial_json": "{\"pa"}),
        ),
        event(&json!({"type": "message_stop"})),
    ];

    // Act
    let invalid_event = MessageStream::new(false, false).push(b"data: {\"type\":\n\n");
    let invalid_input = read(&truncated)
        .expect("the events should read")
        .finish()
        .err();

    // Assert
    assert!(matches!(
        invalid_event,
        Err(TransportError::InvalidResponse(_))
    ));
    assert!(matches!(
        invalid_input,
        Some(TransportError::InvalidResponse(_))
    ));
}

#[test]
fn bounds_the_accumulated_content() {
    // Arrange
    // Each stream starts with the limit already reached, so one more byte
    // of content exceeds it without building megabytes of text.
    let text = json!({"type": "text", "text": ""});
    let mut deltas = MessageStream::new(false, false);
    deltas
        .push(start(0, &text).as_bytes())
        .expect("the start should read");
    deltas.content_bytes = SUCCESS_BODY_LIMIT_BYTES;
    let mut started = MessageStream::new(false, false);
    started.content_bytes = SUCCESS_BODY_LIMIT_BYTES;

    // Act
    let delta_error = deltas.push(delta(0, &json!({"type": "text_delta", "text": "x"})).as_bytes());
    let start_error =
        started.push(start(0, &json!({"type": "redacted_thinking", "data": "x"})).as_bytes());

    // Assert
    assert!(matches!(
        delta_error,
        Err(TransportError::ResponseBodyTooLarge)
    ));
    assert!(matches!(
        start_error,
        Err(TransportError::ResponseBodyTooLarge)
    ));
}

#[test]
fn reads_events_without_their_optional_fields() {
    // Arrange
    let events = [
        event(&json!({"type": "message_start"})),
        event(&json!({"type": "content_block_start", "index": 0})),
        event(&json!({"type": "content_block_delta", "index": 0})),
        event(&json!({"type": "message_delta"})),
        event(&json!({"type": "message_stop"})),
    ];

    // Act
    let message = read(&events)
        .expect("the events should read")
        .finish()
        .expect("the message stopped");

    // Assert
    assert_eq!(message.content, Some(vec![Value::Null]));
    assert_eq!(message.stop_reason, None);
    assert!(message.usage.is_some());
}

#[test]
fn later_usage_counts_replace_earlier_ones() {
    // Arrange
    let events = [
        event(&json!({"type": "message_start", "message": {"usage": {
            "input_tokens": 10, "cache_read_input_tokens": 4, "output_tokens": 1
        }}})),
        event(
            &json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"},
            "usage": {"input_tokens": 12, "cache_creation_input_tokens": 3, "output_tokens": 9}}),
        ),
        event(&json!({"type": "message_stop"})),
    ];

    // Act
    let message = read(&events)
        .expect("the events should read")
        .finish()
        .expect("the message stopped");

    // Assert
    let usage = message.usage.expect("usage");
    assert_eq!(usage.input, Some(12));
    assert_eq!(usage.cache_read, Some(4));
    assert_eq!(usage.cache_creation, Some(3));
    assert_eq!(usage.output, Some(9));
    assert_eq!(message.stop_reason.as_deref(), Some("end_turn"));
}

#[test]
fn an_error_event_without_a_type_names_no_status() {
    // Arrange
    let mut stream = MessageStream::new(false, false);

    // Act
    let error = stream.push(event(&json!({"type": "error"})).as_bytes());

    // Assert
    assert!(matches!(
        error,
        Err(TransportError::StreamFailed {
            code: None,
            message: None,
            status: None,
        })
    ));
}

#[test]
fn marks_the_last_message_block_before_trailing_thinking() {
    // Arrange
    let thinking = json!({"type": "thinking", "thinking": "", "signature": "sig"});
    let mut messages = vec![MessagePayload {
        content: vec![json!({"type": "text", "text": "Answer."}), thinking.clone()],
        role: "assistant",
    }];

    // Act
    mark_cache_breakpoints(&mut [], &mut [], &mut messages);

    // Assert
    assert_eq!(
        messages[0].content,
        [
            json!({"type": "text", "text": "Answer.", "cache_control": {"type": "ephemeral"}}),
            thinking,
        ]
    );
}
