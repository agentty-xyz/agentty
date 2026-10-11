use std::collections::VecDeque;
use std::num::NonZeroU64;

use serde_json::json;

use super::{
    ContextBudget, ContextBudgetError, ContextEstimator, HeuristicContextEstimator, HistoryTurn,
    TurnStop, admit_mandatory_content, advertised_tools, fit_grown_request, select_recent_turns,
};
use crate::input::{ImageContent, ImageMediaType, InputBlock, TurnInput};
use crate::model::{ModelMessage, ModelRequest};
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
    assert_eq!(partial, (vec![ModelMessage::User("recent".into())], 2));
    assert_eq!(empty, (Vec::new(), 3));
}

#[test]
fn grown_requests_are_re_admitted_against_the_whole_budget() {
    // Arrange
    let budget = budget(20).with_reserved_output(2).expect("reserve");
    let schema = OutputSchema::new(json!({"type": "object"})).expect("schema");
    let mut request = ModelRequest::new("prompt", schema).with_tool(ToolDefinition::read());
    let call = ToolCall::read(
        "call-1".to_string(),
        serde_json::from_value(json!({"action": "file", "path": "name.txt"})).expect("arguments"),
        None,
    );

    // Act
    let initial = fit_grown_request(&FixedEstimator, budget, &request, &mut 0, true);
    request.record_tool_call(call.clone());
    request.record_tool_result(&call, "content".to_string());
    let grown = fit_grown_request(&FixedEstimator, budget, &request, &mut 0, true);

    // Assert
    // Reserved 2 + one user message 7 + one tool definition 2 = 11 of 20.
    assert_eq!(initial.expect("initial request fits"), None);
    // The recorded call and result add two 7-weight messages: 25 of 20, and
    // the only result answers the latest call, so nothing can be elided.
    assert!(matches!(
        grown,
        Err(TurnError::ContextBudgetExceeded {
            budget: 20,
            required: 25,
        })
    ));
}

/// Weighs tool results by content bytes and every other item at one unit.
struct ToolResultBytesEstimator;

impl ContextEstimator for ToolResultBytesEstimator {
    fn message_weight(&self, message: &ModelMessage) -> u64 {
        match message {
            ModelMessage::ToolResult { content, .. } => {
                u64::try_from(content.len()).expect("content length fits u64")
            }
            _ => 1,
        }
    }

    fn tool_definition_weight(&self, _tool: &ToolDefinition) -> u64 {
        0
    }
}

/// Records one read round per content in `results` after a prior turn whose
/// tool result is `history_result`.
fn request_with_rounds(history_result: &str, results: &[&str]) -> ModelRequest {
    let schema = OutputSchema::new(json!({"type": "object"})).expect("schema");
    let history_call = read_call("history-call");
    let history = vec![
        ModelMessage::User("earlier".into()),
        ModelMessage::AssistantToolCall(history_call),
        ModelMessage::ToolResult {
            call_id: "history-call".into(),
            content: history_result.into(),
            name: "read".into(),
        },
        ModelMessage::Assistant("{}".into()),
    ];
    let mut request = ModelRequest::with_history(history, "prompt", schema);
    for (index, content) in results.iter().enumerate() {
        let call = read_call(&format!("call-{index}"));
        request.record_tool_call(call.clone());
        request.record_tool_result(&call, (*content).to_string());
    }

    request
}

fn tool_result_content(messages: &[ModelMessage], call_id: &str) -> String {
    messages
        .iter()
        .find_map(|message| match message {
            ModelMessage::ToolResult {
                call_id: id,
                content,
                ..
            } if id == call_id => Some(content.clone()),
            _ => None,
        })
        .expect("tool result")
}

