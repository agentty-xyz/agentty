//! Claude `stream-json` stdin message and turn-boundary unit tests.

use ag_protocol::{TurnPrompt, TurnPromptAttachment, TurnPromptTextSource};
use serde_json::{Value, json};

use crate::agent::claude::{encode_steering_prompt, is_turn_result, stream_json_user_message};

#[test]
fn user_message_is_one_newline_terminated_stream_json_line() {
    // Arrange
    let prompt_text = "first line\nsecond \"quoted\" line";

    // Act
    let message = stream_json_user_message(prompt_text);

    // Assert
    let line = String::from_utf8(message).expect("UTF-8 message");
    let (json_text, rest) = line.split_once('\n').expect("terminated line");
    assert_eq!(rest, "");
    assert_eq!(
        serde_json::from_str::<Value>(json_text).expect("JSON line"),
        json!({
            "type": "user",
            "message": {"role": "user", "content": prompt_text},
            "parent_tool_use_id": null,
        })
    );
}

#[test]
fn steering_prompt_encodes_text_and_rejects_images() {
    // Arrange
    let text_prompt = TurnPrompt::from("focus on the parser");
    let image_prompt = TurnPrompt {
        attachments: vec![TurnPromptAttachment {
            placeholder: "[Image #1]".to_string(),
            local_image_path: "/tmp/image.png".into(),
        }],
        text: "Check [Image #1]".to_string(),
        text_source: TurnPromptTextSource::UserPrompt,
    };

    // Act
    let text_message = encode_steering_prompt(&text_prompt);
    let image_message = encode_steering_prompt(&image_prompt);

    // Assert
    assert_eq!(
        text_message.expect("text prompt"),
        stream_json_user_message("focus on the parser")
    );
    assert_eq!(
        image_message.expect_err("image prompt").to_string(),
        "Claude cannot receive images during a turn"
    );
}

#[test]
fn only_result_payloads_end_a_turn() {
    // Arrange
    let payloads = [
        json!({"type": "result", "subtype": "success"}),
        json!({"type": "assistant"}),
        json!({"result": "no type"}),
    ];

    // Act
    let turn_ends = payloads.each_ref().map(is_turn_result);

    // Assert
    assert_eq!(turn_ends, [true, false, false]);
}
