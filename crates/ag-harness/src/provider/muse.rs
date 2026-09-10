use std::env;

use async_trait::async_trait;

use super::catalog::{ModelConfiguration, ModelConfigurationError, ModelProvider};
use crate::lifecycle::LifecycleObserver;
use crate::model::{
    Model, ModelClient, ModelCompletion, ModelError, ModelMetadata, ModelRequest, ReasoningEffort,
};
use crate::{chat_completion, telemetry};

pub(crate) const DEFAULT_BASE_URL: &str = "https://api.meta.ai/v1";
pub(crate) const MODEL_API_BASE_URL_ENV: &str = "MODEL_API_BASE_URL";
pub(crate) const MODEL_API_KEY_ENV: &str = "MODEL_API_KEY";

/// Standard Muse Spark 1.3 model whose prompts and completions are not used
/// to train Meta models.
pub const MUSE_SPARK_1_3: &str = "muse-spark-1.3";

/// Discounted Muse Spark 1.3 model that permits Meta to use prompts and
/// completions to train future models.
pub const MUSE_SPARK_1_3_CONTRIBUTOR: &str = "muse-spark-1.3-contributor";

pub(crate) const POLICY: chat_completion::ChatCompletionProviderPolicy =
    chat_completion::ChatCompletionProviderPolicy {
        display_name: "Meta Model API",
        reasoning_format: chat_completion::ReasoningFormat::Effort(reasoning_effort_name),
        response_format_with_tools: true,
        structured_output: chat_completion::StructuredOutputMode::JsonSchema,
        telemetry_name: telemetry::PROVIDER_META,
        unsupported_schema_reason: "Muse structured output requires an explicit object root schema",
    };

fn reasoning_effort_name(reasoning_effort: ReasoningEffort) -> &'static str {
    match reasoning_effort {
        ReasoningEffort::Max => ReasoningEffort::XHigh.as_str(),
        reasoning_effort => reasoning_effort.as_str(),
    }
}

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
    /// Returns [`ModelConfigurationError`] when the provider environment or
    /// model identifier is invalid.
    pub fn from_env(model: impl Into<String>) -> Result<Self, ModelConfigurationError> {
        Self::from_environment(model, |name| env::var(name))
    }

    /// Sends metadata-only request lifecycle events to `observer`.
    #[must_use]
    pub fn with_lifecycle_observer(mut self, observer: impl LifecycleObserver + 'static) -> Self {
        self.client = self.client.with_lifecycle_observer(observer);

        self
    }

    fn from_environment(
        model: impl Into<String>,
        environment: impl FnMut(&str) -> Result<String, env::VarError>,
    ) -> Result<Self, ModelConfigurationError> {
        let client = ModelConfiguration::new(ModelProvider::Muse, model)
            .client_from_environment(environment)?;

        Ok(Self { client })
    }
}

#[async_trait]
impl Model for Muse {
    fn metadata(&self) -> Option<ModelMetadata> {
        Some(self.client.metadata().clone())
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        self.client.complete(request).await
    }
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
#[path = "muse_test.rs"]
mod tests;
