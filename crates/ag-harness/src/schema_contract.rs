use std::fmt;
use std::io::{self, Write};
use std::sync::Arc;

use jsonschema::error::ValidationErrorKind;
use jsonschema::{Draft, PatternOptions, ReferencingError, Validator};
use serde_json::Value;
use thiserror::Error;

const DIAGNOSTIC_LIMIT_CHARS: usize = 512;
const REGEX_DFA_LIMIT_BYTES: usize = 2 * 1024 * 1024;
const REGEX_SIZE_LIMIT_BYTES: usize = 256 * 1024;
const SCHEMA_LIMIT_BYTES: usize = 256 * 1024;
pub(crate) const RESPONSE_CONTENT_LIMIT_BYTES: usize = 2 * 1024 * 1024;

/// A validated, provider-independent JSON Schema for model output.
#[derive(Clone)]
pub struct OutputSchema {
    schema: Value,
    validator: Arc<Validator>,
}

impl OutputSchema {
    /// Validates and compiles a JSON Schema for structured model output.
    ///
    /// # Errors
    ///
    /// Returns [`OutputSchemaError`] when the schema is oversized, references
    /// an external resource, or is outside the harness's JSON Schema Draft
    /// 2020-12 safety profile.
    pub fn new(schema: Value) -> Result<Self, OutputSchemaError> {
        if schema.to_string().len() > SCHEMA_LIMIT_BYTES {
            return Err(OutputSchemaError::TooLarge);
        }

        let validator = jsonschema::options()
            .with_draft(Draft::Draft202012)
            .with_pattern_options(
                PatternOptions::regex()
                    .size_limit(REGEX_SIZE_LIMIT_BYTES)
                    .dfa_size_limit(REGEX_DFA_LIMIT_BYTES),
            )
            .build(&schema)
            .map_err(|error| {
                if matches!(
                    error.kind(),
                    ValidationErrorKind::Referencing(ReferencingError::Unretrievable { .. })
                ) {
                    return OutputSchemaError::ExternalReference;
                }

                OutputSchemaError::Invalid {
                    reason: bounded_diagnostic(error),
                }
            })?;

        Ok(Self {
            schema,
            validator: Arc::new(validator),
        })
    }

    /// Returns the underlying JSON Schema document.
    pub fn value(&self) -> &Value {
        &self.schema
    }

    pub(crate) fn has_object_root(&self) -> bool {
        let Some(schema_type) = self.schema.get("type") else {
            return false;
        };

        schema_type == "object"
            || schema_type
                .as_array()
                .is_some_and(|types| types.iter().any(|schema_type| schema_type == "object"))
    }

    /// Parses model content as one JSON object and validates it. Content
    /// that wraps the object in Markdown fences or surrounding prose is
    /// accepted when the embedded object parses; the schema still applies.
    pub(crate) fn parse_and_validate(&self, output: &str) -> Result<Value, OutputValidationError> {
        ensure_content_size(output)?;

        let value = match serde_json::from_str(output) {
            Ok(value) => value,
            Err(error) => embedded_object(output)
                .and_then(|embedded| serde_json::from_str(embedded).ok())
                .ok_or_else(|| {
                    OutputValidationError::InvalidJson(invalid_json_diagnostic(output, &error))
                })?,
        };
        self.validate(&value)?;

        Ok(value)
    }

    pub(crate) fn validate_value(&self, output: &Value) -> Result<(), OutputValidationError> {
        let mut writer = ContentSizeWriter { bytes_written: 0 };
        serde_json::to_writer(&mut writer, output).map_err(|_| OutputValidationError::TooLarge)?;
        self.validate(output)
    }

    fn validate(&self, value: &Value) -> Result<(), OutputValidationError> {
        if let Err(error) = self.validator.validate(value) {
            let path = match error.instance_path().as_str() {
                "" => "$".to_string(),
                path => bounded_diagnostic(path),
            };

            return Err(OutputValidationError::SchemaViolation {
                path,
                reason: bounded_diagnostic(error),
            });
        }

        Ok(())
    }
}

struct ContentSizeWriter {
    bytes_written: usize,
}

impl Write for ContentSizeWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() > RESPONSE_CONTENT_LIMIT_BYTES.saturating_sub(self.bytes_written) {
            return Err(io::ErrorKind::WriteZero.into());
        }
        self.bytes_written += buffer.len();

        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl fmt::Debug for OutputSchema {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutputSchema")
            .field("schema", &self.schema)
            .finish_non_exhaustive()
    }
}

impl PartialEq for OutputSchema {
    fn eq(&self, other: &Self) -> bool {
        self.schema == other.schema
    }
}

impl Eq for OutputSchema {}

/// Failure returned while constructing a structured-output schema.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum OutputSchemaError {
    /// The serialized schema exceeds the harness safety limit.
    #[error("output schema exceeds the size limit")]
    TooLarge,
    /// The schema references a resource outside its own document.
    #[error("output schema contains an external reference")]
    ExternalReference,
    /// The document is invalid or outside the harness safety profile.
    #[error("invalid output schema: {reason}")]
    Invalid {
        /// Validator-provided reason the schema is invalid or unsupported.
        reason: String,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum OutputValidationError {
    InvalidJson(String),
    SchemaViolation { path: String, reason: String },
    TooLarge,
}

/// Locates the outermost `{...}` span so fenced or prose-wrapped objects can
/// still be parsed; the caller decides whether that span is valid JSON.
fn embedded_object(output: &str) -> Option<&str> {
    let start = output.find('{')?;
    let end = output.rfind('}')?;

    (end > start).then(|| &output[start..=end])
}

/// Describes malformed content without reproducing it: the parser
/// diagnostic, the leading character class, and the content length.
fn invalid_json_diagnostic(output: &str, error: &serde_json::Error) -> String {
    let leading = output.trim_start().chars().next().map_or_else(
        || "content is empty".to_string(),
        |first| format!("content starts with {first:?}"),
    );

    bounded_diagnostic(format!("{error}; {leading}; {} bytes", output.len()))
}

pub(crate) fn ensure_content_size(output: &str) -> Result<(), OutputValidationError> {
    if output.len() > RESPONSE_CONTENT_LIMIT_BYTES {
        return Err(OutputValidationError::TooLarge);
    }

    Ok(())
}

pub(crate) fn bounded_diagnostic(reason: impl fmt::Display) -> String {
    let reason = reason.to_string();
    let mut characters = reason.chars();
    let mut summary: String = characters.by_ref().take(DIAGNOSTIC_LIMIT_CHARS).collect();
    if characters.next().is_some() {
        summary.push_str(" ...");
    }

    summary
}

#[cfg(test)]
#[path = "schema_contract_test.rs"]
mod tests;