#[test]
fn grown_requests_elide_oldest_tool_results_from_history_first_below_the_headroom() {
    // Arrange
    let large = "x".repeat(400);
    let request = request_with_rounds(&large, &[large.as_str(); 5]);
    let original = request.clone();
    let mut elided_through = 0;

    // Act
    let fitted = fit_grown_request(
        &ToolResultBytesEstimator,
        budget(2_400),
        &request,
        &mut elided_through,
        true,
    )
    .expect("elided request fits")
    .expect("request needs elision");

    // Assert
    // Six 400-byte results plus nine unit messages weigh 2,409 of 2,400.
    // Each 169-byte notice saves 231, so eliding the history result, then
    // `call-0` and `call-1`, brings the request to 1,716, below the 1,800
    // headroom target; later results keep their content.
    let history_result = tool_result_content(fitted.messages(), "history-call");
    assert!(history_result.contains("\"elided\""));
    assert!(history_result.contains("\"original_bytes\":400"));
    for call_id in ["call-0", "call-1"] {
        assert!(tool_result_content(fitted.messages(), call_id).contains("\"elided\""));
    }
    for call_id in ["call-2", "call-3", "call-4"] {
        assert_eq!(tool_result_content(fitted.messages(), call_id), large);
    }
    // Four history messages, the prompt, and the first two rounds.
    assert_eq!(elided_through, 9);
    assert_eq!(fitted.messages().len(), original.messages().len());
    assert_eq!(request, original);
}

#[test]
fn consecutive_grown_requests_keep_the_elided_prefix_unchanged() {
    // Arrange
    let large = "x".repeat(400);
    let mut request = request_with_rounds(&large, &[large.as_str(); 5]);
    let mut elided_through = 0;
    let first = fit_grown_request(
        &ToolResultBytesEstimator,
        budget(2_400),
        &request,
        &mut elided_through,
        true,
    )
    .expect("elided request fits")
    .expect("request needs elision");
    let call = read_call("call-5");
    request.record_tool_call(call.clone());
    request.record_tool_result(&call, large.clone());

    // Act
    let second = fit_grown_request(
        &ToolResultBytesEstimator,
        budget(2_400),
        &request,
        &mut elided_through,
        true,
    )
    .expect("elided request fits")
    .expect("request needs elision");

    // Assert
    // The new round grows the request to 2,810; eliding the same three
    // results again brings it to 2,117, which fits without eliding `call-2`.
    assert_eq!(
        second.messages()[..first.messages().len()],
        first.messages()[..]
    );
    assert_eq!(tool_result_content(second.messages(), "call-2"), large);
    assert_eq!(elided_through, 9);
}

#[test]
fn grown_requests_keep_the_outcome_of_elided_effects_and_warn_against_repeating_them() {
    // Arrange
    let schema = OutputSchema::new(json!({"type": "object"})).expect("schema");
    let bash_call = ToolCall::from_json(
        "bash-call".to_string(),
        "bash",
        r#"{"command":"make test"}"#,
        None,
    )
    .expect("bash call");
    let bash_result = json!({
        "exit_code": 0,
        "signal": null,
        "stdout": "x".repeat(600),
        "termination": "exited",
    })
    .to_string();
    let history = vec![
        ModelMessage::User("earlier".into()),
        ModelMessage::AssistantToolCall(bash_call),
        ModelMessage::ToolResult {
            call_id: "bash-call".into(),
            content: bash_result.clone(),
            name: "bash".into(),
        },
        ModelMessage::Assistant("{}".into()),
    ];
    let mut request = ModelRequest::with_history(history, "prompt", schema);
    let call = read_call("call-0");
    request.record_tool_call(call.clone());
    request.record_tool_result(&call, "x".repeat(400));

    // Act
    let fitted = fit_grown_request(
        &ToolResultBytesEstimator,
        budget(1_000),
        &request,
        &mut 0,
        true,
    )
    .expect("elided request fits")
    .expect("request needs elision");

    // Assert
    let notice: serde_json::Value =
        serde_json::from_str(&tool_result_content(fitted.messages(), "bash-call"))
            .expect("notice is JSON");
    assert!(
        notice["elided"]
            .as_str()
            .expect("elision text")
            .contains("do not repeat it")
    );
    assert_eq!(notice["original_bytes"], bash_result.len());
    assert_eq!(notice["exit_code"], 0);
    assert_eq!(notice["termination"], "exited");
    assert!(notice.get("signal").is_none());
    assert!(notice.get("stdout").is_none());
}

