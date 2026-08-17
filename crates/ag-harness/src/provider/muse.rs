use std::env;

use async_trait::async_trait;
use thiserror::Error;

use crate::chat_completion;
use crate::model::{
    Model, ModelClient, ModelCompletion, ModelError, ModelMetadataError, ModelRequest,
    ModelResponse, ModelWithMetadata,
};

const DEFAULT_BASE_URL: &str = "https://api.meta.ai/v1";
const MODEL_API_BASE_URL_ENV: &str = "MODEL_API_BASE_URL";
const MODEL_API_KEY_ENV: &str = "MODEL_API_KEY";

/// Standard Muse Spark 1.2 model whose prompts and completions are not used
/// to train Meta models.
pub const MUSE_SPARK_1_2: &str = "muse-spark-1.2";

/// Discounted Muse Spark 1.2 model that permits Meta to use prompts and
/// completions to train future models.
pub const MUSE_SPARK_1_2_CONTRIBUTOR: &str = "muse-spark-1.2-contributor";

pub(crate) const PROVIDER_NAME: &str = "meta";
pub(crate) const POLICY: chat_completion::ChatCompletionProviderPolicy =
    chat_completion::ChatCompletionProviderPolicy {
        display_name: "Meta Model API",
        structured_output: chat_completion::StructuredOutputMode::JsonSchema,
        telemetry_name: PROVIDER_NAME,
        unsupported_schema_reason: "Muse structured output requires an explicit object root schema",
    };

/// Muse model configured from the standard Model API environment variables.
pub struct Muse {
    client: ModelClient,
}

impl Muse {
    /// Creates a Muse model using `MODEL_API_KEY` and the optional
    /// `MODEL_API_BASE_URL` override.
    ///
    /// # Errors
    ///
    /// Returns [`MuseError`] when `MODEL_API_KEY` is unavailable or `model` is
    /// empty.
    pub fn from_env(model: impl Into<String>) -> Result<Self, MuseError> {
        Self::from_environment(model, |name| env::var(name))
    }

    fn from_environment(
        model: impl Into<String>,
        mut environment: impl FnMut(&str) -> Result<String, env::VarError>,
    ) -> Result<Self, MuseError> {
        let api_key = environment(MODEL_API_KEY_ENV).map_err(MuseError::ApiKey)?;
        let base_url =
            environment(MODEL_API_BASE_URL_ENV).unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        let client = ModelClient::muse(MuseConfig {
            api_key,
            base_url,
            model: model.into(),
        })?;

        Ok(Self { client })
    }
}

#[async_trait]
impl Model for Muse {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        self.client.complete(request).await
    }
}

#[async_trait]
impl ModelWithMetadata for Muse {
    async fn complete_with_metadata(
        &self,
        request: ModelRequest,
    ) -> Result<ModelCompletion, ModelError> {
        self.client.complete_with_metadata(request).await
    }
}

