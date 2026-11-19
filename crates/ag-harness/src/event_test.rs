use std::error::Error;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jsonschema::Validator;
use serde_json::json;

use super::{
    CompactionOutcome, CompactionReport, EVENT_BUFFER, EventSink, EventStream, Item, ItemId,
    ItemKind, RunEvent, RunEventKind, RunFailure, RunId, RunSummary, SessionId, ToolCallItem,
    ToolCallStatus, TurnIndex,
};
use crate::catalog::{ModelId, Pricing};
use crate::error::{ErrorKind, RunError};
use crate::lifecycle::ToolUnavailableReason;
use crate::limit::LimitKind;
use crate::run::{RunOutcome, RunOutput, Usage};
use crate::tool::{ReadAction, ToolCall};
use crate::turn::{ContextActivity, HostToolOutcome, ToolActivity, TurnError, TurnReport};
use crate::usage::Cost;

/// Committed JSON schema of [`RunEvent`]; regenerate it from the failure
/// message of [`run_event_schema_matches_the_committed_snapshot`].
const RUN_EVENT_SCHEMA: &str = include_str!("../schema/run_event.json");

/// Builds a settled outcome with `result` and no recorded activity.
fn outcome(result: Result<RunOutput, RunError>) -> RunOutcome {
    RunOutcome {
        commands: Vec::new(),
        cost: None,
        files_changed: Vec::new(),
        records_error: None,
        report: TurnReport::new(Duration::ZERO, Vec::new(), Vec::new()),
        result,
        settlement: Ok(()),
        usage: Usage::default(),
    }
}

fn cancelled() -> Result<RunOutput, RunError> {
    Err(RunError::from(TurnError::Cancelled))
}

fn request_completed() -> RunEventKind {
    RunEventKind::ModelRequestCompleted {
        cost: None,
        model: None,
        usage: Usage::default(),
    }
}

async fn drain(mut stream: EventStream) -> Vec<RunEvent> {
    let mut events = Vec::new();
    while let Some(event) = stream.receiver.recv().await {
        events.push(event);
    }

    events
}

#[tokio::test]
async fn sink_numbers_events_and_items_and_ends_with_the_output() {
    // Arrange
    let (sink, stream) = EventSink::channel(RunId::generate());
    let output = RunOutput::Json(json!({"name": "Ada"}));

    // Act
    let run = stream.run_id().clone();
    let first = sink.start_item(ItemKind::Message).await;
    let second = sink.start_item(ItemKind::Reasoning).await;
    sink.complete(&outcome(Ok(output.clone()))).await;
    sink.emit(request_completed()).await;
    let events = drain(stream).await;

    // Assert
    assert_eq!([first.get(), second.get()], [0, 1]);
    let sequence: Vec<u64> = events.iter().map(|event| event.seq).collect();
    assert_eq!(sequence, [0, 1, 2, 3, 4]);
    assert!(events.iter().all(|event| event.run == run));
    assert_eq!(events[0].run.as_str().len(), 36, "a hyphenated UUID");
    assert_eq!(
        events[2].kind,
        RunEventKind::ItemStarted {
            id: ItemId(2),
            item_kind: ItemKind::Output,
        }
    );
    assert_eq!(
        events[3].kind,
        RunEventKind::ItemCompleted {
            id: ItemId(2),
            item: Item::Output { output },
        }
    );
    assert!(matches!(events[4].kind, RunEventKind::Completed(_)));
}

#[tokio::test]
async fn sink_discards_events_after_the_stream_is_dropped() {
    // Arrange
    let (sink, stream) = EventSink::channel(RunId::generate());
    drop(stream);

    // Act
    for _ in 0..200 {
        sink.emit(request_completed()).await;
    }

    // Assert
    assert_eq!(sink.start_item(ItemKind::ToolCall).await.get(), 0);
}

