use std::collections::VecDeque;

use serde_json::json;

use super::support::{
    ByteEstimator, read_call, reasoned_read_call, result_json, tool_result, write_call,
};
use crate::context::{
    CLEARED_REASONING, ContextEstimator, ELIDED_SOURCE_LIMIT_FACTOR, HistoryTurn, TurnStop,
    fit_history_bytes, select_recent_turns,
};
use crate::model::ModelMessage;
use crate::tool::ToolCall;

const NOTES: &str = "name.txt holds the name; next I edit src/lib.rs.";

fn retained(turn: &HistoryTurn) -> usize {
    turn.messages.iter().map(ModelMessage::retained_bytes).sum()
}

fn tool_heavy_turn(text: &str) -> HistoryTurn {
    let read = reasoned_read_call("r1", Some(&"r".repeat(500)))
        .with_assistant_text(Some(NOTES.to_string()))
        .expect("bounded text");
    let write = write_call("w1", &format!("@@ -1 +1 @@\n-a\n+{}\n", "b".repeat(500)));
    HistoryTurn::from(vec![
        ModelMessage::User(text.to_string()),
        ModelMessage::AssistantToolCalls(vec![read, write]),
        tool_result("r1", "read", &"x".repeat(2000)),
        tool_result(
            "w1",
            "write",
            &json!({"bytes_written": 500, "path": "src/lib.rs", "status": "applied"}).to_string(),
        ),
        ModelMessage::AssistantReasoning {
            content: r#"{"summary":"done"}"#.to_string(),
            reasoning_content: "t".repeat(500),
        },
    ])
}

#[test]
fn elided_turns_clear_tool_traffic_but_keep_well_formed_groups() {
    // Arrange
    let turn = tool_heavy_turn("work");

    // Act
    let elided = turn.elided();

    // Assert
    assert_eq!(elided.messages.len(), turn.messages.len());
    assert_eq!(elided.messages[0], turn.messages[0]);
    let ModelMessage::AssistantToolCalls(calls) = &elided.messages[1] else {
        std::panic::resume_unwind(Box::new("batch expected"));
    };
    assert_eq!(
        calls.iter().map(ToolCall::id).collect::<Vec<_>>(),
        ["r1", "w1"]
    );
    assert_eq!(calls[0].reasoning_content(), Some(CLEARED_REASONING));
    assert_eq!(calls[0].assistant_text(), Some(NOTES));
    assert!(
        calls[1]
            .write_arguments()
            .expect("write arguments")
            .patch()
            .contains("was applied")
    );
    assert_eq!(
        result_json(&elided.messages[2])["original_bytes"],
        json!(2000)
    );
    assert_eq!(elided.messages[3], turn.messages[3]);
    assert_eq!(
        elided.messages[4],
        ModelMessage::AssistantReasoning {
            content: r#"{"summary":"done"}"#.to_string(),
            reasoning_content: CLEARED_REASONING.to_string(),
        }
    );
    // Notices loaded from reduced history are never cleared again.
    assert_eq!(elided.elided(), elided);
}

#[test]
fn brief_turns_keep_the_input_and_any_final_output() {
    // Arrange
    let completed = tool_heavy_turn("work");
    let input_only = HistoryTurn::from(vec![ModelMessage::User("alone".to_string())]);
    let stopped = HistoryTurn {
        messages: vec![
            ModelMessage::User("stopped".to_string()),
            ModelMessage::AssistantToolCall(read_call("call")),
        ],
        stop: Some(TurnStop {
            error_type: "cancelled".to_string(),
            ..TurnStop::default()
        }),
    };

    // Act
    let completed_brief = completed.brief();
    let input_brief = input_only.brief();
    let stopped_brief = stopped.brief();

    // Assert
    assert_eq!(
        completed_brief.messages,
        [
            ModelMessage::User("work".to_string()),
            ModelMessage::AssistantReasoning {
                content: r#"{"summary":"done"}"#.to_string(),
                reasoning_content: CLEARED_REASONING.to_string(),
            },
        ]
    );
    assert_eq!(input_brief, input_only);
    assert_eq!(
        stopped_brief.messages,
        [ModelMessage::User("stopped".to_string())]
    );
    assert_eq!(stopped_brief.stop, stopped.stop);
}

#[test]
fn byte_fitting_prefers_the_full_then_elided_then_brief_form() {
    // Arrange
    let turn = tool_heavy_turn("work");
    let full = retained(&turn);
    let elided = retained(&turn.elided());
    let brief = retained(&turn.brief());

    // Act
    let as_full = turn.clone().fit_bytes(full, full);
    let as_elided = turn.clone().fit_bytes(full - 1, full);
    let as_brief = turn.clone().fit_bytes(elided - 1, full);
    let rejected = turn.clone().fit_bytes(brief - 1, full);

    // Assert
    assert!(brief < elided && elided < full);
    assert_eq!(as_full, Some(turn.clone()));
    assert_eq!(as_elided, Some(turn.elided()));
    assert_eq!(as_brief, Some(turn.brief()));
    assert_eq!(rejected, None);
}

#[test]
fn byte_fitting_skips_the_elided_form_of_a_turn_beyond_the_load_bound() {
    // Arrange
    let turn = tool_heavy_turn("work");
    let full = retained(&turn);
    let bounded_history = full.div_ceil(ELIDED_SOURCE_LIMIT_FACTOR);
    let unbounded_history = bounded_history - 1;

    // Act
    let within_bound = turn.clone().fit_bytes(full - 1, bounded_history);
    let beyond_bound = turn.clone().fit_bytes(full - 1, unbounded_history);

    // Assert
    assert_eq!(within_bound, Some(turn.elided()));
    assert_eq!(beyond_bound, Some(turn.brief()));
}

#[test]
fn history_byte_fitting_keeps_newest_turns_until_one_does_not_fit_briefly() {
    // Arrange
    let oldest = HistoryTurn::from(vec![ModelMessage::User("o".repeat(100))]);
    let middle = HistoryTurn::from(vec![ModelMessage::User("m".repeat(100))]);
    let newest = tool_heavy_turn("newest");
    let elided = retained(&newest.elided());

    // Act
    let kept = fit_history_bytes(
        [newest.clone(), middle.clone(), oldest],
        elided + retained(&middle) + 50,
    );

    // Assert
    assert_eq!(kept, [middle, newest.elided()]);
}

#[test]
fn selection_replays_an_oversized_turn_elided_before_briefly() {
    // Arrange
    let turn = tool_heavy_turn("work");
    let weigh = |messages: Vec<ModelMessage>| -> u64 {
        messages
            .iter()
            .map(|message| ByteEstimator.message_weight(message))
            .sum()
    };
    let elided_weight = weigh(turn.elided().replay());
    let brief_weight = weigh(turn.brief().replay());
    let turns = VecDeque::from(vec![turn.clone()]);

    // Act
    let (elided, elided_evicted) = select_recent_turns(&ByteEstimator, &turns, elided_weight);
    let (brief, brief_evicted) = select_recent_turns(&ByteEstimator, &turns, brief_weight);

    // Assert
    assert_eq!((elided, elided_evicted), (vec![turn.elided().replay()], 0));
    assert_eq!((brief, brief_evicted), (vec![turn.brief().replay()], 0));
}
