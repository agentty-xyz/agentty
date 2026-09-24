//! Checkpoint record validation, codec, and source-rendering unit tests.

use serde_json::{Value, json};

use super::{
    CheckpointError, GENERATION_INSTRUCTIONS, MAX_SUMMARY_BYTES, SessionCheckpoint, render_source,
    summary_schema,
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
fn render_source_carries_forward_every_message_kind_without_reasoning() {
    // Arrange
    let previous = checkpoint();
    let mut png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    png.extend_from_slice(b"payload");
    let image = ImageContent::new(ImageMediaType::Png, png).expect("image");
    let digest = image.content_digest();
    let call = ToolCall::from_json("call-1".to_string(), "read", r#"{"path":"a"}"#, None)
        .expect("tool call");
    let batch = ToolCall::from_json("call-2".to_string(), "read", r#"{"path":"b"}"#, None)
        .expect("tool call");
    let turn = vec![
        ModelMessage::System("policy".to_string()),
        ModelMessage::User("plain".to_string()),
        ModelMessage::UserInput(
            TurnInput::from_blocks(vec![
                InputBlock::Text("look".to_string()),
                InputBlock::Image(image),
            ])
            .expect("input"),
        ),
        ModelMessage::AssistantToolCall(call),
        ModelMessage::AssistantToolCalls(vec![batch]),
        ModelMessage::ToolResult {
            call_id: "call-1".to_string(),
            content: "contents".to_string(),
            name: "read".to_string(),
        },
        ModelMessage::AssistantReasoning {
            content: "{\"answer\":\"done\"}".to_string(),
            reasoning_content: "secret chain".to_string(),
        },
        ModelMessage::Assistant("{\"answer\":\"final\"}".to_string()),
    ];

    // Act
    let source = render_source(Some(&previous), &[&turn]);

    // Assert
    assert!(source.contains("carry its content forward"));
    assert!(source.contains("reviewed the parser"));
    assert!(source.contains("system: policy"));
    assert!(source.contains("user: plain"));
    assert!(source.contains(&format!("[image image/png sha256:{digest}]")));
    assert!(source.contains("[tool call read]"));
    assert!(source.contains("[tool calls read]"));
    assert!(source.contains("tool read: contents"));
    assert!(source.contains("{\"answer\":\"done\"}"));
    assert!(source.contains("{\"answer\":\"final\"}"));
    assert!(!source.contains("secret chain"));
}