#[tokio::test(start_paused = true)]
async fn completion_skips_the_wait_of_a_full_stream_without_a_sequence_gap() {
    // Arrange
    let (sink, stream) = EventSink::channel(RunId::generate());
    for _ in 0..EVENT_BUFFER {
        sink.emit(request_completed()).await;
    }
    let blocked =
        tokio::time::timeout(Duration::from_secs(1), sink.emit(request_completed())).await;

    // Act
    let completed =
        tokio::time::timeout(Duration::from_secs(1), sink.complete(&outcome(cancelled()))).await;
    let events = drain(stream).await;

    // Assert
    assert!(blocked.is_err(), "a full stream holds ordinary events back");
    assert!(completed.is_ok(), "completion never waits for the consumer");
    let sequence: Vec<u64> = events.iter().map(|event| event.seq).collect();
    let expected: Vec<u64> = (0..=u64::try_from(EVENT_BUFFER).unwrap_or_default()).collect();
    assert_eq!(sequence, expected, "an abandoned event takes no number");
    assert!(
        matches!(
            events.last().map(|event| &event.kind),
            Some(RunEventKind::Completed(summary))
                if summary.error.as_ref().map(|error| error.kind) == Some(ErrorKind::Cancelled)
        ),
        "a cancelled run completes without an output item"
    );
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
                tool: "bash".into(),
            },
            ToolCallStatus::Unavailable,
        ),
    ];
    let cases = cases.into_iter().chain(
        [
            (HostToolOutcome::Completed, ToolCallStatus::Completed),
            (HostToolOutcome::ResultTooLarge, ToolCallStatus::Completed),
            (HostToolOutcome::Rejected, ToolCallStatus::Rejected),
            (HostToolOutcome::InvalidArguments, ToolCallStatus::Rejected),
        ]
        .map(|(outcome, status)| {
            (
                ToolActivity::Host {
                    duration,
                    outcome,
                    tool: "fetch".into(),
                },
                status,
            )
        }),
    );
    let cases = cases.collect::<Vec<_>>();

    // Act
    let statuses: Vec<ToolCallStatus> = cases
        .iter()
        .map(|(activity, _)| ToolCallStatus::from(activity))
        .collect();

    // Assert
    let expected: Vec<ToolCallStatus> = cases.iter().map(|(_, status)| *status).collect();
    assert_eq!(statuses, expected);
}

