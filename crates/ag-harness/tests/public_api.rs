//! External-consumer coverage for the `ag-harness` model traits.

use std::error::Error;
use std::sync::{Arc, Mutex};

use ag_harness::{
    CompletionMetadata, CompletionUsage, Harness, LifecycleEventKind, LifecycleMetrics,
    LifecycleObserverSet, Model, ModelCompletion, ModelError, ModelRequest, ModelResponse,
    ModelWithMetadata, OutputSchema, OutputSchemaError,
};
use async_trait::async_trait;
use serde_json::json;

struct ExternalModel;

#[async_trait]
impl Model for ExternalModel {
    async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
        Ok(ModelResponse::Output(json!({ "name": "Ada" })))
    }
}

struct ExternalMetadataModel;

#[async_trait]
impl ModelWithMetadata for ExternalMetadataModel {
    async fn complete_with_metadata(
        &self,
        _request: ModelRequest,
    ) -> Result<ModelCompletion, ModelError> {
        let usage = CompletionUsage::new(None, None, Some(4), Some(2), None, Some(6));
        let metadata = CompletionMetadata::new(
            "stop".to_string(),
            Some("external-response".to_string()),
            Some("external-model".to_string()),
            None,
            Some(usage),
        );

        Ok(ModelCompletion::new(
            metadata,
            ModelResponse::Output(json!({ "name": "Ada" })),
        ))
    }
}

fn request() -> Result<ModelRequest, OutputSchemaError> {
    Ok(ModelRequest::new(
        "extract the name",
        OutputSchema::new(json!({
            "type": "object",
            "properties": {"name": {"type": "string"}},
            "required": ["name"]
        }))?,
    ))
}

#[tokio::test]
async fn external_response_only_provider_implements_model() -> Result<(), Box<dyn Error>> {
    // Arrange
    fn assert_model<ModelType: Model>() {}
    let model = ExternalModel;

    // Act
    let (response, metadata) = model.complete_with_optional_metadata(request()?).await?;

    // Assert
    assert_model::<ExternalModel>();
    assert_eq!(response.output(), Some(&json!({ "name": "Ada" })));
    assert!(metadata.is_none());

    Ok(())
}

#[tokio::test]
async fn external_provider_constructs_metadata_completion_through_dynamic_dispatch()
-> Result<(), Box<dyn Error>> {
    // Arrange
    fn assert_model<ModelType: Model>() {}
    let model: Box<dyn ModelWithMetadata> = Box::new(ExternalMetadataModel);

    // Act
    let completion = model.complete_with_metadata(request()?).await?;

    // Assert
    assert_model::<ExternalMetadataModel>();
    assert_eq!(
        completion.response().output(),
        Some(&json!({ "name": "Ada" }))
    );
    assert_eq!(
        completion.metadata().response_id(),
        Some("external-response")
    );
    assert_eq!(
        completion
            .metadata()
            .usage()
            .and_then(|usage| usage.total_tokens()),
        Some(6)
    );

    Ok(())
}

#[tokio::test]
async fn external_metadata_provider_reaches_harness_lifecycle() -> Result<(), Box<dyn Error>> {
    // Arrange
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed_events = Arc::clone(&events);
    let harness = Harness::new(ExternalMetadataModel).with_lifecycle_observer(move |event| {
        observed_events
            .lock()
            .expect("event recorder should not be poisoned")
            .push(event);
    });

    // Act
    let output = harness
        .run("extract the name", request()?.schema().clone())
        .await?;

    // Assert
    assert_eq!(output, json!({ "name": "Ada" }));
    let events = events
        .lock()
        .expect("event recorder should not be poisoned");
    assert!(events.iter().any(|event| matches!(
        event.kind(),
        LifecycleEventKind::ModelRequestCompleted {
            completion: Some(metadata),
            ..
        } if metadata.response_id() == Some("external-response")
            && metadata.usage().and_then(|usage| usage.total_tokens()) == Some(6)
    )));

    Ok(())
}

#[tokio::test]
async fn external_observer_set_fans_out_lifecycle_events() -> Result<(), Box<dyn Error>> {
    // Arrange
    let first_events = Arc::new(Mutex::new(Vec::new()));
    let observed_first_events = Arc::clone(&first_events);
    let second_events = Arc::new(Mutex::new(Vec::new()));
    let observed_second_events = Arc::clone(&second_events);
    let observers = LifecycleObserverSet::new(move |event| {
        observed_first_events
            .lock()
            .expect("first event recorder should not be poisoned")
            .push(event);
    })
    .with_observer(move |event| {
        observed_second_events
            .lock()
            .expect("second event recorder should not be poisoned")
            .push(event);
    })
    .with_observer(LifecycleMetrics::new());
    let harness = Harness::new(ExternalMetadataModel).with_lifecycle_observer(observers);

    // Act
    let output = harness
        .run("extract the name", request()?.schema().clone())
        .await?;

    // Assert
    assert_eq!(output, json!({ "name": "Ada" }));
    let first_events = first_events
        .lock()
        .expect("first event recorder should not be poisoned");
    let second_events = second_events
        .lock()
        .expect("second event recorder should not be poisoned");
    assert!(!first_events.is_empty());
    assert_eq!(*first_events, *second_events);

    Ok(())
}
