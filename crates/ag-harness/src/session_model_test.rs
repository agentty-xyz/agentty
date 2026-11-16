//! Recorded model snapshot decoding, generation overflow, and cross-provider
//! history projection tests.

use std::borrow::Cow;

#[cfg(feature = "sqlite")]
use serde_json::json;

use crate::context::{CLEARED_REASONING, HistoryTurn, TurnStop};
use crate::model::ModelMessage;
use crate::session_model::{RecordedModel, next_generation, project_turn};
use crate::tool::ToolCall;

/// Heading of every note that carries readable reasoning.
const HEADING: &str = "[harness] Reasoning an earlier model wrote at this step, kept as a note:\n";

/// Note replacing reasoning without readable text.
const OMITTED: &str =
    "[harness] An earlier model reasoned at this step; that reasoning is not readable here.";

/// Encrypted `Responses` reasoning with one summary part.
const SUMMARIZED: &str = r#"[{"type":"reasoning","encrypted_content":"opaque","summary":[{"type":"summary_text","text":"Check a.txt first."}]}]"#;

/// Encrypted `Responses` reasoning without a summary.
const ENCRYPTED: &str = r#"[{"type":"reasoning","encrypted_content":"opaque","summary":[]}]"#;

/// Returns a `read` call of `path` with optional reasoning and text.
fn read_call(id: &str, reasoning: Option<&str>, text: Option<&str>) -> ToolCall {
    ToolCall::from_json(
        id.to_string(),
        "read",
        r#"{"path":"a.txt"}"#,
        reasoning.map(str::to_string),
    )
    .and_then(|call| call.with_assistant_text(text.map(str::to_string)))
    .expect("read call")
}

/// Returns a turn of `messages` reserved under a model of `provider`.
fn turn_from(provider: &str, messages: Vec<ModelMessage>) -> HistoryTurn {
    HistoryTurn {
        messages,
        model: Some(RecordedModel {
            generation: 0,
            model: Some(format!("{provider}-model")),
            provider: Some(provider.to_string()),
            registration_identity: None,
        }),
        stop: None,
    }
}

#[test]
fn next_generation_rejects_overflow() {
    // Act
    let overflow = next_generation(i64::MAX);

    // Assert
    assert!(overflow.is_err());
}

#[cfg(feature = "sqlite")]
#[test]
fn snapshot_rejects_incomplete_or_invalid_identity() {
    // Arrange
    let valid =
        json!({"generation":0,"key":"key","revision":"1","provider":"provider","model":"model"});
    let mut invalid = vec!["not json".to_string()];
    for (field, value) in [
        ("generation", json!(-1)),
        ("key", json!(null)),
        ("revision", json!(null)),
        ("key", json!("")),
        ("provider", json!(null)),
        ("model", json!(null)),
        ("provider", json!("")),
        ("model", json!(" ")),
    ] {
        let mut snapshot = valid.clone();
        snapshot[field] = value;
        invalid.push(snapshot.to_string());
    }

    // Act / Assert
    assert!(RecordedModel::decode(&valid.to_string()).is_ok());
    assert!(
        RecordedModel::decode(
            r#"{"generation":0,"key":null,"revision":null,"provider":null,"model":null}"#
        )
        .is_ok()
    );
    for value in invalid {
        assert!(RecordedModel::decode(&value).is_err(), "{value}");
    }
}

