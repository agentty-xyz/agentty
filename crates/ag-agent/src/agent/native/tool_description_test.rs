use ag_contracts::{ActivityKind, LineRange};
use serde_json::json;

use super::ToolDescription;

/// Describes a call to `name` with `arguments`.
fn describe(name: &str, arguments: &str) -> ToolDescription {
    ToolDescription::new("call-1", name, Some(arguments))
}

#[test]
fn bash_calls_target_the_whole_command_on_one_line() {
    // Arrange
    let arguments = r#"{"command": "{\n  cargo fmt;\n  cargo test 2>&1 | tail -20;\n}"}"#;

    // Act
    let description = describe("bash", arguments);

    // Assert
    assert_eq!(description.kind, ActivityKind::Command);
    assert_eq!(
        description.target.as_deref(),
        Some("{ cargo fmt; cargo test 2>&1 | tail -20; }")
    );
    assert_eq!(
        description.input.as_deref(),
        Some("{\n  cargo fmt;\n  cargo test 2>&1 | tail -20;\n}")
    );
}

#[test]
fn read_actions_select_exploration_kinds_targets_and_ranges() {
    // Arrange
    let calls = [
        r#"{"path": "src/a.rs", "offset": "1129", "limit": 100}"#,
        r#"{"action": "search", "query": "fn render", "path": "src"}"#,
        r#"{"action": "list", "path": "crates"}"#,
        r#"{"action": "list"}"#,
        r#"{"action": "show", "path": "src/b.rs", "side": "base"}"#,
    ];

    // Act
    let descriptions = calls.map(|arguments| {
        let description = describe("read", arguments);

        (description.kind, description.target, description.lines)
    });

    // Assert
    assert_eq!(
        descriptions,
        [
            (
                ActivityKind::Read,
                Some("src/a.rs".to_string()),
                Some(LineRange {
                    first: 1129,
                    last: Some(1228)
                })
            ),
            (ActivityKind::Search, Some("fn render".to_string()), None),
            (ActivityKind::List, Some("crates".to_string()), None),
            (ActivityKind::List, None, None),
            (ActivityKind::Read, Some("src/b.rs".to_string()), None),
        ]
    );
}

#[test]
fn arguments_the_harness_tolerates_describe_the_call() {
    // Arrange
    let string_arguments = json!(r#"{"command": "cargo test"}"#).to_string();
    let calls = [
        (
            "bash",
            r#"{"command": "cargo test", "timeout": 120000}"#.to_string(),
        ),
        ("bash", string_arguments),
        (
            "write",
            r#"{"patch": "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-a\n+b\n"}"#.to_string(),
        ),
    ];

    // Act
    let descriptions = calls.map(|(name, arguments)| {
        let description = describe(name, &arguments);

        (description.kind, description.target)
    });

    // Assert
    assert_eq!(
        descriptions,
        [
            (ActivityKind::Command, Some("cargo test".to_string())),
            (ActivityKind::Command, Some("cargo test".to_string())),
            (ActivityKind::FileChange, Some("src/lib.rs".to_string())),
        ]
    );
}

#[test]
fn write_calls_target_their_path() {
    // Arrange
    let arguments = r#"{"path": "src/composer.rs", "patch": "@@ -1 +1 @@\n-a\n+b\n"}"#;

    // Act
    let description = describe("write", arguments);

    // Assert
    assert_eq!(description.kind, ActivityKind::FileChange);
    assert_eq!(description.target.as_deref(), Some("src/composer.rs"));
    assert_eq!(description.input, None);
}

#[test]
fn undecodable_or_absent_arguments_describe_the_call_by_name() {
    // Arrange
    let calls = [
        ("bash", Some("cargo test")),
        ("read", Some(r#"{"action": "search"}"#)),
        ("read", None),
        ("write", None),
        ("lookup_notes", Some(r#"{"path": "notes.txt"}"#)),
        ("bad name!", Some("{}")),
    ];

    // Act
    let descriptions = calls.map(|(name, arguments)| {
        let description = ToolDescription::new("call-1", name, arguments);

        (description.kind, description.target)
    });

    // Assert
    assert_eq!(
        descriptions,
        [
            (ActivityKind::Command, None),
            // The harness keeps an invalid field combination to correct the
            // model, so the action still decides the kind.
            (ActivityKind::Search, None),
            (ActivityKind::Read, None),
            (ActivityKind::FileChange, None),
            (ActivityKind::Tool, None),
            (ActivityKind::Tool, None),
        ]
    );
}
