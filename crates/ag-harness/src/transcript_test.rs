use std::time::{Duration, UNIX_EPOCH};

use serde_json::json;

use super::{
    StoredTurn, TranscriptInput, TranscriptItem, TranscriptToolCall, TurnRange, TurnRecord,
    TurnStatus,
};
use crate::event::TurnIndex;
use crate::input::{ImageContent, ImageMediaType, InputBlock, TurnInput};
use crate::model::ModelMessage;
use crate::run::{RunOutput, Usage};
use crate::tool::ToolCall;
use crate::turn::{TurnOutcome, TurnReport};

/// Committed JSON schema of [`TurnRecord`]; regenerate it from the failure
/// message of [`turn_record_schema_matches_the_committed_snapshot`].
const TURN_RECORD_SCHEMA: &str = include_str!("../schema/turn_record.json");

/// Returns a settled turn at position 4 holding `messages`, recorded with
/// `options`.
fn stored(messages: Vec<ModelMessage>, options: Option<&str>) -> StoredTurn {
    StoredTurn {
        error_type: None,
        finished_at: UNIX_EPOCH + Duration::from_secs(9),
        messages,
        options: options.map(str::to_string),
        outcome: None,
        position: 4,
        started_at: UNIX_EPOCH + Duration::from_secs(8),
        status: TurnStatus::Completed,
    }
}

/// Returns a `read` call `id`, with `text` as its assistant text.
fn read_call(id: &str, reasoning: Option<&str>, text: Option<&str>) -> ToolCall {
    ToolCall::from_json(
        id.to_string(),
        "read",
        r#"{"path":"a.txt"}"#,
        reasoning.map(str::to_string),
    )
    .and_then(|call| call.with_assistant_text(text.map(str::to_string)))
    .expect("valid read call")
}

fn tool_result(call_id: &str, content: &str) -> ModelMessage {
    ModelMessage::ToolResult {
        call_id: call_id.to_string(),
        content: content.to_string(),
        name: "read".to_string(),
    }
}

fn call_item(call_id: &str, result: Option<&str>) -> TranscriptItem {
    TranscriptItem::ToolCall(TranscriptToolCall {
        arguments: r#"{"path":"a.txt"}"#.to_string(),
        call_id: call_id.to_string(),
        name: "read".to_string(),
        result: result.map(str::to_string),
    })
}

#[test]
fn ranges_report_their_bounds_and_stored_positions() {
    // Arrange
    let ranges = [
        TurnRange::All,
        TurnRange::Last(2),
        TurnRange::Span(TurnIndex::new(1)..TurnIndex::new(3)),
        TurnRange::Span(TurnIndex::new(u64::MAX)..TurnIndex::new(u64::MAX)),
        TurnRange::Last(u64::MAX),
    ];

    // Act
    let bounds: Vec<_> = ranges
        .iter()
        .map(|range| {
            (
                range.start(),
                range.end(),
                range.newest(),
                range.stored_bounds(),
            )
        })
        .collect();

    // Assert
    assert_eq!(
        bounds,
        [
            (TurnIndex::new(0), None, None, (0, i64::MAX, -1)),
            (TurnIndex::new(0), None, Some(2), (0, i64::MAX, 2)),
            (TurnIndex::new(1), Some(TurnIndex::new(3)), None, (1, 3, -1)),
            (
                TurnIndex::new(u64::MAX),
                Some(TurnIndex::new(u64::MAX)),
                None,
                (i64::MAX, i64::MAX, -1)
            ),
            (
                TurnIndex::new(0),
                None,
                Some(u64::MAX),
                (0, i64::MAX, i64::MAX)
            ),
        ]
    );
}

