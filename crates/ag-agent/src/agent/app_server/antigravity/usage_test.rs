//! Antigravity per-turn token usage accounting unit tests.

use ag_contracts::{ContextFill, ModelUsage};

use crate::agent::app_server::antigravity::usage::{TokenUsage, TurnUsageTracker};

#[test]
fn parse_requires_input_and_output_and_defaults_cache_and_thinking() {
    // Arrange
    let cases = [
        (
            serde_json::json!({"cache_read_tokens": 7, "input_tokens": 3, "output_tokens": 2, "thinking_tokens": 1}),
            Some(TokenUsage {
                cache_read: 7,
                input: 3,
                output: 2,
                thinking: 1,
            }),
        ),
        (
            serde_json::json!({"input_tokens": 3, "output_tokens": 2}),
            Some(counters(3, 2)),
        ),
        (serde_json::json!({"output_tokens": 2}), None),
        (serde_json::json!({"input_tokens": 3}), None),
    ];

    for (usage, expected) in cases {
        // Act
        let parsed = TokenUsage::parse(&usage);

        // Assert
        assert_eq!(parsed, expected);
    }
}

#[test]
fn completed_steps_win_over_cumulative_usage_in_step_order_with_normalized_cache() {
    // Arrange
    let mut tracker = TurnUsageTracker::default();
    tracker.record_step(&serde_json::json!({
        "step_index": 4,
        "state": "DONE",
        "usage": {"cache_read_tokens": 30_000, "input_tokens": 278, "output_tokens": 4, "thinking_tokens": 2},
    }));
    tracker.record_step(&serde_json::json!({
        "step_index": 2,
        "state": "DONE",
        "usage": {"cache_read_tokens": 39_000, "input_tokens": 1_000, "output_tokens": 6, "thinking_tokens": 3},
    }));
    let result = serde_json::json!({
        "usage": {"input_tokens": 30_662, "output_tokens": 8},
    });
    let mut previous = Some(counters(30_384, 4));

    // Act
    let usage = tracker.finish(&result, &mut previous);

    // Assert
    assert_eq!(
        usage,
        ModelUsage {
            cache_read_tokens: Some(69_000),
            context: Some(ContextFill {
                last_input_tokens: 30_278,
                peak_input_tokens: 40_000,
                window_tokens: None,
            }),
            input_tokens: 70_278,
            output_tokens: 10,
            reasoning_tokens: Some(5),
            requests: 2,
            ..ModelUsage::new("")
        }
    );
    assert_eq!(previous, Some(counters(30_662, 8)));
}

#[test]
fn cumulative_usage_delta_is_fallback_when_steps_omit_usage() {
    // Arrange
    let tracker = TurnUsageTracker::default();
    let result = serde_json::json!({
        "usage": {"cache_read_tokens": 1_600, "input_tokens": 150, "output_tokens": 27, "thinking_tokens": 8},
    });
    let mut previous = Some(TokenUsage {
        cache_read: 1_000,
        input: 100,
        output: 20,
        thinking: 5,
    });

    // Act
    let usage = tracker.finish(&result, &mut previous);

    // Assert
    assert_eq!(usage, request(650, 7, 600, 3));
}

#[test]
fn active_and_usage_free_steps_leave_one_unreported_request() {
    // Arrange
    let mut tracker = TurnUsageTracker::default();
    tracker.record_step(&serde_json::json!({
        "step_index": 1,
        "state": "ACTIVE",
        "usage": {"input_tokens": 99, "output_tokens": 99},
    }));
    tracker.record_step(&serde_json::json!({
        "step_index": 2,
        "state": "DONE",
    }));
    tracker.record_step(&serde_json::json!({
        "state": "DONE",
        "usage": {"input_tokens": 99, "output_tokens": 99},
    }));
    let result = serde_json::json!({});
    let mut previous = None;

    // Act
    let usage = tracker.finish(&result, &mut previous);

    // Assert
    assert_eq!(usage, ModelUsage::unreported("", 1));
    assert_eq!(previous, None);
}

#[test]
fn duplicate_step_updates_replace_usage_instead_of_double_counting() {
    // Arrange
    let mut tracker = TurnUsageTracker::default();
    tracker.record_step(&serde_json::json!({
        "step_index": 1,
        "state": "DONE",
        "usage": {"input_tokens": 10, "output_tokens": 2},
    }));
    tracker.record_step(&serde_json::json!({
        "step_index": 1,
        "state": "done",
        "usage": {"input_tokens": 12, "output_tokens": 3},
    }));
    let mut previous = None;

    // Act
    let usage = tracker.finish(&serde_json::json!({}), &mut previous);

    // Assert
    assert_eq!(
        usage,
        ModelUsage {
            context: Some(ContextFill::of_request(12, None)),
            ..request(12, 3, 0, 0)
        }
    );
}

#[test]
fn first_cumulative_usage_becomes_turn_usage_and_baseline() {
    // Arrange
    let tracker = TurnUsageTracker::default();
    let result = serde_json::json!({
        "usage": {"input_tokens": 15, "output_tokens": 4},
    });
    let mut previous = None;

    // Act
    let usage = tracker.finish(&result, &mut previous);

    // Assert
    assert_eq!(usage, request(15, 4, 0, 0));
    assert_eq!(previous, Some(counters(15, 4)));
}

#[test]
fn cumulative_usage_delta_saturates_after_provider_counter_reset() {
    // Arrange
    let tracker = TurnUsageTracker::default();
    let result = serde_json::json!({
        "usage": {"input_tokens": 2, "output_tokens": 1},
    });
    let mut previous = Some(counters(100, 20));

    // Act
    let usage = tracker.finish(&result, &mut previous);

    // Assert
    assert_eq!(usage, request(0, 0, 0, 0));
    assert_eq!(previous, Some(counters(2, 1)));
}

/// Returns provider counters without cache-read or thinking tokens.
fn counters(input: u64, output: u64) -> TokenUsage {
    TokenUsage {
        input,
        output,
        ..TokenUsage::default()
    }
}

/// Returns one request's expected usage with input that includes the cache.
fn request(input_tokens: u64, output_tokens: u64, cache_read: u64, reasoning: u64) -> ModelUsage {
    ModelUsage {
        cache_read_tokens: Some(cache_read),
        input_tokens,
        output_tokens,
        reasoning_tokens: Some(reasoning),
        requests: 1,
        ..ModelUsage::new("")
    }
}
