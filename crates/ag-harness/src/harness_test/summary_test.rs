//! Harness context summary unit tests for summary requests, corrections,
//! fallbacks, cancellation, and session history.

use std::collections::VecDeque;
use std::io::Cursor;
use std::num::{NonZeroU32, NonZeroU64};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_core::Stream;
use serde_json::{Value, json};

use super::support::object_schema;
use crate::budget::{BudgetLedger, Estimate, Reservation};
use crate::compaction::summary_schema;
use crate::context::{
    ContextBudget, ContextEstimator, ContextSummaryPolicy, HeuristicContextEstimator,
};
use crate::error::{ErrorKind, RunError};
use crate::event::RunEventKind;
use crate::file_system::MockFileSystem;
use crate::harness::Harness;
use crate::lifecycle::{
    ContextSummaryErrorType, ContextSummaryTrigger, LifecycleEvent, LifecycleEventKind,
};
use crate::limit::{LimitKind, RunLimits};
use crate::model::{
    Model, ModelCompletion, ModelError, ModelMessage, ModelRequest, ModelResponse, ReasoningEffort,
};
use crate::repository::Repository;
use crate::request_option::OutputMode;
use crate::round_summary::{CONTINUED_SUMMARY_PREFIX, ROUND_SUMMARY_INSTRUCTIONS};
use crate::run::{RunOutcome, RunRequest, Usage};
use crate::store::MemoryStore;
use crate::tool::{Tool, ToolCall};
use crate::turn::{ModelRequestPurpose, TurnOutcome};
use crate::usage::Cost;

/// Weight budget that holds about five 8 KB file reads.
const BUDGET: u64 = 12_000;

/// Weight budget whose uncalibrated share leaves room for the summary
/// instruction beside the last request sent before a summary, so the
/// summary continues that request.
const ROOMY_BUDGET: u64 = 14_000;

/// Rounds each turn reads, one distinct file per round, before answering.
const READS: usize = 10;

/// Answers turn requests by reading `f<n>.txt` for [`READS`] rounds and then
/// answering, and summary requests with the scripted responses, in order,
/// then with a valid summary.
struct SummarizingModel {
    keeps_prefix: bool,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
    summaries: Mutex<VecDeque<Result<ModelResponse, ModelError>>>,
}

impl SummarizingModel {
    fn new(
        summaries: Vec<Result<ModelResponse, ModelError>>,
    ) -> (Self, Arc<Mutex<Vec<ModelRequest>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let model = Self {
            keeps_prefix: false,
            requests: Arc::clone(&requests),
            summaries: Mutex::new(summaries.into()),
        };

        (model, requests)
    }

    /// Returns a model that keeps its cached prefix across output schemas,
    /// so the harness continues turn requests to ask for summaries.
    fn keeping_prefix(
        summaries: Vec<Result<ModelResponse, ModelError>>,
    ) -> (Self, Arc<Mutex<Vec<ModelRequest>>>) {
        let (mut model, requests) = Self::new(summaries);
        model.keeps_prefix = true;

        (model, requests)
    }
}

#[async_trait]
impl Model for SummarizingModel {
    fn keeps_prefix_across_schemas(&self) -> bool {
        self.keeps_prefix
    }

    fn supports_text_output(&self) -> bool {
        true
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        let mut requests = self.requests.lock().expect("requests lock");
        requests.push(request.clone());
        if is_summary_request(&request) {
            drop(requests);
            let scripted = self.summaries.lock().expect("summaries lock").pop_front();

            return scripted
                .unwrap_or_else(|| Ok(ModelResponse::Output(valid_summary())))
                .map(ModelCompletion::from_response);
        }
        let turn_requests = requests
            .iter()
            .filter(|request| !is_summary_request(request))
            .count();
        let reads_done = (turn_requests - 1) % (READS + 1);
        let response = if reads_done < READS {
            let arguments = serde_json::from_value(json!({"path": format!("f{reads_done}.txt")}))
                .expect("read arguments");
            ModelResponse::ToolCall(ToolCall::read(
                format!("read_{turn_requests}"),
                arguments,
                None,
            ))
        } else if request.output() == &OutputMode::Text {
            ModelResponse::Text("read everything".to_string())
        } else {
            ModelResponse::Output(json!({"summary": "read everything"}))
        };

        Ok(ModelCompletion::from_response(response))
    }
}