/// Failure returned while configuring Muse from the environment.
#[derive(Debug, Error)]
pub enum MuseError {
    /// `MODEL_API_KEY` is missing or is not valid Unicode.
    #[error("MODEL_API_KEY is unavailable: {0}")]
    ApiKey(#[source] env::VarError),
    /// The selected model identifier is invalid.
    #[error(transparent)]
    Metadata(#[from] ModelMetadataError),
}

/// Configuration for a Muse model served through Meta's Model API.
pub struct MuseConfig {
    /// API key sent as a bearer token.
    pub api_key: String,
    /// API base URL ending in the OpenAI-compatible version path.
    pub base_url: String,
    /// Muse model identifier sent with each request.
    pub model: String,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use wiremock::matchers::{bearer_token, body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::{model, tool};

    fn person_schema_value() -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            },
            "required": ["name"],
            "additionalProperties": false
        })
    }

    fn person_schema() -> crate::OutputSchema {
        crate::OutputSchema::new(person_schema_value()).expect("schema should be valid")
    }

    fn request(prompt: &str) -> model::ModelRequest {
        model::ModelRequest::new(prompt, person_schema())
    }

    fn read_request(prompt: &str) -> model::ModelRequest {
        request(prompt).with_tool(tool::ToolDefinition::read())
    }

    fn read_tool_wire() -> Value {
        let definition = tool::ToolDefinition::read();

        json!({
            "type": "function",
            "function": {
                "description": definition.description(),
                "name": definition.name(),
                "parameters": definition.parameters()
            }
        })
    }

    fn default_environment(name: &str) -> Result<String, env::VarError> {
        if name == MODEL_API_KEY_ENV {
            Ok("test-key".to_string())
        } else {
            Err(env::VarError::NotPresent)
        }
    }

    fn muse(server: &MockServer) -> Muse {
        Muse::from_environment(MUSE_SPARK_1_2, |name| {
            if name == MODEL_API_KEY_ENV {
                Ok("test-key".to_string())
            } else {
                Ok(format!("{}/", server.uri()))
            }
        })
        .expect("fixture environment should be valid")
    }

    fn response_format() -> Value {
        json!({
            "type": "json_schema",
            "json_schema": {
                "name": "ag_harness_output",
                "schema": person_schema_value()
            }
        })
    }

    #[test]
    fn exposes_standard_and_contributor_model_identifiers() {
        // Arrange and Act
        let models = [MUSE_SPARK_1_2, MUSE_SPARK_1_2_CONTRIBUTOR];

        // Assert
        assert_eq!(models, ["muse-spark-1.2", "muse-spark-1.2-contributor"]);
    }

    #[test]
    fn environment_configuration_uses_official_base_url_default() {
        // Arrange and Act
        let muse = Muse::from_environment(MUSE_SPARK_1_2, default_environment)
            .expect("fixture environment should be valid");

        // Assert
        assert_eq!(muse.client.metadata().provider(), "meta");
        assert_eq!(muse.client.metadata().model(), MUSE_SPARK_1_2);
    }

    #[test]
    fn environment_configuration_requires_api_key() {
        // Arrange and Act
        let error = Muse::from_environment(MUSE_SPARK_1_2, |_| Err(env::VarError::NotPresent))
            .err()
            .expect("missing API key should fail");

        // Assert
        assert!(matches!(
            error,
            MuseError::ApiKey(env::VarError::NotPresent)
        ));
    }

    #[test]
    fn environment_configuration_rejects_empty_model() {
        // Arrange and Act
        let error = Muse::from_environment("  ", default_environment)
            .err()
            .expect("empty model should fail");

        // Assert
        assert!(matches!(
            error,
            MuseError::Metadata(ModelMetadataError::EmptyModel)
        ));
    }

    #[test]
    fn metadata_exposes_provider_and_model() {
        // Arrange
        let model = model::ModelClient::muse(MuseConfig {
            api_key: "test-key".to_string(),
            base_url: "https://api.meta.ai/v1".to_string(),
            model: MUSE_SPARK_1_2_CONTRIBUTOR.to_string(),
        })
        .expect("fixture configuration should be valid");

        // Act
        let metadata = model.metadata();

        // Assert
        assert_eq!(metadata.provider(), "meta");
        assert_eq!(metadata.model(), "muse-spark-1.2-contributor");
    }

    #[test]
    fn rejects_empty_model_during_construction() {
        // Arrange
        let config = MuseConfig {
            api_key: "test-key".to_string(),
            base_url: "https://api.meta.ai/v1".to_string(),
            model: "  ".to_string(),
        };

        // Act
        let error = model::ModelClient::muse(config)
            .err()
            .expect("empty model configuration should be rejected");

        // Assert
        assert_eq!(error, model::ModelMetadataError::EmptyModel);
    }

    #[tokio::test]
    async fn completes_native_json_schema_request() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(bearer_token("test-key"))
            .and(body_json(json!({
                "messages": [
                    {"content": "extract the name", "role": "user"}
                ],
                "model": "muse-spark-1.2",
                "response_format": response_format()
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": r#"{"name":"Ada"}"#}
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let model = muse(&server);

        // Act
        let completion = model
            .complete_with_metadata(request("extract the name"))
            .await
            .expect("Muse request should succeed");

        // Assert
        assert_eq!(
            completion.response().output(),
            Some(&json!({ "name": "Ada" }))
        );
        assert_eq!(completion.metadata().finish_reason(), "stop");
    }

    #[tokio::test]
    async fn advertises_and_decodes_read_tool_call() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(bearer_token("test-key"))
            .and(body_json(json!({
                "messages": [
                    {"content": "inspect the manifest", "role": "user"}
                ],
                "model": "muse-spark-1.2",
                "response_format": response_format(),
                "tools": [read_tool_wire()]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "tool_calls",
                    "message": {
                        "content": null,
                        "tool_calls": [{
                            "id": "call_muse_read",
                            "type": "function",
                            "function": {
                                "name": "read",
                                "arguments": r#"{"path":"Cargo.toml","offset":1,"limit":12}"#
                            }
                        }]
                    }
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let model = muse(&server);

        // Act
        let response = model
            .complete(read_request("inspect the manifest"))
            .await
            .expect("Muse read request should decode");

        // Assert
        assert!(response.output().is_none());
        let call = response
            .call()
            .expect("response should contain a tool call");
        assert_eq!(call.id(), "call_muse_read");
        assert_eq!(call.name(), "read");
        assert_eq!(call.arguments().path(), "Cargo.toml");
        assert_eq!(call.arguments().offset(), Some(1));
        assert_eq!(call.arguments().limit(), Some(12));
    }

    #[tokio::test]
    async fn continues_after_read_tool_result() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(bearer_token("test-key"))
            .and(body_json(json!({
                "messages": [
                    {"content": "inspect the manifest", "role": "user"}
                ],
                "model": "muse-spark-1.2",
                "response_format": response_format(),
                "tools": [read_tool_wire()]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "tool_calls",
                    "message": {
                        "content": null,
                        "tool_calls": [{
                            "id": "call_muse_read",
                            "type": "function",
                            "function": {
                                "name": "read",
                                "arguments": r#"{"path":"Cargo.toml"}"#
                            }
                        }]
                    }
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let model = muse(&server);
        let mut request = read_request("inspect the manifest");
        let response = model
            .complete(request.clone())
            .await
            .expect("Muse read request should decode");
        request.record_tool_result(
            response
                .call()
                .expect("response should contain a tool call")
                .clone(),
            r#"{"content":"[package]\nname = \"ag-harness\"","next_offset":null}"#.to_string(),
        );
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(bearer_token("test-key"))
            .and(body_json(json!({
                "messages": [
                    {"content": "inspect the manifest", "role": "user"},
                    {
                        "content": null,
                        "role": "assistant",
                        "tool_calls": [{
                            "function": {
                                "arguments": r#"{"path":"Cargo.toml"}"#,
                                "name": "read"
                            },
                            "id": "call_muse_read",
                            "type": "function"
                        }]
                    },
                    {
                        "content": r#"{"content":"[package]\nname = \"ag-harness\"","next_offset":null}"#,
                        "role": "tool",
                        "tool_call_id": "call_muse_read"
                    }
                ],
                "model": "muse-spark-1.2",
                "response_format": response_format(),
                "tools": [read_tool_wire()]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": r#"{"name":"ag-harness"}"#}
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        // Act
        let response = model
            .complete(request)
            .await
            .expect("Muse continuation should succeed");

        // Assert
        assert_eq!(response.output(), Some(&json!({ "name": "ag-harness" })));
    }

    #[tokio::test]
    async fn retains_local_schema_validation() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": r#"{"name":42}"#}
                }]
            })))
            .mount(&server)
            .await;
        let model = muse(&server);

        // Act
        let error = model
            .complete(request("extract the name"))
            .await
            .expect_err("schema violation should fail");

        // Assert
        assert!(matches!(
            error,
            model::ModelError::SchemaViolation { path, reason }
                if path == "/name" && reason.contains("string")
        ));
    }

    #[tokio::test]
    async fn rejects_schemas_without_explicit_object_root() {
        // Arrange
        let server = MockServer::start().await;
        let model = muse(&server);
        let schema =
            crate::OutputSchema::new(json!({ "type": "array" })).expect("schema should be valid");

        // Act
        let error = model
            .complete(model::ModelRequest::new("list names", schema))
            .await
            .expect_err("schema without an explicit object root should fail");

        // Assert
        assert!(matches!(
            error,
            model::ModelError::UnsupportedOutputSchema { reason }
                if reason == "Muse structured output requires an explicit object root schema"
        ));
        assert!(
            server
                .received_requests()
                .await
                .expect("request recording should be enabled")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn reports_meta_http_failure_without_exposing_the_key() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": {"message": "invalid API key"}
            })))
            .mount(&server)
            .await;
        let model = muse(&server);

        // Act
        let error = model
            .complete(request("hello"))
            .await
            .expect_err("HTTP failure should fail");
        let message = error.to_string();

        // Assert
        assert!(message.contains("Meta Model API returned HTTP 401 Unauthorized"));
        assert!(message.contains("invalid API key"));
        assert!(!message.contains("test-key"));
    }
}
