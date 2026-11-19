//! Harness context summary unit tests for summary requests, corrections,
//! fallbacks, cancellation, and session history.

use std::collections::VecDeque;
use std::io::Cursor;
use std::num::{NonZeroU32, NonZeroU64};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use futures_core::Stream;
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use super::support::object_schema;
use crate::budget::{BudgetLedger, Estimate, Reservation};
use crate::calibration::ContextCalibration;
use crate::catalog::{Capabilities, ModelCatalog, ModelId, ModelSpec, Pricing, Role};
use crate::compaction::{GENERATION_INSTRUCTIONS, summary_schema};
use crate::context::{
    ContextBudget, ContextEstimator, ContextSummaryPolicy, HeuristicContextEstimator,
};
use crate::error::{ErrorKind, RunError};
use crate::event::RunEventKind;
use crate::file_system::MockFileSystem;
use crate::harness::{Harness, HarnessBuilder};
use crate::lifecycle::{
    ContextSummaryErrorType, ContextSummaryTrigger, LifecycleEvent, LifecycleEventKind,
};
use crate::limit::{LimitKind, RunLimits};
use crate::model::{
    CompletionMetadata, CompletionUsage, Model, ModelCompletion, ModelError, ModelMessage,
    ModelMetadata, ModelProfile, ModelRequest, ModelResponse, ReasoningEffort,
};
use crate::repository::Repository;
use crate::request_option::{GenerationParams, OutputMode};
use crate::round_summary::{CONTINUED_SUMMARY_PREFIX, ROUND_SUMMARY_INSTRUCTIONS};
use crate::run::{RunOutcome, RunRequest, Usage};
use crate::schema_contract::OutputSchema;
use crate::store::MemoryStore;
use crate::tool::{ToolCall, ToolDefinition, ToolSet};
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

/// Provider name every named host model reports, so the cataloged models
/// share one provider.
const HOST_PROVIDER: &str = "host";

/// Answers turn requests by reading `f<n>.txt` for [`READS`] rounds and then
/// answering, and summary requests with the scripted responses, in order,
/// then with a valid summary.
struct SummarizingModel {
    keeps_prefix: bool,
    /// Model name reported, under the [`HOST_PROVIDER`] provider, as
    /// metadata, if any.
    name: Option<&'static str>,
    /// Permits observed while answering each summary request.
    permits: Option<PermitProbe>,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
    /// Input tokens reported as the usage of every summary, if any.
    summary_input_tokens: Option<u64>,
    summaries: Mutex<VecDeque<Result<ModelResponse, ModelError>>>,
}

impl SummarizingModel {
    fn new(
        summaries: Vec<Result<ModelResponse, ModelError>>,
    ) -> (Self, Arc<Mutex<Vec<ModelRequest>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let model = Self {
            keeps_prefix: false,
            name: None,
            permits: None,
            requests: Arc::clone(&requests),
            summary_input_tokens: None,
            summaries: Mutex::new(summaries.into()),
        };

        (model, requests)
    }

    /// Returns this model reporting `name` as its metadata.
    fn named(mut self, name: &'static str) -> Self {
        self.name = Some(name);

        self
    }

    /// Returns this model reporting `input_tokens` with every summary.
    fn reporting_summary_usage(mut self, input_tokens: u64) -> Self {
        self.summary_input_tokens = Some(input_tokens);

        self
    }