fn is_summary_request(request: &ModelRequest) -> bool {
    request.messages().first()
        == Some(&ModelMessage::System(
            ROUND_SUMMARY_INSTRUCTIONS.to_string(),
        ))
        || request.prefix_schema().is_some()
}

fn valid_summary() -> Value {
    json!({
        "context": "f0.txt holds eighty-byte lines; marker cobalt-41",
        "decisions": ["read each file once"],
        "state": "earlier files read"
    })
}

fn files() -> MockFileSystem {
    let content = format!("{}\n", "x".repeat(79)).repeat(100).into_bytes();
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_canonicalize()
        .returning(|path| Ok(path.to_path_buf()));
    file_system
        .expect_open_beneath()
        .returning(move |_, _| Ok(Box::new(Cursor::new(content.clone()))));

    file_system
}

fn harness(model: SummarizingModel, policy: ContextSummaryPolicy) -> Harness {
    budgeted_harness(model, policy, BUDGET)
}

fn budgeted_harness(model: SummarizingModel, policy: ContextSummaryPolicy, budget: u64) -> Harness {
    Harness::new(
        model,
        ContextBudget::new(NonZeroU64::new(budget).expect("nonzero budget")),
    )
    .repository(Repository::fixture("repo"))
    .allow(Tool::Read)
    .file_system(files())
    .context_summaries(policy)
}

fn observed(harness: Harness) -> (Harness, Arc<Mutex<Vec<LifecycleEvent>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&events);
    let harness = harness.with_lifecycle_observer(move |event: LifecycleEvent| {
        recorded.lock().expect("events lock").push(event);
    });

    (harness, events)
}

async fn read_files(harness: &Harness) -> TurnOutcome {
    harness
        .run_once("read every file", object_schema())
        .await
        .expect("turn succeeds")
}

fn summary_requests(requests: &Mutex<Vec<ModelRequest>>) -> Vec<ModelRequest> {
    requests
        .lock()
        .expect("requests lock")
        .iter()
        .filter(|request| is_summary_request(request))
        .cloned()
        .collect()
}

fn last_turn_request(requests: &Mutex<Vec<ModelRequest>>) -> ModelRequest {
    requests
        .lock()
        .expect("requests lock")
        .iter()
        .rev()
        .find(|request| !is_summary_request(request))
        .cloned()
        .expect("turn request")
}

fn carries(request: &ModelRequest, text: &str) -> bool {
    request.messages().iter().any(|message| match message {
        ModelMessage::User(content) => content.contains(text),
        _ => false,
    })
}

fn summary_events(events: &Mutex<Vec<LifecycleEvent>>) -> Vec<LifecycleEventKind> {
    events
        .lock()
        .expect("events lock")
        .iter()
        .map(|event| event.kind().clone())
        .filter(|kind| {
            matches!(
                kind,
                LifecycleEventKind::ContextSummaryStarted { .. }
                    | LifecycleEventKind::ContextSummaryCompleted { .. }
                    | LifecycleEventKind::ContextSummaryFailed { .. }
            )
        })
        .collect()
}

#[tokio::test]
async fn turns_send_earlier_rounds_as_a_summary_once_they_outgrow_the_budget() {
    // Arrange
    let (model, requests) = SummarizingModel::new(Vec::new());
    let (harness, events) = observed(
        harness(model, ContextSummaryPolicy::Enabled).model_reasoning_effort(ReasoningEffort::High),
    );

    // Act
    let outcome = read_files(&harness).await;

    // Assert
    let report = outcome.report();
    let summaries = summary_requests(&requests);
    assert_ne!(summaries, Vec::<ModelRequest>::new());
    assert_eq!(summaries[0].tools(), []);
    assert_eq!(
        summaries[0].model_reasoning_effort(),
        Some(ReasoningEffort::Low)
    );
    assert!(summaries[0].prompt().contains("read every file"));
    assert!(carries(&last_turn_request(&requests), "cobalt-41"));
    assert_eq!(
        report
            .model_requests()
            .iter()
            .filter(|request| request.purpose() == ModelRequestPurpose::ContextSummary)
            .count(),
        summaries.len()
    );
    let last = report.model_requests().last().expect("final request");
    assert_eq!(last.purpose(), ModelRequestPurpose::Turn);
    assert!(last.context().summarized_tool_calls() > 0);
    assert!(last.estimated_weight() < last.unreduced_weight());
    assert!(last.estimated_weight() <= last.budget_weight());
    assert!(
        report
            .model_requests()
            .iter()
            .filter(|request| request.purpose() == ModelRequestPurpose::ContextSummary)
            .all(|request| request.budget_weight() == 0 && !request.rejected_output())
    );
    assert_eq!(report.context_summary_failures(), 0);
    let events = summary_events(&events);
    assert!(matches!(
        events[0],
        LifecycleEventKind::ContextSummaryStarted {
            trigger: ContextSummaryTrigger::Proactive,
            turn_id: Some(_),
            ..
        }
    ));
    assert!(matches!(
        events[1],
        LifecycleEventKind::ContextSummaryCompleted {
            summarized_tool_calls,
            ..
        } if summarized_tool_calls > 0
    ));
}

