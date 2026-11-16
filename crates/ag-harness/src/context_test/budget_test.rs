//! Context budget, weight heuristic, mandatory admission, and stopped-turn
//! replay unit tests.

use std::time::Duration;

use serde_json::json;

use super::support::{ByteEstimator, FixedEstimator, budget, read_call, result};
use crate::bash::{BashConfig, Enforcement};
use crate::context::{
    ContextBudgetError, ContextEstimator, HeuristicContextEstimator, HistoryTurn, TurnStop,
    admit_mandatory_content, advertised_tools, window_history_bytes,
};
use crate::input::{ImageContent, ImageMediaType, InputBlock, TurnInput};
use crate::model::ModelMessage;
use crate::policy::ToolPolicy;
use crate::schema_contract::OutputSchema;
use crate::tool::{Tool, ToolDefinition};
use crate::turn::{TurnError, TurnOptions};

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
    let estimator = HeuristicContextEstimator::DEFAULT;
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
fn declared_heuristic_weights_use_their_rates_and_fixed_tokens() {
    // Arrange
    let schema = OutputSchema::new(json!({"type": "object"})).expect("schema");
    let estimator = HeuristicContextEstimator::DEFAULT
        .with_messages(225, 40)
        .with_tool_definitions(340, 2)
        .with_request_tokens(420);
    let in_prompt = estimator.with_output_schema_in_prompt();

    // Act
    let message = estimator.message_weight(&ModelMessage::User("x".repeat(1_000)));
    let tool =
        estimator.tool_definition_weight(&ToolDefinition::bash(Enforcement::FileSystemAndNetwork));
    let request = estimator.request_weight(Some(&schema));
    let request_with_schema = in_prompt.request_weight(Some(&schema));
    let text_request = in_prompt.request_weight(None);

    // Assert
    assert_eq!(message, 265);
    let definition = ToolDefinition::bash(Enforcement::FileSystemAndNetwork);
    let definition_bytes = definition.name().len()
        + definition.description().len()
        + definition.parameters().to_string().len();
    assert_eq!(
        tool,
        (u64::try_from(definition_bytes).expect("bytes") * 340).div_ceil(1_000) + 2
    );
    assert_eq!(request, 420);
    // `{"type":"object"}` is 17 bytes: 4 tokens at 225 per 1,000, plus the
    // 40 every message carries.
    assert_eq!(request_with_schema, 464);
    // A plain-text request states no schema.
    assert_eq!(text_request, 420);
    assert_eq!(
        HeuristicContextEstimator::DEFAULT.request_weight(Some(&schema)),
        0
    );
    assert_eq!(
        HeuristicContextEstimator::default(),
        HeuristicContextEstimator::DEFAULT
    );
}

#[test]
fn mandatory_admission_includes_the_fixed_request_weight() {
    // Arrange
    let estimator = HeuristicContextEstimator::DEFAULT.with_request_tokens(3);

    // Act
    let remaining = admit_mandatory_content(
        &estimator,
        budget(10),
        None,
        &TurnInput::text("abcd"),
        &options_with(ToolPolicy::default()),
    );

    // Assert
    // The input message weighs 1 + 4 and the request 3, leaving 2 of 10.
    assert_eq!(remaining.expect("fits"), 2);
}

