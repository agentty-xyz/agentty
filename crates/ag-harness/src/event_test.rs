use std::error::Error;
use std::time::{Duration, SystemTime};

use serde_json::json;

use super::{
    EventSink, Item, ItemId, ItemKind, RunEvent, RunEventKind, RunFailure, RunSummary,
    ToolCallItem, ToolCallStatus,
};
use crate::catalog::ModelId;
use crate::error::ErrorKind;
use crate::lifecycle::ToolUnavailableReason;
use crate::run::{RunOutput, Usage};
use crate::tool::{ReadAction, Tool, ToolCall};
use crate::turn::{ContextActivity, ToolActivity};

/// Committed JSON schema of [`RunEvent`]; regenerate it from the failure
/// message of [`run_event_schema_matches_the_committed_snapshot`].
const RUN_EVENT_SCHEMA: &str = include_str!("../schema/run_event.json");

#[tokio::test]
async fn sink_numbers_events_and_items_and_stops_after_completion() {
    // Arrange
    let (sink, mut stream) = EventSink::channel();

    // Act
    let first = sink.start_item(ItemKind::Message).await;
    let second = sink.start_item(ItemKind::Reasoning).await;
    sink.complete(RunSummary {
        error: None,
        usage: Usage::default(),
    })
    .await;
    sink.emit(RunEventKind::ModelRequestCompleted {
        usage: Usage::default(),
    })
    .await;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    // Assert
    assert_eq!([first.get(), second.get()], [0, 1]);
    let sequence: Vec<u64> = events.iter().map(|event| event.seq).collect();
    assert_eq!(sequence, [0, 1, 2]);
    assert!(events.iter().all(|event| event.run == events[0].run));
    assert_eq!(events[0].run.as_str().len(), 36, "a hyphenated UUID");
    assert!(matches!(events[2].kind, RunEventKind::Completed(_)));
}

#[tokio::test]
async fn sink_discards_events_after_the_stream_is_dropped() {
    // Arrange
    let (sink, stream) = EventSink::channel();
    drop(stream);

    // Act
    for _ in 0..200 {
        sink.emit(RunEventKind::ModelRequestCompleted {
            usage: Usage::default(),
        })
        .await;
    }

    // Assert
    assert_eq!(sink.start_item(ItemKind::ToolCall).await.get(), 0);
}

#[test]
fn tool_activity_maps_to_how_the_call_was_answered() {
    // Arrange
    let duration = Duration::from_millis(1);
    let cases = [
        (ToolActivity::Bash { duration }, ToolCallStatus::Completed),
        (
            ToolActivity::ReadRejected {
                duration,
                path: "../x".into(),
            },
            ToolCallStatus::Rejected,
        ),
        (
            ToolActivity::ReadRepeated {
                action: ReadAction::File,
                duration,
                summary: "a.txt".into(),
            },
            ToolCallStatus::Skipped,
        ),
        (
            ToolActivity::Unavailable {
                duration,
                reason: ToolUnavailableReason::Denied,
                tool: Tool::Bash.name().into(),
            },
            ToolCallStatus::Unavailable,
        ),
    ];

    // Act
    let statuses: Vec<ToolCallStatus> = cases
        .iter()
        .map(|(activity, _)| ToolCallStatus::from(activity))
        .collect();

    // Assert
    let expected: Vec<ToolCallStatus> = cases.iter().map(|(_, status)| *status).collect();
    assert_eq!(statuses, expected);
}

#[test]
fn run_event_round_trips_through_json() -> Result<(), Box<dyn Error>> {
    // Arrange
    let call = ToolCall::from_json("call-1".into(), "read", r#"{"path":"a.txt"}"#, None)?;
    let activity = ToolActivity::Read {
        duration: Duration::from_millis(3),
        end_line: Some(1),
        path: "a.txt".into(),
        start_line: 1,
        truncated: false,
    };
    let (sink, _stream) = EventSink::channel();
    let kinds = vec![
        RunEventKind::Started {
            model: Some(ModelId::new("openai/gpt")),
            session: "s".into(),
            turn: 2,
        },
        RunEventKind::ItemStarted {
            id: ItemId(0),
            item_kind: ItemKind::ToolCall,
        },
        RunEventKind::ItemCompleted {
            id: ItemId(0),
            item: Item::ToolCall(ToolCallItem::new(&call, "1: hi", &activity)),
        },
        RunEventKind::ItemCompleted {
            id: ItemId(1),
            item: Item::Message {
                content: RunOutput::Json(json!({"name": "Ada"})),
            },
        },
        RunEventKind::ItemCompleted {
            id: ItemId(2),
            item: Item::Reasoning {
                text: "think".into(),
            },
        },
        RunEventKind::ContextReduced(ContextActivity::default()),
        RunEventKind::Completed(RunSummary {
            error: Some(RunFailure {
                kind: ErrorKind::Cancelled,
                message: "turn was cancelled".into(),
                retryable: false,
            }),
            usage: Usage {
                cache_write: Some(1),
                ..Usage::default()
            },
        }),
    ];
    let events: Vec<RunEvent> = kinds
        .into_iter()
        .zip(0_u64..)
        .map(|(kind, seq)| RunEvent {
            at: SystemTime::UNIX_EPOCH,
            kind,
            run: sink.run.clone(),
            seq,
        })
        .collect();

    // Act
    let encoded = serde_json::to_value(&events)?;
    let decoded: Vec<RunEvent> = serde_json::from_value(encoded.clone())?;

    // Assert
    assert_eq!(decoded, events);
    assert_eq!(encoded[0]["kind"]["type"], "started");
    assert_eq!(encoded[2]["kind"]["item"]["type"], "tool_call");
    assert_eq!(encoded[2]["kind"]["item"]["status"], "completed");
    assert_eq!(
        encoded[2]["kind"]["item"]["arguments"],
        r#"{"path":"a.txt"}"#
    );
    assert_eq!(encoded[3]["kind"]["item"]["content"]["type"], "json");
    assert_eq!(encoded[6]["kind"]["error"]["kind"], "cancelled");

    Ok(())
}

#[test]
fn run_event_schema_matches_the_committed_snapshot() -> Result<(), serde_json::Error> {
    // Arrange
    let schema = schemars::schema_for!(RunEvent);

    // Act
    let generated = format!("{}\n", serde_json::to_string_pretty(&schema)?);

    // Assert
    assert!(
        generated == RUN_EVENT_SCHEMA,
        "crates/ag-harness/schema/run_event.json is stale; replace it with:\n{generated}"
    );

    Ok(())
}
