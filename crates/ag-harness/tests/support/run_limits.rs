//! Per-run limits: each limit stops its run with its own kind, keeps the
//! usage and events of completed requests, replays the stopped turn with its
//! stop note, and never carries over to the next run.

use std::error::Error;
use std::future::poll_fn;
use std::num::{NonZeroU32, NonZeroU64};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
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

/// Wall-clock time [`ScriptedModel`] takes per request for a `slow` prompt.
const SLOW_REQUEST: Duration = Duration::from_secs(10);

/// Assistant text sent with every tool-call response.
const CALL_TEXT: &str = "Reading next";

/// Answers by prompt, reporting 10 input and 5 output tokens per request:
/// `answer` with output at once; `batch` with two `read` calls and then
/// output; `loop` with a new `read` call every time; `invalid` with output
/// that violates the schema every time. A `slow ` prefix makes every
/// request take [`SLOW_REQUEST`].
#[derive(Clone, Default)]
struct ScriptedModel {
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}

impl ScriptedModel {
    fn requests(&self) -> Vec<ModelRequest> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl Model for ScriptedModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        let index = {
            let mut requests = self.requests.lock().unwrap_or_else(PoisonError::into_inner);
            requests.push(request.clone());

            requests.len() - 1
        };
        let prompt = match request.prompt().strip_prefix("slow ") {
            Some(prompt) => {
                sleep(SLOW_REQUEST).await;

                prompt
            }
            None => request.prompt(),
        };
        let answered = matches!(
            request.messages().last(),
            Some(ModelMessage::ToolResult { .. })
        );
        let response = match prompt {
            "answer" => ModelResponse::Output(json!({"name": "Ada"})),
            "batch" if answered => ModelResponse::Output(json!({"name": "Ada"})),
            "batch" => ModelResponse::ToolCalls(vec![read_call(index, 0)?, read_call(index, 1)?]),
            "invalid" => ModelResponse::Output(json!({"age": 1})),
            _ => ModelResponse::ToolCall(read_call(index, 0)?),
        };
        let usage = CompletionUsage::default()
            .with_input_tokens(Some(10))
            .with_output_tokens(Some(5));

        Ok(ModelCompletion::new(
            CompletionMetadata::new("stop").with_usage(Some(usage)),
            response,
        ))
    }
}