#[test]
fn foreign_turn_replays_reasoning_as_notes_and_tool_calls_without_it() {
    // Arrange
    let stop = TurnStop {
        error_type: "cancelled".to_string(),
        ..TurnStop::default()
    };
    let turn = HistoryTurn {
        stop: Some(stop.clone()),
        ..turn_from(
            "openai",
            vec![
                ModelMessage::User("inspect".to_string()),
                ModelMessage::AssistantToolCalls(vec![
                    read_call("first", Some(ENCRYPTED), Some("Reading a.txt.")),
                    read_call("second", None, None),
                ]),
                ModelMessage::ToolResult {
                    call_id: "first".to_string(),
                    content: "{}".to_string(),
                    name: "read".to_string(),
                },
                ModelMessage::AssistantToolCall(read_call("third", Some(SUMMARIZED), None)),
                ModelMessage::AssistantReasoning {
                    content: r#"{"answer":"done"}"#.to_string(),
                    reasoning_content: SUMMARIZED.to_string(),
                },
            ],
        )
    };

    // Act
    let projected = project_turn(&turn, Some("kimi"));

    // Assert
    assert_eq!(
        projected.into_owned(),
        HistoryTurn {
            messages: vec![
                ModelMessage::User("inspect".to_string()),
                ModelMessage::AssistantToolCalls(vec![
                    read_call("first", None, Some(&format!("{OMITTED}\n\nReading a.txt."))),
                    read_call("second", None, None),
                ]),
                ModelMessage::ToolResult {
                    call_id: "first".to_string(),
                    content: "{}".to_string(),
                    name: "read".to_string(),
                },
                ModelMessage::AssistantToolCall(read_call(
                    "third",
                    None,
                    Some(&format!("{HEADING}Check a.txt first.")),
                )),
                ModelMessage::Assistant(format!("{HEADING}Check a.txt first.")),
                ModelMessage::Assistant(r#"{"answer":"done"}"#.to_string()),
            ],
            model: turn.model.clone(),
            stop: Some(stop),
        }
    );
}

#[test]
fn foreign_reasoning_notes_carry_readable_text_within_a_bound() {
    // Arrange
    let long = format!("a{}", "é".repeat(3_000));
    let cases = [
        (
            "  Look at a.txt.  ".to_string(),
            format!("{HEADING}Look at a.txt."),
        ),
        (" \n ".to_string(), OMITTED.to_string()),
        (CLEARED_REASONING.to_string(), OMITTED.to_string()),
        (ENCRYPTED.to_string(), OMITTED.to_string()),
        (
            r#"[{"type":"message"}]"#.to_string(),
            format!(r#"{HEADING}[{{"type":"message"}}]"#),
        ),
        ("[]".to_string(), format!("{HEADING}[]")),
        (
            long.clone(),
            format!(
                "{HEADING}{}\n[harness] The rest of this reasoning was cut.",
                &long[..4_095]
            ),
        ),
    ];

    for (reasoning, note) in cases {
        let turn = turn_from(
            "kimi",
            vec![ModelMessage::AssistantReasoning {
                content: "{}".to_string(),
                reasoning_content: reasoning.clone(),
            }],
        );

        // Act
        let projected = project_turn(&turn, Some("openai"));

        // Assert
        assert_eq!(
            projected.messages,
            vec![
                ModelMessage::Assistant(note),
                ModelMessage::Assistant("{}".to_string()),
            ],
            "{reasoning}"
        );
    }
}

#[test]
fn native_turns_keep_provider_reasoning() {
    // Arrange
    let messages = vec![
        ModelMessage::AssistantToolCall(read_call("call", Some("plan"), None)),
        ModelMessage::AssistantReasoning {
            content: "{}".to_string(),
            reasoning_content: "plan".to_string(),
        },
    ];
    let direct = HistoryTurn {
        model: Some(RecordedModel {
            generation: 0,
            model: None,
            provider: None,
            registration_identity: None,
        }),
        ..HistoryTurn::from(messages.clone())
    };
    let native = [
        (turn_from("kimi", messages.clone()), Some("kimi")),
        (HistoryTurn::from(messages), Some("kimi")),
        (direct.clone(), None),
    ];

    // Act
    let projected_direct = project_turn(&direct, Some("kimi"));

    // Assert
    for (turn, target) in &native {
        assert!(matches!(
            project_turn(turn, *target),
            Cow::Borrowed(borrowed) if borrowed == turn
        ));
    }
    assert_eq!(
        projected_direct.messages,
        vec![
            ModelMessage::AssistantToolCall(read_call(
                "call",
                None,
                Some(&format!("{HEADING}plan"))
            )),
            ModelMessage::Assistant(format!("{HEADING}plan")),
            ModelMessage::Assistant("{}".to_string()),
        ]
    );
}
