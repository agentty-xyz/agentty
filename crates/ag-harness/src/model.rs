use std::error::Error;

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;

use crate::schema_contract::{OutputSchema, OutputValidationError};
use crate::telemetry;

mod private {
    pub trait Sealed {}

    impl<T> Sealed for T where T: super::ModelBackend + ?Sized {}
}

/// Provider-neutral model behavior available to applications.
///
/// Implement [`ModelBackend`] to receive this behavior automatically. The
/// shared implementation records telemetry and validates structured output for
/// every provider.
#[async_trait]
pub trait Model: private::Sealed + Send + Sync {
    /// Completes one model request.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError`] when the provider request fails or its response
    /// cannot be converted to the provider-neutral response.
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError>;
}

#[async_trait]
impl<T> Model for T
where
    T: ModelBackend + ?Sized,
{
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        let _duration = telemetry::RequestDuration::start(self.metadata());
        let output = self.generate(&request).await?;

        request
            .schema()
            .parse_and_validate(&output)
            .map(ModelResponse::new)
            .map_err(ModelError::from)
    }
}

/// Provider strategy used by the shared [`Model`] request lifecycle.
#[async_trait]
pub trait ModelBackend: Send + Sync {
    /// Returns stable identity attributes for model telemetry.
    fn metadata(&self) -> ModelMetadata<'_>;

    /// Generates raw structured-output text for one provider-neutral request.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError`] when the provider cannot satisfy the request or
    /// its response cannot be converted to assistant text.
    async fn generate(&self, request: &ModelRequest) -> Result<String, ModelError>;
}

/// Stable provider and model identity used by shared model behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelMetadata<'a> {
    model: &'a str,
    provider: &'static str,
}

impl<'a> ModelMetadata<'a> {
    /// Creates metadata for one provider model.
    pub fn new(provider: &'static str, model: &'a str) -> Self {
        Self { model, provider }
    }

    /// Returns the model identifier sent to the provider.
    pub fn model(&self) -> &'a str {
        self.model
    }

    /// Returns the provider identifier used by telemetry.
    pub fn provider(&self) -> &'static str {
        self.provider
    }
}

/// Provider-neutral input for one model request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRequest {
    prompt: String,
    schema: OutputSchema,
}

impl ModelRequest {
    /// Creates a model request whose response must match `schema`.
    pub fn new(prompt: impl Into<String>, schema: OutputSchema) -> Self {
        Self {
            prompt: prompt.into(),
            schema,
        }
    }

    /// Returns the request prompt.
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    /// Returns the schema that the response must match.
    pub fn schema(&self) -> &OutputSchema {
        &self.schema
    }
}

/// Provider-neutral output from one model request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelResponse {
    output: Value,
}

impl ModelResponse {
    /// Returns the parsed, schema-validated output.
    pub fn output(&self) -> &Value {
        &self.output
    }

    fn new(output: Value) -> Self {
        Self { output }
    }
}

/// Failure returned while completing a model request.
#[derive(Debug, Error)]
pub enum ModelError {
    /// The provider request or response decoding failed.
    #[error("model request failed: {0}")]
    Request(#[source] Box<dyn Error + Send + Sync>),
    /// The provider returned a successful response without assistant content.
    #[error("model returned no response content")]
    InvalidResponse,
    /// The provider stopped before completing the model response.
    #[error("model response is incomplete: {reason}")]
    IncompleteResponse {
        /// Provider-specific reason generation stopped.
        reason: String,
    },
    /// The successful provider response body exceeds the adapter safety limit.
    #[error("model response body exceeds the size limit")]
    ResponseBodyTooLarge,
    /// The provider cannot represent the requested output schema.
    #[error("provider cannot satisfy this output schema: {reason}")]
    UnsupportedOutputSchema {
        /// Provider-specific reason the schema cannot be represented.
        reason: String,
    },
    /// The decoded provider response content exceeds the harness safety limit.
    #[error("model response content exceeds the size limit")]
    ResponseContentTooLarge,
    /// The provider returned malformed JSON for a structured request.
    #[error("model returned invalid JSON: {reason}")]
    InvalidJson {
        /// JSON parser diagnostic without the raw response body.
        reason: String,
    },
    /// The returned JSON does not conform to the requested schema.
    #[error("model output violates the schema at {path}: {reason}")]
    SchemaViolation {
        /// Bounded JSON Pointer-like path to the invalid value, or `$` for the
        /// root.
        path: String,
        /// Validator diagnostic for the failed constraint.
        reason: String,
    },
}

impl ModelError {
    /// Wraps a provider transport or response-decoding failure.
    pub fn request(error: impl Error + Send + Sync + 'static) -> Self {
        Self::Request(Box::new(error))
    }
}

impl From<OutputValidationError> for ModelError {
    fn from(error: OutputValidationError) -> Self {
        match error {
            OutputValidationError::InvalidJson(reason) => Self::InvalidJson { reason },
            OutputValidationError::SchemaViolation { path, reason } => {
                Self::SchemaViolation { path, reason }
            }
            OutputValidationError::TooLarge => Self::ResponseContentTooLarge,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use serde_json::json;

    use super::*;

    enum StubOutcome {
        Failure,
        Output(&'static str),
    }

    struct StubBackend {
        model: &'static str,
        outcome: StubOutcome,
        provider: &'static str,
    }

    #[async_trait]
    impl ModelBackend for StubBackend {
        fn metadata(&self) -> ModelMetadata<'_> {
            ModelMetadata::new(self.provider, self.model)
        }

        async fn generate(&self, request: &ModelRequest) -> Result<String, ModelError> {
            assert_eq!(request.prompt(), "hello");

            match self.outcome {
                StubOutcome::Failure => Err(ModelError::request(io::Error::other("offline"))),
                StubOutcome::Output(output) => Ok(output.to_string()),
            }
        }
    }

    fn object_schema() -> OutputSchema {
        OutputSchema::new(json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            },
            "required": ["name"],
            "additionalProperties": false
        }))
        .expect("schema should be valid")
    }

