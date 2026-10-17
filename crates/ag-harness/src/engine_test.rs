use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::calibration::ContextCalibration;
use crate::context::{ContextBudget, ContextEstimator, ContextFit, HeuristicContextEstimator};
use crate::context_budget_fixture::unbounded_context_budget;
use crate::effect::Effects;
use crate::engine::{Engine, MAX_OUTPUT_CORRECTIONS, RepeatedReads};
use crate::file_system::{FileSystem, LocalFileSystem};
use crate::lifecycle::LifecycleEmitter;
use crate::model::{
    MockModel, Model, ModelCompletion, ModelError, ModelMessage, ModelRequest, ModelResponse,
    ReasoningEffort,
};
use crate::tool::ToolCall;
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

fn tool_call(id: &str, name: &str, arguments: &serde_json::Value) -> ToolCall {
    ToolCall::from_json(id.to_string(), name, &arguments.to_string(), None).expect("tool call")
}

fn read_of(id: &str, arguments: &serde_json::Value) -> ToolCall {
    tool_call(id, "read", arguments)
}

#[test]
fn repeated_reads_point_only_to_an_identical_earlier_read() {
    // Arrange
    let fit = ContextFit::default();
    let mut reads = RepeatedReads::default();
    reads.record(
        &read_of("r1", &json!({"path": "a.txt", "limit": 20})),
        2,
        (5000, 7),
        false,
    );

    // Act
    let identical = reads.repeat_of(&read_of("r2", &json!({"path": "a.txt", "limit": 20})), &fit);
    let equivalent = reads.repeat_of(
        &read_of(
            "r3",
            &json!({"action": "file", "path": "a.txt", "limit": "20"}),
        ),
        &fit,
    );
    let narrower = reads.repeat_of(&read_of("r4", &json!({"path": "a.txt", "limit": 10})), &fit);
    let other = reads.repeat_of(&read_of("r5", &json!({"path": "b.txt", "limit": 20})), &fit);
    let bash = reads.repeat_of(&tool_call("b1", "bash", &json!({"command": "ls"})), &fit);
    let write = reads.repeat_of(
        &tool_call(
            "w1",
            "write",
            &json!({"path": "a.txt", "patch": "@@ -1 +1 @@\n-a\n+b\n"}),
        ),
        &fit,
    );

    // Assert
    assert_eq!(identical, Some(("r1".to_string(), 7)));
    assert_eq!(equivalent, Some(("r1".to_string(), 7)));
    assert_eq!(narrower, None);
    assert_eq!(other, None);
    assert_eq!(bash, None);
    assert_eq!(write, None);
}

#[test]
fn repeated_reads_run_again_after_a_write_or_command() {
    // Arrange
    let fit = ContextFit::default();
    let read = read_of("r1", &json!({"path": "a.txt"}));
    let effects = [
        tool_call("b1", "bash", &json!({"command": "touch a.txt"})),
        tool_call(
            "w1",
            "write",
            &json!({"path": "a.txt", "patch": "@@ -1 +1 @@\n-a\n+b\n"}),
        ),
    ];

    // Act
    let after_effect: Vec<Option<(String, u64)>> = effects
        .iter()
        .map(|effect| {
            let mut reads = RepeatedReads::default();
            reads.record(&read, 2, (5000, 2), false);
            reads.record(effect, 4, (10, 4), false);

            reads.repeat_of(&read, &fit)
        })
        .collect();
    let mut reads = RepeatedReads::default();
    reads.record(&read, 2, (5000, 2), false);
    reads.record(&effects[0], 4, (10, 4), false);
    reads.record(
        &read_of("r2", &json!({"path": "a.txt"})),
        6,
        (5000, 6),
        false,
    );
    let read_again = reads.repeat_of(&read, &fit);
    let malformed = ToolCall::from_json("m1".to_string(), "write", r#"{"path":"#, None)
        .expect("malformed write");
    reads.record(&malformed, 8, (10, 8), false);
    let after_malformed = reads.repeat_of(&read, &fit);

    // Assert
    assert_eq!(after_effect, [None, None]);
    assert_eq!(read_again, Some(("r2".to_string(), 6)));
    assert_eq!(after_malformed, Some(("r2".to_string(), 6)));
}

#[test]
fn repeated_reads_keep_pointing_to_the_executed_read_and_skip_small_results() {
    // Arrange
    let fit = ContextFit::default();
    let read = read_of("r1", &json!({"path": "a.txt"}));
    let small = read_of("s1", &json!({"path": "small.txt"}));
    let mut reads = RepeatedReads::default();
    reads.record(&read, 2, (5000, 2), false);
    reads.record(&read_of("r2", &json!({"path": "a.txt"})), 4, (300, 4), true);
    reads.record(&small, 6, (40, 6), false);

    // Act
    let repeated = reads.repeat_of(&read, &fit);
    let small_repeat = reads.repeat_of(&small, &fit);

    // Assert
    assert_eq!(repeated, Some(("r1".to_string(), 2)));
    assert_eq!(small_repeat, None);
}

#[test]
fn repeated_reads_run_again_once_the_earlier_result_was_reduced() {
    // Arrange
    let read = read_of("r1", &json!({"path": "a.txt"}));
    let mut request = ModelRequest::new("inspect", object_schema());
    request.record_tool_call(read.clone());
    request.record_tool_result(&read, "x".repeat(5000));
    let mut fit = ContextFit::default();
    fit.fit(
        &HeuristicContextEstimator,
        ContextBudget::new(NonZeroU64::new(1000).expect("nonzero budget")),
        &request,
    )
    .expect("fits after capping");
    let mut reads = RepeatedReads::default();
    reads.record(&read, 2, (5000, 2), false);

    // Act
    let repeated = reads.repeat_of(&read, &fit);

    // Assert
    assert_eq!(repeated, None);
}
