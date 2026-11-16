//! Per-run limits: each limit stops its run with its own kind, keeps the
//! usage and events of completed requests, and leaves the session usable.

use std::error::Error;
use std::future::poll_fn;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ag_harness::event::Item;
use ag_harness::model::{
    CompletionMetadata, CompletionUsage, ModelCompletion, ModelMessage, ModelRequest, ModelResponse,
};
use ag_harness::store::MemoryStore;
use ag_harness::tool::ToolCall;
use ag_harness::{
    ErrorKind, EventStream, Harness, LimitKind, Model, ModelError, OutputSchema, RunEvent,
    RunEventKind, RunLimits, RunOutcome, RunOutput, RunRequest, Session,
};
use async_trait::async_trait;
use futures_core::Stream;
use serde_json::json;
use tokio::time::{Instant, sleep};

use crate::context_budget_fixture::unbounded_context_budget;

/// Wall-clock time [`ScriptedModel`] takes per request for the prompt `slow`.
const SLOW_REQUEST: Duration = Duration::from_secs(10);

/// Answers by prompt, reporting 10 input and 5 output tokens per request:
/// `answer` with output at once; `once` with one `read` call and then output;
/// `batch` with two `read` calls and then output; `loop` with a new `read`
/// call every time; `slow` like `loop`, after [`SLOW_REQUEST`].
#[derive(Clone, Default)]
struct ScriptedModel {
    requests: Arc<AtomicUsize>,
}

impl ScriptedModel {
    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Model for ScriptedModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        let index = self.requests.fetch_add(1, Ordering::SeqCst);
        let answered = matches!(
            request.messages().last(),
            Some(ModelMessage::ToolResult { .. })
        );
        let response = match request.prompt() {
            "answer" => ModelResponse::Output(json!({"name": "Ada"})),
            "once" | "batch" if answered => ModelResponse::Output(json!({"name": "Ada"})),
            "batch" => ModelResponse::ToolCalls(vec![read_call(index, 0)?, read_call(index, 1)?]),
            "slow" => {
                sleep(SLOW_REQUEST).await;

                ModelResponse::ToolCall(read_call(index, 0)?)
            }
            _ => ModelResponse::ToolCall(read_call(index, 0)?),
        };
        let usage = CompletionUsage::new(None, None, Some(10), Some(5), None, None);

        Ok(ModelCompletion::new(
            CompletionMetadata::new("stop".into(), None, None, None, Some(usage)),
            response,
        ))
    }
}

/// Returns a distinct `read` call of request `request`.
fn read_call(request: usize, position: usize) -> Result<ToolCall, ModelError> {
    ToolCall::from_json(
        format!("call-{request}-{position}"),
        "read",
        &json!({"path": format!("{request}-{position}.txt")}).to_string(),
        None,
    )
}

fn name_schema() -> Result<OutputSchema, Box<dyn Error>> {
    Ok(OutputSchema::new(json!({
        "type": "object",
        "properties": {"name": {"type": "string"}},
        "required": ["name"]
    }))?)
}

async fn session(model: &ScriptedModel) -> Result<Session, Box<dyn Error>> {
    let harness =
        Harness::new(model.clone(), unbounded_context_budget()).store(Arc::new(MemoryStore::new()));

    Ok(harness.session("limits", name_schema()?).create().await?)
}

/// Runs `prompt` under `limits`, reading every event of the run.
async fn limited_run(
    session: &Session,
    prompt: &str,
    limits: RunLimits,
) -> (Vec<RunEvent>, RunOutcome) {
    let (stream, handle) = session
        .run(RunRequest::new(prompt).limits(limits))
        .into_parts();

    tokio::join!(drain(stream), handle.outcome())
}

async fn drain(mut stream: EventStream) -> Vec<RunEvent> {
    let mut events = Vec::new();
    while let Some(event) = poll_fn(|context| Pin::new(&mut stream).poll_next(context)).await {
        events.push(event);
    }

    events
}

/// Asserts that `outcome` and the last of `events` report a stop at `limit`
/// with the usage of `requests` completed requests, and returns how many
/// tool calls the events completed.
fn assert_stopped_at(
    limit: LimitKind,
    requests: u64,
    events: &[RunEvent],
    outcome: &RunOutcome,
) -> Result<usize, Box<dyn Error>> {
    let error = outcome.result.as_ref().err().ok_or("the run must fail")?;
    assert_eq!(error.kind(), ErrorKind::BudgetExhausted(limit));
    assert!(!error.is_retryable());
    assert_eq!(
        (outcome.usage.input, outcome.usage.output),
        (10 * requests, 5 * requests),
        "completed requests keep their usage"
    );
    let Some(RunEventKind::Completed(summary)) = events.last().map(|event| &event.kind) else {
        return Err("the last event must be `Completed`".into());
    };
    let failure = summary
        .error
        .as_ref()
        .ok_or("the summary must carry the failure")?;
    assert_eq!(failure.kind, ErrorKind::BudgetExhausted(limit));
    assert!(!failure.retryable);
    assert_eq!(summary.usage, outcome.usage);
    let streamed_requests = events
        .iter()
        .filter(|event| matches!(event.kind, RunEventKind::ModelRequestCompleted { .. }))
        .count();
    assert_eq!(u64::try_from(streamed_requests)?, requests);

    Ok(events
        .iter()
        .filter(|event| {
            matches!(
                &event.kind,
                RunEventKind::ItemCompleted {
                    item: Item::ToolCall(_),
                    ..
                }
            )
        })
        .count())
}

