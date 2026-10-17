use std::collections::VecDeque;
use std::io::Cursor;
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::support::object_schema;
use crate::context::{ContextBudget, ContextSummaryPolicy};
use crate::file_system::MockFileSystem;
use crate::harness::Harness;
use crate::lifecycle::{
    ContextSummaryErrorType, ContextSummaryTrigger, LifecycleEvent, LifecycleEventKind,
};
use crate::model::{
    Model, ModelCompletion, ModelError, ModelMessage, ModelRequest, ModelResponse, ReasoningEffort,
};
use crate::repository::Repository;
use crate::round_summary::ROUND_SUMMARY_INSTRUCTIONS;
use crate::store::MemoryStore;
use crate::tool::{Tool, ToolCall};
use crate::turn::{ModelRequestPurpose, TurnOutcome};

/// Weight budget that holds about five 8 KB file reads.
const BUDGET: u64 = 12_000;

/// Rounds each turn reads, one distinct file per round, before answering.
const READS: usize = 10;

/// Answers turn requests by reading `f<n>.txt` for [`READS`] rounds and then
/// answering, and summary requests with the scripted responses, in order,
/// then with a valid summary.
struct SummarizingModel {
    requests: Arc<Mutex<Vec<ModelRequest>>>,
    summaries: Mutex<VecDeque<Result<ModelResponse, ModelError>>>,
}

impl SummarizingModel {
    fn new(
        summaries: Vec<Result<ModelResponse, ModelError>>,
    ) -> (Self, Arc<Mutex<Vec<ModelRequest>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let model = Self {
            requests: Arc::clone(&requests),
            summaries: Mutex::new(summaries.into()),
        };

        (model, requests)
    }
}

#[async_trait]
impl Model for SummarizingModel {
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
    Harness::new(
        model,
        ContextBudget::new(NonZeroU64::new(BUDGET).expect("nonzero budget")),
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
        harness(model, ContextSummaryPolicy::Enabled).model_reasoning_effort(ReasoningEffort::Low),
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
            ContextSummaryErrorType::Model
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