#[test]
fn record_orders_reasoning_text_calls_results_and_json_output() {
    // Arrange
    let turn = StoredTurn {
        outcome: Some(TurnOutcome::new(
            json!({"answer": "done"}),
            TurnReport::new(Duration::ZERO, Vec::new(), Vec::new()),
        )),
        ..stored(
            vec![
                ModelMessage::User("question".to_string()),
                ModelMessage::Instructions("be brief".to_string()),
                ModelMessage::AssistantToolCalls(vec![
                    read_call("first", Some("thinking"), Some("Reading")),
                    read_call("second", None, None),
                ]),
                tool_result("second", "second result"),
                tool_result("first", "first result"),
                tool_result("unknown", "orphan result"),
                ModelMessage::System("ignored".to_string()),
                ModelMessage::AssistantReasoning {
                    content: r#"{"answer":"done"}"#.to_string(),
                    reasoning_content: "concluding".to_string(),
                },
            ],
            Some(r#"{"output_schema":{"type":"object"}}"#),
        )
    };

    // Act
    let record = TurnRecord::from_stored(turn);

    // Assert
    assert_eq!(record.turn, TurnIndex::new(4));
    assert_eq!(record.status, TurnStatus::Completed);
    assert_eq!(
        record.input,
        [TranscriptInput::Text {
            text: "question".to_string()
        }]
    );
    assert_eq!(record.instructions.as_deref(), Some("be brief"));
    assert_eq!(record.usage, Some(Usage::default()));
    assert_eq!(record.started_at, UNIX_EPOCH + Duration::from_secs(8));
    assert_eq!(record.finished_at, UNIX_EPOCH + Duration::from_secs(9));
    assert_eq!(
        record.items,
        [
            TranscriptItem::Reasoning {
                text: "thinking".to_string()
            },
            TranscriptItem::Message {
                text: "Reading".to_string()
            },
            call_item("first", Some("first result")),
            call_item("second", Some("second result")),
            TranscriptItem::Reasoning {
                text: "concluding".to_string()
            },
            TranscriptItem::Output {
                output: RunOutput::Json(json!({"answer": "done"}))
            },
        ]
    );
}

#[test]
fn record_keeps_text_output_unanswered_calls_and_image_input() {
    // Arrange
    let image = ImageContent::new(
        ImageMediaType::Png,
        vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
    )
    .expect("valid PNG");
    let input = TurnInput::from_blocks(vec![
        InputBlock::Text("look".to_string()),
        InputBlock::Image(image),
    ])
    .expect("image input");
    let text_turn = stored(
        vec![
            ModelMessage::UserInput(input),
            ModelMessage::AssistantToolCall(read_call("call", None, None)),
            ModelMessage::Assistant("plain {answer".to_string()),
        ],
        Some(r#"{"output_schema":null}"#),
    );
    let unparsable_json = stored(
        vec![
            ModelMessage::User("question".to_string()),
            ModelMessage::Assistant("not json".to_string()),
        ],
        Some("not an options snapshot"),
    );

    // Act
    let text_record = TurnRecord::from_stored(text_turn);
    let json_record = TurnRecord::from_stored(unparsable_json);

    // Assert
    assert_eq!(
        text_record.input,
        [
            TranscriptInput::Text {
                text: "look".to_string()
            },
            TranscriptInput::Image {
                data: "iVBORw0KGgo=".to_string(),
                media_type: "image/png".to_string(),
            },
        ]
    );
    assert_eq!(text_record.usage, None);
    assert_eq!(
        text_record.items,
        [
            call_item("call", None),
            TranscriptItem::Output {
                output: RunOutput::Text("plain {answer".to_string())
            },
        ]
    );
    assert_eq!(
        json_record.items,
        [TranscriptItem::Output {
            output: RunOutput::Text("not json".to_string())
        }],
        "output that does not parse is kept as text"
    );
}

#[test]
fn record_round_trips_with_millisecond_times() -> Result<(), serde_json::Error> {
    // Arrange
    let record = TurnRecord::from_stored(StoredTurn {
        error_type: Some("cancelled".to_string()),
        started_at: UNIX_EPOCH + Duration::from_millis(1_500),
        status: TurnStatus::Interrupted,
        ..stored(vec![ModelMessage::User("question".to_string())], None)
    });

    // Act
    let encoded = serde_json::to_value(&record)?;
    let decoded: TurnRecord = serde_json::from_value(encoded.clone())?;

    // Assert
    assert_eq!(encoded["started_at"], json!(1_500));
    assert_eq!(encoded["finished_at"], json!(9_000));
    assert_eq!(encoded["status"], json!("interrupted"));
    assert_eq!(decoded, record);

    Ok(())
}

#[test]
fn turn_record_schema_matches_the_committed_snapshot() -> Result<(), serde_json::Error> {
    // Arrange
    let schema = schemars::schema_for!(TurnRecord);

    // Act
    let generated = format!("{}\n", serde_json::to_string_pretty(&schema)?);

    // Assert
    assert!(
        generated == TURN_RECORD_SCHEMA,
        "crates/ag-harness/schema/turn_record.json is stale; replace it with:\n{generated}"
    );

    Ok(())
}