/// Returns a completed `read` call item.
fn read_call_item() -> Result<Item, Box<dyn Error>> {
    let call = ToolCall::from_json("call-1".into(), "read", r#"{"path":"a.txt"}"#, None)?;
    let activity = ToolActivity::Read {
        duration: Duration::from_millis(3),
        end_line: Some(1),
        path: "a.txt".into(),
        start_line: 1,
        truncated: false,
    };

    Ok(Item::ToolCall(ToolCallItem::new(&call, "1: hi", &activity)))
}

/// Returns usage of 500 input tokens, one written to the cache, and its
/// cost at $2 per million input tokens.
fn priced_usage() -> (Usage, Cost) {
    let usage = Usage {
        input: 500,
        cache_write: Some(1),
        ..Usage::default()
    };

    (usage, Cost::of(&usage, Pricing::new(2_000_000, 0)))
}

/// Returns a completed request of `usage` at `cost` that the cataloged
/// summary model `host/cheap` answered.
fn summary_request_completed(usage: Usage, cost: Cost) -> RunEventKind {
    RunEventKind::ModelRequestCompleted {
        cost: Some(cost),
        model: Some(ModelId::new("host/cheap")),
        usage,
    }
}

#[test]
fn run_event_round_trips_through_json_matching_its_schema() -> Result<(), Box<dyn Error>> {
    // Arrange
    let (sink, _stream) = EventSink::channel(RunId::generate());
    let session = SessionId::new("s");
    let turn = TurnIndex::from_position(2);
    let (usage, cost) = priced_usage();
    let kinds = vec![
        RunEventKind::Started {
            model: Some(ModelId::new("openai/gpt")),
            session: session.clone(),
            turn,
        },
        RunEventKind::ItemStarted {
            id: ItemId(0),
            item_kind: ItemKind::ToolCall,
        },
        RunEventKind::ItemCompleted {
            id: ItemId(0),
            item: read_call_item()?,
        },
        RunEventKind::ItemCompleted {
            id: ItemId(1),
            item: Item::Message {
                text: "Reading".into(),
            },
        },
        RunEventKind::ItemCompleted {
            id: ItemId(2),
            item: Item::Reasoning {
                text: "think".into(),
            },
        },
        RunEventKind::ItemCompleted {
            id: ItemId(3),
            item: Item::Output {
                output: RunOutput::Json(json!({"name": "Ada"})),
            },
        },
        RunEventKind::ItemCompleted {
            id: ItemId(4),
            item: Item::Output {
                output: RunOutput::Text("Ada".into()),
            },
        },
        RunEventKind::ContextReduced(ContextActivity::default()),
        summary_request_completed(usage, cost),
        RunEventKind::Completed(RunSummary {
            cost: Some(cost),
            error: Some(RunFailure {
                kind: ErrorKind::Cancelled,
                message: "turn was cancelled".into(),
                retryable: false,
            }),
            usage,
        }),
    ];
    let at = UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
    let events: Vec<RunEvent> = kinds
        .into_iter()
        .zip(0_u64..)
        .map(|(kind, seq)| RunEvent {
            at,
            kind,
            run: sink.run.clone(),
            seq,
        })
        .collect();
    let validator = Validator::new(&serde_json::to_value(schemars::schema_for!(RunEvent))?)?;

    // Act
    let encoded = serde_json::to_value(&events)?;
    let decoded: Vec<RunEvent> = serde_json::from_value(encoded.clone())?;

    // Assert
    assert_eq!(decoded, events);
    for event in encoded.as_array().ok_or("an array of events")? {
        let errors: Vec<String> = validator
            .iter_errors(event)
            .map(|error| error.to_string())
            .collect();
        assert_eq!(errors, Vec::<String>::new(), "{event} matches the schema");
    }
    assert_eq!((session.as_str(), turn.get()), ("s", 2));
    assert_eq!(encoded[0]["at"], 1_700_000_000_123_u64);
    assert_eq!(encoded[0]["kind"]["type"], "started");
    assert_eq!(encoded[0]["kind"]["session"], "s");
    assert_eq!(encoded[0]["kind"]["turn"], 2);
    assert_eq!(encoded[2]["kind"]["item"]["type"], "tool_call");
    assert_eq!(encoded[2]["kind"]["item"]["status"], "completed");
    assert_eq!(
        encoded[2]["kind"]["item"]["arguments"],
        r#"{"path":"a.txt"}"#
    );
    assert_eq!(encoded[3]["kind"]["item"]["text"], "Reading");
    assert_eq!(encoded[5]["kind"]["item"]["output"]["type"], "json");
    assert_eq!(encoded[6]["kind"]["item"]["output"]["type"], "text");
    let priced = json!({"micro_usd": 1000, "pricing": "host"});
    assert_eq!(encoded[8]["kind"]["cost"], priced);
    assert_eq!(encoded[8]["kind"]["model"], "host/cheap");
    assert_eq!(encoded[9]["kind"]["cost"], priced);
    assert_eq!(encoded[9]["kind"]["error"]["kind"], "cancelled");
    let unpriced = serde_json::to_value(request_completed())?;
    assert_eq!(unpriced["cost"], json!(null));
    assert_eq!(unpriced["model"], json!(null));

    Ok(())
}

#[test]
fn compacted_encodes_its_report_matching_the_schema() -> Result<(), Box<dyn Error>> {
    // Arrange
    let (sink, _stream) = EventSink::channel(RunId::generate());
    let events: Vec<RunEvent> = [
        RunEventKind::Compacted(CompactionReport {
            outcome: CompactionOutcome::Compacted {
                covered_through: TurnIndex(1),
                model: Some(ModelId::new("host/cheap")),
            },
        }),
        RunEventKind::Compacted(CompactionReport {
            outcome: CompactionOutcome::NothingToCover,
        }),
        RunEventKind::Compacted(CompactionReport {
            outcome: CompactionOutcome::Failed(RunFailure {
                kind: ErrorKind::InvalidOutput,
                message: "summary rejected".to_string(),
                retryable: false,
            }),
        }),
    ]
    .into_iter()
    .zip(0_u64..)
    .map(|(kind, seq)| RunEvent {
        at: UNIX_EPOCH,
        kind,
        run: sink.run.clone(),
        seq,
    })
    .collect();
    let validator = Validator::new(&serde_json::to_value(schemars::schema_for!(RunEvent))?)?;

    // Act
    let encoded = serde_json::to_value(&events)?;
    let decoded: Vec<RunEvent> = serde_json::from_value(encoded.clone())?;

    // Assert
    assert_eq!(decoded, events);
    for event in encoded.as_array().ok_or("an array of events")? {
        assert!(validator.is_valid(event), "{event} matches the schema");
    }
    assert_eq!(encoded[0]["kind"]["type"], "compacted");
    assert_eq!(encoded[0]["kind"]["outcome"]["status"], "compacted");
    assert_eq!(encoded[0]["kind"]["outcome"]["covered_through"], 1);
    assert_eq!(encoded[0]["kind"]["outcome"]["model"], "host/cheap");
    assert_eq!(encoded[1]["kind"]["outcome"]["status"], "nothing_to_cover");
    assert_eq!(encoded[2]["kind"]["outcome"]["status"], "failed");
    assert_eq!(encoded[2]["kind"]["outcome"]["kind"], "invalid_output");
    assert_eq!(encoded[2]["kind"]["outcome"]["message"], "summary rejected");

    Ok(())
}

#[test]
fn retrying_encodes_its_delay_in_milliseconds_matching_the_schema() -> Result<(), Box<dyn Error>> {
    // Arrange
    let event = RunEvent {
        at: UNIX_EPOCH,
        kind: RunEventKind::Retrying {
            after: Duration::from_millis(1_500),
            attempt: 2,
            kind: ErrorKind::RateLimited,
        },
        run: RunId::generate(),
        seq: 3,
    };
    let unbounded = RunEventKind::Retrying {
        after: Duration::MAX,
        attempt: 1,
        kind: ErrorKind::Overloaded,
    };
    let validator = Validator::new(&serde_json::to_value(schemars::schema_for!(RunEvent))?)?;

    // Act
    let encoded = serde_json::to_value(&event)?;
    let decoded: RunEvent = serde_json::from_value(encoded.clone())?;
    let saturated = serde_json::to_value(&unbounded)?;

    // Assert
    assert_eq!(decoded, event);
    assert!(validator.is_valid(&encoded), "{encoded} matches the schema");
    assert_eq!(
        encoded["kind"],
        json!({"type": "retrying", "after": 1_500, "attempt": 2, "kind": "rate_limited"})
    );
    assert_eq!(saturated["after"], u64::MAX);
    assert!(
        serde_json::from_value::<RunEventKind>(json!({
            "type": "retrying", "after": -1, "attempt": 1, "kind": "transport"
        }))
        .is_err()
    );

    Ok(())
}

#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
#[test]
fn retrying_reads_its_delay_to_the_millisecond_as_the_wire_does() -> Result<(), Box<dyn Error>> {
    // Arrange
    let jittered = Duration::new(1, 234_567_891);

    // Act
    let kind = RunEventKind::retrying(jittered, 1, ErrorKind::Overloaded);
    let decoded: RunEventKind = serde_json::from_value(serde_json::to_value(&kind)?)?;

    // Assert
    assert_eq!(
        kind,
        RunEventKind::Retrying {
            after: Duration::from_millis(1_234),
            attempt: 1,
            kind: ErrorKind::Overloaded,
        }
    );
    assert_eq!(decoded, kind);

    Ok(())
}

#[test]
fn budget_failures_carry_their_limit_beside_a_string_kind() -> Result<(), Box<dyn Error>> {
    // Arrange
    let failure = RunFailure {
        kind: ErrorKind::BudgetExhausted(LimitKind::Tokens),
        message: "run stopped at its tokens limit".into(),
        retryable: false,
    };
    let validator = Validator::new(&serde_json::to_value(schemars::schema_for!(RunFailure))?)?;

    let cancelled = RunFailure {
        kind: ErrorKind::Cancelled,
        ..failure.clone()
    };

    // Act
    let encoded = serde_json::to_value(&failure)?;
    let decoded: RunFailure = serde_json::from_value(encoded.clone())?;
    let cancelled_encoded = serde_json::to_value(&cancelled)?;

    // Assert
    assert_eq!(
        encoded,
        json!({
            "kind": "budget_exhausted",
            "limit": "tokens",
            "message": "run stopped at its tokens limit",
            "retryable": false
        })
    );
    assert_eq!(decoded, failure);
    assert!(validator.is_valid(&encoded));
    assert_eq!(cancelled_encoded["kind"], "cancelled");
    assert_eq!(cancelled_encoded.get("limit"), None);

    Ok(())
}

#[test]
fn run_failure_rejects_a_limit_that_does_not_match_its_kind() {
    // Arrange
    let failure = |kind: &str, limit: Option<&str>| {
        let mut failure = json!({"kind": kind, "message": "stopped", "retryable": false});
        if let Some(limit) = limit {
            failure["limit"] = json!(limit);
        }

        failure
    };

    // Act
    let missing = serde_json::from_value::<RunFailure>(failure("budget_exhausted", None));
    let unexpected = serde_json::from_value::<RunFailure>(failure("cancelled", Some("tokens")));

    // Assert
    assert!(missing.is_err());
    assert!(unexpected.is_err());
}

#[test]
fn run_event_before_the_unix_epoch_does_not_serialize() {
    // Arrange
    let (sink, _stream) = EventSink::channel(RunId::generate());
    let event = RunEvent {
        at: UNIX_EPOCH - Duration::from_secs(1),
        kind: request_completed(),
        run: sink.run.clone(),
        seq: 0,
    };

    // Act
    let encoded = serde_json::to_value(&event);

    // Assert
    assert!(encoded.is_err());
}

#[tokio::test]
async fn sink_timestamps_events_to_the_millisecond() {
    // Arrange
    let (sink, stream) = EventSink::channel(RunId::generate());
    let before = SystemTime::now();

    // Act
    sink.emit(request_completed()).await;
    drop(sink);
    let events = drain(stream).await;

    // Assert
    let elapsed = events[0].at.duration_since(UNIX_EPOCH).unwrap_or_default();
    assert_eq!(elapsed.subsec_nanos() % 1_000_000, 0);
    assert!(events[0].at <= SystemTime::now());
    assert!(events[0].at + Duration::from_millis(1) > before);
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
