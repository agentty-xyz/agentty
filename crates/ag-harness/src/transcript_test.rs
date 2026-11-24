use std::time::{Duration, UNIX_EPOCH};

use serde_json::json;

use super::{TranscriptInput, TurnErrorKind, TurnRange, TurnRecord, TurnStatus};
use crate::catalog::Pricing;
use crate::error::ErrorKind;
use crate::event::{Item, ToolCallItem, ToolCallStatus, TurnIndex};
use crate::input::{ImageContent, ImageMediaType, InputBlock, TurnInput};
use crate::lifecycle::TurnErrorType;
use crate::limit::LimitKind;
use crate::model::{ModelErrorType, ModelMessage};
use crate::run::{RunOutput, Usage};
use crate::store::StoredTurn;
use crate::tool::ToolCall;
use crate::turn::{TurnOutcome, TurnReport};
use crate::usage::{Cost, TurnUsage};

/// Committed JSON schema of [`TurnRecord`]; regenerate it from the failure
/// message of [`turn_record_schema_matches_the_committed_snapshot`].
const TURN_RECORD_SCHEMA: &str = include_str!("../schema/turn_record.json");

/// Returns a completed turn at position 4 holding `messages`, in JSON
/// output mode.
fn stored(messages: Vec<ModelMessage>) -> StoredTurn {
    StoredTurn::new(
        4,
        TurnStatus::Completed,
        UNIX_EPOCH + Duration::from_secs(8),
        UNIX_EPOCH + Duration::from_secs(9),
        messages,
    )
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

/// Returns the transcript item of `read` call `call_id`, answered with
/// `output` or unanswered.
fn call_item(call_id: &str, output: Option<&str>) -> Item {
    let mut item = ToolCallItem::unanswered(&read_call(call_id, None, None));
    if let Some(output) = output {
        item.output = output.to_string();
        item.status = ToolCallStatus::Answered;
    }

    Item::ToolCall(item)
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
fn ranges_round_trip_as_tagged_objects() -> Result<(), serde_json::Error> {
    // Arrange
    let ranges = [
        TurnRange::All,
        TurnRange::Last(2),
        TurnRange::Span(TurnIndex::new(1)..TurnIndex::new(3)),
    ];

    // Act
    let encoded = ranges
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()?;
    let decoded = encoded
        .iter()
        .cloned()
        .map(serde_json::from_value)
        .collect::<Result<Vec<TurnRange>, _>>()?;
    let schema = serde_json::to_value(schemars::schema_for!(TurnRange))?;

    // Assert
    assert_eq!(
        encoded,
        [
            json!({"type": "all"}),
            json!({"type": "last", "count": 2}),
            json!({"type": "span", "end": 3, "start": 1}),
        ]
    );
    assert_eq!(decoded, ranges);
    assert_eq!(schema["title"], json!("TurnRange"));
    assert_eq!(schema["oneOf"].as_array().map(Vec::len), Some(3));

    Ok(())
}

#[test]
fn record_of_a_turn_without_recorded_usage_reads_its_stored_outcome_unpriced() {
    // Arrange
    let outcome = TurnOutcome::new(
        json!({"answer": "done"}),
        TurnReport::new(Duration::ZERO, Vec::new(), Vec::new()),
    );
    let turn = stored(vec![ModelMessage::User("question".to_string())]).with_outcome(Some(outcome));

    // Act
    let record = TurnRecord::from_stored(turn);

    // Assert
    assert_eq!(record.usage, Some(Usage::default()));
    assert_eq!(record.cost, None);
}

#[test]
fn record_orders_reasoning_text_calls_results_and_json_output() {
    // Arrange
    let turn = stored(vec![
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
    ])
    .with_outcome(Some(TurnOutcome::new(
        json!({"answer": "done"}),
        TurnReport::new(Duration::ZERO, Vec::new(), Vec::new()),
    )));
    let usage = Usage {
        input: 3_000,
        ..Usage::default()
    };
    let recorded = TurnUsage {
        cost: Some(Cost::of(&usage, Pricing::new(1_000_000, 0))),
        usage,
    };
    let turn = turn.with_usage(Some(recorded));

    // Act
    let record = TurnRecord::from_stored(turn);

    // Assert
    assert_eq!(record.turn, TurnIndex::new(4));
    assert_eq!(record.status, TurnStatus::Completed);
    assert_eq!(record.error, None);
    assert_eq!(
        record.input,
        [TranscriptInput::Text {
            text: "question".to_string()
        }]
    );
    assert_eq!(record.instructions.as_deref(), Some("be brief"));
    assert_eq!(record.usage, Some(usage));
    assert_eq!(record.cost.map(|cost| cost.micro_usd), Some(3_000));
    assert_eq!(record.started_at, UNIX_EPOCH + Duration::from_secs(8));
    assert_eq!(record.finished_at, UNIX_EPOCH + Duration::from_secs(9));
    assert_eq!(
        record.items,
        [
            Item::Reasoning {
                text: "thinking".to_string()
            },
            Item::Message {
                text: "Reading".to_string()
            },
            call_item("first", Some("first result")),
            call_item("second", Some("second result")),
            Item::Reasoning {
                text: "concluding".to_string()
            },
            Item::Output {
                output: RunOutput::Json(json!({"answer": "done"}))
            },
        ]
    );
}

#[test]
fn record_shows_reasoning_summaries_and_hides_encrypted_reasoning() {
    // Arrange
    let encrypted = json!([
        {"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "gAAAA"}
    ])
    .to_string();
    let summarized = json!([{
        "type": "reasoning",
        "encrypted_content": "gAAAA",
        "summary": [{"type": "summary_text", "text": "Check a.txt."}]
    }])
    .to_string();
    let turn = stored(vec![
        ModelMessage::User("question".to_string()),
        ModelMessage::AssistantToolCall(read_call("first", Some(&encrypted), None)),
        tool_result("first", "first result"),
        ModelMessage::AssistantToolCall(read_call("second", Some(&summarized), None)),
        tool_result("second", "second result"),
        ModelMessage::AssistantReasoning {
            content: r#"{"answer":"done"}"#.to_string(),
            reasoning_content: encrypted,
        },
    ]);

    // Act
    let record = TurnRecord::from_stored(turn);

    // Assert
    assert_eq!(
        record.items,
        [
            call_item("first", Some("first result")),
            Item::Reasoning {
                text: "Check a.txt.".to_string()
            },
            call_item("second", Some("second result")),
            Item::Output {
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
    let text_turn = stored(vec![
        ModelMessage::UserInput(input),
        ModelMessage::AssistantToolCall(read_call("call", None, None)),
        ModelMessage::Assistant("plain {answer".to_string()),
    ])
    .with_text_output(true);
    let unparsable_json = stored(vec![
        ModelMessage::User("question".to_string()),
        ModelMessage::Assistant("not json".to_string()),
    ]);

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
    assert_eq!(text_record.cost, None);
    assert_eq!(
        text_record.items,
        [
            call_item("call", None),
            Item::Output {
                output: RunOutput::Text("plain {answer".to_string())
            },
        ]
    );
    assert_eq!(
        json_record.items,
        [Item::Output {
            output: RunOutput::Text("not json".to_string())
        }],
        "output that does not parse is kept as text"
    );
}

#[test]
fn stored_classifications_read_as_the_kinds_live_runs_report() {
    // Arrange
    let stored_forms: Vec<(String, String)> = TurnErrorKind::ALL
        .iter()
        .map(|error| (format!("{:?}", error.0), error.0.as_str().to_string()))
        .collect();

    // Act
    let kinds: Vec<_> = stored_forms
        .iter()
        .map(|(debug, telemetry)| {
            (
                TurnRecord::stored_error(debug).map(|error| error.kind),
                TurnRecord::stored_error(telemetry).map(|error| error.kind),
            )
        })
        .collect();

    // Assert
    let expected: Vec<_> = [
        ErrorKind::Cancelled,
        ErrorKind::ToolFailed,
        ErrorKind::ContextOverflow,
        ErrorKind::Repetition,
        ErrorKind::Config,
        ErrorKind::Config,
        ErrorKind::Store,
        ErrorKind::BudgetExhausted(LimitKind::ModelRequests),
        ErrorKind::BudgetExhausted(LimitKind::ToolCalls),
        ErrorKind::BudgetExhausted(LimitKind::Tokens),
        ErrorKind::BudgetExhausted(LimitKind::WallTime),
        ErrorKind::Other,
        ErrorKind::Transport,
        ErrorKind::Other,
        ErrorKind::ContextOverflow,
        ErrorKind::InvalidOutput,
        ErrorKind::InvalidOutput,
        ErrorKind::Config,
        ErrorKind::Config,
        ErrorKind::InvalidOutput,
        ErrorKind::InvalidOutput,
        ErrorKind::InvalidOutput,
        ErrorKind::BudgetExhausted(LimitKind::Cost),
        ErrorKind::BudgetExhausted(LimitKind::Tokens),
        ErrorKind::BudgetExhausted(LimitKind::ModelRequests),
        ErrorKind::BudgetExhausted(LimitKind::ToolCalls),
        ErrorKind::BudgetExhausted(LimitKind::WallTime),
        ErrorKind::ApprovalTimeout,
        ErrorKind::SandboxUnavailable,
    ]
    .into_iter()
    .map(|kind| (Some(kind), Some(kind)))
    .collect();
    assert_eq!(kinds, expected);
    assert_eq!(
        stored_forms[7].0, "RunLimit(ModelRequests)",
        "failed turns record the classification's Debug form"
    );
    assert_eq!(
        TurnErrorKind::ALL[13].0,
        TurnErrorType::Model(ModelErrorType::Provider)
    );
    assert_eq!(TurnRecord::stored_error("interrupted"), None);
    assert_eq!(TurnRecord::stored_error("unknown"), None);
}

#[test]
fn record_round_trips_with_millisecond_times_and_typed_errors() -> Result<(), serde_json::Error> {
    // Arrange
    let mut cancelled = stored(vec![ModelMessage::User("question".to_string())])
        .with_error_type(Some("cancelled".to_string()));
    cancelled.started_at = UNIX_EPOCH + Duration::from_millis(1_500);
    cancelled.status = TurnStatus::Interrupted;
    let mut limited = stored(Vec::new()).with_error_type(Some("RunLimit(ToolCalls)".to_string()));
    limited.status = TurnStatus::Failed;
    let records = [
        TurnRecord::from_stored(cancelled),
        TurnRecord::from_stored(limited),
    ];

    // Act
    let encoded = records
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()?;
    let decoded = encoded
        .iter()
        .cloned()
        .map(serde_json::from_value)
        .collect::<Result<Vec<TurnRecord>, _>>()?;
    let mut missing_limit = encoded[1].clone();
    missing_limit["error"] = json!({"kind": "budget_exhausted"});
    let invalid = serde_json::from_value::<TurnRecord>(missing_limit);

    // Assert
    assert_eq!(encoded[0]["started_at"], json!(1_500));
    assert_eq!(encoded[0]["finished_at"], json!(9_000));
    assert_eq!(encoded[0]["status"], json!("interrupted"));
    assert_eq!(encoded[0]["error"], json!({"kind": "cancelled"}));
    assert_eq!(
        encoded[1]["error"],
        json!({"kind": "budget_exhausted", "limit": "tool_calls"})
    );
    assert_eq!(decoded, records);
    assert!(invalid.is_err(), "a budget failure must name its limit");

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