#[tokio::test]
async fn summaries_continue_the_last_turn_request_for_a_model_that_keeps_its_prefix() {
    // Arrange
    let (model, requests) = SummarizingModel::keeping_prefix(Vec::new());
    let harness = budgeted_harness(model, ContextSummaryPolicy::Enabled, ROOMY_BUDGET)
        .model_reasoning_effort(ReasoningEffort::High);

    // Act
    let outcome = read_files(&harness).await;

    // Assert
    let recorded = requests.lock().expect("requests lock").clone();
    let index = recorded
        .iter()
        .position(is_summary_request)
        .expect("a summary request");
    let (summary, previous) = (&recorded[index], &recorded[index - 1]);
    let (instruction, continued) = summary.messages().split_last().expect("instruction");
    assert_eq!(continued, previous.messages());
    assert!(matches!(
        instruction,
        ModelMessage::User(text)
            if text.starts_with(CONTINUED_SUMMARY_PREFIX)
                && text.contains("call `read_1`")
                && text.contains(&summary_schema().expect("summary schema").value().to_string())
    ));
    assert_eq!(summary.tools(), previous.tools());
    assert_eq!(
        summary.model_reasoning_effort(),
        Some(ReasoningEffort::High)
    );
    assert_eq!(summary.prefix_schema(), previous.schema());
    assert_eq!(
        summary.schema(),
        Some(&summary_schema().expect("summary schema"))
    );
    assert!(carries(&last_turn_request(&requests), "cobalt-41"));
    assert_eq!(outcome.report().context_summary_failures(), 0);
    // The estimate that calibrates later requests weighs the turn's tools.
    let estimator = HeuristicContextEstimator::DEFAULT;
    let weight = summary
        .messages()
        .iter()
        .map(|message| estimator.message_weight(message))
        .chain(
            summary
                .tools()
                .iter()
                .map(|tool| estimator.tool_definition_weight(tool)),
        )
        .sum::<u64>();
    let reported = outcome
        .report()
        .model_requests()
        .iter()
        .find(|request| request.purpose() == ModelRequestPurpose::ContextSummary)
        .expect("a reported summary request");
    assert_eq!(reported.estimated_weight(), weight);
}

#[tokio::test]
async fn text_turns_request_summaries_of_their_own_for_a_model_that_keeps_its_prefix() {
    // Arrange
    let (model, requests) = SummarizingModel::keeping_prefix(Vec::new());
    let harness = budgeted_harness(model, ContextSummaryPolicy::Enabled, ROOMY_BUDGET);

    // Act
    let outcome = harness
        .run_once("read every file", OutputMode::Text)
        .await
        .expect("turn succeeds");

    // Assert
    assert_eq!(outcome.output(), &json!("read everything"));
    let summaries = summary_requests(&requests);
    assert_ne!(summaries, Vec::<ModelRequest>::new());
    for summary in summaries {
        assert_eq!(summary.prefix_schema(), None);
        assert_eq!(
            summary.messages().first(),
            Some(&ModelMessage::System(
                ROUND_SUMMARY_INSTRUCTIONS.to_string()
            ))
        );
        assert_eq!(summary.tools(), []);
    }
    assert!(carries(&last_turn_request(&requests), "cobalt-41"));
}