/// Returns a distinct `read` call of request `request`; the first call of a
/// response carries [`CALL_TEXT`].
fn read_call(request: usize, position: usize) -> Result<ToolCall, ModelError> {
    ToolCall::from_json(
        format!("call-{request}-{position}"),
        "read",
        &json!({"path": format!("{request}-{position}.txt")}).to_string(),
        None,
    )?
    .with_assistant_text((position == 0).then(|| CALL_TEXT.to_string()))
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

fn nonzero(value: u32) -> Result<NonZeroU32, Box<dyn Error>> {
    Ok(NonZeroU32::new(value).ok_or("limit must be nonzero")?)
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

/// Runs `follow_up`, which spends more than the stopped run's limits
/// allowed, asserts that it succeeds, and returns the messages its first
/// request replays before the stopped turn's note naming `error_type`.
async fn next_run_replay(
    session: &Session,
    model: &ScriptedModel,
    follow_up: &str,
    error_type: &str,
) -> Result<Vec<ModelMessage>, Box<dyn Error>> {
    let sent_before = model.requests().len();

    let outcome = session.run(RunRequest::new(follow_up)).await;

    assert_eq!(outcome.result?, RunOutput::Json(json!({"name": "Ada"})));
    let requests = model.requests();
    let messages = requests
        .get(sent_before)
        .ok_or("the follow-up run must send a request")?
        .messages();
    let stop_note = format!("[harness] The turn above failed ({error_type}) before completing");
    let note_at = messages
        .iter()
        .position(|message| {
            matches!(
                message,
                ModelMessage::User(note) if note.starts_with(&stop_note)
            )
        })
        .ok_or("the stopped turn must replay with its stop note")?;

    Ok(messages[..note_at].to_vec())
}

/// Asserts that `replay` ends with `call` and the result it got when it ran.
fn assert_ends_with_answered_call(replay: &[ModelMessage], call: &ToolCall) {
    let tail = &replay[replay.len().saturating_sub(2)..];

    assert!(
        matches!(
            tail,
            [ModelMessage::AssistantToolCall(replayed), ModelMessage::ToolResult { call_id, .. }]
                if replayed == call && call_id == call.id()
        ),
        "the replay must end with the answered call: {replay:?}"
    );
}

/// Returns the placeholder a stopped turn replays for a call without a
/// result.
fn placeholder(call_id: &str) -> ModelMessage {
    ModelMessage::ToolResult {
        call_id: call_id.to_string(),
        content: r#"{"error":"no result was recorded before the turn stopped"}"#.to_string(),
        name: "read".to_string(),
    }
}

#[tokio::test]
async fn model_request_limit_stops_before_the_next_model_request() -> Result<(), Box<dyn Error>> {
    // Arrange
    let model = ScriptedModel::default();
    let session = session(&model).await?;
    let limits = RunLimits::default().with_max_model_requests(nonzero(1)?);

    // Act
    let (events, outcome) = limited_run(&session, "loop", limits).await;

    // Assert
    let tool_calls = assert_stopped_at(LimitKind::ModelRequests, 1, &events, &outcome)?;
    assert_eq!(model.requests().len(), 1);
    assert_eq!(tool_calls, 1, "the response's call ran before the stop");
    let replay = next_run_replay(&session, &model, "batch", "RunLimit(ModelRequests)").await?;
    assert_ends_with_answered_call(&replay, &read_call(0, 0)?);

    Ok(())
}

#[tokio::test]
async fn re_sent_requests_count_toward_the_model_request_limit() -> Result<(), Box<dyn Error>> {
    // Arrange
    let model = ScriptedModel::default();
    let session = session(&model).await?;
    let limits = RunLimits::default().with_max_model_requests(nonzero(1)?);

    // Act
    let (events, outcome) = limited_run(&session, "invalid", limits).await;

    // Assert
    let tool_calls = assert_stopped_at(LimitKind::ModelRequests, 1, &events, &outcome)?;
    assert_eq!(tool_calls, 0);
    assert_eq!(
        model.requests().len(),
        1,
        "the correction request is refused"
    );

    Ok(())
}

#[tokio::test]
async fn tool_call_limit_stops_before_a_batch_that_would_exceed_it() -> Result<(), Box<dyn Error>> {
    // Arrange
    let model = ScriptedModel::default();
    let session = session(&model).await?;
    let limits = RunLimits::default().with_max_tool_calls(nonzero(1)?);

    // Act
    let (events, outcome) = limited_run(&session, "batch", limits).await;

    // Assert
    let tool_calls = assert_stopped_at(LimitKind::ToolCalls, 1, &events, &outcome)?;
    assert_eq!(tool_calls, 0, "no call of the exceeding batch runs");
    assert_eq!(outcome.report.tool_calls().len(), 0);
    assert!(
        events.iter().any(|event| matches!(
            &event.kind,
            RunEventKind::ItemCompleted { item: Item::Message { text }, .. } if text == CALL_TEXT
        )),
        "the refused response's assistant text is still streamed"
    );
    let replay = next_run_replay(&session, &model, "batch", "RunLimit(ToolCalls)").await?;
    assert_eq!(
        replay[replay.len().saturating_sub(3)..],
        [
            ModelMessage::AssistantToolCalls(vec![read_call(0, 0)?, read_call(0, 1)?]),
            placeholder("call-0-0"),
            placeholder("call-0-1"),
        ],
        "the refused batch replays as never run"
    );

    Ok(())
}

#[tokio::test]
async fn token_limit_stops_once_completed_requests_reach_it() -> Result<(), Box<dyn Error>> {
    // Arrange
    let model = ScriptedModel::default();
    let session = session(&model).await?;
    let limits = RunLimits::default()
        .with_max_total_tokens(NonZeroU64::new(20).ok_or("limit must be nonzero")?);

    // Act
    let (events, outcome) = limited_run(&session, "loop", limits).await;

    // Assert
    assert_stopped_at(LimitKind::Tokens, 2, &events, &outcome)?;
    assert_eq!(
        outcome.usage.input + outcome.usage.output,
        30,
        "the request that crossed the limit overshoots it"
    );
    assert_eq!(model.requests().len(), 2);
    let replay = next_run_replay(&session, &model, "batch", "RunLimit(Tokens)").await?;
    assert_ends_with_answered_call(&replay, &read_call(1, 0)?);

    Ok(())
}

#[tokio::test(start_paused = true)]
async fn wall_time_limit_stops_a_run_awaiting_its_model() -> Result<(), Box<dyn Error>> {
    // Arrange
    let model = ScriptedModel::default();
    let session = session(&model).await?;
    let started_at = Instant::now();
    let limits = RunLimits::default().with_max_wall_time(Duration::from_secs(15));

    // Act
    let (events, outcome) = limited_run(&session, "slow loop", limits).await;

    // Assert
    let elapsed = started_at.elapsed();
    assert!(
        elapsed >= Duration::from_secs(15) && elapsed < 2 * SLOW_REQUEST,
        "the run stopped at its limit, before the second request finished: {elapsed:?}"
    );
    assert_stopped_at(LimitKind::WallTime, 1, &events, &outcome)?;
    assert_eq!(
        model.requests().len(),
        2,
        "the second request was dropped in flight"
    );
    let follow_up_started = Instant::now();
    let replay = next_run_replay(&session, &model, "slow batch", "RunLimit(WallTime)").await?;
    assert!(
        follow_up_started.elapsed() > Duration::from_secs(15),
        "the follow-up outlasts the stopped run's limit"
    );
    assert_ends_with_answered_call(&replay, &read_call(0, 0)?);

    Ok(())
}

#[tokio::test]
async fn zero_wall_time_is_rejected_before_the_run_starts() -> Result<(), Box<dyn Error>> {
    // Arrange
    let model = ScriptedModel::default();
    let session = session(&model).await?;
    let limits = RunLimits::default().with_max_wall_time(Duration::ZERO);

    // Act
    let (events, outcome) = limited_run(&session, "answer", limits).await;

    // Assert
    let error = outcome.result.as_ref().err().ok_or("the run must fail")?;
    assert_eq!(error.kind(), ErrorKind::Config);
    assert_eq!(model.requests(), Vec::<ModelRequest>::new());
    assert!(matches!(
        events.last().map(|event| &event.kind),
        Some(RunEventKind::Completed(_))
    ));

    Ok(())
}

#[tokio::test]
async fn runs_within_their_limits_behave_like_unlimited_runs() -> Result<(), Box<dyn Error>> {
    // Arrange
    let unlimited_model = ScriptedModel::default();
    let limited_model = ScriptedModel::default();
    let unlimited_session = session(&unlimited_model).await?;
    let limited_session = session(&limited_model).await?;
    let exact = RunLimits::default()
        .with_max_model_requests(nonzero(2)?)
        .with_max_tool_calls(nonzero(2)?)
        .with_max_total_tokens(NonZeroU64::new(30).ok_or("limit must be nonzero")?)
        .with_max_wall_time(Duration::from_secs(3600));

    // Act
    let (unlimited_events, unlimited) =
        limited_run(&unlimited_session, "batch", RunLimits::default()).await;
    let (limited_events, limited) = limited_run(&limited_session, "batch", exact).await;

    // Assert
    for outcome in [&unlimited, &limited] {
        assert_eq!(
            outcome.result.as_ref().ok(),
            Some(&RunOutput::Json(json!({"name": "Ada"})))
        );
        assert_eq!((outcome.usage.input, outcome.usage.output), (20, 10));
    }
    let kinds = |events: &[RunEvent]| -> Vec<RunEventKind> {
        events.iter().map(|event| event.kind.clone()).collect()
    };
    assert_eq!(kinds(&limited_events), kinds(&unlimited_events));
    assert_eq!(
        limited.report.tool_calls().len(),
        unlimited.report.tool_calls().len()
    );
    assert_eq!(limited_model.requests(), unlimited_model.requests());

    Ok(())
}