#[test]
fn grown_requests_keep_a_rejected_write_status_without_claiming_its_effects() {
    // Arrange
    let schema = OutputSchema::new(json!({"type": "object"})).expect("schema");
    let write_call = ToolCall::from_json(
        "write-call".to_string(),
        "write",
        r#"{"path":"name.txt","patch":"text"}"#,
        None,
    )
    .expect("write call");
    let rejected_write = json!({
        "error": "x".repeat(600),
        "path": "name.txt",
        "status": "rejected",
    })
    .to_string();
    let history = vec![
        ModelMessage::User("earlier".into()),
        ModelMessage::AssistantToolCall(write_call),
        ModelMessage::ToolResult {
            call_id: "write-call".into(),
            content: rejected_write,
            name: "write".into(),
        },
        ModelMessage::Assistant("{}".into()),
    ];
    let mut request = ModelRequest::with_history(history, "prompt", schema);
    let call = read_call("call-0");
    request.record_tool_call(call.clone());
    request.record_tool_result(&call, "x".repeat(400));

    // Act
    let fitted = fit_grown_request(
        &ToolResultBytesEstimator,
        budget(1_000),
        &request,
        &mut 0,
        true,
    )
    .expect("elided request fits")
    .expect("request needs elision");

    // Assert
    let notice: serde_json::Value =
        serde_json::from_str(&tool_result_content(fitted.messages(), "write-call"))
            .expect("notice is JSON");
    let elided = notice["elided"].as_str().expect("elision text");
    assert!(elided.contains("outcome recorded here"));
    assert!(!elided.contains("effects remain"));
    assert_eq!(notice["status"], "rejected");
    assert!(notice.get("error").is_none());
}

#[test]
fn grown_requests_keep_results_smaller_than_the_elision_notice() {
    // Arrange
    let request = request_with_rounds("", &["tiny", &"x".repeat(400), &"x".repeat(400)]);

    // Act
    let fitted = fit_grown_request(
        &ToolResultBytesEstimator,
        budget(700),
        &request,
        &mut 0,
        true,
    )
    .expect("elided request fits")
    .expect("request needs elision");

    // Assert
    assert_eq!(tool_result_content(fitted.messages(), "call-0"), "tiny");
    assert!(tool_result_content(fitted.messages(), "call-1").contains("\"elided\""));
    assert_eq!(
        tool_result_content(fitted.messages(), "call-2"),
        "x".repeat(400)
    );
}

#[test]
fn grown_requests_fail_typed_when_the_latest_round_alone_cannot_fit() {
    // Arrange
    let large = "x".repeat(400);
    let request = request_with_rounds(&large, &[&large, &large]);

    // Act
    let rejected = fit_grown_request(
        &ToolResultBytesEstimator,
        budget(500),
        &request,
        &mut 0,
        true,
    );

    // Assert
    // Eliding the history result and the first in-turn result still leaves
    // the latest result, two notices, and six unit messages: 744 of 500.
    assert!(matches!(
        rejected,
        Err(TurnError::ContextBudgetExceeded {
            budget: 500,
            required: 744,
        })
    ));
}

#[test]
fn grown_requests_that_may_not_extend_elision_still_re_elide_the_prefix() {
    // Arrange
    let large = "x".repeat(400);
    let request = request_with_rounds(&large, &[&large, &large, &large]);
    let mut elided_through = 4;

    // Act
    let fitted = fit_grown_request(
        &ToolResultBytesEstimator,
        budget(1_400),
        &request,
        &mut elided_through,
        false,
    )
    .expect("prefix-elided request fits")
    .expect("request needs elision");

    // Assert
    // Re-eliding the history result brings 1,607 to 1,376 of 1,400.
    assert!(tool_result_content(fitted.messages(), "history-call").contains("\"elided\""));
    assert_eq!(tool_result_content(fitted.messages(), "call-0"), large);
    assert_eq!(elided_through, 4);
}

