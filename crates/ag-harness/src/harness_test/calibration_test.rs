use std::io::Cursor;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;

use super::support::object_schema;
use crate::context::{ContextBudget, ContextEstimator, HeuristicContextEstimator};
use crate::file_system::MockFileSystem;
use crate::harness::Harness;
use crate::model::{
    CompletionMetadata, CompletionUsage, Model, ModelCompletion, ModelError, ModelRequest,
    ModelResponse,
};
use crate::model_registry::{ModelCapabilities, ModelRegistry};
use crate::recovery::ExecutionIdentity;
use crate::repository::Repository;
use crate::store::MemoryStore;
use crate::tool::{Tool, ToolCall};
use crate::turn::TurnOutcome;

const BUDGET: u64 = 6_000;
const OVERHEAD: u64 = 500;
const READS: usize = 6;

/// Reads a notes file for [`READS`] rounds per turn, then answers, and
/// reports a fixed [`OVERHEAD`] plus `inflation` times each request's
/// estimated weight as its input tokens, or no usage when `inflation` is
/// `None`.
struct InflatingModel {
    calls: AtomicUsize,
    inflation: Option<u64>,
    weights: Arc<Mutex<Vec<u64>>>,
}

impl InflatingModel {
    fn new(inflation: Option<u64>, weights: &Arc<Mutex<Vec<u64>>>) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            inflation,
            weights: Arc::clone(weights),
        }
    }

    fn estimated_weight(request: &ModelRequest) -> u64 {
        let estimator = HeuristicContextEstimator;
        let messages: u64 = request
            .messages()
            .iter()
            .map(|message| estimator.message_weight(message))
            .sum();
        let tools: u64 = request
            .tools()
            .iter()
            .map(|tool| estimator.tool_definition_weight(tool))
            .sum();

        messages + tools
    }
}

#[async_trait]
impl Model for InflatingModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        let weight = Self::estimated_weight(&request);
        self.weights.lock().expect("weights lock").push(weight);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let response = if call % (READS + 1) < READS {
            // Each round reads another path with the same content, so the
            // loop grows the request without repeating one call.
            let arguments = serde_json::from_value(
                json!({"path": format!("notes_{}.txt", call % (READS + 1))}),
            )
            .expect("read arguments should be valid");
            ModelResponse::ToolCall(ToolCall::read(format!("read_{call}"), arguments, None))
        } else {
            ModelResponse::Output(json!({"summary": "read everything"}))
        };
        let Some(inflation) = self.inflation else {
            return Ok(ModelCompletion::from_response(response));
        };
        let usage = CompletionUsage::new(
            None,
            None,
            Some(OVERHEAD + weight * inflation),
            None,
            None,
            None,
        );
        let metadata = CompletionMetadata::new("stop".to_string(), None, None, None, Some(usage));

        Ok(ModelCompletion::new(metadata, response))
    }
}

