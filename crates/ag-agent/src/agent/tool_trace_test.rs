use std::io::Write as _;

use ag_telemetry::KeyValue;
use serde_json::{Value, json};

use crate::agent::tool_trace::{PREVIEW_BYTES, Preview, ToolMetadata, safe_identity};

#[test]
fn default_metadata_excludes_content_and_project_paths() {
    // Arrange
    let input = json!({"command": "cargo test", "password": "hidden"});
    let output = json!("test failed");
    let metadata = ToolMetadata {
        command: Some("/project/private/bin/cargo test"),
        input: Some(&input),
        name: Some("Bash"),
        output: Some(&output),
        stderr: Some(&output),
        stdout: Some(&output),
    };

    // Act
    let attributes = metadata.attributes(false);

    // Assert
    assert_eq!(
        attributes,
        vec![
            KeyValue::new("gen_ai.tool.name", "Bash"),
            KeyValue::new("process.executable.name", "cargo"),
            KeyValue::new("agentty.tool.output.bytes", 11_i64),
            KeyValue::new("agentty.tool.stdout.bytes", 11_i64),
            KeyValue::new("agentty.tool.stderr.bytes", 11_i64),
        ]
    );
    assert!(!format!("{attributes:?}").contains("private"));
}

#[test]
fn opt_in_exports_commands_arguments_and_separate_output_previews() {
    // Arrange
    let input = json!({"command": "cargo test", "count": 2, "enabled": true, "extra": null});
    let output = json!([{"text": "test failed"}]);
    let stderr = json!("compilation failed");
    let stdout = json!("");
    let metadata = ToolMetadata {
        command: Some("cargo test"),
        input: Some(&input),
        name: Some("shell"),
        output: Some(&output),
        stderr: Some(&stderr),
        stdout: Some(&stdout),
    };

    // Act
    let attributes = metadata.attributes(true);

    // Assert
    for attribute in [
        KeyValue::new("agentty.tool.command", "cargo test"),
        KeyValue::new("gen_ai.tool.call.arguments", input.to_string()),
        KeyValue::new("gen_ai.tool.call.result", output.to_string()),
        KeyValue::new("agentty.tool.stderr", "compilation failed"),
        KeyValue::new("agentty.tool.stdout", ""),
        KeyValue::new(
            "agentty.tool.output.bytes",
            i64::try_from(output.to_string().len()).expect("bounded fixture"),
        ),
        KeyValue::new("agentty.tool.input.truncated", false),
    ] {
        assert!(attributes.contains(&attribute), "{attribute:?}");
    }
}

#[test]
fn credential_markers_suppress_whole_values_before_truncation() {
    // Arrange
    let values = [
        json!({"password": "hidden", "command": "cargo test"}),
        json!([{"nested": "Authorization: Basic hidden"}]),
        json!({"api_key": "hidden"}),
        json!("curl --token hidden"),
        json!("PASSWORD='hidden with spaces' cargo test"),
        json!("https://user:hidden@host/path"),
        json!("http://user:hidden@host"),
        json!(format!("{}\nBearer hidden", "x".repeat(PREVIEW_BYTES))),
    ];

    // Act / Assert
    for output in &values {
        let attributes = ToolMetadata {
            output: Some(output),
            ..ToolMetadata::default()
        }
        .attributes(true);
        let redacted = if output.is_string() {
            "[REDACTED]"
        } else {
            r#""[REDACTED]""#
        };
        assert!(attributes.contains(&KeyValue::new("gen_ai.tool.call.result", redacted)));
        assert!(attributes.contains(&KeyValue::new("agentty.tool.output.truncated", false)));
        assert!(!format!("{attributes:?}").contains("hidden"));
        if !output.is_string() {
            assert_eq!(
                serde_json::from_str::<Value>(redacted).expect("valid redaction JSON"),
                json!("[REDACTED]")
            );
        }
    }
}

#[test]
fn previews_limit_utf8_bytes_and_report_full_output_size() {
    // Arrange
    let values = [
        json!("x".repeat(PREVIEW_BYTES)),
        json!(format!("{}é", "x".repeat(PREVIEW_BYTES - 1))),
    ];

    // Act / Assert
    for value in &values {
        let attributes = ToolMetadata {
            output: Some(value),
            ..ToolMetadata::default()
        }
        .attributes(true);
        let size = value
            .as_str()
            .map_or_else(|| value.to_string().len(), str::len);
        assert!(attributes.contains(&KeyValue::new(
            "agentty.tool.output.bytes",
            i64::try_from(size).expect("bounded fixture")
        )));
        assert!(attributes.contains(&KeyValue::new(
            "agentty.tool.output.truncated",
            size > PREVIEW_BYTES
        )));
        let preview = attributes
            .iter()
            .find(|attribute| attribute.key.as_str() == "gen_ai.tool.call.result")
            .expect("preview");
        assert!(preview.value.to_string().len() <= PREVIEW_BYTES);
        assert!(!preview.value.to_string().contains('�'));
    }
}

