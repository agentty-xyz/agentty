//! Generic function tools advertised to a model.

use std::fmt;

use serde_json::Value;

use crate::model::ModelError;
use crate::schema;

/// One function the model may call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolDefinition {
    description: String,
    name: String,
    parameters: Value,
}

impl ToolDefinition {
    /// Creates a generic function definition.
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: Value) -> Self {
        Self {
            description: description.into(),
            name: name.into(),
            parameters,
        }
    }

    /// Returns the function description.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Returns the function name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the JSON Schema for function arguments.
    pub fn parameters(&self) -> &Value {
        &self.parameters
    }
}

/// One model-requested function call. Execution belongs to the caller.
#[derive(Clone, Eq, PartialEq)]
pub struct ToolCall {
    arguments: Value,
    id: String,
    name: String,
    reasoning_content: Option<String>,
}

impl fmt::Debug for ToolCall {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolCall")
            .field("arguments", &self.arguments)
            .field("id", &self.id)
            .field("name", &self.name)
            .field(
                "reasoning_content",
                &self.reasoning_content.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

impl ToolCall {
    /// Validates and decodes one function call.
    ///
    /// # Errors
    /// Returns a typed error for an invalid call identifier or arguments.
    pub fn from_json(
        id: String,
        name: &str,
        arguments: &str,
        reasoning_content: Option<String>,
    ) -> Result<Self, ModelError> {
        if id.trim().is_empty() || id.len() > 1024 {
            return Err(ModelError::InvalidToolCallId);
        }
        schema::ensure_content_size(arguments).map_err(ModelError::from)?;
        if let Some(reasoning_content) = &reasoning_content {
            schema::ensure_content_size(reasoning_content).map_err(ModelError::from)?;
        }
        let arguments: Value =
            serde_json::from_str(arguments).map_err(|error| ModelError::InvalidToolArguments {
                reason: schema::bounded_diagnostic(error),
            })?;
        if !arguments.is_object() {
            return Err(ModelError::InvalidToolArguments {
                reason: "function arguments must be a JSON object".to_string(),
            });
        }

        Ok(Self {
            arguments,
            id,
            name: name.to_string(),
            reasoning_content,
        })
    }

    /// Returns parsed function arguments.
    pub fn arguments(&self) -> &Value {
        &self.arguments
    }

    /// Returns the provider's call identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the function name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns arguments serialized for provider history replay.
    pub fn arguments_json(&self) -> String {
        self.arguments.to_string()
    }

    /// Returns provider-specific reasoning content for history replay.
    pub fn reasoning_content(&self) -> Option<&str> {
        self.reasoning_content.as_deref()
    }
}

#[cfg(test)]
#[path = "tool_test.rs"]
mod tests;