#[tokio::test]
async fn tool_calls_answering_a_continued_summary_request_are_corrected() {
    // Arrange
    let tool_call = || {
        Ok(ModelResponse::ToolCall(ToolCall::read(
            "summary_read".to_string(),
            serde_json::from_value(json!({"path": "f0.txt"})).expect("read arguments"),
            None,
        )))
    };
    let (corrected, corrected_requests) = SummarizingModel::keeping_prefix(vec![tool_call()]);
    let (exhausted, _) =
        SummarizingModel::keeping_prefix(vec![tool_call(), tool_call(), tool_call()]);
    let (exhausted, events) = observed(budgeted_harness(
        exhausted,
        ContextSummaryPolicy::Enabled,
        ROOMY_BUDGET,
    ));

    // Act
    let corrected = read_files(&budgeted_harness(
        corrected,
        ContextSummaryPolicy::Enabled,
        ROOMY_BUDGET,
    ))
    .await;
    let exhausted = read_files(&exhausted).await;

    // Assert
    let summaries = summary_requests(&corrected_requests);
    assert!(carries(
        &summaries[1],
        "[harness] Those tool calls were not run"
    ));
    assert!(carries(
        &last_turn_request(&corrected_requests),
        "cobalt-41"
    ));
    assert_eq!(corrected.report().context_summary_failures(), 0);
    assert!(exhausted.report().context_summary_failures() >= 1);
    assert!(summary_events(&events).iter().any(|event| matches!(
        event,
        LifecycleEventKind::ContextSummaryFailed {
            error_type: ContextSummaryErrorType::InvalidOutput,
            ..
        }
    )));
}

#[tokio::test]
async fn disabled_summaries_never_request_one() {
    // Arrange
    let (model, requests) = SummarizingModel::new(Vec::new());
    let harness = harness(model, ContextSummaryPolicy::Disabled);

    // Act
    let outcome = read_files(&harness).await;

    // Assert
    assert_eq!(summary_requests(&requests), Vec::<ModelRequest>::new());
    let last = outcome
        .report()
        .model_requests()
        .last()
        .expect("final request");
    assert_eq!(last.context().summarized_tool_calls(), 0);
    assert!(last.context().dropped_tool_calls() + last.context().elided_results() > 0);
}

#[tokio::test]
async fn invalid_summaries_are_corrected_like_final_outputs() {
    // Arrange
    let invalid = || Ok(ModelResponse::Output(json!({"context": "missing fields"})));
    let (model, requests) = SummarizingModel::new(vec![invalid(), invalid()]);
    let harness = harness(model, ContextSummaryPolicy::Enabled);

    // Act
    let outcome = read_files(&harness).await;

    // Assert
    let summaries = summary_requests(&requests);
    assert!(summaries.len() >= 3);
    assert!(carries(
        &summaries[1],
        "[harness] Your previous final response was rejected"
    ));
    assert!(carries(&last_turn_request(&requests), "cobalt-41"));
    assert_eq!(outcome.report().context_summary_failures(), 0);
}

#[tokio::test]
async fn summaries_still_invalid_after_corrections_fail_as_invalid_output() {
    // Arrange
    let invalid = || Ok(ModelResponse::Output(json!({"context": "missing fields"})));
    let (model, requests) = SummarizingModel::new(vec![invalid(), invalid(), invalid()]);
    let (harness, events) = observed(harness(model, ContextSummaryPolicy::Enabled));

    // Act
    let outcome = read_files(&harness).await;

    // Assert
    assert!(summary_requests(&requests).len() >= 3);
    assert!(outcome.report().context_summary_failures() >= 1);
    assert!(matches!(
        summary_events(&events)
            .into_iter()
            .find(|event| matches!(event, LifecycleEventKind::ContextSummaryFailed { .. })),
        Some(LifecycleEventKind::ContextSummaryFailed {
            error_type: ContextSummaryErrorType::InvalidOutput,
            ..
        })
    ));
}