#[test]
fn metadata_only_counts_compact_json_without_retaining_serialized_content() {
    // Arrange
    let controls: String = (0_u8..=31).map(char::from).collect();
    let values = [
        json!(null),
        json!(true),
        json!(false),
        json!(0),
        json!(-42),
        json!(1.25),
        json!(1e-30),
        json!([]),
        json!({}),
        json!("raw text"),
        json!([null, true, false, 12, -7, {"quoted\"key\n": ["é🦀", "\\", controls]}]),
        json!({"stdout": "x".repeat(1_000_000), "stderr": "compilation failed"}),
    ];

    // Act / Assert
    for value in &values {
        let expected = value
            .as_str()
            .map_or_else(|| value.to_string().len(), str::len);
        let preview = Preview::new(value, false);
        assert_eq!(preview.bytes, expected);
        assert_eq!(preview.content, [] as [u8; 0]);
        assert!(!preview.retain);
    }
}

#[test]
fn oversized_structured_values_use_text_previews_instead_of_partial_json() {
    // Arrange
    let values = [
        json!({"message": "é".repeat(PREVIEW_BYTES)}),
        json!(["\"\\\n".repeat(PREVIEW_BYTES)]),
    ];

    // Act / Assert
    for value in &values {
        let attributes = ToolMetadata {
            input: Some(value),
            output: Some(value),
            stdout: Some(value),
            stderr: Some(value),
            ..ToolMetadata::default()
        }
        .attributes(true);
        for (json_key, preview_key, truncated_key) in [
            (
                "gen_ai.tool.call.arguments",
                "agentty.tool.input.preview",
                "agentty.tool.input.truncated",
            ),
            (
                "gen_ai.tool.call.result",
                "agentty.tool.output.preview",
                "agentty.tool.output.truncated",
            ),
            (
                "agentty.tool.stdout",
                "agentty.tool.stdout.preview",
                "agentty.tool.stdout.truncated",
            ),
            (
                "agentty.tool.stderr",
                "agentty.tool.stderr.preview",
                "agentty.tool.stderr.truncated",
            ),
        ] {
            assert!(
                !attributes
                    .iter()
                    .any(|attribute| attribute.key.as_str() == json_key)
            );
            let preview = attributes
                .iter()
                .find(|attribute| attribute.key.as_str() == preview_key)
                .expect("text preview");
            assert!(preview.value.to_string().len() <= PREVIEW_BYTES);
            assert!(!preview.value.to_string().contains('�'));
            assert!(attributes.contains(&KeyValue::new(truncated_key, true)));
        }
        assert!(attributes.contains(&KeyValue::new(
            "agentty.tool.output.bytes",
            i64::try_from(value.to_string().len()).expect("fixture size")
        )));
    }
}

#[test]
fn structured_values_at_the_limit_remain_complete_parseable_json() {
    // Arrange
    let value = json!({"message": "x".repeat(PREVIEW_BYTES - r#"{"message":""}"#.len())});
    assert_eq!(value.to_string().len(), PREVIEW_BYTES);

    // Act
    let attributes = ToolMetadata {
        input: Some(&value),
        output: Some(&value),
        ..ToolMetadata::default()
    }
    .attributes(true);

    // Assert
    for key in ["gen_ai.tool.call.arguments", "gen_ai.tool.call.result"] {
        let content = attributes
            .iter()
            .find(|attribute| attribute.key.as_str() == key)
            .expect("complete JSON")
            .value
            .to_string();
        assert_eq!(
            serde_json::from_str::<Value>(&content).expect("parseable JSON"),
            value
        );
    }
    assert!(attributes.contains(&KeyValue::new("agentty.tool.input.truncated", false)));
    assert!(attributes.contains(&KeyValue::new("agentty.tool.output.truncated", false)));
}

#[test]
fn missing_null_and_unsafe_identities_are_omitted() {
    // Arrange
    let null = json!(null);
    let metadata = ToolMetadata {
        command: Some(""),
        input: Some(&null),
        name: Some("tool /private/path"),
        output: Some(&null),
        stderr: Some(&null),
        stdout: Some(&null),
    };

    // Act / Assert
    assert_eq!(metadata.attributes(true), []);
    assert_eq!(ToolMetadata::default().attributes(false), []);
    for name in [
        "",
        "some tool",
        "api_token",
        "ghp_hidden",
        "/project/path",
        &"a".repeat(129),
    ] {
        assert!(safe_identity(name).is_none());
    }
    for command in [
        "TOKEN=hidden cargo test",
        "'C:\\bin\\cargo' test",
        "\"/usr/bin/cargo\" test",
    ] {
        let attributes = ToolMetadata {
            command: Some(command),
            ..ToolMetadata::default()
        }
        .attributes(false);
        assert_eq!(attributes.is_empty(), command.starts_with("TOKEN"));
    }
    for command in [
        "VAR=/project/path cargo test",
        "'/project with spaces/cargo' test",
        "\"/project with spaces/cargo\" test",
    ] {
        assert_eq!(
            ToolMetadata {
                command: Some(command),
                ..ToolMetadata::default()
            }
            .attributes(false),
            []
        );
    }
}

#[test]
fn writer_counts_without_retaining_disabled_content_and_flushes() {
    // Arrange
    let mut preview = Preview::new(&json!({"data": "content"}), false);
    let initial_bytes = preview.bytes;

    // Act
    assert_eq!(preview.write(b"abc").expect("write"), 3);
    preview.flush().expect("flush");

    // Assert
    assert_eq!(preview.bytes, initial_bytes + 3);
    assert_eq!(preview.content, [] as [u8; 0]);
}
