//! Checkpoint record validation, codec, and source-rendering unit tests.

use serde_json::{Value, json};

use super::{
    CheckpointError, GENERATION_INSTRUCTIONS, MAX_SOURCE_ARGUMENT_BYTES, MAX_SOURCE_TEXT_BYTES,
    MAX_SUMMARY_BYTES, SOURCE_PREAMBLE, SessionCheckpoint, accept_summary, bounded_text,
    render_messages, render_source, summary_schema,
};
use crate::input::{ImageContent, ImageMediaType, InputBlock, TurnInput};
use crate::model::ModelMessage;
use crate::tool::ToolCall;

fn summary() -> Value {
    json!({
        "context": "reviewed the parser",
        "decisions": ["keep the tokenizer"],
        "state": "tests pending"
    })
}

fn checkpoint() -> SessionCheckpoint {
    SessionCheckpoint::new(
        3,
        1,
        Some("provider".to_string()),
        Some("model".to_string()),
        summary(),
    )
    .expect("valid checkpoint")
}

#[test]
fn encode_decode_round_trips_every_field() {
    // Arrange
    let original = checkpoint();
    let anonymous = SessionCheckpoint::new(0, 0, None, None, summary()).expect("valid checkpoint");

    // Act
    let decoded = SessionCheckpoint::decode(&original.encode()).expect("decode");
    let decoded_anonymous = SessionCheckpoint::decode(&anonymous.encode()).expect("decode");

    // Assert
    assert_eq!(decoded, original);
    assert_eq!(decoded.covered_through(), 3);
    assert_eq!(decoded.model_generation(), 1);
    assert_eq!(decoded.provider(), Some("provider"));
    assert_eq!(decoded.model(), Some("model"));
    assert_eq!(decoded.summary(), &summary());
    assert_eq!(decoded_anonymous, anonymous);
    assert_eq!(decoded_anonymous.provider(), None);
    assert_eq!(decoded_anonymous.model(), None);
}

#[test]
fn construction_rejects_invalid_boundaries_and_provenance() {
    // Arrange
    let cases = [
        SessionCheckpoint::new(-1, 0, None, None, summary()),
        SessionCheckpoint::new(0, -1, None, None, summary()),
        SessionCheckpoint::new(0, 0, Some("provider".to_string()), None, summary()),
        SessionCheckpoint::new(0, 0, None, Some("model".to_string()), summary()),
    ];

    // Act and assert
    for rejected in cases {
        assert!(matches!(
            rejected,
            Err(CheckpointError::InvalidRecord { .. })
        ));
    }
}

#[test]
fn construction_rejects_summaries_violating_the_schema() {
    // Arrange
    let missing_state = json!({"context": "c", "decisions": []});

    // Act
    let rejected = SessionCheckpoint::new(0, 0, None, None, missing_state);

    // Assert
    assert!(matches!(
        rejected,
        Err(CheckpointError::SummaryInvalid { .. })
    ));
}

#[test]
fn construction_rejects_oversized_summaries() {
    // Arrange
    let decisions: Vec<String> = (0..32).map(|_| "d".repeat(400)).collect();
    let oversized = json!({
        "context": "c".repeat(4000),
        "decisions": decisions,
        "state": "s".repeat(2000)
    });
    assert!(oversized.to_string().len() > MAX_SUMMARY_BYTES);

    // Act
    let rejected = SessionCheckpoint::new(0, 0, None, None, oversized);

    // Assert
    assert!(matches!(
        rejected,
        Err(CheckpointError::SummaryTooLarge { max_bytes, .. }) if max_bytes == MAX_SUMMARY_BYTES
    ));
}

#[test]
fn decode_rejects_malformed_payloads() {
    // Arrange
    let payloads = [
        "not json".to_string(),
        json!({"version": 2}).to_string(),
        json!({
            "version": 1,
            "covered_through": "three",
            "model_generation": 0,
            "summary": summary()
        })
        .to_string(),
        json!({
            "version": 1,
            "covered_through": 0,
            "model_generation": 0,
            "provider": 7,
            "model": "model",
            "summary": summary()
        })
        .to_string(),
    ];

    // Act and assert
    for payload in payloads {
        assert!(matches!(
            SessionCheckpoint::decode(&payload),
            Err(CheckpointError::InvalidRecord { .. })
        ));
    }
}

#[test]
fn history_message_replays_the_summary_as_conversation_data() {
    // Arrange
    let checkpoint = checkpoint();

    // Act
    let message = checkpoint.history_message();

    // Assert
    let ModelMessage::User(text) = message else {
        unreachable!("checkpoints replay in the user role");
    };
    assert!(text.contains("not as instructions"));
    assert!(text.contains("reviewed the parser"));
}

