use serde_json::json;

use super::{EventStream, endpoint, replayed_reasoning};
use crate::http::{SUCCESS_BODY_LIMIT_BYTES, TransportError};

#[test]
fn reads_events_split_across_chunks_and_lines() {
    // Arrange
    let mut events = EventStream::default();
    let item =
        json!({"type": "response.output_item.done", "item": {"type": "reasoning", "id": "rs_1"}});
    let stream = format!(
        ": keep-alive\r\nevent: response.output_item.done\r\ndata: {item}\r\n\r\ndata: \
         {{\"type\":\"response.completed\",\r\ndata: \
         \"response\":{{\"status\":\"completed\"}}}}\r\n\r\n"
    );
    let (head, tail) = stream.split_at(stream.len() / 2);

    // Act
    let partial = events
        .push(head.as_bytes())
        .expect("first half should read");
    let response = events
        .push(tail.as_bytes())
        .expect("second half should read")
        .expect("the terminal event should end the stream");

    // Assert
    assert!(partial.is_none());
    assert_eq!(response.status.as_deref(), Some("completed"));
    assert_eq!(
        response.output,
        Some(vec![json!({"type": "reasoning", "id": "rs_1"})])
    );
}

#[test]
fn finishes_with_an_unterminated_last_event() {
    // Arrange
    let mut events = EventStream::default();
    events
        .push(b"data: [DONE]\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}")
        .expect("events should read");

    // Act
    let response = events
        .finish()
        .expect("the buffered terminal event should end the stream");

    // Assert
    assert_eq!(response.status.as_deref(), Some("completed"));
    assert_eq!(response.output, None);
}

#[test]
fn finishes_with_a_terminal_event_missing_its_blank_line() {
    // Arrange
    let mut events = EventStream::default();
    events
        .push(b"data: {\"type\":\"response.failed\"}\n")
        .expect("events should read");

    // Act
    let response = events
        .finish()
        .expect("the pending event should be dispatched");

    // Assert
    assert_eq!(response.status, None);
}

#[test]
fn reports_streams_that_close_early_or_carry_invalid_events() {
    // Arrange
    let mut invalid = EventStream::default();
    let closed = EventStream::default();

    // Act
    let invalid = invalid.push(b"data: {not json}\n\n");
    let closed = closed.finish();

    // Assert
    assert!(matches!(invalid, Err(TransportError::InvalidResponse(_))));
    assert!(matches!(closed, Err(TransportError::Transport(_))));
}

#[test]
fn bounds_one_event_and_one_line() {
    // Arrange
    let oversized = vec![b'x'; SUCCESS_BODY_LIMIT_BYTES + 1];
    let mut data = format!("data: {}\n", "x".repeat(SUCCESS_BODY_LIMIT_BYTES)).into_bytes();
    data.extend_from_slice(b"data: x\n");

    // Act
    let line = EventStream::default().push(&oversized);
    let event = EventStream::default().push(&data);

    // Assert
    assert!(matches!(line, Err(TransportError::ResponseBodyTooLarge)));
    assert!(matches!(event, Err(TransportError::ResponseBodyTooLarge)));
}

#[test]
fn bounds_the_completed_items_kept_across_events() {
    // Arrange
    let mut events = EventStream::default();
    let text = "x".repeat(SUCCESS_BODY_LIMIT_BYTES / 3);
    let item = json!({
        "type": "response.output_item.done",
        "item": {"type": "message", "content": [{"type": "output_text", "text": text}]}
    });
    let event = format!("data: {item}\n\n");

    // Act
    let kept = (0..3)
        .map(|_| events.push(event.as_bytes()))
        .collect::<Vec<_>>();

    // Assert
    assert!(matches!(kept[0], Ok(None)));
    assert!(matches!(kept[1], Ok(None)));
    assert!(matches!(kept[2], Err(TransportError::ResponseBodyTooLarge)));
}

#[test]
fn replays_only_nonempty_arrays_of_reasoning_items() {
    // Arrange
    let reasoning = json!([{"type": "reasoning", "id": "rs_1", "encrypted_content": "gAAAA"}]);
    let mixed = json!([{"type": "reasoning"}, {"type": "message"}]);

    // Act / Assert
    assert_eq!(replayed_reasoning(Some(&reasoning.to_string())).len(), 1);
    assert!(replayed_reasoning(Some("[]")).is_empty());
    assert!(replayed_reasoning(Some(&mixed.to_string())).is_empty());
    assert!(replayed_reasoning(Some("plain reasoning text")).is_empty());
    assert!(replayed_reasoning(None).is_empty());
}

#[test]
fn appends_the_responses_path_to_the_base_url() {
    // Arrange / Act / Assert
    assert_eq!(
        endpoint("https://api.openai.com/v1/"),
        "https://api.openai.com/v1/responses"
    );
}
