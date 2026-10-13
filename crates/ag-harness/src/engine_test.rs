use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::calibration::ContextCalibration;
use crate::context::{ContextBudget, ContextEstimator, HeuristicContextEstimator};
use crate::context_budget_fixture::unbounded_context_budget;
use crate::effect::Effects;
use crate::engine::{Engine, MAX_OUTPUT_CORRECTIONS};
use crate::file_system::{FileSystem, LocalFileSystem};
use crate::lifecycle::LifecycleEmitter;
use crate::model::{
    MockModel, Model, ModelCompletion, ModelError, ModelMessage, ModelRequest, ModelResponse,
    ReasoningEffort,
};
use crate::turn::TurnError;
use crate::{OutputSchema, ToolPolicy, TurnOptions};

fn object_schema() -> OutputSchema {
    OutputSchema::new(json!({"type": "object"})).expect("valid schema")
}

#[test]
fn preserves_request_reasoning_effort_over_harness_default() {
    // Arrange
    let mut model = MockModel::new();
    model.expect_metadata().return_const(None);
    let model: Arc<dyn Model> = Arc::new(model);
    let file_system: Arc<dyn FileSystem> = Arc::new(LocalFileSystem);
    let options = TurnOptions::new(object_schema(), ToolPolicy::default());
    let lifecycle = LifecycleEmitter::default();
    let context_calibration = ContextCalibration::default();
    let engine = Engine {
        context_budget: unbounded_context_budget(),
        context_calibration: &context_calibration,
        context_estimator: &HeuristicContextEstimator,
        effects: Effects::default(),
        file_system: &file_system,
        lifecycle: &lifecycle,
        model: &model,
        model_reasoning_effort: Some(ReasoningEffort::Low),
        options: &options,
        repository: None,
    };
    let request = ModelRequest::new("reply", object_schema())
        .with_model_reasoning_effort(ReasoningEffort::High);

    // Act
    let (request, tools) = engine
        .prepare_request(request, None)
        .expect("request preparation should succeed");

    // Assert
    assert_eq!(
        request.model_reasoning_effort(),
        Some(ReasoningEffort::High)
    );
    assert!(tools.read.is_none());
    assert!(tools.write.is_none());
    assert!(tools.bash.is_none());
}

/// Runs one tool-free turn within `context_budget` against `responses`, in
/// order, and returns the outcome with every request the model received.
async fn run_scripted(
    context_budget: ContextBudget,
    responses: Vec<Result<ModelResponse, ModelError>>,
) -> (Result<crate::TurnOutcome, TurnError>, Vec<ModelRequest>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    let responses = Mutex::new(responses.into_iter());
    let mut model = MockModel::new();
    model.expect_metadata().return_const(None);
    model.expect_complete().returning(move |request| {
        recorded.lock().expect("requests lock").push(request);
        responses
            .lock()
            .expect("responses lock")
            .next()
            .expect("scripted response")
            .map(ModelCompletion::from_response)
    });
    let model: Arc<dyn Model> = Arc::new(model);
    let file_system: Arc<dyn FileSystem> = Arc::new(LocalFileSystem);
    let options = TurnOptions::new(object_schema(), ToolPolicy::default());
    let lifecycle = LifecycleEmitter::default();
    let context_calibration = ContextCalibration::default();
    let engine = Engine {
        context_budget,
        context_calibration: &context_calibration,
        context_estimator: &HeuristicContextEstimator,
        effects: Effects::default(),
        file_system: &file_system,
        lifecycle: &lifecycle,
        model: &model,
        model_reasoning_effort: None,
        options: &options,
        repository: None,
    };
    let outcome = engine
        .run(ModelRequest::new("reply", object_schema()), None, None)
        .await
        .map(|(outcome, _)| outcome);
    let requests = requests.lock().expect("requests lock").clone();

    (outcome, requests)
}

fn invalid_json() -> ModelError {
    ModelError::InvalidJson {
        reason: "expected value at line 1 column 1; content starts with 'I'".to_string(),
    }
}

#[tokio::test]
async fn rejected_final_outputs_are_re_requested_with_bounded_feedback() {
    // Arrange
    let responses = vec![
        Err(invalid_json()),
        Ok(ModelResponse::Output(json!("plain text"))),
        Ok(ModelResponse::Output(json!({"answer": "ok"}))),
    ];

    // Act
    let (outcome, requests) = run_scripted(unbounded_context_budget(), responses).await;

    // Assert
    let outcome = outcome.expect("the third output should be accepted");
    assert_eq!(outcome.output(), &json!({"answer": "ok"}));
    assert_eq!(outcome.report().output_corrections(), 2);
    assert_eq!(outcome.report().model_requests().len(), 1);
    assert_eq!(requests.len(), 3);
    let feedback: Vec<&str> = requests[2]
        .messages()
        .iter()
        .filter_map(|message| match message {
            ModelMessage::User(text) if text.starts_with("[harness]") => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(feedback.len(), 2);
    assert!(feedback[0].contains("invalid JSON"));
    assert!(feedback[1].contains("violates the schema"));
    assert!(
        feedback
            .iter()
            .all(|text| text.contains("single JSON value"))
    );
    assert_eq!(requests[1].messages().len(), 2);
}

#[tokio::test]
async fn rejected_final_outputs_fail_the_turn_after_the_correction_limit() {
    // Arrange
    let responses = (0..=MAX_OUTPUT_CORRECTIONS)
        .map(|_| Err(invalid_json()))
        .collect();

    // Act
    let (outcome, requests) = run_scripted(unbounded_context_budget(), responses).await;

    // Assert
    assert!(matches!(
        outcome,
        Err(TurnError::Model(ModelError::InvalidJson { .. }))
    ));
    assert_eq!(requests.len(), MAX_OUTPUT_CORRECTIONS + 1);
}

#[tokio::test]
async fn feedback_that_overflows_a_tool_free_request_fails_the_turn_typed() {
    // Arrange
    let input_weight =
        HeuristicContextEstimator.message_weight(&ModelMessage::User("reply".to_string()));
    let context_budget =
        ContextBudget::new(NonZeroU64::new(input_weight + 1).expect("nonzero budget"));

    // Act
    let (outcome, requests) = run_scripted(context_budget, vec![Err(invalid_json())]).await;

    // Assert
    assert!(matches!(
        outcome,
        Err(TurnError::ContextBudgetExceeded { budget, .. }) if budget == input_weight + 1
    ));
    assert_eq!(requests.len(), 1);
}