#[test]
fn grown_requests_that_may_not_extend_elision_fail_typed_at_their_prefix_elided_weight() {
    // Arrange
    let large = "x".repeat(400);
    let request = request_with_rounds(&large, &[&large, &large, &large]);
    let mut elided_through = 4;

    // Act
    let rejected = fit_grown_request(
        &ToolResultBytesEstimator,
        budget(1_200),
        &request,
        &mut elided_through,
        false,
    );

    // Assert
    // Eliding `call-0` and `call-1` too would fit this request; without
    // extension it is rejected once the history result is re-elided.
    assert!(matches!(
        rejected,
        Err(TurnError::ContextBudgetExceeded {
            budget: 1_200,
            required: 1_376,
        })
    ));
    assert_eq!(elided_through, 4);
}

#[test]
fn selection_elides_or_briefs_a_completed_turn_whose_tool_rounds_do_not_fit() {
    // Arrange
    let large = "x".repeat(400);
    let older = vec![
        ModelMessage::User("older".to_string()),
        ModelMessage::Assistant("{}".to_string()),
    ];
    let mut oversized = vec![ModelMessage::User("prompt".to_string())];
    for call_id in ["call-0", "call-1"] {
        oversized.push(ModelMessage::AssistantToolCall(read_call(call_id)));
        oversized.push(ModelMessage::ToolResult {
            call_id: call_id.to_string(),
            content: large.clone(),
            name: "read".to_string(),
        });
    }
    oversized.push(ModelMessage::Assistant("{}".to_string()));
    let turns = VecDeque::from(vec![
        HistoryTurn::from(older.clone()),
        HistoryTurn::from(oversized.clone()),
    ]);

    // Act
    // The oversized turn weighs 804; eliding its first result brings it to
    // 573, and eliding both still leaves 342.
    let (elided, elided_evicted) = select_recent_turns(&ToolResultBytesEstimator, &turns, 600);
    let (brief, brief_evicted) = select_recent_turns(&ToolResultBytesEstimator, &turns, 300);

    // Assert
    assert_eq!(elided.len(), older.len() + oversized.len());
    assert_eq!(elided[..2], older[..]);
    assert!(tool_result_content(&elided, "call-0").contains("\"elided\""));
    assert_eq!(tool_result_content(&elided, "call-1"), large);
    assert_eq!(elided_evicted, 0);
    assert_eq!(
        brief,
        [
            older,
            vec![
                ModelMessage::User("prompt".to_string()),
                ModelMessage::Assistant("{}".to_string()),
            ],
        ]
        .concat()
    );
    assert_eq!(brief_evicted, 0);
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
fn completed_turns_replay_verbatim_and_briefly_as_input_and_output() {
    // Arrange
    let messages = vec![
        ModelMessage::User("question".to_string()),
        ModelMessage::AssistantToolCall(read_call("call")),
        result("call"),
        ModelMessage::Assistant("answer".to_string()),
    ];
    let turn = HistoryTurn::from(messages.clone());
    let input_only = HistoryTurn::from(vec![ModelMessage::User("only".to_string())]);

    // Act
    let replay = turn.replay();
    let brief = turn.brief_replay();
    let input_only_brief = input_only.brief_replay();

    // Assert
    assert_eq!(replay, messages);
    assert_eq!(
        brief,
        vec![
            ModelMessage::User("question".to_string()),
            ModelMessage::Assistant("answer".to_string()),
        ]
    );
    assert_eq!(
        input_only_brief,
        vec![ModelMessage::User("only".to_string())]
    );
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

    // Assert
    assert_eq!(projected, stopped.brief_replay());
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
