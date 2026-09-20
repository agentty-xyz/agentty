use std::sync::Arc;

use serde_json::json;

use crate::context::HeuristicContextEstimator;
use crate::effect::Effects;
use crate::engine::Engine;
use crate::file_system::{FileSystem, LocalFileSystem};
use crate::lifecycle::LifecycleEmitter;
use crate::model::{MockModel, Model, ModelRequest, ReasoningEffort};
use crate::{OutputSchema, ToolPolicy, TurnLimits, TurnOptions};

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
    let options = TurnOptions::new(
        object_schema(),
        ToolPolicy::default(),
        TurnLimits::default(),
    );
    let lifecycle = LifecycleEmitter::default();
    let engine = Engine {
        context_budget: None,
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
