use std::collections::VecDeque;
use std::num::NonZeroU64;

use serde_json::json;

use super::{
    ContextBudget, ContextBudgetError, ContextEstimator, HeuristicContextEstimator, HistoryTurn,
    TurnStop, admit_mandatory_content, advertised_tools, elide_reasoning, elide_tool_result,
    request_weight, select_recent_turns,
};
use crate::input::{ImageContent, ImageMediaType, InputBlock, TurnInput};
use crate::model::ModelMessage;
use crate::policy::ToolPolicy;
use crate::schema_contract::OutputSchema;
use crate::tool::{Tool, ToolCall, ToolDefinition};
use crate::turn::{TurnError, TurnOptions};

struct FixedEstimator;

impl ContextEstimator for FixedEstimator {
    fn message_weight(&self, _message: &ModelMessage) -> u64 {
        7
    }

    fn tool_definition_weight(&self, _tool: &ToolDefinition) -> u64 {
        2
    }
}

fn budget(max_request_weight: u64) -> ContextBudget {
    ContextBudget::new(NonZeroU64::new(max_request_weight).expect("nonzero budget"))
}

fn options_with(tool_policy: ToolPolicy) -> TurnOptions {
    let schema = OutputSchema::new(json!({"type": "object"})).expect("schema");

    TurnOptions::new(schema, tool_policy)
}

#[test]
fn budget_reserves_output_within_capacity() {
    // Arrange
    let unreserved = budget(20);

    // Act
    let reserved = unreserved.with_reserved_output(5).expect("valid reserve");

    // Assert
    assert_eq!(unreserved.reserved_output_weight(), 0);
    assert_eq!(reserved.max_request_weight().get(), 20);
    assert_eq!(reserved.reserved_output_weight(), 5);
}

#[test]
fn budget_rejects_reservation_without_request_capacity() {
    // Arrange
    let unreserved = budget(20);

    // Act
    let rejected = unreserved.with_reserved_output(20);

    // Assert
    assert_eq!(
        rejected,
        Err(ContextBudgetError::ReservedOutputExceedsBudget {
            max_request_weight: 20,
            reserved_output_weight: 20,
        })
    );
}

#[test]
fn heuristic_weights_are_deterministic_byte_ratios() {
    // Arrange
    let estimator = HeuristicContextEstimator;
    let signature = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    let image = ImageContent::new(ImageMediaType::Png, signature).expect("image");
    let image_input = TurnInput::from_blocks(vec![InputBlock::Image(image)]).expect("image input");

    // Act
    let empty_user = estimator.message_weight(&ModelMessage::User(String::new()));
    let short_user = estimator.message_weight(&ModelMessage::User("abcd".into()));
    let system = estimator.message_weight(&ModelMessage::System("abcdefgh".into()));
    let weighed_image = estimator.message_weight(&ModelMessage::UserInput(image_input));
    let read_tool = estimator.tool_definition_weight(&ToolDefinition::read());

    // Assert
    assert_eq!(empty_user, 4);
    assert_eq!(short_user, 5);
    assert_eq!(system, 6);
    // "data:" + "image/png" + ";base64," + 12 base64 bytes = 34 bytes.
    assert_eq!(weighed_image, 13);
    let definition = ToolDefinition::read();
    let definition_bytes = definition.name().len()
        + definition.description().len()
        + definition.parameters().to_string().len();
    assert_eq!(
        read_tool,
        u64::try_from(definition_bytes).expect("bytes").div_ceil(4) + 4
    );
}

#[test]
fn advertised_tools_follow_the_turn_policy() {
    // Arrange
    let denied = options_with(ToolPolicy::default());
    let read_only = options_with(ToolPolicy::default().allow(Tool::Read));
    let read_write = options_with(ToolPolicy::default().allow(Tool::Read).allow(Tool::Write));
    let bash_only = options_with(ToolPolicy::default().allow(Tool::Bash));

    // Act
    let none = advertised_tools(&denied);
    let read = advertised_tools(&read_only);
    let both = advertised_tools(&read_write);
    let bash = advertised_tools(&bash_only);

    // Assert
    assert_eq!(none.len(), 0);
    assert_eq!(
        read.iter().map(ToolDefinition::name).collect::<Vec<_>>(),
        ["read"]
    );
    assert_eq!(
        both.iter().map(ToolDefinition::name).collect::<Vec<_>>(),
        ["read", "write"]
    );
    assert_eq!(
        bash.iter().map(ToolDefinition::name).collect::<Vec<_>>(),
        ["bash"]
    );
}

