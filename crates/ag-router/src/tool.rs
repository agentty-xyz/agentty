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

/// Key of the JSON object that carries malformed arguments on the wire.
///
/// Chat Completions providers reject a replayed assistant tool call whose
/// arguments are not a JSON object, so malformed text is sent wrapped in an
/// object under this key instead of verbatim.
const MALFORMED_ARGUMENTS_KEY: &str = "malformed_arguments";

/// One model-requested function call. Execution belongs to the caller.
///
/// A call whose arguments are not a JSON object is kept with its raw text and
/// decoding failure, so the caller can answer it with an error result and
/// replay it in later requests.
#[derive(Clone, Eq, PartialEq)]
pub struct ToolCall {
    arguments: Arguments,
    arguments_json: String,
    assistant_text: Option<String>,
    id: String,
    name: String,
    reasoning_content: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Arguments {
    Object(Value),
    Malformed { reason: String },
}

impl fmt::Debug for ToolCall {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolCall")
            .field("arguments", &self.arguments)
            .field("arguments_json", &self.arguments_json)
            .field(
                "assistant_text",
                &self.assistant_text.as_ref().map(|_| "[REDACTED]"),
            )
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
    /// Arguments that are not a JSON object are kept verbatim as malformed
    /// rather than rejected; see [`Self::malformed_reason`].
    ///
    /// # Errors
    /// Returns a typed error for an invalid call identifier or oversized
    /// arguments or reasoning.
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
        let arguments_json = arguments.to_string();
        let arguments = match serde_json::from_str::<Value>(arguments) {
            Ok(value) if value.is_object() => Arguments::Object(value),
            Ok(_) => Arguments::Malformed {
                reason: "function arguments must be a JSON object".to_string(),
            },
            Err(error) => Arguments::Malformed {
                reason: schema::bounded_diagnostic(error),
            },
        };

        Ok(Self {
            arguments,
            arguments_json,
            assistant_text: None,
            id,
            name: name.to_string(),
            reasoning_content,
        })
    }

    /// Attaches the assistant text the model wrote beside its calls, such as
    /// running notes on what it found so far. Text that is empty or only
    /// whitespace is not kept.
    ///
    /// A response's text belongs to its first call, like its reasoning, and
    /// is replayed as the `content` of that assistant tool-call message.
    ///
    /// # Errors
    /// Returns [`ModelError::ResponseContentTooLarge`] for oversized text.
    pub fn with_assistant_text(mut self, text: Option<String>) -> Result<Self, ModelError> {
        if let Some(text) = &text {
            schema::ensure_content_size(text).map_err(ModelError::from)?;
        }
        self.assistant_text = text.filter(|text| !text.trim().is_empty());

        Ok(self)
    }

    /// Returns the parsed arguments object, or `None` when the arguments
    /// were malformed.
    pub fn arguments(&self) -> Option<&Value> {
        match &self.arguments {
            Arguments::Object(value) => Some(value),
            Arguments::Malformed { .. } => None,
        }
    }

    /// Returns why the arguments are not a JSON object, or `None` when they
    /// parsed.
    pub fn malformed_reason(&self) -> Option<&str> {
        match &self.arguments {
            Arguments::Object(_) => None,
            Arguments::Malformed { reason, .. } => Some(reason),
        }
    }

    /// Returns the provider's call identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the function name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the arguments text exactly as the model sent it.
    pub fn arguments_json(&self) -> String {
        self.arguments_json.clone()
    }

    /// Returns the arguments sent when replaying this call to a provider:
    /// the original text when it is a JSON object, otherwise that text
    /// wrapped under [`MALFORMED_ARGUMENTS_KEY`].
    pub(crate) fn wire_arguments(&self) -> String {
        match &self.arguments {
            Arguments::Object(_) => self.arguments_json.clone(),
            Arguments::Malformed { .. } => {
                serde_json::json!({ MALFORMED_ARGUMENTS_KEY: self.arguments_json }).to_string()
            }
        }
    }

    /// Returns provider-specific reasoning content for history replay.
    pub fn reasoning_content(&self) -> Option<&str> {
        self.reasoning_content.as_deref()
    }

    /// Returns the assistant text written beside this call, if any.
    pub fn assistant_text(&self) -> Option<&str> {
        self.assistant_text.as_deref()
    }
}

#[cfg(test)]
#[path = "tool_test.rs"]
mod tests;
