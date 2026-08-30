use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::client::{CodexResponsesClient, CodexResponsesRequest, HttpCodexResponsesClient};
use super::error::CodexClientError;
use crate::lifecycle::LifecycleObserver;
use crate::model::{
    CompletionMetadata, GeneratedResponse, ModelClient, ModelCompletion, ModelError, ModelMessage,
    ModelMetadata, ModelMetadataError, ModelRequest, ModelWithMetadata,
};
use crate::telemetry;

pub(super) const DEFAULT_INSTRUCTIONS: &str = "Return the requested structured result.";

/// Configuration for the experimental ChatGPT-subscription-backed Codex model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexConfig {
    auth_file: Option<PathBuf>,
    model: String,
}

impl CodexConfig {
    /// Creates configuration that discovers the Codex `auth.json` file.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            auth_file: None,
            model: model.into(),
        }
    }

    /// Overrides the Codex `auth.json` file used for `ChatGPT` OAuth
    /// credentials.
    #[must_use]
    pub fn auth_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.auth_file = Some(path.into());

        self
    }

    /// Returns the explicit authentication file, when configured.
    pub fn auth_file_path(&self) -> Option<&Path> {
        self.auth_file.as_deref()
    }

    /// Returns the configured Codex model identifier.
    pub fn model(&self) -> &str {
        &self.model
    }
}

/// Structured-output model using the experimental `ChatGPT` Codex endpoint.
pub struct Codex {
    client: ModelClient,
}

impl Codex {
    /// Creates a ChatGPT-subscription-backed Codex model.
    ///
    /// # Errors
    ///
    /// Returns [`ModelMetadataError`] when the model identifier is empty.
    pub fn new(config: CodexConfig) -> Result<Self, ModelMetadataError> {
        ModelClient::codex(config).map(|client| Self { client })
    }

    /// Sends metadata-only request lifecycle events to `observer`.
    #[must_use]
    pub fn with_lifecycle_observer(mut self, observer: impl LifecycleObserver + 'static) -> Self {
        self.client = self.client.with_lifecycle_observer(observer);

        self
    }

    #[cfg(test)]
    pub(super) fn with_backend(backend: CodexBackend) -> Result<Self, ModelMetadataError> {
        ModelClient::codex_backend(backend).map(|client| Self { client })
    }
}

#[async_trait]
impl ModelWithMetadata for Codex {
    fn metadata(&self) -> Option<ModelMetadata> {
        Some(self.client.metadata().clone())
    }

    async fn complete_with_metadata(
        &self,
        request: ModelRequest,
    ) -> Result<ModelCompletion, ModelError> {
        self.client.complete_with_metadata(request).await
    }
}

pub(crate) struct CodexBackend {
    client: Arc<dyn CodexResponsesClient>,
    model: String,
}

impl CodexBackend {
    pub(crate) fn new(config: CodexConfig) -> Self {
        Self {
            client: Arc::new(HttpCodexResponsesClient::new(config.auth_file)),
            model: config.model,
        }
    }

    pub(crate) fn identity(&self) -> (&'static str, &str) {
        (telemetry::PROVIDER_OPENAI, &self.model)
    }

    pub(crate) async fn generate(
        &self,
        request: &ModelRequest,
    ) -> Result<GeneratedResponse, ModelError> {
        if !request.tools().is_empty() {
            return Err(CodexClientError::UnsupportedTools.into_model_error());
        }
        if !request.schema().has_object_root() {
            return Err(ModelError::UnsupportedOutputSchema {
                reason: "Codex structured output requires an explicit object root schema"
                    .to_string(),
            });
        }
        let prompt = CodexPrompt::from_messages(request.messages())?;
        let completion = self
            .client
            .complete(CodexResponsesRequest {
                input: prompt.input,
                instructions: prompt.instructions,
                model: self.model.clone(),
                output_schema: request.schema().value().clone(),
            })
            .await?;
        let metadata = CompletionMetadata::new(
            completion.status,
            completion.response_id,
            completion
                .response_model
                .or_else(|| Some(self.model.clone())),
            None,
            completion.usage,
        );

        Ok(GeneratedResponse::Output {
            metadata,
            output: completion.output,
        })
    }