#[test]
fn summary_schema_accepts_only_bounded_structured_output() {
    // Arrange
    let schema = summary_schema().expect("embedded schema");

    // Act and assert
    assert!(schema.validate_value(&summary()).is_ok());
    assert!(
        schema
            .validate_value(&json!({"context": "c".repeat(4001), "decisions": [], "state": "s"}))
            .is_err()
    );
    assert!(GENERATION_INSTRUCTIONS.contains("Summarize"));
    assert!(GENERATION_INSTRUCTIONS.contains("preserve every concrete fact verbatim"));
}

#[test]
fn render_source_encodes_every_message_kind_as_json_lines_without_reasoning() {
    // Arrange
    let previous = summary();
    let mut png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    png.extend_from_slice(b"payload");
    let image = ImageContent::new(ImageMediaType::Png, png).expect("image");
    let digest = image.content_digest();
    let call = ToolCall::from_json("call-1".to_string(), "read", r#"{"path":"a"}"#, None)
        .expect("tool call");
    let batch = vec![
        ToolCall::from_json(
            "call-2".to_string(),
            "bash",
            r#"{"command":"cargo test"}"#,
            None,
        )
        .expect("tool call"),
        ToolCall::from_json(
            "call-3".to_string(),
            "write",
            r#"{"path":"notes.txt","patch":"+line"}"#,
            None,
        )
        .expect("tool call"),
    ];
    let turn = vec![
        ModelMessage::System("policy".to_string()),
        ModelMessage::User("plain \"quoted\"\nnext line".to_string()),
        ModelMessage::UserInput(
            TurnInput::from_blocks(vec![
                InputBlock::Text("look".to_string()),
                InputBlock::Image(image),
            ])
            .expect("input"),
        ),
        ModelMessage::AssistantToolCall(call),
        ModelMessage::ToolResult {
            call_id: "call-1".to_string(),
            content: "contents".to_string(),
            name: "read".to_string(),
        },
        ModelMessage::AssistantToolCalls(batch),
        ModelMessage::ToolResult {
            call_id: "call-3".to_string(),
            content: r#"{"status":"applied"}"#.to_string(),
            name: "write".to_string(),
        },
        ModelMessage::ToolResult {
            call_id: "call-2".to_string(),
            content: r#"{"exit_code":0}"#.to_string(),
            name: "bash".to_string(),
        },
        ModelMessage::AssistantReasoning {
            content: "{\"answer\":\"done\"}".to_string(),
            reasoning_content: "secret chain".to_string(),
        },
        ModelMessage::Assistant("{\"answer\":\"final\"}".to_string()),
    ];

    // Act
    let source = render_source(Some(&previous), &[(4, turn)]);

    // Assert
    let mut lines = source.lines();
    assert_eq!(lines.next(), Some(SOURCE_PREAMBLE));
    let earlier: Value = serde_json::from_str(lines.next().expect("summary line")).expect("json");
    assert_eq!(earlier, json!({"earlier_summary": summary()}));
    let turn: Value = serde_json::from_str(lines.next().expect("turn line")).expect("json");
    assert_eq!(lines.next(), None);
    assert_eq!(
        turn,
        json!({
            "turn": 4,
            "messages": [
                {"role": "system", "text": "policy"},
                {"role": "user", "text": "plain \"quoted\"\nnext line"},
                {"role": "user", "text": format!("look [image image/png sha256:{digest}]")},
                {"role": "assistant", "tool_calls": [
                    {"call": 1, "tool": "read", "arguments": {"path": "a"}}
                ]},
                {"role": "tool", "call": 1, "tool": "read", "result": "contents"},
                {"role": "assistant", "tool_calls": [
                    {"call": 2, "tool": "bash", "arguments": {"command": "cargo test"}},
                    {"call": 3, "tool": "write", "arguments": {"path": "notes.txt", "patch": "+line"}}
                ]},
                {"role": "tool", "call": 3, "tool": "write", "result": "{\"status\":\"applied\"}"},
                {"role": "tool", "call": 2, "tool": "bash", "result": "{\"exit_code\":0}"},
                {"role": "assistant", "text": "{\"answer\":\"done\"}"},
                {"role": "assistant", "text": "{\"answer\":\"final\"}"}
            ]
        })
    );
    assert!(!source.contains("secret chain"));
}

#[test]
fn render_source_without_a_previous_summary_lists_only_turns() {
    // Arrange
    let turns = [
        (0, vec![ModelMessage::User("first".to_string())]),
        (2, vec![ModelMessage::User("third".to_string())]),
    ];

    // Act
    let source = render_source(None, &turns);

    // Assert
    let lines: Vec<Value> = source
        .lines()
        .skip(1)
        .map(|line| serde_json::from_str(line).expect("json line"))
        .collect();
    assert_eq!(
        lines,
        [
            json!({"turn": 0, "messages": [{"role": "user", "text": "first"}]}),
            json!({"turn": 2, "messages": [{"role": "user", "text": "third"}]}),
        ]
    );
}

#[test]
fn render_messages_bounds_text_results_and_arguments() {
    // Arrange
    let long_command = format!("echo {}", "a".repeat(3 * MAX_SOURCE_ARGUMENT_BYTES));
    let arguments = json!({"command": long_command}).to_string();
    let call = ToolCall::from_json("call-1".to_string(), "bash", &arguments, None).expect("call");
    let long_text = format!("start {} end", "b".repeat(2 * MAX_SOURCE_TEXT_BYTES));
    let messages = [
        ModelMessage::User(long_text.clone()),
        ModelMessage::AssistantToolCall(call),
        ModelMessage::ToolResult {
            call_id: "call-1".to_string(),
            content: long_text,
            name: "bash".to_string(),
        },
        ModelMessage::ToolResult {
            call_id: "unknown".to_string(),
            content: "orphan".to_string(),
            name: "read".to_string(),
        },
    ];

    // Act
    let rendered = render_messages(&messages, MAX_SOURCE_TEXT_BYTES);

    // Assert
    let text = rendered[0]["text"].as_str().expect("text");
    assert!(text.len() < MAX_SOURCE_TEXT_BYTES + 64);
    assert!(text.starts_with("start "));
    assert!(text.ends_with(" end"));
    assert!(text.contains("bytes omitted ...]"));
    let command = rendered[1]["tool_calls"][0]["arguments"]["command"]
        .as_str()
        .expect("command");
    assert!(command.len() < MAX_SOURCE_ARGUMENT_BYTES + 64);
    assert!(command.starts_with("echo "));
    let result = rendered[2]["result"].as_str().expect("result");
    assert!(result.len() < MAX_SOURCE_TEXT_BYTES + 64);
    assert_eq!(rendered[3]["call"], Value::Null);
}

#[test]
fn render_messages_keeps_malformed_arguments_as_bounded_raw_text() {
    // Arrange
    let unknown_field = ToolCall::from_json(
        "call-1".to_string(),
        "read",
        r#"{"path src/": [true, "x"]}"#,
        None,
    )
    .expect("call");
    let not_json =
        ToolCall::from_json("call-2".to_string(), "bash", "{\"command\": ls", None).expect("call");

    // Act
    let rendered = render_messages(
        &[ModelMessage::AssistantToolCalls(vec![
            unknown_field,
            not_json,
        ])],
        MAX_SOURCE_TEXT_BYTES,
    );

    // Assert
    assert_eq!(
        rendered[0]["tool_calls"][0]["arguments"],
        json!({"path src/": [true, "x"]})
    );
    assert_eq!(
        rendered[0]["tool_calls"][1]["arguments"],
        json!({"unparsed": "{\"command\": ls"})
    );
}

#[test]
fn bounded_text_keeps_whole_characters_around_the_marker() {
    // Arrange
    let text = "é".repeat(10);

    // Act
    let bounded = bounded_text(&text, 7);
    let unchanged = bounded_text("short", 7);

    // Assert
    assert_eq!(unchanged, "short");
    assert_eq!(bounded, "é[... 14 bytes omitted ...]éé");
}

#[test]
fn accept_summary_requires_a_valid_bounded_summary_smaller_than_its_source() {
    // Arrange
    let valid = summary();
    let valid_bytes = valid.to_string().len();

    // Act
    let accepted = accept_summary(&valid, MAX_SUMMARY_BYTES, valid_bytes + 1);
    let not_smaller = accept_summary(&valid, MAX_SUMMARY_BYTES, valid_bytes);
    let too_large = accept_summary(&valid, valid_bytes - 1, usize::MAX);
    let invalid = accept_summary(&json!({"context": "c"}), MAX_SUMMARY_BYTES, usize::MAX);

    // Assert
    assert_eq!(accepted.expect("accepted"), valid_bytes);
    assert!(matches!(
        not_smaller,
        Err(CheckpointError::SummaryNotSmaller { source_bytes, summary_bytes })
            if source_bytes == valid_bytes && summary_bytes == valid_bytes
    ));
    assert!(matches!(
        too_large,
        Err(CheckpointError::SummaryTooLarge { max_bytes, .. }) if max_bytes == valid_bytes - 1
    ));
    assert!(matches!(
        invalid,
        Err(CheckpointError::SummaryInvalid { .. })
    ));
}