#[tokio::test]
async fn failed_summaries_fall_back_and_stop_after_the_limit() {
    // Arrange
    let failures = (0..4).map(|_| Err(ModelError::InvalidResponse)).collect();
    let (model, requests) = SummarizingModel::new(failures);
    let (harness, events) = observed(harness(model, ContextSummaryPolicy::Enabled));

    // Act
    let outcome = read_files(&harness).await;

    // Assert
    assert_eq!(summary_requests(&requests).len(), 2);
    let report = outcome.report();
    assert_eq!(report.context_summary_failures(), 2);
    assert!(
        report
            .model_requests()
            .iter()
            .all(|request| request.purpose() == ModelRequestPurpose::Turn)
    );
    assert!(!carries(&last_turn_request(&requests), "cobalt-41"));
    assert!(summary_events(&events).iter().any(|event| matches!(
        event,
        LifecycleEventKind::ContextSummaryFailed {
            error_type: ContextSummaryErrorType::Model,
            ..
        }
    )));
}

#[tokio::test]
async fn summaries_that_do_not_fit_or_answer_with_a_tool_call_are_rejected() {
    // Arrange
    let oversized = json!({
        "context": "c".repeat(4000),
        "decisions": vec!["d".repeat(400); 20],
        "state": "s".repeat(2000)
    });
    let tool_call = ToolCall::read(
        "summary_read".to_string(),
        serde_json::from_value(json!({"path": "f0.txt"})).expect("read arguments"),
        None,
    );
    let (model, requests) = SummarizingModel::new(vec![
        Ok(ModelResponse::Output(oversized)),
        Ok(ModelResponse::ToolCall(tool_call)),
    ]);
    let (harness, events) = observed(harness(model, ContextSummaryPolicy::Enabled));

    // Act
    let outcome = read_files(&harness).await;

    // Assert
    assert_eq!(summary_requests(&requests).len(), 2);
    let report = outcome.report();
    assert_eq!(report.context_summary_failures(), 2);
    assert_eq!(
        report
            .model_requests()
            .iter()
            .filter(|request| request.purpose() == ModelRequestPurpose::ContextSummary)
            .count(),
        2
    );
    let failures: Vec<ContextSummaryErrorType> = summary_events(&events)
        .into_iter()
        .filter_map(|event| match event {
            LifecycleEventKind::ContextSummaryFailed { error_type, .. } => Some(error_type),
            _ => None,
        })
        .collect();
    assert_eq!(
        failures,
        [
            ContextSummaryErrorType::Rejected,
            ContextSummaryErrorType::InvalidOutput
        ]
    );
}

/// Answers summary requests only after an hour, so the turn is cancelled
/// while a summary is in flight.
struct StalledSummaryModel {
    inner: SummarizingModel,
}

#[async_trait]
impl Model for StalledSummaryModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        if is_summary_request(&request) {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }

        self.inner.complete(request).await
    }
}

#[tokio::test]
async fn cancelling_a_turn_during_a_summary_reports_it_cancelled() {
    // Arrange
    let (inner, _requests) = SummarizingModel::new(Vec::new());
    let harness = Harness::new(
        StalledSummaryModel { inner },
        ContextBudget::new(NonZeroU64::new(BUDGET).expect("nonzero budget")),
    )
    .repository(Repository::fixture("repo"))
    .allow(Tool::Read)
    .file_system(files())
    .context_summaries(ContextSummaryPolicy::Enabled);
    let (harness, events) = observed(harness);

    // Act
    let result = tokio::time::timeout(
        Duration::from_millis(500),
        harness.run_once("read every file", object_schema()),
    )
    .await;

    // Assert
    assert!(result.is_err());
    assert!(matches!(
        summary_events(&events).last(),
        Some(LifecycleEventKind::ContextSummaryFailed {
            error_type: ContextSummaryErrorType::Cancelled,
            ..
        })
    ));
}

#[tokio::test]
async fn session_history_keeps_the_rounds_a_summary_replaced() {
    // Arrange
    let (model, requests) = SummarizingModel::new(Vec::new());
    let harness = harness(model, ContextSummaryPolicy::Enabled).store(Arc::new(MemoryStore::new()));
    let mut session = harness
        .session("summarized", object_schema())
        .create()
        .await
        .expect("session created");

    // Act
    session
        .send("read every file")
        .await
        .expect("first turn succeeds");
    let first_turn_requests = requests.lock().expect("requests lock").len();
    session
        .send("read every file again")
        .await
        .expect("second turn succeeds");

    // Assert
    let second_turn_first_request =
        requests.lock().expect("requests lock")[first_turn_requests].clone();
    assert!(!is_summary_request(&second_turn_first_request));
    assert!(!carries(&second_turn_first_request, "cobalt-41"));
    assert!(
        second_turn_first_request
            .messages()
            .iter()
            .any(|message| matches!(message, ModelMessage::AssistantToolCall(_)))
    );
}