    fn backend(outcome: StubOutcome) -> StubBackend {
        StubBackend {
            model: "stub-large",
            outcome,
            provider: "stub_provider",
        }
    }

    #[tokio::test]
    async fn completes_request_through_shared_model_flow() {
        // Arrange
        let model = backend(StubOutcome::Output(r#"{"name":"Ada"}"#));

        // Act
        let response = model
            .complete(ModelRequest::new("hello", object_schema()))
            .await
            .expect("valid backend output should succeed");

        // Assert
        assert_eq!(response.output(), &json!({ "name": "Ada" }));
    }

    #[test]
    fn metadata_exposes_provider_and_model() {
        // Arrange
        let model = backend(StubOutcome::Output(r#"{"name":"Ada"}"#));

        // Act
        let metadata = model.metadata();

        // Assert
        assert_eq!(metadata.provider(), "stub_provider");
        assert_eq!(metadata.model(), "stub-large");
        assert_eq!(metadata, ModelMetadata::new("stub_provider", "stub-large"));
    }

    #[tokio::test]
    async fn shared_model_flow_returns_provider_failure() {
        // Arrange
        let model = backend(StubOutcome::Failure);

        // Act
        let error = model
            .complete(ModelRequest::new("hello", object_schema()))
            .await
            .expect_err("provider failure should be returned");

        // Assert
        assert_eq!(error.to_string(), "model request failed: offline");
    }

    #[tokio::test]
    async fn shared_model_flow_rejects_invalid_json() {
        // Arrange
        let model = backend(StubOutcome::Output("not JSON"));

        // Act
        let error = model
            .complete(ModelRequest::new("hello", object_schema()))
            .await
            .expect_err("invalid JSON should fail");

        // Assert
        assert!(matches!(error, ModelError::InvalidJson { .. }));
    }

    #[tokio::test]
    async fn shared_model_flow_rejects_schema_violation() {
        // Arrange
        let model = backend(StubOutcome::Output(r#"{"name":42}"#));

        // Act
        let error = model
            .complete(ModelRequest::new("hello", object_schema()))
            .await
            .expect_err("schema violation should fail");

        // Assert
        assert!(matches!(
            error,
            ModelError::SchemaViolation { path, .. } if path == "/name"
        ));
    }

    #[test]
    fn request_contains_prompt_and_schema() {
        // Arrange
        let schema =
            OutputSchema::new(json!({ "type": "object" })).expect("schema should be valid");

        // Act
        let request = ModelRequest::new("hello", schema.clone());

        // Assert
        assert_eq!(request.prompt(), "hello");
        assert_eq!(request.schema(), &schema);
    }

    #[test]
    fn response_exposes_validated_output() {
        // Arrange
        let value = json!({ "name": "Ada" });

        // Act
        let response = ModelResponse::new(value.clone());

        // Assert
        assert_eq!(response.output(), &value);
    }

    #[test]
    fn invalid_response_error_has_user_facing_message() {
        // Arrange and Act
        let message = ModelError::InvalidResponse.to_string();

        // Assert
        assert_eq!(message, "model returned no response content");
    }

    #[test]
    fn incomplete_response_error_includes_reason() {
        // Arrange and Act
        let message = ModelError::IncompleteResponse {
            reason: "length".to_string(),
        }
        .to_string();

        // Assert
        assert_eq!(message, "model response is incomplete: length");
    }

    #[test]
    fn request_error_includes_source_message() {
        // Arrange
        let source = io::Error::other("connection refused");

        // Act
        let message = ModelError::request(source).to_string();

        // Assert
        assert_eq!(message, "model request failed: connection refused");
    }

    #[test]
    fn unsupported_schema_error_includes_reason() {
        // Arrange and Act
        let message = ModelError::UnsupportedOutputSchema {
            reason: "top-level object required".to_string(),
        }
        .to_string();

        // Assert
        assert_eq!(
            message,
            "provider cannot satisfy this output schema: top-level object required"
        );
    }

    #[test]
    fn oversized_response_body_error_has_user_facing_message() {
        // Arrange and Act
        let message = ModelError::ResponseBodyTooLarge.to_string();

        // Assert
        assert_eq!(message, "model response body exceeds the size limit");
    }

    #[test]
    fn converts_invalid_json_error() {
        // Arrange
        let error = OutputValidationError::InvalidJson("expected value".to_string());

        // Act
        let error = ModelError::from(error);

        // Assert
        assert_eq!(
            error.to_string(),
            "model returned invalid JSON: expected value"
        );
    }

    #[test]
    fn converts_schema_violation_error() {
        // Arrange
        let error = OutputValidationError::SchemaViolation {
            path: "/name".to_string(),
            reason: "wrong type".to_string(),
        };

        // Act
        let error = ModelError::from(error);

        // Assert
        assert_eq!(
            error.to_string(),
            "model output violates the schema at /name: wrong type"
        );
    }

    #[test]
    fn converts_oversized_content_error() {
        // Arrange and Act
        let error = ModelError::from(OutputValidationError::TooLarge);

        // Assert
        assert_eq!(
            error.to_string(),
            "model response content exceeds the size limit"
        );
    }
}
