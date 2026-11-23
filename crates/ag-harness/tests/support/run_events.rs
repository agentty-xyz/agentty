//! Public run event stream contract: order, items, terminal completion,
//! backpressure, cancellation, and dropped streams.

use std::error::Error;
use std::future::{pending, poll_fn};
use std::io;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use ag_harness::catalog::{ModelId, ModelSpec};
use ag_harness::event::{
    CompactionOutcome, Item, ItemKind, RunSummary, ToolCallItem, ToolCallStatus,
};
use ag_harness::model::{
    CompletionMetadata, CompletionUsage, ContextBudget, ContextPolicy, ModelCompletion,
    ModelMessage, ModelMetadata, ModelRequest, ModelResponse,
};
use ag_harness::store::MemoryStore;
use ag_harness::tool::{FileSystem, LocalFileSystem, ToolCall};
use ag_harness::{
    ErrorKind, EventStream, Harness, Model, ModelCatalog, ModelError, OutputSchema, Role, Run,
    RunEvent, RunEventKind, RunOutcome, RunOutput, RunRequest, ToolSet,
};
use async_trait::async_trait;
use futures_core::Stream;
use serde_json::json;
use tokio::io::AsyncRead;
use tokio::sync::Notify;
use tokio::time::timeout;

use crate::context_budget_fixture::unbounded_context_budget;
use crate::model_profile_fixture::unbounded_model_profile;
use crate::repository_fixture::repository_with_host_git;

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

/// Bounds waits that would hang if the run never resolved; paused test time
/// reaches it as soon as every task is idle.
const HANG: Duration = Duration::from_secs(60);

/// Answers the first request with a `read` call that carries reasoning and
/// assistant text, and the next with output, a rate limit for the prompt
/// `fail`, or never for the prompt `wait`.
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

/// Reads through the local filesystem, except that opening a file signals
/// `entered` and never finishes.
struct StalledRead {
    entered: Arc<Notify>,
}

#[async_trait]
impl FileSystem for StalledRead {
    async fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        LocalFileSystem.canonicalize(path).await
    }

    async fn open_beneath(
        &self,
        _root: &Path,
        _path: &Path,
    ) -> io::Result<Box<dyn AsyncRead + Send + Unpin>> {
        self.entered.notify_one();

        pending().await
    }

    async fn replace_beneath(
        &self,
        root: &Path,
        path: &Path,
        expected: Option<Vec<u8>>,
        content: Vec<u8>,
    ) -> io::Result<()> {
        LocalFileSystem
            .replace_beneath(root, path, expected, content)
            .await
    }
}

fn completion(response: ModelResponse, output_tokens: u64) -> ModelCompletion {
    let usage = CompletionUsage::default()
        .with_input_tokens(Some(10))
        .with_output_tokens(Some(output_tokens));

    ModelCompletion::new(
        CompletionMetadata::new("stop").with_usage(Some(usage)),
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

/// Waits for the next event through the `Stream` implementation.
async fn next_event(stream: &mut EventStream) -> Option<RunEvent> {
    poll_fn(|context| Pin::new(&mut *stream).poll_next(context)).await
}

async fn drain(mut stream: EventStream) -> Vec<RunEvent> {
    let mut events = Vec::new();
    while let Some(event) = next_event(&mut stream).await {
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
                Item::Output { .. } => "completed Output".to_string(),
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
            "started Output",
            "completed Output",
            "completed",
        ]
    );
    assert_eq!(
        sequence_numbers(&events),
        (0..12).collect::<Vec<u64>>(),
        "events are numbered without gaps"
    );
    assert!(events.iter().all(|event| event.run == events[0].run));
    let RunEventKind::Started {
        model,
        session,
        turn,
        ..
    } = &events[0].kind
    else {
        return Err("the first event must be `Started`".into());
    };
    assert_eq!(*model, None, "a directly supplied model has no catalog ID");
    assert_eq!(session.as_str(), "events");
    assert_eq!(turn.get(), 0, "the session's first turn");
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
            text: "Reading the name".into()
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
        Item::Output {
            output: RunOutput::Json(json!({"name": "Ada"}))
        }
    );
    let streamed_output: u64 = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::ModelRequestCompleted { usage, .. } => Some(usage.output),
            _ => None,
        })
        .sum();
    assert_eq!(streamed_output, outcome.usage.output);
    assert_eq!(streamed_output, 12);
    // A model the host supplied directly has no catalog identifier.
    assert!(events.iter().all(|event| !matches!(
        &event.kind,
        RunEventKind::ModelRequestCompleted { model: Some(_), .. }
    )));
    assert_eq!(completed_error(&events)?, None);
    assert!(outcome.result.is_ok());

    Ok(())
}

