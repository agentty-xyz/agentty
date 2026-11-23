//! Session protocol unit tests for stored defaults and the conversion of
//! stored option failures into session errors.

use serde_json::json;

use crate::request_option::OutputMode;
use crate::session::{NewSession, SessionError};
use crate::turn_options_snapshot::StoredTurnOptionsError;
use crate::{OutputSchema, OutputSchemaError};

#[test]
fn new_sessions_expose_their_default_schema_only_for_json_output() {
    // Arrange
    let schema = OutputSchema::new(json!({"type": "object"})).expect("schema");

    // Act
    let json = NewSession::new("json", schema.clone());
    let text = NewSession::new("text", OutputMode::Text);

    // Assert
    assert_eq!(json.output.schema(), Some(&schema));
    assert_eq!(text.output.schema(), None);
}

#[test]
fn stored_option_failures_become_invalid_data_or_schema_errors() {
    // Arrange
    let decoding = serde_json::from_str::<serde_json::Value>("{").expect_err("invalid JSON");
    let failures = [
        StoredTurnOptionsError::InvalidData {
            reason: "unknown version".into(),
        },
        StoredTurnOptionsError::Json(decoding),
        StoredTurnOptionsError::Schema(OutputSchemaError::TooLarge),
    ];

    // Act
    let errors = failures.map(SessionError::from);

    // Assert
    assert!(matches!(
        &errors,
        [
            SessionError::InvalidData { reason: version },
            SessionError::InvalidData { reason: json },
            SessionError::Schema(OutputSchemaError::TooLarge),
        ] if version == "unknown version"
            && json.starts_with("invalid persistent message JSON")
    ));
}
