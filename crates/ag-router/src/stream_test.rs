//! Server-sent event reading tests.

use super::SseDecoder;
use crate::http::{SUCCESS_BODY_LIMIT_BYTES, TransportError};

#[test]
fn returns_the_data_of_each_event_split_across_chunks() {
    // Arrange
    let mut events = SseDecoder::default();

    // Act
    let first = events
        .push(b": keep-alive\r\nevent: delta\r\ndata: a\r\n\r\ndata: b\ndata:")
        .expect("the first chunk should read");
    let second = events
        .push(b"c\n\n\ndata: tail")
        .expect("the second chunk should read");
    let last = events.finish().expect("the trailing event should read");

    // Assert
    assert_eq!(first, ["a"]);
    assert_eq!(second, ["b\nc"]);
    assert_eq!(last.as_deref(), Some("tail"));
}

#[test]
fn finishes_without_a_pending_event() {
    // Arrange
    let mut events = SseDecoder::default();
    events.push(b"data: a\n\n").expect("the event should read");

    // Act
    let last = events.finish().expect("an empty tail should read");

    // Assert
    assert_eq!(last, None);
}

#[test]
fn bounds_one_line_and_one_event() {
    // Arrange
    let line = vec![b'x'; SUCCESS_BODY_LIMIT_BYTES + 1];
    let event = format!("data: {}\n", "x".repeat(SUCCESS_BODY_LIMIT_BYTES + 1));

    // Act
    let line = SseDecoder::default().push(&line);
    let event = SseDecoder::default().push(event.as_bytes());

    // Assert
    assert!(matches!(line, Err(TransportError::ResponseBodyTooLarge)));
    assert!(matches!(event, Err(TransportError::ResponseBodyTooLarge)));
}
