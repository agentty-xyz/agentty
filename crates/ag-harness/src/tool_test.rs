//! Tool call decoding, `bash`, `read`, and `write` definitions, and argument
//! validation tests.

use jsonschema::Validator;
use serde_json::{Value, json};

use super::{
    Builtin, MAX_PATCH_BYTES, MAX_PATH_BYTES, MAX_QUERY_BYTES, MAX_TOOL_CALL_ID_BYTES,
    READ_DESCRIPTION, ReadAction, ReadArguments, ReadSide, Tool, ToolCall, ToolCallArguments,
    ToolDefinition, ToolDefinitionError, WriteArguments,
};
use crate::bash::Enforcement;
use crate::{ComparisonBase, model, schema_contract};

#[test]
fn every_deprecated_tool_maps_to_the_built_in_of_its_name() {
    // Arrange
    let tools = [Tool::Read, Tool::Write, Tool::Bash];

    // Act
    let builtins = tools.map(Builtin::from);
    let restored = builtins.map(Tool::from);

    // Assert
    assert_eq!(builtins, Builtin::ALL);
    assert_eq!(restored, tools);
    for (tool, builtin) in tools.into_iter().zip(builtins) {
        assert_eq!(tool.name(), builtin.name());
        assert_eq!(Builtin::named(tool.name()), Some(builtin));
    }
    assert_eq!(Builtin::named("fetch"), None);
}

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
fn tool_call_from_json_rejects_empty_names_and_keeps_malformed_arguments() {
    // Arrange
    let inputs = [
        ("", "{}"),
        ("read", "{"),
        ("read", r#"{"path":"name.txt","limit":0}"#),
        ("write", r#"{"path":"../secret","patch":"patch"}"#),
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
    for (result, (name, raw)) in results[1..].iter().zip(&inputs[1..]) {
        let call = result.as_ref().expect("malformed call is kept");
        let malformed = call.malformed_arguments().expect("malformed arguments");
        assert_eq!((call.name(), malformed.raw()), (*name, *raw));
        assert_eq!(malformed.tool().name(), *name);
        assert_eq!(call.arguments_json().expect("raw"), *raw);
        assert!(call.read_arguments().is_none() && call.write_arguments().is_none());
    }
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
        json!({ "action": "file", "path": "Cargo.toml", "offset": "500" }),
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
            "Apply one unified diff to one repository-relative text file. Hunk line counts are ",
            "taken from the hunk body. A hunk whose context and removed lines do not match at ",
            "its `@@` start line applies only where they match exactly one place in the file; ",
            "copy those lines exactly and add context when they repeat. A hunk with only `+` ",
            "lines needs a `-N,0` header to insert after line N. To create an empty file, use ",
            "only `--- /dev/null` and `+++ b/<path>` headers."
        )
    );
    assert!(validator.is_valid(&json!({
        "path": "src/lib.rs",
        "patch": "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n"
    })));
}

#[test]
fn bash_definition_states_exactly_the_executor_enforcement() {
    // Arrange
    let limits = " Output and duration are bounded; long output keeps each stream's start and \
                  end. Applied writes are not rolled back.";

    // Act
    let unsandboxed = ToolDefinition::bash(Enforcement::None);
    let file_system = ToolDefinition::bash(Enforcement::FileSystem);
    let full = ToolDefinition::bash(Enforcement::FileSystemAndNetwork);

    // Assert
    assert_eq!(
        unsandboxed.description(),
        format!(
            "Run Bash directly on the host without a sandbox. Commands run with the host \
             process's own access; writes and network access are not restricted by the \
             harness.{limits}"
        )
    );
    assert_eq!(
        file_system.description(),
        format!(
            "Run Bash inside a host filesystem sandbox. Workspace reads are allowed; writes \
             require host grants. Network access is not restricted by the harness.{limits}"
        )
    );
    assert_eq!(
        full.description(),
        format!(
            "Run Bash inside the host sandbox. Workspace reads are allowed; writes require host \
             grants. Network access is denied.{limits}"
        )
    );
    assert!(
        [&unsandboxed, &file_system, &full]
            .iter()
            .all(|definition| definition.name() == "bash"
                && definition.parameters() == full.parameters())
    );
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
fn read_arguments_retain_invalid_repository_paths_for_feedback() {
    // Arrange
    let invalid_paths = [
        "",
        ".",
        "/",
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
    let arguments = invalid_paths.map(|path| {
        serde_json::from_value::<ReadArguments>(json!({ "action": "file", "path": path }))
            .expect("invalid path should reach tool feedback")
    });

    // Assert
    for (arguments, path) in arguments.iter().zip(invalid_paths) {
        assert_eq!(arguments.path(), path);
        assert!(
            arguments
                .validation_error()
                .is_some_and(|error| error.contains("omit it to inspect the repository root"))
        );
    }
}

#[test]
fn read_arguments_reject_oversized_paths_before_feedback() {
    // Arrange
    let oversized_paths = ["a".repeat(MAX_PATH_BYTES + 1), "../".repeat(MAX_PATH_BYTES)];

    // Act
    let errors = oversized_paths.map(|path| {
        serde_json::from_value::<ReadArguments>(json!({ "action": "file", "path": path }))
            .expect_err("oversized path should fail decoding")
    });

    // Assert
    for error in errors {
        assert!(error.to_string().contains("path exceeds the byte limit"));
    }
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

#[test]
fn read_arguments_accept_canonical_decimal_integer_strings() {
    // Arrange
    let values = [
        (
            r#"{"action":"file","path":"Cargo.toml","offset":"500","limit":"200"}"#,
            (500, 200),
        ),
        (
            r#"{"action":"show","path":"Cargo.toml","side":"head","offset":"1","limit":"18446744073709551615"}"#,
            (1, u64::MAX),
        ),
    ];

    // Act
    let arguments = values.map(|(value, expected)| {
        serde_json::from_str::<ReadArguments>(value)
            .map(|arguments| (arguments, expected))
            .expect("canonical integer strings should decode")
    });

    // Assert
    assert!(arguments.iter().all(|(arguments, expected)| {
        arguments.validation_error().is_none()
            && arguments.offset() == Some(expected.0)
            && arguments.limit() == Some(expected.1)
    }));
    assert_eq!(
        serde_json::to_value(&arguments[0].0).expect("arguments should serialize"),
        json!({ "path": "Cargo.toml", "offset": 500, "limit": 200 })
    );
}

#[test]
fn read_arguments_reject_non_canonical_or_out_of_range_integer_strings() {
    // Arrange
    let values = [
        ("\"\"", "decimal digits"),
        ("\"abc\"", "decimal digits"),
        ("\"-1\"", "decimal digits"),
        ("\"+1\"", "decimal digits"),
        ("\"1.0\"", "decimal digits"),
        ("\"1e2\"", "decimal digits"),
        ("\" 5\"", "decimal digits"),
        ("\"5 \"", "decimal digits"),
        ("\"05\"", "decimal digits"),
        ("\"0\"", "from 1 through u64::MAX"),
        ("\"18446744073709551616\"", "from 1 through u64::MAX"),
        ("true", "did not match"),
    ];

    // Act
    let errors = values.map(|(value, expected)| {
        let error = serde_json::from_str::<ReadArguments>(&format!(
            r#"{{"action":"file","path":"Cargo.toml","limit":{value}}}"#
        ))
        .expect_err("invalid integer string should fail");

        (error.to_string(), expected)
    });

    // Assert
    for (error, expected) in errors {
        assert!(
            error.contains(expected),
            "{error} should mention {expected}"
        );
    }
}

#[test]
fn tool_call_from_json_accepts_read_ranges_sent_as_strings() {
    // Arrange
    let arguments = r#"{"path":"src/lib.rs","offset":"500","limit":"200"}"#;

    // Act
    let call = ToolCall::from_json("call-1".to_string(), "read", arguments, None)
        .expect("string ranges should decode");

    // Assert
    let read = call.read_arguments().expect("call should be a read");
    assert_eq!(read.offset(), Some(500));
    assert_eq!(read.limit(), Some(200));
    assert_eq!(
        call.arguments_json().expect("arguments should serialize"),
        r#"{"limit":200,"offset":500,"path":"src/lib.rs"}"#
    );
}

#[test]
fn read_capabilities_and_description_follow_the_selected_base() {
    // Arrange
    let base = ComparisonBase::fixture("repo");
    let comparison = json!({"action":"diff"});
    let show_base = json!({"action":"show", "side":"base", "path":"name.txt"});
    let show_head = json!({"action":"show", "side":"head", "path":"name.txt"});

    // Act
    let enabled = ToolDefinition::read_with_comparison_base(Some(&base));
    let disabled = ToolDefinition::read_with_comparison_base(None);
    let enabled_schema = Validator::new(enabled.parameters()).expect("enabled schema");
    let disabled_schema = Validator::new(disabled.parameters()).expect("disabled schema");

    // Assert
    assert!(enabled.description().contains(base.oid()));
    assert!(enabled_schema.is_valid(&comparison));
    assert!(enabled_schema.is_valid(&show_base));
    assert!(!disabled_schema.is_valid(&comparison));
    assert!(!disabled_schema.is_valid(&show_base));
    assert!(disabled_schema.is_valid(&show_head));
    assert!(disabled.description().contains("unavailable"));
    assert!(
        serde_json::from_value::<ReadArguments>(json!({"action":"diff", "revision":"HEAD"}))
            .is_err()
    );
}

#[test]
fn malformed_arguments_keep_the_decoding_failure() {
    // Arrange
    let arguments = r#"{"path src/":"lib.rs"}"#;

    // Act
    let call = ToolCall::from_json("call-id".to_string(), "read", arguments, None)
        .expect("kept as malformed");

    // Assert
    let malformed = call.malformed_arguments().expect("malformed");
    assert_eq!(malformed.tool(), Tool::Read);
    assert!(malformed.reason().starts_with("unknown field `path src/`"));
    assert!(matches!(call.arguments(), ToolCallArguments::Malformed(found) if found == malformed));
}

#[test]
fn keeps_bounded_assistant_text_and_drops_blank_text() {
    // Arrange
    let call =
        || ToolCall::from_json("id".to_string(), "read", r#"{"path":"a.rs"}"#, None).expect("read");
    let oversized = "x".repeat(schema_contract::RESPONSE_CONTENT_LIMIT_BYTES + 1);

    // Act
    let noted = call()
        .with_assistant_text(Some("a.rs defines alpha_one".to_string()))
        .expect("bounded text");
    let blank = call()
        .with_assistant_text(Some("\t ".to_string()))
        .expect("blank text");
    let error = call().with_assistant_text(Some(oversized));

    // Assert
    assert_eq!(noted.assistant_text(), Some("a.rs defines alpha_one"));
    assert!(!format!("{noted:?}").contains("alpha_one"));
    assert_eq!(blank.assistant_text(), None);
    assert_eq!(call().assistant_text(), None);
    assert!(matches!(
        error,
        Err(model::ModelError::ResponseContentTooLarge)
    ));
}

#[test]
fn tool_call_from_json_keeps_unknown_tools_verbatim() {
    // Arrange
    let raw = r#"{"file": "name.txt""#;

    // Act
    let call = ToolCall::from_json("call-id".to_string(), "read_file", raw, None)
        .expect("unknown tool call is kept");

    // Assert
    let unknown = call.unknown_tool().expect("unknown tool");
    assert_eq!((unknown.name(), unknown.raw()), ("read_file", raw));
    assert_eq!(call.name(), "read_file");
    assert_eq!(call.arguments_json().expect("raw"), raw);
    assert!(matches!(call.arguments(), ToolCallArguments::Unknown(_)));
    assert!(
        call.malformed_arguments().is_none()
            && call.read_arguments().is_none()
            && call.write_arguments().is_none()
    );
    assert!(
        ToolCall::from_json("call-id".to_string(), "read", r#"{"path":"a"}"#, None)
            .expect("read call")
            .unknown_tool()
            .is_none()
    );
}

#[test]
fn host_definitions_need_a_replayable_name_and_an_object_schema() {
    // Arrange
    let parameters = json!({"type": "object", "properties": {"url": {"type": "string"}}});

    // Act
    let definition = ToolDefinition::new("fetch-url_2", "Fetch a URL.", parameters.clone());
    let invalid_name = ToolDefinition::new("fetch.url", "Fetch a URL.", parameters.clone());
    let empty_name = ToolDefinition::new("", "Fetch a URL.", parameters);
    let invalid_parameters =
        ToolDefinition::new("fetch", "Fetch a URL.", json!({"type": "string"}));

    // Assert
    let definition = definition.expect("valid definition");
    assert_eq!(definition.name(), "fetch-url_2");
    assert_eq!(definition.description(), "Fetch a URL.");
    assert_eq!(
        definition.parameters()["properties"]["url"]["type"],
        "string"
    );
    let invalid_name = invalid_name.expect_err("invalid name");
    assert_eq!(
        invalid_name,
        ToolDefinitionError::InvalidName {
            name: "fetch.url".to_string()
        }
    );
    assert_eq!(
        invalid_name.to_string(),
        "tool name `fetch.url` must be 1 to 64 ASCII letters, digits, `_`, or `-`"
    );
    assert!(matches!(
        empty_name,
        Err(ToolDefinitionError::InvalidName { .. })
    ));
    let invalid_parameters = invalid_parameters.expect_err("invalid parameters");
    assert_eq!(invalid_parameters, ToolDefinitionError::InvalidParameters);
    assert_eq!(
        invalid_parameters.to_string(),
        r#"tool parameters must be a JSON Schema object with "type": "object""#
    );
}

#[test]
fn arguments_value_is_what_a_handler_receives() {
    // Arrange
    let call = |name: &str, raw: &str| {
        ToolCall::from_json("call-id".to_string(), name, raw, None).expect("tool call")
    };
    let read = call("read", r#"{"path":"a.txt","limit":"5"}"#);
    let write = call("write", r#"{"path":"a.txt","patch":"+x"}"#);
    let bash = call("bash", r#"{"command":"true"}"#);
    let host = call("fetch", r#"{"url":"https://example.com"}"#);
    let malformed = call("read", r#"{"path":"a.txt","extra":1}"#);
    let unparsable = call("fetch", r#"{"url""#);
    let blank = call("fetch", " ");
    let not_object = call("fetch", "[1]");

    // Act / Assert
    assert_eq!(
        read.arguments_value().expect("read"),
        json!({"path": "a.txt", "limit": 5})
    );
    assert_eq!(
        write.arguments_value().expect("write"),
        json!({"path": "a.txt", "patch": "+x"})
    );
    assert_eq!(
        bash.arguments_value().expect("bash"),
        json!({"command": "true"})
    );
    assert_eq!(
        host.arguments_value().expect("host"),
        json!({"url": "https://example.com"})
    );
    assert_eq!(
        malformed.arguments_value().expect("malformed but JSON"),
        json!({"path": "a.txt", "extra": 1})
    );
    assert!(
        unparsable
            .arguments_value()
            .expect_err("not JSON")
            .starts_with("failed to parse function arguments: ")
    );
    assert_eq!(blank.arguments_value().expect("blank"), json!({}));
    assert_eq!(
        not_object.arguments_value().expect_err("not an object"),
        "function arguments must be a JSON object"
    );
}

#[test]
fn only_a_name_providers_cannot_replay_is_marked_renamed() {
    // Arrange
    let call = |name: &str| {
        ToolCall::from_json("call-id".to_string(), name, "{}", None).expect("tool call")
    };

    // Act
    let renamed = [
        call("lookup.issue"),
        call(&"x".repeat(65)),
        call("lookup_issue"),
    ]
    .map(|call| call.unknown_tool().expect("unknown tool").renamed());

    // Assert
    assert_eq!(renamed, [true, true, false]);
}