#[test]
fn mandatory_content_admission_accounts_for_every_component() {
    // Arrange
    let budget = budget(25).with_reserved_output(5).expect("reserve");
    let options = options_with(ToolPolicy::default().allow(Tool::Read).allow(Tool::Write));

    // Act
    let remaining = admit_mandatory_content(
        &FixedEstimator,
        budget,
        Some("system"),
        &TurnInput::text("prompt"),
        &options,
    );

    // Assert
    // Reserved 5 + system 7 + input message 7 + two tool definitions 4 = 23
    // of 25.
    assert_eq!(remaining.expect("fits"), 2);
}

#[test]
fn mandatory_admission_weighs_input_as_its_canonical_message() {
    // Arrange
    let budget = budget(5);
    let blocks = vec![InputBlock::Text("ab".into()), InputBlock::Text("cd".into())];
    let input = TurnInput::from_blocks(blocks).expect("input");

    // Act
    let rejected = admit_mandatory_content(
        &HeuristicContextEstimator,
        budget,
        None,
        &input,
        &options_with(ToolPolicy::default()),
    );

    // Assert
    // The request sends the joined user message "ab\n\ncd", so admission
    // weighs its six bytes, including the separator the raw blocks omit.
    assert!(matches!(
        rejected.expect_err("separator bytes exceed the budget"),
        TurnError::ContextBudgetExceeded {
            budget: 5,
            required: 6,
        }
    ));
}

#[test]
fn oversized_mandatory_content_returns_a_typed_error() {
    // Arrange
    let budget = budget(10);
    let options = options_with(ToolPolicy::default().allow(Tool::Read).allow(Tool::Write));

    // Act
    let rejected = admit_mandatory_content(
        &FixedEstimator,
        budget,
        Some("system"),
        &TurnInput::text("prompt"),
        &options,
    );

    // Assert
    let error = rejected.expect_err("over budget");
    assert!(matches!(
        error,
        TurnError::ContextBudgetExceeded {
            budget: 10,
            required: 18,
        }
    ));
}

#[test]
fn selection_keeps_the_most_recent_whole_turns() {
    // Arrange
    let turns = VecDeque::from(vec![
        HistoryTurn::from(vec![
            ModelMessage::User("old".into()),
            ModelMessage::Assistant("old answer".into()),
        ]),
        HistoryTurn::from(vec![ModelMessage::User("middle".into())]),
        HistoryTurn::from(vec![
            ModelMessage::ToolResult {
                call_id: "call".into(),
                content: "result".into(),
                name: "read".into(),
            },
            ModelMessage::Assistant("new answer".into()),
        ]),
    ]);

    // Act
    let (projected, evicted) = select_recent_turns(&FixedEstimator, &turns, 21);
    let projected = projected.concat();

    // Assert
    // The two-message oldest turn is dropped wholesale.
    assert_eq!(evicted, 1);
    assert_eq!(projected.len(), 3);
    assert_eq!(projected[0], ModelMessage::User("middle".into()));
    assert!(matches!(
        &projected[1],
        ModelMessage::ToolResult { call_id, .. } if call_id == "call"
    ));
}

#[test]
fn selection_stops_at_the_first_turn_that_does_not_fit() {
    // Arrange
    let turns = VecDeque::from(vec![
        HistoryTurn::from(vec![ModelMessage::User("small".into())]),
        HistoryTurn::from(vec![
            ModelMessage::User("large".into()),
            ModelMessage::Assistant("large answer".into()),
        ]),
        HistoryTurn::from(vec![ModelMessage::User("recent".into())]),
    ]);

    // Act
    let partial = select_recent_turns(&FixedEstimator, &turns, 8);
    let empty = select_recent_turns(&FixedEstimator, &turns, 0);

    // Assert
    // The older single-message turn would fit, but selection never skips the
    // two-message turn between it and the retained suffix.
    assert_eq!(
        partial,
        (vec![vec![ModelMessage::User("recent".into())]], 2)
    );
    assert_eq!(empty, (Vec::new(), 3));
}

#[test]
fn request_weight_counts_reserved_output_messages_and_tools() {
    // Arrange
    let budget = budget(20).with_reserved_output(2).expect("reserve");
    let messages = [
        ModelMessage::User("prompt".to_string()),
        ModelMessage::User("next".to_string()),
    ];
    let tools = [ToolDefinition::read()];

    // Act
    let weight = request_weight(&FixedEstimator, budget, &messages, &tools);

    // Assert
    // Reserved 2 + two 7-weight messages + one 2-weight tool definition.
    assert_eq!(weight, 18);
}

fn read_call(id: &str) -> ToolCall {
    ToolCall::read(
        id.to_string(),
        serde_json::from_value(json!({"action": "file", "path": "name.txt"})).expect("arguments"),
        None,
    )
}

