//! External-consumer coverage for the `ag-harness` model traits.

use std::error::Error;

use ag_harness::{
    CompletionMetadata, CompletionUsage, Model, ModelCompletion, ModelError, ModelRequest,
    ModelResponse, ModelWithMetadata, OutputSchema, OutputSchemaError,
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
impl Model for ExternalMetadataModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        self.complete_with_metadata(request)
            .await
            .map(ModelCompletion::into_response)
    }
}

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

#[test]
fn external_response_only_provider_implements_model() {
    // Arrange
    fn assert_model<ModelType: Model>() {}

    // Act and Assert
    assert_model::<ExternalModel>();
}

#[tokio::test]
async fn external_provider_constructs_metadata_completion_through_dynamic_dispatch()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let model: Box<dyn ModelWithMetadata> = Box::new(ExternalMetadataModel);

    // Act
    let completion = model.complete_with_metadata(request()?).await?;

    // Assert
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
