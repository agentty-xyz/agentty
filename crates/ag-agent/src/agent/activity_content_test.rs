use serde_json::json;

use super::{ActivityDetails, PREVIEW_LIMIT, TRUNCATED, append_output, bounded, preview};

#[test]
fn previews_extract_commands_text_blocks_and_structured_results() {
    // Arrange, Act, Assert
    for (value, expected) in [
        (json!({"command": "cargo test"}), "cargo test"),
        (json!({"CommandLine": "pwd"}), "pwd"),
        (json!({"file_path": "README.md"}), "README.md"),
        (json!({"path": "README.md"}), "README.md"),
        (json!({"TargetFile": "README.md"}), "README.md"),
        (json!({"target_file": "README.md"}), "README.md"),
    ] {
        let details = ActivityDetails::new(Some(&value), Some(&json!("done")), Some(0));
        assert_eq!(details.input.as_deref(), Some(expected));
        assert_eq!(details.output.as_deref(), Some("done"));
        assert_eq!(details.exit_code, Some(0));
    }
    assert_eq!(
        preview(
            &json!(["first", {"type":"text","text":"second"}, {"content":{"text":"third"}}, {"type":"image"}])
        ),
        Some("first\nsecond\nthird".into())
    );
    assert_eq!(preview(&json!([])), None);
    assert_eq!(preview(&json!(null)), None);
    assert_eq!(
        preview(&json!({"error":"failed"})),
        Some(r#"{"error":"failed"}"#.into())
    );
    assert_eq!(preview(&json!("")), Some(String::new()));
}

#[test]
fn previews_bound_unicode_strip_controls_and_stop_growing() {
    // Arrange
    let oversized = "界".repeat(PREVIEW_LIMIT + 1);
    let capped = format!("{}{TRUNCATED}", "界".repeat(PREVIEW_LIMIT));

    // Act, Assert
    assert_eq!(bounded(&oversized), capped);
    assert_eq!(bounded("a\0\r\u{1b}b\n\tc"), "ab\n\tc");
    assert_eq!(append_output(None, "first"), "first");
    assert_eq!(append_output(Some("first"), " second"), "first second");
    assert_eq!(append_output(Some(&capped), "ignored"), capped);
    assert_eq!(append_output(None, &oversized), capped);
    assert_eq!(bounded(&"x".repeat(PREVIEW_LIMIT)).len(), PREVIEW_LIMIT);
    let arguments = json!({"file_path": oversized, "content": "hidden"});
    assert_eq!(
        ActivityDetails::new(Some(&arguments), None, None).input,
        Some(capped)
    );
}

#[test]
fn structured_arrays_remain_json_and_obey_the_preview_bound() {
    // Arrange, Act, Assert
    for value in [
        json!([{"path":"README.md"}]),
        json!([1, true, null, ["nested"]]),
        json!([{"type":"text","text":"read"}, {"path":"README.md"}]),
    ] {
        assert_eq!(preview(&value), Some(value.to_string()));
    }
    let oversized = json!([{"path":"界".repeat(PREVIEW_LIMIT)}]);
    assert_eq!(preview(&oversized), Some(bounded(&oversized.to_string())));
}
