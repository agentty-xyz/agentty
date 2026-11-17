//! Per-turn request options shared by runs, turns, and model requests: the
//! [`OutputMode`] of the final answer and the [`GenerationParams`] sent to
//! the provider.

use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

use crate::model::{Model, ModelError};
use crate::schema_contract::OutputSchema;

/// Form of a turn's final answer.
///
/// A session stores one mode as its default; a run or turn may select
/// another without changing it. Providers that state the output schema in a
/// leading prompt message (Kimi, Qwen, and OpenAI-compatible servers) change
/// that message when a run selects a mode other than the session default, so
/// such a run, and the next run in the default mode, miss the cached prompt
/// prefix; providers that constrain output natively keep it.
///
/// Sessions store a [`Self::Json`] default as its schema and a [`Self::Text`]
/// default as no schema, so a further variant needs a store migration and a
/// new turn-options snapshot version.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum OutputMode {
    /// A JSON value validated against the schema, returned as
    /// [`crate::RunOutput::Json`].
    Json(OutputSchema),
    /// Plain assistant text, returned as [`crate::RunOutput::Text`]. The
    /// model must support it (see [`crate::Model::supports_text_output`]);
    /// otherwise the turn fails with [`ModelError::UnsupportedOutputMode`].
    Text,
}

impl OutputMode {
    /// Returns the JSON Schema of [`Self::Json`], or `None` for text.
    pub fn schema(&self) -> Option<&OutputSchema> {
        match self {
            Self::Json(schema) => Some(schema),
            Self::Text => None,
        }
    }

    /// Checks that `model` can answer in this mode: that it accepts the
    /// schema, or that it supports text output.
    ///
    /// # Errors
    /// Returns the model's schema rejection, or
    /// [`ModelError::UnsupportedOutputMode`] for text output the model does
    /// not support.
    pub(crate) fn check_model(&self, model: &dyn Model) -> Result<(), ModelError> {
        match self {
            Self::Json(schema) => model.validate_schema(schema),
            Self::Text => Self::require_text_support(model),
        }
    }

    /// Checks only that `model` supports text output when this mode asks for
    /// it; a JSON mode passes without validating its schema.
    ///
    /// # Errors
    /// Returns [`ModelError::UnsupportedOutputMode`] for text output the
    /// model does not support.
    pub(crate) fn require_text_support_if_text(&self, model: &dyn Model) -> Result<(), ModelError> {
        match self {
            Self::Json(_) => Ok(()),
            Self::Text => Self::require_text_support(model),
        }
    }

    fn require_text_support(model: &dyn Model) -> Result<(), ModelError> {
        if model.supports_text_output() {
            return Ok(());
        }

        Err(ModelError::UnsupportedOutputMode {
            reason: "model does not support plain-text output".to_string(),
        })
    }
}

impl From<OutputSchema> for OutputMode {
    fn from(schema: OutputSchema) -> Self {
        Self::Json(schema)
    }
}

/// Provider generation settings for one turn's model requests.
///
/// Every unset parameter keeps the provider's default. Turn-options
/// snapshots and host-request fingerprints record the set parameters as one
/// object, so a later parameter needs no format change.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub struct GenerationParams {
    /// Upper bound on the tokens each model request generates, reasoning
    /// included. Built-in providers send it as `max_output_tokens` to the
    /// Responses API and as `max_tokens` or `max_completion_tokens` to Chat
    /// Completions; a response the bound cuts short fails as incomplete. A
    /// provider that rejects the bound, such as the ChatGPT-plan backend or
    /// the Responses API below 16 tokens, fails the turn before execution
    /// with [`ModelError::UnsupportedGeneration`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<NonZeroU32>,
}

impl GenerationParams {
    /// Bounds the tokens each model request generates.
    #[must_use]
    pub fn with_max_output_tokens(mut self, tokens: NonZeroU32) -> Self {
        self.max_output_tokens = Some(tokens);

        self
    }

    /// Returns whether every parameter keeps the provider's default.
    pub(crate) fn is_default(self) -> bool {
        self == Self::default()
    }
}

#[cfg(test)]
#[path = "request_option_test.rs"]
mod tests;
