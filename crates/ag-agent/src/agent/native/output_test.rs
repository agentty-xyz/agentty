use ag_contracts::{OutputEvent, OutputKind};
use ag_harness::RunEventKind;
use serde_json::{Value, json};

use super::OutputProjector;

/// Decodes one run event from its wire form.
fn event(value: Value) -> RunEventKind {
    serde_json::from_value(value).expect("run event")
}

/// Returns an `item_started` event.
fn started(id: u64, item_kind: &str) -> RunEventKind {
    event(json!({"type": "item_started", "id": id, "item_kind": item_kind}))
}

/// Returns an `item_delta` event carrying `delta`.
fn delta(id: u64, delta: Value) -> RunEventKind {
    let mut value = json!({"type": "item_delta", "id": id});
    value["delta"] = delta;

    event(value)
}

/// Returns an `item_completed` event carrying `item`.
fn completed(id: u64, item: Value) -> RunEventKind {
    let mut value = json!({"type": "item_completed", "id": id});
    value["item"] = item;

    event(value)
}

/// Streams `fragments` as text deltas of message `id` and returns the
/// display text each one added.
fn stream_text(projector: &mut OutputProjector, id: u64, fragments: &[&str]) -> Vec<String> {
    fragments
        .iter()
        .filter_map(|fragment| {
            match projector.project(delta(id, json!({"type": "text", "text": fragment}))) {
                Some(OutputEvent::Delta { text, .. }) => Some(text),
                _ => None,
            }
        })
        .collect()
}

#[test]
fn streamed_protocol_response_shows_only_its_decoded_answer() {
    // Arrange
    let mut projector = OutputProjector::default();
    let opened = projector.project(started(0, "message"));

    // Act
    let added = stream_text(
        &mut projector,
        0,
        &[
            " \n{\"questions\":[{\"text\":\"Which?\"}],\"note\":\"skip\",\"ans",
            "wer\":\"Line\\none ",
            "\\\"quoted\\\" \\\\ \\/ \\t\\r\\b\\f",
            " \\u00",
            "41 \\uD83D",
            "\\uDE00 \\uDC00 \\uD800\\u0042 \\uZZZZ\"",
            ",\"after\":\"hidden\"}",
        ],
    );

    // Assert
    assert_eq!(
        opened,
        Some(OutputEvent::Started {
            id: 0,
            kind: OutputKind::Message,
        })
    );
    assert_eq!(
        added.concat(),
        "Line\none \"quoted\" \\ / \t\r\u{8}\u{c} A \u{1F600} \u{FFFD} \u{FFFD}B \u{FFFD}"
    );
}

#[test]
fn plain_message_streams_verbatim_after_leading_whitespace() {
    // Arrange
    let mut projector = OutputProjector::default();
    projector.project(started(1, "message"));

    // Act
    let added = stream_text(&mut projector, 1, &["  ", "Reading {the} notes", " now."]);

    // Assert
    assert_eq!(added, ["Reading {the} notes", " now."]);
}

#[test]
fn reasoning_streams_its_fragments_while_tool_arguments_stay_unshown() {
    // Arrange
    let mut projector = OutputProjector::default();

    // Act
    let reasoning = projector.project(started(2, "reasoning"));
    let thought = projector.project(delta(2, json!({"type": "reasoning", "text": "Think."})));
    let tool = projector.project(started(3, "tool_call"));
    let arguments = projector.project(delta(
        3,
        json!({"type": "tool_arguments", "arguments": "{\"path\":"}),
    ));
    let unknown = projector.project(delta(9, json!({"type": "text", "text": "lost"})));

    // Assert
    assert_eq!(
        reasoning,
        Some(OutputEvent::Started {
            id: 2,
            kind: OutputKind::Reasoning,
        })
    );
    assert_eq!(
        thought,
        Some(OutputEvent::Delta {
            id: 2,
            text: "Think.".to_string(),
        })
    );
    assert_eq!(
        tool,
        Some(OutputEvent::Started {
            id: 3,
            kind: OutputKind::ToolCall,
        })
    );
    assert_eq!(arguments, None);
    assert_eq!(unknown, None);
}

#[test]
fn completed_items_replace_fragments_and_the_output_becomes_the_answer() {
    // Arrange
    let mut projector = OutputProjector::default();
    projector.project(started(0, "message"));
    let output_started = projector.project(started(4, "output"));

    // Act
    let message = projector.project(completed(
        0,
        json!({"type": "message", "text": "{\"answer\":\"Raw answer\"}"}),
    ));
    let reasoning = projector.project(completed(
        1,
        json!({"type": "reasoning", "text": "Thought."}),
    ));
    let tool = projector.project(completed(
        2,
        json!({
            "type": "tool_call",
            "arguments": "{}",
            "call_id": "call-1",
            "name": "read",
            "output": "",
            "status": "completed",
            "summary": ""
        }),
    ));
    let answer = projector.project(completed(
        4,
        json!({"type": "output", "output": {"type": "json", "value": {"answer": "Done."}}}),
    ));

    // Assert
    assert_eq!(output_started, None);
    assert_eq!(
        message,
        Some(OutputEvent::Completed {
            id: 0,
            text: "Raw answer".to_string(),
        })
    );
    assert_eq!(
        reasoning,
        Some(OutputEvent::Completed {
            id: 1,
            text: "Thought.".to_string(),
        })
    );
    assert_eq!(
        tool,
        Some(OutputEvent::Completed {
            id: 2,
            text: String::new(),
        })
    );
    assert_eq!(answer, Some(OutputEvent::Answer("Done.".to_string())));
}

#[test]
fn retries_and_run_completion_discard_open_items() {
    // Arrange
    let mut projector = OutputProjector::default();
    let retrying = event(json!({
        "type": "retrying",
        "after": 1_000,
        "attempt": 1,
        "kind": "overloaded"
    }));
    let finished = event(json!({
        "type": "completed",
        "cost": null,
        "error": null,
        "usage": {
            "input": 0,
            "output": 0,
            "cache_read": 0,
            "cache_write": null,
            "reasoning": 0,
            "unreported_requests": 0
        }
    }));
    let run_started = event(json!({
        "type": "started",
        "model": null,
        "session": "session",
        "turn": 0
    }));

    // Act
    let retried = projector.project(retrying);
    let completed = projector.project(finished);
    let ignored = projector.project(run_started);

    // Assert
    assert_eq!(retried, Some(OutputEvent::DiscardOpen));
    assert_eq!(completed, Some(OutputEvent::DiscardOpen));
    assert_eq!(ignored, None);
}