#[test]
fn turn_instructions_count_toward_mandatory_admission() {
    // Arrange
    let estimator = HeuristicContextEstimator::DEFAULT.with_request_tokens(3);
    let input = TurnInput::text("abcd");
    let instructed = options_with(ToolPolicy::default()).with_instructions("abcdefgh");

    // Act
    let plain = admit_mandatory_content(
        &estimator,
        budget(10),
        None,
        &input,
        &options_with(ToolPolicy::default()),
    );
    let rejected = admit_mandatory_content(&estimator, budget(10), None, &input, &instructed);

    // Assert
    assert_eq!(plain.expect("fits"), 2);
    assert!(
        rejected.is_err(),
        "the instructions do not fit beside the input"
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
fn advertised_bash_states_the_configured_executor_enforcement() {
    // Arrange
    let bash_policy = ToolPolicy::default().allow(Tool::Bash);
    #[cfg(feature = "native-sandbox")]
    let (configuration, enforcement) = (
        BashConfig::new(
            "/trusted/launcher".into(),
            "/bin/bash".into(),
            "revision".into(),
            Duration::from_secs(5),
            1024,
        ),
        Enforcement::FileSystemAndNetwork,
    );
    #[cfg(not(feature = "native-sandbox"))]
    let (configuration, enforcement) = (
        BashConfig::for_executor(
            std::sync::Arc::new(crate::bash::UnsandboxedExecutor::without_isolation()),
            "/bin/bash".into(),
            "revision".into(),
            Duration::from_secs(5),
            1024,
        ),
        Enforcement::None,
    );
    let configuration = configuration.expect("configuration");
    let unconfigured = options_with(bash_policy);

    // Act
    let configured = advertised_tools(&options_with(bash_policy).with_bash(configuration));
    let missing = advertised_tools(&unconfigured);

    // Assert
    assert_eq!(configured, [ToolDefinition::bash(enforcement)]);
    assert_eq!(missing, [ToolDefinition::bash(Enforcement::None)]);
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

/// Safety cap passed to [`window_history_bytes`] by tests that stay below it.
const HISTORY_CAP: usize = 256 * 1024;

#[test]
fn window_history_bytes_convert_the_weight_left_after_fixed_parts() {
    // Arrange
    let estimator = HeuristicContextEstimator::DEFAULT
        .with_messages(500, 4)
        .with_request_tokens(3);
    let budget = budget(10_000).with_reserved_output(1_000).expect("reserve");
    let options = options_with(ToolPolicy::default().allow(Tool::Read).allow(Tool::Write));
    let tools_weight: u64 = advertised_tools(&options)
        .iter()
        .map(|tool| estimator.tool_definition_weight(tool))
        .sum();

    // Act
    let bytes = window_history_bytes(&estimator, budget, Some("system"), &options, HISTORY_CAP);

    // Assert
    // Of the 9,000 left after reserved output, uncalibrated requests fill
    // 8,100; 900 stay for the input, and request 3 plus the six-byte system
    // prompt 3 + 4 leave the rest for history at two bytes per weight unit.
    let history_weight = 8_100 - 900 - 3 - 7 - tools_weight;
    let expected = usize::try_from(history_weight * 2).expect("small budget");
    assert_eq!(bytes, Some(expected));
}

#[test]
fn window_history_bytes_need_an_estimator_that_weighs_bytes() {
    // Arrange
    let options = options_with(ToolPolicy::default());

    // Act
    let bytes = window_history_bytes(&FixedEstimator, budget(1_000), None, &options, HISTORY_CAP);

    // Assert
    assert_eq!(bytes, None);
}

#[test]
fn window_history_bytes_are_absent_when_fixed_parts_fill_the_window() {
    // Arrange
    let budget = budget(1_000).with_reserved_output(900).expect("reserve");
    let options = options_with(ToolPolicy::default());

    // Act
    let bytes = window_history_bytes(
        &HeuristicContextEstimator::DEFAULT,
        budget,
        Some(&"a system prompt longer than the window ".repeat(20)),
        &options,
        HISTORY_CAP,
    );

    // Assert
    assert_eq!(bytes, None);
}

#[test]
fn window_history_bytes_stop_at_the_safety_cap() {
    // Arrange
    let options = options_with(ToolPolicy::default());

    // Act
    let bytes = window_history_bytes(
        &ByteEstimator,
        budget(u64::MAX),
        None,
        &options,
        HISTORY_CAP,
    );

    // Assert
    assert_eq!(bytes, Some(HISTORY_CAP));
}

#[test]
fn window_history_bytes_probe_mixed_content_for_tokenizer_estimators() {
    // Arrange
    let options = options_with(ToolPolicy::default());

    // Act
    let bytes = window_history_bytes(
        &RunCompressingEstimator,
        budget(1_000),
        None,
        &options,
        HISTORY_CAP,
    );

    // Assert
    // A probe of one repeated character would weigh a single unit and hit the
    // cap; the mixed probe keeps about one byte per weight unit, so the 800
    // history weight left by the margin and input share holds under 1,000.
    let bytes = bytes.expect("history fits");
    assert!((800..1_000).contains(&bytes), "{bytes}");
}

/// Weighs each run of one repeated character as a single unit, like a
/// tokenizer that merges long runs, and every other byte as one unit.
struct RunCompressingEstimator;

impl ContextEstimator for RunCompressingEstimator {
    fn message_weight(&self, message: &ModelMessage) -> u64 {
        let ModelMessage::User(text) = message else {
            return 0;
        };
        let mut previous = None;
        let runs = text
            .chars()
            .filter(|character| previous.replace(*character) != Some(*character))
            .count();

        u64::try_from(runs).expect("run count fits u64")
    }

    fn tool_definition_weight(&self, _tool: &ToolDefinition) -> u64 {
        0
    }

    fn request_weight(&self, _schema: &OutputSchema) -> u64 {
        0
    }
}

#[test]
fn mandatory_admission_weighs_input_as_its_canonical_message() {
    // Arrange
    let budget = budget(5);
    let blocks = vec![InputBlock::Text("ab".into()), InputBlock::Text("cd".into())];
    let input = TurnInput::from_blocks(blocks).expect("input");

    // Act
    let rejected = admit_mandatory_content(
        &HeuristicContextEstimator::DEFAULT,
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

fn placeholder(id: &str) -> ModelMessage {
    ModelMessage::ToolResult {
        call_id: id.to_string(),
        content: r#"{"error":"no result was recorded before the turn stopped"}"#.to_string(),
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
        model: None,
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
        model: None,
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
fn completed_turns_replay_verbatim_and_briefly_as_input_and_output() {
    // Arrange
    let messages = vec![
        ModelMessage::User("question".to_string()),
        ModelMessage::AssistantToolCall(read_call("call")),
        result("call"),
        ModelMessage::Assistant("answer".to_string()),
    ];
    let turn = HistoryTurn::from(messages.clone());

    // Act
    let replay = turn.replay();
    let brief = turn.brief().replay();

    // Assert
    assert_eq!(replay, messages);
    assert_eq!(
        brief,
        [
            ModelMessage::User("question".to_string()),
            ModelMessage::Assistant("answer".to_string()),
        ]
    );
}
