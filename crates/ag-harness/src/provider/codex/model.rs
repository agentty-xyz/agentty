use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::client::{CodexResponsesClient, CodexResponsesRequest, HttpCodexResponsesClient};
use super::error::CodexClientError;
use crate::lifecycle::LifecycleObserver;
use crate::model::{
    CompletionMetadata, GeneratedResponse, Model, ModelClient, ModelCompletion, ModelError,
    ModelMessage, ModelMetadata, ModelMetadataError, ModelRequest,
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
impl Model for Codex {
    fn metadata(&self) -> Option<ModelMetadata> {
        Some(self.client.metadata().clone())
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        self.client.complete(request).await
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
        validate_strict_output_schema(request.schema().value())?;
        let prompt = CodexPrompt::from_messages(request.messages())?;
        let replay_account_fingerprint = match (
            request.provider_context(),
            prompt.replay_account_fingerprint.as_deref(),
        ) {
            (Some(expected), Some(replayed)) if expected != replayed => {
                return Err(CodexClientError::InvalidReasoningReplay.into_model_error());
            }
            (Some(expected), _) => Some(expected.to_string()),
            (None, replayed) => replayed.map(ToString::to_string),
        };
        let completion = self
            .client
            .complete(CodexResponsesRequest {
                input: prompt.input,
                instructions: prompt.instructions,
                model: self.model.clone(),
                output_schema: request.schema().value().clone(),
                reasoning_effort: request.model_reasoning_effort(),
                replay_account_fingerprint,
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

        let provider_context = completion.account_fingerprint.clone();
        let reasoning_content = persisted_replay(
            completion.account_fingerprint,
            completion.reasoning_content,
            &completion.output,
        )?;

        Ok(GeneratedResponse::Output {
            metadata,
            output: completion.output,
            provider_context,
            reasoning_content: Some(reasoning_content),
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
    pub(super) replay_account_fingerprint: Option<String>,
}

impl CodexPrompt {
    pub(super) fn from_messages(messages: &[ModelMessage]) -> Result<Self, ModelError> {
        let mut input = Vec::new();
        let mut instructions = Vec::new();
        let mut replay_account_fingerprint = None;
        for message in messages {
            match message {
                ModelMessage::Assistant(content) => {
                    input.push(response_message("assistant", "output_text", content));
                }
                ModelMessage::AssistantReasoning {
                    content,
                    reasoning_content,
                } => {
                    let replay = replay_items(reasoning_content, content)?;
                    if replay_account_fingerprint
                        .as_ref()
                        .is_some_and(|fingerprint| fingerprint != &replay.account_fingerprint)
                    {
                        return Err(CodexClientError::InvalidReasoningReplay.into_model_error());
                    }
                    replay_account_fingerprint = Some(replay.account_fingerprint);
                    input.extend(replay.items);
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
            replay_account_fingerprint,
        })
    }
}

#[derive(Deserialize, Serialize)]
struct CodexReplay {
    account_fingerprint: String,
    items: Vec<Value>,
}

#[allow(
    clippy::expect_used,
    reason = "Codex replay contains only strings and serde_json::Value"
)]
fn persisted_replay(
    account_fingerprint: Option<String>,
    reasoning_content: Option<String>,
    content: &str,
) -> Result<String, ModelError> {
    let account_fingerprint = account_fingerprint
        .filter(|fingerprint| !fingerprint.is_empty())
        .ok_or_else(|| CodexClientError::InvalidReasoningReplay.into_model_error())?;
    let items = reasoning_content.map_or_else(
        || Ok(vec![response_message("assistant", "output_text", content)]),
        |reasoning_content| {
            serde_json::from_str(&reasoning_content)
                .map_err(|_| CodexClientError::InvalidReasoningReplay.into_model_error())
        },
    )?;
    validate_replay_items(&items, content)?;

    Ok(serde_json::to_string(&CodexReplay {
        account_fingerprint,
        items,
    })
    .expect("Codex replay values should serialize"))
}

fn replay_items(reasoning_content: &str, content: &str) -> Result<CodexReplay, ModelError> {
    let replay: CodexReplay = serde_json::from_str(reasoning_content)
        .map_err(|_| CodexClientError::InvalidReasoningReplay.into_model_error())?;
    if replay.account_fingerprint.is_empty() {
        return Err(CodexClientError::InvalidReasoningReplay.into_model_error());
    }
    validate_replay_items(&replay.items, content)?;

    Ok(replay)
}

fn validate_replay_items(items: &[Value], content: &str) -> Result<(), ModelError> {
    if items.is_empty()
        || items
            .iter()
            .any(|item| match item.get("type").and_then(Value::as_str) {
                Some("reasoning") => false,
                Some("message") => item.get("role").and_then(Value::as_str) != Some("assistant"),
                _ => true,
            })
        || terminal_output(items).is_none_or(|output| !equivalent_output(&output, content))
    {
        return Err(CodexClientError::InvalidReasoningReplay.into_model_error());
    }

    Ok(())
}

fn terminal_output(items: &[Value]) -> Option<String> {
    items.iter().rev().find_map(|item| {
        if item.get("type").and_then(Value::as_str) != Some("message")
            || !matches!(
                item.get("phase").and_then(Value::as_str),
                None | Some("final_answer")
            )
        {
            return None;
        }
        let output = item
            .get("content")
            .and_then(Value::as_array)?
            .iter()
            .filter(|entry| entry.get("type").and_then(Value::as_str) == Some("output_text"))
            .filter_map(|entry| entry.get("text").and_then(Value::as_str))
            .collect::<String>();

        (!output.trim().is_empty()).then_some(output)
    })
}

fn equivalent_output(actual: &str, expected: &str) -> bool {
    match (
        serde_json::from_str::<Value>(actual),
        serde_json::from_str::<Value>(expected),
    ) {
        (Ok(actual), Ok(expected)) => actual == expected,
        _ => actual == expected,
    }
}

fn validate_strict_output_schema(schema: &Value) -> Result<(), ModelError> {
    if schema.get("type").and_then(Value::as_str) != Some("object") || schema.get("anyOf").is_some()
    {
        return Err(unsupported_schema(
            "Codex structured output requires an object root without `anyOf`",
        ));
    }
    validate_strict_schema_node(schema)
}

fn validate_strict_schema_node(schema: &Value) -> Result<(), ModelError> {
    let Some(schema) = schema.as_object() else {
        return Err(unsupported_schema(
            "Codex structured output does not support boolean schemas",
        ));
    };
    for keyword in [
        "allOf",
        "dependentRequired",
        "dependentSchemas",
        "else",
        "if",
        "not",
        "oneOf",
        "then",
    ] {
        if schema.contains_key(keyword) {
            return Err(unsupported_schema(&format!(
                "Codex structured output does not support `{keyword}`"
            )));
        }
    }
    let object_type = match schema.get("type") {
        Some(Value::String(schema_type)) => schema_type == "object",
        Some(Value::Array(types)) => types.iter().any(|schema_type| schema_type == "object"),
        _ => false,
    };
    if object_type {
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .ok_or_else(|| unsupported_schema("Codex object schemas require `properties`"))?;
        let required = schema
            .get("required")
            .and_then(Value::as_array)
            .ok_or_else(|| unsupported_schema("Codex object schemas require `required`"))?;
        if schema.get("additionalProperties") != Some(&Value::Bool(false)) {
            return Err(unsupported_schema(
                "Codex object schemas require `additionalProperties: false`",
            ));
        }
        if required.len() != properties.len()
            || properties.keys().any(|property| {
                !required
                    .iter()
                    .any(|required| required.as_str() == Some(property))
            })
        {
            return Err(unsupported_schema(
                "Codex object schemas require every property",
            ));
        }
        for property in properties.values() {
            validate_strict_schema_node(property)?;
        }
    }
    if let Some(items) = schema.get("items") {
        validate_strict_schema_node(items)?;
    }
    for keyword in ["anyOf", "$defs", "definitions"] {
        if let Some(children) = schema.get(keyword) {
            match children {
                Value::Array(children) => {
                    for child in children {
                        validate_strict_schema_node(child)?;
                    }
                }
                Value::Object(children) => {
                    for child in children.values() {
                        validate_strict_schema_node(child)?;
                    }
                }
                _ => {
                    return Err(unsupported_schema(
                        "Codex schema composition must contain schemas",
                    ));
                }
            }
        }
    }

    Ok(())
}

fn unsupported_schema(reason: &str) -> ModelError {
    ModelError::UnsupportedOutputSchema {
        reason: reason.to_string(),
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
#[path = "model_test.rs"]
mod tests;