#[tokio::test]
async fn cataloged_run_starts_with_the_catalog_model_id() -> Result<(), Box<dyn Error>> {
    // Arrange
    let catalog = Arc::new(
        ModelCatalog::builder()
            .model(
                ModelSpec::new("host/tool-turn").with_profile(unbounded_model_profile()),
                Arc::new(ToolTurnModel),
            )
            .assign(Role::SMART, ["host/tool-turn"])
            .build()?,
    );
    let harness = Harness::builder().catalog(Arc::clone(&catalog)).build()?;

    // Act
    let (events, _) =
        streamed(harness.run(RunRequest::new("Read the name").output(name_schema()?))).await;

    // Assert
    let Some(RunEventKind::Started {
        model: Some(model), ..
    }) = events.first().map(|event| &event.kind)
    else {
        return Err("the first event must be `Started` with a model".into());
    };
    assert_eq!(model, &ModelId::new("host/tool-turn"));
    assert!(
        catalog.spec(model).is_some(),
        "the ID resolves in the catalog"
    );

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
    assert!(
        !labels(&events)
            .iter()
            .any(|label| label == "started Output"),
        "a failed run has no output item"
    );

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
    let started = next_event(&mut stream).await.ok_or("the run starts")?;
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
async fn run_cancelled_during_a_tool_call_leaves_the_call_open() -> Result<(), Box<dyn Error>> {
    // Arrange
    let root = tempfile::tempdir()?;
    std::fs::write(root.path().join("name.txt"), "Ada\n")?;
    let entered = Arc::new(Notify::new());
    let harness = Harness::new(ToolTurnModel, unbounded_context_budget())
        .repository(repository_with_host_git(root.path()))
        .file_system(StalledRead {
            entered: Arc::clone(&entered),
        });
    let request = RunRequest::new("Read the name")
        .output(name_schema()?)
        .tools(ToolSet::empty().with_read());
    let (stream, handle) = harness.run(request).into_parts();
    let cancel = handle.cancel_token();
    let outcome = tokio::spawn(handle.outcome());
    let events = tokio::spawn(drain(stream));

    // Act
    timeout(HANG, entered.notified()).await?;
    cancel.cancel();
    let outcome = timeout(HANG, outcome).await??;
    let events = events.await?;

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
            "completed",
        ],
        "the interrupted call never completes"
    );
    assert!(matches!(
        events[6].kind,
        RunEventKind::ItemStarted {
            item_kind: ItemKind::ToolCall,
            ..
        }
    ));
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

#[tokio::test(start_paused = true)]
async fn slow_consumer_holds_the_run_back_without_losing_events() -> Result<(), Box<dyn Error>> {
    // Arrange
    let harness = Harness::new(BatchModel, unbounded_context_budget());
    let (stream, handle) = harness
        .run(RunRequest::new("batch").output(name_schema()?))
        .into_parts();
    let outcome = tokio::spawn(handle.outcome());
    // Paused time advances only once every task is idle, which the run is
    // only while it waits on its full stream.
    tokio::time::sleep(Duration::from_millis(1)).await;
    let held_back = !outcome.is_finished();

    // Act
    let events = drain(stream).await;
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

#[tokio::test(start_paused = true)]
async fn cancelling_a_run_with_a_full_unread_stream_resolves_its_outcome()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let harness = Harness::new(BatchModel, unbounded_context_budget());
    let (stream, handle) = harness
        .run(RunRequest::new("batch").output(name_schema()?))
        .into_parts();
    let cancel = handle.cancel_token();
    let outcome = tokio::spawn(handle.outcome());
    tokio::time::sleep(Duration::from_millis(1)).await;
    let held_back = !outcome.is_finished();

    // Act
    cancel.cancel();
    let outcome = timeout(HANG, outcome).await??;
    let events = drain(stream).await;

    // Assert
    assert!(held_back, "the unread stream filled and held the run back");
    assert_eq!(
        outcome.result.map_err(|error| error.kind()).err(),
        Some(ErrorKind::Cancelled)
    );
    assert_eq!(completed_error(&events)?, Some(ErrorKind::Cancelled));
    let count = u64::try_from(events.len())?;
    assert_eq!(sequence_numbers(&events), (0..count).collect::<Vec<u64>>());

    Ok(())
}