#[tokio::test]
async fn summaries_still_invalid_after_the_correction_limit_fail() {
    // Arrange
    let invalid = || Ok(ModelResponse::Output(json!({"context": "missing fields"})));
    let (model, requests) = SummarizingModel::new(vec![invalid(), invalid(), invalid()]);
    let harness = harness(model, ContextSummaryPolicy::Enabled);

    // Act
    let outcome = read_files(&harness).await;

    // Assert
    let summaries = summary_requests(&requests);
    assert!(carries(
        &summaries[2],
        "[harness] Your previous final response was rejected"
    ));
    assert!(!carries(
        &summaries[3],
        "[harness] Your previous final response was rejected"
    ));
    assert_eq!(outcome.report().context_summary_failures(), 1);
}

#[tokio::test]
async fn streamed_runs_report_summary_requests_and_reduced_requests() {
    // Arrange
    let (model, requests) = SummarizingModel::new(Vec::new());
    let harness = harness(model, ContextSummaryPolicy::Enabled);
    let (mut stream, handle) = harness
        .run(RunRequest::new("read every file").output(object_schema()))
        .into_parts();
    let drain = async {
        let mut events = Vec::new();
        while let Some(event) =
            std::future::poll_fn(|context| Pin::new(&mut stream).poll_next(context)).await
        {
            events.push(event.kind);
        }

        events
    };

    // Act
    let (events, outcome) = tokio::join!(drain, handle.outcome());

    // Assert
    let requests_sent = requests.lock().expect("requests lock").len();
    let requests_streamed = events
        .iter()
        .filter(|kind| matches!(kind, RunEventKind::ModelRequestCompleted { .. }))
        .count();
    assert_ne!(summary_requests(&requests), Vec::<ModelRequest>::new());
    assert_eq!(requests_streamed, requests_sent);
    assert_eq!(requests_streamed, outcome.report.model_requests().len());
    assert!(events.iter().any(|kind| matches!(
        kind,
        RunEventKind::ContextReduced(activity) if activity.summarized_tool_calls() > 0
    )));
    assert!(matches!(
        events.last(),
        Some(RunEventKind::Completed(summary)) if summary.error.is_none()
    ));
}

/// Runs "read every file" on `harness` under `limits`, returning its events
/// and outcome.
async fn limited_run(harness: &Harness, limits: RunLimits) -> (Vec<RunEventKind>, RunOutcome) {
    let (mut stream, handle) = harness
        .run(
            RunRequest::new("read every file")
                .output(object_schema())
                .limits(limits),
        )
        .into_parts();
    let drain = async {
        let mut events = Vec::new();
        while let Some(event) =
            std::future::poll_fn(|context| Pin::new(&mut stream).poll_next(context)).await
        {
            events.push(event.kind);
        }

        events
    };

    tokio::join!(drain, handle.outcome())
}

#[tokio::test]
async fn summary_requests_count_toward_the_model_request_limit() {
    // Arrange
    let (model, requests) = SummarizingModel::new(Vec::new());
    let (_, unlimited) = limited_run(
        &harness(model, ContextSummaryPolicy::Enabled),
        RunLimits::default(),
    )
    .await;
    let sent = requests.lock().expect("requests lock").len();
    let turn_requests = READS + 1;
    let limit = u32::try_from(sent - 1).expect("request count fits");
    let (model, requests) = SummarizingModel::new(Vec::new());
    let harness = harness(model, ContextSummaryPolicy::Enabled);

    // Act
    let (_, limited) = limited_run(
        &harness,
        RunLimits::default()
            .with_max_model_requests(NonZeroU32::new(limit).expect("nonzero limit")),
    )
    .await;

    // Assert
    assert!(unlimited.result.is_ok());
    assert!(
        sent > turn_requests,
        "the unlimited turn summarized at least once"
    );
    assert_eq!(
        limited.result.err().map(|error| error.kind()),
        Some(ErrorKind::BudgetExhausted(LimitKind::ModelRequests)),
        "a limit above the turn's own requests still stops it"
    );
    assert_eq!(requests.lock().expect("requests lock").len(), sent - 1);
}

