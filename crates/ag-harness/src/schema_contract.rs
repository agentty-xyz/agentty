use std::fmt;
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

    pub(crate) fn parse_and_validate(&self, output: &str) -> Result<Value, OutputValidationError> {
        ensure_content_size(output)?;

        let value = serde_json::from_str(output)
            .map_err(|error| OutputValidationError::InvalidJson(bounded_diagnostic(error)))?;
        if let Err(error) = self.validator.validate(&value) {
            let path = match error.instance_path().as_str() {
                "" => "$".to_string(),
                path => bounded_diagnostic(path),
            };

            return Err(OutputValidationError::SchemaViolation {
                path,
                reason: bounded_diagnostic(error),
            });
        }

        Ok(value)
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
#[derive(Debug, Error, Eq, PartialEq)]
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
mod tests {
    use serde_json::json;

    use super::*;

    fn object_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            },
            "required": ["name"],
            "additionalProperties": false
        })
    }

    #[test]
    fn constructs_valid_schema() {
        // Arrange
        let value = object_schema();

        // Act
        let schema = OutputSchema::new(value.clone()).expect("schema should be valid");

        // Assert
        assert_eq!(schema.value(), &value);
        assert!(schema.has_object_root());
        assert_eq!(schema, schema.clone());
        assert!(format!("{schema:?}").starts_with("OutputSchema"));
    }

    #[test]
    fn identifies_object_root_in_type_array() {
        // Arrange
        let value = json!({ "type": ["object", "null"] });

        // Act
        let schema = OutputSchema::new(value).expect("schema should be valid");

        // Assert
        assert!(schema.has_object_root());
    }

    #[test]
    fn identifies_schemas_without_explicit_object_root() {
        // Arrange
        let values = [
            json!({ "type": "array" }),
            json!({ "type": ["array", "null"] }),
            json!({ "$ref": "#/$defs/result", "$defs": { "result": { "type": "object" } } }),
        ];

        // Act
        let schemas = values.map(|value| OutputSchema::new(value).expect("schema should be valid"));

        // Assert
        assert!(schemas.iter().all(|schema| !schema.has_object_root()));
    }

    #[test]
    fn rejects_oversized_schema() {
        // Arrange
        let value = json!({ "description": "x".repeat(SCHEMA_LIMIT_BYTES) });

        // Act
        let error = OutputSchema::new(value).expect_err("oversized schema should fail");

        // Assert
        assert_eq!(error, OutputSchemaError::TooLarge);
        assert_eq!(error.to_string(), "output schema exceeds the size limit");
    }

    #[test]
    fn rejects_invalid_schema() {
        // Arrange
        let value = json!({ "type": "not-a-json-type" });

        // Act
        let error = OutputSchema::new(value).expect_err("invalid schema should fail");

        // Assert
        assert!(matches!(error, OutputSchemaError::Invalid { .. }));
        assert!(error.to_string().starts_with("invalid output schema:"));
    }

    #[test]
    fn accepts_linear_regex_pattern() {
        // Arrange
        let value = json!({
            "type": "string",
            "pattern": "^(a+)+$"
        });

        // Act
        let schema = OutputSchema::new(value).expect("linear regex should compile");

        // Assert
        assert!(schema.parse_and_validate(r#""aaaa""#).is_ok());
    }

    #[test]
    fn rejects_backtracking_regex_pattern() {
        // Arrange
        let value = json!({
            "type": "string",
            "pattern": "(?=unsafe-lookaround)"
        });

        // Act
        let error = OutputSchema::new(value).expect_err("lookaround should be rejected");

        // Assert
        assert!(matches!(
            error,
            OutputSchemaError::Invalid { reason } if reason.contains("regex")
        ));
    }

    #[test]
    fn rejects_regex_exceeding_compiled_size_limit() {
        // Arrange
        let value = json!({
            "type": "string",
            "pattern": "a{100000}"
        });

        // Act
        let error = OutputSchema::new(value).expect_err("oversized regex should be rejected");

        // Assert
        assert!(matches!(error, OutputSchemaError::Invalid { .. }));
    }

    #[test]
    fn rejects_nested_external_reference() {
        // Arrange
        let value = json!({
            "allOf": [
                { "$ref": "https://example.com/schema.json" }
            ]
        });

        // Act
        let error = OutputSchema::new(value).expect_err("external reference should fail");

        // Assert
        assert_eq!(error, OutputSchemaError::ExternalReference);
        assert_eq!(
            error.to_string(),
            "output schema contains an external reference"
        );
    }

    #[test]
    fn rejects_external_dynamic_reference() {
        // Arrange
        let value = json!({
            "$dynamicRef": "https://example.com/schema.json"
        });

        // Act
        let error = OutputSchema::new(value).expect_err("external dynamic reference should fail");

        // Assert
        assert_eq!(error, OutputSchemaError::ExternalReference);
    }

    #[test]
    fn accepts_external_reference_as_literal_instance_data() {
        // Arrange
        let literal = json!({ "$ref": "https://example.com/value" });
        let value = json!({ "const": literal });

        // Act
        let schema = OutputSchema::new(value).expect("literal reference should be valid");
        let output = schema
            .parse_and_validate(r#"{"$ref":"https://example.com/value"}"#)
            .expect("matching literal should validate");

        // Assert
        assert_eq!(output, literal);
    }

    #[test]
    fn rejects_missing_local_reference_as_invalid() {
        // Arrange
        let value = json!({ "$ref": "#/$defs/missing" });

        // Act
        let error = OutputSchema::new(value).expect_err("missing reference should fail");

        // Assert
        assert!(matches!(error, OutputSchemaError::Invalid { .. }));
    }

    #[test]
    fn accepts_nested_local_reference() {
        // Arrange
        let value = json!({
            "$defs": {
                "name": { "type": "string" }
            },
            "type": "object",
            "properties": {
                "name": { "$ref": "#/$defs/name" }
            }
        });

        // Act
        let schema = OutputSchema::new(value).expect("schema should be valid");

        // Assert
        assert!(schema.has_object_root());
    }

    #[test]
    fn validates_root_local_reference() {
        // Arrange
        let value = json!({
            "$defs": {
                "result": {
                    "type": "object",
                    "required": ["name"],
                    "properties": {
                        "name": { "type": "string" }
                    }
                }
            },
            "$ref": "#/$defs/result"
        });

        // Act
        let schema = OutputSchema::new(value).expect("schema should be valid");
        let output = schema
            .parse_and_validate(r#"{"name":"Ada"}"#)
            .expect("output should be valid");

        // Assert
        assert_eq!(output, json!({ "name": "Ada" }));
        assert!(!schema.has_object_root());
    }

    #[test]
    fn parses_valid_structured_output() {
        // Arrange
        let schema = OutputSchema::new(object_schema()).expect("schema should be valid");

        // Act
        let value = schema
            .parse_and_validate(r#"{"name":"Ada"}"#)
            .expect("output should be valid");

        // Assert
        assert_eq!(value, json!({ "name": "Ada" }));
    }

    #[test]
    fn rejects_malformed_structured_output() {
        // Arrange
        let schema = OutputSchema::new(object_schema()).expect("schema should be valid");

        // Act
        let error = schema
            .parse_and_validate("not JSON")
            .expect_err("malformed output should fail");

        // Assert
        assert!(matches!(error, OutputValidationError::InvalidJson(_)));
    }

    #[test]
    fn rejects_oversized_structured_output() {
        // Arrange
        let schema = OutputSchema::new(object_schema()).expect("schema should be valid");
        let output = "x".repeat(RESPONSE_CONTENT_LIMIT_BYTES + 1);

        // Act
        let error = schema
            .parse_and_validate(&output)
            .expect_err("oversized output should fail");

        // Assert
        assert_eq!(error, OutputValidationError::TooLarge);
    }

    #[test]
    fn reports_nested_schema_violation() {
        // Arrange
        let schema = OutputSchema::new(object_schema()).expect("schema should be valid");

        // Act
        let error = schema
            .parse_and_validate(r#"{"name":42}"#)
            .expect_err("schema violation should fail");

        // Assert
        assert!(matches!(
            error,
            OutputValidationError::SchemaViolation { path, reason }
                if path == "/name" && reason.contains("string")
        ));
    }

    #[test]
    fn bounds_long_schema_violation_path() {
        // Arrange
        let schema = OutputSchema::new(json!({
            "type": "object",
            "additionalProperties": { "type": "string" }
        }))
        .expect("schema should be valid");
        let property = "x".repeat(DIAGNOSTIC_LIMIT_CHARS);
        let output = format!(r#"{{"{property}":42}}"#);

        // Act
        let error = schema
            .parse_and_validate(&output)
            .expect_err("schema violation should fail");

        // Assert
        assert!(matches!(
            error,
            OutputValidationError::SchemaViolation { path, reason }
                if path == format!("/{} ...", "x".repeat(DIAGNOSTIC_LIMIT_CHARS - 1))
                    && reason.contains("string")
        ));
    }

    #[test]
    fn reports_root_schema_violation() {
        // Arrange
        let schema = OutputSchema::new(object_schema()).expect("schema should be valid");

        // Act
        let error = schema
            .parse_and_validate("[]")
            .expect_err("root schema violation should fail");

        // Assert
        assert!(matches!(
            error,
            OutputValidationError::SchemaViolation { path, .. } if path == "$"
        ));
    }

    #[test]
    fn reports_missing_required_property() {
        // Arrange
        let schema = OutputSchema::new(object_schema()).expect("schema should be valid");

        // Act
        let error = schema
            .parse_and_validate("{}")
            .expect_err("missing required property should fail");

        // Assert
        assert!(matches!(
            error,
            OutputValidationError::SchemaViolation { reason, .. }
                if reason.contains("required")
        ));
    }

    #[test]
    fn reports_disallowed_additional_property() {
        // Arrange
        let schema = OutputSchema::new(object_schema()).expect("schema should be valid");

        // Act
        let error = schema
            .parse_and_validate(r#"{"name":"Ada","role":"engineer"}"#)
            .expect_err("additional property should fail");

        // Assert
        assert!(matches!(
            error,
            OutputValidationError::SchemaViolation { reason, .. }
                if reason.contains("Additional properties")
        ));
    }

    #[test]
    fn bounds_long_diagnostic() {
        // Arrange
        let reason = "é".repeat(DIAGNOSTIC_LIMIT_CHARS + 1);

        // Act
        let summary = bounded_diagnostic(reason);

        // Assert
        assert_eq!(
            summary,
            format!("{} ...", "é".repeat(DIAGNOSTIC_LIMIT_CHARS))
        );
    }
}