    /// Returns this model recording the free permits of `probe`'s caps
    /// while it answers each summary request.
    fn probing_permits(mut self, probe: PermitProbe) -> Self {
        self.permits = Some(probe);

        self
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

    fn metadata(&self) -> Option<ModelMetadata> {
        self.name
            .map(|name| ModelMetadata::new(HOST_PROVIDER, name).expect("metadata"))
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        let mut requests = self.requests.lock().expect("requests lock");
        requests.push(request.clone());
        if is_summary_request(&request) {
            drop(requests);
            if let Some(probe) = &self.permits {
                probe.observe();
            }
            let scripted = self.summaries.lock().expect("summaries lock").pop_front();
            let response =
                scripted.unwrap_or_else(|| Ok(ModelResponse::Output(valid_summary())))?;

            return Ok(match self.summary_input_tokens {
                Some(input_tokens) => {
                    let usage =
                        CompletionUsage::new(None, None, Some(input_tokens), None, None, None);
                    let metadata =
                        CompletionMetadata::new("stop".to_string(), None, None, None, Some(usage));

                    ModelCompletion::new(metadata, response)
                }
                None => ModelCompletion::from_response(response),
            });
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

/// Returns whether `request` asks for a round summary or a compaction
/// checkpoint.
fn is_summary_request(request: &ModelRequest) -> bool {
    [ROUND_SUMMARY_INSTRUCTIONS, GENERATION_INSTRUCTIONS]
        .iter()
        .any(|instructions| {
            request.messages().first() == Some(&ModelMessage::System((*instructions).to_string()))
        })
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
    .tools(ToolSet::empty().with_read())
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
    .tools(ToolSet::empty().with_read())
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
    .tools(ToolSet::empty().with_read())
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

/// Free permits of the session and summary models' concurrency caps, set
/// once their catalog is built, which a summary model records while it
/// answers each summary request.
#[derive(Clone, Default)]
struct PermitProbe {
    caps: Arc<OnceLock<(Arc<Semaphore>, Arc<Semaphore>)>>,
    observed: Arc<Mutex<Vec<(usize, usize)>>>,
}

impl PermitProbe {
    /// Records the free permits of the session and summary caps.
    fn observe(&self) {
        if let Some((session, summary)) = self.caps.get() {
            self.observed
                .lock()
                .expect("observed lock")
                .push((session.available_permits(), summary.available_permits()));
        }
    }
}

/// Returns `host/smart` bounded by [`BUDGET`].
fn smart_spec() -> ModelSpec {
    ModelSpec::new("host/smart").with_profile(ModelProfile::new(BUDGET))
}

/// Returns `host/cheap` with structured output, reasoning when `reasoning`
/// is set, and `profile` when given.
fn cheap_spec(reasoning: bool, profile: Option<ModelProfile>) -> ModelSpec {
    let spec = ModelSpec::new("host/cheap").with_capabilities(
        Capabilities::NONE
            .with_structured_output(true)
            .with_reasoning(reasoning),
    );

    match profile {
        Some(profile) => spec.with_profile(profile),
        None => spec,
    }
}

/// Returns a catalog assigning `smart`, named `smart` and described by
/// `smart_spec`, the smart role and `cheap`, named `cheap` and described
/// by `cheap_spec`, the cheap role.
fn summary_catalog(
    (smart, smart_spec): (SummarizingModel, ModelSpec),
    (cheap, cheap_spec): (SummarizingModel, ModelSpec),
) -> Arc<ModelCatalog> {
    let catalog = ModelCatalog::builder()
        .model(smart_spec, Arc::new(smart.named("smart")))
        .model(cheap_spec, Arc::new(cheap.named("cheap")))
        .assign(Role::SMART, ["host/smart"])
        .assign(Role::CHEAP, ["host/cheap"])
        .build()
        .expect("catalog should build");

    Arc::new(catalog)
}

/// Builds a summarizing, session-capable harness on `catalog` and returns
/// it with its recorded lifecycle events.
fn cataloged_harness(catalog: Arc<ModelCatalog>) -> (Harness, Arc<Mutex<Vec<LifecycleEvent>>>) {
    cataloged_harness_with(catalog, |builder| builder)
}

/// Builds [`cataloged_harness`] after `configure` adjusts its builder.
fn cataloged_harness_with(
    catalog: Arc<ModelCatalog>,
    configure: impl FnOnce(HarnessBuilder) -> HarnessBuilder,
) -> (Harness, Arc<Mutex<Vec<LifecycleEvent>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&events);
    let harness = configure(Harness::builder().catalog(catalog))
        .repository(Repository::fixture("repo"))
        .tools(ToolSet::empty().with_read())
        .file_system(files())
        .context_summaries(ContextSummaryPolicy::Enabled)
        .lifecycle_observer(move |event: LifecycleEvent| {
            recorded.lock().expect("events lock").push(event);
        })
        .store(Arc::new(MemoryStore::new()))
        .build()
        .expect("cataloged harness should build");

    (harness, events)
}

/// Returns the model name each lifecycle `ModelRequestStarted` event in
/// `events` reports.
fn started_models(events: &Mutex<Vec<LifecycleEvent>>) -> Vec<Option<String>> {
    events
        .lock()
        .expect("events lock")
        .iter()
        .filter_map(|event| match event.kind() {
            LifecycleEventKind::ModelRequestStarted { model, .. } => {
                Some(model.as_ref().map(|model| model.model().to_string()))
            }
            _ => None,
        })
        .collect()
}

/// Returns whether `calibration` learned from provider usage.
fn calibrated(calibration: &ContextCalibration, budget: ContextBudget) -> bool {
    calibration.effective_budget(budget).1.envelope().is_some()
}

/// Returns the [`RunEventKind::ModelRequestCompleted`] costs in `events`
/// attributed to the model `id`.
fn costs_of(events: &[RunEventKind], id: &str) -> Vec<Option<Cost>> {
    events
        .iter()
        .filter_map(|kind| match kind {
            RunEventKind::ModelRequestCompleted {
                cost,
                model: Some(model),
                ..
            } if model.as_str() == id => Some(*cost),
            _ => None,
        })
        .collect()
}

/// Returns the summary requests of `outcome`'s report.
fn reported_summaries(outcome: &RunOutcome) -> usize {
    outcome
        .report
        .model_requests()
        .iter()
        .filter(|request| request.purpose() == ModelRequestPurpose::ContextSummary)
        .count()
}

#[tokio::test]
async fn round_summaries_go_to_the_cataloged_summary_model_and_are_attributed_to_it() {
    // Arrange
    let (smart, smart_requests) = SummarizingModel::keeping_prefix(Vec::new());
    let (cheap, cheap_requests) = SummarizingModel::new(Vec::new());
    let (harness, lifecycle) = cataloged_harness(summary_catalog(
        (smart, smart_spec()),
        (cheap, cheap_spec(true, None)),
    ));

    // Act
    let (events, outcome) = limited_run(&harness, RunLimits::default()).await;

    // Assert
    assert!(outcome.result.is_ok());
    let smart_requests = smart_requests.lock().expect("requests lock").clone();
    let cheap_requests = cheap_requests.lock().expect("requests lock").clone();
    let summaries = reported_summaries(&outcome);
    assert!(!smart_requests.iter().any(is_summary_request));
    assert_ne!(cheap_requests, Vec::<ModelRequest>::new());
    // The summary model never continues the turn model's cached prefix,
    // and asks for low effort because it declares reasoning.
    assert!(cheap_requests.iter().all(|request| {
        request.prefix_schema().is_none()
            && request.model_reasoning_effort() == Some(ReasoningEffort::Low)
            && request.messages().first()
                == Some(&ModelMessage::System(
                    ROUND_SUMMARY_INSTRUCTIONS.to_string(),
                ))
    }));
    assert_eq!(costs_of(&events, "host/cheap").len(), cheap_requests.len());
    assert_eq!(cheap_requests.len(), summaries);
    assert_eq!(costs_of(&events, "host/smart").len(), smart_requests.len());
    assert!(outcome.report.model_requests().iter().all(|request| {
        let expected = match request.purpose() {
            ModelRequestPurpose::ContextSummary | ModelRequestPurpose::Compaction => "host/cheap",
            ModelRequestPurpose::Turn => "host/smart",
        };

        request.model().map(ModelId::as_str) == Some(expected)
    }));
    assert!(events.iter().any(|kind| matches!(
        kind,
        RunEventKind::Started { model: Some(model), .. } if model.as_str() == "host/smart"
    )));
    // The lifecycle attributes each request to the model that answered it.
    let started = started_models(&lifecycle);
    let started_by = |name: &str| {
        started
            .iter()
            .filter(|model| model.as_deref() == Some(name))
            .count()
    };
    assert_eq!(started_by("cheap"), summaries);
    assert_eq!(started_by("smart"), smart_requests.len());
    assert_eq!(started.len(), smart_requests.len() + summaries);
}

#[tokio::test]
async fn summary_usage_calibrates_only_the_summary_model() {
    // Arrange
    let (smart, _smart_requests) = SummarizingModel::new(Vec::new());
    let (cheap, cheap_requests) = SummarizingModel::new(Vec::new());
    let (harness, _) = cataloged_harness(summary_catalog(
        (smart, smart_spec()),
        (
            cheap.reporting_summary_usage(100),
            cheap_spec(false, Some(ModelProfile::new(1_000_000))),
        ),
    ));
    let config = &harness.inner;
    let summary = config.summary_model.clone().expect("summary model");
    let mut session = harness
        .session("calibrated", object_schema())
        .create()
        .await
        .expect("session created");

    // Act
    session
        .send("read every file")
        .await
        .expect("turn succeeds");
    let after_turn = (
        calibrated(&config.context_calibration, config.context_budget),
        calibrated(&summary.context_calibration, summary.context_budget),
    );
    let checkpoint = session
        .compact()
        .await
        .expect("compact")
        .expect("checkpoint");

    // Assert
    assert_eq!(after_turn, (false, true));
    assert!(!calibrated(
        &config.context_calibration,
        config.context_budget
    ));
    assert_eq!(checkpoint.model(), Some("cheap"));
    let cheap_requests = cheap_requests.lock().expect("requests lock");
    assert!(
        cheap_requests
            .iter()
            .all(|request| request.model_reasoning_effort().is_none()),
        "a summary model without reasoning gets no reasoning effort"
    );
    assert!(
        cheap_requests
            .iter()
            .any(|request| request.messages().first()
                == Some(&ModelMessage::System(GENERATION_INSTRUCTIONS.to_string())))
    );
}

#[tokio::test]
async fn round_summaries_too_large_for_the_summary_model_go_to_the_turn_model() {
    // Arrange
    let (smart, smart_requests) = SummarizingModel::new(Vec::new());
    let (cheap, cheap_requests) = SummarizingModel::new(Vec::new());
    let (harness, _) = cataloged_harness(summary_catalog(
        (smart, smart_spec()),
        (cheap, cheap_spec(true, Some(ModelProfile::new(1_000)))),
    ));

    // Act
    let (events, outcome) = limited_run(&harness, RunLimits::default()).await;

    // Assert
    let smart_requests = smart_requests.lock().expect("requests lock").clone();
    let summaries = reported_summaries(&outcome);
    assert!(outcome.result.is_ok());
    assert!(cheap_requests.lock().expect("requests lock").is_empty());
    assert_eq!(costs_of(&events, "host/cheap").len(), 0);
    assert_ne!(summaries, 0);
    assert_eq!(
        smart_requests
            .iter()
            .filter(|request| is_summary_request(request))
            .count(),
        summaries
    );
    assert_eq!(costs_of(&events, "host/smart").len(), smart_requests.len());
}

#[tokio::test]
async fn summary_requests_are_priced_at_the_summary_models_prices() {
    // Arrange
    let smart_pricing = Pricing::new(1_000_000, 0);
    let cheap_pricing = Pricing::new(3_000_000, 0);
    let (smart, _smart_requests) = SummarizingModel::new(Vec::new());
    let (cheap, _cheap_requests) = SummarizingModel::new(Vec::new());
    let (harness, _) = cataloged_harness(summary_catalog(
        (smart, smart_spec().with_pricing(smart_pricing)),
        (
            cheap.reporting_summary_usage(1_000),
            cheap_spec(false, Some(ModelProfile::new(1_000_000))).with_pricing(cheap_pricing),
        ),
    ));

    // Act
    let (events, outcome) = limited_run(&harness, RunLimits::default()).await;

    // Assert
    let summary_usage = Usage {
        input: 1_000,
        unreported_requests: 1,
        ..Usage::default()
    };
    let summary_cost = Cost::of(&summary_usage, cheap_pricing);
    assert_eq!(summary_cost.micro_usd, 3_000);
    assert_ne!(summary_cost, Cost::of(&summary_usage, smart_pricing));
    let summaries = reported_summaries(&outcome);
    assert_ne!(summaries, 0);
    assert_eq!(
        costs_of(&events, "host/cheap"),
        vec![Some(summary_cost); summaries]
    );
    assert!(
        costs_of(&events, "host/smart")
            .iter()
            .all(|cost| cost.map(|cost| cost.micro_usd) == Some(0))
    );
    assert!(
        outcome
            .report
            .model_requests()
            .iter()
            .filter(|request| request.purpose() == ModelRequestPurpose::ContextSummary)
            .all(|request| request.cost() == Some(summary_cost))
    );
    let summed = 3_000 * u64::try_from(summaries).expect("summary count fits");
    assert_eq!(outcome.cost.map(|cost| cost.micro_usd), Some(summed));
    assert!(matches!(
        events.last(),
        Some(RunEventKind::Completed(summary))
            if summary.cost.map(|cost| cost.micro_usd) == Some(summed)
    ));
}

#[tokio::test]
async fn priced_runs_turn_unpriced_once_an_unpriced_model_answers_a_request() {
    // Arrange: a priced session model summarizing on an explicitly selected
    // unpriced model, and an unpriced session model summarizing on a priced
    // one.
    let pricing = Pricing::new(1_000_000, 0);
    let run = |smart_pricing: Option<Pricing>, cheap_pricing: Option<Pricing>| {
        let (smart, _smart_requests) = SummarizingModel::new(Vec::new());
        let (cheap, _cheap_requests) = SummarizingModel::new(Vec::new());
        let price = |spec: ModelSpec, pricing: Option<Pricing>| match pricing {
            Some(pricing) => spec.with_pricing(pricing),
            None => spec,
        };
        let catalog = summary_catalog(
            (smart, price(smart_spec(), smart_pricing)),
            (
                cheap.reporting_summary_usage(1_000),
                price(
                    cheap_spec(false, Some(ModelProfile::new(1_000_000))),
                    cheap_pricing,
                ),
            ),
        );
        let (harness, _) = cataloged_harness_with(catalog, |builder| {
            builder.summary_model_selector(Role::CHEAP)
        });

        async move { limited_run(&harness, RunLimits::default()).await }
    };

    // Act
    let (unpriced_summary_events, unpriced_summary) = run(Some(pricing), None).await;
    let (priced_summary_events, priced_summary) = run(None, Some(pricing)).await;

    // Assert
    for (events, outcome) in [
        (&unpriced_summary_events, &unpriced_summary),
        (&priced_summary_events, &priced_summary),
    ] {
        assert!(outcome.result.is_ok());
        assert_ne!(reported_summaries(outcome), 0);
        assert_eq!(outcome.cost, None);
        assert!(matches!(
            events.last(),
            Some(RunEventKind::Completed(summary)) if summary.cost.is_none()
        ));
    }
    assert!(
        costs_of(&unpriced_summary_events, "host/smart")
            .iter()
            .all(Option::is_some)
    );
    assert!(
        costs_of(&unpriced_summary_events, "host/cheap")
            .iter()
            .all(Option::is_none)
    );
    assert!(
        costs_of(&priced_summary_events, "host/smart")
            .iter()
            .all(Option::is_none)
    );
    let priced_summaries = costs_of(&priced_summary_events, "host/cheap");
    assert_ne!(priced_summaries, Vec::new());
    assert!(
        priced_summaries
            .iter()
            .all(|cost| cost.map(|cost| cost.micro_usd) == Some(1_000))
    );
}

/// Prefix of the feedback that asks a model to correct a rejected output.
const CORRECTION_FEEDBACK: &str = "[harness] Your previous final response was rejected";

/// Heuristic estimator that weighs correction feedback past any summary
/// model's window, so a corrected request outgrows it.
struct HeavyCorrectionEstimator;

impl ContextEstimator for HeavyCorrectionEstimator {
    fn message_weight(&self, message: &ModelMessage) -> u64 {
        match message {
            ModelMessage::User(text) if text.starts_with(CORRECTION_FEEDBACK) => 1_000_000,
            _ => HeuristicContextEstimator::DEFAULT.message_weight(message),
        }
    }

    fn tool_definition_weight(&self, tool: &ToolDefinition) -> u64 {
        HeuristicContextEstimator::DEFAULT.tool_definition_weight(tool)
    }

    fn request_weight(&self, schema: Option<&OutputSchema>) -> u64 {
        HeuristicContextEstimator::DEFAULT.request_weight(schema)
    }
}

#[tokio::test]
async fn corrections_that_outgrow_the_summary_model_go_to_the_turn_model() {
    // Arrange: the cheap model's window holds the first summary request but
    // not its correction.
    let smart_pricing = Pricing::new(1_000_000, 0);
    let cheap_pricing = Pricing::new(3_000_000, 0);
    let rejected = Ok(ModelResponse::Output(json!({"context": "missing fields"})));
    let (smart, _smart_requests) = SummarizingModel::new(Vec::new());
    let (cheap, cheap_requests) = SummarizingModel::new(vec![rejected]);
    let (harness, _) = cataloged_harness_with(
        summary_catalog(
            (smart, smart_spec().with_pricing(smart_pricing)),
            (
                cheap.reporting_summary_usage(1_000),
                cheap_spec(true, Some(ModelProfile::new(100_000))).with_pricing(cheap_pricing),
            ),
        ),
        |builder| builder.context_estimator(HeavyCorrectionEstimator),
    );

    // Act
    let (_, outcome) = limited_run(&harness, RunLimits::default()).await;

    // Assert
    assert!(outcome.result.is_ok());
    let summaries: Vec<_> = outcome
        .report
        .model_requests()
        .iter()
        .filter(|request| request.purpose() == ModelRequestPurpose::ContextSummary)
        .collect();
    let (rejected, corrected) = (summaries[0], summaries[1]);
    assert!(rejected.rejected_output());
    assert_eq!(rejected.model().map(ModelId::as_str), Some("host/cheap"));
    assert_eq!(rejected.cost().map(|cost| cost.micro_usd), Some(3_000));
    assert!(!corrected.rejected_output());
    assert_eq!(corrected.model().map(ModelId::as_str), Some("host/smart"));
    let cheap_requests = cheap_requests.lock().expect("requests lock");
    assert!(
        !cheap_requests
            .iter()
            .any(|request| carries(request, CORRECTION_FEEDBACK)),
        "the correction never reaches the summary model"
    );
}

/// Ledger that admits every model request and records its estimate and
/// the cost it was charged.
#[derive(Default)]
struct RecordingLedger {
    charged: Mutex<Vec<Option<Cost>>>,
    estimates: Mutex<Vec<Estimate>>,
}

#[async_trait]
impl BudgetLedger for RecordingLedger {
    async fn reserve(&self, estimate: Estimate) -> Result<Reservation, LimitKind> {
        self.estimates
            .lock()
            .expect("estimates lock")
            .push(estimate);

        Ok(Reservation::new(estimate, drop))
    }

    fn commit(&self, _reservation: Reservation, _actual: &Usage, cost: Option<Cost>) {
        self.charged.lock().expect("charged lock").push(cost);
    }
}

/// Returns `host/cheap` priced at `pricing`, with a [`ModelProfile`] whose
/// estimator weighs every request at least 5,000, and a maximum output of
/// 777, each telling its estimates apart from the turn model's.
fn distinct_cheap_spec(pricing: Pricing) -> ModelSpec {
    let profile = ModelProfile::new(1_000_000)
        .with_estimator(HeuristicContextEstimator::DEFAULT.with_request_tokens(5_000));

    cheap_spec(false, Some(profile))
        .with_max_output(777)
        .with_pricing(pricing)
}

#[tokio::test]
async fn summary_requests_reserve_the_summary_models_estimate_from_the_run_ledger() {
    // Arrange
    let cheap_pricing = Pricing::new(3_000_000, 5_000_000);
    let (smart, smart_requests) = SummarizingModel::new(Vec::new());
    let (cheap, cheap_requests) = SummarizingModel::new(Vec::new());
    let (harness, _) = cataloged_harness(summary_catalog(
        (smart, smart_spec().with_max_output(333)),
        (
            cheap.reporting_summary_usage(1_000),
            distinct_cheap_spec(cheap_pricing),
        ),
    ));
    let ledger = Arc::new(RecordingLedger::default());
    let max_output = NonZeroU32::new(222).expect("nonzero output");

    // Act
    let outcome = harness
        .run(
            RunRequest::new("read every file")
                .output(object_schema())
                .generation(GenerationParams::default().with_max_output_tokens(max_output))
                .budget(Arc::clone(&ledger) as Arc<dyn BudgetLedger>),
        )
        .await;

    // Assert
    assert!(outcome.result.is_ok());
    let estimates = ledger.estimates.lock().expect("estimates lock").clone();
    let requests = outcome.report.model_requests();
    assert_eq!(estimates.len(), requests.len());
    let (summaries, turns): (Vec<_>, Vec<_>) = estimates
        .iter()
        .zip(requests)
        .partition(|(_, request)| request.purpose() == ModelRequestPurpose::ContextSummary);
    assert_eq!(
        summaries.len(),
        cheap_requests.lock().expect("requests lock").len()
    );
    assert!(summaries.len() >= 2, "a later summary is calibrated");
    assert_eq!(
        turns.len(),
        smart_requests.lock().expect("requests lock").len()
    );
    // The run's output cap bounds only the turn model's estimates.
    assert!(turns.iter().all(|(estimate, request)| {
        estimate.output_tokens == 222
            && estimate.cost.is_none()
            && estimate.input_tokens == request.estimated_weight()
            && request.calibrated_weight().is_none()
    }));
    // The summary model's estimator weighs, and its calibration scales,
    // each summary estimate's input.
    assert!(summaries.iter().all(|(estimate, request)| {
        let usage = Usage {
            input: estimate.input_tokens,
            output: estimate.output_tokens,
            ..Usage::default()
        };

        estimate.output_tokens == 777
            && request.estimated_weight() >= 5_000
            && estimate.input_tokens
                == request
                    .calibrated_weight()
                    .unwrap_or(request.estimated_weight())
            && estimate.cost == Some(Cost::of(&usage, cheap_pricing))
    }));
    assert!(summaries[1..].iter().all(|(estimate, request)| {
        request.calibrated_weight() == Some(estimate.input_tokens)
            && estimate.input_tokens != request.estimated_weight()
    }));
}

#[tokio::test]
async fn compaction_requests_reserve_and_are_charged_at_the_summary_model() {
    // Arrange
    let cheap_pricing = Pricing::new(3_000_000, 0);
    let (smart, _smart_requests) = SummarizingModel::new(Vec::new());
    let (cheap, _cheap_requests) = SummarizingModel::new(Vec::new());
    let ledger = Arc::new(RecordingLedger::default());
    let (harness, _) = cataloged_harness_with(
        summary_catalog(
            (
                smart,
                smart_spec()
                    .with_max_output(333)
                    .with_pricing(Pricing::new(1_000_000, 0)),
            ),
            (
                cheap.reporting_summary_usage(1_000),
                distinct_cheap_spec(cheap_pricing),
            ),
        ),
        |builder| builder.budget(Arc::clone(&ledger) as Arc<dyn BudgetLedger>),
    );
    let mut session = harness
        .session("charged", object_schema())
        .create()
        .await
        .expect("session created");
    session
        .send("read every file")
        .await
        .expect("turn succeeds");
    ledger.estimates.lock().expect("estimates lock").clear();
    ledger.charged.lock().expect("charged lock").clear();

    // Act
    let checkpoint = session
        .compact()
        .await
        .expect("compact")
        .expect("checkpoint");

    // Assert
    assert_eq!(checkpoint.model(), Some("cheap"));
    let estimates = ledger.estimates.lock().expect("estimates lock").clone();
    assert_eq!(estimates.len(), 1);
    assert_eq!(estimates[0].output_tokens, 777);
    let usage = Usage {
        input: estimates[0].input_tokens,
        output: 777,
        ..Usage::default()
    };
    assert_eq!(estimates[0].cost, Some(Cost::of(&usage, cheap_pricing)));
    let charged = ledger.charged.lock().expect("charged lock").clone();
    assert_eq!(
        charged
            .iter()
            .map(|cost| cost.map(|cost| cost.micro_usd))
            .collect::<Vec<_>>(),
        vec![Some(3_000)]
    );
}

#[tokio::test]
async fn summary_requests_take_the_summary_models_concurrency_permit() {
    // Arrange
    let one = NonZeroU32::new(1).expect("nonzero cap");
    let probe = PermitProbe::default();
    let (smart, _smart_requests) = SummarizingModel::new(Vec::new());
    let (cheap, cheap_requests) = SummarizingModel::new(Vec::new());
    let catalog = summary_catalog(
        (smart, smart_spec().with_max_concurrency(one)),
        (
            cheap.probing_permits(probe.clone()),
            cheap_spec(false, Some(ModelProfile::new(1_000_000))).with_max_concurrency(one),
        ),
    );
    let cap = |id: &str| {
        catalog
            .resolve(&ModelId::new(id).into())
            .expect("cataloged model")
            .concurrency()
            .expect("concurrency cap")
    };
    let _ = probe.caps.set((cap("host/smart"), cap("host/cheap")));
    let (harness, _) = cataloged_harness(Arc::clone(&catalog));

    let mut session = harness
        .session("permits", object_schema())
        .create()
        .await
        .expect("session created");

    // Act
    let (_, outcome) = limited_run(&harness, RunLimits::default()).await;
    let summarized = probe.observed.lock().expect("observed lock").len();
    session
        .send("read every file")
        .await
        .expect("turn succeeds");
    let compacted = session
        .compact()
        .await
        .expect("compact")
        .expect("checkpoint");

    // Assert
    assert!(outcome.result.is_ok());
    assert_eq!(compacted.model(), Some("cheap"));
    let observed = probe.observed.lock().expect("observed lock").clone();
    assert_ne!(summarized, 0);
    assert!(observed.len() > summarized, "compaction is observed too");
    assert_eq!(
        observed.len(),
        cheap_requests.lock().expect("requests lock").len()
    );
    assert!(
        observed.iter().all(|permits| *permits == (1, 0)),
        "summary and compaction requests hold the summary model's permit, not the session model's"
    );
    assert_eq!(cap("host/cheap").available_permits(), 1);
}

#[tokio::test]
async fn summary_model_requests_count_toward_the_runs_limits() {
    // Arrange
    let catalog = || {
        let (smart, smart_requests) = SummarizingModel::new(Vec::new());
        let (cheap, cheap_requests) = SummarizingModel::new(Vec::new());
        let catalog = summary_catalog(
            (smart, smart_spec()),
            (
                cheap.reporting_summary_usage(100),
                cheap_spec(false, Some(ModelProfile::new(1_000_000))),
            ),
        );

        (catalog, smart_requests, cheap_requests)
    };
    let (unlimited, smart_requests, cheap_requests) = catalog();
    let (_, unlimited) = limited_run(&cataloged_harness(unlimited).0, RunLimits::default()).await;
    let sent = smart_requests.lock().expect("requests lock").len()
        + cheap_requests.lock().expect("requests lock").len();
    let limit = u32::try_from(sent - 1).expect("request count fits");
    let (by_requests, _, _) = catalog();
    let (by_tokens, _, cheap_requests) = catalog();

    // Act
    let (_, request_limited) = limited_run(
        &cataloged_harness(by_requests).0,
        RunLimits::default()
            .with_max_model_requests(NonZeroU32::new(limit).expect("nonzero limit")),
    )
    .await;
    let (_, token_limited) = limited_run(
        &cataloged_harness(by_tokens).0,
        RunLimits::default().with_max_total_tokens(NonZeroU64::new(1).expect("nonzero limit")),
    )
    .await;

    // Assert
    assert!(unlimited.result.is_ok());
    assert_ne!(reported_summaries(&unlimited), 0);
    assert_eq!(
        request_limited.result.err().map(|error| error.kind()),
        Some(ErrorKind::BudgetExhausted(LimitKind::ModelRequests)),
        "summary model requests count toward the request limit"
    );
    assert_eq!(request_limited.report.model_requests().len(), sent - 1);
    assert_eq!(
        token_limited.result.err().map(|error| error.kind()),
        Some(ErrorKind::BudgetExhausted(LimitKind::Tokens)),
        "only the summary model reports tokens, so its usage reached the limit"
    );
    assert_eq!(cheap_requests.lock().expect("requests lock").len(), 1);
}
