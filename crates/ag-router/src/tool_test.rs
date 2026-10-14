use serde_json::json;

use crate::model::ModelError;
use crate::tool::{ToolCall, ToolDefinition};

#[test]
fn generic_tool_definition_exposes_wire_fields() {
    // Arrange
    let parameters = json!({"type":"object"});

    // Act
    let definition = ToolDefinition::new("lookup", "Look up an ID", parameters.clone());

    // Assert
    assert_eq!(definition.name(), "lookup");
    assert_eq!(definition.description(), "Look up an ID");
    assert_eq!(definition.parameters(), &parameters);
}

#[test]
fn parses_and_replays_generic_function_call() {
    // Arrange
    let arguments = r#"{"id":7}"#;

    // Act
    let call = ToolCall::from_json(
        "call-1".to_string(),
        "lookup",
        arguments,
        Some("private thought".to_string()),
    )
    .expect("valid call");

    // Assert
    assert_eq!(call.id(), "call-1");
    assert_eq!(call.name(), "lookup");
    assert_eq!(call.arguments(), Some(&json!({"id":7})));
    assert_eq!(call.malformed_reason(), None);
    assert_eq!(call.arguments_json(), arguments);
    assert_eq!(call.wire_arguments(), arguments);
    assert_eq!(call.reasoning_content(), Some("private thought"));
    assert!(!format!("{call:?}").contains("private thought"));
}

#[test]
fn rejects_invalid_function_calls_before_returning_them() {
    // Arrange
    let oversized = "x".repeat(crate::schema::RESPONSE_CONTENT_LIMIT_BYTES + 1);

    // Act / Assert
    assert!(matches!(
        ToolCall::from_json(" ".to_string(), "lookup", "{}", None),
        Err(ModelError::InvalidToolCallId)
    ));
    assert!(matches!(
        ToolCall::from_json("x".repeat(1025), "lookup", "{}", None),
        Err(ModelError::InvalidToolCallId)
    ));
    assert!(matches!(
        ToolCall::from_json("id".to_string(), "lookup", &oversized, None),
        Err(ModelError::ResponseContentTooLarge)
    ));
    assert!(matches!(
        ToolCall::from_json("id".to_string(), "lookup", "{}", Some(oversized)),
        Err(ModelError::ResponseContentTooLarge)
    ));
}

#[test]
fn keeps_malformed_arguments_verbatim_and_wraps_them_on_the_wire() {
    // Arrange
    let cases = [
        (r#"{"path src/": "a.rs""#, "EOF while parsing"),
        ("[]", "function arguments must be a JSON object"),
    ];

    for (raw, reason) in cases {
        // Act
        let call =
            ToolCall::from_json("id".to_string(), "lookup", raw, None).expect("malformed call");

        // Assert
        assert_eq!(call.arguments(), None);
        assert!(
            call.malformed_reason()
                .is_some_and(|actual| actual.contains(reason))
        );
        assert_eq!(call.arguments_json(), raw);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&call.wire_arguments()).expect("wire JSON"),
            json!({"malformed_arguments": raw})
        );
    }
}