fn notes_file_system() -> MockFileSystem {
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

fn budget() -> ContextBudget {
    ContextBudget::new(NonZeroU64::new(BUDGET).expect("nonzero budget"))
}

fn read_loop_harness(model: InflatingModel) -> Harness {
    Harness::new(model, budget())
        .repository(Repository::fixture("repo"))
        .allow(Tool::Read)
        .file_system(notes_file_system())
}

async fn read_notes(harness: &Harness) -> TurnOutcome {
    harness
        .run_once("read the notes repeatedly", object_schema())
        .await
        .expect("the loop should fit the budget")
}

#[tokio::test]
async fn inflated_reported_input_tokens_fit_later_requests_further() {
    // Arrange
    let inflated_weights = Arc::new(Mutex::new(Vec::new()));
    let uncalibrated_weights = Arc::new(Mutex::new(Vec::new()));
    let inflated = read_loop_harness(InflatingModel::new(Some(2), &inflated_weights));
    let uncalibrated = read_loop_harness(InflatingModel::new(None, &uncalibrated_weights));

    // Act
    let outcome = read_notes(&inflated).await;
    let control = read_notes(&uncalibrated).await;

    // Assert
    let inflated_weights = inflated_weights.lock().expect("weights lock").clone();
    let uncalibrated_weights = uncalibrated_weights.lock().expect("weights lock").clone();
    let requests = outcome.report().model_requests();
    assert_eq!(requests.len(), READS + 1);
    assert_eq!(requests[0].calibrated_weight(), None);
    for (request, weight) in requests.iter().zip(&inflated_weights) {
        let reported = OVERHEAD + weight * 2;
        assert_eq!(request.estimated_weight(), *weight);
        assert_eq!(
            request
                .completion()
                .and_then(CompletionMetadata::usage)
                .and_then(|usage| usage.input_tokens()),
            Some(reported)
        );
        assert!(reported <= BUDGET, "request of {reported} tokens overflows");
        if let Some(calibrated) = request.calibrated_weight() {
            assert!(
                calibrated >= reported,
                "{calibrated} under-predicts {reported}"
            );
        }
    }
    // Once growth separates the fixed overhead from the slope, the
    // prediction is exact.
    assert_eq!(
        requests[READS].calibrated_weight(),
        Some(OVERHEAD + inflated_weights[READS] * 2)
    );
    // Without usage, requests keep filling 90% of the budget, beyond what
    // the inflated provider could hold.
    for (request, weight) in control
        .report()
        .model_requests()
        .iter()
        .zip(&uncalibrated_weights)
    {
        assert_eq!(request.calibrated_weight(), None);
        assert_eq!(request.estimated_weight(), *weight);
        assert!(*weight <= BUDGET * 9 / 10);
    }
    assert!(
        uncalibrated_weights
            .iter()
            .any(|weight| OVERHEAD + weight * 2 > BUDGET)
    );
}

#[tokio::test]
async fn calibration_carries_into_later_turns_of_the_same_model() {
    // Arrange
    let weights = Arc::new(Mutex::new(Vec::new()));
    let harness = read_loop_harness(InflatingModel::new(Some(2), &weights));
    read_notes(&harness).await;

    // Act
    let later = read_notes(&harness).await;
    let reconfigured = harness.context_estimator(HeuristicContextEstimator);
    let reset = read_notes(&reconfigured).await;

    // Assert
    assert!(
        later.report().model_requests()[0]
            .calibrated_weight()
            .is_some()
    );
    assert_eq!(reset.report().model_requests()[0].calibrated_weight(), None);
}

#[tokio::test]
async fn switching_models_discards_the_calibration() {
    // Arrange
    let weights = Arc::new(Mutex::new(Vec::new()));
    let mut registry = ModelRegistry::new();
    for key in ["inflated", "other"] {
        registry
            .register(
                ExecutionIdentity::new(key, "1").expect("identity"),
                InflatingModel::new(Some(2), &weights),
                ModelCapabilities {
                    context_budget: budget(),
                    image_input: false,
                    tool_calls: true,
                },
            )
            .expect("register");
    }
    let harness = Harness::from_registry(&registry, "inflated")
        .expect("harness")
        .repository(Repository::fixture("repo"))
        .allow(Tool::Read)
        .file_system(notes_file_system())
        .store(Arc::new(MemoryStore::new()));
    let mut session = harness
        .session("calibrated", object_schema())
        .create()
        .await
        .expect("session");
    session
        .send("read the notes repeatedly")
        .await
        .expect("first turn");
    let calibrated = session
        .send("read the notes again")
        .await
        .expect("calibrated turn");

    // Act
    session
        .switch_model(&registry, "other")
        .await
        .expect("switch");
    let switched = session
        .send("read the notes once more")
        .await
        .expect("switched turn");

    // Assert
    assert!(
        calibrated.report().model_requests()[0]
            .calibrated_weight()
            .is_some()
    );
    assert_eq!(
        switched.report().model_requests()[0].calibrated_weight(),
        None
    );
}
