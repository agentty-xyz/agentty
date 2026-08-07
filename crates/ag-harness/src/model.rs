use std::error::Error;

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;

use crate::schema_contract::{OutputSchema, OutputValidationError};

/// A model backend that completes provider-neutral requests.
#[async_trait]
pub trait Model: Send + Sync {
    /// Completes one model request.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError`] when the provider request fails or its response
    /// cannot be converted to the provider-neutral response.
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError>;
}

/// Provider-neutral input for one model request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRequest {
    prompt: String,
    schema: OutputSchema,
    session_id: Option<String>,
}

impl ModelRequest {
    /// Creates a model request whose response must match `schema`.
    pub fn new(prompt: impl Into<String>, schema: OutputSchema) -> Self {
        Self {
            prompt: prompt.into(),
            schema,
            session_id: None,
        }
    }

    /// Associates the request with the persisted harness session that owns it.
    ///
    /// Instrumented model adapters attach this value to the model-call span as
    /// `gen_ai.conversation.id`. The value is never used as a metric
    /// attribute.
    #[must_use]
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());

        self
    }

    /// Returns the request prompt.
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    /// Returns the schema that the response must match.
    pub fn schema(&self) -> &OutputSchema {
        &self.schema
    }

    /// Returns the persisted harness session identifier, when supplied.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
}

/// Provider-neutral output from one model request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelResponse {
    output: Value,
}

impl ModelResponse {
    /// Creates a response from parsed JSON that passed the caller's schema.
    pub fn new(output: Value) -> Self {
        Self { output }
    }

    /// Returns the parsed, schema-validated output.
    pub fn output(&self) -> &Value {
        &self.output
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
    pub(crate) fn request(error: impl Error + Send + Sync + 'static) -> Self {
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
    fn request_exposes_session_id() {
        // Arrange
        let schema =
            OutputSchema::new(json!({ "type": "object" })).expect("schema should be valid");

        // Act
        let request = ModelRequest::new("hello", schema).with_session_id("session-42");

        // Assert
        assert_eq!(request.session_id(), Some("session-42"));
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
