//! Public run event stream contract: order, items, terminal completion,
//! backpressure, and dropped streams.

use std::error::Error;
use std::future::pending;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use ag_harness::catalog::ModelId;
use ag_harness::event::{Item, RunSummary, ToolCallItem, ToolCallStatus};
use ag_harness::model::{
    CompletionMetadata, CompletionUsage, ModelCompletion, ModelMessage, ModelMetadata,
    ModelRequest, ModelResponse,
};
use ag_harness::store::MemoryStore;
use ag_harness::tool::ToolCall;
use ag_harness::{
    ErrorKind, EventStream, Harness, Model, ModelError, OutputSchema, Run, RunEvent, RunEventKind,
    RunOutcome, RunOutput, RunRequest,
};
use async_trait::async_trait;
use futures_core::Stream;
use serde_json::json;

use crate::context_budget_fixture::unbounded_context_budget;

/// Compiles only while event streams can move into spawned tasks and
/// events can cross threads.
const _: fn() = || {
    fn assert_send<T: Send + 'static>() {}
    fn assert_shareable<T: Clone + Send + Sync + 'static>() {}
    assert_send::<EventStream>();
    assert_shareable::<RunEvent>();
};

/// Distinct calls in the batch of [`BatchModel`]; their events overflow the
/// stream's buffer.
const BATCH_CALLS: usize = 70;

/// Answers the first request with a denied `read` call that carries
/// reasoning and assistant text, and the next with output, a rate limit for
/// the prompt `fail`, or never for the prompt `wait`.
struct ToolTurnModel;

#[async_trait]
impl Model for ToolTurnModel {
    fn metadata(&self) -> Option<ModelMetadata> {
        ModelMetadata::new("test", "tool-turn").ok()
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        if request.prompt() == "wait" {
            return pending().await;
        }
        if !matches!(
            request.messages().last(),
            Some(ModelMessage::ToolResult { .. })
        ) {
            let call = ToolCall::from_json(
                "read-name".into(),
                "read",
                r#"{"path":"name.txt"}"#,
                Some("look it up".into()),
            )?
            .with_assistant_text(Some("Reading the name".into()))?;

            return Ok(completion(ModelResponse::ToolCall(call), 5));
        }
        if request.prompt() == "fail" {
            return Err(ModelError::provider(
                429,
                None,
                io::Error::other("slow down"),
            ));
        }

        Ok(completion(ModelResponse::Output(json!({"name": "Ada"})), 7))
    }
}

/// Answers with [`BATCH_CALLS`] distinct denied `read` calls in one
/// response; the repeated unavailable calls then stop the run.
struct BatchModel;

#[async_trait]
impl Model for BatchModel {
    async fn complete(&self, _request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        let calls = (0..BATCH_CALLS)
            .map(|index| {
                ToolCall::from_json(
                    format!("call-{index}"),
                    "read",
                    &json!({"path": format!("{index}.txt")}).to_string(),
                    None,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(ModelCompletion::from_response(ModelResponse::ToolCalls(
            calls,
        )))
    }
}

fn completion(response: ModelResponse, output_tokens: u64) -> ModelCompletion {
    let usage = CompletionUsage::new(None, None, Some(10), Some(output_tokens), None, None);

    ModelCompletion::new(
        CompletionMetadata::new("stop".into(), None, None, None, Some(usage)),
        response,
    )
}

fn name_schema() -> Result<OutputSchema, Box<dyn Error>> {
    Ok(OutputSchema::new(json!({
        "type": "object",
        "properties": {"name": {"type": "string"}},
        "required": ["name"]
    }))?)
}

/// Runs `run` to its outcome while reading every event of its stream.
async fn streamed(run: Run) -> (Vec<RunEvent>, RunOutcome) {
    let (stream, handle) = run.into_parts();

    tokio::join!(drain(stream), handle.outcome())
}

async fn drain(mut stream: EventStream) -> Vec<RunEvent> {
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    events
}

/// Names each event's kind, with the item kind of item events.
fn labels(events: &[RunEvent]) -> Vec<String> {
    events
        .iter()
        .map(|event| match &event.kind {
            RunEventKind::Started { .. } => "started".to_string(),
            RunEventKind::ItemStarted { item_kind, .. } => format!("started {item_kind:?}"),
            RunEventKind::ItemCompleted { item, .. } => match item {
                Item::Message { .. } => "completed Message".to_string(),
                Item::Reasoning { .. } => "completed Reasoning".to_string(),
                Item::ToolCall(_) => "completed ToolCall".to_string(),
                _ => "completed other".to_string(),
            },
            RunEventKind::ModelRequestCompleted { .. } => "request".to_string(),
            RunEventKind::Completed(_) => "completed".to_string(),
            _ => "other".to_string(),
        })
        .collect()
}

fn sequence_numbers(events: &[RunEvent]) -> Vec<u64> {
    events.iter().map(|event| event.seq).collect()
}

fn completed_items(events: &[RunEvent]) -> Vec<Item> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::ItemCompleted { item, .. } => Some(item.clone()),
            _ => None,
        })
        .collect()
}

