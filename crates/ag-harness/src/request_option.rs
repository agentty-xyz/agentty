//! Per-turn request options shared by runs, turns, and model requests: the
//! [`OutputMode`] of the final answer and the [`GenerationParams`] sent to
//! the provider.

use std::num::NonZeroU32;

use crate::model::{Model, ModelError};
use crate::schema_contract::OutputSchema;

/// Form of a turn's final answer.
///
/// A session stores one mode as its default; a run or turn may select
/// another without changing it.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum OutputMode {
    /// A JSON value validated against the schema, returned as
    /// [`crate::RunOutput::Json`].
    Json(OutputSchema),
    /// Plain assistant text, returned as [`crate::RunOutput::Text`]. The
    /// model must support it (see [`crate::Model::supports_text_output`]);
    /// otherwise the turn fails with an unsupported-output error.
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
    /// [`ModelError::UnsupportedOutputSchema`] for text output the model
    /// does not support.
    pub(crate) fn check_model(&self, model: &dyn Model) -> Result<(), ModelError> {
        match self {
            Self::Json(schema) => model.validate_schema(schema),
            Self::Text => Self::check_text(model),
        }
    }

    /// Checks that `model` supports text output when this mode asks for it.
    ///
    /// # Errors
    /// Returns [`ModelError::UnsupportedOutputSchema`] for text output the
    /// model does not support.
    pub(crate) fn check_text_support(&self, model: &dyn Model) -> Result<(), ModelError> {
        match self {
            Self::Json(_) => Ok(()),
            Self::Text => Self::check_text(model),
        }
    }

    fn check_text(model: &dyn Model) -> Result<(), ModelError> {
        if model.supports_text_output() {
            return Ok(());
        }

        Err(ModelError::UnsupportedOutputSchema {
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
/// Every unset parameter keeps the provider's default.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct GenerationParams {
    /// Upper bound on the tokens each model request generates, reasoning
    /// included. Built-in providers send it as `max_output_tokens` to the
    /// Responses API and as `max_tokens` or `max_completion_tokens` to Chat
    /// Completions; a response the bound cuts short fails as incomplete.
    pub max_output_tokens: Option<NonZeroU32>,
}

impl GenerationParams {
    /// Bounds the tokens each model request generates.
    #[must_use]
    pub fn with_max_output_tokens(mut self, tokens: NonZeroU32) -> Self {
        self.max_output_tokens = Some(tokens);

        self
    }
}