fn placeholder(id: &str) -> ModelMessage {
    ModelMessage::ToolResult {
        call_id: id.to_string(),
        content: r#"{"error":"no result was recorded before the turn stopped"}"#.to_string(),
        name: "read".to_string(),
    }
}

fn result(id: &str) -> ModelMessage {
    ModelMessage::ToolResult {
        call_id: id.to_string(),
        content: "content".to_string(),
        name: "read".to_string(),
    }
}

fn interrupted() -> TurnStop {
    TurnStop {
        error_type: "cancelled".to_string(),
        ..TurnStop::default()
    }
}

#[test]
fn stopped_turn_replay_answers_unfinished_tool_calls_and_ends_with_its_note() {
    // Arrange
    let turn = HistoryTurn {
        messages: vec![
            ModelMessage::User("inspect".to_string()),
            ModelMessage::AssistantToolCall(read_call("single")),
            ModelMessage::AssistantToolCalls(vec![read_call("first"), read_call("second")]),
            result("first"),
        ],
        stop: Some(TurnStop {
            error_type: "Tool".to_string(),
            failed: true,
            unfinished_commands: vec!["cargo test".to_string()],
            unfinished_writes: vec!["src/lib.rs".to_string()],
        }),
    };

    // Act
    let replay = turn.replay();

    // Assert
    assert_eq!(
        replay,
        vec![
            ModelMessage::User("inspect".to_string()),
            ModelMessage::AssistantToolCall(read_call("single")),
            placeholder("single"),
            ModelMessage::AssistantToolCalls(vec![read_call("first"), read_call("second")]),
            result("first"),
            placeholder("second"),
            ModelMessage::User(
                "[harness] The turn above failed (Tool) before completing and shows only what it \
                 recorded. Writes with unknown outcome: `src/lib.rs`. Commands with unknown \
                 outcome: `cargo test`."
                    .to_string()
            ),
        ]
    );
}

#[test]
fn stop_note_bounds_the_effects_it_names() {
    // Arrange
    let long_command = format!("{}é tail", "x".repeat(199));
    let mut commands = vec![long_command];
    commands.extend((1..10).map(|index| format!("command {index}")));
    let turn = HistoryTurn {
        messages: vec![ModelMessage::User("run".to_string())],
        stop: Some(TurnStop {
            unfinished_commands: commands,
            ..interrupted()
        }),
    };

    // Act
    let replay = turn.replay();

    // Assert
    let ModelMessage::User(note) = &replay[1] else {
        std::panic::resume_unwind(Box::new(format!("stop note expected, got {:?}", replay[1])));
    };
    assert!(note.starts_with("[harness] The turn above was interrupted (cancelled)"));
    assert!(note.contains(&format!("`{}`", "x".repeat(199))));
    assert!(!note.contains('é'));
    assert!(note.contains("`command 7`, and 2 more."));
    assert!(!note.contains("command 8"));
    assert!(!note.contains("Writes"));
}

#[test]
fn completed_turns_replay_verbatim_without_a_brief_form() {
    // Arrange
    let messages = vec![
        ModelMessage::User("question".to_string()),
        ModelMessage::Assistant("answer".to_string()),
    ];
    let turn = HistoryTurn::from(messages.clone());

    // Act
    let replay = turn.replay();
    let brief = turn.brief_replay();

    // Assert
    assert_eq!(replay, messages);
    assert_eq!(brief, None);
}

#[test]
fn selection_keeps_only_input_and_note_of_a_stopped_turn_that_does_not_fit() {
    // Arrange
    let stopped = HistoryTurn {
        messages: vec![
            ModelMessage::User("stopped".to_string()),
            ModelMessage::AssistantToolCall(read_call("call")),
            result("call"),
        ],
        stop: Some(interrupted()),
    };
    let turns = VecDeque::from(vec![
        HistoryTurn::from(vec![ModelMessage::User("older".to_string())]),
        stopped.clone(),
    ]);

    // Act
    // The full stopped replay weighs 28 and its brief form 14; nothing
    // older fits after the brief form.
    let (projected, evicted) = select_recent_turns(&FixedEstimator, &turns, 20);
    let (full, full_evicted) = select_recent_turns(&FixedEstimator, &turns, 35);
    let (empty, empty_evicted) = select_recent_turns(&FixedEstimator, &turns, 13);
    let (projected, full) = (projected.concat(), full.concat());

    // Assert
    assert_eq!(
        projected,
        stopped
            .brief_replay()
            .expect("stopped turn has a brief form")
    );
    assert_eq!(evicted, 1);
    assert_eq!(
        full,
        [
            vec![ModelMessage::User("older".to_string())],
            stopped.replay()
        ]
        .concat()
    );
    assert_eq!(full_evicted, 0);
    assert_eq!((empty, empty_evicted), (Vec::new(), 2));
}