/// Answers summary requests that carry correction feedback only after an
/// hour, so a wall-clock limit stops the turn during a re-requested summary.
struct StalledCorrectionModel {
    inner: SummarizingModel,
}

#[async_trait]
impl Model for StalledCorrectionModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        if is_summary_request(&request)
            && carries(
                &request,
                "[harness] Your previous final response was rejected",
            )
        {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }

        self.inner.complete(request).await
    }
}

#[tokio::test(start_paused = true)]
async fn wall_time_stop_during_a_summary_still_streams_its_earlier_requests() {
    // Arrange
    let invalid = Ok(ModelResponse::Output(json!({"context": "missing fields"})));
    let (inner, _requests) = SummarizingModel::new(vec![invalid]);
    let harness = Harness::new(
        StalledCorrectionModel { inner },
        ContextBudget::new(NonZeroU64::new(BUDGET).expect("nonzero budget")),
    )
    .repository(Repository::fixture("repo"))
    .allow(Tool::Read)
    .file_system(files())
    .context_summaries(ContextSummaryPolicy::Enabled);

    // Act
    let (events, outcome) = limited_run(
        &harness,
        RunLimits::default().with_max_wall_time(Duration::from_secs(60)),
    )
    .await;

    // Assert
    let requests_streamed = events
        .iter()
        .filter(|kind| matches!(kind, RunEventKind::ModelRequestCompleted { .. }))
        .count();
    assert_eq!(
        outcome.result.as_ref().err().map(RunError::kind),
        Some(ErrorKind::BudgetExhausted(LimitKind::WallTime))
    );
    assert_eq!(requests_streamed, outcome.report.model_requests().len());
    assert!(
        outcome
            .report
            .model_requests()
            .iter()
            .any(|request| request.purpose() == ModelRequestPurpose::ContextSummary),
        "the rejected summary request is part of the outcome"
    );
}

/// Ledger that admits a fixed number of model requests and refuses every
/// later one at its cost cap.
struct CountingLedger {
    remaining: Mutex<usize>,
}

#[async_trait]
impl BudgetLedger for CountingLedger {
    async fn reserve(&self, estimate: Estimate) -> Result<Reservation, LimitKind> {
        let mut remaining = self.remaining.lock().expect("remaining lock");
        *remaining = remaining.checked_sub(1).ok_or(LimitKind::Cost)?;

        Ok(Reservation::new(estimate, drop))
    }

    fn commit(&self, _reservation: Reservation, _actual: &Usage, _cost: Option<Cost>) {}
}

#[tokio::test]
async fn a_refused_summary_request_ends_the_run_at_the_ledger_cap() {
    // Arrange
    let (model, requests) = SummarizingModel::new(Vec::new());
    let _ = limited_run(
        &harness(model, ContextSummaryPolicy::Enabled),
        RunLimits::default(),
    )
    .await;
    let first_summary = requests
        .lock()
        .expect("requests lock")
        .iter()
        .position(is_summary_request)
        .expect("the turn summarized");
    let (model, requests) = SummarizingModel::new(Vec::new());
    let (observed_harness, events) = observed(harness(model, ContextSummaryPolicy::Enabled));
    let (model, _) = SummarizingModel::new(Vec::new());
    let unobserved_harness = harness(model, ContextSummaryPolicy::Enabled);
    let refused_run = |harness: &Harness| {
        harness.run(
            RunRequest::new("read every file")
                .output(object_schema())
                .budget(Arc::new(CountingLedger {
                    remaining: Mutex::new(first_summary),
                })),
        )
    };

    // Act
    let outcome = refused_run(&observed_harness).await;
    let unobserved = refused_run(&unobserved_harness).await;

    // Assert
    let stopped = Some(ErrorKind::BudgetExhausted(LimitKind::Cost));
    assert_eq!(outcome.result.err().map(|error| error.kind()), stopped);
    assert_eq!(unobserved.result.err().map(|error| error.kind()), stopped);
    assert_eq!(summary_requests(&requests), Vec::<ModelRequest>::new());
    assert_eq!(outcome.report.model_requests().len(), first_summary);
    assert!(summary_events(&events).iter().any(|event| matches!(
        event,
        LifecycleEventKind::ContextSummaryFailed {
            error_type: ContextSummaryErrorType::BudgetExhausted,
            ..
        }
    )));
}
