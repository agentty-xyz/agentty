use jsonschema::Validator;
use serde_json::{Value, json};

use super::{
    MAX_PATCH_BYTES, MAX_PATH_BYTES, MAX_QUERY_BYTES, MAX_TOOL_CALL_ID_BYTES, READ_DESCRIPTION,
    ReadAction, ReadArguments, ReadSide, ToolCall, ToolCallArguments, ToolDefinition,
    WriteArguments,
};
use crate::{model, schema_contract};

#[test]
fn tool_call_from_json_rejects_blank_and_oversized_identifiers() {
    // Arrange
    let identifiers = [
        String::new(),
        " \t\n".to_string(),
        "x".repeat(MAX_TOOL_CALL_ID_BYTES + 1),
        "é".repeat(MAX_TOOL_CALL_ID_BYTES / 2 + 1),
    ];

    // Act
    let results =
        identifiers.map(|id| ToolCall::from_json(id, "read", r#"{"path":"name.txt"}"#, None));

    // Assert
    for result in results {
        let error = result.expect_err("invalid identifier must be rejected");
        assert!(matches!(error, model::ModelError::InvalidToolCallId));
        assert_eq!(error.error_type(), model::ModelErrorType::InvalidToolCall);
        assert_eq!(error.http_status(), None);
        assert_eq!(
            error.to_string(),
            "model returned a blank or oversized tool call identifier"
        );
    }
}

#[test]
fn tool_call_from_json_preserves_identifiers_within_the_byte_limit() {
    // Arrange
    let identifiers = [
        "x".to_string(),
        " provider-id ".to_string(),
        "x".repeat(MAX_TOOL_CALL_ID_BYTES),
        "é".repeat(MAX_TOOL_CALL_ID_BYTES / 2),
    ];

    // Act
    let calls = identifiers.clone().map(|id| {
        ToolCall::from_json(id, "read", r#"{"path":"name.txt"}"#, None)
            .expect("valid identifier must be accepted")
    });

    // Assert
    for (call, expected_id) in calls.iter().zip(identifiers) {
        assert_eq!(call.id(), expected_id);
    }
}

#[test]
fn tool_call_from_json_preserves_read_and_write_payloads() {
    // Arrange
    let inputs = [
        (
            "read",
            r#"{"path":"Cargo.toml"}"#,
            Some("reasoning".to_string()),
        ),
        ("write", r#"{"path":"name.txt","patch":"patch"}"#, None),
    ];

    // Act
    let calls = inputs.clone().map(|(name, arguments, reasoning)| {
        ToolCall::from_json("call-id".to_string(), name, arguments, reasoning)
            .expect("valid built-in call should decode")
    });

    // Assert
    for (call, (name, arguments, reasoning)) in calls.iter().zip(inputs) {
        assert_eq!(call.id(), "call-id");
        assert_eq!(call.name(), name);
        assert_eq!(call.reasoning_content(), reasoning.as_deref());
        assert_eq!(
            serde_json::from_str::<Value>(&call.arguments_json().expect("arguments encode"))
                .expect("encoded arguments are JSON"),
            serde_json::from_str::<Value>(arguments).expect("fixture is JSON")
        );
    }
}

#[test]
fn tool_call_from_json_rejects_unsupported_names_and_invalid_arguments() {
    // Arrange
    let inputs = [
        ("bash", "{}"),
        ("read", "{"),
        ("read", r#"{"path":"../secret"}"#),
        ("read", r#"{"path":"name.txt","limit":0}"#),
        ("write", r#"{"path":"name.txt","patch":""}"#),
        (
            "write",
            r#"{"path":"name.txt","patch":"patch","extra":true}"#,
        ),
    ];

    // Act
    let results = inputs
        .map(|(name, arguments)| ToolCall::from_json("call-id".to_string(), name, arguments, None));

    // Assert
    assert!(matches!(
        results[0],
        Err(model::ModelError::UnsupportedToolName { .. })
    ));
    assert!(
        results[1..]
            .iter()
            .all(|result| matches!(result, Err(model::ModelError::InvalidToolArguments { .. })))
    );
}

#[test]
fn tool_call_from_json_bounds_arguments_and_reasoning() {
    // Arrange
    let oversized = "x".repeat(schema_contract::RESPONSE_CONTENT_LIMIT_BYTES + 1);
    let inputs = [
        (oversized.as_str(), None),
        (r#"{"path":"name.txt"}"#, Some(oversized.clone())),
    ];

    // Act
    let results = inputs.map(|(arguments, reasoning)| {
        ToolCall::from_json("call-id".to_string(), "read", arguments, reasoning)
    });

    // Assert
    assert!(
        results
            .iter()
            .all(|result| matches!(result, Err(model::ModelError::ResponseContentTooLarge)))
    );
}

#[test]
fn tool_call_from_json_retains_correctable_read_action_errors() {
    // Arrange
    let arguments = r#"{"action":"search"}"#;

    // Act
    let call = ToolCall::from_json("call-id".to_string(), "read", arguments, None)
        .expect("structurally valid action should reach tool feedback");

    // Assert
    assert_eq!(
        call.read_arguments()
            .and_then(ReadArguments::validation_error),
        Some("search requires a query and accepts only an optional path and limit")
    );
}

#[test]
fn read_definition_exposes_native_function_contract() {
    // Arrange and Act
    let definition = ToolDefinition::read();
    let validator =
        Validator::new(definition.parameters()).expect("read argument schema should compile");

    // Assert
    assert_eq!(definition.name(), "read");
    assert_eq!(definition.description(), READ_DESCRIPTION);
    assert!(validator.is_valid(&json!({ "path": "Cargo.toml" })));
    assert!(validator.is_valid(&json!({ "action": "file", "path": "Cargo.toml" })));
    assert!(validator.is_valid(&json!({
        "action": "file",
        "path": "crates/ag-harness/src/lib.rs",
        "offset": 1,
        "limit": 12
    })));
    assert!(validator.is_valid(&json!({
        "action": "file",
        "path": "Cargo.toml",
        "offset": u64::MAX,
        "limit": u64::MAX
    })));
    assert!(validator.is_valid(&json!({
        "action": "file",
        "path": "Cargo.toml",
        "offset": 1.0,
        "limit": 1e0
    })));
    assert!(validator.is_valid(&json!({
        "action": "diff",
        "path": null,
        "query": null,
        "side": null,
        "offset": null,
        "limit": null
    })));
}

#[test]
fn read_definition_rejects_invalid_arguments() {
    // Arrange
    let definition = ToolDefinition::read();
    let validator =
        Validator::new(definition.parameters()).expect("read argument schema should compile");
    let offset_above_maximum = serde_json::from_str(
        r#"{"action":"file","path":"Cargo.toml","offset":18446744073709551616}"#,
    )
    .expect("out-of-range offset fixture should be valid JSON");
    let limit_above_maximum = serde_json::from_str(
        r#"{"action":"file","path":"Cargo.toml","limit":18446744073709551616}"#,
    )
    .expect("out-of-range limit fixture should be valid JSON");
    let invalid_arguments = [
        json!({ "action": "file", "path": "" }),
        json!({ "action": "file", "path": "/Cargo.toml" }),
        json!({ "action": "file", "path": "C:\\Cargo.toml" }),
        json!({ "action": "file", "path": "../Cargo.toml" }),
        json!({ "action": "file", "path": ".git/config" }),
        json!({ "action": "file", "path": "nested/.GIT/index" }),
        json!({ "action": "file", "path": "Cargo\0.toml" }),
        json!({ "action": "search", "query": "needle\0suffix" }),
        json!({ "action": "file", "path": "Cargo.toml", "offset": 0 }),
        json!({ "action": "file", "path": "Cargo.toml", "limit": 0 }),
        json!({ "action": "file", "path": "Cargo.toml", "unexpected": true }),
        offset_above_maximum,
        limit_above_maximum,
    ];

    // Act
    let results = invalid_arguments.map(|arguments| validator.is_valid(&arguments));

    // Assert
    assert!(results.into_iter().all(|is_valid| !is_valid));
}

#[test]
fn read_arguments_decode_every_closed_inspection_action() {
    // Arrange
    let values = [
        json!({ "action": "file", "path": "src/lib.rs" }),
        json!({ "action": "list", "path": "src", "limit": 10 }),
        json!({ "action": "search", "query": "Harness", "limit": 5 }),
        json!({ "action": "diff" }),
        json!({
            "action": "show",
            "side": "base",
            "path": "src/lib.rs",
            "offset": 2
        }),
    ];

    // Act
    let arguments = values.map(|value| {
        serde_json::from_value::<ReadArguments>(value).expect("closed read action should decode")
    });

    // Assert
    assert_eq!(arguments[0].action(), ReadAction::File);
    assert_eq!(arguments[1].action(), ReadAction::List);
    assert_eq!(arguments[2].action(), ReadAction::Search);
    assert_eq!(arguments[3].action(), ReadAction::Diff);
    assert_eq!(arguments[4].action(), ReadAction::Show);
    assert_eq!(arguments[2].query(), Some("Harness"));
    assert_eq!(arguments[0].query(), None);
    assert_eq!(arguments[3].path_filter(), None);
    assert_eq!(arguments[4].side(), Some(ReadSide::Base));
    assert_eq!(arguments[0].side(), None);
    assert!(
        arguments
            .iter()
            .all(|arguments| arguments.validation_error().is_none())
    );
}

#[test]
fn read_actions_have_stable_names() {
    // Arrange
    let actions = [
        ReadAction::Diff,
        ReadAction::File,
        ReadAction::List,
        ReadAction::Search,
        ReadAction::Show,
    ];

    // Act
    let names = actions.map(ReadAction::as_str);

    // Assert
    assert_eq!(names, ["diff", "file", "list", "search", "show"]);
}

#[test]
fn read_arguments_retain_schema_valid_action_rejections() {
    // Arrange
    let values = [
        json!({}),
        json!({ "action": "file" }),
        json!({ "action": "list", "offset": 1 }),
        json!({ "action": "search" }),
        json!({ "action": "show", "side": "base" }),
        json!({ "action": "diff", "query": "unexpected" }),
    ];
    let definition = ToolDefinition::read();
    let validator =
        Validator::new(definition.parameters()).expect("read argument schema should compile");

    // Act
    let arguments = values
        .iter()
        .cloned()
        .map(serde_json::from_value::<ReadArguments>)
        .collect::<Result<Vec<_>, _>>()
        .expect("schema-valid arguments should decode for a correctable rejection");

    // Assert
    assert!(values.iter().all(|value| validator.is_valid(value)));
    assert!(
        arguments
            .iter()
            .all(|arguments| arguments.validation_error().is_some())
    );
}

#[test]
fn read_arguments_reject_schema_invalid_input() {
    // Arrange
    let values = [
        json!({ "action": "search", "query": "" }),
        json!({ "action": "search", "query": "needle\0suffix" }),
        json!({ "action": "search", "query": "x".repeat(MAX_QUERY_BYTES + 1) }),
        json!({ "action": "show", "side": "other", "path": "src/lib.rs" }),
        json!({ "action": "unknown" }),
    ];
    let definition = ToolDefinition::read();
    let validator =
        Validator::new(definition.parameters()).expect("read argument schema should compile");

    // Act
    let errors = values
        .iter()
        .cloned()
        .map(serde_json::from_value::<ReadArguments>)
        .collect::<Vec<_>>();

    // Assert
    assert!(values.iter().all(|value| !validator.is_valid(value)));
    assert!(errors.into_iter().all(|result| result.is_err()));
}

#[test]
fn write_definition_exposes_native_function_contract() {
    // Arrange and Act
    let definition = ToolDefinition::write();
    let validator =
        Validator::new(definition.parameters()).expect("write argument schema should compile");

    // Assert
    assert_eq!(definition.name(), "write");
    assert_eq!(
        definition.description(),
        concat!(
            "Apply one unified diff to one repository-relative text file. To create an empty ",
            "file, use only `--- /dev/null` and `+++ b/<path>` headers."
        )
    );
    assert!(validator.is_valid(&json!({
        "path": "src/lib.rs",
        "patch": "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n"
    })));
}

#[test]
fn write_definition_and_arguments_reject_invalid_input() {
    // Arrange
    let definition = ToolDefinition::write();
    let validator =
        Validator::new(definition.parameters()).expect("write argument schema should compile");
    let values = [
        json!({}),
        json!({ "path": "src/lib.rs" }),
        json!({ "path": "src/lib.rs", "patch": "" }),
        json!({ "path": "../lib.rs", "patch": "patch" }),
        json!({ "path": ".git/config", "patch": "patch" }),
        json!({ "path": "nested/.GIT/index", "patch": "patch" }),
        json!({ "path": "src/lib.rs", "patch": "patch", "extra": true }),
        json!({ "path": "a".repeat(MAX_PATH_BYTES + 1), "patch": "patch" }),
        json!({ "path": "src/lib.rs", "patch": "x".repeat(MAX_PATCH_BYTES + 1) }),
    ];

    // Act
    let schema_results = values.clone().map(|value| validator.is_valid(&value));
    let decode_results = values.map(serde_json::from_value::<WriteArguments>);

    // Assert
    assert!(schema_results.into_iter().all(|valid| !valid));
    assert!(decode_results.into_iter().all(|result| result.is_err()));
}

#[test]
fn tool_call_exposes_matching_typed_arguments_and_serialization() {
    // Arrange
    let read_arguments = serde_json::from_value(json!({
        "action": "file",
        "path": "Cargo.toml"
    }))
    .expect("read arguments should decode");
    let write_arguments = serde_json::from_value(json!({
        "path": "src/lib.rs",
        "patch": "patch"
    }))
    .expect("write arguments should decode");
    let read = ToolCall::read(
        "read-id".to_string(),
        read_arguments,
        Some("secret".to_string()),
    );
    let write = ToolCall::write("write-id".to_string(), write_arguments, None);

    // Act
    let read_json = read.arguments_json().expect("read arguments should encode");
    let write_json = write
        .arguments_json()
        .expect("write arguments should encode");

    // Assert
    assert!(read.read_arguments().is_some());
    assert!(read.write_arguments().is_none());
    assert!(write.read_arguments().is_none());
    assert_eq!(read.name(), "read");
    assert_eq!(write.name(), "write");
    let write_arguments = write
        .write_arguments()
        .expect("write arguments should be exposed");
    assert_eq!(write_arguments.path(), "src/lib.rs");
    assert_eq!(write_arguments.patch(), "patch");
    assert_eq!(read_json, r#"{"path":"Cargo.toml"}"#);
    assert_eq!(write_json, r#"{"patch":"patch","path":"src/lib.rs"}"#);
    assert_eq!(read.reasoning_content(), Some("secret"));
    assert!(format!("{read:?}").contains("[REDACTED]"));
    assert!(matches!(
        read.arguments(),
        ToolCallArguments::Read(arguments) if arguments.path() == "Cargo.toml"
    ));
    assert!(matches!(
        write.arguments(),
        ToolCallArguments::Write(arguments)
            if arguments.path() == "src/lib.rs" && arguments.patch() == "patch"
    ));
}

#[test]
fn read_arguments_reject_invalid_repository_paths() {
    // Arrange
    let invalid_paths = [
        "",
        "/Cargo.toml",
        "C:\\Cargo.toml",
        "server\\share",
        "src//lib.rs",
        "src/./lib.rs",
        "../lib.rs",
        ".git",
        ".git/config",
        "nested/.GIT/index",
        "Cargo\0.toml",
    ];

    // Act
    let errors = invalid_paths.map(|path| {
        serde_json::from_value::<ReadArguments>(json!({ "action": "file", "path": path }))
            .expect_err("invalid path should be rejected")
    });

    // Assert
    assert!(
        errors
            .into_iter()
            .all(|error| !error.to_string().is_empty())
    );
}

#[test]
fn read_arguments_treat_optional_null_fields_as_omitted() {
    // Arrange
    let values = [
        json!({ "action": "file", "path": "Cargo.toml" }),
        json!({ "action": "file", "path": "Cargo.toml", "offset": null }),
        json!({ "action": "file", "path": "Cargo.toml", "limit": null }),
        json!({
            "action": "diff",
            "path": null,
            "query": null,
            "side": null,
            "offset": null,
            "limit": null
        }),
    ];

    // Act
    let arguments = values.map(|value| {
        serde_json::from_value::<ReadArguments>(value)
            .expect("optional null fields should decode as omissions")
    });

    // Assert
    assert!(
        arguments
            .iter()
            .all(|arguments| arguments.offset().is_none())
    );
    assert!(
        arguments
            .iter()
            .all(|arguments| arguments.limit().is_none())
    );
    assert_eq!(arguments[3].action(), ReadAction::Diff);
    assert_eq!(arguments[3].path_filter(), None);
}

#[test]
fn read_arguments_accept_maximum_ranges() {
    // Arrange
    let value = json!({
        "action": "file",
        "path": "Cargo.toml",
        "offset": u64::MAX,
        "limit": u64::MAX
    });

    // Act
    let arguments =
        serde_json::from_value::<ReadArguments>(value).expect("maximum u64 ranges should decode");

    // Assert
    assert_eq!(arguments.offset(), Some(u64::MAX));
    assert_eq!(arguments.limit(), Some(u64::MAX));
}

#[test]
fn read_arguments_accept_integral_decimal_and_exponent_ranges() {
    // Arrange
    let values = [
        (
            r#"{"action":"file","path":"Cargo.toml","offset":1.0,"limit":1e0}"#,
            (1, 1),
        ),
        (
            r#"{"action":"file","path":"Cargo.toml","offset":1e2,"limit":100e-2}"#,
            (100, 1),
        ),
        (
            r#"{"action":"file","path":"Cargo.toml","offset":18446744073709551615.0,"limit":18446744073709551615e0}"#,
            (u64::MAX, u64::MAX),
        ),
    ];

    // Act
    let arguments = values.map(|(value, expected)| {
        serde_json::from_str::<ReadArguments>(value)
            .map(|arguments| (arguments, expected))
            .expect("integral numeric forms should decode")
    });

    // Assert
    assert!(arguments.iter().all(|(arguments, expected)| {
        arguments.offset() == Some(expected.0) && arguments.limit() == Some(expected.1)
    }));
}

#[test]
fn read_arguments_reject_non_integral_or_out_of_range_numbers() {
    // Arrange
    let values = [
        r#"{"action":"file","path":"Cargo.toml","offset":-1}"#,
        r#"{"action":"file","path":"Cargo.toml","limit":1.5}"#,
        r#"{"action":"file","path":"Cargo.toml","limit":1e-1}"#,
        r#"{"action":"file","path":"Cargo.toml","offset":18446744073709551616}"#,
        r#"{"action":"file","path":"Cargo.toml","offset":100000000000000000000}"#,
        r#"{"action":"file","path":"Cargo.toml","offset":1e999999999999999999999}"#,
    ];

    // Act
    let errors = values.map(|value| {
        serde_json::from_str::<ReadArguments>(value)
            .expect_err("non-integral or out-of-range number should fail")
    });

    // Assert
    assert!(
        errors
            .into_iter()
            .all(|error| !error.to_string().is_empty())
    );
}
