use ag_contracts::{OutputEvent, OutputKind, TurnNotice};
use ag_harness::RunEventKind;
use serde_json::{Value, json};

use super::{OutputProjector, turn_notice};

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

/// Returns a text `item_delta` event.
fn text(id: u64, text: &str) -> RunEventKind {
    delta(id, json!({"type": "text", "text": text}))
}

/// Returns a tool-arguments `item_delta` event.
fn arguments(id: u64, arguments: &str) -> RunEventKind {
    delta(
        id,
        json!({"type": "tool_arguments", "arguments": arguments}),
    )
}

/// Returns an `item_completed` event carrying `item`.
fn completed(id: u64, item: Value) -> RunEventKind {
    let mut value = json!({"type": "item_completed", "id": id});
    value["item"] = item;

    event(value)
}

/// Returns a completed message item.
fn message(text: &str) -> Value {
    json!({"type": "message", "text": text})
}

/// Returns a completed tool-call item.
fn tool_call() -> Value {
    json!({
        "type": "tool_call",
        "arguments": "{}",
        "call_id": "call-1",
        "name": "read",
        "output": "",
        "status": "completed",
        "summary": ""
    })
}

/// Returns a `model_request_completed` event.
fn request_completed() -> RunEventKind {
    event(json!({
        "type": "model_request_completed",
        "cost": null,
        "model": null,
        "usage": {
            "input": 1,
            "output": 1,
            "cache_read": 0,
            "cache_write": null,
            "reasoning": 0,
            "unreported_requests": 0
        }
    }))
}

/// Returns the completed final output carrying `answer`.
fn output(answer: &str) -> RunEventKind {
    completed(
        9,
        json!({"type": "output", "output": {"type": "json", "value": {"answer": answer}}}),
    )
}

/// Projects `events` in order and returns every output change.
fn project(
    projector: &mut OutputProjector,
    events: impl IntoIterator<Item = RunEventKind>,
) -> Vec<OutputEvent> {
    events
        .into_iter()
        .flat_map(|event| projector.project(event))
        .collect()
}

#[test]
fn streamed_protocol_response_shows_only_its_decoded_answer() {
    // Arrange
    let mut projector = OutputProjector::default();
    let opened = projector.project(started(0, "message"));

    // Act
    let added = project(
        &mut projector,
        [
            " \n{\"questions\":[{\"text\":\"Which?\"}],\"count\":1,\"note\":\"skip\",\"ans",
            "wer\":\"Line\\none ",
            "\\\"quoted\\\" \\\\ \\/ \\t\\r\\b\\f",
            " \\u00",
            "41 \\uD83D",
            "\\uDE00 \\uDC00 \\uD800\\u0042 \\uZZZZ\"",
            ",\"after\":\"hidden\"}",
        ]
        .map(|fragment| text(0, fragment)),
    );

    // Assert
    assert_eq!(
        opened,
        [OutputEvent::Started {
            id: 0,
            kind: OutputKind::Message,
        }]
    );
    let added = added
        .into_iter()
        .map(|event| match event {
            OutputEvent::Delta { id: 0, text } => text,
            other => format!("<unexpected {other:?}>"),
        })
        .collect::<String>();
    assert_eq!(
        added,
        "Line\none \"quoted\" \\ / \t\r\u{8}\u{c} A \u{1F600} \u{FFFD} \u{FFFD}B \u{FFFD}"
    );
}

#[test]
fn plain_message_streams_verbatim_after_leading_whitespace() {
    // Arrange
    let mut projector = OutputProjector::default();
    projector.project(started(1, "message"));

    // Act
    let added = project(
        &mut projector,
        ["  ", "Reading {the} notes", " now."].map(|fragment| text(1, fragment)),
    );

    // Assert
    assert_eq!(
        added,
        [
            OutputEvent::Delta {
                id: 1,
                text: "Reading {the} notes".to_string(),
            },
            OutputEvent::Delta {
                id: 1,
                text: " now.".to_string(),
            },
        ]
    );
}

#[test]
fn tool_calls_show_while_their_arguments_stream_until_their_request_completes() {
    // Arrange
    let mut projector = OutputProjector::default();
    let streaming = project(
        &mut projector,
        [
            started(2, "reasoning"),
            delta(2, json!({"type": "reasoning", "text": "Think."})),
            started(3, "tool_call"),
            arguments(3, "{\"path\":"),
            arguments(3, "\"notes\"}"),
            text(8, "unknown"),
        ],
    );

    // Act
    let committed = project(&mut projector, [request_completed()]);
    let ran = project(
        &mut projector,
        [
            completed(2, json!({"type": "reasoning", "text": "Thought."})),
            completed(3, tool_call()),
            started(4, "tool_call"),
            completed(4, tool_call()),
        ],
    );

    // Assert
    assert_eq!(
        streaming,
        [
            OutputEvent::Started {
                id: 2,
                kind: OutputKind::Reasoning,
            },
            OutputEvent::Delta {
                id: 2,
                text: "Think.".to_string(),
            },
            OutputEvent::Started {
                id: 3,
                kind: OutputKind::ToolCall,
            },
        ]
    );
    assert_eq!(
        committed,
        [OutputEvent::Completed {
            id: 3,
            text: String::new(),
        }]
    );
    assert_eq!(
        ran,
        [OutputEvent::Completed {
            id: 2,
            text: "Thought.".to_string(),
        }]
    );
}