    #[cfg(test)]
    pub(super) fn with_client(
        model: impl Into<String>,
        client: Arc<dyn CodexResponsesClient>,
    ) -> Self {
        Self {
            client,
            model: model.into(),
        }
    }
}

pub(super) struct CodexPrompt {
    pub(super) input: Vec<Value>,
    pub(super) instructions: String,
}

impl CodexPrompt {
    pub(super) fn from_messages(messages: &[ModelMessage]) -> Result<Self, ModelError> {
        let mut input = Vec::new();
        let mut instructions = Vec::new();
        for message in messages {
            match message {
                ModelMessage::Assistant(content) => {
                    input.push(response_message("assistant", "output_text", content));
                }
                ModelMessage::System(content) => instructions.push(content.as_str()),
                ModelMessage::User(content) => {
                    input.push(response_message("user", "input_text", content));
                }
                ModelMessage::AssistantToolCall(_)
                | ModelMessage::AssistantToolCalls(_)
                | ModelMessage::ToolResult { .. } => {
                    return Err(CodexClientError::UnsupportedTools.into_model_error());
                }
            }
        }
        let instructions = instructions.join("\n\n");
        let instructions = match instructions.trim() {
            "" => DEFAULT_INSTRUCTIONS.to_string(),
            instructions => instructions.to_string(),
        };

        Ok(Self {
            input,
            instructions,
        })
    }
}