/// Returns the summary of the run's last event, which must be
/// [`RunEventKind::Completed`].
fn last_summary(events: &[RunEvent]) -> Result<&RunSummary, String> {
    match events.last().map(|event| &event.kind) {
        Some(RunEventKind::Completed(summary)) => Ok(summary),
        other => Err(format!("the last event must be `Completed`, got {other:?}")),
    }
}

/// Returns the failure kind of the run's last event.
fn completed_error(events: &[RunEvent]) -> Result<Option<ErrorKind>, String> {
    Ok(last_summary(events)?.error.as_ref().map(|error| error.kind))
}

#[tokio::test]
async fn tool_turn_streams_ordered_items_and_usage() -> Result<(), Box<dyn Error>> {
    // Arrange
    let harness =
        Harness::new(ToolTurnModel, unbounded_context_budget()).store(Arc::new(MemoryStore::new()));
    let session = harness.session("events", name_schema()?).create().await?;

    // Act
    let (events, outcome) = streamed(session.run(RunRequest::new("Read the name"))).await;

    // Assert
    assert_eq!(
        labels(&events),
        [
            "started",
            "request",
            "started Reasoning",
            "completed Reasoning",
            "started Message",
            "completed Message",
            "started ToolCall",
            "completed ToolCall",
            "request",
            "started Message",
            "completed Message",
            "completed",
        ]
    );
    assert_eq!(
        sequence_numbers(&events),
        (0..12).collect::<Vec<u64>>(),
        "events are numbered without gaps"
    );
    assert!(events.iter().all(|event| event.run == events[0].run));
    let RunEventKind::Started { model, session, .. } = &events[0].kind else {
        return Err("the first event must be `Started`".into());
    };
    assert_eq!(model.as_ref().map(ModelId::as_str), Some("test/tool-turn"));
    assert_eq!(session, "events");
    let items = completed_items(&events);
    assert_eq!(
        items[0],
        Item::Reasoning {
            text: "look it up".into()
        }
    );
    assert_eq!(
        items[1],
        Item::Message {
            content: RunOutput::Text("Reading the name".into())
        }
    );
    let Item::ToolCall(ToolCallItem {
        arguments,
        call_id,
        name,
        output,
        status,
        ..
    }) = &items[2]
    else {
        return Err("the third item must be the tool call".into());
    };
    assert_eq!(
        (call_id.as_str(), name.as_str(), *status),
        ("read-name", "read", ToolCallStatus::Unavailable)
    );
    assert!(arguments.contains("name.txt"));
    assert_ne!(output.as_str(), "");
    assert_eq!(
        items[3],
        Item::Message {
            content: RunOutput::Json(json!({"name": "Ada"}))
        }
    );
    let streamed_output: u64 = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::ModelRequestCompleted { usage } => Some(usage.output),
            _ => None,
        })
        .sum();
    assert_eq!(streamed_output, outcome.usage.output);
    assert_eq!(streamed_output, 12);
    assert_eq!(completed_error(&events)?, None);
    assert!(outcome.result.is_ok());

    Ok(())
}

