use std::error::Error;

use async_trait::async_trait;
use thiserror::Error;

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
}

impl ModelRequest {
    /// Creates a text-only model request.
    pub fn text(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
        }
    }

    /// Returns the request prompt.
    pub fn prompt(&self) -> &str {
        &self.prompt
    }
}

/// Provider-neutral output from one model request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelResponse {
    text: String,
}

impl ModelResponse {
    /// Creates a text-only model response.
    pub fn from_text(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }

    /// Returns the model's response text.
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Failure returned while completing a model request.
#[derive(Debug, Error)]
pub enum ModelError {
    /// The provider request or response decoding failed.
    #[error("model request failed: {0}")]
    Request(#[source] Box<dyn Error + Send + Sync>),
    /// The provider returned a successful response without assistant text.
    #[error("model returned no response text")]
    InvalidResponse,
}

impl ModelError {
    pub(crate) fn request(error: impl Error + Send + Sync + 'static) -> Self {
        Self::Request(Box::new(error))
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    #[test]
    fn text_request_contains_prompt() {
        // Arrange and Act
        let request = ModelRequest::text("hello");

        // Assert
        assert_eq!(request.prompt(), "hello");
    }

    #[test]
    fn text_response_exposes_text() {
        // Arrange and Act
        let response = ModelResponse::from_text("hello");

        // Assert
        assert_eq!(response.text(), "hello");
    }

    #[test]
    fn invalid_response_error_has_user_facing_message() {
        // Arrange and Act
        let message = ModelError::InvalidResponse.to_string();

        // Assert
        assert_eq!(message, "model returned no response text");
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
}