#[tokio::test]
async fn stream_dropped_before_the_run_discards_every_event() -> Result<(), Box<dyn Error>> {
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

#[tokio::test(start_paused = true)]
async fn stream_dropped_while_the_run_waits_on_it_releases_the_run() -> Result<(), Box<dyn Error>> {
    // Arrange
    let harness = Harness::new(BatchModel, unbounded_context_budget());
    let (mut stream, handle) = harness
        .run(RunRequest::new("batch").output(name_schema()?))
        .into_parts();
    let outcome = tokio::spawn(handle.outcome());
    tokio::time::sleep(Duration::from_millis(1)).await;
    for _ in 0..3 {
        next_event(&mut stream).await.ok_or("the run streams")?;
    }
    tokio::time::sleep(Duration::from_millis(1)).await;
    let refilled = !outcome.is_finished();

    // Act
    drop(stream);
    let outcome = timeout(HANG, outcome).await??;

    // Assert
    assert!(refilled, "the run refilled the stream and waits on it");
    assert_eq!(outcome.report.tool_calls().len(), BATCH_CALLS);
    assert_eq!(
        outcome.result.map_err(|error| error.kind()).err(),
        Some(ErrorKind::Repetition)
    );

    Ok(())
}

/// Payload bytes of each compacting turn's input, about a quarter of
/// [`COMPACTING_BUDGET`].
const COMPACTING_INPUT_BYTES: usize = 8_000;

/// Weight budget that holds a turn's input beside two finished turns of
/// history, but not three.
const COMPACTING_BUDGET: u64 = 8_000;

/// Answers checkpoint requests with a summary and turn requests with a
/// name.
struct CompactingModel;

#[async_trait]
impl Model for CompactingModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        let compaction = request
            .schema()
            .is_some_and(|schema| schema.value()["properties"].get("decisions").is_some());
        let response = if compaction {
            json!({"context": "earlier turns named Ada", "decisions": [], "state": "named"})
        } else {
            json!({"name": "Ada"})
        };

        Ok(completion(ModelResponse::Output(response), 5))
    }
}

fn compacting_input(number: usize) -> String {
    format!("turn-{number} {}", "x".repeat(COMPACTING_INPUT_BYTES))
}

#[tokio::test]
async fn auto_compaction_is_configurable_and_matchable_from_outside_the_crate()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let policy = ContextPolicy::auto_compact_at(90).ok_or("threshold in range")?;
    let harness = Harness::new(
        CompactingModel,
        ContextBudget::new(NonZeroU64::new(COMPACTING_BUDGET).ok_or("nonzero budget")?),
    )
    .store(Arc::new(MemoryStore::new()))
    .context_policy(policy);
    let mut session = harness
        .session("compacting", name_schema()?)
        .create()
        .await?;
    for number in 1..=3 {
        session.send(compacting_input(number)).await?;
    }

    // Act
    let (events, outcome) = streamed(session.run(RunRequest::new(compacting_input(4)))).await;

    // Assert
    assert!(outcome.result.is_ok());
    assert!(matches!(
        policy,
        ContextPolicy::AutoCompact {
            threshold_percent: 90,
            ..
        }
    ));
    let compacted = events.iter().find_map(|event| match &event.kind {
        RunEventKind::Compacted(report) => Some(&report.outcome),
        _ => None,
    });
    assert!(matches!(
        compacted,
        Some(CompactionOutcome::Compacted { covered_through, model: None, .. })
            if covered_through.get() == 1
    ));
    assert_eq!(last_summary(&events)?.usage, outcome.usage);

    Ok(())
}