/// Asserts that `session` still answers a run without limits.
async fn assert_session_answers(session: &Session) -> Result<(), Box<dyn Error>> {
    let outcome = session.run(RunRequest::new("answer")).await;

    assert_eq!(outcome.result?, RunOutput::Json(json!({"name": "Ada"})));

    Ok(())
}

#[tokio::test]
async fn rounds_limit_stops_before_the_next_model_request() -> Result<(), Box<dyn Error>> {
    // Arrange
    let model = ScriptedModel::default();
    let session = session(&model).await?;

    // Act
    let (events, outcome) =
        limited_run(&session, "loop", RunLimits::default().with_max_rounds(2)).await;

    // Assert
    let tool_calls = assert_stopped_at(LimitKind::Rounds, 2, &events, &outcome)?;
    assert_eq!(model.requests(), 2);
    assert_eq!(
        tool_calls, 2,
        "the second response's call ran before the stop"
    );
    assert_session_answers(&session).await?;

    Ok(())
}

#[tokio::test]
async fn tool_call_limit_stops_before_a_batch_that_would_exceed_it() -> Result<(), Box<dyn Error>> {
    // Arrange
    let model = ScriptedModel::default();
    let session = session(&model).await?;

    // Act
    let (events, outcome) = limited_run(
        &session,
        "batch",
        RunLimits::default().with_max_tool_calls(1),
    )
    .await;

    // Assert
    let tool_calls = assert_stopped_at(LimitKind::ToolCalls, 1, &events, &outcome)?;
    assert_eq!(tool_calls, 0, "no call of the exceeding batch runs");
    assert_eq!(outcome.report.tool_calls().len(), 0);
    assert_session_answers(&session).await?;

    Ok(())
}

#[tokio::test]
async fn token_limit_stops_once_completed_requests_reach_it() -> Result<(), Box<dyn Error>> {
    // Arrange
    let model = ScriptedModel::default();
    let session = session(&model).await?;

    // Act
    let (events, outcome) = limited_run(
        &session,
        "loop",
        RunLimits::default().with_max_total_tokens(20),
    )
    .await;

    // Assert
    assert_stopped_at(LimitKind::Tokens, 2, &events, &outcome)?;
    assert_eq!(
        outcome.usage.input + outcome.usage.output,
        30,
        "the request that crossed the limit overshoots it"
    );
    assert_session_answers(&session).await?;

    Ok(())
}

#[tokio::test(start_paused = true)]
async fn wall_time_limit_stops_a_run_awaiting_its_model() -> Result<(), Box<dyn Error>> {
    // Arrange
    let model = ScriptedModel::default();
    let session = session(&model).await?;
    let started_at = Instant::now();

    // Act
    let (events, outcome) = limited_run(
        &session,
        "slow",
        RunLimits::default().with_max_wall_time(Duration::from_secs(15)),
    )
    .await;

    // Assert
    let elapsed = started_at.elapsed();
    assert!(
        elapsed >= Duration::from_secs(15) && elapsed < 2 * SLOW_REQUEST,
        "the run stopped at its limit, before the second request finished: {elapsed:?}"
    );
    assert_stopped_at(LimitKind::WallTime, 1, &events, &outcome)?;
    assert_eq!(
        model.requests(),
        2,
        "the second request was dropped in flight"
    );
    assert_session_answers(&session).await?;

    Ok(())
}

#[tokio::test]
async fn runs_within_their_limits_behave_like_unlimited_runs() -> Result<(), Box<dyn Error>> {
    // Arrange
    let model = ScriptedModel::default();
    let session = session(&model).await?;
    let exact = RunLimits::default()
        .with_max_rounds(2)
        .with_max_tool_calls(1)
        .with_max_total_tokens(30)
        .with_max_wall_time(Duration::from_secs(3600));

    // Act
    let (unlimited_events, unlimited) = limited_run(&session, "once", RunLimits::default()).await;
    let (limited_events, limited) = limited_run(&session, "once", exact).await;

    // Assert
    for outcome in [&unlimited, &limited] {
        assert_eq!(
            outcome.result.as_ref().ok(),
            Some(&RunOutput::Json(json!({"name": "Ada"})))
        );
        assert_eq!((outcome.usage.input, outcome.usage.output), (20, 10));
        assert_eq!(outcome.report.tool_calls().len(), 1);
    }
    assert_eq!(unlimited_events.len(), limited_events.len());
    assert_eq!(model.requests(), 4);

    Ok(())
}