pub(super) fn response_message(role: &str, content_type: &str, content: &str) -> Value {
    json!({
        "type": "message",
        "role": role,
        "content": [{ "type": content_type, "text": content }]
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use serde_json::json;

    use super::super::client::{CodexResponsesClient, CodexResponsesRequest};
    use super::super::sse::CodexResponsesCompletion;
    use super::super::test_support::person_schema;
    use super::*;
    use crate::model::{
        GeneratedResponse, Model, ModelError, ModelMessage, ModelMetadataError, ModelRequest,
        ModelWithMetadata,
    };
    use crate::{OutputSchema, ToolDefinition};

    #[derive(Default)]
    struct FixedClient {
        requests: Mutex<Vec<CodexResponsesRequest>>,
    }

    #[async_trait]
    impl CodexResponsesClient for FixedClient {
        async fn complete(
            &self,
            request: CodexResponsesRequest,
        ) -> Result<CodexResponsesCompletion, ModelError> {
            self.requests
                .lock()
                .expect("request recorder should lock")
                .push(request);

            Ok(CodexResponsesCompletion {
                output: r#"{"name":"Ada"}"#.to_string(),
                response_id: Some("response-1".to_string()),
                response_model: None,
                status: "completed".to_string(),
                usage: None,
            })
        }
    }

    #[test]
    fn configuration_exposes_model_and_auth_override() {
        // Arrange
        let config = CodexConfig::new("gpt-test").auth_file("custom-auth.json");

        // Act
        let model = Codex::new(config.clone()).expect("Codex configuration should be valid");
        let invalid = Codex::new(CodexConfig::new(" ")).err();

        // Assert
        assert_eq!(config.model(), "gpt-test");
        assert_eq!(config.auth_file_path(), Some(Path::new("custom-auth.json")));
        assert_eq!(CodexConfig::new("gpt-test").auth_file_path(), None);
        assert_eq!(
            Model::metadata(&model)
                .expect("Codex metadata should be present")
                .model(),
            "gpt-test"
        );
        assert!(matches!(invalid, Some(ModelMetadataError::EmptyModel)));
    }

    #[tokio::test]
    async fn backend_maps_messages_and_returns_structured_output() {
        // Arrange
        let client = Arc::new(FixedClient::default());
        let backend = CodexBackend::with_client("gpt-test", client.clone());
        let request = ModelRequest::with_history(
            vec![ModelMessage::System("Follow the schema".to_string())],
            "Extract the name",
            person_schema(),
        );

        // Act
        let response = backend
            .generate(&request)
            .await
            .expect("fixed response should succeed");

        // Assert
        assert!(matches!(
            &response,
            GeneratedResponse::Output { metadata, output }
                if output == r#"{"name":"Ada"}"#
                    && metadata.response_id() == Some("response-1")
                    && metadata.response_model() == Some("gpt-test")
        ));
        let requests = client
            .requests
            .lock()
            .expect("request recorder should lock");
        assert_eq!(requests[0].instructions, "Follow the schema");
        assert_eq!(requests[0].input[0].get("role"), Some(&json!("user")));
    }

    #[tokio::test]
    async fn backend_rejects_unsupported_capabilities_without_a_request() {
        // Arrange
        let client = Arc::new(FixedClient::default());
        let backend = CodexBackend::with_client("gpt-test", client.clone());
        let tool_request =
            ModelRequest::new("Read", person_schema()).with_tool(ToolDefinition::read());
        let array_schema =
            OutputSchema::new(json!({ "type": "array" })).expect("array schema should compile");

        // Act
        let tool_error = backend.generate(&tool_request).await.err();
        let schema_error = backend
            .generate(&ModelRequest::new("Array", array_schema))
            .await
            .err();

        // Assert
        assert_eq!(
            tool_error
                .as_ref()
                .expect("tools should be rejected")
                .error_type(),
            crate::model::ModelErrorType::UnsupportedCapability
        );
        assert!(matches!(
            schema_error,
            Some(ModelError::UnsupportedOutputSchema { .. })
        ));
        assert!(
            client
                .requests
                .lock()
                .expect("request recorder should lock")
                .is_empty()
        );
    }

    #[test]
    fn prompt_maps_conversation_and_rejects_tool_history() {
        // Arrange
        let messages = [
            ModelMessage::Assistant("prior".to_string()),
            ModelMessage::User("next".to_string()),
        ];
        let blank_system_messages = [
            ModelMessage::System("  \n".to_string()),
            ModelMessage::User("next".to_string()),
        ];
        let tool_result = ModelMessage::ToolResult {
            call_id: "call-1".to_string(),
            content: "contents".to_string(),
            name: "read".to_string(),
        };

        // Act
        let prompt = CodexPrompt::from_messages(&messages).expect("messages should map");
        let blank_system_prompt = CodexPrompt::from_messages(&blank_system_messages)
            .expect("blank system prompt should use the default");
        let error = CodexPrompt::from_messages(&[tool_result]).err();

        // Assert
        assert_eq!(prompt.instructions, DEFAULT_INSTRUCTIONS);
        assert_eq!(blank_system_prompt.instructions, DEFAULT_INSTRUCTIONS);
        assert_eq!(prompt.input[0].get("role"), Some(&json!("assistant")));
        assert_eq!(prompt.input[1].get("role"), Some(&json!("user")));
        assert_eq!(
            error
                .as_ref()
                .expect("tool history should be rejected")
                .error_type(),
            crate::model::ModelErrorType::UnsupportedCapability
        );
    }

    #[tokio::test]
    async fn codex_exposes_public_metadata_and_observer_configuration() {
        // Arrange
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed_events = events.clone();
        let backend = CodexBackend::with_client("gpt-test", Arc::new(FixedClient::default()));
        let model = Codex::with_backend(backend)
            .expect("Codex metadata should be valid")
            .with_lifecycle_observer(move |event| {
                observed_events
                    .lock()
                    .expect("events should lock")
                    .push(event);
            });

        // Act
        let metadata = Model::metadata(&model).expect("metadata should be available");
        let completion = ModelWithMetadata::complete_with_metadata(
            &model,
            ModelRequest::new("Extract", person_schema()),
        )
        .await
        .expect("Codex request should complete");

        // Assert
        assert_eq!(metadata.provider(), "openai");
        assert_eq!(metadata.model(), "gpt-test");
        assert_eq!(
            completion.response().output(),
            Some(&json!({ "name": "Ada" }))
        );
        assert!(!events.lock().expect("events should lock").is_empty());
    }
}
