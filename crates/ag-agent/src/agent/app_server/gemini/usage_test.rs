use serde_json::json;

use super::*;

#[test]
fn prompt_result_text_flattens_nested_content_parts() {
    // Arrange
    let result = json!({
        "content": {
            "parts": [null, "First", {"content": {"text": " second"}}]
        }
    });

    // Act
    let text = extract_prompt_result_text(&result);

    // Assert
    assert_eq!(text.as_deref(), Some("First second"));
}

#[test]
fn prompt_result_text_reads_message_content() {
    // Arrange
    let result = json!({"message": {"content": ["Message text"]}});

    // Act
    let text = extract_prompt_result_text(&result);

    // Assert
    assert_eq!(text.as_deref(), Some("Message text"));
}

#[test]
fn prompt_result_text_combines_output_items() {
    // Arrange
    let result = json!({
        "output": [
            {"text": "First"},
            {"content": {"text": " second"}}
        ]
    });

    // Act
    let text = extract_prompt_result_text(&result);

    // Assert
    assert_eq!(text.as_deref(), Some("First second"));
}
