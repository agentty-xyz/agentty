//! Tests for Claude Code usage: normalized cached input, message-id
//! deduplication, subagent exclusion, and context windows.

use ag_contracts::{ContextFill, ModelUsage};
use serde_json::{Value, json};

use super::{ClaudeUsageMeter, micro_usd};

/// Returns a main-loop assistant message event with `id` and `input` fresh
/// input tokens over a 1,000-token cached prompt.
fn assistant(id: &str, input: u64, parent: Option<&str>) -> Value {
    json!({
        "type": "assistant",
        "parent_tool_use_id": parent,
        "message": {
            "id": id,
            "model": "claude-opus-5-5",
            "usage": {
                "input_tokens": input,
                "cache_read_input_tokens": 1_000,
                "cache_creation_input_tokens": 10,
                "output_tokens": 1
            }
        }
    })
}

#[test]
fn responses_count_once_per_message_id_until_the_result_arrives() {
    // Arrange
    let mut meter = ClaudeUsageMeter::default();

    // Act
    let reported = [
        meter.observe(&assistant("msg-1", 5, None)),
        meter.observe(&assistant("msg-1", 5, None)),
        meter.observe(&assistant("msg-2", 20, None)),
        meter.observe(&assistant("sub-1", 900, Some("toolu-1"))),
        meter.observe(&json!({"type": "system"})),
        meter.observe(&json!({"type": "assistant"})),
    ];
    let usage = meter.usage();

    // Assert
    assert_eq!(reported, [true, false, true, false, false, false]);
    assert_eq!(
        usage,
        ModelUsage {
            cache_read_tokens: Some(2_000),
            cache_write_tokens: Some(20),
            context: Some(ContextFill {
                last_input_tokens: 1_030,
                peak_input_tokens: 1_030,
                window_tokens: None,
            }),
            input_tokens: 2_045,
            output_tokens: 2,
            requests: 2,
            ..ModelUsage::new("")
        }
    );
}

#[test]
fn results_supersede_responses_and_name_the_context_window() {
    // Arrange
    let mut meter = ClaudeUsageMeter::default();
    meter.observe(&assistant("msg-1", 5, None));

    // Act
    meter.observe(&json!({
        "type": "result",
        "usage": {
            "input_tokens": 30,
            "cache_read_input_tokens": 3_000,
            "cache_creation_input_tokens": 0,
            "output_tokens": 400
        },
        "modelUsage": {
            "claude-opus-5-5": {"contextWindow": 1_000_000},
            "claude-haiku-5-5": {}
        }
    }));
    let usage = meter.usage();

    // Assert
    assert_eq!(usage.input_tokens, 3_030);
    assert_eq!(usage.cache_read_tokens, Some(3_000));
    assert_eq!(usage.output_tokens, 400);
    assert_eq!(
        usage.context.and_then(|context| context.window_tokens),
        Some(1_000_000)
    );
}

#[test]
fn reported_cost_sums_models_of_the_latest_result_and_ignores_invalid_costs() {
    // Arrange
    let mut meter = ClaudeUsageMeter::default();
    let silent_cost = meter.reported_cost();

    // Act
    meter.observe_payload(&json!([
        {"type": "result", "usage": {"input_tokens": 1, "output_tokens": 1},
         "modelUsage": {"claude-opus-5-5": {"costUSD": 0.01}, "claude-haiku-5-5": {"costUSD": 0.002}}},
        {"type": "result", "usage": {"input_tokens": 1, "output_tokens": 1},
         "modelUsage": {"claude-opus-5-5": {"costUSD": 0.03}, "claude-sonnet-5-5": {"costUSD": -0.5}}}
    ]));

    // Assert
    assert_eq!(silent_cost, None);
    assert_eq!(meter.reported_cost(), Some(32_000));
    assert_eq!(meter.usage().input_tokens, 2);
}

#[test]
fn micro_usd_rounds_dollars_and_rejects_negative_or_non_finite_amounts() {
    // Arrange
    let cases = [
        (0.012_345_6, Some(12_346)),
        (0.0, Some(0)),
        (-0.5, None),
        (f64::NAN, None),
        (f64::INFINITY, None),
    ];

    for (dollars, expected) in cases {
        // Act
        let micro = micro_usd(dollars);

        // Assert
        assert_eq!(micro, expected, "{dollars}");
    }
}

#[test]
fn missing_counts_are_unreported() {
    // Arrange
    let mut silent = ClaudeUsageMeter::default();
    let mut partial = ClaudeUsageMeter::default();

    // Act
    partial.observe(&json!({"type": "result", "usage": {"output_tokens": 3}}));

    // Assert
    assert_eq!(silent.usage(), ModelUsage::unreported("", 1));
    assert!(!silent.observe(&json!({"type": "assistant", "message": {"id": "msg"}})));
    let partial = partial.usage();
    assert_eq!(partial.unreported_requests, 1);
    assert_eq!(partial.cache_read_tokens, None);
}