#[test]
fn final_output_replaces_the_committed_answer_it_streamed_as() {
    // Arrange
    let mut projector = OutputProjector::default();
    project(
        &mut projector,
        [
            started(0, "message"),
            text(0, "{\"answer\":\"Raw"),
            started(9, "output"),
        ],
    );

    // Act
    let answered = project(
        &mut projector,
        [
            request_completed(),
            completed(0, message("{\"answer\":\"Raw answer\"}")),
            output("Done."),
        ],
    );

    // Assert
    assert_eq!(
        answered,
        [
            OutputEvent::Completed {
                id: 0,
                text: "Raw answer".to_string(),
            },
            OutputEvent::Answer {
                replaces: Some(0),
                text: "Done.".to_string(),
            },
        ]
    );
}

#[test]
fn discarded_response_is_dropped_once_the_next_response_streams() {
    // Arrange
    let mut projector = OutputProjector::default();
    project(
        &mut projector,
        [
            started(0, "message"),
            text(0, "{\"answer\":\"Invalid"),
            started(1, "reasoning"),
            delta(1, json!({"type": "reasoning", "text": "Hm."})),
            started(2, "tool_call"),
            arguments(2, "{"),
        ],
    );
    let discarded = project(
        &mut projector,
        [
            completed(1, json!({"type": "reasoning", "text": "Hm."})),
            completed(0, message("{\"answer\":\"Invalid\"}")),
        ],
    );

    // Act
    let retried = project(
        &mut projector,
        [
            started(3, "message"),
            text(3, "{\"answer\":\"Valid"),
            request_completed(),
            completed(3, message("{\"answer\":\"Valid\"}")),
            output("Valid"),
        ],
    );

    // Assert
    assert_eq!(
        discarded,
        [
            OutputEvent::Completed {
                id: 1,
                text: "Hm.".to_string(),
            },
            OutputEvent::Completed {
                id: 0,
                text: "Invalid".to_string(),
            },
        ]
    );
    assert_eq!(
        retried,
        [
            OutputEvent::Started {
                id: 3,
                kind: OutputKind::Message,
            },
            OutputEvent::Discard { ids: vec![0, 1, 2] },
            OutputEvent::Delta {
                id: 3,
                text: "Valid".to_string(),
            },
            OutputEvent::Completed {
                id: 3,
                text: "Valid".to_string(),
            },
            OutputEvent::Answer {
                replaces: Some(3),
                text: "Valid".to_string(),
            },
        ]
    );
}

#[test]
fn unstreamed_final_answer_after_a_tool_round_replaces_nothing() {
    // Arrange
    let mut projector = OutputProjector::default();

    // Act
    let answered = project(
        &mut projector,
        [
            request_completed(),
            started(0, "message"),
            completed(0, message("{\"answer\":\"Progress\"}")),
            started(1, "tool_call"),
            completed(1, tool_call()),
            output("Done."),
        ],
    );

    // Assert
    assert_eq!(
        answered,
        [
            OutputEvent::Started {
                id: 0,
                kind: OutputKind::Message,
            },
            OutputEvent::Completed {
                id: 0,
                text: "Progress".to_string(),
            },
            OutputEvent::Answer {
                replaces: None,
                text: "Done.".to_string(),
            },
        ]
    );
}

#[test]
fn retries_and_run_completion_drop_unfinished_items() {
    // Arrange
    let mut projector = OutputProjector::default();
    let retrying = || {
        event(json!({
            "type": "retrying",
            "after": 1_000,
            "attempt": 1,
            "kind": "overloaded"
        }))
    };
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
    let idle_retry = project(&mut projector, [retrying()]);
    let retried = project(
        &mut projector,
        [started(0, "message"), text(0, "Partial"), retrying()],
    );
    let ended = project(
        &mut projector,
        [
            started(1, "message"),
            text(1, "Again"),
            started(2, "tool_call"),
            arguments(2, "{"),
            finished,
            run_started,
        ],
    );

    // Assert
    assert_eq!(idle_retry, []);
    assert_eq!(retried[2], OutputEvent::Discard { ids: vec![0] });
    assert_eq!(ended[3], OutputEvent::Discard { ids: vec![1, 2] });
    assert_eq!(ended.len(), 4);
}

/// Returns a `compacted` event with `outcome`.
fn compacted(outcome: &Value) -> RunEventKind {
    event(json!({"type": "compacted", "outcome": outcome}))
}

#[test]
fn compactions_that_changed_the_context_are_noted_once() {
    // Arrange
    let events = [
        compacted(&json!({
            "status": "compacted", "covered_through": 2, "model": "qwen/qwen3.8-flash"
        })),
        compacted(&json!({"status": "compacted", "covered_through": 2, "model": "local"})),
        compacted(&json!({"status": "compacted", "covered_through": 2, "model": null})),
        compacted(&json!({
            "status": "failed",
            "kind": "invalid_output",
            "message": "summary rejected",
            "retryable": false
        })),
        compacted(&json!({"status": "nothing_to_cover"})),
        started(0, "message"),
    ];

    // Act
    let notices = events.iter().map(turn_notice).collect::<Vec<_>>();

    // Assert
    assert_eq!(
        notices,
        [
            Some(TurnNotice::ContextCompacted {
                model: Some("qwen3.8-flash".to_string())
            }),
            Some(TurnNotice::ContextCompacted {
                model: Some("local".to_string())
            }),
            Some(TurnNotice::ContextCompacted { model: None }),
            Some(TurnNotice::ContextCompactionFailed {
                reason: "summary rejected".to_string()
            }),
            None,
            None,
        ]
    );
}