#[tokio::test]
async fn failed_run_completes_its_stream_with_the_failure() -> Result<(), Box<dyn Error>> {
    // Arrange
    let harness = Harness::new(ToolTurnModel, unbounded_context_budget());

    // Act
    let (events, outcome) =
        streamed(harness.run(RunRequest::new("fail").output(name_schema()?))).await;

    // Assert
    assert_eq!(completed_error(&events)?, Some(ErrorKind::RateLimited));
    let summary = last_summary(&events)?;
    assert!(summary.error.as_ref().is_some_and(|error| error.retryable));
    assert_eq!(summary.usage, outcome.usage);
    assert_eq!(labels(&events).first().map(String::as_str), Some("started"));

    Ok(())
}

#[tokio::test]
async fn cancelled_run_completes_its_stream_last() -> Result<(), Box<dyn Error>> {
    // Arrange
    let harness = Harness::new(ToolTurnModel, unbounded_context_budget());
    let (mut stream, handle) = harness
        .run(RunRequest::new("wait").output(name_schema()?))
        .into_parts();
    let cancel = handle.cancel_token();
    let outcome = tokio::spawn(handle.outcome());

    // Act
    let started = stream.next().await.ok_or("the run starts")?;
    cancel.cancel();
    let mut events = vec![started];
    events.extend(drain(stream).await);
    let outcome = outcome.await?;

    // Assert
    assert_eq!(labels(&events), ["started", "completed"]);
    assert_eq!(completed_error(&events)?, Some(ErrorKind::Cancelled));
    assert_eq!(
        outcome.result.map_err(|error| error.kind()).err(),
        Some(ErrorKind::Cancelled)
    );

    Ok(())
}

#[tokio::test]
async fn run_cancelled_before_starting_streams_only_its_completion() -> Result<(), Box<dyn Error>> {
    // Arrange
    let harness = Harness::new(ToolTurnModel, unbounded_context_budget());
    let run = harness.run(RunRequest::new("Read the name").output(name_schema()?));
    run.cancel_token().cancel();

    // Act
    let (events, _) = streamed(run).await;

    // Assert
    assert_eq!(labels(&events), ["completed"]);
    assert_eq!(completed_error(&events)?, Some(ErrorKind::Cancelled));

    Ok(())
}

#[tokio::test]
async fn slow_consumer_holds_the_run_back_without_losing_events() -> Result<(), Box<dyn Error>> {
    // Arrange
    let harness = Harness::new(BatchModel, unbounded_context_budget());
    let (mut stream, handle) = harness
        .run(RunRequest::new("batch").output(name_schema()?))
        .into_parts();
    let outcome = tokio::spawn(handle.outcome());
    tokio::time::sleep(Duration::from_millis(50)).await;
    let held_back = !outcome.is_finished();

    // Act
    let mut events = Vec::new();
    while let Some(event) =
        std::future::poll_fn(|context| Pin::new(&mut stream).poll_next(context)).await
    {
        events.push(event);
    }
    let outcome = outcome.await?;

    // Assert
    assert!(held_back, "the run waits while its stream is full");
    let expected = 2 * BATCH_CALLS + 3;
    assert_eq!(
        sequence_numbers(&events),
        (0..u64::try_from(expected)?).collect::<Vec<u64>>()
    );
    assert_eq!(completed_items(&events).len(), BATCH_CALLS);
    assert_eq!(completed_error(&events)?, Some(ErrorKind::Repetition));
    assert_eq!(
        outcome.result.map_err(|error| error.kind()).err(),
        Some(ErrorKind::Repetition)
    );

    Ok(())
}

#[tokio::test]
async fn dropped_stream_neither_stalls_nor_cancels_the_run() -> Result<(), Box<dyn Error>> {
    // Arrange
    let harness = Harness::new(BatchModel, unbounded_context_budget());
    let (stream, handle) = harness
        .run(RunRequest::new("batch").output(name_schema()?))
        .into_parts();

    // Act
    drop(stream);
    let outcome = handle.outcome().await;

    // Assert
    assert_eq!(outcome.report.tool_calls().len(), BATCH_CALLS);
    assert_eq!(
        outcome.result.map_err(|error| error.kind()).err(),
        Some(ErrorKind::Repetition)
    );

    Ok(())
}