fn reasoning_call(id: &str, reasoning: &str) -> ToolCall {
    ToolCall::from_json(
        id.to_string(),
        "read",
        r#"{"action":"file","path":"name.txt"}"#,
        Some(reasoning.to_string()),
    )
    .expect("tool call")
}

#[test]
fn elide_tool_result_replaces_only_bulky_results() {
    // Arrange
    let bulky = ModelMessage::ToolResult {
        call_id: "call-1".to_string(),
        content: "x".repeat(500),
        name: "bash".to_string(),
    };
    let small = ModelMessage::ToolResult {
        call_id: "call-2".to_string(),
        content: "ok".to_string(),
        name: "bash".to_string(),
    };

    // Act
    let elided = elide_tool_result(&bulky);
    let kept = elide_tool_result(&small);
    let other = elide_tool_result(&ModelMessage::User("x".repeat(500)));

    // Assert
    assert!(matches!(
        &elided,
        Some(ModelMessage::ToolResult { call_id, content, name })
            if call_id == "call-1"
                && name == "bash"
                && serde_json::from_str::<serde_json::Value>(content).is_ok_and(|note| {
                    note["elided"]
                        .as_str()
                        .is_some_and(|text| text.starts_with("500 bytes of earlier `bash` output"))
                })
    ));
    assert_eq!(kept, None);
    assert_eq!(other, None);
}

#[test]
fn elide_reasoning_keeps_a_placeholder_on_every_reasoning_message() {
    // Arrange
    let long = "because ".repeat(20);
    let answer = ModelMessage::AssistantReasoning {
        content: "{}".to_string(),
        reasoning_content: long.clone(),
    };
    let batch = ModelMessage::AssistantToolCalls(vec![
        reasoning_call("call-1", &long),
        reasoning_call("call-2", "short"),
    ]);
    let short = ModelMessage::AssistantToolCall(reasoning_call("call-3", "short"));

    // Act
    let elided_answer = elide_reasoning(&answer);
    let elided_batch = elide_reasoning(&batch);
    let elided_short = elide_reasoning(&short);

    // Assert
    let placeholder = "[earlier reasoning elided to fit the context window]";
    assert_eq!(
        elided_answer,
        Some(ModelMessage::AssistantReasoning {
            content: "{}".to_string(),
            reasoning_content: placeholder.to_string(),
        })
    );
    assert!(matches!(
        &elided_batch,
        Some(ModelMessage::AssistantToolCalls(calls))
            if calls[0].reasoning_content() == Some(placeholder)
                && calls[1].reasoning_content() == Some("short")
    ));
    assert_eq!(elided_short, None);
}

#[test]
fn projection_replays_an_oversized_turn_with_its_tool_output_elided() {
    // Arrange
    let call = reasoning_call("call-1", &"why ".repeat(200));
    let turns = VecDeque::from([
        HistoryTurn::from(vec![ModelMessage::User("older".to_string())]),
        HistoryTurn::from(vec![
            ModelMessage::User("inspect".to_string()),
            ModelMessage::AssistantToolCall(call),
            ModelMessage::ToolResult {
                call_id: "call-1".to_string(),
                content: "x".repeat(4_000),
                name: "read".to_string(),
            },
            ModelMessage::Assistant("{\"answer\":\"done\"}".to_string()),
        ]),
    ]);

    let elided_weight = turns[1]
        .elided_replay()
        .iter()
        .map(|message| HeuristicContextEstimator.message_weight(message))
        .sum::<u64>();
    let full_weight = turns[1]
        .replay()
        .iter()
        .map(|message| HeuristicContextEstimator.message_weight(message))
        .sum::<u64>();

    // Act
    let (projected, evicted) =
        select_recent_turns(&HeuristicContextEstimator, &turns, elided_weight);
    let projected = projected.concat();

    // Assert
    assert!(full_weight > elided_weight);
    assert_eq!(evicted, 1);
    assert_eq!(projected.len(), 4);
    assert_eq!(projected[0], ModelMessage::User("inspect".to_string()));
    assert!(matches!(
        &projected[1],
        ModelMessage::AssistantToolCall(call) if call.reasoning_content().is_some_and(|text| text.starts_with("[earlier"))
    ));
    assert!(matches!(
        &projected[2],
        ModelMessage::ToolResult { content, .. } if content.contains("4000 bytes")
    ));
    assert_eq!(
        projected[3],
        ModelMessage::Assistant("{\"answer\":\"done\"}".to_string())
    );
}
